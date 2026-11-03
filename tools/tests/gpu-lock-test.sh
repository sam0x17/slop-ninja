#!/usr/bin/env bash
# Tests for tools/gpu-lock.sh. They use a fake nvidia-smi and a scratch lock, so
# they need no GPU and never touch the host lock. Usage:
#   tools/tests/gpu-lock-test.sh [SCRATCH_DIR]
# SCRATCH_DIR (default: a new mktemp directory) is removed afterwards.
set -uo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
helper=$(dirname -- "$here")/gpu-lock.sh
host_lock=$(realpath -m -- "$(dirname -- "$(git -C "$here" rev-parse --path-format=absolute --git-common-dir)")/data/locks/gpu.lock")
root=${1:-$(mktemp -d)}
mkdir -p "$root"
root=$(cd -- "$root" && pwd -P)
fake=$root/fake
mkdir -p "$fake/bin"
lock=$root/test.lock
pass=0
fail=0
holder=

cleanup() {
  [[ -n $holder ]] && kill "$holder" 2>/dev/null
  wait 2>/dev/null
  rm -rf -- "$root"
}
trap cleanup EXIT

# Fake nvidia-smi: memory.used lines come from $fake/used ("fail" exits 1,
# "hang" sleeps); compute apps come from $fake/apps.
cat >"$fake/bin/nvidia-smi" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *--query-gpu=memory.used*)
    v=\$(cat "$fake/used")
    [[ \$v == fail ]] && exit 1
    [[ \$v == hang ]] && exec sleep 100
    printf '%s\n' "\$v" ;;
  *--query-compute-apps=*)
    [[ -f "$fake/apps" ]] && cat "$fake/apps"
    exit 0 ;;
  *) exit 9 ;;
esac
EOF
chmod +x "$fake/bin/nvidia-smi"
export PATH="$fake/bin:$PATH"

reset_fake() {
  printf '9\n' >"$fake/used"
  rm -f "$fake/apps" "$root"/exit "$root"/marker "$root"/child.pid
}

check() {
  local name=$1 want=$2 got=$3
  if [[ $want == "$got" ]]; then
    pass=$((pass + 1))
    printf 'ok   %s\n' "$name"
  else
    fail=$((fail + 1))
    printf 'FAIL %s: want %s, got %s\n' "$name" "$want" "$got"
  fi
}

exit_file_value() { [[ -f $root/exit ]] && cat "$root/exit" || echo missing; }
lock_free() { flock -n "$lock" true && echo free || echo held; }
gone() {
  local pid
  pid=$(cat "$root/child.pid" 2>/dev/null) || {
    echo no-pid
    return
  }
  for _ in $(seq 50); do
    kill -0 "$pid" 2>/dev/null || {
      echo gone
      return
    }
    sleep 0.1
  done
  echo alive
}
run() { "$helper" --unsafe-test-lock "$lock" --exit-file "$root/exit" "$@" 2>>"$root/log"; }
# Start the helper itself in the background (hp is its PID). Background jobs of
# a script ignore SIGINT, so restore the default disposition, as a foreground
# start or a systemd unit would have it.
run_bg() {
  env --default-signal=INT,QUIT "$helper" --unsafe-test-lock "$lock" --exit-file "$root/exit" "$@" 2>>"$root/log" &
  hp=$!
}

# Lock resolution ----------------------------------------------------------
out=$("$helper" --print-lock 2>>"$root/log")
check "default lock resolves to host lock" "$host_lock" "$out"
out=$("$helper" --lock "$host_lock" --print-lock 2>>"$root/log")
check "--lock naming the host lock is accepted" "$host_lock" "$out"
"$helper" --lock "$lock" --print-lock >/dev/null 2>>"$root/log"
check "--lock naming another file is refused" 2 $?

mkdir -p "$root/nogit"
for t in bash dirname realpath sed timeout env setsid flock sleep date mv rm mkdir cat; do
  ln -sf "$(command -v "$t")" "$root/nogit/$t"
done
PATH=$root/nogit "$helper" --print-lock >/dev/null 2>>"$root/log"
check "refuses when git is unavailable" 2 $?

mkdir -p "$root/outside"
cp "$helper" "$root/outside/gpu-lock.sh"
(cd / && "$root/outside/gpu-lock.sh" --print-lock >/dev/null 2>>"$root/log")
check "refuses outside the host checkout" 2 $?

# Usage and exit-file handling ----------------------------------------------
reset_fake
run --bogus -- true
check "unknown argument exits 2" 2 $?
check "unknown argument writes exit file" 2 "$(exit_file_value)"

reset_fake
run --interval 0 -- true
check "bad interval writes exit file" 2 "$(exit_file_value)"

reset_fake
"$helper" --unsafe-test-lock "$lock" --exit-file "$root/missing/exit" -- touch "$root/marker" 2>>"$root/log"
check "missing exit-file directory exits 2" 2 $?
check "missing exit-file directory does not run the command" absent "$([[ -e $root/marker ]] && echo present || echo absent)"

# Normal runs ---------------------------------------------------------------
reset_fake
run -- bash -c 'exit 7'
check "command status is returned" 7 $?
check "command status is written" 7 "$(exit_file_value)"
check "lock released after the run" free "$(lock_free)"

reset_fake
run -- bash -c 'kill -KILL $$'
check "signal death of the command is 128+N" 137 $?

reset_fake
run --watch 3 --watch-interval 1 -- bash -c 'echo "$$, 300" >'"$fake"'/apps; printf "%s\n" 350 >'"$fake"'/used; sleep 2.5'
check "own GPU process is not a race" 0 $?

# Refusals ------------------------------------------------------------------
reset_fake
flock -n "$lock" sleep 30 &
holder=$!
sleep 0.5
run -- touch "$root/marker"
check "held lock without --wait exits 75" 75 $?
check "held lock writes 75" 75 "$(exit_file_value)"
kill "$holder" 2>/dev/null
wait "$holder" 2>/dev/null
holder=

reset_fake
printf '600\n' >"$fake/used"
run -- true
check "memory at limit exits 75" 75 $?

reset_fake
printf '9\n600\n' >"$fake/used"
run -- true
check "maximum over GPUs is used" 75 $?

reset_fake
printf '[N/A]\n' >"$fake/used"
run -- true
check "unparsable memory refuses" 75 $?

reset_fake
printf 'hang\n' >"$fake/used"
start=$SECONDS
run --query-timeout 1 -- true
status=$?
check "hung nvidia-smi times out and refuses" 75 "$status"
check "hung nvidia-smi bounded in time" yes "$( ((SECONDS - start < 15)) && echo yes || echo no)"

reset_fake
printf '600\n' >"$fake/used"
(sleep 2 && printf '9\n' >"$fake/used") &
run --wait --interval 1 -- touch "$root/marker"
check "--wait retries until the GPU is free" 0 $?
check "--wait then runs the command" present "$([[ -e $root/marker ]] && echo present || echo absent)"
wait

# Signals -------------------------------------------------------------------
for sig in TERM:143 INT:130 HUP:129; do
  reset_fake
  run_bg --watch 0 -- bash -c 'echo $$ >'"$root"'/child.pid; exec sleep 60'
  for _ in $(seq 50); do [[ -s $root/child.pid ]] && break; sleep 0.1; done
  kill -s "${sig%%:*}" "$hp"
  wait "$hp"
  check "SIG${sig%%:*} exits ${sig#*:}" "${sig#*:}" $?
  check "SIG${sig%%:*} writes the exit file" "${sig#*:}" "$(exit_file_value)"
  check "SIG${sig%%:*} stops the command" gone "$(gone)"
  check "SIG${sig%%:*} releases the lock" free "$(lock_free)"
done

reset_fake
run_bg --watch 0 --grace 1 -- bash -c 'trap "" TERM; echo $$ >'"$root"'/child.pid; while :; do sleep 0.2; done'
for _ in $(seq 50); do [[ -s $root/child.pid ]] && break; sleep 0.1; done
kill -s TERM "$hp"
wait "$hp"
check "command ignoring TERM is killed after grace" 143 $?
check "command ignoring TERM is gone" gone "$(gone)"

# Start-race watch ----------------------------------------------------------
reset_fake
run --watch 5 --watch-interval 1 -- bash -c 'echo $$ >'"$root"'/child.pid; echo "1, 4000" >'"$fake"'/apps; exec sleep 60'
check "foreign GPU process aborts with 76" 76 $?
check "foreign GPU process writes 76" 76 "$(exit_file_value)"
check "foreign GPU process stops the command" gone "$(gone)"

reset_fake
run --watch 6 --watch-interval 1 -- bash -c 'echo $$ >'"$root"'/child.pid; printf "%s\n" 900 >'"$fake"'/used; exec sleep 60'
check "unattributed memory aborts with 76" 76 $?
check "unattributed memory stops the command" gone "$(gone)"

reset_fake
run --watch 4 --watch-interval 1 -- bash -c 'printf "%s\n" fail >'"$fake"'/used; sleep 3'
check "failed query during watch aborts with 76" 76 $?

reset_fake
run --watch 2 --watch-interval 1 -- bash -c 'sleep 3; echo "1, 4000" >'"$fake"'/apps; sleep 1'
check "no watch after the window" 0 $?

printf '%s passed, %s failed\n' "$pass" "$fail"
((fail == 0))

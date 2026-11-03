#!/usr/bin/env bash
# Run one command while holding the build-host host GPU lock.
#
# Usage: gpu-lock.sh [--wait] [--interval S] [--max-used-mib MIB] [--watch S]
#          [--watch-interval S] [--grace S] [--query-timeout S] [--exit-file PATH]
#          [--lock PATH] -- COMMAND [ARG...]
#        gpu-lock.sh --print-lock
#
# Lock. An exclusive flock(1) on the host lock data/locks/gpu.lock under the
# main checkout. The path is resolved from this script's Git common directory,
# so every worktree reaches the same file. The helper refuses to run (status 2)
# when that resolution fails; --lock is accepted only when it names the same file.
#
# Start check. After taking the lock, nvidia-smi memory.used must be below
# --max-used-mib (default 500) on every GPU. Without --wait a failed check exits
# 75. With --wait the helper releases the lock, waits --interval seconds
# (default 60) and tries again, without limit.
#
# Race watch. For --watch seconds after the start (default 30), every
# --watch-interval seconds (default 2), the helper queries the GPU. It aborts
# the command, exiting 76, when a compute process outside its own process tree
# (or its own systemd .service cgroup) appears, or when memory not attributed to
# its own processes stays at or above --max-used-mib on two consecutive polls,
# or when a query fails. The command is not restarted.
#
# Signals. TERM, INT and HUP are forwarded to the command's process group. After
# --grace seconds (default 30) the group is killed. The exit status is
# 128 + signal number.
#
# Exit file. --exit-file receives the final status atomically on every exit
# path: the command's status, 2 for usage or configuration errors, 70 for an
# internal error, 75 for a refusal, 76 for an aborted race, 128+N for signal N.
# The directory must exist and be writable, or the helper exits 2 before
# locking.
#
# Every nvidia-smi query runs under timeout(1) for --query-timeout seconds
# (default 30). The helper only ever signals its own command's process group;
# it never stops another process. The lock stays held while any process that
# inherited the lock descriptor is alive, including a server that the command
# leaves running. Per-process CUDA memory caps are the job's responsibility.
# Linux only: needs flock(1), setsid(1), timeout(1), env --default-signal,
# git and nvidia-smi.
set -uo pipefail

readonly HOST_LOCK=data/locks/gpu.lock
readonly EX_USAGE=2 EX_INTERNAL=70 EX_BUSY=75 EX_RACE=76

wait_for_lock=false
interval=60
max_used_mib=500
watch=30
watch_interval=2
grace=30
query_timeout=30
lock=
lock_arg=
exit_file=
exit_written=false
print_lock=false
unsafe_test_lock=
child=
sleeper=

write_exit() {
  [[ -n $exit_file && $exit_written == false ]] || return 0
  local tmp="$exit_file.tmp.$$"
  if printf '%s\n' "$1" >"$tmp" 2>/dev/null && mv -f "$tmp" "$exit_file" 2>/dev/null; then
    exit_written=true
  else
    rm -f "$tmp" 2>/dev/null
    printf 'gpu-lock: could not write exit file %s\n' "$exit_file" >&2
  fi
}

finish() {
  write_exit "$1"
  trap - EXIT
  exit "$1"
}

die() {
  local status=$1
  shift
  printf 'gpu-lock: %s\n' "$*" >&2
  finish "$status"
}

usage() {
  sed -n '4,7p' "${BASH_SOURCE[0]}" | sed 's/^# //' >&2
  [[ $# -gt 0 ]] && printf 'gpu-lock: %s\n' "$*" >&2
  finish "$EX_USAGE"
}

# True while PID exists and is not a zombie.
alive() {
  local stat
  { read -r stat <"/proc/$1/stat"; } 2>/dev/null || return 1
  stat=${stat##*) }
  [[ ${stat%% *} != Z ]]
}

# Stop our own command: signal its process group, wait up to --grace, then kill it.
stop_child() {
  local sig=$1 ticks=0
  [[ -n $child ]] || return 0
  kill -s "$sig" -- "-$child" 2>/dev/null || kill -s "$sig" "$child" 2>/dev/null
  while alive "$child" && ((ticks < grace * 10)); do
    sleep 0.1
    ((ticks++))
  done
  if alive "$child"; then
    printf 'gpu-lock: command still running after %ss; killing its process group\n' "$grace" >&2
    kill -s KILL -- "-$child" 2>/dev/null || kill -s KILL "$child" 2>/dev/null
  fi
  wait "$child" 2>/dev/null
  child=
}

on_signal() {
  local name=$1 num=$2
  trap '' TERM INT HUP
  printf 'gpu-lock: received SIG%s\n' "$name" >&2
  [[ -n $sleeper ]] && kill "$sleeper" 2>/dev/null
  stop_child "$name"
  finish $((128 + num))
}

on_exit() {
  local status=$?
  # Reached only on an unplanned exit; planned exits go through finish().
  ((status == 0)) && status=$EX_INTERNAL
  [[ -n $child ]] && stop_child TERM
  write_exit "$status"
}

trap on_exit EXIT
trap 'on_signal TERM 15' TERM
trap 'on_signal INT 2' INT
trap 'on_signal HUP 1' HUP

# Interruptible sleep: traps run as soon as a signal arrives.
nap() {
  sleep "$1" &
  sleeper=$!
  wait "$sleeper" 2>/dev/null
  sleeper=
}

# Find --exit-file first so that usage errors still write it.
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
  [[ ${args[i]} == -- ]] && break
  if [[ ${args[i]} == --exit-file ]] && ((i + 1 < ${#args[@]})); then
    exit_file=${args[i + 1]}
  fi
done
if [[ -n $exit_file ]]; then
  dir=$(dirname -- "$exit_file")
  if [[ ! -d $dir || ! -w $dir ]]; then
    exit_file=
    die "$EX_USAGE" "exit-file directory $dir does not exist or is not writable"
  fi
fi

need_value() { (($# >= 2)) && [[ -n $2 ]] || usage "$1 needs a value"; }
while (($#)); do
  case "$1" in
    --wait) wait_for_lock=true; shift ;;
    --print-lock) print_lock=true; shift ;;
    --interval) need_value "$@"; interval=$2; shift 2 ;;
    --max-used-mib) need_value "$@"; max_used_mib=$2; shift 2 ;;
    --watch) need_value "$@"; watch=$2; shift 2 ;;
    --watch-interval) need_value "$@"; watch_interval=$2; shift 2 ;;
    --grace) need_value "$@"; grace=$2; shift 2 ;;
    --query-timeout) need_value "$@"; query_timeout=$2; shift 2 ;;
    --lock) need_value "$@"; lock_arg=$2; shift 2 ;;
    --exit-file) need_value "$@"; shift 2 ;;
    # For the helper's own tests only: lock a scratch file instead of the host lock.
    --unsafe-test-lock) need_value "$@"; unsafe_test_lock=$2; shift 2 ;;
    --) shift; break ;;
    *) usage "unknown argument $1" ;;
  esac
done
for pair in "interval:$interval" "max-used-mib:$max_used_mib" "watch-interval:$watch_interval" \
  "grace:$grace" "query-timeout:$query_timeout"; do
  [[ ${pair#*:} =~ ^[1-9][0-9]*$ ]] || usage "--${pair%%:*} must be a positive integer"
done
[[ $watch =~ ^[0-9]+$ ]] || usage "--watch must be a non-negative integer"

if [[ -n $unsafe_test_lock ]]; then
  [[ -z $lock_arg ]] || usage "--lock and --unsafe-test-lock are exclusive"
  lock=$(realpath -m -- "$unsafe_test_lock")
  printf 'gpu-lock: WARNING: test lock %s is not the host GPU lock\n' "$lock" >&2
else
  here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P) ||
    die "$EX_USAGE" "cannot resolve the helper directory"
  common=$(git -C "$here" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) ||
    die "$EX_USAGE" "cannot resolve the Git common directory from $here; refusing to guess the lock path"
  [[ -n $common ]] || die "$EX_USAGE" "empty Git common directory; refusing to guess the lock path"
  lock=$(realpath -m -- "$(dirname -- "$common")/$HOST_LOCK")
  host=$lock
  if [[ -n $lock_arg && $(realpath -m -- "$lock_arg") != "$host" ]]; then
    die "$EX_USAGE" "--lock $lock_arg is not the host lock $host; refusing to run"
  fi
fi

if $print_lock; then
  printf '%s\n' "$lock"
  finish 0
fi
(($#)) || usage "no command given"

lock_dir=$(dirname -- "$lock")
[[ -d $lock_dir ]] || mkdir -p -- "$lock_dir" || die "$EX_USAGE" "cannot create $lock_dir"

smi() { timeout --kill-after=5 "$query_timeout" nvidia-smi "$@"; }

# Largest memory.used over all GPUs, in MiB; fails when nvidia-smi fails.
gpu_used_mib() {
  local out max=0 v
  out=$(smi --query-gpu=memory.used --format=csv,noheader,nounits) || return 1
  [[ -n $out ]] || return 1
  while read -r v; do
    v=${v//[[:space:]]/}
    [[ $v =~ ^[0-9]+$ ]] || return 1
    ((v > max)) && max=$v
  done <<<"$out"
  printf '%s\n' "$max"
}

# Sum of memory.used over all GPUs, in MiB.
gpu_total_mib() {
  local out sum=0 v
  out=$(smi --query-gpu=memory.used --format=csv,noheader,nounits) || return 1
  [[ -n $out ]] || return 1
  while read -r v; do
    v=${v//[[:space:]]/}
    [[ $v =~ ^[0-9]+$ ]] || return 1
    ((sum += v))
  done <<<"$out"
  printf '%s\n' "$sum"
}

own_cgroup=
{ read -r own_cgroup <"/proc/$$/cgroup"; } 2>/dev/null || own_cgroup=
[[ $own_cgroup == *.service ]] || own_cgroup=

# PIDs in this helper's process tree, one per line.
own_tree() {
  local -A parent=()
  local f stat rest p
  for f in /proc/[0-9]*/stat; do
    { read -r stat <"$f"; } 2>/dev/null || continue
    p=${f#/proc/}
    p=${p%/stat}
    rest=${stat##*) }
    rest=${rest#* }
    parent[$p]=${rest%% *}
  done
  local -a queue=("$$")
  local -A seen=(["$$"]=1)
  while ((${#queue[@]})); do
    local cur=${queue[0]}
    queue=("${queue[@]:1}")
    printf '%s\n' "$cur"
    for p in "${!parent[@]}"; do
      if [[ ${parent[$p]} == "$cur" && -z ${seen[$p]:-} ]]; then
        seen[$p]=1
        queue+=("$p")
      fi
    done
  done
}

is_own() {
  local pid=$1
  [[ -n ${own_pids[$pid]:-} ]] && return 0
  local cg=
  if [[ -n $own_cgroup ]] && { read -r cg <"/proc/$pid/cgroup"; } 2>/dev/null; then
    [[ $cg == "$own_cgroup" ]] && return 0
  fi
  return 1
}

# One race poll, run in the main shell. Sets race_reason and fails when the
# GPU is shared or cannot be checked.
unattributed_strikes=0
race_reason=
race_poll() {
  local apps total own_mib=0 own_unknown=false pid mem p
  apps=$(smi --query-compute-apps=pid,used_memory --format=csv,noheader,nounits) || {
    race_reason="compute-apps query failed"
    return 1
  }
  total=$(gpu_total_mib) || {
    race_reason="memory.used query failed"
    return 1
  }
  declare -gA own_pids=()
  while read -r p; do own_pids[$p]=1; done < <(own_tree)
  while IFS=, read -r pid mem; do
    pid=${pid//[[:space:]]/}
    mem=${mem//[[:space:]]/}
    [[ -z $pid ]] && continue
    if [[ ! $pid =~ ^[0-9]+$ ]]; then
      race_reason="unparsable compute-apps line '$pid,$mem'"
      return 1
    fi
    if ! is_own "$pid"; then
      race_reason="GPU process $pid (${mem} MiB) is not part of this job"
      return 1
    fi
    if [[ $mem =~ ^[0-9]+$ ]]; then
      ((own_mib += mem))
    else
      own_unknown=true
    fi
  done <<<"$apps"
  if ! $own_unknown && ((total - own_mib >= max_used_mib)); then
    ((unattributed_strikes++))
    if ((unattributed_strikes >= 2)); then
      race_reason="$((total - own_mib)) MiB in use is not attributed to this job"
      return 1
    fi
  else
    unattributed_strikes=0
  fi
  return 0
}

exec 9>>"$lock" || die "$EX_USAGE" "cannot open lock $lock"
while :; do
  if flock -n 9; then
    if used=$(gpu_used_mib); then
      if ((used < max_used_mib)); then
        break
      fi
      reason="GPU memory.used ${used} MiB is not below ${max_used_mib} MiB"
    else
      reason="nvidia-smi query failed"
    fi
    flock -u 9
  else
    reason="lock $lock is held by another job"
  fi
  if ! $wait_for_lock; then
    die "$EX_BUSY" "$reason; not starting"
  fi
  printf 'gpu-lock: %s; retrying in %ss\n' "$reason" "$interval" >&2
  nap "$interval"
done

# Holder note for humans; the flock on the inode is the actual lock.
printf 'pid=%s unit=%s started=%s used_mib=%s\n' "$$" "${SLOP_UNIT:-${INVOCATION_ID:-none}}" \
  "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$used" >"$lock"
printf 'gpu-lock: acquired %s with %s MiB in use\n' "$lock" "$used" >&2

# Own session and process group (pid == pgid), default INT/QUIT dispositions.
setsid env --default-signal=INT,QUIT,TERM,HUP "$@" &
child=$!

deadline=$((SECONDS + watch))
while ((watch > 0 && SECONDS < deadline)) && alive "$child"; do
  if ! race_poll; then
    printf 'gpu-lock: race detected during start watch: %s; stopping the command\n' "$race_reason" >&2
    stop_child TERM
    finish "$EX_RACE"
  fi
  nap "$watch_interval"
done

wait "$child"
status=$?
child=
finish "$status"

#!/usr/bin/env bash
# Installs the Slop Ninja native validator as persistent systemd user units.
#
#   deploy/install.sh --instance NAME [--from DIR [--unverified]] [--config FILE]
#                     [--init] [--enable [--start]] [--sandbox MODE] [--dry-run]
#
# --instance NAME  the instance: ~/.config/slop_ninja_validator/NAME.toml
# --from DIR       install slop_ninja_validator and slop-ninja-journal from DIR
#                  into ~/.local/bin. DIR must hold SHA256SUMS, listing both
#                  binaries exactly once, and SHA256SUMS.sig, the release
#                  signature over it. The signature is checked first, with
#                  ssh-keygen -Y verify (OpenSSH 8.1 or later) against the
#                  release key pinned beside this script
#                  (release-allowed_signers, the key in docs/testnet/README.md,
#                  "Release key"; any allowed_signers in DIR is ignored), then
#                  both digests. Each checked copy must then run
#                  (--help) on this host. The checked copies are the
#                  ones installed.
# --unverified     also install from DIR when it has no SHA256SUMS.sig, or no
#                  SHA256SUMS either (a local build). A signature or digest
#                  list DIR does hold is still checked and must pass.
# --config FILE    install FILE as the instance configuration (mode 600) when
#                  none exists; an existing configuration is never replaced
# --init           create the journal if the configuration names none yet
# --enable         enable slop_ninja_validator-dispatch@NAME (starts at boot)
# --start          with --enable, start it now, and fail unless it is
#                  still running, with no restart, 5 s later
# --sandbox MODE   auto (default), bwrap or none. bwrap writes a drop-in for
#                  both units that runs the validator in a bubblewrap sandbox
#                  showing it only its binary, this configuration, the hotkey
#                  files it names and its state directories, after testing
#                  that sandbox (sandbox-unit --check) under the unit's own
#                  restrictions (systemd-run with the unit's hardening); it
#                  fails if bubblewrap is missing or refused. auto does the
#                  same when bubblewrap works; where it does not, auto warns
#                  and continues without a sandbox only when no drop-in is
#                  installed yet, and otherwise fails and keeps the drop-in,
#                  so isolation is removed only by an explicit --sandbox
#                  none, which removes it. Re-run after changing a path in
#                  the configuration (a validator whose drop-in no longer
#                  matches its configuration refuses to start, exit 2).
#                  Without the sandbox the validator can read everything
#                  this account can: run it as a Unix user of its own
#                  (README, "Isolation").
# --dry-run        print every action instead of performing it
#
# The units are user units: lingering (loginctl enable-linger) keeps them
# running without a login session and starts them at boot. Linux only: macOS
# is not a declared platform, so no launchd job is provided.
#
# Each instance keeps its journal, package and caches in a directory of its
# own, ~/.local/state/slop_ninja_validator/NAME (created here): the sandbox
# binds the journal's directory read-write as a whole, so sandbox-unit refuses
# a directory that also holds another instance's files.
#
# Nothing here reads a private key, signs or contacts the chain except `init`,
# which reads only the hotkey's public companion. Re-running is safe.
set -euo pipefail

instance="" from="" config_src="" init=0 enable=0 start=0 dry=0 unverified=0 sandbox=auto
while (($#)); do
  case "$1" in
    --instance) instance="${2:?--instance needs a name}"; shift 2 ;;
    --from) from="${2:?--from needs a directory}"; shift 2 ;;
    --unverified) unverified=1; shift ;;
    --config) config_src="${2:?--config needs a file}"; shift 2 ;;
    --init) init=1; shift ;;
    --enable) enable=1; shift ;;
    --start) start=1; shift ;;
    --sandbox) sandbox="${2:?--sandbox needs auto, bwrap or none}"; shift 2 ;;
    --dry-run) dry=1; shift ;;
    -h|--help) sed -n '2,/^set -/{/^#/p}' "$0"; exit 0 ;;
    *) echo "install.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done

die() { echo "install.sh: $*" >&2; exit 2; }
run() {
  if ((dry)); then
    printf '+'; printf ' %q' "$@"; printf '\n'
  else
    "$@"
  fi
}

[[ -n "$instance" ]] || die "--instance is required"
[[ "$instance" =~ ^[A-Za-z0-9_.-]{1,64}$ && "$instance" != "." && "$instance" != ".." ]] \
  || die "--instance must be a plain name of at most 64 letters, digits, '-', '_' or '.'"
((start == 0 || enable == 1)) || die "--start requires --enable"
[[ "$sandbox" =~ ^(auto|bwrap|none)$ ]] || die "--sandbox must be auto, bwrap or none"
[[ "$(uname -s)" == Linux ]] || die "only Linux is a declared platform; no launchd job is provided"

systemctl_cmd=${SYSTEMCTL:-systemctl}
systemd_run_cmd=${SYSTEMD_RUN:-systemd-run}
bwrap_cmd=${BWRAP:-$(command -v bwrap || true)}
loginctl_cmd=${LOGINCTL:-loginctl}
user=${USER:-$(id -un)}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
bin_dir="$HOME/.local/bin"
config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/slop_ninja_validator"
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
state_dir="${XDG_STATE_HOME:-$HOME/.local/state}/slop_ninja_validator"
instance_state="$state_dir/$instance"
config="$config_dir/$instance.toml"
validator="$bin_dir/slop_ninja_validator"
unit="slop_ninja_validator-dispatch@$instance.service"

if ((dry == 0)); then
  command -v "$systemctl_cmd" >/dev/null || die "systemctl is not available"
fi

# The release key and its principal. Only this pinned copy is trusted: the
# allowed_signers and .pub files a release carries prove nothing by themselves.
release_signers="$here/release-allowed_signers"
release_principal=slop-ninja-testnet-release
binaries=(slop_ninja_validator slop-ninja-journal)

if [[ -n "$from" ]]; then
  [[ -d "$from" ]] || die "--from $from is not a directory"
  # Verify before anything is installed, also in a dry run, and install the
  # private copies checked, so the bytes verified are the bytes installed
  # even if DIR changes meanwhile. A dry run installs nothing: it writes only
  # these copies, to a temporary directory removed on exit.
  stage=$(mktemp -d "${TMPDIR:-/tmp}/slop_ninja_install.XXXXXXXX")
  trap 'rm -rf -- "$stage"' EXIT
  for binary in "${binaries[@]}"; do
    [[ -f "$from/$binary" ]] || die "$from/$binary is missing"
    cp -- "$from/$binary" "$stage/$binary"
  done
  for file in SHA256SUMS SHA256SUMS.sig; do
    if [[ -e "$from/$file" ]]; then
      [[ -f "$from/$file" ]] || die "$from/$file is not a file"
      cp -- "$from/$file" "$stage/$file"
    fi
  done
  if [[ -f "$stage/SHA256SUMS.sig" ]]; then
    [[ -f "$stage/SHA256SUMS" ]] || die "$from has SHA256SUMS.sig but no SHA256SUMS"
    [[ -f "$release_signers" ]] || die "the pinned release key $release_signers is missing"
    command -v ssh-keygen >/dev/null \
      || die "ssh-keygen (OpenSSH 8.1 or later) is needed to verify the release signature"
    if ! verdict=$(ssh-keygen -Y verify -f "$release_signers" -I "$release_principal" -n file \
        -s "$stage/SHA256SUMS.sig" <"$stage/SHA256SUMS" 2>&1); then
      printf '%s\n' "$verdict" >&2
      # An ssh-keygen without -Y (before OpenSSH 8.1) says nothing about the
      # release: name the tool, not the release, as the problem.
      if grep -qiE "(unknown|illegal|invalid) option.*Y" <<<"$verdict"; then
        die "this ssh-keygen cannot verify signatures (-Y needs OpenSSH 8.1 or later); not installing"
      fi
      die "SHA256SUMS.sig in $from is not a valid release signature over its SHA256SUMS; not installing"
    fi
    echo "install.sh: $verdict"
  elif ((unverified)); then
    echo "install.sh: installing from $from without a release signature (--unverified)" >&2
  else
    die "no SHA256SUMS.sig in $from; pass --unverified only for binaries you built yourself"
  fi
  if [[ -f "$stage/SHA256SUMS" ]]; then
    # Each installed binary must have one line in SHA256SUMS, and its digest
    # must match.
    for binary in "${binaries[@]}"; do
      line=$(grep -E "^[0-9a-f]{64} [ *]$binary\$" "$stage/SHA256SUMS" || true)
      [[ -n "$line" && "$line" != *$'\n'* ]] || die "SHA256SUMS in $from must list $binary exactly once"
      [[ "${line:0:64}" == "$(cd "$stage" && sha256sum -- "$binary" | cut -c1-64)" ]] \
        || die "$from/$binary does not match SHA256SUMS"
    done
  else
    echo "install.sh: installing from $from without SHA256SUMS (--unverified)" >&2
  fi
  # Run each checked copy once before anything is replaced, so a release
  # this host cannot execute (another architecture, an older C library)
  # leaves the working binaries in place.
  for binary in "${binaries[@]}"; do
    chmod 0700 "$stage/$binary"
    if ! "$stage/$binary" --help >/dev/null; then
      die "$from/$binary does not run on this host (see the error above; if $(dirname "$stage") is mounted noexec, set TMPDIR to another directory); nothing installed. Build from source and pass --unverified on other platforms"
    fi
  done
  # A dry run prints the source it would install from; the copies are gone
  # when it exits.
  installed_from=$stage
  ((dry == 0)) || installed_from=$from
fi

run install -d -m 0755 "$bin_dir"
run install -d -m 0700 "$config_dir" "$state_dir" "$instance_state"
run install -d -m 0755 "$unit_dir"

if [[ -n "$from" ]]; then
  for binary in "${binaries[@]}"; do
    run install -m 0755 "$installed_from/$binary" "$bin_dir/$binary"
  done
fi

run install -m 0644 "$here/slop_ninja_validator@.service" "$here/slop_ninja_validator-dispatch@.service" "$unit_dir/"

if [[ -e "$config" ]]; then
  echo "install.sh: keeping the existing $config"
  mode=$(stat -c %a "$config")
  [[ "$mode" =~ ^[0-7]00$ ]] || echo "install.sh: $config is mode $mode; chmod 600 it" >&2
elif [[ -n "$config_src" ]]; then
  [[ -f "$config_src" ]] || die "--config $config_src is not a file"
  run install -m 0600 "$config_src" "$config"
else
  run install -m 0600 "$here/../validator.example.toml" "$config"
  echo "install.sh: installed the example as $config; edit every value (netuid 0 is refused) and re-run"
  ((enable == 0)) || die "not enabling an unedited example configuration"
fi

if ((dry == 0)); then
  [[ -x "$validator" ]] || die "$validator is not installed; pass --from"
  if ! summary=$("$validator" --config "$config" check-config); then
    die "check-config refused $config; fix it and re-run"
  fi
  if grep -q '"journal_exists": false' <<<"$summary"; then
    if ((init)); then
      run "$validator" --config "$config" init
    else
      echo "install.sh: no journal yet; create it once with: $validator --config $config init (or --init)"
      ((enable == 0)) || die "not enabling a validator without a journal"
    fi
  fi
else
  run "$validator" --config "$config" check-config
  ((init == 0)) || run "$validator" --config "$config" init
fi

# The unit's own restrictions, for the checks below: a check passes only
# where the unit's process can do the same. hardening_keys are carried into
# a transient unit (systemd-run -p); unit_only_keys are the templates' other
# [Service] settings, which describe the long-running unit only (start,
# restart, stop and logging). Every [Service] key of the templates and the
# drop-in is in one of the two lists (fetch_tests checks it).
hardening_keys="NoNewPrivileges UMask LockPersonality MemoryDenyWriteExecute RestrictRealtime RestrictSUIDSGID RestrictNamespaces KeyringMode RestrictAddressFamilies SystemCallArchitectures SystemCallFilter SystemCallErrorNumber MemoryMax MemorySwapMax"
unit_only_keys="Type ExecStart Restart RestartSec RestartSteps RestartMaxDelaySec RestartPreventExitStatus KillSignal TimeoutStopSec SyslogIdentifier"
# unit_props FILE...: the hardening assignments of the [Service] sections of
# FILE... read in order, as systemd merges them (an empty value resets the
# key), one Key=Value per line.
unit_props() {
  awk -v keys="$hardening_keys" '
    BEGIN { n = split(keys, k, " "); for (i = 1; i <= n; i++) want[k[i]] = 1 }
    FNR == 1 { service = 0 }
    /^\[/ { service = ($0 == "[Service]"); next }
    !service || /^[[:space:]]*(#|;)/ || index($0, "=") == 0 { next }
    {
      key = substr($0, 1, index($0, "=") - 1); value = substr($0, index($0, "=") + 1)
      if (!(key in want)) next
      if (!(key in count)) order[++keys_seen] = key
      if (value == "") { count[key] = 0; next }
      values[key, ++count[key]] = value
    }
    END { for (i = 1; i <= keys_seen; i++) for (j = 1; j <= count[order[i]]; j++) print order[i] "=" values[order[i], j] }
  ' "$@"
}
# under_unit FILE... -- COMMAND...: runs COMMAND in a transient user unit
# with the hardening of FILE... and the unit's cleared environment, waiting
# for it and passing its exit status on.
under_unit() {
  local files=() props=() line
  # A dry run plans a drop-in it has not rendered: skip files not there yet.
  while [[ "$1" != -- ]]; do [[ ! -f "$1" ]] || files+=("$1"); shift; done
  shift
  while IFS= read -r line; do props+=(-p "$line"); done < <(unit_props "${files[@]}")
  run "$systemd_run_cmd" --user --wait --pipe --collect --quiet --service-type=exec "${props[@]}" -- \
    /usr/bin/env -i HOME="$HOME" RUST_BACKTRACE=0 "$@" </dev/null
}

# A check's report is not needed; a dry run prints the planned command.
check_out=/dev/null
((dry == 0)) || check_out=/dev/stdout

# The sandbox drop-ins, one per unit of this instance. In a dry run the
# sandbox is planned, not tested.
dropin_name=50-sandbox.conf
sandbox_units=("slop_ninja_validator-dispatch@$instance.service:dispatch:slop_ninja_validator-dispatch@.service" "slop_ninja_validator@$instance.service:dry-run:slop_ninja_validator@.service")
installed_dropins=()
for entry in "${sandbox_units[@]}"; do
  unit_name=${entry%%:*}
  [[ ! -e "$unit_dir/$unit_name.d/$dropin_name" ]] || installed_dropins+=("$unit_dir/$unit_name.d/$dropin_name")
done
# auto never removes isolation it finds installed: a bubblewrap that stopped
# working (an update, a changed AppArmor policy, another PATH) fails the
# install and keeps the drop-in, so the next restart is not silently
# unsandboxed. Only --sandbox none removes it.
no_bwrap() {
  [[ "$sandbox" == auto ]] || die "--sandbox bwrap: $1"
  ((${#installed_dropins[@]} == 0)) \
    || die "$1, but ${installed_dropins[*]} is installed; the drop-in is kept. Fix bubblewrap and re-run, or pass --sandbox none to remove the isolation"
  echo "install.sh: WARNING: $1, so the validator runs without file isolation" >&2
  sandbox=none
}
if [[ "$sandbox" != none ]]; then
  if [[ -z "$bwrap_cmd" || ! -x "$bwrap_cmd" ]]; then
    no_bwrap "bubblewrap (bwrap) is not installed"
  elif ((dry == 0)) && ! "$bwrap_cmd" --unshare-all --unshare-user --share-net --disable-userns \
      --ro-bind / / -- /bin/true >/dev/null 2>&1; then
    no_bwrap "$bwrap_cmd cannot create a sandbox on this host"
  else
    sandbox=bwrap
  fi
fi
if [[ "$sandbox" == bwrap ]]; then
  for entry in "${sandbox_units[@]}"; do
    unit_name=${entry%%:*} rest=${entry#*:}
    kind=${rest%%:*} template=${rest#*:}
    dropin_dir="$unit_dir/$unit_name.d"
    render=("$validator" --config "$config" sandbox-unit --unit "$kind" --binary "$validator" --bwrap "$bwrap_cmd")
    run install -d -m 0755 "$dropin_dir"
    if ((dry)); then
      printf '+'; printf ' %q' "${render[@]}"; printf ' > %q\n' "$dropin_dir/.$dropin_name.new"
      under_unit "$here/$template" "$dropin_dir/.$dropin_name.new" -- "${render[@]}" --check
      run mv -f "$dropin_dir/.$dropin_name.new" "$dropin_dir/$dropin_name"
    else
      tmp=$(mktemp "$dropin_dir/.$dropin_name.XXXXXX")
      if ! "${render[@]}" >"$tmp"; then
        rm -f -- "$tmp"
        die "sandbox-unit refused the drop-in for $unit_name (see the error above); fix the paths it names, or pass --sandbox none"
      fi
      # The test runs under the unit's hardening and this drop-in's own
      # namespace and system-call lists, as the unit will.
      if ! under_unit "$here/$template" "$tmp" -- "${render[@]}" --check >/dev/null; then
        rm -f -- "$tmp"
        die "the sandbox test failed for $unit_name under the unit's restrictions (see above); fix the paths it names, or pass --sandbox none"
      fi
      chmod 0644 "$tmp"
      mv -f -- "$tmp" "$dropin_dir/$dropin_name"
    fi
  done
  echo "install.sh: the validator runs in a bubblewrap sandbox (drop-in $dropin_name; re-run after changing a configured path)"
else
  for entry in "${sandbox_units[@]}"; do
    unit_name=${entry%%:*} template=${entry##*:}
    if [[ -e "$unit_dir/$unit_name.d/$dropin_name" ]]; then
      run rm -f "$unit_dir/$unit_name.d/$dropin_name"
    fi
    # Without the drop-in the template's own restrictions apply; check that
    # the validator starts under them.
    under_unit "$here/$template" -- "$validator" --config "$config" check-config >"$check_out" \
      || die "check-config fails under the restrictions of $unit_name (see above); this host cannot run the unit as installed"
  done
  echo "install.sh: no file isolation: the validator can read everything $user can; run it as a Unix user of its own (README, \"Isolation\")" >&2
fi

linger=no
if ((dry == 0)); then
  linger=$("$loginctl_cmd" show-user "$user" --property=Linger --value 2>/dev/null || echo no)
fi
[[ "$linger" == yes ]] || run "$loginctl_cmd" enable-linger "$user"
run "$systemctl_cmd" --user daemon-reload

if ((enable)); then
  if ((start)); then
    run "$systemctl_cmd" --user enable --now "$unit"
    # A unit that cannot start (refused by the sandbox, a check, the host)
    # exits at once; one that restarts would hide it behind Restart=always.
    if ((dry == 0)); then
      sleep 5
      state=$("$systemctl_cmd" --user show "$unit" --property=ActiveState --value 2>/dev/null || true)
      restarts=$("$systemctl_cmd" --user show "$unit" --property=NRestarts --value 2>/dev/null || true)
      [[ "$state" == active && "${restarts:-0}" == 0 ]] \
        || die "$unit is not running 5 s after start (ActiveState=${state:-unknown}, NRestarts=${restarts:-unknown}); see journalctl --user -u $unit"
    fi
  else
    run "$systemctl_cmd" --user enable "$unit"
  fi
fi

cat <<EOF
install.sh: done.
  status:  systemctl --user status $unit
  logs:    journalctl --user -u $unit -f
  stop:    systemctl --user stop $unit   (the journal pauses; retained work survives)
EOF

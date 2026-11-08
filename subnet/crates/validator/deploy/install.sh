#!/usr/bin/env bash
# Installs the SlopNinja native validator as persistent systemd user units.
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
# --start          with --enable, start it now (restart it when it is
#                  already running, so the new build and drop-in run now:
#                  use a settled window), then wait, for at most the unit's
#                  own start timeout, until this start logs package_verified.
#                  Fails if the unit stops or restarts first; warns, and
#                  leaves it running, if the timeout passes without it.
#                  Without --start a running unit keeps its old process
#                  until its next restart (the installer says so).
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
# --dry-run        print every action instead of performing it. The
#                  read-only checks still run with the new build
#                  (check-config, and on an upgrade the sandbox render), so
#                  a dry run refuses whatever the install would refuse.
#
# Every check that can refuse runs before anything is replaced. The files
# replaced after that (binaries, unit templates, drop-ins) are copied first
# and put back if a later step fails, so a refusal never leaves a new build
# under an old drop-in for the next automatic restart to start.
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
# Files replaced so far, as "TARGET|COPY" (COPY empty: TARGET did not
# exist), and the directory holding the copies; see keep below. Any exit
# other than success before the installation is complete puts each back.
stage="" backup="" replaced=()
restore() {
  local entry target saved
  for entry in "${replaced[@]}"; do
    target=${entry%%|*} saved=${entry#*|}
    if [[ -n "$saved" ]]; then
      cp -p -- "$saved" "$target.restore.$$" && mv -f -- "$target.restore.$$" "$target"
    else
      rm -f -- "$target"
    fi
  done
  replaced=()
}
cleanup() {
  local status=$?
  if ((status != 0 && ${#replaced[@]})); then
    restore
    echo "install.sh: restored the previous binaries, unit templates and drop-ins" >&2
  fi
  [[ -z "$stage" ]] || rm -rf -- "$stage"
  [[ -z "$backup" ]] || rm -rf -- "$backup"
}
trap cleanup EXIT
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

# The build every check below runs: the verified copy about to be
# installed, or the installed binary when there is no --from.
candidate=$validator
[[ -z "$from" ]] || candidate="$stage/slop_ninja_validator"

# The sandbox mode, decided (and any refusal made) before anything changes.
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

run install -d -m 0755 "$bin_dir"
run install -d -m 0700 "$config_dir" "$state_dir" "$instance_state"
run install -d -m 0755 "$unit_dir"

# The configuration is installed only when none exists, so this replaces
# nothing a running unit reads.
if [[ -e "$config" ]]; then
  echo "install.sh: keeping the existing $config"
  mode=$(stat -c %a "$config")
  [[ "$mode" =~ ^[0-7]00$ ]] || echo "install.sh: $config is mode $mode; chmod 600 it" >&2
  check_config=$config
elif [[ -n "$config_src" ]]; then
  [[ -f "$config_src" ]] || die "--config $config_src is not a file"
  run install -m 0600 "$config_src" "$config"
  check_config=$config
  ((dry == 0)) || check_config=$config_src
else
  run install -m 0600 "$here/../validator.example.toml" "$config"
  echo "install.sh: installed the example as $config; edit every value (netuid 0 is refused) and re-run"
  ((enable == 0)) || die "not enabling an unedited example configuration"
  check_config=$config
  ((dry == 0)) || check_config="$here/../validator.example.toml"
fi

# Every check that can refuse runs here, with the new build, before any
# binary, template or drop-in is replaced, so a refusal leaves the working
# installation (and what its next automatic restart runs) as it was. A dry
# run makes the same checks: they only read.
journal_exists=unknown
if [[ -x "$candidate" ]]; then
  if ! summary=$("$candidate" --config "$check_config" check-config); then
    die "check-config refused $check_config; fix it and re-run (nothing replaced)"
  fi
  if grep -q '"journal_exists": false' <<<"$summary"; then
    journal_exists=no
  elif grep -q '"journal_exists": true' <<<"$summary"; then
    journal_exists=yes
  fi
elif ((dry == 0)); then
  die "$validator is not installed; pass --from"
fi
if [[ "$journal_exists" == no ]] && ((init == 0)); then
  ((enable == 0)) || die "not enabling a validator without a journal; create it with --init (nothing replaced)"
fi
# The drop-in render's refusals (the bind list, strays beside the journal,
# shared or key-holding directories) depend only on the configuration and
# the paths it names, so the new build renders here too and its output is
# discarded: against the installed binary path when there is one, and
# against the configuration being installed (in a dry run, the --config
# file, as nothing is installed yet). The drop-in actually installed is
# rendered again below, once the binary and configuration are in place.
if [[ "$sandbox" == bwrap && -x "$candidate" && -f "$check_config" ]]; then
  render_binary=$validator
  [[ -f "$render_binary" ]] || render_binary=$candidate
  for entry in "${sandbox_units[@]}"; do
    unit_name=${entry%%:*} rest=${entry#*:}
    kind=${rest%%:*}
    if ! "$candidate" --config "$check_config" sandbox-unit --unit "$kind" --binary "$render_binary" \
        --bwrap "$bwrap_cmd" >/dev/null; then
      die "sandbox-unit refused the drop-in for $unit_name (see the error above); nothing replaced. Fix the paths it names, or pass --sandbox none"
    fi
  done
fi

# From here files are replaced. Each is kept first (keep), and any failure
# before the installation is complete puts every one back (cleanup), so the
# next restart of the unit runs the old build with its old drop-in, never
# the new build under the old drop-in.
# keep FILE: remember FILE's current content (or its absence) once.
keep() {
  ((dry == 0)) || return 0
  local entry saved=""
  for entry in "${replaced[@]}"; do [[ "${entry%%|*}" != "$1" ]] || return 0; done
  if [[ -e "$1" ]]; then
    [[ -n "$backup" ]] || backup=$(mktemp -d "${TMPDIR:-/tmp}/slop_ninja_install_backup.XXXXXXXX")
    saved="$backup/${#replaced[@]}"
    cp -p -- "$1" "$saved" || die "cannot keep a copy of $1"
  fi
  replaced+=("$1|$saved")
}

if [[ -n "$from" ]]; then
  for binary in "${binaries[@]}"; do
    keep "$bin_dir/$binary"
    run install -m 0755 "$installed_from/$binary" "$bin_dir/$binary" || die "cannot install $bin_dir/$binary"
  done
fi
for template in slop_ninja_validator@.service slop_ninja_validator-dispatch@.service; do
  keep "$unit_dir/$template"
done
run install -m 0644 "$here/slop_ninja_validator@.service" "$here/slop_ninja_validator-dispatch@.service" "$unit_dir/" \
  || die "cannot install the unit templates"

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
if [[ "$sandbox" == bwrap ]]; then
  for entry in "${sandbox_units[@]}"; do
    unit_name=${entry%%:*} rest=${entry#*:}
    kind=${rest%%:*} template=${rest#*:}
    dropin_dir="$unit_dir/$unit_name.d"
    render=("$validator" --config "$config" sandbox-unit --unit "$kind" --binary "$validator" --bwrap "$bwrap_cmd")
    run install -d -m 0755 "$dropin_dir" || die "cannot create $dropin_dir"
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
      keep "$dropin_dir/$dropin_name"
      mv -f -- "$tmp" "$dropin_dir/$dropin_name" || die "cannot install $dropin_dir/$dropin_name"
    fi
  done
  echo "install.sh: the validator runs in a bubblewrap sandbox (drop-in $dropin_name; re-run after changing a configured path)"
else
  for entry in "${sandbox_units[@]}"; do
    unit_name=${entry%%:*} template=${entry##*:}
    if [[ -e "$unit_dir/$unit_name.d/$dropin_name" ]]; then
      keep "$unit_dir/$unit_name.d/$dropin_name"
      run rm -f "$unit_dir/$unit_name.d/$dropin_name"
    fi
    # Without the drop-in the template's own restrictions apply; check that
    # the validator starts under them.
    under_unit "$here/$template" -- "$validator" --config "$config" check-config >"$check_out" \
      || die "check-config fails under the restrictions of $unit_name (see above); this host cannot run the unit as installed"
  done
  echo "install.sh: no file isolation: the validator can read everything $user can; run it as a Unix user of its own (README, \"Isolation\")" >&2
fi
# Installed and tested: nothing is put back from here on.
replaced_any=${#replaced[@]}
replaced=()

if [[ "$journal_exists" != yes ]]; then
  if ((init)); then
    if ((dry)) || [[ "$journal_exists" == no ]]; then
      run "$validator" --config "$config" init
    fi
  elif [[ "$journal_exists" == no ]]; then
    echo "install.sh: no journal yet; create it once with: $validator --config $config init (or --init)"
  fi
fi

linger=no
if ((dry == 0)); then
  linger=$("$loginctl_cmd" show-user "$user" --property=Linger --value 2>/dev/null || echo no)
fi
[[ "$linger" == yes ]] || run "$loginctl_cmd" enable-linger "$user"
run "$systemctl_cmd" --user daemon-reload

# span_seconds SPAN: a systemd time span as printed by systemctl show
# ("1min 30s", "90s", "500ms"), in whole seconds rounded up; fails on
# "infinity" or anything else.
span_seconds() {
  local word total_ms=0 number unit
  [[ -n "$1" ]] || return 1
  for word in $1; do
    [[ "$word" =~ ^([0-9]+)(us|ms|s|min|h|d)$ ]] || return 1
    number=${BASH_REMATCH[1]} unit=${BASH_REMATCH[2]}
    case "$unit" in
      us) total_ms=$((total_ms + (number + 999) / 1000)) ;;
      ms) total_ms=$((total_ms + number)) ;;
      s) total_ms=$((total_ms + number * 1000)) ;;
      min) total_ms=$((total_ms + number * 60000)) ;;
      h) total_ms=$((total_ms + number * 3600000)) ;;
      d) total_ms=$((total_ms + number * 86400000)) ;;
    esac
  done
  echo $(((total_ms + 999) / 1000))
}
# unit_prop NAME: one property of the dispatch unit, empty when unknown.
unit_prop() { "$systemctl_cmd" --user show "$unit" --property="$1" --value 2>/dev/null || true; }
was_active=no
((dry)) || [[ "$(unit_prop ActiveState)" != active ]] || was_active=yes
if ((enable)); then
  if ((start)); then
    # An active unit keeps running its old process until it restarts, so
    # --start restarts it: the new build and drop-in run now, not first at
    # some later unattended restart. Run it in a settled window (no commit
    # or reveal in flight); the journal recovers an interrupted one anyway.
    if [[ "$was_active" == yes ]]; then
      echo "install.sh: $unit is running; restarting it on the new build"
      run "$systemctl_cmd" --user enable "$unit"
      run "$systemctl_cmd" --user restart "$unit"
    else
      ((dry == 0)) || echo "install.sh: (restarts $unit instead when it is already active)"
      run "$systemctl_cmd" --user enable --now "$unit"
    fi
    if ((dry == 0)); then
      # Started: wait until this invocation verified its package
      # (package_verified, logged once startup recovery, the commit/reveal
      # check, the fetch and the bundle checks passed), or until it exits
      # or restarts, for at most the unit's own start timeout.
      invocation=$(unit_prop InvocationID)
      wait_s=$(span_seconds "$(unit_prop TimeoutStartUSec)") || wait_s=$(span_seconds "$(unit_prop TimeoutStopUSec)") \
        || die "$unit reports no finite start or stop timeout to wait by; follow journalctl --user -u $unit -f until package_verified"
      deadline=$((SECONDS + wait_s))
      verdict=waiting
      while ((SECONDS < deadline)); do
        state=$(unit_prop ActiveState)
        now=$(unit_prop InvocationID)
        if [[ "$state" != active && "$state" != activating ]] || [[ -n "$invocation" && "$now" != "$invocation" ]]; then
          verdict=failed
          break
        fi
        if [[ -n "$invocation" ]] && journalctl --user _SYSTEMD_INVOCATION_ID="$invocation" -o cat --no-pager 2>/dev/null \
            | grep -q 'package_verified'; then
          verdict=verified
          break
        fi
        sleep 1
      done
      case "$verdict" in
        verified) echo "install.sh: $unit verified its package and runs" ;;
        failed) die "$unit stopped or restarted during startup (ActiveState=${state:-unknown}); see journalctl --user -u $unit" ;;
        *) echo "install.sh: WARNING: $unit is running but has not verified its package within its start timeout (startup recovery or the fetch is still waiting); it keeps running. Follow journalctl --user -u $unit -f until package_verified" >&2 ;;
      esac
    fi
  else
    run "$systemctl_cmd" --user enable "$unit"
  fi
fi
if [[ "$was_active" == yes ]] && ((start == 0)) && ((replaced_any)); then
  echo "install.sh: $unit is still running the previous build; the new build and drop-in take effect at its next restart. Restart it now, in a settled window: systemctl --user restart $unit" >&2
fi

cat <<EOF
install.sh: done.
  status:  systemctl --user status $unit
  logs:    journalctl --user -u $unit -f
  stop:    systemctl --user stop $unit   (the journal pauses; retained work survives)
EOF

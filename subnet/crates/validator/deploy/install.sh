#!/usr/bin/env bash
# Installs the Slop Ninja native validator as persistent systemd user units.
#
#   deploy/install.sh --instance NAME [--from DIR [--unverified]] [--config FILE]
#                     [--init] [--enable [--start]] [--dry-run]
#
# --instance NAME  the instance: ~/.config/slop_ninja_validator/NAME.toml
# --from DIR       install slop_ninja_validator and slop-ninja-journal from DIR
#                  into ~/.local/bin. DIR/SHA256SUMS must list both binaries,
#                  and both must match it. Verify the release signature over
#                  SHA256SUMS first (no release key is published yet; see the
#                  validator guide).
# --unverified     install from DIR without SHA256SUMS (a local build)
# --config FILE    install FILE as the instance configuration (mode 600) when
#                  none exists; an existing configuration is never replaced
# --init           create the journal if the configuration names none yet
# --enable         enable slop_ninja_validator-dispatch@NAME (starts at boot)
# --start          with --enable, start it now
# --dry-run        print every action instead of performing it
#
# The units are user units: lingering (loginctl enable-linger) keeps them
# running without a login session and starts them at boot. Linux only: macOS
# is not a declared platform, so no launchd job is provided.
#
# Nothing here reads a key, signs or contacts the chain except `init`, which
# reads only the hotkey's public companion. Re-running is safe.
set -euo pipefail

instance="" from="" config_src="" init=0 enable=0 start=0 dry=0 unverified=0
while (($#)); do
  case "$1" in
    --instance) instance="${2:?--instance needs a name}"; shift 2 ;;
    --from) from="${2:?--from needs a directory}"; shift 2 ;;
    --unverified) unverified=1; shift ;;
    --config) config_src="${2:?--config needs a file}"; shift 2 ;;
    --init) init=1; shift ;;
    --enable) enable=1; shift ;;
    --start) start=1; shift ;;
    --dry-run) dry=1; shift ;;
    -h|--help) sed -n '2,26p' "$0"; exit 0 ;;
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
[[ "$(uname -s)" == Linux ]] || die "only Linux is a declared platform; no launchd job is provided"

systemctl_cmd=${SYSTEMCTL:-systemctl}
loginctl_cmd=${LOGINCTL:-loginctl}
user=${USER:-$(id -un)}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
bin_dir="$HOME/.local/bin"
config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/slop_ninja_validator"
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
state_dir="${XDG_STATE_HOME:-$HOME/.local/state}/slop_ninja_validator"
config="$config_dir/$instance.toml"
validator="$bin_dir/slop_ninja_validator"
unit="slop_ninja_validator-dispatch@$instance.service"

if ((dry == 0)); then
  command -v "$systemctl_cmd" >/dev/null || die "systemctl is not available"
fi

run install -d -m 0755 "$bin_dir"
run install -d -m 0700 "$config_dir" "$state_dir"
run install -d -m 0755 "$unit_dir"

if [[ -n "$from" ]]; then
  [[ -d "$from" ]] || die "--from $from is not a directory"
  if [[ -f "$from/SHA256SUMS" ]]; then
    # Check exactly the two installed binaries, also in a dry run (reading
    # only): each must have one line in SHA256SUMS, and its digest must match.
    for binary in slop_ninja_validator slop-ninja-journal; do
      line=$(grep -E "^[0-9a-f]{64} [ *]$binary\$" "$from/SHA256SUMS" || true)
      [[ -n "$line" && "$line" != *$'\n'* ]] || die "SHA256SUMS in $from must list $binary exactly once"
      [[ "${line:0:64}" == "$(cd "$from" && sha256sum -- "$binary" | cut -c1-64)" ]] \
        || die "$from/$binary does not match SHA256SUMS"
    done
  elif ((unverified)); then
    echo "install.sh: installing from $from without SHA256SUMS (--unverified)" >&2
  else
    die "no SHA256SUMS in $from; pass --unverified only for binaries you built yourself"
  fi
  for binary in slop_ninja_validator slop-ninja-journal; do
    [[ -f "$from/$binary" ]] || die "$from/$binary is missing"
    run install -m 0755 "$from/$binary" "$bin_dir/$binary"
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

linger=no
if ((dry == 0)); then
  linger=$("$loginctl_cmd" show-user "$user" --property=Linger --value 2>/dev/null || echo no)
fi
[[ "$linger" == yes ]] || run "$loginctl_cmd" enable-linger "$user"
run "$systemctl_cmd" --user daemon-reload

if ((enable)); then
  if ((start)); then
    run "$systemctl_cmd" --user enable --now "$unit"
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

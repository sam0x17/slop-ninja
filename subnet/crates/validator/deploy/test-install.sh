#!/usr/bin/env bash
# Tests install.sh's release checks against a scratch release signed by a
# throwaway key, with a stub validator, systemctl and loginctl: no chain call,
# no unit installed or started, nothing written outside the scratch directory.
#
#   deploy/test-install.sh <empty scratch directory>
#
# Checks: syntax; the pinned release key is the one on the testnet landing
# page; a release with a good signature and matching digests installs, and a
# dry run of it installs nothing; refused, with nothing installed: a signature
# by another key (also when the release carries an allowed_signers trusting
# that key), a SHA256SUMS changed after signing, a signature under another
# namespace or principal, a malformed signature, a missing signature, a
# signature without SHA256SUMS, a binary changed after signing, a binary
# listed twice or not at all; --unverified installs a local build without
# SHA256SUMS and a digest list without a signature, but still refuses a bad
# signature or digest it is given; a binary missing from the release; a
# signed binary that does not run on this host (also in a dry run); an
# ssh-keygen without -Y is reported as the tool's fault. Linux only, like
# the installer; needs OpenSSH 8.1 or later.
set -euo pipefail

scratch="${1:?usage: test-install.sh <empty scratch directory>}"
[[ -d "$scratch" && -z "$(ls -A "$scratch")" ]] || { echo "test-install.sh: $scratch must be an empty directory" >&2; exit 2; }
scratch=$(cd "$scratch" && pwd)
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../../../.." && pwd)
principal=slop-ninja-testnet-release
failures=0 passed=0

fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
pass() { passed=$((passed + 1)); }

bash -n "$here/install.sh" && pass || fail "install.sh does not parse"

# The pinned key is the landing page's allowed-signers line, and its
# fingerprint is the published one.
pinned=$(cat "$here/release-allowed_signers")
if grep -qxF -- "$pinned" "$repo/docs/testnet/README.md"; then pass; else fail "release-allowed_signers is not the landing page's allowed-signers line"; fi
[[ "$pinned" == "$principal namespaces=\"file\" ssh-ed25519 "* ]] && pass || fail "release-allowed_signers names another principal or namespace"
fingerprint=$(ssh-keygen -l -f <(cut -d' ' -f3- <<<"$pinned") | cut -d' ' -f2)
if grep -qF -- "\`$fingerprint\`" "$repo/docs/testnet/README.md"; then pass; else fail "the pinned key's fingerprint $fingerprint is not on the landing page"; fi

# A copy of the deploy directory whose pinned key is a throwaway test key.
deploy="$scratch/checkout/crates/validator/deploy"
mkdir -p "$deploy"
cp "$here/install.sh" "$here/slop_ninja_validator@.service" "$here/slop_ninja_validator-dispatch@.service" "$deploy/"
cp "$here/../validator.example.toml" "$deploy/../validator.example.toml"
ssh-keygen -q -t ed25519 -N '' -C test-release -f "$scratch/release-key"
ssh-keygen -q -t ed25519 -N '' -C test-other -f "$scratch/other-key"
printf '%s namespaces="file" %s\n' "$principal" "$(cat "$scratch/release-key.pub")" >"$deploy/release-allowed_signers"

# Stubs: the validator answers check-config with an existing journal; the
# journal tool is inert; systemctl and loginctl record their arguments.
stubs="$scratch/stubs"
mkdir -p "$stubs"
cat >"$stubs/systemctl" <<EOF
#!/usr/bin/env bash
printf 'systemctl %s\n' "\$*" >>"$scratch/calls.log"
EOF
cat >"$stubs/loginctl" <<EOF
#!/usr/bin/env bash
printf 'loginctl %s\n' "\$*" >>"$scratch/calls.log"
[[ "\$1" != show-user ]] || echo yes
EOF
chmod 0755 "$stubs/systemctl" "$stubs/loginctl"
cat >"$scratch/config.toml" <<'EOF'
# test configuration; the stub validator does not read it
EOF

# make_release DIR [MARK]: two stub binaries, SHA256SUMS over them, signed by
# the test release key under namespace "file".
make_release() {
  local dir=$1 mark=${2:-1}
  mkdir -p "$dir"
  cat >"$dir/slop_ninja_validator" <<EOF
#!/usr/bin/env bash
# stub validator $mark
case " \$* " in
  *" check-config "*) echo '{"journal_exists": true}' ;;
  *) exit 0 ;;
esac
EOF
  printf '#!/usr/bin/env bash\n# stub journal %s\nexit 0\n' "$mark" >"$dir/slop-ninja-journal"
  printf 'unrelated release file\n' >"$dir/testnet-trust.json"
  (cd "$dir" && sha256sum slop_ninja_validator slop-ninja-journal testnet-trust.json >SHA256SUMS)
  sign "$dir" "$scratch/release-key" file
}

# sign DIR KEY NAMESPACE: (re)write DIR/SHA256SUMS.sig.
sign() {
  rm -f "$1/SHA256SUMS.sig"
  ssh-keygen -q -Y sign -f "$2" -n "$3" "$1/SHA256SUMS" 2>/dev/null
}

case_no=0
# run_install NAME ARGS...: runs the installer in a fresh home; sets status,
# out (stdout and stderr) and home.
run_install() {
  case_no=$((case_no + 1))
  home="$scratch/home-$case_no"
  mkdir -p "$home"
  status=0
  out=$(env -i PATH="$PATH" HOME="$home" USER=tester TMPDIR="$scratch/tmp" \
    SYSTEMCTL="$stubs/systemctl" LOGINCTL="$stubs/loginctl" \
    bash "$deploy/install.sh" --instance test "$@" 2>&1) || status=$?
}
mkdir -p "$scratch/tmp"

installed() { [[ -e "$home/.local/bin/slop_ninja_validator" || -e "$home/.local/bin/slop-ninja-journal" ]]; }

# expect_refused NAME PATTERN ARGS...: exit 2, PATTERN in the output, nothing
# installed and no directory created in the home.
expect_refused() {
  local name=$1 pattern=$2; shift 2
  run_install "$@"
  if [[ $status -ne 2 ]]; then fail "$name: exit $status, expected 2: $out"; return; fi
  if ! grep -qF -- "$pattern" <<<"$out"; then fail "$name: output lacks '$pattern': $out"; return; fi
  if installed || [[ -n "$(ls -A "$home")" ]]; then fail "$name: something was installed"; return; fi
  pass
}

# expect_installed NAME RELEASE ARGS...: exit 0 and both binaries installed,
# byte-identical to RELEASE's.
expect_installed() {
  local name=$1 release=$2; shift 2
  local want=$release
  run_install --from "$release" --config "$scratch/config.toml" "$@"
  if [[ $status -ne 0 ]]; then fail "$name: exit $status: $out"; return; fi
  for binary in slop_ninja_validator slop-ninja-journal; do
    if ! cmp -s "$want/$binary" "$home/.local/bin/$binary"; then fail "$name: $binary not installed from $want"; return; fi
    [[ -x "$home/.local/bin/$binary" ]] || { fail "$name: $binary is not executable"; return; }
  done
  pass
}

# A good release installs and reports the good signature.
good="$scratch/good"
make_release "$good"
expect_installed "good signature" "$good"
grep -qF "Good \"file\" signature for $principal" <<<"$out" && pass || fail "good signature: no Good line: $out"
grep -qF 'systemctl --user daemon-reload' "$scratch/calls.log" && pass || fail "good signature: no daemon-reload"
[[ -z "$(ls -A "$scratch/tmp")" ]] && pass || fail "good signature: staging copies left behind"

# A dry run checks the signature, prints the install from the release
# directory and installs nothing.
run_install --from "$good" --config "$scratch/config.toml" --dry-run
if [[ $status -eq 0 ]] && grep -qF "Good \"file\" signature" <<<"$out" \
    && grep -qF "+ install -m 0755 $good/slop_ninja_validator" <<<"$out" && ! installed \
    && [[ -z "$(ls -A "$home")" ]]; then pass; else fail "dry run: exit $status: $out"; fi

# Signed by another key.
other="$scratch/other"
make_release "$other"
sign "$other" "$scratch/other-key" file
expect_refused "other key" "not a valid release signature" --from "$other"

# The release's own allowed_signers trusting that key is ignored.
printf '%s namespaces="file" %s\n' "$principal" "$(cat "$scratch/other-key.pub")" >"$other/allowed_signers"
cp "$scratch/other-key.pub" "$other/slop-ninja-release-testnet.pub"
expect_refused "release's own allowed_signers" "not a valid release signature" --from "$other"

# SHA256SUMS changed after signing: here re-listing a replaced binary.
swapped="$scratch/swapped"
make_release "$swapped"
printf '#!/usr/bin/env bash\necho replaced\n' >"$swapped/slop-ninja-journal"
(cd "$swapped" && sha256sum slop_ninja_validator slop-ninja-journal >SHA256SUMS)
expect_refused "SHA256SUMS changed after signing" "not a valid release signature" --from "$swapped"
expect_refused "SHA256SUMS changed after signing, --unverified" "not a valid release signature" --from "$swapped" --unverified

# Another namespace.
namespace="$scratch/namespace"
make_release "$namespace"
sign "$namespace" "$scratch/release-key" git
expect_refused "other namespace" "not a valid release signature" --from "$namespace"

# Another principal: the pinned key under another name does not count.
principal_dir="$scratch/principal"
make_release "$principal_dir"
cp -r "$scratch/checkout" "$scratch/checkout-renamed"
deploy_saved=$deploy
deploy="$scratch/checkout-renamed/crates/validator/deploy"
printf 'someone-else namespaces="file" %s\n' "$(cat "$scratch/release-key.pub")" >"$deploy/release-allowed_signers"
expect_refused "other principal" "not a valid release signature" --from "$principal_dir"
rm -f "$deploy/release-allowed_signers"
expect_refused "pinned key missing" "pinned release key" --from "$principal_dir"
deploy=$deploy_saved

# A malformed signature.
garbage="$scratch/garbage"
make_release "$garbage"
printf 'not a signature\n' >"$garbage/SHA256SUMS.sig"
expect_refused "malformed signature" "not a valid release signature" --from "$garbage"
expect_refused "malformed signature, --unverified" "not a valid release signature" --from "$garbage" --unverified

# No signature: refused unless --unverified, which still checks the digests.
unsigned="$scratch/unsigned"
make_release "$unsigned"
rm -f "$unsigned/SHA256SUMS.sig"
expect_refused "no signature" "no SHA256SUMS.sig" --from "$unsigned"
expect_installed "no signature, --unverified" "$unsigned" --unverified
grep -qF "without a release signature (--unverified)" <<<"$out" && pass || fail "no signature, --unverified: no notice: $out"
printf '#!/usr/bin/env bash\necho tampered\n' >"$unsigned/slop_ninja_validator"
expect_refused "no signature, --unverified, digest mismatch" "does not match SHA256SUMS" --from "$unsigned" --unverified

# A signature without SHA256SUMS.
nosums="$scratch/nosums"
make_release "$nosums"
rm -f "$nosums/SHA256SUMS"
expect_refused "signature without SHA256SUMS" "has SHA256SUMS.sig but no SHA256SUMS" --from "$nosums" --unverified

# A good signature over SHA256SUMS, but a binary changed afterwards.
changed="$scratch/changed"
make_release "$changed"
printf '#!/usr/bin/env bash\necho changed\n' >"$changed/slop_ninja_validator"
expect_refused "binary changed after signing" "does not match SHA256SUMS" --from "$changed"

# A good signature over a SHA256SUMS that lists a binary twice or not at all.
twice="$scratch/twice"
make_release "$twice"
(cd "$twice" && sha256sum slop_ninja_validator slop_ninja_validator slop-ninja-journal >SHA256SUMS)
sign "$twice" "$scratch/release-key" file
expect_refused "binary listed twice" "must list slop_ninja_validator exactly once" --from "$twice"
missing="$scratch/missing"
make_release "$missing"
(cd "$missing" && sha256sum slop_ninja_validator >SHA256SUMS)
sign "$missing" "$scratch/release-key" file
expect_refused "binary not listed" "must list slop-ninja-journal exactly once" --from "$missing"

# A local build: no SHA256SUMS, no signature.
local_build="$scratch/local"
make_release "$local_build"
rm -f "$local_build/SHA256SUMS" "$local_build/SHA256SUMS.sig"
expect_refused "local build" "no SHA256SUMS.sig" --from "$local_build"
expect_installed "local build, --unverified" "$local_build" --unverified
grep -qF "without SHA256SUMS (--unverified)" <<<"$out" && pass || fail "local build, --unverified: no notice: $out"

# A binary missing from the release.
absent="$scratch/absent"
make_release "$absent"
rm -f "$absent/slop-ninja-journal"
expect_refused "binary missing" "slop-ninja-journal is missing" --from "$absent"

# A correctly signed release whose validator cannot run on this host (as an
# exec format error or a missing C library symbol would): nothing replaced.
foreign="$scratch/foreign"
make_release "$foreign"
printf '#!/usr/bin/env bash\necho "cannot execute binary file" >&2\nexit 126\n' >"$foreign/slop_ninja_validator"
(cd "$foreign" && sha256sum slop_ninja_validator slop-ninja-journal testnet-trust.json >SHA256SUMS)
sign "$foreign" "$scratch/release-key" file
expect_refused "binary does not run here" "slop_ninja_validator does not run on this host" --from "$foreign"
expect_refused "binary does not run here, dry run" "slop_ninja_validator does not run on this host" --from "$foreign" --dry-run

# An ssh-keygen without -Y (before OpenSSH 8.1): the tool is named, not the
# release.
old_tools="$scratch/old-tools"
mkdir -p "$old_tools"
printf '#!/usr/bin/env bash\necho "ssh-keygen: illegal option -- Y" >&2\nexit 1\n' >"$old_tools/ssh-keygen"
chmod 0755 "$old_tools/ssh-keygen"
path_saved=$PATH
PATH="$old_tools:$PATH"
expect_refused "ssh-keygen without -Y" "this ssh-keygen cannot verify signatures" --from "$good"
PATH=$path_saved

echo "test-install.sh: $passed passed, $failures failed"
((failures == 0))

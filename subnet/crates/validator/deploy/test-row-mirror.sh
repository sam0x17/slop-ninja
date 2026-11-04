#!/usr/bin/env bash
# test-row-mirror.sh <empty scratch dir>: checks row-mirror.sh against a stub
# `wrangler` serving a fixture bucket from the scratch directory. No network.
set -euo pipefail
[ $# -eq 1 ] || { echo "usage: $0 <empty scratch dir>" >&2; exit 2; }
root=$1
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$root/bucket/authorizations" "$root/bin"
pass=0
ok() { pass=$((pass + 1)); }
fail() { echo "FAIL: $*" >&2; exit 1; }

cat > "$root/bin/wrangler" <<EOF
#!/usr/bin/env bash
# r2 object get <bucket>/<key> --remote --file <out>
[ "\$1 \$2 \$3" = "r2 object get" ] || exit 9
key=\${4#*/}
[ "\$5" = "--remote" ] && [ "\$6" = "--file" ] || exit 9
[ -f "$root/bucket/\$key" ] || exit 1
cp "$root/bucket/\$key" "\$7"
echo "\$key" >> "$root/reads"
EOF
chmod +x "$root/bin/wrangler"
export WRANGLER="$root/bin/wrangler"

object() {
  printf '%s' "$1" > "$root/o"
  local sha
  sha=$(sha256sum < "$root/o" | cut -d' ' -f1)
  mv "$root/o" "$root/bucket/authorizations/$sha.signed"
  echo "$sha"
}
pointer() {
  local body='{"schema":"slop_ninja.public.authorization/v2","kind":"launch"'
  body="$body,\"authorization\":{\"path\":\"authorizations/$1.signed\",\"sha256\":\"$1\",\"bytes\":1}"
  [ -n "${2:-}" ] && body="$body,\"row\":{\"path\":\"authorizations/$2.signed\",\"sha256\":\"$2\",\"bytes\":1}"
  printf '%s}' "$body" > "$root/bucket/authorizations/current.json"
}

m="$root/mirror"
a=$(object auth-1); r=$(object row-1)
pointer "$a" "$r"
bash "$here/row-mirror.sh" "$m" >/dev/null
cmp -s "$m/v1/authorizations/current.json" "$root/bucket/authorizations/current.json" || fail "pointer copied"; ok
[ "$(cat "$m/authorizations/$a.signed")" = auth-1 ] && [ "$(cat "$m/authorizations/$r.signed")" = row-1 ] || fail "objects copied"; ok
[ "$(stat -c %a "$m/v1/authorizations/current.json")" = 600 ] || fail "private modes"; ok
# Unchanged objects are not read again; a new row replaces the old one.
: > "$root/reads"
r2=$(object row-2)
pointer "$a" "$r2"
bash "$here/row-mirror.sh" "$m" >/dev/null
! grep -q "$a" "$root/reads" || fail "an unchanged object was read again"; ok
[ -f "$m/authorizations/$r2.signed" ] && [ ! -e "$m/authorizations/$r.signed" ] || fail "old row removed"; ok
# A served object that does not match its name: nothing changes.
before=$(cat "$m/v1/authorizations/current.json")
r3=$(object row-3)
printf 'tampered' > "$root/bucket/authorizations/$r3.signed"
pointer "$a" "$r3"
if bash "$here/row-mirror.sh" "$m" >/dev/null 2>&1; then fail "a tampered object was accepted"; fi
[ "$(cat "$m/v1/authorizations/current.json")" = "$before" ] || fail "the pointer moved past a bad object"; ok
[ ! -e "$m/authorizations/$r3.signed" ] || fail "a bad object was kept"; ok
# A missing object: the pointer stays.
pointer "$a" "$(printf '%064d' 7)"
if bash "$here/row-mirror.sh" "$m" >/dev/null 2>&1; then fail "a missing object was accepted"; fi
[ "$(cat "$m/v1/authorizations/current.json")" = "$before" ] || fail "the pointer moved past a missing object"; ok
# Relative directories are refused.
if bash "$here/row-mirror.sh" relative >/dev/null 2>&1; then fail "a relative directory was accepted"; fi; ok
[ -z "$(ls -A "$m" | grep '^\.row-mirror' || true)" ] || fail "temporary directory left"; ok
echo "row-mirror tests: $pass passed"

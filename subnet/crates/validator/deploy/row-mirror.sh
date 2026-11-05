#!/usr/bin/env bash
# row-mirror.sh <mirror dir>
#
# Copies the coordinator's private authorization pointer and every object it
# names (the authorization, any renew-by hint refresh, the revocation set and
# the coordinator's signed row) from the receipts bucket into a local mirror
# of the public layout, for a validator whose `[fetch] base_url` is
# `file://<mirror dir>/` while the public host is not deployed:
#
#   <mirror dir>/authorizations/<sha256>.signed   each named object
#   <mirror dir>/v1/authorizations/current.json   the pointer, written last
#
# The pointer's object paths (`authorizations/<sha256>.signed`) resolve
# under the mirror directory as they are. Each object is checked against the
# SHA-256 its name carries and written through a temporary name renamed into
# place, every object before the pointer, so the validator never reads a
# pointer whose objects are missing. The validator verifies every object
# again (digest, length, signature under its pinned keys), so a mirror can
# make a fetch fail but never change what is verified.
#
# Reads only, with the operator's own Cloudflare login (`wrangler r2 object
# get --remote`); writes nothing remote. Objects the pointer no longer names
# are removed from the mirror after the pointer moved on. A failed read
# exits 1 and copies wrangler's output into this script's standard error.
#
# Environment: ROW_MIRROR_BUCKET (default receipts-bucket-testnet),
# WRANGLER (default wrangler).
set -euo pipefail
umask 077

[ $# -eq 1 ] || { echo "usage: $0 <absolute mirror dir>" >&2; exit 2; }
dir=$1
case $dir in /*) ;; *) echo "the mirror directory must be absolute" >&2; exit 2 ;; esac
bucket=${ROW_MIRROR_BUCKET:-receipts-bucket-testnet}
wrangler=${WRANGLER:-wrangler}
export WRANGLER_WRITE_LOGS=false WRANGLER_SEND_METRICS=false

mkdir -p "$dir/authorizations" "$dir/v1/authorizations"
tmp=$(mktemp -d "$dir/.row-mirror.XXXXXX")
trap 'rm -rf -- "$tmp"' EXIT

# A failed read reports wrangler's exit status and its whole output (standard
# output and error, each line prefixed) on standard error, so the timer's log
# keeps the cause, for example a missing or expired login. A successful
# read's progress output is dropped.
get() {
  local rc=0
  "$wrangler" r2 object get "$bucket/$1" --remote --file "$2" >"$tmp/wrangler.log" 2>&1 || rc=$?
  [ "$rc" -eq 0 ] && return 0
  echo "row-mirror: read of $1 failed (wrangler exit $rc)" >&2
  if [ -s "$tmp/wrangler.log" ]; then
    sed 's/^/row-mirror: wrangler: /' "$tmp/wrangler.log" >&2
  else
    echo "row-mirror: wrangler: (no output)" >&2
  fi
  return 1
}

get authorizations/current.json "$tmp/pointer.json"
names=$(grep -oE 'authorizations/[0-9a-f]{64}\.signed' "$tmp/pointer.json" | sort -u || true)
[ -n "$names" ] || { echo "row-mirror: the pointer names no object" >&2; exit 1; }

for name in $names; do
  sha=${name#authorizations/}
  sha=${sha%.signed}
  if [ -f "$dir/$name" ] && [ "$(sha256sum < "$dir/$name" | cut -d' ' -f1)" = "$sha" ]; then
    continue
  fi
  get "$name" "$tmp/object"
  [ "$(sha256sum < "$tmp/object" | cut -d' ' -f1)" = "$sha" ] || {
    echo "row-mirror: $name does not match its digest" >&2
    exit 1
  }
  mv -f -- "$tmp/object" "$dir/$name"
done
sync -f "$dir/authorizations" 2>/dev/null || sync
mv -f -- "$tmp/pointer.json" "$dir/v1/authorizations/current.json"
sync -f "$dir/v1/authorizations" 2>/dev/null || sync

# Objects the pointer no longer names.
for f in "$dir"/authorizations/*.signed; do
  [ -e "$f" ] || continue
  case " $(echo $names) " in *" authorizations/${f##*/} "*) ;; *) rm -f -- "$f" ;; esac
done
echo "row-mirror: $(echo "$names" | wc -l) objects, pointer current"

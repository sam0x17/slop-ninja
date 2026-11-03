#!/bin/bash
# OLMo-2 qualification for the Tier C seal (protocol-v1.json seal.binds:
# "download to Test-only directory, pinned sha256 check, load in pinned
# runtime, 20-token smoke on fixed synthetic prompt; failure blocks the seal").
# Run as the LAST step before the seal and never after it.
#   systemd-run --user --slice=slop-a-comp.slice --unit=slop-a-comp-tierc-olmo2-qualify \
#     -p MemoryMax=32G -p MemorySwapMax=0 --working-directory=. \
#     /path/to/tools/a-comp-tierc/olmo2-qualify.sh
# Exit 0 and receipt.json "pass": true only when both files download with the
# pinned size and sha256 and both load and return exactly 20 tokens. Any other
# outcome writes "pass": false and SEAL-BLOCKED, and exits nonzero.
set -uo pipefail
umask 027
export PATH=~/.linuxbrew/bin:/usr/local/bin:/usr/bin:/bin
HERE=$(cd "$(dirname "$0")" && pwd)
S=data/grammar-private/a-competitive-20261027
PROTO=$S/protocol-v1.json
PROTO_SHA=9ca96f5228c8a40671bb7c2c64d125cb587c7bfe534c3974cb0bf502966c8fab
G=$S/generators/test-only/olmo2
Q=$G/qualification
mkdir -p "$Q"
log(){ echo "$(date -Is) $*" | tee -a "$Q/qualify.log"; }
block(){ log "BLOCKED: $*"; echo "$*" > "$Q/SEAL-BLOCKED"; exit 1; }
[[ ! -e $S/tier-c/seal/SEAL.json ]] || { log "refused: the seal is already written; qualification belongs before it"; exit 2; }
[[ $(sha256sum "$PROTO" | cut -c1-64) == "$PROTO_SHA" ]] || block "protocol-v1.json sha256 differs from the pinned protocol"
rm -f "$Q/SEAL-BLOCKED" "$Q/receipt.json"
exec 9>data/locks/bigmem.lock
log "waiting for the big-memory lock"; flock 9
for kind in instruct base; do
  repo=$(jq -r ".generators.test_only.$kind.repo" "$PROTO"); rev=$(jq -r ".generators.test_only.$kind.revision" "$PROTO")
  file=$(jq -r ".generators.test_only.$kind.filename" "$PROTO"); size=$(jq -r ".generators.test_only.$kind.size_bytes" "$PROTO")
  sha=$(jq -r ".generators.test_only.$kind.sha256" "$PROTO")
  f=$G/$file
  if [[ -f $f.verified ]] && (cd "$G" && sha256sum -c --quiet "$file.verified"); then log "$kind already downloaded and verified"; continue; fi
  # pause (do not fail) while / is below 450 GB free plus this file
  need=$(( 450 * 1024 * 1024 * 1024 + size ))
  while (( $(df --output=avail -B1 / | tail -1) < need )); do log "disk below 450 GB + $file; paused"; sleep 300; done
  url="https://huggingface.co/$repo/resolve/$rev/$file"
  log "download $kind $url"
  ok=0
  for try in 1 2 3 4 5; do
    curl -sS -L --fail --retry 5 --retry-delay 10 --connect-timeout 30 -C - -o "$f.part" "$url" >> "$Q/download-$kind.log" 2>&1 && { ok=1; break; }
    log "curl failed try=$try"
  done
  [[ $ok == 1 ]] || block "$kind download failed"
  got=$(stat -c %s "$f.part")
  [[ $got == "$size" ]] || block "$kind size $got differs from the pinned $size"
  h=$(sha256sum "$f.part" | cut -c1-64)
  [[ $h == "$sha" ]] || block "$kind sha256 $h differs from the pinned $sha"
  mv "$f.part" "$f"
  printf '%s  %s\n' "$h" "$file" > "$f.verified"
  jq -n --arg repo "$repo" --arg rev "$rev" --arg file "$file" --arg url "$url" --argjson size "$size" --arg sha "$h" --arg at "$(date -Is)" \
    '{repo:$repo,revision:$rev,filename:$file,url:$url,size_bytes:$size,sha256:$sha,verified_at:$at}' > "$Q/download-$kind.json"
  log "$kind verified $h"
done
"$HERE/../gpu-lock.sh" --wait --exit-file "$Q/gpu.exit" -- "$HERE/olmo2-qualify-inner.sh" "$Q"
rc=$(cat "$Q/gpu.exit" 2>/dev/null || echo 70)
[[ $rc == 0 && -f $Q/receipt.json && $(jq -r .pass "$Q/receipt.json") == true ]] || block "load or smoke failed (gpu step rc=$rc); see $Q"
log "PASS receipt sha256 $(sha256sum "$Q/receipt.json" | cut -c1-64)"

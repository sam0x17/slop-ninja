#!/bin/bash
# Post-seal Tier C pipeline up to the Pangram Test dispatch (NOT run before the seal):
#   1 acquire     Test humans (W1 test-acquire.sh)
#   2 generate    open-model + OLMo-2 Test positives (test-gen-driver.sh, GPU) and, in parallel,
#                 frontier Test positives (frontier-test-driver.sh, CPU and network)
#   3 bundle      Test bundle (slop_ninja_a_assemble build, config test-bundle-v1.json)
#   4 audit       overlap_checks "after Test acquisition": audit-candidates -> scan-overlap against the
#                 registry -> audit-receipt; any conflict excludes the whole source group and the bundle
#                 is rebuilt once with the exclusions (config test-bundle-v1-audited.json)
#   5 slices      realised slice minimums from pipeline counts (a_comp_tierc_slice_minimums)
#   6 subsample   Pangram Test subsample, n_test 2950 (a_comp_pangram_test_subsample)
#   7 collections Pangram collection allow-list from the bundle's primary collections
#   8 prepare     collector prepare test ... --tail-units 400 (offline; no request)
#   STOP: dispatch, A scoring (test-score-driver.sh) and the evaluation are run by the coordinator.
#
# Usage: test-postseal.sh SEAL_SHA256 FRONTIER_VOICES FRONTIER_FROZEN
#   every binary, script, config and voice file below must be bound in SEAL.json (checked first).
# Durable start (coordinator):
#   systemd-run --user --slice=slop-a-comp.slice --unit=slop-a-comp-tierc-postseal -p MemoryMax=2G \
#     -p MemorySwapMax=0 /bin/bash .../tools/a-comp-tierc/test-postseal.sh SEAL_SHA VOICES FROZEN
# Each step runs as its own durable unit and writes tier-c/test/jobs/<step>.exit; a step whose exit
# file holds 0 is skipped on a rerun; tier-c/test/STATUS.json is rewritten after every step.
# Nothing here prints, samples or scores prose, and nothing sends a request to any provider.
set -uo pipefail
umask 027
export PATH=~/.cargo/bin:/usr/local/bin:/usr/bin:/bin
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=.
S=$REPO/data/grammar-private/a-competitive-20261027
D=$S/data
T=$S/tier-c
W1=$T/workstreams/w1-acquire
W5=$T/workstreams/w5-bundle
SEAL=$T/seal/SEAL.json
J=$T/test/jobs
SEAL_SHA=${1:?SEAL_SHA256}; VOICES=${2:?FRONTIER_VOICES}; FROZEN=${3:?FRONTIER_FROZEN}
# ---- pinned tools (each bound in the seal)
ASM=$T/bin/slop_ninja_a_assemble-8bd827fe
SLICES=$T/bin/a_comp_tierc_slice_minimums-8bd827fe
SUBSAMPLE=$T/bin/a_comp_pangram_test_subsample-9f884629
BULK=$T/bin/a_comp_pangram_bulk-d919cac8
POSITIVES=$T/bin/slop_ninja_a_positives-f2bba965
FRONTIER=$T/bin/a_frontier_positives-eca0eac4
SCAN=$T/bin/slop_ninja_a_pools-ec9a5c2d
REGISTRY=$W1/registries/overlap-registry-preseal-v1.json
CONFIG=$W5/configs/test-bundle-v1.json
PROTOCOL=$S/protocol-v1.json
PROTOCOL_SHA=9ca96f5228c8a40671bb7c2c64d125cb587c7bfe534c3974cb0bf502966c8fab
N_TEST=2950
TAIL_UNITS=400
HELD_OUT=olmo2
TEST_HUMANS=$D/eval-humans-test/test
LOCAL=$D/positives-test/final/test/records.jsonl
FEXPORT=$S/frontier/test-v1/export
BUNDLES=$T/test/bundles
mkdir -p "$J"
sha(){ sha256sum "$1" | cut -c1-64; }
log(){ echo "$(date -Is) $*" | tee -a "$J/postseal.log" >&2; }
die(){ log "REFUSED/STOPPED: $*"; status "stopped: $*"; exit 2; }
status(){
  local steps=()
  for s in acquire gen-local gen-frontier bundle audit-candidates audit-scan audit-receipt bundle-audited slices subsample collections prepare; do
    steps+=("$(jq -n --arg s $s --arg e "$(cat $J/$s.exit 2>/dev/null || echo -)" '{($s): $e}')")
  done
  printf '%s\n' "${steps[@]}" | jq -s --arg n "$1" --arg t "$(date -u +%FT%TZ)" --arg seal "$SEAL_SHA" \
    '{schema: "slop_ninja_a_comp_tierc_test_pipeline_status_v1", updated_utc: $t, seal_sha256: $seal, note: $n,
      exit_files: add, rule: "a step is done only when its exit file holds 0; rerun this script to resume"}' > "$T/test/STATUS.json.tmp" \
    && mv "$T/test/STATUS.json.tmp" "$T/test/STATUS.json"
}
done_(){ [[ $(cat "$J/$1.exit" 2>/dev/null) == 0 ]]; }
# unit STEP MEM BIGMEM(0/1) CMD...: one durable unit per step; the exit file is written by the unit.
unit(){
  local step=$1 mem=$2 big=$3; shift 3
  local cmd; printf -v cmd '%q ' "$@"
  [[ $big == 1 ]] && cmd="flock $REPO/data/locks/bigmem.lock $cmd"
  log "step $step start"
  systemd-run --user --wait --collect --slice=slop-a-comp.slice --unit=slop-a-comp-tierc-ps-$step -p MemoryMax=$mem -p MemorySwapMax=0 \
    /bin/bash -c "$cmd > $J/$step.stdout 2> $J/$step.stderr; echo \$? > $J/$step.exit" > /dev/null 2>&1
  log "step $step exit $(cat $J/$step.exit 2>/dev/null || echo none)"
  status "after $step"
  done_ "$step" || die "step $step failed (see $J/$step.stderr)"
}

# ---- refusals
# Seal addendum (tier-c/seal/ADDENDUM-1.json, written once after the seal and before any frontier
# request): a file is bound when its sha256 is in SEAL.json or in the addendum's "bound" list, and the
# addendum names this seal.
ADD=$T/seal/ADDENDUM-1.json
bound(){ local h; h=$(sha "$1"); grep -q "$h" "$SEAL" && return 0
  [[ -f $ADD && $(jq -r .schema "$ADD") == slop_ninja_a_comp_tierc_seal_addendum_v1 && $(jq -r .seal_sha256 "$ADD") == "$SEAL_SHA" ]] || return 1
  jq -e --arg h "$h" 'any(.bound[]; .sha256 == $h)' "$ADD" > /dev/null; }
[[ -f $SEAL ]] || die "seal not written ($SEAL): Test is sealed"
[[ $(sha "$SEAL") == "$SEAL_SHA" ]] || die "SEAL.json sha256 differs from the argument"
[[ $(jq -r .sealed "$SEAL") == true ]] || die "SEAL.json does not say sealed"
[[ $(sha "$PROTOCOL") == "$PROTOCOL_SHA" ]] || die "protocol file changed"
for f in "$0" "$ASM" "$SLICES" "$SUBSAMPLE" "$BULK" "$POSITIVES" "$FRONTIER" "$SCAN" "$REGISTRY" "$CONFIG" "$VOICES" "$FROZEN" \
         "$W1/jobs/test-acquire.sh" "$HERE/test-gen-driver.sh" "$HERE/test-gen-one.sh" "$HERE/frontier-test-driver.sh" "$HERE/frontier-route-driver.sh"; do
  [[ -f $f ]] || die "missing $f"
  bound "$f" || die "$f ($(sha "$f")) is not bound in SEAL.json or the seal addendum"
done
for b in "$ASM" "$SLICES" "$SUBSAMPLE" "$BULK"; do grep -q "^$(sha "$b") " "$T/bin/SHA256SUMS" || die "$b not in tier-c/bin/SHA256SUMS"; done
SEALARGS=(--seal "$SEAL" --seal-sha256 "$SEAL_SHA")
status "started"

# 1 acquisition (W1 driver; its dedup stage is its own bigmem unit)
if ! done_ acquire; then
  unit acquire 8G 0 /bin/bash "$W1/jobs/test-acquire.sh" "$SEAL_SHA"
fi
[[ $(cat $D/eval-humans-test/jobs/test-acquire.exit 2>/dev/null) == 0 ]] || die "W1 acquisition did not finish"
(cd "$TEST_HUMANS" && sha256sum -c --quiet SHA256SUMS) || die "Test humans fail their SHA256SUMS"

# 2 generation: local (GPU, bigmem inside the driver) and frontier (CPU) in parallel
pids=()
if ! done_ gen-local; then
  ( unit gen-local 32G 0 /bin/bash "$HERE/test-gen-driver.sh" "$SEAL_SHA" "$POSITIVES" ) & pids+=($!)
fi
if ! done_ gen-frontier; then
  ( unit gen-frontier 5G 0 /bin/bash "$HERE/frontier-test-driver.sh" "$SEAL_SHA" "$FRONTIER" "$VOICES" "$FROZEN" ) & pids+=($!)
fi
for p in "${pids[@]}"; do wait "$p"; done
done_ gen-local && done_ gen-frontier || die "Test generation incomplete (rerun resumes the drivers)"
[[ -f $D/positives-test/driver/DONE && -f $S/frontier/test-v1/jobs/DONE ]] || die "a generation driver has no DONE marker"
(cd "$FEXPORT" && sha256sum -c --quiet SHA256SUMS) || die "frontier export fails its SHA256SUMS"

# 3 Test bundle
current(){ jq -r .current "$BUNDLES/CURRENT.json"; }
if ! done_ bundle; then
  unit bundle 6G 0 "$ASM" build --config "$CONFIG" --out "$BUNDLES" --commit 8bd827fe "${SEALARGS[@]}"
fi
B1=$(jq -r .dir "$J/bundle.stdout")
[[ -f $B1/test/rows.jsonl ]] || die "bundle has no test split"

# 4 audit after Test acquisition (generated text and prompts included)
A=$T/test/audit
FRECS=(); for f in $(ls "$FEXPORT"/*/records.jsonl | sort); do FRECS+=(--records "$f"); done
if ! done_ audit-candidates; then
  rm -rf "$A/cands" "$A/cands.tmp-audit"; mkdir -p "$A"
  unit audit-candidates 4G 0 "$ASM" audit-candidates --rows "$B1/test/rows.jsonl" --humans "$TEST_HUMANS/documents.jsonl" \
    --records "$LOCAL" "${FRECS[@]}" "${SEALARGS[@]}" --out "$A/cands"
fi
if ! done_ audit-scan; then
  rm -rf "$A/scan"
  unit audit-scan 6G 0 "$SCAN" scan-overlap --registry "$REGISTRY" --root "$REPO" --source "$A/cands/test-humans" \
    --source "$A/cands/test-bundle" --source "$A/cands/test-prompts" --out "$A/scan"
fi
if ! done_ audit-receipt; then
  rm -f "$A/overlap-receipt.json" "$A/exclusions.jsonl"
  unit audit-receipt 2G 0 "$ASM" audit-receipt --candidates "$A/cands" --scan "$A/scan" "${SEALARGS[@]}" \
    --receipt "$A/overlap-receipt.json" --exclusions "$A/exclusions.jsonl"
fi
BUNDLE=$B1/test
if [[ $(jq -r .any_conflict "$A/overlap-receipt.json") == true ]]; then
  CONFIG2=$T/test/test-bundle-v1-audited.json
  jq --arg x "$A/exclusions.jsonl" '. + {excluded_source_groups: [$x]}' "$CONFIG" > "$CONFIG2"
  if ! done_ bundle-audited; then
    unit bundle-audited 6G 0 "$ASM" build --config "$CONFIG2" --out "$BUNDLES" --commit 8bd827fe "${SEALARGS[@]}"
  fi
  BUNDLE=$(jq -r .dir "$J/bundle-audited.stdout")/test
  log "audit excluded $(wc -l < "$A/exclusions.jsonl") source groups; bundle rebuilt"
else
  echo 0 > "$J/bundle-audited.exit"
fi
[[ $(current)/test == "$BUNDLE" ]] || die "CURRENT.json does not name the final Test bundle"
ROWS=$BUNDLE/rows.jsonl

# 5 realised slice minimums (pipeline counts, before any Test scoring)
if ! done_ slices; then
  rm -f "$T/test/slice-minimums.json"
  unit slices 4G 0 "$SLICES" "${SEALARGS[@]}" --bundle "$BUNDLE" --held-out-generator "$HELD_OUT" --out "$T/test/slice-minimums.json"
fi

# 6 Pangram Test subsample
if ! done_ subsample; then
  rm -rf "$T/test/pangram-subsample"
  unit subsample 4G 0 "$SUBSAMPLE" --protocol "$PROTOCOL" --rows "$ROWS" --n-test "$N_TEST" --out "$T/test/pangram-subsample"
fi

# 7 collection allow-list
if ! done_ collections; then
  rm -f "$T/pangram/collections-test.json"
  unit collections 2G 0 "$ASM" public-collections --rows "$ROWS" --humans "$TEST_HUMANS/documents.jsonl" "${SEALARGS[@]}" \
    --out "$T/pangram/collections-test.json"
fi

# 8 collector prepare (offline: writes the Test job plans; sends nothing)
if ! done_ prepare; then
  [[ ! -e $T/pangram/test ]] || die "$T/pangram/test exists without a 0 prepare exit: inspect before rerunning"
  unit prepare 4G 0 "$BULK" prepare test "$T/test/pangram-subsample/subsample.jsonl" "$ROWS" "$T/pangram/collections-test.json" \
    "$T/pangram/test" --tail-units "$TAIL_UNITS"
fi

# STOP: record what the coordinator dispatches and scores next.
jq -n --arg seal "$SEAL_SHA" --arg bundle "$BUNDLE" --arg rows_sha "$(sha "$ROWS")" \
  --arg sub "$T/test/pangram-subsample" --arg design_sha "$(sha $T/test/pangram-subsample/design.json)" \
  --arg plan "$T/pangram/test/PLAN.json" --arg plan_sha "$(sha $T/pangram/test/PLAN.json)" \
  --arg slices "$T/test/slice-minimums.json" --arg slices_sha "$(sha $T/test/slice-minimums.json)" \
  --arg receipt "$A/overlap-receipt.json" --arg receipt_sha "$(sha $A/overlap-receipt.json)" \
  --arg score "$HERE/test-score-driver.sh $SEAL_SHA $ASM $T/bin/a_comp_test_calibrate-8bd827fe" \
  '{schema: "slop_ninja_a_comp_tierc_ready_for_dispatch_v1", seal_sha256: $seal, test_bundle: $bundle, rows_sha256: $rows_sha,
    pangram_subsample: $sub, design_sha256: $design_sha, collector_plan: $plan, collector_plan_sha256: $plan_sha,
    slice_minimums: $slices, slice_minimums_sha256: $slices_sha, overlap_receipt: $receipt, overlap_receipt_sha256: $receipt_sha,
    next: ["coordinator: re-read the balance, then a_comp_pangram_bulk-d919cac8 run tier-c/pangram/test LEDGER KEYFILE",
           ("coordinator: A scoring (GPU) " + $score),
           "coordinator: a_comp_tierc_evaluate-9f884629 (one run) with --pred/--ops from tier-c/test/score/INDEX.json, --frozen S3A, --held-out-generator olmo2"]}' \
  > "$T/test/READY-FOR-DISPATCH.json"
status "stopped before dispatch: READY-FOR-DISPATCH.json written"
log "POSTSEAL_READY_FOR_DISPATCH"

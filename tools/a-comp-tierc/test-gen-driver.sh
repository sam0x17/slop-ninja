#!/bin/bash
# Post-seal Test positive generation (open-model and OLMo-2 robustness).
# NOT to be run before $T/seal/SEAL.json exists. Resumable: every step writes
# an exit file and a step whose exit file holds 0 is skipped on a rerun.
#
# Usage: test-gen-driver.sh SEAL_SHA256 BIN
#   SEAL_SHA256  sha256 of tier-c/seal/SEAL.json (refuses on mismatch)
#   BIN          sealed slop_ninja_a_positives binary; its sha256 must appear
#                in SEAL.json and in tier-c/bin/SHA256SUMS
# Durable start (coordinator):
#   systemd-run --user --slice=slop-a-comp.slice --unit=slop-a-comp-tierc-testgen \
#     -p MemoryMax=32G -p MemorySwapMax=0 --working-directory=. \
#     /path/to/tools/a-comp-tierc/test-gen-driver.sh <SEAL_SHA256> <BIN>
# The unit holds the host big-memory lock for its whole run (mmapped 13B GGUF).
# Order: plan, OLMo-2 instruct, the seven Train instruct families, OLMo-2 base,
# finalize. Each generator runs under tools/gpu-lock.sh --wait; Test has
# priority over the Train generator (the coordinator stops that unit between
# its generators; the train-priority flag is set while this driver waits).
set -uo pipefail
umask 027
export PATH=~/.linuxbrew/bin:/usr/local/bin:/usr/bin:/bin
HERE=$(cd "$(dirname "$0")" && pwd)
LOCK=$HERE/../gpu-lock.sh
S=data/grammar-private/a-competitive-20261027
T=$S/tier-c
W2=$T/workstreams/w2-generate
SEAL=$T/seal/SEAL.json
TEST_HUMANS=$S/data/eval-humans-test/test
TRAIN=$S/data/splits-v4/train/documents.jsonl
TRAIN_SHA=d65d43100c0409f32a83f623cb25d1216625a86a879f34ed773dd3467a702824
RUN=$S/data/positives-test
D=$RUN/driver
TREF=$W2/configs-test.TEMPLATES.json
QREC=$S/generators/test-only/olmo2/qualification/receipt.json
YIELD=data/locks/train-priority
BIGMEM=data/locks/bigmem.lock
GENS="olmo2_instruct qwen36 phi4mini mistral7b gemma3_4b granite_instruct smollm3_instruct ernie_instruct olmo2_base"
SEAL_SHA=${1:?SEAL_SHA256}; BIN=${2:?BIN}
mkdir -p "$D"
log(){ echo "$(date -Is) $*" | tee -a "$D/driver.log"; }
die(){ log "REFUSED: $*"; exit 2; }
# ---- refusals (before anything reads Test prose)
[[ -f $SEAL ]] || die "seal not written ($SEAL)"
[[ $(sha256sum "$SEAL" | cut -c1-64) == "$SEAL_SHA" ]] || die "SEAL.json sha256 differs from the argument"
BIN_SHA=$(sha256sum "$BIN" | cut -c1-64)
grep -q "$BIN_SHA" "$SEAL" || die "generation binary $BIN_SHA is not bound in SEAL.json"
grep -q "^$BIN_SHA " "$T/bin/SHA256SUMS" || die "generation binary not listed in tier-c/bin/SHA256SUMS"
MAN_SHA=$(sha256sum "$W2/configs-test.MANIFEST.json" | cut -c1-64)
grep -q "$MAN_SHA" "$SEAL" || die "Test generator config manifest $MAN_SHA is not bound in SEAL.json"
(cd "$W2" && jq -r '.configs[] | "\(.sha256)  configs-test/\(.file)"' configs-test.MANIFEST.json | sha256sum -c --quiet -) || die "Test generator configs differ from their manifest"
[[ $(ls "$W2/configs-test"/*.json | wc -l) == $(jq '.configs|length' "$W2/configs-test.MANIFEST.json") ]] || die "configs-test holds files outside the manifest"
[[ -f $QREC && $(jq -r .pass "$QREC") == true ]] || die "OLMo-2 qualification receipt missing or not passed"
grep -q "$(sha256sum "$QREC" | cut -c1-64)" "$SEAL" || die "OLMo-2 qualification receipt is not bound in SEAL.json"
(cd "$TEST_HUMANS" && sha256sum -c --quiet SHA256SUMS) || die "Test humans fail their SHA256SUMS"
[[ $(sha256sum "$TRAIN" | cut -c1-64) == "$TRAIN_SHA" ]] || die "Train IDF source changed"
# Chat-template references: the families' from their CAL/Train run records
# (configs-test.TEMPLATES.json, listed in the manifest), OLMo-2 instruct's
# from the qualification receipt.
[[ $(sha256sum "$TREF" | cut -c1-64) == $(jq -r .templates_sha256 "$W2/configs-test.MANIFEST.json") ]] || die "template references differ from the manifest"
tsha(){ case $1 in olmo2_instruct) jq -r .instruct.chat_template_sha256 "$QREC";; olmo2_base) echo -;; *) jq -r --arg g "$1" '.[$g]' "$TREF";; esac; }
exec 9>"$BIGMEM"
log "waiting for the big-memory lock"; flock 9; log "big-memory lock held"
mine=testgen-$$
release_yield(){ [[ -f $YIELD && $(cat "$YIELD" 2>/dev/null) == "$mine" ]] && rm -f "$YIELD"; }
trap release_yield EXIT
# ---- plan (CPU)
if [[ ! -f $D/plan.exit || $(cat "$D/plan.exit") != 0 ]]; then
  rm -rf "$RUN/plan.tmp"
  log "plan start"
  "$BIN" plan-test --train "$TRAIN" --test "$TEST_HUMANS/documents.jsonl" --seal "$SEAL" --seal-sha256 "$SEAL_SHA" \
    --out "$RUN/plan.tmp" > "$D/plan.json.out" 2> "$D/plan.err"
  rc=$?
  if [[ $rc == 0 ]]; then
    [[ ! -e $RUN/plan ]] || die "plan directory exists without a 0 plan.exit; inspect before rerunning"
    mv "$RUN/plan.tmp" "$RUN/plan"
  fi
  echo $rc > "$D/plan.exit"; log "plan rc=$rc"
  [[ $rc == 0 ]] || exit $rc
fi
# ---- generators (GPU), fixed order
fail=0
for g in $GENS; do
  [[ -f $D/$g.exit && $(cat "$D/$g.exit") == 0 ]] && { log "skip $g (done)"; continue; }
  echo "$mine" > "$YIELD"
  log "start $g"
  "$LOCK" --wait --exit-file "$D/$g.exit" -- "$HERE/test-gen-one.sh" "$BIN" "$g" "$RUN" "$(tsha "$g")"
  rc=$(cat "$D/$g.exit" 2>/dev/null || echo 70)
  log "end $g rc=$rc"
  [[ $rc == 0 ]] || fail=1
done
release_yield
[[ $fail == 0 ]] || { log "a generator step failed; rerun resumes it"; exit 1; }
# ---- finalize (CPU)
if [[ ! -f $D/finalize.exit || $(cat "$D/finalize.exit") != 0 ]]; then
  rm -rf "$RUN/final.tmp"
  "$BIN" finalize --plan "$RUN/plan" --runs "$RUN/runs" --configs "$W2/configs-test" \
    --tokenizer data/model-recovery/v6-20261026/slop_ninja_detector_revision_v0.6.0/encoder/classifier/tokenizer.json \
    --out "$RUN/final.tmp" > "$D/finalize.json.out" 2> "$D/finalize.err"
  rc=$?
  [[ $rc == 0 ]] && { rm -rf "$RUN/final"; mv "$RUN/final.tmp" "$RUN/final"; }
  echo $rc > "$D/finalize.exit"; log "finalize rc=$rc"
  [[ $rc == 0 ]] || exit $rc
fi
log "TEST_GENERATION_DONE"
touch "$D/DONE"

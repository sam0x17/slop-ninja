#!/bin/bash
# Post-seal A scoring of the Test bundle for every fit the Tier C evaluator needs:
# Faux 17/29/41, frozen stage-3 A (S3A) 17/29/41, E0 17/29/41 and V6, each through
# the model path of its CAL raw run (tierc/test_score.py: faux = export_full_a.py raw,
# v6 = rc-work/tools/v6_raw.py, s3a = tierc/stage3_raw.py, e0 = tierc/train_e0_a.py raw)
# and calibrated with that fit's frozen CAL calibration (a_comp_test_calibrate, the
# calibrate_full_a projection). Prediction files are the evaluator's --pred input; the
# operating points are each fit's CAL evaluation (--ops).
#
# Usage:
#   test-score-driver.sh SEAL_SHA256 ASM_BIN CALIB_BIN
#       post-seal: refuses unless tier-c/seal/SEAL.json hashes to SEAL_SHA256, says sealed,
#       and binds ASM_BIN, CALIB_BIN, tierc/test_score.py and this driver; both binaries must
#       be listed in tier-c/bin/SHA256SUMS.
#   test-score-driver.sh --cal-check ASM_BIN CALIB_BIN FIT...
#       mechanical identity check on CAL (no seal; FIT = Faux:17, S3A:17, E0:17, V6:17, ...):
#       the same scoring and calibration path on the CAL scoring rows must reproduce W3's
#       CAL raw run (raw.jsonl, predictions.jsonl) and cal-calibrated.jsonl byte for byte.
#       No metric is computed.
# Durable start (coordinator), after the Test bundle build:
#   systemd-run --user --slice=slop-a-comp.slice --unit=slop-a-comp-tierc-test-score \
#     -p MemoryMax=12G -p MemorySwapMax=0 /bin/bash .../tools/a-comp-tierc/test-score-driver.sh SEAL ASM CALIB
# Resumable: per-fit exit files (raw-<fit>.exit from tools/gpu-lock.sh, pred-<fit>.exit);
# a fit whose pred exit file holds 0 is skipped; a raw directory without its exit file 0 is
# removed and rescored. The unit holds the big-memory lock and, while it waits for and
# holds the GPU, the train-priority flag.
set -uo pipefail
umask 027
export PATH=/usr/local/bin:/usr/bin:/bin
export PYTHONDONTWRITEBYTECODE=1 HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1 TOKENIZERS_PARALLELISM=false
export CUBLAS_WORKSPACE_CONFIG=:4096:8 OMP_NUM_THREADS=1 MKL_NUM_THREADS=1 OPENBLAS_NUM_THREADS=1 CUDA_VISIBLE_DEVICES=0
HERE=$(cd "$(dirname "$0")" && pwd)
LOCK=$HERE/../gpu-lock.sh
REPO=.
S=$REPO/data/grammar-private/a-competitive-20261027
T=$S/tier-c
W3=$T/workstreams/w3-gpu
R=$S/baseline-release/rc-work
SEAL=$T/seal/SEAL.json
OLD=$REPO/data/grammar-private/a-parity-stage2-20261026
ART=$OLD/runtime/encoder-linux-compatible; INIT=$OLD/new-cohort/warm-start-v1/init.json
P=$REPO/data/worktrees/a-comp-baseline/grammar/crates/grammar-eval/python
P_COMMIT=2af4baa510befdb50173169cb4c88b6e7bbef364
TIERC=$(cd "$HERE/../../grammar/crates/grammar-eval/python/tierc" && pwd)
SCORER=$TIERC/test_score.py
ST=$REPO/data/grammar-private/a-encoder-stage3-20261026
PY=$S/venv/bin/python
TB=$(cat $R/TRAINER_BUNDLE)
YIELD=$REPO/data/locks/train-priority
BIGMEM=$REPO/data/locks/bigmem.lock
FITS="Faux:17 Faux:29 Faux:41 S3A:17 S3A:29 S3A:41 E0:17 E0:29 E0:41 V6:17"
# V6 carries seed 17 because its CAL evaluation (the --ops source) records it as V6:17.
sha(){ sha256sum "$1" | cut -c1-64; }

MODE=test
if [[ ${1:-} == --cal-check ]]; then MODE=cal; shift; ASM=${1:?ASM_BIN}; CALIB=${2:?CALIB_BIN}; shift 2; FITS="$*"
else SEAL_SHA=${1:?SEAL_SHA256}; ASM=${2:?ASM_BIN}; CALIB=${3:?CALIB_BIN}; fi
if [[ $MODE == test ]]; then O=$T/test/score; else O=$T/workstreams/w5-bundle/cal-check-gpu; fi
mkdir -p "$O"
log(){ echo "$(date -Is) $*" | tee -a "$O/driver.log" >&2; }
die(){ log "REFUSED: $*"; exit 2; }

# ---- refusals (before anything reads Test prose)
[[ $(git -C $P rev-parse HEAD) == "$P_COMMIT" && -z $(git -C $P status --porcelain -- .) ]] || die "release trainer tree changed"
[[ -z $(git -C "$TIERC" status --porcelain -- .) ]] || die "tierc tree has local changes"
for b in "$ASM" "$CALIB"; do [[ -x $b ]] || die "missing binary $b"; done
if [[ $MODE == test ]]; then
  [[ -f $SEAL ]] || die "seal not written ($SEAL): Test is sealed"
  [[ $(sha "$SEAL") == "$SEAL_SHA" ]] || die "SEAL.json sha256 differs from the argument"
  [[ $(jq -r .sealed "$SEAL") == true ]] || die "SEAL.json does not say sealed"
  for f in "$ASM" "$CALIB" "$SCORER" "$0" "$TIERC/train_e0_a.py"; do
    grep -q "$(sha "$f")" "$SEAL" || die "$f ($(sha "$f")) is not bound in SEAL.json"
  done
  for b in "$ASM" "$CALIB"; do grep -q "^$(sha "$b") " "$T/bin/SHA256SUMS" || die "$b not listed in tier-c/bin/SHA256SUMS"; done
  BUILD=$(jq -r .current $T/test/bundles/CURRENT.json 2>/dev/null) || die "no Test bundle build (tier-c/test/bundles/CURRENT.json)"
  [[ $(cat $T/test/jobs/bundle.exit 2>/dev/null) == 0 ]] || die "Test bundle build not complete"
  [[ $(jq -r .seal.sha256 "$BUILD/BUILD.json") == "$SEAL_SHA" ]] || die "Test bundle built under another seal"
  SR=$T/test/scoring-rows
  SEALARGS=(--seal "$SEAL" --seal-sha256 "$SEAL_SHA")
else
  BUILD=$S/baseline-release/bundles/build-18b3682973982a3f
  SR=$O/scoring-rows-cal
  SEALARGS=()
fi

# ---- scoring rows (CPU, once)
if [[ $(cat "$O/scoring-rows.exit" 2>/dev/null) != 0 ]]; then
  rm -rf "$SR"
  if [[ $MODE == test ]]; then
    "$ASM" scoring-rows --build "$BUILD" --split test "${SEALARGS[@]}" --commit "$(jq -r .code.commit "$BUILD/BUILD.json")" --out "$SR" > "$O/scoring-rows.json" 2> "$O/scoring-rows.err"
  else
    "$ASM" scoring-rows --build "$BUILD" --split cal --commit cal-check --out "$SR" > "$O/scoring-rows.json" 2> "$O/scoring-rows.err"
  fi
  echo $? > "$O/scoring-rows.exit"
  [[ $(cat "$O/scoring-rows.exit") == 0 ]] || die "scoring rows failed (see $O/scoring-rows.err)"
fi

# Fit -> scorer arguments (the CAL raw paths of W3 calraw-inner.sh / e0-inner.sh), calibration dir, roster, share mode.
cfg(){ if [[ $2 == 17 ]]; then echo $R/configs/$1; else echo $R/configs/$1-s2941; fi; }
fit_args(){
  local n=${1%%:*} s=${1##*:}
  case $n in
    Faux) KIND=faux; ARGS=(--config "$(cfg Faux $s)" --fit $R/fits/Faux-seed-$s/fit); C=$W3/calibration/Faux-seed-$s
          ROSTER=$(cfg Faux $s)/contract.json; SM=$(jq -r .binding.arm_definition.share_mode $R/fits/Faux-seed-$s/fit/fit.json); W3RAW=$W3/raw/Faux-seed-$s-cal ;;
    V6) KIND=v6; ARGS=(--config $R/configs/F); C=$W3/calibration/V6; ROSTER=$W3/comparators/V6.json; SM=$(jq -r .share_mode $ROSTER); W3RAW=$W3/raw/V6-cal ;;
    S3A) KIND=s3a; ARGS=(--fit $ST/fits-v1/adapt_last_block-seed-$s --s3contract $ST/fit-bundle-v1/contract.json
           --selection $ST/calibration-run-v1/selection.json --export $ST/calibration-run-v1/runs/adapt_last_block-seed-$s/export)
         C=$W3/calibration/S3A-seed-$s; ROSTER=$W3/comparators/S3A-seed-$s.json; SM=$(jq -r .share_mode $ROSTER); W3RAW=$W3/raw/S3A-seed-$s-cal ;;
    E0) KIND=e0; ARGS=(--contract $R/configs/F/contract.json --recipe $TIERC/e0-recipe-v1.json --fit $W3/e0/fits/E0-seed-$s)
        C=$W3/calibration/E0-seed-$s; ROSTER=$W3/comparators/E0-seed-$s.json; SM=$(jq -r .share_mode $ROSTER); W3RAW=$W3/raw/E0-seed-$s-cal ;;
    *) die "unknown fit $1" ;;
  esac
  TAG=$n-$s
}

exec 9>"$BIGMEM"
log "waiting for the big-memory lock"; flock 9; log "big-memory lock held"
mine="tier-c test-score $$"
keeper(){ while true; do [[ -e $YIELD ]] || printf '%s %s\n' "$mine" "$(date -Is)" > "$YIELD"; sleep 5; done; }
cleanup(){ [[ -n ${kp:-} ]] && kill $kp 2>/dev/null; grep -q "^$mine " "$YIELD" 2>/dev/null && rm -f "$YIELD"; }
trap cleanup EXIT
fail=0
for fit in $FITS; do
  fit_args "$fit"
  [[ $(cat "$O/pred-$TAG.exit" 2>/dev/null) == 0 ]] && { log "skip $fit (done)"; continue; }
  # GPU: raw predictions
  if [[ $(cat "$O/raw-$TAG.exit" 2>/dev/null) != 0 ]]; then
    rm -rf "$O/raw/$TAG" "$O/raw/$TAG.partial"; mkdir -p "$O/raw"
    [[ -n ${kp:-} ]] || { keeper & kp=$!; }
    log "score $fit ($KIND)"
    (cd "$TIERC" && PYTHONPATH=$TIERC:$P "$LOCK" --wait --exit-file "$O/raw-$TAG.exit" -- \
      "$PY" "$SCORER" $KIND --data "$TB" --rows "$SR" --out "$O/raw/$TAG" --artifact "$ART" --init "$INIT" \
      "${ARGS[@]}" "${SEALARGS[@]}" --device cuda) >> "$O/raw-$TAG.log" 2>&1
    rc=$(cat "$O/raw-$TAG.exit" 2>/dev/null || echo 70); log "score $fit rc=$rc"
    [[ $rc == 0 ]] || { fail=1; rm -rf "$O/raw/$TAG"; continue; }
  fi
  # CPU: frozen CAL calibration
  mkdir -p "$O/pred"; rm -f "$O/pred/$TAG.jsonl"
  "$CALIB" --calibration $C/calibration.json --record $C/record.json --raw "$O/raw/$TAG" --rows "$SR" \
    --share-mode "$SM" --roster "$ROSTER" --out "$O/pred/$TAG.jsonl" > "$O/pred-$TAG.json" 2> "$O/pred-$TAG.err"
  rc=$?
  if [[ $MODE == cal && $rc == 0 && $KIND != v6 ]]; then
    # Mechanical identity: raw, uncalibrated and calibrated files equal W3's CAL ones.
    for f in raw.jsonl predictions.jsonl; do [[ $(sha "$O/raw/$TAG/$f") == $(sha "$W3RAW/$f") ]] || { log "$fit: $f differs from W3 CAL"; rc=9; }; done
    [[ $(sha "$O/pred/$TAG.jsonl") == $(sha "$C/cal-calibrated.jsonl") ]] || { log "$fit: calibrated predictions differ from W3 CAL"; rc=9; }
    [[ $rc == 0 ]] && log "$fit: raw, predictions and calibrated CAL files byte-identical to W3"
  elif [[ $MODE == cal && $rc == 0 ]]; then
    # V6: its untrained share/AI/human heads are random draws (unseeded in W3's CAL run), so only the
    # origin path can match: every origin and involvement field and the calibrated origin probabilities.
    python3 -I - "$O/raw/$TAG/raw.jsonl" "$W3RAW/raw.jsonl" "$O/pred/$TAG.jsonl" "$C/cal-calibrated.jsonl" > "$O/v6-origin-check.json" <<'PY' || rc=9
import json, sys
a, b, c, d = ([json.loads(l) for l in open(p)] for p in sys.argv[1:5])
raw_keys = ("id", "text_sha256", "tokens", "origin_logits", "involvement_logit", "uncalibrated_origin")
cal_keys = ("id", "p_human", "p_model_only", "p_mixed")
ok_raw = len(a) == len(b) and all(all(x[k] == y[k] for k in raw_keys) for x, y in zip(a, b))
ok_cal = len(c) == len(d) and all(all(x[k] == y[k] for k in cal_keys) for x, y in zip(c, d))
print(json.dumps({"rows": len(a), "raw_fields_identical": list(raw_keys), "raw_identical": ok_raw,
                  "calibrated_fields_identical": list(cal_keys), "calibrated_identical": ok_cal,
                  "not_compared": "share, conditional_mixed_share, token_mean, ai_logits, human_logits, attribution (untrained random heads)"}))
sys.exit(0 if ok_raw and ok_cal else 1)
PY
    [[ $rc == 0 ]] && log "$fit: origin/involvement raw fields and calibrated origin probabilities identical to W3 (untrained heads not compared)"
  fi
  echo $rc > "$O/pred-$TAG.exit"; log "calibrate $fit rc=$rc"
  [[ $rc == 0 ]] || fail=1
done
cleanup; kp=
[[ $fail == 0 ]] || { log "a fit failed; rerun resumes it"; exit 1; }
# ---- index for the evaluator (--pred NAME:SEED=PATH --ops NAME:SEED=PATH)
{
  echo '{"schema":"slop_ninja_a_comp_test_predictions_index_v1","mode":"'$MODE'","fits":['
  first=1
  for fit in $FITS; do
    fit_args "$fit"; n=${fit%%:*}; s=${fit##*:}
    [[ $first == 1 ]] || echo ','; first=0
    printf '{"system":"%s","seed":%s,"pred":"%s","pred_sha256":"%s","ops":"%s","ops_sha256":"%s","raw_manifest_sha256":"%s","failures":%s}' \
      "$n" "$s" "$O/pred/$TAG.jsonl" "$(sha "$O/pred/$TAG.jsonl")" "$C/cal-eval/evaluation.json" "$(sha "$C/cal-eval/evaluation.json")" \
      "$(sha "$O/raw/$TAG/manifest.json")" "$(jq -c .failures_by_reason "$O/raw/$TAG/manifest.json")"
  done
  echo ']}'
} > "$O/INDEX.json.tmp" && mv "$O/INDEX.json.tmp" "$O/INDEX.json"
log "SCORING_DONE ($MODE)"
touch "$O/DONE"

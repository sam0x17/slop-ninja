#!/bin/bash
# Serve one Test generator (llama-server, flags from its Test config) and run
# its pending Test requests. Runs INSIDE tools/gpu-lock.sh.
# Usage: test-gen-one.sh BIN GEN RUN_ROOT TEMPLATE_SHA256
#   BIN       sealed slop_ninja_a_positives binary (hash checked by the driver)
#   GEN       generator key (olmo2_instruct, qwen36, ..., olmo2_base)
#   RUN_ROOT  Test generation root holding plan/, runs/ and logs/
#   TEMPLATE_SHA256    sha256 the server's /props chat_template must have (instruct)
set -uo pipefail
umask 027
export PATH=~/.linuxbrew/bin:/usr/local/bin:/usr/bin:/bin
S=data/grammar-private/a-competitive-20261027
CONFIGS=$S/tier-c/workstreams/w2-generate/configs-test
INDEX=$S/data/eval-humans/work/prior-index-v5
TOKENIZER=data/model-recovery/v6-20261026/slop_ninja_detector_revision_v0.6.0/encoder/classifier/tokenizer.json
BIN=$1; GEN=$2; RUN=$3; want=$4
CFG=$CONFIGS/$GEN.json
LOG=$RUN/logs/$GEN-$(date +%s); mkdir -p "$LOG"
model=$(jq -r .model_path "$CFG"); runtime=$(jq -r .runtime_path "$CFG"); alias=$(jq -r .alias "$CFG")
port=$(jq -r .port "$CFG"); flags=$(jq -r .server_flags "$CFG"); kind=$(jq -r .kind "$CFG")
export LD_LIBRARY_PATH=${runtime%/*}
srv=
finish(){ rc=$?; trap - EXIT; if [[ -n $srv ]]; then kill $srv 2>/dev/null; wait $srv 2>/dev/null; fi
  cg=$(cut -d: -f3 /proc/self/cgroup); for f in memory.peak memory.events; do cat "/sys/fs/cgroup$cg/$f" >"$LOG/$f" 2>/dev/null; done
  echo $rc > "$LOG/exit"; exit $rc; }
trap finish EXIT
[[ $(jq -r .key "$CFG") == "$GEN" ]] || { echo "config key mismatch"; exit 2; }
[[ $(jq -c .allowed_splits "$CFG") == '["test"]' ]] || { echo "Test config must allow only test"; exit 2; }
printf '%s  %s\n' "$(jq -r .sha256 "$CFG")" "$model" | sha256sum -c - > "$LOG/model-sha.txt" 2>&1 || { echo "model hash mismatch"; exit 2; }
printf '%s  %s\n' "$(jq -r .runtime_sha256 "$CFG")" "$runtime" | sha256sum -c - >> "$LOG/model-sha.txt" 2>&1 || { echo "runtime hash mismatch"; exit 2; }
sha256sum "$CFG" > "$LOG/config.sha256"
nvidia-smi --query-gpu=uuid,name,driver_version,memory.total,memory.used --format=csv > "$LOG/gpu-start.csv"
# shellcheck disable=SC2086
"$runtime" -m "$model" --alias "$alias" --host 127.0.0.1 --port "$port" $flags > "$LOG/server.log" 2>&1 &
srv=$!
up=0
for i in $(seq 1 900); do
  if curl -sf --max-time 3 "http://127.0.0.1:$port/health" >/dev/null; then up=1; break; fi
  kill -0 $srv 2>/dev/null || break
  sleep 1
done
[[ $up == 1 ]] || { echo "server did not become healthy"; exit 3; }
nvidia-smi --query-gpu=memory.used --format=csv > "$LOG/gpu-loaded.csv"
curl -sf "http://127.0.0.1:$port/props" > "$LOG/props.json" || { echo "props unavailable"; exit 3; }
# Chat template frozen per generator: the embedded template seen by the CAL/Train
# runs (families) or by the OLMo-2 qualification receipt (OLMo-2 instruct).
if [[ $kind == instruct ]]; then
  got=$(jq -j .chat_template "$LOG/props.json" | sha256sum | cut -c1-64)
  echo "$got" > "$LOG/chat-template.sha256"
  [[ -n $want && $got == "$want" ]] || { echo "chat template $got differs from the frozen reference ${want:-missing}"; exit 4; }
fi
"$BIN" generate --generator "$GEN" --config "$CFG" --plan "$RUN/plan" --runs "$RUN/runs" \
  --extra-runs "$S/data/positives/runs-v2" --extra-runs "$S/data/positives/runs" \
  --index "$INDEX" --tokenizer "$TOKENIZER" --splits test --workers 4 > "$LOG/client.json" 2> "$LOG/client.log"
rc=$?
nvidia-smi --query-gpu=memory.used --format=csv > "$LOG/gpu-end.csv"
exit $rc

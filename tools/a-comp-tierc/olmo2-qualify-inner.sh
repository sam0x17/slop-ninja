#!/bin/bash
# GPU part of the OLMo-2 qualification; runs INSIDE tools/gpu-lock.sh.
# Loads each pinned GGUF in the pinned llama-server with its Test config's
# flags and runs a 20-token smoke on the fixed synthetic prompts below (no
# corpus text). Writes receipt.json (pass true/false). Usage: olmo2-qualify-inner.sh QDIR
set -uo pipefail
umask 027
export PATH=~/.linuxbrew/bin:/usr/local/bin:/usr/bin:/bin
HERE=$(cd "$(dirname "$0")" && pwd)
S=data/grammar-private/a-competitive-20261027
CONFIGS=$S/tier-c/workstreams/w2-generate/configs-test
PROTO=$S/protocol-v1.json
Q=$1
# Fixed synthetic smoke prompts (bound into the receipt by text and sha256).
CHAT_PROMPT='Write one short paragraph explaining why rivers bend into meanders over time.'
BASE_PROMPT='Over thousands of years, a river flowing across a wide flat plain slowly begins to bend, because'
RUNTIME_SHA=$(jq -r .generators.runtime.llama_server_sha256 "$PROTO")
srv=
stop(){ [[ -n $srv ]] && { kill "$srv" 2>/dev/null; wait "$srv" 2>/dev/null; srv=; }; sleep 3; }
trap stop EXIT
pass=true
nvidia-smi --query-gpu=uuid,name,driver_version,memory.total,memory.used --format=csv > "$Q/gpu-start.csv"
for kind in instruct base; do
  d=$Q/$kind; rm -rf "$d"; mkdir -p "$d"
  cfg=$CONFIGS/olmo2_$kind.json
  model=$(jq -r .model_path "$cfg"); runtime=$(jq -r .runtime_path "$cfg"); port=$(jq -r .port "$cfg")
  flags=$(jq -r .server_flags "$cfg"); alias=$(jq -r .alias "$cfg")
  export LD_LIBRARY_PATH=${runtime%/*}
  st=ok
  [[ $(jq -r .sha256 "$cfg") == $(jq -r ".generators.test_only.$kind.sha256" "$PROTO") ]] || st=config_pin_mismatch
  [[ $(jq -r .runtime_sha256 "$cfg") == "$RUNTIME_SHA" ]] || st=config_runtime_mismatch
  if [[ $st == ok ]]; then
    printf '%s  %s\n' "$(jq -r .sha256 "$cfg")" "$model" | sha256sum -c - > "$d/model-sha-check.txt" 2>&1 || st=model_sha_mismatch
    printf '%s  %s\n' "$RUNTIME_SHA" "$runtime" | sha256sum -c - >> "$d/model-sha-check.txt" 2>&1 || st=runtime_sha_mismatch
  fi
  "$runtime" --version > "$d/runtime-version.txt" 2>&1
  if [[ $st == ok ]]; then
    # shellcheck disable=SC2086
    "$runtime" -m "$model" --alias "$alias" --host 127.0.0.1 --port "$port" $flags > "$d/server.log" 2>&1 &
    srv=$!
    up=0
    for i in $(seq 1 900); do
      if curl -sf --max-time 3 "http://127.0.0.1:$port/health" >/dev/null; then up=1; break; fi
      kill -0 "$srv" 2>/dev/null || break
      sleep 1
    done
    [[ $up == 1 ]] || st=load_failed
  fi
  if [[ $st == ok ]]; then
    nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits > "$d/gpu-mib-loaded.txt"
    curl -sf "http://127.0.0.1:$port/props" > "$d/props.json" || st=props_failed
    jq -j '.chat_template // ""' "$d/props.json" > "$d/chat_template.txt"
    jq -nc --arg p "$BASE_PROMPT" '{prompt:$p,n_predict:20,seed:1,temperature:0,top_k:0,top_p:1,min_p:0,repeat_penalty:1.0,ignore_eos:true,cache_prompt:false}' |
      curl -s --max-time 600 "http://127.0.0.1:$port/completion" -H 'Content-Type: application/json' -d @- > "$d/smoke-completion-20.json"
    [[ $(jq -r '.tokens_predicted // 0' "$d/smoke-completion-20.json") == 20 && -n $(jq -r '.content // ""' "$d/smoke-completion-20.json") ]] || st=completion_smoke_failed
    if [[ $kind == instruct ]]; then
      [[ -s $d/chat_template.txt ]] || st=no_embedded_chat_template
      jq -nc --arg p "$CHAT_PROMPT" --arg m "$alias" '{model:$m,messages:[{role:"user",content:$p}],max_tokens:20,seed:1,temperature:0,top_k:0,top_p:1,min_p:0,repeat_penalty:1.0,ignore_eos:true,chat_template_kwargs:{enable_thinking:false},reasoning_format:"none"}' |
        curl -s --max-time 600 "http://127.0.0.1:$port/v1/chat/completions" -H 'Content-Type: application/json' -d @- > "$d/smoke-chat-20.json"
      [[ $(jq -r '.usage.completion_tokens // 0' "$d/smoke-chat-20.json") == 20 && -n $(jq -r '.choices[0].message.content // ""' "$d/smoke-chat-20.json") ]] || st=chat_smoke_failed
    fi
  fi
  stop
  echo "$st" > "$d/status"
  [[ $st == ok ]] || pass=false
done
tsha=$(sha256sum < "$Q/instruct/chat_template.txt" 2>/dev/null | cut -c1-64)
part(){ # kind
  local d=$Q/$1 cfg=$CONFIGS/olmo2_$1.json
  jq -n --arg kind "$1" --arg status "$(cat "$d/status")" --slurpfile cfg "$cfg" \
    --arg cfg_sha "$(sha256sum "$cfg" | cut -c1-64)" --arg ver "$(head -c 2000 "$d/runtime-version.txt")" \
    --arg vram "$(cat "$d/gpu-mib-loaded.txt" 2>/dev/null)" \
    --arg tsha "$( [[ $1 == instruct ]] && echo "$tsha")" \
    --argjson comp "$(jq -c '{content,tokens_predicted,stop_type,timings}' "$d/smoke-completion-20.json" 2>/dev/null || echo null)" \
    --argjson chat "$( [[ $1 == instruct ]] && (jq -c '{content:.choices[0].message.content,finish_reason:.choices[0].finish_reason,usage}' "$d/smoke-chat-20.json" 2>/dev/null || echo null) || echo null)" \
    --argjson props "$(jq -c '{model_path,total_slots,n_ctx:.default_generation_settings.n_ctx,build_info}' "$d/props.json" 2>/dev/null || echo null)" \
    '{kind:$kind,status:$status,repo:$cfg[0].repo,revision:$cfg[0].revision,filename:$cfg[0].filename,size_bytes:$cfg[0].size_bytes,
      sha256:$cfg[0].sha256,model_path:$cfg[0].model_path,config_sha256:$cfg_sha,server_flags:$cfg[0].server_flags,
      runtime_path:$cfg[0].runtime_path,runtime_sha256:$cfg[0].runtime_sha256,runtime_version:$ver,vram_mib_loaded:$vram,
      props:$props,chat_template_sha256:(if $tsha == "" then null else $tsha end),smoke_completion:$comp,smoke_chat:$chat}'
}
jq -n --argjson pass "$pass" --argjson ins "$(part instruct)" --argjson base "$(part base)" \
  --arg chat_prompt "$CHAT_PROMPT" --arg base_prompt "$BASE_PROMPT" \
  --arg cp_sha "$(printf '%s' "$CHAT_PROMPT" | sha256sum | cut -c1-64)" --arg bp_sha "$(printf '%s' "$BASE_PROMPT" | sha256sum | cut -c1-64)" \
  --arg script_sha "$(cat "$HERE/olmo2-qualify.sh" "$0" | sha256sum | cut -c1-64)" \
  --arg proto_sha "$(sha256sum "$PROTO" | cut -c1-64)" --arg at "$(date -Is)" \
  --argjson dl_i "$(cat "$Q/download-instruct.json" 2>/dev/null || echo null)" --argjson dl_b "$(cat "$Q/download-base.json" 2>/dev/null || echo null)" \
  --arg gpu "$(cat "$Q/gpu-start.csv")" \
  '{schema:"slop_ninja_a_comp_olmo2_qualification_v1",pass:$pass,at:$at,protocol_sha256:$proto_sha,
    directory:"generators/test-only/olmo2 (Test-only)",gpu_start:$gpu,
    smoke:{max_tokens:20,decoding:"temperature 0, seed 1, ignore_eos",chat_prompt:$chat_prompt,chat_prompt_sha256:$cp_sha,
           completion_prompt:$base_prompt,completion_prompt_sha256:$bp_sha,synthetic:true,corpus_text:false},
    downloads:{instruct:$dl_i,base:$dl_b},instruct:$ins,base:$base,scripts_sha256:$script_sha,
    rule:"protocol-v1.json seal.binds OLMo-2 qualification receipt; failure blocks the seal"}' > "$Q/receipt.json.tmp"
mv "$Q/receipt.json.tmp" "$Q/receipt.json"
[[ $pass == true ]]

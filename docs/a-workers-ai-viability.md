# A on Cloudflare Workers AI

Assessed on October 28, 2026. This note asks whether *A* can be served on
Workers AI, given that *A* must return two calibrated document percentages
(confidence of AI involvement and estimated AI share) plus conditional
attribution, and that balanced involvement Brier is the primary comparison.
The current *A* is a fully fine-tuned ModernBERT-base encoder (about 150M
parameters, 2,048-token limit) with small numeric heads, which already run in
Rust in about 0.07 ms ([V6 CFG fusion](a-v6-cfg-fusion.md)). The
[CPU inference note](a-cpu-inference.md) covers Containers, Worker-native
encoders and GPU hosts in detail; this note covers the Workers AI routes and
ranks every route together. [A on a RunPod GPU backend](a-runpod-backend.md)
covers a GPU backend outside Cloudflare behind a Worker.

The evidence comes from three sources:

- official Cloudflare documentation, model schemas and changelogs, read as raw
  Markdown or JSON on 2026-10-28, with each page's own update date;
- one catalog listing, one fine-tune listing and five read-only inference
  calls on a one-line synthetic prompt (negligible usage);
- local CPU measurements on build-host.

No Cloudflare resource was created, changed or deleted, and no GPU time was
used. Quality was measured by read-only scoring of Dev-iter; Dev-select, CAL and
Test were not read. Scripts, embeddings, per-run outputs and redacted API
responses stay in ignored storage.

## Summary

| Question | Answer |
| --- | --- |
| Can our fine-tuned encoder be uploaded to Workers AI? | Not self-serve. Bring-your-own-model through Replicate Cog containers is described as work in progress with design partners (Cloudflare blog posts of 2026-04-16 and 2026-10-01). Requests go through the Custom Requirements Form; Enterprise dedicated instances already run custom models. |
| Is LoRA the only self-serve customization? | Yes. It applies only to text-generation catalog models: 9 bases carry the LoRA flag, with rank at most 32 and an adapter file under 300 MB. No embedding or classification model accepts an adapter. |
| Do responses carry probabilities? | Undocumented but observed. No LoRA-capable model's schema lists logprobs, yet live calls to `llama-3.2-3b-instruct` and to a LoRA-adapted `mistral-7b-instruct-v0.2-lora` returned per-token logprobs with top-5 alternatives and per-prompt-token logprobs. |
| Can a frozen catalog embedding replace the encoder? | No. The best catalog model (`qwen3-embedding-0.6b`) scored 0.1838 Dev-iter Brier with 39.5% human FPR under the shared heads, against 0.1577 and 15.2% for frozen V6 features under the same heads. The bge models scored 0.27-0.30, worse than a constant 0.5. |
| Can a LoRA-adapted catalog LLM be *A*? | Unmeasured. A Llama-3.2-3B experiment is prepared but has not run, because the baseline release holds the GPU. |
| Where does the current *A* run on Cloudflare today? | In a Container on CPU: about $0.008 per 1,000 Dev-iter-length documents of busy time ([CPU inference](a-cpu-inference.md)). Containers have no GPU instance types. |
| Worker-native alternative? | A distilled 32M encoder fits a Worker with Q8 weights. It measured 0.1422 Dev-iter Brier against 0.1484 for the fine-tuned base fit, with optimistic selection ([CPU inference](a-cpu-inference.md#small-encoders)). |

**Recommendation:** keep *A* off Workers AI. Launch with the fine-tuned encoder
in a Container on CPU, and develop a distilled Worker-native *A* as the
all-Workers target. Run the prepared LoRA measurement as a bounded comparator,
and treat a custom-model hosting request to Cloudflare as a separate operator
decision. The ranked list and its consequences are in
[Recommendation](#recommendation).

## Platform facts that constrain A

### Custom models

- Self-serve Workers AI runs catalog models, plus LoRA adapters on a subset of
  them. The fine-tunes overview (updated 2026-04-21) offers only "Upload a LoRA
  adapter and run fine-tuned inference with one of our base models."
- Cloudflare's 2026-04-16 platform post says "we are working on letting users
  bring their own model to Workers AI". The route it describes is Replicate's
  Cog container format, tested "internally with Cloudflare teams and some
  external customers". The 2026-10-01 Clef post still calls this
  work in progress.
- The limits page (updated 2026-09-17) directs "private custom models" to the
  Custom Requirements Form.
- GPU types, size limits, pricing, cold start, version immutability and output
  formats for custom models are undocumented.

### LoRA

These catalog models carry `lora: true` in the live API (2026-10-28); all are
text generation:

| Model | Context | Price per million tokens (in / out) |
| --- | ---: | --- |
| `@cf/meta/llama-3.2-3b-instruct` | 80,000 | $0.051 / $0.335 |
| `@cf/meta/llama-3.2-11b-vision-instruct` | 128,000 | $0.049 / $0.676 |
| `@cf/meta/llama-guard-3-8b` | 131,072 | $0.484 / $0.030 |
| `@cf/qwen/qwen2.5-coder-32b-instruct` | 32,768 | $0.66 / $1.00 |
| `@cf/qwen/qwq-32b` | 24,000 | $0.66 / $1.00 |
| `@cf/mistral/mistral-7b-instruct-v0.2-lora` (beta suffix) | 15,000 | not listed |
| `@cf/google/gemma-2b-it-lora` (beta suffix) | 8,192 | not listed |
| `@cf/google/gemma-7b-it-lora` (beta suffix) | 3,500 | not listed |
| `@cf/meta-llama/llama-2-7b-chat-hf-lora` (beta suffix) | 8,192 | not listed |

Nine further models accept a `lora` string in their input schema without the
capability flag; whether they accept adapters is untested.

Limits from the LoRA page (updated 2026-04-21) and changelogs:

- open beta, "free during this period";
- the base "must not be quantized";
- rank up to 32, an adapter file under 300 MB, files named exactly
  `adapter_config.json` and `adapter_model.safetensors`;
- "up to 100 LoRA adapters per account", and asset files cannot be edited after
  upload;
- the example targets `q_proj` and `v_proj` with `task_type: "CAUSAL_LM"`;
  other target modules, `modules_to_save` and head adapters are undocumented;
- `model_type` must be `mistral`, `gemma` or `llama`, which leaves the two Qwen
  bases without a documented value;
- changelog 2026-05-08: "LoRA models may be deprecated in the future", with
  notice promised before old adapters stop working.

A LoRA adapter changes a decoder's text output. It cannot attach a regression
or classification head, and it cannot adapt an encoder.

### Probabilities and structured output

- The published output schemas of all nine LoRA-flagged models contain only
  `response`, `tool_calls` and `usage`. Thirteen other catalog models document
  `logprobs` and `top_logprobs` (0-20).
- Live calls on 2026-10-28 returned more than the schemas list:
  - `llama-3.2-3b-instruct`: per-generated-token logprobs with 5 alternatives,
    and with `prompt_logprobs: 1`, the logprob and rank of every prompt token
    (43 of 43);
  - `mistral-7b-instruct-v0.2-lora` with a private synthetic adapter: the same
    fields, so logprobs work with an adapter attached.
- Two responses named a different model than the one requested
  (`llama-3.2-3b-instruct-v2` and `gemma-4-26b-a4b-it-external`). Separately, a
  2025-09-18 changelog entry changed EmbeddingGemma's outputs under the same id
  and advised re-indexing. Hosted outputs can therefore change without a new
  model id.
- JSON mode constrains text, not probability, and is documented as best effort.
- Cloudflare's Clef decision models return "a probability for every allowed
  answer" natively, with yes/no questions and calibration-oriented training.
  Customer tuning is available only through a forward-deployed engineering
  partnership.

### Catalog encoders

The catalog has 7 text-embedding and 2 text-classification models. None is
ModernBERT, DeBERTa, RoBERTa, E5, GTE, Nomic or Jina. Every encoder returns one
pooled vector per text, with no token-level output.

| Model | Input limit | Price per million tokens |
| --- | --- | --- |
| `@cf/qwen/qwen3-embedding-0.6b` | 8,192 context; 32 texts per call; optional `instruction` | $0.012 |
| `@cf/baai/bge-m3` | 60,000 context | $0.012 |
| `@cf/baai/bge-small-en-v1.5` / `-base-` / `-large-` | 512 tokens; mean or CLS pooling | $0.020 / $0.067 / $0.204 |
| `@cf/google/embeddinggemma-300m` | beta | not listed |

### Containers and Workers

- **Containers** are generally available (changelog 2026-04-13). The largest
  instance is `standard-4`: 4 vCPU, 12 GiB memory, 20 GB disk. No GPU instance
  type appears on the limits, pricing or FAQ pages. The FAQ (updated
  2026-10-05) gives cold starts "often ... in the 1-3 second range".
- **Workers** allow 128 MB per isolate, Wasm memory included. They are
  single-threaded with Wasm SIMD, allow up to 5 minutes of CPU per request and a
  64 MiB uncompressed bundle (limits page updated 2026-10-12). ModernBERT-base
  cannot be resident in a Worker at any tested precision
  ([CPU inference](a-cpu-inference.md#memory)).

## Reference points on Dev-iter

All quality figures are uncalibrated balanced involvement Brier on the full
Dev-iter split (2,873 documents: 570 human, 1,118 model-only, 1,185 mixed),
with the trainer's family- and origin-balanced weights. Lower is better and a
constant 0.5 scores 0.25. Human FPR is the share of human documents with
P(involvement) ≥ 0.5.

| System | Brier | Human FPR | AUROC | Source |
| --- | ---: | ---: | ---: | --- |
| Frozen V6, starting classifier | 0.1787 | 12.3% (70/570) | 0.890-0.895 | both studies |
| Frozen V6 features, new shared heads | 0.1577 (seed range 0.1470-0.1642) | 15.2% | 0.873 | embedding study |
| Fine-tuned ModernBERT-base fit | 0.1484 | 7.9% (45/570) | 0.906 | [CPU inference](a-cpu-inference.md#small-encoders) |
| Same fit, recommended mixed int8 Container build | 0.1481 | 7.9% (45/570) | – | [CPU inference](a-cpu-inference.md#quantization-quality) |
| Distilled Ettin 32M, Worker-sized | 0.1422 | 5.6% (32/570) | 0.904 | [CPU inference](a-cpu-inference.md#small-encoders) |

Caveats:

- The fine-tuned fit is a screen fit, not the baseline release, which had not
  produced a final export on 2026-10-28.
- Train subsets differ: 12,000 documents for the fine-tuned fit, 24,000 for the
  students and 15,000 for the embedding heads. Cross-row comparisons indicate
  size, not exact ranking.
- The student was selected by Dev-iter Brier from six evaluations with one seed,
  so its score is optimistic.
- Dev-iter documents are short (at most 663 ModernBERT tokens) and 80% of
  positives come from seven local generators. No figure here covers long
  documents, unseen generators or adversarial rewrites.

## Candidate shapes

### 1. Catalog embedding with shared heads in a Worker (measured)

**Shape.** A Worker sends the document to a Workers AI embedding model and feeds
the pooled vector to *A*'s shared encoder and heads in Rust/Wasm. All three
outputs come from one learned shared encoding, as the contract requires.

**Experiment.** The open-weight checkpoints behind the catalog ids ran locally in
fp32 on CPU, truncated to each Workers AI input cap. Train was a stratified
15,000-document subset of build `bad203665202878a` (2,685 positives). The heads
were the shared 64→32 tanh encoder with origin, share and attribution heads,
seeds 17, 29 and 41, with no calibration. A pre-registered rule selected the
epoch on a 15% source-group holdout of Train, never on Dev-iter.

| Input | Brier (seed range) | Human FPR | TPR | AUROC | TPR at 1% FPR | Share MAE (points) | Top-1 attribution |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Frozen V6 features | 0.1577 (0.1470-0.1642) | 15.2% | 0.736 | 0.873 | 0.44 | 24.1 | 0.178 |
| `qwen3-embedding-0.6b`, no instruction | 0.1838 (0.1764-0.1902) | 39.5% | 0.767 | 0.787 | 0.24 | 29.8 | 0.114 |
| `bge-large-en-v1.5`, 512-token windows, mean | 0.2693 (0.2672-0.2717) | 36.4% | 0.562 | 0.657 | 0.07 | 38.6 | 0.120 |
| `bge-large-en-v1.5`, mean (Workers AI default) | 0.2833 (0.2683-0.2941) | 30.2% | 0.513 | 0.658 | 0.08 | 39.0 | 0.119 |
| `bge-m3`, CLS | 0.2968 (0.2917-0.3035) | 29.4% | 0.483 | 0.638 | 0.04 | 39.7 | 0.094 |

- **The gap is signal, not calibration.** A diagnostic Platt recalibration
  cross-fitted on Dev-iter by source group (it reads Dev-iter labels, so it is
  not a candidate result) reaches 0.175 for qwen3, 0.21 for bge and 0.137 for V6
  features. Selecting the epoch on Dev-iter itself still leaves every catalog
  model behind frozen V6 (qwen3 0.183, bge 0.217-0.222).
- **The FPR failure depends on domain.** qwen3 flags 87% of PLOS and 77% of
  Gutenberg humans, against 27% and 39% for frozen V6 features. It is better
  than V6 features on mixed rows (0.276 against 0.328 Brier) and on four of
  seven generators, so it carries some complementary signal.
- **The Worker side works.** The heads have 68,571 parameters at d = 1,024. A
  dependency-free `no_std` Wasm module (3.4 KB, 4.5 KB with SIMD) matches
  PyTorch within 7.2e-7 on every Dev-iter output and runs at p50 0.022-0.032 ms
  in V8 (Node 26), with 3.6-12 ms instantiation. This was not deployed.
- **Precision drift is small; model updates are not covered.** Isotropic
  noise at fp16 scale changed Brier by at most 2e-5 and flipped 0-3 decisions.
  An in-place model update would need a canary check.
- **Pending:** the variant that passes the Workers AI `instruction` string to
  qwen3 was still extracting on CPU when this note was written. The conclusion
  would change only if it reached about 0.158 Brier with an FPR near 15%.

**Cost and latency.** At list price and about 300 tokens per document, the
embedding call costs about $0.0036 per 1,000 documents for qwen3 or bge-m3
($0.025 at 2,048 tokens) and $0.061 for bge-large. Embedding latency is
undocumented and was not measured.

**Verdict.** Rejected as *A*'s representation. Because catalog encoders cannot
be fine-tuned, this result is roughly the ceiling for a self-serve embedding
route. Using qwen3 only as an extra input to a fine-tuned encoder is untested,
and it would add a hosted drift surface to a model that is not Workers-only
anyway.

### 2. LoRA-adapted catalog LLM read through logprobs (prepared, unmeasured)

**Shape.** Fine-tune a LoRA adapter on a LoRA-capable base so that a fixed
prompt is answered with a label token, then read probabilities from the
returned logprobs. The prepared experiment uses `llama-3.2-3b-instruct`, chosen
for its 80,000-token context, the lowest listed price among the flagged bases,
a live call that returned top-5 logprobs, and a base without the beta suffix.
The adapter has rank 16 and alpha 32, targets `q_proj` and `v_proj` only (the
documented modules), and has about 4.6M parameters (about 18 MB). Training uses
QLoRA; scoring loads the base unquantized in bf16, as the service requires.

Two answer formats share one design:

- **Origin:** one token, `human`, `mixed` or `model`.
- **Share:** a two-digit token from `00` to `99`, each a single token. Involvement
  is 1 − P(`00`), model-only is P(`99`), and expected share is the probability-weighted
  mean of the values. That construction satisfies P(model only) ≤ share ≤
  involvement automatically.

Each format has five read-outs: full label-token probabilities, only the top 5
(origin) or top 20 (share) alternatives that the service can return, the same
with a temperature fitted on a Train holdout, the emitted token alone, and the
emitted token mapped to its Train-holdout rates.

**Data.** 11,092 Train documents of build `bad203665202878a` (5,503 human, 3,566
mixed, 2,023 model-only), a 908-document group-disjoint Train holdout for the
temperature and lookup fits, and the full Dev-iter (zero source-group overlap).
Documents average 297 tokens on Dev-iter (maximum 654), plus about 80 prompt
tokens.

**Status.** Not run. The job waits under the GPU lock behind the baseline
release and needs about 1.6 GPU-hours for both formats. Its quality is unknown;
the published frozen-probe literature on LM hidden states does not establish
calibrated Brier or share for a rank-16 adapter read through logprobs.

**Cost and latency.** About 378 input and 1-2 output tokens per Dev-iter
document. If the base model's list price applied, that is about $0.020 per
1,000 documents, or about $0.11 at 2,048 tokens. LoRA inference is free during
the beta, and whether adapter calls will be billed at the base rate is
unknown. Text generation is limited to 300 requests per minute per account.
Latency was not measured.

**Calibration.** Probabilities come only from an undocumented response field:

- The emitted token alone gives hard 0/1 outputs, which perform poorly under
  Brier. A lookup table restores only a few probability levels.
- Top-k truncation hides probability mass. Three origin labels fit in the
  top 5. The 100 share tokens do not fit in 20, so expected share must assign
  the missing mass by rule.
- `prompt_logprobs` can score a fixed answer by teacher forcing, which avoids
  truncation, but it needs one call per candidate answer or a different prompt
  layout.
- A calibration map fitted on the served outputs is valid only while the
  served base is unchanged. The observed `-v2` id shows that the base can move
  under the same name.

**Contract fit.** A single adapter answering origin, share and attribution
prompts shares one base and adapter, which can satisfy the one-model rule.
Attribution needs its own prompt or label vocabulary and is not in the prepared
experiment. Every public *A* artifact would become an adapter for one pinned
catalog base, which limits miner architectures to what LoRA can express. Each
*A* version would also follow the base's deprecation schedule and use one of
the 100 adapter slots per account. Validators do not need to reproduce scores
under the pilot rules, but independent execution would require the base
weights under their licence, and hosted numerics would not be reproducible
outside Cloudflare.

### 3. Hosted-LM token statistics as extra inputs (unmeasured)

Per-prompt-token logprobs and ranks from a base model, such as
`llama-3.2-3b-instruct` at about $0.0001 per 2,048-token document, could
provide GLTR-style features for the shared head: logprob quantiles, rank
histograms and low-surprisal run fractions. Binoculars-style statistics need
full next-token distributions from two models, which the service does not
return. This shape inherits the undocumented-field and drift risks of shape 2.
It also needs a full encoder or another representation beside it, so it adds
cost rather than replacing the encoder.

### 4. Clef decision models, zero-shot (unmeasured)

Clef returns probabilities natively ($0.24 per million input tokens; Clef-flash
$0.038, no output charge). It is not self-serve tunable, its AI-text-detection
calibration is unknown, and it cannot be a public miner artifact. At most it
could be a cheap external comparator.

### 5. Custom model on Workers AI (not available self-serve)

Hosting the fine-tuned encoder itself on Workers AI GPUs would keep the current
quality, keep text inside the Cloudflare boundary and plausibly answer in tens
of milliseconds. It depends on a Custom Requirements Form or design-partner
arrangement whose terms are undocumented. *A* could rely on it only if the
terms cover encoder support, a pinned immutable deployment, a declared numeric
profile, raw-output access and pricing.

### 6. Cloudflare compute outside Workers AI (measured elsewhere)

- **Container on CPU** ([CPU inference](a-cpu-inference.md)): the current fit
  as a mixed int8 ONNX Runtime build moves Brier by −0.0003 with 8 flipped
  decisions. It measured 0.44 s per 512-token document on one local core and
  0.96 s per 2,048 tokens on four. Busy cost is about $0.008 per 1,000 documents
  of about 308 tokens and $0.074 at 2,048 tokens. A warm `standard-4` costs about
  $0.11 per hour, and Cloudflare vCPU speed is unmeasured.
- **Distilled Worker-native encoder** (same note): Ettin 32M with Q8 weights
  fits resident in about 69 MB at 2,048 tokens. It is projected at 3.4-4.2 s of
  Worker CPU and about $0.08 per 1,000 documents of 2,048 tokens. Its quality
  result is optimistic, uses origin distillation only, and drops attribution
  accuracy from 0.191 to 0.175.

### 7. Split hybrids (rejected)

Examples include embeddings for involvement with an LLM adapter for share and
attribution. These break the rule that one shared learned encoder feeds all
outputs. They turn the share-between-model-only-and-involvement ordering into a
clamp after the fact, and they stack two hosted drift surfaces.

## Comparison

Costs are list prices per 1,000 documents of busy time or tokens, excluding free
allowances, request fees, idle capacity and cold starts. For scale, the
[detection price hypothesis](cloudflare-subnet-draft.md#initial-cost-scenarios-and-detection-price-target)
is $0.02 per 1,000-word document, or $20 per 1,000 documents. Every shape costs
less than 1% of that.

| Shape | Dev-iter Brier | Human FPR | $ per 1,000, about 300 tokens | $ per 1,000, 2,048 tokens | Latency | Who controls calibration | Effort | Main risk |
| --- | ---: | ---: | ---: | ---: | --- | --- | --- | --- |
| Fine-tuned encoder, Container CPU int8 | 0.1481 | 7.9% | 0.008 | 0.074 | 0.44 s / 0.96 s measured locally (512 tokens, 1 core / 2,048 tokens, 4 cores); plus 1-3 s cold start | us, on the served build | 1-2 engineer-weeks plus Container qualification | vCPU speed and idle cost unmeasured |
| Distilled 32M encoder, Worker | 0.1422 (optimistic) | 5.6% | not measured | 0.08 | 3.4-4.2 s projected per 2,048 tokens | us, on the Q8 build | 2-4 engineer-weeks runtime plus a full protocol run | quality on long, adversarial and new-generator slices unknown |
| Catalog embedding plus Wasm heads | 0.1838 at best | 39.5% | 0.0036 | 0.025 | head 0.03 ms; embedding call unmeasured | us for the head; the vector can change | days | fails quality and FPR |
| LoRA on `llama-3.2-3b-instruct`, logprobs | not measured | not measured | 0.020 if billed at list price | 0.11 | unmeasured | partly: depends on an undocumented field and a hosted base | 1-2 weeks after a positive measurement | undocumented logprobs, base drift, LoRA deprecation |
| Hosted-LM statistics as extra inputs | not measured | not measured | small, on top of an encoder | about 0.1 extra | unmeasured | partly | moderate | as LoRA, without removing the encoder |
| Clef zero-shot | not measured | not measured | about 0.012 (flash) | about 0.08 (flash) | 39 ms median published | Cloudflare | low | not tunable; not a miner artifact |
| Custom model on Workers AI | as the fine-tuned encoder | as the fine-tuned encoder | unknown | unknown | GPU-class, unmeasured | us, if raw outputs are returned | unknown | availability and terms |

## Calibration implications

- **Calibrated probability needs real model outputs.** Text answers, JSON
  answers and emitted label tokens are decisions, not probabilities. Every
  Workers AI route that produces a usable probability does so through
  logprobs, which are undocumented on the LoRA-capable bases, or through Clef,
  which cannot be tuned.
- **Calibrate on the served build.** The calibration map must be fitted on the
  outputs of the exact served numeric profile: int8 ONNX in a Container, Q8 Wasm
  in a Worker, or a pinned hosted model. It must not be fitted on fp32 training
  outputs.
- **Hosted dependencies need a canary.** Any Workers AI model that feeds *A*
  needs a fixed synthetic canary set scored before each cohort or serving
  window. The requested and returned model ids must be recorded, and any change
  must pause scoring until recalibration. The embedding drift probe shows that
  precision noise is harmless; silent model replacement is the remaining risk.
- **Pooled vectors limit share.** One pooled vector per document gives share
  no token-level evidence. This matters more once sentence-level share becomes
  an extension.

## Recommendation

1. **Keep *A* off Workers AI and launch with our own fine-tuned encoder in a
   Cloudflare Container on CPU.** Use the mixed int8 ONNX build from the
   [CPU inference](a-cpu-inference.md#recommendation) recommendation, with the
   exact f32 graph as the replay reference. Run the numeric heads in Rust,
   either in the same Container or in a Worker. This keeps text inside the
   Cloudflare boundary, keeps calibration under our control, and costs about
   $0.008 per 1,000 Dev-iter-length documents of busy time.
2. **Make a distilled Worker-native *A* the all-Workers target.** It is the only
   measured route that runs inside Workers. Distil the released *A* with all
   heads, run three seeds with a fixed selection split, and calibrate on the Q8
   Wasm build. It must pass the full protocol, including a long-document slice.
3. **Run the prepared LoRA measurement as a bounded comparator** when the GPU is
   free (about 1.6 GPU-hours, after the baseline release). Treat a LoRA-adapted
   catalog LLM as a candidate *A* shape only under three conditions: its logprob
   read-out beats the fine-tuned encoder on Dev-iter Brier, its human FPR is no
   worse, and Cloudflare documents logprobs for that base. Otherwise record the
   result and do not pursue the route.
4. **Ask Cloudflare about custom-model hosting as a parallel operator
   decision.** This route needs no change to *A*. Adopt it only after the terms
   in [shape 5](#5-custom-model-on-workers-ai-not-available-self-serve) are
   documented, with its own qualification.
5. **Do not pursue** catalog embeddings as *A*'s representation, Clef as *A*, or
   split hybrids. Workers AI stays the language-model service for ambiguity
   resolution and later *B* proposers.

## Changes to the A training plan

- **Backbone unchanged.** Workers AI gives no reason to change the baseline
  release recipe (full fine-tune of ModernBERT-base, 2,048 tokens). No catalog
  model can host it or replace it.
- **Served-build calibration.** Fit calibration on the deployed numeric profile.
  Record the profile, memory, latency by length and core count, and the gate it
  passed with each release. Keep MLP Wo in higher precision or add
  quantization-aware training for that input
  ([CPU inference](a-cpu-inference.md#consequences-for-training)).
- **Distillation stage after release.** Add a pre-registered Worker-sized
  student run distilled from the released *A*, with involvement, share and
  attribution targets. Compare it with a base model trained on the same data,
  and keep 8-bit embeddings.
- **Long-document evaluation.** Dev-iter documents stay under 700 tokens. Every
  served build and student needs a 1,024-2,048-token slice and the declared
  windowing rule for longer customer documents.
- **LoRA arm as a diagnostic only.** If run, keep it on Dev-iter with the
  pre-registered read-outs above. Add an attribution prompt before treating it
  as a full *A*. It cannot use Dev-select, CAL or Test unless a protocol
  amendment admits it as a candidate.
- **No catalog-embedding arm.** Drop re-basing onto catalog embeddings. A
  qwen3-plus-encoder fusion is low priority because it adds a hosted
  dependency.
- **Canary set.** Prepare a fixed synthetic canary set and a re-fit trigger, to
  be used whenever any hosted model enters *A*'s inputs.

## Changes needed in the Cloudflare design

These are listed for a separate edit of
[the Cloudflare design](cloudflare-subnet-draft.md); this note does not change
it.

- **Ownership and execution.** Keep the statement that Workers AI is not a host
  for arbitrary uploaded architectures, and make it precise:
  - self-serve customization is LoRA on the nine flagged text-generation bases,
    with no encoder or classification adapters;
  - custom-model hosting is not self-serve;
  - replace "Small numeric *A* scorers ... Workers are the initial target" with:
    the *A* encoder runs in a qualified Container on CPU, the numeric heads run
    in Rust/Wasm, and Worker hosting of the whole *A* requires a smaller
    qualified model.
- **Rust participants and serverless coordination.** Name *A* inference as a
  Container workload with measured bounds: no GPU instance types, 1-3 s
  documented cold start, and a warm pool driven from a Durable Object.
- **Workers resource limits.** Record the measured head figures: a 3.4-4.5 KB
  Wasm module, p50 about 0.03 ms, and 7.2e-7 maximum deviation from the
  reference. Record also that ModernBERT-base cannot be resident in a Worker.
- **Models and version registration.** *A* artifacts are encoder artifacts
  executed in our runtime, not Workers AI models. If a hosted model ever feeds
  *A*, the execution manifest must pin the requested model id, record the
  returned id and canary output digests per cohort, and pause scoring on a
  mismatch. Silent served-id changes were observed.
- **Initial cost scenarios.** `@cf/meta/llama-3.1-8b-instruct-fp8-fast` and
  `@cf/mistral/mistral-7b-instruct-v0.1` are absent from the live catalog; the
  latter was deprecated on 2026-05-30. Replace both tariff proxies with current
  models and recompute. Replace the $0.00001 numeric detection allocation with
  the measured Container busy cost ([CPU inference](a-cpu-inference.md#cost-per-1000-documents)).
- **Scale and rollout, LoRA paragraph.** List the nine flagged bases, including
  `llama-3.2-3b-instruct` with 80,000 tokens of context, which matters for long
  *B* jobs. Record that `-lora` variants may be deprecated, that adapter assets
  cannot be edited, and that there are 100 adapter slots per account. Record
  that logprobs are observed but undocumented on these bases, so neither *A* nor
  *B* may depend on them without documentation.
- **Evidence table.** Replace "Native Rust/MKL passes fixed references but
  exceeds Workers memory; Container serving is unqualified" with references to
  [CPU inference](a-cpu-inference.md) and this note.
- **Privacy and the provider boundary.** No change for a Container-served *A*,
  which stays inside Cloudflare. AI Gateway routing is opt-in by gateway id,
  which is consistent with the direct route the design specifies.

## Limits

- The catalog-embedding vectors came from local open-weight checkpoints. Parity
  with the served vectors (pooling, precision, instruction handling) was not
  checked, although a few synthetic live calls would settle it.
- The embedding heads used a human-heavy Train subset with 48-535 positives per
  local generator. That limits attribution for every input equally and could
  narrow, but is unlikely to close, a 0.09 AUROC gap.
- The LoRA route, hosted-LM statistics and Clef have no quality measurement.
  The logprob observations come from a handful of synthetic calls; their
  maximum `top_logprobs`, determinism, and behaviour under streaming or the
  batch API are unverified.
- Container and Worker figures come from one shared desktop CPU. Cloudflare
  vCPU and workerd speed, memory accounting and cold starts were not measured.
- Prices are list prices on 2026-10-28. The LoRA price after the beta is
  unknown.
- The embedding results apply to build `bad203665202878a`. Later Train builds
  add positives but do not change catalog encoders' lack of fine-tuning.

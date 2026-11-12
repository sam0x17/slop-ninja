# Shared A research inference contract, v1

The executable contract is `src/shared.rs`. The versioned weight artifact is
`slop_ninja_shared_a_weights_v1`; its operator list is exactly `linear`, `tanh`,
`softmax`, `sigmoid`. Numeric JSON tensors carry explicit shapes and row-major
values. The current model uses float32 training and portable float64 Rust
evaluation. Profile rosters, calibration temperature and conditional named-match
thresholds travel with the weights. A checkpoint does not confer quality approval.

The [numeric input schema](schemas/shared-input-v1.schema.json) describes
`SharedInput`: a finite numeric vector `x` and `contract_sha256`. The vector length
must equal the artifact's input dimension and its digest must match the artifact.
The digest is SHA-256 of compact, sorted-key `serde_json::Value` serialization of
the frozen numeric contract, not necessarily the pretty JSON file's byte digest.
That contract binds grammar/document-engine identities, resolution, pool and
feature meanings, Train-fitted vocabulary, normalization, denominator-mask
centering, aggregation and source bindings. Metadata fields are not learned
coordinates. An incompatible exporter or transform fails instead of guessing.

The [prediction schema](schemas/shared-prediction-v1.schema.json) describes the
portable Rust output. The additional semantic constraints are:

- `origin` contains `human_only`, `model_only`, `mixed`, summing to one.
- `ai_confidence_percent = 100 * (origin.model_only + origin.mixed)`.
- `ai_share_percent = 100 * (origin.model_only + origin.mixed * sigmoid(s))`,
  where `s` is the mixed-share head's logit from the shared encoding.
- Thus `0 <= 100 * origin.model_only <= ai_share_percent <= ai_confidence_percent <= 100`.
- Human involvement is `origin.human_only + origin.mixed`; AI involvement is
  `origin.model_only + origin.mixed`. Both conditional roles are always returned.
- Each role returns the highest qualifying named profile, or `other_human` /
  `other_ai`. Thresholds apply to conditional named scores, never to involvement.
  `fallback_asserts_presence` is always false. Conditional scores describe the
  supported named roster; they are not a calibrated probability of gallery absence.

These cross-field equalities and artifact-dependent roster/dimension constraints
are runtime invariants, beyond what the standalone JSON schemas express. An
execution failure produces an error, not an `unknown` attribution or probability.
AI confidence concerns any AI involvement, not accuracy of the share estimate.

`shared_predict` reads JSONL envelopes `{id,input}` and writes `{id,prediction}`.
The ID is passed through for correspondence and never enters the model. PyTorch
research prediction files may additionally retain a learned `encoding` for the
separately versioned reference-retrieval diagnostic; it is not part of this Rust
prediction schema. Neither references nor private profile names are published.

The selected architecture, exact private artifact bindings, reference predictions,
conformance tolerance, commands and measured limits are in
[SUPERVISED_BASELINE.md](SUPERVISED_BASELINE.md). Numeric inference is CPU-only and
the library cross-compiles to Wasm. Complete Workers execution and the native
parser's serverless memory/runtime requirements remain to be qualified.

## Full A research contract (E1 full encoder, E2 token span), v2

E1 fine-tunes every V6 ModernBERT-base tensor except the original classifier,
which stays byte-identical and is used only for the initial check. E2 adds a
token-span involvement head. Both keep the stage-3 shared heads (`2588 -> 64
tanh -> 768` residual, origin, mixed share, human, AI) and the joint loss:
origin cross-entropy plus involvement Brier, share MSE and a weighted
conditional role cross-entropy, each divided by its global supervised weight.
At step 0 every arm reproduces V6's origin probabilities. Every output,
including the two document percentages and both conditional role results,
keeps the contract above.

### Training bundle (shared contract)

Rust assembles a bundle directory; the trainer only reads it:

- `rows.jsonl`, one row per document, with exactly these fields: `id`,
  `split` (`train`, `train_mining`, `dev_iter`, `dev_select`, `cal`; the
  aliases `development_iter`, `development_select`, `calibration` and the
  hyphenated forms are accepted), exactly one of `text` (exact text) or
  `text_ref` (`{path, offset, length}`: UTF-8 bytes in a file, the path
  relative to the bundle), `text_sha256`, `origin` (`human_only`, `mixed`,
  `model_only`), `share` (measured; 0 for human-only, 1 for model-only,
  in (0,1] for mixed, where 1 is a full rewrite of a human source),
  `ai_byte_spans` (sorted disjoint
  `[start,end)` UTF-8 byte replacement spans, or null), `generator_label`
  (the canonical attribution label: the frontier admission label such as a
  reported model id or `requested:<selector>`, a local family id, null for
  humans), `cohort_id`, `cohort_weight`, `source_group`, `domain`, `genre`,
  `voice_id`, `voice_cluster_id`, `voice_origin`, `style_axes` (null or a
  list of persona-axis names; objects are refused because their keys mix
  axes with other metadata), `operation`, `held_out_style` and
  `provenance_sha256`. Unknown fields are refused.
- `weights.json`: `{"base_weighting": "family_origin_balanced", "cohorts":
  {cohort_id: {"policy", "default", "labels": {label | "human": {"records",
  "weight"}}}}}`. Counts are Train rows; every Train row's `cohort_weight`
  must equal its cohort/label weight. Overrides are applied by the assembler
  and only recorded here. A bundle projected with final assembler weights
  instead has `"base_weighting": "assembler_row_weight_v1"`, a top-level
  `train_weight_sum`, and per cohort and label `{"records", "weight_sum",
  "weight_min", "weight_max"}`; the trainer checks each by its Train rows
  (zero-weight rows count with weight 0). The weights.json form must match
  the protocol's `train_weighting` scheme in both directions.
- `MANIFEST.json`: inputs with SHA-256 and counts per split x domain x
  origin x generator. The trainer binds its digest.

A bundle containing any Test row is refused. Rows of splits that are not
requested are skipped before their text is resolved. Train refuses
`held_out_style` rows; a zero cohort weight removes a row from fitting and is
counted in the log.

A trainer bundle (`slop_ninja_a_trainer_bundle_manifest_v1`, written by
`slop_ninja_a_assemble trainer-bundle`) holds one block per split, in the
order Train, Train-mining, Dev-iter, Dev-select, CAL, counted by the
MANIFEST's `rows_by_split`. The reader parses only the requested blocks and
stops after the last one, so a Dev-select or CAL block that a fit does not
open is never parsed or hashed (at most one read buffer past the last
requested block is loaded and dropped). Each kept line must name its block's
split in its bytes before it is parsed or hashed, so a count that overstates
a block is refused on the first line of the next block. A requested block
behind an unrequested Dev-select or CAL block is refused. When such a block
exists, the fit binds the MANIFEST and weights.json digests, the rows digest
recorded in the MANIFEST (not recomputed) and the digests of the blocks it
read, in place of the whole rows file's digest
(`full_a_bundle.bound_digests`). Every other bundle binds the three
whole-file digests. A refused `--report-split` request is refused before the
bundle is read.

Export (`export_full_a.py` `completed_fit`, and so `raw`, `score-rows`,
`export` and `mine_full_a.py`) and `public_a.py select` recompute the digests
the binding recorded (`full_a_bundle.binding_digests`): the blocks the fit
read for a block-bound fit, else the whole files. Limits of a block-bound fit:

- CAL comes after Dev-select in the block order, so `raw --part cal` and
  `offsets --part cal` are refused on a bundle that holds an unopened
  Dev-select block (as is any read of CAL there); CAL is calibrated from a
  bundle without that block.
- `raw` and `score-rows` manifests record the fit's block digests. The Rust
  tools that compare a manifest or binding with the three whole-file digests
  (`calibrate_full_a`, `calibrate_fixed_a`, `full_a_v2` and mining) refuse
  them; the first build that calibrates or mines such a fit extends them.
- `offsets` on a bundle part still hashes the whole rows file for its
  `source_sha256`.

Two run options serve protocol A2's P0 pilot (both off by default; with both
off the training computation is unchanged):

- `--train-allowlist FILE` restricts Train to the ids of a
  `slop_ninja_a_row_allowlist_v1` file (from `a_r2_rights snapshot
  --allowlist`), whose population must be the bundle's Train source
  (`source_splits.train.rows_sha256`). Kept rows keep their weights (no
  refill, no rebalancing); the protocol's train weighting then normalises and
  caps over them. weights.json is still checked over every Train row. The
  contract's AI roster stays the bundle's Train labels, and labels with no
  allow-listed row stay unsupervised (`fit.json` `ai_roster`).
- `--final-step` takes the model after the last scheduled step, with no
  checkpoint selection or early stopping. Dev-iter is scored once, after
  training; the evaluation boundaries still write resume checkpoints and the
  training terms (`train-log.json`). It takes no `--init-fit`.

`bitwise_full_a.py` is the D3 smoke harness. It builds a smoke trainer bundle
from a trainer bundle's Train block alone (at most 1% of its rows, an unkeyed
row rule, a Dev-iter carved from whole Train source groups), runs R0's
trainer (extracted from its commit and checked against R0's binding digests)
and the candidate trainer with every option off on it, and compares every
optimizer step's loss terms and gradient norms, the Dev-iter history, the
weight log, the selected tensors and the raw outputs bit for bit. A D3 pass
also needs every input pinned: the smoke rebuilt from its source, at most 1%
of the Train lines, the R0 binding file at its expected digest with the
reference tree's modules and both runs' environment (torch, CUDA,
transformers model source, device) equal to it, the source MANIFEST,
weights.json, Train block, contract and protocol at their expected digests,
and the real encoder. `compare` exits 0 on a D3 pass, 1 when the runs differ
and 3 when they are equal but a D3 condition fails; `BITWISE.json` records
each condition with the expected and observed digests.

The config directory holds `contract.json`
(`slop_ninja_full_a_contract_v2`: parent artifact, `bundle_manifest_sha256`,
an empty human roster, the AI roster equal to the sorted Train generator
labels, and the Train-only normalization of the frozen V6 h fitted in Rust)
and `protocol.json` (`slop_ninja_full_a_protocol_v2`). The span policy is
`slop_ninja_token_span_label_v1` on the exact final text.

Token labels come only from non-whitespace characters. A token whose characters
are all inside model-produced replacement spans is labelled 1; one with all of
them outside is 0. A token that straddles a boundary is unlabelled. So are
special tokens and whitespace-only tokens. Human-only rows are all 0 and
model-only rows all 1. Mixed rows without measured spans carry no token loss.
Token BCE is the mean over each document's labelled tokens, with that row's
weight divided by the token-supervised mass.

Streaming: rows are parsed line by line; token ids (int32), labels (int8) and
content/word flags (one byte) are packed into flat arrays with row offsets,
and text is dropped after tokenization, so memory is about 6 bytes per token
plus per-row metadata.

### Weights and attribution

Train row weights follow the protocol's `train_weighting`:

- `family_origin_balanced` (positive mass null): the family/origin-balanced
  weights (each `source_group x origin` cell equal within its group, groups
  equal) times the row's cohort weight. The class mass follows the group
  structure: on build 5776ef6d0df9ef59 the 9,987 AI-involved Train rows sit
  in 694 of 35,872 source groups and receive 1.2% of the weight.
- `class_balanced` with `positive_mass` m: humans and AI-involved rows are
  balanced separately (groups equal within the class, origin cells equal
  within a group), times the cohort weight, then the AI-involved class is
  scaled to mass m and humans to 1 - m.
- `assembler_row_weight_v1` (positive mass null): each Train row's
  `cohort_weight` is its final assembler weight (declared origin shares,
  group/document balance and frontier label balance already applied), used
  as the loss weight without any rebalancing. `mine_full_a` bundle extension
  still writes the per-label factor form only.
- Row caps (both row-form schemes, `assembler_row_weight_v1` and
  `label_domain_balanced_v1`): `max_row_weight_ratio` r bounds every row at
  r times its class mean, and `max_row_weight_global_ratio` g bounds it at
  g times the mean weight of all Train rows. Both caps water-fill within
  each class, so every class keeps its mass and the order of its rows. A
  global cap below some class mean cannot keep that mass, and the trainer
  refuses it.

Weights are normalized to sum to 1. Dev-iter and other evaluation splits use
the family/origin-balanced weights only, so the selection metric is the
protocol's. The attribution head
is the conditional AI-role head (`linear 768 -> |AI roster|`, softmax) over
the canonical generator labels, frontier models included, with loss weight
`attribution.loss_weight`. Before the first optimizer step the trainer writes
`cohort-weights.json` (`slop_ninja_full_a_cohort_weight_log_v2`): the
weighting, the effective mass per origin, and per cohort and per label the
declared records and weight, whether it is the default, the trained records
and the share of loss mass. `fit.json` binds its digest; the
exporter and the mining scorer refuse a fit without it.

### Frozen protocol (`slop_ninja_full_a_protocol_v2`)

- seeds, maximum epochs, logical batch, padded microbatch token budget;
- AdamW peak encoder and head learning rates, layer-wise decay, weight decay,
  betas, eps, and two clipping norms: `gradient_clip_norm` for the main model
  (encoder, exported heads, span head; the reversed adversary gradient that
  reaches the encoder belongs here by design) and `aux_gradient_clip_norm`
  for the training-only heads (adversary, projection, share bins). Both
  pre-clip norms are logged per step;
- schedule `warmup_decay_horizon`: linear warmup over
  `warmup_fraction_of_total_steps` of `max_epochs x steps per epoch` (so
  changing H never changes the warmup), then `linear` or `cosine` decay to
  `floor_fraction` at `horizon_epochs` H, constant afterwards; warmup must end
  before H;
- bf16 autocast with FP32 master weights (CUDA only), or FP32; CPU runs must
  use FP32, because bf16 autocast on CPU did not finish one epoch of 128 rows
  in 71 CPU-minutes; gradient checkpointing; eager attention;
- early stopping and checkpoint selection on uncalibrated balanced
  involvement Brier on Dev-iter only (`split: dev_iter`), with evaluations
  per epoch, patience and minimum delta;
- the CUDA cap, the attribution loss weight, the Train weighting and the
  arms.

An arm sets `span_head`, `share_mode`, `share_supervision`, `batching`
(`random`, or `voice_mixed` with `per_generator`), `style_adversary` and
`contrastive` (each null or configured). Selection and reporting stay
separate: `--report-split` (`dev_select` or `cal`) is read once after the
checkpoint is frozen, and only with `--consume-sealed-split` naming the same
split and a new `--use-record` file, written (with the binding and checkpoint
digests) before the split is opened. `fit.json` and the binding record the
Dev-iter rows digest (`dev_iter_rows_sha256`: sorted id, text digest, origin,
share, generator label and source group).

**Share supervision.** The document share MSE reaches the conditional share
q only through P(mixed), so a model unsure between model-only and mixed
barely learns q. In the end-to-end smoke (E1 head, no extra term) full
rewrites (mixed, share 1, spans covering the whole text) were underestimated
by 37.9 points and the [50,90) bins were low. `share_supervision` adds:

- `conditional_loss_weight`: BCE between the conditional share (q in head
  mode, the word-token mean in the token modes) and the realised share on
  mixed rows only, divided by the mixed supervised mass;
- `bins` (null or `{loss_weight, hidden}`): a training-only head on z that
  classifies mixed rows' realised share into (0,10), [10,25), [25,50),
  [50,75), [75,90), [90,100) and exactly 100, so the encoding carries the
  share level. It is never exported.

The span-head arms (`word_token_mean_projected`) read share from the token
mean, which is trained on every token of a full rewrite; they are the
recommended share route, with the conditional term on. The trainer writes `dev_iter-raw.jsonl` (raw heads for Rust CAL and
replay) and `dev_iter-predictions.jsonl` in the evaluator format
(`slop_ninja_a_prediction_v1`: `id`, `p_model_only`, `p_mixed`, `p_human`,
`share`, `attribution` as the conditional distribution over the AI roster).
History rows carry the training terms, the reversal scale and text-free
Dev-iter diagnostics (human flags at 0.5, attribution accuracy per label and
on held-out-style rows).

### Training-only style invariance (`full_a_aux.py`)

- **Style adversary.** A gradient-reversal layer on the shared encoding z
  feeds an MLP (or linear) adversary with a voice-cluster softmax and
  independent style-axis sigmoids. Targets come only from styled AI rows:
  humans carry no voice, so a styled/unstyled target would teach the encoder
  to hide AI involvement. Clusters with fewer than `min_cluster_rows` Train
  rows are not targets. The head minimizes its loss; the encoder receives
  `-lambda` times that gradient, with `lambda` from the reversal schedule
  (`constant`, `linear_ramp` or `dann`: `max * (2 / (1 + exp(-gamma p)) - 1)`
  over `ramp_epochs`, after `delay_epochs`).
- **Supervised contrastive loss.** A projection head (`768 -> hidden ->
  dim`, L2-normalized) on z. For an AI anchor, positives are documents of the
  same generator in another voice; negatives are other generators in the same
  voice and humans; same generator and voice is neither. Anchors need at
  least one of each. The loss is computed over the whole logical batch with a
  gradient cache: one no-graph pass collects z, the loss and dL/dz are taken
  there, and the training microbatches are rerun under the same RNG state
  with dL/dz injected. It is exact across microbatches (tested) and costs one
  extra forward per step.
- **Voice-mixed batching.** Each logical batch takes its proportional share
  of humans (cumulative rounding, never negative). Its AI slots are dealt in
  chunks of `per_generator` rows by earliest deadline: the next chunk goes to
  the live generator whose chunk midpoint `(taken + chunk/2) / total` is
  smallest (seeded ties), so every label, small frontier labels included, is
  spread evenly through the epoch in proportion to its size. Within a batch,
  voices shared by two or more of its generators are taken first, then
  distinct voices within a generator. Humans fill a batch only when no AI row
  is left. Every row appears exactly once per epoch and every batch but the
  last is full, so the epoch objective is unchanged; the fit records the
  fraction of AI rows that are contrastive anchors. On the Train label and
  voice metadata of build 5776ef6d0df9ef59 (53,226 rows, 9,987 AI rows, 20
  labels), batch sizes 32, 64 and 128 with 2-4 rows per generator all gave a
  permutation, each label's mean epoch position 0.498-0.501, the first row
  of every label within the first 1.1% of the epoch, and 99.6-99.9% of AI
  rows as anchors (the earlier largest-remaining rule failed on these counts
  and put the median frontier row at 0.64 of the epoch).

Neither head is exported, replayed or read by the involvement, share or
attribution outputs; at step 0 they change no output.

### Hard-negative mining (`mine_full_a.py`, `examples/mine_full_a.rs`)

1. `mine_full_a.py` scores a splits `train-mining/documents.jsonl` with a
   completed fit (uncalibrated P(any AI); text-free `scores.jsonl`). Every row
   must have `frozen_split` and `split_assignment.split` equal to
   `train_mining`; optional deterministic SHA-256 subsampling. The first
   invocation writes `binding.json` (fit.json and selected-checkpoint
   digests, bundle and pool digests, sample rule, precision, token budget,
   implementation digest); a resume under any other binding is refused, and a
   partial last line from a crash is cut off. CPU scoring is FP32 only.
2. `mine_full_a select` requires a v2 score manifest whose binding names the
   bundle being extended. `--splits DIR` derives every exclusion source from
   the splits directory's SHA256SUMS and requires all of them: the CAL,
   Dev-iter, Dev-select and Test manifests of every part directory (each part
   complete), every diagnostic slice manifest and every reserved evaluation
   group or family list, each checked against its digest; an unrecognised
   manifest is refused. It ranks by P(any AI) (ties by SHA-256 of seed and
   id), keeps rows at or above `--min-p` (default 0.5: mined humans are false
   positives), drops ids and texts already in the bundle and any document,
   source family or writer of those sources, caps rows per family and keeps
   the top N. It writes `mined-rows.jsonl` (Train,
   human-only, cohort `mined_humans_round_K`, exact pool text, provenance
   digest) and appends `round-K-requests.jsonl` to a file queue (`QUEUE.json`)
   with one matched-positive request per kind (`mirror`, `mixed`) per mined
   human: parent id and text digest, family, writer, domain, word count and a
   seeded generator rotation. Nothing is generated here.
3. `mine_full_a extend` appends the mined rows, and once generated the
   matched positives as shared-contract rows in cohort
   `mined_matched_positives_round_K`, declares their cohorts in
   `weights.json` and recounts `MANIFEST.json`.
4. `mine_full_a stop` reads the seed fits and the bundle of each round
   (`--round-fits` and `--round-bundle` pairs; round 0 is the pre-mining arm)
   and applies the protocol rule: stop after the first round whose mean
   Dev-iter balanced Brier falls by less than 0.001, and after three rounds at
   most. It first checks that the rounds are comparable: every seed fit of a
   round was trained on that round's bundle; the seed set, arm, arm
   definition, protocol digest and Dev-iter rows digest are the same in every
   round; and round k's bundle descends from round k-1's through `extend`
   steps that append only round-k mining cohorts, including
   `mined_humans_round_k`.

### Replay operators for the new heads

The replayed document model keeps the involvement, share and attribution
heads and their operators (`linear`, `tanh`, `softmax`, `sigmoid`, the span
operator below); the attribution head is the `ai` linear head over a larger
roster. `src/full_a.rs` reads heads `slop_ninja_full_a_heads_v2` and the
pre-spans heads `slop_ninja_full_a_heads_v1`, which has no `token_head` or
`span_emission` key and the same document operators. It accepts an empty human
roster (0, or 2..=4096 names) and returns `other_human` with no named scores
for it. The `full_a` replay binary reads v3 exports
(`slop_ninja_full_a_export_v3`: heads v2, calibration v3), v4 exports
(`slop_ninja_full_a_export_v4`: heads v3, calibration v3 and
`tower.safetensors`, the spans-S2 span tower) and v2 exports
(`slop_ninja_full_a_export_v2`, the R0 release package: heads v1, calibration
v2, no token head). The pairing, and whether the span tower runs, follow the
export's declared schema only. It skips the supervision requirement of an
empty role and accepts a `roster_empty` role with threshold 1 in the
calibration record.

Span localization (protocol spans-v1, `src/full_a_spans.rs`, Python reference
`grammar-eval/python/full_a_spans.py`) adds one operator: the optional token
head `t = w2 . gelu_tanh(W1 x + b1) + b2` on the final-norm token states,
which never feeds the document heads. Its logits are emitted as share words
(Python `str.isspace` runs), word probabilities (mean token probability over
overlapping word tokens), a share tilt that makes the mean word probability
equal the emitted document share, sealed-splitter sentences with AI share and
confidence percents, and decoded spans (`slop_ninja_span_decoder_v1`) with
exact character and UTF-8 byte offsets. Heads v2 carries the emission spec
(decoder and sentence/span confidence maps); calibration v3 carries the CAL
version of it. Golden vectors in `tests/golden/span-golden-v1.json` (synthetic
text and logits) bind the Rust and Python implementations.

Heads v3 (`slop_ninja_full_a_heads_v3`, protocol spans-S2 arm S2c) has the same
document heads, token head and emission spec. The token head reads a span
tower instead of the final-norm states (`input:
span_tower_final_norm_token_states`). The `span_tower` declaration names the
copied encoder layers (ModernBERT-base: layers 18–21, attention global, local,
local, global, as the encoder assigns them), the capture point (the hidden
state entering layer 18), the encoder's masks and positions, the copied final
norm and the weights file. The runtime (`inference/a-v6-cpu`) runs the encoder
unchanged, then the copied layers from the captured state with the same masks
and positions, the copied norm and the token head. Document outputs never read
the tower. Only heads v3 carries `span_tower`; heads v1 and v2 never do.
`src/full_a_tower.rs` reads heads v3 and the tower declaration. It is a
separate file from the heads runtime that the served profile and the
artifact-check spec pin by digest (`full_a.rs`, `full_a_spans.rs`,
`involvement_calibration.rs` and `shared.rs`), and it leaves those files
unchanged. It dispatches on the declared heads schema only. Heads v1 and v2
bytes go to `full_a.rs` unchanged. For heads v3 it checks the schema, the
token head's input and the declaration, then gives `full_a.rs` the same
object read as heads v2, with every other member's exact bytes. The token
head is the same function and reads the tower's states.
Golden vectors in `tests/golden/span-tower-golden-v1.json` bind the Rust branch
to the Python function. They come from the trainer's own tower module on a
tiny 22-layer ModernBERT with generated dyadic weights, evaluated by the
export's FP64 reference function, and they record the final-norm states, the
tower states and the token logits.

`examples/prepare_full_a_contract.rs` writes `contract.json` for a bundle:
the MANIFEST digest, the empty human roster, the AI roster (sorted Train
generator labels with a positive cohort weight) and a Train-only V6 h
normalization taken from a given file, whose digest it records. With
`--teacher-class NAME` it writes a teacher class's contract instead
(`grammar-eval/python/teacher_a.py`): `teacher_class` is recorded, and
`raw_dimension` and the normalization's dimension are the class's encoder
width. A teacher fit is never exported, so such a contract is never published.

`examples/fit_full_a_normalization.rs` fits that normalization fresh over a
bundle's Train rows with a positive cohort weight:

```text
fit_full_a_normalization requests --bundle B --out NEW [--shards N] [--sample-keep K --sample-modulus M]
v6_embeddings.py --artifact V6_CPU_ARTIFACT --requests NEW/requests-NNN.jsonl --output RUN_NNN   (per shard)
fit_full_a_normalization fit --plan NEW/plan.json --run RUN_000 ... --out NORMALIZATION.json
```

`requests` writes `{id, text}` shards and a plan binding the bundle digests,
the sample rule (whole source groups, SHA-256 of a fixed seed and the group)
and every planned row. `fit` requires one complete extractor run per planned
shard (its request snapshot equal to the shard), every planned row once with
status ok and the planned text digest, and one representation contract; it
computes the equal source-family mean and variance in FP64 over two passes,
scale `max(sqrt(variance), 0.01)`, as the stage-3 fit did. The runs declare one
dimension (768 for the V6 h; a teacher class's width for runs of
`teacher_a.py embed`), and every embedding must have it. The extractor runs
in the pinned V6 CPU runtime (torch 2.14.0+cpu). Measured on build-host with
10 real Train rows: about 5 ms per token single-threaded (1-2.3 s for 208-439
tokens), so the full 53k-row Train is about 30 CPU-hours unsharded; shard it
or sample groups.

### Measured CPU smoke, 2026-10-27

build-host, CPU only, torch 2.9.1, synthetic text only; nothing here is a
quality claim. `smoke_full_a_aux.py`:

- **Streaming load.** 100,000 synthetic rows (text through `text_ref`,
  233 tokens per row on average with the V6 tokenizer, 12 generator labels,
  900 voice clusters) loaded into a Train population in 58.6 s (about 1,700
  rows/s, single thread). Packed arrays took 140 MB; process peak RSS was
  1.07 GB, of which 0.79 GB was reached before loading (the synthetic writer).
  The voice-mixed epoch order took 0.6 s; every AI row in the first 200
  batches of 32 was a contrastive anchor.
- **Loop.** A tiny random ModernBERT fitted the arm with span head,
  voice-mixed batching, style adversary and contrastive loss, scored a
  40-document synthetic Train-mining pool, `mine_full_a select` excluded the
  evaluation family and writer and capped families (10 mined, 20 queued
  requests), `extend` appended cohort `mined_humans_round_1`, a round-1 fit
  ran on the extended bundle and `stop` applied the rule. The whole loop took
  64 s with 0.99 GB peak RSS.
- **Tests.** The Python unit tests cover gradient reversal and its
  schedules, the adversary gradient sign, the contrastive rule and formula,
  gradient-cache exactness against direct backpropagation across
  microbatches, the voice-mixed order, the decay horizon, the bundle reader
  (text references, Test and held-out-style refusal, weight checks, zero
  weights, rosters), the Train-mining pool check, empty-roster roles and
  interrupted-resume identity for the full arm. Rust tests cover selection,
  exclusion, ties, the stop rule and bundle extension.

Real-data fits with these heads, Rust CAL and the Rust replay are recorded
with the baseline release ([A baseline release
candidate](../../../docs/a-baseline-release.md)). Its selected arm, `E1aux`, is
E1 with the style adversary and the generator-contrastive head, and its replay
passed the fixed gates. CUDA step time and memory with the auxiliary heads (the
gradient cache adds one forward per step) are not reported separately.

### Protocol v1.2 settings

- Protocol v1.2 caps each arm at 4 epochs. The trainer's epoch count and decay
  horizon are caller settings within that cap; the baseline release trained
  for 2 epochs.
- The style adversary with voice-mixed batching and the generator-contrastive
  loss are the protocol's style-robust objectives: Dev-iter choices within E1
  and E2, each compared with and without by a one-epoch seed-17 run.
- `train_weighting` and `share_supervision` are Dev-iter fitting choices, like
  the cohort weights, and the run manifest records them. The release's seed-17
  screen compared label × domain balanced weighting on Dev-iter.
- Protocol v1.2 calibrates with `isotonic_v1_clipped`: one origin
  temperature `T` (balanced three-class NLL on CAL over `T = exp(0.01k)`,
  `k = -300..300`, nearest 1 on a tie), then isotonic on the scaled
  involvement `sigmoid(g_T)`, clamped to `[1/(2 n_h), 1 - 1/(2 n_+)]`, with
  `g_T = lse(l1/T, l2/T) - l0/T + (g - (lse(l1, l2) - l0))/T` (the last term
  is E2's span-gate offset) and the model-only/mixed ratio of
  `softmax(l/T)`. `calibrate_full_a` implements it as policy v3
  (`slop_ninja_involvement_calibration_policy_v3`); the fallback
  `origin_temperature_v1` applies only on a non-finite value, an empty class
  or a fit error. Rust (`involvement_calibration.rs`, `full_a.rs`) and the
  Python export replay (`full_a.apply_origin_calibration`) apply both maps
  from the origin logits. Policy v2 remains for the shared-contract
  experiments.

### Span operator and document composition

Rust implements these in `src/full_a.rs`. Native token states come from
`inference/a-v6-cpu/src/full_encoder.rs`. The content positions are every
position except the CLS/SEP frame. With the final-norm token states `x_i`:

```text
t_i    = w_t . x_i + b_t
pooled = sum_content softmax(w_a . x_i + b_a)_i t_i     (attention)
       | max_content t_i                                 (max)
mean   = mean_S sigmoid(t_i)
         S = content                (token_mean_projected, v1, legacy)
         S = word tokens            (word_token_mean_projected, v2)
l      = origin(z) / base_temperature;  lp = log_softmax(l)
g      = logsumexp(lp1, lp2) - lp0 + gate * pooled       (gate absent in E1)
p      = CAL(g);  r = softmax(l1, l2)[0]
origin = [1 - p, p r, p (1 - r)]
share  = clamp(o1 + o2 sigmoid(s), o1, p)               (head mode)
       | clamp(mean, o1, p)                              (token modes)
```

Word tokens (`slop_ninja_word_token_mask_v1`) are content positions whose
character offsets in the exact text cover at least one non-whitespace
character, whitespace meaning Python `str.isspace` (Unicode White_Space plus
U+001C..U+001F; Rust mirrors it and a test enumerates every code point). They
are exactly the tokens that can carry a span label, so the v2 mean no longer
averages unsupervised newline or space runs into a word-share estimate.
Pooling still uses every content token. Rust computes the mask from the
tokenizer's character offsets; replay checks it against the Python mask.

The gate is initialized to 0, so E2 starts as E1. The model-only/mixed split is
the origin head's ratio, so share stays between `P(model_only)` and
`P(involvement)` before and after calibration. Human role involvement is
`o0 + o2`. Named-profile thresholds apply to conditional named scores only.

### CAL-only involvement calibration

`src/involvement_calibration.rs` and `examples/calibrate_full_a.rs` fit on CAL
only, after a sealed selection. The inputs are the v2 artifacts:

```text
calibrate_full_a BUNDLE CONFIG FIT RAW_CAL_RUN SEALED_SELECTION POLICY.json NEW_CALIBRATION.json
```

- BUNDLE holds the CAL rows; only their metadata (id, text digest, origin,
  generator label, source group) is read. CONFIG holds its v2 contract.
- FIT is the completed v2 fit; RAW_CAL_RUN is `export_full_a.py raw --part
  cal` of its selected checkpoint (`slop_ninja_full_a_raw_predictions_v2`,
  bound to the bundle, contract, fit and checkpoint).
- SEALED_SELECTION is `slop_ninja_full_a_selection_v2`: `complete`,
  `selection_split` `dev_iter` or `dev_select`, `calibration_read_for_selection`
  false, `final_test_opened` false, the bundle digests, and `runs` with
  `{arm, seed, fit_sha256, selected_checkpoint_sha256}`. No tool writes it
  yet; the finalist selection step must.

The output is `slop_ninja_full_a_calibration_v2` (`fit_split` `cal`). The
human role is `roster_empty` (it always returns `other_human`; no threshold
is fitted). The AI role's identities are the CAL positives' generator labels:
known when in the roster, withheld otherwise. When CAL holds no withheld
generator, the threshold maximizes known-correct acceptance alone and the
record says so. `calibration_holdout_full_a BUNDLE POLICY RUNS NEW` reads
`{"runs": [{"format": "full_a", "arm", "seed", "dev_iter", "cal"}]}` raw run
directories of one checkpoint and reports the CAL-fitted maps on Dev-iter. The policy is declared
before CAL opens (`slop_ninja_involvement_calibration_policy_v2`). The input is
`sigmoid(g)` and the target is documented any-AI involvement, with
family/origin-balanced weights:

- Primary map, `isotonic_bounded_v2`: the `isotonic_v1` fit (exact score ties
  aggregated, weighted pool-adjacent-violators, linear interpolation between
  knots, constant outside the CAL support) with every level clamped to
  `[epsilon, 1 - epsilon]`, `epsilon = 1/(2*(min(CAL humans, CAL positives)+1))`.
  It never returns exactly 0 or 1, including for scores beyond the CAL support.
- Fallback, `involvement_temperature_v1`: `sigmoid(g/T)` with
  `T = exp(0.1k)`, `k = -30..30`, nearest 1 on an exact tie.

The bounded map is selected unless a recorded mechanical failure occurs: the
declared support minimums (human and positive rows and families) are not met,
or PAV yields a single level. CAL scores never choose the map. Source-family
cross-fitting (families sorted by SHA-256, dealt round robin, every map and its
weights refit inside each training fold) is reported per held-out fold: Brier,
exact 0/1 outputs, values at the bound, scores beyond the fold's support, the
largest human and smallest positive prediction, for the bounded, unbounded
and temperature maps. `examples/calibration_holdout_full_a.rs` applies the
CAL-fitted maps to a held-out partition for the same diagnostics. Policy v1
(unbounded `isotonic_v1`, chosen by cross-fitted Brier) loads only to replay
frozen evidence. Uncalibrated exports use `identity_v1` with threshold 1 and
require the explicit diagnostic flag. The Pangram comparator's map is defined
by the protocol, not here; whether it receives the same bound is a protocol
decision.

### Export and replay

The Python boundary is in `grammar/crates/grammar-eval/python`:
`train_full_a.py`, `export_full_a.py raw` (Dev-iter, Dev-select or CAL raw
heads plus evaluator predictions; each takes `--data BUNDLE --config CONFIG`)
and `export_full_a.py export`. The export has these parts:

- `encoder.safetensors`: the parent's exact 138-tensor roster, FP32, with the
  classifier unchanged;
- `heads.json`: heads v2 in export v3, with the scalar gate in `span_head`
  and the optional spans-v1 token head and emission spec (heads v1 in the
  earlier export v2; heads v3 in export v4);
- in export v4 only, `tower.safetensors`: the span tower's 25 FP32 tensors
  (`span_tower.layers.{0..3}.*` and `span_tower.norm.weight`), each with the
  shape of the encoder tensor it copies. The manifest binds the fit
  (`fit_hashes`: fit, binding, encoder, heads and checkpoint digests). The
  exporter checks that the encoder file and every document tensor equal the
  init fit's (for seed 17, the release's) bit for bit, and that the tower and
  token head equal the fit's `heads.safetensors`;
- the contract and the CAL receipt;
- synthetic reference requests with their Python outputs, and in export v3
  optional private local Dev-iter reference requests; v3 references are FP64
  (the gate) and FP32 (a drift diagnostic), while v2 records the trainer's
  FP32 function on the reference device.

`inference/a-v6-cpu` binaries:

```text
full_a PARENT_ENCODER EXPORT REQUESTS.jsonl NEW_RUN [--uncalibrated-diagnostics] [--f32-diagnostic]
compare_full_a EXPORT NEW_RUN NEW_REPORT.json [--local] [--fp32]
trace_full_a PARENT_ENCODER EXPORT CASE NEW_TRACE [--f64]
trace_full_a compare REFERENCE_TRACE TRACE NEW_REPORT
calibrate_full_a BUNDLE CONFIG FIT RAW_CAL_RUN SEALED_SELECTION POLICY.json NEW_CALIBRATION.json
calibration_holdout_full_a BUNDLE POLICY.json RUNS.json NEW_REPORT.json
compare_full_a_fits FIT_A FIT_B NEW_REPORT.json
```

The full-A numeric profile is FP64 (`candle_f64_full_a_v2`): the FP32 weights
are widened exactly and every encoder operation, including LayerNorm (exact
two-pass) and softmax, runs in FP64. The rotary tables are the exception: as in
HF's `ModernBertRotaryEmbedding`, the function A was trained and evaluated
with, each angle is one float32 product of position and inverse frequency, and
cos/sin are taken in float32 and then widened exactly. Recomputing the angles
in float64 (profile v1) is a different function, about 1e-4 rad away at
position 2,047. The V6 FP32 graph stays available as `--f32-diagnostic` and is
unchanged bit for bit. The export's FP64 gate reference (`reference_rotary:
hf_float32_tables_widened_v1`) is the same trained function in float64, with
the attention softmax in float64 (a rounding-only difference); the FP32
reference is compared as a drift diagnostic. `trace_full_a.py` produces the
matching Python per-operation traces (CPU or CUDA FP32, or FP64 with FP64
softmax and HF's float32 rotary tables widened) for synthetic cases. The
2026-10-27 measurements below used profile v1.

The comparison gates are unchanged: raw h 1e-4, shared z 1e-3, outputs 1e-5,
involvement logit 1e-4 and token mean 1e-5. Attribution labels and, when the
reference records it, the word-token mask must match. A v2 export is compared
with its recorded FP32 reference predictions, with these gates and the original
report (`slop_ninja_full_a_replay_comparison_v1`); `--local` and `--fp32` apply
to v3 and v4 exports only. A v4 export is compared exactly as v3, and its Rust
run must bind the export's tower file.

### Measured synthetic qualification, 2026-10-27

This used build-host, the RTX 4090 under the host GPU lock, torch 2.9.1+cu128
and transformers 4.57.6. Text was synthetic only, so nothing here is a quality
claim.

- **Function preservation.** Initial E1 and E2 matched the V6 classifier within
  6.2e-8 and the bundled references within 8.3e-7.
- **Determinism.** Repeated gradients were bitwise identical.
- **Memory and throughput.** These were bf16 checkpointed eager training steps
  on 2,048-token documents under a 0.45 CUDA cap:

  | Token budget | Documents | Peak allocation | Throughput |
  |---:|---:|---:|---:|
  | 2,048 | 1 | 3.86 GB | about 9.9k tokens/s |
  | 8,192 | 4 | 6.55 GB | about 11.6k tokens/s |
  | 16,384 | 8 | out of memory | — |

- **End to end.** A tiny synthetic fit ran trainer, raw CAL, Rust CAL,
  calibrated export and ordinary-profile Rust CPU replay. Rust CAL chose
  temperature for E1 and isotonic for E2. Replay passed every gate for E1 and
  E2, uncalibrated and calibrated. The largest raw-h error was 6.9e-5.

### Repair measurements, 2026-10-27

build-host, RTX 4090 under the host GPU lock, same runtime. Diagnostic data are
the exposed stage-3 Train/Development/CAL; replay cases are synthetic. Nothing
here is selection or Test evidence.

- **E2 replay drift.** The phase-1 E2 seed-17 export failed the 1e-5 output
  gate under FP32 Rust replay (1.244e-5, synthetic-unicode). Per-operation
  traces against a PyTorch FP64 run show it starts in the encoder, not the
  heads: no operator differs (Rust FP64 matches PyTorch FP64 within 3e-13 on
  h), local FP32 operation errors are about 1e-6, and E2's massive residual
  activations (about 1.2e4 on the final separator) amplify rounding from
  layers 14-15 onward. FP32 CPU arithmetic lands 5.4e-5 (Rust) and 6.8e-5
  (PyTorch CPU) from the FP64 h; the CUDA reference lands 1.2e-5. The span
  gate (0.015) contributes almost nothing; the largest output error was an AI
  conditional named score.
- **FP64 profile.** Under `candle_f64_full_a_v1` every gate passes, unchanged:
  E2 seed 17 output error 2.36e-6 and raw h 1.21e-5 (the CUDA reference's own
  FP32 error), E1 7.3e-7. The FP32 diagnostic reproduces the phase-1 failure
  exactly. FP64 encoder time was 1.9 times FP32 on the three synthetic cases
  (3.16 s against 1.67 s, one thread).
- **Word-token share and determinism.** Two E2 seed-17 fits with the v2
  word-token share and a CAL report split were bitwise identical on CUDA:
  selected checkpoint, encoder and heads, history and every Development and
  CAL prediction. The FP64 replay of that export passed with output error
  4.7e-7, with identical Python and Rust word masks (newline and trailing-space
  tokens excluded). The enlarged step-0 gate covered 14 Train rows from 91 to
  1,088 tokens (the longest) with 6e-8 preservation error and 2.9e-6 padded
  agreement.
- **Bounded isotonic.** With 40 CAL humans, `epsilon` is 1/82. Across the six
  phase-1 E1/E2 runs the bounded map produced no exact 0 or 1 on Development or
  in any CAL cross-fit fold, against 334-347 exact ones per run on Development
  and 508-533 exact 0/1 values across folds for `isotonic_v1`, including 1-2
  Development humans mapped to exactly 1 per run. Development Brier moved only
  0.0001-0.0004: the cost is humans above the top CAL human block. With 40
  CAL humans the bounded map still trailed temperature on Development in all
  six runs (by 0.00001-0.0091) and in the CAL cross-fit in five of six; for the
  new E2 word-share fit it led (0.0525 against 0.0551). The bound removes the
  0/1 defect, not the small-CAL overfit.

Current evidence beyond these synthetic measurements:

- corpus fits and the release replay gate are in [A baseline release
  candidate](../../../docs/a-baseline-release.md);
- FP64 replay latency and memory at 2,048 tokens, and the f32 serving builds,
  are in [A inference on CPU](../../../docs/a-cpu-inference.md);
- f32 serving on Cloudflare Containers is qualified in [f32 serving of A on
  Cloudflare Containers](../../../docs/a-container-serving.md).

The MKL profile with these heads and Workers/Wasm serving remain unqualified.

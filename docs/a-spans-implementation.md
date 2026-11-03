# A span localization: implementation

This note records what is implemented for [protocol spans-v1.1](a-spans-protocol.md)
and the smoke runs that checked the pipeline end to end before the protocol
fits. The smoke is not protocol evidence. The protocol fits, gates and span
metrics are reported in [span localization results](a-spans-results.md).

## What is implemented

**Token labels.** The existing label policy `slop_ninja_token_span_label_v1`
maps a row's exact model-produced byte spans to token labels. A token is
labelled only from its non-whitespace characters. Special tokens,
whitespace-only tokens, tokens that straddle a span edge, and padding all carry
the ignore label. Human-only rows are all human and model-only rows all AI.

**Token head.** A separate head reads the final-norm token states:
`t = w2 · gelu_tanh(W1 x + b1) + b2`, with a hidden width of 256. It never
feeds the document heads, so document outputs keep their meaning. The loss is
binary cross-entropy on labelled tokens, averaged per document and weighted by
the row's token weight. A row's token weight is either its row weight or a
`token_weight` field carried by the bundle.

**Trainer.** Arms can add `token_head`, `freeze` and `token_weighting`.
`--init-fit` starts from a completed fit's selected checkpoint. With
`freeze: encoder_and_document_heads` (the S0 arms), only the token head trains.
The fit then checks that the encoder and document-head tensors still equal the
initial fit's, bit for bit, and that the Dev-iter document outputs equal the
initial fit's own dump. Checkpoints can be selected by the raw mixed-document
word Brier on Dev-iter, optionally limited to evaluations whose document Brier
stays within a stated allowance (the S1 rule). Token logits of the selected
checkpoint are written for every evaluated split.

The training-only auxiliary heads (style adversary, contrastive projection)
are not in a selected checkpoint. A joint (S1) fit started from one therefore
starts those heads fresh.

Before the first optimizer step the trainer compares padded microbatches with
unpadded single-document passes on a fixed set of Train rows. The pooled
origin probabilities must agree within 1e-4 and the token-head probabilities
within 1e-3; the token-head logit difference is recorded. A padding-mask
defect gives errors of order 0.1. An optional protocol field
`span_metric_exclusions` names, by SHA-256, Dev-iter rows that the trainer's
span metrics skip (the meta-flagged mixed rows); its document metrics keep
them.

**Plus bundle.** `slop_ninja_a_spliced bundle-plus` adds the frozen v1.1
spliced Train records to the release trainer bundle as cohort
`spliced_rewrite_v1`, with `pair_id`, `source_kind` and the edit-history
digest. In every domain that holds spliced rows, those rows take 25% of the
domain's AI-involved assembler mass and the domain's release AI-involved rows
keep 75%. Human rows are unchanged, so the human share of the document loss
stays where it was. `token_weight` gives the spliced rows 60% of the
token-loss mass; every other row keeps its release row-weight proportion.
Dev-iter and CAL lines are copied byte for byte; Dev-select and Test lines
are skipped unparsed. `project-dev` writes one Dev-iter span half as trainer
rows for scoring only.

**Scoring routes.** `export_full_a.py offsets` writes tokenizer-only span
inputs per row: character offsets, the word-token mask, and the trainer's
token and word labels. `score-rows` writes raw heads, token logits and span
inputs of a token-head fit for rows outside its bundle: a Dev-iter span half,
or the consumed Test for a labelled retrospective report only.

**Evaluator.** `a_spans_evaluate` (an example in `grammar-a-baseline`)
applies the runtime's own emission functions to the dumped logits. It reports
the protocol span metrics with slices, three-seed arm means and paired
source-group bootstrap intervals, and a document block (balanced involvement
Brier, share MAE, operating-point flags). It also runs the 108-point decoder
grid and fits the CAL sentence and span maps. A `raw_calibrated` input applies
a fit's frozen CAL document map to raw heads exactly as the CAL calibrator and
the runtime do.

**Emission.** The same rule is implemented in Rust (`full_a_spans.rs`) and as
a Python reference (`full_a_spans.py`):

1. Words are maximal runs of non-whitespace characters under the Python
   `str.isspace` rule. These are the words of the share target.
2. A word's probability is the mean token probability over the word-bearing
   tokens that overlap it. A word with no such token makes span emission
   unsupported for that request; the document outputs still stand.
3. A share tilt shifts every word's logit by one bias, found by bisection, so
   that the mean word probability equals the emitted document share.
4. Sentences come from the sealed splitter used to build the positives. Every
   word lies in exactly one sentence. A sentence reports its AI share (the mean
   tilted word probability) and its AI confidence (a confidence map of its
   highest word probability, capped at the document confidence).
5. The hysteresis decoder `slop_ninja_span_decoder_v1` turns tilted word
   probabilities into spans. Each span reports exact character and UTF-8 byte
   offsets, its word count, its AI share and its confidence, capped at the
   document confidence.

The sentence and span confidence maps are either the identity or a clipped
isotonic map fitted on CAL. Golden vectors on synthetic text and logits are
checked into `grammar/crates/grammar-a-baseline/tests/golden/` and bind the two
implementations. A test checks that the runtime copy of the sentence splitter
equals the sealed splitter. The same comparison over every Train, Dev-iter and
CAL text of the release bundle (94,827 rows; counts only, no model) found no
difference, and no word outside a sentence.

**Export and runtime.** Export v3 writes heads v2: the token head plus the
span emission spec, meaning the decoder and the two confidence maps.
Calibration v3 carries the CAL version of that spec. An uncalibrated export
uses identity maps and a declared diagnostic decoder. The reference set adds
twelve synthetic span cases to the three release cases. They cover CR and
CRLF line endings, input that NFC normalization changes, emoji, combining
marks, zero-width characters, a single word, one-word lines, no sentence
punctuation, quotes and brackets at seams, Unicode and Python-only
whitespace, mixed scripts, and a 2,048-token document. The export can also
add a stratified local Dev-iter reference set, which stays in private
storage. The Rust `full_a` runtime reads only v3 exports. With a token head,
it adds `sentences` and `spans` to each prediction, plus a `span_status`
field. The replay comparator applies the protocol's R1 and R2 gates.

**Numeric profile and replay reference.** The runtime's qualified profile is
`candle_f64_full_a_v2`: the FP32 weights are widened exactly and every
encoder operation runs in FP64, except the rotary tables. Those are built as
HF's `ModernBertRotaryEmbedding` builds them, the function the model was
trained and evaluated with: each angle is one float32 product of position and
inverse frequency, cos and sin are taken in float32, and the tables are then
widened. The gate reference is the same trained function in FP64: a float64
copy of the exported model, with HF's own rotary tables and the attention
softmax in float64, a rounding-only difference. The export records this as
`reference_rotary: hf_float32_tables_widened_v1`, and the comparator refuses
an FP64 reference without it. The FP32 reference, the trainer's own
evaluation function, is compared with the same gates as a reported drift
diagnostic.

## Smoke run

The smoke ran on October 30, 2026, at commit `7aba3a46` (run two) and again
at `958fdad4` (run three), with bitwise identical fits ([commit
aliases](a-spans-aliases.md)). Its shape is an S0
arm: the encoder and document heads were frozen at the release seed-17 fit, and
only the token head trained, for 300 steps (one epoch at batch 32) on a
stratified 9,600-row diagnostic Train subset. Learning rate 1e-3, cosine
schedule, 6% warm-up. Dev-iter was evaluated at steps 0, 75, 150, 225 and
300. A diagnostic subset fit is never calibrated or released, and these
numbers are not selection evidence.

**Document outputs did not change.** At evaluation 0, the Dev-iter document
outputs equal the release fit's own dump exactly: the largest absolute
difference over all 2,873 rows is 0.0. After training, the encoder and
document-head tensors are bitwise equal to the release fit's, and the Dev-iter
involvement Brier stays at 0.063932 at every evaluation. The padded and
unpadded token logits agree to 5.6e-5. A second run with the same inputs gave
bitwise identical token-head weights.

**Token head (raw probabilities, before the tilt).** Briers are on Dev-iter,
with documents weighted by the balanced population weights.

| Step | Mixed-doc word Brier | All-doc word Brier | Mixed-doc token Brier | Human docs with any word ≥ 0.5 | Mean word probability minus share (MAE) |
|---:|---:|---:|---:|---:|---:|
| 0 (untrained) | 0.2546 | 0.2596 | 0.2540 | 570 of 570 | 0.405 |
| 75 | 0.1692 | 0.0669 | 0.1766 | 245 of 570 | 0.048 |
| 150 (selected) | 0.1591 | 0.0642 | 0.1649 | 241 of 570 | 0.037 |
| 225 | 0.1658 | 0.0647 | 0.1717 | 215 of 570 | 0.042 |
| 300 | 0.1595 | 0.0635 | 0.1654 | 224 of 570 | 0.040 |

The human-document count uses raw word probabilities at 0.5, before the tilt
and before any decoder. It is far above the protocol's decoder limit of 22.
The real arms, the tilt and the decoder choice are what that limit applies to.
Every Dev-iter word had a word-bearing token, so emission was never
unsupported.

**Rust replay (gates R1 and R2).** The first two smoke runs (commits
`cb745eac` and `7aba3a46`) used a runtime that formed the rotary angles in
float64. Against the FP32 reference the token probability error reached
4.2e-4 on the 2,048-token case. The second run then compared against an FP64
reference patched to use float64 angles too, and passed at 2.6e-13. An audit
showed that this patched reference was a different function from the one the
model was trained with. Against an FP64 reference with HF's unpatched rotary
tables, that runtime missed by 3.89e-4 (token probability, 2,048 tokens) and
4.5e-5 (involvement logit), nearly the whole FP32 gap. The gap was the
runtime's rotary rule, not rounding in the reference. Runs one and two are
superseded.

The third smoke run (commit `958fdad4`) used the corrected runtime and the
unpatched reference. Its fit is bitwise identical to the second run's. The
export carried the 15 synthetic cases and 200 stratified Dev-iter documents:

| Reference | Set | Token probability | Raw h | Involvement logit | Outputs | Offsets |
|---|---|---:|---:|---:|---:|---|
| FP64 (gate) | 15 synthetic | 1.7e-6 | 1.2e-6 | 8.4e-7 | 3.0e-7 | all equal, 23 of 23 spans |
| FP64 (gate) | 200 local Dev-iter | 4.4e-6 | 3.2e-6 | 1.3e-6 | 1.9e-7 | all equal, 626 of 626 spans |
| FP32 (diagnostic) | 15 synthetic | 5.4e-5 | 3.8e-5 | 2.5e-5 | 9.5e-6 | all equal |
| FP32 (diagnostic) | 200 local Dev-iter | 1.7e-4 | 1.3e-4 | 2.7e-5 | 3.4e-6 | all equal |

Both gate sets pass every R1 and R2 gate. No case had a decision value within
1e-6 of a decoder threshold. The document gates are raw h, shared z, outputs,
the involvement logit and the token mean. The span gates are token, word,
sentence and span values, plus exact tokenizer, word, sentence and span
offsets. The remaining FP64 gap, at the 1e-6 level, comes from different
float32 cos and sin implementations (CUDA against glibc), not from a different
function. The FP32 rows measure the trainer's FP32 evaluation against its
exact-arithmetic counterpart: they exceed the 1e-5 span gates and, on the
Dev-iter set, the 1e-4 raw-h gate.

**CPU latency** (FP64, one thread, median of three synthetic requests; not
gated). Run two had the host to itself; run three shared it with other
campaign jobs, which explains its slower 2,048-token encoder time:

| Tokens | Encoder (run 2) | Encoder (run 3) | Token head | Decoder |
|---:|---:|---:|---:|---:|
| 512 | 3.0 s | 2.9 s | 0.036 s | 0.4 ms |
| 1,024 | 9.0 s | 9.6 s | 0.073 s | 0.8 ms |
| 2,048 | 26.4 s | 35.0 s | 0.146–0.156 s | 1.6 ms |

## Not yet implemented

- a v3 re-export of the release with no token head, to check that its document
  outputs are bit-identical to the v2 replay. The exporter verifies the fit's
  bound source hashes, and the release fit is bound to the code from before
  this change, so it needs its own route.
- the token head and span decoder in the R0 reference artifact and in the f32
  Container serving build (the FP64 Rust runtime has them);
- span emission for inputs longer than 2,048 tokens.

# A baseline release candidate

Recorded on October 28, 2026. This note describes the *A* baseline built for the
initial testnet deploy: its training data, recipe, selection, calibration, and
measured limits. It was produced under the
[competitive A protocol](a-competitive-protocol.md) v1.2.

Nothing was deployed. The sealed Test split was not opened and no external
detector was called. Weights, predictions, bundles, and per-run records stay
in ignored storage.

**Update, October 31, 2026.** The release is now the October 28 document
model (R0) plus a span head: the S0+ seed-17 token head of the
[span localization results](a-spans-results.md), packaged as export v3. Every
document output stays bit-identical to R0's. The R0 package (export v2)
remains replayable by the same Rust runtime
([Span head](#span-head-export-v3)).

## Summary

| Item | Value |
|---|---|
| Selected arm | `E1aux`: full fine-tune from V6 with a style adversary and a generator-contrastive head |
| Released weights | Seed 17, selected at Dev-iter evaluation 13 |
| Dev-iter balanced Brier, uncalibrated | 0.0640 (three-seed mean) against 0.1843 for V6 |
| Dev-select balanced Brier, uncalibrated | 0.0619 (three-seed mean) against 0.1820 for V6; seed 17 scores 0.0611 |
| Paired difference against V6 on Dev-select | -0.1202, 95% interval [-0.1293, -0.1115] |
| Calibration | CAL only: origin temperature, then `isotonic_v1_clipped` (the primary rule; no fallback) |
| 1% human-FPR operating point | Calibrated involvement >= 0.9090909090909091, fitted on CAL |
| Rust replay gate | Passed; the largest output error is 7.3e-7, against a gate of 1e-5 (7.2e-7 on the current runtime) |
| CPU latency, FP64 reference runtime, one thread | 27.3 s per 2,048 tokens and 2.96 s per 512 tokens; the span head adds 0.14 s at 2,048 tokens |
| Span head (export v3) | S0+ seed-17 token head on the frozen encoder; sentence and span outputs added, document outputs bit-identical to R0 |
| Span replay gate (export v3) | Passed; the largest token-probability error is 6.1e-6 and every span offset is equal |
| Claims not made | Sealed-Test performance, a Pangram comparison, frontier-model detection or attribution, production qualification |

## Model

*A* is a ModernBERT-base encoder of about 150M parameters with a 2,048-token
limit. It is fully fine-tuned from the frozen V6 detector and has small numeric
heads.

It returns the origin probabilities and the two document percentages:

- **Confidence:** the calibrated probability of any AI involvement.
- **Share:** the expected AI-written or AI-rewritten final-word share. It is
  kept between the model-only probability and the involvement probability.

It also returns conditional human and AI attribution, with a named profile or
the `other_human` and `other_ai` fallbacks.

The shipped package is export v3 (`slop_ninja_full_a_export_v3`, heads v2,
calibration v3), run by the Rust `full_a` runtime. It adds sentences and
spans with AI share and confidence percentages
([Span head](#span-head-export-v3)). The R0 package (export v2, heads v1,
calibration v2) also holds:

- the calibration and the operating points;
- the reference requests and their outputs;
- the selection, calibration, and evaluation records;
- a model card and SHA-256 sums for every file.

The runtime reads both. It chooses the reading from the export's declared
schema alone.

The Container serves R0's document outputs from the R0 package (export v2),
without the token head. Its f32 ONNX graph is exported from the encoder
weights outside the package, so the package's `SHA256SUMS` does not cover it.
The tracked
[served profile](../inference/a-container-serve/README.md#served-profile) binds
the graph's digest (`08848d95…ca4cb7`) to the R0 package's `SHA256SUMS` and to
the release files it was derived from.

## Training data

The Train bundle has 86,575 rows. Of these, 83,956 carry weight; a learner-essay
diagnostic pool is held at zero weight. By origin, the rows are:

- 49,179 human-only;
- 21,482 mixed;
- 15,914 model-only.

They cover 36 domains, and every Train domain with humans also has positives.

| Cohort kind | Share of positive loss mass |
|---|---:|
| Local open-weight generations (7 generators, styled voices and edit operations) | 81.7% |
| Hosted frontier generations | 6.6% |
| RAID (paired and human-paraphrase cohorts) | 6.2% |
| Earlier-stage legacy positives | 5.4% |
| Hosted captures | 0.2% |

**Coverage.** An earlier fit collapsed because Train positives were missing from
most Dev-iter domains. A stratified Train top-up of the seven Dev-iter
generators fixed that coverage gap. Every Dev-iter domain, and every
generator × Dev-iter domain cell, now has Train positives, with at least 59
rows per cell.

**Weights.** Row weights use `assembler_row_weight_v1`, capped at 5× the mean.
The Kish effective sample size is 32,827 of 83,956 weighted rows.

**RAID** is capped at 20% of positive mass and realises 6.2%:

- Its unpaired-domain cohort is excluded.
- So are rows with a repeated 8-gram word share of 0.5 or more, and rows over
  the token limit.
- Its 375 DIPPER paraphrases are labelled model.

RAID rows carry no Wikipedia page id or Reddit author, so the split rules could
not bind them to evaluation reservations. A title-hash check against the
reservation manifests found seven RAID source groups (36 Train rows) with the
exact title of a reserved evaluation page:

- three are sealed-Test Wikipedia pages;
- one each are in Dev-iter and Dev-select;
- two are in CAL.

None of those pages is in a scored bundle. The scored Dev-iter, Dev-select and
CAL bundles also contain no Reddit rows, so the reported numbers are unaffected.
It remains unverified whether RAID's Reddit rows come from authors reserved for
evaluation, because resolving them would need sealed-Test text.

A pre-registered acquisition screen therefore applies to any later evaluation
acquisition, including sealed Test. It skips:

- the matched pages;
- any reserved Reddit author whose posts overlap a RAID Train row;
- any reserved unit whose text overlaps a RAID Train row.

The next Train build drops the seven groups.

**Excluded human pools.** Out-of-domain human-only pools without paired
positives are excluded from Train: corporate email, parliamentary speech and
grant abstracts. Including them would bring back a domain-means-human
shortcut.

**Evaluation authors.** Frontier and local records that imitate an
evaluation-split author are withdrawn. An independent check found:

- no Train row imitates any author in the 18 evaluation manifests;
- no withdrawn record id remains.

## Recipe and selection

- **Training.** AdamW with encoder learning rate 1e-5 and heads 3e-4, a cosine
  schedule with 6% warm-up, 2 epochs and logical batch 32. Attribution loss
  weight 0.25.
- **Checkpoint choice.** Uncalibrated balanced involvement Brier on Dev-iter,
  checked eight times per epoch, with patience 4. A fit that selects the
  untrained start (evaluation 0) is rejected; none of the six final fits did.
- **Seed-17 screen.** Three variants were screened on Dev-iter, and all three
  beat V6:

  | Variant | Dev-iter Brier |
  |---|---:|
  | The plain recipe (F) | 0.0677 |
  | F with label × domain balanced weighting | 0.1065 |
  | F with style adversary and contrastive heads (Faux) | 0.0636 |
  | V6 | 0.1843 |

- **Finalists.** F and Faux were declared, with hashes, before any further
  fit. Seeds 29 and 41 then ran on the same bundle hash. All six fits' weights
  and configurations were hashed before Dev-select opened.
- **Dev-select** was scored once, for the final selection. The rule chooses the
  lowest three-seed mean: Faux 0.0619 against F 0.0642. The paired Faux − F
  difference is −0.0023, with interval [−0.0040, −0.0008]. It excludes zero, so
  the tie rule does not apply. Dev-iter and Dev-select rank the finalists the
  same way.

## Results

Uncalibrated balanced involvement Brier for the released recipe (three-seed mean)
against V6, per domain:

| Domain | Dev-iter V6 | Dev-iter A | Dev-select V6 | Dev-select A |
|---|---:|---:|---:|---:|
| FineWeb | 0.166 | 0.056 | 0.174 | 0.062 |
| Global Voices | 0.168 | 0.066 | 0.192 | 0.069 |
| PLOS | 0.176 | 0.072 | 0.150 | 0.053 |
| Project Gutenberg | 0.167 | 0.032 | 0.176 | 0.034 |
| US Federal Register | 0.287 | 0.093 | 0.255 | 0.078 |
| Wikinews | 0.187 | 0.057 | 0.179 | 0.070 |
| Wikipedia | 0.165 | 0.058 | 0.169 | 0.060 |

Every domain and every generator improves on V6 in both splits. The per-generator
Dev-select Brier for A ranges from 0.050 (Gemma 3) to 0.067 (Phi-4).

Other measures for the released recipe:

- **Share MAE:** 8.6 points on Dev-iter and 8.2 on Dev-select, against 20.9
  and 23.9 for V6.
- **Attribution:** top-1 AI attribution is about 0.58 overall and 0.71-0.72 on
  voices held out from Train.
- **Seed 17 at an uncalibrated 0.5 threshold:**

  | Split | Humans flagged | TPR |
  |---|---:|---:|
  | Dev-iter | 38 of 570 (V6: 70) | 0.930 |
  | Dev-select | 79 of 1,140 (V6: 161) | 0.918 |

## Calibration and operating point

The calibration is fitted on CAL alone (5,379 documents) after selection.
Origin temperature is 1.1275. The isotonic map succeeded, so the
temperature-only fallback was not used.

| Target FPR | Threshold | CAL humans flagged (in sample) | CAL TPR |
|---|---|---:|---:|
| 1% | 0.9090909090909091 (`0x3fed1745d1745d17`) | 8 of 1,140 | 0.831 |
| 0.1% | 0.9998820476527482 (`0x3fefff08a2d0d58b`) | 0 of 1,140 | 0.712 |

Both thresholds are isotonic levels; the 0.1% point is the top clipped level.
A document is flagged when its calibrated involvement, compared as f64, is at
least the threshold, and ties count as flagged. The value to compare is the
involvement as the runtime emits it: the AI-role involvement probability, which
equals one minus the human-only probability at these values. Re-adding the
model-only and mixed probabilities can move a document 1 ulp off its level,
and the rounded percentage is not exact either.

**Correction.** The first record of this release gave 0.8673587081891581
(`0x3febc1670cb257fe`) and 0.9797889063552662 (`0x3fef5a6e43b8f175`). The
evaluator had formed each value by re-adding the model-only and mixed
probabilities. Their rounding noise split one isotonic level into separate
candidates, so each threshold sat 1 ulp above a level that its own documents
share. Applied to the whole level, the protocol tie rule flags 17 of 1,140 CAL
humans at 0.8673587, more than the 11 allowed, so the 1% point moves to the next
level. The evaluator now reads the emitted involvement. The calibration map,
the CAL predictions and the selection are unchanged, and Dev-select was not
scored again.

Two diagnostics check the 1% point. Neither is a gate.

**Dev-iter at the CAL threshold.** It flags 11 of 570 humans (1.9%), with TPR
0.856. Per-generator TPR ranges from 0.764 (Phi-4) to 0.916 (Gemma 3). These
are the same as at the superseded threshold, because no Dev-iter document falls
between the two levels.

**Out-of-domain humans.** Train-side documents from the three pools excluded
from Train, with up to 300 per pool and at most 2 per writer. They were scored
by the Rust runtime with the calibrated export:

| Pool | Flagged at the 1% point |
|---|---:|
| Corporate email | 5 of 188 |
| Parliamentary speech | 0 of 300 |
| Biomedical grant abstracts | 4 of 300 |

## Span head (export v3)

The span head is a small token head on the encoder's final-norm token states.
It never feeds the document heads. It was trained on the frozen R0 seed-17
fit (encoder and document heads frozen) with the spliced-rewrite cohort. Its
decoder and CAL maps were chosen under a private protocol frozen before
they were measured ([Shipped configuration](a-spans-results.md#shipped-configuration)):

- **Decoder** `slop_ninja_span_decoder_v1`: `tau_hi` 0.7, `delta` 0, at least
  5 words, gap 0, sentence snap at 80%. It was chosen on the 108-point grid
  under the limit of 22 of 570 Dev-iter humans with a span; no extension was
  needed.
- **CAL maps:** sentence and span confidence maps fitted on CAL, with R0's
  document calibration unchanged.

The package keeps R0's encoder, document heads, contract values, CAL
document map and operating points. On October 31 these checks passed:

- **Static identity.** The encoder file is byte-identical to R0's. Every
  document-head weight and field equals R0's bit for bit. The only additions
  are the token head, its emission spec and its supervision flag. The
  contract differs only in its training-bundle digest.
- **Document outputs through the Rust runtime.** One runtime build ran R0's
  v2 package and the v3 package on the same 227 requests:
  - R0's 3 reference cases;
  - the 9 latency requests;
  - the 15 synthetic reference cases;
  - the 200 local Dev-iter replay cases.

  Every document output (origin, confidence, share, attribution, raw heads,
  raw h and shared z) is bit-identical.
- **Span replay (R1).** The FP64 Rust replay matches the package's FP64
  Python reference on the 15 synthetic and 200 local cases:

  | Largest error | Synthetic | Local |
  |---|---:|---:|
  | Document outputs | 3.0e-7 | 2.7e-7 |
  | Token probability | 1.4e-6 | 6.1e-6 |
  | Word probability, tilted | 6.0e-7 | 1.5e-6 |
  | Sentence share and confidence | 1.7e-7 | 9.8e-7 |
  | Span share and confidence | 3.0e-8 | 2.1e-7 |

  Every bound is 1e-5. Every tokenizer, word, sentence and span offset is
  equal (11 and 241 spans), and no case lies near a decoder threshold. The
  FP32 Python function is a drift diagnostic only: against it, token
  probabilities differ by up to 2.5e-4 on the local cases.

Span quality is reported in the [span localization
results](a-spans-results.md). On the held-out `span_report` set, character
F1 is 0.410 for the shipped head.

## Runtime

The Rust FP64 replay of the calibrated export matches the Python reference
outputs within the fixed gates.

On October 31 the copied ModernBERT source changed how it builds the rotary
tables, to match the trained function (`candle_f64_full_a_v2`). Each angle is
one float32 product, and the FP64 profile widens the float32 tables exactly.
The FP32 graph is bit-identical before and after
([qualification](../inference/a-v6-cpu/QUALIFICATION.md)). On the current
runtime R0's replay gate passes with smaller errors:

| Largest error against R0's reference | Release runtime | Current runtime | Gate |
|---|---:|---:|---:|
| Raw h | 1.35e-5 | 9.54e-6 | 1e-4 |
| Shared z | 1.33e-5 | 9.59e-6 | 1e-3 |
| Outputs | 7.31e-7 | 7.16e-7 | 1e-5 |
| Involvement logit | 3.33e-6 | 2.38e-6 | 1e-4 |

On one thread, the reference runtime takes, as the median of three synthetic
requests:

| Tokens | Time |
|---:|---:|
| 512 | 2.96 s |
| 1,024 | 8.80 s |
| 2,048 | 27.3 s |

The v3 package was measured on October 31 with the current runtime, on one
thread, as the median of three runs of the same requests. The host was shared,
with a load average of 13–23 on 32 threads. R0's package was run
between two package runs:

| Tokens | v3 package total | Token head | Span decoder | R0 package, same runtime |
|---:|---:|---:|---:|---:|
| 512 | 2.94 s | 0.035 s | 0.0004 s | 3.09 s |
| 1,024 | 8.85 s | 0.072 s | 0.0008 s | 8.54 s |
| 2,048 | 28.2 s | 0.143 s | 0.0016 s | 27.9 s |

The second package run took 2.92, 8.78 and 28.4 s. The encoder dominates, and
its run-to-run spread is larger than the span head's cost.

Peak resident memory is about 3.4 GB. This runtime is the numeric reference,
not the serving build. The [CPU inference note](a-cpu-inference.md) measures
faster f32 and mixed-int8 builds of the same shape. The mixed-int8 build
failed its pre-registered no-quality-loss qualification against this artifact,
and so did its one pre-declared fallback, so serving stays f32
([serving qualification](a-cpu-inference.md#serving-qualification-of-the-release-build)).

## Limits

- **No Test or external claim.** No sealed-Test result and no Pangram
  comparison exist yet. The Tier C evaluation follows this release and is not
  a prerequisite for the testnet deploy.
- **Dev-select was opened once,** for the final selection. Its numbers are
  selection-time estimates for the chosen arm, not unbiased held-out
  estimates.
- **The calibration and operating points come from CAL alone,** and CAL values
  are in sample. The Dev-select FPR at the operating point was not measured.
- **RAID lineage.** RAID's Reddit rows have no author key, and seven RAID
  source groups share titles with reserved evaluation pages. Sealed-Test
  results are valid only if the acquisition screen is applied first.
- **Frontier models are unevaluated.** Every evaluation positive comes from the
  seven local open-weight generators, plus one CAL-only generator. Detection
  and attribution of hosted frontier models have not been evaluated.
- **Weak slices:**
  - Phi-4 and Mistral recall;
  - the US Federal Register domain;
  - human false positives in Global Voices and FineWeb, and in corporate email
    out of domain;
  - attribution for Granite, SmolLM3 and Phi-4;
  - share error of about 8 points.
- **Span head.** The shipped head finds fewer spans than the joint
  fine-tune that was not adopted. On `span_report` its character F1 is
  0.410, it decodes no span in 42% of the documents, and clause and
  single-sentence edits are mostly missed. Its decoder meets the
  human-document limit with zero slack. On the consumed Test, retrospectively,
  156 of 6,132 humans carry a decoded span
  ([span limits](a-spans-results.md#limits)).
- **English only.**
- **Workers AI.** The model cannot run on Workers AI as-is, because Workers AI
  accepts no custom encoder upload ([Workers AI viability](a-workers-ai-viability.md)).
  The planned serving path is a CPU Container. The f32 Container serving
  build does not carry the span head yet.
- **Public distribution.** The weights are published as reference packages
  under CC BY-SA 4.0 when public access opens
  ([reference packages](testnet/reference-packages.md)).

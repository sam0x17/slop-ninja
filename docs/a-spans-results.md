# A span localization results

Results of protocol spans-v1.1 ([protocol](a-spans-protocol.md),
[implementation](a-spans-implementation.md), [data](a-spans-data.md)),
measured October 30–31, 2026. Every arm, seed and gate of the protocol was
run. The release candidate (`E1aux`, seed 17,
[baseline release](a-baseline-release.md)) is not replaced. This page gives
aggregate numbers only; per-document outputs and paths are kept in ignored
storage. An independent review rebuilt every headline number from the saved
predictions and matched them; the qualifiers it found are stated below.

## Summary

- **Shipped: the release plus the S0+ head.** The shipped span configuration
  keeps the release's document outputs byte-identical and adds the S0+
  seed-17 token head, with the same decoder as S1+ and its own CAL span maps.
  S1+ is not adopted ([Shipped configuration](#shipped-configuration)).
- **Follow-up S2: partial; S2c is a candidate only.** The separate span tower
  on the frozen release encoder (S2c) keeps the document outputs
  bit-identical, passes every S2 gate and beats the shipped head's recipe on
  the primary span metric, but does not reach S1+. Its export passes the Rust
  replay (R1). Its own decoder, from the declared grid extension, puts spans
  on 21 of the 570 Dev-iter humans (limit 22). The shipped configuration is
  unchanged, and adopting S2c is a user decision
  ([Follow-up arm S2](#follow-up-arm-s2)). On November 1, 2026, S2c was
  adopted as the span head of Slop Ninja's first span-serving package
  ([reference packages](testnet/reference-packages.md)); `R_0` stays R0.
- **Protocol-selected arm: S1+**, a joint fine-tune of the encoder, document
  heads and token head on the release Train bundle plus the spliced-rewrite
  cohort, with token-loss weight 1.0. Its seed-17 fit was the protocol's span
  candidate.
- **Every gate passes for S1+**: document Brier noninferiority for the
  artifact and the recipe, the CAL operating points, share, attribution and
  Rust replay. S0- and S0+ pass by construction (their document outputs are
  bit-identical to the release); S1- fails three gates.
- **S1+ would change the document outputs; the shipped configuration does
  not.** S1+ passes D1 because better scores on AI-involved documents offset
  clearly worse uncalibrated scores on human documents: human-only Brier
  rises from 0.061 to 0.093 (three-seed means), and 58, 56 and 71 of 570
  Dev-iter humans reach 0.5, against 38, 33 and 42 for the release. CAL
  calibration absorbs most of the shift
  ([Human documents](#human-documents)). The shipped configuration's
  document outputs are byte-identical to the release's.
- **Spans.** On the Dev-iter `span_report` half (823 spliced documents that
  no choice scored), the shipped calibrated S0+ head reaches tilted word
  Brier 0.128 and character F1 0.410 (recall 0.402; precision 0.862 over the
  477 documents with a decoded span). It decodes no span in the other 346
  (42%) and mostly misses clause and single-sentence edits. The three S1+
  seeds reach 0.088 and 0.570 (recall 0.548; precision 0.918). Every one of
  these documents edits a Dev-iter human that the decoder and trainer
  choices did use, so these figures are somewhat optimistic. On the Dev-iter
  selection population the primary span metric falls from 0.1448 (frozen
  release encoder, S0-) to 0.1412 (S0+) and 0.1205 (S1+), three-seed means.
- **Consistency holds exactly**: after the share tilt, the mean word
  probability equals the emitted document share to within 6e-16.
- **Replay.** The shipped calibrated v3 export replays in Rust FP64 within
  3.0e-7 of the FP64 Python reference in its document outputs and 6.1e-6 in
  token probability, against a 1e-5 bound, with all span offsets equal. The
  S1+ candidate's calibrated export replays within 8.5e-6. On the Rust FP64 runtime, CPU latency at 2,048 tokens is 26.8
  seconds, of which the token head is 0.14 s and the decoder 0.0015 s.
- **Protocol deviations.** The trainer's start-gate tolerance was relaxed
  twice without a protocol amendment, and the frozen gate table was
  regenerated in place ([Protocol deviations](#protocol-deviations)).

## Shipped configuration

On October 31, 2026 the shipped span configuration was set to the release
plus the S0+ seed-17 token head. That head was trained on the frozen release
seed-17 fit (encoder and document heads frozen) with the spliced cohort. S1+
is not adopted. Its uncalibrated human-document scores are clearly worse
([Human documents](#human-documents)). At its 0.1% CAL point it flags 6
consumed-Test humans; the release flags none. The release's document outputs,
CAL document map and operating points are unchanged.

A private protocol addendum (v1.3-ship) was frozen before any metric below
was computed for this artifact. It fixed the remaining choices:

- the decoder rule;
- a grid extension, used only if no point met the human limit;
- the CAL span maps;
- a single `span_report` evaluation.

The v1.1 run had already computed the S0+ seed-17 decoder grid as a
conditional step. The fresh grid reproduces it row for row.

- **Document outputs equal the release's.** On Dev-iter, CAL and the
  retrospective Test, the raw outputs and predictions of S0+ seed 17 are
  byte-identical to the release's. Gates D1–D4 and every release document
  number therefore hold unchanged.
- **Decoder.** The v1.1 rule on the 108-point grid selects the same point as
  for S1+: `tau_hi` 0.7, `delta` 0, minimum 5 words, gap 0, sentence snap at
  80%.
  - Exactly 22 of the 570 Dev-iter humans carry a span (limit 22, zero
    slack). Only 2 of the 108 points meet the limit, and they differ only in
    the snap.
  - The grid tilts word probabilities to the uncalibrated document share.
    With the shipped artifact's calibrated tilt (the release CAL document
    map), 20 of the 570 Dev-iter humans carry a span. This count was
    computed after the choices were frozen.
  - The declared extension (larger `tau_hi` and minimum word counts) was not
    needed.
  - On the 2,011 mixed documents of the selection population, character F1
    is 0.559 (precision 0.924, recall 0.533), against 0.620 for S1+ seed 17.
  - With the same decoder, S0+ seeds 29 and 41 put spans on 19 and 17
    Dev-iter humans, within the limit. S1+ seeds 29 and 41 put spans on 30
    and 29.
- **CAL maps.** The maps were refitted for this artifact as for S1+, on the
  release's calibrated CAL document outputs with the chosen decoder:
  - sentence confidence: 58,814 sentences, 148 knots;
  - span confidence: 7,875 decoded CAL spans (664 with fewer than half AI
    words), 36 knots.

  The same fit from the S0+ raw heads, with the release map applied in the
  evaluator, gives identical maps.

**`span_report`.** The 823 spliced documents were scored once, after the
decoder and maps were frozen. The [`span_report`
qualifiers](#span_report-documents-held-out-their-parents-were-not) apply:
precision is conditional on decoding, and every document edits a Dev-iter
human from the decoder's human-limit population.

| Output | Word Brier (tilted / raw) | Character F1 (P / R) | Span F1 (P / R) | Mean IoU | Sentence share MAE | Documents without a span |
|---|---|---|---|---:|---:|---:|
| S0+ seed 17, identity maps | 0.129 / 0.104 | 0.410 (0.860 / 0.401) | 0.241 (0.412 / 0.297) | 0.306 | 0.232 | 344 |
| **S0+ seed 17, calibrated (shipped)** | 0.128 | 0.410 (0.862 / 0.402) | 0.245 (0.425 / 0.300) | 0.306 | 0.234 | 346 |
| S1+ seed 17, identity maps | 0.083 / 0.071 | 0.571 (0.937 / 0.538) | 0.426 (0.616 / 0.454) | 0.452 | 0.160 | 234 |
| S1+ seed 17, calibrated | 0.083 | 0.560 (0.939 / 0.529) | 0.420 (0.615 / 0.449) | 0.443 | 0.159 | 242 |

- The shipped artifact decodes no span in 346 of the 823 documents (42%), so
  its precision covers 477 documents. S1+ decodes no span in 242 (29%).
- **Character F1 by policy** (S0+ / S1+, seed 17, identity maps): most of
  the document 0.854 / 0.880, paragraph 0.601 / 0.702, insertion
  0.461 / 0.705, condensation 0.485 / 0.652, compound 0.482 / 0.613,
  scattered 0.358 / 0.550, one sentence 0.170 / 0.402, clause
  0.040 / 0.113. Documents with an AI share under 10% reach 0.101 / 0.257.
  The calibrated S0+ artifact reaches 0.167 on one sentence and 0.051 on
  clauses.
- **Calibration** (calibrated seed 17, S0+ against S1+):

  | Measure | S0+ | S1+ |
  |---|---:|---:|
  | Sentence-confidence ECE | 0.086 | 0.058 |
  | Sentence-confidence Brier | 0.142 | 0.103 |
  | Sentence-share ECE | 0.070 | 0.024 |
  | Span-confidence ECE | 0.063 | 0.038 |

  The identity map's sentence ECE is 0.178 for S0+.
- **Boundaries** (matched spans, calibrated seed 17): the median start and
  end errors are 0 words. The P90 is 17 words at the start and 14 at the
  end, over 402 matched spans. S1+ has 11 and 9 over 589.

**Retrospective, Test consumed; not selection evidence.** S0+ seed 17 with the
chosen decoder, identity maps and uncalibrated tilt. S1+ seed 17 is shown
beside it on the same basis.

| Population | Documents | S0+ word Brier (tilted / raw) | S0+ character F1 (P / R) | S0+ span F1 | S1+ seed-17 character F1 |
|---|---:|---|---|---:|---:|
| Mixed, all | 5,239 | 0.205 / 0.214 | 0.478 (0.934 / 0.451) | 0.243 | 0.501 |
| Planner sentence runs | 2,715 | 0.146 / 0.154 | 0.582 (0.934 / 0.548) | 0.300 | 0.621 |
| Hosted frontier rewrites | 346 | 0.168 / 0.181 | 0.522 (0.941 / 0.493) | 0.218 | 0.570 |
| Full rewrites | 615 | 0.098 / 0.161 | 0.808 (1.000 / 0.761) | 0.411 | 0.790 |
| OLMo-2 base-model continuations and splices | 1,563 | 0.423 / 0.405 | 0.021 (0.431 / 0.023) | 0.007 | 0.020 |
| Model-only | 3,533 | 0.003 / 0.009 | 0.994 (1.000 / 0.992) | 0.961 | 0.990 |

- **Test humans with a decoded span:** 159 of 6,132 (2.6%) with identity
  maps and 156 (2.5%) for the calibrated artifact. S1+ seed 17 has 173 and
  153. The calibrated artifact's 156 include 25 of 1,980 learner essays and 10
  of 866 spoken-conversation transcripts.
- **Calibrated artifact.** Sentence-confidence ECE is 0.010 over all Test
  sentences and 0.150 on mixed documents. Span-confidence ECE is 0.008.
- **Document outputs** equal the release's: involvement Brier 0.0645, share
  MAE 6.86 points, 49 of 6,132 humans flagged at the 1% point and none at
  0.1%.

**Shipped package.** The calibrated v3 export of S0+ seed 17 is the shipped
package. Its encoder, document-head weights and CAL document map are R0's bit
for bit. Through one Rust runtime build, its document outputs equal the R0
package's bit for bit on 227 requests: R0's 3 reference cases, 9 latency
requests, 15 synthetic cases and the 200 local Dev-iter replay cases. It passes R1
([Rust replay and latency](#rust-replay-and-latency),
[baseline release](a-baseline-release.md#span-head-export-v3)).

## Fits

Fourteen fits, all from the release lineage, with seeds 17, 29 and 41 and one
epoch each. S0 arms start from the release recipe's seed-s fit; S1 arms start
from the S0 fit of the same data and seed:

| Arm | Data | Training | Trainer time per fit | Selected checkpoints (seed 17 / 29 / 41) |
|---|---|---|---:|---|
| S0- | release Train (83,956 weighted rows) | token head only | 7.5 min | 4 / 2 / 4 of 4 |
| S0+ | release Train + 38,292 spliced rows | token head only | 10 min | 4 / 3 / 1 of 4 |
| S1- | release Train | joint, lambda 1.0 | 28 min | 5 / 2 / 3 of 8 |
| S1+ | release Train + spliced | joint, lambda 1.0 | 38 min | 7 / 6 / 3 of 8 |

- In the plus bundle the spliced rows hold 25.0% of the AI-involved
  document-loss mass and 60% of the token-loss mass. The human share of the
  document-loss mass is unchanged at 34.5%.
- **Lambda.** One seed-17 fit per data arm at lambda 0.3 and 1.0. Both pass
  gate D1's point estimate in both arms. Lambda 1.0 has the lower primary span
  metric in both (S1-: 0.1389 against 0.1416; S1+: 0.1194 against 0.1239).
- **Checkpoints.** In every S1 fit the document Brier first rises above the
  0.002 guard and then recovers as the learning rate decays; no joint step
  was rejected.
- S0 fits leave the encoder and document heads bitwise unchanged. Their
  Dev-iter and CAL document outputs are byte-identical to the release.
- The 14 completed fits took 5.3 GPU hours of trainer time against the
  20-hour ceiling; scoring, exports and the CAL runs added about half an hour.

Three fit starts stopped in the trainer's padded-microbatch start gate before
any optimizer step and were rerun from the same initialization after the gate
was relaxed ([Protocol deviations](#protocol-deviations)).

## Selection

The primary span metric is the tilted word Brier on the mixed documents of
the Dev-iter bundle plus the `span_select` half (2,011 documents). It tilts to
the uncalibrated document share and reads no CAL quantity. Lower is better.

| Arm | Seed 17 | Seed 29 | Seed 41 | Mean | All gates |
|---|---:|---:|---:|---:|---|
| S0- | 0.1435 | 0.1449 | 0.1459 | 0.1448 | pass |
| S0+ | 0.1392 | 0.1402 | 0.1443 | 0.1412 | pass |
| S1- | 0.1389 | 0.1485 | 0.1378 | 0.1417 | fail |
| S1+ | 0.1194 | 0.1176 | 0.1246 | **0.1205** | pass |

The top two passing arms are S1+ and S0+. The S0+ − S1+ paired three-seed
difference is +0.0207, with a 95% interval of [+0.0178, +0.0235]. The
interval excludes zero, so S1+ is selected. S0+ also beats S0- (+0.0035,
interval [+0.0031, +0.0040]).

## Gates

Seed-17 fits against release seed 17 on Dev-iter unless stated:

| Gate | Rule | S0- | S0+ | S1- | S1+ |
|---|---|---|---|---|---|
| D1 artifact | paired 95% upper bound <= +0.003 | 0 (identical) | 0 (identical) | -0.0015, upper +0.0013 | -0.0009, upper +0.0028 |
| D1 recipe | three-seed mean <= 0.06663 | 0.06403 | 0.06403 | 0.06323 | 0.06386 |
| D2 CAL 1% point | <= 8 of 1,140 CAL humans | 8 | 8 | **9** | 7 |
| D2 CAL 0.1% point | 0 CAL humans | 0 | 0 | **1** | 0 |
| D2 Dev-iter at CAL 1% | <= 11 of 570 | 11 | 11 | **12** | 7 |
| D3 share MAE (points) | <= release (8.73) + 0.5 | 8.73 | 8.73 | 8.05 | 8.50 |
| D4 attribution top-1 | >= release (0.593) - 0.01 | 0.593 | 0.593 | **0.577** | 0.601 |
| R1 Rust replay | <= 1e-5, offsets equal | pass | pass | **fail** (2.1e-5) | pass |

- S1+'s three-seed Dev-iter Brier is 0.06386 against 0.06403 for the release
  recipe (difference -0.0002, interval [-0.0037, +0.0036]).
- S1+'s own CAL refit has CAL TPR 0.819 at its 1% point (release 0.831) and
  0.751 at 0.1% (release 0.712).
- S1- fails on its own CAL refit (D2), on attribution (D4) and on one local
  replay case whose token probability differs by 2.1e-5 (R1). The R1 failure
  is consistent with FP64 rounding differences rather than a defect: its
  raw h (3.5e-6) and output (3.5e-7) errors are in line with the passing
  arms, and all 502 local span offsets are equal. It does not affect
  selection, because S1- also fails D2 and D4.
- **D3 on mixed documents.** D3 is the share MAE over all origins. On mixed
  documents alone, seed 17 worsens from 17.75 to 18.44 points (+0.69, more
  than the gate's 0.5 tolerance). The other origins improve enough that the
  all-origin figure falls from 8.73 to 8.50. The three-seed mixed-document
  figure improves from 18.06 to 17.68.
- **R1 margin.** The candidate's calibrated output error, 8.5e-6, is close to
  the 1e-5 bound. Steep segments of the isotonic map amplify FP64
  differences of about 1e-7: the local output error is 9.9e-8 uncalibrated
  and 8.5e-6 calibrated, and the word-probability error 4.4e-7 and 5.0e-6.
  Passing on the 215 replay cases does not establish the bound on other
  inputs.

### Human documents

D1 is a balanced Brier over every Dev-iter document of the primary
population, so it cannot separate human from AI-involved documents. S1+
passes it because better scores on AI-involved documents offset clearly worse
uncalibrated scores on human documents. The joint fine-tune therefore changes
the document outputs in substance, although the aggregate gate passes.

Uncalibrated, Dev-iter primary population (570 humans, balanced weights):

| Measure | Release | S1+ |
|---|---:|---:|
| Human-only Brier, seed 17 / three-seed mean | 0.0662 / 0.0611 | 0.0831 / 0.0930 |
| AI-involved-only Brier, seed 17 | 0.0638 | 0.0532 |
| Humans at involvement 0.5 or more, seeds 17 / 29 / 41 | 38 / 33 / 42 | 58 / 56 / 71 |
| Human ECE (10 bins), three-seed mean | 0.156 | 0.220 |

- The paired three-seed human-only difference, S1+ minus release, is
  +0.0319 with a 95% interval of [+0.0250, +0.0392]. S1-, the same joint
  fine-tune without the spliced rows, moves it by only +0.0042
  [+0.0010, +0.0078]. The shift therefore comes mainly with the spliced Train
  rows, which are mostly unchanged human text labelled AI-involved (on the
  Dev-iter halves, about 70% of a spliced document's bytes are parent text).
- S1+'s highest count at 0.5, 71 humans on seed 41, exceeds V6's 70 on the
  same documents.
- By human genre (paired three-seed difference), every genre with at least
  25 source groups is worse, with intervals above zero. News is +0.057
  [+0.019, +0.110], science +0.054 [+0.033, +0.080], citizen-media news
  +0.042 [+0.020, +0.068], web +0.030 [+0.013, +0.047], encyclopedia +0.025
  [+0.015, +0.036] and government +0.019 [+0.005, +0.035]. The five smaller
  genres (2 to 10 groups each) range from -0.017 to +0.025.
- **After CAL.** With each system's own CAL map, S1+ scores humans better on
  Brier (Dev-iter 0.0759 release, 0.0698 S1+, seed 17), so calibration
  absorbs most of the shift. More humans still reach 0.5 (36 against 41 of
  570). At the CAL 1% threshold S1+ flags fewer Dev-iter humans (7 against
  11, gate D2).
- **Retrospective, Test consumed; not selection evidence** (seed 17, 6,132
  Test humans). Uncalibrated, the human-only Brier rises from 0.0403 to
  0.0477 and the humans at 0.5 from 271 to 314. With each system's CAL map
  the human-only Brier is 0.0459 (release) against 0.0409 (S1+), with 237
  against 246 humans at 0.5. At its 0.1% CAL point the candidate flags 6 Test
  humans; the release flags none.

## Decoder and calibration

**Decoder** (chosen on the S1+ seed-17 fit, Dev-iter selection population):
`tau_hi` 0.7, `delta` 0, minimum 5 words, gap 0, sentence snap at 80%.

- Balanced character F1 is 0.620 on the selection mixed documents, with 22 of
  the 570 Dev-iter human documents carrying a span (the limit is 22).
- Only 2 of the 108 grid points meet the human-document limit. The best
  unconstrained point has F1 0.728, but 102 of the 570 humans carry a span.
- **The grid is exhausted at its conservative corner.** The chosen point has
  the grid's largest `tau_hi` (0.7), `delta` 0, the largest minimum (5
  words) and gap 0. The two qualifying points differ only in the sentence
  snap, and both put spans on exactly 22 humans. No grid point has fewer, so
  the limit holds with zero slack and the grid had no more conservative
  setting to offer.
- The same decoder gives 30 and 29 humans with spans on S1+ seeds 29 and
  41, above the limit chosen on seed 17. On the consumed Test (retrospective),
  153 to 193 of the 6,132 humans carry a span
  ([Retrospective Test report](#retrospective-test-report)).

**CAL maps.** Clipped isotonic maps were fitted on CAL with the chosen
decoder, tilting to the calibrated CAL share; the 34 meta-flagged CAL rows
were excluded.

- Sentence confidence: 58,814 sentences (20,110 without AI words, 38,704
  with), 170 knots.
- Span confidence: 6,678 decoded spans (404 with fewer than half AI words),
  34 knots.
- The document calibration is S1+'s own CAL refit (the release policy).

## Span metrics

Character and span precision are averaged only over documents with at least
one decoded span. Recall and F1 count every AI-involved document, with F1 0
for a document without a span. Precision is therefore conditional on
decoding, and every precision figure on this page follows this rule.

### Dev-iter selection population (used for selection; optimistic)

Chosen decoder, identity confidence maps, uncalibrated tilt; three-seed means
on the 2,011 mixed documents:

| Arm | Word Brier (tilted / raw) | Token Brier | Character F1 (P / R) | Span F1 | Mean IoU | Sentence share MAE |
|---|---|---:|---|---:|---:|---:|
| S0- | 0.145 / 0.140 | 0.150 | 0.524 (0.919 / 0.501) | 0.261 | 0.365 | 0.250 |
| S0+ | 0.141 / 0.132 | 0.146 | 0.543 (0.923 / 0.520) | 0.280 | 0.384 | 0.244 |
| S1- | 0.142 / 0.135 | 0.146 | 0.564 (0.908 / 0.551) | 0.324 | 0.419 | 0.237 |
| S1+ | **0.121 / 0.109** | **0.124** | **0.634 (0.936 / 0.607)** | **0.430** | **0.501** | **0.207** |

For S1+ seed 17 by cohort (character F1): planner sentence runs 0.573,
plan-v2 operations 0.671, full rewrites 0.799, spliced documents 0.560.
Model-only documents miss 0.9% of their words.

### `span_report` (documents held out; their parents were not)

The 823 spliced Dev-iter documents of the report half, scored once after the
CAL fits. No choice scored these documents, and they share no source group
or parent with `span_select`. They are not independent of the choices,
though:

- Every one of them is an edit of a Dev-iter bundle human (283 distinct
  parents in 249 source groups), and about 70% of each document's bytes are
  unchanged parent text.
- Those parents are among the 570 humans of the decoder's human-document
  limit, which was binding (22 of 22). They are also in the S1 trainer's
  document guard, the Dev-iter whole-split involvement Brier. The trainer's
  checkpoint metric scored the bundle's mixed documents, which share 248 of
  the 249 source groups.

Span precision and character F1 here are therefore somewhat optimistic.

Precision is conditional on decoding. The uncalibrated seeds decode no span
in 234, 221 and 247 of the 823 documents. The calibrated artifact decodes no
span in 242 (29%), which leaves 581 documents for its precision.

| Output | Word Brier (tilted / raw) | Character F1 (P / R) | Span F1 (P / R) | Mean IoU | Sentence share MAE |
|---|---|---|---|---:|---:|
| S1+, three-seed mean | 0.088 / 0.074 | 0.570 (0.918 / 0.548) | 0.425 (0.605 / 0.464) | 0.460 | 0.171 |
| S1+ seed 17, calibrated | 0.083 | 0.560 (0.939 / 0.529) | 0.420 (0.615 / 0.449) | 0.443 | 0.159 |

Seed-level tilted word Brier: 0.083, 0.086 and 0.096. Calibration of the
seed-17 artifact on `span_report`:

- Sentence confidence: ECE 0.058 and Brier 0.103 against "the sentence
  contains an AI word" (three-seed ECE 0.122 with the identity map).
- Sentence share: ECE 0.024 against the true AI word fraction.
- Span confidence: ECE 0.038 against "at least half the span's words are AI".

For matched spans (seed 17) the median start and end errors are 0 words; the
P90 is 11 words at the start and 9 at the end.

Three-seed character F1 by slice:

- **Policy:** most of the document 0.893, paragraph 0.715, insertion 0.696,
  condensation 0.649, compound 0.624, scattered 0.558, one sentence 0.378,
  clause 0.114.
- **Generator:** Gemma-3 0.649, Phi-4-mini 0.631, Qwen3.6 0.573, Granite
  0.560, ERNIE 0.557, Mistral 0.525, SmolLM3 0.476.
- **Domain:** Project Gutenberg 0.661, Global Voices 0.591, Wikinews 0.582,
  FineWeb 0.567, Wikipedia 0.566, PLOS 0.551, Federal Register 0.537.
- **Share bin:** under 10% 0.237; 10–25% 0.565; 25–50% 0.667; 50–75% 0.747;
  75% and over 0.910.
- **Typography:** normalized stratum 0.561; raw stratum 0.580 (raw documents
  with a marker character the parent never uses 0.767, without 0.506).
- **Change fraction:** edits with change fraction 0.3 or more 0.557 (683
  documents); 0.1–0.3 0.234 (20); insertions only 0.696 (119).
- **Copied words.** With the words copied from the replaced human text
  removed from scoring, the calibrated seed-17 artifact has word Brier 0.070
  and character F1 0.566. Documents with a copied run of 8 or more words have
  character F1 0.608 and word Brier 0.149 (three-seed means).

### Consistency

- After the tilt, the mean word probability equals the emitted document share
  within 6e-16 in every evaluated document.
- Before the tilt, the mean raw word probability differs from the document
  share by 0.061 on average on the selection mixed documents (S1+, three-seed
  mean) and by 0.056 on `span_report`.
- The tilt costs word Brier on spliced documents (`span_report` 0.088 tilted
  against 0.074 raw): there the document share head is less accurate than the
  token head, and the consistency rule follows the share.

## Rust replay and latency

FP64 Rust runtime (`candle_f64_full_a_v2`) against the FP64 Python reference
of the trained function. Each set holds 15 synthetic cases and 200 local
Dev-iter documents.

| Export | Max token-probability error | Max output error | Span offsets equal |
|---|---:|---:|---|
| S0- seed 17, uncalibrated | 5.7e-6 | 1.9e-7 | 26/26, 563/563 |
| S0+ seed 17, uncalibrated | 6.1e-6 | 3.0e-7 | 23/23, 554/554 |
| S1- seed 17, uncalibrated | **2.1e-5** | 3.5e-7 | 28/28, 502/502 |
| S1+ seed 17, uncalibrated | 2.9e-6 | 1.0e-7 | 21/21, 528/528 |
| S1+ seed 17, calibrated (candidate) | 2.9e-6 | 8.5e-6 | 5/5, 203/203 |
| **S0+ seed 17, calibrated (shipped)** | 6.1e-6 | 3.0e-7 | 11/11, 241/241 |

- The FP32 Python function is reported as a drift diagnostic only. For the
  candidate it differs by up to 4.9e-5 in token probability and 3.6e-5 in
  raw h.
- **Latency** (Rust FP64 runtime built at commit `958fdad4`, retained as
  binary `full_a-df6c38e5` under its branch ID ([commit
  aliases](a-spans-aliases.md)), profile `candle_f64_full_a_v2`, one thread,
  median of 3; reported, not gated):

  | Tokens | Encoder | Token head | Decoder |
  |---:|---:|---:|---:|
  | 512 | 2.92 s | 0.036 s | 0.0004 s |
  | 1,024 | 8.44 s | 0.072 s | 0.0008 s |
  | 2,048 | 26.65 s | 0.143 s | 0.0015 s |

  The release takes 2.96, 8.80 and 27.28 s in total at the same lengths.
- **Shipped package latency** (the compatibility runtime, same profile, one
  thread, median of 3, on a shared host): 2.94, 8.85 and 28.2 s in total at
  512, 1,024 and 2,048 tokens, of which the token head takes 0.035, 0.072
  and 0.143 s. R0's package on the same runtime, run between two package
  runs, took 3.09, 8.54 and 27.9 s
  ([baseline release](a-baseline-release.md#runtime)).

## Retrospective Test report

**Retrospective, Test consumed; not selection evidence.** The consumed Tier C
Test bundle (14,904 documents: 6,132 human, 3,533 model-only, 5,239 mixed) was
scored once, after every choice above was frozen and the `span_report` result
recorded. Nothing was changed afterwards. The release's document outputs are
represented by the S0- seed-17 fit, whose Test raw outputs are byte-identical
to the release's.

**Spans** (S1+, chosen decoder, identity maps, uncalibrated tilt; three-seed
means):

| Population | Documents | Word Brier (tilted / raw) | Character F1 (P / R) | Span F1 | Mean IoU |
|---|---:|---|---|---:|---:|
| Mixed, all | 5,239 | 0.198 / 0.193 | 0.518 (0.943 / 0.487) | 0.309 | 0.393 |
| Planner sentence runs | 2,715 | 0.130 / 0.123 | 0.644 (0.948 / 0.607) | 0.400 | 0.498 |
| Hosted frontier rewrites | 346 | 0.152 / 0.149 | 0.581 (0.961 / 0.539) | 0.320 | 0.427 |
| Full rewrites | 615 | 0.109 / 0.133 | 0.809 (1.000 / 0.755) | 0.427 | 0.577 |
| OLMo-2 base-model continuations and splices | 1,563 | 0.431 / 0.413 | 0.022 (0.401 / 0.023) | 0.008 | 0.014 |
| Model-only | 3,533 | 0.004 / 0.008 | 0.992 (1.000 / 0.989) | 0.947 | 0.966 |

- Seed-level primary metric on Test mixed documents: 0.202, 0.195 and 0.199.
- Of the 6,132 Test humans, 173, 193 and 183 carry a decoded span (seeds 17,
  29 and 41; 2.8–3.1%). The calibrated seed-17 artifact flags 153 (2.5%),
  including 33 of 1,980 learner essays and 14 of 866 spoken-conversation
  transcripts. The Dev-iter limit was 3.9%.
- The base-model robustness cohort (OLMo-2 base continuations and splices) is
  essentially undetected at the span level. No base checkpoint was used in the
  spliced Train data.

**Calibrated seed-17 artifact.** Sentence-confidence ECE is 0.014 over all
Test sentences and 0.157 on mixed documents; span-confidence ECE is 0.012.

**Document outputs** (balanced weights over all Test documents; each system
at its own CAL operating points):

| Output | Involvement Brier | Share MAE (points) | Humans flagged at 1% | TPR at 1% | Humans flagged at 0.1% | TPR at 0.1% |
|---|---:|---:|---:|---:|---:|---:|
| Release | 0.0645 | 6.86 | 49 of 6,132 | 0.713 | 0 | 0.606 |
| S1+ seed 17 | 0.0637 | 6.06 | 30 of 6,132 | 0.704 | 6 | 0.643 |

The paired Brier difference, candidate minus release, is -0.0008 with a 95%
source-group interval of [-0.0021, +0.0004]. These figures use the span
evaluator's balanced weighting over all Test documents. They are not the Tier
C protocol's populations and are not comparable to its verdict. On Test
humans alone the candidate is worse uncalibrated and better calibrated
([Human documents](#human-documents)).

## Protocol deviations

None of these changes the training computation or the selection, but each
departs from the frozen record:

- **Start gate relaxed twice, without an amendment.** Before the first
  optimizer step the trainer compares padded and unpadded passes. Three starts
  stopped there, all of them in arms trained on the spliced data. The first
  was S0+ seed 29: its token-head logits differed by 1.7e-4 against a 1e-4
  logit bound. Commit `cc356b9b` (22:32Z on October 30) then bounded token
  probabilities at 1e-4. Both S1+ lambda starts then differed by 1.6e-4 in
  probability. Commit `d6dd8b45` (23:52Z) raised the bound to 1e-3. Both
  commits change only the trainer's start-gate function, so the training
  computation is unchanged, and the stopped starts were rerun from the same
  initialization. Protocol spans-v1.1 requires a v1.2 amendment record for
  any later change, and none was written at the time. A private v1.2 addendum
  now records both changes.
- **"Before any span metric was used for a choice" is inaccurate.** The
  private implementation record dates both changes to that point. Within-fit
  checkpoint selection on the Dev-iter span metric had already run. S0- seeds
  17, 29 and 41 and S0+ seeds 17 and 41 had finished by the time the first
  change was recorded. All six S0 fits and both S1- seed-17 lambda fits
  finished before the second. The arm-level choices (lambda, arm, decoder)
  came after both changes.
- **Gate table regenerated in place.** `eval/gates.json` was rewritten at
  03:58Z on October 31, after the choices were frozen (02:54Z). The Test raw
  scoring job had started a few seconds earlier but wrote no output until
  04:01Z, so no Test result existed at the rewrite. The rewrite added S1-'s
  three-seed recipe value (0.06323, a D1 recipe pass). The frozen hash,
  `c55d8420…`, can no longer be checked against a file. The current file,
  `8d16c32f…`, is recorded only in the freeze record's addendum. The frozen
  selection record, whose hash still matches, gives the same gate verdicts
  except S1-'s D1 recipe (then not computed). Rerunning the selection on the
  regenerated table selects S1+ again.

## Limits

These limits describe the shipped configuration: the release's document
outputs plus the calibrated S0+ seed-17 token head. Its document outputs are
byte-identical to the release's, so the release's document-level limits
apply unchanged ([baseline release](a-baseline-release.md)). Facts that hold
only for S1+, the protocol-selected arm that was not adopted, are listed
separately below.

- **Shipped head finds fewer spans.** On `span_report` the calibrated S0+
  head reaches character F1 0.410, against 0.560 for calibrated S1+ seed 17.
  It recovers about 40% of the AI characters (recall 0.402; 0.401 with
  identity maps).
- **Short edits are mostly missed.** Clause edits reach character F1 0.051
  and single-sentence edits 0.167 for the calibrated artifact (0.040 and
  0.170 with identity maps). Documents with an AI share under 10% reach
  0.104 (0.101).
- **Conditional precision.** Character and span precision exclude documents
  without a decoded span. The calibrated artifact decodes no span in 346 of
  the 823 `span_report` documents (42%; 344 with identity maps), so its
  precision covers 477 documents.
- **Conservative decoder with zero slack.** The human-document limit admits
  two grid points. Both sit at the grid's most conservative corner and put
  spans on exactly 22 of the 570 Dev-iter humans, the limit, so the grid
  holds no setting with fewer false spans on human documents. The decoder
  was chosen on seed 17 with the uncalibrated tilt. With the same decoder,
  S0+ seeds 29 and 41 put spans on 19 and 17 of the 570 humans, within the
  limit, and the shipped artifact's calibrated tilt puts spans on 20.
- **Optimistic selection evidence.** Dev-iter humans were seen by the
  baseline's early stopping, and Dev-iter chose every span setting.
  `span_report` documents were not scored for any choice. Their parents are
  Dev-iter bundle humans, however, which bound the decoder's human limit,
  and the trainer's checkpoint metric scored the bundle's mixed documents,
  which share 248 of their 249 source groups. Test is consumed and
  retrospective.
- **Spliced data only on the report half.** `span_report` contains spliced
  documents only; the release-style planner and plan-v2 rows of Dev-iter were
  part of selection.
- **Thin replay margin.** The shipped package's token-probability error
  (6.1e-6) is within a factor of two of the 1e-5 bound, and 215 cases do not
  establish it for other inputs.
- **Typography.** Typography still helps where marker characters remain.
  In the raw stratum of `span_report`, the calibrated artifact reaches
  character F1 0.583 on documents with a marker character the parent never
  uses (106 documents) and 0.338 on the others (283). The three S1+ seeds
  reach 0.767 and 0.506.
- **Base-model text.** Spans written by a base checkpoint are almost never
  found: on the Test OLMo-2 robustness cohort the shipped head reaches
  character F1 0.021 (retrospective). The spliced data used instruct models
  only.
- **Calibration mismatch.** The CAL positives are planner runs and full
  rewrites; the spliced policies appear only in the `span_report` measurement
  of the maps.
- **Token-only cues.** Words copied unchanged inside AI spans count as AI and
  cannot be told apart from text alone.
- **Scope.** Inputs above 2,048 tokens remain unsupported. The f32 Container
  serving build does not have the token head yet.

**S1+ only (protocol-selected, not adopted).** These facts do not describe
the shipped configuration:

- **S1+ changes the document outputs.** The joint fine-tune scores Dev-iter
  humans clearly worse before calibration (human-only Brier 0.061 to 0.093),
  and an aggregate D1 pass does not show this. After CAL it is better on
  Brier, but more humans reach 0.5.
- **S1+ exceeds the human limit on other seeds.** With the decoder chosen on
  seed 17, S1+ seeds 29 and 41 put spans on 30 and 29 of the 570 Dev-iter
  humans, above the limit of 22.
- **S1+ also misses short edits.** S1+ recovers about half the AI characters
  on `span_report` (three-seed recall 0.548). Clause edits (character F1
  0.114) and single sentences (0.378) are mostly missed. Its calibrated
  seed-17 artifact decodes no span in 242 of the 823 documents (29%).
- **S1+ replay margin.** The S1+ candidate's calibrated output error (8.5e-6)
  is within a factor of two of the 1e-5 bound.
- **S1+ at the 0.1% operating point on Test.** S1+ flags 6 Test humans at
  its 0.1% CAL point. The release, and therefore the shipped configuration,
  flags none (retrospective only).

## Follow-up arm S2

Protocol spans-S2 ([pre-registration](a-spans-protocol.md#follow-up-protocol-spans-s2))
asked for an arm with S1+'s span quality but without its human-document
shift. Its fits, gates and post-selection steps ran on October 31 and
November 1, 2026, on the release lineage and the v1.1 data, so the
comparisons with S0+, S1+ and the release stay paired. Gate, selection and
decoder numbers are from Dev-iter (the bundle plus the `span_select` half),
or from CAL where a gate or map needs it. `span_report` (a re-read
population) and the consumed Test (retrospective only) were each evaluated
once, after every choice was frozen. S2c's `span_report` outputs were
produced earlier, before the decoder and maps were frozen, but no metric was
computed on them until after both freezes
([Records and deviations](#records-and-deviations)). Dev-select was never
parsed.

**Outcome: S2 partial.** S2c with tower learning rate 3e-5 is the selected
arm. It is a candidate only:

- The shipped configuration (the release's document outputs plus the S0+
  seed-17 token head) is unchanged.
- Adopting S2c is a separate user decision. Because its document outputs are
  bit-identical to the release's, it fits the standing decision as it is.
- **Adopted on November 1, 2026** as the span head of the S2c span package
  ([reference packages](testnet/reference-packages.md)), not as a reference.
- **It beats the shipped head's recipe.** On the primary span metric, S0+
  minus S2c is +0.0118, with a paired three-seed 95% interval of
  [+0.0104, +0.0131] over 504 source groups.
- **It does not reach S1+.** S2c minus S1+ is +0.0089 [+0.0064, +0.0115]. The
  upper bound is above the margin of 0.0036, so the label is "partial", not
  "success".
- S2a and S2b fail the gates (S2a on H2; S2b on D2 and both H1 forms), so
  neither is ranked.

After selection, S2c stayed the candidate. It was the only ranked arm, so no
other arm could have taken its place.

- **R1 passes** on the seed-17 export. All 215 cases are within the fixed
  gates and every offset is equal. The largest token-probability error is
  4.4e-6, against a gate of 1e-5
  ([Tower export and runtime](#tower-export-and-runtime)).
- **Decoder.** No point of the 108-point grid meets the 22-human limit; the
  fewest humans with a span is 34. The declared extension (576 points)
  chooses `tau_hi` 0.85, `delta` 0.2, a minimum of 10 words, gap 2 and
  sentence snap at 80%. With it, character F1 on the Dev-iter mixed
  documents is 0.640, and 21 of the 570 Dev-iter humans carry a span
  ([Decoder, CAL maps and reports for S2c](#decoder-cal-maps-and-reports-for-s2c)).
- **Later reports, each system at its own decoder.**
  - On `span_report` (a re-read population), with identity maps at seed 17,
    S2c reaches character F1 0.519 and span F1 0.485. S0+ reaches 0.410 and
    0.241, and S1+ 0.571 and 0.426.
  - On the consumed Test (retrospective), with identity maps at seed 17,
    S2c reaches character F1 0.535 on mixed documents. S0+ reaches 0.478 and
    S1+ 0.501.
  - S2c also puts spans on more Test humans: 210 of 6,132 for the calibrated
    artifact, against 156 for the shipped artifact.

### Arms and fits

All arms train on the S1+ data (the release Train bundle plus the 38,292
spliced rows) from the S0+ fit of the same seed, with seeds 17, 29 and 41,
one epoch and token-loss weight 1.0:

| Arm | Mechanism | GPU time per fit | Selected evaluation (17 / 29 / 41, of 8) |
|---|---|---:|---|
| S2a | S1+ joint fine-tune; every document label term replaced by the same term against the frozen release fit's FP32 outputs | 56–63 min | 7 / 6 / 4 |
| S2b | S1+ plus each spliced row's untouched parent as a human contrast entry in the same batch; human document mass unchanged | 53–59 min | 7 / 6 / 6 |
| S2c | encoder and document heads frozen; a 4-layer copy of encoder layers 18–21 and the final norm feeds the token head | 24–27 min | 5 / 6 / 4 |

- **S2c's tower learning rate** was chosen on seed 17 before any other S2c
  seed or any arm comparison. The primary span metric was 0.1276 at 3e-5
  against 0.1334 at 5e-6, so 3e-5 won.
- **S2c's document outputs** are byte-identical to the release recipe's at
  every seed. This was checked on Dev-iter and CAL raw outputs and
  predictions, and its encoder and document heads stay bitwise equal to the
  initial fit.
- **S2a's teacher** matched the initial fit on the 2,873 evaluated Dev-iter
  rows with error 0.
- **S2b** found the parent of every spliced row (38,292 pairs). Human
  document weights were scaled by 0.678, so the total human mass is unchanged
  (relative error 0).
- No joint step was rejected. Every selected checkpoint is after evaluation
  0.
- GPU use was 30,883 s (8.6 h) of the 12-hour ceiling. This includes 2,339 s
  for the smoke and the margin prerequisite. Against the per-fit estimates
  (S2a 57, S2b 51, S2c 21 min), S2a was close, and S2b and S2c fits ran
  2–8 and 3–6 minutes longer.

**One interrupted step.** The S2b seed-29 `span_select` scoring step was
interrupted once:

- On its first attempt the scorer finished (834 rows, exit 0).
- The GPU lock's start watch then found an unrelated GPU process (1,186 MiB)
  and exited 76, which stopped the chain. The chain was restarted a minute
  later.
- On restart, the existing output matched its manifest hashes, so the step
  was not scored again. The recorded output is the first attempt's, and it
  was not re-scored to test determinism.
- It enters only S2b's span metrics. S2b failed the gates, so no gate,
  ranking or selection depends on it.

### Margins

Each margin was fixed by rule before any S2 fit, as a reseed scale (one
sample standard deviation over three seeds), not a tuned value:

- **H1:** `m_H1` = 0.00753. This is the spread of the release recipe's
  human-only Brier (0.0662, 0.0524, 0.0645).
- **H2:** `m_H2` = 1.73 at the 1% point and 2.31 at the 0.1% point. These
  are the spreads of the release recipe's Dev-iter human flags at each seed's
  own CAL points (11, 8, 8 and 0, 4, 0). The release seed-17 control
  reproduced its CAL map, thresholds and counts exactly.
- **Outcome label:** `m_S` = 0.00361. This is the spread of S1+'s primary
  span metric (0.1194, 0.1176, 0.1246).

### Gates

Seed-17 fits against release seed 17 on Dev-iter unless stated. The D rules
are v1.1's; H1 and H2 are new in S2:

| Gate | Rule | S2a | S2b | S2c |
|---|---|---|---|---|
| D1 artifact | paired 95% upper bound <= +0.003 | -0.0032, upper -0.0013 | -0.0016, upper +0.0021 | 0 (identical) |
| D1 recipe | three-seed mean <= 0.06663 | 0.06133 | 0.06250 | 0.06403 |
| D2 CAL 1% point | <= 8 of 1,140 CAL humans | 8 | 8 | 8 |
| D2 CAL 0.1% point | 0 CAL humans | 0 | **1** | 0 |
| D2 Dev-iter at CAL 1% | <= 11 of 570 | 11 | 6 | 11 |
| D3 share MAE (points) | <= release (8.73) + 0.5 | 8.39 | 8.40 | 8.73 |
| D4 attribution top-1 | >= release (0.593) - 0.01 | 0.590 | 0.595 | 0.593 |
| H1 artifact | paired 95% upper bound <= 0.00753 | -0.0033, upper +0.00002 | **+0.0141, upper +0.0219** | 0 |
| H1 recipe | three-seed paired upper bound <= 0.00753 | -0.0019, upper +0.0010 | **+0.0239, upper +0.0313** | 0 |
| H2 artifact, 1% point | <= 11 + 1.73 Dev-iter humans | 11 | 6 | 11 |
| H2 artifact, 0.1% point | <= 0 + 2.31 | **3** | 1 | 0 |
| H2 recipe, 1% point | three-seed mean <= 9.00 + 1.73 | 8.67 | 5.00 | 9.00 |
| H2 recipe, 0.1% point | three-seed mean <= 1.33 + 2.31 | 1.67 | 1.00 | 1.33 |
| R1 Rust replay | <= 1e-5, offsets equal | not run (not selected) | not run (not selected) | pass (215 cases; after selection) |

- **S2c** passes D1–D4, H1 and H2 by construction. This was verified bitwise
  for all three seeds, not assumed.
- **S2a** fails only H2 artifact. Its seed-17 refit flags 3 Dev-iter humans
  at the 0.1% point, against 0 for the release; the limit is 2.31. The three
  come from FineWeb, Global Voices and Wikipedia. Its 0.1% threshold equals
  the release's, and no CAL human reaches it. Its D2 Dev-iter count is 11,
  exactly the limit.
- **S2b** fails D2 (1 CAL human at its 0.1% point; seeds 29 and 41 also have
  one each) and both forms of H1. The paired contrast removed only about a
  quarter of S1+'s human-document shift (+0.0239 against +0.0319).
- CAL TPR at the seed-17 1% and 0.1% points: S2a 0.831 and 0.775, S2b 0.824
  and 0.775, release 0.831 and 0.712.
- For comparison, the gate check before any S2 fit showed that S1+ fails H1
  (seed-17 upper bound 0.0246, recipe 0.0392) and passes H2 artifact on
  Dev-iter (1 human at its 0.1% point, 7 at 1%).

### Human documents in S2

Uncalibrated unless stated, Dev-iter primary population (570 humans,
balanced weights). Paired intervals are three-seed source-group intervals
against the release recipe:

| Measure | Release recipe | S1+ | S2a | S2b | S2c |
|---|---|---|---|---|---|
| Human-only Brier, seeds 17 / 29 / 41 | 0.0662 / 0.0524 / 0.0645 | 0.0831 / 0.0901 / 0.1059 | 0.0630 / 0.0436 / 0.0708 | 0.0804 / 0.0935 / 0.0809 | as release |
| Three-seed mean | 0.0611 | 0.0930 | 0.0591 | 0.0849 | 0.0611 |
| Paired difference against the release recipe | – | +0.0319 [+0.0250, +0.0392] | -0.0019 [-0.0051, +0.0010] | +0.0239 [+0.0170, +0.0313] | 0 |
| Humans at involvement 0.5 or more | 38 / 33 / 42 | 58 / 56 / 71 | 34 / 22 / 45 | 56 / 61 / 62 | as release |
| Human ECE (10 bins), three-seed mean | 0.156 | 0.220 | 0.155 | 0.207 | as release |
| Human-only Brier, own CAL map | 0.0759 / 0.0698 / 0.0746 | 0.0698 (seed 17) | 0.0686 / 0.0657 / 0.0714 | 0.0692 / 0.0669 / 0.0732 | as release |
| Humans at 0.5, own CAL map | 36 / 34 / 57 | 41 (seed 17) | 32 / 34 / 44 | 34 / 40 / 78 | as release |

- **S2a by genre.** Among the genres with at least 25 source groups, only
  encyclopedia is worse with an interval above zero: +0.0039
  [+0.0003, +0.0075], against +0.025 for S1+. The other five range from
  -0.008 (web) to +0.007 (news), and every one of their intervals includes
  zero.
- **S2b by genre.** S2b keeps a smaller version of S1+'s pattern. Science is
  +0.046 [+0.026, +0.074], news +0.046 [+0.005, +0.109], encyclopedia +0.023
  [+0.014, +0.033], citizen-media news +0.023 [+0.001, +0.049] and government
  +0.013 [+0.001, +0.025].
- After CAL, both joint arms score humans better on Brier than the release.
  S2b seed 41 still puts 78 humans at 0.5 or more, against 57 for the release
  seed.

### Span metrics in S2

The primary span metric is v1.1's selection measure: tilted word Brier on the
2,011 mixed documents of the Dev-iter selection population. It is
decoder-free and reads no CAL quantity. Lower is better.

| Arm | Seed 17 | Seed 29 | Seed 41 | Mean | Gates |
|---|---:|---:|---:|---:|---|
| S0+ (shipped head's recipe) | 0.1392 | 0.1402 | 0.1443 | 0.1412 | pass (v1.1) |
| S1+ (v1.1-selected, not adopted) | 0.1194 | 0.1176 | 0.1246 | 0.1205 | fails H1 |
| **S2c-lr3e-5** | 0.1276 | 0.1288 | 0.1320 | **0.1295** | pass |
| S2a | 0.1180 | 0.1220 | 0.1231 | 0.1211 | fail (H2) |
| S2b | 0.1191 | 0.1165 | 0.1226 | 0.1194 | fail (D2, H1) |

The decoded figures below use the v1.1 decoder (the point the v1.3 rule chose
again for the shipped head), identity maps and the uncalibrated tilt. They
are three-seed means on the same 2,011 documents:

| Arm | Word Brier (tilted / raw) | Token Brier | Character F1 (P / R) | Span F1 | Mean IoU | Sentence share MAE | Dev-iter humans with a span (17 / 29 / 41) |
|---|---|---:|---|---:|---:|---:|---|
| S0+ | 0.141 / 0.132 | 0.146 | 0.543 (0.923 / 0.520) | 0.280 | 0.384 | 0.244 | 22 / 19 / 17 |
| S1+ | 0.121 / 0.109 | 0.124 | 0.634 (0.936 / 0.607) | 0.430 | 0.501 | 0.207 | 22 / 30 / 29 |
| **S2c** | 0.129 / 0.109 | 0.132 | 0.626 (0.923 / 0.608) | 0.422 | 0.495 | 0.217 | 34 / 25 / 34 |
| S2a | 0.121 / 0.104 | 0.124 | 0.645 (0.925 / 0.630) | 0.451 | 0.525 | 0.207 | 33 / 21 / 42 |
| S2b | 0.119 / 0.108 | 0.122 | 0.634 (0.942 / 0.603) | 0.426 | 0.493 | 0.204 | 19 / 29 / 27 |

- **S2c recovers most of S1+'s decoded span quality.** Its character F1 is
  0.626 against 0.634 for S1+ and 0.543 for S0+. Its raw word Brier is
  almost S1+'s (0.1093 against 0.1088). The tilt costs S2c more, 0.020
  against 0.012 for S1+, because S2c tilts to the release's document share
  while S1+ retrained its share head. This gap is where most of S2c's
  distance from S1+ on the primary metric comes from.
- **By slice** (character F1; S0+ / S1+ / S2c): spliced documents
  0.417 / 0.568 / 0.536, one sentence 0.175 / 0.326 / 0.298, clause
  0.032 / 0.068 / 0.065, AI share under 10% 0.176 / 0.311 / 0.283.
- **S2c's human spans are above the limit under this decoder.** It puts
  spans on 34, 25 and 34 of the 570 Dev-iter humans. S2c's own decoder,
  chosen afterwards by the v1.3 rule on seed 17, brings seed 17 to 21
  ([Decoder, CAL maps and reports for S2c](#decoder-cal-maps-and-reports-for-s2c)).
- **Ungated arms, shown for context only.** These comparisons are not
  selection evidence. S2b minus S1+ is -0.0011 [-0.0020, -0.0003], so S2b
  is slightly better on the metric. S2a minus S1+ is +0.0005
  [-0.0016, +0.0027], level. S0+ minus S2a is +0.0202 and S0+ minus S2b is
  +0.0218.
- After the tilt, the mean word probability equals the emitted document
  share within 6e-16 for every arm.

### Tower export and runtime

The S2c seed-17 fit is exported as export v4 (`slop_ninja_full_a_export_v4`,
heads `slop_ninja_full_a_heads_v3`). The export is uncalibrated, with the
identity maps and the diagnostic decoder, which is the form R1 uses. Export
v3 is unchanged and still serves every fit without a tower.

- **Contents.** The release's encoder file and document heads, the token
  head, and `tower.safetensors`. That file holds 25 FP32 tensors (20,060,928
  parameters): copies of encoder layers 18–21 (global, local, local, global
  attention) and of the final norm. The manifest binds the fit's digests
  (fit, binding, encoder, heads and selected checkpoint), and they equal the
  inputs recorded for the post-selection build.
- **The release's document outputs are unchanged.**
  - The encoder file is the release's (`a167c607…`), and the 12 document-head
    tensors equal the release's bit for bit.
  - The heads differ from the S0+ uncalibrated export only in the token
    head's input and weights and in the tower declaration.
  - The FP64 Python document references equal the S0+ export's bit for bit
    on all 215 reference cases.
  - Through the Rust runtime, the export and the S0+ export give
    bit-identical document outputs on 227 requests: the release's 3
    reference cases, the 9 latency cases, and the 15 synthetic and 200 local
    reference cases.
- **Runtime.** The tower runtime is its own file,
  `grammar-a-baseline/src/full_a_tower.rs`. The heads and spans runtime files
  that the served profile and the artifact-check spec pin are byte-identical
  to main's. The tower branch runs only when the export declares it (export
  v4); it never checks a runtime version. It runs the encoder unchanged, then
  the four copied layers from the hidden state entering layer 18 with the
  encoder's masks and positions, then the copied norm and the token head.
- **Golden vectors.** Vectors from the trainer's own tower module bind the
  branch to the Python function. The FP64 profile agrees to 2.4e-15 when only
  position 0 is used, and to 3.8e-8 otherwise. The 3.8e-8 bound comes from
  rounding in the rotary tables: PyTorch's float32 cos/sin and glibc's differ
  by one ulp in about 5% of entries.
- **Requalification.** The a-v6-cpu source pins were requalified against a
  build of main (`72911dad`). The warm-A roster, the release package and the
  shipped package give bit-identical results to that build.
- **Latency** (FP64, one thread, median of 3 on a shared host with load
  11–25; reported, not gated). The tower adds:

  | Tokens | Tower | Encoder (same run) | Tower / encoder (same run) | Tower / S0+ run's encoder |
  |---:|---:|---:|---:|---:|
  | 512 | 0.53 s | 3.00 s | 18% | 18% |
  | 1,024 | 1.64 s | 8.65 s | 19% | 20% |
  | 2,048 | 5.28 s | 28.9 s | 18% | 20% |

  - A repeat run gave 0.52, 1.63 and 4.85 s for the tower.
  - The S0+ export, run between the two, took 2.96, 8.11 and 26.8 s in the
    encoder. In the export's own runs the encoder took 1–8% longer, so the
    ratio moves by a few percent with the denominator. The structural share
    is 4 of 22 layers (18%). If latency matters for adoption, it should be
    measured again on a quiet host.
  - The token head (0.036, 0.071 and 0.148 s) and the decoder (under 2 ms)
    are unchanged.
  - Peak resident memory rises from 3.29 to 3.48 GiB.
- **R1 passes.** The export was replayed through the Rust FP64 profile (one
  thread) against its FP64 Python reference. The comparison uses the fixed
  gates: 1e-5 for document outputs and for token, word, sentence and span
  quantities, and exact offsets. The largest errors, with the shipped
  package's R1 beside them:

  | Set | Cases | Spans with equal offsets | Token probability | Word probability | Document outputs | Shipped package (token / word) |
  |---|---:|---:|---:|---:|---:|---|
  | Synthetic | 15 | 67 of 67 | 2.1e-6 | 1.7e-6 | 3.0e-7 | 1.4e-6 / 6.0e-7 |
  | Local Dev-iter | 200 | 422 of 422 | 4.4e-6 | 4.2e-6 | 1.9e-7 | 6.1e-6 / 1.5e-6 |

  - Tokenizer, word and sentence offsets are equal, and no case has a
    decision value within 1e-6 of a decoder threshold. The involvement
    logit, raw `h` and shared `z` are within 3.2e-6, the same as the shipped
    package.
  - The largest token-probability error is about 2.3 times inside the gate.
  - The FP32 Python outputs are a drift diagnostic only. As for every
    earlier export, they fail the fixed gates (largest token-probability
    error 8.2e-5).
- **Review fixes before R1.**
  - The runtime now also requires the manifest's tower file to be the
    weights file that the heads' tower declaration names.
  - The exporter checks that each tower copy keeps its source layer's rotary
    base. On the real fit the global copies have base 160,000 and the local
    copies 10,000, each equal to its source layer.
  - Comparison reports carry two added keys (`export_schema`,
    `span_tower_sha256`) under the unchanged report schema, as recorded in
    the a-v6-cpu qualification notes.
  - The rebuilt runtime's outputs equal the earlier build's on all 215
    cases, apart from timing.
  - R1 was run again on the landed sources, with the tower runtime in its
    own file. Every prediction equals the earlier runs' apart from timing,
    and the maxima are unchanged.

### Decoder, CAL maps and reports for S2c

These steps follow the shipped head's v1.3 rules. Each choice was recorded in
the private freeze log before the next step ran. The decoder and maps were
frozen before any `span_report` metric was computed, although the
`span_report` outputs had been produced before both freezes
([Records and deviations](#records-and-deviations)). The Test was read only
after the `span_report` result had been recorded.

**Decoder.** The rule maximizes balanced character F1 on the Dev-iter mixed
documents, with word probabilities tilted to the uncalibrated document share
and identity maps. At most 22 of the 570 Dev-iter humans may carry a span.

- None of the 108 v1.1 grid points meets the limit. The fewest humans with a
  span at any point is 34.
- The extension declared for the shipped head was therefore used. It adds
  `tau_hi` values up to 0.95 and minimum word counts up to 10, for 576
  points. Of these, 200 meet the limit.
- The chosen point is `tau_hi` 0.85, `delta` 0.2, a minimum of 10 words,
  gap 2 and sentence snap at 80%. Its character F1 is 0.640 (precision
  0.922, recall 0.631), with 21 humans carrying a span. The runner-up is the
  same point without the snap (0.638).
- The minimum word count sits at the top of the extended grid. By rule there
  is no further extension.
- For comparison, the shipped head's decoder gives the shipped artifact
  character F1 0.559 with 22 humans.
- With S2c's decoder, the Dev-iter results per seed are:

  | Seed | Character F1 (P / R) | Span F1 | Mean IoU | Dev-iter humans with a span |
  |---:|---|---:|---:|---:|
  | 17 | 0.640 (0.922 / 0.631) | 0.560 | 0.559 | 21 |
  | 29 | 0.622 (0.932 / 0.606) | 0.540 | 0.538 | 22 |
  | 41 | 0.631 (0.914 / 0.625) | 0.553 | 0.549 | 27 |

  The limit applies to seed 17 only. Seed 41 exceeds it.

The decoder grid read Dev-iter from the Dev-iter-only bundle file. A control
recomputed the shipped head's recorded grid from that file and matched it row
for row. The evaluator gained the optional grid field that the extension
specifies. Its output for the 108 v1.1 points is byte-identical to the
previous evaluator's.

**CAL maps.** The maps were fitted as for the shipped head: on CAL, with the
chosen decoder, the release CAL document map and the release's calibrated CAL
outputs. S2c's CAL document outputs are byte-identical to the release's. The
34 meta-flagged CAL mixed rows were excluded.

- Sentence confidence: 58,814 sentences, 154 knots.
- Span confidence: 4,669 decoded CAL spans (222 with fewer than half AI
  words), 35 knots.

The same fit from S2c's raw heads, with the release map applied in the
evaluator, gives identical maps. The fit read CAL from the CAL-only bundle
file. A control recomputed the shipped head's recorded maps from that file
and matched them exactly.

**`span_report` (re-read; not fresh evidence).** The 823 spliced documents
had been scored for S1+ and for the shipped head. Their parents are Dev-iter
humans. For S2c their outputs were produced once, before the decoder and
maps were frozen, and evaluated once, after both freezes. Each system uses
its own decoder. Precision is conditional on decoding.

| Output | Word Brier (tilted / raw) | Character F1 (P / R) | Span F1 (P / R) | Mean IoU | Sentence share MAE | Documents without a span |
|---|---|---|---|---:|---:|---:|
| S2c seed 17, identity maps | 0.111 / 0.074 | 0.519 (0.867 / 0.532) | 0.485 (0.814 / 0.487) | 0.455 | 0.194 | 288 |
| **S2c seed 17, calibrated** | 0.109 | 0.524 (0.868 / 0.537) | 0.494 (0.822 / 0.495) | 0.460 | 0.195 | 283 |
| S2c three-seed mean, identity maps | 0.109 / 0.074 | 0.514 | 0.477 | 0.450 | 0.189 | – |
| S0+ seed 17, calibrated (shipped) | 0.128 | 0.410 (0.862 / 0.402) | 0.245 (0.425 / 0.300) | 0.306 | 0.234 | 346 |
| S1+ three-seed mean, identity maps | 0.088 / 0.074 | 0.570 | 0.425 | 0.460 | 0.171 | – |
| S1+ seed 17, calibrated | 0.083 | 0.560 (0.939 / 0.529) | 0.420 (0.615 / 0.449) | 0.443 | 0.159 | 242 |

- **Raw word Brier matches S1+; the tilted measure does not.** The
  three-seed raw word Brier is 0.074 for both arms. Tilted, S2c's is 0.109
  against S1+'s 0.088, because S2c tilts to the release's document share.
- **S2c's decoded spans are more precise; it covers fewer AI characters.**
  Against S1+, S2c has the higher span F1 (0.477 against 0.425 over three
  seeds; span precision 0.814 against 0.616 at seed 17) and the lower
  character F1 (0.514 against 0.570). Its decoder is more conservative and
  leaves 288 of the 823 documents without a span, against 234 for S1+.
- **Calibration** of the calibrated seed-17 artifacts (S2c / shipped / S1+):
  - sentence-confidence ECE 0.086 / 0.086 / 0.058;
  - sentence-confidence Brier 0.132 / 0.142 / 0.103;
  - sentence-share ECE 0.067 / 0.070 / 0.024;
  - span-confidence ECE 0.084 / 0.063 / 0.038.
- **Boundaries.** The P90 start and end errors are 12 and 12 words over 642
  matched spans. The shipped artifact has 17 and 14 over 402, and S1+ 11 and
  9 over 589.
- The seed-17 document outputs on `span_report` equal the shipped head's.

**Retrospective, Test consumed; not selection evidence.** Seeds 17, 29 and 41
were scored on the 14,904 Test rows once, after the `span_report` result was
recorded. The table shows seed 17 with its decoder, identity maps and the
uncalibrated tilt. S0+ seed 17 (the shipped head) and S1+ seed 17 are beside
it on the same basis, each with its own decoder.

| Population | Documents | S2c word Brier (tilted / raw) | S2c character F1 (P / R) | S2c span F1 | S0+ character F1 | S1+ seed-17 character F1 |
|---|---:|---|---|---:|---:|---:|
| Mixed, all | 5,239 | 0.199 / 0.198 | 0.535 (0.928 / 0.514) | 0.445 | 0.478 | 0.501 |
| Planner sentence runs | 2,715 | 0.134 / 0.127 | 0.664 (0.933 / 0.640) | 0.563 | 0.582 | 0.621 |
| Hosted frontier rewrites | 346 | 0.154 / 0.163 | 0.602 (0.940 / 0.580) | 0.480 | 0.522 | 0.570 |
| Full rewrites | 615 | 0.107 / 0.134 | 0.834 (1.000 / 0.785) | 0.656 | 0.808 | 0.790 |
| OLMo-2 base-model continuations and splices | 1,563 | 0.427 / 0.421 | 0.026 (0.363 / 0.029) | 0.018 | 0.021 | 0.020 |
| Model-only | 3,533 | 0.003 / 0.006 | 0.995 (1.000 / 0.993) | 0.983 | 0.994 | 0.990 |

- **Three seeds.** On all mixed Test documents, S2c's three-seed mean
  character F1 is 0.526, against 0.518 for S1+.
- **Test humans with a decoded span.**
  - S2c with identity maps: 204, 154 and 214 of 6,132 for seeds 17, 29
    and 41.
  - The calibrated S2c artifact: 210 (3.4%).
  - The shipped artifact: 159 with identity maps and 156 calibrated (2.5%).
  - S1+: 173, 193 and 183 with identity maps; 153 for its calibrated seed 17.
  - The calibrated artifact's 210 include 28 of 1,980 learner essays, 15 of
    866 spoken-conversation transcripts and 21 of 150 Wikisource texts. The
    shipped artifact has 25, 10 and 16.
  - S2c's decoder meets the Dev-iter limit, but it does not hold the
    shipped artifact's Test human rate.
- **Calibrated artifact.** Sentence-confidence ECE is 0.008 over all Test
  sentences (shipped 0.010) and 0.157 on mixed documents (shipped 0.150).
  Span-confidence ECE is 0.011 (shipped 0.008).
- **Document outputs.** At all three seeds, the Test raw outputs and
  predictions are byte-identical to the release recipe's. The calibrated
  seed-17 artifact therefore has the release's numbers: involvement Brier
  0.0645, share MAE 6.86 points, and 49 of 6,132 humans flagged at the 1%
  point and none at 0.1%. S1+ at its own CAL points flags 30 and 6.

### Records and deviations

- **Integrity.** Every frozen hash was checked before the gates. The gate
  inputs (CAL refits, release evaluator, human-gate tool) were computed
  before any arm-level S2 span metric. The selection record was written and
  hashed before any post-selection step.
- **Controls.** Every control reproduced the existing records exactly:
  - the release recipe's Dev-iter Brier;
  - the gate tool's per-seed values, checked against the release evaluator
    for all 12 systems;
  - the calibrated release and S1+ human figures and S1+'s by-genre shifts;
  - the S0+ and S1+ span metrics and their human span counts.
- **Amendment.** The only amendment changed the smoke subset strata to
  include the generator label. It was recorded before any S2 fit and before
  any completed smoke fit.
- **CAL span maps.** CAL span maps were not fitted per fit. The protocol
  fits them for the selected arm only, after its decoder is chosen.
- **Early `span_report` outputs.** The protocol scores `span_report` once,
  after every choice is frozen. S2c's three seeds were put through the model
  on `span_report` at 10:08:21–10:09:46Z on November 1, in the same GPU job
  as the `span_select` rescoring control. That was before the G576 decoder
  grid ran (10:13Z), before the decoder was frozen (10:17:16Z) and before
  the CAL maps were frozen (10:18:55Z). The job produced outputs only. The
  first `span_report` metric was computed at 10:18:59Z, after both freezes,
  and neither choice reads `span_report`, so no result is affected. The
  freeze log's decoder entry discloses the early scoring; the private results
  record does not list it as a deviation.
- **Span counts in the gate tables.** The human span counts in
  [Span metrics in S2](#span-metrics-in-s2) use the v1.1 decoder, because
  they were computed before S2c's decoder was chosen.
- **The interrupted step.** Its handling is described under
  [Arms and fits](#arms-and-fits).
- **Dev-select.** For the gates and selection, the span evaluator opened the
  Train-bundle rows file, which also holds Dev-select lines. It skips every
  other-split line by substring before parsing, so no Dev-select line was
  parsed or scored, but the file's bytes were read. The v1.1 span evaluation
  read the same file the same way. The post-selection evaluations read
  Dev-iter and CAL from their own bundle files instead, so they read no
  Dev-select byte. Controls reproduced the shipped head's recorded grid and
  maps from those files exactly.
- **Grid extension.** The extension needed the evaluator's optional grid
  field. It was implemented as the shipped head's protocol specifies, with
  unit tests. Its output for the 108 v1.1 points is byte-identical to the
  previous evaluator's.
- **Review fixes.** An independent review of the tower build found no
  blocking defect. Its minor findings were fixed before R1 (see
  [Tower export and runtime](#tower-export-and-runtime)). The scoring path
  is unchanged since the S2 fits: rescoring the seed-17 `span_select` half
  with the fixed code reproduced the earlier output byte for byte.
- **Test seeds.** The retrospective Test scoring covered three seeds, as for
  S1+ in v1.1. The shipped head's report covered seed 17 only.
- **GPU use.** 31,727 s (8.8 h) of the 12-hour ceiling, including the
  export (56 s) and the post-selection scoring (788 s).

### Limits of S2

These are the limits the protocol stated in advance, followed by those found
in the run:

- **Optimistic human gates.** Dev-iter humans were seen by the baseline's
  early stopping and by v1.1's decoder limit. H1 and H2 on Dev-iter are
  therefore optimistic for every arm, including the release. No fresh human
  population exists in the release lineage.
- **Noisy margins.** Three seeds give a noisy standard deviation. The
  margins are a reseed scale, not a power calculation.
- **`span_report` is not fresh.** It is a re-read population whose parents
  are Dev-iter humans.
- **The Test is consumed.** Every Test number is retrospective and not
  selection evidence.
- **S2a's tilted metric.** S2a's tilted span metric is bounded by the
  release document share on spliced documents. Its raw word Brier, 0.104, is
  the lowest of all arms.
- **S2c's cost.** It adds four encoder layers for the token head: 0.53, 1.64
  and 5.28 s at 512, 1,024 and 2,048 tokens on one CPU thread, about 18–20%
  of encoder time on a shared host. It needs the export v4 runtime branch.
  R1 passes, but the largest token-probability error is only about 2.3
  times inside the gate.
- **The human limit is tight.** S2c's decoder meets the 22-human limit on
  seed 17 (21) and seed 29 (22), but not on seed 41 (27). It sits at the top
  of the extended grid's minimum word count. On the consumed Test it puts
  spans on more humans than the shipped artifact (210 against 156 of 6,132).
- **No calibrated S2c package.** R1 ran on the uncalibrated export, as the
  protocol specifies. A calibrated v4 export with the S2c decoder and maps
  has not been built or replayed.

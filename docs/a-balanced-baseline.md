# Shared A on fresh source families

The [frozen final-Test evaluation](a-final-test.md) is complete. This page retains
Development evidence and the selection rationale; final quality and coverage
are reported separately. The selected model remains `warm_text`, and Test is
excluded from all further fitting and selection.

Measured on build-host, October 26, 2026. The best current shared A candidate
starts from V6's original classifier with a learned text-only residual. It
improves mixed-inclusive involvement and share estimates over the earlier head
architecture. Adding native CFG counts does not improve its primary involvement
comparison or overall share error. Aggregate CFG alone remains inadequate. **This is
Development evidence, not final-Test qualification or Pangram parity.**

The subsequent function-preserving V6 warm-start comparison completed six more
fits. Its text-only candidate is the current Development selection, mean balanced
involvement Brier **0.03791**, versus **0.04454** with added CFG. Both began with
V6's original origin predictions. All results below remain Development evidence;
Test was unopened at selection. Preserve the first-stage selection as a separate receipt.

A subsequent Train expansion to 1,264 queries from 108 works completed all six
matched residual fits and Rust replays. It worsened Development involvement and
share performance; the earlier text-only selection remains current. The new
examples and fitted artifacts are retained. See the Train-expansion comparison
below; more data with a different domain mix is not a pure sample-count test.

A subsequent [source admission](a-training-data-admission.json) added 196
historical records from original V6 Train and 29 independent books, growing the
available source-work pool from 108 to 333. The books add works for five existing
writer profiles; individual writers in the other sources remain masked.
Three uncertain or editorially mixed editions were excluded. Exact sources,
rights, byte spans and cumulative duplicate checks are retained. The completed
comparison below left the earlier model selected before final Test opened.

Generation and assembly for those new roots have completed. The historical
batch retains 2,104 records after two terminal generation failures and 50
duplicate exclusions; the books retain all 261 records. Every failed or
excluded output remains in the evidence. Combined with prior Train data, the
pool contains at most 3,629 queries before cumulative duplicate removal.
Feature qualification and all twelve new fits have completed on exactly 3,629
Train queries. The [source-size results](a-source-size-results.json) show text
Development Brier improving from 0.05047 to 0.04618 to 0.04390 across 72, 132 and
333 works under the fixed weighting scheme. The earlier 0.03791 text residual
remains selected; full-data text+CFG gives 0.04933. The sixteen eligible joint
configurations and all three seeds are now bound in an immutable final selection
receipt. Final evaluation follows that unchanged receipt.

## Source-size comparison

The three nested levels contain 845, 1,469 and 3,629 Train queries from 72, 132
and 333 independent works. Each retains the original 12 works at half the loss
mass; five added source strata receive one tenth each, with equal family and
observed-origin weights inside each stratum. All descendants follow their work.
Features, normalization, profiles, initialization, architecture, optimizer and
250-step budget are frozen. The table averages seeds 17/29/41 on the unchanged
288-query, 16-family Development population; Calibration alone fits temperatures
and attribution thresholds.

| Input and Train works | Balanced involvement Brier ↓ | Human FPR at 0.5 ↓ | Share MAE, points ↓ |
| --- | ---: | ---: | ---: |
| Earlier text residual, 12 | 0.03791 | 2.08% | 8.46 |
| Text, 72 | 0.05047 | 7.29% | 8.70 |
| Text, 132 | 0.04618 | 3.13% | 8.45 |
| Text, 333 | 0.04390 | 5.21% | 8.18 |
| Text+CFG, 333 | 0.04933 | 6.25% | 8.93 |

The 333-minus-72 text Brier difference is −0.00657, with a paired family interval
of [−0.01278, −0.00140]. The adjacent-level intervals include zero. The full-data
text candidate remains worse than the earlier selection by +0.00599
[−0.00660, +0.01840]; it does not replace it under the predeclared mean-Brier rule.
Adding CFG changes Brier by +0.00543 [−0.00344, +0.01411] and increases share MAE
by 0.74 points [0.27, 1.28]. The original selected model still trails CAL-mapped
Pangram by +0.01894 [0.00172, 0.03405]. No additional paid calls were needed;
all 702 retained Dev/CAL observations were reused. These intervals resample
source families after averaging the three fixed seeds, not individual examples.

Full-data text known-generator top-1 reaches 80.21%, up from 77.21% for the
earlier selection (paired gain 2.99 points [1.17, 4.82]). Known-human top-1 is
30.16%, versus 31.15%; its interval includes zero. The small share-MAE gain over
the earlier selection also has an interval spanning zero. Conditional identity
metrics score documented roles even when predicted involvement is low; they do
not qualify attribution, open-set behavior or rare false-positive guarantees.
The full aggregate retains origin, calibration, share-bin, retrieval and fallback
metrics for every seed and both Dev/CAL splits.

More independently sourced works helped within this fixed mixture, but sufficient
data and freedom from overfitting are not established. The loss-weight
concentration corresponds to 40.0, 43.6 and 45.8 equally weighted families, far
below the raw family counts; this is a concentration diagnostic, not an effective
independent sample size. At step 250, full-data text Train Brier is 0.01162 and
Development Brier 0.05290 before calibration. Selected checkpoints are earlier,
at steps 77–92. Longer fitting is therefore not supported by this curve.
Historical exposure, proxy human histories and the narrow fiction Development
population remain limitations. Pooled CFG counts do not test full tree topology.

All 24 new Dev/CAL Rust prediction replays passed. The twelve GPU fits report
28.83 seconds total fitting time, excluding process startup, feature extraction
and evaluation; peak Torch allocation was 240,966,656 bytes and the training
cgroup peaked at 3,286,659,072 bytes. The new 2,104-record V6 CPU export took
58m40s on one core; the 261-record book export took 8m47s, both fully supported.
Retained CFG parsing used three capped one-core workers at 29m49s–30m45s each;
book parsing took 11m10s. These are batch timings, not per-request percentiles.
Full-data text Rust numeric heads measured about 0.076ms p50 and 0.081ms p95,
excluding the unchanged text encoder. Cached feature merging does not remove
first-time parsing/encoding costs, and Cloudflare serving remains unqualified.

Final evaluation used the reserved works and generator with that frozen
sixteen-configuration, three-seed roster. No further model or threshold selection
may use that Test population.

## Data and experiment

The [public snapshot](../datasets/a-balanced-20261026-v1/README.md) contains 937
records from 54 independent source works: 215 Train queries, 288 Development,
414 Calibration and 20 independent reference excerpts. Twelve known-writer
Train references are present in the 929-record fitting/export population; eight
withheld-writer references remain separate from fitting and normalization.
Six known writers have independent works in each partition. Withheld writers
differ between Development, Calibration and reserved Test. Source works and
every descendant remain in one split.

Qwen3.6-35B-A3B and Phi-4-mini-instruct generated comparable scratch, requested
20%, requested 60%, and full-rewrite examples. Gemma-3-4B-it is Calibration-only;
Mistral-7B-Instruct-v0.3 is reserved for Test. Exact quantizations, weight hashes,
runtime/sampler identities and source rights are retained. Generation produced
829 of 832 planned non-Test outputs; three failed Phi scratch tasks remain
explicit exclusions. No detector selected generated outputs.

The reserved Mistral runtime also passed two fixed synthetic probes on the
build-host RTX 4090: original narrative generation and rewriting a synthetic
passage, both ending normally. Model/runtime hashes, template/tokenization,
sampling settings and exact exchanges are retained by the
[`generator_preflight` helper](../grammar/crates/grammar-localbench/README.md#synthetic-generator-qualification).
This qualifies those runtime operations only. No Test source was acquired or
opened, and no detector or generation-quality result follows from the probes.

Share targets count final words inside exact generated replacement spans divided
by all final words. Original/replacement/assembled bytes and bounds are retained.
Requested rewrite percentage is not the target. A complete source-conditioned
rewrite remains mixed history even at 100% replacement share. Pangram outputs
are observations, never labels. There are 96 fractional Train targets; all 215
Train queries supervise origin and share, 168 supervise human identity, and
191 supervise generator identity. Missing contributor targets are masked.

All works are historical public-domain fiction proxies, not authenticated
contemporary human-only workflows. The frozen metadata audit removed one title
matching a V6 source and found no remaining checked-title match. This does not
exclude pretraining exposure, alternate-edition overlap, unrecorded grammar
exposure, or common acquisition artifacts. Publication makes the released data
exposed research evidence. The 18 reserved Test works were acquired only after
the final selection freeze.

The canonical grammar, fallback, resolver and feature meanings remain frozen.
Every one of the 929 inputs produced a usable numeric export; all 929 were
supported by qualified frozen V6 without truncation. Text has 768 penultimate
coordinates; grammatical-leaf CFG has 1,820 normalized count/mask coordinates,
without dictionary word identities. Train queries alone fit vocabulary and
normalization. The text, combined and padded-CFG controls share 2,588 inputs,
the same learned `64 → 32` tanh encoder, and the required origin/share/conditional
attribution heads. The recovered upstream V6 encoder stays frozen; these are
newly initialized shared heads, not function-preserving V6 initialization.

All 39 declared fits completed: 12 aggregate/leaf CFG fits, 21 matched fusion
and single-task controls, and six ordered/global CFG probes. Seeds 17/29/41,
250 epochs, AdamW, and losses were fixed beforehand. Queries receive equal
family weight and equal observed-origin weight within each family. This chosen
research prevalence is not deployment prevalence. Development selects
checkpoints; Calibration alone fits temperatures and named-profile cutoffs.
Every fitted numeric head passed Rust reference replay. Controls' deliberately
untrained outputs remain diagnostics.

## Development quality

Involvement Brier below uses the declared family-and-origin balancing. Human FPR
uses cutoff 0.5 and 32 human excerpts from 16 works. Share MAE uses natural
equal-family weights and measured span targets. Ranges include every seed.

| Candidate | Involvement Brier ↓ | Human FPR ↓ | Share MAE, percentage points ↓ |
| --- | ---: | ---: | ---: |
| Frozen V6, unchanged historical calibration | 0.13809 | 12.5% | Not provided |
| Text-only joint | 0.04103–0.04626 | 6.3–9.4% | 12.66–13.41 |
| Text + CFG joint | 0.04474–0.04762 | 3.1–6.3% | 10.04–11.10 |
| Grammatical-leaf CFG joint | 0.14224–0.14618 | 37.5–43.8% | 20.93–22.50 |
| All aggregate CFG joint | 0.14769–0.15910 | 43.8–50.0% | 21.53–24.05 |
| Four-bin ordered CFG joint | 0.17365–0.17650 | 68.8–75.0% | 29.15–29.87 |
| Matched global CFG control | 0.14141–0.15102 | 43.8–56.3% | 21.26–23.67 |
| Uniform three-origin prior | 0.22222 | 100% | Untrained diagnostic |

For comparison with natural sampled-family frequencies, V6 involvement Brier is
0.22778, text 0.05858–0.06074, and combined 0.06290–0.06789. These differ from the
balanced table because mixed descendants outnumber original human excerpts.
V6's historical 95.1% accuracy and 0.03264 Brier used a different pure-origin
population and must not be substituted for its results here.

The balanced combined-minus-text Brier difference, averaged over the fixed seeds,
is +0.00313 (95% family-bootstrap interval −0.00720 to +0.01262). CFG does not
show a reliable involvement gain. Combined-minus-frozen-V6 is −0.09198
[−0.12168, −0.06141]. Intervals resample whole works, are conditional on
Development checkpoint selection, and do not establish generalization or rare
false-positive guarantees. The first-stage selection is text-only, mean Brier
0.04299, with seed 17 the predeclared representative rather than the best seed.

Combined features reduce low-share error: among 55 Development examples with
realized share in `(0,25%]`, MAE is 11.22–12.98 points versus text's 18.62–20.03.
Combined overall share MSE is 190.22–214.48 squared points and bias −4.67 to
−2.78 points. Text MSE is 270.07–309.81 and bias −1.56 to −0.26. The 50 examples
in `(50,75%]` have combined MAE 11.86–12.35; there are no `(75,100%)` examples.
Full replacements are a separate endpoint, not evidence for that missing range.
Controlled spans do not establish arbitrary editing-history inference.

Balanced three-way accuracy is 93.6–95.0% for text and 93.6–94.8% for combined;
balanced involvement ECE is 0.052–0.067 and 0.041–0.058, respectively. These are
small Development estimates, not calibrated production assurances.

## Attribution and shared-training tradeoffs

Known-human conditional top-1 remains weak: text 26.8–31.5%, combined
20.8–24.4%, across six trained writers. Known-generator top-1 is 73.8–78.9%
for text and 80.1–82.4% for combined across two trained generators. Both roles
are returned and evaluated on documented participation regardless of predicted
involvement; absent roles are not scored as asserted contributors.

Text rejects 85.7–94.6% of known human profiles to `other_human`, with
82.1–94.6% rejection of withheld writers. Combined known fallback is 82.1–84.5%,
with withheld rejection 89.3–91.1%. Known-generator fallback remains 75.4–80.9%
for text and 66.8–72.7% for combined. Development has no withheld generator;
Calibration's Gemma results fit the cutoff and are not independent rejection
validation. The untouched Test generator is still required.

Independent known-writer reference retrieval is only 17.3–19.6% top-1 for text
and 16.1–18.5% for combined. A separate frozen CFG gallery evaluation uses six
known writers plus only that partition's two withheld writers, with two reference
excerpts each. Development leaf-CFG retrieval averages 16.7% top-1 on known
writers and 17.3% on withheld writers; all-CFG averages 16.3% and 14.9%. The
reference transformations and encoding fit nothing, and all original fitting
files and non-retrieval metrics remained unchanged. The text/fusion adapter for
withheld partition references is not implemented. There is no independent
generator-reference gallery qualification.

Joint versus origin-only training changes balanced Brier by −0.00394 for text
(family interval includes zero) and −0.01699 for combined [−0.03333, −0.00335].
Combined joint human top-1 trails attribution-only by 6.15 points
[−9.92, −2.78], while generator top-1 improves 2.73 points [+0.26, +5.08].
Sharing an encoder does not guarantee that every task benefits.

## Function-preserving V6 comparison

The frozen V6 encoder supplies its exact 768-coordinate classifier input `h`.
A learned residual produces `z = h + residual(normalized_h, cfg)`; its output
weights and bias start at zero. The origin head copies V6's classifier and
historical temperature. New share and attribution heads consume the same `z`.
This preserves origin predictions at initialization, not untrained share or
attribution predictions. Both variants matched all 929 V6 examples before any
updates: maximum absolute probability error was 1.67e-7 on CPU and 1.44e-7 on CUDA,
below the frozen 1e-6 limit. The recovered release remains unchanged.

Text-only and combined use the same 64-wide residual, 768-coordinate shared
encoding, optimizer, 250 epochs and seeds 17/29/41. Text-only masks the CFG input
block. Dev chooses checkpoints, and CAL alone fits temperature/profile cutoffs.
All six fitted models supervise all required A heads and pass Rust replay on both
Dev and CAL. Numeric results and family-aware paired comparisons are in
[the warm-start evidence](a-warm-start-results.json).

| Warm-start candidate | Balanced involvement Brier ↓ | Human FPR at 0.5 ↓ | Share MAE, points ↓ |
| --- | ---: | ---: | ---: |
| V6 + text-only residual | 0.03664–0.03910 | 0–3.1% | 8.27–8.73 |
| V6 + text/CFG residual | 0.04287–0.04555 | 3.1–6.3% | 8.81–9.27 |

Adding this CFG representation does not improve the matched warm-start candidate
in any seed on involvement or overall share error. Warm text improves over the
earlier shared-head architecture, but that comparison also changes capacity and
the skip connection; it does not isolate initialization as the cause. The new
selection includes all prior candidates plus this pair, retains all three seeds,
and uses seed 17 as the predeclared representative rather than the best seed.

The combined-minus-text balanced Brier gap is +0.00663, with a 95% family-bootstrap
interval of [−0.00019, +0.01399]; it does not establish a reliable involvement
difference. Its share MAE gap is +0.55 points [+0.11, +0.99]. Warm-text minus the
earlier text architecture has Brier gap −0.00508 [−0.01482, +0.00379], also
uncertain. These intervals average the fixed seeds before resampling works;
they are conditional on Development checkpoint selection.

Warm text known-writer top-1 is 30.4–31.5%, known-generator top-1 is 76.2–78.1%,
and independent known-writer retrieval is 25.6–29.2%. Conditional identity remains
weak: known-human fallback is 89.3–95.8%, and known-generator fallback is
73.0–80.5%. These are not customer-profile or open-generator qualifications.

Only 215 Train queries from 12 works fit these heads, including 96 fractional
examples. All six uncalibrated Dev curves choose epochs 39–45 and worsen by
epoch 250. More steps at this fixed schedule have not helped; this does not test
longer training with more independent examples, a different schedule, or an
adapted upstream encoder. The frozen-encoder/count-feature pilot is not a ceiling
on CFG's potential. The next training-scale comparison should increase independent
works, generators and measured edit histories, track Train/Dev curves and retain
text-only controls. Larger datasets and eventual encoder adaptation need their
own frozen protocols; final Test must not guide those decisions.

The representative warm text adapter/heads occupy 1,811,040 bytes as loaded Rust
f64 numeric weights, including normalization; exported JSON is 6,886,569 bytes.
Head-only p50/p95 latency is 0.076/0.082ms. Its maximum replay difference is
0.0000187 percentage points. These measurements exclude the unchanged roughly
598MB ModernBERT weight dependency, tokenizer and first-time encoder execution.
The [full Rust CPU path](../inference/a-v6-cpu/QUALIFICATION.md) now passes a
fixed synthetic suite for this selected text artifact. Cloudflare execution
remains unqualified; head-only timing does not represent full inference.

## Selected residual single-task controls

The [matched adaptation controls](a-residual-controls-results.json) compare the
selected text residual's existing joint fits with three origin-only and three
attribution-only fits. Data, architecture, frozen V6 initialization, Train
weighting, seeds and 250-step budget match. Origin-only selects checkpoints by
Dev involvement Brier; attribution-only selects by Dev conditional supervised
cross-entropy and fits no origin temperature. All controls inherit V6's prior
origin supervision, so this measures adaptation tradeoffs rather than training
independent representations from scratch. Unused heads are frozen and marked
untrained; the controls are not complete A candidates or selection inputs.

Origin-only balanced Dev Brier is 0.03817–0.03984, versus joint's
0.03664–0.03910. Joint-minus-origin-only averages −0.00112, with 95% family
interval [−0.00378, +0.00149]: a detection gain is uncertain. Joint human top-1
improves by 2.38 percentage points [−3.17, +7.34] over attribution-only, also
uncertain. Known-generator top-1 improves by 5.34 points [+2.34, +8.59], comparing
joint's 76.2–78.1% against attribution-only's 70.7–72.7%. Sharing supervision
helps generator classification on these Development families; it does not
establish a general mutual benefit, stronger writer identification, or parity.

All six new fits passed the unchanged 929-input initial-origin gate and twelve
Dev/CAL Rust replays. Twenty-six synthetic Python tests and six focused Rust
loader tests passed, including inactive-head behavior, protocol binding,
cross-language replay and bit-identical default-joint synthetic updates against
the frozen earlier trainer. Six summary tests also passed. Artifacts and source
snapshots remain in ignored `train-scale/controls-selected-v1/`. No new detector
calls or final-Test reads occurred; selection remains the earlier joint text
residual. New independent data remains necessary.

## Train-expansion comparison

The [cumulative augmentation snapshot](../datasets/a-train-augmentation-20261026-v1/README.md)
adds 1,049 admitted examples from 96 historical V6 Train roots: 96 human proxies
and 953 generated descendants. All 960 generation tasks are accounted for;
seven duplicate or unchanged outputs were excluded before fitting. The original
215 Train queries remain, giving 1,264 queries from 108 source families, including
669 fractional-share targets. Individual authors of the added sources are
unestablished, so their human-attribution targets remain masked. The new data do
not expand the six known writer profiles. Frozen Development (288), Calibration
(414) and independent reference (12) inputs remained unchanged; Test was
unopened during this comparison. Original V6 exposure remains explicit.

All 1,978 inputs passed the initial V6 probability check (CPU maximum error
2.07e-7, tolerance 1e-6); every fit also passed on its CUDA training device.
Vocabulary, normalization, architecture, initialization, three seeds and 250
full-batch optimizer steps match the earlier residual experiment. Source-family
and within-family origin balancing is unchanged. This adds different domains
and rewrite histories as well as more examples, so it cannot isolate dataset
size as the cause. The [aggregate evidence](a-train-scale-results.json) records
all six fits, paired family intervals, required output metrics and artifact hashes.

| Training population and input | Balanced involvement Brier ↓ | Human FPR at 0.5 ↓ | Share MAE, points ↓ |
| --- | ---: | ---: | ---: |
| Original Train, text residual | 0.03664–0.03910 | 0–3.1% | 8.27–8.73 |
| Expanded Train, text residual | 0.07354–0.08332 | 12.5–18.8% | 10.40–11.30 |
| Expanded Train, text/CFG residual | 0.07044–0.07211 | 12.5% | 10.44–11.66 |

The expanded-minus-original text Brier gap is +0.03962 (95% family interval
+0.01642 to +0.06177); for combined it is +0.02648 [+0.01167, +0.04369]. Within
the expanded population, combined-minus-text is −0.00651 [−0.01503, +0.00275],
an uncertain improvement, and share MAE changes by +0.31 points [−0.34, +0.99].
All intervals average the fixed seeds before resampling the 16 Development
families and remain conditional on checkpoint selection. Known-human top-1 is
23.8–28.6% across the six fits; known-generator top-1 is 74.2–77.7%. Frequent
fallbacks remain: 73.8–98.2% for known humans and 80.1–87.5% for known generators.
No attribution qualification follows from these results.

Selected epochs are 84–113. Training loss continues to fall while Development
Brier worsens later; extending this fixed training schedule is not the next
supported intervention. The fixed source-mixture comparison below tests preserving
the original cohort's contribution while retaining all added examples.
Sentence/tree CFG representations remain a separate question: the residual here
sees only pooled grammatical-leaf counts, not tree topology or sentence order.

All twelve Dev/CAL Rust replay checks passed. For the representative expanded
text fit, numeric adapter/head weights occupy 1,811,040 bytes as Rust f64 values;
CPU p50/p95 is 0.075/0.081ms, excluding parsing and the unchanged ModernBERT
encoder. Training uses the 20% CUDA allocation cap and a 10GiB, zero-swap cgroup;
the job peaked at about 2.97GiB host memory. Numeric weights, checkpoints,
predictions, learning curves and recovery files are retained under ignored
`data/grammar-private/a-parity-stage2-20261026/train-scale/`. Reproduction uses
`numeric-and-fit-v1.sh` and `evaluate-v1.sh` in that root with new output paths.
All 702 Pangram observations were reused with the frozen Calibration-only mapping;
there were no additional paid calls. Pangram's balanced involvement Brier remains
0.01896; its percentage is not used as a share target. The earlier text residual
remains selected and parity remains unproven.

## Fixed source-mixture comparison

Uniform family weighting made the original 12 works only 12/108 (11.1%) of Train
loss mass after adding 96 works. One predeclared 50/50 original/added cohort
weighting was tested, with equal family and observed-origin weights inside each
cohort. All 1,264 examples, numeric inputs, supervision, initialization, seeds and
250 optimizer steps remain identical to the expanded comparison. Dev/CAL weights
are unchanged. Missing-target masks renormalize each task separately; all named
human supervision still comes from original Train. No ratio sweep was performed.

| Expanded Train weighting and input | Balanced involvement Brier ↓ | Human FPR at 0.5 ↓ | Share MAE, points ↓ |
| --- | ---: | ---: | ---: |
| Uniform family, text | 0.07354–0.08332 | 12.5–18.8% | 10.40–11.30 |
| 50/50 cohorts, text | 0.04517–0.04830 | 6.3–9.4% | 8.43–8.96 |
| 50/50 cohorts, text/CFG | 0.05322–0.05541 | 6.3% | 9.09–9.60 |

The [matched weighting evidence](a-cohort-mixture-results.json) gives a mean text
Brier reduction of 0.03119 (95% family interval −0.05290 to −0.01343), and combined
reduction of 0.01655 [−0.02908, −0.00585]. The [original-Train comparison](a-cohort-mixture-original-results.json)
still favors the earlier text residual by 0.00843 on average, with an uncertain
interval [−0.00208, +0.01905] for mixture-minus-original. It remains the selected
candidate. Within the mixture, combined-minus-text is +0.00813 [−0.00091, +0.01727];
this does not establish a CFG gain. These are fixed-seed, Development-selected
family intervals, not independent Test or deployment qualification.

All six fits and twelve Dev/CAL Rust replays passed; all 1,978 initial predictions
again satisfy the unchanged V6 equivalence gate. Known-writer top-1 ranges
26.2–32.7% and known-generator top-1 77.7–80.5%. Selected epochs are 58–87, with
later overfitting still visible. The existing 702 paid observations were reused;
there were no extra Pangram calls. Exact protocols, weights, checkpoints and
per-row predictions are retained under ignored `train-scale/mixture-v1/` inside
the current task root. `run.sh` reproduces the fits; `final-evidence/` retains
the final aggregate reports, source snapshots and validation receipts. Eighteen
synthetic Python tests, six Rust summary tests, and rejection checks for unmarked
mixture fits and unmatched populations passed. Original observations are intact.

This isolates source weighting as one contributor to the regression. It does
not establish a sufficient dataset size: 108 works and repeated rewrites remain
limited, particularly for six named writers and 16 Development families. Further
growth should add independent works and established writers across balanced
genres, preserve whole-work splits, and retain learning curves. No number of
variants from the same source establishes independent generalization evidence.

## Pangram interpretation

All 702 exact Development/Calibration texts have permanently retained successful
Pangram observations: selector `pangram-4`, returned version `4.1`. Reuse audits
found no matching earlier text; exactly 702 distinct POSTs were made. Inputs,
intents, raw exchanges, task identities, timing and digests remain in ignored
storage. No quota-stop or uncertain request
occurred. No paid rerun is needed for these comparisons.

Pangram does not separately expose our AI-share and any-AI-confidence meanings.
Its raw nonhuman content fraction has **5.23-point MAE** against our measured
replacement-share convention, and zero observed human flags at 0.5. Our shared
models have not matched that share result. The conventions are not identical,
and zero flags among 32 excerpts is not a rare-FPR guarantee.

The initial Calibration-only temperature proxy has balanced involvement Brier
0.09119, versus natural-family 0.17705. This is **our transformed comparator**,
not native Pangram confidence. Temperature alone cannot shift the 0.5 boundary
to reflect that a small positive content share can mean confident involvement.
A separately identified CAL-only weighted isotonic mapping gives balanced Brier
**0.01896**, or 0.03171 with natural family weights. It beats all six text/combined
joint runs: balanced model-minus-Pangram gaps are +0.02207 to +0.02730 for text
and +0.02578 to +0.02866 for combined; every individual paired family interval
is above zero. It still has zero observed human flags at 0.5. The raw responses,
share comparisons and model predictions are unchanged; no new paid calls occurred.

The isotonic fit combines tied CAL fractions, then uses weighted
pool-adjacent-violators with linear interpolation and clamped endpoints. Its
weights match the declared family/origin balancing. The option was added after
the original Development comparison was inspected, so this is an exposed
diagnostic, not an independently predeclared result. Intervals hold the fitted
CAL map fixed and exclude calibration-estimation uncertainty. This diagnostic
did not qualify Test inference. Nine synthetic tests cover weights, ties,
monotonicity, endpoint behavior and CAL/Development separation; all six legacy
default-mode replays are byte-identical. Neither proxy is a training label or
native Pangram confidence. Production histories and exact spans remain the ground
truth, and Pangram remains ahead on this comparison.

## Coverage, runtime and reproduction

The parser accounted for all 1,383,903 input bytes. Of 286,601 tokens, 43 remain
unresolved; 86 of 15,045 windows reached a resource limit. There were no execution
or projection failures and no excluded documents. Native-complete windows number
6,151, lexical-fallback-complete 3,229, and partial 5,579. These tiers remain
explicit; usable input does not mean fully grammatical coverage.

First-time CFG extraction took 34m51s, including 5.29s parser loading, with
document p50/p95 1.92/4.79s and 441,212 KiB peak process RSS. V6 extraction took
28m41s with 1,142,536 KiB peak process RSS. Saved embeddings do not remove this
first-time encoder cost. Jobs ran on build-host with explicit CPU/RAM caps and no
swap. CFG's 6 GiB cgroup peak includes retained output page cache; process RSS
is reported separately.

The initial MLP text joint seed17's numeric head fits in about 1.30s after setup, peaks at
93,028,864 bytes of allocated CUDA memory, and has a 676,827-byte checkpoint.
Exported JSON weights are 5,120,458 bytes; loaded Rust numeric weights are
1,345,376 bytes. Head-only Rust p50/p95 is 0.062/0.065ms, with 42,528 KiB peak
process RSS for the replay. Maximum reference difference is 0.0000151 percentage
points. Full V6 adds its unchanged 598 MB weight file and tokenizer/encoder work.
The full text/fusion pipeline is not qualified for Rust/Wasm Workers or a
compute-only Container. No production Python/GPU dependency or deployment is
authorized by these research measurements.

The [aggregate evidence](a-balanced-baseline-results.json) contains all seeds,
controls, calibration/share metrics, paired intervals and hashes. Exact public
source and generation provenance is in the dataset snapshot. Private fitted
weights and recovery artifacts stay under the recorded `a-parity-stage2-20261026`
task root's `new-cohort/`, including immutable extraction receipts, copied
trainer source, checkpoints, numeric contracts and Rust reference predictions.

```sh
# Run within the independent grammar workspace, using a checkout-owned target.
cargo run --release -p grammar-a-baseline --example assemble_balanced_dataset -- NEW_DATASET SOURCE_MANIFEST QUERY_MANIFEST HUMANS.jsonl CONFIG_DIR QWEN_OUTPUT PHI_OUTPUT GEMMA_OUTPUT
cargo run --release -p grammar-a-baseline --example prepare_v6_fusion -- join CFG_NUMERIC REQUESTS EMBEDDINGS NEW_NUMERIC
python crates/grammar-eval/python/shared_a.py train --data NUMERIC --out NEW_FIT --device cuda --seed 17 --epochs 250 --control joint --export-encodings --weighting family_origin_balanced
cargo run --release -p grammar-a-baseline --example evaluate_supervised -- NUMERIC FIT development NEW_REPORT
cargo run --release -p grammar-a-baseline --example prepare_ordered_cfg -- CFG_LEAF_NUMERIC EXPORT NEW_ORDERED_BUNDLE
cargo run --release -p grammar-a-baseline --example freeze_balanced_selection -- COHORT NEW_SELECTION
cargo run --release -p grammar-a-baseline --example freeze_balanced_selection -- COHORT NEW_SELECTION_WITH_WARM --include-warm-start
cargo run --release -p grammar-a-baseline --example publish_balanced_evidence -- COHORT NEW_PUBLIC_REPORT
```

Use the qualified pinned Python environments and the retained durable scripts
for actual replay; outputs are new-only. The warm-start comparison and amended
selection are frozen; the final-Test report records subsequent evaluation. Warm-start
training/replay commands are in the [crate README](../grammar/crates/grammar-a-baseline/README.md#frozen-v6-residual-warm-start).
The evidence supports text-based shared A development and a larger-data
CFG-fusion comparison. It does not support launching an aggregate CFG-only
candidate. Ordered token/tree models remain
unproven: the failed probe only bins whole parser-window counts into four
positions. Sentence and paragraph scoring remain later qualifications.

# Mixed-int8 CPU serving of A

Qualified on build-host on October 28, 2026. This note records whether the mixed
int8 ONNX build of *A* can serve in place of the f32 release model. If it
qualified, the operator's preferred serving path for *A* was a Cloudflare
Container on CPU running the int8 build; a RunPod GPU backend behind a Worker is
only a fallback. The condition was **no quality reduction** against the f32
release model ([the baseline release](a-baseline-release.md), arm Faux, seed
17), operationalised as *no measurable reduction* under gates fixed in advance.

**Verdict: not viable (quality reduction).** The primary recipe and the single
pre-declared fallback both failed pre-registered gates. Serving stays f32 until
some recipe passes a new pre-registration. Latency was not the problem: every
build is well inside its limit.

Nothing was deployed. No Cloudflare resource was created, no hosted service was
called and no GPU was used. Dev-select was not scored, and no Test prose was
read. CAL was scored only to fit the int8 builds' calibrators, under protocol
Amendment 3. Weights, predictions, scripts, the pre-registration and per-run
receipts stay in ignored storage. The recipe and the earlier screen evidence are
in [CPU inference](a-cpu-inference.md).

## Summary

| Question | Answer |
| --- | --- |
| Verdict | **NOT_VIABLE (quality reduction).** Primary recipe: 20 failing gates. Fallback FB1: 5 failing gates. |
| Primary recipe | Fails raw parity on Dev-iter and on long documents, calibrated share parity, raw and calibrated Brier, TPR, and one domain/generator cell. Its raw Brier interval lies entirely above zero, so its quality loss is measured. |
| Fallback FB1 (4 layers kept f32) | Passes every raw quality gate (raw Brier −0.0002). Fails attribution top-1 agreement (0.9715 on Dev-iter, 0.942 on long documents, limit 0.98), calibrated share parity (max \|Δ\| 0.155) and the calibrated Brier upper bound (+0.00148, limit +0.001). It failed parity and non-inferiority gates; no quality metric shows it significantly worse. |
| Latency, 2,048 tokens, 4 vCPU (p95) | FB1 int8 0.95 s, primary int8 0.86 s, f32 banded 1.30 s. Limit 4.0 s. |
| Busy cost per 1,000 documents | f32 banded on standard-4: about $0.14 at 2,048 tokens and $0.016 at 300 tokens, one request at a time. int8 FB1 at saturation: $0.010 for Dev-iter-length documents and $0.077 for 1,024-2,048-token documents. |
| Selected serving path | f32 in a Cloudflare Container on CPU (about a 4 vCPU instance), called from the coordinator Worker. f32 *A* does not fit a Worker isolate. A measured Container deployment is still required and needs operator approval. |
| Fallback | A RunPod GPU backend behind a Worker, for paid traffic only, after its own qualification ([A on a RunPod GPU backend](a-runpod-backend.md)). |

## What was qualified

**Reference.** The release candidate's own f32 model and its CAL calibration and
operating points. Dev-iter raw values come from the release's recorded
predictions. Long-document values come from the f32 banded ONNX graph through
the same Rust runner as int8, then the release calibration.

**Primary recipe** (the [CPU inference](a-cpu-inference.md) candidate, applied
unchanged):
- ONNX opset 17 with banded local attention (each 64-query block against its
  192-key window), inputs padded to a multiple of 64 with masked positions;
- exact SmoothQuant-style rescaling with α = 0.8, folded into the norm weights,
  the V rows of Wqkv and the gate rows of MLP Wi;
- dynamic per-channel int8 for Wqkv, attention Wo and MLP Wi; MLP Wo,
  embeddings, norms and heads stay f32.

The rescaling factors were recomputed for the release weights from 206 Train
rows (two per domain × origin stratum, seed 5); no labels were used. The int8
graph is 314 MiB with 66 int8 weight products.

**Fallback FB1** (pre-declared, at most one attempt): the primary recipe plus
every weight product of the four encoder layers most sensitive to quantisation
kept in f32. Sensitivity was the mean \|ΔP\| from quantising one layer at a time,
measured on the 206 rescaling rows only. Layers 9, 0, 12 and 1 were kept. The
graph is 361 MiB with 54 int8 weight products.

**Runner.** One Rust binary produced every ONNX score and every latency number:
the `ort` crate 2.0.0-rc.10 loading ONNX Runtime 1.31.1 and `tokenizers` 0.22.2
with the release tokenizer. Graph optimisation level 3, batch 1, one inter-op
thread; it emits every raw head.

**Data.**
- Dev-iter: 2,873 documents (570 human). The evaluator's primary population is
  1,664 documents.
- CAL: 5,379 documents, used only for the int8 calibrators.
- LDP-v1: a fixed set of 520 unlabelled long documents (1,032-2,048 tokens,
  mean 1,636), assembled from Train-side text that is absent from every split
  of the release bundle. It has five strata: same-source human, cross-source
  human, model-only, attacked model-only, and human-then-model splices.
  Dev-iter documents are at most 663 tokens, so LDP-v1 is the only
  long-context test.

## Pre-registered gates

The gates, limits, decision rule and fallback were written before any ONNX or
int8 output of the release model existed. No limit was changed afterwards. All
differences are int8 minus f32. P(involvement) is p_model_only + p_mixed.

| Group | Gate | Limit |
| --- | --- | --- |
| Construction | V1 export tensors equal the release files bitwise | exact |
| | V2 f32 banded ONNX against the recorded Dev-iter raw outputs | \|ΔP\| ≤ 1e-4, origin logit ≤ 1e-3, share ≤ 1e-4 |
| | V3 rescaled f32 against unrescaled f32 | \|ΔP\| ≤ 1e-5 |
| | V4 runner token ids equal the release tokenizer's | 100% |
| | V5 calibration code reproduces the release's CAL apply and refit | ≤ 1e-12, identical fit |
| | V6 bootstrap port reproduces the evaluator's interval | ≤ 1e-12 |
| | V7 int8 execution failures on Dev-iter, CAL and LDP | none |
| Raw parity (Dev-iter and LDP) | P1 / P2 / P3 max / p99 / mean \|ΔP\| | ≤ 0.10 / 0.045 / 0.0075 |
| | P4 decision flips at 0.5 | ≤ 14 on Dev-iter, ≤ 5 on LDP |
| | P5 flips at each build's raw 1% operating point | ≤ 20 on Dev-iter, ≤ 5 on LDP |
| | P6 share \|Δ\|, max / p99 / mean | ≤ 0.10 / 0.045 / 0.0075 |
| | P7 attribution top-1 agreement, all / f32 P ≥ 0.5 | ≥ 0.98 / ≥ 0.97 |
| Calibrated parity (each build's own CAL map) | PC1 / PC2 / PC3 max / p99 / mean \|ΔP\| | ≤ 0.28 / 0.19 / 0.019 |
| | PC4 / PC5 flips at 0.5 / at each build's CAL 1% point | ≤ 3.5% of documents |
| | PC6 share \|Δ\|, max / p99 / mean | ≤ 0.10 / 0.045 / 0.0075 |
| Quality, Dev-iter, raw **and** calibrated | Q1 balanced involvement Brier, whole split | point ≤ +0.0005 and paired source-group bootstrap upper bound ≤ +0.001 |
| | Q2 the same on the evaluator primary population | same |
| | Q3 / Q4 humans flagged at 0.5 / at the 1% point | ≤ f32 + 1 |
| | Q5 / Q6 TPR at 0.5 / at the 1% point, primary positives | ≥ f32 − 0.005 |
| | Q7 AUROC, primary and whole split | ≥ f32 − 0.002 |
| | Q8 share MAE, primary | ≤ f32 + 0.5 points |
| | Q9 attribution top-1 accuracy, primary positives | ≥ f32 − 0.005 |
| | Q10 balanced Brier per domain or generator cell with ≥ 50 rows | ≤ +0.005 |
| Latency | L1 single-request p95 at 2,048 tokens, 4 vCPU shape | ≤ 4.0 s |

The bootstrap is the protocol's: 1,000 paired source-group draws with a fixed
xorshift seed, using the 25th and 975th ordered draws as bounds.

The raw parity limits are about 1.5× the screen model's values. The calibrated
parity limits PC1-PC5 are about 1.5× a Dev-iter two-half cross-fit, because a
clipped isotonic map turns a small raw shift into a full step jump. PC6 reuses
the raw share limits (see [Review](#independent-review)).

**Decision rule.**
- **VIABLE:** every V, P, PC, Q and L1 gate passes.
- **NOT_VIABLE (quality reduction):** any P, PC or Q gate fails.
- **NOT_VIABLE (latency):** only L1 fails; no fallback is tried.
- **NOT_VIABLE (construction):** a V gate fails; only a mechanical fix and an
  identical rerun are allowed.
- **Fallback:** FB1 runs once, after a P, PC or Q failure. If it passes, the
  verdict is VIABLE_FALLBACK; if it fails, NOT_VIABLE is final.
- **Corrections:** a correction made after results can withdraw a VIABLE but
  cannot create one.

## Results

Run 2 decides (see [Two runs](#two-runs)). Every construction gate passed for
both builds:
- V2: the f32 banded graph matched the recorded raw outputs to 6.5e-6 in
  P(involvement), 1.2e-4 in origin logits and 1.6e-5 in share.
- V3: rescaling changed P(involvement) by at most 9.4e-6.
- V4: every token id matched.
- V5: the calibration apply matched to 8.9e-16, and the refit was identical.
- V7: there were no execution failures.

Bold marks a failure.

**Dev-iter parity**

| Gate | Primary | FB1 |
| --- | --- | --- |
| P1 / P2 / P3 \|ΔP\|, raw | **0.114** / **0.065** / 0.0052 | 0.073 / 0.043 / 0.0035 |
| P4 / P5 flips, raw | **16** / 12 | 10 / 12 |
| P6 share \|Δ\|, raw (max / p99 / mean) | **0.116** / **0.051** / 0.0053 | 0.063 / 0.029 / 0.0036 |
| P7 attribution agreement (all / f32-positive) | **0.962** / 0.972 | **0.9715** / 0.979 |
| PC1 / PC2 / PC3 \|ΔP\|, calibrated | 0.212 / 0.096 / 0.0090 | 0.161 / 0.069 / 0.0068 |
| PC4 / PC5 flips, calibrated | 16 / 16 | 7 / 13 |
| PC6 share \|Δ\|, calibrated | **0.152** / **0.068** / 0.0066 | **0.155** / 0.038 / 0.0047 |

**Long-document parity (LDP-v1, 520 documents)**

| Gate | Primary | FB1 |
| --- | --- | --- |
| P1 / P2 / P3 \|ΔP\|, raw | **0.162** / **0.063** / **0.0086** | 0.056 / 0.036 / 0.0049 |
| P4 / P5 flips, raw | 5 / **6** | 3 / 3 |
| P6 share \|Δ\|, raw | **0.174** / **0.067** / 0.0067 | 0.051 / 0.038 / 0.0040 |
| P7 attribution agreement (all / f32-positive) | **0.938** / **0.954** | **0.942** / **0.946** |
| PC1 / PC2 / PC3 \|ΔP\|, calibrated | 0.212 / 0.124 / 0.0141 | 0.165 / 0.105 / 0.0113 |
| PC4 / PC5 flips, calibrated | 4 / 4 | 3 / 3 |
| PC6 share \|Δ\|, calibrated | **0.217** / **0.097** / **0.0091** | **0.114** / **0.065** / 0.0066 |

**Dev-iter quality** (95% intervals in brackets)

| Gate | Primary, raw | FB1, raw | Primary, calibrated | FB1, calibrated |
| --- | --- | --- | --- | --- |
| Q1 Brier Δ, whole split | **+0.0007 [+0.0000, +0.0013]** | −0.0002 [−0.0005, +0.0002] | **+0.0005 [−0.0005, +0.0016]** | +0.00015 [−0.00068, +0.00098] |
| Q2 Brier Δ, primary | **+0.0008 [+0.0001, +0.0016]** | −0.0002 [−0.0006, +0.0003] | **+0.0008 [−0.0006, +0.0022]** | **+0.00045 [−0.00051, +0.00148]** |
| Q3 humans flagged at 0.5 | 38 → 38 | 38 → 39 | 36 → 37 | 36 → 37 |
| Q4 humans flagged at the 1% point | 5 → 5 | 5 → 5 | 11 → 11 | 11 → 11 |
| Q5 TPR Δ at 0.5 | **−0.0064** | 0.0000 | +0.0037 | +0.0018 |
| Q6 TPR Δ at the 1% point | +0.0055 | +0.0009 | **−0.0091** | −0.0037 |
| Q7 AUROC Δ, primary | −0.0001 | +0.0004 | −0.0017 | +0.0003 |
| Q8 share MAE Δ, points | +0.13 | +0.03 | +0.11 | +0.01 |
| Q9 attribution accuracy Δ | +0.0018 | +0.0037 | +0.0018 | +0.0037 |
| Q10 worst cell Brier Δ | +0.0028 | +0.0006 | **+0.0073** | +0.0019 |

The primary recipe's Q1 calibrated point is +0.00054, just over the +0.0005
limit. Its whole-split raw interval starts at +0.00003.

**What the failures mean.**
- **Primary recipe.** It reduces quality measurably on the release model. Its
  raw Brier intervals lie entirely above zero, it loses TPR, and its parity
  errors are larger on long documents than on Dev-iter.
- **FB1.** It shows no significant quality reduction: raw Brier −0.0002, both
  raw intervals include zero, and attribution accuracy rises by 0.004. It still
  fails three pre-registered tests of sameness and non-inferiority:
  - its top attribution differs from f32 on 2.9% of Dev-iter documents and
    5.8% of long documents;
  - single calibrated share values move by up to 0.155;
  - the calibrated Brier upper bound on the primary population, +0.00148, is
    above +0.001.

  Under the decision rule any such failure is a quality reduction, so FB1
  is not served.

### Two runs

At 20:10 the release runner corrected the release candidate's CAL operating
points. Thresholds now read the emitted calibrated involvement (1 − p_human)
instead of re-adding p_model_only + p_mixed, and a new evaluator build
(`a_comp_evaluate-5915cd86`) applies the same rule. Model files, calibration and
selection did not change. As the pre-registration requires, the whole
qualification was rerun on the corrected artifact, and the int8 builds used the
same corrected rule.

- **Run 2 decides.** Run 1 is reported alongside it.
- **What stayed the same.** Every build and every raw score was bitwise
  identical between the runs, and so were the verdicts and the failing-gate
  lists.
- **What moved.** Only gates that depend on operating points changed (P5, PC5,
  Q4, Q6). The f32 CAL 1% point moved from 0.8674 to 0.9091, and the primary
  int8 point from 0.8436 to 0.9063.

Run 1 did not measure latency, because its graphs are identical to run 2's.

## Calibration amendment

CAL is otherwise reserved for the release calibrator. Protocol v1.2
**Amendment 3** ([protocol](a-competitive-protocol.md), commit `06d186f8`) was
committed and pushed before any int8 CAL scoring. It permits exactly one further
use of CAL: fitting the protocol calibrator (`cal-policy-v3`, including its
temperature-only fallback for a recorded mechanical failure) and the 1% and 0.1%
CAL operating points of the mixed-int8 build of the released seed-17 weights,
and once more for the single pre-declared fallback build. No comparison,
selection, tuning or gate uses CAL values. The release's f32 calibration and
operating points are unchanged. Dev-select is not used, and Test is not touched.

Under it, every CAL document was scored by each int8 build, and that build's
calibrator was refitted. CAL reliability is reported only:

| Build | Temperature | Isotonic levels | ECE10 | ECE10, humans | CAL 1% point | CAL 0.1% point |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| f32 release | 1.1275 | 32 | 0.0083 | 0.1831 | 0.90909 | 0.99988 |
| Primary int8 | 1.1275 | 25 | 0.0094 | 0.1846 | 0.90625 | 0.99988 |
| FB1 int8 | 1.1163 | 28 | 0.0102 | 0.1829 | 0.89983 | 0.98687 |

Both int8 fits used the isotonic map, with no fallback. Any future int8 build
needs its own CAL fit under a new amendment that names that purpose.

## Latency and cost

Shapes are cgroups on the test host, an Intel i9-13900KF (AVX2 and AVX-VNNI, no
AVX-512):
- **standard-4:** 400% CPU and 12 GiB memory on four hardware threads of two
  P-cores. One vCPU is one hardware thread, which is conservative. This shape
  decides L1. The four-distinct-core variant measured the same within 2%.
- **2 vCPU:** 200% CPU and 8 GiB memory on both hardware threads of one
  P-core.

Timing covers text in to heads out: tokenisation, padding, the ONNX Runtime run
and post-processing. Every latency run met the pre-registered quiet condition
(pinned CPUs at most 15% busy over the previous 10 s).

**Single-request latency** (seconds, p50 / p95; 40 requests per length):

| Build and shape | 300 tokens | 512 | 1,024 | 2,048 |
| --- | ---: | ---: | ---: | ---: |
| f32 banded, standard-4 | 0.147 / 0.151 | 0.238 / 0.245 | 0.530 / 0.547 | 1.272 / 1.299 |
| FB1 int8, standard-4 | 0.094 / 0.099 | 0.155 / 0.160 | 0.361 / 0.378 | 0.913 / 0.954 |
| Primary int8, standard-4 | 0.082 / 0.084 | 0.138 / 0.144 | 0.327 / 0.342 | 0.846 / 0.859 |
| FB1 int8, 2 vCPU | 0.166 / 0.169 | 0.278 / 0.283 | 0.638 / 0.647 | 1.595 / 1.618 |

f32 at 2 vCPU was not measured.

**Saturated throughput, FB1 int8.** All 2,873 Dev-iter documents (mean 308
tokens) or all 520 LDP-v1 documents (mean 1,636 tokens), with one shared
in-order queue. There were no failures.

| Shape and mode | Dev-iter docs/s | LDP docs/s | LDP per-document p95 | LDP cgroup peak |
| --- | ---: | ---: | ---: | ---: |
| standard-4, 4 × 1-thread sessions | 10.87 | 1.46 | 3.60 s | 7,009 MiB |
| standard-4, one 4-thread session | 9.95 | 1.43 | 0.91 s | 1,779 MiB |
| 2 vCPU, 2 × 1-thread sessions | 5.79 | 0.84 | 3.08 s | 3,399 MiB |
| 2 vCPU, one 2-thread session | 5.56 | 0.82 | 1.58 s | 1,974 MiB |

Saturated throughput was not measured for the f32 graph, or for the rest of the
primary build's shapes. The primary build's remaining runs were stopped when
other jobs pushed the shared memory slice over its limit. They were reported
only.

**Cold start, FB1.** A fresh process was ready in a median 0.80 s (maximum 2.04 s)
on standard-4, and returned its first 512-token result in 0.96 s (maximum
2.20 s). On 2 vCPU the figures were 0.77 s and 1.06 s. All runs used a warm page
cache, so image pull and model download are not included.

**Cost per 1,000 documents.** Busy time only, at list prices read on 2026-10-28
from the Containers pricing page (updated 2026-10-05; see [CPU
inference](a-cpu-inference.md#cost-per-1000-documents)): $0.000020 per active
vCPU-second, $0.0000025 per provisioned GiB-second and $0.00000007 per
provisioned GB-second of disk.
- **standard-4** (4 vCPU, 12 GiB, 20 GB): $0.0001114 per busy second.
- **2 vCPU instance:** $0.0000611 per busy second, assuming 8 GiB and 16 GB of
  disk. That corresponds to Cloudflare's `standard-3` type, whose
  specification was not part of the recorded price read; confirm it before
  relying on this row. Its CPU part alone is $0.000040 per second.

| Build, shape and mode | Dev-iter-length documents | 1,024-2,048-token documents (LDP-v1) | 2,048 tokens, one request at a time |
| --- | ---: | ---: | ---: |
| f32 banded, standard-4 | about $0.016 at 300 tokens, one at a time | – | $0.142 |
| FB1 int8, standard-4, 4 × 1-thread | $0.010 | $0.077 | – |
| FB1 int8, standard-4, one 4-thread | $0.011 | $0.078 | $0.102 |
| FB1 int8, 2 vCPU, 2 × 1-thread | $0.011 | $0.073 | – |
| FB1 int8, 2 vCPU, one 2-thread | $0.011 | $0.074 | $0.098 |

- **f32 at saturation** is not measured on this runner. The earlier
  one-thread banded f32 timing gives about $0.139 per 1,000 documents of 2,048
  tokens on standard-4 with four single-thread sessions
  ([RunPod comparison](a-runpod-backend.md)). The measured f32 / int8
  single-request ratio (1.4× at 2,048 tokens, 1.6× at 300) suggests about
  $0.016 for Dev-iter-length documents and $0.11 for LDP-length documents.
  These figures are projections.
- **Idle cost.** An awake but idle standard-4 costs $0.0000314/s (about $0.11
  per hour). The assumed 2 vCPU shape costs $0.0000211/s (about $0.08 per hour).
- **Not included:** the Workers Paid base fee, cold starts, storage, egress
  and the Worker in front of the Container.
- **Memory.** Four single-thread sessions on long documents peak at 6.8 GiB
  (7,009 MiB) on standard-4 (12 GiB). On the 2 vCPU shape, two single-thread
  sessions peak at 3.3 GiB, which fits the assumed 8 GiB.

At these rates the f32 build's busy cost is a small fraction of any per-document
price. The int8 build would mainly have bought latency headroom: 0.95 s against
1.30 s p95 at 2,048 tokens.

## Independent review

A separate reviewer recomputed every run-2 P, PC and Q value from the scored
outputs, using its own calibration apply, balanced weights, xorshift bootstrap,
thresholds and AUROC code. Its values matched the recorded gates to within
1e-8, with identical pass/fail. It also reproduced the CAL 1% points (0.90625
primary, 0.89983 FB1 and 0.90909 f32). The verdict was not refuted.

**Other checks that passed.**
- **LDP-v1 exclusions.** None of the 2,534 constituent rows, row text hashes or
  source groups appears in any split of the release bundle. None of the 14,576
  blank-line pieces matches a bundle text hash, exactly or after whitespace
  normalisation.
- **Calibrator inputs.** The int8 CAL inputs are exactly the 5,379 CAL
  documents, with no Dev-iter or Dev-select overlap. The isotonic knots match
  recomputed CAL scores to 4.7e-8, and the raw manifest hash is bound into the
  calibration.
- **Package.** The packaged FB1 build's checksums verify, and its status is
  `not_qualified_do_not_serve`. Its replay reference reproduces bitwise, and
  its tolerance check against the f32 reference passes on all three synthetic
  requests.
- **Unchanged inputs.** The release candidate's recorded files are unchanged.
  The threshold file matches its recorded hash. The only code change between
  the runs is the operating-point rule.
- **FB1 layers.** The kept layers match the Train-only sensitivity ranking.
- **Latency.** A quiet rerun gave FB1 0.945 s (recorded 0.954 s) and f32
  banded 1.300 s (recorded 1.299 s).

**Findings and how they are handled.**

| Finding | Status |
| --- | --- |
| An earlier summary of this result said the int8 build "measurably reduces quality". That holds for the primary recipe only. FB1 failed parity and non-inferiority gates without a significant measured loss. | Applied. This note and [CPU inference](a-cpu-inference.md) describe FB1 that way. The NOT_VIABLE label still follows the decision rule. |
| FB1's attribution disagreements are mostly near-ties. Of 82 Dev-iter disagreements, 43 have an f32 top-2 gap below 0.01 (median gap 0.0097, median f32 top-1 probability 0.24, across 33 profiles). 33 are on human documents. On LDP, 19 of 30 have a gap below 0.01. | Recorded. P7 was pre-registered and is not relaxed. A future pre-registration should consider a margin-aware attribution parity gate. |
| PC6's calibrated share limits copy the raw-scale screen evidence, whereas PC1-PC5 were scaled from a calibrated cross-fit to allow for isotonic step jumps. Calibrated share inherits those steps, so PC6 may be too tight. | **Unresolved.** It does not change the verdict, because FB1 also fails P7 and calibrated Q2. A future pre-registration should scale PC6 from calibrated evidence. |
| The pre-registration's timing is established only by filesystem times and its recorded hash, not by a commit made before results. Its statement that no release-model result existed is slightly inaccurate: the Faux seed-17 PyTorch Dev-iter predictions existed about an hour earlier. | **Unresolved, disclosed.** The times are consistent: the pre-registration was last changed at 15:16. The release selection was written at 19:25, and the first int8 CAL scoring ran at 20:14, after Amendment 3 at 16:42. No ONNX or int8 output of any release finalist existed when it was written, and its limits come from screen-model evidence. A future pre-registration should be committed before any run starts. |
| Run 2 used evaluator `a_comp_evaluate-5915cd86` for Q2, where the pre-registration names `2314d433`. | Accepted. The change follows the release correction, and the reviewer's independent port reproduces Q2 exactly. |
| The pipeline's f32 calibrated Dev-iter values differed from one of the release's calibrated Dev-iter files by up to 2.1e-6. | Traced. The pipeline matches the release's corrected bit-exact file to 1.1e-16. The 2.1e-6 gap is between the release's pre-correction file and that corrected file, on 13 of 2,873 documents. The reviewer's own apply matched the pre-correction file, so two apply paths differ by ≤ 2.1e-6. The source of that gap is not identified. No gate outcome depends on it. |

## Incidents

Each incident was rerun before any result was read:
- A host power outage left zero-length dry-run files.
- A dry-run scoring unit was killed at its memory cap and rerun with a larger cap
  and fewer sessions.
- The first release setup failed to parse `./`-prefixed checksum paths; the
  parser was fixed and the setup rerun.

The dry run used a different finalist (F, seed 17) and served as a pipeline test
only.

## What remains before production

*A* serves in f32. Before production:
1. **Measured Cloudflare Container deployment.** Every figure above is a local
   cgroup on one consumer CPU. Before production, the following must be measured
   on real Container instances: CPU microarchitecture and feature level
   (including AVX-VNNI), single-request and saturated latency, cold start with
   image pull and model load, memory accounting and idle cost. Native ONNX
   Runtime kernels differ by CPU feature level, so numerics must also be checked
   on the Container's CPUs. **A Container deployment needs the operator's
   approval** and has not been made.
2. **f32 banded ONNX qualification.** V2 shows the banded f32 graph matching the
   release's recorded Dev-iter outputs to 6.5e-6 in P(involvement). It has not
   passed the formal replay gate against the release replay reference. The
   1.2e-4 maximum origin-logit difference is close to that gate's 1e-4
   involvement-logit tolerance, so this step is not a formality. The fast f32
   Rust graph remains the exact replay reference.
3. **Runtime dependency.** ONNX Runtime must be accepted as a native dependency
   of the Rust Container runtime (`ort`, dynamic loading) or replaced. That
   includes pinning the library and recording its hash in the execution
   manifest.
4. **Request path.** The coordinator Worker needs an authenticated path to the
   Container, with receipts. The heads and calibration either run in the
   Container or are recomputed in the Worker. The served numeric profile,
   calibration and replay reference must be recorded with the release.
5. **Any int8 retry** needs a new pre-registration, committed before any run. It
   needs its own CAL amendment, a margin-aware attribution parity gate, and a
   PC6 limit scaled from calibrated evidence. The packaged FB1 build stays
   marked "not qualified, do not serve".

## RunPod fallback

A GPU backend behind a Worker remains the fallback if the CPU Container cannot
meet demand or latency. [A on a RunPod GPU backend](a-runpod-backend.md) covers
it. In short:
- **Scope.** Paid customer traffic only, as a fallback or burst path. Hidden
  evaluation stays inside Cloudflare.
- **Cost.** A saturated RTX 4090 costs about $0.009 per 1,000 documents of
  2,048 tokens at planning throughput. Sporadic single requests cost far more,
  because idle time is billed.
- **Before use**, it needs:
  - a GPU benchmark of *A*;
  - its own qualified GPU numeric profile and calibrator;
  - the provider-boundary decision and written provider confirmations.

## Limits

- **One host.** All latency and cost figures come from one shared consumer CPU
  under cgroup shapes. Cloudflare Container vCPUs were not measured.
- **Evaluation scope.** Quality gates use Dev-iter only, with source-group
  bootstrap intervals. Long-document evidence is parity only, without labels.
  This is not a Test or external claim.
- **Released seed only.** Only seed 17 of the released arm was qualified. The
  verdict applies to this recipe and fallback on this model. The earlier
  screen-model result (Brier −0.0003, 8 flips) did not carry over to the
  release model.
- **Prices.** List prices on 2026-10-28, busy time only. The 2 vCPU instance
  specification is an assumption, as stated above.

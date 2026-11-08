# SlopNinja supervised shared A baseline

Measured October 26, 2026 on build-host, Linux x86_64. A shared CFG-derived encoder
now has fitted origin, fractional-share, human and model-attribution heads, with
frozen Test evaluation. **All heads being trained does not qualify their quality.**
Pangram parity, reliable share estimates, human attribution, general open-set
rejection, rare false-positive bounds and full Workers execution remain unqualified.
[Earlier structural checkpoints](SHARED_BASELINE.md) retain their own populations
and measured limits.

## Frozen selection and supervision

`SELECTION_FREEZE.json` selected `leaf_grammar` by the lowest mean calibrated
Development involvement Brier across seeds **17, 29, 41**, among all, structure and
leaf-grammar inputs. Their means were 0.150904, 0.152428 and **0.129774**;
availability-only measured 0.147845 but was a diagnostic, not an eligible syntax
candidate. These means are from the retained Rust selection report. Seed 17 is
the representative by predeclared seed order, not best Test performance. Matched
origin-only and attribution-only controls were fitted on the selected inputs
before Test. No parameters, normalization, thresholds or selection changed on Test.
The freeze's `final_test_opened: false` records its time of creation; the later
Test numeric manifest records the authorized opening.

Selected architecture: **2062 -> 64 tanh -> 32 tanh**, with three-way origin,
conditional mixed-share sigmoid, four-name human and three-name model heads.
The 2,062 inputs are 1,031 grammatical leaf category/POS/morphology expectations
and their Train-centered denominator masks. They are **not dictionary lexical
identities or raw word features**. Native, fallback, partial and limited-search
pools remain distinct. Vocabulary, `log1p(count/denominator)` normalization and
mask centering use Train queries only; scale floor is 0.01.

AdamW uses learning rate 0.001, weight decay 0.01 and a 250-epoch budget. Joint
loss weights are origin CE 1, involvement Brier 1, share MSE 1 and conditional
human/model CE 0.25 each, with missing targets masked. Attribution-only retains
conditional CE weight 1 each. Development selects origin/joint checkpoints by
involvement Brier and attribution-only checkpoints by supervised conditional CE.
CAL fits origin temperature and each named-confidence threshold; the latter
balances correct accepted known identities against rejected withheld identities.
Thresholds never use predicted role presence as a cutoff.

| Partition | Queries / reference rows | Source families | Origin / share / fractional-share labels |
| --- | ---: | ---: | --- |
| Train | 229 | 34 | 229 / 169 / 16 |
| Development | 106 | 16 | 106 / 96 / 24 |
| Calibration | 107 | 16 | 107 / 97 / 24 |
| Independent reference | 8 | 4 | 0 / 0 / 0 |
| Final Test | 102 | 15 | 102 / 93 / 24 |

Assembly excluded all ten copies of cross-split duplicate from-scratch outputs
from 460 requested non-Test records. Final Test excluded three generated copies
matching prior or retained text from 105 requested records. Exact exclusions and
failed early duplicate-check attempts are retained. These exclusions preceded
each population's feature export; no label was invented to fill a missing row.

Train has 32 human-identity targets across four writers and 159 model-identity
targets across three models. Each named writer has one Train work; eight reference
rows cover one separate work per writer, two rows each. References are excluded
from training, vocabulary and normalization. The normalized arithmetic mean of
raw learned reference encodings, followed by cosine ranking of normalized query
encodings, is the separate `slop_ninja_reference_normalized_mean_cosine_v1`
diagnostic. There are no AI reference profiles or withheld-human reference profiles.

## Final Test measurements

Test contains **21 human-only labels, 36 model-only and 45 mixed**. Historical
human labels remain proxies, not authenticated production histories. Metrics
weight source families equally within each metric's eligible rows; FPR percentages
are not raw fractions of 21 documents. Percentages below are rounded from reports.

| Control / seed | Selected epoch | Involvement Brier | Human FPR at 0.5 | ECE10 | Three-way accuracy | Share MAE, pp |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Joint 17 | 36 | 0.1258245612 | 66.67% | 0.058347 | 57.56% | 30.1760 |
| Joint 29 | 33 | 0.1291294109 | 86.67% | 0.056441 | 59.56% | 31.6473 |
| Joint 41 | 38 | 0.1341190633 | 66.67% | 0.116483 | 53.63% | 34.7222 |
| Origin-only 17 | 43 | 0.1295506632 | 66.67% | 0.063707 | 48.89% | 35.1589* |
| Origin-only 29 | 32 | 0.1307402475 | 86.67% | 0.072565 | 62.44% | 33.0127* |
| Origin-only 41 | 43 | 0.1332643509 | 60.00% | 0.081620 | 51.56% | 35.9944* |
| Attribution-only 17 / 29 / 41 | 65 / 67 / 57 | untrained | untrained | untrained | untrained | untrained |

\* Origin-only share interpolation is fixed at one half and untrained; both
attribution heads are untrained. Attribution-only origin is untrained and
uncalibrated, with uniform origin probabilities: its diagnostic Brier 0.1792592593,
FPR 100% and share MAE 45.5982 pp are not trained detection/share performance.
The Train-family prior has Test Brier 0.1640740741. Joint does not consistently
beat matched origin-only across seeds. Joint Brier's source-family bootstrap 95%
intervals are [0.100107, 0.152287], [0.109974, 0.147438] and [0.112633, 0.157343].
They measure sampling conditional on the frozen model, not seed or grammar uncertainty.

Share has 93 labeled Test rows, including 24 fractional rows; nine are masked.
For representative joint seed 17, realized-share-bin MAE is:

| Realized share | Rows / families | MAE, pp |
| --- | --- | ---: |
| 0% | 21 / 15 | 44.2371 |
| (0%, 25%] | 5 / 4 | 28.4687 |
| (25%, 50%] | 7 / 5 | 23.8769 |
| (50%, 75%] | 8 / 6 | 14.6973 |
| (75%, 100%) | 4 / 4 | 6.0098 |
| 100% | 48 / 15 | 28.7891 |

The 100% share bin need not equal the model-only class: source-conditioned
rewrites can retain human contribution while all final words are model-produced.
Seed 17 share bias is -5.5056 pp, mean squared error is 1,225.3870 pp squared,
and MAE's family interval is [26.0094, 34.9708] pp.
These errors and only 16 fractional Train labels do not qualify interpolation or
natural editing and long-document behavior.

Identity scoring uses documented present contributors regardless of predicted
involvement. Test has 32 known human rows/four families and 16 withheld human
rows/two families; AI has 60 known rows/12 families and six withheld rows/three
families. The evaluator excludes 36 absent human and 21 absent AI roles, plus
18 present humans and 15 present AIs without documented identities.

| Control / seed | Human top-1 / MRR | AI top-1 / MRR | Human reference top-1 |
| --- | --- | --- | ---: |
| Joint 17 | 31.25% / 0.5391 | 72.32% / 0.8448 | 28.125% |
| Joint 29 | 28.125% / 0.5234 | 58.73% / 0.7721 | 21.875% |
| Joint 41 | 28.125% / 0.5417 | 62.50% / 0.7855 | 21.875% |
| Attribution-only 17 | 34.375% / 0.5677 | 78.57% / 0.8697 | 34.375% |
| Attribution-only 29 | 18.75% / 0.4766 | 83.73% / 0.9071 | 25.00% |
| Attribution-only 41 | 34.375% / 0.5729 | 75.60% / 0.8618 | 34.375% |

Origin-only named-head metrics are uniform-score tie diagnostics, not trained
attribution. Its separate encoder-reference top-1 is 31.25% / 15.625% / 25.00%.
Conditional closed-set accuracy does not measure accepted named correctness:

| Control / seed | Human correct named / withheld rejection | AI correct named / withheld rejection |
| --- | --- | --- |
| Joint 17 | 15.625% / 81.25% | 16.67% / 100% |
| Joint 29 | 6.25% / 87.50% | 17.56% / 100% |
| Joint 41 | 25.00% / 12.50% | 36.86% / 33.33% |
| Attribution-only 17 | 0% / 100% | 41.52% / 83.33% |
| Attribution-only 29 | 18.75% / 37.50% | 61.11% / 33.33% |
| Attribution-only 41 | 0% / 100% | 57.44% / 66.67% |

The same single withheld OLMo model appears in CAL, Development and Test on
different source families. Its rejection is not evidence for arbitrary unseen
model identities. Writer samples are small; generator identity also correlates
with source families, acquisition pipeline and historical versus new generation.
Representative seed 17 returns the fallback on 71.875% of known-human rows and
83.3333% of known-AI rows under family weighting. Strong withheld rejection thus
coexists with frequent rejection of supported identities; it is not sufficient
by itself. Conditional matches and `other_human`/`other_ai` fallbacks do not authenticate
identity or assert role presence/gallery absence. AI top-3 is uninformative with
only three named classes. Reference matching is not the classification head and
has no calibrated open-set rule.

## Matched Pangram comparison

Pangram is the paired raw-text comparator for this cohort. A bounded inventory
of `data/` through depth five found no retained ModernBERT safetensors, ONNX or
`.pt` model artifact. This does not establish absence outside that search scope.
Historical V6 results use a separate cohort and are not a paired control here.

All 102 Test documents are paired. Calls requested `pangram-4`; retained responses
report version **4.1**. Pangram's frozen CAL mapping clips its
nonhuman content fraction to [0.001, 0.999], converts to log-odds and fits a
temperature (2.0137527075) using equal-family CAL Brier. This is **our mapped
involvement score**, not Pangram's native any-AI probability. Its raw content
fraction also has different semantics from measured replacement-span share.

Mapped Pangram Test Brier is **0.0957448598**, with zero observed false positives
among the 21 human-labeled documents at 0.5. Joint-minus-Pangram paired Brier gaps
are +0.0300797014 / +0.0333845511 / +0.0383742035; paired family-bootstrap 95%
intervals are [-0.006305, 0.065672] / [-0.002273, 0.068265] /
[0.006440, 0.069967]. These results do not establish parity or equivalence.
Neither zero observed errors nor degenerate bootstrap intervals establish a rare
false-positive guarantee. The source-family Test partition was reserved from
model selection, but conservative grammar exposure and historical human proxies
remain: the numeric manifest explicitly retains `clean_holdout: false`.

## CFG accounting and runtime boundary

These are exact counters from the two export reports, not complete-English-parse
claims. The 450 non-Test documents include eight references.

| Export total | Non-Test | Test |
| --- | ---: | ---: |
| Documents | 450 | 102 |
| Input bytes = accounted bytes | 641,749 | 138,732 |
| Tokens | 124,165 | 26,903 |
| Represented tokens | 124,130 | 26,869 |
| Unresolved tokens | 35 | 34 |
| Windows | 5,638 | 1,223 |
| Native-complete / fallback-complete windows | 2,888 / 1,256 | 495 / 312 |
| Partial / resource-limited windows | 1,417 / 77 | 391 / 17 |
| Tokens lacking real lexical readings | 2,362 | 722 |
| Execution failures / projection failures / budget-skipped windows | 0 / 0 / 0 | 0 / 0 / 0 |
| Forced boundaries | 1 | 0 |

Partial and limited-search pools preserve their limitations. Byte accounting and
zero execution failures do not certify linguistic correctness or detector quality.
The frozen [document fallback pipeline](../../DOCUMENT_FALLBACK.md) records unresolved
regions rather than dropping them or fabricating dictionary identities.

Representative weights use `slop_ninja_shared_a_weights_v1`: **1,075,800 bytes**
of loaded float64 numeric parameters, **4,066,228 JSON bytes**. Development Rust/
PyTorch conformance checks named scores, profile/fallback names, role involvement,
origin, confidence and share. Its maximum difference is **0.0000076602649 pp**;
1,000 warm numeric calls measured p50 **0.048906 ms**, p95 **0.050269 ms**.
Streaming `shared_predict` process peak RSS was **8,156 KiB**. The later Test
conformance measured maximum difference **0.0000084890833 pp**, p50 0.046416 ms
and p95 0.049440 ms. These are distinct runs, not an end-to-end service benchmark.

Numeric inference excludes native grammar load, parsing and projection. The
102-document Test extraction took **3:31.55 wall time** and peaked at
**426,864 KiB RSS**. Full Workers parsing/transport/runtime memory and latency
remain separately unqualified; the small numeric head does not establish that
the complete path fits Workers limits. Measurements are Linux x86_64 CPU and
PyTorch 2.9.1+cu128 fitting evidence, not macOS or Workers qualification.

The completed `project-and-smoke/run.log` records **nine synthetic projector
tests passed**, followed by the separately invoked conformance test: all **450
non-Test documents, including eight references**, matched the prepared vectors
bit for bit across **927,900 float64 coordinates**. Three unlabelled synthetic
documents covering unfamiliar words, Unicode/layout and empty input then passed
CFG export, frozen projection and numeric prediction end to end. The
`grammar-a-baseline` library also built for `wasm32-unknown-unknown`. That build
does not establish Wasm execution, Workers integration, parser memory bounds or
service qualification. The complete project-and-smoke job exited zero.

### Completed paired layout diagnostic

All **102 Test documents** were paired with a whitespace-collapse view using
`rust_unicode_split_whitespace_join_ascii_space_v1`: split on Unicode whitespace,
then join with one ASCII space. Whitespace changed in **71 documents**; word
sequences and targets were unchanged. Original inputs remain retained. Extraction
completed with zero execution failures. The representative joint-17 weights,
numeric transform, named thresholds and CAL mappings stayed frozen; no retraining
or selection occurred.

| Family-weighted measurement | Original | Layout view |
| --- | ---: | ---: |
| Joint-17 involvement Brier | 0.12582456118639124 | 0.1256306261485173 |
| Human FPR at 0.5 | 66.6667% | 66.6667% |
| Share MAE, pp | 30.1760486417 | 30.1073696993 |
| Mapped Pangram Brier | 0.09574485982389375 | 0.09550355412131747 |

The layout-view joint-minus-Pangram Brier gap is **0.030127072027199782**,
with paired source-family 95% interval [-0.006399, 0.065744]. Pangram uses the
same frozen CAL mapping temperature **2.0137527075**, not a layout-fitted mapping.

Per-document absolute prediction changes use **equal weight for all 102 paired
documents**, including those with unchanged whitespace. These descriptive
statistics differ from the family weighting of performance above.

| Absolute delta, pp | Mean | p95 | Maximum | Documents above 1 pp |
| --- | ---: | ---: | ---: | ---: |
| AI-involvement confidence | 0.1252442940 | 0.3575768959 | 4.2182172112 | 4 |
| AI share | 0.1122577979 | 0.3635725437 | 4.2591999692 | 3 |

One document crossed the 50% confidence boundary. Neither conditional human nor
AI profile/fallback name changed. Those name comparisons include absent roles
and are **not false-attribution counts**. Small aggregate changes coexist with
individual changes above four percentage points; one whitespace perturbation
does not establish general formatting robustness or change the quality limits.

## Artifacts and reproduction

All private data, weights and live logs remain ignored beneath
`data/grammar-private/a-baseline-20261026/supervised-v1/` (`STAGE` below).
Checked-in prose contains no source passages or identifying private manifests.

| Artifact under `STAGE` | Purpose |
| --- | --- |
| `SELECTION_FREEZE.json` | Pre-Test feature/seed policy and selected hashes |
| `fit-v1/shared_a.py`, `fit-v1/TRAINER.sha256` | Exact tested trainer snapshot |
| `fit-v1/leaf_grammar/numeric/{contract,manifest,population}.json` | Frozen Train transform and non-Test population |
| `fit-v1/leaf_grammar/{joint,origin_only,attribution_only}-{17,29,41}/` | Nine fits, weights, CAL metadata, encodings and predictions |
| `test-evaluation/numeric/` | Frozen transform, copied non-Test bindings, authorized Test binding/audit |
| `test-evaluation/*-report.json` | Nine Test evaluation reports |
| `fit-v1/leaf_grammar/*/test-predictions.json.provenance.json` | Frozen Test/manifest/contract/weights bindings; no fitting |
| `test-comparison/{joint,origin_only,attribution_only}-{17,29,41}.json` | Existing matched Pangram reports |
| `export/report.json`, `test-evaluation/export/report.json` | Source accounting and parser counters |
| `conformance/{rust.json,stream-resources.txt}` | Development numeric conformance and streaming RSS |
| `test-evaluation/{rust-conformance.json,extraction-resources.txt}` | Test numeric conformance and separate extraction resources |
| `project-and-smoke/{run.log,run.exit}`, `project-and-smoke.sh` | Projector tests, exact non-Test vector parity, synthetic end-to-end smoke and Wasm library build |
| `layout-dataset/manifest.json`, `layout-evaluation/` | Frozen paired whitespace policy, transformed inputs and joint-17 predictions |
| `layout-comparison/{pangram,sensitivity}.json` | Paired mapped-Pangram comparison and equal-document prediction deltas |
| `layout-comparison/{run.log,run.exit}` | Successful completion and comparison commands' results |
| `audit/audit.md`, `audit/evidence.json` | Earlier independent nine-fit full-feature audit; not a Test audit |

Trainer SHA-256: `f477d516b1d2556c7a487c25593861b505ce22cd254ee79cda578f935003a6ee`.
Selected numeric contract: `12fa7056135eb0d361378618dc60936eed1a1f43663c512c51a5aa9e03507d53`.
Representative joint-17 weights: `6621ec634684e70da6610a0d02c760b366c759c3348ab725507ddffb51a3dde3`.
The freeze and per-fit reports retain the other checkpoint, calibration and input hashes.

Run reproduction on build-host in task-owned, memory-capped, zero-swap jobs; start
with one Cargo job and one parser worker, using this checkout's own build target.
Commands below require retained private artifacts and fresh output paths; they do
not contact a model or detector provider. The [shared inference contract](SHARED_CONTRACT.md)
defines the versioned numeric input/output schemas and cross-field invariants.

```sh
TASK_ROOT=data/grammar-private/a-baseline-20261026
STAGE="$TASK_ROOT/supervised-v1"
FIT="$STAGE/fit-v1/leaf_grammar"
BIN="$TASK_ROOT/grammar-target/release/examples"
CFG_EXPORT="$TASK_ROOT/fallback-measure-v3/cfg_document_export"

# Non-Test preparation, then fitting: repeat controls and seeds as frozen above.
"$BIN/prepare_supervised_forest" "$STAGE/export" "$STAGE/dataset/manifest.json" \
  NEW_NUMERIC_DIR --ablation leaf_grammar
"$TASK_ROOT/venv/bin/python" "$STAGE/fit-v1/shared_a.py" train \
  --data "$FIT/numeric" --out NEW_FIT_DIR --control joint \
  --seed 17 --epochs 250 --device cuda --export-encodings

# Frozen Test transform/inference/evaluation; no fitting or threshold selection.
"$BIN/transform_supervised_forest" "$FIT/numeric" "$STAGE/test-evaluation/export" \
  "$STAGE/test-dataset/manifest.json" NEW_TEST_NUMERIC_DIR
"$TASK_ROOT/venv/bin/python" "$STAGE/fit-v1/shared_a.py" predict-split \
  --data NEW_TEST_NUMERIC_DIR --artifact "$FIT/joint-17/weights.json" \
  --split test --out NEW_TEST_PREDICTIONS.json
# Existing frozen predictions are already in FIT/joint-17/test-predictions.json.
"$BIN/evaluate_supervised" "$STAGE/test-evaluation/numeric" "$FIT/joint-17" \
  test NEW_REPORT.json
"$BIN/compare_supervised_pangram" "$STAGE/test-evaluation/numeric" "$FIT/joint-17" \
  test "$STAGE/test-pangram/results" "$STAGE/pangram/calibration" NEW_COMPARISON.json
"$BIN/shared_inference" "$FIT/joint-17/weights.json" \
  "$STAGE/test-evaluation/numeric/test.jsonl" "$FIT/joint-17/test-predictions.json" \
  NEW_RUST_CONFORMANCE.json

# Recompute layout diagnostics from retained observations; no provider calls.
"$BIN/compare_layout_predictions" "$FIT/joint-17/test-predictions.json" \
  "$STAGE/layout-evaluation/model/test-predictions.json" \
  "$STAGE/layout-dataset/manifest.json" NEW_LAYOUT_SENSITIVITY.json
"$BIN/compare_supervised_pangram" "$STAGE/layout-evaluation/numeric" \
  "$STAGE/layout-evaluation/model" test "$STAGE/layout-pangram/results" \
  "$STAGE/pangram/calibration" NEW_LAYOUT_COMPARISON.json
```

For an arbitrary raw document, the native entry path is `cfg_document_export`
then `project_frozen_documents` then `shared_predict`. The projector accepts an
export bound to the frozen engine/contract and emits `{id,input}` rows without
fitting or requiring origin, share, writer or generator labels. This entry point
passed the synthetic and non-Test conformance checks described above; those
checks do not extend the numeric-only timing scope.

```sh
"$CFG_EXPORT" jsonl "$TASK_ROOT/worktree" INPUT_ID_TEXT.jsonl NEW_EXPORT_DIR
"$BIN/project_frozen_documents" "$FIT/numeric/contract.json" NEW_EXPORT_DIR NEW_NUMERIC.jsonl
"$BIN/shared_predict" "$FIT/joint-17/weights.json" NEW_NUMERIC.jsonl NEW_PREDICTIONS.jsonl
```

Freeze source works and all derivatives together in any next data experiment.
The supported next step is more independent writer works, source-balanced model
examples, measured fractional supervision and distinct withheld calibration/
evaluation identities under the same architecture, retaining paired formatting
checks. These measurements do not justify claiming qualification or
starting an unrestricted architecture search. This fitted aggregate-count model
beats the Train-family prior on involvement Brier but does not approach acceptable
human false positives. It supports a bounded further CFG-only experiment, not
the claim that CFG-only A is viable for launch. It does not test ordered/tree
representations or persistent dictionary lexical features. Documented contemporary
human production histories, more fractional rewrites, and independent unseen
generator identities are the largest missing data qualifications.

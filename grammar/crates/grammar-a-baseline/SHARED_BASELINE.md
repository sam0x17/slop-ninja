# Shared A structural baseline checkpoints

Current status, October 26, 2026: the
[supervised shared A baseline](SUPERVISED_BASELINE.md) has fitted origin, fractional
share and both attribution heads, with frozen Test results for three seeds and
matched controls. Quality and Pangram parity remain unqualified. The measured
structural checkpoints below are retained as history; their populations,
supervision and feature spaces differ from the current supervised cohort.

## Earlier forest baseline with total source accounting

The frozen document pipeline exported all 65 requested non-Test documents with
zero execution failures, including ten documents previously unavailable to the
selected-tree training/export path. Source-family assignments were preserved:
44 Train documents / 15 families, 11 Development / 4, and 10 Calibration / 4.
All are grammar-exposed research examples; at this earlier checkpoint, 15 reserved
Test documents remained unopened. This is not a clean generalization evaluation. The separate robustness
smoke inputs are not used for fitting or calibration.

`prepare_forest_shared` fits 4,299 structural/availability coordinates from Train
only, yielding 8,598 numeric inputs after appending mask coordinates. Native-complete,
fallback-complete, partial, search-limited and lexical-recovery expectations
occupy different pools. Token-rate counts use all document token opportunities;
document availability counts/flags use denominator one. Train family-weighted
`log1p` normalization has a 0.01 scale floor. Masks are centered by their Train
presence means, with that transform explicitly exported. Raw text, IDs, labels,
hashes, provenance and review metadata are excluded from learned inputs.

The mask centering corrects a demonstrated numerical failure: duplicating 4,299
constant-one mask inputs saturated every first-layer unit in an initial fit.
A Train-only diagnostic measured mean absolute mask contribution 12.66 versus
0.12 from numeric values, with no hidden-state variation. Centering reduced mask
contribution and saturation to zero and restored varying document encodings.
That defective fit is retained locally as diagnostic evidence, not a model result.

The architecture remains `8598 -> 64 tanh -> 32 tanh`, with the common encoding
feeding all four heads. AdamW, learning rate 0.001, weight decay 0.01 and 250
epochs match the initial setup. Both joint and origin-only controls use seeds
17, 29 and 41, the same data, encoder capacity, budget, Development checkpoint
selection and Calibration temperature fitting. The representative checkpoint is
joint seed 17, chosen by seed order rather than best observed result.

Supervision is 44 origin labels and 30 endpoint-share labels. There are **zero
measured mixed-share targets and zero human/generator attribution targets**.
Thus “joint” here compares origin plus endpoint share against origin alone;
it does not test the proposed reciprocal benefit of attribution and detection.
Attribution-only fitting and open-set retrieval remain unavailable, not successful.
This exposed cohort contains no fallback words, so learning fallback-specific
signals also remains unproven despite the pipeline's tested marker carriage.

| Development measurement | Joint seeds 17 / 29 / 41 | Origin-only seeds 17 / 29 / 41 |
| --- | --- | --- |
| Calibrated involvement Brier | 0.3642 / 0.3267 / 0.2660 | 0.4700 / 0.2661 / 0.3314 |
| Mean Brier (sample seed SD) | 0.3190 (0.0496) | 0.3558 (0.1041) |
| Mean Brier before temperature fitting | 0.2312 | 0.2352 |
| Human false positives among 4 documents | 2 / 1 / 0 | 2 / 4 / 1 |
| Three-way origin accuracy | 0.4167 / 0.5000 / 0.5833 | 0.4167 / 0.2917 / 0.5000 |
| Endpoint-share MAE, percentage points | 43.65 / 29.78 / 27.91 | 44.15 / 50.64 / 36.70 |

The family-weighted prior Brier is 0.2353. Calibration on only four families
worsened both controls on Development. Joint learning shows a small average
advantage over this origin-only control, but neither beats the prior after
calibration. This does not establish useful CFG-only detection, attribution
benefit, reliable share interpolation, or a rare false-positive bound. Full
per-seed calibration bins, origin scores, share squared error/bias, endpoint
slices and source-family bootstrap intervals remain in the local reports.

Pangram 4.1 observations were reused for the same 11 Development and 10
Calibration documents, with no additional API calls or invented labels. Pangram
has 0/4 human false positives and mapped involvement Brier below 0.001 here.
The mapping uses the nonhuman text fraction and Calibration only, as explained
for the earlier experiment below; it is not a native Pangram involvement
probability. Its extreme fitted temperature and this tiny exposed sample do
not establish general calibration. **Pangram parity has not been reached.**

The representative model exports 16,958,640 JSON bytes and 4,420,912 numeric
float64 weight bytes. Rust matches PyTorch reference percentages within
0.000004706 percentage points. In 1,000 warm calls its numeric head measured
median 0.203 ms and p95 0.211 ms; the measured process peaked at 28,168 KiB RSS.
These figures exclude the seconds-scale native parsing/export work reported
separately. The existing portable Rust runtime uses only affine, tanh, softmax
and sigmoid operators; Workers execution of the full pipeline is unqualified.

Artifacts live in ignored `data/grammar-private/a-baseline-20261026/`:
`fallback-fit-v2/` contains all six fitted checkpoints, numeric data contract,
split/population manifests, predictions, weights and evaluation/Pangram reports;
`fallback-finish-v1/` retains aggregate results, activation diagnostic and CPU
measurements. `fallback-measure-v3/` retains the source-bound feature export.
Reproduce with the owned-target build of `prepare_forest_shared`, then:

```sh
prepare_forest_shared DOCUMENT_EXPORT_DIR NEW_NUMERIC_DIR
python grammar/crates/grammar-eval/python/shared_a.py train \
  --data NEW_NUMERIC_DIR --out NEW_MODEL_DIR --control joint \
  --seed 17 --epochs 250 --device cuda
evaluate_shared NEW_NUMERIC_DIR NEW_MODEL_DIR development NEW_REPORT.json
shared_inference NEW_MODEL_DIR/weights.json NEW_NUMERIC_DIR/development.jsonl \
  NEW_MODEL_DIR/development-predictions.json NEW_RUST_CHECK.json
```

Repeat the declared seeds for both controls. Use task-owned durable jobs and
resource limits as in the retained scripts; training reserves at most 20% of
CUDA memory. The next justified work is a larger, provenance-qualified population
with independent writers, multiple generators and measured rewrite spans under
this frozen input pipeline. Aggregate expectations on 44 exposed Train documents
do not settle the viability of ordered or tree encoders. Grammar review waves
remain paused.

## Earlier selected-tree checkpoint

Measured on build-host's RTX4090, October26,2026. The first real shared-encoder
model is fitted and saved. It falls well short of Pangram on this small exposed
development screen. It is a research checkpoint, not the completed A qualification.

The canonical grammar is accepted `english-wave5-public-v1`, identity
`c9d09e65ccc4a27cf2a76a91f23ada9efef5818d269d89a0025ff46c1e4a4c03`, from
repository base `cc5253d4`. Unfinished wave proposals are excluded. The CFG campaign
is stopped with verified stock partial checkpoints; local recovery instructions
and its caches/receipts are retained. New training work does not resume the wave.
Fresh-data parsing uses a separately frozen opt-in generic-word fallback policy;
the first fit below uses retained ordinary selected trees. Fallback-complete
parses remain distinct from reviewed mechanical grammar coverage.

## Data and fit

The requested80-document cohort spans28 source families and was exposed during
grammar development. A native-verified cache export under one current
`default_campaign` profile produced66 usable documents:18 historical human
proxies,27 model drafts and21 mixed edits. Fourteen documents lack usable current
selected trees and remain in the requested-population ledger. The older70-row
export combined historical parser profiles and is not this training input.

The deterministic source-family split has37Train rows/15families,
9Development/4families,9Calibration/4families and11reservedTest/5families.
All descendants stay with their source work. These are exposed research splits,
not clean holdouts. The reserved Test has not been opened. Historical human-proxy
labels do not establish a documented human-only workflow. The generated cohort
has one Qwen revision; it cannot qualify generator generalization.

Rust fits a Train-only vocabulary of904 structural coordinates: category counts,
production arities, ordered category edges and tree shape. Counts are divided
by their opportunity denominators, transformed with `log1p`, and standardized
using equal source-family weights on Train. Each coordinate has an availability
bit, giving1808 numeric inputs. Source words, labels, author/generator IDs,
parser hashes and review metadata are excluded. Lexical/combined features are
unavailable. This tests aggregate structural counts, not ordered/tree encoders.

The encoder is `1808 -> 64 tanh -> 32 tanh`. Origin, conditional mixed share,
human and generator heads consume that same encoding. Training uses AdamW,
learning rate0.001, weight decay0.01,250epochs, seed17, with earliest best
Development involvement Brier checkpoint(epoch30). Temperature0.449329 is fitted
on Calibration after checkpoint selection. The initial fit took0.96seconds in
the training loop and allocated71,201,280peak CUDA bytes; this excludes environment
setup, data preparation and parsing.

Supervision covers37origin targets and25share endpoints. **There are no measured
mixed-share or attribution targets in this fit.** Those losses are masked. Both
conditional attribution outputs therefore return their fallback with involvement
probability; this establishes output compatibility, not learned attribution.
No joint-versus-single-task benefit is claimed. Further data and matched repeated
seed controls remain required.

## Development observations

Metrics average source families, then documents within each family. Only four
Development families and three human-proxy documents are present.

| Measurement | Shared structural A | Training prior | Pangram comparator |
| --- | ---: | ---: | ---: |
| AI-involvement Brier |0.1916|0.2088|<0.001 after separate Calibration mapping|
| Human false positives at0.5 |2/3|—|0/3|
| Three-way origin accuracy |0.5417|—|Not equated with API text fractions|
| Share endpoint MAE |36.97percentage points|—|No replacement-span qualification|

Pangram received all11requested Development documents and all10Calibration
documents, including CFG export failures. The paired table uses the same9usable
Development documents as A. All21API requests completed. The requested selector
was `pangram-4`; the response version was4.1. Exact inputs, task receipts, raw
responses, versions and timings remain ignored.

The API exposes AI/AI-assisted/human text fractions, not a native probability
that any AI contributed. For the Brier comparison only, the sum of nonhuman
fractions is clipped to[0.001,0.999] and passed through a log-odds temperature map
fit on the same9Calibration documents, using family-weighted Brier and the
predeclared `exp(k*0.1)`,k=-30..30grid. The chosen temperature is0.049787. Pangram
separates this tiny Development sample perfectly; the near-zero mapped Brier
does not establish calibrated confidence elsewhere. Its predictions never
supply training labels or share ground truth. Source-family bootstrap intervals
are retained, but this sample cannot support rare false-positive guarantees.

## Runtime and artifacts

The PyTorch checkpoint is about466KiB. The explicit JSON weight artifact is
3,584,179bytes; Rust stores944,432numeric weight bytes in float64. The only
operators are dense affine layers,tanh,softmax andsigmoid. Rust predictions
match the saved PyTorch references within0.000006991percentage points.

On Linuxx86_64,1000warm CPU calls measured median42.6microseconds,
p9582.9microseconds andp9985.2microseconds. Artifact loading took7.8milliseconds;
the benchmark process peaked at8176KiB RSS. These are **numeric inference costs**,
not first-time CFG parsing/resolution. The library cross-compiles for
`wasm32-unknown-unknown`; execution inside Workers is not yet measured. Production
targets Rust/Wasm Workers, with a compute-only serverless Container if needed.
No Python/GPU daemon or Workers AI catalog support is required by the numeric
runtime. Deployment implementation is outside this experiment.

Retained artifacts are under ignored
`data/grammar-private/a-baseline-20261026/`: `numeric-v1/` binds schemas,
vocabulary, normalization, splits and population provenance; `first-fit-v1/model/`
holds `model.pt`,`weights.json`,predictions and fit receipt;
`evaluate-first-v1/`,`compare-pangram-v1/` and`inference-check-v1/` hold reports.
`pause/RESUME.md` describes campaign recovery. Original executed source snapshots,
failed attempts, requirements lock and systemd memory/exit receipts are retained.
Private texts/profiles, numeric artifacts and provider logs are not committed.

Reproduction commands, from a frozen checkout and its own Cargo target:

```sh
cargo run --release --manifest-path grammar/Cargo.toml -p grammar-native \
  --example cfg_vector_prepare -- REPO COHORT.jsonl NEW_EXPORT MATCHED_CACHE_REPORT.json
cargo run --release --manifest-path grammar/Cargo.toml -p grammar-a-baseline \
  --example prepare_shared -- NEW_EXPORT NEW_NUMERIC
PYTHON grammar/crates/grammar-eval/python/shared_a.py train \
  --data NEW_NUMERIC --out NEW_FIT --device cuda --seed 17 --epochs 250
cargo run --release --manifest-path grammar/Cargo.toml -p grammar-a-baseline \
  --example evaluate_shared -- NEW_NUMERIC NEW_FIT development NEW_REPORT.json
cargo run --release --manifest-path grammar/Cargo.toml -p grammar-a-baseline \
  --example shared_inference -- NEW_FIT/weights.json NEW_NUMERIC/development.jsonl \
  NEW_FIT/development-predictions.json NEW_RUNTIME_REPORT.json
```

Use `CUBLAS_WORKSPACE_CONFIG=:4096:8`, one CPU training thread, a durable capped
zero-swap job and the retained PyTorch2.9.1+cu128/Python3.12environment. Paths marked
NEW must not exist. The original fit's executed trainer snapshot is retained
alongside its checkpoint. Subsequent inference serialization hardening preserves
the architecture while allowing small floating-point differences.

The next experiment must add independently attributed works, multiple generators,
measured partial-rewrite spans and source-family-separated clean populations.
Freeze fallback and ambiguity handling before fresh parsing, preserve failures,
and compare joint/origin-only/attribution-only fits with repeated seeds. The
current result supports running that bounded comparison; it does not establish
CFG-only viability or Pangram parity, and it does not rule out richer CFG features.

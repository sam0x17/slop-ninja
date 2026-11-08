# SlopNinja starter A numeric baseline

This crate contains the original **numeric contract and synthetic origin baseline**
and the separate `shared` Rust inference module for the
[supervised shared A research baseline](SUPERVISED_BASELINE.md), updated October
26, 2026. The selected 2062 -> 64 -> 32 model has trained origin, fractional-share
and human/model attribution heads, plus frozen Test results and matched controls.
High false positives, share error and weak attribution prevent qualification;
trained heads do not establish production quality. The
[earlier structural checkpoints](SHARED_BASELINE.md) preserve their measured limits.
The native raw-document path is `cfg_document_export` -> `project_frozen_documents`
-> `shared_predict`; frozen projection matched all 450 non-Test vectors exactly,
and three unlabelled synthetic inputs passed end to end. The library builds for
`wasm32-unknown-unknown`; full Workers execution remains unqualified.
The subsequent [V6/text-plus-CFG pilot](../../../docs/a-v6-cfg-fusion.md)
fits the same shared-head contract with frozen text activations and matched
CFG-only/text-only controls. Its Development improvement does not qualify
Pangram parity or the full ModernBERT pipeline for Rust/serverless serving.
The legacy `fit`/`predict` CLI described below remains an origin-only control.
The current [shared inference contract](SHARED_CONTRACT.md) specifies numeric
input/output schemas, frozen preprocessing bindings and output invariants.
Neither numeric runtime parses prose nor calls an LLM, a
provider, a text encoder, or the network. Local CFG proposal inference belongs
to the campaign engine, outside this learned numeric runner.

The public API is `Contract::identity`, `validate_export`, `fit`, `predict`, and
`evaluate`. `fit` returns `FitOutcome::Unavailable` rather than inventing labels
when the identical eligible rows lack the configured minimum groups/classes.
`predict` accepts only `NumericInput`; its serde schema rejects prose fields.

The crate is registered as `crates/grammar-a-baseline` in `grammar/Cargo.toml`.
This crate does not require changes to the native vector or detector crates.
Build and run the synthetic tests on build-host in the coordinator's capped,
zero-swap build job, using this checkout's own build directory:

```sh
cargo test --manifest-path grammar/Cargo.toml -p grammar-a-baseline
```

## Frozen V6 residual warm start

The October 26 warm-start path uses the exact retained 768-dimensional V6
classifier input `h` and the frozen 1820-dimensional grammatical-leaf CFG block
`v`. `prepare_warm_start` reads the frozen fusion bundle and original embedding
rows; it never reconstructs raw activations by inverting rounded normalization.
The inherited Train-only text mean/scale apply inside the adapter:

```text
u = concat((h - text_mean) / text_scale, v)       # 2588 coordinates
z = h + Linear_768(tanh(Linear_64(u)))           # one shared 768-vector
origin = softmax(origin_head(z) / base_temperature / temperature)
share = origin[model_only] + origin[mixed] * sigmoid(mixed_share_head(z))
```

The final residual affine starts with zero weights and bias; the origin head
copies V6's classifier and historical `base_temperature = 1.8776105792643432`.
The additional CAL temperature starts at one. Share, human and model attribution
heads all consume `z`; their initial random outputs are unqualified diagnostics.
The upstream V6 encoder remains frozen. Both controls have identical shapes:
`text` zeros the CFG block, while `combined` retains it.

The current CFG block is a document summary with **910 grammatical-leaf
coordinates and 910 opportunity-mask coordinates**. A leaf key combines its
parse-outcome pool, inventory/fallback marker, POS, grammatical category and
features (such as number, tense or verb form), and morphology kind. Dictionary
word identities, raw words, grammar item IDs and tree/Merkle paths are absent.
Native-complete, fallback-complete, partial, search-limited and lexical-recovery
observations occupy separate pools; recovery counts do not assert a complete
syntactic parse.

Rust computes expected leaf counts across the retained derivations under the
frozen resolver, sums window counts, and divides each count by the total document
token count. It applies `log1p(count / tokens)`, then the frozen Train mean and
standard deviation (floor 0.01). The vocabulary contains sorted grammatical keys
declared in at least two original Train queries. Train extension freezes that
vocabulary and normalization. Masks indicate whether the rate denominator exists,
not whether the parse is complete or exhaustive. They are centered by their
original Train means; an unavailable value has zero payload. Thus the 1,820
coordinates are not 1,820 distinct grammatical measurements.

This representation discards sentence order and tree topology after counting.
It can distinguish grammatical distributions and fallback use, but two documents
with the same pooled leaf measurements have the same CFG input. A sentence
sequence or tree encoder would be a separate representation experiment. The
standalone four-position-bin probe is also separate from this residual input.
The adapter receives `768 + 1820 = 2588` floats, produces a 64-unit tanh hidden
state, and projects a correction back to the shared 768-vector consumed by all
heads. Zero-initializing that correction preserves the V6 origin function before
training; it does not initialize trained share or attribution outputs.

For the frozen Train-extension experiment, `train --mixture-protocol PROTOCOL`
optionally assigns half the loss weight to original Train families and half to
added Train families. Within each cohort, source families and observed origin
classes have equal weight. Every supervised task renormalizes after masking;
unknown added human identities therefore cannot dilute or invent human targets.
The input-bound `slop_ninja_v6_residual_cohort_mixture_v1` protocol fixes 250
steps, seeds 17/29/41 and the single 50/50 choice. It changes neither learned
inputs nor Dev/CAL weights. Each fit retains the exact protocol, cohort/masked
task mass audit and weight digest. Without this option, behavior is unchanged.
The Rust summarizer requires `--cohort-mixture` for these fits and validates
matched expanded inputs; adding `--train-scale` instead compares against the
original Train parent. The named mixture field is additional to the unchanged
`query_weighting` field describing within-cohort and evaluation weighting.

`train --control origin_only|attribution_only --control-protocol PROTOCOL` runs
matched research adaptation controls for the selected residual. A separate
`slop_ninja_v6_residual_adaptation_controls_v1` protocol binds the selected
modality, numeric manifest, initialization, current equivalence receipt, budget,
selection and permitted calibration. Both controls inherit V6's earlier origin
supervision; attribution-only is not origin-free pretraining. Inactive heads are
frozen, share interpolation is an untrained fixed half, and untrained attribution
returns uniform conditional scores with fallback. Fit records distinguish
available labels from used supervision. These controls are diagnostic and cannot
replace a complete joint A candidate. The Rust loader validates the control
metadata without changing prediction equations. Combining a nonjoint control
with cohort-mixture training is rejected pending a separately qualified protocol.
`summarize_adaptation_controls NUMERIC_TEXT JOINT_FITS_TEXT CONTROL_FITS GATE OUT`
binds the matched reports and publishes paired source-family comparisons.

[`export_v6_classifier.py`](../grammar-eval/python/export_v6_classifier.py)
reads the immutable qualified artifact through its original bundled loader,
preserving artifact/file digests, exact dependency and CPU environment checks,
and bundled reference-probability checks. It exports only the classifier into
`slop_ninja_v6_classifier_init_v1`, recording artifact ID, manifest hash and actual
weight-file hash. It disables artifact bytecode writes, uses offline loading,
rejects output inside the artifact and never overwrites an initializer. Do not
modify the artifact or relax these checks to obtain an export.

[`warm_start_a.py`](../grammar-eval/python/warm_start_a.py) requires both controls
to match every retained baseline origin probability within absolute `1e-6`
before fitting. Its receipt binds the initializer, implementations, contracts,
all four non-Test numeric files and baseline predictions. Each fit rechecks
those bindings and initial equivalence on its training device before creating
an optimizer. References participate only in equivalence and frozen encoding;
they never fit normalization, profiles or weights. Test files are not opened.
The original experiment requires exactly 929 rows. The explicit Train-extension
path below qualifies all original and added rows under a versioned receipt.

The fixed recipe trains the residual and all heads for 250 full-batch AdamW
epochs, seeds 17/29/41, learning rate `.001`, weight decay `.01`, gradient clip
`5`, and `family_origin_balanced` query weights. The reused `shared_a` joint loss
is origin CE plus involvement Brier plus share MSE, with coefficient `.25` for
each conditional attribution CE. Development involvement Brier selects the
checkpoint, including epoch zero; CAL alone selects the extra temperature and
named-profile thresholds. No architecture or seed search is performed.

Run from the checkout root in a durable build-host job, with one CPU, zero swap
and explicit RAM limits (6 GiB for qualification, 10 GiB for training). Training
limits CUDA allocation to 20% of the selected GPU and disables TF32. Set `c` to
the task directory containing the frozen fusion/embedding bundles and protocol;
set `bins` to this checkout's release examples, `v6_artifact`/`v6_python` to the
qualified immutable artifact/runtime, and `train_python` to the PyTorch training
environment. Use new output paths; preserve failed attempts and receipts.

```sh
py=grammar/crates/grammar-eval/python
export PYTHONDONTWRITEBYTECODE=1 OMP_NUM_THREADS=1 MKL_NUM_THREADS=1
export CUDA_VISIBLE_DEVICES=0 CUBLAS_WORKSPACE_CONFIG=:4096:8
"$train_python" -m unittest discover -s "$py" -p test_warm_start_a.py
"$bins/prepare_warm_start" "$c/numeric-fusion-v1" "$c/embeddings-v1" \
  "$c/WARM_START_PROTOCOL.md" "$c/numeric-warm-v1"
"$v6_python" "$py/export_v6_classifier.py" --artifact "$v6_artifact" \
  --out "$c/warm-start-v1/init.json"
"$train_python" "$py/warm_start_a.py" qualify \
  --text "$c/numeric-warm-v1/text" --combined "$c/numeric-warm-v1/combined" \
  --init "$c/warm-start-v1/init.json" --out "$c/warm-start-v1/equivalence.json"
for mode in text combined; do
  mkdir -p "$c/fit-warm-v1/$mode"
  for seed in 17 29 41; do
    "$train_python" "$py/warm_start_a.py" train --data "$c/numeric-warm-v1/$mode" \
      --init "$c/warm-start-v1/init.json" --equivalence "$c/warm-start-v1/equivalence.json" \
      --seed "$seed" --device cuda --out "$c/fit-warm-v1/$mode/joint-$seed"
  done
done
```

Weights use `slop_ninja_v6_residual_a_weights_v1`: row-major `{shape, values}`
tensors for `residual.0`, `residual.2`, `origin`, `mixed_share`, `human` and `ai`,
plus input-contract/initializer digests, float32 normalization buffer values,
both temperatures, profiles, thresholds and supervision counts. Fit receipts,
split predictions and independent Train reference encodings accompany them.
`WarmSharedModel` implements the numeric adapter and heads in Rust;
`shared_predict` and `shared_inference` dispatch both weight schemas. For each
fit and Development/CAL split, replay with
`shared_inference WEIGHTS NUMERIC_ROWS PYTORCH_PREDICTIONS NEW_OUTPUT` before
evaluation. Numeric replay does not qualify full V6/ModernBERT encoding, CFG
parsing, or end-to-end Rust/serverless serving. Pretraining exposure remains
unknown, and historical author labels remain proxies. Measured quality belongs
in the experiment report, separately from this architecture contract.

After frozen Development/CAL replay, evaluation and retained Pangram comparisons,
`summarize_warm_start WARM_NUMERIC WARM_FITS OLD_NUMERIC OLD_FITS INIT EQUIVALENCE
NEW_REPORT` checks artifact bindings and writes aggregate-only JSON. It reuses
the fusion summarizer's fixed-seed family bootstrap for paired Brier and share
MAE differences; no text, profile names or per-document results enter that file.

### Cumulative Train extensions

Before opening the reserved final Test, `freeze_final_selection COHORT TRAIN_SCALE
NEW_RECEIPT` verifies the eight original joint configurations, two uniform Train
extensions, two cohort-mixture fits and four source-size configurations, each
with seeds 17/29/41. It requires the completed twelve-fit source-size report.
Selection keeps the predeclared mean Development involvement-Brier rule, then
human FPR, active input dimension and candidate name. All sixteen configurations
remain in final-Test accounting, including regressions; seed 17 is the
representative artifact, and all three seeds are evaluated.

The immutable v3 receipt binds numeric contracts, fitted normalization/vocabulary,
checkpoints, numeric weights, independent reference encodings, Dev/CAL
predictions/reports, sampling protocols and the retained CAL-only Pangram
isotonic mapping. Consumers revalidate those files before applying frozen
transforms or comparators. `freeze_final_selection --audit-existing COHORT
TRAIN_SCALE NEW_AUDIT` checks the twelve already completed configurations while
source-size work is pending. That diagnostic cannot authorize Test access.
Neither command downloads Test sources, calls a detector, trains a model or
qualifies serving. The final-Test source/descendant isolation audit and complete
requested-population accounting remain required.

`audit_final_test_overlap PLAN.json NEW_OUTPUT` constructs a cumulative overlap
ledger from explicitly pinned original populations, every Train extension,
reference populations, admission/exclusion records and historical V6 metadata.
It also rechecks every frozen candidate's numeric population. Before acquisition,
it rejects reserved-work ID, family, normalized title/edition-alias and original
scratch-prompt conflicts. The ledger retains exact and whitespace-normalized
text hashes for later checks; historical Test prose is not reopened to fill
missing hashes. Input JSON pointers select only prior populations, excluding
the intended current Test reservations. Pin all such projections explicitly:
the tool cannot discover an omitted external corpus or unknown work alias.
`audit_final_test_overlap --check-excerpts SELECTION LEDGER QUERIES_DIRECTORY
NEW_OUTPUT` checks extracted queries and separate references against that ledger
before generation. Conflicts retain evidence and stop; neither route repairs a
cross-split conflict by silently removing only its Test copy.

The root example `a_acquire_balanced_sources TSV METADATA_ROOT NEW_OUTPUT
--final-test SELECTION` adds a separate receipt-gated route for the exact reserved
sixteen Test query works and two Test reference works. Its default route still
excludes Test. Both retain official catalog rights evidence, HTTP exchanges,
exact downloaded bytes and immutable resume bindings. No download runs from tests.

`assemble_balanced_final_test PLAN.json NEW_OUTPUT` validates the exact reserved
works, Qwen/Phi/Mistral generation evidence, replacement spans and cumulative
overlap ledger. It preserves the 416 requested-query denominator and records
extraction, generation and within-family duplicate exclusions. Four requested
withheld-writer reference excerpts remain separate. Its pinned-input plan is
documented in [the assembly interface](examples/assemble_balanced_final_test/README.md).

`transform_final_cfg SELECTION CANDIDATE RECORDS MANIFEST CFG_EXPORT NEW_OUTPUT`
applies the frozen `cfg_all`, `cfg_leaf`, `ordered_cfg` or `global_cfg_control`
contract to Test queries. Ordered/global projections reuse retained windows;
no vocabulary, normalization or profile fitting occurs. Parent files remain
unchanged, and an all-failed extraction still produces population accounting.

`prepare_text_requests RECORDS MANIFEST NEW_OUTPUT --final-test SELECTION`
validates the frozen selection and exact Test-only query roster, then exports
only routing IDs and unchanged text for the frozen V6 encoder. Labels, provenance
and reference prose stay outside those requests. Its default route still rejects
Test. This export requires no CFG success and fits no preprocessing statistics.

`prepare_frozen_text_evaluation SELECTION CANDIDATE RECORDS MANIFEST CFG_BUNDLE
EMBEDDINGS NEW_OUTPUT` joins Test queries for the frozen warm and MLP text/combined
candidates. It preserves parent files and contracts, applies frozen normalization
only where required, keeps warm raw activations intact, and retains unsupported
inputs and upstream failures. It neither trains nor handles reference-gallery
construction. The retained Test dataset manifest and numeric population audit
are required handoff artifacts.
Text-only support depends on V6 success; its CFG block is a fixed architectural
zero, and parser failures remain diagnostics. Combined support requires both
feature sources. Report each model's support and the common paired population.

`predict_final_test NUMERIC FIT SELECTION NEW_OUTPUT` runs the frozen Rust
numeric model and publishes a separate prediction array and receipt, preserving
inference failures. `evaluate_supervised --frozen-test NUMERIC FIT
PREDICTION_RECEIPT SELECTION NEW_REPORT [TEST_GALLERY]` reuses the existing
origin, share, conditional-attribution and family-bootstrap metrics. Neither
command writes into fitted artifacts or selects thresholds on Test. The optional
gallery and full accounting contract are in [the prediction interface](examples/predict_final_test/README.md).

`prepare_final_test_references select|project|encode` provides the separate
gallery path: four Test-withheld excerpts are admitted and transformed under
frozen feature contracts, then encoded alongside twelve unchanged Train
reference excerpts by each model's shared encoder. The existing mean-pooling
and cosine-matching contract remains diagnostic and does not expand identity
heads. [Reference commands and contracts](examples/prepare_final_test_references/README.md)
retain all unavailable reference inputs explicitly.

`summarize_final_test` validates the complete sixteen-candidate, three-seed
roster and emits an aggregate-only report from frozen predictions. It reports
all heads, share bins, reference retrieval, fixed matched feature comparisons
and family-bootstrap involvement gaps without reselection. The [plan format](examples/summarize_final_test/README.md)
pins every input; [the final evaluation](../../../docs/a-final-test.md) records
the measured quality and remaining limits.

The root Pangram runner accepts `prepare test INPUT MANIFEST test OUTPUT KEYFILE
MODEL --reuse-from DEVELOPMENT_OR_CALIBRATION_ARCHIVE` for an offline exact-text
reuse audit before a paid Test batch. It retains original observations and
reports the new Test destination explicitly; historical Test archives remain
excluded from scanning. Omitting `prepare` uses the existing bounded, journaled
API path. Detector percentages still require the frozen Calibration mapping for
involvement comparisons and are never fractional-share labels.

The separate comparator routes are:

```text
compare_supervised_pangram --frozen-test NUMERIC FIT PREDICTION_RECEIPT TEST_RESULTS SELECTION NEW_REPORT
evaluate_frozen_v6 --frozen-test NUMERIC TEST_EMBEDDINGS FROZEN_EMBEDDING_MANIFEST SELECTION NEW_REPORT
```

The Pangram route applies the frozen CAL isotonic mapping without reading CAL
rows or fitting it again. V6 remains origin-only and preserves its 2,048-token,
no-truncation support boundary. Both validate exact input/prediction bindings
and report requested, supported and failed populations separately. Prediction
receipt fields are documented in
[`test_binding.rs`](examples/compare_supervised_pangram/test_binding.rs); write
evaluation outputs outside frozen fit directories.

Keep corpus additions as versioned shards. Preserve original sources, rights,
production histories, exact replacement spans, realized final-word shares,
generator/model/runtime digests, every attempted request/response and terminal
failure. Keep every paid detector observation with its exact input hash and
response provenance; reuse it for matching input/model observations. Retention
does not automatically admit a record to training. Restricted text, fitted
profiles and invocation logs stay in ignored storage; publish only audited,
authorized corpus projections with fixed splits and checksums in `datasets/`.
Existing snapshots and model artifacts remain immutable.

`select_train_augmentation` selects unchanged historical human roots from the
original V6 Train shard under its pinned source/rights/date/length protocol.
It checks protected source families, title/known-author metadata, and current
non-Test text hashes without opening Test prose. Historical roots remain
qualified proxies; unidentified writers stay masked. A collection shortfall is
reported rather than replaced from another collection. The separate root-CLI
`a_balanced_generate ... --recipe train_scale_v1` requires Train-only generator
configs and records scratch generation plus requested 10/40/80/100% rewrites.
Requested percentages are never substituted for realized span labels.

`admit_train_expansion ADMISSION_PLAN.json NEW_OUT` admits a fixed allow-list
of additional original V6 Train roots. It binds cumulative populations and
metadata reservations, including candidate books, before checking exact and
whitespace-normalized duplicates. Every request receives an admission or
exclusion; a completed audit does not mean all requests were accepted.

`prepare_train_books` adds independent works for already established writers.
Its `plan AUDIT_ROOT ALIAS_REVIEW RETAINED_PLAN NEW_OUT` command freezes a
Train-only acquisition roster after bibliographic alias and withheld-writer
checks. Run the root workspace's `a_acquire_balanced_sources` with the emitted
TSV, then `prepare_author_queries --fresh-balanced` with that acquisition
manifest. Finally run `prepare_train_books admit PLAN_DIR ACQUISITION_DIR
EXCERPT_DIR NEW_OUT`. It admits only the fixed 20%-position excerpt, retaining
exact byte spans and source notices, and rejects out-of-bound or duplicate text
without selecting a replacement passage. Book roots have a distinct versioned
manifest; they are not implicitly compatible with the historical-record
generation/assembly recipe. Tokenizer qualification remains a separate gate.

The two assembly routes consume completed, pinned generation evidence:

| Roots | Assembler | Generation recipe per root/model |
| --- | --- | --- |
| Original historical-record selection or retained admission | `assemble_train_augmentation PLAN.json NEW_OUT` | `--recipe train_scale_v1`: scratch + 10/40/80/100% rewrites |
| Independent book admission | `assemble_book_augmentation PLAN.json NEW_OUT` | Default v2 narrative recipe, no recipe flag: scratch + 20/60/100% rewrites |

Both offer `--verify-roots PLAN.json NEW_RECEIPT` before generation completes.
The retained route verifies the admission requests, selection, exclusions and
original-record ancestry; unidentified historical writers remain masked. The
book route verifies the acquisition/alias chain and exact deterministic PG
excerpt spans, retaining established writer identities. Its `--plan ROOTS
RETAINED_ROOTS CONFIG1 CONFIG2 PROPS1 PROPS2 NEW_PLAN.json` command also pins the
prepared generation protocol, binary, launcher and server properties. Its current
contract requests 29 humans plus 232 generation tasks, or 261 records; the 32
source candidates and three metadata exclusions remain separate admission
accounting. These counts describe admission and requested work, not fitted results.

Both assemblers validate matched recipes, source/config hashes, request intents,
responses, terminal failures and realized share. Incomplete runs block assembly.
Duplicate roots, IDs or protected families fail; generated text duplicates are
excluded with all evidence retained. Outputs use
`slop_ninja_a_supervised_dataset_v1` with `train_extension_only`, a bound
`source_manifest_sha256`, and reconciled requested/accepted/failed/excluded counts.
Neither assembler invokes a model or opens Test prose.

Count independent source families as well as rows. Several rewrites or excerpts
of one work remain one family, entirely within one split. Dataset size alone
does not establish generalization: retain genre/generator balance, learning
curves, repeated seeds and family-aware uncertainty. Development guides model
selection; the final Test must remain untouched during data or model tuning.

After audited Train-only assembly and frozen CFG/V6 extraction:

```sh
"$bins/transform_train_extension" "$c/numeric-cfg-v1/leaf_grammar" \
  "$scale/export-v1" "$scale/dataset-v1/manifest.json" "$scale/cfg-fragment-v1"
"$bins/prepare_warm_start" --extend-train "$c/numeric-warm-v1" \
  "$scale/cfg-fragment-v1" "$scale/embeddings-v1" "$scale/dataset-v1/manifest.json" \
  "$scale/GENERATION_PROTOCOL.md" "$scale/FIT_PROTOCOL.json" \
  "$scale/numeric-warm-v1"
```

The fragment preserves the frozen CFG vocabulary, normalization and profile
roster. Warm-bundle assembly checks matching text hashes and V6 representation,
requires a common complete feature population, and appends disjoint Train
queries. It creates new contract hashes while preserving all original numeric
values, targets and baseline predictions. The trainer independently compares
the enlarged bundle against its frozen parent before qualification or fitting.
It also binds the complete admitted roster and exclusions to the selected-root
manifest and frozen generation protocol; a different or reduced fragment cannot
stand in for that roster. Admission snapshots accompany the new numeric bundle.
Development, CAL and reference populations cannot be enlarged by this command.
Use the same qualification/training commands above with new output paths;
learning curves include Train and Development involvement Brier and Train loss.
More training examples do not establish cleaner evaluation or undo V6 exposure.

`merge_train_extensions PINNED_PLAN.json NEW_OUT` combines completed Train
datasets with their cached CFG fragments and raw V6 activations. Its ordered
shard plan pins each manifest and the protected non-Test populations. It checks
identical CFG contracts/fitted parents and V6 representation snapshots, exact
row rosters and source hashes, and reserved Test IDs. Earlier shards have
precedence; within a shard human roots precede generated copies. A duplicate
human root or reused family fails; later normalized generated duplicates become
explicit exclusions. Dataset, CFG audit/features and activations retain the
same selected IDs. Original requested tasks, terminal failures, exclusions and
extraction-cost receipts survive the merge.

The new output contains `dataset/`, `cfg/`, `embeddings/` and a bound
`source-roots.json`. Pass those three bundles to `prepare_warm_start
--extend-train` with a separately frozen generation/fit protocol. Existing
qualified rows are copied without reparsing, encoder inference, vocabulary or
normalization fitting. Requalify initial equivalence on the complete enlarged
numeric bundle before any fit; cached merging alone is not model qualification.

`prepare_source_size_curve NUMERIC_BUNDLE EXTENSION_DATASET_MANIFEST DESIGN
INIT NEW_SELECTIONS` prepares the frozen source-size comparison. It joins each
numeric extension ID and family to the admitted dataset and requires matching
text/combined supervision. Five added source collections form the strata:
historical books, historical Wikipedia, PLOS ONE, Wikinews and Project Gutenberg
novels. The selector emits `12.json`, `24.json` and `all.json`, selecting 12 or 24
families per stratum, or all admitted families. Selection sorts the SHA-256 of
`slop_ninja_a_source_size_curve_v1`, NUL, and the source-group key, with the key
as tie-breaker. Levels are nested and retain every admitted descendant of each
chosen family. A stratum shortfall blocks selection without substitution.

All original Train queries remain selected with total loss mass 0.5. Each added
stratum has mass 0.1; within each stratum, weight is equal across families, then
observed origin classes within a family, then rows in that family/origin cell.
`warm_start_a.py train --sampling-protocol SELECTION.json` independently checks
the numeric/parent/dataset/initializer bindings, selection and weights, then
removes unselected rows from the optimizer batch. It preserves the exact sampling
protocol and effective task masses after supervision masking. The option is
joint-only and cannot combine with `--mixture-protocol`. Original normalization,
profiles, Development/CAL weights and reference galleries remain frozen.

The frozen design permits text-only fits at all three levels and combined fits
only at `all`, each with seeds 17/29/41 and the same 250-step residual recipe:
12 planned fits. Development involvement Brier selects checkpoints; CAL alone
fits temperature and named thresholds. Initialization equivalence still covers
the entire available numeric population before optimizer updates. Test remains
closed. This workflow makes no claim that the planned full pool has been fitted
or improves generalization.

`summarize_source_size_curve LARGE_ROOT ORIGINAL_ROOT NEW_REPORT.json` requires
all 12 fits, their sampling receipts, Dev/CAL evaluations and Rust replays. It
compares the nested sizes directly under the fixed stratum mixture, compares
full-size text with added CFG, and retains the selected original text, frozen
V6 and archived Pangram controls. It reports all heads, per-seed metrics, paired
source-family intervals and recorded Train/Dev curves. Document counts and
balanced origin cells are separate; confidence-interval endpoints are never
averaged across seeds. The origin-loss weight-concentration diagnostic
`1 / sum(family_mass²)` is distinct from raw family count and is not a guarantee
of independent observations or absence of overfitting. The output excludes
private IDs, profiles and paths. The command performs no fitting or live calls.

```sh
"$bins/prepare_source_size_curve" "$numeric_bundle" "$extension_manifest" \
  "$design_protocol" "$initializer" "$new_selections"
"$train_python" "$py/warm_start_a.py" train --data "$numeric_bundle/text" \
  --init "$initializer" --equivalence "$equivalence_receipt" \
  --sampling-protocol "$new_selections/12.json" --seed 17 --device cuda \
  --out "$new_fit"
```

## Frozen input contract

`Contract` binds parser, resolution, projection, inventory, segmentation,
whole-document aggregation, source manifest, ordered coordinate catalogs,
dimensions, POS catalog, normalization, row policy and explicit source-group
split assignments. Contract identity hashes its deterministic serde JSON.
Exporters must freeze these choices before Development/Calibration/Test use.

`StableId::leaf(persistent_lexical_semantic_id, morphology_semantic_id)` hashes
the fixed `LEAF_SEMANTIC_NAMESPACE`, not an inventory snapshot. Adding unrelated
words/rules changes the contract/inventory receipt but not existing leaf IDs.
Changing a leaf's semantics requires a new persistent semantic ID. These IDs
are categorical keys: their digits never enter the scalar feature vector.
POS zero is unknown; other integer ordinals name the frozen POS catalog.
Bulk store handles/offsets must resolve through persistent semantic bindings;
without that mapping export an unresolved/unavailable outcome. Chart node IDs,
Merkle paths and proof metadata stay in the separate source-bound audit receipt.
Hashes do not make lexical information anonymous.

Each `NumericInput` has paired lexical and grammar blocks containing values,
opportunity denominators and explicit availability bits. Unavailable values
must carry zero payload/denominator. An available measured zero retains a
positive denominator. `PerOpportunity` normalization divides in Rust; `Counts`
uses supplied nonnegative measurements. OOV, unresolved and unknown-POS token
counts, POS availability, lexical completeness and covered/total grammar units
remain explicit. OOV and unresolved leaves are disjoint. Do not choose the first
parse to fill a missing value or silently truncate unparsed document regions.

Contract/model layout V2 adds `fallback_tokens` and `fallback_by_pos` in fixed
POS catalog order (unknown at zero). Generic fallback POS is separate from a
dictionary leaf with the same POS; never hash an OOV spelling into a fabricated
dictionary identity. Checked per-POS counts sum to the fallback total, bounded
by observed tokens and consistent with unknown/known POS counts. Fallback may
overlap OOV but need not be a subset of it. These are generic grammatical
fallbacks, not reviewed dictionary senses. Inventory snapshots and node/Merkle
paths still do not enter persistent dictionary leaf identity.

The derived lexical vector is scaled values, availability bits, three token
fractions (OOV/unresolved/unknown POS), token-denominator availability, POS
availability and lexical completeness. The grammar vector is scaled values,
availability bits, coverage fraction and coverage-denominator availability.
The combined vector is their exact concatenation. RMS scales are fitted once
on available Train observations with source-family weights; masks and fractions
are unscaled. The floor is frozen in `TrainConfig`. No held-out vocabulary,
scale or availability-dependent feature selection is fitted.
V2 appends the total fallback fraction and one fraction per frozen POS after
the six existing lexical status channels; the token-denominator availability
bit also covers these fractions. Lexical width is `2*L + 7 + POS_count`.

`CompletePairedDocuments` is the default synthetic fixture policy: all heads use
the same complete-document intersection. `MaskedPairedDocuments` retains partial
rows in all heads and reports incomplete/unavailable counts. Neither policy
turns a partial parse into complete document evidence. A masked fit without any
measured grammar observations in Train is unavailable. Grammar evidence is
explicitly `native_cfg`, `external_linguistic`, or `synthetic`; this crate checks
numeric bindings, not the truth of an exporter's claimed parse provenance.
Generic linguistic statistics must use `external_linguistic`.

## Export boundary and leakage checks

The coordinator/exporter supplies three JSON inputs:

- `contract.json`: serialized `Contract` (all fields required).
- `rows.jsonl`: serialized `NumericRow` records. Training metadata and labels
  are outside `NumericInput`; source/group/text/receipt identities are hashes.
- `audit.json`: `ExportAudit`, binding the contract, `rows_identity`, exporter,
  source manifest and all fitting/exposure groups. The latter includes groups
  used for grammar/rule development and parse-selection tuning, not just model
  optimization; all declared exposure groups must be assigned Train.

Hashes are assertions from the preprocessing boundary. This crate cannot
recompute a text hash or discover undeclared exposure without source text.
The exporter must bind exact bytes and a fixed whitespace-normalization policy
in its source manifest. It must supply complete root-to-parent ancestor IDs;
roots have empty ancestors and name themselves. Known ancestry must reproduce
those ordered prefixes. Source groups, lineage IDs, exact hashes and normalized
hashes cannot cross splits. Duplicate row IDs are rejected. Exact duplicate
texts within one split are retained as dependent observations, not independent
confirmation. Fixed assignments are checked rather than regenerated.

Train takes only Train/Development/Calibration rows, never Test. Excluded and
unlabelled rows also remain in the fit leakage ledger. Each retained source
family receives equal total loss weight; its rows divide that weight equally.
This is family weighting, not equal family-origin weighting. Origin order is
`human_only`, `model_only`, `mixed`. Human-proxy labels do not certify a known
human-only workflow, and source-conditioned generation is not from-scratch data.

## Executable comparison

```sh
slop_ninja_a_baseline contract-id --contract contract.json
slop_ninja_a_baseline validate --contract contract.json --rows rows.jsonl --audit audit.json
slop_ninja_a_baseline train --contract contract.json --rows rows.jsonl --audit audit.json --out fit
slop_ninja_a_baseline predict --artifact fit/artifact.json --request numeric-request.json --out predictions.json
slop_ninja_a_baseline evaluate --artifact fit/artifact.json --rows test.jsonl --audit test-audit.json --out evaluation.json
```

Optional `train --config config.json` accepts a complete serialized `TrainConfig`.
Defaults: full-batch deterministic float64 Adam, 200 epochs, patience 25,
learning rate 0.03, L2 0.01, RMS floor 0.01, two groups per split and at least
one row of each class per split. No randomness or seed selection is involved.
The equations follow the existing Rust origin softmax control, adapted here
without taking its prose/features runner dependency.

Every head starts from the same family-weighted class prior and selects the
earliest Development weighted-log-loss minimum (improvement tolerance 1e-9,
including epoch zero). Calibration fits only a positive temperature over the
fixed grid `exp(k * 0.1)`, k=-40..40, with temperature one retained as a candidate.
No architecture is selected: lexical, grammar and combined are all retained.
Artifacts bind implementation, contract, export audit, complete/eligible rows,
scales, recipe and fit ledger. Test evaluation freezes all parameters and checks
the complete fit ledger for group, duplicate and ancestry leakage.

The numeric prediction request is
`{"contract_sha256":"<hash>","inputs":[<NumericInput>, ...]}`. The CLI rejects a
different contract. Predictions contain all three heads in fixed order; a
complete-pair model returns `unavailable` for an incomplete input. Optional
`abstain_below` is fixed in the recipe before fitting. Reports keep all eligible
rows in primary loss/Brier/accuracy denominators and separately report accepted
fraction/selective accuracy. Exact probability ties use the fixed class order.
No population false-positive guarantee, calibrated document authorship proof,
fidelity assessment, semantic review or grammar admission follows from a score.

Outputs must be new; incomplete files are never silently overwritten. An
unavailable fit saves `outcome.json` and exits nonzero without an artifact.
Use ignored task-owned `data/` for real exports/models. Dense starter fitting
uses memory proportional to rows times included dimensions; qualify dimensions
and resource limits before increasing corpus size. This crate has no native
corpus exporter or automatic compatibility route around unresolved derivations.
Existing `grammar-native-vector` validated counts/opportunities are usable input
building blocks, not an already-qualified full-document training dataset. See
the shared report for the distinct real-data preparation/training/inference commands.

## Public A recipe tools

The [public training recipe](../../../docs/testnet/training-guide.md) uses
four examples of this crate. `public_a_bundle` builds a trainer bundle from
the published dataset snapshots named in
`grammar-eval/python/public_a/recipe.json` (rule
`slop_ninja_a_public_bundle_v1`; Test and reference files are never opened).
`prepare_full_a_contract --roster FILE` writes a contract whose AI roster is
the fixed list in FILE rather than the Train generator labels; every
positive-weight Train label must be in it. `fit_full_a_normalization` and
`calibrate_full_a` run unchanged.

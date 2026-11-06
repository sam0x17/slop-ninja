# Reference detector

V6 is the latest experimental binary detector. Download the
weights and CPU inference bundle (release `detector-revision-v0.6.0`, not yet in the public repository)
and read the [complete results](releases/v0.6.0/RESULTS.md),
[usage instructions](releases/v0.6.0/README.md) and
[component notices](releases/v0.6.0/NOTICE.md).
Its declared primary binary log loss improves from 0.2653 to 0.1007 after
two model revisions, with a paired 95% difference interval of
[-0.2229, -0.1075]. Across all 117 assigned human roots, stricter-cutoff
false positives fall from 7 to 3; looser-cutoff false positives rise from 9
to 12. Mixed-origin classification and several source/profile slices remain
weak. V5 remains available for these sensitivity and three-class comparisons.

All fourteen evaluations and seven full-cohort paired comparisons are in the
[aggregate evidence](results/encoder-revision-v6.json), with its
[assembly verification](results/encoder-revision-v6-verification.json).
The [fresh Pangram comparison](results/pangram-revision-v6-comparison.json)
covers 18 human, 108 model-only and 36 mixed-origin texts from 18 families.
Pangram detects 108/108 model-only and 34/36 mixed-origin texts, compared with
98/108 and 29/36 for V6 at its stricter cutoff and 102/108 and 33/36 at its
looser cutoff. Observed human flags are 0/18 for Pangram and V6 strict and
2/18 for V6 loose. The [six paired subset comparisons](results/pangram-revision-v6-subsets.json)
retain the uncertainty from this small cohort. The artifact remains an
unqualified research candidate; Pangram is still the external quality anchor.

Current work focuses on [A's robustness](A_ROBUSTNESS.md). Full-document B gains
transferred weakly to Pangram. The coverage audit found only three generating
model families in V6 Train, no held-out-style evaluation, and a 1,050-token
maximum training length despite the 2,048-token input limit. V6 remains the
frozen comparison artifact while these gaps are investigated.

## Earlier experiments

The first trained candidate is a ModernBERT origin classifier. Read the
[pilot results](PILOT_RESULTS.md) and download the
experimental release (`detector-pilot-v0.1.0`, not yet in the public repository).
It passed inference checks after transfer from the Studio, but has substantial
generator-transfer and formatting weaknesses. The release includes the selected
encoder, all three linear controls and all 16 aggregate evaluations.
The [style-diverse pilot](STYLE_PILOT_RESULTS.md) added fresh sources, two teachers,
varied instructions and matched whitespace exposure. Its candidate improved
pooled log loss but never chose the mixed class on any test view. It is available
as a separate experimental release, with all comparisons and exclusions reported.
The [word/grammar development probe](LINEAR_STYLE_RESULTS.md) recovered mixed
predictions with smaller optimization steps and independent coordinate budgets.
The [standard-corpus plan](STANDARD_CORPUS.md) describes admitted HC3 sources and
the reusable Pangram annotation workflow. The first [Pangram comparison](PANGRAM_CORPUS_RESULTS.md)
flagged all 38 model/mixed Development texts, compared with 20 for the encoder.
Three passes had identical document fractions; auxiliary window scores varied.
The [next corpus expansion](CORPUS_EXPANSION_V4.md) adds historical encyclopedia
sources, prepares narrative summaries and introduces OLMo as a third generator.
The [v4 encoder experiment](ENCODER_EXPANSION_V4.md) specifies balanced source
weighting and two learning rates, with Development selection fixed before fitting.
Its [completed comparison](ENCODER_EXPANSION_RESULTS.md) improved fresh Test
accuracy from 52.9% to 78.4% and recovered mixed-class predictions. It remains
experimental and still trails Pangram on the previously scored Development texts.
Download the v4 weights and inference bundle (release `detector-corpus-v0.4.0`, not yet in the public repository).

The [v5 narrative experiment](ENCODER_NARRATIVE_V5.md) adds 289 admitted historical
plot summaries and reserves Phi-4 for an unseen-generator Test. Its
[completed comparison](ENCODER_NARRATIVE_RESULTS.md) reduced primary-Test binary
log loss from 0.6970 to 0.1430 and human false positives from 5/25 to 1/25, while
detecting all 25 model-written drafts. Phi-4 log loss also improved, but detection
fell from 21/21 to 19/21 at the stricter threshold. The missed drafts used Anti-AI
and Fix Slop instructions. V5 is the next experimental binary detector; v4 remains
available for the reported sensitivity comparison.
Download the v5 weights and CPU inference bundle (release `detector-narrative-v0.5.0`, not yet in the public repository).
The [v5 Pangram follow-up](PANGRAM_V5_RESULTS.md) uses the same 57 previously
annotated Development texts: v5 detects 32/38 model-involved texts at its stricter
threshold and 34/38 at its looser threshold, with 1/19 human false positives.
Pangram detects 38/38 with 0/19 human flags. The [annotation expansion](PANGRAM_CORPUS_EXPANSION.md)
prepared 2,925 Train texts. The [revised fixed-budget collection](PANGRAM_DATASET_V2.md)
uses small, varied cohorts and targets coverage across 60-100% detected content.
The [generator-attribution plan](GENERATOR_ATTRIBUTION.md) adds GPT/Claude CLI
captures and preserves original generators, revisers, and prompt profiles as
separate labels. Raw captures remain separate from admitted training records.
The [96-text matched comparison](GENERATOR_ATTRIBUTION.md#pangram-annotation-of-hosted-captures)
found 85/96 Pangram flags under the existing 10% content rule and 72/96 v5 flags
at its stricter threshold, including seven that Pangram misses. On Claude Opus,
the counts were 20/24 and 11/24. All source families occur in Train, so this is
a diagnostic rather than a fresh benchmark. The batch adds 19 examples between
60% and 100% exclusive, plus 22 below 60%.
An early [Train-only overlap audit](NARRATIVE_GENERATION_QA.md) found substantial
rewriting in Qwen's requested copyedits. The [clarified main target](ENCODER_NARRATIVE_V5_TARGET.md)
is fully model-written prose deliberately steered away from the model's usual
voice. Selection now prioritizes human-versus-model-draft detection, with explicit
writing-profile slices. Selection was frozen before either Test opened.
The [narrative corpus report](NARRATIVE_CORPUS_RESULTS.md) records completed
provider cohorts and all exclusions.
Its first Development comparison found that v4 flags nine of 26 historical human
narrative sources, despite detecting all 26 model-written drafts. V5 reduces
this register's observed false positives, but these small historical samples
cannot establish a population false-positive bound or Pangram parity.

The [model-only revision probe](MODEL_ONLY_REVISION_PROBE.md) records strong
DeepSeek style steering, a Fix Slop pass and a DeepSeek-to-Qwen rewrite. V4
detected all four synthetic passages, but the rewrites introduced meaning or
tone problems. Both same-model and cross-model revisions retain model-only
lineage; these examples remain outside the frozen v5 experiment.
The [revision-chain generator](MODEL_REVISIONS.md) now records this workflow
directly, retaining every ancestor and a model-only label through successive
rewrites by the same or different models.
The [next revision collection](ENCODER_REVISION_DATA_V6.md) declares Qwen and
Mistral passes over 471 existing Train drafts. A separate
[source review](SOURCE_REVIEW_V6.md) admitted 351 previously unused works.
The [frozen v6 comparison](ENCODER_REVISION_V6.md) reserves 117 each for
Development, Calibration and Test, with no fresh sources used for gradients.
It compares drafts and two successive revisions on the same completed families,
plus a separate check on all assigned human Test roots. The evaluator supports
[explicit family views](ml/README.md) over complete revision archives, so
intermediate texts do not become extra calibration observations. Generation has
closed, and the two-rate fit completed on the Studio with 3,833 Train records.
The [evaluation handoff](ENCODER_REVISION_V6_EXECUTION.md) checks the completed
frontier, freezes thresholds and runs the declared comparisons. The
[completed v6 report](releases/v0.6.0/RESULTS.md) records all stages and the
all-human diagnostic; Test results did not trigger another fit or model switch.
The separate [fresh Pangram comparison](PANGRAM_REVISION_V6.md) samples paired
primary/Phi-4 Test families without scores, balancing source collection and
original composer within its reserved allocation. It preserves both routes' drafts,
revisions and human edits, with shared human roots counted once for billing.
The [prepared sample](results/pangram-revision-v6-preparation.json) contains 162
texts from 18 of the 103 families completed by both routes. Each collection
(PLOS, Wikinews and CMU summaries) contributes six families, and each primary
composer (Mistral, OLMo and Qwen) contributes six; every collection/composer
combination contributes two. Both routes cover all six initial writing profiles.
The batch estimate and its conservative reservation are recorded in the private
ledger. A third balanced round would have exceeded the reserved allocation, so
the sampler stopped after two. All 162 requests succeeded after the selected artifact and thresholds
were frozen. The [collection report](results/pangram-revision-v6-collection.json)
records the limited score diversity: 106/108 model-only texts score in the 100%
bin, with only eight nonhuman texts between 60% and 100% exclusive.
The [reporting tools](PANGRAM_REVISION_V6_REPORTING.md) reuse saved Test
predictions, recompute subset metrics and join the completed Pangram annotations.
They retain failed requests and numerical differences between repeated exports.
The [release attribution extractor](V6_RELEASE_PREPARATION.md) produced credits
for 4,903 fitting, Development and Calibration records from 685 source families,
including complete ancestry. These credits accompany the immutable encoder.

The [scientific register diagnostic](SCIENTIFIC_REGISTER_V6_RESULTS.md) probes
the same frozen V6 with six new Qwen/Mistral drafts and three human controls.
All four admitted drafts exceeded its strict threshold; the lower-scoring maize
draft was excluded for source copying. Every draft failed strict fidelity.
This small cohort did not reproduce the near-zero scores observed on private
B outputs, so A robustness and Pangram alignment remain open work.

The first reference candidate estimates three document-origin classes:
`human_only`, `model_only`, and `mixed`. Rust handles corpus admission,
provenance, splitting, word/grammar features, linear models and evaluation.
The optional [ModernBERT path](ml/README.md) uses PyTorch for encoder training
and inference. Neither path infers a percentage of AI-written words.

Read the [implementation plan](PLAN.md), [source register](SOURCES.md) and
[frozen pilot protocol](PILOT_PROTOCOL.md) for the scope and evidence limits.
The initial corpus uses licensed historical human proxies, recorded
source-conditioned model drafts and model copyedits of those proxies. It does
not contain verified contemporary human/model collaborations.

## Build and collect

Run these commands from this directory:

```sh
cargo build --release
cargo run --release --bin slop_ninja_detector -- acquire \
  --output-dir ../../data/baseline-detector/acquisition-v1 --plos 80 --wikinews 80
cargo run --release --bin slop_ninja_detector -- split \
  --input ../../data/baseline-detector/acquisition-v1/human-records.jsonl \
  --output ../../data/baseline-detector/acquisition-v1/frozen-roots.jsonl \
  --seed slop-ninja-origin-pilot-v1
```

Acquisition retains raw source and license captures, extraction decisions and
rejections. Records bind exact text hashes, source versions, attribution and
origin evidence. The splitter refuses to reshuffle assigned partitions. An
original and all of its generated descendants belong to one source family.
Identical and normalized duplicates are joined before splitting.

## Generate recorded examples

For new corpora, use the [versioned prompt profiles](PROMPT_PROFILES.md) with
`--prompt-profile-set style-mix-v1`, fresh frozen input and a new output directory.
The mix includes varied registers, explicit anti-AI phrasing instructions and an
adapted Fix Slop snapshot. The command below retains the original pilot prompt
pair for reproduction; omitting the profile option preserves its cache IDs.

`generate_samples` calls an explicitly configured local model endpoint. Its
model-spec JSON binds the model ID, immutable revision/file hash, Apache-2.0
license evidence, quantization, runtime, temperature and token limit. See the
struct in `src/bin/generate_samples.rs` for the exact fields. Verify the loaded
weights against that spec before using an export.
The pilot's Mistral holdout was served by a pinned MLX adapter that applied a
tokenizer regex correction; `manifests/pilot-generation-v1.json` binds its
hash, and its source remains in Git history.

```sh
cargo run --release --bin generate_samples -- \
  --input ../../data/baseline-detector/acquisition-v1/frozen-roots.jsonl \
  --model-spec ../../data/baseline-detector/generators/qwen-pilot.json \
  --output-dir ../../data/baseline-detector/qwen-pilot \
  --base-url http://127.0.0.1:18123/v1 --concurrency 4 --max-calls 320 \
  --complete-families-only
```

Requests and responses stay in ignored `data/`. Calls are capped, failed attempts
are retained, and completed responses are reused only after validation against
the request and model spec. The optional `LM_STUDIO_API_KEY` is read from the
environment and never written into the request archive. Use an SSH tunnel for a
remote Studio endpoint. Full-family export requires all tasks to have been
attempted and reports every family excluded because an output failed admission.
Before any new request, the generator requires a reachable endpoint with a
nonempty model catalog. A failed readiness check leaves the run resumable and
does not count every source as a failed model output.

To pause generation, create `PAUSE` in that run's output directory:

```sh
touch ../../data/baseline-detector/qwen-pilot/PAUSE
```

Workers stop taking new tasks when they observe the file. Requests already in
flight finish and checkpoint before the process exits. An incomplete paused run
has `status: "paused"` and `complete_export_ready: false` in `summary.json`; it
writes no cohort export, including when partial-export flags were supplied.
A clean exit means the drain succeeded, not that the cohort is complete. Keep
the file in place until the process exits, then remove it and rerun the same
generation command to resume from validated cached responses:

```sh
rm ../../data/baseline-detector/qwen-pilot/PAUSE
```

The pause file also blocks new invocations until removed. Pausing does not change
prompts, task identities or admission rules.

## Train and evaluate the Rust controls

Install the repository's pinned spaCy environment first. From this directory,
the existing environment is `../../.venv/bin/python`.

```sh
cargo run --release --bin slop_ninja_detector -- featurize \
  --input ../../data/baseline-detector/qwen-pilot/records.jsonl \
  --output ../../data/baseline-detector/qwen-pilot/features.jsonl \
  --mode combined --python ../../.venv/bin/python
cargo run --release --bin slop_ninja_detector -- train \
  --features ../../data/baseline-detector/qwen-pilot/features.jsonl \
  --mode combined --max-coordinates 8192 \
  --output-dir ../../data/baseline-detector/runs/linear-combined-v1
```

Use `--mode word` and `--mode grammar` with the same feature file for the two
controls. Vocabulary/scaling fit on training data; development log loss selects
the checkpoint; calibration data fit the temperature and operating thresholds.
Test feature values never enter the fitting function. Open a frozen test only
after candidate selection:

```sh
cargo run --release --bin slop_ninja_detector -- evaluate \
  --features ../../data/baseline-detector/qwen-pilot/features.jsonl \
  --artifact ../../data/baseline-detector/runs/linear-combined-v1/model.json \
  --output ../../data/baseline-detector/runs/linear-combined-v1-test.json \
  --open-final-test
```

Check `--help` for inference, shard export and audit commands. The runner rejects
unsupported lengths and schema/parser mismatches instead of truncating text or
changing preprocessing. Artifacts bind class order, preprocessing, source code,
dependencies and numerical rules. Later source edits may require rebuilding an
artifact under a new identity.

Corpus text, invocation logs, fitted checkpoints and run caches stay in `data/`.
Publish a reference candidate only with its model manifest, provenance, exact
runtime and measured limitations. Low observed false-positive counts in this
small pilot cannot establish a reliable 1% or 5% population bound.

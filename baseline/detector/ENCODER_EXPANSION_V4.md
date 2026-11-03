# Encoder expansion experiment v4

Declared before fitting or opening new detector predictions. This experiment
tests a larger corpus with three generator families and an additional prose
register. It changes the data mixture and training objective together; it cannot
isolate the causal contribution of each change. It makes no Pangram parity claim.

## Corpus assembly

Use the frozen v2 primary corpus, SHA256
`6377d431b570a53cb9e5cfa97adb28627c24eb8fa68be263df6c86f94065fa06`,
and the [v4 historical source expansion](CORPUS_EXPANSION_V4.md). Complete the
OLMo, Qwen and Mistral draft/edit pairs for the selected HC3 families. Add OLMo
draft/edit pairs for v2's 97 Train families in news and science. Each source
retains its original partition; no split is rerolled.

Require terminal generation summaries and exact corpus hashes. Admit a provider's
pair only when both draft and edit pass the existing declared checks. Preserve
all failures and distinguish model-output exclusions from transport failures.
Shared human roots are identical records, deduplicated once. Each root must have
at least one complete provider pair; omissions and attempted denominators remain
in the generation reports.

Train uses every complete pair available for each family. Development,
Calibration and fresh Test use one provider pair per family, ranked by SHA256 of
the fixed domain `slop-ninja-expansion-evaluation-provider-v1`, source-group ID and
model revision. Use the lowest-ranked available complete pair and retain its
human source once. This gives equal origin-class counts and equal family weight
in the evaluation partitions. It does not guarantee equal provider counts;
report the observed counts and exclusions. Selection uses no detector scores.

The 17 old v2 Test families are retired from the new fitting and confirmation
corpus. Their records remain archived for explicitly labeled diagnostics. Fresh
Test consists only of new HC3 source families. Their public historical text may
already occur in pretrained models, so experiment-level novelty does not imply
pretraining novelty. All labels retain their recorded evidence strengths.

Before fitting, freeze the assembled corpus, all four shard hashes, attribution
manifest and paired whitespace view. Run the encoder's token audit on every
partition without computing predictions. Preserve any failures and amend the
assembly explicitly if its rows do not fit the 1,024-token contract; never
truncate or silently drop text in the trainer.

## Fitting and selection

Fit two ModernBERT-base candidates from the same pinned upstream checkpoint,
using learning rates `0.00001` and `0.00002`. Both use seed 17, five epochs,
batch size four, AdamW, weight decay 0.01 and gradient-norm clipping at 1.0.
Keep the three-class head and reference inference contract unchanged. No
separate class weights are applied.

For Train only, weight each row by
`N / (K * count(source_family, origin))`, where `N` is the number of training
rows and `K` is the number of observed family-origin pairs. Require all three
origins per family. The weights sum to `N`; every family-origin pair has equal
total loss weight regardless of how many generators produced examples for it.
Average these weighted per-row losses in each minibatch. Shuffle all rows once
per epoch; do not duplicate human source rows. Retain the v2 alternating raw and
whitespace-collapsed exposure rule, with exactly one view per row per epoch.

Select an epoch only if its Development recall is positive for every origin,
then minimize ordinary Development log loss. Break exact epoch ties in favor of
the earlier epoch. Report the unrestricted loss winner as well. If no epoch
qualifies, save its history and produce no qualified candidate. This coverage
gate prevents silently choosing a model that never recognizes one class; it is
a minimal requirement, not a claim of useful recall.

Select between eligible candidates by the same uncalibrated Development loss;
break exact ties in favor of the lower learning rate. Calibration values and
Test predictions do not enter selection. After epoch choice, the existing
trainer fits a scalar temperature on the ordinary, class-balanced Calibration
partition. Record both candidate identities and the final selection before
opening fresh Test. Fit operating thresholds using human Calibration examples
and the existing strict-greater-than boundary rule.

## Comparison and limits

Compare the selected candidate with the immutable v2 encoder on the same new
Development and fresh Test records. Preserve each model's own frozen calibration
and threshold provenance. Report three-class confusion, class recall, log loss,
Brier score and human false positives, including provider slices and source-family
uncertainty. Different operating points do not establish matched-FPR superiority.
The new Test pool is small; do not claim rare false-positive certification.

Keep unused provider variants for later, explicitly declared comparisons. Use
the previously purchased Pangram observations only for the old Development
cohort; do not recycle them as fresh confirmation or origin labels. No additional
paid calls are part of this experiment. A positive result would remain an
experimental detector, with narrative/informal registers and documented mixed
workflows still requiring work. Preserve the ShareAlike attribution and declared
weight license in any public artifact from this corpus.

## Execution

`assemble_expansion` merges terminal generation exports with the frozen v2
primary corpus and writes both the selected corpus and an archive of all variants.
The archive includes retired Test families and unused evaluation variants; use
only `records.jsonl` for this experiment. Export its shards and derive the fixed
whitespace view with the existing Rust commands, then audit the encoder lengths.

`encoder_frontier` executes the two declared fits in sequence. It checks the
token audit against all four shard hashes, records inputs and invocations before
fitting, and writes `selection.json` before any final Test evaluation. Each
candidate retains its own log and calibration. An ineligible run retains its
epoch history without producing a selected artifact. Other training failures
stop the frontier and preserve the partial logs.

`encoder_evaluation` runs each artifact's bundled CPU inference code. First use
`freeze-thresholds` with that artifact's original Calibration shard; the command
checks its hash against the frozen training report. It preserves the calibrated
probabilities and freezes the two operating points in `thresholds.json`.
Use `evaluate` with that file for Development or final Test. Final Test requires
`--open-final-test`; the command rejects Calibration overlap and never updates
temperature or thresholds. Provider slices include the human roots paired with
that provider's generated texts. Evaluation outputs must use a new directory
outside the artifact, under an existing parent directory.

For an existing detector, preserve its archived Calibration export with
`--calibration-predictions` and `--expected-predictions-sha256`. Recomputing v2's
Calibration scores locally changed probabilities by up to `1.1841e-7` and moved
its strict cutoff by about `1e-8`. Those observations fit within the declared
`1e-6` reference-vector tolerance, but the original decision threshold must stay
fixed. The comparison therefore uses the original v2 export. Reports flag
decisions within twice the reference-vector tolerance of a cutoff; this is a
diagnostic margin, not a proven error bound for arbitrary texts or hardware.

`compare_encoder_reports` compares the two frozen reports for the same corpus
and partition. It verifies matching prediction IDs, source families and origin
labels, then resamples complete families in 512 paired bootstrap replicates.
Report candidate-minus-v2 changes in log loss, Brier score and accuracy with
percentile intervals. Degenerate intervals are omitted. These intervals describe
variation across the observed families; they do not certify performance across
new registers or generators. Each detector retains its own operating thresholds.

For example, from `baseline/detector/`, with the selected artifact and its
original Calibration shard:

```sh
cargo run --release --bin encoder_evaluation -- \
  --artifact /path/to/selected-artifact --python .venv/bin/python \
  --output-dir /path/to/comparison/thresholds \
  freeze-thresholds --calibration-jsonl /path/to/original/calibration.jsonl
cargo run --release --bin encoder_evaluation -- \
  --artifact /path/to/selected-artifact --python .venv/bin/python \
  --output-dir /path/to/comparison/development \
  evaluate --records /path/to/assembly/records.jsonl \
  --thresholds /path/to/comparison/thresholds/thresholds.json --split development
```

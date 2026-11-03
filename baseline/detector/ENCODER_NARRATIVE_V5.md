# Narrative detector experiment v5

The [target clarification](ENCODER_NARRATIVE_V5_TARGET.md) supersedes this initial
selection objective and primary comparison. It was recorded before fitting or
detector predictions. The running generation campaign retains this original
protocol's frozen snapshot and prompts.

Declared before narrative generation, fitting or new detector predictions.
The experiment adds historical narrative summaries and tests transfer to a
generator absent from fitting. It also reduces the learning rates and maximum
epochs after v4's late-epoch Development loss increased. These changes are tested
together; the comparison cannot isolate their individual effects.

## Frozen sources and generation

Use the 289 admitted CMU source families, SHA256
`6967c60542f60b22ccc55e748fbc3d3f7b487acfbc5b4ea0cc33ba304ef2d574`.
Keep their 202 Train, 26 Development, 34 Calibration and 27 Test assignments.
The [source report](CMU_SOURCE_RESULTS.md) records all 400 reviewed summaries,
the historical matches, exclusions and retained collection/source notices.
Summary-author identities and strong human-workflow labels remain unavailable.

Generate one source-conditioned draft and one light-to-moderate edit per family
with each pinned Qwen, Mistral and OLMo model from v4. Use temperature 0.7 and
the existing token caps: 1,800 for Qwen/Mistral, 1,200 for OLMo. Assign the same
six profiles before generation: `source-matched`, `plain`, `informal`, `direct`,
`anti-ai` and `fix-slop`. Preserve the profile catalog, assignment algorithm and
fidelity guard from v4. The new register requests plot-summary prose with the
original character relationships, event order, qualifications and intended tone.

Reserve the [verified Phi-4 model](ml/PHI4_GENERATOR_PROBE.md) for Test alone.
It receives the same full input corpus for deterministic profile assignment,
then selects only its 27 preassigned Test families. Generate one draft/edit pair
per selected family at temperature 0.7 and a 1,200-token cap. No Phi-4 output
enters Train, Development, Calibration, checkpoint selection or threshold fitting.
The compatibility probe used separate synthetic facts and no detector scores.

There are at most 578 calls per training provider and 54 Phi-4 calls, or 1,788
local calls in total. Freeze executable, protocol, runtime and model-spec hashes
before starting. Run one local server at a time, check readiness before calls,
retain every request/response/error, and stop that server after its cohort.
Preserve transport failures separately from output exclusions. Admit a provider's
family only if both operations pass the existing generation checks; never redraw
a prompt to replace an unsuccessful response. Attempted denominators and all
excluded families remain in the report. Passing these checks does not establish
semantic fidelity or a documented human-plus-model writing workflow.

## Assembly and length audit

Merge the three completed training-provider cohorts with the v4 primary corpus,
SHA256 `e7d2a2a0727b8c257c8c7dd4f57daae64b0d1bd4c6e3051ef6fd7e63f0d1c87b`.
Retire v4's 17 opened Test families from both fitting and fresh confirmation;
v2's opened Test families were already absent from this prior corpus. Preserve
all other old assignments and exact records. Deduplicate identical human roots.
Train uses every available complete provider pair with one human source per
family. For each new Development, Calibration and primary Test family, select
one available provider's complete pair using the existing fixed hash of domain,
source-group ID and model revision. No detector scores enter this selection.

Keep the Phi-4 Test-only corpus separate. The primary and Phi-4 Test views may
share human sources and topics, so their outcomes are correlated. Report each
view's admitted denominator and their source-family intersection. Different
generation failure rates can change the source mixture. Within either view,
both detectors must score the exact same rows; do not compare unmatched pools.
These public historical sources may have occurred in pretrained models.

Freeze the merged corpus, shard hashes, attribution manifest and separate
whitespace-collapsed view. Audit all raw partitions, alternate Train and Phi-4
Test with the pinned ModernBERT tokenizer, without predictions. Require every
row to fit 1,024 tokens, including special tokens. Any overflow stops fitting
and requires a recorded protocol amendment before proceeding; do not truncate
or silently remove an overlength example.

## Fitting and selection

Fit from the same pinned ModernBERT-base checkpoint as v4, independently at
learning rates `0.0000075` and `0.00001`. Each fit uses four epochs, seed 17,
batch size four, AdamW, weight decay 0.01 and gradient clipping at 1.0. Keep v4's
three-class head, raw/whitespace alternation and equal total loss weight per
source-family/origin pair. Apply no separate class weights. Preserve the full
history, including epochs that fail the existing positive-recall gate.

Within each run, require positive Development recall for every origin, then
choose minimum uncalibrated Development log loss; earlier epochs win exact ties.
Choose between eligible runs by that same loss, with the lower learning rate
winning exact ties. No eligible epoch means no selected artifact. Calibration
or Test results cannot influence this choice. Fit each selected epoch's scalar
temperature on Calibration using the existing trainer, then freeze the final
candidate identity before opening either Test view. Keep the existing inference
contract and strict-greater-than operating threshold rule.

## Comparison and reporting

Compare the selected candidate against immutable detector v4 on the same
Development records, primary Test and Phi-4 Test. Keep v4's original calibration
and thresholds. Fit the candidate's thresholds using only its own frozen human
Calibration records. Report confusion, per-origin recall, log loss, Brier score,
human false positives and binary detection recall, with provider/register slices
and 512 paired source-family bootstrap replicates. Report both Test views even
if they disagree. Neither Test outcome can trigger another fit within this
experiment; any follow-up must mark these families as opened.

The primary comparison is candidate-minus-v4 three-class Test log loss. The
Phi-4 comparison checks unseen-generator transfer. Describe tradeoffs in class
recall and human false positives even if log loss improves. These small historical
corpora cannot certify rare false-positive rates, Pangram parity, broad-register
performance or faithful transformation. Publication of another experimental
artifact must preserve source/collection attribution and the existing ShareAlike
weight policy. No new Pangram calls or annotation-derived training targets are
part of this experiment.

`generation_campaign` runs the frozen bounded plan and stops after generation.
Use a new output directory for a new campaign. To pause an active cohort, create
`PAUSE` inside that cohort's directory; its current call drains, its server stops
and subsequent models do not start. A campaign-level `PAUSE` stops before the
next model. Inspect retained state before explicitly resuming an individual
cohort. Assembly and fitting are separate commands with their own frozen inputs.

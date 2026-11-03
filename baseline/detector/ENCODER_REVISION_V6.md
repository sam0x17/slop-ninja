# V6 detector comparison: repeated model revision

This protocol fixes the fresh-source allocation, generation routes, model
comparison and observation rules before fresh generation or fitting. The
[Train revision collection](ENCODER_REVISION_DATA_V6.md) is already running
under its separate declaration. V5 remains the released reference until the
new comparison is complete. No v6 detector result is available at declaration.

## Source allocation

Use all 351 sources admitted in the [v6 review](SOURCE_REVIEW_V6.md), with
input SHA256
`195a632721f370f84b5ab3bc8c71a5e7f620fdb1c3a4796e04c639cef2887ad4`.
Keep the 100 PLOS, 100 Wikinews and 151 CMU sources. All are historical human
proxies, and none matches the 852-work historical union under the recorded
identity and exact/lexical-text checks. Retire the full historical Test union.

Reserve the fresh sources for Development, Calibration and Test. Within
each collection, rank roots by SHA256 of the JSON tuple
`("slop-ninja-v6-evaluation-split-v1", source_group)`.
Traverse collections in canonical lexical order, concatenate those lists,
and cycle Development, Calibration, Test over the resulting positions.
This assigns 117 works to each partition. These sources contribute no
training-gradient examples. Keep all assignments when generation fails.

For each partition independently, traverse collections in lexical order.
Within each collection, rank its assigned roots by SHA256 of
`("slop-ninja-v6-evaluation-composer-v1", source_group)`.
Concatenate these lists and cycle Qwen, Mistral, OLMo. Each composer receives
39 roots per partition and 117 overall. Freeze one complete root file per
composer before any generation. Each file's existing `style-mix-v1` profile
assignment uses source-matched, plain, informal, direct, anti-ai and fix-slop.
Freeze its exact profile map against that file's hash; do not recalculate
profiles on a later subset. Profile assignment remains balanced by the
existing split/register rule, independently within each composer file.

Also freeze a Phi-4 root file containing all 117 assigned Test sources.
Use the same six-profile catalog with its own frozen assignment map.
Phi-4 selection is independent of primary generation outcomes. Its prompts
need not match the primary composer's profile for each individual source;
report those assignments and avoid a causal comparison between composers.

## Recorded generation

Use the same pinned and commercially admitted Qwen3-30B-A3B Instruct-2507,
Mistral Small 3.2 24B Instruct-2506, OLMo-3 7B Instruct and Phi-4 MLX
conversions used in v5. Before a cohort starts, verify installed files and
freeze its actual host/runtime, model spec, generator executable, dependency
and prompt hashes. Keep temperature 0.7 and a maximum of 1,800 generated tokens,
with one request at a time per owned server. Source text is data, not an
instruction. Existing completion, source-copy, length and formatting checks
remain unchanged. Preserve exact requests, responses and rejected outputs.

For each primary source, request one independent model-composed draft and
one direct edit of the human root from its assigned composer. This is
702 requests across the three composer cohorts. Revise each admitted draft
with Qwen using the existing Anti-AI mode, then revise the accepted result
with Mistral using Fix Slop. There are at most 351 requests per revision pass.
Do not pass failed branches' earlier leaves to the next stage.

For Phi-4, request its own draft/edit pair for all assigned Test roots,
then apply Phi-4 Anti-AI and Phi-4 Fix Slop passes. This route uses Phi-4
at every stage, with at most 468 requests. Phi-4 stays out of training,
Development and Calibration. We previously inspected its v5 Test results;
describe it as previously observed and excluded from fitting. Do not call
it a previously unseen generator. Its new Test sources remain unused in
earlier experiments.

The fresh generation budget is at most 1,872 local or Studio requests.
This is separate from the already declared maximum of 942 Train revisions.
No detector feedback, output redraws, paid generation or Pangram calls are
part of either collection. Pauses may reuse exact caches. Archive every
attempt and failure; do not replace a source or change its split.

## Admission and observation views

Retain a master source/attempt archive, including failed roots, accepted
siblings and intermediate outputs. For each route, separately assemble
complete-family ancestry records containing the human root, original draft,
paired human edit, first revision and second revision. Admission requires
all four generation requests to have completed and passed the existing
checks. Primary and Phi-4 routes have independent completion cohorts;
report their overlap and all stage denominators.

For each complete-family archive, freeze three explicit observation views:
the same human and mixed-origin records paired with the original draft,
first revision or second revision. All three views use the identical complete
family set and full archive hash. The terminal view is the primary model
selection and evaluation target. Other views show retained draft detection
and intermediate-stage behavior. Within a view, compare v5 and the candidate
on the exact same IDs and text. Do not count shared human roots as independent
observations across stages or routes.

The primary route always ends with Qwen followed by Mistral. Its performance
measures that recorded workflow; it does not isolate revision style, depth or
generator effects. Phi-4 measures a separate repeated self-revision route.
Instructions to preserve meaning and tone do not establish successful
preservation. Keep factual and tonal review separate from origin labels.

Generate and close the cohorts before fitting or scoring. Only complete
two-pass Train revision chains enter the new training corpus; retain all
original v5 Train records regardless of revision failure. Add every admitted
revision stage with its recorded model-only label. Source-conditioned model
drafts remain distinct from edits of human prose, which remain mixed-origin.

## Fitting and candidate selection

Fit from the unchanged original ModernBERT-base checkpoint, using the
three-class head and exactly the two learning rates `0.0000075` and `0.00001`,
four epochs per rate, seed 17, batch size four, AdamW, weight decay 0.01 and
gradient-norm clipping at 1.0. Use equal total loss weight per source family
and origin, without additional class weights. Retain the existing paired
Train whitespace augmentation: one raw or whitespace-collapsed exposure per
row and epoch, with the whole family sharing the declared parity schedule.

The candidate supports 2,048 tokens including special tokens. Audit complete
ancestry shards with the pinned tokenizer before fitting. No truncation,
undocumented windowing or silent record dropping is permitted. If a complete
archive exceeds that limit, stop and record a protocol amendment before any
fit. Freeze checkpoint, shards, rights manifest, whitespace view, token audit,
Development/Calibration observation views and executable runner hashes.

Select the epoch and learning rate by minimum uncalibrated human-versus-model
binary Development log loss on the terminal view, using
`P(AI) = P(model_only) + P(mixed)`. Require nonzero recall for both binary
classes at the diagnostic boundary `P(AI) > 0.5`; ties count as human.
Earlier epochs, then lower rates, break exact loss ties. Retain every epoch's
binary and three-class metrics, including ineligible epochs. Mixed-origin
examples contribute to training and calibration, but not this selection loss.

After epoch selection, fit scalar temperature on the candidate's terminal
Calibration trios. Freeze the nominal 1% and 5% human false-positive operating
points from its human Calibration records. These records come from complete
primary chains; report that conditioning. Temperature and Calibration results
cannot choose a learning rate. V5 retains its original temperature and frozen
thresholds. New source allocation, calibration data, revision exposures and
the supported input length all belong to the v6 recipe; this is not a causal
estimate for training-data changes alone.

## Frozen comparison and coverage

V5 accepts at most 1,024 tokens. Before any detector scores, declare the
common-support subset of each Test route: complete families for which every
record used by the three stage views fits that bound. Keep all ancestors,
exact texts and original split assignments in the derived comparison archive.
Freeze its membership using tokenizer metadata only. Do not truncate a record
to make v5 accept it. Report the common-support denominator and each excluded
length, together with v6 results on the full 2,048-token cohort. A favorable
common-support result cannot substitute for the latter.

The primary paired statistic is candidate-minus-v5 binary log loss on the
common-support terminal Test view. Also report binary Brier score, human
false positives and model-only sensitivity at each model's frozen thresholds,
three-class diagnostics, initial-profile/composer slices and all stage views.
Retain the existing 512 paired source-family bootstrap draws. Different frozen
thresholds do not establish superiority at a matched population false-positive
rate. Neither these historical samples nor a zero observed count certifies a
rare population error rate or Pangram parity.

Independently evaluate all 117 assigned human Test roots, including roots whose
generation failed. Report human false positives with collection slices and
explicitly absent model sensitivity. Preserve unsupported v5 lengths as
coverage failures and report v6 on all supported roots. This diagnostic exposes
human-source selection effects from the complete-chain rule. If all roots fit
v5, compare the same full human set directly. It is not another independent
human sample to add to the paired-route counts.

Freeze candidate selection, both models' thresholds, full/common-support views,
human-root coverage and all tool hashes before Test execution. Test outcomes
cannot trigger another fit or switch the selected candidate within v6.
Publish aggregate results, exclusions, bindings and the selected research
artifact with required attribution, including unfavorable results. Keep raw
corpora, per-record predictions and exact lineage records ignored.

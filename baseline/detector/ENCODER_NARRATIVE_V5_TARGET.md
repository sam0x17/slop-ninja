# V5 target clarification: fully model-written prose

The user's clarified priority is text written entirely by a model while making a
strong attempt to avoid that model's usual voice, including deliberate removal
of recognizable AI phrasing and shifts toward an intended writer's style.
This amendment was recorded during generation, before any v5 fit, Development
detector score or Test prediction. It supersedes the selection objective and
primary comparison in [the initial v5 protocol](ENCODER_NARRATIVE_V5.md).

The running generation campaign remains useful: each source has an independently
composed model draft as well as an edit. The six existing profiles include
`anti-ai` and `fix-slop`, alongside source-matched, plain, informal and direct
writing. Keep its frozen prompts, caches, source partitions and attempted
denominators. Do not relabel an edit of human prose as entirely model-written.
The [overlap audit](NARRATIVE_GENERATION_QA.md) measures that separate edit lane;
lack of light edits is not a gap in the clarified main target.

## Selection before Test

Keep the declared two learning rates, four epochs, source/origin loss weights
and three-class training head. Treat mixed-origin examples as auxiliary training
data and retain their diagnostics. For checkpoint selection, use only human and
fully model-written Development examples. Minimize their uncalibrated binary
log loss using the existing AI score
`P(AI) = P(model_only) + P(mixed)`.

Require nonzero recall for both human and fully model-written examples at the
fixed diagnostic boundary `P(AI) > 0.5`; exact ties count as human. This is a
minimal non-degeneracy gate, not the deployment operating point. Mixed-class
recall cannot disqualify a checkpoint. Earlier epochs and then lower learning
rates break exact loss ties. Record both the original three-class metrics and
the new objective for every epoch, including ineligible epochs.

Use `--selection-objective human_model` with the updated Rust frontier and
trainer. Freeze new runner/tool hashes separately; do not overwrite the running
generation executable, its protocol snapshot or the original v4 artifacts.
The existing scalar temperature still fits on the full three-class Calibration
partition after epoch selection. Freeze operating thresholds using its human
Calibration rows. Calibration and Test remain excluded from checkpoint choice.

## Comparison and next data priority

The primary comparison becomes candidate-minus-v4 human/model binary Test log
loss, with binary Brier score, human false positives and sensitivity to fully
model-written drafts at each detector's original frozen operating thresholds.
Keep the old three-class comparison as a diagnostic. Preserve 512 paired
source-family bootstrap draws for both comparisons. Report primary Test and
the separate Phi-4 Test, including shared families and all generation exclusions.

Report writing-profile and generator/profile slices, with the corresponding
human roots in every slice. Show `anti-ai` and `fix-slop` explicitly even when
their outcomes are unfavorable or their denominators are small. A strong pooled
result cannot substitute for detection on these style-steered drafts. Missing
historical profile metadata belongs in an explicit unprofiled category.

These prompts establish attempted style steering, not successful removal of a
model's stylistic signature. The current open-weight generators also do not
establish transfer to every current frontier model. Further data should emphasize
fully model-written passages under stronger voice constraints and recorded model
self-revision. Any model-written passage revised entirely by models retains
model-only production lineage. Evaluate preserved meaning and intended tone
separately, without converting low detector scores into fidelity labels.

Neither Test outcome may trigger another fit within this experiment. Preserve
the original sample and source-admission decisions. No new Pangram calls are
required by this amendment.

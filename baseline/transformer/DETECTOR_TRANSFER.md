# Does improvement on A transfer to Pangram?

The current A model is insufficient as the sole optimization guide for B.
Large reductions on A have repeatedly produced little or no reduction in
Pangram's flagged fraction. Ordinary detection accuracy and usefulness for
ranking adversarial revisions are separate properties.

## Full-document check, 2026-09-14

Three exact versions of one private development document were checked on
Pangram 4.0. Each observation covers the complete document. The A results use
the unchanged V6 artifact and its reference CPU runtime, with exact-text checks
against the newly prepared document plans.

| Version | A model/assistance probability | Pangram AI-written | Pangram AI-assisted | Pangram human |
| --- | ---: | ---: | ---: | ---: |
| Original | 90.3883% | 100% | 0% | 0% |
| Five-edit repair bundle | 77.2311% | 100% | 0% | 0% |
| Repair bundle and six sentence joins | 51.8885% | 96.3020% | 0% | 3.6980% |

From original to final candidate, A fell 38.4998 percentage points. Pangram's
flagged fraction fell 3.6980 points. The repair bundle alone reduced A by
13.1572 points without changing Pangram's content fractions. The three texts
fit A's context limit; this discrepancy does not arise from truncating this
comparison to excerpts.

The collector verified successful task identities, bound submitted hashes,
returned hashes and class sums. All three echoes differed only in whitespace.
All returned windows remain archived. The original and repair bundle each
received one window, with assistance scores 0.970631 and 0.925656. The final
candidate received three windows, including one human-labeled span. Different
window boundaries prevent treating the first window's score as a matched
document measurement. Window assistance scores are auxiliary diagnostics.

Candidate selection was adaptive to A; the candidates were frozen before
these Pangram calls. This is one source group with two revisions, not three
independent validation examples. There were no repeat submissions. The complete
final candidate still has unresolved density and register concerns described
in the [sentence-join results](RESULTS_BOUNDARIES_V1.md). Neither editorial
acceptance nor joint detector success follows from these measurements.

All new private text, plans, CPU observations, provider responses and the exact
paired comparison stay on the Studio outside the repository and datasets.
These materials are excluded from detector training and commercial corpora.
Only aggregate findings are recorded here.

The spend of the three calls and of the recorded submissions is recorded in
the private ledger.

## Earlier comparisons

The two [synthetic notices](RESULTS_MENU_V1.md) reached 4.14% and 6.90% on A
from 72.85% and 85.06%, respectively. Pangram classified both originals and
both final revisions as 100% AI-written. The later
[proposal repairs](RESULTS_PROPOSALS_V1.md) reached 3.97% and 7.26% on A while
remaining 100% AI-written on Pangram. Those repairs reuse the workshop source
and must not be counted as additional independent source groups.

These repeated failures establish that transfer cannot be assumed. The small,
adaptively selected set does not estimate a population correlation, identify
Pangram's internal representation or prove that A has poor accuracy on every
detection task. Style shortcuts, limited coverage of editing methods and score
calibration are hypotheses to distinguish. A probability and a Pangram content
fraction are different quantities, so equal percentage-point changes are not
an expected calibration identity.

## Required next evaluation

Freeze source groups and the evaluation policy before opening new external
scores. Use commercially cleared material with recorded generating model,
requested style, original composition versus revision, and every revising
model. Keep a source and all descendants in one split. Evaluate held-out
styles, generators and editing methods separately, including passages revised
by multiple models. A random text split can otherwise conceal those gaps.

Within each complete source, compare a fixed set of editorially reviewed
alternatives: unchanged text, A-selected candidates, and candidates selected
without A at the same query budget. Preserve local word/clause edits and
combinations as separate intervention families. Keep the source, meaning
checks and requested tone fixed. Evaluate independent source groups; do not
inflate the sample count with hundreds of related menu states.

Report these measurements separately:

- A's ordinary detection errors by generator, style and revision history.
- Paired changes in A and Pangram's AI-plus-assisted fraction, including
  zero-change and worsening cases.
- Transfer among candidates with at least a 10-percentage-point A reduction:
  how many also reduce Pangram's flagged fraction by at least 10 points.
  These are operational materiality thresholds, not a calibration claim.
- Within-source ranking agreement and joint below-10% success, with uncertainty
  computed over source groups. Saturated Pangram fractions produce ties that
  must remain visible.
- Meaning, requested tone and writing-quality judgments, with evaluator
  provenance and whether detector scores were visible.

Further paid evaluation needs a new spending decision, recorded in the private
ledger. Until this test succeeds, report A-only gains as search
observations, not progress confirmed on Pangram. Use the existing annotated
corpus to investigate A's weaknesses first. Private book text and B outputs
remain evaluation-only; disagreement does not authorize adding them to A's
training data.

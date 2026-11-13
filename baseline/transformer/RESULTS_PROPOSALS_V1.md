# Local word and clause proposal comparison

The model supplied a clause combination that reduced A, but neither reviewed
repair met the joint target on Pangram. This comparison produced no joint
detector success. It also exposed failures in instruction following and
meaning preservation that a lower A score would have concealed.

The [protocol](PROTOCOL_PROPOSALS_V1.md) uses the pinned Qwen3-30B-A3B-Instruct-2507
MLX checkpoint, with one recorded call in each of four cells. The source was the
same adaptive workshop notice, with the same two imperfect Global Voices
reference excerpts. Both references alone and references plus measured grammar
and function-word rates were tested for lexical and clause alternatives.

## Proposals and review

All four calls completed, yielding 32 proposed alternatives. Both lexical
batches exceeded the four-word span limit on every suggestion. The importer
rejected all 16. The two clause batches supplied 16 mechanically valid spans,
which produced 93 states each through depth three, or 185 distinct texts.
The complete combinations were scored with the unchanged V6 A artifact.

Mechanical acceptance did not establish fidelity. The individual reviews
excluded changes that omitted the workbench-only restriction or the explicit
experience-elsewhere detail, shifted an availability claim toward uncertain
uptake, or added bureaucratic wording. Some model rationales also misidentified
active and passive constructions or falsely claimed unchanged word counts.
The reviews were saved before opening the new A scores.

| Proposal condition | Mechanically accepted | Best A across all states | Best A after individual exclusions |
|---|---:|---:|---:|
| Lexical, references | 0/8 | No states | No states |
| Lexical, measured | 0/8 | No states | No states |
| Clause, references | 8/8 | 2.92% | 2.92% |
| Clause, measured | 8/8 | 86.69% | 94.85%, unchanged parent |

The 2.92% candidate was not accepted or submitted to Pangram. A subsequent
full-source review caught a qualification that the individual review had
missed: the original described shorter waits as *an aim*, while the proposal
called them *the goal*. The repair restored a nonexclusive goal. A fourth
choice named the trial arrangements explicitly as the object of the staff's
decision, removing the competing antecedents of “them.”

All 16 subsets of those four choices were measured in a separate repair menu.
Its three core edits scored 89.17%, 89.88% and 91.91% individually on A; together
they scored 3.97%. Adding the explicit referent raised A to 7.26% while improving
clarity. Selecting only individually successful edits would have missed this
combination. A later [class-logit audit](../detector/EDIT_INTERACTION_RESULTS_V1.md)
used the saved responses to separate these effects. Single-edit class-logit
changes would predict 72.43% for the three-edit repair; both class contrasts
show substantial additional changes when the edits are combined. The audit
made no new model or provider calls.

## External comparison

Two complete repaired passages received full-source reviews after A scoring
and before Pangram submission. The four-edit passage was preferred for its
explicit referent. Reviews checked every original fact, condition and
qualification, requested tone, readability, cleanup and provisional author fit.
These remain assistant judgments, without independent preservation or author-fit
validation. The unmodified parent and original source were submitted as controls.

| Text | A probability |
|---|---:|
| Original source | Not rescored in this batch |
| Current parent | 94.85% |
| Three repaired edits | 3.97% |
| Four edits, explicit referent | 7.26% |

Their Pangram fractions are kept privately and not published.

Both revised passages improved the original function-word/grammar distance:
1.25666 for the parent, 1.22600 for three edits and 1.20150 for four. The optional
factorized order distance worsened from 0.85199 to 0.86360 for both revisions.
These measurements disagree about direction; neither establishes author identity
or writing quality. The fixed mixture comparisons are retained.

Pangram returned one segment per text. Its continuous assistance score is an
auxiliary observation; the goal uses the returned AI plus AI-assisted fractions,
and none of the four observations met it. The returned version was 4.0, and every
text echo differed only in whitespace. No request was retried.

## Records and next step

The [aggregate summary](results/proposals-v1/summary.json) binds the exact inputs,
generation records, reviews, model and results. Separate files retain the
[reference clause ranking](results/proposals-v1/clause-references.json),
[measured clause ranking](results/proposals-v1/clause-measured.json),
[repair ranking](results/proposals-v1/repair.json) and
[external observations](results/proposals-v1/external.json). Raw passages,
reference excerpts, prompts and responses remain ignored.

The comparison made four generation calls and 201 A requests covering 197
distinct texts, followed by four Pangram observations. The paid batch's
spend is recorded in the private ledger.

All model work, spaCy extraction, builds, tests and Python bookkeeping ran on
the Studio. Ten B tests, formatting and clippy passed, including one new test
for proposal guards. The pinned generator files were verified, and the owned
generator instance was unloaded after the four calls. An oversized unexecuted
prompt preparation was retained; removing provenance metadata from the prompts
allowed both variants to fit the 8,192-token context. No Linux rerun or attested
execution is claimed.

Measured guidance performed worse in this four-cell comparison, but a single
source, one seed and two weak reference samples cannot establish a general
effect. The lexical failures favor nominating bounded spans before generation
in the next proposal interface. The [long-form benchmark](LONG_FORM.md) is the
next evaluation priority: score and review assembled multi-paragraph works,
then chapters and books. No B weights were trained and no new whitepaper
accuracy milestone was reached.

# Clause-boundary and parser comparison

On one private development document, the small parser generated five boundary
edits and the transformer parser generated eight. Three were shared, yielding
ten distinct changes. Each changed text raised exploratory A and increased both
measured reference distances within its own parser space. The original A was
90.39%; the changed texts ranged from 90.60% to 92.75%.

A second menu tested all 128 combinations of six semicolon splits and an earlier
five-edit repair bundle. None of the splits helped in any of its 64 tested
contexts. Their conditional increases ranged from 0.0255 to 4.9652 percentage
points. Keeping individually unsuccessful edits therefore did not expose a useful
combination in this menu. This is a result about this neighborhood, not a general
claim that sentence splitting cannot help.

The best combination contained only the earlier repair bundle. The reference CPU
runner confirmed A at 77.23%, differing from MPS by `2.303e-6` in combined
probability. Both transformer-parser reference distances worsened. Among states
with neither distance increased, the unchanged source was best. No candidate
reached 10%, no Pangram calls were made, and none received final author-fit or
editing-quality approval.

A is the combined model-only and mixed-origin class probability, not the
percentage of AI-generated words. All 128 texts were measured on MPS; only the
selected repair bundle received a new CPU confirmation. The unchanged source's
exact prior reference observation was reused. Individual conditional effects
remain exploratory.

The source, 47 preservation checks, six reference works and intended voice were
fixed. Reference distances from the two parser spaces are not directly
comparable. Text, full plans and observations remain in ignored private storage.
This work adds no joint-detector, independent-quality or book-level milestone.

The subsequent corpus work prioritizes [direct syntax inspection](../../grammar/CAMPAIGN.md).

## Complete-sentence joins

The reverse operation in catalog v4 produced ten single joins under each parser,
with five shared: fifteen distinct changed texts. All fifteen lowered exploratory
A and improved both reference distances within their own parser space. The best
single join scored 88.34%, a 2.04-point decrease. These small improvements alone
did not establish useful editing performance.

Eight joins were selected for a collective research comparison after reading
the affected sentences and paragraphs, before inspecting their new A scores.
The remaining seven had reservations about rhetorical pauses, topic transitions,
density or antecedent scope. Combining the eight eligible joins with the earlier
five-edit repair bundle enumerated 512 subsets. Overlaps and existing edit limits
rejected 130; all 382 admitted whole texts were measured on MPS.

The best candidate contains six joins and the repair bundle. Reference CPU
inference confirmed **51.8885% A**, compared with 90.3883% for the unchanged source
and 77.2311% for the earlier bundle. CPU and MPS differ by `2.203e-6` in combined
probability. This candidate was also the lowest-A state with neither reference
distance increased: function-word/grammar distance fell from 0.63437 to 0.60898,
and full word/grammar distance from 0.74608 to 0.73278. The input fits the reference
runner's limit at 1,878 tokens. The subsequent
[paired Pangram check](DETECTOR_TRANSFER.md) measured 100% AI-written for the
original and repair bundle, and 96.3020% for this candidate. The large A gain
therefore transferred only weakly to Pangram.

The combined result cannot be predicted by adding the isolated score changes.
That calculation gives 70.44% A on the MPS observations; adding changes in log
odds instead gives 61.95%. The measured combination is 51.89%. This documents
non-additivity in this finite menu, including on the log-odds scale; it does not
identify a general attribution rule or establish transfer to another detector.

Sentence-load measurements supplied 77.61% to 102.59% of the net squared-distance
gain in the single-join function/grammar comparisons. In some cases the other
families collectively worsened. An offline query-order comparison therefore
also excluded sentence load while retaining the remaining weights and scales:

| Order or comparison | Candidate queries | Best A, retaining the source |
| --- | ---: | ---: |
| Full word/grammar distance | 8 | 74.06% |
| Function-word/grammar distance | 8 | 74.06% |
| Function/grammar movement excluding sentence load | 8 | 51.89% |
| Uniform subsets, exact expected best | 8 | 62.83% |
| Exhaustive menu | 381 | 51.89% |

The root query is additional and common to every comparison. These are offline
orders on a fully scored development menu; the experiment actually scored all
382 texts. A replay on the older split menu also favored removing sentence load,
but it is previously explored material. The [blog comparison](BLOG_GUIDANCE.md)
tests the same orders with other reference writers and another fixed menu.

Direct reading of the complete finalist found retained claims and qualifications,
with unresolved editorial concerns: a chain joining three architectural
statements, a dense chapter-outline sentence and inherited formal glossary
wording. The clearer essay location and separated intellectual-lineage statement
retain earlier accessibility gains. This is an assisted review, not independent
validation. Author fit and no-regression in writing quality are not approved.
The source, 47 preservation checks, six original reference works and intended
audience remain fixed. All private source text, candidate text, plans and reviews
for this study stay on the Studio outside the repository and datasets.

This improves the observed full-document A minimum. It supplies no joint
A/Pangram success, approved final transformation or book-level milestone.

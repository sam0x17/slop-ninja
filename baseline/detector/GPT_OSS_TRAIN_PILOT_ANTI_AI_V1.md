# GPT-OSS anti-AI-profile review

All four attempts in the fifth [frozen pilot](GPT_OSS_TRAIN_PILOT_V1.md) profile
returned complete responses. Three passed collection checks. The fourth was
excluded because its serialized response contains three analysis messages before
one final answer, outside the V1 parser's accepted format. Manual review read
all four raw responses and final passages, including that excluded answer.

| Source | Main findings |
| --- | --- |
| Novel summary | Changed Morgra's name to Morg, described the wolf character as a woman, omitted role definitions and added emotional interpretation. |
| Missing-data explanation | Preserved much of the explanation but merged distinct causes and added methods absent from the source. |
| Cardiac-cell abstract | Again moved assessment timing to injection; added male mice, measurement details and specific outcomes; strengthened qualifications. |
| Awards report | Dropped two required Revenant awards and a nationality, repeated a nomination and added evaluative ceremony commentary. |

The awards response's three analysis messages show the model adding commentary
to meet a self-chosen exact word count and then cutting required facts. A false
costume-design award appears only in a discarded draft and is not counted as a
final-text defect. The shorter novel and medical responses contain substantive
errors without extended counting loops. Approximate-length instructions alone
do not explain the cohort's fidelity problems.

The [review](results/gpt-oss-train-pilot-anti-ai-review-v1.json) preserves the
four judgments, observed format and original collector outcomes. Its
[binding check](results/gpt-oss-train-pilot-anti-ai-check-v1.json) verifies exact
source, request, response, final-text and available record identities. The
excluded answer has no record and remains excluded from the frozen pilot.
The [versioned parser follow-up](HARMONY_RESPONSE_V2_RESULTS.md) addresses its
format for future collection without changing these historical outcomes.

Twenty of 24 planned attempts now have complete manual review. All have
strict-fidelity defects; 18 passed collector admission, one earlier attempt
failed the source-copy check, and this attempt failed response extraction.
These remain recorded model attempts, but do not support a controlled comparison
in which only style changed. No active training records, detector observations
or Pangram calls follow from this review. The last profile continues under its
original frozen settings. Its subsequent
[Fix Slop review](GPT_OSS_TRAIN_PILOT_FIX_SLOP_V1.md) completes the 24-attempt pilot.

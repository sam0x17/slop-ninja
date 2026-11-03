# GPT-OSS plain-profile review

All four plain-profile requests in the [frozen pilot](GPT_OSS_TRAIN_PILOT_V1.md)
finished. Three passed collection checks; one was excluded for copying a
contiguous 20-word source span. Manual review read all four complete responses,
including the excluded attempt, and found source-fidelity defects in each.

| Source | Main fidelity findings |
| --- | --- |
| CMU novel summary | Omitted role terms, corrupted an introduced name, added repetitive sentiment and intensified a relationship. |
| HC3 missing-data explanation | Added definitions, statistical consequences and treatment methods; narrowed scope and weakened the unit/nonresponse distinction. |
| PLOS cardiac-cell abstract | Again moved assessment timing to injection; added sex and measurement details; varied the strength of the arrhythmia result. |
| Wikinews awards report | Omitted a nomination and a nationality; also failed the independent source-copy check. |

The [review ledger](results/gpt-oss-train-pilot-plain-review-v1.json) separates
meaning, qualification, tone, realized style and copying. The
[byte check](results/gpt-oss-train-pilot-plain-check-v1.json) verifies source,
request, response and exact final-channel text for every attempt, including the
one without an admitted record. Word counts use Unicode whitespace explicitly.

The PLOS and Wikinews responses show fidelity defects without the extended
counting traces seen in other outputs. Length instructions remain a hypothesis
to investigate, not an established cause. These four exposed sources cannot
isolate a prompt's causal effect or estimate population performance.

Together with the [source-matched review](GPT_OSS_TRAIN_PILOT_SOURCE_MATCHED_V1.md),
eight of 24 planned attempts have been read. The remaining four profiles retain
the same frozen plans. Successful model-origin collection and faithful editing
remain separate judgments. This pilot has admitted no active training records
and makes no A or Pangram calls. Generated prose and raw responses stay outside
Git; the committed artifacts contain review findings and bindings.

The subsequent [informal-profile review](GPT_OSS_TRAIN_PILOT_INFORMAL_V1.md)
covers the next four completed attempts and distinguishes requested register
from the style actually produced.

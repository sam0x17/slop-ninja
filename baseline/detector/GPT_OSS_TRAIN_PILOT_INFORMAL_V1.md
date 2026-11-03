# GPT-OSS informal-profile review

The third profile in the [frozen pilot](GPT_OSS_TRAIN_PILOT_V1.md) completed all
four requests and passed the existing collector checks. Manual review of every
source and output found fidelity defects in all four. The requested informal
register was often weakly realized; it must remain separate from observed style
in later corpus annotations.

| Source | Main findings |
| --- | --- |
| Novel summary | Preserved unexplained role terms but lost their definitions; added character psychology and repeated thematic conclusions. |
| Missing-data explanation | Narrowed values to numbers; dropped several causes and distinctions; added technical definitions, methods and mandatory advice. |
| Cardiac-cell abstract | Again moved assessment timing to injection; added male mice and a mechanistic interpretation; weakened or changed qualifications. |
| Awards report | Omitted a nomination and nationality; added a comparative ranking, a ceremony interpretation and the unsupported year 2026. |

The [review](results/gpt-oss-train-pilot-informal-review-v1.json) records the
preserved content, defects, register and tone. Its
[binding check](results/gpt-oss-train-pilot-informal-check-v1.json) verifies the
four exact sources, requests, responses, final-channel texts and records.
Assistant judgments remain separate from the mechanical checks.

The awards response explicitly added its final year phrase while extending the
draft to a self-imposed exact word count. The prompt requested approximate
length. The medical timing error occurred without an extended counting trace.
Length pressure is therefore a specific hypothesis for a later comparison; it
does not explain every defect. Fixing the template date also does not stop a
model from applying that date to a historical source.

Twelve of 24 planned attempts have now been reviewed. All have strict-fidelity
failures; 11 passed collector admission and one earlier plain-profile attempt
failed the source-copy check. These findings do not erase model-origin evidence,
but they prevent a controlled interpretation in which only style changed. The
pilot makes no detector or Pangram calls and admits no active training records.
The subsequent [direct-profile review](GPT_OSS_TRAIN_PILOT_DIRECT_V1.md) covers
four more completed outputs. The remaining declared profiles continue with
unchanged prompts and settings.

# GPT-OSS direct-profile review

The fourth profile in the [frozen pilot](GPT_OSS_TRAIN_PILOT_V1.md) completed all
four requests and passed collector checks. Manual review of each complete raw
response and final passage found fidelity defects in all four.

| Source | Main findings |
| --- | --- |
| Novel summary | Omitted role definitions; invented obstacles, nightly storytelling and a belief that the prophecy could be broken immediately. |
| Missing-data explanation | Clearer than earlier attempts, with the research fields retained; still changed unavailable information to nonexistent information, lost a cause and strengthened possible handling to typical handling. |
| Cardiac-cell abstract | Moved assessment timing to injection; added male mice and an unsupported percentage; deleted the outcome variable from a ventricular-function sentence. |
| Awards report | Restored the film's nomination but omitted nationalities and added a generic approving conclusion. |

The medical response's raw drafting trace invented numerical results and then
repeatedly deleted words to reach an exact length. The final text retained a
percentage without a clear quantity and a sentence with no outcome variable.
These defects occur in the saved final output. The prompt requested approximate
length; the exact-count target was the model's choice. The clearer missing-data
response lacked an extended counting trace. This observation motivates a later
length-instruction comparison without establishing a general cause of failure.

The [review ledger](results/gpt-oss-train-pilot-direct-review-v1.json) preserves
all four judgments and source obligations. Its
[binding check](results/gpt-oss-train-pilot-direct-check-v1.json) verifies exact
source, request, response, final-channel and record identities, along with usage
and Unicode-whitespace word counts. Assistant fidelity judgments remain separate
from those mechanical checks.

Sixteen of 24 planned attempts have now received complete review. All have
strict-fidelity defects; 15 passed collector admission and one earlier plain
attempt failed the source-copy check. Model origin remains known, but these
outputs do not support a controlled claim that only style changed. No active
training records or detector calls follow from this review. The remaining
profiles retain their frozen prompts and settings. The subsequent
[anti-AI-profile review](GPT_OSS_TRAIN_PILOT_ANTI_AI_V1.md) covers four more
complete responses, including a collector format exclusion.

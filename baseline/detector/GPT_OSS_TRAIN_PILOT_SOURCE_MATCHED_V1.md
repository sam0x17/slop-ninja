# GPT-OSS source-matched pilot review

All four initial requests in the [frozen pilot](GPT_OSS_TRAIN_PILOT_V1.md)
completed normally and passed the existing collection checks. Manual review of
the complete sources, raw responses and final passages found meaning changes in
every output. These four sources do not estimate population quality or isolate
the causal effect of a prompt.

| Source | Main fidelity findings |
| --- | --- |
| PLOS cardiac-cell abstract | Changed assessment timing into injection timing; added method and outcome details; strengthened improved function to restored function. |
| HC3 missing-data explanation | Added treatment methods and consequences; narrowed unavailable information to nonexistent data; turned a possibility into a claim about common practice. |
| CMU novel summary | Omitted pack-role definitions and source details; invented events and a moral interpretation; introduced awkward missing determiners. |
| Wikinews awards report | Omitted a nationality; narrowed first Oscar to first Best Actor award; added a character name and an unsupported celebratory conclusion. |

The [review ledger](results/gpt-oss-train-pilot-source-matched-review-v1.json)
records preserved information, omissions, additions, qualifications, register and
tone separately. The [byte check](results/gpt-oss-train-pilot-source-matched-check-v1.json)
binds that review to the saved source, request, response, record and final text.
The semantic judgments are assistant review, not independently annotated gold.

The saved generation traces show word counting and additions made to approach
the requested length. In the novel summary, this also led to deleting determiners.
This suggests a prompt failure to investigate after the declared pilot; it does
not establish what an alternative prompt would do. The remaining 20 requests
retain their frozen sources, prompts and settings across plain, informal,
direct, anti-AI and Fix Slop profiles. They run sequentially on the Studio and
stop on an execution failure, with no automatic retry.

All four outputs retain valid model-origin evidence despite the fidelity
failures. They cannot support a controlled comparison of style with meaning held
constant. No pilot output has entered the active training archive or release
corpus, and this pilot makes no A or Pangram calls. Exact generated prose and
live invocation logs remain in ignored storage.

The subsequent [plain-profile review](GPT_OSS_TRAIN_PILOT_PLAIN_V1.md) covers all
four next attempts, including one source-copy exclusion. It also finds fidelity
defects in responses without extended counting traces.

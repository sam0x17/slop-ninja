# Model-only writing and revision probe

Measured 2026-09-11. DeepSeek V4 Flash composed a fictional library notice,
rewrote it under strong instructions to avoid its usual voice, and applied a
further Fix Slop pass. After the user requested cross-model examples, Qwen
rewrote the original DeepSeek notice in a separate branch. All four passages
remain entirely model-written. Changing the revising model does not create a
human contribution or a mixed-origin label.

V4 detected all four passages at both of its original operating thresholds.
The attempted cleanup increased its AI score in this example. The rewrites also
introduced problems with meaning or intended tone. This is one synthetic topic,
with four correlated outputs and no human control. It establishes neither an
evasion rate nor broad detector performance.

| Passage | Parent | Words | V4 AI score | V4 origin prediction |
| --- | --- | ---: | ---: | --- |
| DeepSeek initial notice | Synthetic fact sheet | 227 | 0.9076 | Mixed |
| DeepSeek voice rewrite | Initial notice | 204 | 0.9790 | Mixed |
| DeepSeek Fix Slop pass | DeepSeek voice rewrite | 202 | 0.9811 | Mixed |
| Qwen voice rewrite | Initial DeepSeek notice | 229 | 0.9878 | Model only |

The AI score is `P(model_only) + P(mixed)`, not the fraction of words written by
AI. The unchanged thresholds are strict scores greater than
`0.9055049212676647` and `0.902395786182547`. Although the binary decisions detected
model involvement, the three-class head incorrectly chose mixed origin for the
three DeepSeek passages. Production lineage supplies their model-only labels.

## Meaning and tone

The same synthetic fact sheet accompanied every request. It specifies 13
operational facts and conditions, including a conditional noon target, preserved
loan records and queue order, and a reason for delaying restoration until checks
pass. The requested voice is a practical, considerate municipal librarian.
The coding assistant checked each output before seeing detector scores; it knew
the prompts and model identities. This is an assisted review, without an
independent human judgment or an automated fidelity guarantee.

The initial notice retained the 13 checks, with an added conventional closing.
The DeepSeek voice rewrite replaced read-only catalog access with a claim that
search would not show live changes. Read-only access does not establish whether
search results refresh. The Fix Slop pass repeated that unsupported claim, added
an unwanted introduction and retained punctuation and rhetorical patterns that
the instructions asked it to remove. No one repaired these outputs before
scoring; the introduction remains in the scored text.

Qwen preserved the operational details but added an illustration of a delay of
a few hours, for which the fact sheet supplied no estimate. Its repeated emphatic
instructions made the notice more admonishing than the requested considerate
tone. Cross-model editing supplied another example of the intended production
workflow; it did not improve this notice's detector score or tonal fidelity.

Future data should include both same-model and cross-model revisions, with the
original composer, every reviser and exact intermediate text recorded. Preserve
the whole source family across splits and distinguish generation paths in
evaluation. Do not map these chains onto the existing human-copyedit category.
Facts can remain present while an added implication changes meaning, so a
fact-coverage count alone is insufficient for transformation scoring.

## Runtime and evidence

The Studio already held the DeepSeek weights. All five installed shards,
161,869,615,520 bytes in total, matched the pinned Hub LFS hashes. The
[file manifest](manifests/deepseek-gguf-files.json) records them. The conversion is
`unsloth/DeepSeek-V4-Flash-0731-GGUF@fbbb5b93fb787c21338159b0af3318bb3f4d9768`.
DeepSeek's pinned [model card](https://huggingface.co/deepseek-ai/DeepSeek-V4-Flash-0731/blob/7872f01b1d1fe23eabc4c98b48bffcef5a386062/README.md)
and [MIT grant](https://huggingface.co/deepseek-ai/DeepSeek-V4-Flash-0731/blob/7872f01b1d1fe23eabc4c98b48bffcef5a386062/LICENSE)
were captured and checked against their Git blob identities. The grant's SHA256
is `f2c6c602815669d292889e5be8c802f2ed950653b77999b1584e8e6aed25d040`.
Keep the DeepSeek copyright and permission notice when distributing its weights.

Serving used LM Studio's llama.cpp 2.28.2 backend, full GPU offload, six experts
per token as in the upstream configuration, and speculative decoding disabled.
Loading took 179.67 seconds. The three requests took 192.26, 223.19 and 239.20
seconds, with reported generation rates around 1.6 tokens/second. This runtime
is too slow for a large generation campaign without further work.

The Qwen branch used the existing pinned Qwen3-30B-A3B-Instruct-2507 MLX model.
Every installed file was checked again against its recorded manifest. LM
Studio's MLX 1.11.0 backend completed the request in 35.82 seconds, reporting
10.94 tokens/second. These are different requests and models, not a controlled
backend comparison. Both owned model instances were unloaded afterward.

All calls used temperature 1.0, top-p 1.0, an 8,192-token context, a 1,536-token
output cap and one request at a time. DeepSeek reasoning was disabled; the
non-thinking Qwen checkpoint received no reasoning-control parameter. Each
response was below the output cap and reported zero reasoning tokens. Calls
used the [native chat API](https://lmstudio.ai/docs/developer/rest/chat), without
tools or stored server conversations. Exact prompts, responses, timing and
reviews remain in ignored local storage.

The [aggregate report](results/model-only-revision-probe-v1.json) binds those
artifacts and the original v4 detector. All writing finished before CPU detector
inference. There was no detector feedback into generation, no redraw and no
Pangram call. None of these passages enters the frozen v5 corpus, fitting,
Calibration or Test. This probe changes no v5 checkpoint-selection rule.

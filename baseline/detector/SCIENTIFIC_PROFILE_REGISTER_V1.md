# Scientific introductions with the requested register

The first scientific pilot appended a research-journal register after each
style profile. All Qwen informal drafts remained academic; two Mistral informal
drafts became conversational but failed the source-copy guard. The completed
[V6 diagnostic](SCIENTIFIC_PILOT_V6_RESULTS.md) also found lower confidence on
Mistral drafts. Those observations do not isolate the prompt's effect, but they
justify checking its conflicting register instructions before collecting more.

This new pilot uses the same three already allocated Train roots and the same
pinned Qwen and Mistral checkpoints, each under the existing informal profile.
It permits one new draft per source/model pair, six requests in total. Preserve
every attempt. The previous 24-request pilot, its reviews, scores and admission
decisions remain fixed; its remaining 80 calls stay paused.

The opt-in `--scientific-profile-register` mode replaces the collection's
research-journal requirement with a complete introduction for the audience and
style specified by the profile. The output instruction requests the complete
introduction rather than repeating a scientific-genre label. Source facts,
qualifications, background, competing explanations, unresolved questions,
purpose and citation labels remain required. The profile itself, source text,
temperature 0.7 and output limit of 4,096 generator tokens keep their bytes or
values. This applies only to profiled initial drafts of scientific introductions.
Other collections and the default prompt keep their prior behavior.

Set an explicit 1,800-second request deadline for this pilot. The prior client
deadline was 900 seconds, and one request expired before a response could be
captured. A longer deadline does not guarantee completion. Keep timeouts and
late or missing responses distinct from completed drafts; do not retry or draw
replacement samples. The deadline is run metadata and does not change a model
request or its cache key. Runs with different explicit deadlines must use
different output directories.

Before generation, use `--plan-only` to materialize prospective requests in new
directories without contacting a server. Check the source bytes, unchanged
profile instructions, complete six-pair coverage and revised register. Recreate
the old 24 requests with the default mode and compare their task IDs and request
bytes with the captured archive. The prospective plans are not observed calls.
Bind the new plans, code, models, runtime files and protocol before launching.

Run the two three-request cases sequentially on Studio using isolated loopback
workers. Preserve each case's exit status. A case with no admitted drafts does
not erase its responses or prevent the independent other case from being
attempted. Do not restart LM Studio, change the active detector fits or launch
more than one generation worker at a time. Keep the original source-relative
length and source-copy guards, recorded provenance and uncapped A-token audit.

Read every captured output against its full source before using detector scores
to evaluate this recipe. Judge realized conversational register, copying,
completeness, citation handling, changed claims and tonal intent separately.
An admitted draft with fidelity errors can remain an origin-training candidate;
it cannot demonstrate a style-only effect with meaning held fixed. This small
stochastic pilot cannot establish a causal prompt effect or general accuracy.
No new fit, Pangram call, threshold change or reference promotion is part of it.

The [completed review](results/scientific-profile-register-review-v1.json) covers
all six captures and their complete sources. Register changed unevenly, and all
six failed strict fidelity. Four passed automatic origin admission; two Mistral
drafts failed the source-copy guard. The copy audit independently confirmed both
rejections, with 3.86% and 34.21% of their words covered by matching runs of at
least 20 words. All nine complete source/draft inputs fit V6's token limit.
The [frozen diagnostic protocol](SCIENTIFIC_REGISTER_V6_DIAGNOSTIC_V1.md) retains
every capture, human control and admission outcome for scoring.

The [completed V6 diagnostic](SCIENTIFIC_REGISTER_V6_RESULTS.md) flags all four
admitted drafts at the strict threshold. No new draft scores below 10%; the
private B failure mode remains unresolved. The remaining calls stay paused.

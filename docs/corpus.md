# Corpus design and collection

We want to find editing choices that improve prose and preserve its meaning while reducing detector scores. Word frequencies and grammatical constructions give us interpretable measurements to investigate. They do not reveal Pangram's internal coordinates or establish that a word caused a detector decision.

## What exists now

The Rust collector supports explicit OpenAI/Anthropic API models and installed Codex/Claude CLIs. Direct API calls are not retried. The harness invokes each CLI once, records its events, and rejects model switches; the CLI can perform internal retries. The six prompts in `examples/prompts.jsonl` are small workflow examples, not a benchmark. Real source-conditioned model outputs and a human reference exist in the retained detector evidence. [Public dataset loaders](public-datasets.md) provide a separate path to scale.

The earlier 130 Pangram analyses in `experiments/pangram-preface` belong to **one source family**. They include repeated detector calls, fragments, and many closely related edits. Their generation provenance is insufficient for a clean per-model comparison. Use them to study detector repeatability and perturbations; do not count them as 130 independent texts or label them retrospectively as a particular frontier model.

## The sampling unit

Assign a stable `group_id` to an underlying source, prompt family, or writing assignment. Keep every version, paragraph, paraphrase, model response, and human response derived from that source in the same split. A random split of paragraphs from the same book would put much of the vocabulary and argument of the test material into training.

Choose `train`, `dev`, and `test` groups before generation. Use training for frequency estimates, the development split for choosing edits and thresholds, and a locked test set for the final claim. Reserve `exploratory` for unverified or experimental material. If you inspect test failures and revise the process, treat those examples as development material and collect a new test set. For an authorship comparison, also hold out authors and source publications where possible.

Store enough provenance to reconstruct a sample:

| Field | Meaning |
| --- | --- |
| `text` | Exact generated or source text, including whitespace |
| `corpus`, `source_kind` | Collection label and model, human, or unknown origin |
| `provider`, `model`, `model_requested` | API/CLI provider, observed model ID (or explicitly unreported requested marker), and requested ID |
| `domain`, `register` | Subject area and intended writing style |
| `group_id`, `split` | Source family and preassigned evaluation partition |
| `metadata` | Prompt, request settings, dates, IDs, usage, raw response, and hashes |

A product name such as Codex is insufficient model provenance. For a CLI-produced sample, record the product/version, resolved model if exposed, all applicable prompts and instructions, tool use, and context. If those details are unavailable, mark them unknown and keep that corpus separate from direct API generations. Direct API and interactive coding-agent outputs answer different questions.

Record documented provider watermark deployment separately from observed model identity and detector scores. Anthropic now documents text watermarking for specific models; older sample status may remain unknown. See [watermark provenance](watermark-provenance.md). Keep original generation records intact and avoid treating a low Pangram score as a watermark test.

## Matched comparisons

Start with a balanced set of writing assignments covering the domains and registers in which the editor should work. Give each model the same assignment and approximate length target. Sample several independent completions per assignment. Record sampling and reasoning settings, and do not silently replace an unsupported setting or unavailable model with another one.

For the human comparison, obtain original writing with credible provenance and permission to use it, ideally responses to the same assignments. Existing essays or books can help cover real writing, but track author, publication, date, license, editorial history, and any known AI assistance. A web page is not a verified human sample. A historical collection may avoid recent model assistance while introducing differences in period, subject matter, spelling, and editing.

Keep the task comparable. Contrasting technical model answers with human fiction mainly measures genre. Even within technical writing, cache documentation and philosophy will differ strongly in nouns. Estimate model differences within domain/register strata; then check whether they hold when an entire domain, author, or prompt family is excluded. Report sample counts and lengths for every stratum.

Preserve the original bytes separately from any analysis normalization. Record tokenizer and parser versions. Count all words using one documented convention, and use separate denominators for grammatical features. A passive-clause rate per clause cannot be compared directly with a word rate per token. Parse failures and unsupported languages are missing measurements, not zero occurrences.

## Frequencies and vectors

The simplest lexical profile is a count for each normalized word plus the total number of words counted. Keep document-level counts as well as totals so we can inspect concentration, deduplicate samples, and estimate uncertainty by source group. A word that occurs 100 times in one source provides weaker evidence of a general pattern than a word appearing across 100 unrelated sources.

Use smoothed rate ratios or log odds to rank candidate anomalies, with minimum occurrence and source-group counts. Show the raw counts and denominators beside every score. If a word occurs once in a reference corpus, a large ratio against that reference remains uncertain. Bootstrap complete source groups rather than pretending adjacent tokens are independent observations. Correct for, or explicitly describe, the many comparisons involved in searching thousands of features.

Build lexical and grammatical vectors separately before combining them. Track function words, content words, part-of-speech sequences, dependency relations, sentence lengths, and selected clause patterns. Normalize each family according to its opportunities. Fit feature selection and scaling on training data only. Evaluate whether grammar adds useful information beyond topic and length; do not assume a larger vector is a better one.

These distances describe our measured features. They are neither calibrated probabilities of authorship nor distances to Pangram's decision boundary. A rare word is an editing hypothesis only when a substitute preserves the intended claim and voice.

## Provider behavior

Rust's `collection::collect_batch` writes attributed corpus JSONL and keeps per-invocation records. `load_prompts` validates the full manifest, rejects duplicate IDs, and checks that one group does not appear in different splits before generation begins. Direct APIs use `OPENAI_API_KEY` or `ANTHROPIC_API_KEY` in the environment; the CLI adapters reuse existing sign-ins. Anthropic keys spanning multiple workspaces may also require `ANTHROPIC_WORKSPACE_ID`, as described in the [official authentication overview](https://platform.claude.com/docs/en/api/overview).

OpenAI requests use the Responses API with `store: false`. The collector reads assistant text from completed message items and rejects incomplete output and explicit refusals; the API can return multiple kinds of output items. See the [official OpenAI Responses reference](https://developers.openai.com/api/reference/cli/resources/responses/methods/create).

Anthropic requests use the Messages API and require a natural `end_turn`. The collector excludes thinking blocks from corpus text and rejects truncated, refused, tool-directed, and interrupted output. These distinctions follow the [Messages reference](https://platform.claude.com/docs/en/api/messages/create) and [stop-reason documentation](https://platform.claude.com/docs/en/build-with-claude/handling-stop-reasons). The request includes the documented [API version header](https://platform.claude.com/docs/en/api/versioning).

On failure, the collection directory retains the request record and error, captured CLI events, or any decoded API response received before sample validation. Keep refusals and failures in the denominator when reporting collection success; silently replacing them would bias the sample. Request headers and API keys are excluded from stored API provenance. A connection failure may occur after billing, so an uncertain or rejected invocation requires inspection before another submission.

## Testing editing hypotheses

First test one lexical edit, then one grammatical change, then combinations. Preserve exact candidate bytes and record the intended mechanism before scoring. Alternate unchanged controls and candidate calls in a randomized order. Keep the original and candidate in the same full-document context as well as testing any section in isolation.

Select edits on the development split. Evaluate a frozen process on new groups, with a fixed generation/query budget and repeated detector calls. Report all scores and the fraction of documents meeting the target, not just the best attempt. A single low score on one passage cannot establish reliable performance below 10%.

Require a claim-by-claim preservation check and a blinded readability/tone comparison before accepting a rewrite. Numbers, named entities, citations, qualifications, causal direction, objections, and the strength of a conclusion all matter. Embedding similarity can screen candidates but cannot certify that these survived. The preface experiment already showed why detector improvement alone is insufficient: a fluent substitution can strengthen a claim or invent an authorial intention.

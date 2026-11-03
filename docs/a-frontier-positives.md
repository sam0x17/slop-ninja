# Frontier-model positives for A

`examples/a_frontier_positives.rs` (omitted from the public copy) collects hosted frontier-model positives
for A's involvement, share and per-model attribution heads. It sends only public
Train-side passages, and Dev-iter, Dev-select or CAL passages once their prose
exists, through the providers' official command-line clients on the
collection host. Test, held, Train-mining and private rows fail its source
check. Hosted outputs are admitted for A training and evaluation under
`docs/a-hosted-output-admission.json` (decision of 2026-10-27). The records
form their own cohort, `hosted_frontier_v1`, with a declared weight policy (see
[Admission, labels and cohort weight](#admission-labels-and-cohort-weight)).
They are never used for evaluation selection.

```text
a_frontier_positives plan CONFIG.json SOURCES.jsonl VOICES.jsonl NEW_OUT
a_frontier_positives collect OUT LABEL [--limit N] [--shard I/N] [--retry-uncertain]
    [--guard-config PATH] [--profile cc_cohort_profile_v2 --profile-population POPULATION]
a_frontier_positives assemble OUT LABEL NEW_EXPORT
a_frontier_positives publish DEST PLAN_DIR...
a_frontier_positives withdraw-eval-authors OUT MANIFEST...
a_frontier_positives voices LIBRARY_DIR NEW_VOICES.jsonl
a_frontier_positives status OUT
a_frontier_positives guard-check [--guard-config PATH]
```

A collection is a series of immutable plans (waves). Each wave excludes the
source families of the earlier ones (`exclude_source_groups_from`), so a new
style library, new domains or the evaluation splits' passages enter as a new
wave while earlier stores keep resuming. `publish` exports every label across
all waves into `DEST/<label>/` and replaces each label directory atomically.

## Design

For each passage block, a model receives:

- one voice ladder: the same passage and model in 3-5 distinct one-line voices,
  as full rewrites or as the same span. The pilot alternated full and R3
  ladders; `ladder_bins` rotates the ladder operation per block (the scale run
  uses full, R1, R2, R3, R4 and R5, which spreads realised shares evenly);
- a plain mirror and a voiced mirror, which are from-scratch writing matched to
  the passage's topic, length and key terms;
- three exact-span rewrites in rotating bins R1-R5, some of them voiced;
- one plain full rewrite.

Voices are drawn by digest from cell keys that contain no model label. Every
model in a shared block therefore gets identical cells: the same passage under
the same voices across generators. Private blocks draw voices independently.
Without `voice_origin_balance` (below), voice is therefore independent of
generator by construction.

Spans use the sealed sentence splitter, word policy and TF-IDF key terms of
`grammar-a-positives` at `edd08682`, vendored verbatim. The bins and anchors
follow the local plan, with two simplifications:

- R5 keeps one whole sentence, at the start or the end;
- two-run spans are not used.

Every record is therefore a single request. Span rewrites see only the span.
Labels use the realised share: the final words in the replacement span divided
by all final words. A full rewrite is mixed history with share 1. Mirrors are
`model_only`.

The voice library is a JSONL file of `voice_prompt` or `brief` rows. Each row
carries its origin, its axis values and `held_out_style`. The plan excludes
held-out styles, items with a recorded evaluation-author conflict, and any
imitated author whose key matches an `author:` group or family key in the
manifests that the config lists as `eval_author_manifests`. That list must
name every evaluation and reserved split file the bundle assembler treats as
evaluation (all Dev-iter, Dev-select, CAL and Test manifests, including the
added domains, the reserved-unassigned groups and the diagnostic slices); the
plan does not discover them itself.

`withdraw-eval-authors OUT MANIFEST...` applies the same author rule to a
plan that already exists. It appends one row per affected request to
`OUT/withdrawn.jsonl`, recording the author key, the request's state at
withdrawal and the manifests' digests. It is idempotent and leaves the
plan's own files unchanged. `collect` never sends a withdrawn request and
marks a pending one `withdrawn`. Assembly drops a withdrawn request even if
it was accepted earlier, so the next `publish` removes its record and lists
it in `not-assembled.jsonl`. Its state and receipts stay as evidence.
`status` reports the withdrawn count per label. `voices` converts a compiled style library (accepted voice prompts
and briefs, with their cluster, genre, partition, `rewrite_safe` flag and, for
human-derived items, their source family) into this format, recording the
library files' digests in every row. A library item outside the `train`
partition is a held-out style.

Partitions come only from the frozen style partition file that the plan
config names as `frozen_partitions` (written by `slop_ninja_a_assemble
freeze-styles`; see `grammar/crates/grammar-a-assemble/README.md`). The plan
replaces each voice row's partition with the frozen one, drops voices the file
does not list, moves a voice whose normalised text equals an evaluation item's
text into that item's partition, and records the file's path and SHA-256 in
`plan.json`. A plan without `frozen_partitions` is refused.

A plan draws only the `train` partition by default. A held-out-style wave sets
`voice_partitions`, for example `["train", "dev_iter"]`. It may then cover only
passages of its own non-Train split; the plan fails on any other passage.
`exclude_voices_used` names earlier `voices-used.jsonl` exports. Any voice
already used for Train records is then dropped from the held-out partitions.

Draws are cluster balanced. A voiced cell first chooses a brief of the
passage's genre (probability `brief_share`, 0.35 in the scale run) or a one-line
voice prompt, then a cluster uniformly by digest, then an item in that cluster.
A ladder takes its voice prompts from distinct clusters. Rewrites use only
`rewrite_safe` items. A human-derived item is never applied to the passage or
source family it was derived from. Draw keys never contain a model label.

The optional `exclude_topic_substrings` list screens passages before sending.
It exists because provider bio safeguards refuse some benign science
abstracts (see the pilot below). `exclude_topic_substrings_by_transport` screens
a passage out of one transport's blocks only. Shared blocks pass every screen,
and a screened passage can still be a private block of the other transport.
The plan manifest records the screened counts. A Claude screen reduces the
biomedical coverage of the Claude labels.

Per-model `private_passages` overrides `private_passages_per_model`, so models
can receive different volumes while sharing the same core blocks.

**Collections and domains.** A source row's `collection` must be one of the
admitted public collections, matched by exact name in a fixed table, never by
prefix. The table gives each collection the domain that the Train split
manifest assigns to its documents, which is also the domain the bundle
assembler uses for humans and positives (`fineweb_blog_cc_historical` is
`web_blog`, `plos_blog_historical` is `science_blog`,
`wikipedia_talk_wikiconv_historical` is `wikipedia_talk` and
`stackexchange_historical` is `stack_exchange`). Each domain names its own
register in mirror prompts. Plans made before the table spelled the two blog
domains `fineweb_blog` and `plos_blog`. Their requests keep those names, and
the assembler takes a positive's domain from its parent document. Passages are
handed out round-robin across domains in table order, one per source family.
With the optional `domain_request_targets` (`{domain: requests}`, one positive
target for every domain that has eligible passages), each block instead goes
to the domain whose planned requests, counting the block, are the smallest
fraction of its target. Domains therefore fill in proportion from the first
block, and a shared block (one block per model) lands in a domain whose target
can absorb it. `plan.json` records eligible passages and planned requests per
domain.

**Voice origin across waves.** Within one plan without a balance, every model
draws from the same voices, so voice origin (invented, human-derived, the
hand-written pilot seed) is independent of generator. Across waves it is not: the pilot and smoke waves
used only the pilot seed, wave 1 only invented library voices, and models
received different volumes per wave. On the 3,247 records published on
2026-10-27 at 14:49 EDT (2,120 of them voiced), bias-corrected Cramer's V
between generator and voice origin is 0.117 (pilot seed: 29-46 voiced records
per model; invented: 84-217), although each plan drew from a single origin. The trainer must not let origin stand in for
generator, so later waves set `voice_origin_balance`:

```json
"voice_origin_balance": {
  "shares": {"invented": 0.55, "human_derived": 0.40, "pilot_seed_v1_hand_written": 0.05},
  "prior_records": ["<S>/data/positives-frontier"]
}
```

Every voice in the plan's voices file needs an origin listed in `shares`, and
every listed origin needs a usable voice (concatenate the pilot-seed file with
the library snapshot to keep that origin). A draw then stays within one origin.
Shared-block cells choose the origin by digest from `shares`, so every model
still gets identical shared cells. A private-block cell takes the origin with
the largest deficit for that model: the target share of its voiced total, counting
its voiced records in `prior_records`, minus its count so far. A ladder takes one
origin for all its rungs. If an origin has no eligible voice the next one is
used. `plan.json` records per-model voiced requests by origin, their Cramer's V,
and with a balance the prior and projected counts and Cramer's V. Without
`voice_origin_balance` draws are exactly those of earlier builds. The balance
works on requests; refusals, rejections and unfinished requests move the
realised mix, which `coverage.json` reports.

A balanced plan offsets earlier waves by giving each model a different
private-block origin mix, so its own requests are associated with generator,
and the projected cumulative value holds only once every model's requests in
that plan are collected. Wave 2 (planned 2026-10-28) illustrates the risk: its
own voiced requests have bias-corrected V 0.18 and the projection over prior
records plus requests is 0.033, but if the GPT-6 Astra requests that carry
most of its human-derived voices stay uncollected while that model's older
invented-only requests finish, the cumulative value is about 0.15. Collection
should therefore run a model's balancing requests before older unfinished
requests of the same model, and training should check the realised value in
`coverage.json` rather than the plan's projection.

## Evaluation-split plans

`plan-eval EVAL_CONFIG SPLIT_HUMANS VOICES NEW_OUT` writes a plan for one
evaluation split (Dev-iter, Dev-select, CAL or, after the seal, Test) by the
protocol's mechanical frontier rule (`protocol-v1.json`
`positive_plan.frontier`). Ranks use the protocol plan keys within the split
and the evaluation domain, so `SPLIT_HUMANS` must hold every human of the
split.

- **Who and what.** A human gets one item when `rank(k_frontier) mod 4 == 0`.
  The model is roster slot `rank(k_frontier_gen) mod L` (roster labels
  ascending). `rank(k_frontier_kind) mod 2` picks a mirror or a mixed
  rewrite, whose bin is `[R1..R6][rank(k_frontier_bin) mod 6]` under the span
  rules above (one run; R5 keeps the first or last sentence; R6 rewrites the
  whole passage).
- **Voice.** Voiced unless `rank(k_frontier_voice) mod 4 == 0`: one digest
  draw from the split's frozen voice partition, without voices used in
  another split's records or imitating an evaluation author.
- **Prompts.** The templates above with the protocol register of the
  evaluation domain; mirror terms take IDF from that domain's Train pool.
- **Acceptance.** The protocol checks replace the Train-side screen:
  non-empty, no chat-template special tokens, words within [0.5, 1.5] of the
  target, not byte-identical to the replaced span, not a duplicate, and the
  assembled document within 2,048 A-tokenizer tokens. A Claude call must
  report the roster model. At most two attempts are made; rate-limit and
  quota stops are not attempts. A refusal or a second failure is terminal.
- **Unavailable models.** `close-label OUT LABEL REASON` makes that label's
  open items `terminal_unavailable`. `eval-status OUT` exits 0 only when
  every item is accepted or terminal.
- **Test.** A Test plan names the seal file and its SHA-256; planning,
  collection and assembly refuse when the file is missing or differs.

## Form profile in acceptance

`collect --profile cc_cohort_profile_v2 --profile-population P` adds protocol
A2's profile v2 (§6.2, as amended by amendments 7 and 10) to acceptance, for
Train-side and evaluation plans alike. Without `--profile`, or with
`--profile raw`, acceptance and receipts are unchanged; the v1 profiles are
refused.
- After the plan's own acceptance, the response is laid out against its
  passage with `grammar_a_form::profile::child_v2`: the layout transfer, the
  form screen when the passage (or the replaced span) passes it, and the echo
  screen (`echo::screen_item`, raw and rendered). The window
  `cc_final_length_window_v1` (150..=450 whitespace words) then runs on the
  final document: the laid-out mirror, or the passage with the laid-out span.
  Any reason rejects the attempt, which retries within the attempt limit.
- Conventions come from the passage's own majorities, then keyed draws from
  the prior of the plan's passages, in the passage's domain cell, then
  plan-wide, under P's seed `norm_prior_seed(P)`. The draw's document key is
  the request id (`FORM_KEY`, `form_key`); the prior's source is
  `FORM_PRIOR_SOURCE`. The frontier re-verdict uses the same key and prior so
  that its verdicts agree with collection's. The plan's passages must share
  one split. P must be on amendment 10 item 10.8's closed list,
  `grammar_a_form::profile::R2_POPULATIONS`.
- The receipt keeps the raw text and adds `form_profile` (the profile's ids,
  P, its seed, the prior's digest, the key and the prior's source) and
  `form_check`, the attempt's own record for every output, accepted or not:
  the conventions found and applied, the layout class and measured
  properties, the transfer, the form screen's and the echo screen's outcomes
  (each comparison's target, raw and rendered), the window with its document
  and word count, the key, the passage, its domain and its convention counts.
  No record holds text. Assembly re-audits the plan's own acceptance; laying
  out assembled records belongs to the frontier re-verdict.
- **Binding.** Before its first attempt, a v2 run writes the prior record
  (`OUT/runs/form-prior-<sha256>.json`, the exact bytes of the digest) and the
  label's binding (`OUT/runs/LABEL/form-profile.json`), mode 600. A v2 run is
  refused when the label is bound to another record, when another label of
  the plan is (the plan is one batch), or when the label already has requests
  collected with the profile off; a run with the profile off is refused on a
  bound label.

## Frontier re-verdict

`renorm OUT NEW_DEST --profile cc_cohort_profile_v2 --profile-population P`
re-verdicts the plan's frontier records under profile v2 with no model call
(protocol A2 §4.3, B4, with amendment 20's readings). It invokes no CLI and
writes a new directory (files mode 600, directories 700).
- A plan whose manifest, passages or requests name old Dev-select or the
  consumed Test (`dev_select`, `test`) is refused before any of its lines is
  parsed: each line's `frozen_split` or `split` is read from its bytes first.
- The records are the plan's export rows under the current rules
  (`assembled_rows`, label by label, as `assemble` builds them).
- Each record's accepted response is judged as `collect --profile` judges an
  output: the plan's own acceptance (re-audited), then the profile on the
  receipt text trimmed, with the same key (`form_key`, the request id) and
  prior (`FORM_PRIOR_SOURCE`), so the verdicts agree with collection's.
- `reverdict.jsonl` holds one record per frontier record (no text).
  `NEW_DEST/LABEL/records.jsonl` and `NEW_DEST/LABEL/manifest.json` hold the
  records that pass, laid out (the laid-out mirror, or the passage with the
  laid-out span, share evidence recomputed), with `form_profile_v2` naming the
  raw response's digest and the raw record's text digest and span. Each label
  directory is an export directory with `assemble`'s manifest, so the bundle
  assembler's `frontier_exports` reads `NEW_DEST` with the label as generator
  key. `recollection-plan.jsonl`, keyed by request id, holds the records that
  fail, for Stage 0-H: `recollect` while `collect`'s attempt count (rate
  limits excluded, at most two) leaves an attempt, otherwise
  `drop_out_of_attempts`. The prior record and `RENORM.json` complete it.
- **R-form** (protocol A2 §7.2, amendment 19; B3b). Beside each label's
  export, under names that no export reader takes for N-form records, and
  with the N-form files unchanged:
  - `NEW_DEST/LABEL/raw-twins.jsonl`: each passing record's raw twin, the
    assembled raw export row (the receipt text trimmed, as the mirror or in
    the passage at the request's span) with the N-form record's labels, id
    `rform:<id>`, `twin_of` and the N-form document's window verdict. It is
    rebuilt from the passage and the raw response and refused unless the
    receipt's `text_sha256`, the row's `replacement_sha256` and, for a
    receipt collected under the profile, its `form_check`
    `raw_response_sha256` all name that raw response.
  - `NEW_DEST/LABEL/screen-fail.jsonl`: `rform_screen_fail_v1`, one document
    per request whose first raw response failed the form screen only, read
    from the attempt receipts and window-checked; its labels are an export
    row built from that attempt's receipt.
    `NEW_DEST/LABEL/screen-fail-left-out.jsonl` notes, without text, the
    documents left out (window, assembly, a lost event stream or a receipt
    text that differs from its events).
  - A raw response is an attempt with status `accepted`, or
    `rejected_output` with exit code 0 (protocol A2 amendment 25 item
    25.1). The CLI's exit status is the frontier transport's response
    status, as item 19.1 reads an HTTP status: an attempt whose CLI did not
    exit with 0 is no raw response, whatever text its receipt holds. That
    covers `failed` (a text that passed the screen from a CLI that exited
    non-zero included), `rejected_output` with a non-zero or missing exit
    code, `uncertain_timeout` and `uncertain_interrupted`. `refused`,
    `not_generated_rate_limited` and `in_flight` are not raw responses
    either. Every attempt with a receipt must have its state's status. A
    request's first raw response is its lowest-numbered one, judged as
    recorded: the record response by this re-verdict, a `rejected_output` by
    its receipt, which fails the form screen only when its `form_check`
    reasons are all form-screen codes (`form_line_*`, `form_paragraph_*`,
    `form_frame_*`) and its `screen_error` is exactly the profile's rejection
    of them, so the plan's own acceptance (`screen_text`, with
    `eval_acceptance` for an evaluation plan) passed (item 25.3). A receipt
    collected with the profile off never does. A withdrawn request has no
    R-form document, as assembly drops its N-form record (item 25.2).
  - The R-form documents of one run are unique by text; a duplicate refuses
    the run. N0's and S0-13's positives take the raw twins only; the stratum
    enters only the R-form evaluation populations.
- Stage 0-H re-collects each `recollect` row for its own request id with this
  run's prior record, keyed by the request id, continuing from
  `attempts_used`. `collect --profile` refuses a label that already holds
  profile-off requests and starts a new plan's attempts at zero, so no
  collector mode does this yet; one must be reviewed and landed before any
  Stage 0-H call.

## Transport

Every call uses `medium` reasoning effort, and the effort is recorded per
record. OpenAI models are reached through the Codex CLI and Claude models
through the Claude CLI, each with its installed client configuration.

**Private config.** `collect` reads its call guard and the optional call-log
settings only from a private JSON file, named by
`--guard-config` or `A_FRONTIER_GUARD_CONFIG`. The file, its schema
and its description live in ignored private storage. The tracked code has no
defaults for these settings, plans do not carry them, and `collect` rejects any
other option. Each receipt and collect log records the config's SHA-256.
`guard-check` loads the config and runs the guard check without calling a
model. A collector built from main reads config schema v4 and refuses earlier
configs, whose keys have earlier names; the sealed Test builds read `v1`. The
config refuses unknown keys.

**OpenAI.** Calls run as `codex exec` with the installed Codex client
configuration; the collector passes no provider, endpoint or model-catalog
override. Only per-request
controls are set: model, medium reasoning effort, instructions, disabled
features and the sandbox. Calls run in an ephemeral, read-only sandbox, with
the shell, apps, browser, computer-use and goals features disabled and web
search off. `model_instructions_file` holds a one-line writing instruction
instead of the coding-agent prompt.

`codex_transport = "lean-v2"` also disables the remaining optional features
and turns off the project-document, skills, permissions, collaboration-mode,
apps and environment instructions. The default model catalog still declares
the model's own client-side tools. In the first requests on 2026-10-28 that
added about 3,900 input tokens per request (median overhead beyond the prompt
about 4,300 tokens, against about 420 with the earlier trimmed catalog). Plans made before this rule wrote a
trimmed `codex-model-catalog.json`; `collect` still checks that file's hash but
no longer passes it to the CLI.

Codex events report no resolved model, so records carry the requested selector
as their label (`requested:<selector>`, basis `requested_selector`; see
[Admission, labels and cohort weight](#admission-labels-and-cohort-weight)). When
the private config names a call log, the collector keeps the matching logged
call with the attempt. It confirms the logged model and
`reasoning_effort=medium` and gives a per-call list-price cost estimate.

**Call guard.** Before every call the collector runs the guard check that the
private config describes, and keeps its snapshot privately. When the check
does not allow a call, `collect` stops with exit 75, or with exit 76 when the
check cannot be read. The CLI's own stop flags also stop the run.
The rate readings that the CLI reports with each call become diagnostics
in the collect log and never stop the run. A request rejected with a rate limit
ends only that run with exit 73. Without a configured guard check, per-call
maxima for those readings apply.

**Claude.** Calls use `claude -p --safe-mode --tools "" --strict-mcp-config
--no-session-persistence --max-turns 1 --system-prompt <same instruction>
--effort medium` with stream-json events. The reported `assistant.message.model`
is the identity. A `system/model_refusal_*` event, or output from more than one
model in `modelUsage`, is a refusal and never the requested model's prose.
The CLI can retract a refused turn and answer with a fallback model.

**Retained per attempt.** Each attempt keeps the exact prompt, argv, raw stdout
events, stderr, CLI versions and paths, timings, usage, list-price cost
estimate, guard snapshot, the private config's hash and the collector's
source hash. Receipts written from 2026-10-30 carry the schema
`slop_ninja_frontier_attempt_receipt_v2` and keep the joined call-log rows
under `request_log`. Earlier receipts have no schema field and an older key for
those rows, and they stay as written. `assemble` copies the rows into the
record under the receipt's key, and it re-parses the raw events
under the current rules. It excludes any record that an earlier collector build
accepted but that now fails, such as a refusal fallback.

## Stopping and resuming

`collect` walks a label's requests in plan order and skips every request
already in a terminal state:

- `accepted`;
- `terminal_rejected`: two mechanical rejections;
- `terminal_failed`: two transport failures;
- `terminal_refused`;
- `skipped_passage_refused`;
- `withdrawn`: listed in the plan's `withdrawn.jsonl`, never sent;
- `uncertain`: a timeout or interruption, retried only with
  `--retry-uncertain`.

An attempt interrupted after its intent was written is reconciled from its
captured events. Exit codes:

| Exit | Meaning |
|---:|---|
| 0 | Done, or a `STOP` file is present |
| 73 | One Codex model is rate limited, or one Claude request was rejected with a guard check configured; other work can continue |
| 75 | Claude rate limit without a guard check, or the guard check stopped collection. Rerun later and the collector resumes |
| 76 | Outage: the guard check could not be read four times, or three consecutive transport failures |
| 77 | `--limit` reached and unfinished requests remain |

`--shard I/N` restricts a run to the requests whose id digest is `I` modulo `N`.
Several workers can then share one label without touching the same request.
Refusals are recorded one file per passage and transport, created atomically,
so concurrent workers never lose one.

Collection stops on provider rate limits and on the privately configured
guard, which is checked before every call. A rate-limit rejection produces
no text and is retried on the next run.

**Safeguard refusals.** These are deterministic for the passage. After the first
refusal of a passage on a transport, the remaining requests for that passage on
that transport are skipped. This avoids sending a flagged passage repeatedly
through the same CLI.

Acceptance requires all of the following:

- non-empty prose;
- no control tokens or code fences;
- no leading preamble;
- a word count within [0.5, 1.5] of the target;
- no duplicate output;
- for rewrites, a change from the replaced span.

## Pilot, 2026-10-27

**Setup.**
- Plan `pilot-v1`: six public Train passages (PLOS ×2, Gutenberg fiction,
  Federal Register, Wikinews, Wikipedia), all shared by every model, 60 requests
  per model.
- Voices: a hand-written pilot seed of 32 one-line voice prompts and 6 briefs,
  used because no style library output existed yet.
- Collector build v1, with later builds resuming the same store. Codex CLI
  0.163.0 and Claude Code 2.1.311, both at medium effort.
- Workers ran concurrently: four Codex and two or three Claude.

**Models.** The pilot covered these generally available models:
- Codex CLI: `gpt-6-astra`, `gpt-6.1-sol`, `gpt-6.1-terra`, `gpt-6-sol`,
  `gpt-6-luna`, `gpt-5.6-sol`, `gpt-5.6-terra` and `gpt-5.6-luna`;
- Claude CLI: `opus` (`claude-opus-5-5`), `sonnet` (`claude-sonnet-5-5`),
  `haiku` (`claude-haiku-5-5`) and `fable` (`claude-fable-5-1`).

No Gemini CLI or other provider is installed.

Rates are accepted records per minute of wall time from a label's first to its
last attempt. The last column is a list-price cost estimate per accepted record,
from the call log's `cost_usd` or Claude's `total_cost_usd`.

| Model | Assembled / 60 | Rate (per min) | Mean latency (s) | Input tokens (cached) | Output tokens (reasoning) | List-price cost estimate per record ($) |
|---|---:|---:|---:|---:|---:|---:|
| gpt-6-astra | 60 | 2.6 | 21.5 | 9,942 (1,515) | 250 (12) | 0.098 |
| gpt-6.1-sol | 60 | 3.9 | 14.5 | 9,929 (3,162) | 271 (34) | 0.017 |
| gpt-6.1-terra | 60 | 8.2 | 7.0 | 9,877 (3,027) | 235 (1) | 0.008 |
| gpt-6-sol | 60 | 5.7 | 10.2 | 9,868 (3,379) | 382 (161) | 0.017 |
| gpt-6-luna | 60 | 3.3 | 9.2 | 9,912 (1,726) | 228 (0) | 0.001 |
| gpt-5.6-sol | 60 | 1.1 | 36.6 | 9,760 (2,948) | 387 (153) | 0.037 |
| gpt-5.6-terra | 60 | 1.3 | 37.6 | 9,607 (2,671) | 310 (90) | 0.019 |
| gpt-5.6-luna | 60 | 0.7 | 12.9 | 8,053 (1,173) | 301 (66) | 0.002 |
| claude-opus-5-5 | 49 | 3.6 | 8.2 | 1,157 (16) | 507 (89) | 0.020 |
| claude-sonnet-5-5 | 48 | 3.7 | 9.1 | 1,091 (0) | 443 (42) | 0.009 |
| claude-haiku-5-5 | 48 | 2.1 | 6.5 | 1,109 (0) | 929 (538) | 0.0007 |
| claude-fable-5-1 | 46 | 2.9 | 10.0 | 1,126 (14) | 490 (60) | 0.047 |

Two wall-time rates include stops:
- gpt-5.6-luna: about 70 minutes of a per-model `turn_rate_limit_exceeded` stop.
  Its 60 records took about 16 minutes once it resumed.
- gpt-5.6-sol: a 10-minute outage stop.

**GPT failures.** All 480 GPT requests were accepted. gpt-5.6-luna's rate limit
appeared to the CLI only as "stream disconnected" after five internal
reconnects. The call-log join identified it, and the run stopped with
exit 75 and resumed later without loss. A transient timeout of the status
query stopped one run; that query now retries.

**Claude failures.**
- **Safeguard refusals.** All four Claude models refused one PLOS ONE abstract
  on influenza-virus immobilisation (`[bio]` safeguard). Opus and Fable
  sometimes fell back silently to `claude-opus-5`, and five such outputs were
  accepted by the first build. Re-audit excludes them, and the collector now
  treats fallbacks as refusals and skips the rest of a refused passage.
- **Length.** Voiced full rewrites ran past 1.5 times the source length: 3 to
  10 rejections per model, most from voices such as letters or speeches. Codex
  outputs had none. Plans can now set `rewrite_word_target` (prompts v2) to
  state the target length.
- **Preamble.** One Haiku output began with a preamble.

**Share coverage.** Every exported record has an exact replacement span. Each
GPT label covers mirrors, full rewrites and realised shares in [10,25),
[25,50), [50,75), [75,90) and [90,100). The [50,75) bin is heavy because of the
R3 ladders, and no record falls in (0,10).

**Concurrent work.** Other jobs ran at the same time as the pilot, so the
rates are not isolated throughput.

## Exports

`publish DEST PLAN_DIR...` writes, for each label:

- `records.jsonl`: assembled records, re-audited under the current rules and
  deduplicated by replacement text across waves;
- `failures.jsonl`: every non-accepted attempt, with its status, error,
  category and retained receipt directory;
- `not-assembled.jsonl`: requests without a record and the reason;
- `coverage.json`: counts by operation, requested and realised share bin,
  voice kind, origin and axis, split and domain; distinct voice clusters; and
  the share of passages with a ladder of at least three accepted rungs;
- `manifest.json` and `SHA256SUMS`.

`DEST/coverage.json` adds bias-corrected Cramer's V between generator and voice
cluster, voice axis, voice origin, and operation × bin, over all records, plus
generator × voice origin within each plan and the voiced records per generator
and origin. Generators are keyed by their canonical attribution label
(`ai_attribution_label`). Values near zero mean voice is independent of generator. `DEST/voices-used.jsonl` lists every voice
used, with its library partition at generation time and its records per split.
A later held-out-style slice can therefore exclude any style already used for
Train records, even if a later library build moves its cluster.

`publish` deletes the `admission-labels.jsonl` and `admission-labels.json`
files that earlier builds wrote; every record now carries its admission and
label itself.

## Admission, labels and cohort weight

**Record schema.** Every assembled record is
`slop_ninja_frontier_positive_v2`. Plans made by builds with the admission
module record `record_schema`, the basis and the cohort weight policy in
`plan.json`, and their records take those. Plans made earlier (pilot-v1,
smoke-v6, wave1) have no `record_schema`. Their records take the basis of the
manifest bytes they were collected under (the manifest's `predecessor`
digest) and the current weight policy, and `ai_identity_detail.migrated_from`
keeps the v1 `ai_identity` and `ai_identity_basis` they replace. Text, spans,
generation fields and receipts are unchanged.

**Admission basis.** A v2 record carries, at the top level and under
`rights.training_admission`, the object that `a_hosted_admission check`
accepts: `schema: slop_ninja_a_hosted_training_admission_v1`,
`manifest_schema: slop_ninja_a_hosted_output_admission_v1`, the manifest path
and its SHA-256, the decision date, permitted uses, cohort, weight policy id
and `reasoning_effort: medium`. `rights.commercial_training` is `true` on that
basis, `rights.provider_output_rights_reviewed` is `false`, and
`rights.source` keeps the passage's own rights. The basis is the one recorded in
the plan (or the predecessor digest for plans without one), so a later manifest
edit does not change a plan's records. An export manifest cites its records'
common basis, or each distinct basis with its record count.

**Labels.** One vocabulary across hosted cohorts, as in the admission
manifest's `identity_policy`: the reported prose model when the CLI reports one
(`ai_identity_basis: reported`, for example `claude-opus-5-5`), otherwise
`requested:<selector>` (`ai_identity_basis: requested_selector`, for example
`requested:gpt-6-astra`, the same label as Astra's records in
`frontier-captures-v1` and `hosted-style-v1`). v2 records carry
`ai_attribution_label`, `ai_identity_basis`, `ai_identity` (the reported model
or null), `ai_identity_supervision_available` and `ai_identity_detail` with the
raw `transport`, `model_requested` and `model_reported`; `generation` keeps
`selector_requested` and `model_reported` as captured. The v1 names map one to
one: `cli_reported_model` is `reported` and
`requested_selector_no_fallback_reported` is `requested_selector`; v1
`ai_identity` values `codex-cli:<selector>` become `requested:<selector>`.
Labels are computed from the captured transport, selector and reported model,
never from the v1 fields.

**Cohort and weight.** Every record carries
`cohort_id: hosted_frontier_v1` and `cohort_wave` (the plan directory name).
The declared policy `hosted_frontier_label_balanced_v1` sets the default weight:
each attribution label in the cohort gets the same total weight. With uniform
other factors this is `w = N / (L × n_label)` over the cohort records a run
admits (N records, L labels, n_label records of the label), which the record's
`cohort_weight_policy.default_rule` states. The assembler applies the policy on
the final Train weights, after domain, origin, family and cohort factors (see
`grammar/crates/grammar-a-assemble/README.md`, Weights), so the label totals are
equal in the weights a trainer uses. A trainer may
override it, including weight 0 to remove the cohort, but its run manifest must
record per cohort and label the count and the weight used, and whether it is
the default; a run that uses the cohort without that entry is invalid.

Check a label with:

```text
a_hosted_admission check docs/a-hosted-output-admission.json DEST/<label>/records.jsonl
```

## Collection drivers

`tools/a-comp-tierc/frontier-route-driver.sh` (omitted from the public copy) collects through one provider CLI
(`codex` or `claude`) as a durable user unit, with the private config named by
`GUARD_CONFIG`. It reads a job list of `PLAN_DIR LABEL SHARD` lines. An
optional `after=PLAN_DIR2` field holds a job until every line of that plan with
the same label is done or broken, so a wave's balancing requests can run before
an older wave's unfinished requests of the same model. Lines can be appended
while the driver runs.

Each worker runs one `collect` chunk of 25 requests, then takes the runnable job
whose last chunk started longest ago. The next step depends on the exit code:

- 0: one `--retry-uncertain` pass. The job is marked done only when that pass
  exits 0; otherwise the driver logs its exit code and the job stays open;
- 73 on the Codex side: that label backs off 2-5 minutes with jitter. The
  backoff doubles for each consecutive run that accepted nothing, up to
  15 minutes;
- 73 on the Claude side: that job backs off 30 minutes;
- 75: the whole provider backs off (`PROVIDER_BACKOFF`, one hour by default)
  and then runs its guard check again;
- 76: the provider backs off 10 minutes;
- anything else is a crash. The job backs off 30 minutes, and three crashes
  park it in `broken/`.

A `STOP-codex` or `STOP-claude` file stops that side's workers between
chunks.

## Wave 1, 2026-10-27

**Plan.**
- Voices: an interim compile of the style library, with 662 Train-partition
  voice prompts and 1,066 Train-partition briefs from two inventor families.
- Blocks: 30 shared blocks for all 12 models. Private blocks: Astra 200, the
  other GPT models 10 each, Haiku 20 and Sonnet 15.
- Requests: 6,650. Every request uses a Train passage, and the passages come
  from seven domains, including FineWeb and PLOS blogs.
- Claude screen: virus, pathogen, toxin and similar terms.
- Workers: 4 on the Codex side (Astra in two shards, every other model one
  worker at a time) and 2 on the Claude side.

**Rates and cost, first two hours.** Codex ran about 17 accepted records per
minute over 4 workers, and Claude about 7 per minute over 2 workers. The mean
list-price cost estimate per record was Astra $0.020, gpt-5.6-sol $0.018,
gpt-5.6-terra $0.010 and gpt-6-sol $0.005. The other GPT models were below
$0.005. Claude records cost Fable $0.040, Opus $0.025, Sonnet $0.008 and Haiku
$0.001.

**Coverage.** Generator against voice cluster, voice axis and operation × bin
all have a bias-corrected Cramer's V of 0.0. 98% of passages carry a ladder of
at least three accepted rungs.

**Failures.**
- Length: 6 Haiku outputs ran past the 1.5x length bound.
- Preambles: 5 Claude outputs began with one.
- Refusals: one Opus refusal fell back to another model, and the rest of that
  passage was skipped on the Claude side.
- Timeouts: two Astra calls timed out as `uncertain` during a provider outage of
  about 10 minutes. They are retried at the end of the label.
- One Fable call reported tools or MCP servers as not disabled. It was retried.

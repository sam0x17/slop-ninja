# Generator attribution and broader model coverage

Treat generating models as identifiable author classes, separately from human
authorship, prompt personas and Pangram scores. A detector score is an annotation
of the text; it does not determine which model produced it.

The current Train annotations cover original Qwen, Mistral and OLMo drafts,
human-text edits, and Qwen/Mistral revision chains. Expand coverage with GPT and
Claude, collecting hosted model outputs through the providers' official
command-line clients. Keep model variants
separate whenever the recorded identifiers distinguish them. Pangram spend is
recorded in the private ledger. Give further Train annotations to broader
generators before collecting more of the original three.

## Labels and evidence

Every annotation can carry `slop_ninja_generator_attribution_v1` metadata:

- Requested model, reported model, declared revision, runtime and quantization.
- A generator label derived from the recorded model identifiers, independent of
  prompt style. Keep any later alias reconciliation in an explicit registry.
- Source family and full ancestry, ordered from source root to final text.
- Original draft generator, final model stage, and every intervening model step.
- Operation, prompt hash and profile, generation settings, timestamp, and hashes
  of the archived request and response.

Keep labels for original generator and final reviser independently. A GPT draft
revised by Claude remains a two-model workflow; it is not a clean Claude-only
authorship example. A model edit of a human source keeps its mixed-origin label.
Requested personas and style instructions are covariates, not new authors.

The `attribution` module derives this information from the existing validated
corpus. New annotation plans include it directly. `annotation_weights` also
derives it for archived observations without rewriting old plans or receipts.

Hosted aliases do not establish immutable model weights. Capture the requested
alias, any model identifier actually returned by the service, CLI version and
collection date. Leave unexposed temperatures, seeds, token limits and snapshot
identifiers unknown. In particular, a successful Codex CLI capture does not
justify inventing a provider snapshot or treating a requested temperature as
an observed setting. Hosted and frontier outputs are admitted for A training
and evaluation under the [hosted admission manifest](../../docs/a-hosted-output-admission.json).
Retained captures are admitted by exact record hash; new collections record
their admission basis when they are prepared. Hosted records form their own
cohort with explicit training weights, so their influence can be measured and
the cohort removed. A reported prose model is the attribution label; when only
a requested selector is known, the label is `requested:<selector>`, reported
as its own identity basis rather than as a resolved snapshot. New hosted
generation uses medium reasoning effort, recorded with each request.

## Collection and evaluation

Use the same source/style combinations across providers. The next source set
contains 24 previously unannotated Train families: one per source collection and
prompt profile, selected with the existing deterministic sampler and exclusions
for all four Pangram cohorts. It contains 72 existing seed records, SHA256
`a956aad4715d76975ec8e884475d8c396d44af3e490cc257902c5571410c3ddf`.
Preserve each CLI's exact input, output, completion status and usage. Disable
unrequested tools and custom context; keep any unavoidable harness differences
in the runtime metadata. Preserve unsuccessful and rejected attempts.

Train a future attribution head on generator identities and report family-level
and variant-level results separately. Split by source family before fitting:
all provider versions of one source belong in the same partition. Evaluate on
held-out topics and prompt profiles as well as held-out source families, so a
model cannot earn attribution accuracy from a provider-specific subject or style
instruction. Keep cross-model revisions as their own evaluation task.

The current expansion remains adaptive Train collection. It does not change
the frozen v6 fit or its final Test procedure. A score-balanced weighting view
retains every observation, gives equal total weight to each observed origin,
then to each occupied score bin within that origin. Publish the cell counts and
effective sample size alongside it: weighting rare bins supplies no additional
independent examples. This view is prepared for a later declared experiment;
it is not evidence of an improved fitted model.

CLI interfaces were checked against [Codex non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode)
and [Claude's CLI reference](https://code.claude.com/docs/en/cli-reference).

The matched capture run started on the Studio on 2026-09-12. It requests 24
drafts each from `gpt-6-astra`, `gpt-5.6-sol`, Claude `opus` and Claude `sonnet`
over the same frozen sources and profiles. The runner passed its parser tests,
format check and focused Clippy check before starting. The repository's full
Rust tests and all-targets Clippy check also passed. Capture
counts, successful completion and corpus admission remain separate milestones.
This job makes no Pangram calls. The earlier interface probes are retained
separately and do not replace any member of the matched cohort.

`audit_cli_captures` reviews these archives offline. It replays the recorded
stdout through the shared CLI parser, checks request/output/log hashes and
compares the resulting prose and generator metadata with each capture. It
also verifies the invoked model and exact source/profile prompt against the
original cohort, including source subsets used when resuming a run. The
expected source/model matrix rejects duplicate requests across run directories.
Keep the earlier completed captures when combining a resumed collection.

The text checks use the existing draft policy: 80-800 lexical words, between
half and twice the source length, no reasoning or code-fence wrapper, a changed
text hash, and the existing 20-word-span/12-word-sentence source-copy check.
Every completed capture remains in the review, including those that fail a
text check. Failed or unfinished attempts and missing matrix members are
reported separately. `--allow-partial` permits a clearly marked interim view;
the default requires the full requested matrix. These checks do not assess
meaning, tonal fidelity or readability. Training admission comes from the
hosted admission manifest, not from these checks.

For the current Studio collection, run the auditor with all three capture roots,
the original 72-record source file, and the four expected requested-model
identifiers. Keep its output under ignored `data/` or the remote run directory.
The compiler optimization change after the first three GPT-6 captures does
not redraw those requests; the resumed source subset excludes those families
for GPT-6 alone. Other models still receive all 24 original source/profile
combinations.

The third capture directory stopped during Claude version discovery, before
issuing any Claude generation request. The fourth directory continues those
unissued requests and retains all 48 completed GPT captures from the earlier
directories. Future collectors save version stdout, stderr and exit status
before requiring a successful version check; a preflight failure remains
distinct from a failed model request.

```sh
audit_cli_captures --sources source-records.jsonl \
  --capture-root matched-cohort-v2 --capture-root matched-cohort-v3 \
  --capture-root matched-cohort-v4 \
  --expected-model gpt-6-astra --expected-model gpt-5.6-sol \
  --expected-model opus --expected-model sonnet --output-dir capture-review-v1
```

Under the project's publication policy, these hosted outputs may be published,
each naming its provider. The project admitted them for A training and
evaluation on 2026-10-27; the
[hosted admission manifest](../../docs/a-hosted-output-admission.json) records
that decision by exact record hash. Source-text licenses remain separately
preserved. The archived reviews keep their capture-time
`provider_output_rights_reviewed` and `commercial_training_admitted: false`
fields unchanged.

## Completed matched collection

The [capture report](results/frontier-captures-v1.json) records all 96 outputs:
24 each from GPT-6, GPT-5.6, Claude Opus and Claude Sonnet. Every requested
source/model pair completed once, and all 96 outputs passed the archive and
mechanical text checks. No generation request was repeated during the resumed
collection. The raw text totals 129,060 bytes, with 83-326 lexical words per
passage. Each source collection contributes 24 captures; each prompt profile
contributes 16. All 24 source families remain Train.

Claude's prose messages identify `claude-opus-5` and `claude-sonnet-5`.
Helper models present only in usage accounting are retained as usage metadata,
not recorded as the prose author. The Codex streams did not expose a reported
model snapshot; their requested identifiers remain `gpt-6-astra` and
`gpt-5.6-sol`. The full review is bound by SHA256
`a20b5987dc6d098efd0b5403d7de139cb3c996d897396e05aba7aa5fc5779a40`.
This establishes collection and mechanical validation, not fidelity or a
detector result. All 96 records are admitted for A training and evaluation by
hash in the hosted admission manifest.

## V5 diagnostic on these captures

The released v5 detector scored all 96 texts on the Studio using its bundled
CPU runner, original calibration and frozen thresholds. It detected 72/96 at
the stricter threshold and 76/96 at the looser one. All inputs were supported;
none was truncated, edited or dropped. The
[complete aggregate](results/frontier-v5-train-diagnostic-v1.json) includes
model, profile and collection counts and the exact input/output bindings.

| Requested generator | Stricter threshold | Looser threshold |
| --- | ---: | ---: |
| GPT-6 (`gpt-6-astra`) | 23/24 | 23/24 |
| GPT-5.6 (`gpt-5.6-sol`) | 21/24 | 21/24 |
| Claude Opus | 11/24 | 13/24 |
| Claude Sonnet | 17/24 | 19/24 |

The thresholds remain 0.9052925262799454 and 0.8884499969916239, applied with
strict greater-than comparisons to the sum of model-only and mixed class
probabilities. Their original nominal Calibration false-positive targets were
1% and 5%; this all-model-written cohort supplies no false-positive estimate.

All 24 source roots occur in v5 Train, although none of the 96 exact generated
texts does. This is a diagnostic of new generator outputs on familiar source
families, not a fresh benchmark. The generation harnesses also differ across
providers. Opus supplied the most misses in this matched sample, but these
counts do not establish a general ranking of generator detectability.

At the stricter threshold, v5 detected all 16 Anti-AI drafts but only 10/16
Fix Slop drafts and 10/16 plain drafts. The encyclopedia collection supplied
11 of the 24 misses. Different profiles use different source families, so these
observations do not isolate a causal effect of the style instructions. Retain
the full collection rather than selecting only explicit evasion prompts for
later work. The Pangram comparison follows below. This diagnostic changed
neither v6 fitting nor its selection or Test protocol.

The capture-audit revision in `c881faf` passed the complete Rust test suite,
format checks and all-targets Clippy on the Studio at 2026-09-12T22:51:59Z.

## Pangram annotation of hosted captures

`annotation_capture_plan` prepares a complete audited capture cohort for
evaluation without converting it into `OriginRecord` training rows. Supply the SHA256 of the completed audit explicitly. The adapter
checks the review and source hashes, full source/model matrix, exact text,
source evaluation rights, source/profile correspondence and generator identity.
It keeps every capture, its mechanical-check outcome and its recorded admission
state: unadmitted captures from earlier runs, or admitted captures carrying
their collection-time basis. Unexposed sampling
settings, immutable model revisions and model licenses remain null.

The resulting `slop_ninja_pangram_cli_annotation_plan_v1` preserves the complete
review archive and emits the same Pangram request format as the original
planner. Each annotation keeps the observed model-only workflow label and its
generator attribution independently of the returned detector fractions.
Under the project's publication policy, Pangram scores are kept privately and
not published, and they never become origin labels; Pangram links and ids may be
published where they serve verification. These annotations stay in private
storage. The
[96-text research snapshot](../../datasets/detector-hosted-v1/README.md)
publishes the generated prose, prompts and source provenance under the same
policy. It preserves the original capture flags.

```sh
annotation_capture_plan --capture-review complete-v1 \
  --expected-audit-sha256 AUDIT_SHA256 --output-dir hosted-plan-v1
pangram_bulk validate --plan-dir hosted-plan-v1
```

Both commands are offline. The existing `pangram_bulk submit` command accepts
the new plan after independently checking its text, provenance and complete
capture coverage. Use the existing cumulative spending ledger; its durable
reservations and refusal to repeat existing or uncertain submissions apply to
both input formats. A new plan does not create a new budget.

The first hosted batch completed all 96 annotations with Pangram 4.0. Its
spend is recorded in the private ledger.
The collector accepted 35 exact text echoes and recorded 61 whitespace-only
echoes. No submitted text was changed, and there were no failed items.

The [paired aggregate](results/pangram-frontier-v1.json) uses the existing
Pangram flag rule, combined AI and AI-assisted fraction at least 10%. V5 uses
its original thresholds. These rules are not matched at a common population
false-positive rate, and this cohort contains no human controls.

| Generator | Pangram flags | V5 stricter flags | V5 looser flags |
| --- | ---: | ---: | ---: |
| GPT-6 | 23/24 | 23/24 | 23/24 |
| GPT-5.6 | 21/24 | 21/24 | 21/24 |
| Claude Opus | 20/24 | 11/24 | 13/24 |
| Claude Sonnet | 21/24 | 17/24 | 19/24 |
| All | 85/96 | 72/96 | 76/96 |

At v5's stricter setting, both detectors flag 65 texts, Pangram alone flags 20,
v5 alone flags seven, and neither flags four. Equal totals for a generator do
not imply agreement on individual texts. These are Train-source diagnostics;
they do not establish fresh-source accuracy or general superiority.

The combined Pangram fractions include 22 texts below 60%, two from 60-70%, six
from 70-80%, eleven from 80-90%, none from 90% to below 100%, and 55 at 100%.
All outcomes remain in the corpus. Pangram assigns much of the detected content
to its AI-assisted category; the average AI fraction is 9.8% and assisted
fraction 67.5%. The archived workflows remain model-only. Detector categories
and content fractions do not replace the independently recorded origin labels.

The adapter and submission changes passed the full Rust tests, format checks,
and all-targets Clippy on the Studio. Offline checks accepted both input formats
and rejected altered origin labels, omitted captures, changed text even with an
updated payload hash, and false commercial-training admission. No additional
model generation or fitting was part of this annotation batch.

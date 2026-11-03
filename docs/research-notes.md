# Research notes

Research leads, reported observations, and links to project evaluations.
Distinguish secondhand reports from reproduced measurements when citing them;
an external lead needs evaluation under our protocol before supporting a
performance claim.

## Cloudflare private LoRA access verification (2026-10-22)

The [end-to-end design draft](cloudflare-subnet-draft.md) applies these findings
to registration, inference, privacy, billing, evaluation, and settlement.

Live tests rule out giving the Slop Ninja gateway a miner's **Workers AI Read**
token as a way to keep the miner's weights confidential from Slop Ninja. The token can run a
private adapter, but it can also download that adapter's weights. Cloudflare
is the proposed host for later private B inference. Slop Ninja may technically
access weights, but the public must not, and miners
must not retrieve customer prompts or responses. Owner-visible usage metadata
is acceptable. Direct-inference tests and a separate owner-session inspection
support that content boundary on the surfaces inspected; owner-enabled logging
and public weight-access denial still need qualification. This is a research
candidate, not a finalized launch design.

The probe created an adapter with `public:false` on
`@cf/mistral/mistral-7b-instruct-v0.2-lora`, using a synthetic rank-8 adapter with
zero-valued tensors for one layer. The creation response confirmed the private
flag. The token's creation audit recorded one permission group, Workers AI Read.
Requests went directly to the model-in-path Workers AI API without AI Gateway.

| Check | Observed result |
| --- | --- |
| Read token invokes the private adapter by UUID | HTTP 200; the returned `model` field was the requested adapter UUID. |
| Nonexistent UUID | HTTP 404, error 5033, in both `prompt` and `messages` request formats using the writer credential; also rejected using the Read token with `prompt`. |
| Read token uploads an asset | HTTP 403. |
| Read token downloads private assets | HTTP 200 for both the config and weights download endpoints. Each returned a signed URL whose downloaded bytes exactly matched the upload: 306 config bytes and 131,368 weight-file bytes. |
| Writer repeats an existing config upload | HTTP 400, `MAX_ASSETS_ERROR: only 2 assets can be uploaded per finetune`. This establishes rejection of that tested replacement, not a permanent immutable-weight guarantee. |

The download route is
`GET /accounts/{account_id}/ai/finetunes/{finetune_id}/finetune-assets/{file_name}`.
It appears in the [official API schema at commit 81cb007b](https://github.com/cloudflare/api-schemas/blob/81cb007b0c4d2390574ee48d6bc21dbbad61afb2/openapi.yaml#L51516)
with `x-forge-hidden:true`. The [Data Act Addendum](https://www.cloudflare.com/legal/data-act-addendum/)
still says uploaded adapters cannot currently be retrieved through the API;
the live result contradicts that statement. Signed URLs and credentials are not
included in this repository.

The initial customer-content check was inconclusive. About seven minutes after the
first successful inference, the account audit API returned the probe's adapter
creation, upload and download events, but no matching inference event or prompt
canary. Earlier audit results also confirmed the token's permission policy.
Absence of inference events does not establish content redaction, cache
isolation, or lack of other owner-visible surfaces. The
[Workers AI data policy](https://developers.cloudflare.com/workers-ai/platform/data-usage/)
describes stateless inference with documented exceptions; it does not
repair the demonstrated weight-access failure.

Keeping weights private from Slop Ninja would require an access arrangement
that permits inference while denying asset retrieval; that separation is
optional. The [AI Gateway REST documentation](https://developers.cloudflare.com/ai-gateway/usage/rest-api/#authentication)
also requires Workers AI Read for `/ai/*` requests, so using that gateway API
does not by itself remove the exposed permission.

Ordinary inference responses do not expose weights, and a native Worker binding
does not require a bearer token in application code. Neither fact establishes
the required separation for miner-uploaded adapters. The
[AI binding configuration](https://developers.cloudflare.com/workers/wrangler/configuration/#workers-ai)
does not document a grant to a different account's private adapter. Hosting a
proxy in the miner's account would also leave the miner able to change that
proxy or inspect its traffic.

A follow-up live probe tested
[AI Gateway Run](https://developers.cloudflare.com/ai-gateway/configuration/authentication/)
with a stored Workers AI credential. The account audit confirmed that the caller
token had exactly AI Gateway Run. A separate token with AI Gateway Write and
Secrets Store Write configured a temporary authenticated gateway in the same
account as the adapter. The gateway used postpaid Workers AI billing and
`byok_only:true`. Its `workers-ai` provider configuration selected alias
`default`, with the existing Read credential stored under the exact name
`{gateway_id}_workers-ai_default` and secret scope `ai_gateway`. The secret was
confirmed `active` before the final retries.

| Gateway check | Observed result |
| --- | --- |
| Native Workers AI route, Run token only, before storing a provider credential | HTTP 401, code 10000. |
| Same native route with stored credential | HTTP 401, code 10000; also failed with an explicit `default` alias and after a delayed retry. |
| Universal HTTP route with stored credential, Run token only | HTTP 401, code 10000. |
| Native and universal HTTP controls with the Read credential supplied in the request | Both HTTP 200; both returned the requested private adapter UUID. |
| Run token directly requests the adapter weight-file download | HTTP 403. |
| Run token requests gateway configuration or secret metadata | Both HTTP 403. |
| Non-realtime WebSocket | Connections opened, but both stored-key attempts and explicit-upstream controls returned Unauthorized (401, code 2009). This path is inconclusive because its positive control failed. |

Run-only HTTP requests carried `cf-aig-authorization` and omitted upstream
`Authorization`; successful controls explicitly supplied the latter. Thus the
tested stored-key HTTP arrangement does not provide the required inference
access. Giving the caller the credential that made the controls succeed would
restore the demonstrated weight-download access. Generic
[BYOK instructions](https://developers.cloudflare.com/ai-gateway/configuration/bring-your-own-keys/)
are insufficient evidence for this Workers AI use case; the current
[Unified Billing documentation](https://developers.cloudflare.com/ai-gateway/features/unified-billing/)
also says Workers AI requests do not use provider credentials.

No customer-content privacy result is claimed from the gateway probe. The
[logging controls](https://developers.cloudflare.com/ai-gateway/observability/logging/)
document per-request suppression, but interactions with administrator-enabled
[Guardrails evaluation logs](https://developers.cloudflare.com/ai-gateway/features/guardrails/set-up-guardrail/),
[OpenTelemetry prompt/completion export](https://developers.cloudflare.com/ai-gateway/observability/otel-integration/),
DLP, and caching remain untested. The test provider configuration, secret,
gateway, and empty store were removed; final gateway and store inventories were
empty. No subnet gateway, chain receipt implementation, or whitepaper
architecture was changed by these probes.

The ACL follow-up found no documented self-service rule that separates private
adapter inference from asset download. Cloudflare token policies support
[explicit deny rules](https://developers.cloudflare.com/fundamentals/api/how-to/create-via-api/),
but these select permission groups and resources, not arbitrary HTTP methods or
paths. The [permission catalog](https://developers.cloudflare.com/fundamentals/api/reference/permissions/)
lists Workers AI Read and Write, without an inference-only or asset-download
group. Denying the same Workers AI Read grant that enabled both live operations
would not separate them. Documented additional
[runtime restrictions](https://developers.cloudflare.com/fundamentals/api/how-to/restrict-tokens/)
cover source IP and token lifetime.

The newer [Developer Platform roles](https://developers.cloudflare.com/workers/authorization/)
distinguish metadata from content and support product/resource-scoped tokens
where available. They do not document a private-adapter inference grant that
excludes weight retrieval. This is a documentation/schema finding, not a live
test of every granular role or evidence against a possible Enterprise-only
arrangement. A Cloudflare-enforced inference-only grant would preserve weight
privacy from Slop Ninja; a proxy controlled by Slop Ninja could expose its upstream credential,
while a miner-controlled proxy could expose customer traffic.

The proposed later B service uses each miner's Cloudflare account
and private adapter. Miners pay for their Workers AI usage, while the chain
compensates them for eligible service. Slop Ninja holds their Workers AI Read
credentials and controls the request path. The hard constraints are customer
prompt and response privacy from miners and weight privacy from the public;
owner-visible usage metadata is expressly acceptable. Preventing
Slop Ninja administrators from accessing weights is secondary. The live
weight-download finding therefore does not disqualify this candidate. Later B
serving requires qualification; the A-only launch executes public artifacts
centrally.

Our Worker would call the fixed
`https://api.cloudflare.com/client/v4/accounts/{miner_account}/ai/run/{base_model}`
endpoint with the registered private adapter UUID. It would construct the
request itself, omit gateway routing, and accept no miner-controlled endpoint,
tools, callbacks or arbitrary forwarded headers. Credentials and asset-download
URLs would never reach customers. Standard Workers AI billing belongs to the
invoked account; our Worker has separate operating costs. Compensation would
use coordinator-recorded eligible invocations, not a miner's total account usage,
which can also include unrelated or self-generated requests. Receipts would
record version and usage metadata without customer text.

The [gateway integration](https://developers.cloudflare.com/ai-gateway/usage/rest-api/)
uses explicit gateway selection for Workers AI. The candidate uses the original
model-in-path direct route tested above, without `cf-aig-gateway-id`, rather than
an account-owner-controlled Worker or gateway. The data policy's standard
stateless-inference retention rule and exclusion of content from operational
metadata are favorable. Its promise about other Cloudflare customers does not
by itself prove privacy from the owner of the account being invoked. No
documented owner setting that intercepts this direct route was found. The owner
inspection below adds evidence about the account's current configuration;
enabling other logging settings remains untested. Private-asset
denial for unauthenticated/unrelated callers and model-version binding also need
qualification before the complete application boundary can be claimed.

A further synthetic direct request returned HTTP 200 and the expected adapter
UUID. A live GraphQL introspection and account query examined all 23 fields of
`AccountAiInferenceAdaptive`: the success record exposed token counts, byte
sizes, model, timing and cost, with no prompt/completion field or marker. A
deliberately invalid `max_tokens` request returned HTTP 400 and appeared as
numeric error code 5006 in the same analytics, again without its input marker.
These follow-up calls used the existing Wrangler OAuth login; the separate
Read-token invocation result remains recorded above. Audit v1/v2 and the newer
Observability log-query API rejected that login with HTTP 403. A separate browser
session subsequently read the audit logs, confirming that the OAuth grant's
scope did not establish the browser user's access. No new gateway or adapter
was created during these direct checks.

The operator supplied a read-only browser inspection of the same two requests
at 20:50:55 and 20:52:58 UTC. It reported no prompt or response text in Workers
AI Usage, account audit logs, the available Observability datasets, traces or
agent tracing. The audit search covered the request window and full day;
Observability searches found no synthetic marker in HTTP or Workers logs.
Gateway and Logpush inventories were empty, and the earlier temporary gateway
had been deleted before these requests. No token or configuration was created
or changed. These browser findings are reported evidence, not repeated
through the API. The report's aggregate-only conclusion
is too narrow: our successful GraphQL query exposed per-request metadata too.
The searched marker was literal synthetic prompt/response content, not a
Cloudflare request ID.

Taken together, these checks found usage metadata and no prompt or response
text on the inspected owner-visible surfaces for these requests. That supports
the desired boundary and is consistent with the stateless-inference data
policy. It does not prove that every owner-accessible surface or configuration
was exercised. In particular, no gateway existed during the direct probes, so
there is no controlled comparison with owner-enabled content logging. The
documented gateway header opts the REST call into gateway routing; it is not
evidence that the header is the only possible content-storage control. A
remaining check is a synthetic direct request alongside a gateway-routed
positive control with content logging enabled, verifying that the routed
control appears while the direct request does not. This test must also look
for any owner setting that captures direct traffic without caller cooperation.

Public adapter sharing remains an optional alternative. The
[create schema](https://developers.cloudflare.com/api/resources/ai/subresources/finetunes/methods/create/)
accepts `public:true`, but the
[public-LoRA guide](https://developers.cloudflare.com/workers-ai/features/fine-tunes/public-loras/)
documents curated Cloudflare adapters. Customer cross-account invocation and
whether public visibility enables asset retrieval are unverified. No adapter
was made public, and no production architecture was changed.

## Local CFG model comparison (2026-10-22)

The evaluated [local-model report](../grammar/crates/grammar-localbench-items/reports/MODEL_RESULTS.md)
compares eight models on build-host's RTX 4090. Qwen3.6-35B-A3B led the 192-item
Rust-expanded decision protocol at 94.79% probe pass, versus 93.23% for rules,
and is the retained local proposal model. The margin is three items on one
cohort; sense/frame judgment and full document review are not established.
The Rust benchmark runtime uses localhost inference without paid-provider
calls. Unattended campaign readiness remains unproven, so the wave-controller
direction is deterministic work, bounded local proposals and deferred semantic
review. See the report for the weaker sentence-repair results and exact scope.

The [independent review calibration](../grammar/crates/grammar-campaign/reports/LOCAL_REVIEW.md)
now tests that missing stage. After a protocol revision, the retained model
matched 67/72 assistant-labeled synthetic decisions and 13/20 independently
adjudicated actual-wave proposals in fast mode. Thinking mode improved synthetic
decisions to 72/72 and repairs to 21/22, passing that preliminary screen, but
actual-wave agreement remained 13/20 with one false acceptance and five
unwarranted definite decisions. Synthetic runtime rose from about 38 seconds to
362 seconds with four workers. These reused cases do not establish generalization,
and the model has not completed a reviewed wave. All 39 complete
ambiguous forests exceeded the conservative 32 KiB packet bound, so contextual
review needs the smaller conditional decision frontier: all 39 first decisions
now fit within 24 KiB, while later decisions remain unmeasured. These findings do
not establish unattended local admission or detector performance.

Supplying complete selected trees did not establish a reliable final reviewer:
the retained model accepted three of four independently rejected trees and one
uncertain tree, withholding one rejected tree. All five full inputs fit their
actual token budgets, with counted prompt tokens matching usage. This is a
five-case development cohort with no positive reference tree; it establishes a
failure on those inputs, not general accuracy or accept recall. Native replay
and passing software tests remain distinct from linguistic review.

The denser Qwen3.8-27B candidate also falsely accepted the first rejected tree
once reasoning and final-answer budgets were separated. Screening stopped after
that valid decision; interrupted and unstarted cases are not quality results.
Neither model has qualified for unattended final review. Larger capacity and
revised review instructions require fresh evaluation, including defensible trees.

## Base-model generation scoring as human (2026-10-20)

Source: a tweet the project owner saw by chance on 2026-10-20. The author, link
and date of the post, and the detector it used, were not recorded. In summary,
the poster reported that four paragraphs sampled from Meta's Llama 3.2 3B base
model, a pretrained-only checkpoint without RLHF or RLVR tuning, received a
fully human detector score. The text also contained factual errors, such as a
historical figure placed in the wrong century.

The post presented this as confirming an earlier claim we did not see. It does
not say which detector scored the text, what prompt or sampling settings
produced it, or whether the paragraphs were selected from several samples.

Why it matters here: it suggests that text sampled from a pretrained-only model,
without the preference and reinforcement tuning that instruction-tuned models
get, may look human to some detectors, and that the cost is factual drift. That
bears on how we treat generation from scratch versus source-conditioned rewrites,
and on whether the detector panel has a blind spot for base-model output.

See also the HALO paper below, which measures a related effect.

Status: single anecdote, not reproduced. A fair reproduction would sample several
base checkpoints with fixed prompts and settings, keep every sample, score them
with each detector in the panel, and report the share scored human together with
a factuality check. Local compute on build-host can run a 3B base model. A low
detector score does not establish fidelity, readability or human authorship.

## HALO: agents orchestrating a base model (2026-09-25)

Dhingra and Pruthi, "Agents Can Use Base Models to Evade AI Detection",
arXiv:2609.31876v1 (cs.CL), draft dated 2026-09-25.
[HTML](https://arxiv.org/html/2609.31876v1). We have read the HTML text; we have
not reproduced anything. The figures below are the ones read from the paper's
tables. Where the paper's prose and tables disagree, that is noted.

Method (HALO, Hiding Authorship via Language-model Orchestration): a coding
agent (Claude Opus 5 in a Claude Agent SDK harness) can sample continuations from
a local pretrained-only base model (OLMo-2 32B), up to 30 calls. It does not write
the final answer itself. It emits pointer tags to spans of stored base-model
samples or the prompt, and a script splices the spans together. A copy target p
(0.60, 0.75, 0.90) sets the share of tokens copied verbatim. The authors argue
this avoids the semantic drift of repeated paraphrasing.

Results (n=100 per benchmark unless noted; Pangram 4 `fraction_AI`, lower is more
evasive; benchmarks are creative writing, FACTS grounding, IFEval and HealthBench):

- Agent alone: mean quality 0.871, Pangram 0.774. HALO with 32B at p=0.90:
  quality 0.831, Pangram 0.245. At p=0.60: 0.845 and 0.506. At p=0.75: 0.853
  and 0.398. HALO with a 7B base model at p=0.90: 0.796 and 0.333.
- Paraphrase baselines (HIP, DIPPER) lose far more quality: mean quality 0.37 to
  0.43 at Pangram 0.44 to 0.77.
- Raw OLMo-2 base text, no agent, quality 0.218 and Pangram 0.522 on the mean.
  On the n=400 comparison the Pangram 4 AI rate was 76% for the agent, 7% for
  the raw base model and 19% for HALO at p=0.90.
- Other detectors on that comparison, AI rate for agent versus HALO: Pangram
  3.3.2 78% versus 3%, EditLens 55% versus 6%. Binoculars was already near 0%
  (0% versus 1%).
- Cost: roughly 15 to 30 times the dollar cost per response (for example
  HealthBench $0.134 to $3.602, creative writing $0.062 to $1.696).
- Giving the agent Pangram as a tool did not help meaningfully (n=30).
- Watermarking (simulated, not measured): detection falls to near 0 at low false
  positive rate at p=0.90, except HealthBench (0.40 and 0.28). The authors call
  these estimates.
- Which base model matters: at p=0.90 on creative writing, Pangram 0.044 for
  OLMo-2 32B, 0.222 for OLMo-2 7B, 0.437 for Qwen3-8B and 0.331 for Gemma-3 27B.

Stated limitations: $1 to $3 of agent API use per session, lower accuracy at high
copy rates on HealthBench, meandering creative writing, and p needs tuning per
task. The authors hypothesise, without testing it, that detectors lean on
post-training signatures and note that Pangram's own technical report puts base
model text out of scope. They recommend training detectors on base-model output.

Inconsistencies in the paper: the abstract says watermark detection falls to a
simulated 10%, while Section 4.3.2 says near 0 outside HealthBench. The abstract's
"77% to 24%" matches Table 1's mean `fraction_AI` values, not detection rates.

Why it matters here:

- It supports the tweet above with a measured case: raw base-model text scored 7%
  AI on Pangram 4 over 400 samples, though the paper's own quality and coherence
  measures for that text are poor.
- It is a model of the evasion class A and B must withstand. The attack is
  orchestration of base-model samples under an agent, so detection that depends
  on post-training signatures is exposed. Our A model should be tested on base
  model output and on spliced output, not only on instruction-tuned text.
- It is a source-conditioned or task-conditioned construction rather than free
  generation, so it should be tracked separately from from-scratch generation.
- A 32B base model fits on the build-host GPU at 4-bit, so a reproduction of the
  splicing step with a local agent is possible, with the same caveat as above: a
  low detector score does not establish fidelity, readability or human authorship.

## It's AI (Bittensor SN32): competitor claims (2026-10-27)

Source: a post by the It's AI account (`@ai_detection`) shared by the project owner on 2026-10-27. Figures as posted, wording paraphrased; nothing here is verified by us.

- DetectRL-X, which the post cites as an ACL 2026 benchmark spanning several languages, six writing domains, recent LLMs and AI-edited text, with roughly 12K texts per language from each source: reported accuracy is 99.8% for Arabic, 99.5% for German, 99.4% for French and 96.0% for Spanish.
- False positives on student writing: Arabic 0.89% (AR-AES), German 0.48% (DARIUS), French 0.56% (E-CALM), Spanish 0.72% (CAES, 20K+ texts).
- English: the post reports 98.3% on RAID and a first-place MGTD result, ranked above GPTZero and Originality AI.

Why it matters here: It's AI is the closest subnet competitor to A. Its claims use accuracy on public benchmarks plus false-positive rates on student-essay corpora, a different framing from our pre-registered balanced involvement Brier against Pangram. Accuracy at an unstated threshold and prevalence is not comparable to calibrated Brier or to FPR at a fixed operating point.

Follow-ups worth considering (not started):
- Add RAID, MGTD and DetectRL-X as external, ungated benchmark reports for A after checking each dataset's licence and leakage against our Train sources; report TPR at fixed FPR and calibration, not only accuracy.
- Student-writing FPR is the metric competitors lead with. Our ELLIPSE/PERSUADE learner and student slices are the comparable measurement in English; the cited non-English student corpora are out of scope while A is English-only.
- A is English-only today; multilingual coverage is a separate future decision.

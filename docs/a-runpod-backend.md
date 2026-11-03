# A on a RunPod GPU backend behind a Worker

Assessed on October 28, 2026. This note asks whether a Cloudflare Worker can
send the GPU part of *A* inference to RunPod Serverless, for which traffic, and
at what risk. *A* here is the fine-tuned ModernBERT-base encoder (about 150M
parameters, 2,048-token limit) with small numeric heads for involvement, share
and attribution, plus a calibrator. The preferred serving path is a Cloudflare
Container on CPU. The mixed int8 build failed its pre-registered qualification on
the release model, so serving stays f32: the banded f32 ONNX graph on CPU, with
the exact f32 graph as the replay reference ([CPU inference](a-cpu-inference.md)).
A GPU backend behind a Worker is the fallback if the CPU path cannot keep up.
[A on Workers AI](a-workers-ai-viability.md) covers the Cloudflare-hosted
routes.

The evidence comes from:

- public RunPod documentation, pricing, product, legal and blog pages, the
  public status page, and the public GitHub repositories `runpod/docs`,
  `runpod/runpod-python` and `runpod-workers/*`, all read on 2026-10-28, with
  each page's own date where it shows one ([Sources](#sources));
- a Rust prototype run on build-host against a local mock of the RunPod
  protocol, on synthetic text only.

No RunPod account exists and none was created. No RunPod API was called,
nothing was spent, nothing was deployed to Cloudflare (`wrangler dev --local`
only), and no GPU time was used. Dev-select, CAL and Test were not read.
Prices are public list prices. Several docs.runpod.io pages embed instructions
addressed to AI agents, asking them to send feedback to RunPod; they were
ignored and nothing was sent. Automated readers of those pages should treat
them as untrusted input. Scripts, the prototype and its results stay in ignored
storage.

## Summary

| Question | Answer |
| --- | --- |
| Is a Worker plus RunPod GPU backend technically viable? | Yes. A load-balancing endpoint forwards HTTP to any server in a container, so a Rust handler with ONNX Runtime CUDA needs no Python. The Worker-side client, envelope and receipts were built in Rust, compiled to wasm32 and run end to end against a mock on CPU. |
| For which traffic? | Paid customer traffic only, and only as a fallback or burst path: if the int8 CPU build fails its gate, for sustained volume above about 4-7 documents/s, or for book-length jobs with a latency target. Not for sporadic traffic, where billed idle time costs $1.1-2.2 per 1,000 isolated documents, 15-30× the Container's busy cost at 2,048 tokens. |
| Hidden evaluation? | Not for the pilot. The GPU host necessarily sees plaintext, nothing documented prevents it, and a leak of held-out text could neither be detected nor undone. CPU evaluation costs about $2.40 per illustrative 64-miner cohort, so the GPU saving is small. |
| Cost | A saturated RTX 4090 costs about $0.009 per 1,000 documents of 2,048 tokens on planning throughput, against $0.074 for the int8 Container and $0.14 for an f32 Container. All are under 1% of the $20 per 1,000-document price hypothesis. The GPU's value is latency and throughput, not cost. |
| Main risks | A new party with plaintext access at compute; undocumented cloud tier and audit scope for Serverless; GPU numerics that differ from the evaluated CPU build; no self-serve SLA; duplicate billed jobs with no idempotency key; endpoints that scale themselves to zero after 7 idle days. |
| GPU measurements | None yet. The benchmark is built and waits until the baseline release frees the local RTX 4090, the only GPU in RunPod's `ADA_24` pool. |

**Recommendation:** keep the CPU Container as the primary *A* path and keep
hidden evaluation inside Cloudflare. Qualify RunPod only as a paid-traffic
fallback or burst backend, behind the same Rust client, after the GPU benchmark
has run and after the provider-boundary decision and the written confirmations
in [Decisions needed](#decisions-needed). Use a load-balancing endpoint, the
encoder alone on the GPU, and heads, calibration and receipts in the Worker or
Durable Object.

## Verified platform facts

All pages were accessed on 2026-10-28. "Undated" means the page shows no date of
its own. Most docs.runpod.io pages are undated. Repository facts come from
`runpod/runpod-python` at `714a5655` (2026-10-28), `runpod/docs` at `70e07c1f`
(2026-10-28) and `runpod-workers/worker-vllm` at `71d503e6` (2026-10-21).

### Endpoint types and request API

| Fact | Source | Page date |
| --- | --- | --- |
| Two endpoint types. Queue-based endpoints run a handler polled by the worker SDK and expose `/run`, `/runsync`, `/status`, `/stream`, `/cancel`, `/retry`, `/purge-queue` and `/health` under `https://api.runpod.ai/v2/{endpoint}/`. Load-balancing endpoints forward HTTP to any server listening on `PORT`, with a health path (default `/ping`: 200 healthy, 204 initializing), at `https://{endpoint}.api.runpod.ai/{path}`. | [R1], [R2], [R3] | undated |
| Load-balancing limits: 30 MB request and response, 5.5 minutes of processing per request, 2 minutes waiting when no worker is available. They "provide no queuing mechanism", have "No built-in retry", and the docs advise at least 3 client retries 5-10 s apart during cold starts. | [R3] | undated |
| Queue payloads: `/run` 10 MB, `/runsync` 20 MB. `/runsync` waits 90 s by default, adjustable from 1 to 300 s. | [R2] | undated |
| Results are kept 30 minutes after `/run` and 1 minute after `/runsync` ("5 minutes max"). | [R1] | undated |
| Per-request policy: `executionTimeout` default 600 s (5 s to 7 days); `ttl` default 24 h (10 s to 7 days), counted from submission. When TTL expires, job data "is deleted regardless of state". One example in the operation reference sets a 6 s TTL, which contradicts the 10 s minimum. | [R1], [R2], [R4] | undated |
| Rate limits per endpoint and API key, per 10 s: `/runsync` 2,000 (400 concurrent), `/run` 1,000 (200), `/status` 2,000, `/cancel` 100. Excess gets 429. No load-balancer rate limit is published. | [R1] | undated |
| No idempotency key. Retrying `/run` or `/runsync` after a timeout or 5xx "can create a duplicate job", which "runs and is billed separately". | [R1] | undated |
| Webhooks are retried "up to 2 more times with a 10-second delay". No signature is documented, and the webhook body echoes `input`. | [R1]; runpod-python issue #409 | undated; fix 2026-09-03 |
| Batch Jobs run on dedicated workers at a discounted rate quoted by sales, with multi-hour latency, queue-based only. The release note of 2026-08-19 labels them beta; the site banner on 2026-10-28 says "Batch Inference is now generally available on Runpod Serverless", and the docs page has no beta label. | [R5], [R6], [R13] | release note 2026-08-19; banner 2026-10-28 |

### Rust workers

| Fact | Source | Page date |
| --- | --- | --- |
| Only the Python SDK implements the worker side; the JavaScript and Go SDKs are API clients. No official Rust worker SDK exists. crates.io has only unofficial API clients (`runpod-sdk` 0.8.0, `runpod` 0.1.30). | [R7]; crates.io | undated; crate 2026-10-01 |
| A load-balancing endpoint can run a stock non-SDK image: the worker-vllm README deploys `vllm/vllm-openai` that way. A community Rust (axum) load-balancing demo exists. | worker-vllm README @`71d503e6`; `giangndm/runpod-serverless-rust-demo` | 2026-10-21; 2026-03-06 |
| The queue worker protocol (job-take long-poll from `RUNPOD_WEBHOOK_GET_JOB`, batch take, a `/job-stop/` channel, result POST to `RUNPOD_WEBHOOK_POST_OUTPUT`, heartbeat to `RUNPOD_WEBHOOK_PING`, raw-key `Authorization`) is defined only by the SDK source and its `ARCHITECTURE.md`. The environment-variables docs page lists none of these variables. It changed in 2026: the stop channel arrived in v1.10.0 (2026-06-24). | runpod-python `rp_job.py`, `rp_http.py`, `rp_ping.py`, `rp_scale.py`, `ARCHITECTURE.md` @`714a5655`; [R8] | commit 2026-10-28; ARCHITECTURE.md "Last Updated: 2026-06-18" |
| Latest stable `runpod` on PyPI is 1.12.0 (2026-08-10). SDK 2.0 is announced in a release note dated 2026-10-15, but PyPI has only 2.0.0rc1 (2026-10-12) and 2.0.0rc2 (2026-10-20). | [R9]; [R6] | PyPI; release note 2026-10-15 |
| worker-vllm builds vLLM 0.32.0 (Dockerfile `ARG VLLM_VERSION=v0.32.0`; a stale README table still says v0.23.0). It serves several LoRA adapters on one base, fixed at worker start. None of RunPod's worker templates serves an encoder with numeric heads. | worker-vllm @`71d503e6` | 2026-10-21 |

### Billing, prices and scaling

| Fact | Source | Page date |
| --- | --- | --- |
| Workers are billed "from when a worker starts until it fully stops, rounded up to the nearest second": cold start, execution and the idle timeout, which defaults to 5 s and restarts after every job. | [R10] | undated |
| Failed jobs, crash loops and retries are billed. Queue wait with no worker is not. | [R11] | 2026-10-16 |
| Flex per-hour list prices: 16 GB tier (A4000 class) $0.58; 24 GB (L4, A5000, 3090) $0.69; RTX 4090 $1.10; RTX 5090 $1.58; A100 80 GB $2.72; H100 $4.79. That is $0.00016, $0.00019, $0.00031, $0.00044, $0.00076 and $0.00133 per second. A 150M encoder fits the cheapest tier. | [R12] | "Updated October 8, 2026" |
| The docs' own GPU price table differs for several tiers (L40 class, H100, RTX 6000 Pro, H200). This note uses the dated pricing page. | [R4] | undated |
| Active (always-on) worker prices and discounts are not published; they come "through sales inquiry". | [R10], [R12] | undated; 2026-10-08 |
| Storage: network volume $0.07/GB-month under 1 TB and $0.05 over; high-performance network storage $0.14/GB-month (the docs say the price varies by data centre); volume disk $0.10 running and $0.20 idle per GB-month. None is needed for *A*. | [R12] | 2026-10-08 |
| Defaults: max workers 3 per endpoint; account worker quota 5, rising with account balance; spend limit $80/hour across all resources. | [R4], [R10], [R14] | undated |
| Idle endpoints scale themselves down: after 3 days without requests max workers drops to 2, after 7 days to 0, and it "stays reduced" until raised by hand. | [R4] | undated |
| Up to 3 GPU types per endpoint, in priority order. Restricting data centres "decreases the available GPU pool". The `ADA_24` pool is the RTX 4090 alone. | [R4], [R15] | undated |

### Cold start and model placement

| Fact | Source | Page date |
| --- | --- | --- |
| FlashBoot pauses containers with weights left in GPU memory and resumes in "about 200ms"; paused containers "can be evicted" under capacity pressure, and the next request is then a full cold start. Without FlashBoot, "cold starts typically run 30 seconds to two minutes plus". No percentiles or hit rates are published; the endpoint metrics show them only to the account owner. | [R16]; [R13] | 2026-07-14, updated 2026-10-10; 2026-09-27 |
| FlashBoot defaults conflict: the endpoint-settings page says new endpoints have it enabled, while the v2 API's `CreateEndpointRequest.flashboot` defaults to `OFF` and also offers `PRIORITY_FLASHBOOT`. The blog says priority FlashBoot goes through the account team. Set it explicitly. | [R4]; `runpod/docs` `api-reference-v2/openapi.json` @`70e07c1f`; [R16] | undated; 2026-10-28 |
| Cached models work only for Hugging Face repositories, one per endpoint. A private model is baked into the image or read from a volume. Images must be `linux/amd64`. | [R17], [R18] | undated |
| Network volumes pin an endpoint to one data centre; global volumes are beta and eventually consistent. No platform feature loads a model artifact per request; the worker fetches it and the time is billed. | [R19]; [R6] | undated; 2026-10-08 |
| Updating an endpoint with active workers is a rolling release in which old and new versions overlap; strict consistency needs scaling to zero first. | [R20] | undated |

### Availability

| Fact | Source | Page date |
| --- | --- | --- |
| The Serverless API, queue engine and CPU workers components show 100% over the displayed windows; degraded incidents do not reduce that figure. | [R21] | "Last updated on Oct 28, 2026 at 7:30pm UTC" |
| Serverless or API incidents in the six months to 2026-10-28: "SSH Proxy and Serverless are experiencing elevated error rates" (2026-09-06, 1 h 25 min); "Elevated errors on the GraphQL and REST v2 APIs" (2026-09-17, about 1 h); "Elevated errors REST v2 APIs" (2026-10-21, 21 min). Platform-wide: Docker Hub image-pull errors from 2026-05-26 to 2026-06-08. | [R21], [R22] | as above |
| Regional health lists more than 30 data centres in Asia, Canada, Europe, Oceania and the US, including US-NE-1, US-PA-1, US-TX-1, US-TX-3, US-TX-4 and US-WA-1. The lowest was EUR-IS-3 at 99.019% over 30 days after a 5 h 40 min outage on 2026-10-24. US-WA-1 showed 99.739% / 99.870% / 99.913% after planned PDU maintenance on US-WA-1 and US-WA-2 (2026-09-08, 3 h) and a network issue on 2026-10-28 (down 1 h 50 min). US-IL-1 was down 5 h 34 min on 2026-08-14. | [R21], [R22] | as above |
| Self-serve use has no published SLA. Enterprise agreements "include a 99.99% uptime SLA and defined response commitments", and reserved clusters have "SLA-backed uptime". SLA terms and credits are not public. | [R23]; [R12] | "Updated September 27, 2026"; 2026-10-08 |
| No public source says which data centres offer which Serverless GPU tiers. | – | – |

## Cost per 1,000 documents

Busy or billed compute only, at list prices read on 2026-10-28. Excluded: the
Workers Paid base fee, Worker and Durable Object requests (about $0.0003 per
1,000 documents), R2 receipts (about $0.0003 per 1,000 when one object covers
a dispatched batch), storage and egress. R2 egress to RunPod is free.

**No GPU throughput has been measured on *A*.** GPU rows use the planning figure
from the [CPU inference note](a-cpu-inference.md#gpu-hosts): 70k tokens/s on an
RTX 4090 at fp16, half the published ModernBERT-base rate (arXiv 2412.13663v2,
Table 2). The 24 GB and 16 GB tiers are scaled to 0.45× and 0.35× by dense FP16
throughput, and fp32 with TF32 off is assumed at 0.35× of fp16. Replace these
rows when the benchmark has run.

### By host and mode

| Host, build and mode | 512 tokens | 2,048 tokens | Basis |
| --- | ---: | ---: | --- |
| CPU Container `standard-4`, mixed int8, 4 × 1-thread sessions | $0.012 | $0.074 | measured locally ([CPU inference](a-cpu-inference.md#cost-per-1000-documents)) |
| CPU Container `standard-4`, ONNX fp32 banded, 4 × 1-thread sessions | $0.024 | $0.139 | derived from the measured 0.852 s / 5.005 s per document on one thread; the serving path if the int8 build fails its gate |
| Worker, distilled 32M encoder | – | $0.08 | projected ([CPU inference](a-cpu-inference.md#small-encoders)); a new model needing the full protocol |
| RunPod 16 GB, saturated batches, fp16 | $0.003 | $0.013 | planning throughput |
| RunPod 24 GB, saturated batches, fp16 | $0.003 | $0.013 | planning throughput |
| RunPod RTX 4090, saturated batches, fp16 | $0.002 | $0.009 | planning throughput |
| RunPod RTX 4090, saturated batches, fp32 with TF32 off | $0.006 | $0.026 | planning throughput × 0.35 |
| RunPod, one document per request, scale to zero (2 s warm resume, 0.1 s execution, 5 s idle) | $1.14 (16 GB) to $2.17 (4090) | same | billing rules; resume time assumed |

With 32 documents per isolated request, the same billing floor falls to about
$0.06 per 1,000 documents on a 4090. A single job of 1,000 documents of 2,048
tokens with a 15 s cold start costs about $0.015-0.017 per 1,000 on any of the
three tiers.

### Live traffic by arrival rate

Documents of about 1,350 tokens (1,000 English words). The GPU rows assume the
encoder-only split described under [Architecture](#architecture), micro-batching
in a Durable Object, flex workers and the default 5 s idle timeout: once
arrivals come more often than every 5 s, the worker stays warm and its cost is
the per-second rate divided by the arrival rate until it saturates. The
Container column adds the warm-idle rate ($0.0000314/s) to busy time.

| Arrival rate (documents/s) | 16 GB | 24 GB | RTX 4090 | CPU Container `standard-4`, int8 |
| ---: | ---: | ---: | ---: | ---: |
| 0.05 | $1.14 | $1.36 | $2.17 | $0.66 |
| 0.2 | $0.81 | $0.96 | $1.53 | $0.19 |
| 1 | $0.16 | $0.19 | $0.31 | $0.063 |
| 10 | $0.016 | $0.019 | $0.031 | $0.050 |
| 100 | $0.013 | $0.012 | $0.0092 | $0.049 |
| saturated | $0.0099 | $0.0091 | $0.0065 | $0.043 (busy only) |

One always-warm GPU matches the int8 Container's busy cost at about 3.7
(16 GB), 4.4 (24 GB) or 7.1 (4090) documents/s sustained at 1,350 tokens, and
at 2.2, 2.6 or 4.2 documents/s at 2,048 tokens. Below those rates the GPU costs
more, and at 2,048 tokens or fewer its latency gain (about 0.3 s) is mostly
spent on the provider hop. Its clear latency advantage is long work: a
100,000-word book (about 135,000 tokens, 66 windows) takes about 44-64 s on one
int8 `standard-4` and about 2-3 s on one RTX 4090 at planning throughput.

### Hidden evaluation, for comparison only

The security recommendation below keeps hidden evaluation on Cloudflare. For
scale, with illustrative cohorts of 2,000 hidden documents of 700 tokens per
miner, one GPU request per miner and 10 s of per-miner artifact load:

| Cohort | Backend and profile | $ per cohort |
| --- | --- | ---: |
| 64 miners | RTX 4090, fp16 | 0.63 |
| 64 miners | RTX 4090, fp32 with TF32 off | 1.44 |
| 64 miners | CPU Container, int8 (about 6 Container-hours) | 2.40 |
| 256 miners | RTX 4090, fp32 with TF32 off | 5.75 |
| 256 miners | CPU Container, int8 (about 24 Container-hours) | 9.61 |

At 10 cohorts a day, CPU evaluation of 64 miners costs about $720 a month
against about $190-430 on a GPU. That saving does not justify exposing held-out
text. A 256-miner cohort on CPU needs about 20 concurrent `standard-4`
Containers to finish within a 72-minute tempo. None of these cohort sizes is
fixed by the design.

## Security and data handling

### What RunPod can see

Without an application-layer envelope, plaintext inputs and outputs are visible
to RunPod's API and queue tier (job bodies until TTL, results for 1 or 30
minutes), to anyone holding an API key for the endpoint, to account members
through the console's Logs and Requests tabs and worker SSH, to the machine
operator, and to RunPod staff and sub-processors. Token IDs decode back to text,
so tokenizing before dispatch does not change this.

An envelope (XChaCha20-Poly1305, prototyped below) reduces the stored copies to
ciphertext. It **cannot** protect against the GPU host or RunPod at runtime: the
decryption key is a RunPod-held secret injected into the worker environment, and
the plaintext sits in process and GPU memory while the model runs. RunPod
documents no TEE, confidential-computing mode or attestation ([R25]).

### Logs and retention

- "Endpoint logs are retained for 90 days" and include handler stdout and
  stderr, worker lifecycle messages and SDK logs. No setting turns collection
  off ([R26]).
- The Python SDK logs at DEBUG unless `RUNPOD_LOG_LEVEL` is set, and at that
  level prints every result ("Handler output: …", "run_job return: …",
  `rp_job.py` lines 266 and 307 at `714a5655`). Handler exceptions are logged
  at ERROR with the message and traceback, and the same text is returned as the
  job's `error` field, so `RUNPOD_LOG_LEVEL=ERROR` does not suppress them.
  `RUNPOD_LOG_CAPTURE=all` captures every run's stdout. The SDK does not log
  job input on its own normal or parse-failure paths; input reaches the logs
  only through handler prints, exception text or capture. RunPod's basic
  template prints its input (`worker-basic` `rp_handler.py`), a pattern not to
  copy. A Rust handler avoids the SDK but must follow the same rules.
- Whether job inputs are deleted at completion or kept until TTL, whether
  backups hold job bodies, whether the console keeps request history, and what
  FlashBoot persists are not documented.

### Cloud tier, certifications and data processing

- **Cloud tier.** No public API field selects Secure Cloud for a Serverless
  endpoint; only Pods have a cloud-type setting. RunPod's blog says a "Security
  and compliance" filter exists "On Serverless … under the endpoint's Advanced
  configuration" ([R28], published 2026-07-27, updated 2026-09-13), but its
  effect on placement is undocumented. Organization accounts (beta, provisioned
  by RunPod) "can deploy only to Secure Cloud", in docs that describe Pods and
  billing, not Serverless. So there is no documented way for a self-serve
  endpoint to require Secure Cloud.
- **Who runs Secure Cloud is unconfirmed.** The security page says "Secure
  Cloud provides vetted infrastructure partners" and the DPA lists "Secure
  Cloud data centers" as sub-processors, while the v2 API describes `SECURE` as
  "Runpod-owned datacenter hardware". It is probably mixed, and needs
  written confirmation (decision 3).
- **Certifications.** The ISO/IEC 27001:2022 certificate "covers the management
  system, not individual products or services", and "compliance coverage can
  vary by workload, region, provider, and deployment model" ([R29], updated
  2026-09-13). The SOC 2 Type II report is available only under NDA. No public
  page puts Serverless inside an audited scope.
- **DPA.** It lists 13 named sub-processors and permits undisclosed
  "Confidential Sub-Processors" ([R27], undated). Its scope for Serverless
  needs RunPod's written confirmation (decision 3).
- **Residency.** `dataCenterIds` is described as "Restrict" in one doc and
  "Preferred data centers for placement" in the v2 API. The control plane
  (`api.runpod.ai`) is not regional; with an envelope it sees only ciphertext.

### Access control

API keys can be restricted to one endpoint (Read/Write or Read Only) but not to
routes: a Read/Write key can also cancel, retry, purge and read results. Key
expiry is announced in the November 2024 release notes but absent from the
current credentials page. There is no IP allowlist or mTLS. The endpoint can be
tied to the coordinator only through the key, envelope authentication inside the
handler, and worker and timeout caps that bound the cost of a leaked key ([R31]).

### Recommendations by traffic

| Traffic | Verdict | Conditions |
| --- | --- | --- |
| Paid customer inference | **Conditional; not yet.** The CPU Container stays primary and RunPod is at most a fallback or burst path. | The provider-boundary decision; written confirmation of Secure Cloud placement for Serverless and audit scope; a DPA if documents can contain personal data; every mitigation below; a dedicated endpoint and key pinned to named data centres; a qualified GPU profile with its own calibrator. |
| Coordinator hidden evaluation | **Not acceptable for the pilot.** Keep it on Cloudflare. | Revisit only by explicit decision accepting runtime exposure of held-out text, and then with FlashBoot off, a fresh worker per batch, a dedicated endpoint and key, and no pre-reveal artifacts. |
| Public synthetic benchmarks and load tests | Acceptable once an account is authorised. | Public synthetic text only, under the production envelope and logging rules. |

Required mitigations if RunPod is used at all:

1. **Envelope.** XChaCha20-Poly1305 with HKDF-SHA256 subkeys per direction and
   purpose; associated data binds version, direction, key id and attempt
   identity. Separate key, endpoint and API key per purpose. Rotate keys on a
   schedule and on membership change.
2. **No content in logs.** No prints of inputs or outputs; a catch-all that
   returns only an enumerated error code inside the envelope; a local test that
   runs a canary string through the handler and asserts it appears in neither
   stdout nor stderr.
3. **Short lifetimes.** A short per-request TTL (queue-based) or the synchronous
   load-balancing path; no `webhook` or `s3Config` fields carrying content; no
   network volumes for payloads.
4. **Pinning.** Image by digest from a private registry, weights verified by
   SHA-256 at load, keys never in the image, FlashBoot set explicitly, exposed
   ports off, no SSH keys on the production account.
5. **Tamper detection.** Output invariants (share between the model-only and
   involvement probabilities), known-answer canaries, and sampled re-scoring on
   the CPU path, with a mismatch treated as a provider fault.
6. **Content-free receipts** with keyed commitments only, never bare text hashes
   (see [Receipts](#receipts)).

## Architecture

```mermaid
flowchart LR
  C[Customer] --> API[API Worker<br/>auth, quote, idempotency]
  API --> J[Job DO<br/>durable job, tokenization, heads, calibrator]
  J --> BT[Batcher DO<br/>batches, attempts, circuit breaker]
  BT -->|sealed token batch| RP[(RunPod load-balancing endpoint<br/>Rust axum + ONNX Runtime CUDA)]
  RP -->|sealed pooled vectors| BT
  BT -. circuit open or budget cap .-> CT[CPU Container<br/>int8 or f32]
  CT --> BT
  BT --> J
  J --> RC[Receipt DO and R2]
  CR[Cron sweep, 1 per minute] --> BT
```

- **RunPod does bounded computation only.** Job state, batching, retries,
  attempt identities, budgets and receipts live in Durable Objects, Queues and
  R2, as for the Container. RunPod's queue and result store are never the system
  of record. Queue messages carry identifiers only.
- **Endpoint type: load-balancing.** It is the documented contract for a
  non-Python worker: a Rust `axum` server answering `/ping` and one encode
  route. It costs a queue, so the Batcher DO owns backpressure and retries
  through cold starts, within the 30 MB and 5.5-minute limits. A queue-based
  endpoint would add buffering and `/status` recovery but depends on the
  undocumented worker protocol; the prototype implements that protocol, and its
  client, envelope and receipt logic carry over to either type. Whether the
  load-balancing gateway enforces the API key is undocumented, so the handler
  authenticates every request itself.
- **Split: encoder on the GPU; tokenization, heads, calibrator, invariants and
  receipts in the Worker or Durable Object.** The heads consume one pooled
  768-dimensional vector per document (3 KB at f32), which plain Wasm f32
  computes identically on every host. This keeps calibration versioned with *A*
  rather than inside an image, keeps a customer's private writer vector inside
  Cloudflare, and makes the CPU fallback differ only in the encoder vector. If a
  later share head reads token states, the payload grows to about 3 MB per
  2,048 tokens and the whole model may be better placed on the GPU.
- **Batching and shapes.** Flush by padded tokens, document count or age (25-150
  ms, capped by the latency budget). Use a fixed shape set: length buckets of
  128 to 2,048 tokens and a few fixed batch sizes, so ONNX Runtime can warm every
  shape at load and results do not depend on batch companions. Unpadded varlen
  execution is faster but allowed only if the batch-invariance test passes.
  Documents longer than 2,048 tokens are split into the manifest's declared
  windows and combined in the Worker, with window diagnostics kept apart from
  the calibrated document score.
- **Attempts and duplicates.** Each dispatch has a deterministic attempt
  identity persisted before the request (`PLANNED → DISPATCHING → RESPONDED →
  VERIFIED → APPLIED`). The handler is a pure function of artifact, profile and
  input, and the Durable Object accepts the first verified response. A later
  response for the same attempt with the same output digest is recorded as a
  duplicate for billing; a different digest opens the circuit as a
  nondeterminism incident and never replaces the first result.
- **Timeouts and fallback.** Live batches get a 10 s client deadline and at most
  3 GPU attempts or 2 s of added delay before the CPU Container takes over. A
  circuit breaker per endpoint and profile opens on consecutive failures, error
  rate, delay, a canary or duplicate mismatch, or a detected endpoint
  configuration change, and closes after three canaries match their recorded
  digests. Until the circuit is warm, live traffic stays on the Container, so a
  GPU cold start never stalls a customer.
- **Endpoint hygiene.** A daily synthetic canary on every endpoint, which also
  stops RunPod's 3- and 7-day idle scale-down from silently disabling a
  fallback endpoint. Alerts on circuit state, canary mismatch, unexpected GPU
  type, configuration change and GPU-seconds burn rate.
- **Cost controls.** Max workers 3 for live traffic at launch, active workers 0,
  idle timeout 5 s, tight execution timeouts, and a GPU-seconds ledger in the
  Durable Object with hourly and daily caps. Reaching a cap moves live traffic
  to the Container. The handler fails fast on configuration errors, because
  crash loops are billed.
- **Region.** A small set of US data centres near the coordinator's Durable
  Object placement, with at least four allowed so that a single-GPU-type
  endpoint can still be scheduled. EU customers would need a separate endpoint
  restricted to EU data centres. Cloudflare-to-RunPod round-trip time is
  unmeasured.
- **If hidden evaluation ever moved to a GPU,** it would need its own endpoint
  on a single GPU type, artifacts loaded by digest from short-lived presigned
  R2 URLs into one fixed graph template per architecture and profile (miners
  never supply graphs or code), one scoring profile frozen per cohort, and
  backend outages that pause dispatch without advancing, expiring or
  re-profiling the cohort.

### Receipts

Each dispatched request writes one content-free receipt object to R2 with one
entry per attempt, bound into the coordinator's signed package as today. Entries
carry the attempt and invocation identities, the artifact, manifest and
calibrator digests, the numeric profile, the image and runtime digests, the GPU
type reported by the handler, an opaque endpoint alias and keyed digests of
provider job ids (never account identifiers), request and response ciphertext
digests and sizes, keyed input and output commitments, timing, token counts and
status. They never carry text, tokens, vectors, outputs or bare text hashes.
Uncertain outcomes are recorded as such and reconciled. Because RunPod has no
idempotency key, one logical attempt may map to more than one billed provider
job. A receipt remains an operator assertion: nothing proves RunPod ran the
pinned image.

## Qualification plan for a GPU build

Profiles, each with its own identity in the *A* release record:

| Profile | Where | Class | Calibrator |
| --- | --- | --- | --- |
| Exact CPU reference (f64 and fast f32 graphs) | CPU | exact (existing replay gate) | the release calibrator |
| GPU fp32 with TF32 off | ONNX Runtime CUDA | exact only if it passes the existing replay gate unchanged | shares the exact-class calibrator |
| GPU fp16 (softmax, LayerNorm and pooling in fp32) | ONNX Runtime CUDA | approximate | its own, fitted on its outputs |
| CPU mixed int8 | Container | approximate | its own, fitted on its outputs |

Zero-spend steps on the local RTX 4090, after the baseline release frees the
GPU, as durable jobs with memory limits and zero swap:

1. **Throughput.** fp32 (TF32 off), fp32 with TF32 and fp16, at 512 and 2,048
   tokens and batch sizes 1-64, with per-process GPU memory. This replaces the
   planning throughput in every cost row.
2. **Exactness.** Run the GPU fp32 build against the existing replay gate
   (raw encoding 1e-4, shared encoding 1e-3, outputs 1e-5, involvement logit
   1e-4). The tolerances are not relaxed afterwards; a build that misses them is
   approximate class.
3. **Approximate-class quality.** Hold any GPU serving profile to the same
   pre-registered no-measurable-reduction gate as the int8 CPU build, against
   the f32 release model on raw and calibrated outputs: Brier, human flags,
   TPR, AUROC, attribution, share error, per-domain and per-generator cells,
   and long-document parity. A profile without its own fitted and gated
   calibrator is not servable.
4. **Determinism and batch invariance.** Same image, GPU type and shape give
   bit-identical output across 3 runs and worker restarts, with deterministic
   compute on, CPU fallback disabled and every node asserted on the CUDA
   provider. A window gives the same pooled vector at every batch position, with
   5 random companion sets and in every batch size of its bucket.
5. **Handler costs.** Session creation, shape warm-up and artifact load time,
   and the Wasm tokenizer and real heads per document.
6. **Image.** Build the runtime image on build-host (no CI): the Rust handler,
   ONNX Runtime GPU libraries matching the CPU build's 1.31 line, the graph
   templates and, for the live image, the release weights; no Python; pinned
   base by digest; non-root. Record image digest to runtime digest in the release
   record. A changed image or runtime is a new runtime digest that needs
   requalification; a changed graph or precision is a new profile.

Steps that need an account (not authorised now): cold-start and FlashBoot
distributions, queue delay, Cloudflare-to-RunPod round-trip time, whether the
load-balancing gateway enforces the key, whether `image@sha256:` references
are accepted, R2 download speed from candidate data centres, Secure Cloud
placement, key scoping and expiry, billing alerts, and a canary on the target
GPU pool. Every RunPod GPU type an endpoint may fall back to needs its own
canary digests, and a scoring profile used for evaluation would be pinned to
one type.

## Prototype status

A Cargo workspace of about 3,100 lines of Rust, in ignored storage:

- **Worker-side client** (`worker-rs` 0.8.7, a 459 KB wasm32 build, and a native
  build). It batches by count and bytes, seals the batch, submits with
  `/runsync`, polls `/status`, cancels at the deadline, retries under one
  idempotency key, validates the response (invocation, count, model hash and
  output bounds), calls a fallback hook and writes one content-free receipt per
  attempt to R2.
- **Envelope.** XChaCha20-Poly1305 with HKDF-SHA256 subkeys for request,
  response, idempotency, content commitment and opaque references; tests bind
  direction, idempotency key, key id and tampering.
- **RunPod-compatible worker.** It speaks the queue worker protocol as observed
  in runpod-python (job-take, job-done and heartbeat), checks the model hash,
  tokenizes, builds length-bucketed micro-batches bounded by documents, tokens
  and b·L², and runs *A* through ONNX Runtime 1.31.1 via `ort` 2.0.0-rc.10. A
  CUDA feature build loads the CUDA 13 runtime and fails cleanly when no device
  is visible.
- **Mock** of the client and worker APIs, enforcing execution timeout, TTL,
  result retention and payload limits, with fault injection.

Results on CPU (int8 candidate, 4 threads, contended host at load 10-22), all on
synthetic text:

- **Overhead.** Transport, cryptography and JSON add 0.9-2.5 ms per batch;
  inference is the rest (0.22 s for one 512-token document, 1.2 s for one
  2,048-token document). Ciphertexts are 2.9 KB for one document and 91 KB for
  16,000 tokens. The mock adds no queue delay, so RunPod's real queue and
  dispatch overhead is unmeasured.
- **Fault scenarios.** Two submit 503s, a lost result, no capacity, a wrong
  shared secret and a model-hash mismatch each produced the designed receipts
  and outcome, with one idempotency key across attempts. No plaintext appeared
  in any of 64 transport bodies or 27 receipts.
- **wasm32 gateway under local workerd.** 24 documents went Worker to mock to
  handler and back in one request; outputs equal in-process scoring to 2.8e-17
  in probability.
- **Batching on CPU buys nothing.** Engine time per document is flat or worse as
  the batch grows (0.172 s at batch 1 against 0.216 s at batch 32, 512 tokens).
- **The int8 build is not shape-invariant.** Its per-tensor activation
  quantization makes a document's score depend on its batch companions (max
  |ΔP| 0.0077, share up to 0.49 points) and on the padding multiple (64 against
  128: max |ΔP| 0.0089, share up to 0.45 points), with no decision flips. The
  fp32 banded graph is bit-identical in both tests. On CPU, score int8 one
  document per session, or batch only fp32 or fp16 graphs.
- **The GPU ONNX Runtime build on the CPU provider** reproduces the CPU build
  exactly.

A review of the prototype found two defects to fix before any reuse:

- **Stale idempotency cache.** The handler's warm cache is keyed by the
  idempotency key alone. A second request reusing an invocation id with a
  different document received the first document's scores, and the client's
  response checks did not detect it. The cache key and the sealed response must
  bind the request's content commitment or ciphertext digest.
- **Orphaned job on submit timeout.** With a client deadline shorter than the
  `/runsync` wait and no capacity, the submit timed out before a job id
  returned, so no cancel could be sent; the job ran once capacity returned and
  would have been billed, and the fallback did not run. The client must keep its
  wait below the deadline, set a short TTL, treat a submit timeout as uncertain,
  fall back, and reconcile.

**The GPU benchmark is pending until after the baseline release.** It is
prepared as a durable job that waits for the baseline release and the GPU lock:
fp32 with TF32 off, fp16 and fp32 with TF32 at 512 and 2,048 tokens and batch
sizes 1-64, with parity against a CPU f32 reference on 500 synthetic texts of
16-2,048 tokens. Until it runs, every GPU figure in this note is a planning
estimate.

## Decisions needed

1. **Provider boundary.** Accept RunPod, and whoever operates its GPU hosts, as
   an additional party with plaintext access at compute for paid customer
   documents? If so, the design's trust statement and customer privacy terms
   must name it.
2. **Hidden evaluation.** Confirm that hidden evaluation stays on Cloudflare
   (recommended). Moving it later would be a separate decision accepting runtime
   exposure of held-out text.
3. **Contact and account.** Authorise contact with RunPod to obtain written
   confirmation of Secure Cloud placement and audit scope for Serverless, who
   operates Secure Cloud hardware (RunPod or partner data centres), the DPA,
   the confidential sub-processor list and retention behaviour, and an
   active-worker quote; and authorise an account for the measurements that
   need one. Both are outside this assessment.
4. **When the GPU path is used.** Proposed triggers: the int8 CPU build fails its
   gate and f32 CPU latency or cost is unacceptable; a sustained live rate above
   about 5 documents/s at 1,000 words or more; or long-document jobs with a
   latency target the Container cannot meet.
5. **Serving parity.** Serve GPU traffic only under a profile that passes the
   same quality gate as the CPU serving build, with its own calibrator, or only
   under an exact-class GPU fp32 profile.
6. **Endpoint type.** Load-balancing with a Rust server (recommended), or
   queue-based with a Rust implementation of the undocumented worker protocol.
7. **Relation to the all-Cloudflare goal.** RunPod sits outside Cloudflare,
   unlike the Container, Workers AI and distilled-Worker routes. Decide whether
   an external GPU fallback is acceptable at all, or whether the fallback for a
   failed int8 gate is the f32 CPU Container.

## Changes needed elsewhere

Not made here. If decisions 1 and 7 accept a RunPod fallback,
[the Cloudflare design](cloudflare-subnet-draft.md) would need:

- **Launch scope and privacy.** The statement that Cloudflare and Slop Ninja are
  trusted with text, and that Cloudflare is the trusted inference host, would
  name the GPU provider for paid traffic. A new subsection under *Privacy and the
  provider boundary* would cover the envelope, the provider's log and job
  retention, the no-content-logging rule for the handler, and the fact that the
  envelope does not protect against the GPU host.
- **Ownership and execution; Rust participants and serverless coordination.**
  Name the external GPU backend as bounded computation with no cohort authority,
  like the Container, with Durable Objects as the system of record, ONNX Runtime
  CUDA as a native dependency, and a Rust handler with no Python.
- **Models and version registration.** Execution manifests list GPU profiles
  with their class, gate, calibrator and runtime digest, and serving binds one
  profile per job.
- **Certified R2 inference receipts.** Add the profile, image and runtime
  digests, GPU type, opaque endpoint alias, keyed provider job references and
  the case of several billed provider jobs per attempt.
- **Initial cost scenarios and scale.** Add the GPU rows and break-even rates
  above, billed idle time, provider spend caps and the triggers in decision 4.
- **Tempo-aligned cohorts.** Only if evaluation ever uses a GPU: a backend outage
  pauses dispatch, and a cohort never switches profile midway.

The whitepaper's trust statement would change in step. AGENTS.md would need:

- the coordinator and execution rules, which name Cloudflare Workers, Durable
  Objects, Queues, R2 and Containers as the only execution places, extended to a
  qualified external GPU backend for bounded paid-inference computation, with
  hidden evaluation kept on Cloudflare;
- the trust statement for the later *B* service, if *B* proposers could ever use
  the same backend;
- a recorded provider choice, alongside the existing provider decisions, once
  decision 3 settles placement and audit scope;
- the Rust-only rule applied to the GPU handler, with ONNX Runtime CUDA listed as
  a native dependency.

[Miner models and training data](miner-models-and-data.md) says Slop Ninja
serves admitted public artifacts in its own infrastructure; that sentence would
name the provider if miner artifacts were ever served from it.

## Limits

- No RunPod measurement of any kind: queue delay, cold start, FlashBoot hit rate,
  network latency and billing behaviour all need an account.
- No GPU measurement on *A*. Every GPU cost and latency figure rests on a halved
  published throughput figure and tier-scaling estimates.
- The mock follows RunPod's docs and runpod-python's code, not RunPod itself.
- CPU figures come from one shared consumer CPU under load. Container vCPU
  speed, cold start and idle cost are unmeasured.
- Prototype outputs are uncalibrated, and share and attribution supervision are
  out of scope.
- Several RunPod pages contradict each other (FlashBoot default, TTL minimum,
  GPU prices, Secure Cloud ownership, data-centre restriction). Each conflict is
  recorded above; the live behaviour needs confirmation.
- Prices are list prices on 2026-10-28 and exclude any negotiated discount.

## Sources

All accessed 2026-10-28.

| Ref | Page | Page date |
| --- | --- | --- |
| R1 | [Send API requests](https://docs.runpod.io/serverless/endpoints/send-requests) | undated |
| R2 | [Operation reference](https://docs.runpod.io/serverless/endpoints/operation-reference) | undated |
| R3 | [Load balancing endpoints](https://docs.runpod.io/serverless/load-balancing/overview) | undated |
| R4 | [Endpoint settings](https://docs.runpod.io/serverless/endpoints/endpoint-configurations) | undated |
| R5 | [Batch jobs](https://docs.runpod.io/serverless/batch-jobs) | undated |
| R6 | [Release notes](https://docs.runpod.io/release-notes) | entries dated per release |
| R7 | [Serverless SDKs](https://docs.runpod.io/serverless/sdks) | undated |
| R8 | [Environment variables](https://docs.runpod.io/serverless/development/environment-variables) | undated |
| R9 | [PyPI `runpod` metadata](https://pypi.org/pypi/runpod/json) | per release |
| R10 | [Serverless pricing](https://docs.runpod.io/serverless/pricing) | undated |
| R11 | [Failed requests, real charges](https://www.runpod.io/blog/failed-requests-real-charges-how-runpod-serverless-billing-works-when-workers-fail) | 2026-10-16 |
| R12 | [Pricing](https://www.runpod.io/pricing) | "Updated October 8, 2026" |
| R13 | [Serverless product page](https://www.runpod.io/product/serverless) | "Updated September 27, 2026" |
| R14 | [Workers overview](https://docs.runpod.io/serverless/workers/overview) | undated |
| R15 | [GPU types](https://docs.runpod.io/references/gpu-types) | undated |
| R16 | [Serverless GPU cold starts and FlashBoot](https://www.runpod.io/blog/serverless-gpu-cold-starts-flashboot) | published 2026-07-14, updated 2026-10-10 |
| R17 | [Cached models](https://docs.runpod.io/serverless/endpoints/model-caching) | undated |
| R18 | [Deploy workers](https://docs.runpod.io/serverless/workers/deploy) | undated |
| R19 | [Network volumes](https://docs.runpod.io/storage/network-volumes) | undated |
| R20 | [Rolling releases](https://docs.runpod.io/serverless/endpoints/rolling-releases) | undated |
| R21 | [Status page](https://uptime.runpod.io/) | "Last updated on Oct 28, 2026 at 7:30pm UTC" |
| R22 | [Incident history](https://uptime.runpod.io/incidents) | incident dates as listed |
| R23 | [Serverless Enterprise](https://www.runpod.io/serverless-enterprise) | "Updated September 27, 2026" |
| R25 | [Security and compliance](https://docs.runpod.io/references/security-and-compliance) | undated |
| R26 | [Logs](https://docs.runpod.io/serverless/development/logs) | undated |
| R27 | [Data processing agreement](https://www.runpod.io/legal/data-processing-agreement) | undated |
| R28 | [Configuring Runpod](https://www.runpod.io/blog/configuring-runpod) | published 2026-07-27, updated 2026-09-13 |
| R29 | [Compliance](https://www.runpod.io/legal/compliance) | "Updated September 13, 2026" |
| R30 | [Privacy policy](https://www.runpod.io/legal/privacy-policy) | "Last updated on August 7th, 2025" |
| R31 | [Credentials and API keys](https://docs.runpod.io/get-started/credentials) | undated |

Repositories, read as shallow clones: `github.com/runpod/runpod-python` at
`714a5655` (2026-10-28), `github.com/runpod/docs` at `70e07c1f` (2026-10-28),
`github.com/runpod-workers/worker-vllm` at `71d503e6` (2026-10-21) and
`github.com/runpod-workers/worker-basic` at `e6c0944` (2025-07-03). Cloudflare
limits and prices are as cited in the [CPU inference note](a-cpu-inference.md).

# f32 serving of A on Cloudflare Containers

Measured on October 29, 2026. This note qualifies the serving path chosen in
[A inference on CPU](a-cpu-inference.md#recommendation): the baseline release *A*
([release candidate](a-baseline-release.md)) in f32, on a CPU Container that a
Worker calls. Int8 failed its no-quality-loss qualification
([mixed-int8 CPU serving](a-int8-serving.md)), so f32 is the only serving
profile considered here. Earlier figures came from local cgroups shaped like a
Container. These come from Cloudflare.

The test ran in the operator's own Cloudflare account, as a private,
test-named deployment that was deleted at the end. Only synthetic text, written
for this test, left the host: no evaluation split, private or customer-like
text. Model weights travelled inside the image. Model files, raw responses and
receipts stay in ignored storage.

## Summary

| Question | Answer |
| --- | --- |
| f32 parity on Cloudflare | **Pass.** Against the release f64 replay runtime, the largest output difference is 4.7e-6 (attribution scores). Confidence is identical, share differs by at most 1.2e-7 and labels match. The encoder output *h* differs by 8.8e-5. All are within the release replay gate. In that test (one instance of each size), outputs were bitwise identical across standard-2, -3 and -4. Across many instances they are not: in the fan-out load rehearsals of 2026-10-31, copies of one artifact scored on different lanes differed from each other by an RMS of 3 to 6e-8 in confidence, and their mean Brier by up to about 1e-9 (cause not verified; CPU kernel selection across hosts is the likely one). |
| Container CPU | AMD EPYC, family 25 model 17 (Zen 4), 2.55 GHz, with AVX-512 including VNNI and BF16. The same model on every size. |
| Warm latency, 2,048 tokens (inside the Container, p50 / p95) | standard-2: 7.9 / 8.6 s. standard-3: 4.7 / 5.1 s. standard-4: 3.2 / 3.8-4.8 s. This is 1.6× (one vCPU) to 2.5× (four vCPUs) slower than the local test host. |
| Warm latency, about 300 tokens | standard-2: 0.90 / 1.06 s. standard-3: 0.66 / 0.83 s. standard-4: 0.40-0.47 / 0.54-0.79 s. |
| Worker forwarding overhead | 0.11-0.35 s per request (p50 health round trip from the test host), depending on where the instance was placed. |
| Cold start from zero instances | The container answers in a median 0.5 s. The model is ready after a median 9.4 s (4.6-14.6 s) and the first 512-token result arrives after a median 11.9 s. Loading the 601 MB graph takes most of it. The first start after deploy took 27 s, and one restart stalled (see [Cold start](#cold-start)). |
| Throughput, standard-4 | Four 1-thread sessions: 0.35-0.42 documents/s at 2,048 tokens and 3.2-3.3 documents/s at about 300 tokens. One 4-thread session: 0.29-0.33 and 1.3-2.0. |
| Cost per 1,000 documents, busy time | 2,048 tokens: $0.23 (standard-4, four sessions) to $0.34 (standard-4, one session). About 300 tokens: $0.027-0.050. That is about 3× the earlier int8 projection. |
| Idle and keep-warm | Awake but idle: $0.057 per hour (standard-2), $0.076 (standard-3), $0.113 (standard-4). Idle CPU was under 0.6% of a vCPU. |
| Memory | One session peaks at 0.80-0.81 GB, four sessions at 4.7 GB. The image is 582 MB. |
| Teardown | The Worker, the three Container applications and their images were deleted. Wrangler lists no containers or images, and reports that the Worker does not exist. |

## What was deployed

The server source, served profile and Dockerfile are tracked in
[`inference/a-container-serve`](../inference/a-container-serve/README.md). The
model files, the ONNX Runtime library and the measurement files stay in ignored
storage.

- **Image.** Runtime base `gcr.io/distroless/cc-debian12:nonroot`, pinned by
  digest. It contains:
  - the ONNX Runtime 1.31.1 CPU library from the `onnxruntime-1.31.1` cp312
    `manylinux_2_28_x86_64` wheel, the same library as the local measurements;
  - the f32 banded ONNX graph of the release weights (601 MB), exported for the
    int8 qualification;
  - the release `tokenizer.json`, `heads.json`, `contract.json`,
    `calibration.json` and `manifest.json`;
  - a 5.8 MB Rust server.

  Everything is read-only and runs as a non-root user. The binary was built in
  `rust:1.99.1-slim-bookworm`, pinned by digest, from a locked dependency set:
  `ort` 2.0.0-rc.10 (dynamic loading), `tokenizers` 0.22.2 and `tiny_http`
  0.12.0.
- **Server.** It tokenizes with CLS…SEP and no truncation; inputs over 2,048
  tokens return `unsupported_length` and no prediction.
  - The encoder runs as in the local measurements: graph optimisation level 3,
    one inter-op thread, batch 1, padded to a multiple of 64 with a masked pad.
  - Only the graph's `h` output is used. The release f64 heads runtime then
    computes the origin, the calibrated confidence and share, and the
    conditional attribution with its thresholds. The measured build's sources
    were byte-identical to the ones bound in the release replay receipt; the
    current sources give bit-identical results
    ([Heads runtime update](#heads-runtime-update)).
  - At startup the server checks the SHA-256 of the small model files and of
    the heads and calibration sources, and repeats the release runner's
    export, contract and calibration guards. The measured build hashed the
    601 MB graph after readiness and only reported the digest; the tracked
    source refuses to report ready until the graph digest matches the
    [served profile](../inference/a-container-serve/README.md#served-profile).
  - It listens at once and loads the model in the background, so time to first
    response and time to model ready are reported separately.
  - Session count and threads per session come from the environment.
  - It logs no text, ids or tokens.
- **Worker.** One test-named Worker on `workers.dev`, with no route, custom
  domain, preview URL or log capture. Every request needs a bearer secret set
  with `wrangler secret` and compared in constant time; requests without it or
  with a wrong one received 401.
  - Three Durable Object classes, each bound to a Container application of a
    different size (standard-2, standard-3, standard-4), each with
    `max_instances: 2`.
  - The Durable Object name sets sessions and threads. On the first request the
    object starts the container with Internet access disabled, then retries the
    port until it answers.
  - The inactivity timeout was 10 minutes. Test endpoints stopped instances
    explicitly between cold-start cycles.
- **Clients.** All requests came from the test host through the Worker, using a
  Rust measurement client. Runs on different instances sometimes overlapped in
  time; they share no CPU but share the network path.

## Parity

Requests: the three release replay requests, three synthetic documents at each
of about 300, 512, 1,024 and 2,048 tokens, and one 5,944-token document that
must be rejected. The synthetic texts were generated for this test from a fixed
seed and fixed sentence templates.

The references are:

- **Release f64 replay:** the release `full_a` runtime (Candle f64, one thread)
  on the same requests. On the three replay requests it reproduces the
  release's recorded replay comparison exactly (outputs 7.3e-7).
- **Release reference predictions:** the export's recorded outputs. They exist
  only for the three replay requests.
- **Local f32 image:** the same image run locally on an Intel Raptor Lake host
  (AVX2, no AVX-512).

Maximum absolute difference of the Cloudflare outputs (one set; standard-2, -3
and -4 were bitwise identical in this test, one instance each; see the summary
for the differences measured across many instances):

| Output | vs release f64 replay (15 requests) | vs reference predictions (3) | vs local f32 image (15) | Release replay gate |
| --- | ---: | ---: | ---: | ---: |
| Confidence (calibrated involvement) | 0 | 0 | 0 | 1e-5 |
| Share | 1.2e-7 | 1.3e-7 | 2.4e-7 | 1e-5 |
| Origin probabilities | 4.4e-7 | 2.3e-7 | 2.3e-7 | 1e-5 |
| AI conditional attribution scores | 4.7e-6 | 4.4e-7 | 1.4e-6 | 1e-5 |
| Involvement logit (raw) | 3.2e-5 | 1.7e-6 | 1.4e-5 | 1e-4 |
| Origin logits (raw) | 2.3e-5 | 1.8e-6 | 7.5e-6 | – |
| Encoder output *h* | 8.8e-5 | 8.8e-6 | 1.2e-5 | 1e-4 |
| Attribution labels | identical | identical | identical | – |

The over-limit document returned `unsupported_length` on every size.

**Verdict:** f32 ONNX serving on Cloudflare's Zen 4 CPUs reproduces the release
replay reference within the release replay gate on every compared output.

Limits of the verdict:

- **Saturated confidence.** The synthetic texts all fall on the top level of
  the calibration map (99.988%), so they do not exercise calibrated confidence
  between levels. The two short replay requests (90.9% and 98.0%) do, and the
  raw logits and share, which vary across all requests, are compared directly.
- **Coverage.** Synthetic text only, on one CPU model. This does not replace a
  distributional parity check on evaluation-like text run inside the provider
  boundary.
- **Instruction set.** The local check ran ONNX Runtime's AVX2 path and
  Cloudflare ran its AVX-512 path. Their outputs differ by up to 1.4e-6 on
  outputs, so a host with another feature level needs its own check.

## Heads runtime update

On October 31, 2026 the served profile's heads runtime pins moved to the
current `grammar-a-baseline` sources: `full_a.rs` (which now also reads the
span-head export v3), `full_a_spans.rs` (compiled because `full_a.rs` uses it),
`involvement_calibration.rs` (refactored without a numeric change) and
`shared.rs` (unchanged). The served package is still R0 (export v2, no token
head), so the Container returns no sentences or spans. Main's previous pins
named an older `involvement_calibration.rs`, so a build from main refused to
start until this update.

The f32 path was requalified locally on one Intel Raptor Lake host (AVX2),
with the same served files and ONNX Runtime library, on 81 requests (76
scored, 5 unsupported): the 16 parity requests above, the 9 release latency
requests, the 15 synthetic span-reference requests and the 41-case warm-A
roster.

- **Before and after.** `a-container-serve` (one session, four threads) built
  from the current sources and from the receipt's sources returns
  bit-identical responses: every prediction, raw output, graph output, `h`,
  digest and status (64,676 numbers). `a-scorer` (two sessions, one thread)
  built from the current sources and the deployed testnet scorer binary
  (`9efb681a…`, the receipt's sources) return bit-identical results, output
  digests included. The current server also matches every number of the
  October 29 local image run on its 16 requests, and the scorer's predictions
  equal the server's bit for bit.
- **Against the release FP64 replay.** Against the current `full_a` FP64
  runtime on the 75 requests both score, the largest differences are 6.7e-6
  (attribution scores), 1.5e-6 (share), 1.3e-6 (origin), 2.1e-5 (involvement
  logit) and 3.6e-5 (*h*), with confidence identical and labels equal: within
  the release replay gate. On the 15 parity requests they are smaller than in
  the table above (*h* 1.4e-5 against 8.3e-5 for the local image), because the
  current FP64 runtime builds its rotary tables as the trained float32 function
  does ([Runtime](a-baseline-release.md#runtime)).
- **Input support.** One warm-roster case, text made only of the control
  separators U+001C to U+001F, is `empty_text` in the reference runtime, which
  treats them as whitespace as Python does, but is scored by the Container,
  whose empty-text check uses Rust whitespace. This difference predates the
  update and is unchanged by it.

## CPU, memory and image

- **CPU, every size:** "AMD EPYC", AuthenticAMD family 25 model 17 stepping 2,
  microcode 0x1000065, reported at 2.55 GHz. The flags include AVX2, FMA, F16C,
  AVX-512 F/DQ/BW/VL/CD/IFMA/VBMI/VBMI2/VNNI/BF16/BITALG/VPOPCNTDQ, SHA, VAES
  and GFNI, but no VEX AVX-VNNI. The guest sees 1, 2 or 4 processors, each its
  own core with no SMT siblings. The kernel is `6.18.54-cloudflare-microvm`.
  The guest reports 6.0, 8.0 and 11.9 GiB of memory and no swap. No cgroup CPU
  or memory limit is visible inside the VM.
- **Memory:** the process peaks at 0.80-0.81 GB with one session of any thread
  count, up to 2,048 tokens. Four sessions after saturated 2,048-token traffic
  peak at 4.68 GB. All sizes fit; standard-2 holds one session with about
  5 GiB to spare.
- **Image:** 582 MB compressed; the graph alone is 601 MB uncompressed. Wrangler pushed
  one repository per Container application.

## Warm latency

There was one warm-up run per document, then five timed repeats of each of
the three documents per length: 15 samples per cell. "Server" is the time
inside the Container (tokenizer, queue, encoder, heads). "Through Worker" is
measured at the client on the test host. Each cell gives p50 / p95 in seconds,
with one session using all vCPUs as threads.

| Size | Tokens | Server | Through Worker |
| --- | ---: | ---: | ---: |
| standard-2 (1 vCPU) | 300 | 0.90 / 1.06 | 1.04 / 2.76 |
| | 512 | 1.50 / 1.65 | 1.57 / 6.15 |
| | 1,024 | 3.30 / 3.78 | 3.41 / 6.39 |
| | 2,048 | 7.92 / 8.55 | 8.01 / 8.70 |
| standard-3 (2 vCPU) | 300 | 0.66 / 0.83 | 0.77 / 0.93 |
| | 512 | 1.08 / 1.31 | 1.16 / 1.38 |
| | 1,024 | 2.03 / 2.48 | 2.11 / 2.58 |
| | 2,048 | 4.72 / 5.15 | 4.80 / 5.21 |
| standard-4 (4 vCPU), run 1 | 300 | 0.40 / 0.54 | 0.70 / 0.81 |
| | 512 | 0.78 / 1.04 | 1.10 / 1.32 |
| | 1,024 | 1.31 / 1.68 | 1.58 / 1.95 |
| | 2,048 | 3.25 / 4.82 | 3.56 / 5.10 |
| standard-4, run 2 (no other runs in flight) | 300 | 0.47 / 0.79 | 0.77 / 1.07 |
| | 512 | 0.78 / 1.00 | 1.06 / 1.31 |
| | 1,024 | 1.49 / 1.96 | 1.82 / 2.26 |
| | 2,048 | 3.24 / 3.78 | 3.57 / 4.09 |

- **Against the local host.** Quiet local measurements on record (Intel i9-13900KF,
  same graph and library) are 5.0 s for one thread and 1.27 / 1.30 s for four
  vCPUs at 2,048 tokens. One Cloudflare vCPU is therefore about 1.6× slower than
  a local P-core thread, and four vCPUs are 2.5× slower, so threading scales
  worse here. The 1.30 s p95 planning figure does not hold on Cloudflare; plan
  on about 3.2-3.8 s on standard-4.
- **Overhead.** The difference between the two columns is the network and
  forwarding path from the test host. Health checks measured it separately at
  0.11-0.35 s p50. One standard-4 instance took 0.24 s from Durable Object to
  container alone, against about 0.1 s for the others, so placement varies by
  instance. The large p95 values through the Worker on standard-2 came from
  runs that overlapped other tests on the same client.

## Throughput

All throughput runs were on standard-4. Each run sent a fixed list of
documents with 1-4 concurrent clients, after one warm-up request per client.
Standard-2 and standard-3 were run with one client only.

| Configuration | Clients | 2,048 tokens: documents/s (tokens/s) | about 300 tokens: documents/s (tokens/s) | Per-request p50 at 2,048 |
| --- | ---: | --- | --- | ---: |
| standard-4, 4 sessions × 1 thread | 4 | 0.34-0.42 (705-857) | 3.16-3.32 (943-992) | 8.9-9.6 s |
| | 2 | 0.23 (466) | – | 8.5 s |
| standard-4, 1 session × 4 threads | 1 | 0.29 (598) | 1.30-1.33 (388-396) | 3.4 s |
| | 2 | 0.31 (635) | – | 6.2 s (queued) |
| | 4 | 0.33 (686) | 2.04 (611) | 11.2 s (queued) |
| standard-3, 1 session × 2 threads | 1 | 0.21 (437) | 1.47 (439) | 4.7 s |
| standard-2, 1 session × 1 thread | 1 | 0.12 (247) | 0.91 (271) | 8.2 s |

- **Throughput against latency.** For throughput, four single-thread sessions
  beat one four-thread session by 1.1-1.6×. For latency, one four-thread
  session is 2.7× faster per document. A coordinator can run both on
  standard-4: one session for interactive requests, or four for batch backfill.
- **CPU use.** Measured with the guest's cgroup CPU counter, a 2,048-token
  document costs:
  - 8.2 vCPU-seconds on a single-thread session (8.2 on standard-2, 8.2 on
    standard-4);
  - 9.1 on a two-thread session;
  - 11.9 on a four-thread session.

  At about 300 tokens the figures are 0.93-0.97, 1.19 and 1.84. Extra threads
  add CPU time per document, and Containers bill CPU on active use.
- **Client errors.** 3 of 654 benchmark requests failed at the client during
  concurrent runs: two transport errors within 20 ms, likely a reused idle
  connection, and one truncated response body. No server-side error was
  recorded.

## Cold start

Each cycle stopped the instance through the Durable Object, waited for the
Durable Object to report it stopped, paused 10 s, then polled `/healthz` every
0.2 s and sent one 512-token request as soon as the model was ready. There
were 19 cycles across the three sizes. Six of them were the first start of a
Durable Object name.

| Measure | Median | Range (18 cycles) |
| --- | ---: | ---: |
| First response from the container process | 0.49 s | 0.28-1.37 s |
| Model ready, measured inside the process | 9.4 s | 4.6-14.6 s |
| Model ready, measured at the client | 10.4 s | 5.2-26.3 s |
| First 512-token result | 11.9 s | 6.4-27.2 s |

- **The first start after deploy** took 19.7 s to the first container
  response and 27.2 s to the first result, consistent with fetching the image
  onto a new host. Later starts and new instance names, on any size, answered
  within 1.4 s.
- **Model load.** Session creation, which reads the 601 MB graph from the
  container's disk, takes 3.9-8.3 s. The local host recorded 0.7-4 s for
  model load. Four
  sessions need 13.8-14.6 s to be ready. Load is the dominant cold-start cost.
  A smaller graph or a filesystem snapshot are the obvious ways to reduce it,
  and neither is qualified.
- **One failed restart.** On standard-4, one restart 10 s after a stop went
  wrong:
  - the first request got HTTP 500 from the Durable Object after 3.3 s;
  - the next waited out the test Worker's 180 s port timeout and got 504;
  - the following request started a fresh container, which answered in
    0.7 s.

  A production coordinator must catch container start failures, destroy and
  restart with a bounded backoff, and not hold requests on a dead port.
- **Scale to zero.** Each cycle starts from zero running instances for that
  Durable Object. The test did not measure starts after long idle periods,
  beyond the three never-run names, or in other regions.

## Cost

Containers pricing page (updated 2026-10-05): $0.000020 per vCPU-second of
active CPU, $0.0000025 per GiB-second of provisioned memory, and $0.00000007
per GB-second of provisioned disk, all billed per 10 ms while the instance
runs. Workers Paid includes 375 vCPU-minutes, 25 GiB-hours and 200 GB-hours
per month.

Cost per 1,000 documents is the measured active CPU seconds per document times
the CPU rate, plus memory and disk for the instance seconds per document under
saturation. Instance seconds per document are the server p50 divided by the
number of sessions. The "measured wall" column uses the run's actual wall time,
including client overhead. Requests and Worker CPU are excluded; they add well
under $0.01 per 1,000.

| Configuration | 2,048 tokens, saturated | 2,048 tokens, measured wall | about 300 tokens, saturated | about 300 tokens, measured wall |
| --- | ---: | ---: | ---: | ---: |
| standard-4, 4 sessions × 1 thread | $0.23 | $0.25 | $0.027 | $0.029 |
| standard-4, 1 session × 4 threads | $0.34 | $0.35 | $0.050 | $0.061 |
| standard-3, 1 session × 2 threads | $0.28 | $0.28 | $0.036 | $0.038 |
| standard-2, 1 session × 1 thread | $0.29 | $0.29 | $0.034 | $0.036 |

The earlier projection from the local host was $0.074 per 1,000 at 2,048 tokens
and $0.008 at about 300 tokens. That projection was for the int8 candidate,
which did not qualify. Served f32 on Cloudflare costs about 3× more at 2,048
tokens and 3.4× more at about 300 tokens.

**Idle and keep-warm.** An awake instance bills provisioned memory and disk,
plus its idle CPU. Measured idle CPU was 0.004-0.006 vCPU, which adds under
$0.001 per hour.

| Size | Awake but idle | Per hour | Kept warm for a 730-hour month |
| --- | ---: | ---: | ---: |
| standard-2 | $0.0000158/s | $0.057 | $41.5 |
| standard-3 | $0.0000211/s | $0.076 | $55.5 |
| standard-4 | $0.0000314/s | $0.113 | $82.5 |

A cold start bills 5-15 s of awake time, $0.0002-0.0017 at busy rates. The
inactivity timeout therefore decides most of the low-traffic cost:

- With a 10-minute timeout, each isolated request on standard-4 also bills up
  to $0.019 of idle.
- Scaling to zero after each request saves that idle, but adds 10-12 s of
  latency to the next request.

## Remaining launch qualifications

- **Distributional parity.** Run f32 parity on evaluation-like text inside the
  provider boundary, covering calibrated confidence between map levels, and
  pin the Container CPU feature level in the execution manifest. Outputs from
  AVX-512 (Cloudflare) and AVX2 (local) hosts differ at the 1e-6 level.
- **Latency budget.** The planning figure must move from 1.30 s to the measured
  3.2-3.8 s p95 per 2,048-token document on standard-4, or a faster numeric
  profile must qualify. That means a new int8 pre-registration, or a smaller
  distilled *A*.
- **Cold start.** The model-ready time (median 9.4 s) needs a product decision
  between keep-warm cost and first-request latency. Reducing load time
  (snapshot, smaller graph, preloaded page cache) is unqualified.
- **Start failures.** The coordinator's Container client must handle start
  errors and dead ports with bounded retries. Pending work must be persisted
  independently of the container.
- **ONNX Runtime as a native dependency.** It ran correctly from the pinned
  wheel library on Cloudflare's CPUs. Its pin, licence notice and update
  policy in the release manifest are not yet recorded.
- **Concurrency and sizing.** The Worker must route interactive requests to a
  latency configuration and batch work to multi-session instances. It also
  needs measured `max_instances` and queueing under real arrival rates. Only
  1-4 concurrent clients were tested.
- **Placement.** Forwarding overhead varied from 0.1 to 0.35 s between
  instances. Region placement and its effect on latency were not studied.

## Limits

- **Coverage.** One account, one day and one CPU model, with a handful of
  instances. Shared-host noise shows in the p95 values.
- **Clients.** All clients ran on one host, over its own network path.
- **Inputs.** Synthetic text only; no evaluation-like or customer-like text.
- **Costs.** Computed from measured busy CPU and published list prices, not
  read from an invoice.
- **Measurements not made.** No measurement after long idle beyond new
  instance names, and no run in another region.

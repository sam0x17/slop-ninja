# A inference on CPU, Workers and GPU hosts

Measured on build-host on October 28, 2026. This note covers three questions about
the current *A* shape: how fast CPU inference can get, whether it can run CPU-only
inside a Cloudflare Worker, and what a GPU host would cost. *A* here is a
fine-tuned ModernBERT-base encoder (149.6M parameters: 38.7M embedding, 110.3M
encoder, 0.6M heads), with a 2,048-token limit and the pinned ModernBERT tokenizer.
Its small numeric heads (origin, involvement, share, attribution) and calibration
map add negligible compute.

Nothing was deployed. No Cloudflare resources were created, no GPU host was
contacted, and no GPU time was used. Prices come from public pages read on
2026-10-28, with each page's own date where it shows one. Benchmarks used token
IDs only. Quality was measured by read-only scoring of Dev-iter. Weights,
predictions, scripts and per-run receipts stay in ignored storage.

## Summary

| Question | Answer |
| --- | --- |
| Fastest exact CPU path | 5.3 s per 2,048-token document on one core, f32 Rust with banded local attention. It passes the existing replay gate and is 2.5× faster than `inference/a-v6-cpu`. |
| Mixed int8 CPU build | 2.65 s on one core and 0.96 s on four cores per 2,048-token document (0.44 s and 0.16 s at 512 tokens), through ONNX Runtime. On the screen model its Dev-iter Brier moved −0.0003 and 8 decisions flipped. On the baseline release model it failed its pre-registered no-quality-loss qualification, and so did the one pre-declared fallback (see [Serving qualification](#serving-qualification-of-the-release-build)). |
| f32 CPU serving path | The banded f32 ONNX graph on the same runner takes 1.30 s (p95) per 2,048-token document on four local vCPUs. On a Cloudflare standard-4 Container it takes 3.8-4.8 s (p95), and it passes the release replay gate there (see [f32 serving on Cloudflare Containers](a-container-serving.md)). Serving stays f32 until an int8 recipe qualifies. |
| Plain int8 | Not usable. +0.069 Brier and 475 flipped decisions. The error comes entirely from the MLP output projection. |
| CPU-only Worker, ModernBERT-base | Not as a resident model: even int4 needs 131.5 MB of Wasm memory at 2,048 tokens. Layer streaming from R2 or the Cache API fits (about 84-95 MB peak), but it is projected at 21-27 s of CPU per 2,048-token document, uses a quantized numeric profile that fails the replay gate, and is unmeasured on workerd. |
| CPU-only Worker, small distilled A | Fits without streaming. A 32M distilled student measured 0.1422 Dev-iter Brier against 0.1484 for the base fit, with caveats below. It is projected at 3.4-4.2 s of Worker CPU per 2,048-token document. |
| Cloudflare Container (CPU) | Fits. Measured f32 on Cloudflare: $0.23-0.34 per 1,000 documents of 2,048 tokens and $0.027-0.050 per 1,000 of about 300 tokens, busy time only. Cold start to model ready is a median 9.4 s ([f32 serving on Cloudflare Containers](a-container-serving.md)). The int8 figures below are a local projection for a build that did not qualify. |
| RunPod GPU | About $0.009 per 1,000 documents of 2,048 tokens on a saturated, batched RTX 4090. Sporadic single requests cost about $1.6 per 1,000, because the 5 s idle timeout is billed. The throughput is a planning figure, not a measurement on *A*. |

## Setup and measurement rules

- **CPU:** Intel Core i9-13900KF (Raptor Lake, 8 P-cores with SMT and 16 E-cores;
  AVX2, FMA, F16C and AVX-VNNI; no AVX-512).
- **Shapes:** the Worker shape is one thread pinned to one P-core thread. The
  Container shape is four threads on four distinct P-cores. Every job ran as a
  user systemd unit with a CPU set, a memory cap and zero swap.
- **Contention:** the host was shared with other campaign jobs throughout. The
  1-minute load average is recorded with every number. SMT siblings were not
  reserved; Worker rows were kept only when the pinned core's sibling was idle.
- **Repeats:** 7 warm runs for one thread and 9 for four threads (5 for Rust and
  3 for the reference graphs), after a warm-up. Tables give median / p90.
- **Inputs:** exact-length token sequences of 256-2,048 tokens taken from public
  Train human rows, batch 1, no padding. The banded graph pads to a multiple of 64
  with a masked pad.
- **Models:** speed used frozen V6 with shared heads; speed does not depend on the
  weight values. Quality used the newest full-*A* fit available on 2026-10-28, a
  fine-tuned-encoder screen fit, because the baseline release had not yet produced
  a final export. The Worker proof of concept used deterministic pseudo-random
  weights with the real tensor shapes, so it measures only time and memory.
- **Runtimes:** ONNX Runtime 1.31.1 from Python and from the Rust `ort` crate
  2.0.0-rc.10 loading the same library, and Candle 0.11.0 (native x86-64-v3 and a
  `wasm32` SIMD128 build under V8 14.6 in Node 26).

## Native CPU latency

### Runtime comparison at 2,048 tokens

| Graph | 1 thread | 4 threads | Peak RSS | Replay gate | Load |
| --- | ---: | ---: | ---: | --- | --- |
| Qualified FP64 replay graph | 26.53 / 26.77 s | 21.36 / 22.22 s | 3.3 GiB | pass | 8-11 |
| `inference/a-v6-cpu` F32 graph | 13.21 / 13.25 s | 11.25 / 11.39 s | 1.6 GiB | pass (f32 diagnostic) | 8-11 |
| F32 graph + MKL sgemm | 10.86 / 11.16 s | 6.68 / 7.79 s | 1.3 GiB | not run | 8-13 |
| **Fast f32, banded local attention** | **5.27 / 5.48 s** | **3.40 / 3.60 s** | 1.16 GiB | **pass** | 8-13 |
| Fast f32 without banding | 8.83 / 10.18 s | 5.11 / 6.54 s | 1.16 GiB | not run | 8-13 |
| Fast f32, Candle Q8_0 weights | 10.15 / 10.43 s | 5.12 / 8.19 s | 0.84 GiB | fail | 8-13 |
| Fast f32 compiled to Wasm SIMD128 (V8) | 19.63 / 21.56 s | – | 1,175 MiB Wasm | not run | 3-5 |
| **ONNX Runtime mixed int8 (candidate)** | **2.65 / 2.69 s** | **0.96 / 0.97 s** | 1,044 MiB (Rust `ort`) | not applicable | 3-5 |

The fast f32 graph drops attention masks for unpadded documents, fuses softmax and
LayerNorm, computes each 64-query local block against its 192-key window instead
of a full masked score matrix, and blocks global attention by query. Moving from
f64 to f32 and dropping masks gives 2×, and banding gives another 1.7×. The repo's
earlier 13 s (`inference/a-v6-cpu`) and 19 s (MKL qualification profile in
[encoder adaptation](a-encoder-adaptation.md)) figures bracket the F32 row. The
fast graph and its replay results are in ignored storage. It is not yet part of
the tracked runtime.

Profile of the fast f32 graph on one thread: weight matrix products 62% (Wqkv 22%,
MLP Wi 22%, MLP Wo 11%, attention Wo 7%), global attention 24%, local attention
6%, GeGLU 5%, RoPE 2%, LayerNorm 1%. MKL adds only 2% to this path. Candle's GGML
quantized kernels are tuned for decode-style vectors and run prefill 2× slower than
f32. Candle Q4_K cannot be used because the 1,152-wide intermediate is not
divisible by 256. The remaining speedup therefore comes from int8 GEMM with VNNI,
which ONNX Runtime's MLAS provides. The Rust runtime should call ONNX Runtime
through `ort` rather than grow its own int8 kernels.

### ONNX Runtime by length (quiet pass, load 3-5)

Seconds per document, median / p90.

| Variant | Threads | 256 | 512 | 1,024 | 2,048 | tokens/s at 2,048 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| fp32 | 1 | 0.501 / 0.502 | 1.120 / 1.130 | 2.719 / 2.727 | 5.741 / 6.758 | 356 |
| fp32, banded | 1 | 0.472 / 0.472 | 0.852 / 0.898 | 2.162 / 2.166 | 5.005 / 5.010 | 409 |
| int8 dynamic, banded | 1 | 0.197 / 0.198 | 0.447 / 0.447 | 1.083 / 1.086 | 2.310 / 2.700 | 886 |
| **Mixed int8 candidate** | 1 | **0.201 / 0.201** | **0.444 / 0.445** | **1.052 / 1.054** | **2.648 / 2.685** | 773 |
| Same, Rust `ort` | 1 | 0.201 / 0.203 | 0.444 / 0.504 | 1.047 / 1.053 | 2.650 / 2.710 | 773 |
| fp32 | 4 | 0.138 / 0.264 | 0.261 / 0.283 | 0.735 / 0.739 | 1.693 / 1.981 | 1,209 |
| int8 dynamic, banded | 4 | 0.060 / 0.060 | 0.129 / 0.131 | 0.313 / 0.314 | 0.864 / 0.866 | 2,369 |
| **Mixed int8 candidate** | 4 | **0.073 / 0.074** | **0.156 / 0.156** | **0.371 / 0.371** | **0.962 / 0.967** | 2,128 |
| Same, Rust `ort` | 4 | 0.073 / 0.073 | 0.156 / 0.156 | 0.371 / 0.374 | 0.966 / 0.970 | 2,120 |

- **Throughput:** four concurrent single-thread sessions on four P-cores scored all
  2,873 Dev-iter documents (885,756 tokens) in 214 s: 4,139 tokens/s, or 13.4
  documents/s. For batch work, four single-thread documents in parallel beat one
  four-thread document. One to four threads speeds a single 2,048-token document
  up by 2.7-3.4×.
- **int4 and fp16 are slower than int8 on this CPU** (6.8-7.4 s and 7.1 s at 2,048
  tokens, one thread, load 9-13). MatMulNBits dequantizes and there is no native
  fp16 GEMM, so they only save memory.
- **Model load** takes 0.7-4 s.

The candidate build is ONNX opset 17 with banded local attention and an exact
SmoothQuant-style rescaling (α = 0.8) folded into the weights. It uses dynamic
per-channel int8 for Wqkv, attention Wo and MLP Wi, and fp32 for MLP Wo and the
embeddings. It occupies 314 MiB. The rescaling divides each input channel by
s = max|X|^α / max|W|^(1−α), clamped to [0.01, 100], with activation maxima from
64 public Train rows. It is folded exactly into the attention and MLP norm
weights, the V rows of Wqkv and the gate rows of MLP Wi; layer 0's Wqkv reads the
residual stream and is not rescaled. Before quantization the rescaled fp32 graph
changes probabilities by at most 8e-7. The fp32 ONNX export matches PyTorch CUDA
Dev-iter predictions to 3.9e-6.

## Quantization quality

Uncalibrated P(involvement) on Dev-iter (2,873 documents, 570 human), compared with
the fp32 export. Brier uses the trainer's family- and origin-balanced weights. A
human flag is a human document with P ≥ 0.5; a flip is a 0.5 decision that differs
from fp32.

| Variant | Size MiB | Brier | ΔBrier | Human flags | Flips | max abs ΔP |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| fp32 (ONNX, banded ONNX and fast Rust agree) | 572 | 0.1484 | – | 45 | 0 | ≤ 4e-6 |
| fp16 | 287 | 0.1485 | +0.0000 | 45 | 0 | 0.011 |
| int8 dynamic, all products | 256 | 0.2174 | +0.0690 | 7 | 475 | 0.552 |
| int8 static, quantile ranges | 256 | 0.1809 | +0.0325 | 25 | 289 | 0.540 |
| int8 static, min-max ranges | 256 | 0.2748 | +0.1264 | 1 | 882 | 0.721 |
| int4 (block 32) | 215 | 0.1682 | +0.0198 | 33 | 145 | 0.38 |
| int4 including embeddings | 91 | 0.1550 | +0.0066 | 41 | 102 | 0.322 |
| int8 MLP Wo only | – | 0.2177 | +0.0693 | 8 | 471 | 0.578 |
| int8 all except MLP Wo | – | 0.1487 | +0.0002 | 46 | 29 | 0.086 |
| rescaled α=0.8, all int8 | – | 0.1423 | −0.0061 | 55 | 47 | 0.134 |
| **Candidate: banded, rescaled α=0.8, all except MLP Wo** | 314 | **0.1481** | **−0.0003** | **45** | **8** | **0.067** |
| Candle Q8_0 (blockwise activations) | – | 0.1473 | −0.0011 | 47 | 11 | 0.052 |
| Candle Q4_0 | – | 0.1714 | +0.0230 | 30 | 158 | 0.385 |

- **The int8 failure is MLP Wo alone.** Its input is the GeGLU product, whose
  activation outliers do not survive per-tensor activation quantization. Rescaling
  alone lowers Brier but adds human flags, so it is a perturbation, not a gain.
  Rescaling plus fp32 MLP Wo stays at noise level. Brier reductions in some rows
  are quantization noise; fp32 remains the reference.
- **Blockwise activation quantization** (Candle Q8_0) is also accurate, but no fast
  CPU kernel here uses it.
- **Frozen V6** shows the same pattern: plain int8 dynamic adds +0.0716 and int4
  adds +0.0255.

### Replay gate

The existing synthetic full-*A* replay gate (raw encoding 1e-4, shared encoding
1e-3, outputs on the 0-1 scale 1e-5, involvement logit 1e-4) was rerun:

| Run | Pass | max raw encoding | max outputs | max involvement logit |
| --- | --- | ---: | ---: | ---: |
| Qualified FP64 graph | yes | 7.1e-6 | 9.9e-7 | 2.8e-6 |
| F32 diagnostic | yes | 2.5e-5 | 4.5e-7 | 2.5e-6 |
| Fast f32, banded | yes | 7.8e-5 | 5.7e-6 | 2.3e-5 |
| Fast Q8_0 | no | 0.17 | 0.011 | 6.9e-3 |

The replay gate is an exactness gate, and quantized builds are expected to fail it.
A quantized *A* is therefore a separate frozen artifact. It needs a pre-registered
distributional gate (for example, flips and max |ΔP| against fp32 on a fixed Dev
slice, plus relaxed replay tolerances), its own calibration map, and its own replay
reference. The ONNX build is not wired into the `full_a` replay tooling; its parity
evidence is export validation plus Dev-iter agreement.

### Serving qualification of the release build

The mixed int8 recipe above was qualified against the baseline release model
(Faux, seed 17; see [the baseline release](a-baseline-release.md)) on October 28,
2026. It ran under a pre-registered no-measurable-reduction gate set, fixed before
any ONNX or int8 output of a release finalist existed. The gates are:

- **Construction:** exact weights, f32 export parity, rescaling parity, tokenizer
  parity, calibration code and bootstrap port.
- **Parity:** int8 against f32 on Dev-iter and on a fixed set of 520 long
  documents of 1,024-2,048 tokens drawn from Train-side text outside the bundle.
- **Quality:** Dev-iter quality on raw outputs and on CAL-calibrated outputs.
  Each build uses its own calibrator, so the int8 build was refitted on CAL under
  protocol Amendment 3.
- **Latency:** at 2,048 tokens.

**Result: not viable (quality reduction).** Serving stays f32 until a recipe
passes a new pre-registration.

| Gate (Dev-iter unless noted) | Limit | Primary recipe | Fallback: same, plus the 4 most sensitive layers in f32 |
| --- | --- | --- | --- |
| max / p99 \|ΔP\|, raw | 0.10 / 0.045 | **0.114 / 0.065** | 0.073 / 0.043 |
| decision flips at 0.5, raw | ≤ 14 | **16** | 10 |
| attribution top-1 agreement, raw | ≥ 0.98 | **0.962** | **0.971** (long documents **0.942**) |
| calibrated share, max \|Δ\| | ≤ 0.10 | **0.152** | **0.155** |
| balanced Brier Δ, raw, primary population | point ≤ +0.0005, upper ≤ +0.001 | **+0.0008 [+0.0001, +0.0016]** | −0.0002 [−0.0006, +0.0003] |
| balanced Brier Δ, calibrated, primary population | same | **+0.0008 [−0.0006, +0.0022]** | **+0.0005 [−0.0005, +0.0015]** |
| TPR at 0.5, raw / at the CAL 1% point, calibrated | ≥ −0.005 | **−0.006** / **−0.009** | 0.000 / −0.004 |
| p95 latency at 2,048 tokens, 4 vCPU | ≤ 4.0 s | 0.86 s | 0.95 s |

Bold marks a failure. All construction gates passed for both builds.

How the two builds differ:
- **The primary recipe** fails because its raw outputs degrade measurably on
  this model. Its Brier interval excludes zero, and its errors are larger on long
  documents (max |ΔP| 0.162).
- **The fallback** has no measurable raw Brier change, but it still fails three
  gates:
  - it disagrees with f32 on the top attribution for 2.9% of documents
    (5.8% of long ones), although attribution accuracy is unchanged (+0.004);
  - single calibrated share values move by up to 0.155;
  - its calibrated Brier upper bound is +0.0015.
- **Layers kept in f32.** The fallback keeps layers 0, 1, 9 and 12 in f32, ranked
  by sensitivity on the 206 rescaling rows. It has 54 of 66 weight products in
  int8 and a 361 MiB graph.

The release candidate's CAL operating points were corrected during the
qualification: thresholds now read the emitted calibrated involvement instead of
re-adding its split. The whole qualification was rerun on the corrected artifact.
Both runs gave the same verdict, and every build and score was bitwise identical.
The full gate list, Container cost per 1,000 documents, the calibration
amendment and the independent review are in
[mixed-int8 CPU serving of A](a-int8-serving.md).

Speed of the fallback build on this host, through the Rust `ort` runner:

| Container shape | p50 / p95 at 2,048 tokens | Saturated, Dev-iter documents |
| --- | --- | --- |
| 4 vCPU (two P-cores with SMT) | 0.91 / 0.95 s | 10.9 documents/s (4 × 1-thread sessions) |
| 2 vCPU | 1.60 / 1.62 s | 5.8 documents/s |

- **f32 on the same runner.** The f32 banded graph measured 1.27 / 1.30 s on the
  4 vCPU shape.
- **Memory.** Saturated long documents need up to 7 GB of cgroup memory with four
  single-thread sessions, and 2 GB with one 4-thread session.
- **Cold start.** A median 0.8 s to ready (maximum 2.0 s), and about 1 s to the
  first 512-token result.

## Cloudflare Workers

Workers platform limits (Workers limits page, updated 2026-10-12): 128 MB per
isolate including the JavaScript heap and Wasm memory, one thread, Wasm SIMD128,
CPU up to 5 minutes per request through `limits.cpu_ms` (default 30 s), 64 MiB
uncompressed bundle, and global scope that must finish within 1 s. Workers CPU
costs $0.02 per million CPU-ms (pricing page, updated 2026-10-02); duration and
memory are not billed.

### Memory

A fully resident ModernBERT-base does not fit at any precision tested:

| Form, 2,048 tokens | Weights | Wasm linear memory | Fits 128 MB |
| --- | ---: | ---: | --- |
| f32 | 598 MB | 646 MB | no |
| Q8 layers and embedding | 159 MB | 217 MB | no |
| Q4 layers and embedding | 84 MB | 131.5 MB | no (111 MB at 512 tokens, before heap and code) |

The ONNX mixed int8 candidate (314 MiB) and int4 ONNX (91 MiB plus activations) also
exceed it, and every quality-safe form exceeds the bundle limit.

It fits with layer streaming: only the embedding, one or two layers and bounded
activations stay resident, and the other layers are read from R2 or the Cache API
for every document. Attention is chunked per head (256 global or 128 local queries
per chunk) and the MLP runs in 256-row chunks. Measured peaks at 2,048 tokens,
including about 8 MB of V8 heap:

| Layers / embedding (streamed) | Peak | Verdict |
| --- | ---: | --- |
| Q4 / Q4 | about 84 MB | fits |
| Q8 / Q4 | about 95 MB | fits |
| F16 / Q4 | about 105-107 MB | marginal |
| Q8 / Q8 | about 113 MB | too tight |

Activations are about 20 MB at 512 tokens, 23 MB at 1,024 and 38-39 MB at 2,048.
The tokenizer, built for wasm32 separately, peaks at 23.8 MB while parsing its
3.58 MB JSON and keeps 0.92 MB live afterwards; load it before the weights or ship
a pre-parsed vocabulary. Whether workerd counts compiled code and fetched buffers
against the limit is undocumented, so plan for at most 100-105 MB of measured
linear memory plus heap. Stream response bodies directly into linear memory;
buffering a layer as an ArrayBuffer roughly doubles the transient.

### Latency and cost

One Wasm SIMD128 thread, idle P-core at 5.5 GHz, lowmem streamed build:

| Encoder | 512 | 1,024 | 2,048 |
| --- | ---: | ---: | ---: |
| ModernBERT-base (Q8, Q4, F16 or F32 storage) | 2.69-2.74 s | 5.80-5.94 s | 13.25-13.66 s |
| Same, Candle integer-dot kernels | 8.1-8.9 s | – | 28.5-30.9 s |
| Same code, native AVX2 | 1.03-1.13 s | – | 5.36-5.89 s |

- **Storage precision does not change speed.** Every format dequantizes into the
  same f32 GEMM, which already runs at about 52 GFLOP/s, close to the 128-bit SIMD
  limit. A fused attention kernel might save 2-3 s; the optimized floor is about
  10-11 s locally.
- **Projection to Cloudflare:** Cloudflare does not publish which CPUs serve
  Workers. Its Gen 12 and Gen 13 servers (EPYC 9684X and 9965) have all-core boosts
  of 3.42 and 3.35 GHz. Multiplying local times by 1.6-2.0 gives 21-27 s of CPU per
  2,048-token document, and up to about 54 s if a co-tenant occupies the SMT
  sibling (a busy sibling doubled local times).
- **Layout hazard:** one configuration (F16 layers, Q4 embedding, in-place stream,
  2,048 tokens) reproducibly ran 1.9-2.3× slower at a steady clock, consistent with
  cache-set aliasing. A production build needs a fixed buffer layout and a speed
  regression gate.

| Host for the streamed base forward | 2,048 tokens | 512 tokens |
| --- | ---: | ---: |
| Worker CPU billing, factor 2.0 (range 1.6-4.0) | $0.54 ($0.43-1.07) per 1,000 | $0.11 per 1,000 |
| Durable Object duration billing (128 MB at $12.50 per million GB-s, about 28 s wall) | about $0.04 per 1,000 | about $0.01 per 1,000 |

Requests and 22 R2 reads per document add under $0.01 per 1,000. The Durable Object
figure uses the Durable Objects pricing page (updated 2026-10-18). Durable Objects
have the same 128 MB and CPU caps, and objects of one class can share an isolate's
memory and its single thread. End-to-end latency for one 2,048-token document is
about 20-55 s including unmeasured layer I/O, and about 5-6 s at 512 tokens.

Plain Wasm f32 SIMD128 arithmetic is IEEE-deterministic, so one Worker artifact
would give identical outputs on every host. Native ONNX Runtime kernels differ by
CPU feature level, so a Container build must be qualified on the Container's CPUs.

## Small encoders

Students were trained on CPU (eight threads, one epoch) on a 24,000-document
stratified Train subset, with *A*'s output heads. "Fine-tuned" students start from the
public Ettin encoder. "Distilled" students add KL divergence to the base fit's
three-way origin distribution on the same Train rows. The base fit was trained on
GPU on a 12,000-document subset. Each student was selected by Dev-iter Brier from
six evaluations with one seed, so its absolute scores are optimistic.

| Model | Parameters | Brier | Human flags | TPR at 0.5 | AUROC | TPR at 5% FPR | Attribution accuracy |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Frozen V6 start | 150M | 0.1787 | 70 | 0.751 | 0.895 | 0.634 | 0.070 |
| Base fit (fine-tuned ModernBERT-base) | 150M | 0.1484 | 45 | 0.735 | 0.906 | 0.684 | 0.191 |
| Ettin 17M, fine-tuned | 17.2M | 0.2172 | 58 | 0.648 | 0.849 | 0.594 | 0.157 |
| Ettin 32M, fine-tuned | 32.4M | 0.2019 | 29 | 0.670 | 0.872 | 0.666 | 0.164 |
| Ettin 17M, distilled | 17.2M | 0.1658 | 18 | 0.672 | 0.895 | 0.715 | 0.158 |
| **Ettin 32M, distilled** | 32.4M | **0.1422** | 32 | 0.736 | 0.904 | 0.727 | 0.175 |

- **Paired row bootstrap** (ignores source-group clustering): distilled 32M minus
  base fit −0.0062 (95% interval −0.0112 to −0.0014); distilled 17M minus base fit
  +0.0173 (+0.0116 to +0.0228). Distillation improved the students by
  0.05-0.06 Brier over plain fine-tuning.
- **The comparison is not like for like:** the students' Train subset was twice
  the size of the base fit's. The result shows that a 32M distilled *A* is competitive, not that it
  beats the base encoder.
- **Weight-only quantization for the Worker path** (distilled 32M): Q8 layers and
  Q8 embedding +0.0013 Brier, 13 flips; a Q4 embedding costs +0.020 and 133 flips.
  Small encoders must keep an 8-bit embedding.
- **ONNX int8** (distilled 32M): plain int8 dynamic +0.057; int8 except MLP Wo
  −0.0022 with 38 flips (no rescaling applied).

Speed and memory at 2,048 tokens (Worker memory includes about 8 MB of V8 heap):

| Encoder | Worker Wasm, one thread | Projected Worker CPU | Worker memory | ONNX CPU, 1 / 4 threads (load 20-22) |
| --- | ---: | ---: | ---: | ---: |
| Ettin 17M | 0.85 s | 1.4-1.7 s | about 47 MB (Q8, resident) | 0.64 / 0.20 s (int8 except MLP Wo) |
| Ettin 32M | 2.1 s | 3.4-4.2 s | about 69 MB (Q8, resident) | 1.44 / 0.45 s (int8 except MLP Wo) |
| 68M shape (untrained) | 6.0 s | 9.6-12 s | about 80 MB (Q4, resident) | – |

At factor 2.0 the Worker CPU cost is about $0.08 per 1,000 documents of 2,048
tokens for 32M and $0.03 for 17M. Both fit the bundle limit, with no streaming.
The ONNX small-encoder timings ran under heavy host load and use an unbanded graph,
so they overstate the cost relative to the base model.

## Cost per 1,000 documents

Busy time only, list prices read 2026-10-28. Excluded: the Workers Paid base fee,
Container idle and memory while idle, cold starts, storage and egress.

| Host and build | 256 tokens | 512 | 1,024 | 2,048 |
| --- | ---: | ---: | ---: | ---: |
| Container standard-4, 4 × 1-thread mixed int8 candidate | $0.0056 | $0.0123 | $0.0292 | $0.0735 |
| Container standard-4, one 4-thread document | $0.0081 | $0.0173 | $0.0412 | $0.107 |
| Container standard-2, 1 thread (assumes 1 vCPU = 1 local P-core thread) | $0.0072 | $0.0159 | $0.0376 | $0.0948 |
| Worker, streamed base (factor 2.0) | – | $0.11 | $0.24 | $0.54 |
| Durable Object, streamed base | – | $0.01 | – | $0.04 |
| Worker, distilled 32M (factor 2.0) | – | – | – | $0.08 |
| RunPod RTX 4090, saturated batches (planning throughput) | $0.0011 | $0.0023 | $0.0045 | $0.0091 |
| RunPod RTX 4090, isolated requests (5 s billed idle) | $1.58 | $1.58 | $1.59 | $1.59 |

Container rates (Containers pricing page, updated 2026-10-05): $0.000020 per active
vCPU-second plus provisioned memory and disk. A busy standard-4 (4 vCPU, 12 GiB)
costs $0.000111/s and a standard-2 (1 vCPU, 6 GiB) $0.0000358/s. Awake but idle,
they cost $0.0000314/s and $0.0000158/s; keeping a standard-4 warm costs about $0.11
per hour. Dev-iter-length documents (about 308 tokens) at the measured 4,139
tokens/s cost about $0.008 per 1,000. Container CPUs are a different
microarchitecture from the test host and need their own measurement, including
whether AVX-VNNI is available for int8.

### GPU hosts

No GPU throughput was measured on *A*. The planning figure is half the published
ModernBERT-base RTX 4090 rate (148k tokens/s at 512 tokens and 124-134k at 8,192;
arXiv 2412.13663v2, Table 2), or 70k tokens/s, which is about 34 documents/s at
2,048 tokens. Other GPUs are scaled by estimated dense FP16 throughput. A
2,048-token document needs about 0.57 TFLOP.

RunPod Serverless flex rates (pricing page, updated 2026-10-08), with billing from
worker start to stop rounded up to the second, including cold start, execution and
the idle timeout (default 5 s):

| RunPod tier | $/s | Saturated, 2,048 tokens | Saturated, 512 tokens | One 1,000-document job (15 s cold start) | Isolated requests, warm resume, 5 s idle | Always-on per month |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 16 GB (A4000 class) | 0.00016 | $0.013 | $0.003 | $0.017 | $1.14 | about $423 |
| 24 GB (L4, A5000, 3090) | 0.00019 | $0.013 | $0.003 | $0.017 | $1.36 | about $504 |
| 24 GB PRO (RTX 4090) | 0.00031 | $0.009 | $0.002 | $0.015 | $2.17 | about $803 |
| 80 GB (A100) | 0.00076 | $0.022 | $0.005 | $0.037 | $5.36 | about $1,986 |

The isolated-request column assumes a 2 s warm resume and 0.1 s of execution; the
cost table above it assumes no resume time. Active-worker discounts are no longer
published. Always-on rows use the flex rate as an upper bound. An always-on 4090
breaks even with the standard-4 Container only near 11 million 2,048-token
documents a month. FlashBoot is free but has no published hit rate.

Peer hosts, saturated, 2,048 tokens:

| Host | GPU | $ per 1,000 | Low-traffic billing unit | Price source |
| --- | --- | ---: | --- | --- |
| Modal | L4, plus 1 core and 4 GiB | $0.016 | idle window 2 s to 20 min, default 60 s | modal.com/pricing, undated |
| Modal | T4 | $0.027 | as above | as above |
| Replicate | T4 / L40S | $0.032 / $0.024 | private models billed for setup and idle while online | replicate.com/pricing, undated |
| Baseten | L4 | $0.016 | per-minute billing | baseten.co/pricing, undated |
| Google Cloud Run | L4, no zonal redundancy | $0.019 | instance billing, up to 10 min of billed idle | GPU docs updated 2026-10-26 |
| Hugging Face Endpoints | L4 (GCP) | $0.013 | scale to zero after at least 15 min | inference-endpoints docs, undated |

A GPU is 8× cheaper than the Container only when it is kept saturated. At sporadic
traffic it is 20× more expensive, because idle and cold-start time dominate. Its
gain at low traffic is latency, not cost.

Using any GPU host outside Cloudflare would move customer text and *A* weights
outside the current provider boundary. That needs a design decision, confirmation
of which services the provider's security attestations cover, a handler that
never logs text (RunPod keeps endpoint logs for 90 days), and an authenticated
request path from the coordinator. [A on a RunPod GPU backend](a-runpod-backend.md)
assesses that boundary, RunPod's platform and data handling, and a Rust
prototype of the Worker-to-RunPod path.

## Recommendation

1. **Now: serve *A* in f32.** The mixed int8 build failed its pre-registered
   qualification on the release model ([above](#serving-qualification-of-the-release-build)),
   so it is not a serving artifact. Under the operator's preference for Cloudflare
   Containers on CPU, the measured f32 option is the banded f32 ONNX graph through
   the Rust `ort` crate, with documents bucketed to multiples of 64 tokens. Its p95
   is 1.30 s per 2,048-token document on four vCPUs. The fast f32 Rust graph remains
   the exact replay reference. A GPU backend behind a Worker remains the fallback
   ([RunPod](a-runpod-backend.md)). On Cloudflare the f32 graph passes the release
   replay gate, with a p95 of 3.8-4.8 s per 2,048-token document on standard-4.
   CPU speed, cold start and idle cost were measured; see
   [f32 serving on Cloudflare Containers](a-container-serving.md) for those figures
   and the remaining launch qualifications. Any later int8 recipe needs a new
   pre-registration.
2. **Next: a Worker-sized distilled *A*.** A distilled 32M student fits a Worker
   resident with Q8 weights, at a projected 3.4-4.2 s of CPU per 2,048-token
   document and about $0.08 per 1,000. It is a new model that must pass the full
   protocol, including long-document evaluation, share and attribution. Streaming
   ModernBERT-base through a Worker fits only marginally, costs 21-55 s of CPU per
   2,048-token document and has no qualified numeric profile, so it is not
   recommended.
3. **When available: Workers AI custom models.** Custom-model upload to Workers AI
   is not self-serve as of October 2026 (see [A on Workers AI](a-workers-ai-viability.md)). If it
   becomes available with encoder support, *A* could run on Cloudflare GPUs without
   leaving the provider boundary. That requires its own qualification.
4. **Burst only: RunPod or a peer GPU host.** Only for large batched backfills or
   evaluation runs where the GPU can be kept saturated, and only after the provider
   boundary decision above. A 16 GB or 24 GB tier is sufficient for a 150M encoder;
   list up to three GPU types for fallback, bake weights into the image and batch
   within each request. See [A on a RunPod GPU backend](a-runpod-backend.md).

## Consequences for training

- **Quantization awareness.** The int8 failure is concentrated in MLP Wo's GeGLU
  input. Either keep MLP Wo in higher precision (the current recipe) or add
  quantization-aware fine-tuning or an activation-outlier penalty on that input so
  that full int8 holds. Record the rescaling factors and the quantized artifact with
  the release, and fit calibration on the served build, not on fp32 outputs.
- **Distillation targets.** Distilling the base fit's origin distribution improved
  small students by 0.05-0.06 Brier. Distill from the final release *A* and add
  involvement, share and attribution targets, since attribution accuracy fell from
  0.191 to 0.175. Evaluate students against a base model trained on the same data,
  with more than one seed.
- **Long documents.** Dev-iter documents are at most 663 tokens, so neither
  quantization nor students were evaluated at 1,024-2,048 tokens. A deployable
  build needs a long-document slice in its gate.
- **Worker-oriented shapes.** Keep embeddings at 8 bits or more for small models
  (a Q4 embedding cost +0.020 Brier at 32M), and keep hidden sizes divisible by the
  quantization block sizes.
- **Deployability record.** Each release should record its served numeric profile,
  memory, latency by length and core count, and the gate it passed.

## Changes needed in the Cloudflare design

These are listed for a separate edit of
[the Cloudflare design](cloudflare-subnet-draft.md):

- **Ownership and execution:** the statement that small numeric *A* scorers run in
  the Rust/Wasm runtime with Workers as the initial target does not hold for the
  ModernBERT-base *A*. State that *A* runs in a qualified Container on CPU, and that
  Worker hosting requires a smaller qualified *A*.
- **Rust participants and serverless coordination:** the execution split should
  name *A* inference as a Container workload with measured resource bounds, and
  record ONNX Runtime as a native dependency to accept or replace.
- **Workers resource limits:** add the measured *A* figures (memory by precision,
  CPU per length, bundle size) in place of the general requirement to measure.
- **Models and version registration:** the execution manifest should distinguish
  the exact replay profile from a quantized served profile with its own
  distributional gate, calibration and replay reference. Publish both, and note
  that native kernels differ by CPU feature level.
- **Initial cost scenarios:** replace the unmeasured $0.00001 numeric detection
  allocation with the busy costs of f32 measured on Cloudflare: about $0.00003-0.00005
  for a 300-token document and $0.00023-0.00034 for a 2,048-token document
  ([f32 serving on Cloudflare Containers](a-container-serving.md)). Add
  warm-instance idle cost (about $0.11 per hour for a standard-4) and a cold
  start of about 10 s to model ready.
- **Privacy and the provider boundary:** a GPU fallback outside Cloudflare is not
  covered and would need a new section. Without that section, the design should say
  that no external GPU host is used.
- **Workers AI:** record that custom encoder upload is not self-serve, so *A* does
  not depend on Workers AI at launch.

## Limits

- All figures come from one shared consumer CPU (load 3-31 across runs). Cloudflare
  Container vCPUs, workerd Wasm speed and memory accounting, and R2 or Cache
  throughput into a Worker were not measured.
- No GPU measurement was taken. GPU costs rest on a published throughput figure,
  halved.
- Quality covers involvement only, uncalibrated, on Dev-iter documents of at most
  663 tokens. Share and attribution heads were not compared for quantized builds.
- The Worker proof of concept uses random weights; its quantized formats were not
  replayed apart from Q8_0. F16 storage was not replayed.
- ONNX Runtime Web was not benchmarked.
- Prices are list prices on 2026-10-28 and exclude volume discounts.

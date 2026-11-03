# Text-only warm A: full Rust CPU qualification

On 2026-10-26 the complete exact-text → frozen V6 → warm shared A path passed
the fixed synthetic qualification on build-host, Linux x86_64, Intel i9-13900KF.
This qualifies the selected joint text artifact and measured host/runtime.
Cloudflare Containers, Wasm, other CPUs/platforms, concurrent requests and
deployment remain unqualified. No corpus or Test prose, training, GPU or live
provider calls were used. The immutable prototype/artifact and parent trainer,
head runtime and baseline README were unchanged by this task.

The main-checkout package was subsequently rebuilt against the accepted
adaptation-control-capable head loader and requalified on the same fixed oracle.
All seven focused tests and all 41 cases passed again, with identical maxima in
the numerical table below. The inference binary SHA-256 is
`d57773d59a21a4afa59c2eaa26b8f4319dd13cb2c03ca069156960eba6a209ab`;
the comparison binary is
`5b4a6388c0d4b691e617e75e36362541188909b21019bf7cb8be33606cab61de`.
Exact source, artifact, oracle and binary bindings remain under ignored
`train-scale/runtime-main-v1/` in the A task root. No oracle was regenerated.
This run took 53.41 seconds for the full roster with process peak RSS
1,749,300 KiB; 2048-token encoder forwards took 13.06–13.40 seconds. The cgroup
peak of 3,393,474,560 bytes also includes compilation and is not an inference-only
measurement. Original detailed measurements and receipts below remain retained.

On 2026-10-30 the package was requalified after the copied ModernBERT,
LayerNorm and trace sources gained the optional MKL and FP64 numeric profiles
and f64 trace output (the FP32 graph, weights and gates are unchanged). The
same fixed oracle and roster were used and no oracle was regenerated. All 41
cases passed (36 supported, five unsupported receipts) with the same maxima as
the table below, and every input hash re-verified after the run. The focused
tests passed, including the source-pin test. `UPSTREAM.json` and the binary's
source check now pin the requalified bytes. The inference binary SHA-256 is
`f9d26b6dbae22e3ab9a471b270774e784ac16e394cec7398c539ae56afa3d6aa`, the
comparison binary is
`1f51967c80b60f128dac0e1853c5d29e34be9d8e1625289ab8910780625804dd`, the
comparison receipt is
`9d40b3c93404573f6126976fc6bae4c119b2ef5a9f61febd0680af25afc1f03d` and the
runtime receipt is
`aa0c51eb6c9396951e759443bba73a2837b3bb02d9832ea35b8797ae1cc9e1e3`. The
run took 59.20 seconds with one CPU, 6 GiB and zero swap; process peak RSS
was 1,749,824 KiB. Detailed bindings are under ignored
`train-scale/runtime-squash-v1/` in the A task root.

On 2026-10-31 the package was requalified after the copied ModernBERT source
changed how it builds the rotary tables. Each angle is now one float32 product
of position and inverse frequency, as in HF's `ModernBertRotaryEmbedding`, and
the FP64 profile (now `candle_f64_full_a_v2`) widens the float32 cos/sin
tables exactly instead of recomputing the angles in float64. The FP32 graph
builds the same tables as before. The fixed oracle, roster and release cases
were reused, nothing was regenerated, and every input hash re-verified after
the run. Each check compares the build of this source with the build of the
previous source:

- **Warm-A roster (FP32 graph).** All 41 cases passed (36 supported, five
  unsupported receipts) with the same maxima as the table below. Every output
  of the 41 cases is bit-identical before and after; only run bindings and
  timings differ.
- **Full-A release package (R0, export v2), three reference cases.** The FP32
  diagnostic graph is bit-identical before and after. The FP64 profile moves
  closer to the recorded CUDA reference. Before the change, the release
  runtime binary and the previous build give bit-identical outputs.

  | Largest error against the reference | Before | After | Gate |
  | --- | ---: | ---: | ---: |
  | Raw h | 1.352e-5 | 9.537e-6 | 1e-4 |
  | Shared z | 1.328e-5 | 9.587e-6 | 1e-3 |
  | Outputs, 0..1 scale | 7.313e-7 | 7.159e-7 | 1e-5 |
  | Involvement logit | 3.327e-6 | 2.383e-6 | 1e-4 |

The focused tests passed, including the source-pin test. `UPSTREAM.json` and
the binary's source check now pin the new `modernbert.rs` bytes. The
inference binary SHA-256 is
`9f1bb77b957ae971c5532b8385fbb1cc4c01da9ef439adb7d297a30fef570b89`, the
comparison binary is
`bb8a74a3f9ff359d8a128f1000cc6f7d6e3580a18d9e7fb31a00fa3ce772dea0`, the
comparison receipt is
`3de5c03e56893c000fff26ee65bdec68c3c1092a9fecdbb8d0e56dc377919392` and the
runtime receipt is
`1818ece0b71c1a564159634ab0cb0943d16bb837ed4dfb657a793315ce53699e`. The
roster run took 64.71 seconds on one thread with 6 GiB and zero swap; process
peak RSS was 1,749,608 KiB. Detailed bindings are under ignored
`spans-v1/replay/qualification-c1/` in the competitive A task root.

On 2026-11-01 the package was requalified after the copied ModernBERT source
gained the spans-S2 span tower branch. `ModernBert::forward_with_branch` returns
the unchanged final-norm states and, when asked, the hidden state entering a
chosen layer together with the encoder's attention masks. `SpanTower` runs
copies of the top encoder layers on that state, then a copied final norm. The
copies have their own weights; each keeps its layer's attention kind and
rotary table. `forward` is `forward_with_branch` without a branch, and no
existing operation changed. The heads v3 reader is a separate
`grammar-a-baseline` file, `src/full_a_tower.rs`. The heads runtime files that
the served profile and the artifact-check spec pin (`full_a.rs`,
`full_a_spans.rs`, `involvement_calibration.rs` and `shared.rs`) are
byte-identical to main's. The fixed oracle, roster and release cases were
reused, nothing was regenerated, and every input hash re-verified after each
run. Each check compares the build of this source (`658889e1`) with a build of
main before the follow-up (`72911dad`):

- **Warm-A roster (FP32 graph).** All 41 cases passed (36 supported, five
  unsupported receipts) with the same maxima as the table below. Every
  prediction is bit-identical before and after; only run bindings and timings
  differ.
- **Full-A release package (R0, export v2), three reference cases.** The FP64
  and FP32 diagnostic outputs are bit-identical before and after. The replay
  comparison passes with the maxima of the 2026-10-31 "After" column.
- **Shipped spans package (export v3: R0 plus the S0+ token head), 15
  synthetic and 200 local reference cases.** FP64 document outputs, sentences
  and spans are bit-identical before and after, and its replay comparison
  passes with unchanged maxima.
- **The new branch.** Golden vectors from the Python FP64 reference of the
  trainer's tower module (a tiny 22-layer ModernBERT with generated weights)
  bind the branch. For the one-token case, which uses only position 0 where
  both runtimes' rotary tables are exact, the FP64 profile agrees to 2.4e-15.
  The longer cases agree to 3.8e-8. That bound comes from the float32 cos/sin
  of the rotary tables: PyTorch's vectorized float32 cos/sin and glibc's
  `cosf`/`sinf`, which Rust calls, differ by one ulp in about 5% of table
  entries (measured on 2,048 positions for both rotary bases). This already
  applies to the encoder path of the FP64 profile. It is far inside the 1e-5
  replay gates.
- **S2c export v4 (R1).** The 15 synthetic and 200 local reference cases pass
  the fixed gates, with all 489 spans at equal offsets. The largest
  token-probability error is 4.4e-6 and the largest document-output error is
  3.0e-7. Every prediction equals that of the build that ran R1 before
  (branch commit `08a0c84f`), apart from timing.

The focused tests passed, including the source-pin test. `UPSTREAM.json` and
the binary's source check now pin the new `modernbert.rs` bytes. The inference
binary SHA-256 is
`0242f9e888bdd3950bbdd3c6adb212096b2501349d080a30507b41fe5c265f41`, the
comparison binary is
`a85d0d059ff93899fed0c1f1d93ec7abaefbceb721bfe1338c784059225a87fe`, the
comparison receipt is
`b025ef453767ec023bfbefde7158215d693a3d1d0f63cad9f74834065df234b4` and the
runtime receipt is
`a2e95382bc43e70a192624540afeddf2ce7f537bdbdf8a96ce0f69b1d43f453a`. The
roster run took 113.36 seconds on one thread with 6 GiB and zero swap, and
main's build took 70.29 seconds; each ran beside another replay or test job.
Process peak RSS was 1,747,652 KiB. Detailed bindings are under ignored
`spans-v1/s2/land3/` in the competitive A task root, and main's build under
`spans-v1/s2/land2/`.

Compared with main's build, `compare_full_a` writes two additive keys into
every v3 and v4 comparison report: `export_schema` (the export manifest's
schema) and `span_tower_sha256` (the bound tower file's digest; null for v3).
The report schema stays `slop_ninja_full_a_replay_comparison_v2`, because no
existing key, gate, case or maximum changed. Both shipped-package reports from
this build differ from main's in exactly these two keys and in the run receipt
digest; the R0 report differs only in the run receipt digest. A reader that
compares report bytes across builds must ignore these keys. Every `full_a` run
also binds `tower_runtime_sha256`, the digest of `full_a_tower.rs`, beside the
existing runtime digests. The runtime guard also requires a v4 manifest's
`span_tower.file` to be the weights file named by the heads' own tower
declaration. For a v4 export, `compare_full_a` also requires the tower file
read from the export to have the manifest's digest, not only the receipt's
copy of it, and the run to bind `tower_runtime_sha256`; no v2 or v3 check
changed. Unit tests cover the export member check (a changed digest or file,
unsafe names, a link that leaves the export), the v4 tower file and the run's
source digests.

The crate has its own Cargo workspace/lock and builds in `rust-a-target`.
Seven focused tests passed (six inference/operator tests and one comparison
test), and the locked offline release build passed. Boundary tests cover exact
request fields/bytes, duplicate IDs, raw h plus explicit zero CFG, combined and
diagnostic-control rejection, all five positive supervision requirements,
contract/representation/normalization binding and unchanged copied operators.

The frozen roster contains 29 numerical cases and 12 tokenizer cases. All 41
passed: 36 supported forwards and five unsupported receipts (four empty forms
and the exact 2049-token input). All token IDs/status/counts matched. The 2048
limit includes special tokens; nothing was truncated or silently discarded.
Both original bundled references are included in the numerical roster and the
offline loader also performs their unchanged reference checks.

| Comparison over every supported request | Maximum absolute error | Frozen gate |
| --- | ---: | ---: |
| Every A probability/share, 0..1 scale | 0.000003371928144035863 | 0.00001 |
| Same A error in percentage points | 0.0003371928144035863 | 0.001 |
| Frozen V6 calibrated probability | 0.0000007782300480707605 | 0.000001 |
| Raw classifier-input h, all 768 coordinates | 0.00003288686275482178 | 0.0001 |
| Shared z, all 768 coordinates | 0.00003461153375089454 | 0.001 |
| Frozen V6 logits, diagnostic | 0.000028133392333984375 | No separate gate |

Every origin probability, involvement confidence, estimated share, role
involvement probability and conditional named score was compared. Profile
rosters, selected profiles and fallback semantics matched exactly. All 55,296
h/z coordinates across the 36 forwards were checked. No case or gate was
changed based on its outputs. FP32 encoder and Python-head arithmetic are not
bit-identical to the existing Rust f64 head runtime; the fixed bounds above
describe the measured agreement. Individual outputs and profile names remain
in ignored evidence, not in this report or crate.

At the first qualification the copied ModernBERT, LayerNorm and trace files
were byte-identical to the qualified prototype; their later numeric-profile
additions and rotary-table change were requalified as above. Only the wrapper exposes the already computed `head.norm`
tensor before the original classifier. Raw h enters the existing WarmSharedModel
with 1,820 zero CFG coordinates. The frozen Train normalization applies only
inside the residual branch, while the raw bypass and common shared encoding
remain intact. No graph, kernel, architecture, weights or calibration was changed.

Measurements used one CPU, 6 GiB memory and zero swap. Offline oracle and Rust
inference ran sequentially in the same task-owned unit. These are single-run
observations with cached model files, not percentiles or cold-start estimates.

| Tokens including special tokens | Full V6 encoder forward | Shared A heads |
| --- | ---: | ---: |
| 3 (three controls) | 34.55–41.59 ms | 0.120–0.138 ms |
| 128 | 236.78 ms | 0.130 ms |
| 512 | 1.434 s | 0.142 ms |
| 1024 | 4.232 s | 0.138 ms |
| 2048 (three controls) | 13.121–13.490 s | 0.125–0.151 ms |

The complete Rust process took 53.88 s for 36 forwards plus five rejections,
loading, hashing, diagnostics and durable receipts. Encoder forwards totaled
52.8643 s; shared A heads totaled 0.004570 s. Encoder construction took 0.17410 s;
head construction 0.008106 s; initial artifact verification 0.26282 s.
Per-request tokenization totaled 0.01097 s. Diagnostic z capture repeats only
the small residual adapter and is separately timed, not included in the head
column. These full-encoder costs must not be represented by the earlier
head-only latency numbers.

| Resource | Measurement |
| --- | ---: |
| Rust maximum process RSS | 1,749,604 KiB (about 1.67 GiB) |
| Whole sequential job cgroup peak | 1,795,768,320 bytes |
| Memory cap / swap cap | 6,442,450,944 bytes / 0 |
| CPU cgroup quota | 100000 / 100000 |
| Swap / OOM / memory-limit events | 0 / 0 / 0 |
| Frozen encoder safetensors | 598,442,860 bytes |
| Warm A JSON weights | 6,886,569 bytes |
| Combined encoder/head weight files | 605,329,429 bytes |
| Loaded A numeric weights and normalization, f64 | 1,811,040 bytes |
| Release inference executable | 8,890,528 bytes |

Weight-file sizes exclude tokenizer/legal metadata; process RSS includes the
encoder, tokenizer and heads. The shared cgroup peak also includes the sequential
offline oracle and file-cache pages. Build peak was 969,650,176 bytes, also under
the 6 GiB cap. The executable dynamically links only the Linux loader, libc,
libm and libgcc_s; there is no Python, libtorch or CUDA runtime dependency.

The original unsuccessful oracle attempt stopped before forwarding because the
frozen loader requires environment setup before importing PyTorch. A second
runner attempt stopped on accidentally version-renamed input paths. Both failed
attempts remain ignored. The corrected third run passed without any numerical,
kernel or artifact change. An initial offline Cargo metadata query requested an
uncached macOS-only package; restricting metadata resolution to the measured
Linux target resolved it without a download. Its lockfile retains the encoder's
qualified active dependencies; added dependencies come from grammar-a-baseline.
The macOS-only enum-as-inner resolution differs and is not qualified here.

Evidence bindings:

| Artifact | SHA-256 |
| --- | --- |
| Inference executable | `dd224ddab1f3f6360143bf893b73fd98de0016f57cfa6e62a5ba0dbb910f2ebc` |
| Cargo.lock | `3e685f4d34f0e08dca7ea8d47002c7dedcb3622d8fdff72b08fdc7ca0c99cd8a` |
| Selected warm weights | `f8bcb42532b5575bd90a46f320d1d99a9e0e18c655e94f0260c72bffa346b5a1` |
| Contract file | `33fa8f55666474058eec8c8bf2fed2cb46279e27aa5c244076be7874462bf49f` |
| Compact contract | `07be1109f824409dab0714084c2e67dc0c89379bc964f42f30c1b3d4bb4a6a9e` |
| Full comparison receipt, ignored | `8da17fd3926e9d92cf48ca9d5bb30330a3902fd0037eaf2fbdf1b3b129834503` |
| Runtime receipt, ignored | `813b5f0d299f319bf030bba1cea63cf0d6a6af3763bb31c8bf27850aab7ac092` |
| Offline oracle, ignored | `c272a7ffd2a579d5d3ee4126331c11284ad862af1956f74c809b22787c764c1e` |

Encoder/source/license pins are in `UPSTREAM.json`. Before/after hashing verified
the selected weights, contract, qualified prototype sources, artifact files,
fixture protocol and executables unchanged. The runtime receipt also binds the
compiled `warm_shared.rs` and `shared.rs` dependencies. Rebuilding against a
changed path dependency requires renewed qualification. Retain all detailed
evidence under the ignored task root's `rust-a/` directory; do not publish
weights, named-profile results or per-request outputs.

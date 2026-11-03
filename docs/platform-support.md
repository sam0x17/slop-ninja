# Linux and macOS support

Linux and macOS are development and participation targets for Slop Ninja tools:
Linux x86-64 and arm64, plus macOS on Apple silicon. Source compatibility, a
tested research runtime, and a qualified production configuration are separate
claims. The [whitepaper](../whitepaper/main.tex) and
[Cloudflare design](cloudflare-subnet-draft.md) define native participation
clients and serverless production execution.

| Component | Platform contract |
| --- | --- |
| Rust protocol, corpus, grammar, and vector tools | Linux and macOS targets; platform-neutral formats and configurable paths |
| Production miners and other validators | Native Rust using `rtao`, with independent signed automatic updates and ordered resumable migrations; qualify each supported OS/architecture and the native chain integration |
| Coordinator, including lead-validator duties, public *A* serving, and challenge generation/execution/scoring | Rust application logic on Cloudflare serverless compute; Workers chain signing/recovery, supported model formats, and numerical behavior require qualification |
| V6 reference inference | Research-only pinned Python runner and artifact; test the exact runtime before comparing scores |
| Model training | Optional research ML backends, with separately tested CPU, MPS, or CUDA configurations |
| *B* research orchestration | Rust with an explicitly identified local model endpoint and pinned inference profile |
| Proposed later *B* serving | Cloudflare-hosted adapters in miner-owned accounts; numeric rankers run under Slop Ninja's control |

## Research runtimes

The V6 detector release is a research artifact, not the proposed shared *A*
model. Use its bundled runner, requirements, model bundle, calibration, and
reference vectors together. A different wheel, backend, tokenizer, or runner
is a different runtime variant and needs its own identity and evaluation. A
Linux CPU result does not establish CUDA compatibility or macOS equivalence.
Its Python dependency is not a production miner/validator requirement.

MLX is an optional Mac research backend. Other research backends need their own
pinned checkpoint, tokenizer, runtime, and decoding configuration. Do not carry
scores between backends without an explicit comparison.

## V6 Linux CPU runtime

Use the [CPU variant exporter](../baseline/detector/ml/export_v6_cpu_runtime.py)
to package the frozen V6 release. Install
`baseline/detector/ml/requirements-linux-cpu.lock` in an isolated Python 3.12
environment, run `pip check`, and export with
`--parent /path/to/release/encoder --output /path/to/new/variant`. The exporter
verifies the parent and preserves weights, tokenizer, calibration, reference
vectors, and runner code. It declares `torch==2.14.0+cpu` under a new artifact
identity. Run the exported `runner/infer.py` against the platform requests
described below; preserve package and numerical checks.

The September 13 smoke comparison (omitted from the public copy)
recorded successful reference replay on Linux aarch64 and macOS arm64. Two
nonempty probes differed by at most `1.704829469817426e-7`, below the declared
`1e-6` tolerance; empty-input rejection agreed. This is limited runtime
evidence, not an accuracy result or an error bound for every input. Linux
x86-64 and CUDA were not tested in that comparison. The variant remains the
same trained detector and supplies no additional model diversity.

On October 26, build-host's Linux x86-64 CPU passed the unchanged bundled
reference checks with Python 3.12.15, torch 2.14.0+cpu and transformers 4.57.6
using the exporter's `--cpu-kernel-profile portable-compatible`. This variant's
entry point is `runner/infer_compatible.py`; it sets `MKL_CBWR=COMPATIBLE`,
`ATEN_CPU_CAPABILITY=default`, and one OMP/MKL thread before importing PyTorch.
The original weights, calibration, runner and `1e-6` tolerance remain unchanged.
Default host kernels missed that tolerance by a small amount, so their failed
replay is retained separately and is not the reported comparator.

Artifact `506b1c66a6a2ffc4eb4e50b9b057657ce0e5b6f04cff8caf3df496c956dc0d5f`
scored all 315 prepared documents, spanning 120–777 tokens, without failures.
The [matched comparison](a-reference-comparison.md) records its limits. Replay
peaked at 1,073,120 KiB process RSS; the 102-document Test batch took 151.22
seconds including startup, hash/dependency/reference checks and inference.
Its FP32 weights occupy 598,442,860 bytes. These measurements do not qualify
CUDA, Rust/Wasm, or a Cloudflare Container. The separate Rust qualification below
covers the complete tokenizer/encoder/shared-head path; the earlier small-head
measurements alone did not qualify that encoder.

## V6 and shared A on Rust CPU

The standalone [Rust inference crate](../inference/a-v6-cpu/README.md) runs exact
request text through the frozen V6 encoder and selected joint text residual with
all required A outputs. Linux x86_64 on build-host's Intel i9-13900KF is the tested
profile. Runtime dependencies exclude Python, libtorch, CUDA, a resident server
and network access. The artifact and fitted profile weights remain separate.

All 41 fixed synthetic cases pass: 36 forwards and five explicit unsupported
receipts. The maximum A probability/share error is 3.38e-6; frozen V6 remains
within its unchanged 1e-6 gate. Raw/shared encodings, token IDs, role probabilities,
conditional identities and fallbacks are checked. Empty inputs and more than
2048 tokens (including special tokens) are rejected without truncation. This
qualifies the pinned artifact and fixtures, not arbitrary input or other CPUs.

The main-checkout replay peaks at 1,749,300 KiB process RSS, with 2048-token
encoder forwards taking 13.06–13.40 seconds on one CPU. The unchanged encoder
weights occupy about 598 MB. This artifact therefore needs the allowed
compute-only Container qualification path or a materially smaller future model;
ordinary Workers/Wasm fit is not established. Container execution, cold starts,
concurrency, macOS and Wasm remain unqualified. No deployment was implemented.
See [the bound qualification](../inference/a-v6-cpu/QUALIFICATION.md) for limits,
licenses, numerical checks and reproducible commands.

## Production execution targets

Miners and other validators hold their own chain keys in their native Rust
applications. The operator's lead validator is the coordinator, implemented
entirely on Cloudflare Workers with Durable Objects for authoritative state,
Queues for bounded dispatch, and private artifact/evidence storage. That service
also holds its own validator hotkey and performs its chain signing, submission,
and recovery. Shared Rust/Wasm logic, Workers chain transport/signing, and the
Durable Object journal backend need separate qualification; the native `rtao`
runtime and file journal are not assumed to work unchanged in Workers. Platform-generated
JavaScript bindings are compatible with the Rust application-code requirement.
See the
[native adapter](../subnet/README.md) for its current implementation boundary.

Live `rtao-rpc` finalized-block webhooks are the coordinator's primary
heartbeat on Finney and testnet, one Worker invocation per block; receiver
integration and qualification remain Slop Ninja's work. Authenticate and
persist each delivery, verify finality through RPC, and recover duplicates or
gaps using a durable finalized cursor. A one-minute cron watchdog falls back to
the public OpenTensor RPC through the same per-block path when deliveries stall
or the cursor lags, without a resident subscriber or polling server.

All production inference and challenge execution is serverless, with no local
miner, validator, or development-host serving dependency. Public *A* artifacts
execute in Slop Ninja's account. A qualified serverless Container may
supply bounded computation, while coordinator state and scheduling remain in
Workers and Durable Objects. Later *B* serving uses direct Workers AI calls to
registered private adapters. Its content-privacy qualification must establish
that miners cannot retrieve customer prompts or responses. Cloudflare and
Slop Ninja are trusted operators; no confidential-hardware attestation is
claimed. See the current [privacy policy](../whitepaper/sections/05-confidentiality.tex).

Participant updates and recovery proceed independently of other validators.
Apply each required state migration in order, retaining keys, pending work,
journals, and frozen cohort semantics. Reconcile external effects before signing
resumes; compatibility determines whether rollback is safe. Other validators
affect timely on-chain weight adoption only, never application progress.

## Verification

Run substantial builds, training, and detector evaluation on the designated
`build-host` compute host. Keep task output under one recorded task-owned root,
use explicit resource limits and durable jobs, and preserve unrelated workloads.
Platform qualification requires the relevant artifact and inference checks on
the target configuration; a successful build alone is insufficient.

For the V6 research artifact, run its released `runner/infer.py` on
`baseline/detector/ml/platform-requests.jsonl`. Retain the host, environment,
artifact identity, exact outputs, and installation log. The loader checks its
bundled reference vectors first. Compare probabilities within the artifact's
declared tolerance and preserve unsupported-input behavior.

Record architecture, backend, and any emulation. Production model formats,
Linux/macOS participant releases, and Cloudflare execution profiles remain
subject to their own qualification checks. These targets are not a claim that
all combinations have been tested.

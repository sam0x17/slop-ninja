# SlopNinja A: V6 CPU text inference

Standalone Rust workspace for exact request text → frozen full V6 → shared warm
A outputs. This is local compute qualification, not deployment qualification.
The copied encoder/operator sources are pinned in `UPSTREAM.json`; the encoder
artifact, warm weights, named profiles, requests and per-request outputs remain
outside the crate in ignored storage. No weights are redistributed here.

Build from this directory with its own target, one Cargo job and offline locked
dependencies. Use the task host and a durable job capped at 6 GiB, zero swap and
one CPU. The grammar and root workspace targets are independent.

```sh
CARGO_TARGET_DIR="$TASK_ROOT/rust-a-target" cargo build --offline --locked --release -j 1
RAYON_NUM_THREADS=1 CANDLE_NUM_THREADS=1 OMP_NUM_THREADS=1 MKL_NUM_THREADS=1 \
  "$TASK_ROOT/rust-a-target/release/slop_ninja_a_v6_cpu" \
  "$ENCODER_ARTIFACT" "$WARM_WEIGHTS" "$WARM_CONTRACT" \
  "$IGNORED_REQUESTS_JSONL" "$NEW_IGNORED_OUTPUT"
```

Requests are JSONL objects containing exactly `id` and `text`. IDs must be unique
and nonempty. Exact UTF-8 text is hashed without trimming or rewriting; the pinned
tokenizer performs its qualified NFC/BPE processing. Empty inputs and inputs over
2048 tokens, including special tokens, produce explicit unsupported receipts
without predictions. Full token counts are retained; truncation and padding are
disabled. Input files have a 64 MiB limit and documents are processed sequentially.

The wrapper returns the FP32 tensor immediately before the original V6 classifier:
attention-mask mean pooling → prediction-head dense → erf-GELU → head LayerNorm.
It passes those **raw 768 values plus 1,820 explicit CFG zeros** to the existing
`grammar_a_baseline::warm_shared::WarmSharedModel`. Frozen Train normalization
applies only inside the residual branch; the raw residual bypass is preserved.
All A heads use the same shared representation. The qualified V6 computation,
including its original classifier, is unchanged. No Python, libtorch, CUDA, GPU,
server or network is used by the inference executable.

Only text contracts and joint weights with positive origin, share, fractional
mixed share, human and AI supervision are admitted. Combined contracts and
origin-only/attribution-only diagnostics are rejected. Contract compact hash,
encoder identity/files, classifier-input hook, coordinate layout, frozen Train
normalization and profile roster must match. The existing head loader additionally
validates operators, shapes, temperatures, thresholds and finite weights.

The new output directory contains a durable start receipt, per-request
`predictions.jsonl`, and a final complete `receipt.json` only after the input and
artifact hashes reverify. Incomplete runs must not be treated as completed
inference. Receipts include exact request/contract/weight/source hashes, sizes,
tokenization, encoder and head timings. No in-place resume or output overwrite is
supported. Keep outputs ignored because they contain conditional profile results.

`--diagnostics` also retains token IDs, raw h, shared z and frozen V6 outputs for
offline comparison. Computing z for that receipt repeats only the small residual
adapter; its extra time is separate from the normal shared-head timing. Full-model
memory and latency measurements must be distinguished from previous head-only
measurements. Hashing/loading and per-request forwarding are recorded separately.

`qualification/PROTOCOL.md` fixes the already qualified synthetic roster and gates.
`qualification/oracle.py` is an offline CPU/PyTorch test oracle only; it is not a
runtime dependency. `compare_qualification` checks all final outputs, token IDs,
statuses and both 768-coordinate tensors, retaining failures before returning
nonzero. Per-case numerical outputs remain ignored; the qualification report
contains aggregate results. Other artifacts/platforms, batching, concurrency,
Wasm and Cloudflare Containers require separate qualification.

The wrapper and SlopNinja code use the repository's MIT convention. Copied
Candle source retains its Apache-2.0/MIT licenses; the PyTorch LayerNorm port
retains its upstream license in `licenses/PYTORCH`. Original model and training
data rights remain with the supplied immutable artifact and are not replaced by
the code license. The preserved LayerNorm source comment references its original
sidecar layout; `UPSTREAM.json` gives the local license mapping.

## Final-block derivatives

`last_block` loads the unchanged qualified parent plus an exactly scoped
six-tensor FP32 layer-21 overlay and shared A heads. The export manifest binds
the parent, input contract, normalization, numeric weights and calibration.
Calibrated exports must bind the exact checkpoint, temperature and both named
thresholds to a completed CAL-only receipt. Unknown tensor names, earlier-layer
overrides, changed shapes and nonfinite values are rejected.

```text
cargo build --locked --release --bin last_block --bin compare_last_block
target/release/last_block PARENT_ENCODER EXPORT REQUESTS.jsonl NEW_RUN
target/release/last_block PARENT_ENCODER EXPORT EXPORT/reference-requests.jsonl NEW_REFERENCE_RUN --uncalibrated-diagnostics
target/release/compare_last_block EXPORT NEW_REFERENCE_RUN NEW_REPORT.json
```

Use the same single-thread environment and bounded execution setup described
above. The diagnostic flag permits an explicitly uncalibrated research export
and captures both encodings for comparison; it does not change predictions.
`export_last_block_a.py` in the research trainer directory produces the separate
weights and reference files. CUDA references match the training runtime; CPU
PyTorch interchangeability has not passed the tighter raw-encoding gate for the
new derivative. The early CUDA-reference check passed three synthetic cases;
that does not qualify final weights, all supported inputs, Workers/Wasm or
Cloudflare Containers. See [current evidence](../../docs/a-encoder-adaptation.md).

The optional `mkl` feature is a separate Linux CPU numerical profile for the
current PyTorch 2.9.1 training runtime. It uses Intel MKL 2024.2 and ports that
runtime's DEFAULT LayerNorm merge and softmax reduction order. It changes no
weights or tolerances. All six fitted conditions/seeds pass the three original
synthetic CUDA references under this profile. They also pass the fixed 44-case
boundary roster, including 2,048-token inference and five unsupported-input
checks per run, within the unchanged raw-h/shared-z/output gates. The ordinary
Candle profile remains separate. The MKL profile
currently requires the explicit diagnostic flag and `MKL_CBWR=COMPATIBLE`.

Build with `--features mkl` and `MKLROOT` pointing to an independently installed
2024.2 SDK, then make its Intel OpenMP shared library available at runtime.
The investigated SDK uses `mkl-static==2024.2.2`, `mkl-include==2024.2.2` and
`intel-openmp==2024.2.1`, with wheel hashes and licenses retained in the task
evidence. No Python is required by the resulting executable. The dependency's
automatic 2020 MKL download lacks a Candle-required symbol and is not the
qualified build route. These native libraries are not a Workers/Wasm backend;
Cloudflare Container performance/packaging remains unqualified.

`trace_last_block` and the research `trace_last_block_a.py` inspect only bound
synthetic cases, verify tracing preserves outputs, and compare per-operation
tensors. Trace comparison reports do not infer whether operators changed.

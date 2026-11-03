# Accelerator feedback for B search

`screen` evaluates exact candidate texts on an optional accelerator. Rust freezes
the request bytes and bridge, records the execution, and validates every returned
ID, label, probability and unsupported-input result. Python performs the required
PyTorch model calls. The source model, calibration and 2,048-token limit stay fixed.
No text is truncated and no inference request goes to an external provider.

```sh
slop_ninja_transformer screen \
  --requests /path/to/detector-requests.jsonl \
  --artifact /path/to/released-A-encoder \
  --python /path/to/pinned/python \
  --device mps --out /path/to/new-screen
```

Each request contains only `id` and `text`, with the ID equal to the SHA-256 of
the exact text. A run accepts 1-512 distinct requests, up to 64 MiB of request
data. The output directory must be new. Plans, exact requests, bridge source,
logs and partial output survive a failed or interrupted run. No automatic retry
occurs. A validated run also produces `observations.jsonl` and `summary.json`.

The bridge first verifies the released package and its CPU reference vectors,
then moves the model to the explicitly requested device. Accelerator results
use a separate schema and `weight_artifact_id`, with `canonical_evidence: false`.
They cannot serve as canonical observations for `menu rank`. Use their values
to explore candidates, then confirm shortlisted exact texts through the packaged
CPU runner and retain the ordinary editorial review. Agreement on a small sample
does not qualify a new reference execution policy.

MPS is available for macOS and CUDA is an optional Linux path. Neither backend
is required by the core editing code. There is no automatic CPU fallback; CUDA
TF32 is disabled in this experiment. Only macOS arm64/MPS was exercised for this
change. CUDA still needs an execution check on suitable hardware. The released
package's Python and ML dependency pins still apply. See the
[PyTorch MPS documentation](https://docs.pytorch.org/docs/2.14/notes/mps.html).

A private seven-input device probe agreed with CPU on all threshold decisions.
The maximum difference in combined model/assistance probability was
`6.97e-7`; the maximum individual class difference was `1.09e-6`, slightly above
the reference contract's `1e-6` tolerance. These were related development texts
on one Mac, not a cross-platform equivalence test.

CPU confirmation subsequently completed for all 185 texts in the wider probe.
The maximum combined-probability difference was `1.173e-6` and the maximum
individual-class difference was `1.452e-6`. All threshold decisions agreed,
but none of these texts scored below 10%. The probe therefore does not establish
agreement near the acceptance threshold or qualify a new reference runtime.

Timing depended on the input sequence. The seven-input probe had a median
subsequent-call time of 0.164 seconds. Across a wider set of 185 related complete
texts, the median was 1.479 seconds and the sum of call times was 269.19 seconds,
excluding model startup. Both measurements came from the initial probe bridge;
other project CPU work was running on the Studio. Do not treat the fastest
observations as a general throughput guarantee or a book-length benchmark.

The Rust command passed its MPS execution check with seven supported inputs and
one oversized synthetic input. The latter remained explicitly unscored. The 13
B tests, formatting, Clippy and a release build passed on the Studio. No paid
calls are part of these tests. This adds experimental feedback speed, with no
new B quality milestone, confidentiality qualification or whitepaper strength
claim.

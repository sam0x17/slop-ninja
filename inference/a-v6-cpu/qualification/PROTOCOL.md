# Synthetic CPU qualification for text-only warm A

Freeze this protocol before the new end-to-end forwards. Use exactly the 29
numeric cases and 12 tokenizer cases already frozen in rust-v6's extended-v1
expectations. Preserve every case, including duplicates under distinct IDs,
empty inputs and the 2049-token unsupported input. Do not select by prediction.
No corpus, reserved Test, live provider call, fit, GPU or deployment is allowed.

Runtime: copied qualified ModernBERT, reference LayerNorm and trace sources,
unchanged. The wrapper exposes the existing FP32 `head.norm` output, exactly the
input to the frozen classifier, as raw 768-dimensional h. Append 1820 explicit
zeros and call the existing Rust WarmSharedModel. Frozen Train normalization is
applied only within its residual branch. Require text contract/mode, joint
weights, and positive origin/share/mixed_fractional_share/human/ai supervision.
Reject diagnostic controls and combined inputs before encoding.

The offline oracle uses the frozen V6 extraction helper and existing Python
WarmStartA/predictions implementations with the selected immutable weights;
no fitting or data loading. Bind their sources, contract, weights, fixture and
artifact hashes. Capture h, shared z, frozen V6 logits/probabilities and every A
output. Compare exact request text hashes, token IDs/status/count, profile roster,
selected profile, fallback semantics and dimensions. Numerical gates fixed here:

- Frozen V6 calibrated probability: maximum absolute error <= 1e-6, unchanged.
- Every A probability and share on a 0..1 scale: maximum absolute error <= 1e-5
  (the existing head qualification's 0.001 percentage-point bound).
- Raw h: maximum absolute coordinate error <= 1e-4.
- Shared z: maximum absolute coordinate error <= 1e-3.

Record all deltas and failures. Tensor gates bound this synthetic qualification;
they do not assert bit equality or arbitrary-input guarantees. Do not change
kernels, weights or tolerances in response to a failed gate. Profile names,
weights and per-request outputs remain in ignored evidence. Public reporting is
aggregate only; these synthetic input fixtures may be retained separately.

Use durable task-owned units, CPU quota 100%, memory 6 GiB, zero swap and one
Cargo job; set all runtime thread counts to one. Run offline oracle and Rust
inference sequentially. Measure full encoder construction/forward and A-head
cost separately; report process RSS, cgroup peak, artifact/weight sizes and
single-run latency. Ordinary Workers and Cloudflare Containers remain unqualified.

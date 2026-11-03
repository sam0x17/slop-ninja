# ModernBERT runtime spike

2026-09-10, local macOS 26.6.2 arm64, Python 3.12.14, PyTorch 2.14.0,
Transformers 4.57.6. Dependency versions are pinned in `requirements.lock`.

We loaded `answerdotai/ModernBERT-base` revision
`8949b909ec900327062f0ebf497f51aef5e6f0c8`, initialized a three-class head and
used two synthetic strings with arbitrary labels. The following measurements
check execution. They measure no origin-detection accuracy.

| Check | Observation |
| --- | --- |
| Parameters including classification head | 149,607,171 |
| CPU forward, one thread, two 11-token padded sequences | 0.0395 seconds |
| One full-model AdamW gradient step on MPS | 2.69 seconds |
| Safetensors save/reload CPU maximum logit difference | 0.0 |
| Static `[2, 11]` PyTorch exported graph save/reload | Succeeded |
| Exported graph maximum CPU logit difference | 0.0 |
| Input above the configured token limit | Rejected |

The step timing includes first-use MPS setup and is not a sustained training
throughput estimate. A separate full-model profile used four synthetic sequences
of 1,024 tokens. Its first two MPS AdamW steps took 0.923 and 0.471 seconds;
observed driver allocation after the second step was 11.04 GiB. This establishes
local memory feasibility for the proposed batch size. Two steps do not measure
sustained epoch throughput. Neither additional memory nor the 8,192-token
upstream context limit establishes validated detector behavior at those lengths.

The artifact includes its runtime source and hashes. The ML pipeline also ran
through a separate one-epoch synthetic train/development/calibration control,
offline JSONL inference, explicit synthetic test evaluation, and deliberate
calibration-file corruption. Normal requests returned probabilities summing to
one; empty and overlength inputs returned no score; corruption was rejected.
The shard-length audit retained all twelve synthetic fixture rows and reported
overlength IDs without model execution. Updated bundles include the exact ML
source, FP64 probability postprocessing and synthetic CPU reference vectors;
the loader checked those vectors within the declared `1e-6` tolerance.
A separate synthetic control checked calibration/test probability exports against
individual reference inference; all six rows matched exactly and summed to one
within `1e-10`. Export rejected a test without acknowledgement and a calibration
file whose bytes differed from the recorded training input. Calibration fitting
and export both use the same single-request CPU contract.
All synthetic weights and logs remain ignored. A trained detector release still
requires the admitted corpus, frozen evaluation and release review in the plan.

The export used a static input shape and the PyTorch runtime. This does not
establish a general variable-length ONNX graph or a Rust inference backend.

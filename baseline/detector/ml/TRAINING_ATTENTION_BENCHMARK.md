# MPS training attention comparison

Experiment, 2026-09-11: keep eager attention. The SDPA prototype did not meet the
5% speed target and exceeded the 2% regression limit on the shorter case. It also
failed the declared initial-gradient comparison on the longer case. The queued
v4 detector recipe continues to use its original frozen code.

ModernBERT supports an SDPA implementation, so the experiment changed only the
`load_checkpoint` constructor's attention argument in an isolated copy of
`common.py`. The benchmark file stayed byte-identical across all six runs.
[ModernBERT documentation](https://github.com/huggingface/transformers/blob/main/docs/source/en/model_doc/modernbert.md).

## Timing

Machine: M3 Ultra with 256 GiB RAM, macOS 26.5.2, Python 3.12.14, PyTorch 2.14.0,
Transformers 4.57.6. Both cases used MPS, float32, batch size four, AdamW at
`2e-5`, weight decay `0.01` and gradient clipping at `1.0`. The classifier seed
was 17; synthetic token IDs used seed 1337. Each run used one separate gradient
capture step, three warmup steps and five timed optimizer steps. Timings include
synchronization and exclude checkpoint loading and gradient-file writes.

The 512-token case used lengths `[128, 256, 384, 512]`; the 1,024-token case used
`[256, 512, 768, 1024]`. Padding masks were retained. Inputs and labels were
synthetic. These measurements do not establish detector quality.

| Padded length | Original/current eager | SDPA candidate | Candidate time change |
| --- | ---: | ---: | ---: |
| 512 | 2.638 s/step | 2.737 s/step | 3.74% slower |
| 1,024 | 5.092 s/step | 5.164 s/step | 1.42% slower |

Values are medians of three per-run medians. SDPA's geometric-mean throughput
ratio was `0.97490`, a 2.51% decrease. The retained implementation is eager, so
the final gain against both the original and current baselines is zero.

The shorter case was noisy: within-run coefficients of variation ranged from
6.6% to 16.3%. Both implementations received two extra complete runs after the
first measurements. The longer case ranged from 0.5% to 3.0%. These results
support rejecting a performance change; they do not establish a precise general
speed penalty for SDPA on other hardware or workloads.

## Numerical comparison

Each case compared all 149,607,171 initial gradient entries from the first eager
and SDPA runs. The guard allowed `abs(delta) <= 1e-5 + 1e-3 * abs(reference)` for
each entry, and absolute initial-logit and loss differences up to `1e-4`.

| Padded length | Gradient entries outside tolerance | Relative gradient L2 difference | Maximum initial-logit difference |
| --- | ---: | ---: | ---: |
| 512 | 0 | `1.384e-5` | `4.768e-6` |
| 1,024 | 29 | `4.654e-5` | `9.149e-6` |

Both initial loss differences were below `1e-6`. The 1,024-token case failed the
per-entry guard despite its small aggregate difference. PyTorch documents that
attention backends can produce different floating-point results.
[PyTorch SDPA documentation](https://docs.pytorch.org/docs/main/generated/torch.nn.functional.scaled_dot_product_attention.html).

## Outcome and reproduction

One optimization was attempted, none accepted, and the isolated SDPA prototype
was rejected before application to the trainer. There were no working-tree
reverts or full resets. Four extra runs addressed timing noise. The only project
files added are this report and the fixed benchmark; training code is unchanged.

Run the benchmark with the pinned detector environment and checkpoint:

```sh
.venv/bin/python ml/benchmark_training.py \
  --checkpoint /path/to/pinned-modernbert-base --output /path/to/new-run
```

For the SDPA comparison, use an isolated runner directory containing the same
benchmark and a copy of `common.py`, changing only `attn_implementation="eager"`
to `"sdpa"` in `load_checkpoint`. Keep the CPU reference loader unchanged. Use
fresh output directories for every run. The benchmark hash is
`2b8ccc58709029ffdb0b4418f296aba494b6448295516945383c45c6bb163ba3`;
the original `common.py` hash is
`3d53d47d924a52456586d2c1e9a6fe7b58f40ff3e999aae631a57bc2d183c434`.
Local reports, prototype code, gradient artifacts and optimizer state remain in
ignored experiment storage.

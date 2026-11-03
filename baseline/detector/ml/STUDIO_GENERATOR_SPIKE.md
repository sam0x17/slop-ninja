# Studio generator runtime check

2026-09-11, Apple M3 Ultra Mac Studio, 256 GiB RAM. These measurements use one
fixed synthetic prompt of 191 words, temperature zero, seed 17 and a completion
cap of 128 tokens. No benchmark output enters the training corpus. Each row is
a single run, except where stated; the timings are operational checks, not a
controlled hardware comparison.

These measurements preceded the final corpus runtime choice. The
[pilot protocol](../PILOT_PROTOCOL.md) records the adopted native M5 run.

| Runtime and weights | Requests at once | Completion tokens / elapsed seconds | Aggregate tokens/second |
| --- | ---: | ---: | ---: |
| LM Studio llama.cpp 2.28.2, Qwen Q6_K, server parallel 4 | 1 | 128 / 15.92 | 8.04 |
| Same, server parallel 1 | 1 | 128 / 16.67 | 7.68 |
| LM Studio llama.cpp 2.23.1, Qwen Q6_K, parallel 1 | 1 | 128 / 16.76 | 7.64 |
| LM Studio llama.cpp 2.28.2, Qwen Q6_K, flash attention off | 1 | 128 / 18.90 | 6.77 |
| Homebrew llama.cpp 0.3.0, original Qwen Q6_K | 1 | 128 / 14.21 | 9.01 |
| Homebrew llama.cpp 0.3.0, Q6_K requantized to Q8_0 | 1 | 128 / 14.06 | 9.10 |
| LM Studio MLX 1.11.0, Qwen 4bit, first request | 1 | 127 / 8.40 | 15.12 |
| Same, warm repeat | 1 | 127 / 10.16 | 12.50 |
| LM Studio MLX 1.11.0, Qwen 4bit, parallel 4 | 4 | 508 / 28.26 | 17.98 |

The MLX backend reports 127 completion tokens for this 128-token cap. The table
uses the reported counts. Timings include request overhead and prompt processing.
The native Q6 and Q8 servers separately reported decode rates of 9.45 and 9.27
tokens/second. Their prompt-processing rates were 296 and 641 tokens/second.
The Q8 conversion improved prompt processing, with no observed decode gain.

We selected MLX with client/server parallelism 4 and context 8,192 because it
gave the highest measured aggregate rate. The short benchmark does not predict
full-corpus throughput, and all runs remain much slower than expected for this
hardware. We found no RAM pressure or reported power restriction. Process
arguments confirmed full GPU layer offload, GPU KV cache and zero CPU MoE layers
for the original GGUF run; the native server also confirmed all 49 layers on
the GPU. Changing the llama.cpp version or flash-attention setting did not help.
We have not established the cause of the slow decode.

The selected generator is
`mlx-community/Qwen3-30B-A3B-Instruct-2507-4bit`, pinned to revision
`e9675aa3ca5f900ccef55267914466d55ab325fa`. All 14 runtime files were checked
against the revision: SHA-256 for LFS files and Git blob SHA-1 for ordinary
files. The model card declares Apache-2.0 and identifies the upstream Qwen
checkpoint and mlx-lm 0.26.3 conversion. The captured upstream Apache license
has SHA-256 `05cab46843576551502bfdf712f84e93e6e9590d9997306ed4f6635ef82811d9`.
The pilot keeps its prespecified generation prompts, temperature 0.7, completion
cap 1,800 and source-family splits; it records the new runtime and quantization.

Detailed measurements, exact synthetic requests, model file hashes, license
captures and load configurations are ignored under `../.venv/studio-*`.
The temporary Q8 file was derived from Q6, not downloaded upstream Q8 weights.
Its SHA-256 was
`64d13490ef2ba46642391ac100622d5c1c6f873943248970caf80093261f9f2c`.
We removed that disposable 32.48 GB file after recording the conversion and
measurements. The original Q6 model remains. Native benchmark servers were
stopped, and the selected GGUF backend was restored to version 2.28.2.

Sources: [Qwen MLX model card](https://huggingface.co/mlx-community/Qwen3-30B-A3B-Instruct-2507-4bit/blob/e9675aa3ca5f900ccef55267914466d55ab325fa/README.md),
[upstream license](https://huggingface.co/Qwen/Qwen3-30B-A3B-Instruct-2507/blob/0d7cf23991f47feeb3a57ecb4c9cee8ea4a17bfe/LICENSE).

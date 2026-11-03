# Local native MLX generator check

On 2026-09-11, the local Apple M5 Max (40 GPU cores, 128 GiB RAM) generated
128 tokens in 1.23 seconds through the native MLX HTTP server. The machine was
on battery power; no power settings were changed. About 75 GiB of physical
memory was available before loading the model.

We copied the same Qwen MLX checkpoint used in the
[Studio check](STUDIO_GENERATOR_SPIKE.md):
`mlx-community/Qwen3-30B-A3B-Instruct-2507-4bit` at
`e9675aa3ca5f900ccef55267914466d55ab325fa`. All 14 runtime files matched the
previously verified SHA-256 manifest, totaling 17,197,084,801 bytes. No weights
were converted or modified.

The runtime was MLX-LM 0.31.3, MLX/MLX-Metal 0.32.2, Transformers 5.17.0 and
Python 3.12. The first installation paired MLX-LM 0.26.3 with MLX 0.32.2 and
failed before generation: the older `SwitchLinear.to_quantized` method did not
accept the newer core's `mode` argument. Updating MLX-LM resolved that interface
mismatch while keeping the checkpoint bytes unchanged.

Every run used the same 191-word synthetic prompt, temperature zero, seed 17
and a 128-token cap. All runs processed 221 prompt tokens and generated 128
tokens. No output entered the corpus.

| Execution | Elapsed seconds | Completion tokens/second, including prompt processing |
| --- | ---: | ---: |
| Direct API, first generation | 2.276 | 56.23 |
| Direct API, warm model, fresh KV cache | 1.105 | 115.82 |
| Direct API, warm repeat, fresh KV cache | 1.100 | 116.36 |
| HTTP, first request | 1.293 | 98.97 |
| HTTP, warm repeat, fresh KV cache | 1.234 | 103.73 |

The direct API reported warm decode rates of 133.67 and 134.73 tokens/second,
excluding prompt processing. Peak MLX memory was 17.45 GB; model loading took
0.70 seconds after the files had been read for hash verification. Battery
charge changed from 47% to 46% across the three direct runs. HTTP requests
reported zero cached prompt tokens. The direct benchmark preceded HTTP server
startup, so the first HTTP request was not a measurement of initial Metal
kernel compilation.

The warm HTTP rate was 8.30 times the earlier Studio single-request rate of
12.50 tokens/second. This comparison changes hardware and runtime together.
Five short sequential requests do not establish sustained corpus throughput,
thermal behavior, or the cause of the Studio slowdown.

A follow-up on the Studio used the same dependency versions, Python 3.12.14,
checkpoint bytes and rendered prompt. The Studio ran macOS 26.5.2 on AC power;
the local M5 ran macOS 26.6.2. We unloaded the idle LM Studio model before the
native measurement. Its three direct runs took 10.980, 12.493 and 12.353 seconds,
or 11.66, 10.25 and 10.36 completion tokens/second including prompt processing.
Warm decode rates were 12.29 and 12.42 tokens/second; peak MLX memory remained
17.45 GB. The slowdown persisted outside LM Studio. We did not start a native
Studio server, because this runtime was slower than the existing Studio setup.
The original LM Studio alias was restored with context 8,192 and parallelism 4.

Each machine reproduced its own output across the three temperature-zero runs,
but the two machines produced different text despite the identical inputs and
dependency versions. These observations do not establish repeatability across
hardware. The generator binds runtime identity into its run and task hashes,
so switching runtime requires a new run directory and model specification;
existing response records must retain their original runtime provenance.

The isolated environment is `../.venv/local-generator/`. The exact dependency
lock, direct benchmark script, raw HTTP events and both measurement reports
are ignored under `../.venv/local-generator-*` and
`../.venv/local-performance-native-mlx-Qwen*`. The exact checkpoint copy is
ignored under `../../../data/baseline-detector/local-qwen-mlx-model/`.
The Studio comparison and dependency lock are ignored as
`../.venv/studio-performance-native-mlx-Qwen.json` and
`../.venv/studio-generator-requirements.lock`.

For a local research server, run from the repository root:

```sh
HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1 \
baseline/detector/.venv/local-generator/bin/mlx_lm.server \
  --model "$PWD/data/baseline-detector/local-qwen-mlx-model" \
  --host 127.0.0.1 --port 18125 \
  --allowed-origins http://127.0.0.1:18125 \
  --decode-concurrency 1 --prompt-concurrency 1 --prompt-cache-size 0
```

Use `model: "default_model"` in chat requests to select the checkpoint supplied
on the command line. No remote tokenizer code is enabled. Corpus generation
through this endpoint would require a separate run and recorded runtime
identity; the benchmark did not change the ongoing Studio run.

The local server was stopped after the synthetic requests to avoid leaving a
loaded model on battery. A detached restart script is prepared at
`../.venv/start-local-generator.sh`; it records its PID and appends server logs
inside the ignored runtime directory. Starting it does not start corpus work.

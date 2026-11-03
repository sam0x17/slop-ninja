# Native corpus generation

The pilot's Mistral holdout uses MLX-LM in a separate Python 3.12 environment.
Install `requirements-generator.lock` there. The encoder uses a different
Transformers version, pinned by `requirements.lock`; keep its environment intact.

The pinned Mistral conversion contains a tokenizer regex that Transformers
requests correcting at load time. `mistral_server.py` passes
`fix_mistral_regex=True` and disables remote tokenizer code. The checkpoint files
stay unchanged. The adapter uses MLX-LM 0.31.3's model provider and accepts only
the checkpoint supplied on the command line, or the `default_model` alias.

From the repository root, after obtaining and verifying the files listed in
`baseline/detector/manifests/mistral-mlx-files.json`:

```sh
python3.12 -m venv baseline/detector/.venv/generator
baseline/detector/.venv/generator/bin/python -m pip install \
  -r baseline/detector/ml/requirements-generator.lock
HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1 \
baseline/detector/.venv/generator/bin/python \
  baseline/detector/ml/mistral_server.py \
  --model "$PWD/data/baseline-detector/local-mistral-mlx-model" \
  --host 127.0.0.1 --port 18126 \
  --allowed-origins http://127.0.0.1:18126 \
  --decode-concurrency 1 --prompt-concurrency 1 --prompt-cache-size 0 \
  --max-tokens 1800
```

The Rust generation client uses `http://127.0.0.1:18126/v1` and model ID
`default_model`. Each run's model specification must record the actual hardware,
runtime, tokenizer correction, adapter hash, dependency lock and checkpoint
manifest. A runtime change requires a distinct run identity.

The corrected synthetic HTTP check returned 128 tokens in 4.76 seconds on the
M5 Max, with temperature 0.7, no seed and no cached prompt tokens. This verifies
the serving path; it is not a detector score or a semantic-fidelity result.
The holdout changes hardware and serving runtime along with the checkpoint;
see the [pilot protocol](../PILOT_PROTOCOL.md).

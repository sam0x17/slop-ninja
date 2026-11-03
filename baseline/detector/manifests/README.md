# Pinned source metadata

`acquisition-v1.jsonl` records the 160 source works reviewed for the first pilot.
It contains attribution, retrieval and rights evidence hashes; corpus text and
raw captures remain in ignored storage. See [the acquisition report](../ACQUISITION.md)
for sampling and historical-proxy limitations.

`qwen-mlx-files.json` and `mistral-mlx-files.json` pin the downloaded generator
files. Each entry records the local SHA-256 and byte count, plus verification
against the pinned repository's LFS SHA-256 or Git blob SHA-1. These manifests
identify checkpoint bytes, independently of the inference runtime. Exact runtime,
sampling settings, prompts and response hashes belong to each generation run.

The pinned community model cards declare Apache-2.0 and identify their upstream
models. Preserve the upstream notices with any redistributed model package:

- [Qwen MLX revision](https://huggingface.co/mlx-community/Qwen3-30B-A3B-Instruct-2507-4bit/tree/e9675aa3ca5f900ccef55267914466d55ab325fa),
  [Qwen upstream license](https://huggingface.co/Qwen/Qwen3-30B-A3B-Instruct-2507/blob/0d7cf23991f47feeb3a57ecb4c9cee8ea4a17bfe/LICENSE).
- [Mistral MLX revision](https://huggingface.co/mlx-community/Mistral-Small-3.2-24B-Instruct-2506-4bit/tree/2a1d5eabfc504747bdc24178394821a1efc0edde),
  [Mistral upstream model card](https://huggingface.co/mistralai/Mistral-Small-3.2-24B-Instruct-2506).

These records do not certify the models' upstream training corpora or establish
semantic fidelity of their generated passages.

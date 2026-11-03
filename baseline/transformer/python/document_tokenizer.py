"""Token counts and character offsets only; never load or execute detector weights."""

import argparse
import hashlib
import importlib.metadata
import json
from pathlib import Path
import sys

from transformers import AutoTokenizer


def digest(data):
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact", type=Path, required=True)
    args = parser.parse_args()
    raw = (args.artifact / "manifest.json").read_bytes()
    manifest = json.loads(raw)
    artifact_id = manifest.pop("artifact_id")
    canonical = json.dumps(manifest, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()
    if digest(canonical) != artifact_id:
        raise ValueError("artifact manifest hash mismatch")
    if (manifest["schema"] != "slop_ninja.encoder.v1"
            or manifest["preprocessing"] != "exact_utf8_hf_tokenizer_no_truncation_v1"
            or manifest["labels"] != ["human_only", "model_only", "mixed"]):
        raise ValueError("unsupported tokenizer contract")
    versions = {name: importlib.metadata.version(name) for name in ("transformers", "tokenizers")}
    if any(version != manifest["reference_runtime"]["packages"][name]
           for name, version in versions.items()):
        raise ValueError("tokenizer package version mismatch")
    if list(sys.version_info[:2]) != manifest["reference_runtime"]["python_major_minor"]:
        raise ValueError("tokenizer Python version mismatch")
    # This bridge currently supports the packaged fast-tokenizer layout. Refuse
    # extra tokenizer files rather than load unmanifested optional configuration.
    assets = {"config.json", "tokenizer.json", "tokenizer_config.json", "special_tokens_map.json"}
    observed = {p.name for p in (args.artifact / "classifier").iterdir()}
    if observed != assets | {"model.safetensors"}:
        raise ValueError("unsupported classifier file layout")
    for name in assets:
        rel = "classifier/" + name
        if digest((args.artifact / rel).read_bytes()) != manifest["files"][rel]:
            raise ValueError("tokenizer asset hash mismatch: " + rel)
    tokenizer = AutoTokenizer.from_pretrained(
        args.artifact / "classifier", local_files_only=True, trust_remote_code=False,
    )
    if not tokenizer.is_fast:
        raise ValueError("exact character offsets require a fast tokenizer")
    print(json.dumps({
        "artifact_id": artifact_id,
        "manifest_sha256": digest(raw),
        "max_tokens": manifest["max_tokens"],
        "runtime": "cpu_float32_eager",
        "tokenizer_packages": versions,
        "inference_runner_sha256": manifest["files"]["runner/infer.py"],
    }), flush=True)
    for line in sys.stdin:
        request = json.loads(line)
        text = request["text"]
        encoded = tokenizer(text, add_special_tokens=True, truncation=False,
                            return_offsets_mapping=request["offsets"])
        response = {"text_sha256": digest(text.encode()), "token_count": len(encoded["input_ids"])}
        if request["offsets"]:
            # Offsets index Python Unicode scalars, not UTF-8 bytes. Special
            # tokens have empty offsets; keep only content-token positions.
            response["offsets"] = [[a, b] for a, b in encoded["offset_mapping"] if b > a]
        print(json.dumps(response, allow_nan=False), flush=True)


if __name__ == "__main__":
    main()

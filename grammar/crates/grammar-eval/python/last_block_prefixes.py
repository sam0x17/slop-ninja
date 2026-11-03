"""Cache immutable V6 prefixes for Rust-prepared exact-text requests.

Only the model/tokenizer boundary is Python. Corpus admission, splits, labels
and pooling/statistical preparation remain Rust. Each input is exactly id/text;
neither routing identity nor metadata is fed to the encoder. No truncation.
Completed rows and unsupported-input observations survive restart. An existing
output can resume only with identical requests, artifact and implementation.
"""
from __future__ import annotations
import argparse
import fcntl
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import time

import torch
from safetensors.torch import save_file, load_file
import last_block_a as model
import v6_embeddings as bridge


def read(path):
    return json.loads(Path(path).read_bytes())


def immutable(path, value):
    data = bridge.canonical(value) + b"\n"
    if path.exists():
        model.demand(path.read_bytes() == data, "immutable artifact differs: " + str(path))
    else:
        with path.open("xb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())


def run(args):
    root = Path(args.out)
    root.mkdir(exist_ok=True)
    with (root / ".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        compute(args, root)


def compute(args, root):
    requests = Path(args.requests)
    source = requests.read_bytes()
    rows = [json.loads(line) for line in source.splitlines()]
    model.demand(rows and all(set(r) == {"id", "text"} and isinstance(r["id"], str)
        and r["id"] and isinstance(r["text"], str) for r in rows), "exact id/text requests required")
    model.demand(len({r["id"] for r in rows}) == len(rows), "duplicate request id")
    model.demand(args.device in ("cpu", "cuda"), "unsupported device")
    device = torch.device(args.device)
    model.configure(device)
    encoder, tokenizer, artifact, profile = model.load_encoder(args.artifact, device)
    runtime = {n: importlib.metadata.version(n) for n in
               ("torch", "transformers", "safetensors", "numpy", "tokenizers")}
    contract = {"schema": "slop_ninja_v6_final_block_prefix_v1", "artifact_id": artifact["artifact_id"],
        "artifact_profile": profile, "last_layer": model.LAST_LAYER, "dtype": "float32",
        "dimension": 768, "maximum_tokens_including_specials": 2048,
        "padding": "none_one_request", "truncation": "forbidden", "device": str(device),
        "packages": runtime, "capture_implementation_sha256": bridge.sha256(model.__file__),
        "bridge_implementation_sha256": bridge.sha256(__file__),
        "transformers_model_source_sha256": bridge.sha256(__import__(encoder.__module__, fromlist=["x"]).__file__)}
    contract_hash = bridge.digest(bridge.canonical(contract))
    binding = {"schema": "slop_ninja_v6_prefix_job_v1", "requests_sha256": bridge.digest(source),
               "requested": len(rows), "contract_sha256": contract_hash, "contract": contract}
    immutable(root / "binding.json", binding)
    (root / "rows").mkdir(exist_ok=True)
    started = time.monotonic()
    results = []
    for i, row in enumerate(rows):
        text_hash = bridge.digest(row["text"].encode())
        key = bridge.digest(bridge.canonical({"id": row["id"], "text_sha256": text_hash,
                                           "contract_sha256": contract_hash}))
        receipt = root / "rows" / (key + ".json")
        tensor_path = root / "rows" / (key + ".safetensors")
        if receipt.exists():
            result = read(receipt)
            model.demand(result["id"] == row["id"] and result["text_sha256"] == text_hash
                         and result["contract_sha256"] == contract_hash, "cached row binding differs")
            if result["status"] == "ok":
                model.demand(bridge.sha256(tensor_path) == result["prefix_sha256"], "prefix bytes changed")
                t = load_file(tensor_path, device="cpu")
                model.demand(set(t) == {"prefix", "frozen_encoding"} and
                             list(t["prefix"].shape) == [1, result["tokens"], 768] and
                             list(t["frozen_encoding"].shape) == [1, 768], "invalid cached tensors")
            else:
                model.demand(result["status"] == "unsupported", "invalid retained status")
        else:
            t0 = time.monotonic()
            result = {"id": row["id"], "text_sha256": text_hash, "contract_sha256": contract_hash}
            try:
                ids = model.token_ids(tokenizer, row["text"], device)
            except ValueError as e:
                result.update(status="unsupported", reason=str(e))
            else:
                with torch.no_grad():
                    prefix, h = model.capture_prefix(encoder, ids)
                    replay = model.prefix_encoding(encoder, prefix)
                    error = float((h - replay).abs().max())
                    model.demand(error <= 1e-6, "real-input prefix replay mismatch")
                    tensors = {"prefix": prefix.cpu().contiguous(), "frozen_encoding": h.cpu().contiguous()}
                    model.demand(all(bool(torch.isfinite(t).all()) for t in tensors.values()), "nonfinite encoder value")
                    temp = tensor_path.with_suffix(".tmp")
                    save_file(tensors, str(temp))
                    with temp.open("rb") as f:
                        os.fsync(f.fileno())
                    # An interrupted tensor without a receipt is uncommitted;
                    # it may be deterministically recomputed in this task root.
                    os.replace(temp, tensor_path)
                    result.update(status="ok", tokens=int(ids.shape[1]),
                                  prefix_path=str(tensor_path.relative_to(root)),
                                  prefix_sha256=bridge.sha256(tensor_path),
                                  prefix_replay_max_error=error,
                                  frozen_encoding=h[0].cpu().tolist())
            result["elapsed_seconds"] = time.monotonic() - t0
            immutable(receipt, result)
        results.append(result)
        if (i + 1) % 100 == 0:
            print(json.dumps({"completed": i + 1, "requested": len(rows),
                              "elapsed_seconds": time.monotonic() - started}), flush=True)
    index = b"".join(bridge.canonical(r) + b"\n" for r in results)
    with (root / "rows.jsonl.tmp").open("wb") as f:
        f.write(index)
        f.flush()
        os.fsync(f.fileno())
    os.replace(root / "rows.jsonl.tmp", root / "rows.jsonl")
    report = {"schema": "slop_ninja_v6_prefix_export_v1", "complete": True,
              "binding_sha256": bridge.sha256(root / "binding.json"), "contract_sha256": contract_hash,
              "requested": len(rows), "supported": sum(r["status"] == "ok" for r in results),
              "unsupported": sum(r["status"] != "ok" for r in results),
              "rows_sha256": bridge.digest(index), "elapsed_seconds_this_invocation": time.monotonic() - started,
              "peak_cuda_allocated_bytes": torch.cuda.max_memory_allocated() if device.type == "cuda" else None,
              "cuda_allocation_cap_fraction": .50, "real_data_fit": False}
    bridge.write_json(root / "manifest.json", report)
    print(json.dumps(report), flush=True)


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--artifact", required=True)
    p.add_argument("--requests", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--device", default="cuda", choices=("cpu", "cuda"))
    run(p.parse_args())

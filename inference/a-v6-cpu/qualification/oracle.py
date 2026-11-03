"""Offline synthetic ML oracle; never imported by the Rust runtime.

ARTIFACT WEIGHTS CONTRACT V6_EXPECTATIONS V6_HELPER PYTHON_HELPERS NEW_OUT
Only the previously frozen synthetic fixture roster is opened, never a dataset.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import struct
import sys
import time

sys.dont_write_bytecode = True

def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()

def read(path):
    return json.loads(Path(path).read_text())

def write(path, value):
    with path.open("x") as f:
        json.dump(value, f, allow_nan=False)
        f.write("\n")
        f.flush()
        os.fsync(f.fileno())

artifact, weights_path, contract_path, fixture_path, helper_path, python_helpers, out = map(Path, sys.argv[1:])
assert not out.exists()
assert sha(weights_path) == "f8bcb42532b5575bd90a46f320d1d99a9e0e18c655e94f0260c72bffa346b5a1"
assert sha(contract_path) == "33fa8f55666474058eec8c8bf2fed2cb46279e27aa5c244076be7874462bf49f"
assert sha(fixture_path) == "bc961fbd2b337b5e11872a3461d02555e7af8cafb79363e2d6c24d37f53f2451"
assert sha(helper_path) == "7fef2bdc62a895e34bf6ca3c81b36ebd9a6ca8949d3fa018f9aa47449015cbab"
base = read(fixture_path)
assert base["synthetic_only"] is True and len(base["numeric_cases"]) == 29 and len(base["tokenizer_cases"]) == 12
spec = importlib.util.spec_from_file_location("_qualified_v6", helper_path)
v6 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(v6)
w, contract = read(weights_path), read(contract_path)
assert w["input_mode"] == contract["ablation"] == "text" and w["control"] == "joint"
assert all(w["supervision"][k] > 0 for k in ("origin", "share", "mixed_fractional_share", "human", "ai"))
out.mkdir()
manifest, profile = v6.artifact_binding(artifact.resolve())
common, tokenizer, model, temperature = v6.load_runtime(artifact.resolve(), manifest)
sys.path.insert(0, str(python_helpers))
import warm_start_a as warm
import torch
source_paths = [Path(__file__), helper_path, Path(warm.__file__), Path(warm.shared.__file__), weights_path, contract_path, fixture_path]
source_hashes = {str(p): sha(p) for p in source_paths}
assert base["artifact_id"] == manifest["artifact_id"]
torch.set_num_threads(1)
if torch.get_num_interop_threads() != 1:
    torch.set_num_interop_threads(1)
init = {"base_temperature": w["base_temperature"], "weights": {k: w["weights"][k] for k in ("origin.weight", "origin.bias")}}
heads = warm.WarmStartA(init, w["text_mean"], w["text_scale"], len(w["profiles"]["human"]), len(w["profiles"]["ai"]), "text").eval()
state = {k: torch.tensor(t["values"], dtype=torch.float32).reshape(t["shape"]) for k, t in w["weights"].items()}
state.update(text_mean=torch.tensor(w["text_mean"], dtype=torch.float32), text_scale=torch.tensor(w["text_scale"], dtype=torch.float32))
heads.load_state_dict(state, strict=True)
requests = [{"id": prefix + row["case"], "text": row["text"]}
            for prefix, cases in (("numeric:", base["numeric_cases"]), ("tokenizer:", base["tokenizer_cases"]))
            for row in cases]
with (out / "requests.jsonl").open("x") as f:
    for row in requests:
        f.write(json.dumps(row, ensure_ascii=False) + "\n")
    f.flush(); os.fsync(f.fileno())
write(out / "started.json", {"synthetic_only": True, "requested": len(requests), "sources": source_hashes, "runtime_profile": profile})
completed = []
with torch.no_grad(), (out / "completed.jsonl").open("x") as journal:
    for row, original in zip(requests, base["numeric_cases"] + base["tokenizer_cases"]):
        start = time.perf_counter()
        ids = [] if not row["text"].strip() else common.token_ids(tokenizer, row["text"], None)
        assert ids == original["ids"]
        reason = "empty_text" if not ids else ("unsupported_length" if len(ids) > 2048 else None)
        result = {"id": row["id"], "text_sha256": hashlib.sha256(row["text"].encode()).hexdigest(),
                  "token_ids": ids, "token_count": len(ids), "status": "unsupported_input" if reason else "ok", "reason": reason}
        if not reason:
            full = v6.forward(common, tokenizer, model, ids, temperature)
            h = full["embedding"]
            assert len(h) == 768
            numeric = {"contract_sha256": w["contract_sha256"], "x": h + [0.0] * 1820}
            p = warm.shared.predictions(heads, [numeric], w["temperature"], w["contract_sha256"],
                                         w["profiles"], w["thresholds"], torch.device("cpu"), export_encodings=True)[0]
            result.update(h_raw_f32=h, z_shared_f32=p.pop("encoding"), prediction=p,
                          v6_logits_f32=full["logits"], v6_probabilities_f64=full["probabilities"],
                          h_sha256=hashlib.sha256(struct.pack("<768f", *h)).hexdigest())
            if "probabilities" in original:
                assert max(abs(a-b) for a,b in zip(full["probabilities"], original["probabilities"])) <= 1e-12
        result["seconds"] = time.perf_counter() - start
        journal.write(json.dumps(result, allow_nan=False) + "\n"); journal.flush(); os.fsync(journal.fileno())
        completed.append(result)
        print(json.dumps({"id": row["id"], "tokens": len(ids), "status": result["status"], "seconds": result["seconds"]}), flush=True)
assert all(sha(p) == h for p,h in source_hashes.items()), "oracle inputs/source changed during run"
v6.artifact_binding(artifact.resolve())
write(out / "oracle.json", {"schema": "slop_ninja_a_v6_cpu_synthetic_oracle_v1", "synthetic_only": True,
                           "source_hashes": source_hashes, "requests_sha256": sha(out / "requests.jsonl"),
                           "weights_sha256": sha(weights_path), "contract_file_sha256": sha(contract_path),
                           "encoder_manifest_sha256": sha(artifact / "manifest.json"), "rows": completed})

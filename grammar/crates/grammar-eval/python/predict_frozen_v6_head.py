"""Apply the immutable V6 FP32 classifier to bound cached original encodings.

Rust owns corpus selection/labels and prepares these exact numeric requests.
This ML bridge never opens prose, fits weights, changes calibration or truncates.
"""
import argparse
import json
from pathlib import Path
import time

import torch
import last_block_a as architecture
import v6_embeddings as bridge
from warm_start_a import initializer


def check_scope(manifest, seal=None):
    final_test = seal is not None
    expected = "slop_ninja_last_block_test_fixed_comparators_v1" if final_test else "slop_ninja_last_block_fixed_comparators_v1"
    architecture.demand(manifest["schema"] == expected and manifest["complete"] is True and
                        manifest["final_test_opened"] is final_test and
                        manifest["fitting_or_calibration_performed"] is False, "bound comparator scope required")
    if final_test:
        architecture.demand(seal["schema"] == "slop_ninja_last_block_final_test_seal_v1" and
                            seal["complete"] is True and seal["model_reselection_permitted"] is False and
                            manifest["test_used_for_selection"] is False,
                            "frozen Test seal required")
        architecture.demand(any(p["sha256"] == manifest["original_prepared_manifest_sha256"]
                                for p in seal["binding_files"]), "original comparator not sealed")
        architecture.demand(any(p["path"] == manifest["v6_initializer_path"] and
                                p["sha256"] == manifest["v6_initializer_sha256"]
                                for p in seal["binding_files"]), "V6 initializer not sealed")
    return ("test", "reference") if final_test else ("development", "calibration", "reference")


@torch.no_grad()
def run(args):
    root, out = Path(args.prepared), Path(args.out)
    architecture.demand(not out.exists(), "new output required")
    manifest = json.loads((root / "manifest.json").read_bytes())
    seal = json.loads(Path(args.test_seal).read_bytes()) if args.test_seal else None
    allowed = check_scope(manifest, seal)
    if seal is not None:
        architecture.demand(bridge.sha256(args.test_seal) == manifest["selection_receipt_sha256"], "seal changed")
    for name in ("v6-numeric-requests.jsonl",):
        architecture.demand(bridge.sha256(root / name) == manifest["files_sha256"][name], "input changed")
    path = Path(manifest["v6_initializer_path"])
    architecture.demand(bridge.sha256(path) == manifest["v6_initializer_sha256"], "classifier changed")
    init = initializer(path)
    device = torch.device("cuda")
    architecture.configure(device)
    layer = torch.nn.Linear(768, 3, bias=True, device=device).eval()
    for name in ("weight", "bias"):
        spec = init["weights"]["origin." + name]
        getattr(layer, name).copy_(torch.tensor(spec["values"], dtype=torch.float32, device=device).reshape(spec["shape"]))
    requests = [json.loads(line) for line in (root / "v6-numeric-requests.jsonl").read_bytes().splitlines()]
    seen, output = set(), []
    started = time.monotonic()
    for row in requests:
        architecture.demand(row["id"] not in seen and row["part"] in allowed, "scope/duplicate")
        seen.add(row["id"])
        h = torch.tensor([row["h_raw_f32"]], dtype=torch.float32, device=device)
        architecture.demand(h.shape == (1, 768) and bool(torch.isfinite(h).all()), "bad encoding")
        logits = layer(h)
        probabilities = (logits.double() / init["base_temperature"]).softmax(-1)[0].cpu().tolist()
        output.append({"id": row["id"], "part": row["part"], "text_sha256": row["text_sha256"],
                       "prefix_sha256": row["prefix_sha256"], "logits_f32": logits[0].cpu().tolist(),
                       "prediction": {"origin": dict(zip(("human_only", "model_only", "mixed"), probabilities)),
                                      "ai_confidence_percent": 100. * (probabilities[1] + probabilities[2])}})
    out.mkdir()
    bridge.write_json(out / "predictions.json", output)
    bridge.write_json(out / "manifest.json", {
        "schema": "slop_ninja_frozen_v6_head_predictions_v1" if seal is None else
                  "slop_ninja_frozen_v6_test_head_predictions_v1", "complete": True,
        "prepared_manifest_sha256": bridge.sha256(root / "manifest.json"),
        "predictions_sha256": bridge.sha256(out / "predictions.json"),
        "initializer_sha256": bridge.sha256(path), "original_temperature": init["base_temperature"],
        "requested": len(requests), "supported": len(output), "unsupported": 0,
        "torch": torch.__version__, "device": str(device), "elapsed_seconds": time.monotonic() - started,
        "peak_cuda_allocated_bytes": torch.cuda.max_memory_allocated(),
        "full_encoder_cost_included": False, "fitting_or_calibration_performed": False,
        "final_test_opened": seal is not None, "share_or_attribution_provided": False,
        "selection_receipt_sha256": manifest.get("selection_receipt_sha256"),
        "implementation_sha256": bridge.sha256(__file__)})
    print(json.dumps({"complete": True, "supported": len(output)}), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prepared", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--test-seal", help="Explicit sealed Test route; never fits or recalibrates")
    run(parser.parse_args())

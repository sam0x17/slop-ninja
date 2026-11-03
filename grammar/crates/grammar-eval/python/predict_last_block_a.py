"""Raw numeric predictions after the Rust Development selection is sealed.

No calibration or corpus handling here: Rust supplies bound prefix/partition
rows and owns calibration/statistics. This bridge applies the selected encoder
and all common-encoding heads, including independent reference rows.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import time

import torch
from safetensors.torch import load_file

import export_last_block_a as export
import last_block_a as architecture
import shared_a as shared
import train_last_block_a as trainer
import v6_embeddings as bridge
from warm_start_a import WarmStartA, initializer


def test_scope(manifest, seal, contract_hash, fitting_hash, selection_hash, part):
    shared.demand(manifest["schema"] == "slop_ninja_last_block_test_dataset_v1" and
                  manifest["complete"] is True and manifest["final_test_opened"] is True and
                  manifest["test_used_for_selection"] is False and
                  manifest["normalization_refit"] is False and
                  manifest["profile_enrollment_changed"] is False and
                  manifest["contract_sha256"] == contract_hash and
                  manifest["fitting_dataset_manifest_sha256"] == fitting_hash and
                  manifest["selection_sha256"] == selection_hash,
                  "unchanged fitting contract and sealed Test transform required")
    shared.demand(seal["schema"] == "slop_ninja_last_block_final_test_seal_v1" and
                  seal["complete"] is True and seal["model_reselection_permitted"] is False and
                  seal["test_used_for_selection"] is False and
                  seal["dataset"]["sha256"] == fitting_hash and
                  seal["selection"]["sha256"] == selection_hash and part in ("test", "reference"),
                  "Test seal does not bind this fitting dataset/selection")


@torch.no_grad()
def run(args):
    root, fit, output = (Path(p).resolve() for p in (args.data, args.fit, args.out))
    shared.demand(not output.exists(), "new prediction output required")
    manifest, contract = export.read(root / "manifest.json"), export.read(root / "contract.json")
    final_test = args.test_seal is not None
    if final_test:
        shared.demand(args.fit_data is not None, "Test requires original fitting bundle")
        fitting_root = Path(args.fit_data).resolve()
        fitting_manifest = export.read(fitting_root / "manifest.json")
        seal = export.read(args.test_seal)
        export.require_digest(args.test_seal, manifest["selection_receipt_sha256"])
        export.require_digest(root / "seal.json", manifest["selection_receipt_sha256"])
        test_scope(manifest, seal, bridge.sha256(fitting_root / "contract.json"),
                   bridge.sha256(fitting_root / "manifest.json"), bridge.sha256(args.selection), args.part)
    else:
        shared.demand(args.fit_data is None and args.part in ("development", "calibration", "reference"),
                      "Test cannot enter the non-Test route")
        fitting_root, fitting_manifest = root, manifest
    shared.demand(fitting_manifest["schema"] == "slop_ninja_last_block_a_dataset_v1" and
                  fitting_manifest["complete"] is True and fitting_manifest["final_test_opened"] is False,
                  "qualified original non-Test fitting dataset required")
    export.require_digest(root / "contract.json", manifest["contract_sha256"])
    selection = export.read(args.selection)
    shared.demand(selection["schema"] == "slop_ninja_last_block_condition_selection_v1" and
                  selection["complete"] is True and selection["selection_split"] == "development" and
                  selection["final_test_opened"] is False and
                  selection["calibration_read_for_selection"] is False and
                  selection["seeds"] == [17, 29, 41] and
                  selection["dataset_manifest_sha256"] == bridge.sha256(fitting_root / "manifest.json"),
                  "sealed matched Development selection required before CAL prediction")
    binding, saved, checkpoint = export.select_checkpoint(fit, fitting_manifest, contract, args.init, False)
    export.require_digest(fitting_root / "manifest.json", binding["dataset_manifest_sha256"])
    selected = [r for r in selection["runs"] if r["condition"] == binding["condition"] and r["seed"] == binding["seed"]]
    shared.demand(len(selected) == 1 and selected[0]["selected_checkpoint_sha256"] == bridge.sha256(checkpoint),
                  "checkpoint not bound by selection")
    if final_test:
        sealed = [r for r in seal["runs"] if r["condition"] == binding["condition"] and r["seed"] == binding["seed"]]
        shared.demand(len(sealed) == 1 and sealed[0]["checkpoint"]["sha256"] == bridge.sha256(checkpoint),
                      "checkpoint absent from frozen Test seal")
    name = args.part + ".jsonl"
    export.require_digest(root / name, manifest["files_sha256"][name])
    rows = [json.loads(line) for line in (root / name).read_bytes().splitlines()]
    shared.demand(rows and len({r["id"] for r in rows}) == len(rows), "empty or duplicate population")
    reference_parts = ("test",) if final_test else ("train", "development", "calibration")
    shared.demand(all((r["purpose"] == "reference" and r["frozen_split"] in
                      reference_parts) if args.part == "reference" else
                     (r["purpose"] == "query" and r["frozen_split"] == args.part) for r in rows),
                  "wrong partition/purpose")
    device = torch.device(args.device)
    architecture.configure(device)
    encoder, _, parent, _ = architecture.load_encoder(args.artifact, device)
    shared.demand(parent["artifact_id"] == contract["parent_artifact_id"], "parent changed")
    norm = contract["embedding_normalization"]
    profiles = {k: contract[k + "_profiles"] for k in ("human", "ai")}
    heads = WarmStartA(initializer(args.init), norm["mean"], norm["scale"],
                       len(profiles["human"]), len(profiles["ai"]), "text").to(device)
    joint = architecture.LastBlockA(encoder, heads, binding["condition"] == "adapt_last_block").eval()
    trainer.restore(joint, saved["state"])
    output.mkdir()
    started = time.monotonic()
    with (output / "predictions.jsonl").open("xb") as f:
        for row in rows:
            export.require_digest(row["prefix"]["path"], row["prefix"]["sha256"])
            tensors = load_file(row["prefix"]["path"], device=str(device))
            shared.demand(set(tensors) == {"prefix", "frozen_encoding"} and
                          tensors["prefix"].shape == (1, row["prefix"]["tokens"], 768) and
                          0 < row["prefix"]["tokens"] <= 2048 and
                          tensors["frozen_encoding"].shape == (1, 768) and
                          all(v.dtype == torch.float32 and bool(torch.isfinite(v).all()) for v in tensors.values()),
                          "invalid bound prefix tensors")
            prediction = trainer.forward(joint, tensors)
            result = {"id": row["id"], "prefix_sha256": row["prefix"]["sha256"],
                      "origin_logits": prediction["origin_logits"][0].cpu().tolist(),
                      "conditional_mixed_share": float(prediction["conditional_mixed_share"][0]),
                      "human_logits": prediction["human_logits"][0].cpu().tolist(),
                      "ai_logits": prediction["ai_logits"][0].cpu().tolist(),
                      "encoding": prediction["encoding"][0].cpu().tolist()}
            f.write(bridge.canonical(result) + b"\n")
            f.flush()
    receipt = {"schema": "slop_ninja_last_block_test_raw_predictions_v1" if final_test else
               "slop_ninja_last_block_raw_predictions_v1", "complete": True,
               "partition": args.part, "requested": len(rows), "supported": len(rows), "unsupported": 0,
               "condition": binding["condition"], "seed": binding["seed"],
               "selection_sha256": bridge.sha256(args.selection),
               "dataset_manifest_sha256": bridge.sha256(root / "manifest.json"),
               "partition_sha256": manifest["files_sha256"][name],
               "contract_sha256": manifest["contract_sha256"],
               "selected_checkpoint_sha256": bridge.sha256(checkpoint),
               "predictions_sha256": bridge.sha256(output / "predictions.jsonl"),
               "implementation_sha256": bridge.sha256(__file__), "final_test_opened": final_test,
               "origin_logits_semantics": "original_classifier_linear_divided_by_base_temperature; no_CAL_temperature",
               "elapsed_seconds": time.monotonic() - started,
               "device": str(device), "torch": torch.__version__,
               "peak_cuda_allocated_bytes": torch.cuda.max_memory_allocated() if device.type == "cuda" else None}
    if final_test:
        receipt.update(selection_receipt_sha256=manifest["selection_receipt_sha256"],
                       fitting_dataset_manifest_sha256=binding["dataset_manifest_sha256"],
                       test_used_for_selection=False)
    bridge.write_json(output / "manifest.json", receipt)
    print(json.dumps({"complete": True, "condition": binding["condition"], "seed": binding["seed"],
                      "partition": args.part, "supported": len(rows)}), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("data", "fit", "selection", "artifact", "init", "out"):
        parser.add_argument("--" + key, required=True)
    parser.add_argument("--part", choices=("development", "calibration", "reference", "test"), required=True)
    parser.add_argument("--test-seal", help="Explicit frozen Test route; never permits fitting or reselection")
    parser.add_argument("--fit-data", help="Original non-Test fitting bundle, required with --test-seal")
    parser.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
    run(parser.parse_args())

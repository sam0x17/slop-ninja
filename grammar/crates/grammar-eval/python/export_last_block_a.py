"""Export a fitted final-block derivative without modifying recovered V6.

The optional epoch-one path exports the first real checkpoint as explicitly
uncalibrated evidence. Calibration is a separately bound Rust-prepared artifact;
this module does not discover corpora, choose a model, or fit a threshold.
Synthetic full-encoder references accompany every numeric export.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil

# Match the qualified V6 CPU oracle's reduction/kernels. These do not alter the
# CUDA training runtime; export receipts record them even for CUDA references.
os.environ.setdefault("MKL_CBWR", "COMPATIBLE")
os.environ.setdefault("ATEN_CPU_CAPABILITY", "default")

import torch
from safetensors.torch import save_file

import last_block_a as architecture
import shared_a as shared
import train_last_block_a as trainer
import v6_embeddings as bridge
import warm_start_a as heads_module


TENSORS = ["attn_norm.weight", "attn.Wqkv.weight", "attn.Wo.weight",
           "mlp_norm.weight", "mlp.Wi.weight", "mlp.Wo.weight"]


def read(path):
    return json.loads(Path(path).read_bytes())


def require_digest(path, expected):
    shared.demand(bridge.sha256(path) == expected, "changed bound file: " + str(path))


def select_checkpoint(fit, manifest, contract, init, epoch_one):
    """Check an export's exact data/checkpoint lineage, including early exports."""
    binding = read(fit / "binding.json")
    shared.demand(binding["schema"] == "slop_ninja_last_block_fit_binding_v1" and
                  binding["condition"] in ("frozen_last_block", "adapt_last_block") and
                  binding["seed"] in (17, 29, 41) and
                  binding["contract_sha256"] == manifest["contract_sha256"] and
                  binding["protocol_sha256"] == manifest["protocol_sha256"] and
                  binding["parent_artifact_id"] == contract["parent_artifact_id"],
                  "fit input/parent lineage differs")
    require_digest(init, binding["initializer_sha256"])
    for module, key in ((architecture, "architecture_sha256"), (shared, "shared_loss_sha256"),
                        (heads_module, "shared_heads_implementation_sha256"),
                        (bridge, "artifact_loader_sha256")):
        require_digest(module.__file__, binding[key])
    checkpoint = fit / ("epoch-1.pt" if epoch_one else "selected.pt")
    if not epoch_one:
        report = read(fit / "fit.json")
        shared.demand(report["complete"] is True and report["real_data_fit"] is True and
                      report["final_test_opened"] is False and report["binding"] == binding,
                      "completed real fit required")
        require_digest(checkpoint, report["selected_checkpoint_sha256"])
    saved = torch.load(checkpoint, map_location="cpu", weights_only=False)
    shared.demand(saved["binding"] == binding and 0 <= saved["epoch"] <= 12 and
                  (not epoch_one or saved["epoch"] == 1), "checkpoint lineage differs")
    if not epoch_one:
        shared.demand(saved["epoch"] == report["selected_epoch"], "selected epoch differs")
    return binding, saved, checkpoint


def calibration(path, binding, checkpoint_hash, contract_hash):
    if path is None:
        return 1., {"human": 1., "ai": 1.}, False, None
    value = read(path)
    shared.demand(value["schema"] == "slop_ninja_last_block_calibration_v1" and
                  value["complete"] is True and value["fit_split"] == "calibration" and
                  value["final_test_opened"] is False and
                  value["condition"] == binding["condition"] and value["seed"] == binding["seed"] and
                  value["selected_checkpoint_sha256"] == checkpoint_hash and
                  value["contract_sha256"] == contract_hash and
                  value["origin_calibrated"] is True and
                  all(value["roles"][r]["qualified"] is True for r in ("human", "ai")),
                  "qualified CAL-only lineage required")
    return value["temperature"], {r: value["roles"][r]["threshold"] for r in ("human", "ai")}, True, value


def supervision(rows, epoch):
    counts = {k: sum(r["targets"].get(k) is not None for r in rows)
              for k in ("origin", "share", "human", "ai")}
    counts["mixed_fractional_share"] = sum(r["targets"].get("origin") == 2 and
                                           r["targets"].get("share") is not None for r in rows)
    return {k: n if epoch > 0 else 0 for k, n in counts.items()}


@torch.no_grad()
def export(args):
    root, fit, parent, out = (Path(p).resolve() for p in (args.data, args.fit, args.artifact, args.out))
    shared.demand(not out.exists() and not out.is_relative_to(parent), "new export outside parent required")
    manifest, contract = read(root / "manifest.json"), read(root / "contract.json")
    shared.demand(manifest["schema"] == "slop_ninja_last_block_a_dataset_v1" and
                  manifest["complete"] is True and manifest["final_test_opened"] is False,
                  "qualified fitting bundle required")
    require_digest(root / "contract.json", manifest["contract_sha256"])
    require_digest(root / "protocol.json", manifest["protocol_sha256"])
    binding, saved, checkpoint = select_checkpoint(fit, manifest, contract, args.init, args.epoch_one)
    require_digest(root / "manifest.json", binding["dataset_manifest_sha256"])
    shared.demand(not (args.epoch_one and args.calibration), "early checkpoint is diagnostic only")
    temperature, thresholds, calibrated, cal = calibration(
        args.calibration, binding, bridge.sha256(checkpoint), manifest["contract_sha256"])
    device = torch.device(args.device)
    architecture.configure(device)
    encoder, tokenizer, parent_manifest, _ = architecture.load_encoder(parent, device)
    shared.demand(parent_manifest["artifact_id"] == contract["parent_artifact_id"], "parent identity differs")
    profiles = {r: contract[r + "_profiles"] for r in ("human", "ai")}
    norm = contract["embedding_normalization"]
    heads = heads_module.WarmStartA(heads_module.initializer(args.init), norm["mean"], norm["scale"],
                                  len(profiles["human"]), len(profiles["ai"]), "text").to(device)
    joint = architecture.LastBlockA(encoder, heads, binding["condition"] == "adapt_last_block").eval()
    if binding["condition"] == "frozen_last_block":
        shared.demand(all(torch.equal(v.cpu(), saved["state"]["last_layer"][k])
                          for k, v in encoder.model.layers[21].state_dict().items()),
                      "frozen condition changed encoder weights")
    trainer.restore(joint, saved["state"])
    shared.demand(set(saved["state"]["last_layer"]) == set(TENSORS), "wrong final-block tensor roster")
    for name, expected in (("text_mean", norm["mean"]), ("text_scale", norm["scale"])):
        shared.demand(torch.equal(getattr(heads, name).cpu(), torch.tensor(expected, dtype=torch.float32)),
                      "checkpoint normalization differs from Train contract")
    train_bytes = (root / "train.jsonl").read_bytes()
    shared.demand(bridge.digest(train_bytes) == manifest["files_sha256"]["train.jsonl"], "Train labels changed")
    rows = [json.loads(line) for line in train_bytes.splitlines()]
    counts = supervision(rows, saved["epoch"])
    shared.demand(not calibrated or (saved["epoch"] > 0 and all(counts.values())),
                  "calibrated A needs supervised fitted heads")
    weights = heads_module.artifact(heads, {"ablation": "text"}, manifest, args.init,
                                   temperature, profiles, thresholds, counts)
    # References include punctuation, Unicode, multiple paragraphs and a longer
    # sequence. They are synthetic and do not access held-out corpus text.
    cases = [
        {"id": "synthetic-short", "text": "The gardener carried a small basket through the quiet village."},
        {"id": "synthetic-unicode", "text": "“Café,” said the traveller. Rain fell at 3:15.\n\nWe stayed indoors."},
        {"id": "synthetic-long", "text": "The experiment measured rainfall, temperature, and plant growth. " * 48},
    ]
    fixture_path = getattr(args, "synthetic_fixture", None)
    fixture_hash = None
    if fixture_path:
        fixture_hash = bridge.sha256(fixture_path)
        shared.demand(fixture_hash == "bc961fbd2b337b5e11872a3461d02555e7af8cafb79363e2d6c24d37f53f2451",
                      "only the previously frozen synthetic boundary roster is permitted")
        fixture = read(fixture_path)
        shared.demand(fixture["synthetic_only"] is True and len(fixture["numeric_cases"]) == 29
                      and len(fixture["tokenizer_cases"]) == 12, "complete original boundary roster required")
        for kind in ("numeric", "tokenizer"):
            for case in fixture[kind + "_cases"]:
                cases.append({"id": "boundary-" + kind + "-" + case["case"], "text": case["text"],
                              "expected_token_ids": case["ids"], "expected_status": case.get("status", "ok")})
    references = []
    for case in cases:
        token_ids = [] if not case["text"].strip() else tokenizer(case["text"], add_special_tokens=True,
                                                                  truncation=False, padding=False)["input_ids"]
        status = "empty_text" if not case["text"].strip() else ("unsupported_length" if len(token_ids) > 2048 else "ok")
        if "expected_token_ids" in case:
            shared.demand(token_ids == case["expected_token_ids"] and status == case["expected_status"],
                          "frozen tokenizer boundary changed")
        if status != "ok":
            references.append({"id": case["id"], "text_sha256": bridge.digest(case["text"].encode()),
                               "token_ids": token_ids, "status": "unsupported_input", "reason": status,
                               "prediction": None, "calibrated": calibrated})
            continue
        ids = architecture.token_ids(tokenizer, case["text"], device)
        h = architecture.full_encoding(encoder, ids)
        x = torch.cat((h, h.new_zeros((1, 1820))), 1)[0].cpu().tolist()
        prediction = shared.predictions(heads, [{"contract_sha256": manifest["contract_sha256"], "x": x}],
                                        temperature, manifest["contract_sha256"], profiles,
                                        thresholds, device, True)[0]
        references.append({"id": case["id"], "text_sha256": bridge.digest(case["text"].encode()),
                           "token_ids": ids[0].cpu().tolist(), "h_raw_f32": h[0].cpu().tolist(),
                           "prediction": prediction, "calibrated": calibrated})
    out.mkdir()
    shutil.copyfile(root / "contract.json", out / "contract.json")
    save_file({k: v.cpu().contiguous() for k, v in saved["state"]["last_layer"].items()},
              str(out / "last-layer.safetensors"))
    bridge.write_json(out / "weights.json", weights)
    with (out / "reference-requests.jsonl").open("xb") as f:
        for case in cases:
            f.write(bridge.canonical({k: case[k] for k in ("id", "text")}) + b"\n")
    bridge.write_json(out / "reference-predictions.json", references)
    if cal:
        shutil.copyfile(args.calibration, out / "calibration.json")
    member = lambda name: {"file": name, "sha256": bridge.sha256(out / name)}
    report = {"schema": "slop_ninja_last_block_a_export_v1", "complete": True,
              "condition": binding["condition"], "seed": binding["seed"], "selected_epoch": saved["epoch"],
              "parent_artifact_id": parent_manifest["artifact_id"],
              "parent_manifest_sha256": bridge.sha256(parent / "manifest.json"),
              "layer": 21, "tensor_names": TENSORS, "contract": member("contract.json"),
              "weights": member("weights.json"), "last_layer": member("last-layer.safetensors"),
              "reference_requests": member("reference-requests.jsonl"),
              "reference_predictions": member("reference-predictions.json"),
              "calibration": member("calibration.json") if cal else None,
              "calibrated": calibrated, "early_epoch_one_diagnostic": args.epoch_one,
              "selected_checkpoint_sha256": bridge.sha256(checkpoint), "fit_binding": binding,
              "exporter_sha256": bridge.sha256(__file__), "final_test_opened": False,
              "reference_device": str(device), "production_qualified": False,
              "synthetic_boundary_fixture_sha256": fixture_hash,
              "runtime": {"torch": torch.__version__, "cuda": torch.version.cuda,
                          "cpu_environment": {k: os.environ.get(k) for k in
                              ("MKL_CBWR", "ATEN_CPU_CAPABILITY", "OMP_NUM_THREADS", "MKL_NUM_THREADS")}},
              "limits": ["Rust full-encoder numerical replay and runtime measurements still required.",
                         "Uncalibrated named outputs use threshold1 and are diagnostics, not qualified attribution.",
                         "No Workers/Wasm or Cloudflare Container qualification follows from numeric export."]}
    bridge.write_json(out / "manifest.json", report)
    print(json.dumps({"complete": True, "epoch": saved["epoch"], "calibrated": calibrated,
                      "synthetic_reference_cases": len(references)}), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("data", "fit", "artifact", "init", "out"):
        parser.add_argument("--" + key, required=True)
    parser.add_argument("--epoch-one", action="store_true")
    parser.add_argument("--calibration")
    parser.add_argument("--synthetic-fixture", help="Pinned original 29 numeric and 12 tokenizer cases, appended to all three standard cases")
    parser.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
    export(parser.parse_args())

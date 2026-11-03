"""Frozen stage-3 adapted A (final-block adaptation, a-encoder-stage3 fits-v1) raw
predictions for one non-Test partition of a full-A v2 bundle, for the protocol's
fixed-comparator CAL calibration. Weights are unchanged: the V6 encoder with the
fit's selected layer-21 tensors and its selected shared heads (stage-3 contract
normalization and rosters). The full-text forward is the stage-3 LastBlockA
forward (layers 0..20 are V6's, so it equals the cached-prefix path); it runs
through FullA (span head none, share mode head), the same raw/prediction code as
export_full_a.py raw and v6_raw.py. No fitting, no calibration, never Test.

Before scoring, the stage-3 export's reference requests are replayed: token ids
must match and the encoder output (h_raw_f32) and shared encoding must agree
within 1e-5 of the stage-3 export, or the run stops.

Run with the release trainer modules on PYTHONPATH (grammar-eval/python at the
release commit), from this directory-free location:
  stage3_raw.py BUNDLE FIT STAGE3_CONTRACT STAGE3_SELECTION STAGE3_EXPORT ARTIFACT INIT PART OUT"""
import json, sys
from pathlib import Path
import torch
from safetensors.torch import load_file
import full_a, full_a_bundle as bundle, shared_a as shared, train_full_a as trainer, v6_embeddings as bridge
import last_block_a as base
from warm_start_a import WarmStartA, initializer

LOGIT_SEMANTICS = "log(p_model+p_mixed)-log(p_human) of the final uncalibrated origin"
TOLERANCE = 1e-5

root, fit, s3contract, selection, export, artifact, init, part, out = sys.argv[1:10]
root, fit, export, out = Path(root).resolve(), Path(fit).resolve(), Path(export).resolve(), Path(out).resolve()
shared.demand(part == "cal", "only CAL is scored here; Test is never read")
shared.demand(not out.exists(), "new output required")
read = lambda p: json.loads(Path(p).read_text())
digests = bundle.bundle_digests(root)
report = read(fit / "fit.json")
b = report["binding"]
shared.demand(report["schema"] == "slop_ninja_last_block_a_fit_v1" and report["complete"] is True and
              report["final_test_opened"] is False and b["condition"] == "adapt_last_block",
              "complete adapted stage-3 fit required")
shared.demand(bridge.sha256(fit / "last-layer.safetensors") == report["last_layer_sha256"] and
              bridge.sha256(fit / "shared-heads.safetensors") == report["shared_heads_sha256"] and
              bridge.sha256(fit / "selected.pt") == report["selected_checkpoint_sha256"],
              "stage-3 weights differ from fit.json")
contract = read(s3contract)
shared.demand(bridge.sha256(s3contract) == b["contract_sha256"], "stage-3 contract differs from the fit binding")
shared.demand(bridge.sha256(init) == b["initializer_sha256"], "initializer differs from the fit binding")
sel = read(selection)
shared.demand(sel["schema"] == "slop_ninja_last_block_condition_selection_v1" and sel["complete"] is True and
              sel["selected_condition"] == "adapt_last_block" and sel["calibration_read_for_selection"] is False and
              sel["final_test_opened"] is False, "sealed stage-3 Development selection required")
emanifest = read(export / "manifest.json")
shared.demand(emanifest["fit_binding"] == b and emanifest["last_layer"]["sha256"] == report["last_layer_sha256"],
              "stage-3 export is not of this fit")

device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
trainer.configure(device, 0.45)
encoder, tokenizer, parent, _ = base.load_encoder(artifact, device)
shared.demand(parent["artifact_id"] == contract["parent_artifact_id"] == b["parent_artifact_id"], "parent differs")
norm = contract["embedding_normalization"]
profiles = {k: list(contract[k + "_profiles"]) for k in ("human", "ai")}
heads = WarmStartA(initializer(init), norm["mean"], norm["scale"], len(profiles["human"]), len(profiles["ai"]), "text")
encoder.model.layers[base.LAST_LAYER].load_state_dict(load_file(str(fit / "last-layer.safetensors")), strict=True)
heads.load_state_dict(load_file(str(fit / "shared-heads.safetensors")), strict=True)
model = full_a.FullA(encoder, heads, None, "head", None).to(device)
model.token_loss_weight = 0.
model.eval()

# Replay the stage-3 export's reference requests (synthetic texts) before any CAL row.
qual = []
refs = {r["id"]: r for r in read(export / "reference-predictions.json")}
with torch.no_grad():
    for line in (export / "reference-requests.jsonl").read_text().splitlines():
        q = json.loads(line)
        ref = refs[q["id"]]
        ids = tokenizer(q["text"], add_special_tokens=True, truncation=False, padding=False)["input_ids"]
        shared.demand(ids == ref["token_ids"], f"reference {q['id']}: token ids differ")
        t = torch.tensor([ids], dtype=torch.long, device=device)
        o = model(t, torch.ones_like(t), torch.ones_like(t, dtype=torch.bool), autocast=False)
        eh = float((o["h"][0].cpu().double() - torch.tensor(ref["h_raw_f32"], dtype=torch.float64)).abs().max())
        ez = float((o["encoding"][0].cpu().double() - torch.tensor(ref["prediction"]["encoding"], dtype=torch.float64)).abs().max())
        qual.append({"id": q["id"], "tokens": len(ids), "h_max_abs_error": eh, "encoding_max_abs_error": ez})
        shared.demand(eh <= TOLERANCE and ez <= TOLERANCE, f"reference {q['id']}: replay error {eh} / {ez}")

population = bundle.Population(root, part, full_a.hf_tokenize(tokenizer), set(tokenizer.all_special_ids),
                               profiles["ai"], cohort_weighted=False)
out.mkdir()
with torch.no_grad(), (out / "raw.jsonl").open("xb") as f, (out / "predictions.jsonl").open("xb") as g:
    for i, meta in enumerate(population.meta):
        ids, mask, content, _, words = population.batch([i], device, tokenizer.pad_token_id)
        o = model(ids, mask, content, autocast=False, words=words)
        f.write(bridge.canonical(trainer.raw_prediction(meta, o, population.lengths[i])) + b"\n")
        g.write(bridge.canonical(trainer.contract_prediction(meta, o, profiles["ai"])) + b"\n")
bridge.write_json(out / "manifest.json", {"schema": "slop_ninja_a_comp_fixed_raw_v1", "complete": True,
    "partition": part, "rows": len(population), "system": "frozen stage-3 adapted A (adapt_last_block)",
    "seed": b["seed"], "condition": b["condition"], "bundle_sha256": digests,
    "stage3_fit_sha256": bridge.sha256(fit / "fit.json"), "stage3_selected_checkpoint_sha256": report["selected_checkpoint_sha256"],
    "stage3_selected_epoch": report["selected_epoch"],
    "last_layer_sha256": report["last_layer_sha256"], "shared_heads_sha256": report["shared_heads_sha256"],
    "stage3_contract_sha256": b["contract_sha256"], "stage3_selection_sha256": bridge.sha256(selection),
    "stage3_export_manifest_sha256": bridge.sha256(export / "manifest.json"),
    "init_sha256": bridge.sha256(init), "parent_artifact_id": parent["artifact_id"],
    "ai_profiles": profiles["ai"], "human_profiles": profiles["human"],
    "trained_selected_heads": report["trained_selected_heads"], "share_mode": "head",
    "involvement_logit_semantics": LOGIT_SEMANTICS,
    "origin_logits_semantics": "original_classifier_linear_divided_by_base_temperature; no CAL map",
    "reference_replay": {"tolerance": TOLERANCE, "cases": qual},
    "unrostered_generator_labels": dict(population.unrostered),
    "predictions_sha256": bridge.sha256(out / "predictions.jsonl"), "raw_sha256": bridge.sha256(out / "raw.jsonl"),
    "implementation_sha256": bridge.sha256(__file__), "trainer_sha256": bridge.sha256(trainer.__file__),
    "full_a_sha256": bridge.sha256(full_a.__file__), "final_test_opened": False, "calibrated": False,
    "device": str(device), "torch": torch.__version__})
print(json.dumps({"complete": True, "partition": part, "rows": len(population), "seed": b["seed"], "replay": qual}), flush=True)

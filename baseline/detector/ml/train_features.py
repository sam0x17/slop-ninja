"""Fit a feature-only neural origin head from an immutable Rust export."""

import argparse
import json
import math
from pathlib import Path
import random
import shutil
import time

import torch
from safetensors.torch import load_model, save_model

from common import LABELS, configure_cpu, fit_temperature, metrics, sha256, software, write_json
from feature_nn import FeatureHead, feature_gradient, load_bundle, predict, read_export
from train import evaluation_view, human_model_metrics, source_origin_weights


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vectors", type=Path, required=True)
    parser.add_argument("--source-shards", type=Path, required=True)
    parser.add_argument("--views", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--hidden-width", type=int, choices=[0, 128, 256, 512, 1024], required=True)
    parser.add_argument("--epochs", type=int, default=30)
    parser.add_argument("--batch-size", type=int, default=64)
    parser.add_argument("--learning-rate", type=float, default=3e-4)
    parser.add_argument("--seed", type=int, default=17)
    args = parser.parse_args()
    if (args.output.exists() or min(args.epochs, args.batch_size, args.learning_rate) <= 0
            or not math.isfinite(args.learning_rate)):
        parser.error("use a new output directory and positive training settings")
    configure_cpu(2)
    torch.manual_seed(args.seed)
    torch.use_deterministic_algorithms(True)
    rng = random.Random(args.seed)
    rows, tensors, export, source_hashes = read_export(args.vectors, args.source_shards)
    archive_counts = {split: len(part) for split, part in rows.items()}
    views = {}
    for split in ("development", "calibration"):
        selected, views[split] = evaluation_view(args.views / (split + "-R2.json"),
                args.source_shards / (split + ".jsonl"), split, rows[split])
        tensors[split] = tensors[split][[r["tensor_index"] for r in selected]]
        rows[split] = selected
    if any({r["label"] for r in part} != {0, 1, 2} for part in rows.values()):
        raise ValueError("all three origins required in every partition")
    weight_map = source_origin_weights(rows["train"])
    weights = torch.tensor([weight_map[r["id"]] for r in rows["train"]])
    labels = {split: [r["label"] for r in part] for split, part in rows.items()}
    train_labels = torch.tensor(labels["train"], dtype=torch.long)
    model = FeatureHead(export["dimensions"], args.hidden_width)
    optimizer = torch.optim.AdamW(model.parameters(), lr=args.learning_rate, weight_decay=0.001)
    args.output.mkdir(parents=True, exist_ok=False)
    source_files = ["feature_nn.py", "train_features.py", "common.py", "train.py", "checkpoints.py", "requirements.lock"]
    settings = {"schema": "slop_ninja_feature_nn_training_v1", "status": "fitting",
                "config": model.config, "seed": args.seed, "epochs_requested": args.epochs,
                "batch_size": args.batch_size, "learning_rate": args.learning_rate,
                "optimizer": "AdamW", "weight_decay": 0.001, "gradient_norm_limit": 1.0,
                "device": "cpu", "cpu_threads": 2, "dtype": "float32", "software": software(),
                "sample_weighting": "source_origin", "formatting_augmentation": False,
                "origin_consistency_loss": False, "archive_counts": archive_counts,
                "selected_counts": {k: len(v) for k, v in rows.items()}, "evaluation_views": views,
                "source_partition_sha256": source_hashes, "export_manifest_sha256": sha256(args.vectors / "manifest.json"),
                "source_code_sha256": {f: sha256(Path(__file__).parent / f) for f in source_files},
                "parameters": sum(p.numel() for p in model.parameters()),
                "selection": "earliest minimum R2 Development human/model binary NLL among epochs with both recalls above zero at probability 0.5",
                "final_test_opened": False, "provider_loss": False}
    write_json(args.output / "training.json", settings)
    history, best_epoch, best_loss = [], None, float("inf")
    started = time.monotonic()
    selected_path = args.output / "model.safetensors"
    for epoch in range(args.epochs):
        order = list(range(len(train_labels)))
        rng.shuffle(order)
        model.train()
        total = 0.0
        for start in range(0, len(order), args.batch_size):
            index = order[start:start + args.batch_size]
            optimizer.zero_grad(set_to_none=True)
            logits = model(tensors["train"][index])
            loss = (torch.nn.functional.cross_entropy(logits, train_labels[index], reduction="none") * weights[index]).mean()
            if not torch.isfinite(loss):
                raise ValueError("nonfinite training loss")
            loss.backward()
            torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0, error_if_nonfinite=True)
            optimizer.step()
            total += float(loss.detach()) * len(index)
        train_logits = predict(model, tensors["train"])
        development_logits = predict(model, tensors["development"])
        selection = human_model_metrics(development_logits, labels["development"])
        eligible = all(r > 0 for r in selection["recall_at_half"])
        entry = {"epoch": epoch + 1, "training_loss": total / len(order),
                 "train": metrics(train_logits, labels["train"]),
                 "train_human_model": human_model_metrics(train_logits, labels["train"]),
                 "development": metrics(development_logits, labels["development"]),
                 "development_human_model": selection, "eligible": eligible,
                 "elapsed_seconds": time.monotonic() - started}
        history.append(entry)
        with (args.output / "epochs.jsonl").open("a") as stream:
            stream.write(json.dumps(entry, allow_nan=False) + "\n")
        print(json.dumps({"epoch": epoch + 1, "train_nll": entry["train_human_model"]["log_loss"],
                          "development_nll": selection["log_loss"], "eligible": eligible}), flush=True)
        if eligible and selection["log_loss"] < best_loss:
            best_epoch, best_loss = epoch + 1, selection["log_loss"]
            save_model(model, str(selected_path))
    settings.update(epoch_history=history, selected_epoch=best_epoch, elapsed_seconds=time.monotonic() - started)
    if best_epoch is None:
        settings["status"] = "no_eligible_epoch"
        write_json(args.output / "training.json", settings)
        write_json(args.output / "completion.json", {"status": "no_eligible_epoch", "classifier_exported": False})
        return
    load_model(model, str(selected_path), strict=True)
    model.eval()
    calibration_logits = predict(model, tensors["calibration"], 1)
    calibration = fit_temperature(calibration_logits, labels["calibration"])
    calibration["before"] = metrics(calibration_logits, labels["calibration"])
    calibration["after"] = metrics(calibration_logits, labels["calibration"], calibration["temperature"])
    # Exercise gradients on Train only. This is numeric evidence, not edit ranking evidence.
    sample = tensors["train"][:1]
    reference_logits, gradients = feature_gradient(model, sample)
    if not torch.allclose(reference_logits.double(), predict(model, sample, 1), atol=1e-6, rtol=0):
        raise ValueError("gradient and ordinary inference disagree")
    for split in ("development", "calibration"):
        logits = predict(model, tensors[split], 1)
        observations = [{"id": row["id"], "source_group": row["source_group"], "origin": row["origin"],
                         "text_sha256": row["hash"], "logits": logit.tolist()}
                        for row, logit in zip(rows[split], logits)]
        write_json(args.output / (split + "-logits.json"), observations)
    settings.update(status="unqualified_research_candidate", input_gradient_finite=True,
                    input_gradient_l2=float(gradients.norm()))
    write_json(args.output / "training.json", settings)
    write_json(args.output / "config.json", model.config)
    write_json(args.output / "calibration.json", calibration)
    references = []
    for values in tensors["train"][:3]:
        references.append({"feature_space_sha256": export["feature_space_sha256"],
                           "vector": {"values": [[i, float(v)] for i, v in enumerate(values) if v != 0],
                                      "unknown_coordinates": 0, "unavailable_families": []}})
    write_json(args.output / "reference_vectors.json", {
        "scope": "numeric replay vectors from Train, not text-level accuracy evidence; diagnostic metadata is not used",
        "rows": references, "logits": predict(model, tensors["train"][:3], 1).tolist()})
    for name in ("feature_space.json", "data_rights.json"):
        shutil.copyfile(args.vectors / name, args.output / name)
    runtime = args.output / "runtime"
    runtime.mkdir()
    for name in source_files:
        shutil.copyfile(Path(__file__).parent / name, runtime / name)
    files = {str(p.relative_to(args.output)): sha256(p) for p in args.output.rglob("*") if p.is_file()}
    write_json(args.output / "manifest.json", {"schema": "slop_ninja_feature_nn_v1", "files": files,
                "labels": LABELS, "feature_space_sha256": export["feature_space_sha256"],
                "parser_identity": export["parser_identity"], "feature_schema": export["feature_schema"],
                "temperature": calibration["temperature"], "status": "unqualified_research_candidate",
                "probability_event": "document model-origin probability, not provider-estimated AI text fraction",
                "dtype": "float32", "reference_device": "cpu", "reference_batch_size": 1})
    load_bundle(args.output)
    write_json(args.output / "completion.json", {"status": "fit_and_calibration_complete", "classifier_exported": True,
                "evaluation_complete": False, "useful_gradients_verified": False})


if __name__ == "__main__":
    main()

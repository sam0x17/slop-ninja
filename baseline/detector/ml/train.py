"""Fine-tune the three-class encoder from separately exported, admitted Rust shards."""

import argparse
from collections import Counter
import hashlib
import json
import math
from pathlib import Path
import random
import re
import sys
import tempfile
import time

import torch
from safetensors.torch import load_model, save_model

from common import (LABELS, batch_tensors, check_package_sources, check_partition_separation, class_counts,
                    configure_cpu, fit_temperature, load_checkpoint, load_partition,
                    logits_for, metrics, package_artifact, sha256, software)


def paired_training_view(original, alternate):
    """Require exact family/label coverage for alternate formatting exposures."""
    by_id = {r["id"]: r for r in alternate}
    if len(by_id) != len(alternate) or set(by_id) != {r["id"] for r in original}:
        raise ValueError("training formatting view must cover each original ID exactly once")
    for row in original:
        other = by_id[row["id"]]
        if any(row[k] != other[k] for k in ["source_group", "label", "evidence"]):
            raise ValueError("training formatting view changed family, label or evidence")
    return by_id


def verify_whitespace_only(original_path, alternate_path):
    whitespace = re.compile(r"[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+")
    originals = {r["id"]: r for r in map(json.loads, original_path.read_text().splitlines())}
    for other in map(json.loads, alternate_path.read_text().splitlines()):
        expected = whitespace.sub(" ", originals[other["id"]]["text"]).strip(" ")
        if other["text"] != expected:
            raise ValueError("training view changes more than the fixed whitespace transformation")


def use_alternate(group, epoch):
    rank = hashlib.sha256(("slop-ninja-format-exposure-v1:" + group).encode()).digest()[0]
    return (rank + epoch) % 2 == 0


def source_origin_weights(rows):
    """Give each source family and origin equal total loss weight within Train."""
    counts = Counter((r["source_group"], r["label"]) for r in rows)
    families = {r["source_group"] for r in rows}
    if len(counts) != 3 * len(families):
        raise ValueError("source-origin weighting requires all three origins per family")
    scale = len(rows) / len(counts)
    return {r["id"]: scale / counts[r["source_group"], r["label"]] for r in rows}


def load_origin_pairs(path, train_path, whitespace_path, rows):
    """Validate the Rust pairing handoff against the actual ML inputs."""
    manifest = json.loads(path.read_text())
    if (manifest.get("schema") != "slop_ninja_origin_pairs_v1"
            or manifest.get("train_sha256") != sha256(train_path)
            or manifest.get("whitespace_sha256") != sha256(whitespace_path)):
        raise ValueError("origin pairs do not bind the training inputs")
    by_id = {row["id"]: row for row in rows}
    pairs = manifest["pairs"]
    if (len(by_id) != len(rows) or set(pairs) != set(by_id)
            or len(set(pairs.values())) != len(rows)
            or set(pairs.values()) != set(by_id)
            or manifest["records"] != len(rows)):
        raise ValueError("origin pairing must be a permutation over all Train rows")
    for anchor, peer in pairs.items():
        left, right = by_id[anchor], by_id[peer]
        if left["source_group"] != right["source_group"] or left["label"] != right["label"]:
            raise ValueError("origin pairing changed family or label")
        if anchor != peer and (left["label"] != 1 or right["label"] != 1):
            raise ValueError("only recorded model-only lineages may change peer ID")
    changed = sum(anchor != peer for anchor, peer in pairs.items())
    if (manifest["model_revision_pairs"] != changed
            or manifest["formatting_only_pairs"] != len(rows) - changed):
        raise ValueError("origin pair counts disagree with the mapping")
    return pairs, manifest


def paired_origin_loss(logits, peer_logits, labels, weights, consistency_weight):
    """Equal two-view supervised exposure plus symmetric three-class JS loss."""
    logp = torch.log_softmax(logits, dim=-1)
    logq = torch.log_softmax(peer_logits, dim=-1)
    logm = torch.logaddexp(logp, logq) - math.log(2.0)
    divergence = 0.5 * ((logp.exp() * (logp - logm)).sum(-1)
                        + (logq.exp() * (logq - logm)).sum(-1))
    supervised = 0.5 * (torch.nn.functional.cross_entropy(logits, labels, reduction="none")
                        + torch.nn.functional.cross_entropy(peer_logits, labels, reduction="none"))
    return ((supervised + consistency_weight * divergence) * weights).mean(), (divergence * weights).mean()


def human_model_metrics(logits, labels):
    """Evaluate complete model drafts against humans; mixed examples are auxiliary."""
    labels = torch.tensor(labels, dtype=torch.long)
    keep = labels != 2
    selected = logits.double()[keep]
    truth = labels[keep]
    counts = [int((truth == i).sum()) for i in range(2)]
    if not all(counts):
        raise ValueError("human/model selection requires both human and model-only examples")
    # Log-sum-exp preserves the existing P(AI) = P(model_only) + P(mixed) score.
    binary_logits = torch.stack((selected[:, 0], torch.logsumexp(selected[:, 1:], dim=1)), dim=1)
    probabilities = torch.softmax(binary_logits, dim=1)
    predicted = (probabilities[:, 1] > 0.5).long()
    confusion = [[int(((truth == i) & (predicted == j)).sum()) for j in range(2)] for i in range(2)]
    return {"count": len(truth), "class_counts": counts, "excluded_mixed_rows": int((~keep).sum()),
            "log_loss": float(torch.nn.functional.cross_entropy(binary_logits, truth)),
            "binary_brier": float(((probabilities[:, 1] - truth) ** 2).mean()),
            "accuracy_at_half": float((truth == predicted).double().mean()),
            "confusion_true_rows_predicted_columns": confusion,
            "recall_at_half": [confusion[i][i] / counts[i] for i in range(2)]}


def validate_data_rights(path, shard_paths, partitions):
    """Check the Rust admission handoff before fitting; never infer data rights here."""
    required = {(row["id"], row["hash"]) for rows in partitions.values()
                for row in rows if row["share_alike"]}
    if path is None:
        if required:
            raise ValueError("ShareAlike training requires Rust-exported attribution and the declared release license")
        return None
    manifest = json.loads(path.read_text())
    content = manifest["content"]
    encoded = json.dumps(content, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False).encode()
    if content["schema"] != "slop_ninja_data_release_obligations_v1" or hashlib.sha256(encoded).hexdigest() != manifest["content_sha256"]:
        raise ValueError("invalid data-rights manifest")
    for name, shard in shard_paths.items():
        if content["partitions"][name]["sha256"] != sha256(shard):
            raise ValueError("data rights do not bind the actual training/development/calibration shards")
    observed = {(row["record_id"], row["text_sha256"]) for row in content["share_alike_notices"]}
    if required and (content["model_release_license"] != "CC-BY-SA-4.0" or not required.issubset(observed)):
        raise ValueError("ShareAlike manifest is missing the release license or a source notice")
    return manifest


def evaluation_view(path, shard, split, rows):
    """Select a Rust-validated trio per family after checking the full ML shard."""
    view_bytes = path.read_bytes()
    view = json.loads(view_bytes)
    if (split not in {"development", "calibration"}
            or view.get("schema") != "slop_ninja_origin_evaluation_view_v1"
            or view.get("split") != split or view.get("records_sha256") != sha256(shard)):
        raise ValueError("evaluation view does not bind this full Development/Calibration shard")
    by_id = {row["id"]: row for row in rows}
    if len(by_id) != len(rows):
        raise ValueError("duplicate record ID in the full evaluation archive")
    families = view["families"]
    groups = [family["source_group"] for family in families]
    if groups != sorted({row["source_group"] for row in rows}):
        raise ValueError("evaluation view must select every source family exactly once in canonical order")
    selected = []
    seen = set()
    for family in families:
        for label, role in enumerate(("human", "model", "mixed")):
            binding = family[role]
            row = by_id.get(binding["id"])
            if (row is None or row["id"] in seen or row["label"] != label
                    or row["source_group"] != family["source_group"]
                    or row["hash"] != binding["text_sha256"]):
                raise ValueError("evaluation view ID, origin, family or text differs from the full shard")
            seen.add(row["id"])
            selected.append(row)
    if not selected:
        raise ValueError("empty evaluation view")
    return selected, {"sha256": hashlib.sha256(view_bytes).hexdigest(), "records_sha256": view["records_sha256"],
                      "split": split, "selected_rows": len(selected), "source_groups": len(groups)}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--checkpoint", required=True, type=Path)
    p.add_argument("--data-rights", type=Path, help="Rust-exported attribution and release obligations")
    for name in ["train", "development", "calibration"]:
        p.add_argument(f"--{name}-jsonl", required=True, type=Path)
    for name in ["development", "calibration"]:
        p.add_argument(f"--{name}-view", type=Path,
                       help="Frozen Rust trio selection over the complete ancestry shard; use both views together")
    p.add_argument("--output", required=True, type=Path)
    p.add_argument("--device", choices=["cpu", "mps", "cuda"], default="cpu")
    p.add_argument("--max-tokens", type=int, default=1024)
    p.add_argument("--batch-size", type=int, default=4)
    p.add_argument("--epochs", type=int, default=3)
    p.add_argument("--learning-rate", type=float, default=2e-5)
    p.add_argument("--weight-decay", type=float, default=0.01)
    p.add_argument("--class-weights", choices=["none", "inverse_frequency"], default="none")
    p.add_argument("--sample-weighting", choices=["none", "source_origin"], default="none")
    p.add_argument("--require-class-coverage", action="store_true",
                   help="Require nonzero Development recall for each class in the selected objective")
    p.add_argument("--selection-objective", choices=["three_class", "human_model"], default="three_class",
                   help="Choose uncalibrated Development three-class NLL or human-versus-model-draft binary NLL")
    p.add_argument("--seed", type=int, default=17)
    p.add_argument("--freeze-encoder", action="store_true", help="Train the classification layers only as a control")
    p.add_argument("--training-whitespace-view", type=Path,
                   help="Paired Rust whitespace-derived Train shard; alternate raw/collapsed exposure by family and epoch")
    p.add_argument("--training-origin-pairs", type=Path,
                   help="Rust Train-lineage permutation; pair each anchor with its peer in the opposite formatting view")
    p.add_argument("--origin-consistency-weight", type=float, default=0.0,
                   help="Three-class Jensen-Shannon penalty; zero retains the identical two-view exposure control")
    p.add_argument("--synthetic-smoke-only", action="store_true", help="Mark output as an unvalidated synthetic control")
    args = p.parse_args()
    if args.output.exists() or (args.output.parent / (args.output.name + "-no-eligible-epoch.json")).exists():
        p.error("output already exists; use a new immutable run directory")
    if min(args.batch_size, args.epochs, args.max_tokens) < 1 or args.max_tokens > 8192:
        p.error("positive batch/epoch/token limits required; max tokens cannot exceed 8192")
    if args.learning_rate <= 0 or args.weight_decay < 0:
        p.error("invalid optimizer parameters")
    if args.sample_weighting != "none" and args.class_weights != "none":
        p.error("choose source-origin sample weights or class weights, not both")
    if bool(args.development_view) != bool(args.calibration_view):
        p.error("provide both --development-view and --calibration-view, or neither")
    if not math.isfinite(args.origin_consistency_weight) or args.origin_consistency_weight < 0:
        p.error("origin consistency weight must be finite and nonnegative")
    if args.training_origin_pairs and (not args.training_whitespace_view or args.sample_weighting != "source_origin"):
        p.error("origin pairing requires a whitespace view and source-origin weighting")
    if args.origin_consistency_weight and not args.training_origin_pairs:
        p.error("origin consistency requires --training-origin-pairs")
    if args.device == "mps" and not torch.backends.mps.is_available():
        p.error("MPS is unavailable")
    if args.device == "cuda" and not torch.cuda.is_available():
        p.error("CUDA is unavailable; choose --device cpu or install a compatible CUDA runtime")
    check_package_sources(args.checkpoint)
    configure_cpu()
    torch.manual_seed(args.seed)
    rng = random.Random(args.seed)
    tokenizer, model = load_checkpoint(args.checkpoint, args.seed)
    partitions = {name: load_partition(getattr(args, f"{name}_jsonl"), name, tokenizer, args.max_tokens)
                  for name in ["train", "development", "calibration"]}
    check_partition_separation(partitions)
    data_rights = validate_data_rights(args.data_rights,
        {name: getattr(args, f"{name}_jsonl") for name in partitions}, partitions)
    alternate = None
    if args.training_whitespace_view:
        alternate_rows = load_partition(args.training_whitespace_view, "train", tokenizer, args.max_tokens)
        alternate = paired_training_view(partitions["train"], alternate_rows)
        verify_whitespace_only(args.train_jsonl, args.training_whitespace_view)
        check_partition_separation({**partitions, "train": partitions["train"] + alternate_rows})
    pairs, pair_manifest = None, None
    raw_training = {r["id"]: r for r in partitions["train"]}
    if args.training_origin_pairs:
        pairs, pair_manifest = load_origin_pairs(args.training_origin_pairs, args.train_jsonl,
                                                args.training_whitespace_view, partitions["train"])
    if any(row["evidence"] == "synthetic_fixture" for rows in partitions.values() for row in rows) and not args.synthetic_smoke_only:
        raise ValueError("synthetic fixtures require --synthetic-smoke-only")
    archive_counts = {name: class_counts(rows) for name, rows in partitions.items()}
    views = {}
    if args.development_view:
        # Admission, rights, split separation and token limits cover every ancestor.
        # Only the declared observations enter epoch selection and temperature fit.
        for name in ["development", "calibration"]:
            partitions[name], views[name] = evaluation_view(
                getattr(args, f"{name}_view"), getattr(args, f"{name}_jsonl"), name, partitions[name])
    counts = {name: class_counts(rows) for name, rows in partitions.items()}
    sample_weights = source_origin_weights(partitions["train"]) if args.sample_weighting == "source_origin" else None
    if any(n == 0 for values in counts.values() for n in values):
        raise ValueError("all three classes must occur in training, development and calibration")
    if args.freeze_encoder:
        for parameter in model.base_model.parameters():
            parameter.requires_grad_(False)
    model.to(args.device)
    parameters = [parameter for parameter in model.parameters() if parameter.requires_grad]
    optimizer = torch.optim.AdamW(parameters, lr=args.learning_rate, weight_decay=args.weight_decay)
    weights = None
    if args.class_weights == "inverse_frequency":
        weights = torch.tensor([sum(counts["train"]) / (3 * n) for n in counts["train"]], device=args.device)
    epochs = []
    best_loss = float("inf")
    best_epoch = None
    started = time.monotonic()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".encoder-selection-", dir=args.output.parent) as temporary:
        selected = Path(temporary) / "selected.safetensors"
        for epoch in range(1, args.epochs + 1):
            order = list(partitions["train"])
            if alternate:
                order = [alternate[r["id"]] if use_alternate(r["source_group"], epoch) else r for r in order]
            rng.shuffle(order)
            model.train()
            loss_total = 0.0
            divergence_total = 0.0
            epoch_start = time.monotonic()
            for start in range(0, len(order), args.batch_size):
                rows = order[start:start + args.batch_size]
                inputs = batch_tensors(rows, tokenizer, args.device)
                labels = torch.tensor([r["label"] for r in rows], device=args.device)
                optimizer.zero_grad(set_to_none=True)
                logits = model(**inputs).logits
                if pairs is not None:
                    peers = [(raw_training if use_alternate(r["source_group"], epoch) else alternate)[pairs[r["id"]]]
                             for r in rows]
                    peer_logits = model(**batch_tensors(peers, tokenizer, args.device)).logits
                    batch_weights = torch.tensor([sample_weights[r["id"]] for r in rows], device=args.device)
                    loss, divergence = paired_origin_loss(logits, peer_logits, labels, batch_weights,
                                                         args.origin_consistency_weight)
                    divergence_total += float(divergence.detach().cpu()) * len(rows)
                elif sample_weights is None:
                    loss = torch.nn.functional.cross_entropy(logits, labels, weight=weights)
                else:
                    per_row = torch.nn.functional.cross_entropy(logits, labels, reduction="none")
                    batch_weights = torch.tensor([sample_weights[r["id"]] for r in rows], device=args.device)
                    loss = (per_row * batch_weights).mean()
                if not torch.isfinite(loss):
                    raise ValueError("non-finite training loss")
                loss.backward()
                torch.nn.utils.clip_grad_norm_(parameters, 1.0, error_if_nonfinite=True)
                optimizer.step()
                loss_total += float(loss.detach().cpu()) * len(rows)
                if pairs is not None and (start // args.batch_size + 1) % 16 == 0:
                    print(json.dumps({"event": "origin_pair_training_progress", "epoch": epoch,
                                      "rows_completed": start + len(rows), "rows_total": len(order),
                                      "mean_loss": loss_total / (start + len(rows)),
                                      "elapsed_seconds": time.monotonic() - epoch_start}), file=sys.stderr, flush=True)
            development_logits = logits_for(model, tokenizer, partitions["development"], args.batch_size, args.device)
            development = metrics(development_logits, [r["label"] for r in partitions["development"]])
            summary = {"epoch": epoch, "training_loss": loss_total / len(order), "development": development,
                       "elapsed_seconds": time.monotonic() - epoch_start}
            if args.selection_objective == "human_model":
                selection_metrics = human_model_metrics(development_logits, [r["label"] for r in partitions["development"]])
                summary["development_human_model"] = selection_metrics
                eligible = not args.require_class_coverage or all(recall > 0 for recall in selection_metrics["recall_at_half"])
                selection_loss = selection_metrics["log_loss"]
            else:
                eligible = not args.require_class_coverage or all(development["per_class"][label]["recall"] > 0 for label in LABELS)
                selection_loss = development["log_loss"]
            summary["selection_objective"] = args.selection_objective
            summary["selection_loss"] = selection_loss
            summary["selection_eligible"] = eligible
            if alternate:
                summary["whitespace_view_rows"] = sum(use_alternate(r["source_group"], epoch) for r in order)
            if pairs is not None:
                summary["origin_pair_js_divergence"] = divergence_total / len(order)
                summary["training_forward_rows"] = 2 * len(order)
            epochs.append(summary)
            print(json.dumps(summary), flush=True)
            if eligible and selection_loss < best_loss:
                best_loss = selection_loss
                best_epoch = epoch
                save_model(model, selected)
        # Model choice is complete before calibration. Final-test input has no CLI argument here.
        if best_epoch is None:
            failure_path = args.output.parent / (args.output.name + "-no-eligible-epoch.json")
            failure_path.write_text(json.dumps({"status":"no_eligible_epoch","epoch_history":epochs,
                "criterion":"nonzero Development recall for every class in the selected objective; then minimum NLL",
                "selection_objective":args.selection_objective,
                "final_test_opened":False}, indent=2) + "\n")
            raise ValueError(f"no epoch met class coverage; diagnostic retained at {failure_path}")
        load_model(model, selected, strict=True, device=args.device)
        model.cpu().eval()
        calibration_logits = logits_for(model, tokenizer, partitions["calibration"], 1, "cpu")
        calibration_labels = [r["label"] for r in partitions["calibration"]]
        calibration = fit_temperature(calibration_logits, calibration_labels)
        calibration["before"] = metrics(calibration_logits, calibration_labels)
        calibration["after"] = metrics(calibration_logits, calibration_labels, calibration["temperature"])
        training = {
            "status": "synthetic_smoke_only_not_a_detector_release" if args.synthetic_smoke_only else "unqualified_research_candidate",
            "checkpoint_pin_sha256": sha256(args.checkpoint / "checkpoint.json"),
            "software": software(), "device": args.device, "dtype": "float32", "seed": args.seed,
            "optimizer": "AdamW", "learning_rate": args.learning_rate, "weight_decay": args.weight_decay,
            "gradient_norm_limit": 1.0, "batch_size": args.batch_size, "epochs_requested": args.epochs,
            "calibration_batch_size": 1,
            "class_weight_policy": args.class_weights, "class_weights": weights.cpu().tolist() if weights is not None else None,
            "sample_weight_policy": args.sample_weighting,
            "sample_weights": {"min":min(sample_weights.values()),"max":max(sample_weights.values()),
                               "sum":sum(sample_weights.values()),"normalization":"sum equals Train row count; each family-origin has equal total weight"} if sample_weights else None,
            "freeze_encoder": args.freeze_encoder, "trainable_parameters": sum(p.numel() for p in parameters),
            "parameters": sum(p.numel() for p in model.parameters()), "class_counts": counts,
            "source_group_counts": {name: len({r["source_group"] for r in rows}) for name, rows in partitions.items()},
            "evidence_counts": {name: {evidence: sum(r["evidence"] == evidence for r in rows)
                                       for evidence in sorted({r["evidence"] for r in rows})}
                                for name, rows in partitions.items()},
            "partition_sha256": {name: sha256(getattr(args, f"{name}_jsonl")) for name in partitions},
            "selection_metric": "development_unweighted_log_loss" if args.selection_objective == "three_class" else "development_human_model_binary_log_loss",
            "selection_objective":args.selection_objective,"selected_epoch": best_epoch,
            "unrestricted_best_development_epoch": min(epochs, key=lambda e: e["selection_loss"])["epoch"],
            "requires_nonzero_development_class_recall":args.require_class_coverage and args.selection_objective == "three_class",
            "requires_nonzero_development_binary_recall":args.require_class_coverage and args.selection_objective == "human_model",
            "epoch_history": epochs, "elapsed_seconds": time.monotonic() - started,
            "final_test_opened": False,
        }
        if alternate:
            training["formatting_augmentation"] = {
                "method": "slop-ninja-format-exposure-v1",
                "alternate_train_sha256": sha256(args.training_whitespace_view),
                "policy": "one exposure per row per epoch; alternate raw/collapsed by source-family hash parity and epoch; all siblings share exposure",
                "raw_rows": len(partitions["train"]), "alternate_rows": len(alternate),
                "development_and_calibration": "unchanged original text",
            }
            if pairs is not None:
                training["formatting_augmentation"]["policy"] = (
                    "Anchor view follows the original source-family parity schedule; peer uses the opposite "
                    "view. The bijective within-origin/family pairing preserves total supervised weight per record.")
        if pairs is not None:
            training["origin_pairing"] = {
                "manifest_sha256": sha256(args.training_origin_pairs),
                "manifest": pair_manifest,
                "consistency_weight": args.origin_consistency_weight,
                "loss": "mean source-origin-weighted (0.5*(anchor CE + peer CE) + weight*three-class Jensen-Shannon divergence)",
                "forward_passes_per_step": 2,
                "dropout": "Independent training-mode draws on each view; both branches receive gradients",
                "new_source_records": 0,
            }
        if args.data_rights:
            training["data_rights_sha256"] = sha256(args.data_rights)
        if views:
            training["evaluation_views"] = views
            training["archive_class_counts"] = archive_counts
            training["evaluation_view_policy"] = (
                "Train exposes all admitted stages with the declared weighting; Development and Calibration "
                "use one frozen human/model/mixed trio per family. Full ancestry shards retain admission, "
                "rights, split-separation and token-limit checks. No Test input enters fitting.")
        manifest = package_artifact(args.output, tokenizer, model, calibration, training, args.max_tokens, args.checkpoint, data_rights)
        print(json.dumps({"artifact": str(args.output), "artifact_id": manifest["artifact_id"], "status": training["status"]}))


if __name__ == "__main__":
    main()

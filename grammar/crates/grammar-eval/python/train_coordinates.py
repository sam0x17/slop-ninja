"""Learn supported coordinate multipliers around frozen family-level scorers."""
import argparse
import copy
import json
import math
import statistics
import time
import traceback
from pathlib import Path

import numpy as np
import torch

from coordinate_io import IDENTITY, load_export, load_inputs_manifest, validate_binding
from coordinate_model import LAMBDA_GRID, SEEDS, CoordinateMetric
from frontier_common import demand, read_json, sha256, write_json
from train_frontier import configure


METRICS = ["top1", "top5", "mrr"]
SOURCES = ["train_coordinates.py", "coordinate_io.py", "coordinate_model.py", "frontier_common.py",
           "frontier_models.py", "train_frontier.py", "training-lock.json"]


def validate_protocol(protocol):
    demand(protocol["schema"] == "slopninja-coordinate-weighting-protocol-v1", "unsupported coordinate protocol")
    demand(protocol["baseline_model"] == "combined_spline" and protocol["inherited_seeds"] == SEEDS, "frozen baselines differ")
    expected = {"lambda_grid": LAMBDA_GRID, "epochs": 200, "learning_rate": 0.03, "beta1": 0.9,
                "beta2": 0.999, "epsilon": 1e-8, "eta_initial": 0, "weight_bounds": [0.5, 2],
                "penalty": "lambda times mean over nonempty selected families of mean eta squared within family",
                "device": "cpu", "dtype": "float64", "threads": 8, "deterministic": True}
    demand(protocol["optimization"] == expected, "coordinate optimization differs from frozen design")
    for key, value in {"min_authors": 10, "min_posts": 30, "word_cap": 512, "bigram_cap": 512,
                       "categorical_grammar_cap_per_family": 96, "unselected_multiplier": 1}.items():
        demand(protocol["coordinate_selection"][key] == value, f"coordinate selection differs: {key}")
    demand(protocol["selection"]["checkpoint"] == "macro_author.top1, then macro_author.mrr, then earliest epoch including 0",
           "checkpoint selection differs")
    demand(protocol["selection"]["lambda"] == "mean selected seed macro_author.top1, then mean MRR, then stronger lambda",
           "regularization selection differs")
    demand(len(protocol["baseline_checkpoints"]) == 3 and sorted(row["seed"] for row in protocol["baseline_checkpoints"]) == SEEDS,
           "baseline checkpoint catalog differs")
    demand(len(protocol["cohorts"]["test"]) == 3, "three fixed test galleries required")


def validate_baselines(baseline_fit, protocol):
    demand(sha256(baseline_fit / "selection.json") == protocol["baseline_selection_sha256"], "baseline model selection changed")
    selection = read_json(baseline_fit / "selection.json")
    demand(selection["schema"] == "slopninja-frozen-author-selection-v1" and selection["selected_architectures"]["combined"] == "combined_spline",
           "unexpected source selected model")
    for key in IDENTITY:
        demand(selection[key] == protocol[key], f"baseline identity differs: {key}")
    for filename, key in [("calibrations.json", "calibrations_sha256"), ("implementation.json", "implementation_sha256")]:
        demand(sha256(baseline_fit / filename) == selection[key], f"baseline source artifact changed: {filename}")
    implementation = read_json(baseline_fit / "implementation.json")
    for filename, digest in implementation["source_sha256"].items():
        demand(sha256(Path(__file__).parent / filename) == digest, f"frozen baseline source changed: {filename}")
    calibration = read_json(baseline_fit / "calibrations.json")["subsets"]["combined"]
    demand(calibration["family_indices"] == list(range(14)) and calibration["families"] == selection["families"],
           "baseline families are reordered or truncated")
    output = {}
    for binding in protocol["baseline_checkpoints"]:
        matches = [row for row in selection["runs"] if row["config"]["name"] == "combined_spline" and row["seed"] == binding["seed"]]
        demand(len(matches) == 1 and matches[0]["status"] == "complete", "baseline fit missing or duplicated")
        run = matches[0]
        for key in ["selected_epoch", "selected_checkpoint", "selected_checkpoint_sha256"]:
            demand(run[key] == binding[key], f"baseline checkpoint binding differs: {key}")
        path = baseline_fit / "combined_spline" / f"seed-{binding['seed']}" / binding["selected_checkpoint"]
        demand(path.resolve().is_relative_to(baseline_fit.resolve()) and sha256(path) == binding["selected_checkpoint_sha256"], "baseline checkpoint changed")
        stored = torch.load(path, weights_only=True)
        demand(stored["schema"] == "slopninja-author-selection-checkpoint-v1" and stored["config"] == run["config"], "baseline checkpoint architecture differs")
        demand(stored["seed"] == binding["seed"] and stored["epoch"] == binding["selected_epoch"], "baseline checkpoint seed/epoch differs")
        state = stored["model_state"]
        demand(torch.equal(state["family_indices"], torch.arange(14)), "baseline coordinate family ordering differs")
        demand(state["theta"].shape == (14,) and torch.equal(state["knots"], torch.tensor(calibration["knots"], dtype=torch.float64)), "baseline spline identity differs")
        demand(stored["validation"] == run["selected_validation"], "baseline validation record differs")
        output[binding["seed"]] = {"state": state, "run": run, "sha256": binding["selected_checkpoint_sha256"]}
    return selection, output


def metric_rows(logits, labels):
    logits = np.asarray(logits)
    demand(np.isfinite(logits).all(), "nonfinite candidate logits")
    own = logits[np.arange(len(logits)), labels]
    better = (logits > own[:, None]).sum(1)
    tied = (logits == own[:, None]).sum(1)
    first, last = better + 1, better + tied
    harmonic = np.concatenate(([0.0], np.cumsum(1 / np.arange(1, logits.shape[1] + 1))))
    return np.stack((np.maximum(np.minimum(1, last) + 1 - first, 0) / tied,
                     np.maximum(np.minimum(5, last) + 1 - first, 0) / tied,
                     (harmonic[last] - harmonic[first - 1]) / tied), 1)


def summarize_rows(rows, authors):
    demand(len(rows) == len(authors) and len(rows) > 0, "empty or mismatched author metric rows")
    groups = {}
    for author, values in zip(authors, rows):
        groups.setdefault(author, []).append(values)
    per_author = {author: dict(zip(METRICS, np.mean(values, axis=0).tolist())) for author, values in sorted(groups.items())}
    return {"query_count": len(rows), "author_count": len(per_author),
            "micro": dict(zip(METRICS, np.mean(rows, axis=0).tolist())),
            "macro_author": {metric: statistics.mean(row[metric] for row in per_author.values()) for metric in METRICS},
            "per_author": per_author}


def score(model, data, baseline=False):
    with torch.no_grad():
        logits = (model.score_distances(data["inputs"].raw_distances) if baseline else model(data["inputs"])).numpy()
    rows = metric_rows(logits, data["labels"])
    return summarize_rows(rows, data["query_authors"]), logits, rows


def loss_components(model, data, regularization):
    inputs = data["inputs"]
    logits = model(inputs)
    cross_entropy = (torch.nn.functional.cross_entropy(logits, inputs.own_indices, reduction="none") * inputs.loss_weights).sum()
    penalty = model.penalty()
    return cross_entropy + regularization * penalty, cross_entropy, penalty


def baseline_identity(training, validation, baselines):
    audit = {}
    for seed in SEEDS:
        model = CoordinateMetric(training["inputs"].family_indices, baselines[seed]["state"])
        demand(torch.equal(model.adjusted_distances(training["inputs"]), training["inputs"].raw_distances), "eta-zero train geometry differs")
        demand(torch.equal(model.adjusted_distances(validation["inputs"]), validation["inputs"].raw_distances), "eta-zero validation geometry differs")
        metrics, _, _ = score(model, validation)
        expected = baselines[seed]["run"]["selected_validation"]
        demand(metrics["per_author"].keys() == expected["per_author"].keys(), "baseline validation author set differs")
        error = max(abs(metrics["per_author"][author][key] - expected["per_author"][author][key])
                    for author in metrics["per_author"] for key in METRICS)
        demand(error < 1e-12, "eta-zero validation ranks differ from frozen baseline")
        with torch.no_grad():
            loss = loss_components(model, training, 0)[1].item()
        loss_error = abs(loss - baselines[seed]["run"]["selected_training_cross_entropy"])
        demand(loss_error < 1e-9, "eta-zero training loss differs from frozen baseline")
        audit[str(seed)] = {"training_cross_entropy": loss, "training_loss_error": loss_error,
                            "maximum_validation_metric_error": error, "validation": metrics}
    return audit


def lambda_name(value):
    return "lambda-" + format(value, "g").replace(".", "p")


def train_fit(regularization, seed, training, validation, baseline, catalog_hash, out):
    out.mkdir()
    (out / "checkpoints").mkdir()
    model = CoordinateMetric(training["inputs"].family_indices, baseline["state"])
    optimizer = torch.optim.Adam(model.parameters(), lr=0.03, betas=(0.9, 0.999), eps=1e-8, foreach=False, fused=False)
    best, best_score = None, (-math.inf, -math.inf)
    began = time.perf_counter()
    with (out / "epochs.jsonl").open("w") as history:
        for epoch in range(201):
            optimizer.zero_grad(set_to_none=True)
            objective, cross_entropy, penalty = loss_components(model, training, regularization)
            demand(bool(torch.isfinite(objective)), "nonfinite coordinate objective")
            objective.backward()
            demand(bool(torch.isfinite(model.eta.grad).all()), "nonfinite coordinate gradient")
            metrics, _, _ = score(model, validation)
            score_key = (metrics["macro_author"]["top1"], metrics["macro_author"]["mrr"])
            checkpoint = out / "checkpoints" / f"epoch-{epoch:03}.pt"
            torch.save({"schema": "slopninja-coordinate-checkpoint-v1", "lambda": regularization,
                        "inherited_seed": seed, "epoch": epoch, "eta": model.eta.detach().clone(),
                        "optimizer_state": optimizer.state_dict(), "baseline_checkpoint_sha256": baseline["sha256"],
                        "catalog_sha256": catalog_hash, "training_cross_entropy": cross_entropy.item(),
                        "penalty": penalty.item(), "objective": objective.item(), "validation": metrics}, checkpoint)
            record = {"epoch": epoch, "lambda": regularization, "inherited_seed": seed,
                      "training_cross_entropy": cross_entropy.item(), "unscaled_penalty": penalty.item(),
                      "objective": objective.item(), "validation": metrics, "eta": model.eta.detach().tolist(),
                      "gradient_l2": model.eta.grad.norm().item(), "checkpoint": str(checkpoint.relative_to(out)),
                      "checkpoint_sha256": sha256(checkpoint), "elapsed_seconds": time.perf_counter() - began}
            history.write(json.dumps(record, allow_nan=False) + "\n")
            history.flush()
            if score_key > best_score:
                best_score, best = score_key, copy.deepcopy(record)
            if epoch % 50 == 0:
                print(f"lambda {regularization:g} seed {seed} epoch {epoch}: CE {cross_entropy.item():.6f}, validation top1 {score_key[0]:.6f}", flush=True)
            if epoch < 200:
                optimizer.step()
                model.constrain()
    demand(best is not None, "no coordinate checkpoint")
    stored = torch.load(out / best["checkpoint"], weights_only=True)
    with torch.no_grad():
        model.eta.copy_(stored["eta"])
    demand(score(model, validation)[0] == best["validation"], "coordinate selected checkpoint replay differs")
    result = {"status": "complete", "lambda": regularization, "inherited_seed": seed,
              "actual_trainable_parameters": len(model.eta), "selected_epoch": best["epoch"],
              "selected_checkpoint": best["checkpoint"], "selected_checkpoint_sha256": best["checkpoint_sha256"],
              "selected_validation": best["validation"], "selected_eta": best["eta"],
              "baseline_checkpoint_sha256": baseline["sha256"], "catalog_sha256": catalog_hash,
              "selected_training_cross_entropy": best["training_cross_entropy"], "selected_objective": best["objective"],
              "epochs_sha256": sha256(out / "epochs.jsonl"), "elapsed_seconds": time.perf_counter() - began}
    write_json(out / "summary.json", result)
    return result


def choose_lambda(runs):
    expected = {(value, seed) for value in LAMBDA_GRID for seed in SEEDS}
    demand(len(runs) == 12 and {(row["lambda"], row["inherited_seed"]) for row in runs} == expected and
           all(row["status"] == "complete" for row in runs), "all twelve prescribed fits must complete before selection")
    candidates = []
    for value in LAMBDA_GRID:
        seeds = [row for row in runs if row["lambda"] == value]
        means = {metric: statistics.mean(row["selected_validation"]["macro_author"][metric] for row in seeds) for metric in METRICS}
        candidates.append({"lambda": value, "mean_seed_validation": means})
    candidates.sort(key=lambda row: (-row["mean_seed_validation"]["top1"], -row["mean_seed_validation"]["mrr"], -row["lambda"]))
    return candidates[0]["lambda"], candidates


def train(args):
    demand(not args.out.exists(), "coordinate training output already exists")
    protocol = read_json(args.protocol)
    validate_protocol(protocol)
    protocol_hash = sha256(args.protocol)
    inputs = load_inputs_manifest(args.inputs, protocol_hash, ["train", "validation"])
    base_selection, baselines = validate_baselines(args.baseline_fit, protocol)
    training = load_export(args.training_export, inputs["exports"]["train"]["manifest_sha256"], inputs["catalog_sha256"], protocol, "train")
    validation = load_export(args.validation_export, inputs["exports"]["validation"]["manifest_sha256"], inputs["catalog_sha256"], protocol, "dev", training["catalog"])
    validate_binding(training, inputs["exports"]["train"], protocol["cohorts"]["training"], protocol["reference_space_sha256"])
    validate_binding(validation, inputs["exports"]["validation"], protocol["cohorts"]["validation"], protocol["reference_space_sha256"])
    demand(inputs["validation_eligibility_sha256"] == protocol["cohorts"]["validation"]["eligibility_sha256"], "validation eligibility binding differs")
    demand(training["authors"] == base_selection["training_author_ids"], "original training author gallery differs")
    demand(validation["authors"] == base_selection["selection_author_ids"], "reused validation author gallery differs")
    demand(set(training["authors"]).isdisjoint(validation["authors"]), "training and validation author overlap")
    identity = baseline_identity(training, validation, baselines)
    args.out.mkdir(parents=True)
    write_json(args.out / "protocol.json", protocol)
    write_json(args.out / "inputs.json", inputs)
    write_json(args.out / "selected-coordinates.json", training["catalog"])
    source = Path(__file__).parent
    manifest = {"schema": "slopninja-coordinate-training-implementation-v1", "protocol_sha256": protocol_hash,
                "inputs_sha256": sha256(args.inputs), "catalog_sha256": inputs["catalog_sha256"],
                "selected_coordinates_sha256": sha256(args.out / "selected-coordinates.json"),
                "coordinate_count": len(training["catalog"]["coordinates"]),
                "per_family_coordinate_counts": {name: sum(row["family"] == name for row in training["catalog"]["coordinates"])
                                                 for name in training["catalog"]["families"]},
                "training_author_ids": training["authors"], "validation_author_ids": validation["authors"],
                "training_query_count": len(training["rows"]), "validation_query_count": len(validation["rows"]),
                "source_sha256": {name: sha256(source / name) for name in SOURCES},
                "baseline_identity": identity, "training_residual_audit": training["residual_audit"],
                "validation_residual_audit": validation["residual_audit"], "test_exports_opened": False,
                "baseline_selection_sha256": protocol["baseline_selection_sha256"]}
    write_json(args.out / "implementation.json", manifest)
    runs = []
    for value in LAMBDA_GRID:
        directory = args.out / lambda_name(value)
        directory.mkdir()
        for seed in SEEDS:
            destination = directory / f"seed-{seed}"
            try:
                result = train_fit(value, seed, training, validation, baselines[seed], inputs["catalog_sha256"], destination)
            except Exception as error:
                destination.mkdir(exist_ok=True)
                failure = {"status": "failed", "lambda": value, "inherited_seed": seed,
                           "error": str(error), "traceback": traceback.format_exc()}
                write_json(destination / "failure.json", failure)
                runs.append(failure)
                write_json(args.out / "progress.json", {"status": "stopped_after_failed_fit", "runs": runs})
                raise
            runs.append(result)
            write_json(args.out / "progress.json", {"completed_fits": len(runs), "planned_fits": 12, "runs": runs})
    selected, candidates = choose_lambda(runs)
    result = {"schema": "slopninja-frozen-coordinate-weights-v1", "selected_lambda": selected,
              "lambda_candidates": candidates, "runs": runs, "inherited_seeds": SEEDS,
              "protocol_sha256": protocol_hash, "inputs_sha256": sha256(args.inputs),
              "implementation_sha256": sha256(args.out / "implementation.json"),
              "catalog_sha256": inputs["catalog_sha256"], "selected_coordinates_sha256": sha256(args.out / "selected-coordinates.json"),
              "baseline_selection_sha256": protocol["baseline_selection_sha256"],
              **{key: protocol[key] for key in IDENTITY}, "completed_fits": 12, "failed_fits": 0}
    write_json(args.out / "selection.json", result)
    weights = []
    chosen = {row["inherited_seed"]: row for row in runs if row["lambda"] == selected}
    for index, coordinate in enumerate(training["catalog"]["coordinates"]):
        weights.append({**coordinate, "weights_by_inherited_seed": {str(seed): math.exp(chosen[seed]["selected_eta"][index]) for seed in SEEDS}})
    write_json(args.out / "coordinate-weights.json", {"selected_lambda": selected, "coordinates": weights})
    print(f"FROZEN coordinate lambda {selected:g}; selection SHA256 {sha256(args.out / 'selection.json')}", flush=True)


def paired_interval(left, right):
    demand(left.keys() == right.keys() and left, "paired coordinate gallery catalog differs")
    galleries = []
    for name in sorted(left):
        demand(left[name].keys() == right[name].keys() and left[name], "paired coordinate author catalog differs")
        rows = np.array([[left[name][author][metric] - right[name][author][metric] for metric in METRICS] for author in sorted(left[name])])
        galleries.append((name, rows))
    total = sum(len(rows) for _, rows in galleries)
    point = np.concatenate([rows for _, rows in galleries]).mean(0)
    state, mask, samples = 0x6A09E667F3BCC909, (1 << 64) - 1, []
    for _ in range(2000):
        value = np.zeros(3)
        for _, rows in galleries:
            for _ in rows:
                state ^= (state << 13) & mask
                state ^= state >> 7
                state ^= (state << 17) & mask
                value += rows[state % len(rows)]
        samples.append(value / total)
    ordered = np.sort(samples, 0)
    return {"author_count": total, "gallery_order": [name for name, _ in galleries],
            "gallery_author_counts": {name: len(rows) for name, rows in galleries},
            "gallery_mean_differences": {name: dict(zip(METRICS, rows.mean(0).tolist())) for name, rows in galleries},
            "replicates": 2000, "seed": "0x6a09e667f3bcc909",
            "metrics": {metric: {"mean_difference": point[index].item(), "percentile_95": [ordered[49, index].item(), ordered[1949, index].item()]}
                        for index, metric in enumerate(METRICS)}}


def pool(summaries):
    rows = {}
    for summary in summaries.values():
        demand(rows.keys().isdisjoint(summary["per_author"]), "author occurs in multiple test galleries")
        rows.update(summary["per_author"])
    total_queries = sum(row["query_count"] for row in summaries.values())
    return {"query_count": total_queries, "author_count": len(rows), "per_author": rows,
            "macro_author": {metric: statistics.mean(row[metric] for row in rows.values()) for metric in METRICS},
            "micro": {metric: sum(row["micro"][metric] * row["query_count"] for row in summaries.values()) / total_queries for metric in METRICS}}


def evaluate(args):
    demand(not args.out.exists(), "coordinate test output already exists")
    protocol = read_json(args.protocol)
    validate_protocol(protocol)
    selection_path = args.fit / "selection.json"
    selection = read_json(selection_path)
    demand(selection["schema"] == "slopninja-frozen-coordinate-weights-v1" and selection["protocol_sha256"] == sha256(args.protocol), "coordinate model/protocol binding differs")
    demand(read_json(args.fit / "protocol.json") == protocol, "stored coordinate protocol changed")
    chosen, candidates = choose_lambda(selection["runs"])
    demand(chosen == selection["selected_lambda"] and candidates == selection["lambda_candidates"], "frozen shared lambda differs")
    for filename, key in [("implementation.json", "implementation_sha256"), ("selected-coordinates.json", "selected_coordinates_sha256")]:
        demand(sha256(args.fit / filename) == selection[key], f"frozen coordinate fit artifact changed: {filename}")
    implementation = read_json(args.fit / "implementation.json")
    for filename, digest in implementation["source_sha256"].items():
        demand(sha256(Path(__file__).parent / filename) == digest, f"frozen coordinate scorer source changed: {filename}")
    base_selection, baselines = validate_baselines(args.baseline_fit, protocol)
    names = [row["name"] for row in protocol["cohorts"]["test"]]
    inputs = load_inputs_manifest(args.inputs, sha256(args.protocol), names)
    demand(inputs["selection_sha256"] == sha256(selection_path) and inputs["catalog_sha256"] == selection["catalog_sha256"], "test inputs do not bind frozen coordinate model/catalog")
    demand(len(args.export) == 3, "supply three test exports in frozen protocol order")
    catalog = read_json(args.fit / "selected-coordinates.json")
    galleries = {}
    seen = set(base_selection["training_author_ids"]) | set(base_selection["selection_author_ids"])
    for descriptor, name, directory in zip(protocol["cohorts"]["test"], names, args.export):
        data = load_export(directory, inputs["exports"][name]["manifest_sha256"], selection["catalog_sha256"], protocol, "test", catalog)
        binding = inputs["exports"][name]
        validate_binding(data, binding, descriptor, protocol["reference_space_sha256"])
        demand(seen.isdisjoint(data["authors"]), "test author overlap")
        seen.update(data["authors"])
        galleries[name] = data
    checkpoints = {}
    for seed in SEEDS:
        matches = [row for row in selection["runs"] if row["lambda"] == chosen and row["inherited_seed"] == seed]
        demand(len(matches) == 1, "selected inherited seed missing")
        run = matches[0]
        path = args.fit / lambda_name(chosen) / f"seed-{seed}" / run["selected_checkpoint"]
        demand(path.resolve().is_relative_to(args.fit.resolve()) and sha256(path) == run["selected_checkpoint_sha256"], "selected coordinate checkpoint changed")
        stored = torch.load(path, weights_only=True)
        demand(stored["schema"] == "slopninja-coordinate-checkpoint-v1" and stored["lambda"] == chosen and stored["inherited_seed"] == seed and stored["epoch"] == run["selected_epoch"], "coordinate checkpoint identity differs")
        demand(stored["catalog_sha256"] == selection["catalog_sha256"] and stored["baseline_checkpoint_sha256"] == baselines[seed]["sha256"], "coordinate checkpoint baseline/catalog differs")
        demand(stored["validation"] == run["selected_validation"] and stored["eta"].tolist() == run["selected_eta"], "selected coordinate parameters differ")
        checkpoints[seed] = (run, stored)
    args.out.mkdir(parents=True)
    write_json(args.out / "input-validation.json", {"protocol_sha256": sha256(args.protocol), "inputs_sha256": sha256(args.inputs),
                                                   "selection_sha256": sha256(selection_path), "export_manifest_sha256": {name: data["manifest_sha256"] for name, data in galleries.items()},
                                                   "residual_audits": {name: data["residual_audit"] for name, data in galleries.items()}})
    model_results = {}
    for variant in ["baseline", "coordinate"]:
        seed_records, arrays = [], {}
        for seed in SEEDS:
            run, stored = checkpoints[seed]
            model = CoordinateMetric(torch.tensor([row["family_index"] for row in catalog["coordinates"]]), baselines[seed]["state"])
            if variant == "coordinate":
                with torch.no_grad():
                    model.eta.copy_(stored["eta"])
            per_gallery = {}
            for name in sorted(galleries):
                data = galleries[name]
                summary, logits, rows = score(model, data, baseline=variant == "baseline")
                arrays.setdefault(name, []).append(rows)
                path = args.out / f"{variant}-seed-{seed}-{name}-scores.json"
                write_json(path, {"query_ids": [row["id"] for row in data["rows"]], "candidate_authors": data["authors"],
                                  "logits": logits.tolist(), "per_query_metrics": rows.tolist()})
                per_gallery[name] = {"metrics": summary, "scores_sha256": sha256(path)}
            seed_records.append({"inherited_seed": seed, "selected_coordinate_epoch": run["selected_epoch"],
                                 "baseline_checkpoint_sha256": baselines[seed]["sha256"], "galleries": per_gallery,
                                 "pooled": pool({name: row["metrics"] for name, row in per_gallery.items()})})
        average = {name: summarize_rows(np.mean(rows, 0), galleries[name]["query_authors"]) for name, rows in arrays.items()}
        model_results[variant] = {"seed_mean": pool(average), "galleries": average, "seeds": seed_records,
                                  "seed_macro_top1_range": [min(row["pooled"]["macro_author"]["top1"] for row in seed_records), max(row["pooled"]["macro_author"]["top1"] for row in seed_records)]}
    comparison = paired_interval({name: row["per_author"] for name, row in model_results["coordinate"]["galleries"].items()},
                                 {name: row["per_author"] for name, row in model_results["baseline"]["galleries"].items()})
    primary = comparison["metrics"]["top1"]
    result = {"schema": "slopninja-coordinate-test-result-v1", "selected_lambda": chosen, "coordinate_count": len(catalog["coordinates"]),
              "protocol_sha256": sha256(args.protocol), "selection_sha256": sha256(selection_path), "inputs_sha256": sha256(args.inputs),
              "models": model_results, "comparison": comparison,
              "primary": {"comparison": "coordinate_minus_matched_frozen_baseline", "metric": "macro_author.top1", **primary,
                          "positive_lower_95_bound": primary["percentile_95"][0] > 0},
              "training_calls_during_test": 0, "external_model_calls": 0, "detector_calls": 0}
    write_json(args.out / "report.json", result)
    print(f"Coordinate test: delta {primary['mean_difference']:.6f}, 95% interval {primary['percentile_95']}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    fitting = subcommands.add_parser("train")
    fitting.add_argument("--training-export", type=Path, required=True)
    fitting.add_argument("--validation-export", type=Path, required=True)
    testing = subcommands.add_parser("evaluate")
    testing.add_argument("--fit", type=Path, required=True)
    testing.add_argument("--export", action="append", type=Path, required=True)
    for command in [fitting, testing]:
        command.add_argument("--baseline-fit", type=Path, required=True)
        command.add_argument("--protocol", type=Path, required=True)
        command.add_argument("--inputs", type=Path, required=True)
        command.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    configure()
    train(args) if args.command == "train" else evaluate(args)


if __name__ == "__main__":
    main()

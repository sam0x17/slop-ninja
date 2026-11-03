"""Fit and evaluate a frozen frontier of supported coordinate weighting models."""
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

from coordinate_frontier_io import load_export, load_inputs
from coordinate_frontier_models import BLOCKS, LAMBDAS, SEEDS, FrontierMetric, configurations, reset_eta, subset_data
from coordinate_io import IDENTITY
from frontier_common import demand, read_json, sha256, write_json
from train_coordinates import lambda_name, loss_components, paired_interval, pool, score, summarize_rows, validate_baselines
from train_frontier import configure


METRICS = ["top1", "top5", "mrr"]
SOURCES = ["coordinate_frontier.py", "coordinate_frontier_io.py", "coordinate_frontier_models.py",
           "coordinate_model.py", "coordinate_io.py", "train_coordinates.py", "frontier_common.py",
           "frontier_models.py", "train_frontier.py", "training-lock.json"]


def validate_protocol(protocol):
    demand(protocol["schema"] == "slopninja-coordinate-frontier-protocol-v1", "unsupported coordinate frontier protocol")
    demand(protocol["inherited_seeds"] == SEEDS and protocol["lambda_grid"] == LAMBDAS, "frontier grid differs")
    expected = {"epochs": 200, "learning_rate": 0.03, "beta1": 0.9, "beta2": 0.999, "epsilon": 1e-8,
                "eta_initial": 0, "weight_bounds": [0.5, 2], "device": "cpu", "dtype": "float64",
                "threads": 8, "deterministic": True,
                "penalty": "lambda / 14 times sum over nonempty adjustable families of mean eta squared within family; inactive families contribute zero",
                "penalty_family_denominator": 14,
                "trainable": "selected coordinate eta only; original family coefficients, spline knots/slopes and temperature frozen",
                "failure_policy": "stop on any failed fit and retain artifacts; all 192 fits must complete before selection"}
    demand(protocol["optimization"] == expected, "frontier optimization differs from frozen design")
    demand(protocol["baseline_model"] == "combined_spline", "unexpected frontier family baseline")
    demand(len(protocol["cohorts"]["validation"]) == 3 and len(protocol["cohorts"]["test"]) == 3,
           "frontier requires three fixed validation and three fixed test galleries")
    for key, value in {"min_authors": 10, "min_posts": 30, "max_coordinates": 7353,
                       "max_lexical_coordinates": 2189, "max_grammar_coordinates": 5164,
                       "caps_base": {"word": 512, "word_bigram": 512, "grammar": 96},
                       "unselected_multiplier": 1}.items():
        demand(protocol["coordinate_selection"][key] == value, f"frozen frontier catalog policy differs: {key}")
    demand(protocol["selection"]["checkpoint"] == "macro_author.top1, then macro_author.mrr, then earliest epoch including 0",
           "frontier checkpoint ordering differs")
    demand(protocol["selection"]["lambda"] == "per config mean selected seed macro_author.top1, then mean MRR, then stronger lambda",
           "frontier lambda ordering differs")
    demand(protocol["selection"]["config"] == "mean selected seed macro_author.top1, then mean MRR, then fewer active coordinates, then canonical config id",
           "frontier configuration ordering differs")


def validate_incumbent(directory, protocol, baselines):
    demand(sha256(directory / "selection.json") == protocol["incumbent_selection_sha256"], "frozen incumbent selection changed")
    selection = read_json(directory / "selection.json")
    demand(selection["schema"] == "slopninja-frozen-coordinate-weights-v1" and selection["selected_lambda"] == 10,
           "unexpected coordinate incumbent")
    demand(selection["catalog_sha256"] == protocol["incumbent_catalog_sha256"], "incumbent original catalog binding differs")
    for filename, key in [("implementation.json", "implementation_sha256"),
                          ("selected-coordinates.json", "selected_coordinates_sha256")]:
        demand(sha256(directory / filename) == selection[key], f"incumbent artifact changed: {filename}")
    implementation = read_json(directory / "implementation.json")
    for name, digest in implementation["source_sha256"].items():
        demand(sha256(Path(__file__).parent / name) == digest, f"frozen incumbent source changed: {name}")
    catalog = read_json(directory / "selected-coordinates.json")
    for key in IDENTITY:
        demand(selection[key] == protocol[key] == catalog[key], f"incumbent source identity differs: {key}")
    states = {}
    for binding in protocol["incumbent_checkpoints"]:
        seed = binding["inherited_seed"]
        runs = [row for row in selection["runs"] if row["lambda"] == 10 and row["inherited_seed"] == seed]
        demand(len(runs) == 1 and runs[0]["status"] == "complete", "incumbent seed fit differs")
        run = runs[0]
        for key in ["selected_epoch", "selected_checkpoint", "selected_checkpoint_sha256"]:
            demand(run[key] == binding[key], f"incumbent selected state differs: {key}")
        path = directory / "lambda-10" / f"seed-{seed}" / run["selected_checkpoint"]
        demand(path.resolve().is_relative_to(directory.resolve()) and sha256(path) == binding["selected_checkpoint_sha256"],
               "incumbent checkpoint digest differs")
        stored = torch.load(path, weights_only=True)
        demand(stored["schema"] == "slopninja-coordinate-checkpoint-v1" and stored["catalog_sha256"] == selection["catalog_sha256"],
               "incumbent checkpoint schema/catalog differs")
        demand(stored["baseline_checkpoint_sha256"] == baselines[seed]["sha256"] and stored["eta"].tolist() == run["selected_eta"],
               "incumbent baseline or coordinate state differs")
        states[seed] = {"eta": stored["eta"], "sha256": binding["selected_checkpoint_sha256"]}
    demand(sorted(states) == SEEDS, "incumbent seed coverage differs")
    return catalog, states


def validation_score(model, galleries):
    summaries = {name: score(model, data)[0] for name, data in galleries.items()}
    return pool(summaries), summaries


def check_zero_identity(training, validation, baselines):
    result = {}
    for seed in SEEDS:
        model = FrontierMetric(training["inputs"].family_indices, baselines[seed]["state"])
        demand(torch.equal(model.adjusted_distances(training["inputs"]), training["inputs"].raw_distances),
               "zero frontier eta changes training distances")
        with torch.no_grad():
            _, cross_entropy, _ = loss_components(model, training, 1)
        previous = baselines[seed]["run"]["selected_training_cross_entropy"]
        error = abs(cross_entropy.item() - previous)
        demand(error < 1e-9, "frontier training geometry does not reproduce frozen baseline loss")
        for data in validation.values():
            demand(torch.equal(model.adjusted_distances(data["inputs"]), data["inputs"].raw_distances),
                   "zero frontier eta changes validation distances")
        metrics, _ = validation_score(model, validation)
        result[str(seed)] = {"training_cross_entropy": cross_entropy.item(), "baseline_training_loss_error": error,
                             "validation": metrics}
    return result


def load_replay(incumbent_fit, config, regularization, seed):
    if config["name"] != "all_m1" or regularization not in [0.1, 1, 10]:
        return None
    path = incumbent_fit / lambda_name(regularization) / f"seed-{seed}" / "epochs.jsonl"
    summary = read_json(path.parent / "summary.json")
    prior_runs = [row for row in read_json(incumbent_fit / "selection.json")["runs"]
                  if row["lambda"] == regularization and row["inherited_seed"] == seed]
    demand(len(prior_runs) == 1 and sha256(path) == summary["epochs_sha256"] == prior_runs[0]["epochs_sha256"],
           "prior trajectory changed")
    rows = [json.loads(line) for line in path.read_text().splitlines()]
    demand([row["epoch"] for row in rows] == list(range(201)), "prior trajectory epoch coverage differs")
    return rows


def train_fit(config, regularization, seed, training, validation, baseline, catalog_hash, destination, replay):
    destination.mkdir(parents=True)
    (destination / "checkpoints").mkdir()
    model = FrontierMetric(training["inputs"].family_indices, baseline["state"])
    demand(model.eta.numel() == config["coordinate_count"], "frontier has dormant or missing trainable coordinates")
    optimizer = torch.optim.Adam(model.parameters(), lr=0.03, betas=(0.9, 0.999), eps=1e-8, foreach=False, fused=False)
    best, best_key = None, (-math.inf, -math.inf)
    maximum_replay_error = {"eta": 0.0, "training_cross_entropy": 0.0, "objective": 0.0}
    began = time.perf_counter()
    with (destination / "epochs.jsonl").open("w") as stream:
        for epoch in range(201):
            optimizer.zero_grad(set_to_none=True)
            objective, cross_entropy, penalty = loss_components(model, training, regularization)
            demand(bool(torch.isfinite(objective)), "nonfinite frontier training objective")
            objective.backward()
            demand(bool(torch.isfinite(model.eta.grad).all()), "nonfinite frontier gradient")
            if replay is not None:
                expected = replay[epoch]
                errors = {"eta": (model.eta.detach() - torch.tensor(expected["eta"], dtype=torch.float64)).abs().max().item(),
                          "training_cross_entropy": abs(cross_entropy.item() - expected["training_cross_entropy"]),
                          "objective": abs(objective.item() - expected["objective"])}
                for key, error in errors.items():
                    maximum_replay_error[key] = max(maximum_replay_error[key], error)
                    demand(error < 1e-8, f"overlapping original trajectory differs at epoch {epoch}: {key}")
            metrics, per_gallery = validation_score(model, validation)
            key = (metrics["macro_author"]["top1"], metrics["macro_author"]["mrr"])
            checkpoint = destination / "checkpoints" / f"epoch-{epoch:03}.pt"
            torch.save({"schema": "slopninja-coordinate-frontier-checkpoint-v1", "config": config,
                        "lambda": regularization, "inherited_seed": seed, "epoch": epoch,
                        "eta": model.eta.detach().clone(), "optimizer_state": optimizer.state_dict(),
                        "baseline_checkpoint_sha256": baseline["sha256"], "catalog_sha256": catalog_hash,
                        "training_cross_entropy": cross_entropy.item(), "penalty": penalty.item(),
                        "objective": objective.item(), "validation": metrics}, checkpoint)
            record = {"epoch": epoch, "lambda": regularization, "inherited_seed": seed,
                      "training_cross_entropy": cross_entropy.item(), "unscaled_penalty": penalty.item(),
                      "objective": objective.item(), "validation": metrics, "validation_galleries": per_gallery,
                      "eta": model.eta.detach().tolist(), "gradient_l2": model.eta.grad.norm().item(),
                      "checkpoint": str(checkpoint.relative_to(destination)), "checkpoint_sha256": sha256(checkpoint),
                      "elapsed_seconds": time.perf_counter() - began}
            stream.write(json.dumps(record, allow_nan=False) + "\n")
            stream.flush()
            if key > best_key:
                best_key, best = key, copy.deepcopy(record)
            if epoch % 100 == 0:
                print(f"{config['name']} lambda {regularization:g} seed {seed} epoch {epoch}: validation top1 {key[0]:.6f}", flush=True)
            if epoch < 200:
                optimizer.step()
                model.constrain()
    stored = torch.load(destination / best["checkpoint"], weights_only=True)
    with torch.no_grad():
        model.eta.copy_(stored["eta"])
    demand(validation_score(model, validation)[0] == best["validation"], "selected frontier checkpoint replay differs")
    result = {"status": "complete", "config": config, "lambda": regularization, "inherited_seed": seed,
              "actual_trainable_parameters": model.eta.numel(), "selected_epoch": best["epoch"],
              "selected_checkpoint": best["checkpoint"], "selected_checkpoint_sha256": best["checkpoint_sha256"],
              "selected_validation": best["validation"], "selected_eta": best["eta"],
              "selected_training_cross_entropy": best["training_cross_entropy"], "selected_objective": best["objective"],
              "baseline_checkpoint_sha256": baseline["sha256"], "catalog_sha256": catalog_hash,
              "epochs_sha256": sha256(destination / "epochs.jsonl"), "elapsed_seconds": time.perf_counter() - began,
              "prior_trajectory_replay": None if replay is None else {"epochs": 201, "maximum_errors": maximum_replay_error}}
    write_json(destination / "summary.json", result)
    return result


def select_frontier(runs, configs):
    expected = {(config["name"], value, seed) for config in configs for value in LAMBDAS for seed in SEEDS}
    actual = {(row["config"]["name"], row["lambda"], row["inherited_seed"]) for row in runs}
    demand(len(runs) == len(expected) == 192 and actual == expected and all(row["status"] == "complete" for row in runs),
           "all 192 prescribed frontier fits must complete before selection")
    cells = []
    for config in configs:
        candidates = []
        for value in LAMBDAS:
            selected = [row for row in runs if row["config"]["name"] == config["name"] and row["lambda"] == value]
            means = {metric: statistics.mean(row["selected_validation"]["macro_author"][metric] for row in selected)
                     for metric in METRICS}
            candidates.append({"lambda": value, "mean_seed_validation": means})
        candidates.sort(key=lambda row: (-row["mean_seed_validation"]["top1"], -row["mean_seed_validation"]["mrr"], -row["lambda"]))
        cells.append({"config": config, "selected_lambda": candidates[0]["lambda"],
                      "mean_seed_validation": candidates[0]["mean_seed_validation"], "lambda_candidates": candidates})
    ordering = lambda row: (-row["mean_seed_validation"]["top1"], -row["mean_seed_validation"]["mrr"],
                            row["config"]["coordinate_count"], row["config"]["name"])
    winner = min(cells, key=ordering)["config"]["name"]
    masks = {block: min((row for row in cells if row["config"]["block"] == block), key=ordering)["config"]["name"]
             for block in BLOCKS}
    return {"cells": cells, "winner": winner, "mask_winners": masks}


def incumbent_indices(catalog, incumbent_catalog):
    lookup = {(row["family"], row["feature"]): index for index, row in enumerate(catalog["coordinates"])}
    indices = [lookup[(row["family"], row["feature"])] for row in incumbent_catalog["coordinates"]]
    demand(indices == catalog["subsets"]["all_m1"]["max_column_indices"], "incumbent is not the exact nested x1 catalog")
    for previous, index in zip(incumbent_catalog["coordinates"], indices):
        actual = catalog["coordinates"][index]
        demand(all(actual[key] == previous[key] for key in previous), "incumbent coordinate geometry or support differs")
    return indices


def train(args):
    demand(not args.out.exists(), "frontier training output already exists")
    protocol = read_json(args.protocol)
    validate_protocol(protocol)
    names = [row["name"] for row in protocol["cohorts"]["validation"]]
    demand(len(args.validation_export) == 3, "supply three validation exports in protocol order")
    inputs = load_inputs(args.inputs, sha256(args.protocol), ["train", *names])
    _, baselines = validate_baselines(args.baseline_fit, protocol)
    incumbent_catalog, incumbent_states = validate_incumbent(args.incumbent_fit, protocol, baselines)
    training = load_export(args.training_export, inputs["exports"]["train"], inputs["catalog_sha256"], protocol,
                           "train", protocol["cohorts"]["training"])
    catalog = training["catalog"]
    configs = configurations(catalog)
    declarations = [{"id": row["name"], "adjustable_subset": row["block"], "cap_size": row["name"].split("_", 1)[1],
                     "trainable_coordinates": row["coordinate_count"]} for row in configs]
    demand(declarations == protocol["configs"], "frontier configuration order or parameter counts differ")
    demand(catalog["original_catalog_sha256"] == protocol["coordinate_selection"]["source_support_catalog_sha256"],
           "frontier original support catalog binding differs")
    previous_indices = incumbent_indices(catalog, incumbent_catalog)
    validation, seen = {}, set(training["authors"])
    for descriptor, path in zip(protocol["cohorts"]["validation"], args.validation_export):
        name = descriptor["name"]
        data = load_export(path, inputs["exports"][name], inputs["catalog_sha256"], protocol, "dev", descriptor, catalog)
        demand(seen.isdisjoint(data["authors"]), "frontier validation author overlap")
        seen.update(data["authors"])
        validation[name] = data
    identity = check_zero_identity(subset_data(training, previous_indices),
                                   {name: subset_data(data, previous_indices) for name, data in validation.items()}, baselines)
    args.out.mkdir(parents=True)
    write_json(args.out / "protocol.json", protocol)
    write_json(args.out / "inputs.json", inputs)
    write_json(args.out / "catalog.json", catalog)
    manifest = {"schema": "slopninja-coordinate-frontier-training-implementation-v1",
                "protocol_sha256": sha256(args.protocol), "inputs_sha256": sha256(args.inputs),
                "catalog_sha256": inputs["catalog_sha256"], "stored_catalog_sha256": sha256(args.out / "catalog.json"),
                "source_sha256": {name: sha256(Path(__file__).parent / name) for name in SOURCES},
                "training_author_ids": training["authors"], "validation_author_ids": sorted(seen - set(training["authors"])),
                "training_queries": len(training["rows"]), "validation_queries": sum(len(data["rows"]) for data in validation.values()),
                "baseline_identity": identity, "configurations": configs, "test_exports_opened": False,
                "residual_audits": {"train": training["residual_audit"], **{name: data["residual_audit"] for name, data in validation.items()}}}
    write_json(args.out / "implementation.json", manifest)
    runs = []
    for config in configs:
        columns = config["max_column_indices"]
        selected_training = subset_data(training, columns)
        selected_validation = {name: subset_data(data, columns) for name, data in validation.items()}
        for value in LAMBDAS:
            for seed in SEEDS:
                destination = args.out / config["name"] / lambda_name(value) / f"seed-{seed}"
                try:
                    replay = load_replay(args.incumbent_fit, config, value, seed)
                    run = train_fit(config, value, seed, selected_training, selected_validation, baselines[seed],
                                    inputs["catalog_sha256"], destination, replay)
                except Exception as error:
                    destination.mkdir(parents=True, exist_ok=True)
                    failure = {"status": "failed", "config": config, "lambda": value, "inherited_seed": seed,
                               "error": str(error), "traceback": traceback.format_exc()}
                    write_json(destination / "failure.json", failure)
                    write_json(args.out / "progress.json", {"status": "stopped_after_failed_fit", "runs": runs + [failure]})
                    raise
                runs.append(run)
                write_json(args.out / "progress.json", {"completed_fits": len(runs), "planned_fits": 192, "runs": runs})
                print(f"Frontier progress {len(runs)}/192; selected epoch {run['selected_epoch']}", flush=True)
    selected = select_frontier(runs, configs)
    result = {"schema": "slopninja-frozen-coordinate-frontier-v1", **selected, "runs": runs,
              "protocol_sha256": sha256(args.protocol), "inputs_sha256": sha256(args.inputs),
              "implementation_sha256": sha256(args.out / "implementation.json"),
              "catalog_sha256": inputs["catalog_sha256"], "stored_catalog_sha256": sha256(args.out / "catalog.json"),
              "incumbent_selection_sha256": protocol["incumbent_selection_sha256"],
              "inherited_seeds": SEEDS, "completed_fits": 192, "failed_fits": 0,
              **{key: protocol[key] for key in IDENTITY}}
    write_json(args.out / "selection.json", result)
    print(f"FROZEN frontier winner {result['winner']}; selection SHA256 {sha256(args.out / 'selection.json')}", flush=True)


def selected_state(fit, selection, cell, seed, baseline_hash):
    matches = [row for row in selection["runs"] if row["config"]["name"] == cell["config"]["name"] and
               row["lambda"] == cell["selected_lambda"] and row["inherited_seed"] == seed]
    demand(len(matches) == 1, "selected frontier seed missing")
    run = matches[0]
    path = fit / cell["config"]["name"] / lambda_name(cell["selected_lambda"]) / f"seed-{seed}" / run["selected_checkpoint"]
    demand(path.resolve().is_relative_to(fit.resolve()) and sha256(path) == run["selected_checkpoint_sha256"],
           "selected frontier checkpoint changed")
    stored = torch.load(path, weights_only=True)
    demand(stored["schema"] == "slopninja-coordinate-frontier-checkpoint-v1" and stored["config"] == cell["config"],
           "selected frontier checkpoint configuration differs")
    demand(stored["lambda"] == run["lambda"] and stored["inherited_seed"] == seed and stored["epoch"] == run["selected_epoch"],
           "selected frontier checkpoint identity differs")
    demand(stored["catalog_sha256"] == selection["catalog_sha256"] and stored["baseline_checkpoint_sha256"] == baseline_hash,
           "selected frontier checkpoint catalog or baseline differs")
    demand(stored["eta"].tolist() == run["selected_eta"] and stored["validation"] == run["selected_validation"],
           "selected frontier parameters or validation record differ")
    demand(bool(torch.isfinite(stored["eta"]).all()) and bool((stored["eta"].abs() <= math.log(2)).all()),
           "selected frontier coordinates violate frozen bounds")
    return stored["eta"], run


def evaluate(args):
    demand(not args.out.exists(), "frontier test output already exists")
    protocol = read_json(args.protocol)
    validate_protocol(protocol)
    selection_path = args.fit / "selection.json"
    selection = read_json(selection_path)
    demand(selection["schema"] == "slopninja-frozen-coordinate-frontier-v1" and
           selection["protocol_sha256"] == sha256(args.protocol), "frontier selection/protocol binding differs")
    demand(read_json(args.fit / "protocol.json") == protocol, "stored frontier protocol changed")
    for filename, key in [("implementation.json", "implementation_sha256"), ("catalog.json", "stored_catalog_sha256")]:
        demand(sha256(args.fit / filename) == selection[key], f"frozen frontier artifact changed: {filename}")
    implementation = read_json(args.fit / "implementation.json")
    for name, digest in implementation["source_sha256"].items():
        demand(sha256(Path(__file__).parent / name) == digest, f"frozen frontier scorer source changed: {name}")
    catalog = read_json(args.fit / "catalog.json")
    configs = configurations(catalog)
    frozen = select_frontier(selection["runs"], configs)
    demand(all(selection[key] == value for key, value in frozen.items()), "frozen frontier model selection differs")
    _, baselines = validate_baselines(args.baseline_fit, protocol)
    incumbent_catalog, incumbent_states = validate_incumbent(args.incumbent_fit, protocol, baselines)
    previous_indices = incumbent_indices(catalog, incumbent_catalog)
    descriptors = protocol["cohorts"]["test"]
    names = [row["name"] for row in descriptors]
    inputs = load_inputs(args.inputs, sha256(args.protocol), names, sha256(selection_path))
    demand(inputs["catalog_sha256"] == selection["catalog_sha256"] and len(args.export) == 3,
           "test maximal catalog or gallery count differs")
    galleries, seen = {}, set(implementation["training_author_ids"]) | set(implementation["validation_author_ids"])
    for descriptor, directory in zip(descriptors, args.export):
        name = descriptor["name"]
        data = load_export(directory, inputs["exports"][name], inputs["catalog_sha256"], protocol, "test", descriptor, catalog)
        demand(seen.isdisjoint(data["authors"]), "frontier test author overlap")
        seen.update(data["authors"])
        galleries[name] = data
    cells = {row["config"]["name"]: row for row in selection["cells"]}
    states = {(name, seed): selected_state(args.fit, selection, cell, seed, baselines[seed]["sha256"])
              for name, cell in cells.items() for seed in SEEDS}
    args.out.mkdir(parents=True)
    write_json(args.out / "input-validation.json", {"protocol_sha256": sha256(args.protocol),
               "inputs_sha256": sha256(args.inputs), "selection_sha256": sha256(selection_path),
               "export_manifest_sha256": {name: data["manifest_sha256"] for name, data in galleries.items()},
               "residual_audits": {name: data["residual_audit"] for name, data in galleries.items()}})
    combined_name = selection["mask_winners"]["all"]
    variants = [("family_baseline", None, None), ("incumbent", None, None)]
    variants += [(name, name, None) for name in cells]
    variants += [("combined_word_eta_only", combined_name, "word"),
                 ("combined_grammar_eta_only", combined_name, "grammar"),
                 ("combined_zero_eta", combined_name, "neither")]
    lexical = {catalog["families"].index(name) for name in ["word", "word_bigram"]}
    results, baseline_logits = {}, {}
    for variant, cell_name, reset in variants:
        indices = previous_indices if cell_name is None else cells[cell_name]["config"]["max_column_indices"]
        selected_galleries = {name: subset_data(data, indices) for name, data in galleries.items()}
        seed_records, arrays, score_times = [], {}, []
        for seed in SEEDS:
            families = next(iter(selected_galleries.values()))["inputs"].family_indices
            model = FrontierMetric(families, baselines[seed]["state"])
            run = None
            if cell_name is not None:
                eta, run = states[(cell_name, seed)]
                with torch.no_grad():
                    model.eta.copy_(eta)
            elif variant == "incumbent":
                with torch.no_grad():
                    model.eta.copy_(incumbent_states[seed]["eta"])
            if reset is not None:
                reset_eta(model, reset, lexical)
            summaries = {}
            for name, data in selected_galleries.items():
                began = time.perf_counter()
                summary, logits, rows = score(model, data, baseline=variant == "family_baseline")
                score_times.append(time.perf_counter() - began)
                if variant == "family_baseline":
                    baseline_logits[(seed, name)] = logits
                if variant == "combined_zero_eta":
                    demand(np.array_equal(logits, baseline_logits[(seed, name)]), "resetting all eta does not reproduce family baseline")
                arrays.setdefault(name, []).append(rows)
                path = args.out / f"{variant}-seed-{seed}-{name}-scores.json"
                write_json(path, {"query_ids": [row["id"] for row in data["rows"]], "candidate_authors": data["authors"],
                                  "logits": logits.tolist(), "per_query_metrics": rows.tolist()})
                summaries[name] = {"metrics": summary, "scores_sha256": sha256(path)}
            seed_records.append({"inherited_seed": seed, "selected_epoch": None if run is None else run["selected_epoch"],
                                 "galleries": summaries, "pooled": pool({name: row["metrics"] for name, row in summaries.items()})})
        average = {name: summarize_rows(np.mean(rows, 0), galleries[name]["query_authors"]) for name, rows in arrays.items()}
        results[variant] = {"seed_mean": pool(average), "galleries": average, "seeds": seed_records,
                            "coordinate_count": 0 if variant == "family_baseline" else len(indices),
                            "source_config": cell_name, "conditional_eta_reset": reset,
                            "selected_lambda": None if cell_name is None else cells[cell_name]["selected_lambda"],
                            "scoring_seconds_including_metric_aggregation": sum(score_times),
                            "seed_macro_top1_range": [min(row["pooled"]["macro_author"]["top1"] for row in seed_records),
                                                      max(row["pooled"]["macro_author"]["top1"] for row in seed_records)]}
        print(f"Scored frozen variant {variant}", flush=True)
    def compare(left, right):
        value = paired_interval({name: row["per_author"] for name, row in results[left]["galleries"].items()},
                                {name: row["per_author"] for name, row in results[right]["galleries"].items()})
        return {"left": left, "right": right, **value}
    comparisons = {"winner_minus_incumbent": compare(selection["winner"], "incumbent"),
                   "winner_minus_family_baseline": compare(selection["winner"], "family_baseline"),
                   "combined_minus_word_eta_only": compare(combined_name, "combined_word_eta_only"),
                   "combined_minus_grammar_eta_only": compare(combined_name, "combined_grammar_eta_only")}
    primary = comparisons["winner_minus_incumbent"]["metrics"]["top1"]
    result = {"schema": "slopninja-coordinate-frontier-test-result-v1", "winner": selection["winner"],
              "mask_winners": selection["mask_winners"], "models": results, "comparisons": comparisons,
              "primary": {"comparison": "winner_minus_incumbent", "metric": "macro_author.top1", **primary,
                          "positive_lower_95_bound": primary["percentile_95"][0] > 0},
              "protocol_sha256": sha256(args.protocol), "selection_sha256": sha256(selection_path),
              "inputs_sha256": sha256(args.inputs), "training_calls_during_test": 0,
              "external_model_calls": 0, "detector_calls": 0}
    write_json(args.out / "report.json", result)
    print(f"Frontier test: {selection['winner']} minus incumbent {primary['mean_difference']:.6f}, 95% interval {primary['percentile_95']}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    fitting = commands.add_parser("train")
    fitting.add_argument("--training-export", type=Path, required=True)
    fitting.add_argument("--validation-export", type=Path, action="append", required=True)
    testing = commands.add_parser("evaluate")
    testing.add_argument("--fit", type=Path, required=True)
    testing.add_argument("--export", type=Path, action="append", required=True)
    for command in [fitting, testing]:
        command.add_argument("--baseline-fit", type=Path, required=True)
        command.add_argument("--incumbent-fit", type=Path, required=True)
        command.add_argument("--protocol", type=Path, required=True)
        command.add_argument("--inputs", type=Path, required=True)
        command.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    configure()
    train(args) if args.command == "train" else evaluate(args)


if __name__ == "__main__":
    main()

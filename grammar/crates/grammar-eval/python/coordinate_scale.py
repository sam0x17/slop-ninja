"""One fixed-representation contrast: 100 versus 300 training authors."""
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

from coordinate_frontier import incumbent_indices, selected_state, validate_incumbent, validation_score
from coordinate_frontier_models import LAMBDAS, SEEDS, FrontierMetric, reset_eta, subset_data
from coordinate_io import IDENTITY
from coordinate_scale_io import load_fixed_export, load_inputs
from frontier_common import demand, read_json, sha256, write_json
from train_coordinates import lambda_name, paired_interval, pool, score, summarize_rows, validate_baselines
from train_frontier import configure


METRICS = ["top1", "top5", "mrr"]
SOURCES = ["coordinate_scale.py", "coordinate_scale_io.py", "coordinate_frontier.py", "coordinate_frontier_io.py",
           "coordinate_frontier_models.py", "coordinate_model.py", "coordinate_io.py", "train_coordinates.py",
           "frontier_common.py", "frontier_models.py", "train_frontier.py", "training-lock.json"]


def validate_protocol(protocol):
    demand(protocol["schema"] == "slopninja-training-author-scale-protocol-v1", "unsupported training-scale protocol")
    demand(protocol["inherited_seeds"] == SEEDS and protocol["lambda_grid"] == LAMBDAS, "training-scale optimization grid differs")
    demand(protocol["control_config"] == "all_m2" and protocol["control_selected_lambda"] == 0.1,
           "training-scale frozen control differs")
    expected = {"epochs": 200, "learning_rate": 0.03, "beta1": 0.9, "beta2": 0.999, "epsilon": 1e-8,
                "eta_initial": 0, "weight_bounds": [0.5, 2], "device": "cpu", "dtype": "float64",
                "threads": 8, "deterministic": True, "penalty_family_denominator": 14}
    for key, value in expected.items():
        demand(protocol["optimization"][key] == value, f"training-scale optimizer differs: {key}")
    demand(len(protocol["cohorts"]["training_panels"]) == 3 and len(protocol["cohorts"]["validation"]) == 3 and
           len(protocol["cohorts"]["test"]) == 6, "training-scale panel or gallery counts differ")
    demand(protocol["coordinate_count"] == 3557 and protocol["maximal_coordinate_count"] == 7353,
           "training-scale fixed representation size differs")
    for key, value in {"control_authors": 100, "treatment_authors": 300, "panel_count": 3,
                       "authors_per_panel": 100, "queries_per_panel": [1224, 1363, 1302],
                       "total_treatment_queries": 3889, "candidate_authors_per_query": 100}.items():
        demand(protocol["training"][key] == value, f"training-scale panel design differs: {key}")
    demand(protocol["selection"]["checkpoint"] == "macro_author.top1, then macro_author.mrr, then earliest epoch including 0" and
           protocol["selection"]["lambda"] == "mean selected seed macro_author.top1, then mean MRR, then stronger lambda",
           "training-scale selection ordering differs")


def choose_lambda(runs):
    expected = {(value, seed) for value in LAMBDAS for seed in SEEDS}
    demand(len(runs) == 12 and {(row["lambda"], row["inherited_seed"]) for row in runs} == expected and
           all(row["status"] == "complete" for row in runs), "all twelve prescribed treatment/control fits must be complete")
    candidates = []
    for value in LAMBDAS:
        selected = [row for row in runs if row["lambda"] == value]
        means = {metric: statistics.mean(row["selected_validation"]["macro_author"][metric] for row in selected)
                 for metric in METRICS}
        candidates.append({"lambda": value, "mean_seed_validation": means})
    candidates.sort(key=lambda row: (-row["mean_seed_validation"]["top1"], -row["mean_seed_validation"]["mrr"], -row["lambda"]))
    return candidates[0]["lambda"], candidates


def panel_cross_entropy(model, data):
    losses = torch.nn.functional.cross_entropy(model(data["inputs"]), data["inputs"].own_indices, reduction="none")
    return (losses * data["inputs"].loss_weights).sum()


def backward_panels(model, panels, regularization):
    """Accumulate each panel once, then add one penalty before the caller's Adam step."""
    author_total = sum(len(data["authors"]) for data in panels.values())
    values, cross_entropy = {}, 0.0
    for name, data in panels.items():
        weight = len(data["authors"]) / author_total
        value = panel_cross_entropy(model, data)
        demand(bool(torch.isfinite(value)), "nonfinite training panel cross entropy")
        (weight * value).backward()
        values[name] = {"cross_entropy": value.item(), "author_weight": weight}
        cross_entropy += weight * value.item()
    penalty = model.penalty()
    (regularization * penalty).backward()
    objective = cross_entropy + regularization * penalty.item()
    demand(math.isfinite(objective) and bool(torch.isfinite(model.eta.grad).all()), "nonfinite pooled objective or gradient")
    return {"training_cross_entropy": cross_entropy, "unscaled_penalty": penalty.item(), "objective": objective,
            "panel_losses": values}


def validate_control(directory, protocol, baselines):
    selection_path = directory / "selection.json"
    demand(sha256(selection_path) == protocol["control_selection_sha256"], "frozen 100-author control selection changed")
    selection = read_json(selection_path)
    demand(selection["schema"] == "slopninja-frozen-coordinate-frontier-v1" and
           selection["protocol_sha256"] == protocol["source_frontier_protocol_sha256"], "control source frontier identity differs")
    demand(selection["inputs_sha256"] == protocol["control_training_inputs_sha256"] ==
           sha256(directory.parent / "training-inputs.json"), "original control training inputs changed")
    for filename, key in [("implementation.json", "implementation_sha256"), ("catalog.json", "stored_catalog_sha256")]:
        demand(sha256(directory / filename) == selection[key], f"control artifact changed: {filename}")
    implementation = read_json(directory / "implementation.json")
    for name, digest in implementation["source_sha256"].items():
        demand(sha256(Path(__file__).parent / name) == digest, f"frozen control source changed: {name}")
    catalog = read_json(directory / "catalog.json")
    for key in IDENTITY:
        demand(selection[key] == protocol[key] == catalog[key], f"control source geometry differs: {key}")
    cells = [row for row in selection["cells"] if row["config"]["name"] == "all_m2"]
    demand(len(cells) == 1 and cells[0]["config"]["coordinate_count"] == 3557, "fixed control coordinate subset differs")
    cell = cells[0]
    demand(selection["catalog_sha256"] == protocol["catalog_sha256"] and
           cell["config"]["indices_sha256"] == protocol["fixed_subset_indices_sha256"], "control fixed coordinate geometry differs")
    runs = [row for row in selection["runs"] if row["config"]["name"] == "all_m2"]
    selected, candidates = choose_lambda(runs)
    demand(selected == cell["selected_lambda"] == protocol["control_selected_lambda"] and candidates == cell["lambda_candidates"],
           "recomputed control regularization selection differs")
    bindings = []
    for run in runs:
        parent = directory / "all_m2" / lambda_name(run["lambda"]) / f"seed-{run['inherited_seed']}"
        demand(read_json(parent / "summary.json") == run, "reused control summary changed")
        history = parent / "epochs.jsonl"
        demand(sha256(history) == run["epochs_sha256"], "reused control trajectory changed")
        checkpoint_bindings = []
        best, best_key = None, (-math.inf, -math.inf)
        with history.open() as stream:
            for epoch, line in enumerate(stream):
                record = json.loads(line)
                demand(record["epoch"] == epoch, "reused control trajectory epoch ordering differs")
                path = parent / record["checkpoint"]
                demand(path.resolve().is_relative_to(parent.resolve()) and sha256(path) == record["checkpoint_sha256"],
                       "reused control checkpoint changed")
                key = (record["validation"]["macro_author"]["top1"], record["validation"]["macro_author"]["mrr"])
                if key > best_key:
                    best_key, best = key, record
                checkpoint_bindings.append({"epoch": epoch, "path": str(path.relative_to(directory)),
                                            "sha256": record["checkpoint_sha256"]})
        demand(len(checkpoint_bindings) == 201, "reused control must retain all 201 checkpoint states")
        demand(best["epoch"] == run["selected_epoch"] and best["validation"] == run["selected_validation"] and
               best["eta"] == run["selected_eta"] and best["checkpoint_sha256"] == run["selected_checkpoint_sha256"],
               "rederived control checkpoint selection differs")
        bindings.append({"lambda": run["lambda"], "inherited_seed": run["inherited_seed"],
                         "epochs_sha256": run["epochs_sha256"], "checkpoints": checkpoint_bindings})
    states = {}
    for binding in protocol["control_checkpoints"]:
        seed = binding["inherited_seed"]
        eta, run = selected_state(directory, selection, cell, seed, baselines[seed]["sha256"])
        for key in ["selected_epoch", "selected_checkpoint", "selected_checkpoint_sha256"]:
            demand(run[key] == binding[key], f"control selected checkpoint binding differs: {key}")
        states[seed] = {"eta": eta, "run": run}
    demand(sorted(states) == SEEDS, "control seed coverage differs")
    receipt = {"schema": "slopninja-training-scale-control-reuse-v1", "control_selection_sha256": sha256(selection_path),
               "recomputed_selected_lambda": selected, "lambda_candidates": candidates, "runs": bindings,
               "reused_fit_count": 12, "reused_checkpoint_count": 2412, "control_optimizer_steps": 0}
    return catalog, cell["config"], implementation, states, receipt


def train_fit(regularization, seed, panels, validation, baseline, catalog_hash, config, out):
    out.mkdir(parents=True)
    (out / "checkpoints").mkdir()
    model = FrontierMetric(next(iter(panels.values()))["inputs"].family_indices, baseline["state"])
    demand(model.eta.numel() == 3557, "treatment representation width differs")
    optimizer = torch.optim.Adam(model.parameters(), lr=0.03, betas=(0.9, 0.999), eps=1e-8, foreach=False, fused=False)
    best, best_key = None, (-math.inf, -math.inf)
    began = time.perf_counter()
    with (out / "epochs.jsonl").open("w") as stream:
        for epoch in range(201):
            optimizer.zero_grad(set_to_none=True)
            objective = backward_panels(model, panels, regularization)
            metrics, per_gallery = validation_score(model, validation)
            key = (metrics["macro_author"]["top1"], metrics["macro_author"]["mrr"])
            checkpoint = out / "checkpoints" / f"epoch-{epoch:03}.pt"
            torch.save({"schema": "slopninja-training-author-scale-checkpoint-v1", "lambda": regularization,
                        "inherited_seed": seed, "epoch": epoch, "config": config, "eta": model.eta.detach().clone(),
                        "optimizer_state": optimizer.state_dict(), "baseline_checkpoint_sha256": baseline["sha256"],
                        "catalog_sha256": catalog_hash, **objective, "validation": metrics}, checkpoint)
            record = {"epoch": epoch, "lambda": regularization, "inherited_seed": seed, **objective,
                      "validation": metrics, "validation_galleries": per_gallery, "eta": model.eta.detach().tolist(),
                      "gradient_l2": model.eta.grad.norm().item(), "checkpoint": str(checkpoint.relative_to(out)),
                      "checkpoint_sha256": sha256(checkpoint), "elapsed_seconds": time.perf_counter() - began}
            stream.write(json.dumps(record, allow_nan=False) + "\n")
            stream.flush()
            if key > best_key:
                best_key, best = key, copy.deepcopy(record)
            if epoch % 50 == 0:
                print(f"300-author treatment lambda {regularization:g} seed {seed} epoch {epoch}: CE {objective['training_cross_entropy']:.6f}, validation top1 {key[0]:.6f}", flush=True)
            if epoch < 200:
                optimizer.step()
                model.constrain()
    stored = torch.load(out / best["checkpoint"], weights_only=True)
    with torch.no_grad():
        model.eta.copy_(stored["eta"])
    demand(validation_score(model, validation)[0] == best["validation"], "selected treatment checkpoint replay differs")
    result = {"status": "complete", "lambda": regularization, "inherited_seed": seed,
              "actual_trainable_parameters": 3557, "selected_epoch": best["epoch"],
              "selected_checkpoint": best["checkpoint"], "selected_checkpoint_sha256": best["checkpoint_sha256"],
              "selected_validation": best["validation"], "selected_eta": best["eta"],
              "selected_training_cross_entropy": best["training_cross_entropy"], "selected_objective": best["objective"],
              "selected_panel_losses": best["panel_losses"], "baseline_checkpoint_sha256": baseline["sha256"],
              "catalog_sha256": catalog_hash, "epochs_sha256": sha256(out / "epochs.jsonl"),
              "elapsed_seconds": time.perf_counter() - began}
    write_json(out / "summary.json", result)
    return result


def train(args):
    demand(not args.out.exists(), "training-scale fit output already exists")
    protocol = read_json(args.protocol)
    validate_protocol(protocol)
    protocol_hash = sha256(args.protocol)
    inputs = load_inputs(args.inputs, protocol, protocol_hash, True)
    demand(inputs["catalog_sha256"] == protocol["catalog_sha256"], "training-scale input catalog changed")
    _, baselines = validate_baselines(args.baseline_fit, protocol)
    validate_incumbent(args.incumbent_fit, protocol, baselines)
    catalog, config, control_implementation, controls, control_receipt = validate_control(args.control_fit, protocol, baselines)
    prior_inputs = read_json(args.control_fit.parent / "training-inputs.json")
    demand(inputs["validation_eligibility_sha256"] == prior_inputs["validation_eligibility_sha256"] ==
           protocol["cohorts"]["validation_eligibility_sha256"], "reused validation eligibility differs from control")
    training_descriptors = protocol["cohorts"]["training_panels"]
    validation_descriptors = protocol["cohorts"]["validation"]
    demand(len(args.training_export) == 3 and len(args.validation_export) == 3, "supply three training and three validation exports")
    demand([row["name"] for row in training_descriptors] == protocol["training"]["panel_order"], "training panel order differs")
    panels, validation, seen, export_audits = {}, {}, set(), {}
    for index, (descriptor, directory) in enumerate(zip(training_descriptors, args.training_export)):
        name = descriptor["name"]
        binding = inputs["exports"][name]
        if index == 0:
            demand(binding["manifest_sha256"] == prior_inputs["exports"]["train"]["manifest_sha256"], "original control training export changed")
        data = load_fixed_export(directory, binding, inputs["catalog_sha256"], protocol, "train", descriptor, catalog, index != 0)
        demand(len(data["rows"]) == protocol["training"]["queries_per_panel"][index] == descriptor["expected_train_queries"],
               "training panel query count differs")
        demand(seen.isdisjoint(data["authors"]), "training panel authors overlap")
        seen.update(data["authors"])
        panels[name] = subset_data(data, config["max_column_indices"])
        export_audits[name] = {"manifest_sha256": data["manifest_sha256"], "residual_audit": data["residual_audit"]}
    training_authors = sorted(seen)
    demand(len(training_authors) == 300, "treatment must have exactly 300 fitting authors")
    for descriptor, directory in zip(validation_descriptors, args.validation_export):
        name = descriptor["name"]
        binding = inputs["exports"][name]
        demand(binding["manifest_sha256"] == prior_inputs["exports"][name]["manifest_sha256"], "reused validation export changed")
        data = load_fixed_export(directory, binding, inputs["catalog_sha256"], protocol, "dev", descriptor, catalog, False)
        demand(seen.isdisjoint(data["authors"]), "training/validation authors overlap")
        seen.update(data["authors"])
        validation[name] = subset_data(data, config["max_column_indices"])
        export_audits[name] = {"manifest_sha256": data["manifest_sha256"], "residual_audit": data["residual_audit"]}
    validation_authors = sorted(seen - set(training_authors))
    demand(validation_authors == control_implementation["validation_author_ids"] and
           sum(len(data["rows"]) for data in validation.values()) == 514, "control validation query/author set changed")
    identity = {}
    for seed in SEEDS:
        model = FrontierMetric(next(iter(panels.values()))["inputs"].family_indices, baselines[seed]["state"])
        for data in [*panels.values(), *validation.values()]:
            demand(torch.equal(model.adjusted_distances(data["inputs"]), data["inputs"].raw_distances),
                   "zero treatment eta changes complete family distances")
        with torch.no_grad():
            original_loss = panel_cross_entropy(model, next(iter(panels.values()))).item()
        demand(abs(original_loss - baselines[seed]["run"]["selected_training_cross_entropy"]) < 1e-9,
               "original panel baseline loss changed")
        with torch.no_grad():
            model.eta.copy_(controls[seed]["eta"])
        metrics, _ = validation_score(model, validation)
        demand(metrics == controls[seed]["run"]["selected_validation"], "reused control validation scores do not reproduce exactly")
        identity[str(seed)] = {"original_panel_baseline_cross_entropy": original_loss,
                               "control_validation_exact_replay": True, "control_validation": metrics}
    args.out.mkdir(parents=True)
    write_json(args.out / "protocol.json", protocol)
    write_json(args.out / "inputs.json", inputs)
    write_json(args.out / "catalog.json", catalog)
    write_json(args.out / "control-reuse.json", control_receipt)
    implementation = {"schema": "slopninja-training-author-scale-implementation-v1", "protocol_sha256": protocol_hash,
                      "inputs_sha256": sha256(args.inputs), "catalog_sha256": inputs["catalog_sha256"],
                      "stored_catalog_sha256": sha256(args.out / "catalog.json"),
                      "source_sha256": {name: sha256(Path(__file__).parent / name) for name in SOURCES},
                      "training_author_ids": training_authors, "validation_author_ids": validation_authors,
                      "training_panels": {name: {"authors": data["authors"], "query_count": len(data["rows"]), "loss_weight": 1/3}
                                          for name, data in panels.items()},
                      "validation_queries": 514, "config": config, "identity_gates": identity,
                      "export_audits": export_audits, "test_exports_opened": False, "control_optimizer_steps": 0}
    write_json(args.out / "implementation.json", implementation)
    runs = []
    for value in LAMBDAS:
        for seed in SEEDS:
            destination = args.out / lambda_name(value) / f"seed-{seed}"
            try:
                run = train_fit(value, seed, panels, validation, baselines[seed], inputs["catalog_sha256"], config, destination)
            except Exception as error:
                destination.mkdir(parents=True, exist_ok=True)
                failure = {"status": "failed", "lambda": value, "inherited_seed": seed,
                           "error": str(error), "traceback": traceback.format_exc()}
                write_json(destination / "failure.json", failure)
                write_json(args.out / "progress.json", {"status": "stopped_after_failed_fit", "runs": runs + [failure]})
                raise
            runs.append(run)
            write_json(args.out / "progress.json", {"completed_new_fits": len(runs), "planned_new_fits": 12, "runs": runs})
    selected, candidates = choose_lambda(runs)
    result = {"schema": "slopninja-frozen-training-author-scale-v1", "selected_lambda": selected,
              "lambda_candidates": candidates, "runs": runs, "config": config, "inherited_seeds": SEEDS,
              "protocol_sha256": protocol_hash, "inputs_sha256": sha256(args.inputs),
              "implementation_sha256": sha256(args.out / "implementation.json"),
              "stored_catalog_sha256": sha256(args.out / "catalog.json"), "catalog_sha256": inputs["catalog_sha256"],
              "control_reuse_sha256": sha256(args.out / "control-reuse.json"),
              "control_selection_sha256": protocol["control_selection_sha256"],
              "completed_new_fits": 12, "failed_new_fits": 0, "control_optimizer_steps": 0,
              **{key: protocol[key] for key in IDENTITY}}
    write_json(args.out / "selection.json", result)
    print(f"FROZEN 300-author treatment lambda {selected:g}; selection SHA256 {sha256(args.out / 'selection.json')}", flush=True)


def treatment_states(fit, selection, baselines):
    states = {}
    for seed in SEEDS:
        matching = [row for row in selection["runs"] if row["lambda"] == selection["selected_lambda"] and row["inherited_seed"] == seed]
        demand(len(matching) == 1, "selected treatment seed is missing")
        run = matching[0]
        path = fit / lambda_name(run["lambda"]) / f"seed-{seed}" / run["selected_checkpoint"]
        demand(path.resolve().is_relative_to(fit.resolve()) and sha256(path) == run["selected_checkpoint_sha256"],
               "selected treatment checkpoint changed")
        stored = torch.load(path, weights_only=True)
        demand(stored["schema"] == "slopninja-training-author-scale-checkpoint-v1" and stored["config"] == selection["config"],
               "selected treatment checkpoint geometry differs")
        demand(stored["lambda"] == run["lambda"] and stored["inherited_seed"] == seed and stored["epoch"] == run["selected_epoch"],
               "selected treatment checkpoint identity differs")
        demand(stored["catalog_sha256"] == selection["catalog_sha256"] and stored["baseline_checkpoint_sha256"] == baselines[seed]["sha256"],
               "selected treatment checkpoint source differs")
        demand(stored["eta"].tolist() == run["selected_eta"] and stored["validation"] == run["selected_validation"],
               "selected treatment eta or validation changed")
        demand(bool(torch.isfinite(stored["eta"]).all()) and bool((stored["eta"].abs() <= math.log(2)).all()),
               "selected treatment violates coordinate bounds")
        states[seed] = {"eta": stored["eta"], "run": run}
    return states


def evaluate(args):
    demand(not args.out.exists(), "training-scale test output already exists")
    protocol = read_json(args.protocol)
    validate_protocol(protocol)
    selection_path = args.fit / "selection.json"
    selection = read_json(selection_path)
    demand(selection["schema"] == "slopninja-frozen-training-author-scale-v1" and
           selection["protocol_sha256"] == sha256(args.protocol), "training-scale selection/protocol differs")
    demand(read_json(args.fit / "protocol.json") == protocol, "stored training-scale protocol changed")
    selected, candidates = choose_lambda(selection["runs"])
    demand(selected == selection["selected_lambda"] and candidates == selection["lambda_candidates"], "frozen treatment lambda selection differs")
    for filename, key in [("implementation.json", "implementation_sha256"), ("catalog.json", "stored_catalog_sha256"),
                          ("control-reuse.json", "control_reuse_sha256")]:
        demand(sha256(args.fit / filename) == selection[key], f"frozen training-scale artifact changed: {filename}")
    implementation = read_json(args.fit / "implementation.json")
    for name, digest in implementation["source_sha256"].items():
        demand(sha256(Path(__file__).parent / name) == digest, f"frozen training-scale scorer source changed: {name}")
    _, baselines = validate_baselines(args.baseline_fit, protocol)
    catalog, config, _, controls, receipt = validate_control(args.control_fit, protocol, baselines)
    demand(receipt == read_json(args.fit / "control-reuse.json") and config == selection["config"], "frozen control reuse differs")
    incumbent_catalog, incumbents = validate_incumbent(args.incumbent_fit, protocol, baselines)
    previous_indices = incumbent_indices(catalog, incumbent_catalog)
    treatments = treatment_states(args.fit, selection, baselines)
    inputs = load_inputs(args.inputs, protocol, sha256(args.protocol), False, sha256(selection_path))
    demand(inputs["catalog_sha256"] == selection["catalog_sha256"] and len(args.export) == 6,
           "training-scale test catalog or gallery count differs")
    seen = set(implementation["training_author_ids"]) | set(implementation["validation_author_ids"])
    galleries = {}
    for descriptor, directory in zip(protocol["cohorts"]["test"], args.export):
        name = descriptor["name"]
        data = load_fixed_export(directory, inputs["exports"][name], inputs["catalog_sha256"], protocol, "test", descriptor, catalog, True)
        demand(seen.isdisjoint(data["authors"]), "training-scale test authors overlap fitting or other galleries")
        seen.update(data["authors"])
        galleries[name] = data
    args.out.mkdir(parents=True)
    write_json(args.out / "input-validation.json", {"protocol_sha256": sha256(args.protocol), "inputs_sha256": sha256(args.inputs),
               "selection_sha256": sha256(selection_path), "export_manifest_sha256": {name: data["manifest_sha256"] for name, data in galleries.items()},
               "residual_audits": {name: data["residual_audit"] for name, data in galleries.items()}})
    variants = [("family_baseline", None), ("incumbent", None), ("control_100", None), ("treatment_300", None),
                ("treatment_word_eta_only", "word"), ("treatment_grammar_eta_only", "grammar"), ("treatment_zero_eta", "neither")]
    lexical = {catalog["families"].index(name) for name in ["word", "word_bigram"]}
    results, baseline_logits = {}, {}
    for variant, reset in variants:
        indices = previous_indices if variant == "incumbent" else config["max_column_indices"]
        data_by_gallery = {name: subset_data(data, indices) for name, data in galleries.items()}
        seed_records, arrays, durations = [], {}, []
        for seed in SEEDS:
            model = FrontierMetric(next(iter(data_by_gallery.values()))["inputs"].family_indices, baselines[seed]["state"])
            selected_state = None
            if variant == "incumbent":
                selected_state = incumbents[seed]
            elif variant == "control_100":
                selected_state = controls[seed]
            elif variant.startswith("treatment"):
                selected_state = treatments[seed]
            if selected_state is not None:
                with torch.no_grad():
                    model.eta.copy_(selected_state["eta"])
            if reset is not None:
                reset_eta(model, reset, lexical)
            summaries = {}
            for name, data in data_by_gallery.items():
                began = time.perf_counter()
                summary, logits, rows = score(model, data, baseline=variant == "family_baseline")
                durations.append(time.perf_counter() - began)
                if variant == "family_baseline":
                    baseline_logits[(seed, name)] = logits
                if variant == "treatment_zero_eta":
                    demand(np.array_equal(logits, baseline_logits[(seed, name)]), "zero treatment eta does not exactly restore family baseline")
                arrays.setdefault(name, []).append(rows)
                path = args.out / f"{variant}-seed-{seed}-{name}-scores.json"
                write_json(path, {"query_ids": [row["id"] for row in data["rows"]], "candidate_authors": data["authors"],
                                  "logits": logits.tolist(), "per_query_metrics": rows.tolist()})
                summaries[name] = {"metrics": summary, "scores_sha256": sha256(path)}
            run = None if selected_state is None else selected_state.get("run")
            seed_records.append({"inherited_seed": seed, "selected_epoch": None if run is None else run["selected_epoch"],
                                 "galleries": summaries, "pooled": pool({name: row["metrics"] for name, row in summaries.items()})})
        average = {name: summarize_rows(np.mean(rows, 0), galleries[name]["query_authors"]) for name, rows in arrays.items()}
        results[variant] = {"seed_mean": pool(average), "galleries": average, "seeds": seed_records,
                            "coordinate_count": 0 if variant == "family_baseline" else len(indices), "conditional_eta_reset": reset,
                            "scoring_seconds_including_metric_aggregation": sum(durations),
                            "seed_macro_top1_range": [min(row["pooled"]["macro_author"]["top1"] for row in seed_records),
                                                      max(row["pooled"]["macro_author"]["top1"] for row in seed_records)]}
        print(f"Scored fixed training-scale variant {variant}", flush=True)
    pairs = [("treatment_minus_control", "treatment_300", "control_100"),
             ("treatment_minus_incumbent", "treatment_300", "incumbent"),
             ("control_minus_incumbent", "control_100", "incumbent"),
             ("treatment_minus_family_baseline", "treatment_300", "family_baseline"),
             ("treatment_minus_word_eta_only", "treatment_300", "treatment_word_eta_only"),
             ("treatment_minus_grammar_eta_only", "treatment_300", "treatment_grammar_eta_only")]
    comparisons = {}
    for name, left, right in pairs:
        interval = paired_interval({gallery: row["per_author"] for gallery, row in results[left]["galleries"].items()},
                                   {gallery: row["per_author"] for gallery, row in results[right]["galleries"].items()})
        comparisons[name] = {"left": left, "right": right, **interval}
    primary = comparisons["treatment_minus_control"]["metrics"]["top1"]
    result = {"schema": "slopninja-training-author-scale-test-result-v1", "selected_treatment_lambda": selected,
              "control_lambda": protocol["control_selected_lambda"], "models": results, "comparisons": comparisons,
              "primary": {"comparison": "treatment_minus_control", "metric": "macro_author.top1", **primary,
                          "positive_lower_95_bound": primary["percentile_95"][0] > 0},
              "protocol_sha256": sha256(args.protocol), "selection_sha256": sha256(selection_path), "inputs_sha256": sha256(args.inputs),
              "training_calls_during_test": 0, "control_optimizer_steps": 0, "external_model_calls": 0, "detector_calls": 0}
    write_json(args.out / "report.json", result)
    print(f"Training scale test: treatment minus control {primary['mean_difference']:.6f}, 95% interval {primary['percentile_95']}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    fitting = commands.add_parser("train")
    fitting.add_argument("--training-export", type=Path, action="append", required=True)
    fitting.add_argument("--validation-export", type=Path, action="append", required=True)
    testing = commands.add_parser("evaluate")
    testing.add_argument("--fit", type=Path, required=True)
    testing.add_argument("--export", type=Path, action="append", required=True)
    for command in [fitting, testing]:
        command.add_argument("--control-fit", type=Path, required=True)
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

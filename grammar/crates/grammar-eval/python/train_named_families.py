"""Fit and evaluate frozen matched raw14/raw15 named-family spline calibrations."""
import argparse
import copy
import json
import math
import platform
import shutil
import time
import traceback
from pathlib import Path

import numpy as np
import torch

from frontier_common import demand, read_json, sha256, write_json
from named_family_io import load_inputs, validate_protocol
from named_family_metrics import METRICS, paired_interval, pool, score, summarize, validation_score
from named_family_model import NamedFamilyMetric, backward_panels, fit_shared_knots


SOURCES = ["train_named_families.py", "named_family_model.py", "named_family_metrics.py", "named_family_io.py",
           "test_named_families.py", "frontier_common.py", "training-lock.json"]


def configure(protocol):
    torch.set_default_dtype(torch.float64)
    torch.set_num_threads(protocol["optimization"]["threads"])
    torch.use_deterministic_algorithms(True)


def model_for(protocol, arm, calibration, seed):
    model = NamedFamilyMetric(protocol["families"], protocol["baseline_families"], arm["families"],
                              calibration["knots"], seed, protocol["initialization"])
    demand(sum(parameter.numel() for parameter in model.parameters()) == arm["trainable_parameter_count"],
           "actual trainable parameter count differs")
    return model


def complete_runs(runs, protocol):
    expected = {(arm["name"], seed) for arm in protocol["arms"] for seed in protocol["seeds"]}
    actual = [(run["arm"]["name"], run["seed"]) for run in runs]
    demand(len(runs) == len(expected) and set(actual) == expected and all(run["status"] == "complete" for run in runs),
           "all six prescribed fits must complete exactly once before selection")
    return runs


def initialization_gates(protocol, calibration):
    gates = {}
    for seed in protocol["seeds"]:
        baseline, extended = [model_for(protocol, arm, calibration, seed) for arm in protocol["arms"]]
        indices = [extended.families.index(family) for family in baseline.families]
        demand(torch.equal(baseline.theta.detach(), extended.theta.detach()[indices]), "shared named initial theta differs")
        left = baseline.theta.softmax(0) / baseline.log_tau.exp()
        right = (extended.theta.softmax(0) / extended.log_tau.exp())[indices]
        error = (left - right).abs().max().item()
        demand(error < 1e-13, "shared initial effective coefficients differ")
        gates[str(seed)] = {"maximum_effective_coefficient_error": error, "raw14": baseline.parameter_json(),
                            "raw15": extended.parameter_json(), "named_initialization": extended.initialization}
    return gates


def train_fit(protocol, arm, seed, calibration, training, validation, output):
    output.mkdir(parents=True)
    (output / "checkpoints").mkdir()
    model = model_for(protocol, arm, calibration, seed)
    optimization = protocol["optimization"]
    optimizer = torch.optim.Adam(model.parameters(), lr=optimization["learning_rate"],
                                 betas=(optimization["beta1"], optimization["beta2"]), eps=optimization["epsilon"],
                                 foreach=False, fused=False)
    best, best_key = None, (-math.inf, -math.inf)
    started = time.perf_counter()
    with (output / "epochs.jsonl").open("w") as stream:
        for epoch in range(optimization["epochs"] + 1):
            optimizer.zero_grad(set_to_none=True)
            losses = backward_panels(model, training)
            validation_metrics, gallery_metrics = validation_score(model, validation)
            key = tuple(validation_metrics["macro_author"][name] for name in ("top1", "mrr"))
            demand(all(math.isfinite(value) for value in key), "nonfinite checkpoint selection metrics")
            checkpoint = output / "checkpoints" / f"epoch-{epoch:03}.pt"
            torch.save({"schema": "slopninja-named-family-calibration-checkpoint-v1", "arm": arm, "seed": seed,
                        "epoch": epoch, "model_state": model.state_dict(), "optimizer_state": optimizer.state_dict(),
                        "calibration": calibration, **losses, "validation": validation_metrics}, checkpoint)
            record = {"epoch": epoch, "arm": arm["name"], "seed": seed, **losses,
                      "validation": validation_metrics, "validation_galleries": gallery_metrics,
                      "parameters": model.parameter_json(),
                      "gradient_l2": torch.cat([parameter.grad.reshape(-1) for parameter in model.parameters()]).norm().item(),
                      "checkpoint": str(checkpoint.relative_to(output)), "checkpoint_sha256": sha256(checkpoint),
                      "elapsed_seconds": time.perf_counter() - started}
            stream.write(json.dumps(record, allow_nan=False) + "\n")
            stream.flush()
            if key > best_key:
                best, best_key = copy.deepcopy(record), key
            if epoch % 50 == 0:
                print(f"{arm['name']} seed {seed} epoch {epoch}: CE {losses['training_cross_entropy']:.6f}, validation top1 {key[0]:.6f}", flush=True)
            if epoch < optimization["epochs"]:
                optimizer.step()
                model.constrain(optimization["log_temperature_bounds"], optimization["log_slope_bounds"])
    state = torch.load(output / best["checkpoint"], map_location="cpu", weights_only=True)
    model.load_state_dict(state["model_state"])
    demand(validation_score(model, validation)[0] == best["validation"], "selected checkpoint validation replay differs")
    result = {"status": "complete", "arm": arm, "seed": seed,
              "actual_trainable_parameters": sum(parameter.numel() for parameter in model.parameters()),
              "selected_epoch": best["epoch"], "selected_checkpoint": best["checkpoint"],
              "selected_checkpoint_sha256": best["checkpoint_sha256"], "selected_parameters": best["parameters"],
              "selected_validation": best["validation"], "selected_training_cross_entropy": best["training_cross_entropy"],
              "selected_panel_cross_entropies": best["panel_cross_entropies"], "epochs_sha256": sha256(output / "epochs.jsonl"),
              "elapsed_seconds": time.perf_counter() - started}
    write_json(output / "summary.json", result)
    return result


def selected_model(fit, run, protocol, calibration):
    output = fit / run["arm"]["name"] / f"seed-{run['seed']}"
    demand(read_json(output / "summary.json") == run and sha256(output / "epochs.jsonl") == run["epochs_sha256"],
           "selected run summary or trajectory changed")
    checkpoint = (output / run["selected_checkpoint"]).resolve()
    demand(checkpoint.is_relative_to(output.resolve()) and sha256(checkpoint) == run["selected_checkpoint_sha256"],
           "selected checkpoint changed or escapes its run")
    state = torch.load(checkpoint, map_location="cpu", weights_only=True)
    demand(state["schema"] == "slopninja-named-family-calibration-checkpoint-v1" and state["arm"] == run["arm"]
           and state["seed"] == run["seed"] and state["epoch"] == run["selected_epoch"]
           and state["calibration"] == calibration and state["validation"] == run["selected_validation"],
           "selected checkpoint identity/calibration differs")
    model = model_for(protocol, run["arm"], calibration, run["seed"])
    expected_indices = model.family_indices.clone()
    expected_knots = model.knots.clone()
    for name, value in state["model_state"].items():
        expected = model.state_dict()[name]
        demand(value.dtype == expected.dtype and value.shape == expected.shape and bool(torch.isfinite(value).all()),
               "selected tensor shape/dtype/finiteness differs")
    model.load_state_dict(state["model_state"])
    demand(torch.equal(model.family_indices, expected_indices) and torch.equal(model.knots, expected_knots),
           "selected fixed family mapping or knots differ")
    demand(model.parameter_json() == run["selected_parameters"], "selected parameter JSON differs from checkpoint")
    for value, bounds in ((model.log_tau, protocol["optimization"]["log_temperature_bounds"]),
                          (model.log_slope, protocol["optimization"]["log_slope_bounds"])):
        demand(bool(((value >= bounds[0]) & (value <= bounds[1])).all()), "selected parameter outside frozen bounds")
    return model


def export_portable(fit, selection, protocol, calibration):
    directory = fit / "portable"
    demand(not directory.exists(), "portable output already exists")
    artifacts = []
    for run in complete_runs(selection["runs"], protocol):
        model = selected_model(fit, run, protocol, calibration)
        parameters = model.parameter_json()
        artifact = {"schema": "slopninja-named-family-metric-v1", "families": model.families,
                    "source_space_id": protocol["source_space_id"], "reference_space_sha256": protocol["reference_space_sha256"],
                    "feature_schema": protocol["baseline_feature_schema"] if run["arm"]["name"] == "raw14" else protocol["feature_schema"],
                    "parser_identity": protocol["parser_identity"],
                    **{key: parameters[key] for key in ("family_weights", "knots", "slopes", "temperature")},
                    "provenance_sha256": {"selection": sha256(fit / "selection.json"), "protocol": selection["protocol_sha256"],
                                          "calibration": selection["calibration_sha256"], "implementation": selection["implementation_sha256"],
                                          "checkpoint": run["selected_checkpoint_sha256"]}}
        artifacts.append((run, artifact))
    directory.mkdir()
    bindings = []
    for run, artifact in artifacts:
        path = directory / f"{run['arm']['name']}-seed-{run['seed']}.json"
        write_json(path, artifact)
        bindings.append({"arm": run["arm"]["name"], "seed": run["seed"], "selected_epoch": run["selected_epoch"],
                         "path": path.name, "sha256": sha256(path)})
    write_json(directory / "manifest.json", {"schema": "slopninja-portable-named-family-metrics-v1", "models": bindings,
                                            "selection_sha256": sha256(fit / "selection.json"), "training_calls": 0})


def train(args, protocol):
    demand(not args.out.exists(), "training output already exists")
    inputs, data, eligibility_hashes = load_inputs(args.protocol, protocol, args.protocol_sha256, args.inputs, args.inputs_sha256, True)
    training, validation = data["training"], data["validation"]
    calibration = fit_shared_knots(training, protocol["baseline_families"], protocol["knots"]["count"])
    gates = initialization_gates(protocol, calibration)
    args.out.mkdir(parents=True)
    snapshot = args.out / "executed-source"
    snapshot.mkdir()
    for name in SOURCES:
        shutil.copy2(Path(__file__).parent / name, snapshot / name)
    write_json(args.out / "protocol.json", protocol)
    write_json(args.out / "inputs.json", inputs)
    write_json(args.out / "calibration.json", calibration)
    source_bindings = {name: sha256(Path(__file__).parent / name) for name in SOURCES}
    implementation = {"schema": "slopninja-named-family-calibration-implementation-v1",
                      "protocol_sha256": args.protocol_sha256, "inputs_sha256": args.inputs_sha256,
                      "calibration_sha256": sha256(args.out / "calibration.json"), "source_sha256": source_bindings,
                      "training_author_ids": sorted(author for panel in training.values() for author in panel["authors"]),
                      "validation_author_ids": sorted(author for gallery in validation.values() for author in gallery["authors"]),
                      "all_allocated_training_validation_author_ids": sorted({author for binding in inputs["exports"].values()
                                                                            for author in binding["eligible_author_ids"] + binding["excluded_author_ids"]}),
                      "initialization_gates": gates, "eligibility_sha256": eligibility_hashes,
                      "training_queries": sum(len(panel["rows"]) for panel in training.values()),
                      "validation_queries": sum(len(gallery["rows"]) for gallery in validation.values()),
                      "runtime": {"python": platform.python_version(), "torch": str(torch.__version__), "numpy": np.__version__,
                                  "threads": torch.get_num_threads(), "dtype": "float64", "deterministic": True},
                      "test_arrays_opened": False}
    write_json(args.out / "implementation.json", implementation)
    runs = []
    for arm in protocol["arms"]:
        for seed in protocol["seeds"]:
            output = args.out / arm["name"] / f"seed-{seed}"
            try:
                result = train_fit(protocol, arm, seed, calibration, training, validation, output)
            except Exception as error:
                result = {"status": "failed", "arm": arm, "seed": seed, "error": f"{type(error).__name__}: {error}",
                          "traceback": traceback.format_exc()}
                output.mkdir(parents=True, exist_ok=True)
                write_json(output / "failure.json", result)
                runs.append(result)
                write_json(args.out / "progress.json", {"status": "failed; study stopped", "runs": runs})
                raise
            runs.append(result)
            write_json(args.out / "progress.json", {"status": "training", "runs": runs})
    complete_runs(runs, protocol)
    selection = {"schema": "slopninja-frozen-named-family-calibration-v1", "runs": runs,
                 "arms": protocol["arms"], "seeds": protocol["seeds"], "completed_fits": len(runs), "failed_fits": 0,
                 "protocol_sha256": args.protocol_sha256, "inputs_sha256": args.inputs_sha256,
                 "implementation_sha256": sha256(args.out / "implementation.json"),
                 "calibration_sha256": sha256(args.out / "calibration.json"),
                 "source_space_id": protocol["source_space_id"], "baseline_feature_schema": protocol["baseline_feature_schema"],
                 "feature_schema": protocol["feature_schema"], "parser_identity": protocol["parser_identity"]}
    write_json(args.out / "selection.json", selection)
    export_portable(args.out, selection, protocol, calibration)
    write_json(args.out / "progress.json", {"status": "selection frozen", "selection_sha256": sha256(args.out / "selection.json"),
                                          "completed_fits": len(runs), "failed_fits": 0})
    print(f"FROZEN matched named-family selection SHA256 {sha256(args.out / 'selection.json')}", flush=True)


def evaluate(args, protocol):
    demand(args.fit and not args.out.exists(), "evaluation requires a fit and fresh output directory")
    selection_path = args.fit / "selection.json"
    selection = read_json(selection_path)
    demand(selection["schema"] == "slopninja-frozen-named-family-calibration-v1" and selection["protocol_sha256"] == args.protocol_sha256,
           "frozen model protocol differs")
    demand(read_json(args.fit / "protocol.json") == protocol, "stored fitting protocol differs")
    complete_runs(selection["runs"], protocol)
    for filename, key in (("implementation.json", "implementation_sha256"), ("calibration.json", "calibration_sha256")):
        demand(sha256(args.fit / filename) == selection[key], f"frozen fitting artifact changed: {filename}")
    implementation, calibration = read_json(args.fit / "implementation.json"), read_json(args.fit / "calibration.json")
    for name, digest in implementation["source_sha256"].items():
        demand(sha256(Path(__file__).parent / name) == digest and sha256(args.fit / "executed-source" / name) == digest,
               f"frozen fitting source changed: {name}")
    inputs, data, eligibility_hashes = load_inputs(args.protocol, protocol, args.protocol_sha256, args.inputs, args.inputs_sha256,
                                                  False, sha256(selection_path))
    test = data["test"]
    allocated = {author for binding in inputs["exports"].values() for author in binding["eligible_author_ids"] + binding["excluded_author_ids"]}
    demand(allocated.isdisjoint(implementation["all_allocated_training_validation_author_ids"]), "test allocation overlaps fitting/selection authors")
    loaded = {(run["arm"]["name"], run["seed"]): selected_model(args.fit, run, protocol, calibration) for run in selection["runs"]}
    args.out.mkdir(parents=True)
    models = {}
    for arm in protocol["arms"]:
        seed_rows, seed_reports = {}, []
        started = time.perf_counter()
        for seed in protocol["seeds"]:
            model = loaded[(arm["name"], seed)]
            summaries, rows = {}, {}
            run = next(run for run in selection["runs"] if run["arm"]["name"] == arm["name"] and run["seed"] == seed)
            for name, gallery in test.items():
                summary, logits, metrics = score(model, gallery)
                summaries[name], rows[name] = summary, metrics
                write_json(args.out / f"{arm['name']}-seed-{seed}-{name}-scores.json",
                           {"query_ids": [row["id"] for row in gallery["rows"]], "candidate_authors": gallery["authors"],
                            "logits": logits.tolist(), "per_query_metrics": metrics.tolist()})
            seed_rows[seed] = rows
            seed_reports.append({"seed": seed, "selected_epoch": run["selected_epoch"], "galleries": summaries, "pooled": pool(summaries)})
        galleries = {name: summarize(np.mean([seed_rows[seed][name] for seed in protocol["seeds"]], axis=0), gallery["query_authors"])
                     for name, gallery in test.items()}
        models[arm["name"]] = {"seed_mean": pool(galleries), "galleries": galleries, "seeds": seed_reports,
                               "actual_trainable_parameters": arm["trainable_parameter_count"], "families": arm["families"],
                               "scoring_seconds_including_metrics": time.perf_counter() - started,
                               "seed_macro_top1_range": [min(row["pooled"]["macro_author"]["top1"] for row in seed_reports),
                                                         max(row["pooled"]["macro_author"]["top1"] for row in seed_reports)]}
    comparison = paired_interval({name: value["per_author"] for name, value in models["raw15"]["galleries"].items()},
                                 {name: value["per_author"] for name, value in models["raw14"]["galleries"].items()})
    primary = {"comparison": "raw15_minus_raw14", "metric": "top1", **comparison["metrics"]["top1"],
               "superiority_criterion_met": comparison["metrics"]["top1"]["percentile_95"][0] > 0}
    report = {"schema": "slopninja-named-family-calibration-test-result-v1", "models": models,
              "comparisons": {"raw15_minus_raw14": comparison}, "primary": primary,
              "protocol_sha256": args.protocol_sha256, "selection_sha256": sha256(selection_path), "inputs_sha256": args.inputs_sha256,
              "eligibility_sha256": eligibility_hashes, "training_calls_during_test": 0, "external_model_calls": 0, "detector_calls": 0}
    write_json(args.out / "report.json", report)
    write_json(args.out / "manifest.json", {"schema": "slopninja-named-family-calibration-test-manifest-v1",
                                           "protocol_sha256": args.protocol_sha256, "selection_sha256": sha256(selection_path),
                                           "input_sha256": args.inputs_sha256,
                                           "artifacts_sha256": {path.name: sha256(path) for path in sorted(args.out.glob("*.json"))}})
    print(json.dumps(primary), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["train", "evaluate"])
    for name in ("protocol", "inputs", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    for name in ("protocol-sha256", "inputs-sha256"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--fit", type=Path)
    args = parser.parse_args()
    demand(sha256(args.protocol) == args.protocol_sha256, "protocol differs from supplied frozen SHA")
    protocol = read_json(args.protocol)
    validate_protocol(protocol)
    configure(protocol)
    (train if args.mode == "train" else evaluate)(args, protocol)


if __name__ == "__main__":
    main()

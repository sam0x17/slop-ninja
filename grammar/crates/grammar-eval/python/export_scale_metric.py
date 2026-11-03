"""Export all three frozen 300-author treatment states as portable Metric v1 JSON."""
import argparse
import hashlib
import json
import math
from pathlib import Path

import torch


IDENTITY = ("source_space_id", "feature_schema", "parser_identity")
GEOMETRY = ("family_index", "family", "feature", "kind", "scale", "original_family_axis_count")
SEEDS = [0, 17, 29]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read(path):
    return json.loads(path.read_text())


def index_hash(indices):
    return hashlib.sha256(json.dumps(indices, separators=(",", ":")).encode()).hexdigest()


def bound_path(root, relative):
    path = (root / relative).resolve()
    require(path.is_relative_to(root.resolve()), "artifact escapes its bound directory")
    return path


def selected_coordinates(catalog, config, protocol):
    """Gather the actual learned axes; the maximal catalog is never truncated by prefix."""
    require(catalog["schema"] == "slopninja-coordinate-frontier-catalog-v1", "unsupported maximal catalog")
    require(config["name"] == protocol["coordinate_selection"]["fixed_subset"] == "all_m2",
            "expected the frozen all_m2 representation")
    subset = catalog["subsets"]["all_m2"]
    require(all(config[key] == value for key, value in subset.items()), "selected config differs from catalog subset")
    indices = subset["max_column_indices"]
    require(all(type(index) is int for index in indices) and indices == sorted(set(indices)),
            "subset indices must be sorted unique integers")
    require(indices and indices[0] >= 0 and indices[-1] < len(catalog["coordinates"]), "subset index out of range")
    require(len(catalog["coordinates"]) == protocol["maximal_coordinate_count"], "maximal catalog size differs")
    require(len(indices) == subset["coordinate_count"] == protocol["coordinate_count"], "selected coordinate count differs")
    require(index_hash(indices) == subset["indices_sha256"] == protocol["fixed_subset_indices_sha256"]
            == protocol["coordinate_selection"]["fixed_subset_indices_sha256"], "subset index hash differs")
    return [catalog["coordinates"][index] for index in indices]


def complete_geometry(catalog, space, coordinates):
    """Keep the original numeric denominators and every numeric axis, including inactive ones."""
    names = catalog["families"]
    require(names == sorted(space["family_schemas"]) and len(names) == len(set(names)), "family order differs")
    axes = {name: {} for name in names}
    for axis in space["axes"]:
        family, feature = axis["family"], axis["feature"]
        require(family in axes and feature not in axes[family], "unknown or duplicate original axis")
        require(axis["kind"] == space["family_schemas"][family]["kind"], "original axis kind differs")
        require(math.isfinite(axis["scale"]) and axis["scale"] > 0, "invalid original scale")
        axes[family][feature] = axis
    families = []
    for name in names:
        kind = space["family_schemas"][name]["kind"]
        require(kind in ("distribution", "metric", "rate"), "unsupported family geometry")
        families.append({"name": name, "kind": kind, "original_axis_count": len(axes[name]),
                         "numerical_axes": [] if kind == "distribution" else
                         [{"feature": feature, "scale": axis["scale"]} for feature, axis in sorted(axes[name].items())]})
    keys = [(axis["family"], axis["feature"]) for axis in coordinates]
    require(keys == sorted(set(keys)), "selected axes must be sorted and unique")
    for axis in coordinates:
        family, feature = axis["family"], axis["feature"]
        require(family in axes and feature in axes[family], "selected axis absent from original space")
        original = axes[family][feature]
        require(axis["family_index"] == names.index(family) and axis["kind"] == original["kind"],
                "selected axis family geometry differs")
        require(axis["scale"] == original["scale"] and axis["original_family_axis_count"] == len(axes[family]),
                "selected axis scale or full-family denominator differs")
        require(axis["kind"] != "distribution" or axis["scale"] == 1, "categorical scale must remain one")
    return families


def validate_selection(selection, protocol):
    require(selection["schema"] == "slopninja-frozen-training-author-scale-v1", "unsupported treatment selection")
    require(selection["inherited_seeds"] == protocol["inherited_seeds"] == SEEDS, "must export all three inherited seeds")
    require(selection["completed_new_fits"] == 12 and selection["failed_new_fits"] == 0,
            "treatment selection requires twelve completed fits")
    require(selection["control_optimizer_steps"] == 0, "control was refitted")
    runs = selection["runs"]
    expected = {(regularization, seed) for regularization in protocol["lambda_grid"] for seed in SEEDS}
    actual = [(run["lambda"], run["inherited_seed"]) for run in runs]
    require(len(actual) == len(expected) == 12 and set(actual) == expected,
            "missing or duplicate treatment runs")
    require(all(run["status"] == "complete" and run["actual_trainable_parameters"] == protocol["coordinate_count"]
                for run in runs), "incomplete treatment run or parameter count mismatch")
    candidates = []
    for regularization in protocol["lambda_grid"]:
        group = [run for run in runs if run["lambda"] == regularization]
        means = {key: sum(run["selected_validation"]["macro_author"][key] for run in group) / 3
                 for key in ("top1", "mrr")}
        require(all(math.isfinite(value) for value in means.values()), "nonfinite selected validation metrics")
        candidates.append((means["top1"], means["mrr"], regularization))
    require(selection["selected_lambda"] == max(candidates)[2], "frozen regularization selection does not reproduce")
    return [next(run for run in runs if run["lambda"] == selection["selected_lambda"] and run["inherited_seed"] == seed)
            for seed in SEEDS]


def validated_eta(stored, run, selection, protocol):
    require(stored["schema"] == "slopninja-training-author-scale-checkpoint-v1", "unsupported treatment checkpoint")
    require(stored["inherited_seed"] == run["inherited_seed"] and stored["epoch"] == run["selected_epoch"]
            and stored["lambda"] == run["lambda"] == selection["selected_lambda"], "treatment checkpoint identity differs")
    require(stored["config"] == selection["config"] and stored["catalog_sha256"] == selection["catalog_sha256"]
            == run["catalog_sha256"], "treatment geometry binding differs")
    require(stored["baseline_checkpoint_sha256"] == run["baseline_checkpoint_sha256"], "treatment baseline binding differs")
    require(stored["validation"] == run["selected_validation"], "treatment selected metrics differ")
    eta = stored["eta"]
    require(isinstance(eta, torch.Tensor) and eta.dtype == torch.float64 and eta.shape == (protocol["coordinate_count"],)
            and bool(torch.isfinite(eta).all()), "treatment eta must be finite float64 with exactly the selected width")
    lower, upper = map(math.log, protocol["optimization"]["weight_bounds"])
    require(bool(((eta >= lower) & (eta <= upper)).all()) and eta.tolist() == run["selected_eta"],
            "treatment eta differs from selection or frozen bounds")
    return eta


def portable_coordinates(coordinates, eta):
    require(len(coordinates) == eta.numel(), "coordinate parameter width differs")
    return [{**{key: axis[key] for key in GEOMETRY}, "log_multiplier": log_weight, "multiplier": weight}
            for axis, log_weight, weight in zip(coordinates, eta.tolist(), eta.exp().tolist())]


def export(args):
    require(not args.out.exists(), "use a fresh output directory")
    require(sha256(args.selection) == args.selection_sha256, "selection SHA differs from the supplied frozen binding")
    require(sha256(args.protocol) == args.protocol_sha256, "protocol SHA differs from the supplied frozen binding")
    selection, protocol, catalog, space = map(read, (args.selection, args.protocol, args.catalog, args.space))
    require(protocol["schema"] == "slopninja-training-author-scale-protocol-v1", "unsupported protocol")
    require(selection["protocol_sha256"] == args.protocol_sha256, "selected protocol differs")
    require(selection["catalog_sha256"] == protocol["catalog_sha256"] == sha256(args.catalog), "maximal catalog changed")
    require(protocol["coordinate_selection"]["catalog_sha256"] == protocol["catalog_sha256"], "coordinate catalog binding differs")
    require(selection["control_selection_sha256"] == protocol["control_selection_sha256"], "frozen control selection binding differs")
    require(protocol["baseline_model"] == "combined_spline", "expected the frozen combined spline family baseline")
    require(protocol["reference_space_sha256"] == sha256(args.space), "original space changed")
    require(protocol["coordinate_count"] == 3557 and protocol["maximal_coordinate_count"] == 7353,
            "unexpected frozen representation size")
    require(protocol["optimization"]["weight_bounds"] == [0.5, 2], "frozen coordinate bounds differ")
    for key in IDENTITY:
        require(selection[key] == protocol[key] == catalog[key], f"source identity differs: {key}")
        require(space["id" if key == "source_space_id" else key] == selection[key], f"space identity differs: {key}")
    require(catalog["families"] == protocol["families"] and len(catalog["families"]) == 14, "expected frozen fourteen-family scorer")
    selected_runs = validate_selection(selection, protocol)
    coordinates = selected_coordinates(catalog, selection["config"], protocol)
    families = complete_geometry(catalog, space, coordinates)
    fit = args.selection.parent
    require(read(fit / "protocol.json") == protocol, "stored fit protocol differs")
    for filename, key in (("implementation.json", "implementation_sha256"), ("catalog.json", "stored_catalog_sha256"),
                          ("control-reuse.json", "control_reuse_sha256")):
        require(sha256(fit / filename) == selection[key], f"selected fit artifact changed: {filename}")
    implementation = read(fit / "implementation.json")
    for key in ("protocol_sha256", "inputs_sha256", "catalog_sha256", "stored_catalog_sha256", "config"):
        require(implementation[key] == selection[key], f"implementation binding differs: {key}")
    for name, digest in implementation["source_sha256"].items():
        require(sha256(bound_path(Path(__file__).parent, name)) == digest, f"frozen training source changed: {name}")
    stored_catalog = read(fit / "catalog.json")
    require(all(catalog[key] == value for key, value in stored_catalog.items()), "stored and maximal catalog differ")
    input_path = args.protocol.parent / "training-inputs.json"
    require(sha256(input_path) == selection["inputs_sha256"], "training input manifest changed")
    inputs = read(input_path)
    require(inputs["protocol_sha256"] == args.protocol_sha256 and inputs["catalog_sha256"] == selection["catalog_sha256"],
            "training input source binding differs")
    base_selection_path = args.baseline_fit / "selection.json"
    require(sha256(base_selection_path) == protocol["baseline_selection_sha256"], "baseline selection changed")
    base_selection = read(base_selection_path)
    require(base_selection["schema"] == "slopninja-frozen-author-selection-v1", "unsupported family baseline selection")
    require(base_selection["families"] == catalog["families"], "baseline family order differs")
    for key in IDENTITY:
        require(base_selection[key] == selection[key], f"baseline source identity differs: {key}")
    calibration_path = args.baseline_fit / "calibrations.json"
    require(sha256(calibration_path) == base_selection["calibrations_sha256"], "baseline calibration changed")
    calibration = read(calibration_path)["subsets"]["combined"]
    require(calibration["families"] == catalog["families"] and calibration["family_indices"] == list(range(14)),
            "baseline calibration family indices differ")
    outputs, bindings = [], []
    for run in selected_runs:
        seed = run["inherited_seed"]
        directory = fit / ("lambda-" + format(selection["selected_lambda"], "g").replace(".", "p")) / f"seed-{seed}"
        require(read(directory / "summary.json") == run, "selected treatment summary changed")
        require(sha256(directory / "epochs.jsonl") == run["epochs_sha256"], "selected treatment trajectory changed")
        checkpoint = bound_path(directory, run["selected_checkpoint"])
        require(sha256(checkpoint) == run["selected_checkpoint_sha256"], "selected treatment checkpoint changed")
        eta = validated_eta(torch.load(checkpoint, map_location="cpu", weights_only=True), run, selection, protocol)
        bases = [row for row in protocol["baseline_checkpoints"] if row["seed"] == seed]
        require(len(bases) == 1, "baseline checkpoint missing or duplicated")
        binding = bases[0]
        base_runs = [row for row in base_selection["runs"] if row["config"]["name"] == "combined_spline" and row["seed"] == seed]
        require(len(base_runs) == 1, "selected baseline run missing or duplicated")
        for key in ("selected_epoch", "selected_checkpoint", "selected_checkpoint_sha256"):
            require(base_runs[0][key] == binding[key], f"baseline selected state differs: {key}")
        require(binding["selected_checkpoint_sha256"] == run["baseline_checkpoint_sha256"], "treatment uses a different baseline")
        base_path = bound_path(args.baseline_fit / "combined_spline" / f"seed-{seed}", binding["selected_checkpoint"])
        require(sha256(base_path) == binding["selected_checkpoint_sha256"], "baseline checkpoint changed")
        base = torch.load(base_path, map_location="cpu", weights_only=True)
        require(base["schema"] == "slopninja-author-selection-checkpoint-v1" and base["seed"] == seed
                and base["epoch"] == binding["selected_epoch"] and base["config"] == base_runs[0]["config"], "baseline checkpoint identity differs")
        state = base["model_state"]
        require(state["family_indices"].dtype == torch.int64 and torch.equal(state["family_indices"], torch.arange(14)),
                "baseline family indices differ")
        for key, shape in (("theta", (14,)), ("knots", (30,)), ("log_slope", (30,)), ("log_tau", ())):
            require(state[key].dtype == torch.float64 and state[key].shape == shape and bool(torch.isfinite(state[key]).all()),
                    f"invalid frozen baseline tensor: {key}")
        require(state["knots"].tolist() == calibration["knots"] and bool((state["knots"] > 0).all())
                and bool((state["knots"][1:] > state["knots"][:-1]).all()), "frozen spline knots differ")
        artifact = {"schema": "slopninja-positive-coordinate-metric-v1", **{key: selection[key] for key in IDENTITY},
                    "families": families, "coordinates": portable_coordinates(coordinates, eta),
                    "family_weights": state["theta"].softmax(0).tolist(), "knots": state["knots"].tolist(),
                    "slopes": [1.0] + state["log_slope"].exp().tolist(), "temperature": state["log_tau"].exp().item(),
                    "provenance_sha256": {"selection": args.selection_sha256, "protocol": args.protocol_sha256,
                                          "catalog": sha256(args.catalog), "space": sha256(args.space),
                                          "fixed_subset_indices": index_hash(selection["config"]["max_column_indices"]),
                                          "coordinate_checkpoint": sha256(checkpoint), "baseline_checkpoint": sha256(base_path),
                                          "baseline_selection": sha256(base_selection_path), "baseline_calibration": sha256(calibration_path),
                                          "training_inputs": sha256(input_path), "training_implementation": selection["implementation_sha256"],
                                          "adapter_source": sha256(Path(__file__))}}
        encoded = json.dumps(artifact, separators=(",", ":"), allow_nan=False) + "\n"
        outputs.append((seed, encoded))
        bindings.append({"inherited_seed": seed, "selected_epoch": run["selected_epoch"], "lambda": run["lambda"],
                         "coordinate_checkpoint_sha256": sha256(checkpoint), "baseline_checkpoint_sha256": sha256(base_path),
                         "path": f"metric-seed-{seed}.json", "sha256": hashlib.sha256(encoded.encode()).hexdigest()})
    args.out.mkdir(parents=True)
    for seed, encoded in outputs:
        (args.out / f"metric-seed-{seed}.json").write_text(encoded)
    manifest = {"schema": "slopninja-portable-coordinate-models-v1", "models": bindings, "training_calls": 0,
                "fixed_subset_name": "all_m2", "coordinate_count": len(coordinates),
                "maximal_coordinate_count": len(catalog["coordinates"]),
                "fixed_subset_indices_sha256": protocol["fixed_subset_indices_sha256"],
                "selection_sha256": args.selection_sha256, "protocol_sha256": args.protocol_sha256,
                "adapter_source_sha256": sha256(Path(__file__)), "frozen_sources_verified": implementation["source_sha256"]}
    (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2, allow_nan=False) + "\n")
    print(json.dumps(bindings))
    return manifest


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("selection", "protocol", "catalog", "space", "baseline-fit", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    for name in ("selection-sha256", "protocol-sha256"):
        parser.add_argument("--" + name, required=True)
    export(parser.parse_args())

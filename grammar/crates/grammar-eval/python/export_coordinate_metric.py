"""Convert frozen PyTorch coordinate checkpoints to portable Rust inference JSON."""
import argparse
import hashlib
import json
from pathlib import Path

import torch


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read(path):
    return json.loads(path.read_text())


def bound_path(root, relative):
    path = (root / relative).resolve()
    require(path.is_relative_to(root.resolve()), "checkpoint escapes its fit directory")
    return path


def export(args):
    require(not args.out.exists(), "use a fresh output directory")
    selection, protocol = read(args.selection), read(args.protocol)
    catalog, space = read(args.catalog), read(args.space)
    require(selection["schema"] == "slopninja-frozen-coordinate-weights-v1", "unsupported selection")
    require(selection["protocol_sha256"] == sha256(args.protocol), "selection protocol changed")
    require(selection["catalog_sha256"] == sha256(args.catalog), "selected catalog changed")
    require(protocol["reference_space_sha256"] == sha256(args.space), "original space changed")
    for key in ["source_space_id", "feature_schema", "parser_identity"]:
        require(selection[key] == protocol[key] == catalog[key], f"identity differs: {key}")
    require(space["id"] == selection["source_space_id"], "source space identity differs")
    require(space["feature_schema"] == selection["feature_schema"]
            and space["parser_identity"] == selection["parser_identity"], "source feature identity differs")
    require(catalog["families"] == sorted(space["family_schemas"]), "family order differs")
    require(len(catalog["families"]) == 14, "expected frozen fourteen-family scorer")
    base_selection_path = args.baseline_fit / "selection.json"
    require(sha256(base_selection_path) == protocol["baseline_selection_sha256"], "baseline selection changed")
    base_selection = read(base_selection_path)
    require(catalog["families"] == base_selection["families"], "baseline family order differs")
    families = []
    for name in catalog["families"]:
        axes = [axis for axis in space["axes"] if axis["family"] == name]
        kind = space["family_schemas"][name]["kind"]
        families.append({"name": name, "kind": kind, "original_axis_count": len(axes),
                         "numerical_axes": [] if kind == "distribution" else
                         [{"feature": axis["feature"], "scale": axis["scale"]} for axis in axes]})
    outputs = []
    for seed in selection["inherited_seeds"]:
        runs = [row for row in selection["runs"]
                if row["lambda"] == selection["selected_lambda"] and row["inherited_seed"] == seed]
        require(len(runs) == 1, "selected coordinate run missing or duplicated")
        run = runs[0]
        directory = args.selection.parent / ("lambda-" + format(selection["selected_lambda"], "g").replace(".", "p")) / f"seed-{seed}"
        checkpoint = bound_path(directory, run["selected_checkpoint"])
        require(sha256(checkpoint) == run["selected_checkpoint_sha256"], "coordinate checkpoint changed")
        stored = torch.load(checkpoint, map_location="cpu", weights_only=True)
        require(stored["schema"] == "slopninja-coordinate-checkpoint-v1"
                and stored["inherited_seed"] == seed and stored["epoch"] == run["selected_epoch"]
                and stored["lambda"] == selection["selected_lambda"], "coordinate checkpoint identity differs")
        require(stored["catalog_sha256"] == selection["catalog_sha256"]
                and stored["baseline_checkpoint_sha256"] == run["baseline_checkpoint_sha256"], "coordinate source binding differs")
        eta = stored["eta"]
        require(eta.dtype == torch.float64 and eta.shape == (len(catalog["coordinates"]),)
                and bool(torch.isfinite(eta).all()) and eta.tolist() == run["selected_eta"], "coordinate parameters differ")
        bases = [row for row in protocol["baseline_checkpoints"] if row["seed"] == seed]
        require(len(bases) == 1, "baseline checkpoint missing or duplicated")
        binding = bases[0]
        base_runs = [row for row in base_selection["runs"]
                     if row["config"]["name"] == "combined_spline" and row["seed"] == seed]
        require(len(base_runs) == 1 and base_runs[0]["selected_checkpoint_sha256"] == binding["selected_checkpoint_sha256"]
                == run["baseline_checkpoint_sha256"], "baseline selection binding differs")
        base_path = bound_path(args.baseline_fit / "combined_spline" / f"seed-{seed}", binding["selected_checkpoint"])
        require(sha256(base_path) == binding["selected_checkpoint_sha256"], "baseline checkpoint changed")
        base = torch.load(base_path, map_location="cpu", weights_only=True)
        require(base["schema"] == "slopninja-author-selection-checkpoint-v1" and base["seed"] == seed
                and base["epoch"] == binding["selected_epoch"], "baseline checkpoint identity differs")
        state = base["model_state"]
        require(torch.equal(state["family_indices"], torch.arange(14)), "baseline family indices differ")
        require(all(state[key].dtype == torch.float64 and bool(torch.isfinite(state[key]).all())
                    for key in ["theta", "knots", "log_slope", "log_tau"]), "nonfinite or non-f64 baseline")
        coordinates = []
        for axis, log_weight, weight in zip(catalog["coordinates"], eta.tolist(), eta.exp().tolist()):
            coordinate = {key: axis[key] for key in ["family_index", "family", "feature", "kind", "scale", "original_family_axis_count"]}
            coordinate.update(log_multiplier=log_weight, multiplier=weight)
            coordinates.append(coordinate)
        artifact = {"schema": "slopninja-positive-coordinate-metric-v1",
                    **{key: selection[key] for key in ["source_space_id", "feature_schema", "parser_identity"]},
                    "families": families, "coordinates": coordinates,
                    "family_weights": state["theta"].softmax(0).tolist(),
                    "knots": state["knots"].tolist(), "slopes": [1.0] + state["log_slope"].exp().tolist(),
                    "temperature": state["log_tau"].exp().item(),
                    "provenance_sha256": {"selection": sha256(args.selection), "protocol": sha256(args.protocol),
                                          "catalog": sha256(args.catalog), "space": sha256(args.space),
                                          "coordinate_checkpoint": sha256(checkpoint), "baseline_checkpoint": sha256(base_path),
                                          "adapter_source": sha256(Path(__file__))}}
        outputs.append((seed, artifact))
    args.out.mkdir(parents=True)
    bindings = []
    for seed, artifact in outputs:
        path = args.out / f"metric-seed-{seed}.json"
        path.write_text(json.dumps(artifact, separators=(",", ":"), allow_nan=False) + "\n")
        bindings.append({"inherited_seed": seed, "path": path.name, "sha256": sha256(path)})
    (args.out / "manifest.json").write_text(json.dumps({"schema": "slopninja-portable-coordinate-models-v1",
                                                       "models": bindings, "training_calls": 0}, indent=2) + "\n")
    print(json.dumps(bindings))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["selection", "protocol", "catalog", "space", "baseline-fit", "out"]:
        parser.add_argument("--" + name, type=Path, required=True)
    export(parser.parse_args())

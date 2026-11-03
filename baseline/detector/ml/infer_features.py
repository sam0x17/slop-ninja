"""Score a Rust-projected vector and optionally return its feature gradient."""

import argparse
import json
from pathlib import Path

import torch

from common import configure_cpu, sha256
from feature_nn import dense_values, feature_gradient, load_bundle, margin, predict


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--vector-json", type=Path, required=True)
    parser.add_argument("--gradient", action="store_true")
    args = parser.parse_args()
    configure_cpu(1)
    model, manifest = load_bundle(args.artifact)
    vector = json.loads(args.vector_json.read_text())
    if vector.get("status") != "ok" or vector.get("schema") != "slop_ninja_projected_feature_vector_v1":
        raise ValueError("an explicitly successful Rust-projected vector is required")
    if any(vector[k] != manifest[k] for k in ("feature_space_sha256", "parser_identity", "feature_schema")):
        raise ValueError("projection belongs to another parser or feature space")
    if vector["feature_space_file_sha256"] != manifest["files"]["feature_space.json"]:
        raise ValueError("projection does not bind the bundled feature-space file")
    if vector["dimensions"] != model.config["dimensions"]:
        raise ValueError("projection dimension differs from the model")
    values = dense_values([vector], model.config["dimensions"], manifest["feature_space_sha256"])
    gradients = None
    if args.gradient:
        logits, gradients = feature_gradient(model, values)
        logits = logits.double()
    else:
        logits = predict(model, values, 1)
    probabilities = torch.softmax(logits / manifest["temperature"], dim=-1)[0]
    result = {"schema": "slop_ninja_feature_nn_prediction_v1", "status": "ok",
              "artifact_manifest_sha256": sha256(args.artifact / "manifest.json"),
              "projection_sha256": sha256(args.vector_json), "text_sha256": vector["text_sha256"],
              "feature_space_sha256": manifest["feature_space_sha256"],
              "logits": logits[0].tolist(), "probabilities": probabilities.tolist(),
              "model_origin_probability": float(probabilities[1:].sum()),
              "uncalibrated_model_origin_margin": float(margin(logits)[0]),
              "unknown_coordinates": vector["vector"]["unknown_coordinates"],
              "unavailable_families": vector["vector"]["unavailable_families"],
              "entrypoint_sha256": sha256(__file__), "qualification": manifest["status"]}
    if gradients is not None:
        result["gradient_of_margin_wrt_scaled_features"] = gradients[0].tolist()
    print(json.dumps(result, allow_nan=False))


if __name__ == "__main__":
    main()

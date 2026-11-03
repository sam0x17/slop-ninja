"""CPU probabilities for saved fit logits or Rust-projected diagnostic requests."""

import argparse
import json
from pathlib import Path

import torch

from common import LABELS, configure_cpu, sha256
from feature_nn import dense_values, feature_gradient, load_bundle, margin, predict


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact", required=True, type=Path)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--partition", choices=["development", "calibration"])
    source.add_argument("--projections", type=Path)
    parser.add_argument("--gradient-ids", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if args.output.exists() or (args.gradient_ids and not args.projections):
        parser.error("use a new output and projection requests for gradients")
    configure_cpu(1)
    model, manifest = load_bundle(args.artifact)
    artifact_hash = sha256(args.artifact / "manifest.json")
    gradients = {}
    gradient_ids = set(json.loads(args.gradient_ids.read_text())) if args.gradient_ids else set()
    if args.partition:
        source_path = args.artifact / (args.partition + "-logits.json")
        if sha256(source_path) != manifest["files"][source_path.name]:
            raise ValueError("saved logits differ from the bundle")
        rows = json.loads(source_path.read_text())
        logits = torch.tensor([row["logits"] for row in rows], dtype=torch.float64)
    else:
        source_path = args.projections
        rows = [json.loads(line) for line in source_path.read_text().splitlines()]
        ids = [r["id"] for r in rows]
        if len(set(ids)) != len(ids) or not gradient_ids.issubset(ids):
            raise ValueError("repeated request or missing gradient request")
        for row in rows:
            if (row.get("status") != "ok" or row.get("schema") != "slop_ninja_projected_feature_vector_v1"
                    or row["feature_space_file_sha256"] != manifest["files"]["feature_space.json"]
                    or row["dimensions"] != model.config["dimensions"]
                    or any(row[k] != manifest[k] for k in ("feature_space_sha256", "parser_identity", "feature_schema"))):
                raise ValueError("unsupported or incompatible projected input; no rows skipped")
        values = dense_values(rows, model.config["dimensions"], manifest["feature_space_sha256"])
        logits = predict(model, values, 1)
        for i, row in enumerate(rows):
            if row["id"] in gradient_ids:
                gradient_logits, gradient = feature_gradient(model, values[i:i + 1])
                if not torch.allclose(gradient_logits.double()[0], logits[i], atol=1e-6, rtol=0):
                    raise ValueError("gradient forward differs from ordinary inference")
                gradients[row["id"]] = gradient[0].tolist()
    if len(logits.shape) != 2 or logits.shape != (len(rows), 3) or not torch.isfinite(logits).all():
        raise ValueError("invalid batch logits")
    probabilities = torch.softmax(logits / manifest["temperature"], dim=-1)
    margins = margin(logits)
    with args.output.open("x") as stream:
        for i, row in enumerate(rows):
            result = {"id": row["id"], "text_sha256": row["text_sha256"], "status": "ok",
                      "artifact_id": artifact_hash, "labels": LABELS,
                      "logits": logits[i].tolist(), "probabilities": probabilities[i].tolist(),
                      "model_or_assistance_probability": float(probabilities[i, 1:].sum()),
                      "uncalibrated_model_origin_margin": float(margins[i]),
                      "feature_space_sha256": manifest["feature_space_sha256"]}
            if args.partition:
                result.update(source_group=row["source_group"], origin=row["origin"])
            if row["id"] in gradients:
                result["gradient"] = gradients[row["id"]]
            stream.write(json.dumps(result, allow_nan=False) + "\n")
    print(json.dumps({"status": "complete", "records": len(rows), "gradients": len(gradients),
                      "artifact_manifest_sha256": artifact_hash, "input_sha256": sha256(source_path),
                      "output_sha256": sha256(args.output), "entrypoint_sha256": sha256(__file__)}))


if __name__ == "__main__":
    main()

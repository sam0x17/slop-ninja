"""Export only the qualified V6 classifier; run in its immutable CPU runtime.

Example: python export_v6_classifier.py --artifact runtime/encoder-linux-compatible
         --out new-cohort/warm-start-v1/init.json
The original loader's file, dependency and bundled reference checks remain intact.
No corpus, provider, fitting or encoder export is involved.
"""
import argparse
import json
import os
from pathlib import Path

import v6_embeddings as v6


SCHEMA = "slop_ninja_v6_classifier_init_v1"
BASE_TEMPERATURE = 1.8776105792643432


def export(artifact, output):
    artifact, output = Path(artifact).resolve(strict=True), Path(output)
    v6.demand(not output.exists(), "initializer already exists")
    v6.demand(not output.resolve().is_relative_to(artifact), "output inside immutable artifact")
    manifest, profile = v6.artifact_binding(artifact)
    common, _tokenizer, model, temperature = v6.load_runtime(artifact, manifest)
    v6.demand(temperature == BASE_TEMPERATURE, "frozen base temperature differs")
    weights = {}
    for name, expected in (("weight", (3, 768)), ("bias", (3,))):
        tensor = getattr(model.classifier, name).detach()
        v6.demand(tuple(tensor.shape) == expected and bool(common.torch.isfinite(tensor).all()),
                  "invalid original classifier")
        weights[f"origin.{name}"] = {"shape": list(expected), "values": tensor.flatten().tolist()}
    weight_file = "classifier/model.safetensors"
    weight_hash = v6.sha256(artifact / weight_file)
    v6.demand(weight_hash == manifest["files"][weight_file], "weight bytes changed during export")
    result = {"schema": SCHEMA, "artifact_id": manifest["artifact_id"],
              "artifact_path": str(artifact), "artifact_manifest_sha256": v6.sha256(artifact / "manifest.json"),
              "parent_artifact_id": v6.PARENT_ID, "parent_manifest_sha256": v6.PARENT_MANIFEST_SHA256,
              "weight_file": weight_file, "weight_file_sha256": weight_hash,
              "origin_order": v6.LABELS, "base_temperature": temperature,
              "dimension": 768, "weights": weights, "dtype": "float32",
              "runtime_profile": profile, "original_dependency_and_reference_checks_passed": True,
              "exporter_sha256": v6.sha256(__file__), "loader_bridge_sha256": v6.sha256(v6.__file__),
              "upstream_encoder_exported": False, "fitting_performed": False, "final_test_opened": False}
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("x") as handle:
        json.dump(result, handle, allow_nan=False, indent=2)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    print(json.dumps({"initializer": str(output), "sha256": v6.sha256(output),
                      "artifact_id": manifest["artifact_id"], "weight_file_sha256": weight_hash}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    export(args.artifact, args.out)

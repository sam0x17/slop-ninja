"""Neural origin heads over frozen Rust word/grammar coordinates."""

import hashlib
import json
import math
from pathlib import Path

import torch
from safetensors.torch import load_model

from common import LABELS, sha256, verify_files


class FeatureHead(torch.nn.Module):
    def __init__(self, dimensions, hidden_width=0, dropout=0.1):
        super().__init__()
        if dimensions < 1 or hidden_width < 0 or not 0 <= dropout < 1:
            raise ValueError("invalid feature head dimensions or dropout")
        self.config = dict(dimensions=dimensions, hidden_width=hidden_width, dropout=dropout)
        self.network = (torch.nn.Sequential(torch.nn.Linear(dimensions, hidden_width),
                        torch.nn.GELU(), torch.nn.Dropout(dropout), torch.nn.Linear(hidden_width, 3))
                        if hidden_width else torch.nn.Linear(dimensions, 3))

    def forward(self, values):
        return self.network(values)


def margin(logits):
    return torch.logsumexp(logits[..., 1:], dim=-1) - logits[..., 0]


def feature_gradient(model, values):
    """Gradient of the uncalibrated model-origin margin in scaled coordinates."""
    model.eval()
    inputs = values.detach().clone().requires_grad_(True)
    logits = model(inputs)
    gradients, = torch.autograd.grad(margin(logits).sum(), inputs)
    if not torch.isfinite(gradients).all():
        raise ValueError("nonfinite input gradient")
    return logits.detach(), gradients.detach()


def dense_values(rows, dimensions, space_sha256):
    values = torch.zeros((len(rows), dimensions), dtype=torch.float32)
    for i, row in enumerate(rows):
        if row["feature_space_sha256"] != space_sha256:
            raise ValueError("row belongs to another coordinate space")
        previous = -1
        vector = row["vector"]
        unknown = vector["unknown_coordinates"]
        if (type(unknown) is not int or unknown < 0
                or not isinstance(vector["unavailable_families"], list)
                or any(not isinstance(v, str) for v in vector["unavailable_families"])):
            raise ValueError("invalid feature availability diagnostics")
        for index, value in vector["values"]:
            if (type(index) is not int or not previous < index < dimensions
                    or type(value) not in (float, int) or not math.isfinite(value)):
                raise ValueError("invalid, repeated or unsorted sparse coordinate")
            values[i, index] = value
            previous = index
    if not torch.isfinite(values).all():
        raise ValueError("feature conversion exceeds float32")
    return values


def read_export(directory, shards):
    """Verify the Rust ML handoff; do not acquire, split or admit corpus data."""
    directory, shards = Path(directory), Path(shards)
    manifest = json.loads((directory / "manifest.json").read_text())
    if (manifest["schema"] != "slop_ninja_neural_feature_export_v1"
            or manifest["labels"] != LABELS or manifest["fitting_split"] != "train"
            or manifest["classifier_fitted"] or manifest["test_vectors_exported"]
            or set(manifest["partitions"]) != {"train", "development", "calibration"}):
        raise ValueError("unsupported feature export")
    files = {"feature_space.json": manifest["feature_space_file_sha256"],
             "data_rights.json": manifest["data_rights_sha256"]}
    for split, part in manifest["partitions"].items():
        if part["file"] != split + ".jsonl":
            raise ValueError("unexpected partition filename")
        files[part["file"]] = part["sha256"]
    verify_files(directory, files)
    space = json.loads((directory / "feature_space.json").read_text())
    content = space["content"]
    if (space["content_sha256"] != manifest["feature_space_sha256"]
            or len(content["coordinates"]) != manifest["dimensions"]
            or content["parser_identity"] != manifest["parser_identity"]
            or content["feature_schema"] != manifest["feature_schema"]
            or content["training_rows"] != manifest["partitions"]["train"]["records"]):
        raise ValueError("feature space differs from the export manifest")
    rights = json.loads((directory / "data_rights.json").read_text())
    encoded = json.dumps(rights["content"], sort_keys=True, separators=(",", ":"),
                         ensure_ascii=False, allow_nan=False).encode()
    if (hashlib.sha256(encoded).hexdigest() != rights["content_sha256"]
            or rights["content"]["schema"] != "slop_ninja_data_release_obligations_v1"
            or rights["content"]["partitions"]["feature_file_sha256"] != manifest["feature_file_sha256"]):
        raise ValueError("rights do not bind this feature cache")
    notices = {(r["record_id"], r["text_sha256"]) for r in rights["content"]["share_alike_notices"]}
    partitions, tensors, source_hashes = {}, {}, {}
    for split, part in manifest["partitions"].items():
        source = shards / (split + ".jsonl")
        source_hashes[split] = sha256(source)
        originals = {}
        with source.open() as stream:
            for line in stream:
                row = json.loads(line)
                if (row["schema"] != "slop-ninja-origin-record-v1" or row["split"] != split
                        or row["origin"] not in LABELS or row["id"] in originals
                        or hashlib.sha256(row["text"].encode()).hexdigest() != row["text_sha256"]):
                    raise ValueError("invalid or duplicate original source record")
                if row["rights"]["license"].startswith("CC-BY-SA-") and (
                        (row["id"], row["text_sha256"]) not in notices
                        or rights["content"]["model_release_license"] != "CC-BY-SA-4.0"):
                    raise ValueError("required source attribution or release license missing")
                originals[row["id"]] = {k: row[k] for k in ("id", "source_group", "origin", "text_sha256", "evidence")}
        with (directory / part["file"]).open() as stream:
            rows = [json.loads(line) for line in stream]
        ids = [r["id"] for r in rows]
        if len(set(ids)) != len(rows) or set(ids) != set(originals) or len(rows) != part["records"]:
            raise ValueError("vector rows must cover the original archive exactly once")
        for index, row in enumerate(rows):
            original = originals[row["id"]]
            if row["split"] != split or any(row[k] != original[k] for k in ("source_group", "origin", "text_sha256")):
                raise ValueError("vector row differs from its source record")
            row.update(label=LABELS.index(row["origin"]), hash=row["text_sha256"],
                       evidence=original["evidence"], tensor_index=index)
        if (len({r["source_group"] for r in rows}) != part["source_groups"]
                or {label: sum(r["origin"] == label for r in rows) for label in LABELS} != part["class_counts"]):
            raise ValueError("partition counts differ from the manifest")
        tensors[split] = dense_values(rows, manifest["dimensions"], manifest["feature_space_sha256"])
        partitions[split] = [{k: v for k, v in r.items() if k != "vector"} for r in rows]
    from common import check_partition_separation
    check_partition_separation(partitions)
    return partitions, tensors, manifest, source_hashes


@torch.no_grad()
def predict(model, values, batch_size=64):
    model.eval()
    logits = torch.cat([model(batch) for batch in values.split(batch_size)]).double()
    if not torch.isfinite(logits).all():
        raise ValueError("nonfinite feature-head output")
    return logits


def load_bundle(directory):
    directory = Path(directory)
    manifest = json.loads((directory / "manifest.json").read_text())
    if manifest["schema"] != "slop_ninja_feature_nn_v1" or manifest["labels"] != LABELS:
        raise ValueError("unsupported neural feature bundle")
    required = {"config.json", "model.safetensors", "feature_space.json", "data_rights.json", "calibration.json",
                "reference_vectors.json", "runtime/feature_nn.py", "runtime/common.py", "runtime/checkpoints.py"}
    if not required.issubset(manifest["files"]):
        raise ValueError("bundle does not bind required model/runtime files")
    verify_files(directory, manifest["files"])
    for name in ("feature_nn.py", "common.py", "checkpoints.py"):
        if sha256(Path(__file__).parent / name) != manifest["files"]["runtime/" + name]:
            raise ValueError("use the bundle's frozen runtime")
    config = json.loads((directory / "config.json").read_text())
    space = json.loads((directory / "feature_space.json").read_text())
    calibration = json.loads((directory / "calibration.json").read_text())
    if (len(space["content"]["coordinates"]) != config["dimensions"]
            or space["content_sha256"] != manifest["feature_space_sha256"]
            or space["content"]["parser_identity"] != manifest["parser_identity"]
            or space["content"]["feature_schema"] != manifest["feature_schema"]
            or calibration["temperature"] != manifest["temperature"]
            or not math.isfinite(manifest["temperature"]) or manifest["temperature"] <= 0):
        raise ValueError("bundle space or calibration differs from its manifest")
    model = FeatureHead(**config)
    load_model(model, str(directory / "model.safetensors"), strict=True)
    model.eval()
    references = json.loads((directory / "reference_vectors.json").read_text())
    values = dense_values(references["rows"], config["dimensions"], manifest["feature_space_sha256"])
    actual = predict(model, values, 1)
    expected = torch.tensor(references["logits"], dtype=torch.float64)
    if actual.shape != expected.shape or not torch.allclose(actual, expected, atol=1e-6, rtol=0):
        raise ValueError("bundle reference logits differ in this runtime")
    return model, manifest

"""Validated row-major exports for coordinate metric learning."""
from collections import Counter
import hashlib
import json
from pathlib import Path

import numpy as np
import torch

from coordinate_model import CoordinateInputs
from frontier_common import balanced_loss, demand, read_json, sha256


IDENTITY = ["source_space_id", "feature_schema", "parser_identity"]


def safe_path(root, relative):
    demand(not Path(relative).is_absolute(), "absolute array path")
    result = (root / relative).resolve()
    demand(result.is_relative_to(root.resolve()), "array path escapes export")
    return result


def load_array(root, binding, expected_shape):
    demand(binding["dtype"] == "float64-le" and binding["shape"] == list(expected_shape),
           "array dtype or declared shape differs")
    path = safe_path(root, binding["path"])
    demand(sha256(path) == binding["sha256"], "exported array digest differs")
    demand(path.stat().st_size == int(np.prod(expected_shape)) * 8, "exported array byte count differs")
    array = np.fromfile(path, dtype="<f8").reshape(expected_shape)
    demand(np.isfinite(array).all(), "nonfinite exported array")
    return torch.from_numpy(array)


def validate_catalog(catalog, identity):
    demand(catalog["schema"] == "slopninja-selected-coordinate-catalog-v1", "unsupported selected-coordinate catalog")
    for key in IDENTITY:
        demand(catalog[key] == identity[key], f"coordinate source identity differs: {key}")
    families = catalog["families"]
    demand(families == sorted(set(families)) and len(families) == 14, "coordinate family catalog differs")
    demand(catalog["minimum_author_df"] == 10 and catalog["minimum_post_df"] == 30, "coordinate support cutoff differs")
    coordinates = catalog["coordinates"]
    names = [(row["family"], row["feature"]) for row in coordinates]
    demand(names == sorted(set(names)) and 0 < len(names) <= 1996, "coordinate ordering or budget differs")
    counts = Counter()
    numerical_count = 0
    for row in coordinates:
        demand(0 <= row["family_index"] < 14 and families[row["family_index"]] == row["family"], "coordinate family index differs")
        demand(np.isfinite(row["scale"]) and row["scale"] > 0 and row["original_family_axis_count"] > 0,
               "invalid original coordinate scale or family denominator")
        if row["kind"] == "distribution":
            demand(row["train_author_df"] >= 10 and row["train_post_df"] >= 30, "categorical coordinate lacks frozen training support")
            counts[row["family"]] += 1
        else:
            demand(row["kind"] in {"metric", "rate"}, "unknown coordinate kind")
            numerical_count += 1
    demand(numerical_count <= 12, "unexpected number of original numerical axes")
    for family, cap in catalog["categorical_family_caps"].items():
        expected = 512 if family in {"word", "word_bigram"} else 96
        demand(cap == expected and counts[family] <= expected, "per-family coordinate cap differs")
    demand(set(counts).issubset(catalog["categorical_family_caps"]), "categorical family lacks fixed cap")
    return {key: value for key, value in catalog.items() if key != "support"}


def load_export(root, expected_manifest_hash, expected_catalog_hash, identity, split, catalog=None):
    manifest_path = root / "manifest.json"
    demand(sha256(manifest_path) == expected_manifest_hash, "coordinate manifest differs from frozen inputs")
    manifest = read_json(manifest_path)
    demand(manifest["schema"] == "slopninja-coordinate-export-v1" and manifest["split"] == split,
           "coordinate export schema or split differs")
    demand(sha256(root / "audit.json") == manifest["audit_sha256"], "coordinate source/cache audit changed")
    for key in IDENTITY:
        demand(manifest[key] == identity[key], f"export identity differs: {key}")
    demand(manifest["catalog_sha256"] == expected_catalog_hash == sha256(root / "catalog.json"),
           "selected coordinate catalog changed")
    if catalog is None:
        catalog = validate_catalog(read_json(root / "catalog.json"), identity)
    demand(manifest["families"] == catalog["families"], "export family ordering differs")
    families = torch.tensor([row["family_index"] for row in catalog["coordinates"]])
    k, q, authors = len(families), manifest["query_count"], manifest["authors"]
    demand(manifest["coordinate_count"] == k and q > 0, "export coordinate/query count differs")
    demand(authors == sorted(set(authors)) and len(authors) >= 2, "candidate author ordering differs")
    demand(sha256(root / "queries.json") == manifest["queries_sha256"], "query metadata digest differs")
    rows = read_json(root / "queries.json")
    demand(len(rows) == q and [row["id"] for row in rows] == sorted({row["id"] for row in rows}), "query ordering or unique IDs differ")
    for row in rows:
        demand(row["split"] == split and 0 <= row["own"] < len(authors) and authors[row["own"]] == row["author"],
               "query split or author index differs")
        demand(row["source_group"] == f"blog:{row['author']}:date:{row['date']}", "query date-group binding differs")
    weights = None
    if split == "train":
        expected = balanced_loss(rows)
        demand(np.allclose(expected, [row["loss_weight"] for row in rows], rtol=1e-13, atol=1e-15), "training loss balance differs")
        weights = torch.tensor(expected, dtype=torch.float64)
        demand(abs(weights.sum().item() - 1) < 1e-12, "training loss mass differs")
    arrays = manifest["arrays"]
    expected_arrays = {"query_coordinates", "author_coordinates", "family_distances"}
    if split == "train":
        expected_arrays.add("own_heldout_coordinates")
    demand(set(arrays) == expected_arrays, "unexpected array catalog")
    query = load_array(root, arrays["query_coordinates"], (q, k))
    target = load_array(root, arrays["author_coordinates"], (len(authors), k))
    raw = load_array(root, arrays["family_distances"], (q, len(authors), 14))
    held = load_array(root, arrays["own_heldout_coordinates"], (q, k)) if split == "train" else None
    labels = torch.tensor([row["own"] for row in rows])
    inputs = CoordinateInputs(query, target, raw, labels, families, held, weights)
    residual_audit = inputs.validate_residual_tail()
    return {"inputs": inputs, "manifest": manifest, "catalog": catalog, "rows": rows,
            "authors": authors, "query_authors": [row["author"] for row in rows], "labels": labels.numpy(),
            "residual_audit": residual_audit, "manifest_sha256": expected_manifest_hash}


def validate_binding(data, binding, descriptor, reference_space_hash):
    demand(data["authors"] == binding["eligible_author_ids"] and len(data["rows"]) == binding["query_count"],
           "export eligibility differs from frozen input binding")
    removed = binding["excluded_author_ids"]
    demand(removed == sorted(set(removed)) and set(removed).isdisjoint(data["authors"]), "invalid excluded author list")
    all_authors = sorted(data["authors"] + removed)
    digest = hashlib.sha256(json.dumps(all_authors, separators=(",", ":")).encode()).hexdigest()
    demand(digest == descriptor["author_ids_sha256"], "source cohort author digest differs")
    demand(binding["source_summary_sha256"] == descriptor["summary_sha256"], "source cohort summary differs")
    demand(reference_space_hash in data["manifest"]["input_bindings"].values(), "export does not bind original reference-space bytes")


def load_inputs_manifest(path, protocol_hash, roles):
    manifest = read_json(path)
    demand(manifest["schema"] == "slopninja-coordinate-inputs-v1" and manifest["protocol_sha256"] == protocol_hash,
           "coordinate inputs protocol binding differs")
    demand(set(manifest["exports"]) == set(roles), "coordinate inputs export roles differ")
    return manifest

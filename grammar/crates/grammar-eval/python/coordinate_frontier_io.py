"""Hash-bound maximal coordinate exports for the representation frontier."""
import numpy as np
import torch

from coordinate_frontier_models import configurations, subset_data
from coordinate_io import IDENTITY, load_array, validate_binding
from frontier_common import balanced_loss, demand, read_json, sha256


def validate_catalog(catalog, identity):
    demand(catalog["schema"] == "slopninja-coordinate-frontier-catalog-v1", "unsupported frontier catalog")
    for key in IDENTITY:
        demand(catalog[key] == identity[key], f"frontier catalog identity differs: {key}")
    families = catalog["families"]
    demand(families == sorted(set(families)) and len(families) == 14, "frontier family ordering differs")
    demand(catalog["minimum_author_df"] == 10 and catalog["minimum_post_df"] == 30,
           "frontier coordinate support cutoff differs")
    coordinates = catalog["coordinates"]
    keys = [(row["family"], row["feature"]) for row in coordinates]
    demand(keys == sorted(set(keys)), "maximal coordinate ordering differs")
    ranks, numerical = {}, 0
    for row in coordinates:
        demand(families[row["family_index"]] == row["family"], "frontier family index differs")
        demand(np.isfinite(row["scale"]) and row["scale"] > 0 and row["original_family_axis_count"] > 0,
               "invalid frontier coordinate scale or original denominator")
        if row["kind"] == "distribution":
            demand(row["train_author_df"] >= 10 and row["train_post_df"] >= 30, "unsupported maximal coordinate")
            ranks.setdefault(row["family"], []).append(row)
        else:
            demand(row["kind"] in {"rate", "metric"} and row["support_rank"] is None, "invalid numerical coordinate")
            numerical += 1
    demand(numerical == 12, "original twelve variable numerical coordinates required")
    for family, rows in ranks.items():
        ordered = sorted(rows, key=lambda row: (-row["train_author_df"], -row["train_post_df"], row["feature"]))
        demand([row["support_rank"] for row in ordered] == list(range(1, len(rows) + 1)),
               f"within-family support ranks differ: {family}")
    configs = configurations(catalog)
    demand(len(coordinates) == 7353, "frozen maximal training catalog count differs")
    return {key: value for key, value in catalog.items() if key != "support"}, configs


def load_export(root, binding, catalog_hash, identity, split, descriptor, catalog=None):
    path = root / "manifest.json"
    demand(sha256(path) == binding["manifest_sha256"], "frontier export manifest changed")
    manifest = read_json(path)
    demand(manifest["schema"] == "slopninja-coordinate-frontier-export-v1" and manifest["split"] == split,
           "frontier export schema or split differs")
    for key in IDENTITY:
        demand(manifest[key] == identity[key], f"frontier export identity differs: {key}")
    demand(manifest["catalog_sha256"] == catalog_hash == sha256(root / "catalog.json"), "frontier maximal catalog changed")
    demand(sha256(root / "audit.json") == manifest["audit_sha256"], "frontier source audit changed")
    if catalog is None:
        catalog, _ = validate_catalog(read_json(root / "catalog.json"), identity)
    demand(manifest["families"] == catalog["families"], "frontier export family order differs")
    authors, q, k = manifest["authors"], manifest["query_count"], len(catalog["coordinates"])
    demand(authors == sorted(set(authors)) and len(authors) >= 2 and q > 0, "invalid frontier author or query catalog")
    demand(manifest["coordinate_count"] == k, "frontier export coordinate width differs")
    demand(sha256(root / "queries.json") == manifest["queries_sha256"], "frontier query metadata changed")
    rows = read_json(root / "queries.json")
    demand(len(rows) == q and [row["id"] for row in rows] == sorted({row["id"] for row in rows}),
           "frontier query ordering differs")
    for row in rows:
        demand(row["split"] == split and 0 <= row["own"] < len(authors) and authors[row["own"]] == row["author"],
               "frontier query split or candidate index differs")
        demand(row["source_group"] == f"blog:{row['author']}:date:{row['date']}", "frontier date-group binding differs")
    expected_arrays = {"query_coordinates", "author_coordinates", "family_distances"}
    if split == "train":
        expected_arrays.add("own_heldout_coordinates")
    arrays = manifest["arrays"]
    demand(set(arrays) == expected_arrays, "frontier binary array catalog differs")
    weights = None
    if split == "train":
        expected = balanced_loss(rows)
        demand(np.allclose(expected, [row["loss_weight"] for row in rows], rtol=1e-13, atol=1e-15),
               "frontier training loss balance differs")
        weights = torch.tensor(expected, dtype=torch.float64)
    labels = torch.tensor([row["own"] for row in rows])
    result = {"manifest": manifest, "manifest_sha256": binding["manifest_sha256"], "catalog": catalog,
              "rows": rows, "authors": authors, "query_authors": [row["author"] for row in rows],
              "labels_tensor": labels, "labels": labels.numpy(), "loss_weights": weights,
              "query": load_array(root, arrays["query_coordinates"], (q, k)),
              "target": load_array(root, arrays["author_coordinates"], (len(authors), k)),
              "raw": load_array(root, arrays["family_distances"], (q, len(authors), 14)),
              "held": load_array(root, arrays["own_heldout_coordinates"], (q, k)) if split == "train" else None}
    validate_binding(result, binding, descriptor, identity["reference_space_sha256"])
    result["residual_audit"] = subset_data(result, list(range(k)))["inputs"].validate_residual_tail()
    return result


def load_inputs(path, protocol_hash, roles, selection_hash=None):
    result = read_json(path)
    demand(result["schema"] == "slopninja-coordinate-frontier-inputs-v1" and result["protocol_sha256"] == protocol_hash,
           "frontier input protocol binding differs")
    demand(set(result["exports"]) == set(roles), "frontier input gallery roles differ")
    role = "train" if "train" in roles else "test"
    demand(result["role"] == role, "frontier input role differs")
    eligibility_name = "validation" if role == "train" else "test"
    eligibility_path = path.parent / f"{eligibility_name}-eligibility.json"
    demand(sha256(eligibility_path) == result[f"{eligibility_name}_eligibility_sha256"], "frozen frontier eligibility changed")
    eligibility = read_json(eligibility_path)
    demand(eligibility["schema"] == "slopninja-coordinate-frontier-eligibility-v1", "unknown frontier eligibility schema")
    expected_roles = set(roles) - {"train"}
    demand({row["id"] for row in eligibility["cohorts"]} == expected_roles and len(eligibility["cohorts"]) == 3,
           "frontier eligibility gallery catalog differs")
    for row in eligibility["cohorts"]:
        binding = result["exports"][row["id"]]
        demand(row["eligible_author_ids"] == binding["eligible_author_ids"] and row["candidate_authors"] == len(binding["eligible_author_ids"]),
               "frontier eligibility author catalog differs")
        count_key = "dev_posts" if role == "train" else "test_posts"
        demand(row[count_key] == binding["query_count"], "frontier eligibility query count differs")
    if selection_hash is not None:
        demand(result["selection_sha256"] == selection_hash, "test inputs do not bind frozen frontier selection")
    return result

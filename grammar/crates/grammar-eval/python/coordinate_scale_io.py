"""Frozen panel and eligibility bindings for the training-author scale contrast."""
from coordinate_frontier_io import load_export
from frontier_common import demand, read_json, sha256


def validate_eligibility(path, digest, schema, role, protocol_hash, names, bindings, split):
    demand(sha256(path) == digest, "training-scale eligibility file changed")
    eligibility = read_json(path)
    demand(eligibility["schema"] == schema and eligibility["role"] == role and
           eligibility["protocol_sha256"] == protocol_hash, "training-scale eligibility identity differs")
    rows = eligibility["cohorts"]
    demand(len(rows) == len(names) and {row["id"] for row in rows} == set(names), "training-scale eligibility cohort catalog differs")
    for row in rows:
        entry = bindings[row["id"]]
        demand(row["eligible_author_ids"] == entry["eligible_author_ids"] and
               row["candidate_authors"] == len(entry["eligible_author_ids"]), "training-scale eligible author catalog differs")
        demand(row[f"{split}_posts"] == entry["query_count"], "training-scale eligible query count differs")
    return eligibility


def load_inputs(path, protocol, protocol_hash, training, selection_hash=None):
    value = read_json(path)
    role = "train" if training else "test"
    demand(value["schema"] == "slopninja-training-author-scale-inputs-v1" and value["role"] == role and
           value["protocol_sha256"] == protocol_hash, "training-scale input identity differs")
    cohorts = protocol["cohorts"]
    train_names = [row["name"] for row in cohorts["training_panels"]]
    validation_names = [row["name"] for row in cohorts["validation"]]
    test_names = [row["name"] for row in cohorts["test"]]
    expected = train_names + validation_names if training else test_names
    demand(len(value["exports"]) == len(expected) and set(value["exports"]) == set(expected), "training-scale export roles differ")
    schema = "slopninja-training-author-scale-eligibility-v1"
    if training:
        validate_eligibility(path.parent / "training-eligibility.json", value["training_eligibility_sha256"], schema,
                             "training", protocol_hash, train_names, value["exports"], "train")
        validate_eligibility(path.parent / "validation-eligibility.json", value["validation_eligibility_sha256"],
                             "slopninja-coordinate-frontier-eligibility-v1", "validation",
                             protocol["source_frontier_protocol_sha256"], validation_names, value["exports"], "dev")
    else:
        demand(value["selection_sha256"] == selection_hash, "test inputs do not bind frozen training-scale model")
        eligibility = validate_eligibility(path.parent / "test-eligibility.json", value["test_eligibility_sha256"], schema,
                                           "test", protocol_hash, test_names, value["exports"], "test")
        demand(eligibility["selection_sha256"] == selection_hash, "test eligibility does not bind frozen training-scale model")
    return value


def load_fixed_export(path, binding, catalog_hash, protocol, split, descriptor, catalog, newly_exported):
    data = load_export(path, binding, catalog_hash, protocol, split, descriptor, catalog)
    if newly_exported:
        manifest = data["manifest"]
        demand(manifest["role"] == ("training_panel" if split == "train" else "test_gallery"), "fixed export role differs")
        demand(manifest["training_catalog_policy"] == "apply_frozen_catalog_without_fitting", "fixed export refitted coordinate catalog")
        demand(manifest["fixed_subset_name"] == "all_m2" and manifest["fixed_subset"] == catalog["subsets"]["all_m2"],
               "fixed export coordinate subset differs")
    demand(split != "train" or len(data["authors"]) == 100, "training-scale fitting panels require exactly 100 authors")
    return data

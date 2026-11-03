"""Validate frozen, named-family TRAIN/DEV/TEST inputs for matched calibration."""
import hashlib
import json
import math
from collections import Counter, defaultdict
from pathlib import Path

import numpy as np
import torch

from frontier_common import demand, read_json, sha256


def ids_hash(ids):
    return hashlib.sha256(json.dumps(sorted(ids), separators=(",", ":")).encode()).hexdigest()


def digest_leaves(value):
    if isinstance(value, dict):
        return set().union(*(digest_leaves(item) for item in value.values()))
    if isinstance(value, list):
        return set().union(*(digest_leaves(item) for item in value))
    if isinstance(value, str) and len(value) == 64 and all(char in "0123456789abcdef" for char in value):
        return {value}
    return set()


def validate_protocol(protocol):
    demand(protocol["schema"] == "slopninja-clause-family-study-protocol-v1", "unsupported named-family protocol")
    baseline, families = protocol["baseline_families"], protocol["families"]
    demand(baseline == sorted(set(baseline)) and len(baseline) == 14, "baseline family catalog differs")
    demand(families == sorted(set(baseline) | {protocol["added_family"]}) and len(families) == 15
           and protocol["added_family"] == "clause_child_backoff_v1", "new family catalog differs")
    demand(protocol["arms"] == [{"name": "raw14", "families": baseline, "trainable_parameter_count": 45},
                                {"name": "raw15", "families": families, "trainable_parameter_count": 46}], "matched arms differ")
    demand(protocol["seeds"] == [0, 17, 29] and protocol["new_fit_count"] == 6 and protocol["checkpoint_count"] == 1206,
           "matched fit grid differs")
    expected = {"epochs": 200, "learning_rate": 0.03, "beta1": 0.9, "beta2": 0.999, "epsilon": 1e-8,
                "device": "cpu", "dtype": "float64", "threads": 8, "deterministic": True,
                "knots": 30, "log_temperature_bounds": [-6, 2], "log_slope_bounds": [-6, 6], "regularization": 0}
    for key, value in expected.items():
        demand(protocol["optimization"][key] == value, f"optimizer setting differs: {key}")
    initial = {"lexical_strength": 0.25, "grammar_strength": 0.5 / 12, "jitter_std": 0.05, "zero_jitter_seed": 0,
               "baseline_temperature": 0.1, "noise_order": "baseline_families followed by sorted new families",
               "temperature_rule": "baseline_temperature * baseline_strength_sum / active_strength_sum", "initial_log_slopes": 0}
    for key, value in initial.items():
        demand(protocol["initialization"][key] == value, f"initialization setting differs: {key}")
    demand(protocol["knots"] == {"count": 30, "source": "pooled baseline14 TRAIN distances from all three panels",
                                "schedule": "dyadic breadth-first quantiles; skip duplicate/nonpositive/max-endpoint proposals; sort accepted prefix",
                                "interpolation": "linear empirical quantile"}, "knot fitting policy differs")
    demand(protocol["selection"]["checkpoint_rule"] == "macro_author.top1, then macro_author.mrr, then earliest epoch including 0",
           "checkpoint selection rule differs")
    demand(protocol["training"]["panel_order"] == ["authors-v1", "authors-replication-v1", "authors-spline-confirmation-v1"]
           and protocol["training"]["queries_per_panel"] == [1224, 1363, 1302]
           and protocol["training"]["authors_per_panel"] == 100, "fixed TRAIN panel scope differs")
    demand(len(descriptors(protocol, "validation")) == 2 and len(descriptors(protocol, "test")) == 4, "fresh gallery counts differ")
    demand(protocol["bootstrap"]["replicates"] == 2000 and protocol["bootstrap"]["seed"] == "0x6a09e667f3bcc909"
           and protocol["bootstrap"]["percentile_indices_zero_based"] == [49, 1949], "bootstrap policy differs")


def descriptors(protocol, role):
    if role == "training":
        prior = {row["name"]: row for row in protocol["cohorts"]["prior_full_cohorts"]}
        return [{**prior[name], "role": "training", "query_split": "train", "requested_authors": 100,
                 "expected_queries": protocol["training"]["queries_per_panel"][index],
                 "original_coordinate_manifest_sha256": protocol["training"]["panels"][index]["manifest_sha256"]}
                for index, name in enumerate(protocol["training"]["panel_order"])]
    return [row for row in protocol["cohorts"]["new_cohorts"] if row["role"] == role]


def weights_by_author_date(rows):
    counts = defaultdict(Counter)
    for row in rows:
        demand(row["source_group"] == f"blog:{row['author']}:date:{row['date']}", "source group/date identity differs")
        counts[row["author"]][row["date"]] += 1
    demand(counts, "empty training rows")
    return [1 / len(counts) / len(counts[row["author"]]) / counts[row["author"]][row["date"]] for row in rows]


def bound_array(directory, binding, shape):
    relative = Path(binding["path"])
    path = (directory / relative).resolve()
    demand(not relative.is_absolute() and path.is_relative_to(directory.resolve()), "array path escapes export")
    demand(binding["dtype"] == "float64-le" and binding["shape"] == list(shape), "array dtype/shape differs")
    demand(path.stat().st_size == 8 * math.prod(shape) == binding["bytes"], "array byte length differs")
    demand(sha256(path) == binding["sha256"], "array content hash differs")
    values = np.fromfile(path, dtype="<f8").reshape(shape)
    demand(np.isfinite(values).all() and (values >= 0).all(), "nonfinite or negative family distance")
    return values


def load_export(directory, binding, descriptor, eligibility, protocol, protocol_hash):
    directory = Path(directory)
    manifest = read_json(directory / "manifest.json")
    digest = sha256(directory / "manifest.json")
    demand(digest == binding["manifest_sha256"] == eligibility["named_tensor_manifest_sha256"], "named tensor manifest changed")
    demand(manifest["schema"] == "slopninja-named-family-tensor-v1", "unsupported named tensor")
    demand(manifest["protocol_sha256"] == protocol_hash, "tensor protocol binding differs")
    for key in ("source_space_id", "reference_space_sha256", "feature_schema", "parser_identity", "families", "baseline_families"):
        demand(manifest[key] == protocol[key], f"tensor identity differs: {key}")
    demand(manifest["legacy_feature_schema"] == protocol["baseline_feature_schema"]
           and manifest["new_family"] == protocol["added_family"], "legacy/new feature identity differs")
    indices = [protocol["families"].index(name) for name in protocol["baseline_families"]]
    demand(manifest["baseline_family_indices"] == indices, "baseline family projection indices differ")
    authors = manifest["authors"]
    demand(authors == sorted(set(authors)) == binding["eligible_author_ids"] == eligibility["eligible_author_ids"], "eligible author catalog differs")
    excluded = binding["excluded_author_ids"]
    demand(excluded == sorted(set(excluded)) and set(authors).isdisjoint(excluded)
           and sorted(eligibility["excluded_authors"]) == excluded, "author attrition differs")
    demand(ids_hash(authors + excluded) == descriptor["author_ids_sha256"], "source author allocation differs")
    demand(binding["source_summary_sha256"] == descriptor["summary_sha256"] == eligibility["source_summary_sha256"],
           "source cohort summary differs")
    split = descriptor["query_split"]
    demand(manifest["split"] == split and eligibility["role"] == descriptor["role"], "tensor role/split differs")
    for filename, key in (("queries.json", "queries_sha256"), ("audit.json", "audit_sha256"), ("targets.json", "targets_sha256")):
        demand(sha256(directory / filename) == manifest[key], f"named tensor artifact changed: {filename}")
    source_digests = digest_leaves(manifest["input_bindings"])
    for key in ("rust_implementation_sha256", "annotation_audit_sha256"):
        demand(eligibility[key] in source_digests, f"prepared source is not bound by tensor: {key}")
    rows = read_json(directory / "queries.json")
    demand(rows and len(rows) == manifest["query_count"] == binding["query_count"] == eligibility["query_count"]
           == eligibility["dev_posts" if split == "dev" else split + "_posts"], "query count differs")
    demand(len({row["id"] for row in rows}) == len(rows), "duplicate query IDs")
    for row in rows:
        demand(row["split"] == split and row["author"] in authors and type(row["own"]) is int
               and 0 <= row["own"] < len(authors) and authors[row["own"]] == row["author"], "query label/split differs")
    demand(set(row["author"] for row in rows) == set(authors), "query author coverage differs")
    x = bound_array(directory, manifest["arrays"]["family_distances"], (len(rows), len(authors), len(protocol["families"])))
    old = bound_array(directory, manifest["arrays"]["baseline_family_distances"], (len(rows), len(authors), len(indices)))
    demand(manifest["baseline_source_array_sha256"] == manifest["arrays"]["baseline_family_distances"]["sha256"],
           "copied baseline source array hash differs")
    demand(x[:, :, indices].copy(order="C").tobytes() == old.tobytes(), "shared baseline14 distances are not byte-identical")
    weights = weights_by_author_date(rows)
    if split == "train":
        demand(len(authors) == 100 and len(rows) == descriptor["expected_queries"] and not excluded,
               "fixed training panel differs")
        demand(manifest["coordinate_export_manifest_sha256"] == descriptor["original_coordinate_manifest_sha256"],
               "original TRAIN coordinate manifest differs")
        demand(all(math.isclose(row["loss_weight"], weight, rel_tol=1e-13, abs_tol=1e-15) for row, weight in zip(rows, weights)),
               "author/date/post training weights differ")
        demand(all(len({row["date"] for row in rows if row["author"] == author}) >= 2 for author in authors),
               "training author lacks whole-date exclusion support")
    return {"x": torch.from_numpy(x), "own": torch.tensor([row["own"] for row in rows]),
            "loss_weight": torch.tensor(weights, dtype=torch.float64), "rows": rows,
            "query_authors": [row["author"] for row in rows], "authors": authors, "families": manifest["families"],
            "split": split, "manifest_sha256": digest, "manifest": manifest}


def prior_author_ids(protocol_path, protocol):
    prior = set()
    for row in protocol["cohorts"]["prior_full_cohorts"]:
        path = protocol_path.parent.parent / row["name"] / "summary.json"
        demand(sha256(path) == row["summary_sha256"], "prior author source summary changed")
        summary = read_json(path)
        authors = [author["author_id"] for author in summary["authors"]]
        demand(ids_hash(authors) == row["author_ids_sha256"] and prior.isdisjoint(authors), "prior author catalog differs or overlaps")
        prior.update(authors)
    return prior


def load_inputs(protocol_path, protocol, protocol_hash, path, expected_hash, training, selection_hash=None):
    path, protocol_path = Path(path), Path(protocol_path)
    demand(sha256(path) == expected_hash, "input manifest differs from frozen supplied SHA")
    inputs = read_json(path)
    demand(inputs["schema"] == "slopninja-named-family-calibration-inputs-v1" and inputs["protocol_sha256"] == protocol_hash
           and inputs["role"] == ("train" if training else "test"), "input manifest identity differs")
    if not training:
        demand(selection_hash and inputs["selection_sha256"] == selection_hash, "test inputs are not bound to the frozen selection")
    roles = ["training", "validation"] if training else ["test"]
    expected = [row["name"] for role in roles for row in descriptors(protocol, role)]
    demand(set(inputs["exports"]) == set(expected), "input export scope differs")
    prior = prior_author_ids(protocol_path, protocol)
    seen, seen_queries, all_data, eligibility_hashes = set(), set(), {}, {}
    for role in roles:
        eligibility_path = path.parent / f"{role}-eligibility.json"
        digest = sha256(eligibility_path)
        demand(digest == inputs[f"{role}_eligibility_sha256"], "eligibility manifest changed")
        eligibility = read_json(eligibility_path)
        demand(eligibility["schema"] == "slopninja-named-family-calibration-eligibility-v1"
               and eligibility["protocol_sha256"] == protocol_hash and eligibility["role"] == role
               and eligibility["no_performance_based_exclusions"] is True, "eligibility identity/policy differs")
        definitions = descriptors(protocol, role)
        eligibility_rows = {row["id"]: row for row in eligibility["cohorts"]}
        demand(len(eligibility_rows) == len(eligibility["cohorts"]) == len(definitions)
               and set(eligibility_rows) == {row["name"] for row in definitions}, "eligibility cohort catalog differs")
        datasets = {}
        for descriptor in definitions:
            name = descriptor["name"]
            binding = inputs["exports"][name]
            directory = (path.parent / binding["path"]).resolve()
            demand(directory.is_relative_to(protocol_path.parent.parent.resolve()), "input export escapes corpus root")
            data = load_export(directory, binding, descriptor, eligibility_rows[name], protocol, protocol_hash)
            allocated = set(binding["eligible_author_ids"] + binding["excluded_author_ids"])
            demand(seen.isdisjoint(allocated), "authors overlap between allocated panels/galleries")
            if role != "training":
                demand(prior.isdisjoint(allocated), "fresh gallery reuses an earlier allocated author")
            query_ids = {row["id"] for row in data["rows"]}
            demand(seen_queries.isdisjoint(query_ids), "queries overlap between panels/galleries")
            seen.update(allocated)
            seen_queries.update(query_ids)
            datasets[name] = data
        all_data[role] = datasets
        eligibility_hashes[role] = digest
    return inputs, all_data, eligibility_hashes

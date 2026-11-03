"""Hash-bound author-disjoint ML inputs; no source prose or test selection."""
import math
from pathlib import Path

import torch

from author_selection_models import CONFIGS, SEEDS
from frontier_common import balanced_loss, demand, load_queries, read_json, sha256


SCHEMA = "slopninja-author-disjoint-selection-protocol-v1"
ELIGIBILITY_SCHEMA = "slopninja-author-selection-eligibility-v1"
IDENTITY = ["source_space_id", "feature_schema", "parser_identity"]
CHECKPOINT_RULE = "macro_author.top1, then macro_author.mrr, then earliest epoch including0"
ARCHITECTURE_RULE = "mean selected seed macro_author.top1, then mean MRR, then fewer parameters, then name"


def validate_protocol(protocol):
    demand(protocol["schema"] == SCHEMA, "unsupported author-selection protocol")
    demand(protocol["configs"] == CONFIGS and protocol["seeds"] == SEEDS, "model catalog or seeds differ")
    expected = {"epochs": 200, "learning_rate": 0.03, "beta1": 0.9, "beta2": 0.999, "epsilon": 1e-8,
                "device": "cpu", "dtype": "float64", "threads": 8, "knots": 30,
                "log_temperature_bounds": [-6, 2], "log_slope_bounds": [-6, 6], "initial_temperature": 0.1}
    for key, value in expected.items():
        demand(protocol["training"][key] == value, f"frozen training setting differs: {key}")
    demand(protocol["selection"] == {"split": "dev", "checkpoint": CHECKPOINT_RULE, "architecture": ARCHITECTURE_RULE},
           "selection procedure differs")
    demand(protocol["primary"] == {"left_subset": "combined", "right_subset": "words", "metric": "macro_author.top1"},
           "primary comparison differs")
    demand(protocol["bootstrap"] == {"replicates": 2000, "seed": "0x6a09e667f3bcc909",
                                      "percentile_indices_zero_based": [49, 1949], "stratify": "gallery",
                                      "weighting": "equal authors across eligible galleries"}, "bootstrap settings differ")
    cohorts = protocol["cohorts"]
    prior = cohorts["excluded_prior_author_ids"]
    demand(prior == sorted(set(prior)) and len(prior) == 400, "must exclude every one of 400 prior export authors")
    demand(len(cohorts["test"]) == 3, "exactly three test galleries required")
    descriptors = [cohorts["validation"]] + cohorts["test"]
    demand(len({row["id"] for row in descriptors}) == 4, "duplicate cohort ID")
    observed = set(prior)
    for descriptor in descriptors:
        authors = descriptor["author_ids"]
        demand(authors == sorted(set(authors)) and len(authors) == 100, "each source cohort needs 100 sorted unique authors")
        demand(observed.isdisjoint(authors), "author overlap across source, validation, or test cohorts")
        observed.update(authors)


def load_training_tensor(original_fit, protocol):
    selection_path = original_fit / "selection.json"
    demand(sha256(selection_path) == protocol["source_selection_sha256"], "source selection digest differs")
    selection = read_json(selection_path)
    demand(selection["schema"] == "unslop-frozen-author-weights-v1", "unsupported original source selection")
    tensor_path = original_fit / "training-tensor.json"
    demand(sha256(tensor_path) == protocol["source_tensor_sha256"] == selection["training_tensor_sha256"],
           "source training tensor digest differs")
    tensor = read_json(tensor_path)
    demand(tensor["schema"] == "unslop-neural-distance-tensor-v1", "unsupported source tensor")
    for key in IDENTITY:
        demand(tensor[key] == protocol[key] == selection[key], f"source tensor identity differs: {key}")
    families, authors, rows = tensor["families"], tensor["authors"], tensor["rows"]
    demand(families == sorted(set(families)) and len(families) == 14, "original family catalog differs")
    demand(authors == sorted(set(authors)) == selection["selection_author_ids"] and len(authors) == 100,
           "original training author catalog differs")
    demand(set(authors).issubset(protocol["cohorts"]["excluded_prior_author_ids"]), "prior exclusions omit training authors")
    demand(len(rows) == selection["training_post_count"] and len({row["id"] for row in rows}) == len(rows),
           "source training count or unique IDs differ")
    weights = balanced_loss(rows)
    for row, weight in zip(rows, weights):
        demand(0 <= row["own"] < len(authors) and authors[row["own"]] == row["author"], "source training labels differ")
        demand(row["source_group"].startswith(f"blog:{row['author']}:date:"), "source date-group binding differs")
        demand(math.isclose(row["loss_weight"], weight, abs_tol=1e-15, rel_tol=1e-13), "source loss weighting differs")
    x = torch.tensor([row["distances"] for row in rows], dtype=torch.float64)
    demand(x.shape == (len(rows), len(authors), 14), "source training tensor shape differs")
    demand(bool(torch.isfinite(x).all()) and bool((x >= 0).all()), "invalid source training distances")
    return {"x": x, "own": torch.tensor([row["own"] for row in rows]),
            "loss_weight": torch.tensor(weights, dtype=torch.float64), "families": families,
            "author_ids": authors, "post_ids": [row["id"] for row in rows]}


def validate_eligibility(descriptor, eligible, role, exclusions, audit):
    demand(eligible["role"] == role, "eligible cohort role differs")
    authors = eligible["eligible_author_ids"]
    removed = eligible["excluded_authors"]
    demand(authors == sorted(set(authors)) and len(authors) >= 2, "invalid eligible author catalog")
    demand(set(authors).isdisjoint(removed) and set(authors) | set(removed) == set(descriptor["author_ids"]),
           "eligible and excluded authors do not partition source cohort")
    demand(exclusions["eligible_author_ids"] == authors and exclusions["excluded_authors"] == removed,
           "actual author exclusions differ from frozen manifest")
    demand(exclusions["candidate_authors"] == eligible["candidate_authors"] == len(authors), "eligible candidate count differs")
    demand(exclusions["input_authors"] == 100, "source cohort size differs")
    demand(len({row["post"]["id"] for row in audit}) == len(audit), "duplicate annotation audit IDs")
    demand(all(row["post"]["author_id"] in descriptor["author_ids"] for row in audit), "audit contains unknown author")
    demand(all(row["status"] != "eligible" for row in audit if row["post"]["author_id"] in removed),
           "excluded author retained in audit")
    declared = {(row["id"], row["split"]) for row in eligible["excluded_posts"]}
    observed = {(row["post"]["id"], row["post"]["split"]) for row in audit
                if row["status"] != "eligible" and row["post"]["author_id"] not in removed}
    demand(len(declared) == len(eligible["excluded_posts"]) and declared == observed, "post exclusions differ")
    for split in ["train", "dev", "test"]:
        actual = sum(row["status"] == "eligible" and row["post"]["split"] == split for row in audit)
        demand(actual == eligible[f"{split}_posts"], f"eligible {split} count differs")
    return authors


def load_cohort(evaluation, descriptor, eligible, protocol, families, role):
    split = "dev" if role == "validation" else "test"
    for filename, key in [("implementation.json", "rust_implementation_sha256"),
                          ("annotation-audit.json", "annotation_audit_sha256"),
                          ("author-exclusions.json", "author_exclusions_sha256")]:
        demand(sha256(evaluation / filename) == eligible[key], f"eligible artifact digest differs: {filename}")
    implementation = read_json(evaluation / "implementation.json")
    demand(implementation["schema"] == "unslop-author-evaluation-implementation-v1", "unsupported Rust preparation")
    # Other split query files and report scores are deliberately not opened.
    filenames = ["protocol.json", "source-summary.json", "space.json", "annotation-audit.json",
                 "author-exclusions.json", f"{split}-queries.jsonl"]
    for filename in filenames:
        demand(sha256(evaluation / filename) == implementation["artifacts_sha256"][filename],
               f"Rust artifact changed: {filename}")
    fresh_protocol = read_json(evaluation / "protocol.json")
    demand(fresh_protocol["input_bindings"]["summary_sha256"] == descriptor["summary_sha256"], "source export differs")
    frozen = fresh_protocol["frozen_selection"]
    demand(frozen["source_space_id"] == protocol["source_space_id"] and
           frozen["source_space_sha256"] == protocol["reference_space_sha256"], "original numerical reference differs")
    for key in ["feature_schema", "parser_identity"]:
        demand(fresh_protocol[key] == protocol[key], f"fresh extractor identity differs: {key}")
    summary = read_json(evaluation / "source-summary.json")
    demand(sorted(row["author_id"] for row in summary["authors"]) == descriptor["author_ids"], "source author catalog differs")
    audit = read_json(evaluation / "annotation-audit.json")
    exclusions = read_json(evaluation / "author-exclusions.json")
    authors = validate_eligibility(descriptor, eligible, role, exclusions, audit)
    space = read_json(evaluation / "space.json")
    demand(sorted(space["family_schemas"]) == families, "fresh family catalog differs")
    demand(space["feature_schema"] == protocol["feature_schema"] and space["parser_identity"] == protocol["parser_identity"],
           "fresh space feature/parser identity differs")
    queries = load_queries(evaluation / f"{split}-queries.jsonl", authors, families, split)
    expected = {row["post"]["id"]: row["post"] for row in audit if row["status"] == "eligible" and row["post"]["split"] == split}
    demand({row["post"]["id"] for row in queries["rows"]} == set(expected), "query IDs differ from exact eligible split")
    demand(all(row["post"] == expected[row["post"]["id"]] for row in queries["rows"]), "query source metadata differs")
    demand(set(queries["query_authors"]) == set(authors), "eligible query split lacks an author")
    queries["id"] = descriptor["id"]
    queries["candidate_authors"] = authors
    queries["artifact_sha256"] = {name: sha256(evaluation / name) for name in filenames + ["implementation.json"]}
    return queries


def eligibility_manifest(path, protocol_hash, expected_ids):
    manifest = read_json(path)
    demand(manifest["schema"] == ELIGIBILITY_SCHEMA and manifest["protocol_sha256"] == protocol_hash,
           "eligibility manifest protocol binding differs")
    demand(manifest["no_performance_based_exclusions"] is True, "eligibility was not frozen independently of scores")
    demand(set(manifest["cohorts"]) == set(expected_ids), "eligibility manifest cohort IDs differ")
    return manifest

"""Confirm previously frozen author scorers on a separately frozen fresh cohort.

This module contains no optimizer, feature fitting, or checkpoint selection.
The original frontier trainer and scoring helpers remain unchanged.
"""
import argparse
import time
from pathlib import Path

import numpy as np
import torch

from frontier_common import (METRIC_NAMES, demand, load_queries, query_metrics,
                             read_json, score_model, sha256, summarize, write_json)
from frontier_models import CATALOG, original_weights, make_model
from train_frontier import configure, paired_difference


SCHEMA = "unslop-spline-confirmation-protocol-v1"
MODELS = ["spline_45", "diagonal_15", "tied_8"]
SEEDS = [0, 17, 29]
IDENTITY = ["source_space_id", "feature_schema", "parser_identity"]
CHECKPOINT_POLICY = "reuse prior per-seed development-selected checkpoints; no fitting or reselection"


def validate_protocol(protocol):
    demand(protocol["schema"] == SCHEMA, "unsupported confirmation protocol")
    demand(protocol["models"] == MODELS and protocol["seeds"] == SEEDS,
           "confirmation models or seeds differ from fixed comparison")
    demand(protocol["split"] == "test", "confirmation uses the fresh test split only")
    demand(protocol["checkpoint_policy"] == CHECKPOINT_POLICY, "checkpoint policy differs")
    demand(protocol["primary"] == {"left": "spline_45", "right": "diagonal_15", "metric": "macro_author.top1"},
           "primary comparison differs")
    demand(protocol["bootstrap"] == {"replicates": 2000, "seed": "0x6a09e667f3bcc909",
                                     "percentile_indices_zero_based": [49, 1949]},
           "bootstrap protocol differs")
    for key, expected in [("device", "cpu"), ("dtype", "float64"), ("threads", 8)]:
        demand(protocol[key] == expected, f"confirmation runtime differs: {key}")
    authors = protocol["cohort"]["author_ids"]
    prior = protocol["cohort"]["excluded_prior_author_ids"]
    demand(authors == sorted(set(authors)) and len(authors) == 100,
           "new source cohort must contain 100 unique sorted authors")
    demand(prior == sorted(set(prior)) and len(prior) == 300,
           "expected all 300 prior export authors")
    demand(set(authors).isdisjoint(prior), "confirmation authors overlap a prior cohort")


def validate_source(fit, protocol):
    """Read only previously frozen model files, never earlier test scores."""
    demand(sha256(fit / "selection.json") == protocol["source_selection_sha256"],
           "prior frontier selection changed")
    selection = read_json(fit / "selection.json")
    demand(selection["schema"] == "unslop-frozen-model-size-frontier-v1", "unsupported source frontier")
    for key in IDENTITY:
        demand(selection[key] == protocol[key], f"source identity differs: {key}")
    demand(set(selection["selection_author_ids"]).issubset(protocol["cohort"]["excluded_prior_author_ids"]),
           "prior exclusions do not cover training authors")
    for filename, key in [("transforms.json", "transforms_sha256"), ("implementation.json", "implementation_sha256")]:
        demand(sha256(fit / filename) == selection[key], f"frozen source artifact changed: {filename}")
    implementation = read_json(fit / "implementation.json")
    source = Path(__file__).parent
    for filename, digest in implementation["source_sha256"].items():
        demand(sha256(source / filename) == digest, f"frozen training/scoring source changed: {filename}")
    demand(implementation["identity"] == {key: selection[key] for key in IDENTITY},
           "old implementation identity differs")
    transforms = read_json(fit / "transforms.json")
    selected_runs = {}
    checkpoint_bindings = []
    declared_checkpoints = {(row["config"]["name"], row["seed"]): row for row in protocol["selected_checkpoints"]}
    demand(len(declared_checkpoints) == len(protocol["selected_checkpoints"]) == 9,
           "protocol must bind exactly nine selected checkpoints")
    for name in MODELS:
        config = next(row for row in CATALOG if row["name"] == name)
        demand(config in selection["models"], "requested architecture missing from frozen catalog")
        runs = [row for row in selection["runs"] if row["config"]["name"] == name]
        demand(len(runs) == 3 and sorted(row["seed"] for row in runs) == SEEDS,
               "source architecture lacks exactly three fixed seeds")
        for run in sorted(runs, key=lambda row: row["seed"]):
            demand(run["status"] == "complete" and run["config"] == config,
                   "source seed failed or architecture differs")
            declared = declared_checkpoints.get((name, run["seed"]))
            demand(declared is not None and all(run[key] == declared[key] for key in
                   ["config", "seed", "selected_epoch", "selected_checkpoint", "selected_checkpoint_sha256"]),
                   "source checkpoint differs from new frozen confirmation protocol")
            path = fit / name / f"seed-{run['seed']}" / run["selected_checkpoint"]
            demand(path.resolve().is_relative_to(fit.resolve()), "checkpoint path escapes frozen fit")
            demand(sha256(path) == run["selected_checkpoint_sha256"], "frozen selected checkpoint changed")
            stored = torch.load(path, weights_only=True)
            demand(stored["schema"] == "unslop-frontier-checkpoint-v1" and stored["config"] == config,
                   "checkpoint schema or architecture differs")
            demand(stored["seed"] == run["seed"] and stored["epoch"] == run["selected_epoch"],
                   "checkpoint seed or selected epoch differs")
            demand(stored["development"] == run["selected_development"] and
                   stored["training_cross_entropy"] == run["selected_training_cross_entropy"],
                   "checkpoint differs from frozen selected record")
            selected_runs[(name, run["seed"])] = (config, run, stored)
            checkpoint_bindings.append({"model": name, "seed": run["seed"], "epoch": run["selected_epoch"],
                                        "sha256": run["selected_checkpoint_sha256"]})
    return selection, transforms, selected_runs, checkpoint_bindings


def validate_eligibility(protocol, protocol_hash, eligibility, exclusions, audit):
    demand(eligibility["schema"] == "unslop-spline-confirmation-eligibility-v1", "unsupported eligibility manifest")
    demand(eligibility["protocol_sha256"] == protocol_hash, "eligibility refers to a different protocol")
    demand(eligibility["no_performance_based_exclusions"] is True, "eligibility was not frozen independently of scores")
    authors = eligibility["eligible_author_ids"]
    removed = eligibility["excluded_authors"]
    demand(authors == sorted(set(authors)) and len(authors) >= 2, "invalid eligible author list")
    demand(set(authors).isdisjoint(removed) and set(authors) | set(removed) == set(protocol["cohort"]["author_ids"]),
           "eligible and excluded authors do not partition the frozen cohort")
    demand(exclusions["eligible_author_ids"] == authors and exclusions["excluded_authors"] == removed,
           "Rust author exclusions differ from frozen eligibility")
    demand(exclusions["candidate_authors"] == eligibility["candidate_authors"] == len(authors),
           "eligible candidate count differs")
    demand(exclusions["input_authors"] == len(protocol["cohort"]["author_ids"]), "Rust original author count differs")
    demand(len({row["post"]["id"] for row in audit}) == len(audit), "duplicate annotation audit IDs")
    demand(all(row["post"]["author_id"] in protocol["cohort"]["author_ids"] for row in audit),
           "audit contains unknown source authors")
    removed_posts = {(row["id"], row["split"]) for row in eligibility["excluded_posts"]}
    demand(len(removed_posts) == len(eligibility["excluded_posts"]), "duplicate excluded-post declarations")
    actual_removed = {(row["post"]["id"], row["post"]["split"]) for row in audit
                      if row["status"] != "eligible" and row["post"]["author_id"] not in removed}
    demand(actual_removed == removed_posts, "post exclusions differ from frozen eligibility")
    demand(all(row["status"] != "eligible" for row in audit if row["post"]["author_id"] in removed),
           "excluded author remains eligible in audit")
    for split in ["train", "dev", "test"]:
        count = sum(row["status"] == "eligible" and row["post"]["split"] == split for row in audit)
        demand(count == eligibility[f"{split}_posts"], f"eligible {split} count differs")
    return authors


def validate_fresh(evaluation, protocol, protocol_hash, eligibility, selection):
    demand(sha256(evaluation / "implementation.json") == eligibility["rust_implementation_sha256"],
           "Rust implementation differs from frozen eligibility")
    for filename, key in [("annotation-audit.json", "annotation_audit_sha256"),
                          ("author-exclusions.json", "author_exclusions_sha256")]:
        demand(sha256(evaluation / filename) == eligibility[key], f"eligibility artifact changed: {filename}")
    implementation = read_json(evaluation / "implementation.json")
    demand(implementation["schema"] == "unslop-author-evaluation-implementation-v1", "unsupported Rust evaluation")
    filenames = ["source-summary.json", "protocol.json", "space.json", "author-exclusions.json",
                 "annotation-audit.json", "report.json", "test-queries.jsonl"]
    for name in filenames:
        demand(sha256(evaluation / name) == implementation["artifacts_sha256"][name],
               f"Rust evaluation artifact changed: {name}")
    fresh_protocol = read_json(evaluation / "protocol.json")
    demand(fresh_protocol["input_bindings"]["summary_sha256"] == protocol["cohort"]["summary_sha256"],
           "source cohort summary differs")
    demand(fresh_protocol["frozen_selection"]["source_space_id"] == protocol["source_space_id"],
           "fresh numerical reference space differs")
    demand(fresh_protocol["frozen_selection"]["source_space_sha256"] == protocol["reference_space_sha256"],
           "fresh reference space bytes differ")
    for key in ["feature_schema", "parser_identity"]:
        demand(fresh_protocol[key] == protocol[key], f"fresh identity differs: {key}")
    summary = read_json(evaluation / "source-summary.json")
    demand(sorted(row["author_id"] for row in summary["authors"]) == protocol["cohort"]["author_ids"],
           "fresh source author catalog differs")
    exclusions = read_json(evaluation / "author-exclusions.json")
    audit = read_json(evaluation / "annotation-audit.json")
    authors = validate_eligibility(protocol, protocol_hash, eligibility, exclusions, audit)
    fresh_space = read_json(evaluation / "space.json")
    demand(fresh_space["feature_schema"] == protocol["feature_schema"] and
           fresh_space["parser_identity"] == protocol["parser_identity"], "fresh space identity differs")
    demand(sorted(fresh_space["family_schemas"]) == selection["families"], "fresh space family catalog differs")
    queries = load_queries(evaluation / "test-queries.jsonl", authors, selection["families"], "test")
    eligible_rows = {row["post"]["id"]: row["post"] for row in audit
                     if row["status"] == "eligible" and row["post"]["split"] == "test"}
    demand({row["post"]["id"] for row in queries["rows"]} == set(eligible_rows), "test query IDs differ from eligibility")
    demand(all(row["post"] == eligible_rows[row["post"]["id"]] for row in queries["rows"]), "query provenance differs from audit")
    demand(set(queries["query_authors"]) == set(authors), "test queries omit an eligible candidate author")
    report = read_json(evaluation / "report.json")
    demand(report["candidate_authors"] == len(authors) and report["test"]["query_count"] == len(queries["rows"]),
           "Rust report candidate or query count differs")
    demand(report["space_id"] == fresh_space["id"] and report["train_posts"] == eligibility["train_posts"],
           "Rust report space or training count differs")
    return queries, report, {name: sha256(evaluation / name) for name in filenames}


def aggregate_seeds(seed_arrays, query_authors):
    demand(sorted(seed_arrays) == SEEDS, "exactly the original three seeds are required")
    arrays = [seed_arrays[seed] for seed in SEEDS]
    demand(all(array.shape == arrays[0].shape for array in arrays), "seed query arrays differ")
    return summarize(np.mean(arrays, axis=0), query_authors)


def run(args):
    demand(not args.out.exists(), "confirmation output already exists")
    protocol = read_json(args.protocol)
    validate_protocol(protocol)
    eligibility = read_json(args.eligibility)
    protocol_hash, eligibility_hash = sha256(args.protocol), sha256(args.eligibility)
    selection, transforms, runs, checkpoints = validate_source(args.fit, protocol)
    queries, rust_report, artifact_hashes = validate_fresh(args.evaluation, protocol, protocol_hash, eligibility, selection)
    # Create outputs only after every input and frozen checkpoint has validated.
    args.out.mkdir(parents=True)
    manifest = {"schema": "unslop-spline-confirmation-input-validation-v1", "protocol_sha256": protocol_hash,
                "eligibility_sha256": eligibility_hash, "source_selection_sha256": protocol["source_selection_sha256"],
                "source_transforms_sha256": selection["transforms_sha256"], "source_implementation_sha256": selection["implementation_sha256"],
                "checkpoints": checkpoints, "fresh_artifacts_sha256": artifact_hashes,
                "confirmation_source_sha256": sha256(Path(__file__)), "torch": torch.__version__,
                "candidate_authors": eligibility["candidate_authors"], "test_posts": eligibility["test_posts"],
                "training_calls": 0, "external_model_calls": 0, "detector_calls": 0}
    write_json(args.out / "input-validation.json", manifest)
    write_json(args.out / "protocol.json", protocol)
    write_json(args.out / "eligibility.json", eligibility)
    models = []
    for name in MODELS:
        seeds, arrays = [], {}
        for seed in SEEDS:
            config, run_info, checkpoint = runs[(name, seed)]
            model = make_model(config, selection["families"], transforms, seed)
            model.load_state_dict(checkpoint["model_state"])
            model.eval()
            began = time.perf_counter()
            metrics, logits, per_query = score_model(model, queries)
            elapsed = time.perf_counter() - began
            arrays[seed] = per_query
            output = args.out / f"{name}-seed-{seed}-scores.json"
            write_json(output, {"query_ids": [row["post"]["id"] for row in queries["rows"]],
                                "candidate_authors": eligibility["eligible_author_ids"], "logits": logits.tolist(),
                                "per_query_metrics": per_query.tolist()})
            seeds.append({"seed": seed, "selected_epoch": run_info["selected_epoch"], "metrics": metrics,
                          "scoring_seconds": elapsed, "scores_sha256": sha256(output),
                          "checkpoint_sha256": run_info["selected_checkpoint_sha256"]})
        models.append({"name": name, "parameters": config["parameters"], "seed_mean": aggregate_seeds(arrays, queries["query_authors"]),
                       "seeds": seeds, "seed_macro_top1_range": [min(row["metrics"]["macro_author"]["top1"] for row in seeds),
                                                                  max(row["metrics"]["macro_author"]["top1"] for row in seeds)]})
    fixed = {}
    for baseline in ["word", "combined"]:
        weights = original_weights(selection["families"])
        if baseline == "word":
            weights *= torch.tensor([1 if family in {"word", "word_bigram"} else 0 for family in selection["families"]])
            weights /= weights.sum()
        logits = -(queries["x"] * weights).sum(dim=-1).numpy()
        metrics = query_metrics(logits, queries["own"], queries["impostors"])
        fixed[baseline] = summarize(metrics, queries["query_authors"])
        expected = rust_report["test"]["variants"][baseline]["macro_author"]
        demand(all(abs(fixed[baseline]["macro_author"][key] - expected[key]) < 1e-12 for key in METRIC_NAMES),
               f"fixed {baseline} baseline differs from Rust")
    word = fixed["word"]
    by_name = {row["name"]: row for row in models}
    comparisons = {}
    for name in MODELS:
        per_author = by_name[name]["seed_mean"]["per_author"]
        if name != "diagonal_15":
            comparisons[f"{name}_minus_diagonal_15"] = paired_difference(per_author, by_name["diagonal_15"]["seed_mean"]["per_author"])
        comparisons[f"{name}_minus_word"] = paired_difference(per_author, word["per_author"])
    primary = comparisons["spline_45_minus_diagonal_15"]["metrics"]["top1"]
    result = {"schema": "unslop-spline-confirmation-result-v1", "split": "test", "protocol_sha256": protocol_hash,
              "eligibility_sha256": eligibility_hash, "source_selection_sha256": protocol["source_selection_sha256"],
              "input_validation_sha256": sha256(args.out / "input-validation.json"), "models": models,
              "fixed_word_baseline": word, "fixed_combined_baseline": fixed["combined"], "comparisons": comparisons,
              "primary": {"comparison": "spline_45_minus_diagonal_15", "metric": "macro_author.top1", **primary,
                          "positive_lower_95_bound": primary["percentile_95"][0] > 0},
              "candidate_authors": eligibility["candidate_authors"], "test_posts": eligibility["test_posts"],
              "seed_interpretation": "mean of per-query metrics from three frozen independent fits; no ensemble or best-seed selection",
              "training_calls": 0, "external_model_calls": 0, "detector_calls": 0}
    write_json(args.out / "report.json", result)
    print(f"Evaluated frozen confirmation on {eligibility['candidate_authors']} authors / {eligibility['test_posts']} posts: "
          f"spline45-minus-diagonal15 {primary['mean_difference']:.6f}, 95% interval {primary['percentile_95']}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["fit", "evaluation", "protocol", "eligibility", "out"]:
        parser.add_argument(f"--{name}", type=Path, required=True)
    args = parser.parse_args()
    configure()
    run(args)


if __name__ == "__main__":
    main()

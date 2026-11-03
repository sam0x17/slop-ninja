"""Train a frozen model-size frontier, then evaluate frozen fits on new authors."""
import argparse
import copy
import json
import math
import platform
import statistics
import time
import traceback
from pathlib import Path

import numpy as np
import torch

from frontier_common import (METRIC_NAMES, demand, load_queries, load_training,
                             query_metrics, read_json, score_model, sha256,
                             summarize, write_json)
from frontier_models import CATALOG, SEEDS, fit_transforms, make_model, original_weights


def configure():
    torch.set_num_threads(8)
    torch.set_num_interop_threads(1)
    torch.use_deterministic_algorithms(True)
    torch.set_default_dtype(torch.float64)
    demand(torch.__version__.split("+")[0] == "2.14.0", "training requires pinned PyTorch 2.14.0")


def validate_protocol(protocol):
    demand(protocol["schema"] == "unslop-model-size-frontier-protocol-v1", "unsupported frontier protocol")
    demand(protocol["models"] == CATALOG, "frozen model catalog differs")
    training = protocol["training"]
    for key, expected in {"seeds": SEEDS, "epochs": 200, "optimizer": "full-batch Adam",
                          "learning_rate": 0.03, "beta1": 0.9, "beta2": 0.999, "epsilon": 1e-8,
                          "device": "cpu", "dtype": "float64", "threads": 8,
                          "deterministic_algorithms": True, "log_temperature_bounds": [-6.0, 2.0]}.items():
        demand(training[key] == expected, f"frozen training setting differs: {key}")
    demand(protocol["spline"]["initial_log_slopes"] == 0 and protocol["spline"]["log_slope_bounds"] == [-6.0, 6.0], "spline settings differ")
    demand(protocol["mlp"]["initialization"] == "seeded Xavier uniform weight matrices, zero hidden biases", "MLP initialization differs")


def optimizer_for(model):
    return torch.optim.Adam(model.parameters(), lr=0.03, betas=(0.9, 0.999), eps=1e-8, foreach=False, fused=False)


def training_loss(model, training):
    logits = model(training["x"])
    return (torch.nn.functional.cross_entropy(logits, training["own"], reduction="none") * training["loss_weight"]).sum()


def state_lists(model):
    return {name: parameter.detach().tolist() for name, parameter in model.named_parameters()}


def run_fit(config, seed, training, transforms, directory):
    directory.mkdir()
    checkpoints = directory / "checkpoints"
    checkpoints.mkdir()
    model = make_model(config, training["families"], transforms, seed)
    optimizer = optimizer_for(model)
    history = directory / "epochs.jsonl"
    best, best_score = None, (-math.inf, -math.inf)
    began = time.perf_counter()
    training_seconds = 0.0
    scoring_seconds = 0.0
    original = None
    replay_errors = []
    if config["kind"] == "diagonal" and seed == 0:
        original = [json.loads(line) for line in (training["original_fit"] / "epochs.jsonl").read_text().splitlines()]
        demand(len(original) == 201, "original epoch history differs")
    with history.open("w") as log:
        for epoch in range(201):
            start = time.perf_counter()
            optimizer.zero_grad(set_to_none=True)
            loss = training_loss(model, training)
            demand(bool(torch.isfinite(loss)), "nonfinite training loss")
            loss.backward()
            demand(all(bool(torch.isfinite(p.grad).all()) for p in model.parameters()), "nonfinite gradient")
            gradient_norm = math.sqrt(sum(p.grad.square().sum().item() for p in model.parameters()))
            training_seconds += time.perf_counter() - start
            start = time.perf_counter()
            development, _, _ = score_model(model, training["development"])
            scoring_seconds += time.perf_counter() - start
            score = (development["macro_author"]["top1"], development["macro_author"]["mrr"])
            record = {"epoch": epoch, "training_cross_entropy": loss.item(), "gradient_l2": gradient_norm,
                      "development": development, "parameters": state_lists(model),
                      "elapsed_seconds": time.perf_counter() - began,
                      "training_seconds_cumulative": training_seconds,
                      "development_scoring_seconds_cumulative": scoring_seconds}
            checkpoint_path = checkpoints / f"epoch-{epoch:03}.pt"
            torch.save({"schema": "unslop-frontier-checkpoint-v1", "config": config, "seed": seed,
                        "epoch": epoch, "model_state": model.state_dict(), "optimizer_state": optimizer.state_dict(),
                        "training_cross_entropy": loss.item(), "development": development}, checkpoint_path)
            record["checkpoint"] = str(checkpoint_path.relative_to(directory))
            record["checkpoint_sha256"] = sha256(checkpoint_path)
            log.write(json.dumps(record, allow_nan=False) + "\n")
            log.flush()
            if score > best_score:
                best_score, best = score, copy.deepcopy(record)
            if original is not None:
                expected = original[epoch]
                errors = {"epoch": epoch,
                          "loss": abs(expected["training_cross_entropy"] - loss.item()),
                          "theta": max(abs(a - b) for a, b in zip(expected["model"]["theta"], model.theta.detach().tolist())),
                          "log_tau": abs(expected["model"]["log_tau"] - model.log_tau.item()),
                          "dev_top1": abs(expected["development"]["macro_author"]["top1"] - score[0]),
                          "dev_mrr": abs(expected["development"]["macro_author"]["mrr"] - score[1])}
                replay_errors.append(errors)
                demand(max(errors[key] for key in errors if key != "epoch") < 1e-8, f"Rust baseline replay differs at epoch {epoch}: {errors}")
            if epoch % 50 == 0:
                print(f"{config['name']} seed {seed} epoch {epoch}: loss {loss.item():.6f}, dev top1 {score[0]:.6f}", flush=True)
            if epoch < 200:
                start = time.perf_counter()
                optimizer.step()
                model.constrain()
                training_seconds += time.perf_counter() - start
    demand(best is not None, "missing checkpoint selection")
    best_state = torch.load(directory / best["checkpoint"], weights_only=True)
    model.load_state_dict(best_state["model_state"])
    reproduced, _, _ = score_model(model, training["development"])
    demand(reproduced == best["development"], "selected checkpoint replay differs")
    # Query scoring cost excludes checkpoint I/O; perform one warm-up and five repeats.
    with torch.no_grad():
        model(training["development"]["x"])
        durations = []
        for _ in range(5):
            start = time.perf_counter()
            model(training["development"]["x"])
            durations.append(time.perf_counter() - start)
    summary = {"status": "complete", "config": config, "seed": seed,
               "actual_trainable_parameters": sum(p.numel() for p in model.parameters()),
               "selected_epoch": best["epoch"], "selected_checkpoint": best["checkpoint"],
               "selected_checkpoint_sha256": best["checkpoint_sha256"],
               "selected_development": best["development"], "selected_training_cross_entropy": best["training_cross_entropy"],
               "epochs_sha256": sha256(history), "elapsed_seconds": time.perf_counter() - began,
               "training_seconds": training_seconds, "development_scoring_seconds": scoring_seconds,
               "scoring_seconds_median": statistics.median(durations),
               "scoring_query_count": len(training["development"]["rows"]),
               "scoring_candidate_count": len(training["authors"])}
    if original is not None:
        old_selection = read_json(training["original_fit"] / "selection.json")
        demand(best["epoch"] == old_selection["selected_checkpoint"]["epoch"], "original selected epoch differs")
        audit = {"status": "pass", "tolerance": 1e-8, "epochs": replay_errors,
                 "selected_epoch": best["epoch"], "original_selected_epoch": old_selection["selected_checkpoint"]["epoch"]}
        write_json(directory / "rust-replay.json", audit)
        summary["rust_replay_sha256"] = sha256(directory / "rust-replay.json")
    write_json(directory / "summary.json", summary)
    return summary


def choose_model(summaries):
    candidates = []
    for config in CATALOG:
        runs = [row for row in summaries if row["config"]["name"] == config["name"] and row["status"] == "complete"]
        if sorted(row["seed"] for row in runs) != SEEDS:
            continue
        means = {key: statistics.mean(row["selected_development"]["macro_author"][key] for row in runs) for key in METRIC_NAMES}
        candidates.append({"name": config["name"], "parameters": config["parameters"], "mean_seed_development": means})
    demand(candidates, "no model completed all seeds")
    candidates.sort(key=lambda row: (-row["mean_seed_development"]["top1"], -row["mean_seed_development"]["mrr"], row["parameters"], row["name"]))
    return candidates[0]["name"], candidates


def train(args):
    demand(not args.out.exists(), "training output already exists")
    protocol = read_json(args.protocol)
    validate_protocol(protocol)
    args.out.mkdir(parents=True)
    write_json(args.out / "protocol.json", protocol)
    eligibility = read_json(args.eligibility_addendum)
    demand(eligibility["schema"] == "unslop-model-size-frontier-eligibility-addendum-v1" and eligibility["no_performance_based_exclusions"] is True, "unsupported eligibility addendum")
    write_json(args.out / "eligibility-addendum.json", eligibility)
    training = load_training(args.original_fit, args.evaluation)
    demand(training["identity"]["source_space_id"] == protocol["source_space_id"], "frozen space differs")
    transforms = fit_transforms(training["x"])
    write_json(args.out / "transforms.json", transforms)
    source = Path(__file__).parent
    manifest = {"schema": "unslop-frontier-training-implementation-v1", "protocol_sha256": sha256(args.protocol),
                "inputs": training["bindings"], "identity": training["identity"], "authors": training["authors"],
                "families": training["families"], "training_count": len(training["rows"]),
                "development_count": len(training["development"]["rows"]),
                "torch": torch.__version__, "python": platform.python_version(),
                "eligibility_addendum_sha256": sha256(args.eligibility_addendum),
                "source_sha256": {name: sha256(source / name) for name in
                                  ["train_frontier.py", "frontier_models.py", "frontier_common.py", "training-lock.json"]}}
    write_json(args.out / "implementation.json", manifest)
    summaries = []
    for config in CATALOG:
        model_path = args.out / config["name"]
        model_path.mkdir()
        for seed in SEEDS:
            directory = model_path / f"seed-{seed}"
            start = time.perf_counter()
            try:
                summary = run_fit(config, seed, training, transforms, directory)
            except Exception as error:
                summary = {"status": "failed", "config": config, "seed": seed, "error": str(error),
                           "traceback": traceback.format_exc(), "elapsed_seconds": time.perf_counter() - start}
                directory.mkdir(exist_ok=True)
                write_json(directory / "failure.json", summary)
                print(f"FAILED {config['name']} seed {seed}: {error}", flush=True)
            summaries.append(summary)
            write_json(args.out / "progress.json", {"completed_runs": len(summaries), "planned_runs": 45, "runs": summaries})
            print(f"Finished {config['name']} seed {seed}: {summary['status']} ({time.perf_counter() - start:.1f}s)", flush=True)
    winner, candidates = choose_model(summaries)
    output = {"schema": "unslop-frozen-model-size-frontier-v1", "selected_model": winner,
              "model_selection": protocol["training"]["model_selection"], "models": CATALOG, "seeds": SEEDS,
              "selection_author_ids": training["authors"], "families": training["families"], **training["identity"],
              "input_sha256": training["bindings"], "protocol_sha256": sha256(args.protocol),
              "eligibility_addendum_sha256": sha256(args.eligibility_addendum),
              "transforms_sha256": sha256(args.out / "transforms.json"),
              "implementation_sha256": sha256(args.out / "implementation.json"),
              "candidates": candidates, "runs": summaries,
              "completed_runs": sum(row["status"] == "complete" for row in summaries),
              "failed_runs": sum(row["status"] == "failed" for row in summaries)}
    write_json(args.out / "selection.json", output)
    print(f"FROZEN selected model {winner}; selection SHA256 {sha256(args.out / 'selection.json')}", flush=True)


def paired_difference(left, right):
    demand(left.keys() == right.keys() and left, "paired author catalogs differ")
    rows = np.array([[left[author][key] - right[author][key] for key in METRIC_NAMES] for author in sorted(left)])
    state, mask = 0x6A09E667F3BCC909, (1 << 64) - 1
    samples = []
    for _ in range(2000):
        indices = []
        for _ in rows:
            state ^= (state << 13) & mask
            state ^= state >> 7
            state ^= (state << 17) & mask
            indices.append(state % len(rows))
        samples.append(rows[indices].mean(axis=0))
    ordered = np.sort(np.array(samples), axis=0)
    return {"author_count": len(rows), "replicates": 2000, "seed": "0x6a09e667f3bcc909",
            "metrics": {key: {"mean_difference": rows[:, i].mean().item(), "percentile_95": [ordered[49, i].item(), ordered[1949, i].item()]}
                        for i, key in enumerate(METRIC_NAMES)}}


def evaluate(args):
    demand(not args.out.exists(), "evaluation output already exists")
    fit = args.fit
    selection = read_json(fit / "selection.json")
    demand(selection["schema"] == "unslop-frozen-model-size-frontier-v1", "unsupported frontier selection")
    demand(sha256(fit / "transforms.json") == selection["transforms_sha256"], "fitted transforms changed")
    demand(sha256(fit / "implementation.json") == selection["implementation_sha256"], "training implementation changed")
    frozen_protocol = read_json(fit / "protocol.json")
    demand(frozen_protocol == read_json(args.protocol), "frontier protocol changed")
    # Stored protocol is rewritten as pretty JSON at training time, so its exact
    # original-byte digest is bound separately in the selection.
    demand(sha256(args.protocol) == selection["protocol_sha256"], "original protocol digest changed")
    validate_protocol(frozen_protocol)
    eligibility = read_json(args.eligibility_addendum)
    demand(sha256(args.eligibility_addendum) == selection["eligibility_addendum_sha256"], "frozen eligibility addendum changed")
    demand(eligibility == read_json(fit / "eligibility-addendum.json"), "stored eligibility addendum changed")
    run_manifest = read_json(args.evaluation / "implementation.json")
    for name in ["source-summary.json", "author-exclusions.json", "annotation-audit.json", "protocol.json", "report.json", f"{args.split}-queries.jsonl"]:
        demand(sha256(args.evaluation / name) == run_manifest["artifacts_sha256"][name], f"fresh evaluation artifact changed: {name}")
    fresh_protocol = read_json(args.evaluation / "protocol.json")
    demand(fresh_protocol["input_bindings"]["summary_sha256"] == frozen_protocol["fresh_cohort"]["summary_sha256"], "fresh cohort summary differs")
    demand(fresh_protocol["frozen_selection"]["source_space_id"] == selection["source_space_id"], "fresh numerical space differs")
    demand(fresh_protocol["parser_identity"] == selection["parser_identity"] and fresh_protocol["feature_schema"] == selection["feature_schema"], "fresh parser/features differ")
    summary = read_json(args.evaluation / "source-summary.json")
    all_authors = sorted(row["author_id"] for row in summary["authors"])
    demand(len(all_authors) == len(set(all_authors)) == 100, "fresh source author catalog differs")
    exclusions = read_json(args.evaluation / "author-exclusions.json")
    authors = exclusions["eligible_author_ids"]
    demand(authors == sorted(set(authors)) and set(authors).isdisjoint(exclusions["excluded_authors"]), "invalid author attrition")
    demand(set(authors) | set(exclusions["excluded_authors"]) == set(all_authors), "unaccounted fresh author attrition")
    demand(set(exclusions["excluded_authors"]) == set(eligibility["excluded_authors"]), "excluded authors differ from frozen addendum")
    demand(len(authors) == eligibility["candidate_authors"], "eligible author count differs from frozen addendum")
    demand(set(authors).isdisjoint(selection["selection_author_ids"]), "fresh authors overlap training authors")
    report = read_json(args.evaluation / "report.json")
    demand(len(authors) == report["candidate_authors"] == exclusions["candidate_authors"], "fresh candidate count differs")
    # Hash-bind the evaluator's files before reading any query scores.
    query_path = args.evaluation / f"{args.split}-queries.jsonl"
    queries = load_queries(query_path, authors, selection["families"], args.split)
    audit = read_json(args.evaluation / "annotation-audit.json")
    unexpected = {row["post"]["id"] for row in audit if row["status"] != "eligible" and row["post"]["author_id"] not in eligibility["excluded_authors"]}
    demand(unexpected == {row["id"] for row in eligibility["excluded_posts"]}, "excluded posts differ from frozen addendum")
    for split in ["train", "dev", "test"]:
        count = sum(row["status"] == "eligible" and row["post"]["split"] == split for row in audit)
        demand(count == eligibility[f"{split}_posts"], f"eligible {split} count differs from frozen addendum")
    eligible_ids = {row["post"]["id"] for row in audit if row["status"] == "eligible" and row["post"]["split"] == args.split}
    demand({row["post"]["id"] for row in queries["rows"]} == eligible_ids, "fresh query eligibility differs")
    demand(len(queries["rows"]) == report[args.split]["query_count"], "fresh query count differs")
    transforms = read_json(fit / "transforms.json")
    args.out.mkdir(parents=True)
    records = []
    per_model = {}
    for config in selection["models"]:
        run_summaries = [run for run in selection["runs"] if run["config"] == config and run["status"] == "complete"]
        if len(run_summaries) != 3:
            records.append({"config": config, "status": "ineligible_failed_training", "completed_seeds": len(run_summaries)})
            continue
        seed_results, seed_metrics = [], []
        for run in run_summaries:
            seed = run["seed"]
            checkpoint = fit / config["name"] / f"seed-{seed}" / run["selected_checkpoint"]
            demand(sha256(checkpoint) == run["selected_checkpoint_sha256"], "selected checkpoint changed")
            stored = torch.load(checkpoint, weights_only=True)
            demand(stored["config"] == config and stored["seed"] == seed and stored["epoch"] == run["selected_epoch"], "checkpoint metadata differs")
            model = make_model(config, selection["families"], transforms, seed)
            model.load_state_dict(stored["model_state"])
            began = time.perf_counter()
            metrics, logits, per_query = score_model(model, queries)
            duration = time.perf_counter() - began
            score_path = args.out / f"{config['name']}-seed-{seed}-scores.json"
            write_json(score_path, {"query_ids": [row["post"]["id"] for row in queries["rows"]], "candidate_authors": authors,
                                    "logits": logits.tolist(), "per_query_metrics": per_query.tolist()})
            seed_results.append({"seed": seed, "selected_epoch": run["selected_epoch"], "metrics": metrics,
                                 "scoring_seconds": duration, "scores_sha256": sha256(score_path)})
            seed_metrics.append(per_query)
        average = summarize(np.mean(seed_metrics, axis=0), queries["query_authors"])
        per_model[config["name"]] = average["per_author"]
        records.append({"config": config, "status": "complete", "parameter_ratio_to_15": config["parameters"] / 15,
                        "seed_mean": average, "seeds": seed_results,
                        "seed_macro_top1_range": [min(row["metrics"]["macro_author"]["top1"] for row in seed_results), max(row["metrics"]["macro_author"]["top1"] for row in seed_results)],
                        "training_seconds_sum": sum(run["training_seconds"] for run in run_summaries),
                        "training_elapsed_seconds_sum": sum(run["elapsed_seconds"] for run in run_summaries)})
    weights = original_weights(selection["families"])
    word_weights = weights * torch.tensor([1 if f in {"word", "word_bigram"} else 0 for f in selection["families"]])
    word_weights /= word_weights.sum()
    fixed = {}
    for name, w in [("word", word_weights), ("combined", weights)]:
        logits = -(queries["x"] * w).sum(dim=-1).numpy()
        metrics = query_metrics(logits, queries["own"], queries["impostors"])
        fixed[name] = summarize(metrics, queries["query_authors"])
        expected = report[args.split]["variants"][name]["macro_author"]
        demand(all(abs(fixed[name]["macro_author"][key] - expected[key]) < 1e-12 for key in METRIC_NAMES), "Rust fixed baseline metrics differ")
    for record in records:
        if record["status"] == "complete":
            per_author = record["seed_mean"]["per_author"]
            record["paired_vs_diagonal_15"] = paired_difference(per_author, per_model["diagonal_15"])
            record["paired_vs_word"] = paired_difference(per_author, fixed["word"]["per_author"])
    output = {"schema": "unslop-model-size-frontier-evaluation-v1", "split": args.split,
              "selected_model": selection["selected_model"], "selection_sha256": sha256(fit / "selection.json"),
              "fresh_query_sha256": sha256(query_path), "fresh_run_manifest_sha256": sha256(args.evaluation / "implementation.json"),
              "author_exclusions": exclusions, "eligible_query_count":len(queries["rows"]),
              "eligibility_addendum_sha256": selection["eligibility_addendum_sha256"],
              "fresh_report_sha256": sha256(args.evaluation / "report.json"), "fixed_baselines": fixed, "models": records,
              "seed_interpretation": "mean of per-query and per-author metrics over independent fits, not an ensemble or best-seed selection"}
    write_json(args.out / "report.json", output)
    print(f"Evaluated frozen frontier: selected {selection['selected_model']}; {len(queries['rows'])} {args.split} queries", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    training = commands.add_parser("train")
    training.add_argument("--original-fit", type=Path, required=True)
    training.add_argument("--evaluation", type=Path, required=True)
    scoring = commands.add_parser("evaluate")
    scoring.add_argument("--fit", type=Path, required=True)
    scoring.add_argument("--evaluation", type=Path, required=True)
    scoring.add_argument("--split", choices=["dev", "test"], default="test")
    for command in [training, scoring]:
        command.add_argument("--protocol", type=Path, required=True)
        command.add_argument("--eligibility-addendum", type=Path, required=True)
        command.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    configure()
    train(args) if args.command == "train" else evaluate(args)


if __name__ == "__main__":
    main()

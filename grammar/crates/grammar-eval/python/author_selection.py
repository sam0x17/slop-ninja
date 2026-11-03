"""Select word/grammar scorers on disjoint authors, then test frozen pipelines."""
import argparse
import copy
import json
import math
import statistics
import time
import traceback
from pathlib import Path

import numpy as np
import torch

from author_selection_io import (ARCHITECTURE_RULE, IDENTITY, eligibility_manifest,
                                 load_cohort, load_training_tensor, validate_protocol)
from author_selection_models import (CONFIGS, SEEDS, SUBSETS, fit_knots,
                                     included_families, make_model, prior_weights)
from frontier_common import (METRIC_NAMES, demand, query_metrics, read_json,
                             score_model, sha256, summarize, write_json)
from train_frontier import configure


SOURCES = ["author_selection.py", "author_selection_io.py", "author_selection_models.py",
           "frontier_common.py", "frontier_models.py", "train_frontier.py", "training-lock.json"]


def fit_calibrations(training):
    output = {"schema": "slopninja-subset-distance-calibrations-v1", "subsets": {}}
    for subset in SUBSETS:
        families = included_families(training["families"], subset)
        indices = [training["families"].index(name) for name in families]
        fitted = fit_knots(training["x"][:, :, indices])
        output["subsets"][subset] = {"families": families, "family_indices": indices, **fitted}
    return output


def train_seed(config, seed, training, validation, calibrations, out):
    out.mkdir()
    checkpoints = out / "checkpoints"
    checkpoints.mkdir()
    model = make_model(config, training["families"], calibrations["subsets"][config["subset"]]["knots"], seed)
    optimizer = torch.optim.Adam(model.parameters(), lr=0.03, betas=(0.9, 0.999), eps=1e-8, foreach=False, fused=False)
    best_score, best = (-math.inf, -math.inf), None
    start_time = time.perf_counter()
    train_seconds, validation_seconds = 0.0, 0.0
    with (out / "epochs.jsonl").open("w") as history:
        for epoch in range(201):
            began = time.perf_counter()
            optimizer.zero_grad(set_to_none=True)
            logits = model(training["x"])
            loss = (torch.nn.functional.cross_entropy(logits, training["own"], reduction="none") * training["loss_weight"]).sum()
            demand(bool(torch.isfinite(loss)), "nonfinite training loss")
            loss.backward()
            demand(all(bool(torch.isfinite(parameter.grad).all()) for parameter in model.parameters()), "nonfinite gradient")
            gradient_l2 = math.sqrt(sum(parameter.grad.square().sum().item() for parameter in model.parameters()))
            train_seconds += time.perf_counter() - began
            began = time.perf_counter()
            metrics, _, _ = score_model(model, validation)
            validation_seconds += time.perf_counter() - began
            score = (metrics["macro_author"]["top1"], metrics["macro_author"]["mrr"])
            path = checkpoints / f"epoch-{epoch:03}.pt"
            torch.save({"schema": "slopninja-author-selection-checkpoint-v1", "config": config, "seed": seed,
                        "epoch": epoch, "model_state": model.state_dict(), "optimizer_state": optimizer.state_dict(),
                        "training_cross_entropy": loss.item(), "validation": metrics}, path)
            record = {"epoch": epoch, "training_cross_entropy": loss.item(), "validation": metrics,
                      "parameters": {name: parameter.detach().tolist() for name, parameter in model.named_parameters()},
                      "gradient_l2": gradient_l2, "checkpoint": str(path.relative_to(out)), "checkpoint_sha256": sha256(path),
                      "elapsed_seconds": time.perf_counter() - start_time, "training_seconds_cumulative": train_seconds,
                      "validation_seconds_cumulative": validation_seconds}
            history.write(json.dumps(record, allow_nan=False) + "\n")
            history.flush()
            if score > best_score:
                best_score, best = score, copy.deepcopy(record)
            if epoch % 50 == 0:
                print(f"{config['name']} seed {seed} epoch {epoch}: loss {loss.item():.6f}, fresh-author validation top1 {score[0]:.6f}", flush=True)
            if epoch < 200:
                began = time.perf_counter()
                optimizer.step()
                model.constrain()
                train_seconds += time.perf_counter() - began
    demand(best is not None, "no validation checkpoint")
    stored = torch.load(out / best["checkpoint"], weights_only=True)
    model.load_state_dict(stored["model_state"])
    replay, _, _ = score_model(model, validation)
    demand(replay == best["validation"], "selected validation checkpoint replay differs")
    result = {"status": "complete", "config": config, "seed": seed,
              "actual_trainable_parameters": sum(parameter.numel() for parameter in model.parameters()),
              "selected_epoch": best["epoch"], "selected_checkpoint": best["checkpoint"],
              "selected_checkpoint_sha256": best["checkpoint_sha256"], "selected_validation": best["validation"],
              "selected_training_cross_entropy": best["training_cross_entropy"], "selected_parameters": best["parameters"],
              "epochs_sha256": sha256(out / "epochs.jsonl"), "elapsed_seconds": time.perf_counter() - start_time,
              "training_seconds": train_seconds, "validation_seconds": validation_seconds}
    write_json(out / "summary.json", result)
    return result


def select_architectures(runs):
    expected = {(config["name"], seed) for config in CONFIGS for seed in SEEDS}
    observed = {(run["config"]["name"], run["seed"]) for run in runs}
    demand(len(runs) == 18 and observed == expected and all(run["status"] == "complete" for run in runs),
           "study stops unless all eighteen prescribed fits complete successfully")
    selected, candidates = {}, {}
    for subset in SUBSETS:
        choices = []
        for config in CONFIGS:
            if config["subset"] != subset:
                continue
            seeds = [run for run in runs if run["config"] == config and run["status"] == "complete"]
            if len(seeds) != 3 or sorted(run["seed"] for run in seeds) != SEEDS:
                continue
            means = {key: statistics.mean(run["selected_validation"]["macro_author"][key] for run in seeds) for key in METRIC_NAMES}
            choices.append({"name": config["name"], "parameters": config["parameters"], "mean_seed_validation": means})
        demand(choices, f"no architecture completed all seeds for subset {subset}")
        choices.sort(key=lambda row: (-row["mean_seed_validation"]["top1"], -row["mean_seed_validation"]["mrr"], row["parameters"], row["name"]))
        selected[subset], candidates[subset] = choices[0]["name"], choices
    return selected, candidates


def train(args):
    demand(not args.out.exists(), "training output already exists")
    protocol = read_json(args.protocol)
    validate_protocol(protocol)
    protocol_hash = sha256(args.protocol)
    descriptor = protocol["cohorts"]["validation"]
    eligibility = eligibility_manifest(args.validation_eligibility, protocol_hash, [descriptor["id"]])
    training = load_training_tensor(args.original_fit, protocol)
    validation = load_cohort(args.validation_evaluation, descriptor, eligibility["cohorts"][descriptor["id"]],
                             protocol, training["families"], "validation")
    calibrations = fit_calibrations(training)
    args.out.mkdir(parents=True)
    write_json(args.out / "protocol.json", protocol)
    write_json(args.out / "validation-eligibility.json", eligibility)
    write_json(args.out / "calibrations.json", calibrations)
    source = Path(__file__).parent
    implementation = {"schema": "slopninja-author-selection-implementation-v1", "protocol_sha256": protocol_hash,
                      "validation_eligibility_sha256": sha256(args.validation_eligibility),
                      "source_tensor_sha256": protocol["source_tensor_sha256"], "source_selection_sha256": protocol["source_selection_sha256"],
                      "identity": {key: protocol[key] for key in IDENTITY}, "families": training["families"],
                      "training_author_ids": training["author_ids"], "training_post_count": len(training["post_ids"]),
                      "validation_author_ids": validation["candidate_authors"], "validation_query_count": len(validation["rows"]),
                      "validation_artifact_sha256": validation["artifact_sha256"],
                      "source_sha256": {name: sha256(source / name) for name in SOURCES},
                      "torch": torch.__version__, "original_development_queries_read": False,
                      "validation_test_queries_read": False, "test_cohort_queries_read": False}
    write_json(args.out / "implementation.json", implementation)
    runs = []
    for config in CONFIGS:
        directory = args.out / config["name"]
        directory.mkdir()
        for seed in SEEDS:
            destination = directory / f"seed-{seed}"
            began = time.perf_counter()
            try:
                result = train_seed(config, seed, training, validation, calibrations, destination)
            except Exception as error:
                destination.mkdir(exist_ok=True)
                result = {"status": "failed", "config": config, "seed": seed, "error": str(error),
                          "traceback": traceback.format_exc(), "elapsed_seconds": time.perf_counter() - began}
                write_json(destination / "failure.json", result)
                print(f"FAILED {config['name']} seed {seed}: {error}", flush=True)
                runs.append(result)
                write_json(args.out / "progress.json", {"completed_attempts": len(runs), "planned_attempts": 18, "runs": runs,
                                                         "status": "stopped_after_failed_fit"})
                raise RuntimeError(f"study stopped after failed fit {config['name']} seed {seed}") from error
            runs.append(result)
            write_json(args.out / "progress.json", {"completed_attempts": len(runs), "planned_attempts": 18, "runs": runs})
    selected, candidates = select_architectures(runs)
    selection = {"schema": "slopninja-frozen-author-selection-v1", "configs": CONFIGS, "seeds": SEEDS,
                 "selected_architectures": selected, "architecture_candidates": candidates, "selection_rule": ARCHITECTURE_RULE,
                 "protocol_sha256": protocol_hash, "validation_eligibility_sha256": sha256(args.validation_eligibility),
                 "calibrations_sha256": sha256(args.out / "calibrations.json"), "implementation_sha256": sha256(args.out / "implementation.json"),
                 "families": training["families"], **{key: protocol[key] for key in IDENTITY},
                 "training_author_ids": training["author_ids"], "selection_author_ids": validation["candidate_authors"],
                 "runs": runs, "completed_runs": sum(run["status"] == "complete" for run in runs),
                 "failed_runs": sum(run["status"] == "failed" for run in runs)}
    write_json(args.out / "selection.json", selection)
    print(f"FROZEN architectures {selected}; selection SHA256 {sha256(args.out / 'selection.json')}", flush=True)


def stratified_difference(left, right):
    """One xorshift stream; resample n_g authors per gallery, then average all N."""
    demand(left.keys() == right.keys() and left, "paired gallery catalogs differ")
    galleries, counts, all_rows = [], [], []
    for gallery in sorted(left):
        a, b = left[gallery], right[gallery]
        demand(a.keys() == b.keys() and a, "paired author catalogs differ")
        rows = np.array([[a[author][metric] - b[author][metric] for metric in METRIC_NAMES] for author in sorted(a)])
        galleries.append((gallery, rows))
        counts.append(len(rows))
        all_rows.extend(rows)
    total = sum(counts)
    state, mask = 0x6A09E667F3BCC909, (1 << 64) - 1
    samples = []
    for _ in range(2000):
        accum = np.zeros(len(METRIC_NAMES))
        for _, rows in galleries:
            for _ in rows:
                state ^= (state << 13) & mask
                state ^= state >> 7
                state ^= (state << 17) & mask
                accum += rows[state % len(rows)]
        samples.append(accum / total)
    ordered = np.sort(samples, axis=0)
    means = np.mean(all_rows, axis=0)
    return {"gallery_order": [name for name, _ in galleries], "gallery_author_counts": dict(zip([name for name, _ in galleries], counts)),
            "gallery_mean_differences": {gallery: {name: rows[:, index].mean().item() for index, name in enumerate(METRIC_NAMES)}
                                         for gallery, rows in galleries},
            "author_count": total, "replicates": 2000, "seed": "0x6a09e667f3bcc909",
            "metrics": {name: {"mean_difference": means[index].item(), "percentile_95": [ordered[49, index].item(), ordered[1949, index].item()]}
                        for index, name in enumerate(METRIC_NAMES)}}


def pooled_summary(gallery_metrics):
    per_author = {}
    for gallery, summary in gallery_metrics.items():
        for author, metrics in summary["per_author"].items():
            demand(author not in per_author, "author occurs in multiple galleries")
            per_author[author] = metrics
    mean = {key: statistics.mean(row[key] for row in per_author.values()) for key in METRIC_NAMES}
    total_queries = sum(row["query_count"] for row in gallery_metrics.values())
    micro = {key: sum(row["query_count"] * row["micro"][key] for row in gallery_metrics.values()) / total_queries for key in METRIC_NAMES}
    return {"query_count": total_queries, "author_count": len(per_author), "micro": micro,
            "macro_author": mean, "per_author": per_author}


def evaluate(args):
    demand(not args.out.exists(), "test output already exists")
    protocol = read_json(args.protocol)
    validate_protocol(protocol)
    protocol_hash = sha256(args.protocol)
    selection_path = args.fit / "selection.json"
    selection = read_json(selection_path)
    demand(selection["schema"] == "slopninja-frozen-author-selection-v1" and selection["protocol_sha256"] == protocol_hash,
           "source selection protocol differs")
    demand(read_json(args.fit / "protocol.json") == protocol, "stored training protocol differs")
    for key in IDENTITY:
        demand(selection[key] == protocol[key], f"source selection identity differs: {key}")
    demand(selection["configs"] == CONFIGS and selection["seeds"] == SEEDS, "frozen source catalog differs")
    for filename, key in [("calibrations.json", "calibrations_sha256"), ("implementation.json", "implementation_sha256")]:
        demand(sha256(args.fit / filename) == selection[key], f"frozen fit artifact changed: {filename}")
    implementation = read_json(args.fit / "implementation.json")
    for filename, digest in implementation["source_sha256"].items():
        demand(sha256(Path(__file__).parent / filename) == digest, f"frozen source changed: {filename}")
    selected, candidates = select_architectures(selection["runs"])
    demand(selected == selection["selected_architectures"] and candidates == selection["architecture_candidates"],
           "frozen validation-selected architectures differ")
    eligibility = eligibility_manifest(args.eligibility, protocol_hash, [row["id"] for row in protocol["cohorts"]["test"]])
    demand(eligibility["selection_sha256"] == sha256(selection_path), "test eligibility does not bind frozen model selection")
    demand(len(args.evaluation) == 3, "provide the three fixed test evaluation directories in protocol order")
    galleries = {}
    for descriptor, directory in zip(protocol["cohorts"]["test"], args.evaluation):
        galleries[descriptor["id"]] = load_cohort(directory, descriptor, eligibility["cohorts"][descriptor["id"]],
                                                   protocol, selection["families"], "test")
    calibrations = read_json(args.fit / "calibrations.json")
    checked = {}
    for config in CONFIGS:
        runs = [run for run in selection["runs"] if run["config"] == config and run["status"] == "complete"]
        demand(len(runs) == 3 and sorted(run["seed"] for run in runs) == SEEDS, "all six cells must have exactly three completed seeds")
        for run in runs:
            path = args.fit / config["name"] / f"seed-{run['seed']}" / run["selected_checkpoint"]
            demand(path.resolve().is_relative_to(args.fit.resolve()), "checkpoint path escapes source fit")
            demand(sha256(path) == run["selected_checkpoint_sha256"], "selected checkpoint changed")
            checkpoint = torch.load(path, weights_only=True)
            demand(checkpoint["schema"] == "slopninja-author-selection-checkpoint-v1" and checkpoint["config"] == config,
                   "checkpoint architecture differs")
            demand(checkpoint["seed"] == run["seed"] and checkpoint["epoch"] == run["selected_epoch"], "checkpoint seed or epoch differs")
            demand(checkpoint["validation"] == run["selected_validation"], "checkpoint validation record differs")
            checked[(config["name"], run["seed"])] = (run, checkpoint)
    args.out.mkdir(parents=True)
    input_manifest = {"schema": "slopninja-author-selection-test-inputs-v1", "protocol_sha256": protocol_hash,
                      "selection_sha256": sha256(selection_path), "eligibility_sha256": sha256(args.eligibility),
                      "gallery_artifact_sha256": {name: data["artifact_sha256"] for name, data in galleries.items()},
                      "implementation_sha256": selection["implementation_sha256"]}
    write_json(args.out / "input-validation.json", input_manifest)
    models, comparison_inputs = [], {}
    for config in CONFIGS:
        per_gallery, seed_metrics_all = {}, {seed: {} for seed in SEEDS}
        seed_records = []
        for seed in SEEDS:
            run_info, checkpoint = checked[(config["name"], seed)]
            model = make_model(config, selection["families"], calibrations["subsets"][config["subset"]]["knots"], seed)
            model.load_state_dict(checkpoint["model_state"])
            model.eval()
            records = {}
            for name in sorted(galleries):
                queries = galleries[name]
                summary, logits, per_query = score_model(model, queries)
                seed_metrics_all[seed][name] = (summary, per_query)
                path = args.out / f"{config['name']}-seed-{seed}-{name}-scores.json"
                write_json(path, {"query_ids": [row["post"]["id"] for row in queries["rows"]],
                                  "candidate_authors": queries["candidate_authors"], "logits": logits.tolist(),
                                  "per_query_metrics": per_query.tolist()})
                records[name] = {"metrics": summary, "scores_sha256": sha256(path)}
            seed_records.append({"seed": seed, "selected_epoch": run_info["selected_epoch"],
                                 "selected_checkpoint_sha256": run_info["selected_checkpoint_sha256"],
                                 "galleries": records, "pooled": pooled_summary({name: row["metrics"] for name, row in records.items()})})
        for name, queries in galleries.items():
            arrays = [seed_metrics_all[seed][name][1] for seed in SEEDS]
            per_gallery[name] = summarize(np.mean(arrays, axis=0), queries["query_authors"])
        average = pooled_summary(per_gallery)
        models.append({"config": config, "seed_mean": average, "galleries": per_gallery, "seeds": seed_records,
                       "seed_macro_top1_range": [min(row["pooled"]["macro_author"]["top1"] for row in seed_records),
                                                  max(row["pooled"]["macro_author"]["top1"] for row in seed_records)]})
        comparison_inputs[config["name"]] = {name: summary["per_author"] for name, summary in per_gallery.items()}
    fixed = {}
    for subset in SUBSETS:
        included = included_families(selection["families"], subset)
        indices = [selection["families"].index(name) for name in included]
        weights = prior_weights(included)
        summaries = {}
        for name, queries in galleries.items():
            logits = -(queries["x"][:, :, indices] * weights).sum(-1).numpy()
            metrics = query_metrics(logits, queries["own"], queries["impostors"])
            summaries[name] = summarize(metrics, queries["query_authors"])
        fixed[subset] = {"pooled": pooled_summary(summaries), "galleries": summaries}
        comparison_inputs[f"fixed_{subset}"] = {name: summary["per_author"] for name, summary in summaries.items()}
    pairs = [(selected["combined"], selected["words"]), (selected["combined"], selected["grammar"]),
             ("combined_linear", "words_linear"), ("combined_spline", "words_spline")]
    for subset in SUBSETS:
        pairs += [(f"{subset}_spline", f"{subset}_linear"), (selected[subset], f"fixed_{subset}")]
    comparisons = {}
    for left, right in pairs:
        key = f"{left}_minus_{right}"
        if key not in comparisons:
            comparisons[key] = stratified_difference(comparison_inputs[left], comparison_inputs[right])
    primary_key = f"{selected['combined']}_minus_{selected['words']}"
    primary = comparisons[primary_key]["metrics"]["top1"]
    report = {"schema": "slopninja-author-disjoint-selection-result-v1", "protocol_sha256": protocol_hash,
              "selection_sha256": sha256(selection_path), "eligibility_sha256": sha256(args.eligibility),
              "selected_architectures": selected, "models": models, "fixed_baselines": fixed, "comparisons": comparisons,
              "primary": {"comparison": primary_key, "metric": "macro_author.top1", **primary,
                          "positive_lower_95_bound": primary["percentile_95"][0] > 0},
              "gallery_order": sorted(galleries), "gallery_author_counts": {name: len(galleries[name]["candidate_authors"]) for name in sorted(galleries)},
              "gallery_query_counts": {name: len(galleries[name]["rows"]) for name in sorted(galleries)},
              "content_proxy_diagnostics": {name: {
                  "query_count": len(queries["rows"]),
                  "unavailable_query_count": sum(not row["content_proxy_available"] for row in queries["rows"]),
                  "tied_impostor_query_count": sum(len(row["equally_close_content_impostors"]) > 1 for row in queries["rows"]),
                  "interpretation": "pre-existing content-matched impostor diagnostic; missing vocabulary and exact ties limit interpretation; never used for model or checkpoint selection"
              } for name, queries in galleries.items()},
              "seed_interpretation": "mean query metrics across three independent fits, then equal authors; no ensemble or best seed",
              "pipeline_interpretation": "comparison of validation-selected fitted pipelines, including subset-specific training calibrations",
              "training_calls_during_test": 0, "external_model_calls": 0, "detector_calls": 0}
    write_json(args.out / "report.json", report)
    print(f"Author-disjoint test: {primary_key} {primary['mean_difference']:.6f}, 95% interval {primary['percentile_95']}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    fitting = subcommands.add_parser("train")
    fitting.add_argument("--original-fit", type=Path, required=True)
    fitting.add_argument("--validation-evaluation", type=Path, required=True)
    fitting.add_argument("--validation-eligibility", type=Path, required=True)
    testing = subcommands.add_parser("evaluate")
    testing.add_argument("--fit", type=Path, required=True)
    testing.add_argument("--evaluation", type=Path, action="append", required=True)
    testing.add_argument("--eligibility", type=Path, required=True)
    for command in [fitting, testing]:
        command.add_argument("--protocol", type=Path, required=True)
        command.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    configure()
    train(args) if args.command == "train" else evaluate(args)


if __name__ == "__main__":
    main()

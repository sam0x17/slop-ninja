"""Frozen document-projection comparison; no parser, corpus text or TEST selection."""
import argparse
import datetime
import hashlib
import json
import platform
import random
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path

import numpy as np
import torch
from torch import nn


RECIPE = {
    "seeds": [0, 17, 29],
    "architectures": [{"name": "linear", "hidden_dimension": None},
                      {"name": "mlp_gelu", "hidden_dimension": 256}],
    "input_dimension": 3557, "output_dimension": 128, "bias": True,
    "temperature": 0.1, "norm_epsilon": 1e-12,
    "norm_failure": "fail_on_norm_at_or_below_epsilon_or_nonfinite",
    "pooling": "l2_documents_then_mean_documents_within_date_then_mean_dates_then_l2_prototype",
    "training_positive": "exclude_whole_query_date_before_prototype_normalization",
    "loss_weighting": "equal_authors_then_dates_then_posts;equal_train_panels",
    "input_preprocessing": "frozen_original_columns_only;no_eta_no_dropout_no_batchnorm",
    "optimizer": {"name": "AdamW", "learning_rate": 0.001, "weight_decay": 0.01,
                  "betas": [0.9, 0.999], "epsilon": 1e-8},
    "epochs": 200, "development_every": 5, "device": "cpu", "dtype": "float32", "threads": 8,
    "checkpoint_selection": ["macro_author_top1", "macro_author_mrr", "earliest_epoch"],
    "architecture_selection": ["mean_seed_macro_author_top1", "mean_seed_macro_author_mrr", "fewer_parameters"],
    "identity_control": "same_pooling_on_original_3557_coordinates",
}
METRICS = ["top1", "top5", "mrr"]
GEOMETRY_KEYS = ["source_space_id", "feature_schema", "parser_identity", "coordinate_keys_sha256", "model_sha256"]


def demand(condition, message):
    if not condition:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def sha(path):
    return digest(Path(path).read_bytes())


def read_json(path):
    def pairs(items):
        result = {}
        for key, value in items:
            demand(key not in result, f"duplicate JSON key: {key}")
            result[key] = value
        return result
    return json.loads(Path(path).read_bytes(), object_pairs_hook=pairs)


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x") as output:
        output.write(json.dumps(value, indent=2, allow_nan=False) + "\n")
    return {"path": str(path.resolve()), "sha256": sha(path)}


def resolve(base, path):
    return (Path(base) / path).resolve()


def bound_file(root, entry):
    path = resolve(root, entry["path"])
    demand(path.is_relative_to(Path(root).resolve()), "export artifact escapes its directory")
    demand(sha(path) == entry["sha256"], "export artifact hash mismatch")
    return path


def valid_date(value):
    demand(isinstance(value, str) and len(value) == 10 and
           datetime.date.fromisoformat(value).isoformat() == value, "invalid calendar date")


def unit(vectors):
    norms = torch.linalg.vector_norm(vectors, dim=-1, keepdim=True)
    demand(bool(torch.isfinite(vectors).all()) and bool(torch.isfinite(norms).all()) and
           bool((norms > RECIPE["norm_epsilon"]).all()), "zero or nonfinite document/prototype norm")
    return vectors / norms


class Pool:
    """Static index maps; all document/date/author averages remain differentiable."""
    def __init__(self, rows, authors, training):
        self.rows, self.authors, self.training = rows, authors, training
        author_index = {author: index for index, author in enumerate(authors)}
        self.references = [i for i, row in enumerate(rows) if row["split"] in {"train", "reference"}]
        self.queries = [i for i, row in enumerate(rows) if row["split"] in {"train", "query"}]
        keys = sorted({(rows[i]["author"], rows[i]["date"]) for i in self.references})
        date_index = {key: index for index, key in enumerate(keys)}
        dates = [date_index[(rows[i]["author"], rows[i]["date"])] for i in self.references]
        self.reference_indices = torch.tensor(self.references, dtype=torch.int64)
        self.query_indices = torch.tensor(self.queries, dtype=torch.int64)
        self.reference_dates = torch.tensor(dates, dtype=torch.int64)
        self.date_counts = torch.tensor([Counter(dates)[i] for i in range(len(keys))], dtype=torch.float32)
        self.date_authors = torch.tensor([author_index[author] for author, _ in keys], dtype=torch.int64)
        counts = Counter(author for author, _ in keys)
        demand(all(counts[author] >= (2 if training else 1) for author in authors), "insufficient reference dates")
        self.author_date_counts = torch.tensor([counts[a] for a in authors], dtype=torch.float32)
        self.own = torch.tensor([author_index[rows[i]["author"]] for i in self.queries], dtype=torch.int64)
        self.query_dates = torch.tensor([date_index[(rows[i]["author"], rows[i]["date"])]
                                         for i in self.queries], dtype=torch.int64) if training else None
        query_dates = defaultdict(Counter)
        for i in self.queries:
            query_dates[rows[i]["author"]][rows[i]["date"]] += 1
        self.loss_weights = torch.tensor([
            1 / len(query_dates) / len(query_dates[rows[i]["author"]]) /
            query_dates[rows[i]["author"]][rows[i]["date"]] for i in self.queries
        ], dtype=torch.float32)

    def prototypes(self, normalized_documents):
        width = normalized_documents.shape[1]
        date_sums = normalized_documents.new_zeros((len(self.date_counts), width)).index_add(
            0, self.reference_dates, normalized_documents.index_select(0, self.reference_indices))
        date_means = date_sums / self.date_counts.to(normalized_documents.dtype).unsqueeze(1)
        author_sums = normalized_documents.new_zeros((len(self.authors), width)).index_add(
            0, self.date_authors, date_means)
        counts = self.author_date_counts.to(normalized_documents.dtype).unsqueeze(1)
        prototypes = unit(author_sums / counts)
        held = None
        if self.training:
            held = unit((author_sums.index_select(0, self.own) - date_means.index_select(0, self.query_dates)) /
                        (counts.index_select(0, self.own) - 1))
        return prototypes, held

    def logits(self, model, coordinates):
        documents = unit(model(coordinates))
        targets, held = self.prototypes(documents)
        queries = documents.index_select(0, self.query_indices)
        scores = queries @ targets.T / RECIPE["temperature"]
        if held is not None:
            replacement = (queries * held).sum(dim=1, keepdim=True) / RECIPE["temperature"]
            scores = scores.scatter(1, self.own.unsqueeze(1), replacement)
        demand(bool(torch.isfinite(scores).all()), "nonfinite cosine scores")
        return scores


def load_export(base, binding, allowed_roles):
    root = resolve(base, binding["path"])
    demand(sha(root / "manifest.json") == binding["manifest_sha256"], "manifest hash mismatch")
    manifest = read_json(root / "manifest.json")
    demand(manifest["schema"] == "slopninja-document-coordinates-v1" and manifest["role"] in allowed_roles,
           "unexpected document export schema/role")
    demand(manifest["dimension"] == RECIPE["input_dimension"], "coordinate dimension differs")
    authors = manifest["authors"]
    demand(authors == sorted(set(authors)) and len(authors) >= 2 and all(isinstance(a, str) and a for a in authors),
           "invalid author catalog")
    geometry = manifest["geometry"]
    demand(all(isinstance(geometry[k], str) and geometry[k] for k in GEOMETRY_KEYS), "incomplete geometry identity")
    rows = read_json(bound_file(root, manifest["documents"]))
    demand(isinstance(rows, list) and rows and
           all(isinstance(r["id"], str) and r["id"] for r in rows) and
           len(rows) == len({r["id"] for r in rows}), "empty or duplicated document catalog")
    groups, dates, text_hashes = {}, {}, set()
    training = manifest["role"] == "train"
    for row in rows:
        demand(all(isinstance(row[k], str) and row[k] for k in
                   ["id", "author", "date", "source_group", "text_sha256", "split", "source_split"]), "missing document identity")
        demand(row["author"] in authors and row["split"] in ({"train"} if training else {"reference", "query"}),
               "document author/split differs")
        valid_date(row["date"])
        text_hash = row["text_sha256"]
        demand(len(text_hash) == 64 and all(c in "0123456789abcdef" for c in text_hash) and text_hash not in text_hashes,
               "invalid or repeated source text hash")
        text_hashes.add(text_hash)
        group = (row["author"], row["date"], row["split"])
        demand(groups.setdefault(row["source_group"], group) == group, "source group crosses author/date/split")
        demand(dates.setdefault((row["author"], row["date"]), row["split"]) == row["split"], "whole date crosses split")
    if not training:
        for author in authors:
            references = [r["date"] for r in rows if r["author"] == author and r["split"] == "reference"]
            queries = [r["date"] for r in rows if r["author"] == author and r["split"] == "query"]
            demand(references and (not queries or max(references) < min(queries)), "reference/query chronology differs")
    tensor = manifest["coordinates"]
    demand(tensor["shape"] == [len(rows), RECIPE["input_dimension"]] and tensor["dtype"] == "float64-le",
           "tensor declaration differs")
    tensor_path = bound_file(root, tensor)
    demand(tensor_path.stat().st_size == len(rows) * RECIPE["input_dimension"] * 8, "tensor byte count differs")
    values = np.fromfile(tensor_path, dtype="<f8").reshape(tensor["shape"])
    demand(np.isfinite(values).all(), "nonfinite exported coordinates")
    coordinates = torch.from_numpy(values.astype(np.float32))
    demand(bool(torch.isfinite(coordinates).all()), "float32 coordinate conversion overflow")
    pool = Pool(rows, authors, training)
    demand(pool.queries, "export contains no queries")
    return {"name": binding["name"], "root": root, "manifest": manifest, "rows": rows,
            "authors": authors, "pool": pool, "coordinates": coordinates,
            "binding": {"name": binding["name"], "path": str(root), "manifest_sha256": binding["manifest_sha256"]}}


def check_panels(panels, forbidden=None):
    forbidden = forbidden or {}
    seen_authors = set(forbidden.get("authors", []))
    seen_ids = set(forbidden.get("document_ids", []))
    seen_hashes = set(forbidden.get("text_sha256", []))
    seen_groups = set(forbidden.get("source_groups", []))
    demand(len({p["name"] for p in panels}) == len(panels), "duplicated panel name")
    geometry = {key: panels[0]["manifest"]["geometry"][key] for key in GEOMETRY_KEYS}
    for panel in panels:
        demand(not seen_authors.intersection(panel["authors"]), "panel authors overlap fitting/development/another gallery")
        seen_authors.update(panel["authors"])
        demand({key: panel["manifest"]["geometry"][key] for key in GEOMETRY_KEYS} == geometry, "panel geometry differs")
        for key, seen in [("id", seen_ids), ("text_sha256", seen_hashes)]:
            current = {r[key] for r in panel["rows"]}
            demand(not seen.intersection(current), f"panels repeat {key}")
            seen.update(current)
        current = {r["source_group"] for r in panel["rows"]}
        demand(not seen_groups.intersection(current), "panels repeat source groups")
        seen_groups.update(current)
    return geometry


def make_model(name, seed):
    random.seed(seed)
    np.random.seed(seed)
    torch.manual_seed(seed)
    if name == "identity":
        return nn.Identity()
    config = next(c for c in RECIPE["architectures"] if c["name"] == name)
    if config["hidden_dimension"] is None:
        return nn.Linear(RECIPE["input_dimension"], RECIPE["output_dimension"], bias=True)
    return nn.Sequential(nn.Linear(RECIPE["input_dimension"], config["hidden_dimension"], bias=True),
                         nn.GELU(approximate="none"),
                         nn.Linear(config["hidden_dimension"], RECIPE["output_dimension"], bias=True))


def metric_rows(scores, pool):
    values = scores.detach().numpy()
    labels = pool.own.numpy()
    true = values[np.arange(len(values)), labels]
    before = (values > true[:, None]).sum(axis=1)
    ties = (values == true[:, None]).sum(axis=1)
    low, high = before + 1, before + ties
    harmonic = np.concatenate(([0.0], np.cumsum(1 / np.arange(1, len(pool.authors) + 1))))
    metrics = np.stack((np.maximum(1 - before, 0).clip(max=ties) / ties,
                        np.maximum(5 - before, 0).clip(max=ties) / ties,
                        (harmonic[high] - harmonic[low - 1]) / ties), axis=1)
    return [{**{k: pool.rows[i][k] for k in ["id", "author", "date", "source_group", "text_sha256"]},
             "rank_min": int(low[j]), "rank_max": int(high[j]),
             "metrics": dict(zip(METRICS, metrics[j].tolist()))} for j, i in enumerate(pool.queries)]


def summarize(rows, authors):
    by_author = defaultdict(list)
    for row in rows:
        by_author[row["author"]].append([row["metrics"][k] for k in METRICS])
    author_rows, supported = [], []
    for author in sorted(authors):
        values = by_author[author]
        average = np.mean(values, axis=0).tolist() if values else None
        if average is not None:
            supported.append(average)
        author_rows.append({"author": author, "query_count": len(values),
                            "metrics": dict(zip(METRICS, average)) if average is not None else None})
    return {"requested_authors": len(authors), "scored_authors": len(supported), "query_count": len(rows),
            "macro_author": dict(zip(METRICS, np.mean(supported, axis=0).tolist())) if supported else None,
            "micro_query": {k: float(np.mean([r["metrics"][k] for r in rows])) for k in METRICS} if rows else None,
            "authors": author_rows}


def evaluate_model(model, panels, out=None):
    model.eval()
    all_rows, gallery_results = [], []
    with torch.no_grad():
        for panel in panels:
            scores = panel["pool"].logits(model, panel["coordinates"])
            rows = metric_rows(scores, panel["pool"])
            all_rows.extend(rows)
            result = {"name": panel["name"], "gallery_size": len(panel["authors"]),
                      "summary": summarize(rows, panel["authors"])}
            if out is not None:
                directory = Path(out) / panel["name"]
                directory.mkdir(parents=True, exist_ok=False)
                path = directory / "scores.npy"
                np.save(path, scores.numpy(), allow_pickle=False)
                result["scores"] = {"path": str(path.resolve()), "sha256": sha(path),
                                    "shape": list(scores.shape), "dtype": "float32", "format": "npy"}
                result["queries"] = write_json(directory / "queries.json", rows)
            gallery_results.append(result)
    return {"summary": summarize(all_rows, [a for p in panels for a in p["authors"]]), "galleries": gallery_results}


def training_loss(model, panels, backward=False):
    value = 0.0
    for panel in panels:
        scores = panel["pool"].logits(model, panel["coordinates"])
        losses = nn.functional.cross_entropy(scores, panel["pool"].own, reduction="none")
        loss = (losses * panel["pool"].loss_weights).sum() / len(panels)
        demand(bool(torch.isfinite(loss)), "nonfinite training loss")
        if backward:
            loss.backward()
        value += float(loss.detach())
    return value


def runtime():
    torch.set_num_threads(RECIPE["threads"])
    torch.set_default_dtype(torch.float32)
    torch.use_deterministic_algorithms(True)
    return {"python": sys.version, "torch": torch.__version__, "numpy": np.__version__,
            "platform": platform.platform(), "device": "cpu", "dtype": "float32", "threads": torch.get_num_threads(),
            "deterministic_algorithms": torch.are_deterministic_algorithms_enabled(),
            "initialization": "installed torch.nn.Linear default initialization; reset seed separately for each arm/seed",
            "gelu_approximate": "none"}


def train(args):
    protocol_path, out = Path(args.protocol).resolve(), Path(args.out)
    protocol = read_json(protocol_path)
    demand(protocol["schema"] == "slopninja-author-projection-protocol-v1" and protocol.get("status") == "frozen",
           "training requires the frozen projection protocol")
    demand(protocol["recipe"] == RECIPE, "recipe differs from fixed implementation")
    source_bindings = []
    for source in protocol["sources"]:
        path = resolve(protocol_path.parent, source["path"])
        demand(sha(path) == source["sha256"], "bound study source changed")
        source_bindings.append({"path": str(path), "sha256": source["sha256"]})
    runner_hash = sha(__file__)
    demand(any(s["path"] == str(Path(__file__).resolve()) and s["sha256"] == runner_hash for s in source_bindings),
           "executing runner absent from protocol source bindings")
    training = [load_export(protocol_path.parent, b, {"train"}) for b in protocol["train"]]
    development = [load_export(protocol_path.parent, b, {"dev"}) for b in protocol["development"]]
    demand(len(training) == 3 and all(len(p["authors"]) == 100 for p in training) and
           sum(len(p["rows"]) for p in training) == 3889, "fixed three-panel TRAIN scope differs")
    demand(development, "development galleries absent")
    geometry = check_panels(training + development)
    protocol_hash = sha(protocol_path)
    (out / "protocol.json").write_bytes(protocol_path.read_bytes())
    write_json(out / "implementation.json", {"runtime": runtime(), "runner_sha256": runner_hash,
               "source_bindings": source_bindings, "protocol_path": str(protocol_path), "protocol_sha256": protocol_hash,
               "training": [p["binding"] for p in training], "development": [p["binding"] for p in development]})
    identities = {"authors": sorted(a for p in training + development for a in p["authors"]),
                  "document_ids": sorted(r["id"] for p in training + development for r in p["rows"]),
                  "text_sha256": sorted(r["text_sha256"] for p in training + development for r in p["rows"]),
                  "source_groups": sorted({r["source_group"] for p in training + development for r in p["rows"]})}
    identity_binding = write_json(out / "training-development-identities.json", identities)
    identity = evaluate_model(make_model("identity", 0), development, out / "identity-development")
    write_json(out / "identity-development.json", identity)
    runs = []
    for config in RECIPE["architectures"]:
        name = config["name"]
        for seed in RECIPE["seeds"]:
            started = time.perf_counter()
            model = make_model(name, seed)
            parameters = sum(p.numel() for p in model.parameters())
            demand(parameters == {"linear": 455424, "mlp_gelu": 943744}[name], "parameter count differs")
            opt = RECIPE["optimizer"]
            optimizer = torch.optim.AdamW(model.parameters(), lr=opt["learning_rate"], weight_decay=opt["weight_decay"],
                                         betas=tuple(opt["betas"]), eps=opt["epsilon"])
            directory = out / name / f"seed-{seed}"
            directory.mkdir(parents=True)
            history, best, best_key = [], None, None
            for epoch in range(RECIPE["epochs"] + 1):
                if epoch:
                    model.train()
                    optimizer.zero_grad(set_to_none=True)
                    training_loss(model, training, backward=True)
                    demand(all(p.grad is not None and bool(torch.isfinite(p.grad).all()) for p in model.parameters()),
                           "missing or nonfinite training gradient")
                    optimizer.step()
                if epoch % RECIPE["development_every"]:
                    continue
                with torch.no_grad():
                    loss = training_loss(model, training)
                evaluation = evaluate_model(model, development)
                checkpoint = directory / f"epoch-{epoch:03}.pt"
                torch.save(model.state_dict(), checkpoint)
                entry = {"epoch": epoch, "training_loss": loss, "development": evaluation,
                         "checkpoint": {"path": str(checkpoint.relative_to(out)), "sha256": sha(checkpoint)}}
                history.append(entry)
                metrics = evaluation["summary"]["macro_author"]
                key = (metrics["top1"], metrics["mrr"], -epoch)
                if best_key is None or key > best_key:
                    best, best_key = entry, key
                print(f"{name} seed={seed} epoch={epoch} loss={loss:.8f} dev_top1={metrics['top1']:.8f}", flush=True)
            history_binding = write_json(directory / "history.json", {"architecture": name, "seed": seed,
                                         "parameters": parameters, "evaluated_epochs": history})
            runs.append({"architecture": name, "seed": seed, "parameters": parameters,
                         "selected_epoch": best["epoch"], "checkpoint": best["checkpoint"],
                         "training_loss": best["training_loss"], "development": best["development"],
                         "history": history_binding, "elapsed_seconds": time.perf_counter() - started})
    choices = []
    for config in RECIPE["architectures"]:
        selected = [r for r in runs if r["architecture"] == config["name"]]
        demand([r["seed"] for r in selected] == RECIPE["seeds"], "incomplete architecture seeds")
        choices.append({"architecture": config["name"], "parameters": selected[0]["parameters"],
                        "mean_seed_macro_author": {k: float(np.mean([r["development"]["summary"]["macro_author"][k]
                                                                   for r in selected])) for k in METRICS}})
    winner = max(choices, key=lambda c: (c["mean_seed_macro_author"]["top1"], c["mean_seed_macro_author"]["mrr"],
                                       -c["parameters"]))["architecture"]
    demand(sha(__file__) == runner_hash and sha(protocol_path) == protocol_hash and
           all(sha(s["path"]) == s["sha256"] for s in source_bindings), "source/protocol changed during fitting")
    write_json(out / "selection.json", {"schema": "slopninja-author-projection-selection-v1", "protocol_sha256": protocol_hash,
               "runner_sha256": runner_hash, "geometry": geometry, "recipe": RECIPE,
               "training_development_identities": identity_binding,
               "fitting_authors": [a for p in training for a in p["authors"]],
               "development_authors": [a for p in development for a in p["authors"]],
               "runs": runs, "architecture_choices": choices, "selected_architecture": winner,
               "seed_policy": "all three seeds retained; no seed selection; metrics averaged, no score ensemble",
               "identity_development": identity, "test_accessed": False})


def evaluate(args):
    fit, out, input_path = Path(args.fit).resolve(), Path(args.out), Path(args.inputs).resolve()
    inputs = read_json(input_path)
    demand(inputs["schema"] == "slopninja-author-projection-evaluation-inputs-v1" and
           inputs["selection_sha256"] == sha(fit / "selection.json"), "evaluation does not bind frozen selection")
    selection = read_json(fit / "selection.json")
    demand(selection["schema"] == "slopninja-author-projection-selection-v1" and selection["recipe"] == RECIPE and
           selection["runner_sha256"] == sha(__file__) and selection["protocol_sha256"] == sha(fit / "protocol.json"),
           "frozen fit/recipe/runner changed")
    panels = [load_export(input_path.parent, b, {"test", "transfer"}) for b in inputs["exports"]]
    demand(panels, "evaluation exports absent")
    identities = read_json(bound_file(fit, selection["training_development_identities"]))
    geometry = check_panels(panels, identities)
    demand(geometry == selection["geometry"], "evaluation geometry differs from fitting")
    write_json(out / "implementation.json", {"runtime": runtime(), "selection_sha256": inputs["selection_sha256"],
               "inputs_sha256": sha(input_path), "exports": [p["binding"] for p in panels], "runner_sha256": sha(__file__)})
    identity = evaluate_model(make_model("identity", 0), panels, out / "identity")
    results = []
    expected = {(c["name"], seed) for c in RECIPE["architectures"] for seed in RECIPE["seeds"]}
    demand(len(selection["runs"]) == 6 and {(r["architecture"], r["seed"]) for r in selection["runs"]} == expected,
           "selected run catalog differs")
    for run in selection["runs"]:
        path = bound_file(fit, run["checkpoint"])
        model = make_model(run["architecture"], run["seed"])
        model.load_state_dict(torch.load(path, map_location="cpu", weights_only=True), strict=True)
        result = evaluate_model(model, panels, out / run["architecture"] / f"seed-{run['seed']}")
        results.append({"architecture": run["architecture"], "seed": run["seed"], "selected_epoch": run["selected_epoch"],
                        "checkpoint_sha256": run["checkpoint"]["sha256"], **result})
    summaries = []
    for config in RECIPE["architectures"]:
        chosen = [r for r in results if r["architecture"] == config["name"]]
        summaries.append({"architecture": config["name"], "selected_on_development": config["name"] == selection["selected_architecture"],
                          "mean_seed_macro_author": {k: float(np.mean([r["summary"]["macro_author"][k]
                                                                      for r in chosen])) for k in METRICS}})
    write_json(out / "report.json", {"schema": "slopninja-author-projection-evaluation-v1",
               "selection_sha256": inputs["selection_sha256"], "inputs_sha256": sha(input_path),
               "selected_architecture": selection["selected_architecture"], "architectures": summaries,
               "identity": identity, "runs": results, "optimizer_steps": 0,
               "limits": ["Author retrieval is not AI-origin detection, writing quality or fidelity.",
                          "The learned projections and identity control use only the fixed 3557 columns; the historical full model retains additional feature tails.",
                          "All seed results are retained; testing does not reselect the architecture, checkpoint or seed."]})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    training = commands.add_parser("train")
    training.add_argument("--protocol", required=True)
    evaluation = commands.add_parser("eval")
    evaluation.add_argument("--fit", required=True)
    evaluation.add_argument("--inputs", required=True)
    for command in [training, evaluation]:
        command.add_argument("--out", required=True)
    args = parser.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=False)
    try:
        (train if args.command == "train" else evaluate)(args)
    except Exception as error:
        write_json(out / "failure.json", {"status": "failed", "error_type": type(error).__name__, "error": str(error),
                   "runner_sha256": sha(__file__), "no_automatic_retry": True})
        raise


if __name__ == "__main__":
    main()

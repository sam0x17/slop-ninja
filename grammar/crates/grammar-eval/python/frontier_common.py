"""Validated distance inputs and tie-aware author retrieval for ML experiments."""
import hashlib
import json
import math
from collections import Counter, defaultdict
from pathlib import Path

import numpy as np
import torch


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def read_json(path):
    return json.loads(Path(path).read_text())


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2, allow_nan=False) + "\n")


def demand(condition, message):
    if not condition:
        raise ValueError(message)


def balanced_loss(rows):
    counts = defaultdict(Counter)
    for row in rows:
        counts[row["author"]][row["source_group"]] += 1
    return [
        1 / len(counts) / len(counts[row["author"]]) / counts[row["author"]][row["source_group"]]
        for row in rows
    ]


def load_training(original_fit, evaluation):
    original_fit, evaluation = Path(original_fit), Path(evaluation)
    selection = read_json(original_fit / "selection.json")
    demand(selection["schema"] == "unslop-frozen-author-weights-v1", "unexpected original selection")
    tensor_path = original_fit / "training-tensor.json"
    demand(sha256(tensor_path) == selection["training_tensor_sha256"], "original training tensor changed")
    tensor = read_json(tensor_path)
    demand(tensor["schema"] == "unslop-neural-distance-tensor-v1", "unexpected training tensor")
    for key in ["source_space_id", "feature_schema", "parser_identity"]:
        demand(tensor[key] == selection[key], f"original tensor identity mismatch: {key}")
    authors, families = tensor["authors"], tensor["families"]
    demand(authors == sorted(set(authors)) == selection["selection_author_ids"], "author order differs")
    demand(families == sorted(set(families)) and len(families) == 14, "family catalog differs")
    rows = tensor["rows"]
    demand(len(rows) == selection["training_post_count"], "training count differs")
    demand(len({row["id"] for row in rows}) == len(rows), "duplicate training row")
    demands = balanced_loss(rows)
    for row, weight in zip(rows, demands):
        demand(0 <= row["own"] < len(authors) and authors[row["own"]] == row["author"], "training label differs")
        demand(row["source_group"].startswith(f"blog:{row['author']}:date:"), "source group differs")
        demand(math.isclose(row["loss_weight"], weight, abs_tol=1e-15, rel_tol=1e-13), "training loss balance differs")
    x = torch.tensor([row["distances"] for row in rows], dtype=torch.float64)
    demand(x.shape == (len(rows), len(authors), len(families)), "training distance shape differs")
    demand(bool(torch.isfinite(x).all()) and bool((x >= 0).all()), "invalid training distances")
    demand(math.isclose(sum(demands), 1, abs_tol=1e-12), "training mass differs")
    implementation = read_json(evaluation / "implementation.json")
    dev_path = evaluation / "dev-queries.jsonl"
    demand(sha256(dev_path) == implementation["artifacts_sha256"]["dev-queries.jsonl"], "original dev changed")
    demand(sha256(dev_path) in selection["input_sha256"].values(), "dev not bound to original selection")
    development = load_queries(dev_path, authors, families, required_split="dev")
    demand(len(development["rows"]) == selection["development_query_count"], "development count differs")
    demand(set(development["query_authors"]) == set(authors), "development author coverage differs")
    return {
        "x": x,
        "own": torch.tensor([row["own"] for row in rows]),
        "loss_weight": torch.tensor(demands, dtype=torch.float64),
        "rows": rows,
        "authors": authors,
        "families": families,
        "development": development,
        "identity": {key: selection[key] for key in ["source_space_id", "feature_schema", "parser_identity"]},
        "bindings": {"original_selection_sha256": sha256(original_fit / "selection.json"),
                     "training_tensor_sha256": sha256(tensor_path), "development_queries_sha256": sha256(dev_path)},
        "original_fit": original_fit,
    }


def load_queries(path, authors, families, required_split):
    rows = [json.loads(line) for line in Path(path).read_text().splitlines()]
    demand(rows and len({row["post"]["id"] for row in rows}) == len(rows), "empty or duplicate query rows")
    distances, own, impostors, query_authors = [], [], [], []
    for row in rows:
        demand(row["post"]["split"] == required_split, "unexpected query split")
        author = row["post"]["author_id"]
        demand(author in authors, "unknown true author")
        values = row["unweighted_squared_family_distances"]
        demand(set(values) == set(authors), "candidate catalog differs")
        demand(all(set(values[a]) == set(families) for a in authors), "query family catalog differs")
        distances.append([[values[a][f] for f in families] for a in authors])
        own.append(authors.index(author))
        query_authors.append(author)
        impostor = row["content_matched_impostor"]
        demand(impostor in authors and impostor != author, "invalid topic impostor")
        impostors.append(authors.index(impostor))
    x = torch.tensor(distances, dtype=torch.float64)
    demand(bool(torch.isfinite(x).all()) and bool((x >= 0).all()), "invalid query distances")
    return {"x": x, "own": np.array(own), "impostors": np.array(impostors),
            "query_authors": query_authors, "rows": rows}


def query_metrics(logits, own, impostors):
    """Higher logits are better; exact ties receive expected random-order rank."""
    values = np.asarray(logits)
    demand(np.isfinite(values).all(), "nonfinite scoring output")
    true = values[np.arange(len(values)), own]
    before = (values > true[:, None]).sum(axis=1)
    ties = (values == true[:, None]).sum(axis=1)
    rank_min, rank_max = before + 1, before + ties
    harmonic = np.concatenate(([0.0], np.cumsum(1 / np.arange(1, values.shape[1] + 1))))
    other = values[np.arange(len(values)), impostors]
    return np.stack((
        np.maximum(np.minimum(1, rank_max) + 1 - rank_min, 0) / ties,
        np.maximum(np.minimum(5, rank_max) + 1 - rank_min, 0) / ties,
        (harmonic[rank_max] - harmonic[rank_min - 1]) / ties,
        (true > other).astype(float) + 0.5 * (true == other),
    ), axis=1)


METRIC_NAMES = ["top1", "top5", "mrr", "own_vs_content_impostor"]


def summarize(metrics, authors):
    values = np.asarray(metrics)
    author_rows = defaultdict(list)
    for author, row in zip(authors, values):
        author_rows[author].append(row)
    per_author = {author: np.mean(rows, axis=0) for author, rows in sorted(author_rows.items())}
    return {"query_count": len(values), "author_count": len(per_author),
            "micro": dict(zip(METRIC_NAMES, values.mean(axis=0).tolist())),
            "macro_author": dict(zip(METRIC_NAMES, np.mean(list(per_author.values()), axis=0).tolist())),
            "per_author": {author: dict(zip(METRIC_NAMES, row.tolist())) for author, row in per_author.items()}}


def score_model(model, queries):
    with torch.no_grad():
        logits = model(queries["x"]).detach().cpu().numpy()
    metrics = query_metrics(logits, queries["own"], queries["impostors"])
    return summarize(metrics, queries["query_authors"]), logits, metrics

"""Exact-tie retrieval metrics and paired author bootstrap for calibration fits."""
import statistics

import numpy as np
import torch

from frontier_common import demand


METRICS = ["top1", "top5", "mrr"]


def metric_rows(logits, own):
    logits, own = np.asarray(logits), np.asarray(own)
    demand(logits.ndim == 2 and len(logits) > 0 and own.shape == (len(logits),)
           and np.issubdtype(own.dtype, np.integer) and np.isfinite(logits).all()
           and (own >= 0).all() and (own < logits.shape[1]).all(), "invalid retrieval scores or labels")
    true = logits[np.arange(len(logits)), own]
    better, ties = (logits > true[:, None]).sum(1), (logits == true[:, None]).sum(1)
    first, last = better + 1, better + ties
    harmonic = np.concatenate(([0.0], np.cumsum(1 / np.arange(1, logits.shape[1] + 1))))
    return np.stack((np.maximum(np.minimum(1, last) + 1 - first, 0) / ties,
                     np.maximum(np.minimum(5, last) + 1 - first, 0) / ties,
                     (harmonic[last] - harmonic[first - 1]) / ties), 1)


def summarize(rows, authors):
    demand(len(rows) == len(authors) and len(rows), "empty or mismatched metric rows")
    groups = {}
    for author, row in zip(authors, rows):
        groups.setdefault(author, []).append(row)
    per_author = {author: dict(zip(METRICS, np.mean(values, 0).tolist())) for author, values in sorted(groups.items())}
    return {"query_count": len(rows), "author_count": len(per_author), "per_author": per_author,
            "micro": dict(zip(METRICS, np.mean(rows, 0).tolist())),
            "macro_author": {metric: statistics.mean(row[metric] for row in per_author.values()) for metric in METRICS}}


def pool(summaries):
    authors = {}
    for summary in summaries.values():
        demand(authors.keys().isdisjoint(summary["per_author"]), "gallery author overlap")
        authors.update(summary["per_author"])
    demand(authors, "no author metrics to pool")
    queries = sum(row["query_count"] for row in summaries.values())
    return {"query_count": queries, "author_count": len(authors), "per_author": authors,
            "macro_author": {metric: statistics.mean(row[metric] for row in authors.values()) for metric in METRICS},
            "micro": {metric: sum(row["micro"][metric] * row["query_count"] for row in summaries.values()) / queries for metric in METRICS}}


def score(model, data):
    with torch.no_grad():
        logits = model(data["x"]).numpy()
    rows = metric_rows(logits, data["own"].numpy())
    return summarize(rows, data["query_authors"]), logits, rows


def validation_score(model, galleries):
    summaries = {name: score(model, data)[0] for name, data in galleries.items()}
    return pool(summaries), summaries


def paired_interval(left, right):
    demand(left.keys() == right.keys() and left, "paired gallery catalog differs")
    galleries = []
    for name in sorted(left):
        demand(left[name].keys() == right[name].keys() and left[name], "paired author catalog differs")
        rows = np.array([[left[name][author][metric] - right[name][author][metric] for metric in METRICS]
                         for author in sorted(left[name])])
        galleries.append((name, rows))
    total = sum(len(rows) for _, rows in galleries)
    point = np.concatenate([rows for _, rows in galleries]).mean(0)
    state, mask, samples = 0x6A09E667F3BCC909, (1 << 64) - 1, []
    for _ in range(2000):
        value = np.zeros(3)
        for _, rows in galleries:
            for _ in rows:
                state ^= (state << 13) & mask
                state ^= state >> 7
                state ^= (state << 17) & mask
                value += rows[state % len(rows)]
        samples.append(value / total)
    ordered = np.sort(samples, axis=0)
    return {"author_count": total, "gallery_order": [name for name, _ in galleries],
            "gallery_author_counts": {name: len(rows) for name, rows in galleries},
            "gallery_mean_differences": {name: dict(zip(METRICS, rows.mean(0).tolist())) for name, rows in galleries},
            "replicates": 2000, "seed": "0x6a09e667f3bcc909",
            "metrics": {metric: {"mean_difference": point[i].item(), "percentile_95": [ordered[49, i].item(), ordered[1949, i].item()]}
                        for i, metric in enumerate(METRICS)}}

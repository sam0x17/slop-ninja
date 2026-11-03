"""Interpretable feature contrasts; these statistics are not detector probabilities."""

import math


def contrast(left, right, min_count=5, min_documents=3):
    """Half-count smoothed binary log odds with an approximate variance.

    Each feature is treated as occurring/not occurring per family opportunity.
    The four-cell variance ignores within-document dependence, so z is a
    ranking heuristic, not a calibrated significance test. Document counts and
    source-group counts must accompany it. No learned detector geometry implied.
    """
    if left["extractor"] != right["extractor"] or left["family"] != right["family"]:
        raise ValueError("Contrasts require identical extractor and feature family")
    n, m = left["total"], right["total"]
    if n <= 0 or m <= 0:
        raise ValueError("Contrasts require nonempty opportunity counts on both sides")
    if min_count < 1 or min_documents < 1:
        raise ValueError("Minimum evidence counts must be positive")
    output = []
    for word in left["counts"].keys() | right["counts"].keys():
        a, b = left["counts"].get(word, 0), right["counts"].get(word, 0)
        if not (0 <= a <= n and 0 <= b <= m):
            raise ValueError("Feature count exceeds opportunity count")
        cells = (a + .5, n - a + .5, b + .5, m - b + .5)
        log_odds = math.log(cells[0] / cells[1]) - math.log(cells[2] / cells[3])
        z = log_odds / math.sqrt(sum(1 / cell for cell in cells))
        lp, rp = (a + .5) / (n + 1), (b + .5) / (m + 1)
        ld, rd = left["document_counts"].get(word, 0), right["document_counts"].get(word, 0)
        lg, rg = left["group_counts"].get(word, 0), right["group_counts"].get(word, 0)
        output.append({"feature": word, "left_count": a, "right_count": b,
                       "left_per_1000": 1000 * a / n, "right_per_1000": 1000 * b / m,
                       "smoothed_lift": lp / rp, "log2_odds": log_odds / math.log(2),
                       "z_heuristic": z, "left_documents": ld, "right_documents": rd,
                       "left_source_groups": lg, "right_source_groups": rg,
                       "enough_evidence": a + b >= min_count and max(ld, rd) >= min_documents
                       and max(lg, rg) >= min_documents})
    return sorted(output, key=lambda r: (-abs(r["z_heuristic"]), r["feature"]))


def vector(extracted, family="word"):
    """Sparse frequency coordinates, normalized by the stated family denominator."""
    total = extracted["totals"][family]
    return {feature: count / total for feature, count in sorted(extracted["families"][family].items())} if total else {}


def document_profile(extracted, family="word"):
    counts = extracted["families"][family]
    return {"extractor": extracted["extractor"], "family": family,
            "counts": counts, "total": extracted["totals"][family],
            "documents": 1, "groups": 1, "document_counts": {k: 1 for k in counts},
            "group_counts": {k: 1 for k in counts}}

"""Nested coordinate catalogs and adjustable subsets of a fixed family scorer."""
import hashlib
import json

import torch

from coordinate_model import CoordinateInputs, CoordinateMetric
from frontier_common import demand


SEEDS = [0, 17, 29]
LAMBDAS = [0.1, 1.0, 10.0, 100.0]
SIZES = [("m0p5", 0.5), ("m1", 1), ("m2", 2), ("m4", 4), ("m8", 8), ("supported", None)]
BLOCKS = ["word", "grammar", "all"]


def index_digest(indices):
    return hashlib.sha256(json.dumps(indices, separators=(",", ":")).encode()).hexdigest()


def expected_subsets(catalog):
    coordinates = catalog["coordinates"]
    answer, known = {}, {}
    for block in BLOCKS:
        for suffix, multiplier in SIZES:
            name = f"{block}_{suffix}"
            indices = []
            family_caps = {}
            for index, row in enumerate(coordinates):
                lexical = row["family"] in {"word", "word_bigram"}
                if (block == "word" and not lexical) or (block == "grammar" and lexical):
                    continue
                cap = None if multiplier is None else int(multiplier * (512 if lexical else 96))
                if row["kind"] == "distribution":
                    family_caps[row["family"]] = cap if cap is not None else family_caps.get(row["family"], 0) + 1
                if row["kind"] != "distribution" or cap is None or row["support_rank"] <= cap:
                    indices.append(index)
            key = (block, tuple(indices))
            canonical = known.setdefault(key, name)
            answer[name] = {"block": block, "cap_multiplier": multiplier, "max_column_indices": indices,
                            "coordinate_count": len(indices), "canonical_name": canonical,
                            "indices_sha256": index_digest(indices), "family_caps": family_caps}
    return answer


def configurations(catalog):
    expected = expected_subsets(catalog)
    demand(catalog["subsets"] == expected, "nested coordinate subsets or aliases differ from training support")
    configs = []
    for name, row in expected.items():
        if row["canonical_name"] == name:
            demand(row["coordinate_count"] > 0, "empty adjustable coordinate subset")
            configs.append({"name": name, **row, "aliases": [other for other, value in expected.items()
                                                            if value["canonical_name"] == name]})
    demand(len(configs) == 16, "frontier must have sixteen deduplicated configurations")
    return configs


def subset_data(data, indices):
    columns = torch.tensor(indices, dtype=torch.int64)
    families = torch.tensor([data["catalog"]["coordinates"][index]["family_index"] for index in indices])
    held = None if data["held"] is None else data["held"].index_select(1, columns)
    inputs = CoordinateInputs(data["query"].index_select(1, columns), data["target"].index_select(1, columns),
                              data["raw"], data["labels_tensor"], families, held, data["loss_weights"])
    return {"inputs": inputs, "rows": data["rows"], "authors": data["authors"],
            "query_authors": data["query_authors"], "labels": data["labels"]}


class FrontierMetric(CoordinateMetric):
    """Only selected eta entries train; inactive families contribute zero penalty."""
    def penalty(self):
        # Keep the effective shrinkage for a coordinate fixed across learning masks.
        return torch.stack([self.eta.index_select(0, columns).square().mean()
                            for columns in self.penalty_groups]).sum() / 14


def reset_eta(model, keep, lexical_family_indices):
    """Post-fit conditional ablation, with all baseline distances still present."""
    demand(keep in {"word", "grammar", "neither"}, "unknown coordinate reset")
    with torch.no_grad():
        for index, family in enumerate(model.coordinate_families.tolist()):
            if keep == "neither" or (keep == "word" and family not in lexical_family_indices) or (
                    keep == "grammar" and family in lexical_family_indices):
                model.eta[index] = 0

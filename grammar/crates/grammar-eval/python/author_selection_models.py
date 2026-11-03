"""Positive family metrics with genuinely smaller word/grammar parameter sets."""
import math

import torch
from torch import nn

from frontier_common import demand


SEEDS = [0, 17, 29]
SUBSETS = ["words", "grammar", "combined"]
CONFIGS = [
    {"name": f"{subset}_{kind}", "subset": subset, "kind": kind,
     "parameters": count + 1 + (30 if kind == "spline" else 0)}
    for subset, count in [("words", 2), ("grammar", 12), ("combined", 14)]
    for kind in ["linear", "spline"]
]


def included_families(families, subset):
    demand(subset in SUBSETS, "unknown feature subset")
    selected = [name for name in families if subset == "combined" or
                ((name in {"word", "word_bigram"}) == (subset == "words"))]
    demand(len(selected) == {"words": 2, "grammar": 12, "combined": 14}[subset],
           "unexpected included family count")
    return selected


def prior_weights(families):
    weights = torch.tensor([0.25 if family in {"word", "word_bigram"} else 0.5 / 12
                            for family in families], dtype=torch.float64)
    return weights / weights.sum()


def fit_knots(distances, count=30):
    """Fixed dyadic proposal sequence; input must contain original train only."""
    ordered = torch.sort(distances.reshape(-1)).values
    maximum = ordered[-1].item()
    accepted, proposals, depth = [], [], 1
    while len(accepted) < count:
        demand(depth <= 24, "not enough distinct positive interior knots")
        denominator = 2 ** depth
        for numerator in range(1, denominator, 2):
            quantile = numerator / denominator
            position = quantile * (len(ordered) - 1)
            lo = int(position)
            hi = min(lo + 1, len(ordered) - 1)
            value = (ordered[lo] + (ordered[hi] - ordered[lo]) * (position - lo)).item()
            reason = "accepted" if 0 < value < maximum and value not in accepted else "nonpositive_or_maximum_or_duplicate"
            proposals.append({"quantile": quantile, "value": value, "status": reason})
            if reason == "accepted":
                accepted.append(value)
                if len(accepted) == count:
                    break
        depth += 1
    return {"knots": sorted(accepted), "accepted_proposal_order": accepted,
            "proposals": proposals, "population_count": distances.numel(),
            "interpolation": "linear empirical quantile"}


class FamilyMetric(nn.Module):
    def __init__(self, config, families, knots, seed):
        super().__init__()
        self.families = included_families(families, config["subset"])
        self.register_buffer("family_indices", torch.tensor([families.index(name) for name in self.families]))
        theta = prior_weights(self.families).log()
        if seed:
            theta += 0.05 * torch.randn_like(theta)
        self.theta = nn.Parameter(theta)
        self.log_tau = nn.Parameter(torch.tensor(math.log(0.1), dtype=torch.float64))
        if config["kind"] == "spline":
            demand(len(knots) == 30 and knots == sorted(set(knots)) and knots[0] > 0,
                   "spline requires exactly 30 distinct positive sorted knots")
            self.register_buffer("knots", torch.tensor(knots, dtype=torch.float64))
            self.log_slope = nn.Parameter(torch.zeros(30, dtype=torch.float64))
        else:
            demand(config["kind"] == "linear", "unknown family metric architecture")
            self.register_buffer("knots", torch.tensor([], dtype=torch.float64))
            self.register_parameter("log_slope", None)
        self._prepared = {}
        demand(sum(parameter.numel() for parameter in self.parameters()) == config["parameters"],
               "actual parameter count differs from protocol")

    def prepare(self, original):
        key = (id(original), original._version)
        if key not in self._prepared:
            values = original.index_select(-1, self.family_indices)
            if self.log_slope is None:
                self._prepared[key] = (original, values, None)
            else:
                intervals = torch.bucketize(values.contiguous(), self.knots)
                left = torch.cat((torch.zeros(1, dtype=values.dtype), self.knots))
                self._prepared[key] = (original, values - left[intervals], intervals)
        return self._prepared[key][1:]

    def forward(self, original):
        values, intervals = self.prepare(original)
        if self.log_slope is not None:
            slopes = torch.cat((torch.ones(1, dtype=values.dtype), self.log_slope.exp()))
            lengths = torch.diff(torch.cat((torch.zeros(1, dtype=values.dtype), self.knots)))
            areas = torch.cat((torch.zeros(1, dtype=values.dtype), torch.cumsum(slopes[:-1] * lengths, dim=0)))
            values = areas[intervals] + slopes[intervals] * values
        return -(values * self.theta.softmax(0)).sum(-1) / self.log_tau.exp()

    def constrain(self):
        with torch.no_grad():
            self.log_tau.clamp_(-6, 2)
            if self.log_slope is not None:
                self.log_slope.clamp_(-6, 6)


def make_model(config, families, knots, seed):
    torch.manual_seed(seed)
    demand(config in CONFIGS and seed in SEEDS, "unknown model or seed")
    return FamilyMetric(config, families, knots, seed)

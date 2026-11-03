"""Fixed positive metrics, monotone splines, and shared candidate-scoring MLPs."""
import math

import torch
from torch import nn

from frontier_common import demand


SEEDS = [0, 17, 29]
CATALOG = [
    {"name": "tied_8", "kind": "tied", "groups": 7, "parameters": 8},
    {"name": "tied_12", "kind": "tied", "groups": 11, "parameters": 12},
    {"name": "diagonal_15", "kind": "diagonal", "parameters": 15},
] + [
    {"name": f"spline_{15 + knots:02}", "kind": "spline", "knots": knots, "parameters": 15 + knots}
    for knots in [3, 5, 8, 15, 30, 45]
] + [{"name": "linear_14", "kind": "linear", "parameters": 14}] + [
    {"name": f"mlp_{16 * hidden}", "kind": "mlp", "hidden": hidden, "parameters": 16 * hidden}
    for hidden in [4, 16, 64, 256, 1024]
]


def original_weights(families):
    return torch.tensor([0.25 if f in {"word", "word_bigram"} else 0.5 / 12 for f in families], dtype=torch.float64)


def groups_for(families, count):
    if count == 7:
        groups = [["word"], ["word_bigram"], ["pos", "pos_tag", "pos_bigram", "pos_trigram"],
                  ["dependency", "dependency_path_2", "dependency_path_3"],
                  ["ordered_head_frame", "clause_head_frame"], ["morphology_bundle"],
                  ["syntax_load", "syntax_sentence_load"]]
    elif count == 11:
        groups = [["pos", "pos_tag"], ["dependency_path_2", "dependency_path_3"],
                  ["syntax_load", "syntax_sentence_load"]]
        tied = set(sum(groups, []))
        groups += [[family] for family in families if family not in tied]
    else:
        groups = [[family] for family in families]
    demand(len(groups) == count and sorted(sum(groups, [])) == families, "invalid tied family groups")
    return groups


def fit_transforms(x):
    """Fit statistics and a nested knot proposal sequence from training inputs only."""
    log_x = torch.log1p(x)
    mean = log_x.mean(dim=(0, 1))
    scale = log_x.std(dim=(0, 1), correction=0).clamp_min(1e-8)
    ordered = torch.sort(x.reshape(-1)).values
    maximum = ordered[-1].item()
    accepted, proposals = [], []
    depth = 1
    while len(accepted) < 45:
        demand(depth <= 24, "not enough distinct positive interior training knots")
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
                if len(accepted) == 45:
                    break
        depth += 1
    return {"schema": "unslop-frontier-training-transforms-v1", "log1p_mean": mean.tolist(),
            "log1p_scale": scale.tolist(), "standard_deviation": "population, floor 1e-8",
            "accepted_knot_proposal_order": accepted, "knot_proposals": proposals,
            "knot_population_count": x.numel(), "knot_interpolation": "linear empirical quantile"}


class PositiveMetric(nn.Module):
    def __init__(self, config, families, transforms, seed):
        super().__init__()
        groups = groups_for(families, config.get("groups", 14))
        prior = original_weights(families)
        group_mass = torch.tensor([sum(prior[families.index(f)] for f in group) for group in groups], dtype=torch.float64)
        indices, ratios = [], []
        for index, family in enumerate(families):
            group = next(g for g, members in enumerate(groups) if family in members)
            indices.append(group)
            ratios.append((prior[index] / group_mass[group]).item())
        self.register_buffer("group_index", torch.tensor(indices))
        self.register_buffer("within_group_ratio", torch.tensor(ratios, dtype=torch.float64))
        theta = group_mass.log()
        if seed:
            theta = theta + torch.randn_like(theta) * 0.05
        self.theta = nn.Parameter(theta)
        self.log_tau = nn.Parameter(torch.tensor(math.log(0.1), dtype=torch.float64))
        knot_count = config.get("knots", 0)
        knots = sorted(transforms["accepted_knot_proposal_order"][:knot_count])
        self.register_buffer("knots", torch.tensor(knots, dtype=torch.float64))
        if knot_count:
            self.log_slope = nn.Parameter(torch.zeros(knot_count, dtype=torch.float64))
        else:
            self.register_parameter("log_slope", None)
        self._prepared = {}

    def weights(self):
        return self.theta.softmax(dim=0)[self.group_index] * self.within_group_ratio

    def warp(self, x):
        if self.log_slope is None:
            return x
        # Distances and knots stay fixed during a fit. Cache only this indexing;
        # slope-dependent areas are rebuilt in the autograd graph every call.
        cache_key = (id(x), x._version)
        if cache_key not in self._prepared:
            intervals = torch.bucketize(x.contiguous(), self.knots)
            left = torch.cat((torch.zeros(1, dtype=x.dtype), self.knots))
            residual = x - left[intervals]
            self._prepared[cache_key] = (x, intervals, residual)
        _, intervals, residual = self._prepared[cache_key]
        slopes = torch.cat((torch.ones(1, dtype=x.dtype), self.log_slope.exp()))
        lengths = torch.diff(torch.cat((torch.zeros(1, dtype=x.dtype), self.knots)))
        areas = torch.cat((torch.zeros(1, dtype=x.dtype), torch.cumsum(slopes[:-1] * lengths, dim=0)))
        return areas[intervals] + slopes[intervals] * residual

    def forward(self, x):
        return -(self.warp(x) * self.weights()).sum(dim=-1) / self.log_tau.exp()

    def constrain(self):
        with torch.no_grad():
            self.log_tau.clamp_(-6, 2)
            if self.log_slope is not None:
                self.log_slope.clamp_(-6, 6)


class LearnedScorer(nn.Module):
    def __init__(self, config, transforms):
        super().__init__()
        self.register_buffer("input_mean", torch.tensor(transforms["log1p_mean"], dtype=torch.float64))
        self.register_buffer("input_scale", torch.tensor(transforms["log1p_scale"], dtype=torch.float64))
        if config["kind"] == "linear":
            self.network = nn.Linear(14, 1, bias=False, dtype=torch.float64)
        else:
            self.network = nn.Sequential(nn.Linear(14, config["hidden"], dtype=torch.float64), nn.Tanh(),
                                         nn.Linear(config["hidden"], 1, bias=False, dtype=torch.float64))
        for module in self.network.modules():
            if isinstance(module, nn.Linear):
                nn.init.xavier_uniform_(module.weight)
                if module.bias is not None:
                    nn.init.zeros_(module.bias)
        self._prepared = {}

    def forward(self, x):
        cache_key = (id(x), x._version)
        if cache_key not in self._prepared:
            self._prepared[cache_key] = (x, (x.log1p() - self.input_mean) / self.input_scale)
        return self.network(self._prepared[cache_key][1]).squeeze(-1)

    def constrain(self):
        pass


def make_model(config, families, transforms, seed):
    torch.manual_seed(seed)
    model = PositiveMetric(config, families, transforms, seed) if config["kind"] in {"tied", "diagonal", "spline"} else LearnedScorer(config, transforms)
    count = sum(parameter.numel() for parameter in model.parameters())
    demand(count == config["parameters"], f"actual parameter count differs: {config['name']}: {count}")
    return model

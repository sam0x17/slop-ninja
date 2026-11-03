"""Positive spline calibration over an explicit ordered set of family names."""
import math

import torch
from torch import nn

from frontier_common import demand


def initial_state(all_families, baseline_families, active_families, seed, settings):
    demand(all_families == sorted(set(all_families)), "input family names must be sorted and unique")
    demand(baseline_families == sorted(set(baseline_families)) and set(baseline_families) <= set(all_families),
           "baseline family catalog differs")
    demand(active_families == sorted(set(active_families)) and active_families and set(active_families) <= set(all_families),
           "active family mask is empty, repeated, or unknown")
    noise_order = baseline_families + sorted(set(all_families) - set(baseline_families))
    theta = torch.tensor([math.log(settings["lexical_strength"] if name in {"word", "word_bigram"}
                                  else settings["grammar_strength"]) for name in noise_order], dtype=torch.float64)
    if seed != settings["zero_jitter_seed"]:
        generator = torch.Generator(device="cpu").manual_seed(seed)
        theta += settings["jitter_std"] * torch.randn(len(noise_order), generator=generator, dtype=torch.float64)
    active_indices = [noise_order.index(name) for name in active_families]
    baseline_sum = theta[:len(baseline_families)].exp().sum()
    active_sum = theta[active_indices].exp().sum()
    temperature = settings["baseline_temperature"] * (baseline_sum / active_sum).item()
    demand(math.isfinite(temperature) and temperature > 0, "invalid matched initial temperature")
    return {"theta": theta[active_indices], "log_tau": torch.tensor(math.log(temperature), dtype=torch.float64),
            "noise_order": noise_order, "named_theta": dict(zip(noise_order, theta.tolist())),
            "baseline_strength_sum": baseline_sum.item(), "active_strength_sum": active_sum.item(),
            "temperature": temperature}


def fit_shared_knots(training_panels, baseline_families, count):
    """Only supplied TRAIN baseline columns enter the empirical knot population."""
    demand(training_panels and count > 0, "empty knot fitting scope")
    values, source_rows = [], []
    for name, data in training_panels.items():
        demand(data["split"] == "train", "knot fitting requires TRAIN tensors only")
        indices = [data["families"].index(family) for family in baseline_families]
        x = data["x"].index_select(-1, torch.tensor(indices))
        demand(x.dtype == torch.float64 and bool(torch.isfinite(x).all()) and bool((x >= 0).all()), "invalid knot population")
        values.append(x.reshape(-1))
        source_rows.append({"name": name, "manifest_sha256": data["manifest_sha256"], "values": x.numel()})
    ordered = torch.sort(torch.cat(values)).values
    maximum, accepted, proposals, depth = ordered[-1].item(), [], [], 1
    demand(maximum > 0 and torch.unique_consecutive(ordered).numel() >= count + 1, "insufficient distinct knot values")
    while len(accepted) < count:
        demand(depth <= 24, "not enough distinct interior dyadic knots")
        denominator = 2 ** depth
        for numerator in range(1, denominator, 2):
            quantile = numerator / denominator
            position = quantile * (len(ordered) - 1)
            low = int(position)
            high = min(low + 1, len(ordered) - 1)
            value = (ordered[low] + (ordered[high] - ordered[low]) * (position - low)).item()
            status = "accepted" if 0 < value < maximum and value not in accepted else "nonpositive_or_maximum_or_duplicate"
            proposals.append({"quantile": quantile, "value": value, "status": status})
            if status == "accepted":
                accepted.append(value)
                if len(accepted) == count:
                    break
        depth += 1
    return {"schema": "slopninja-named-family-calibration-knots-v1", "baseline_families": baseline_families,
            "knots": sorted(accepted), "accepted_proposal_order": accepted, "proposals": proposals,
            "population_count": ordered.numel(), "source_panels": source_rows, "interpolation": "linear empirical quantile"}


class NamedFamilyMetric(nn.Module):
    def __init__(self, all_families, baseline_families, active_families, knots, seed, settings):
        super().__init__()
        self.all_families = list(all_families)
        self.families = list(active_families)
        initial = initial_state(all_families, baseline_families, active_families, seed, settings)
        self.initialization = {key: value for key, value in initial.items() if key not in {"theta", "log_tau"}}
        demand(knots and knots == sorted(set(knots)) and all(math.isfinite(value) and value > 0 for value in knots),
               "invalid shared knots")
        self.register_buffer("family_indices", torch.tensor([all_families.index(name) for name in active_families]))
        self.register_buffer("knots", torch.tensor(knots, dtype=torch.float64))
        self.theta = nn.Parameter(initial["theta"])
        self.log_tau = nn.Parameter(initial["log_tau"])
        self.log_slope = nn.Parameter(torch.full((len(knots),), float(settings["initial_log_slopes"]), dtype=torch.float64))
        self._prepared = {}

    def prepare(self, original):
        demand(original.dtype == torch.float64 and original.device.type == "cpu" and original.ndim == 3
               and original.shape[-1] == len(self.all_families), "input tensor shape/dtype differs from named family catalog")
        key = (id(original), original._version, self.knots._version, self.family_indices._version)
        if key not in self._prepared:
            demand(not original.requires_grad and bool(torch.isfinite(original).all()) and bool((original >= 0).all()),
                   "expected finite nonnegative frozen distances")
            values = original.index_select(-1, self.family_indices)
            intervals = torch.bucketize(values.contiguous(), self.knots)
            left = torch.cat((torch.zeros(1, dtype=torch.float64), self.knots))
            self._prepared[key] = (original, values - left[intervals], intervals)
        return self._prepared[key][1:]

    def forward(self, original):
        residual, intervals = self.prepare(original)
        slopes = torch.cat((torch.ones(1, dtype=torch.float64), self.log_slope.exp()))
        lengths = torch.diff(torch.cat((torch.zeros(1, dtype=torch.float64), self.knots)))
        areas = torch.cat((torch.zeros(1, dtype=torch.float64), torch.cumsum(slopes[:-1] * lengths, 0)))
        warped = areas[intervals] + slopes[intervals] * residual
        return -(warped * self.theta.softmax(0)).sum(-1) / self.log_tau.exp()

    def constrain(self, temperature_bounds, slope_bounds):
        with torch.no_grad():
            self.log_tau.clamp_(*temperature_bounds)
            self.log_slope.clamp_(*slope_bounds)

    def parameter_json(self):
        demand(all(bool(torch.isfinite(parameter).all()) for parameter in self.parameters()), "nonfinite model parameters")
        return {"families": self.families, "input_families": self.all_families,
                "family_indices": self.family_indices.tolist(), "theta": self.theta.detach().tolist(),
                "log_tau": self.log_tau.item(), "log_slope": self.log_slope.detach().tolist(),
                "family_weights": self.theta.softmax(0).detach().tolist(), "temperature": self.log_tau.exp().item(),
                "knots": self.knots.tolist(), "slopes": [1.0] + self.log_slope.exp().detach().tolist()}


def balanced_panel_loss(model, data):
    losses = torch.nn.functional.cross_entropy(model(data["x"]), data["own"], reduction="none")
    value = (losses * data["loss_weight"]).sum()
    demand(bool(torch.isfinite(value)), "nonfinite panel cross entropy")
    return value


def backward_panels(model, panels):
    demand(panels and len({len(data["authors"]) for data in panels.values()}) == 1,
           "equal panel mean requires the same author count in every TRAIN panel")
    results = {}
    for name, data in panels.items():
        value = balanced_panel_loss(model, data)
        (value / len(panels)).backward()
        results[name] = value.item()
    demand(all(parameter.grad is not None and bool(torch.isfinite(parameter.grad).all()) for parameter in model.parameters()),
           "missing or nonfinite model gradient")
    return {"training_cross_entropy": sum(results.values()) / len(results), "panel_cross_entropies": results}

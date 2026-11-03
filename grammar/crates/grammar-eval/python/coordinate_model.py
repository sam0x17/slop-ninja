"""Positive coordinate residuals with a frozen, differentiable family scorer."""
import math

import torch
from torch import nn

from frontier_common import demand


LAMBDA_GRID = [0.01, 0.1, 1.0, 10.0]
SEEDS = [0, 17, 29]
LOG_WEIGHT_BOUND = math.log(2.0)


class CoordinateInputs:
    """Per-family matrices, never a query-by-candidate-by-coordinate tensor."""
    def __init__(self, query, target, raw_distances, own_indices, family_indices,
                 held_own=None, loss_weights=None):
        query_count, coordinate_count = query.shape
        candidate_count = target.shape[0]
        demand(target.shape[1] == coordinate_count, "query/target coordinate widths differ")
        demand(raw_distances.shape[:2] == (query_count, candidate_count), "raw distance query/candidate shape differs")
        demand(family_indices.shape == (coordinate_count,), "coordinate family indices differ")
        demand(own_indices.shape == (query_count,), "query labels differ")
        demand(bool((own_indices >= 0).all()) and bool((own_indices < candidate_count).all()), "query label outside gallery")
        demand(bool((family_indices >= 0).all()) and bool((family_indices < raw_distances.shape[-1]).all()), "unknown coordinate family")
        demand(held_own is None or held_own.shape == query.shape, "held-date coordinates differ")
        tensors = [query, target, raw_distances] + ([] if held_own is None else [held_own])
        demand(all(tensor.dtype == torch.float64 and bool(torch.isfinite(tensor).all()) for tensor in tensors),
               "coordinate data must be finite float64")
        demand(bool((raw_distances >= 0).all()), "negative raw family distance")
        self.raw_distances = raw_distances
        self.own_indices = own_indices
        self.loss_weights = loss_weights
        self.family_indices = family_indices
        self.query_count, self.candidate_count = query_count, candidate_count
        self.families = []
        for family in range(raw_distances.shape[-1]):
            columns = torch.nonzero(family_indices == family).flatten()
            q = query.index_select(1, columns).contiguous()
            p = target.index_select(1, columns).contiguous()
            own_delta = None if held_own is None else (q - held_own.index_select(1, columns)).square()
            self.families.append((columns, q, q.square(), p, p.square(), own_delta))

    def selected_squared_distances(self, coefficients):
        output = []
        for columns, q, q2, p, p2, own_delta in self.families:
            if not len(columns):
                output.append(torch.zeros_like(self.raw_distances[:, :, 0]))
                continue
            value = coefficients.index_select(0, columns)
            distances = (q2 @ value).unsqueeze(1) + (p2 @ value).unsqueeze(0) - 2 * (q * value) @ p.T
            if own_delta is not None:
                distances = distances.scatter(1, self.own_indices.unsqueeze(1), (own_delta @ value).unsqueeze(1))
            output.append(distances)
        return torch.stack(output, dim=-1)

    def validate_residual_tail(self, tolerance=1e-9):
        selected = self.selected_squared_distances(torch.ones(len(self.family_indices), dtype=torch.float64))
        residual = self.raw_distances - selected
        demand(residual.min().item() >= -tolerance, "selected coordinate contribution exceeds the complete family distance")
        demand(selected.min().item() >= -tolerance, "negative selected squared coordinate distance")
        return {"minimum_unselected_tail": residual.min().item(), "minimum_selected_distance": selected.min().item(),
                "maximum_selected_distance": selected.max().item(), "tolerance": tolerance}


class CoordinateMetric(nn.Module):
    """Only eta is trainable; derivatives pass through the frozen spline."""
    def __init__(self, coordinate_families, baseline_state):
        super().__init__()
        self.eta = nn.Parameter(torch.zeros(len(coordinate_families), dtype=torch.float64))
        self.register_buffer("coordinate_families", coordinate_families.clone())
        self.register_buffer("family_weights", baseline_state["theta"].softmax(0).detach().clone())
        self.register_buffer("temperature", baseline_state["log_tau"].exp().detach().clone())
        knots = baseline_state["knots"].detach().clone()
        slopes = torch.cat((torch.ones(1, dtype=torch.float64), baseline_state["log_slope"].exp().detach()))
        self.register_buffer("knots", knots)
        self.register_buffer("slopes", slopes)
        self.register_buffer("left_edges", torch.cat((torch.zeros(1, dtype=torch.float64), knots)))
        lengths = torch.diff(self.left_edges)
        self.register_buffer("areas", torch.cat((torch.zeros(1, dtype=torch.float64), torch.cumsum(slopes[:-1] * lengths, 0))))
        self.penalty_groups = [torch.nonzero(coordinate_families == family).flatten()
                               for family in range(len(self.family_weights))]
        self.penalty_groups = [columns for columns in self.penalty_groups if len(columns)]
        demand(self.penalty_groups, "no supported coordinates to train")
        demand(bool(torch.isfinite(self.family_weights).all()) and bool((self.family_weights > 0).all()), "invalid frozen family weights")
        demand(bool(torch.isfinite(self.slopes).all()) and bool((self.slopes > 0).all()), "invalid frozen slopes")
        demand(bool(self.temperature > 0) and bool(torch.isfinite(self.temperature)), "invalid frozen temperature")

    def adjusted_distances(self, inputs):
        demand(torch.equal(inputs.family_indices, self.coordinate_families), "coordinate family catalog differs")
        adjusted = inputs.raw_distances + inputs.selected_squared_distances(self.eta.expm1())
        demand(bool(torch.isfinite(adjusted).all()) and adjusted.min().item() >= -1e-9,
               "nonfinite or negative adjusted family distance")
        return adjusted.clamp_min(0)

    def score_distances(self, distances):
        intervals = torch.bucketize(distances.contiguous(), self.knots)
        calibrated = self.areas[intervals] + self.slopes[intervals] * (distances - self.left_edges[intervals])
        return -(calibrated * self.family_weights).sum(-1) / self.temperature

    def forward(self, inputs):
        return self.score_distances(self.adjusted_distances(inputs))

    def penalty(self):
        return torch.stack([self.eta.index_select(0, columns).square().mean() for columns in self.penalty_groups]).mean()

    def constrain(self):
        with torch.no_grad():
            self.eta.clamp_(-LOG_WEIGHT_BOUND, LOG_WEIGHT_BOUND)

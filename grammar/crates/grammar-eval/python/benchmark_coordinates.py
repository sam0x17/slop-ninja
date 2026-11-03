"""Synthetic CPU timing for the matrix-factorized coordinate learner."""
import argparse
import json
import statistics
import time
from pathlib import Path

import torch

from coordinate_model import CoordinateInputs, CoordinateMetric
from frontier_common import write_json
from train_frontier import configure


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    configure()
    torch.manual_seed(111)
    counts = [512, 512] + [96] * 10 + [6, 6]
    families = torch.repeat_interleave(torch.arange(14), torch.tensor(counts))
    query = torch.rand(1224, sum(counts)) * 0.01
    target = torch.rand(100, sum(counts)) * 0.01
    held = torch.rand_like(query) * 0.01
    labels = torch.arange(1224) % 100
    inputs = CoordinateInputs(query, target, torch.ones(1224, 100, 14), labels, families, held)
    inputs.raw_distances = inputs.selected_squared_distances(torch.ones(sum(counts))) + 0.1
    state = {"theta": torch.zeros(14), "log_tau": torch.tensor(-2.302585092994046),
             "knots": torch.linspace(0.05, 0.2, 30), "log_slope": torch.zeros(30)}
    model = CoordinateMetric(families, state)
    optimizer = torch.optim.Adam(model.parameters(), lr=0.03, foreach=False, fused=False)
    durations = []
    for iteration in range(6):
        began = time.perf_counter()
        optimizer.zero_grad(set_to_none=True)
        loss = torch.nn.functional.cross_entropy(model(inputs), labels) + model.penalty()
        loss.backward()
        optimizer.step()
        model.constrain()
        elapsed = time.perf_counter() - began
        if iteration:
            durations.append(elapsed)
    result = {"schema": "slopninja-coordinate-synthetic-benchmark-v1", "synthetic_only": True,
              "queries": 1224, "candidates": 100, "coordinates": sum(counts), "families": 14,
              "trainable_parameters": sum(parameter.numel() for parameter in model.parameters()),
              "dtype": "float64", "threads": 8, "torch": torch.__version__, "step_seconds": durations,
              "median_step_seconds": statistics.median(durations),
              "projected_12_fit_200_epoch_training_seconds": statistics.median(durations) * 2400,
              "largest_tensor_axes": "Q*K, A*K, Q*A*14; no Q*A*K"}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    write_json(args.out, result)
    print(json.dumps(result), flush=True)


if __name__ == "__main__":
    main()

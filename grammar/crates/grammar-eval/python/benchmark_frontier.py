"""Synthetic-only hardware benchmark; never opens corpus or evaluation data."""
import argparse
import json
import os
import platform
import statistics
import time
from pathlib import Path

import torch


def benchmark(hidden, dtype, threads, steps):
    torch.set_num_threads(threads)
    torch.manual_seed(123)
    x = torch.rand(1224, 100, 14, dtype=dtype)
    own = torch.arange(1224) % 100
    weights = torch.full((1224,), 1.0 / 1224, dtype=dtype)
    network = torch.nn.Sequential(
        torch.nn.Linear(14, hidden, dtype=dtype),
        torch.nn.Tanh(),
        torch.nn.Linear(hidden, 1, bias=False, dtype=dtype),
    )
    log_tau = torch.nn.Parameter(torch.tensor(-2.302585092994046, dtype=dtype))
    parameters = list(network.parameters()) + [log_tau]
    optimizer = torch.optim.Adam(parameters, lr=0.03)
    timings = []
    for step in range(steps + 1):
        start = time.perf_counter()
        optimizer.zero_grad(set_to_none=True)
        logits = network(x).squeeze(-1) / log_tau.exp()
        loss = (torch.nn.functional.cross_entropy(logits, own, reduction="none") * weights).sum()
        loss.backward()
        optimizer.step()
        with torch.no_grad():
            log_tau.clamp_(-6, 2)
        elapsed = time.perf_counter() - start
        if step:
            timings.append(elapsed)
    return {
        "hidden": hidden,
        "trainable_parameters": sum(parameter.numel() for parameter in parameters),
        "dtype": str(dtype),
        "threads": threads,
        "step_seconds": timings,
        "median_step_seconds": statistics.median(timings),
        "projected_three_seed_200_epoch_seconds": statistics.median(timings) * 600,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--steps", type=int, default=3)
    args = parser.parse_args()
    torch.use_deterministic_algorithms(True)
    torch.set_num_interop_threads(1)
    report = {
        "schema": "unslop-synthetic-frontier-hardware-v1",
        "synthetic_only": True,
        "python": platform.python_version(),
        "platform": platform.platform(),
        "cpu_count": os.cpu_count(),
        "torch": torch.__version__,
        "mps_available": torch.backends.mps.is_available(),
        "examples": 1224,
        "candidates": 100,
        "families": 14,
        "results": [],
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    for hidden in [4, 16, 64, 256, 1024]:
        for dtype in [torch.float32, torch.float64]:
            result = benchmark(hidden, dtype, 8, args.steps)
            report["results"].append(result)
            print(json.dumps(result), flush=True)
            args.out.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()

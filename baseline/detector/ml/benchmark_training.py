"""Fixed synthetic ModernBERT training benchmark; no corpus or detector evaluation."""
import argparse
import gc
import json
from pathlib import Path
import statistics
import time

import torch
from safetensors.torch import save_file

from common import configure_cpu, load_checkpoint, sha256, software


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--checkpoint", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    if not torch.backends.mps.is_available():
        raise ValueError("MPS is required for this benchmark")
    configure_cpu()
    report = {
        "schema": "slop_ninja_synthetic_training_benchmark_v1",
        "checkpoint_sha256": sha256(args.checkpoint / "checkpoint.json"),
        "common_py_sha256": sha256(Path(__file__).with_name("common.py")),
        "benchmark_py_sha256": sha256(__file__), "software": software(),
        "warmup_steps": 3, "measured_steps": 5, "batch_size": 4,
        "synthetic_inputs_and_arbitrary_labels": True, "corpus_loaded": False,
        "cases": {},
    }
    for lengths in [(128, 256, 384, 512), (256, 512, 768, 1024)]:
        name = f"padded-{max(lengths)}"
        _, model = load_checkpoint(args.checkpoint, 17)
        attention = model.config._attn_implementation
        model.to("mps").train()
        generator = torch.Generator().manual_seed(1337)
        ids = torch.randint(256, 40000, (4, max(lengths)), generator=generator)
        mask = torch.zeros_like(ids)
        for i, length in enumerate(lengths):
            mask[i, :length] = 1
            ids[i, length:] = 0
        inputs = {"input_ids": ids.to("mps"), "attention_mask": mask.to("mps")}
        labels = torch.tensor([0, 1, 2, 0], device="mps")
        weights = torch.tensor([1.0, 0.5, 0.5, 2.0], device="mps")
        parameters = list(model.parameters())
        optimizer = torch.optim.AdamW(parameters, lr=2e-5, weight_decay=0.01)

        def step(capture=False):
            optimizer.zero_grad(set_to_none=True)
            logits = model(**inputs).logits
            losses = torch.nn.functional.cross_entropy(logits, labels, reduction="none")
            loss = (losses * weights).mean()
            if not torch.isfinite(loss):
                raise ValueError("non-finite loss")
            loss.backward()
            if capture:
                gradients = {key: value.grad.detach().cpu().contiguous()
                             for key, value in model.named_parameters() if value.grad is not None}
                save_file(gradients, args.output / f"{name}-initial-gradients.safetensors")
                first = {"logits": logits.detach().cpu().tolist(), "loss": float(loss.detach().cpu())}
            norm = torch.nn.utils.clip_grad_norm_(parameters, 1.0, error_if_nonfinite=True)
            optimizer.step()
            torch.mps.synchronize()
            if capture:
                return {**first, "gradient_norm": float(norm.detach().cpu())}
            return float(loss.detach().cpu())

        # Capture the first gradient separately; disk writes never enter timings.
        first = step(capture=True)
        for _ in range(3):
            step()
        timings = []
        losses = []
        for _ in range(5):
            torch.mps.synchronize()
            start = time.perf_counter()
            losses.append(step())
            timings.append(time.perf_counter() - start)
        result = {"attention": attention, "token_lengths": lengths, "initial": first,
                  "step_seconds": timings, "median_step_seconds": statistics.median(timings),
                  "mean_step_seconds": statistics.mean(timings),
                  "coefficient_of_variation": statistics.pstdev(timings) / statistics.mean(timings),
                  "measured_losses": losses,
                  "initial_gradients_sha256": sha256(args.output / f"{name}-initial-gradients.safetensors")}
        report["cases"][name] = result
        (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps({"case": name, **result}), flush=True)
        del step, optimizer, parameters, model, inputs, labels, weights
        gc.collect()
        torch.mps.empty_cache()


if __name__ == "__main__":
    main()

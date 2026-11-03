"""Check synthetic tensor execution/export. This produces no qualified detector."""

import argparse
import math
from pathlib import Path
import time

import torch
from transformers import AutoModelForSequenceClassification

from common import (configure_cpu, load_checkpoint, package_artifact, sha256,
                    software, token_ids, write_json)


class LogitsOnly(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        self.model = model

    def forward(self, input_ids, attention_mask):
        return self.model(input_ids=input_ids, attention_mask=attention_mask).logits


def input_gradient_probe(model, tokenizer, inputs, reference):
    """Verify a local embedding derivative on synthetic texts, not edit usefulness."""
    model.eval()
    model.zero_grad(set_to_none=True)
    embeddings = model.get_input_embeddings()(inputs["input_ids"]).detach().requires_grad_(True)

    def margin(value):
        logits = model(inputs_embeds=value, attention_mask=inputs["attention_mask"]).logits
        score = (torch.logsumexp(logits[:, 1:], dim=-1) - logits[:, 0]).mean()
        return score, logits

    score, logits = margin(embeddings)
    difference = float((logits.detach() - reference).abs().max())
    if difference > 1e-6:
        raise ValueError("embedding inputs disagree with token-ID reference logits")
    gradient, = torch.autograd.grad(score, embeddings)
    content = inputs["attention_mask"].bool()
    for special in tokenizer.all_special_ids:
        content &= inputs["input_ids"] != special
    content_gradient = gradient * content.unsqueeze(-1)
    norm = content_gradient.norm()
    if not torch.isfinite(gradient).all() or not torch.isfinite(norm) or float(norm) <= 0:
        raise ValueError("nonfinite or zero content-embedding gradient")
    direction = content_gradient / norm
    expected = float((gradient * direction).sum())
    epsilon = 0.001
    with torch.no_grad():
        plus, _ = margin(embeddings.detach() + epsilon * direction)
        minus, _ = margin(embeddings.detach() - epsilon * direction)
    observed = float((plus - minus) / (2 * epsilon))
    error = abs(observed - expected)
    tolerance = 0.0005 + 0.02 * abs(expected)
    if not math.isfinite(observed) or error > tolerance:
        raise ValueError("embedding gradient disagrees with the finite-difference probe")
    return {"objective": "mean logsumexp(model_only,mixed) minus human_only logit",
            "embedding_shape": list(embeddings.shape), "content_tokens": int(content.sum()),
            "token_id_embedding_max_logit_difference": difference,
            "content_gradient_norm": float(norm), "finite_difference_epsilon": epsilon,
            "autograd_directional_derivative": expected, "finite_difference_derivative": observed,
            "absolute_error": error, "allowed_error": tolerance,
            "direction": "normalized content-token gradient; special and padding tokens unchanged",
            "provider_transfer_tested": False, "semantic_edit_validated": False}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--checkpoint", required=True, type=Path)
    p.add_argument("--output", required=True, type=Path)
    p.add_argument("--device", choices=["cpu", "mps"], default="cpu")
    p.add_argument("--try-export", action="store_true")
    args = p.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    configure_cpu()
    tokenizer, model = load_checkpoint(args.checkpoint, 17)
    texts = ["An editor checked this short paragraph.", "The second sample is a synthetic runtime check."]
    inputs = tokenizer(texts, return_tensors="pt", padding=True)
    model.eval()
    start = time.monotonic()
    with torch.inference_mode():
        initial = model(**inputs).logits
    cpu_forward_seconds = time.monotonic() - start
    model.to(args.device).train()
    optimizer = torch.optim.AdamW(model.parameters(), lr=2e-5)
    # Arbitrary labels exercise the gradient path; they are no production-history evidence.
    synthetic_labels = torch.tensor([0, 1], device=args.device)
    device_inputs = {k: v.to(args.device) for k, v in inputs.items()}
    start = time.monotonic()
    loss = torch.nn.functional.cross_entropy(model(**device_inputs).logits, synthetic_labels)
    loss.backward()
    optimizer.step()
    if args.device == "mps":
        torch.mps.synchronize()
    step_seconds = time.monotonic() - start
    model.cpu().eval()
    with torch.inference_mode():
        reference = model(**inputs).logits
    gradient_report = input_gradient_probe(model, tokenizer, inputs, reference)
    training = {"status": "synthetic_smoke_only_not_a_detector_release",
                "checkpoint_pin_sha256": sha256(args.checkpoint / "checkpoint.json"), "software": software(),
                "device": args.device, "steps": 1, "parameters": sum(p.numel() for p in model.parameters()),
                "seed": 17, "synthetic_labels_are_arbitrary": True, "final_test_opened": False}
    manifest = package_artifact(args.output / "control_bundle", tokenizer, model,
                                {"temperature": 1.0, "status": "not_fitted_synthetic_control"}, training, 64, args.checkpoint)
    reloaded = AutoModelForSequenceClassification.from_pretrained(
        args.output / "control_bundle" / "classifier", local_files_only=True, trust_remote_code=False,
        attn_implementation="eager", reference_compile=False, dtype=torch.float32,
    ).eval()
    with torch.inference_mode():
        reload_logits = reloaded(**inputs).logits
    reload_difference = float((reference - reload_logits).abs().max())
    if reload_difference > 1e-6:
        raise ValueError("safetensors reload exceeded the CPU tolerance")
    length_rejected = False
    try:
        token_ids(tokenizer, "word " * 200, 64)
    except ValueError:
        length_rejected = True
    if not length_rejected:
        raise ValueError("unsupported input was not rejected")
    report = {**training, "cpu_forward_seconds": cpu_forward_seconds, "optimizer_step_seconds": step_seconds,
              "loss": float(loss.detach().cpu()), "initial_logits": initial.tolist(),
              "reference_logits": reference.tolist(), "safetensors_reload_max_logit_difference": reload_difference,
              "input_gradient_probe": gradient_report,
              "length_limit_rejected": length_rejected, "artifact_id": manifest["artifact_id"],
              "export": {"attempted": args.try_export}}
    if args.try_export:
        try:
            exported = torch.export.export(LogitsOnly(model), (inputs["input_ids"], inputs["attention_mask"]))
            torch.export.save(exported, args.output / "synthetic_static_graph.pt2")
            graph = torch.export.load(args.output / "synthetic_static_graph.pt2").module()
            with torch.inference_mode():
                graph_logits = graph(inputs["input_ids"], inputs["attention_mask"])
            difference = float((reference - graph_logits).abs().max())
            report["export"].update(success=difference <= 1e-6, max_logit_difference=difference,
                                    input_shape=list(inputs["input_ids"].shape),
                                    limitation="Static-shape PyTorch graph smoke only; Rust/ONNX inference is not implemented.")
        except Exception as error:
            report["export"].update(success=False, exception_type=type(error).__name__, error=str(error)[:6000])
    write_json(args.output / "smoke_report.json", report)
    print(args.output / "smoke_report.json")


if __name__ == "__main__":
    main()

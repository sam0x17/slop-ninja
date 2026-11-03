"""V6 final-block adaptation primitives and synthetic CUDA qualification.

Rust prepares corpus requests, supervision and partitions. This module owns only
the Torch model boundary. Prefixes are exact FP32 activations before layer 21;
they can be reused because layers 0..20 never train. No padding or truncation.
The original recovered artifact is read-only. Qualification is not a real fit.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import time

os.environ.setdefault("HF_HUB_OFFLINE", "1")
os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")
os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")
os.environ.setdefault("CUBLAS_WORKSPACE_CONFIG", ":4096:8")

import torch
from torch import nn
from transformers import AutoModelForSequenceClassification, AutoTokenizer
import v6_embeddings as bridge
from warm_start_a import WarmStartA, initializer


LAST_LAYER = 21
MAX_TOKENS = 2048


def demand(ok, message):
    if not ok:
        raise ValueError(message)


def tensor_digest(t):
    # NumPy is a dependency of this measured runtime, not production inference.
    return hashlib.sha256(t.detach().cpu().contiguous().numpy().tobytes()).hexdigest()


def load_encoder(artifact, device):
    artifact = Path(artifact).resolve(strict=True)
    manifest, profile = bridge.artifact_binding(artifact)
    for name, expected in manifest["files"].items():
        demand(bridge.sha256(artifact / name) == expected, "artifact member changed: " + name)
    model = AutoModelForSequenceClassification.from_pretrained(
        artifact / "classifier", local_files_only=True, attn_implementation="eager",
        torch_dtype=torch.float32, reference_compile=False).to(device).eval()
    tokenizer = AutoTokenizer.from_pretrained(artifact / "classifier", local_files_only=True)
    demand(model.__class__.__name__ == "ModernBertForSequenceClassification" and
           len(model.model.layers) == LAST_LAYER + 1 and model.config.hidden_size == 768 and
           model.config.classifier_pooling == "mean", "unsupported V6 architecture")
    demand(all(p.dtype == torch.float32 for p in model.parameters()), "FP32 required")
    demand(all(float(getattr(model.config, name)) == 0.0 for name in
               ("attention_dropout", "embedding_dropout", "mlp_dropout", "classifier_dropout")),
           "prefix reuse requires frozen zero-dropout contract")
    model.requires_grad_(False)
    return model, tokenizer, manifest, profile


def token_ids(tokenizer, text, device):
    demand(isinstance(text, str) and text.strip(), "empty input unsupported")
    ids = tokenizer(text, add_special_tokens=True, truncation=False, padding=False)["input_ids"]
    demand(0 < len(ids) <= MAX_TOKENS, "overlength input unsupported; no truncation")
    return torch.tensor([ids], dtype=torch.long, device=device)


def full_encoding(encoder, ids):
    demand(ids.ndim == 2 and ids.shape[0] == 1 and 0 < ids.shape[1] <= MAX_TOKENS,
           "one unpadded request required")
    mask = torch.ones_like(ids)
    outputs = encoder.model(input_ids=ids, attention_mask=mask, return_dict=True)
    pooled = (outputs.last_hidden_state * mask.unsqueeze(-1)).sum(1) / mask.sum(1, keepdim=True)
    return encoder.drop(encoder.head(pooled))


@torch.no_grad()
def capture_prefix(encoder, ids):
    captured = []
    def hook(_module, args):
        captured.append(args[0].detach().clone())
    handle = encoder.model.layers[LAST_LAYER].register_forward_pre_hook(hook)
    try:
        h = full_encoding(encoder, ids)
    finally:
        handle.remove()
    demand(len(captured) == 1 and captured[0].shape == (1, ids.shape[1], 768),
           "unexpected final-block prefix")
    return captured[0], h


def prefix_encoding(encoder, prefix):
    demand(prefix.ndim == 3 and prefix.shape[0] == 1 and prefix.shape[2] == 768 and
           0 < prefix.shape[1] <= MAX_TOKENS and prefix.dtype == torch.float32,
           "one unpadded FP32 prefix required")
    n = prefix.shape[1]
    mask = torch.ones((1, n), device=prefix.device, dtype=torch.long)
    global_mask, sliding = encoder.model._update_attention_mask(mask, output_attentions=False)
    result = encoder.model.layers[LAST_LAYER](
        prefix, attention_mask=global_mask, sliding_window_mask=sliding,
        position_ids=torch.arange(n, device=prefix.device).unsqueeze(0), output_attentions=False)[0]
    result = encoder.model.final_norm(result)
    # Preserve the original sum/divide order; Tensor.mean can choose a different
    # reduction and unnecessarily change the frozen classifier representation.
    pooled = (result * mask.unsqueeze(-1)).sum(1) / mask.sum(1, keepdim=True)
    return encoder.drop(encoder.head(pooled))


class LastBlockA(nn.Module):
    def __init__(self, encoder, heads, adapt):
        super().__init__()
        self.encoder, self.heads, self.adapt = encoder, heads, adapt
        encoder.requires_grad_(False)
        encoder.model.layers[LAST_LAYER].requires_grad_(adapt)
        heads.requires_grad_(True)

    def from_prefix(self, prefix, temperature=1.0):
        h = prefix_encoding(self.encoder, prefix)
        return self.heads(torch.cat((h, h.new_zeros((h.shape[0], 1820))), 1), temperature)

    def forward(self, ids, temperature=1.0):
        h = full_encoding(self.encoder, ids)
        return self.heads(torch.cat((h, h.new_zeros((h.shape[0], 1820))), 1), temperature)

    def trainable_encoder_names(self):
        return [n for n, p in self.encoder.named_parameters() if p.requires_grad]


def configure(device):
    torch.set_num_threads(1)
    torch.set_num_interop_threads(1)
    torch.manual_seed(17)
    torch.use_deterministic_algorithms(True)
    if device.type == "cuda":
        demand(torch.cuda.is_available(), "CUDA unavailable")
        torch.cuda.set_per_process_memory_fraction(0.50)
        torch.backends.cuda.matmul.allow_tf32 = False
        torch.backends.cudnn.allow_tf32 = False
        torch.cuda.reset_peak_memory_stats()


def qualify(args):
    out = Path(args.out)
    demand(not out.exists(), "new output directory required")
    out.mkdir()
    device = torch.device(args.device)
    configure(device)
    started = time.monotonic()
    model, tokenizer, manifest, profile = load_encoder(args.artifact, device)
    init = initializer(args.init)
    heads = WarmStartA(init, [0.] * 768, [1.] * 768, 2, 2, "text").to(device)
    joint = LastBlockA(model, heads, True).eval()
    allowed_prefix = f"model.layers.{LAST_LAYER}."
    trainable = joint.trainable_encoder_names()
    demand(bool(trainable) and all(n.startswith(allowed_prefix) for n in trainable),
           "unexpected trainable encoder parameter")
    refs = json.loads((Path(args.artifact) / "reference_vectors.json").read_bytes())
    cases = [(r["text"], r) for r in refs] + [("The gardener carried a small basket through the quiet village.", None),
        ("The experiment measured rainfall, temperature, and the growth of the plants. " * 32, None)]
    records = []
    prefix = ids = None
    with torch.no_grad():
        for text, ref in cases:
            ids = token_ids(tokenizer, text, device)
            prefix, h = capture_prefix(model, ids)
            restored = prefix_encoding(model, prefix)
            delta = float((h - restored).abs().max())
            demand(delta <= 1e-6, f"prefix replay changes representation: {delta}")
            p = (model.classifier(h).double() / init["base_temperature"]).softmax(-1)[0]
            q = joint.from_prefix(prefix)["origin"][0]
            a_delta = float((p - q).abs().max())
            demand(a_delta <= 1e-6, "initial shared A does not preserve V6")
            reference_error = None
            if ref:
                reference_error = max(abs(float(a) - b) for a, b in zip(p, ref["probabilities"]))
                demand(reference_error <= 1e-5 and ids.shape[1] == ref["token_count"],
                       "CUDA packaging reference qualification failed")
            records.append({"text_sha256":bridge.digest(text.encode()),"tokens":ids.shape[1],
                "prefix_max_absolute_error":delta,"initial_A_probability_error":a_delta,
                "bundled_reference_probability_error":reference_error})
    # Meaningful gradient gate on synthetic supervision. The result is discarded;
    # corpus fitting will use a fresh copy of the immutable encoder.
    before = {n:tensor_digest(p) for n,p in model.named_parameters()}
    optimizer = torch.optim.AdamW([
        {"params":model.model.layers[LAST_LAYER].parameters(),"lr":1e-5},
        {"params":heads.parameters(),"lr":1e-3}], weight_decay=.01)
    result = joint.from_prefix(prefix)
    loss = (torch.nn.functional.cross_entropy(result["origin_logits"],torch.tensor([2],device=device)) +
            (result["share"]-.85).square().mean() +
            .25*torch.nn.functional.cross_entropy(result["human_logits"],torch.tensor([0],device=device)) +
            .25*torch.nn.functional.cross_entropy(result["ai_logits"],torch.tensor([1],device=device)))
    loss.backward()
    demand(all(p.grad is None for n,p in model.named_parameters() if not n.startswith(allowed_prefix)),
           "gradient reached frozen encoder")
    demand(any(p.grad is not None and bool(p.grad.abs().sum()>0) for p in model.model.layers[LAST_LAYER].parameters()),
           "final block received no gradient")
    optimizer.step()
    changed = [n for n,p in model.named_parameters() if tensor_digest(p)!=before[n]]
    demand(changed and all(n.startswith(allowed_prefix) for n in changed), "wrong encoder weights changed")
    with torch.no_grad():
        from_full, from_cache = joint(ids), joint.from_prefix(prefix)
        trained_delta = max(float((from_full[k]-from_cache[k]).abs().max()) for k in from_full)
        demand(trained_delta<=1e-6, "cached prefix disagrees after last-block update")
        p, share = from_cache["origin"],from_cache["share"]
        demand(bool(((p[:,1]<=share)&(share<=p[:,1]+p[:,2])).all()), "share bounds violated")
    runtime={n:importlib.metadata.version(n) for n in ["torch","transformers","numpy","safetensors","tokenizers"]}
    report={"schema":"slop_ninja_last_block_cuda_qualification_v1","complete":True,
        "synthetic_only":True,"real_data_fit":False,"device":str(device),"packages":runtime,
        "artifact_id":manifest["artifact_id"],"parent_packaging_profile":profile,
        "initializer_sha256":bridge.sha256(args.init),"cases":records,
        "reference_probability_tolerance":1e-5,"prefix_tolerance":1e-6,
        "encoder_trainable_names":trainable,"encoder_changed_names":changed,
        "encoder_trainable_parameters":sum(p.numel() for p in model.parameters() if p.requires_grad),
        "frozen_parameters_unchanged":True,"after_step_full_vs_prefix_max_error":trained_delta,
        "memory_cap_fraction":.50,"peak_cuda_allocated_bytes":torch.cuda.max_memory_allocated() if device.type=="cuda" else None,
        "elapsed_seconds":time.monotonic()-started,"implementation_sha256":bridge.sha256(__file__),
        "transformers_model_source_sha256":bridge.sha256(__import__(model.__module__,fromlist=["x"]).__file__),
        "limits":["Synthetic packaging/gradient qualification only; no fitted model or quality claim.",
                  "Different Torch/CUDA runtime than frozen CPU release; measured agreement, not bit identity.",
                  "Rust inference for newly trained encoder weights remains to be qualified."]}
    bridge.write_json(out / "report.json",report)
    print(json.dumps({"complete":True,"cases":len(records),"changed_parameters":len(changed),"peak_cuda_allocated_bytes":report["peak_cuda_allocated_bytes"]}))


if __name__ == "__main__":
    p=argparse.ArgumentParser()
    p.add_argument("--artifact",required=True); p.add_argument("--init",required=True)
    p.add_argument("--out",required=True); p.add_argument("--device",choices=["cpu","cuda"],default="cuda")
    qualify(p.parse_args())

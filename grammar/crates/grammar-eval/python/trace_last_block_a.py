"""Trace a bound synthetic derivative reference without changing model operators."""
import argparse
import json
from pathlib import Path

import torch
from safetensors.torch import load_file
import last_block_a as architecture
import v6_embeddings as bridge


@torch.no_grad()
def run(args):
    root, out = Path(args.export), Path(args.out)
    architecture.demand(not out.exists(), "new trace required")
    m = json.loads((root / "manifest.json").read_bytes())
    architecture.demand(m["schema"] == "slop_ninja_last_block_a_export_v1" and m["complete"], "bound export required")
    for key, name in (("last_layer", "last-layer.safetensors"), ("reference_requests", "reference-requests.jsonl"),
                      ("reference_predictions", "reference-predictions.json")):
        architecture.demand(m[key]["file"] == name and bridge.sha256(root / name) == m[key]["sha256"], "member changed")
    requests = [json.loads(line) for line in (root / "reference-requests.jsonl").read_bytes().splitlines()]
    requests = [r for r in requests if r["id"] == args.case]
    architecture.demand(len(requests) == 1, "missing/duplicate synthetic case")
    device = torch.device(args.device)
    architecture.configure(device)
    model, tokenizer, _, _ = architecture.load_encoder(args.artifact, device)
    model.model.layers[21].load_state_dict(load_file(root / "last-layer.safetensors", device=str(device)))
    ids = architecture.token_ids(tokenizer, requests[0]["text"], device)
    baseline = architecture.full_encoding(model, ids)
    expected = next(r for r in json.loads((root / "reference-predictions.json").read_bytes()) if r["id"] == args.case)
    export_error = float((baseline.cpu().double()[0] - torch.tensor(expected["h_raw_f32"], dtype=torch.float64)).abs().max())
    if args.device == "cuda":
        architecture.demand(export_error == 0., "CUDA trace is not the frozen reference")
    tensors = []
    def capture(name):
        def hook(_module, _args, result):
            if isinstance(result, tuple):
                result = result[0]
            value = result.detach().cpu().clone()
            tensors.append({"name": name, "shape": list(value.shape), "values": value.flatten().tolist()})
        return hook
    names = ["model.embeddings.tok_embeddings", "model.embeddings.norm"]
    for i in range(22):
        names += [f"model.layers.{i}.{s}" for s in ("attn_norm", "attn.Wqkv", "attn.Wo", "mlp_norm", "mlp.Wi", "mlp.act", "mlp.Wo")]
        names.append(f"model.layers.{i}")
    names += ["model.final_norm", "head.dense", "head.act", "head.norm", "classifier"]
    modules = dict(model.named_modules())
    handles = [modules[name].register_forward_hook(capture(name)) for name in names]
    from transformers.models.modernbert import modeling_modernbert as modernbert
    original_attention = modernbert.MODERNBERT_ATTENTION_FUNCTION["eager"]
    def attention(module, **kw):
        # Preserve the original forward and independently reconstruct only its
        # intermediates. The exact check prevents a misleading diagnostic trace.
        result = original_attention(module, **kw)
        qkv = kw["qkv"]
        cos, sin = module.rotary_emb(qkv, position_ids=kw["position_ids"])
        q, k, v = qkv.transpose(3, 1).unbind(dim=2)
        q, k = modernbert.apply_rotary_pos_emb(q, k, cos, sin)
        prefix = f"model.layers.{module.layer_id}.attn."
        capture(prefix + "rotated_q")(None, None, q)
        capture(prefix + "rotated_k")(None, None, k)
        scores = torch.matmul(q, k.transpose(2, 3)) * module.head_dim**-0.5
        capture(prefix + "scores")(None, None, scores)
        mask = kw["sliding_window_mask"] if kw["local_attention"] != (-1, -1) else kw["attention_mask"]
        probabilities = torch.nn.functional.softmax(scores + mask, dim=-1, dtype=torch.float32).to(q.dtype)
        capture(prefix + "probabilities")(None, None, probabilities)
        context = torch.matmul(probabilities, v)
        capture(prefix + "context")(None, None, context)
        reconstructed = context.transpose(1, 2).contiguous().view(kw["bs"], -1, kw["dim"])
        architecture.demand(torch.equal(result[0], reconstructed), "attention trace differs from original")
        return result
    modernbert.MODERNBERT_ATTENTION_FUNCTION["eager"] = attention
    try:
        h = architecture.full_encoding(model, ids)
        logits = model.classifier(h)
    finally:
        modernbert.MODERNBERT_ATTENTION_FUNCTION["eager"] = original_attention
        for handle in handles:
            handle.remove()
    architecture.demand(torch.equal(baseline, h), "trace changed computation")
    bridge.write_json(out, {"schema": "slop_ninja_last_block_trace_v1", "synthetic_only": True, "case": args.case,
                           "ids": ids[0].cpu().tolist(), "device": args.device, "export_manifest_sha256": bridge.sha256(root / "manifest.json"),
                           "trace_preserves_outputs_exactly": True, "export_reference_h_error": export_error,
                           "h_raw_f32": h[0].cpu().tolist(), "logits": logits[0].cpu().tolist(), "tensors": tensors,
                           "implementation_sha256": bridge.sha256(__file__)})


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--artifact", required=True)
    p.add_argument("--export", required=True)
    p.add_argument("--case", choices=("synthetic-short", "synthetic-unicode", "synthetic-long"), required=True)
    p.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
    p.add_argument("--out", required=True)
    run(p.parse_args())

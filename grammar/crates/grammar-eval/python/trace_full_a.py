"""Per-operation trace of a bound full-A export on one synthetic reference case.

Diagnostic only: it localizes Rust replay drift (encoder versus heads, which
operation first diverges, reduced precision versus reduction order). Model
operators are unchanged in FP32: the eager attention is reconstructed and must
equal the original exactly. `--dtype float64` runs the same graph in FP64 with
the attention softmax also in FP64 and HF's float32 rotary tables widened (the
trained function); it is the numerical reference for the comparison, never an
exported prediction. Synthetic cases
only; no corpus row is read.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path

os.environ.setdefault("MKL_CBWR", "COMPATIBLE")
os.environ.setdefault("ATEN_CPU_CAPABILITY", "default")

import torch
from safetensors.torch import load_file

import full_a
import last_block_a as base
import v6_embeddings as bridge

CASES = ("synthetic-short", "synthetic-unicode")
SCHEMA = "slop_ninja_full_a_trace_v1"


def bound_export(root):
    m = json.loads((root / "manifest.json").read_bytes())
    base.demand(m["schema"] == "slop_ninja_full_a_export_v1" and m["complete"] is True and
                m["final_test_opened"] is False, "bound full-A export required")
    for key in ("encoder", "heads", "reference_requests", "reference_predictions"):
        base.demand(bridge.sha256(root / m[key]["file"]) == m[key]["sha256"], "export member changed: " + key)
    return m


def names():
    out = ["model.embeddings.tok_embeddings", "model.embeddings.norm"]
    for i in range(22):
        out += [f"model.layers.{i}.{s}" for s in ("attn_norm", "attn.Wqkv", "attn.Wo", "mlp_norm", "mlp.Wi",
                                                  "mlp.act", "mlp.Wo")]
        out.append(f"model.layers.{i}")
    return out + ["model.final_norm", "head.dense", "head.act", "head.norm"]


@torch.no_grad()
def run(args):
    root, out = Path(args.export).resolve(strict=True), Path(args.out)
    base.demand(not out.exists(), "new trace required")
    manifest = bound_export(root)
    requests = [json.loads(x) for x in (root / "reference-requests.jsonl").read_bytes().splitlines()]
    requests = [r for r in requests if r["id"] == args.case]
    base.demand(len(requests) == 1, "missing/duplicate synthetic case")
    expected = next(r for r in json.loads((root / "reference-predictions.json").read_bytes()) if r["id"] == args.case)
    device = torch.device(args.device)
    base.configure(device)
    dtype = {"float32": torch.float32, "float64": torch.float64}[args.dtype]
    encoder, tokenizer, _, _ = base.load_encoder(args.artifact, device)
    encoder.load_state_dict(load_file(str(root / "encoder.safetensors"), device=str(device)), strict=True)
    encoder = encoder.to(dtype)
    ids, _ = full_a.hf_tokenize(tokenizer)(requests[0]["text"])
    base.demand(ids == expected["token_ids"], "token ids differ from the export reference")
    t = torch.tensor([ids], dtype=torch.long, device=device)
    mask = torch.ones_like(t)

    def encode():
        hidden = encoder.model(input_ids=t, attention_mask=mask, return_dict=True).last_hidden_state
        m = mask.unsqueeze(-1).to(hidden.dtype)
        return hidden, encoder.drop(encoder.head((hidden * m).sum(1) / m.sum(1)))

    tensors = []

    def capture(name):
        def hook(_module, _args, result):
            if isinstance(result, tuple):
                result = result[0]
            v = result.detach().to("cpu", torch.float64)
            tensors.append({"name": name, "shape": list(v.shape), "values": v.flatten().tolist()})
        return hook

    from transformers.models.modernbert import modeling_modernbert as mb
    original = mb.MODERNBERT_ATTENTION_FUNCTION["eager"]

    def attention(module, **kw):
        qkv = kw["qkv"]
        # HF's own rotary tables in every dtype: float32 angles and cos/sin, cast
        # to the model dtype (the trained function; the Rust FP64 profile widens
        # the same float32 tables).
        cos, sin = module.rotary_emb(qkv, position_ids=kw["position_ids"])
        q, k, v = qkv.transpose(3, 1).unbind(dim=2)
        q, k = mb.apply_rotary_pos_emb(q, k, cos, sin)
        prefix = f"model.layers.{module.layer_id}.attn."
        capture(prefix + "rotated_q")(None, None, q)
        capture(prefix + "rotated_k")(None, None, k)
        scores = torch.matmul(q, k.transpose(2, 3)) * module.head_dim ** -0.5
        capture(prefix + "scores")(None, None, scores)
        m = kw["sliding_window_mask"] if kw["local_attention"] != (-1, -1) else kw["attention_mask"]
        softmax_dtype = torch.float32 if dtype == torch.float32 else torch.float64
        probabilities = torch.nn.functional.softmax(scores + m, dim=-1, dtype=softmax_dtype).to(q.dtype)
        capture(prefix + "probabilities")(None, None, probabilities)
        context = torch.matmul(probabilities, v)
        capture(prefix + "context")(None, None, context)
        result = (context.transpose(1, 2).contiguous().view(kw["bs"], -1, kw["dim"]),)
        if dtype == torch.float32:
            base.demand(torch.equal(original(module, **kw)[0], result[0]), "attention trace differs from original")
        return result

    if dtype == torch.float32:
        baseline_hidden, baseline_h = encode()
    modules = dict(encoder.named_modules())
    handles = [modules[n].register_forward_hook(capture(n)) for n in names()]
    mb.MODERNBERT_ATTENTION_FUNCTION["eager"] = attention
    try:
        hidden, h = encode()
    finally:
        mb.MODERNBERT_ATTENTION_FUNCTION["eager"] = original
        for handle in handles:
            handle.remove()
    if dtype == torch.float32:
        base.demand(torch.equal(baseline_h, h) and torch.equal(baseline_hidden, hidden), "trace changed computation")
    reference_h = torch.tensor(expected["h_raw_f32"], dtype=torch.float64)
    bridge.write_json(out, {
        "schema": SCHEMA, "synthetic_only": True, "case": args.case, "ids": ids, "device": args.device,
        "dtype": args.dtype, "export_manifest_sha256": bridge.sha256(root / "manifest.json"),
        "export_reference_device": manifest["reference_device"],
        "max_abs_h_vs_export_reference": float((h[0].to("cpu", torch.float64) - reference_h).abs().max()),
        "h_raw": h[0].to("cpu", torch.float64).tolist(),
        "token_states": hidden[0].to("cpu", torch.float64).flatten().tolist(),
        "tensors": tensors, "torch": torch.__version__, "implementation_sha256": bridge.sha256(__file__)})
    print(json.dumps({"complete": True, "case": args.case, "device": args.device, "dtype": args.dtype}), flush=True)


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--artifact", required=True)
    p.add_argument("--export", required=True)
    p.add_argument("--case", choices=CASES, required=True)
    p.add_argument("--device", choices=("cpu", "cuda"), default="cpu")
    p.add_argument("--dtype", choices=("float32", "float64"), default="float32")
    p.add_argument("--out", required=True)
    run(p.parse_args())

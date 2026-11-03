"""Synthetic CUDA qualification of full-A training on the real V6 artifact.

Synthetic text only: no corpus, provider or detector access, no quality claim.
1. Initial E1/E2 models reproduce V6 on its bundled reference vectors.
2. Measured peak CUDA memory and step time for bf16 + gradient-checkpointed
   training steps at 2,048-token documents, per microbatch token budget and
   attention implementation, and a repeated-gradient determinism check.
3. A tiny synthetic bundle (programmatic word sequences, synthetic profiles and
   spans) is written for an end-to-end trainer -> export -> Rust replay check.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import random
import time

import torch

import full_a
import full_a_bundle as bundle
import last_block_a as base
import synthetic_full_a as synthetic
import train_full_a as trainer
import v6_embeddings as bridge
from warm_start_a import WarmStartA, initializer

synthetic_text = synthetic.synthetic_text


def protocol(epochs=2):
    p = synthetic.protocol_template()
    p.update(max_epochs=epochs, logical_batch_size=8, microbatch_token_budget=8192,
             precision={"train": "bf16_autocast", "evaluate": "fp32"})
    p["optimizer"].update(encoder_peak_learning_rate=2e-5, heads_learning_rate=3e-4)
    p["early_stopping"].update(evaluations_per_epoch=1, patience_evaluations=3, min_delta=1e-4)
    return p


@torch.no_grad()
def write_bundle(out, encoder, tokenizer, parent, device):
    """Synthetic shared-contract bundle (rows.jsonl, weights.json, MANIFEST.json)
    plus its config (contract with a synthetic Train h normalization, protocol)."""
    rows = synthetic.factorial_rows(seed=20261027, groups=6, words=(40, 220), splits=("train", "dev_iter", "cal"))
    hs = [base.full_encoding(encoder, base.token_ids(tokenizer, r["text"], device))[0].double().cpu()
          for r in rows if r["split"] == "train"]
    h = torch.stack(hs)
    digests = synthetic.write_bundle(out / "bundle", rows)
    labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
    contract = synthetic.contract_for(digests[bundle.MANIFEST], labels, parent["artifact_id"])
    contract["synthetic_only"] = True
    contract["embedding_normalization"].update(mean=h.mean(0).tolist(), scale=h.std(0).clamp_min(1e-6).tolist(),
                                               note="synthetic qualification: unweighted Train mean/std")
    (out / "config").mkdir(parents=True)
    (out / "config" / "contract.json").write_bytes(bridge.canonical(contract) + b"\n")
    (out / "config" / "protocol.json").write_bytes(bridge.canonical(protocol()) + b"\n")


def measure(model, tokenizer, device, budget, steps=4):
    rng = random.Random(budget)
    docs = []
    while len(docs) < budget // 2048:
        ids = tokenizer(synthetic_text(rng, 1900), add_special_tokens=True)["input_ids"][:2047] + [tokenizer.sep_token_id]
        docs.append(ids)
    ids = torch.tensor(docs, device=device)
    mask = torch.ones_like(ids)
    content = torch.ones_like(ids, dtype=torch.bool)
    content[:, 0] = content[:, -1] = False
    opt = torch.optim.AdamW([p for p in model.parameters() if p.requires_grad], lr=1e-9)
    torch.cuda.synchronize()
    torch.cuda.reset_peak_memory_stats()
    times = []
    for step in range(steps):
        t0 = time.monotonic()
        opt.zero_grad(set_to_none=True)
        out = model(ids, mask, content)
        loss = out["origin"][:, 1:].sum() + out["share"].sum()
        if out["token_logits"] is not None:
            loss = loss + out["token_logits"].float().mean()
        loss.backward()
        opt.step()
        torch.cuda.synchronize()
        times.append(time.monotonic() - t0)
    tokens = ids.numel()
    warm = times[1:]
    return {"budget_tokens": budget, "documents": len(docs), "tokens_per_step": tokens,
            "peak_cuda_allocated_bytes": torch.cuda.max_memory_allocated(),
            "step_seconds": times, "tokens_per_second_after_first": tokens * len(warm) / sum(warm)}


def run(args):
    out = Path(args.out)
    out.mkdir()
    device = torch.device("cuda")
    trainer.configure(device, .45)
    started = time.monotonic()
    encoder, tokenizer, parent, profile = base.load_encoder(args.artifact, device)
    init = initializer(args.init)
    report = {"schema": "slop_ninja_full_a_cuda_qualification_v1", "synthetic_only": True, "real_data_fit": False,
              "artifact_id": parent["artifact_id"], "initializer_sha256": bridge.sha256(args.init),
              "implementation_sha256": {m.__name__: bridge.sha256(m.__file__) for m in (full_a, trainer)},
              "torch": torch.__version__, "cuda": torch.version.cuda, "device_name": torch.cuda.get_device_name(0),
              "memory_cap_fraction": .45}
    refs = json.loads((Path(args.artifact) / "reference_vectors.json").read_bytes())
    special = set(tokenizer.all_special_ids)
    preservation = []
    for span, mode in ((None, "head"), ("attention", "token_mean_projected"), ("max", "token_mean_projected")):
        heads = WarmStartA(init, [0.] * 768, [1.] * 768, 2, 2, "text")
        model = full_a.FullA(encoder, heads, None if span is None else full_a.SpanHead(span), mode,
                             torch.bfloat16).to(device).eval()
        worst_v6, worst_ref = 0., 0.
        with torch.no_grad():
            for r in refs:
                ids = base.token_ids(tokenizer, r["text"], device)
                content = torch.tensor([full_a.content_mask_for(ids[0].tolist(), special)], device=device)
                o = model(ids, torch.ones_like(ids), content, autocast=False)
                p = (encoder.classifier(o["h"]).double() / init["base_temperature"]).softmax(-1)
                worst_v6 = max(worst_v6, float((o["origin"].double() - p).abs().max()))
                worst_ref = max(worst_ref, max(abs(float(a) - b) for a, b in zip(o["origin"][0], r["probabilities"])))
        base.demand(worst_v6 <= 1e-6 and worst_ref <= 1e-5, "initial full A does not preserve V6")
        preservation.append({"span_head": span, "share_mode": mode, "cases": len(refs),
                             "max_error_vs_V6_classifier": worst_v6, "max_error_vs_bundled_reference": worst_ref})
    report["function_preservation"] = preservation
    encoder.gradient_checkpointing_enable(gradient_checkpointing_kwargs={"use_reentrant": False})
    pristine = {k: v.detach().clone() for k, v in encoder.state_dict().items()}
    perf = []
    for attention in ("eager", "sdpa"):
        try:
            encoder.set_attn_implementation(attention)
        except (ValueError, RuntimeError) as e:
            perf.append({"attention_implementation": attention, "status": "unavailable", "error": str(e)[:400]})
            continue
        for budget in (2048, 8192, 16384):
            encoder.load_state_dict(pristine)
            heads = WarmStartA(init, [0.] * 768, [1.] * 768, 2, 2, "text")
            model = full_a.FullA(encoder, heads, full_a.SpanHead("attention"), "token_mean_projected",
                                 torch.bfloat16).to(device).train()
            try:
                row = measure(model, tokenizer, device, budget)
                row["status"] = "ok"
            except (torch.OutOfMemoryError, RuntimeError) as e:
                row = {"budget_tokens": budget, "status": "failed", "error": str(e)[:400]}
                torch.cuda.empty_cache()
            row["attention_implementation"] = attention
            perf.append(row)
            print(json.dumps(row), flush=True)
    report["training_step_measurements"] = perf
    encoder.load_state_dict(pristine)
    encoder.set_attn_implementation("eager")
    # Determinism: the same microbatch twice from the same weights -> identical gradients.
    heads = WarmStartA(init, [0.] * 768, [1.] * 768, 2, 2, "text")
    model = full_a.FullA(encoder, heads, full_a.SpanHead("attention"), "token_mean_projected",
                         torch.bfloat16).to(device).train()
    rng = random.Random(5)
    ids = torch.tensor([tokenizer(synthetic_text(rng, 600))["input_ids"]], device=device)
    content = torch.ones_like(ids, dtype=torch.bool)
    content[:, 0] = content[:, -1] = False
    grads = []
    for _ in range(2):
        model.zero_grad(set_to_none=True)
        o = model(ids, torch.ones_like(ids), content)
        (o["origin"][:, 1:].sum() + o["share"].sum() + o["token_logits"].mean()).backward()
        grads.append(hashlib.sha256(b"".join(p.grad.detach().float().cpu().numpy().tobytes()
                                             for p in model.parameters() if p.grad is not None)).hexdigest())
    report["repeated_gradient_bitwise_identical"] = grads[0] == grads[1]
    encoder.load_state_dict(pristine)
    encoder.eval()
    write_bundle(out / "synthetic", encoder, tokenizer, parent, device)
    report["synthetic_bundle_manifest_sha256"] = bridge.sha256(out / "synthetic" / "bundle" / bundle.MANIFEST)
    report["elapsed_seconds"] = time.monotonic() - started
    bridge.write_json(out / "report.json", report)
    print(json.dumps({"complete": True, "deterministic": report["repeated_gradient_bitwise_identical"]}), flush=True)


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    for key in ("artifact", "init", "out"):
        p.add_argument("--" + key, required=True)
    run(p.parse_args())

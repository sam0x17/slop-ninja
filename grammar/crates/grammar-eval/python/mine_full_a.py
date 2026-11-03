"""Score the Train-mining human pool with a completed full-A fit.

Inference only: the selected checkpoint's uncalibrated P(any AI) per pool
document, streamed from a splits `train-mining/documents.jsonl`. Every row
must carry `frozen_split` and `split_assignment.split` equal to
`train_mining` and `origin` `human_only`; anything else (CAL, Dev-iter,
Dev-select, Test, held or Train rows) stops the run before its text is
tokenized. Selection, the stop rule, the next-round bundle and the matched
positive request queue are Rust (`mine_full_a` in grammar-a-baseline).

Outputs `scores.jsonl` (id, text_sha256, source_group, domain, tokens,
p_any_ai; no text) and `manifest.json`. Documents beyond 2,048 tokens are
skipped and counted (no truncation). `--sample-keep K --sample-modulus M`
scores the deterministic subsample whose SHA-256("seed:id") prefix is below
K/M (the pool is millions of rows).

Resume is bound: the first invocation writes `binding.json` (fit.json and
selected-checkpoint digests, bundle and pool digests, the sample rule,
precision, token budget and implementation digest); a later invocation in the
same directory refuses to continue under any other binding. A partial last
line left by a crash is truncated away before appending; completed ids are
kept.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import time

import torch

import export_full_a as exporter
import full_a
import full_a_bundle as bundle
import train_full_a as trainer
import v6_embeddings as bridge


SCORE_SCHEMA = "slop_ninja_full_a_mining_scores_v2"
BINDING_SCHEMA = "slop_ninja_full_a_mining_binding_v1"
POOL_SPLIT = "train_mining"


def sampled(seed, row_id, keep, modulus):
    return int(hashlib.sha256(f"{seed}:{row_id}".encode()).hexdigest()[:16], 16) % modulus < keep


def pool_rows(path, sample=None):
    """Yield (row without text, text) for Train-mining human rows; refuse others.

    sample=(seed, keep, modulus) skips unsampled rows after the split check."""
    with open(path, "rb") as f:
        for n, line in enumerate(f, 1):
            if not line.strip():
                continue
            r = json.loads(line)
            assignment = r.get("split_assignment") or {}
            bundle.demand(r.get("frozen_split") == POOL_SPLIT and assignment.get("split") == POOL_SPLIT,
                          f"pool row {n}: only Train-mining rows may be mined")
            bundle.demand(r.get("origin") == "human_only", f"pool row {n}: mining pool rows are human-only")
            if sample is not None and not sampled(sample[0], r["id"], sample[1], sample[2]):
                continue
            text = r.pop("text")
            bundle.demand(isinstance(text, str) and hashlib.sha256(text.encode()).hexdigest() == r["text_sha256"],
                          f"pool row {n}: exact text binding differs")
            yield r, text


def score_binding(fit, report, digests, pool_sha, sample, precision, budget):
    return {"schema": BINDING_SCHEMA, "fit_sha256": bridge.sha256(fit / "fit.json"),
            "selected_checkpoint_sha256": report["selected_checkpoint_sha256"],
            "bundle_sha256": digests, "pool_sha256": pool_sha,
            "sample": None if sample is None else {"rule": "int(sha256(seed:id)[:16],16) % modulus < keep",
                                                   "seed": sample[0], "keep": sample[1], "modulus": sample[2]},
            "precision": precision, "padded_token_budget": budget,
            "implementation_sha256": bridge.sha256(__file__)}


def bind(out, binding):
    """Write the binding on first use; refuse a resume under another binding."""
    path = out / "binding.json"
    if path.exists():
        bundle.demand(bundle.read_json(path) == json.loads(json.dumps(binding)),
                      "mining resume binding differs (fit, checkpoint, bundle, pool, sample rule, precision or "
                      "budget); use a new output directory")
    else:
        bundle.demand(not (out / "scores.jsonl").exists(), "scores without a binding; use a new output directory")
        bridge.write_json(path, binding)


def completed_ids(path):
    """Ids already scored. A partial final line (crash mid-write) is cut off."""
    done = set()
    if not path.exists():
        return done
    data = path.read_bytes()
    end = data.rfind(b"\n") + 1
    if end != len(data):
        with path.open("r+b") as f:
            f.truncate(end)
            f.flush()
            os.fsync(f.fileno())
    for line in data[:end].splitlines():
        if line.strip():
            done.add(json.loads(line)["id"])
    return done


@torch.no_grad()
def score(args):
    root, config, fit, out = (Path(p).resolve() for p in (args.data, args.config, args.fit, args.out))
    out.mkdir(exist_ok=True)
    digests, report, binding, saved = exporter.completed_fit(root, config, fit, args.init)
    device = torch.device(args.device)
    bundle.demand(args.precision == "fp32" or device.type == "cuda",
                  "CPU scoring must use FP32 (bf16 autocast on CPU is pathologically slow)")
    protocol = trainer.validate_protocol(bundle.read_json(config / "protocol.json"))
    trainer.configure(device, protocol["cuda_memory_fraction"])
    model, tokenizer, contract, protocol, arm, profiles = exporter.restore_model(args, root, config, binding,
                                                                                saved, device)
    tokenize = full_a.hf_tokenize(tokenizer)
    special = set(tokenizer.all_special_ids)
    scores_path = out / "scores.jsonl"
    pool = Path(args.pool).resolve(strict=True)
    pool_sha = bundle.sha256_file(pool)
    sample = None if not args.sample_modulus else (args.sample_seed, args.sample_keep, args.sample_modulus)
    bundle.demand(sample is None or 0 < args.sample_keep <= args.sample_modulus, "sample fraction")
    budget = args.token_budget
    resume_binding = score_binding(fit, report, digests, pool_sha, sample, args.precision, budget)
    bind(out, resume_binding)
    done = completed_ids(scores_path)
    started, skipped, scored = time.monotonic(), 0, 0

    def flush(batch, f):
        nonlocal scored
        n = max(len(b[1]) for b in batch)
        ids = torch.full((len(batch), n), tokenizer.pad_token_id, dtype=torch.long)
        mask = torch.zeros((len(batch), n), dtype=torch.long)
        content = torch.zeros((len(batch), n), dtype=torch.bool)
        words = torch.zeros((len(batch), n), dtype=torch.bool)
        for j, (_, t, c, w) in enumerate(batch):
            ids[j, :len(t)] = torch.tensor(t)
            mask[j, :len(t)] = 1
            content[j, :len(t)] = torch.tensor(c)
            words[j, :len(t)] = torch.tensor(w)
        o = model(ids.to(device), mask.to(device), content.to(device), autocast=args.precision == "bf16",
                  words=words.to(device))
        p = o["origin"][:, 1:].sum(-1).double().tolist()
        for (r, t, _, _), v in zip(batch, p):
            f.write(bridge.canonical({"id": r["id"], "text_sha256": r["text_sha256"],
                                      "source_group": r["source_group"], "domain": r.get("domain"),
                                      "tokens": len(t), "p_any_ai": v}) + b"\n")
            scored += 1
        f.flush()

    with scores_path.open("ab") as f:
        batch = []
        for r, text in pool_rows(pool, sample):
            if r["id"] in done:
                continue
            try:
                t, offsets = tokenize(text)
            except ValueError:
                skipped += 1
                continue
            c = full_a.content_mask_for(t, special)
            w = full_a.word_mask_for(text, t, offsets, special)
            width = max([len(b[1]) for b in batch] + [len(t)])
            if batch and width * (len(batch) + 1) > budget:
                flush(batch, f)
                batch = []
            batch.append((r, t, c, w))
            if args.limit and scored + len(batch) >= args.limit:
                break
        if batch:
            flush(batch, f)
    bridge.write_json(out / "manifest.json", {"schema": SCORE_SCHEMA, "complete": not args.limit,
        "binding": resume_binding, "binding_sha256": bridge.sha256(out / "binding.json"),
        "pool": str(pool), "pool_sha256": pool_sha, "pool_split": POOL_SPLIT, "fit_sha256": bridge.sha256(fit / "fit.json"),
        "selected_checkpoint_sha256": report["selected_checkpoint_sha256"], "arm": binding["arm"],
        "seed": binding["seed"], "bundle_sha256": digests, "scores_sha256": bridge.sha256(scores_path),
        "rows_scored_this_invocation": scored, "rows_previously_scored": len(done),
        "skipped_overlength_or_empty": skipped, "precision": args.precision, "padded_token_budget": budget,
        "sample": None if not args.sample_modulus else {"rule": "int(sha256(seed:id)[:16],16) % modulus < keep",
                                                        "seed": args.sample_seed, "keep": args.sample_keep,
                                                        "modulus": args.sample_modulus},
        "score": "uncalibrated P(model_only)+P(mixed) of the selected checkpoint",
        "implementation_sha256": bridge.sha256(__file__), "seconds": time.monotonic() - started,
        "final_test_opened": False, "calibration_or_development_scored": False})
    print(json.dumps({"complete": True, "scored": scored, "skipped": skipped}), flush=True)


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    for key in ("data", "config", "fit", "artifact", "init", "pool", "out"):
        p.add_argument("--" + key, required=True)
    p.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
    p.add_argument("--precision", choices=("fp32", "bf16"), default="fp32")
    p.add_argument("--token-budget", type=int, default=16384)
    p.add_argument("--limit", type=int, default=0, help="score at most this many rows (smoke only)")
    p.add_argument("--sample-seed", default="slop_ninja_a_mining_v1")
    p.add_argument("--sample-keep", type=int, default=0)
    p.add_argument("--sample-modulus", type=int, default=0)
    score(p.parse_args())

"""E0 control (protocol arms.E0): the stage-3 final-block recipe (train_last_block_a.py)
fitted on the new Train, i.e. a full-A v2 trainer bundle with its own labels and
its assembler row weights.

Kept from the stage-3 recipe: V6 layers 0..20, final norm and classifier frozen;
layer 21 and the shared heads (WarmStartA, text mode, zero CFG) trained with
AdamW (heads 3e-4, final block 1e-5, weight decay 0.01), gradient clip 5.0,
logical batches of 64 drawn by torch.randperm with the seed's generator, one
unpadded FP32 document at a time with the batch objective scaled by
len(Train)/len(batch), the stage-3 joint objective (origin cross-entropy plus the
squared involvement term, squared share error, 0.25 x attribution
cross-entropy) with each task's weight divided by its Train weight total,
evaluation at epoch 0 and after every epoch, the best epoch kept (strictly
lower score, so the earliest on an exact tie), the initial-A-equals-V6 gate,
deterministic CUDA with TF32 off and memory fraction 0.50.

Changed by the protocol and the new Train (recorded in E0-RECIPE.json):
- at most 4 epochs (fit_rules.max_epochs), not 12;
- selection on Dev-iter with the protocol metric (uncalibrated balanced
  involvement Brier, family/origin balanced Dev-iter weights);
- Train weights are the bundle's assembler_row_weight_v1 weights under the
  release protocols' train_weighting, not stage-3 family-origin balance;
- no human head: the new Train's human roster is empty (as in every release
  arm), so the attribution term covers the AI roster only;
- the per-epoch Train-population evaluation (a stage-3 diagnostic) is not run;
- budget (arms.budget_rule): a seed stops before an epoch that would cross the arm's deadline and keeps its
  best completed epoch (budget-limited); its first training epoch is estimated from the arm's other seeds,
  and a seed that cannot complete one training epoch is not fitted (not-fitted.json, no fit.json);
- prefixes (exact FP32 layer-21 inputs, unpadded) are computed from the full-A
  tokenization and cached on disk only while the filesystem keeps the
  configured free space; otherwise computed on the fly. Both give the same
  activations.

Run with the release trainer modules on PYTHONPATH (grammar-eval/python at the
release commit). Subcommands:
  fit  --data BUNDLE --contract C --recipe E0-RECIPE.json --artifact A --init I --cache DIR --out FIT --seed S
       [--deadline-unix T]
  raw  --data BUNDLE --contract C --recipe R --artifact A --init I --fit FIT --part cal --out RAW
       (full-text forward; first replays Dev-iter through both the full-text and the prefix path and
        recomputes the fit's selected Dev-iter Brier)
"""
from __future__ import annotations
import argparse
import json
import math
import os
from pathlib import Path
import shutil
import time

import torch
from torch.nn import functional as F
from safetensors.torch import load_file, save_file

import full_a, full_a_bundle as bundle, shared_a as shared, train_full_a as trainer, v6_embeddings as bridge
import last_block_a as architecture
import warm_start_a
from warm_start_a import WarmStartA, initializer

RECIPE_SCHEMA = "slop_ninja_a_comp_e0_recipe_v1"
FIT_SCHEMA = "slop_ninja_a_comp_e0_fit_v1"
LOGIT_SEMANTICS = "log(p_model+p_mixed)-log(p_human) of the final uncalibrated origin"
SELECTION_SPLIT = "dev_iter"


def read(p):
    return json.loads(Path(p).read_bytes())


def validate_recipe(r):
    shared.demand(r["schema"] == RECIPE_SCHEMA, "E0 recipe schema")
    f = r["fit"]
    shared.demand(f["seeds"] == [17, 29, 41] and type(f["max_epochs"]) is int and 1 <= f["max_epochs"] <= 4 and
                  f["logical_batch_size"] == 64 and f["heads_learning_rate"] == .0003 and
                  f["adapted_final_block_learning_rate"] == .00001 and f["weight_decay"] == .01 and
                  f["gradient_clip_norm"] == 5.0 and f["attribution_loss_weight"] == .25 and
                  f["cuda_memory_fraction"] == .50 and f["selection_split"] == SELECTION_SPLIT,
                  "E0 recipe differs from the stage-3 schedule under the protocol epoch cap")
    shared.demand(f["train_weighting"]["scheme"] == "assembler_row_weight_v1", "new-Train row weights required")
    shared.demand(type(r["prefix_cache"]["min_free_bytes"]) is int and r["prefix_cache"]["min_free_bytes"] > 0,
                  "prefix cache free-space floor")
    return r


class Objective:
    """Stage-3 joint objective under arbitrary row weights (no human head)."""

    def __init__(self, meta, weights, ai_index, role_weight):
        self.meta, self.weights, self.ai_index, self.role_weight = meta, list(weights), ai_index, role_weight
        self.den = {"origin": 0., "share": 0., "ai": 0.}
        for m, w in zip(meta, self.weights):
            self.den["origin"] += w
            self.den["share"] += w
            if m.label is not None and m.label in ai_index:
                self.den["ai"] += w
        shared.demand(self.den["origin"] > 0, "origin supervision required")

    def __call__(self, out, i):
        m, w = self.meta[i], self.weights[i]
        y = torch.tensor([m.origin], dtype=torch.long, device=out["origin"].device)
        loss = (F.cross_entropy(out["origin_logits"], y) + (out["origin"][0, 1:].sum() - float(m.origin != 0)).square()) \
            * (w / self.den["origin"])
        loss = loss + (out["share"][0] - m.share).square() * (w / self.den["share"])
        if m.label is not None and m.label in self.ai_index and self.den["ai"] > 0:
            t = torch.tensor([self.ai_index[m.label]], dtype=torch.long, device=out["origin"].device)
            loss = loss + self.role_weight * F.cross_entropy(out["ai_logits"], t) * (w / self.den["ai"])
        return loss


def balanced_brier(involvements, population):
    """Protocol selection metric: uncalibrated balanced involvement Brier over the
    population's family/origin balanced weights."""
    total = 0.
    for v, m, w in zip(involvements, population.meta, population.weights):
        total += w / population.denominators["origin"] * (v - float(m.origin != 0)) ** 2
    return total


def better(score, best):
    """Strictly lower wins, so the earliest epoch keeps an exact tie."""
    return score < best


class Prefixes:
    """Exact FP32 layer-21 inputs of one population, cached under ROOT/<split>/<id-digest>.safetensors
    while the filesystem keeps at least min_free bytes free; otherwise recomputed."""

    def __init__(self, encoder, population, device, pad_id, root, min_free, split):
        self.encoder, self.population, self.device, self.pad_id = encoder, population, device, pad_id
        self.dir = Path(root) / split
        self.dir.mkdir(parents=True, exist_ok=True)
        self.min_free = min_free
        self.stats = {"computed": 0, "cache_hits": 0, "cached": 0, "not_cached_low_space": 0}

    def path(self, i):
        m = self.population.meta[i]
        return self.dir / (bridge.digest(f"{m.id}\0{m.text_sha256}".encode()) + ".safetensors")

    def ids(self, i):
        ids, _, _, _, _ = self.population.batch([i], self.device, self.pad_id)
        return ids

    def get(self, i):
        p = self.path(i)
        if p.exists():
            v = load_file(str(p), device=str(self.device))
            n = int(self.population.lengths[i])
            shared.demand(set(v) == {"prefix", "frozen_encoding"} and v["prefix"].shape == (1, n, 768) and
                          v["prefix"].dtype == torch.float32, "cached prefix shape")
            self.stats["cache_hits"] += 1
            return v
        with torch.no_grad():
            prefix, h = architecture.capture_prefix(self.encoder, self.ids(i))
        v = {"prefix": prefix.contiguous(), "frozen_encoding": h.contiguous()}
        self.stats["computed"] += 1
        if shutil.disk_usage(self.dir).free - 4 * prefix.numel() > self.min_free:
            tmp = p.with_suffix(".tmp")
            save_file({k: t.cpu() for k, t in v.items()}, str(tmp))
            os.replace(tmp, p)
            self.stats["cached"] += 1
        else:
            self.stats["not_cached_low_space"] += 1
        return v


def sibling_epoch_seconds(out):
    """Budget estimate for a seed's first training epoch: the longest training epoch after the first
    of the arm's other seeds (prefix cache warm), else their longest training epoch; None if none ran."""
    late, any_ = [], []
    for h in Path(out).parent.glob("*/history.json"):
        if h.parent == Path(out):
            continue
        for r in json.loads(h.read_text()):
            if r["epoch"] > 0:
                any_.append(r["epoch_seconds"])
                if r["epoch"] > 1:
                    late.append(r["epoch_seconds"])
    return max(late) if late else (max(any_) if any_ else None)


def load_model(args, contract, device, init):
    encoder, tokenizer, parent, _ = architecture.load_encoder(args.artifact, device)
    shared.demand(parent["artifact_id"] == contract["parent_artifact_id"], "encoder parent changed")
    norm = contract["embedding_normalization"]
    shared.demand(contract["human_profiles"] == [], "the new Train has an empty human roster")
    heads = WarmStartA(init, norm["mean"], norm["scale"], 0, len(contract["ai_profiles"]), "text").to(device)
    return encoder, tokenizer, parent, heads


def state(joint):
    return {"last_layer": {k: v.detach().cpu().clone() for k, v in joint.encoder.model.layers[21].state_dict().items()},
            "heads": {k: v.detach().cpu().clone() for k, v in joint.heads.state_dict().items()}}


def restore(joint, saved):
    joint.encoder.model.layers[21].load_state_dict(saved["last_layer"], strict=True)
    joint.heads.load_state_dict(saved["heads"], strict=True)


def atomic_checkpoint(path, value):
    tmp = path.with_suffix(".tmp")
    with tmp.open("wb") as f:
        torch.save(value, f)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


@torch.no_grad()
def evaluate(joint, prefixes, population):
    joint.eval()
    inv = []
    for i in range(len(population)):
        out = joint.from_prefix(prefixes.get(i)["prefix"])
        inv.append(float(out["origin"][0, 1:].sum()))
    return balanced_brier(inv, population), inv


def common(args):
    root = Path(args.data).resolve(strict=True)
    digests = bundle.bundle_digests(root)
    contract = trainer.validate_contract(read(args.contract), digests[bundle.MANIFEST])
    recipe = validate_recipe(read(args.recipe))
    device = torch.device(args.device)
    architecture.configure(device)  # deterministic, TF32 off, CUDA memory fraction 0.50 (stage 3)
    return root, digests, contract, recipe, device


def fit(args):
    root, digests, contract, recipe, device = common(args)
    f = recipe["fit"]
    shared.demand(args.seed in f["seeds"], "seed outside the recipe")
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    torch.manual_seed(args.seed)
    init = initializer(args.init)
    encoder, tokenizer, parent, heads = load_model(args, contract, device, init)
    joint = architecture.LastBlockA(encoder, heads, True)
    ai = list(contract["ai_profiles"])
    ai_index = {k: i for i, k in enumerate(ai)}
    tokenize, special, pad = full_a.hf_tokenize(tokenizer), set(tokenizer.all_special_ids), tokenizer.pad_token_id
    train = bundle.Population(root, "train", tokenize, special, ai, cohort_weighted=True, weighting=f["train_weighting"])
    dev = bundle.Population(root, SELECTION_SPLIT, tokenize, special, ai, cohort_weighted=False)
    trainer.disjoint(train, dev)
    shared.demand(bundle.ai_roster(root) == ai, "AI roster differs from the Train generator labels")
    weights_doc = read(root / bundle.WEIGHTS)
    weight_log = bundle.finish_weight_log(bundle.validate_weights(weights_doc, train.meta, train.dropped,
                                          scheme=f["train_weighting"]["scheme"]), train, weights_doc.get("overrides"))
    objective = Objective(train.meta, train.weights, ai_index, f["attribution_loss_weight"])
    cache_min = recipe["prefix_cache"]["min_free_bytes"]
    tp = Prefixes(encoder, train, device, pad, args.cache, cache_min, "train")
    dp = Prefixes(encoder, dev, device, pad, args.cache, cache_min, SELECTION_SPLIT)
    optimizer = torch.optim.AdamW([{"params": heads.parameters(), "lr": f["heads_learning_rate"]},
                                   {"params": encoder.model.layers[21].parameters(),
                                    "lr": f["adapted_final_block_learning_rate"]}], weight_decay=f["weight_decay"])
    order_rng = torch.Generator().manual_seed(args.seed)
    binding = {"schema": "slop_ninja_a_comp_e0_binding_v1", "seed": args.seed, "bundle_sha256": digests,
               "contract_sha256": bridge.sha256(args.contract), "recipe_sha256": bridge.sha256(args.recipe),
               "initializer_sha256": bridge.sha256(args.init), "parent_artifact_id": parent["artifact_id"],
               "trainer_sha256": bridge.sha256(__file__), "architecture_sha256": bridge.sha256(architecture.__file__),
               "bundle_reader_sha256": bridge.sha256(bundle.__file__), "model_sha256": bridge.sha256(full_a.__file__),
               "shared_heads_implementation_sha256": bridge.sha256(warm_start_a.__file__),
               "artifact_loader_sha256": bridge.sha256(bridge.__file__),
               "transformers_model_source_sha256": bridge.sha256(__import__(encoder.__module__, fromlist=["x"]).__file__),
               "torch": torch.__version__, "cuda": torch.version.cuda, "device": str(device),
               "train_rows": len(train), "dev_iter_rows": len(dev), "dev_iter_rows_sha256": dev.rows_digest()}
    recovery = out / "recovery.pt"
    best, best_epoch, best_state, history, first = float("inf"), 0, None, [], 0
    if recovery.exists():
        saved = torch.load(recovery, map_location="cpu", weights_only=False)
        shared.demand(saved["binding"] == binding, "resume binding differs")
        restore(joint, saved["current"])
        optimizer.load_state_dict(saved["optimizer"])
        order_rng.set_state(saved["order_rng"])
        torch.set_rng_state(saved["torch_rng"])
        if device.type == "cuda":
            torch.cuda.set_rng_state_all(saved["cuda_rng"])
        best, best_epoch, best_state, history = saved["best_score"], saved["best_epoch"], saved["best"], saved["history"]
        first = saved["epoch"] + 1
    else:
        shared.demand(not (out / "binding.json").exists(), "incomplete first epoch; use a fresh output directory")
        bridge.write_json(out / "binding.json", binding)
        with torch.no_grad():
            for i in range(min(4, len(train))):
                v = tp.get(i)
                actual = joint.from_prefix(v["prefix"])["origin"]
                expected = (encoder.classifier(v["frozen_encoding"]) / heads.base_temperature).softmax(-1)
                shared.demand(float((actual - expected).abs().max()) <= 1e-6, "initial A differs from V6")
    started = time.monotonic()
    budget_limited = False
    for epoch in range(first, f["max_epochs"] + 1):
        t0 = time.monotonic()
        if epoch:
            last = history[-1]["epoch_seconds"] if history and history[-1]["epoch"] > 0 else sibling_epoch_seconds(out)
            if args.deadline_unix and last and time.time() + last > args.deadline_unix:
                budget_limited = True
                break
            joint.train()
            order = torch.randperm(len(train), generator=order_rng).tolist()
            n = len(order)
            for begin in range(0, n, f["logical_batch_size"]):
                indices = order[begin:begin + f["logical_batch_size"]]
                optimizer.zero_grad(set_to_none=True)
                for i in indices:
                    o = joint.from_prefix(tp.get(i)["prefix"])
                    term = objective(o, i) * (n / len(indices))
                    shared.demand(bool(torch.isfinite(term)), "nonfinite loss")
                    term.backward()
                torch.nn.utils.clip_grad_norm_([p for p in joint.parameters() if p.requires_grad],
                                               f["gradient_clip_norm"], error_if_nonfinite=True)
                optimizer.step()
        score, _ = evaluate(joint, dp, dev)
        shared.demand(math.isfinite(score), "nonfinite Dev-iter score")
        if better(score, best):
            best, best_epoch, best_state = score, epoch, state(joint)
        row = {"epoch": epoch, "dev_iter_uncalibrated_balanced_involvement_brier": score,
               "epoch_seconds": time.monotonic() - t0, "elapsed_seconds_this_invocation": time.monotonic() - started,
               "train_prefixes": dict(tp.stats), "dev_iter_prefixes": dict(dp.stats)}
        history.append(row)
        atomic_checkpoint(recovery, {"binding": binding, "epoch": epoch, "current": state(joint),
                                     "optimizer": optimizer.state_dict(), "order_rng": order_rng.get_state(),
                                     "torch_rng": torch.get_rng_state(),
                                     "cuda_rng": torch.cuda.get_rng_state_all() if device.type == "cuda" else [],
                                     "best_score": best, "best_epoch": best_epoch, "best": best_state, "history": history})
        bridge.write_json(out / "history.json", history)
        print(json.dumps(row), flush=True)
    if budget_limited and max(r["epoch"] for r in history) == 0:
        # No training epoch fits in the remaining budget: the seed is not fitted (no fit.json).
        bridge.write_json(out / "not-fitted.json", {"schema": "slop_ninja_a_comp_e0_not_fitted_v1", "seed": args.seed,
            "reason": "the arm's GPU-hour budget ends before one training epoch can complete", "binding": binding,
            "deadline_unix": args.deadline_unix, "epoch_estimate_seconds": sibling_epoch_seconds(out)})
        if recovery.exists():
            recovery.unlink()
        return
    restore(joint, best_state)
    atomic_checkpoint(out / "selected.pt", {"binding": binding, "epoch": best_epoch, "state": best_state})
    save_file(best_state["last_layer"], str(out / "last-layer.safetensors"))
    save_file(best_state["heads"], str(out / "shared-heads.safetensors"))
    completed = max(r["epoch"] for r in history)
    bridge.write_json(out / "fit.json", {"schema": FIT_SCHEMA, "complete": not budget_limited, "binding": binding,
        "max_epochs": f["max_epochs"], "epochs_completed": completed, "budget_limited": budget_limited,
        "selected_epoch": best_epoch, "dev_iter_uncalibrated_balanced_involvement_brier": best,
        "selection_rule": "lowest uncalibrated balanced Dev-iter Brier over epochs 0..max, earliest on exact tie",
        "trained_selected_heads": {"origin": best_epoch > 0, "share": best_epoch > 0, "ai": best_epoch > 0, "human": False},
        "cohort_weight_log": weight_log, "final_test_opened": False, "calibrated": False,
        "selected_checkpoint_sha256": bridge.sha256(out / "selected.pt"),
        "last_layer_sha256": bridge.sha256(out / "last-layer.safetensors"),
        "shared_heads_sha256": bridge.sha256(out / "shared-heads.safetensors"),
        "peak_cuda_allocated_bytes": torch.cuda.max_memory_allocated() if device.type == "cuda" else None,
        "limitations": ["Dev-iter selection evidence only; CAL calibration and Test follow the protocol.",
                        "Epoch 0 share and attribution outputs are untrained diagnostics."]})
    if recovery.exists():
        recovery.unlink()


def raw(args):
    root, digests, contract, recipe, device = common(args)
    shared.demand(args.part == "cal", "only CAL is scored here; Test is never read")
    fitdir, out = Path(args.fit).resolve(strict=True), Path(args.out)
    shared.demand(not out.exists(), "new output required")
    report = read(fitdir / "fit.json")
    shared.demand(report["schema"] == FIT_SCHEMA and report["final_test_opened"] is False and
                  report["binding"]["bundle_sha256"] == digests and
                  report["binding"]["contract_sha256"] == bridge.sha256(args.contract) and
                  report["binding"]["recipe_sha256"] == bridge.sha256(args.recipe), "E0 fit of this bundle and recipe")
    for name, key in (("last-layer.safetensors", "last_layer_sha256"), ("shared-heads.safetensors", "shared_heads_sha256"),
                      ("selected.pt", "selected_checkpoint_sha256")):
        shared.demand(bridge.sha256(fitdir / name) == report[key], f"{name} differs from fit.json")
    init = initializer(args.init)
    encoder, tokenizer, parent, heads = load_model(args, contract, device, init)
    encoder.model.layers[21].load_state_dict(load_file(str(fitdir / "last-layer.safetensors")), strict=True)
    heads.load_state_dict(load_file(str(fitdir / "shared-heads.safetensors")), strict=True)
    joint = architecture.LastBlockA(encoder, heads, True).eval()
    model = full_a.FullA(encoder, heads, None, "head", None).to(device).eval()
    model.token_loss_weight = 0.
    ai = list(contract["ai_profiles"])
    tokenize, special, pad = full_a.hf_tokenize(tokenizer), set(tokenizer.all_special_ids), tokenizer.pad_token_id
    # Replay: Dev-iter through the full-text and the prefix path; the selected Dev-iter score must reproduce.
    dev = bundle.Population(root, SELECTION_SPLIT, tokenize, special, ai, cohort_weighted=False)
    worst, inv = 0., []
    with torch.no_grad():
        for i in range(len(dev)):
            ids, mask, content, _, words = dev.batch([i], device, pad)
            o = model(ids, mask, content, autocast=False, words=words)
            prefix, _ = architecture.capture_prefix(encoder, ids)
            q = joint.from_prefix(prefix)
            worst = max(worst, float((o["origin"] - q["origin"]).abs().max()), float((o["ai_logits"] - q["ai_logits"]).abs().max()))
            inv.append(float(q["origin"][0, 1:].sum()))
    replay_score = balanced_brier(inv, dev)
    shared.demand(worst <= 1e-5, f"full-text and prefix paths differ by {worst}")
    shared.demand(abs(replay_score - report["dev_iter_uncalibrated_balanced_involvement_brier"]) <= 1e-9,
                  "selected Dev-iter score does not reproduce")
    population = bundle.Population(root, args.part, tokenize, special, ai, cohort_weighted=False)
    out.mkdir(parents=True)
    with torch.no_grad(), (out / "raw.jsonl").open("xb") as fr, (out / "predictions.jsonl").open("xb") as fp:
        for i, meta in enumerate(population.meta):
            ids, mask, content, _, words = population.batch([i], device, pad)
            o = model(ids, mask, content, autocast=False, words=words)
            fr.write(bridge.canonical(trainer.raw_prediction(meta, o, population.lengths[i])) + b"\n")
            fp.write(bridge.canonical(trainer.contract_prediction(meta, o, ai)) + b"\n")
    bridge.write_json(out / "manifest.json", {"schema": "slop_ninja_a_comp_fixed_raw_v1", "complete": True,
        "partition": args.part, "rows": len(population), "system": "E0 (stage-3 final-block recipe on the new Train)",
        "seed": report["binding"]["seed"], "bundle_sha256": digests, "e0_fit_sha256": bridge.sha256(fitdir / "fit.json"),
        "selected_checkpoint_sha256": report["selected_checkpoint_sha256"], "selected_epoch": report["selected_epoch"],
        "last_layer_sha256": report["last_layer_sha256"], "shared_heads_sha256": report["shared_heads_sha256"],
        "contract_sha256": bridge.sha256(args.contract), "recipe_sha256": bridge.sha256(args.recipe),
        "init_sha256": bridge.sha256(args.init), "parent_artifact_id": parent["artifact_id"],
        "ai_profiles": ai, "human_profiles": [], "trained_selected_heads": report["trained_selected_heads"],
        "share_mode": "head", "involvement_logit_semantics": LOGIT_SEMANTICS,
        "origin_logits_semantics": "original_classifier_linear_divided_by_base_temperature; no CAL map",
        "dev_iter_replay": {"full_text_vs_prefix_max_abs_error": worst, "selected_score": replay_score},
        "unrostered_generator_labels": dict(population.unrostered),
        "predictions_sha256": bridge.sha256(out / "predictions.jsonl"), "raw_sha256": bridge.sha256(out / "raw.jsonl"),
        "implementation_sha256": bridge.sha256(__file__), "final_test_opened": False, "calibrated": False,
        "device": str(device), "torch": torch.__version__})
    print(json.dumps({"complete": True, "partition": args.part, "rows": len(population),
                      "seed": report["binding"]["seed"], "replay_max_abs_error": worst}), flush=True)


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="cmd", required=True)
    for name in ("fit", "raw"):
        s = sub.add_parser(name)
        for key in ("data", "contract", "recipe", "artifact", "init", "out"):
            s.add_argument("--" + key, required=True)
        s.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
    sub.choices["fit"].add_argument("--cache", required=True)
    sub.choices["fit"].add_argument("--seed", type=int, required=True)
    sub.choices["fit"].add_argument("--deadline-unix", type=float, default=None)
    sub.choices["raw"].add_argument("--fit", required=True)
    sub.choices["raw"].add_argument("--part", required=True)
    a = p.parse_args()
    fit(a) if a.cmd == "fit" else raw(a)

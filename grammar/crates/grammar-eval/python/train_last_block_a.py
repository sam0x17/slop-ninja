"""Matched final-block/frozen A fitting on a frozen Rust-prepared bundle.

Only model training lives here. No corpus discovery, source splitting, vocabulary
or normalization fitting, detector calls, calibration, or Test input. Input rows
carry routing/supervision beside a hash-bound safetensors prefix. Only the prefix
enters the learned encoder. Global task-mask denominators preserve masked loss
weighting while gradients accumulate one unpadded document at a time.
"""
from __future__ import annotations
import argparse
from collections import OrderedDict
import fcntl
import json
import math
import os
from pathlib import Path
import time

import torch
from torch.nn import functional as F
from safetensors.torch import load_file, save_file
import last_block_a as architecture
import shared_a as shared
import v6_embeddings as bridge
import warm_start_a
from warm_start_a import WarmStartA, initializer


def read(p):
    return json.loads(Path(p).read_bytes())


def atomic_checkpoint(path, value):
    temporary = path.with_suffix(".tmp")
    with temporary.open("wb") as f:
        torch.save(value, f)
        f.flush()
        os.fsync(f.fileno())
    os.replace(temporary, path)


class Population:
    def __init__(self, rows, profiles):
        self.rows = rows
        shared.demand(rows and all(r["purpose"] == "query" for r in rows), "query population required")
        self.weights = shared.query_weights(rows, "family_origin_balanced")
        self.denominators = {k: 0.0 for k in ("origin", "share", "human", "ai")}
        self.cache = OrderedDict()
        for r, weight in zip(rows, self.weights):
            targets = r["targets"]
            for k in self.denominators:
                t = targets.get(k)
                if t is not None:
                    if k == "origin":
                        shared.demand(type(t) is int and 0 <= t < 3, "invalid origin target")
                    elif k == "share":
                        shared.demand(type(t) in (int, float) and math.isfinite(t) and 0 <= t <= 1,
                                      "invalid share target")
                    else:
                        label = shared.role_label(r, k, profiles[k])
                        shared.demand(type(t) is int and 0 <= t < len(profiles[k]) and
                                      label["present"] is True and label["known"] and
                                      profiles[k][t] == label["identity"], "unbound conditional role")
                    self.denominators[k] += weight
            p = Path(r["prefix"]["path"])
            shared.demand(p.suffix == ".safetensors" and p.is_file() and
                          bridge.sha256(p) == r["prefix"]["sha256"], "prefix file binding differs")
        shared.demand(self.denominators["origin"] > 0, "origin supervision required")

    def tensors(self, index, device):
        if index not in self.cache:
            r = self.rows[index]
            values = load_file(r["prefix"]["path"], device="cpu")
            shared.demand(set(values) == {"prefix", "frozen_encoding"}, "unexpected prefix tensors")
            p, h = values["prefix"], values["frozen_encoding"]
            shared.demand(p.dtype == h.dtype == torch.float32 and p.ndim == 3 and
                          p.shape[0] == 1 and p.shape[2] == 768 and 0 < p.shape[1] <= 2048 and
                          h.shape == (1, 768) and bool(torch.isfinite(p).all()) and
                          bool(torch.isfinite(h).all()), "invalid prefix shape/value")
            self.cache[index] = values
            if len(self.cache) > 128:
                self.cache.popitem(last=False)
        self.cache.move_to_end(index)
        return {k: v.to(device) for k, v in self.cache[index].items()}

    def objective(self, out, index):
        targets, w = self.rows[index]["targets"], self.weights[index]
        loss = out["encoding"].sum() * 0
        for key in self.denominators:
            y = targets.get(key)
            if y is None:
                continue
            if key == "origin":
                target = torch.tensor([y], dtype=torch.long, device=out["origin"].device)
                value = F.cross_entropy(out["origin_logits"], target) + (out["origin"][0, 1:].sum() - float(y != 0)).square()
            elif key == "share":
                value = (out["share"][0] - y).square()
            else:
                target = torch.tensor([y], dtype=torch.long, device=out["origin"].device)
                value = .25 * F.cross_entropy(out[f"{key}_logits"], target)
            loss = loss + value * (w / self.denominators[key])
        return loss


def forward(joint, tensors):
    if joint.adapt:
        return joint.from_prefix(tensors["prefix"])
    h = tensors["frozen_encoding"]
    return joint.heads(torch.cat((h, h.new_zeros((1, 1820))), 1))


@torch.no_grad()
def evaluate(joint, population, device, profiles, export=False):
    joint.eval()
    score, loss, predictions = 0.0, 0.0, []
    for i, (r, w) in enumerate(zip(population.rows, population.weights)):
        out = forward(joint, population.tensors(i, device))
        y = r["targets"].get("origin")
        if y is not None:
            score += w / population.denominators["origin"] * float((out["origin"][0, 1:].sum() - float(y != 0)).square())
        loss += float(population.objective(out, i))
        if export:
            p = out["origin"][0].cpu().tolist()
            p = [x / sum(p) for x in p]
            q = float(out["conditional_mixed_share"][0])
            share = p[1] + p[2] * q
            shared.demand(0 <= p[1] <= share <= p[1] + p[2] <= 1.000001, "output bounds")
            # Early Development output is explicitly uncalibrated. Named score
            # arrays are diagnostic; CAL must qualify acceptance thresholds.
            roles = {}
            for role, involvement in (("human", p[0] + p[2]), ("ai", p[1] + p[2])):
                values = out[f"{role}_logits"][0].softmax(-1).cpu().tolist()
                roles[role] = {"profile": f"other_{role}", "involvement_probability": involvement,
                               "conditional_named_scores": dict(zip(profiles[role], values)),
                               "acceptance_calibrated": False, "fallback_asserts_presence": False}
            predictions.append({"id": r["id"], "origin": dict(zip(("human_only", "model_only", "mixed"), p)),
                                "ai_confidence_percent": 100 * (p[1] + p[2]), "ai_share_percent": 100 * share,
                                "conditional_mixed_share": q, "attribution": roles,
                                "encoding": out["encoding"][0].cpu().tolist(), "calibrated": False})
    return score, loss, predictions


def state(joint):
    return {"last_layer": {k: v.detach().cpu().clone() for k, v in joint.encoder.model.layers[21].state_dict().items()},
            "heads": {k: v.detach().cpu().clone() for k, v in joint.heads.state_dict().items()}}


def restore(joint, saved):
    joint.encoder.model.layers[21].load_state_dict(saved["last_layer"], strict=True)
    joint.heads.load_state_dict(saved["heads"], strict=True)


def run(args):
    root, output = Path(args.data).resolve(strict=True), Path(args.out)
    output.mkdir(exist_ok=True)
    with (output / ".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        fit(args, root, output)


def fit(args, root, output):
    manifest = read(root / "manifest.json")
    shared.demand(manifest["schema"] == "slop_ninja_last_block_a_dataset_v1" and manifest["complete"] is True
                  and manifest["final_test_opened"] is False, "qualified non-Test fitting bundle required")
    contract, protocol = read(root / "contract.json"), read(root / "protocol.json")
    shared.demand(bridge.sha256(root / "contract.json") == manifest["contract_sha256"] and
                  bridge.sha256(root / "protocol.json") == manifest["protocol_sha256"], "contract/protocol changed")
    schedule = protocol["fit"]
    shared.demand(schedule["seeds"] == [17, 29, 41] and schedule["epochs"] == 12 and
                  schedule["logical_batch_size"] == 64 and schedule["heads_learning_rate"] == .0003 and
                  schedule["adapted_final_block_learning_rate"] == .00001 and
                  schedule["weight_decay"] == .01 and schedule["gradient_clip_norm"] == 5.0,
                  "unsupported frozen comparison schedule")
    shared.demand(args.seed in schedule["seeds"], "seed outside frozen schedule")
    profiles = {k: contract[k + "_profiles"] for k in ("human", "ai")}
    for p in profiles.values():
        shared.demand(len(p) >= 2 and len(set(p)) == len(p), "attribution needs multiple unique profiles")
    norm = contract["embedding_normalization"]
    shared.demand(norm["fit"] == "train_query_only_equal_source_family_weight" and
                  norm["calibration_used_for_fit"] is False and norm["development_used_for_fit"] is False
                  and norm["reference_used_for_fit"] is False and norm["final_test_opened"] is False,
                  "normalization must be Train-only")
    device = torch.device(args.device)
    architecture.configure(device)
    torch.manual_seed(args.seed)
    encoder, _, parent, _ = architecture.load_encoder(args.artifact, device)
    shared.demand(contract["parent_artifact_id"] == parent["artifact_id"], "encoder parent changed")
    heads = WarmStartA(initializer(args.init), norm["mean"], norm["scale"],
                       len(profiles["human"]), len(profiles["ai"]), "text").to(device)
    joint = architecture.LastBlockA(encoder, heads, args.condition == "adapt_last_block")
    populations = {}
    for part in ("train", "development"):
        file = root / (part + ".jsonl")
        shared.demand(bridge.sha256(file) == manifest["files_sha256"][file.name], "split file changed")
        rows = [json.loads(line) for line in file.read_bytes().splitlines()]
        shared.demand(all(r["frozen_split"] == part and r["purpose"] == "query" for r in rows), "wrong split/purpose")
        populations[part] = Population(rows, profiles)
    train, dev = populations["train"], populations["development"]
    shared.demand(not {r["source_group"] for r in train.rows} & {r["source_group"] for r in dev.rows}, "family leakage")
    all_ids = [r["id"] for pop in populations.values() for r in pop.rows]
    shared.demand(len(set(all_ids)) == len(all_ids), "duplicate training/development id")
    for role in profiles:
        shared.demand({r["targets"].get(role) for r in train.rows} - {None} == set(range(len(profiles[role]))),
                      "profile roster not established by Train supervision")
    groups = [{"params": heads.parameters(), "lr": .0003}]
    if joint.adapt:
        groups.append({"params": encoder.model.layers[21].parameters(), "lr": .00001})
    optimizer = torch.optim.AdamW(groups, weight_decay=.01)
    order_rng = torch.Generator().manual_seed(args.seed)
    binding = {"schema": "slop_ninja_last_block_fit_binding_v1", "condition": args.condition,
               "seed": args.seed, "dataset_manifest_sha256": bridge.sha256(root / "manifest.json"),
               "contract_sha256": manifest["contract_sha256"], "protocol_sha256": manifest["protocol_sha256"],
               "initializer_sha256": bridge.sha256(args.init), "parent_artifact_id": parent["artifact_id"],
               "trainer_sha256": bridge.sha256(__file__), "architecture_sha256": bridge.sha256(architecture.__file__),
               "shared_loss_sha256": bridge.sha256(shared.__file__),
               "shared_heads_implementation_sha256": bridge.sha256(warm_start_a.__file__),
               "artifact_loader_sha256": bridge.sha256(bridge.__file__),
               "transformers_model_source_sha256": bridge.sha256(__import__(encoder.__module__, fromlist=["x"]).__file__),
               "torch": torch.__version__, "cuda": torch.version.cuda, "device": str(device)}
    recovery = output / "recovery.pt"
    best, best_epoch, best_state, history, first_epoch = float("inf"), 0, None, [], 0
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
        first_epoch = saved["epoch"] + 1
    else:
        shared.demand(not (output / "binding.json").exists(), "incomplete first epoch; use a fresh output directory")
        bridge.write_json(output / "binding.json", binding)
        with torch.no_grad():
            for i in range(min(4, len(train.rows))):
                tensors = train.tensors(i, device)
                actual = forward(joint, tensors)["origin"]
                expected = (encoder.classifier(tensors["frozen_encoding"]) / heads.base_temperature).softmax(-1)
                shared.demand(float((actual - expected).abs().max()) <= 1e-6, "initial A differs from V6")
    start = time.monotonic()
    for epoch in range(first_epoch, 13):
        if epoch:
            joint.train()
            order = torch.randperm(len(train.rows), generator=order_rng).tolist()
            for begin in range(0, len(order), 64):
                indices = order[begin:begin + 64]
                optimizer.zero_grad(set_to_none=True)
                for index in indices:
                    prediction = forward(joint, train.tensors(index, device))
                    objective = train.objective(prediction, index) * (len(train.rows) / len(indices))
                    shared.demand(bool(torch.isfinite(objective)), "nonfinite loss")
                    objective.backward()
                torch.nn.utils.clip_grad_norm_([p for p in joint.parameters() if p.requires_grad], 5., error_if_nonfinite=True)
                optimizer.step()
        score, objective, predictions = evaluate(joint, dev, device, profiles, export=True)
        shared.demand(math.isfinite(score) and math.isfinite(objective), "nonfinite Development result")
        if score < best:
            best, best_epoch, best_state = score, epoch, state(joint)
        train_score, train_loss, _ = evaluate(joint, train, device, profiles)
        row = {"epoch": epoch, "development_involvement_brier": score, "development_joint_loss": objective,
               "train_involvement_brier": train_score, "train_joint_loss": train_loss,
               "elapsed_seconds_this_invocation": time.monotonic() - start}
        history.append(row)
        current = state(joint)
        atomic_checkpoint(recovery, {"binding": binding, "epoch": epoch, "current": current,
            "optimizer": optimizer.state_dict(), "order_rng": order_rng.get_state(), "torch_rng": torch.get_rng_state(),
            "cuda_rng": torch.cuda.get_rng_state_all() if device.type == "cuda" else [],
            "best_score": best, "best_epoch": best_epoch, "best": best_state, "history": history})
        if epoch in (0, 1):
            atomic_checkpoint(output / f"epoch-{epoch}.pt", {"binding": binding, "epoch": epoch, "state": current})
            bridge.write_json(output / f"epoch-{epoch}-development.json", predictions)
        bridge.write_json(output / "history.json", history)
        print(json.dumps(row), flush=True)
    restore(joint, best_state)
    atomic_checkpoint(output / "selected.pt", {"binding": binding, "epoch": best_epoch, "state": best_state})
    save_file(best_state["last_layer"], str(output / "last-layer.safetensors"))
    save_file(best_state["heads"], str(output / "shared-heads.safetensors"))
    _, _, predictions = evaluate(joint, dev, device, profiles, export=True)
    bridge.write_json(output / "development-predictions.json", predictions)
    bridge.write_json(output / "fit.json", {"schema": "slop_ninja_last_block_a_fit_v1", "complete": True,
        "binding": binding, "epochs": 12, "selected_epoch": best_epoch, "development_involvement_brier": best,
        "real_data_fit": True, "calibrated": False, "final_test_opened": False,
        "trained_selected_heads": {k: best_epoch > 0 and train.denominators[k] > 0 for k in train.denominators},
        "peak_cuda_allocated_bytes": torch.cuda.max_memory_allocated() if device.type == "cuda" else None,
        "cuda_allocation_fraction": .50, "train_rows": len(train.rows), "development_rows": len(dev.rows),
        "selected_checkpoint_sha256": bridge.sha256(output / "selected.pt"),
        "last_layer_sha256": bridge.sha256(output / "last-layer.safetensors"),
        "shared_heads_sha256": bridge.sha256(output / "shared-heads.safetensors"),
        "limitations": ["Development selection evidence, not final Test.", "CAL and Rust derivative qualification still required.",
                       "Epoch0 share and attribution outputs are untrained diagnostics."]})


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    for key in ("data", "artifact", "init", "out"):
        p.add_argument("--" + key, required=True)
    p.add_argument("--condition", choices=("frozen_last_block", "adapt_last_block"), required=True)
    p.add_argument("--seed", type=int, required=True)
    p.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
    run(p.parse_args())

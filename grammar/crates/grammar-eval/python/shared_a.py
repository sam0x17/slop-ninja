"""Shared numeric CFG encoder for A; Rust owns preparation and evaluation.

Only Train/Development/Calibration queries select or fit parameters. Independent
reference rows can be encoded after fitting; pooling and retrieval live in Rust.
The strict Rust weights-v1 shape is unchanged. Qualification lives in fit.json.
"""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import time

import torch
from torch import nn
from torch.nn import functional as F


ROLES = ("human", "ai")
ENCODING_SCHEMA = "slop_ninja_shared_a_encoding_v1"
JOINT_CONDITIONAL_CE_WEIGHT = 0.25


def task_weights(control):
    return {"origin_ce": 0.0 if control == "attribution_only" else 1.0,
            "involvement_brier": 0.0 if control == "attribution_only" else 1.0,
            "share_mse": 1.0 if control == "joint" else 0.0,
            **{f"{role}_conditional_ce": (JOINT_CONDITIONAL_CE_WEIGHT if control == "joint"
                                         else 1.0 if control == "attribution_only" else 0.0)
               for role in ROLES}}


def demand(ok, message):
    if not ok:
        raise ValueError(message)


def read(path):
    return json.loads(Path(path).read_bytes())


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write(path, value):
    with Path(path).open("x") as handle:
        json.dump(value, handle, allow_nan=False, indent=2)
        handle.write("\n")


class SharedA(nn.Module):
    def __init__(self, dimension, human_profiles=0, ai_profiles=0, hidden=64, embedding=32):
        super().__init__()
        self.encoder = nn.Sequential(nn.Linear(dimension, hidden), nn.Tanh(),
                                     nn.Linear(hidden, embedding), nn.Tanh())
        self.origin = nn.Linear(embedding, 3)
        self.mixed_share = nn.Linear(embedding, 1)
        self.human = nn.Linear(embedding, max(1, human_profiles))
        self.ai = nn.Linear(embedding, max(1, ai_profiles))

    def forward(self, x, temperature=1.0):
        z = self.encoder(x)
        origin_logits = self.origin(z)
        p = (origin_logits / temperature).softmax(-1)
        q = self.mixed_share(z).squeeze(-1).sigmoid()
        return {"encoding": z, "origin_logits": origin_logits, "origin": p,
                "conditional_mixed_share": q, "share": p[:, 1] + p[:, 2] * q,
                "human_logits": self.human(z), "ai_logits": self.ai(z)}


def role_label(row, role, profiles):
    """Legacy null targets convey no withheld-identity evidence."""
    if "roles" in row:
        label = row["roles"][role]
        demand(label["present"] is None or type(label["present"]) is bool, "role presence")
        demand(type(label["known"]) is bool, "role known mask")
        demand(label["identity"] is None or isinstance(label["identity"], str), "role identity")
        if label["identity"] is not None:
            demand(label["known"] == (label["identity"] in profiles), "known identity roster mismatch")
        return label
    target = row["targets"].get(role)
    origin = row["targets"].get("origin")
    present = None if origin is None else origin in ((0, 2) if role == "human" else (1, 2))
    return {"present": present, "identity": profiles[target] if target is not None else None,
            "known": target is not None}


def query_weights(rows, weighting="family"):
    """Explicit research prevalence; targets determine loss weights, never inputs."""
    demand(weighting in ("family", "family_origin_balanced"), "unsupported query weighting")
    groups = Counter(r["source_group"] for r in rows)
    if weighting == "family":
        return [1.0 / (len(groups) * groups[r["source_group"]]) for r in rows]
    cells = Counter((r["source_group"], r["targets"].get("origin")) for r in rows)
    strata = Counter(group for group, _ in cells)
    return [1.0 / (len(groups) * strata[r["source_group"]] *
                   cells[(r["source_group"], r["targets"].get("origin"))]) for r in rows]


def dataset(root, part, manifest, device, profiles=None, weighting="family"):
    binding = next(row for row in manifest["splits"] if row["split"] == part)
    path = root / f"{part}.jsonl"
    demand(sha(path) == binding["sha256"], f"changed {part} inputs")
    rows = [json.loads(line) for line in path.read_text().splitlines()]
    demand(bool(rows), f"empty {part}")
    demand(all(r.get("purpose", "query") == "query" for r in rows), "references cannot enter fitting")
    demand(len({r["id"] for r in rows}) == len(rows), "duplicate query ids")
    demand(all(r["input"]["contract_sha256"] == manifest["contract_sha256"] for r in rows), "contract mismatch")
    x = torch.tensor([r["input"]["x"] for r in rows], dtype=torch.float32, device=device)
    demand(x.ndim == 2 and bool(torch.isfinite(x).all()), "invalid features")
    weights = torch.tensor(query_weights(rows, weighting), device=device)
    weights = weights / weights.sum()
    origins = [r["targets"].get("origin") for r in rows]
    demand(all(y is None or (type(y) is int and 0 <= y < 3) for y in origins), "origin label")
    y = torch.tensor([v if v is not None else -1 for v in origins], dtype=torch.long, device=device)
    shares = [r["targets"].get("share") for r in rows]
    demand(all(v is None or (isinstance(v, (int, float)) and 0 <= v <= 1) for v in shares), "share label")
    share_mask = torch.tensor([v is not None for v in shares], dtype=torch.bool, device=device)
    roles = {}
    for role in ROLES:
        targets = [r["targets"].get(role) for r in rows]
        demand(all(v is None or (type(v) is int and v >= 0) for v in targets), "identity target")
        if profiles is not None:
            for row, target in zip(rows, targets):
                demand(target is None or target < len(profiles[role]), "identity index out of roster")
                label = role_label(row, role, profiles[role])
                if target is not None:
                    demand(label["present"] is True and label["known"] and
                           label["identity"] == profiles[role][target], "conditional target lacks documented contributor")
        roles[role] = torch.tensor([v if v is not None else -1 for v in targets], dtype=torch.long, device=device)
    return {"rows": rows, "x": x, "y": y, "weight": weights,
            "share": torch.tensor([v if v is not None else 0. for v in shares], device=device),
            "share_mask": share_mask, **roles}


def masked_mean(values, weights, mask):
    w = weights[mask]
    return (values * (w / w.sum())).sum()


def conditional_ce(out, batch):
    terms = []
    for role in ROLES:
        mask = batch[role] >= 0
        if bool(mask.any()):
            demand(out[f"{role}_logits"].shape[1] > 1, "one identity cannot qualify attribution")
            terms.append(masked_mean(F.cross_entropy(out[f"{role}_logits"][mask], batch[role][mask],
                                                       reduction="none"), batch["weight"], mask))
    return terms


def loss(model, batch, control):
    out = model(batch["x"])
    weights, y = batch["weight"], batch["y"]
    total = out["encoding"].sum() * 0.0
    mask = y >= 0
    if control != "attribution_only" and bool(mask.any()):
        involvement = out["origin"][mask, 1:].sum(-1)
        total = total + masked_mean(F.cross_entropy(out["origin_logits"][mask], y[mask], reduction="none") +
                                   (involvement - (y[mask] != 0).float()).square(), weights, mask)
    if control == "joint" and bool(batch["share_mask"].any()):
        mask = batch["share_mask"]
        total = total + masked_mean((out["share"][mask] - batch["share"][mask]).square(), weights, mask)
    if control != "origin_only":
        total = total + (JOINT_CONDITIONAL_CE_WEIGHT if control == "joint" else 1.0) * sum(conditional_ce(out, batch))
    return total


@torch.no_grad()
def criterion(model, batch, temperature=1.0, control="joint"):
    out = model(batch["x"], temperature)
    if control == "attribution_only":
        terms = conditional_ce(out, batch)
        demand(bool(terms), "attribution-only selection requires supervised Development identities")
        return float(torch.stack(terms).mean())
    mask = batch["y"] >= 0
    demand(bool(mask.any()), "origin selection/calibration requires labelled queries")
    p = out["origin"][mask, 1:].sum(-1)
    return float(masked_mean((p - (batch["y"][mask] != 0).float()).square(), batch["weight"], mask))


def calibrate_named(scores, labels, groups, trained=True):
    """CAL-only threshold: half correct accepted known, half rejected withheld.

    Equal source-family weighting within each class; presence is documented truth,
    never the model's involvement probability. Missing evidence cannot qualify.
    """
    known = [i for i, r in enumerate(labels) if r["present"] is True and r["known"] and r["identity"] is not None]
    withheld = [i for i, r in enumerate(labels) if r["present"] is True and not r["known"] and r["identity"] is not None]
    report = {"schema": "slop_ninja_conditional_named_threshold_v1", "split": "calibration",
              "objective": "0.5*known_correct_named_acceptance+0.5*withheld_rejection",
              "weighting": "equal_source_family_within_known_and_withheld", "known_rows": len(known),
              "withheld_rows": len(withheld), "known_families": len({groups[i] for i in known}),
              "withheld_families": len({groups[i] for i in withheld}),
              "presence_gate": "documented_present_only_no_predicted_involvement_cutoff",
              "limitations": ["CAL selection is not independent evaluation or a rejection guarantee",
                              "softmax confidence is conditional on the named roster, not identity presence",
                              "v1 threshold <=1 with >= acceptance cannot reject exactly-one confidence"]}
    if not trained or not known or not withheld:
        return 1.0, {**report, "threshold": 1.0, "qualified": False, "objective_value": None,
                     "reason": "untrained_head" if not trained else "requires_known_and_withheld_CAL_identities"}
    def average(indices, values):
        counts = Counter(groups[i] for i in indices)
        return sum(values(i) / counts[groups[i]] / len(counts) for i in indices)
    observed = sorted({scores[i][1] for i in known + withheld})
    candidates = [0., 1.] + [(a + b) / 2 for a, b in zip(observed, observed[1:])]
    def rates(threshold):
        return (average(known, lambda i: float(scores[i][1] >= threshold and scores[i][0] == labels[i]["identity"])),
                average(withheld, lambda i: float(scores[i][1] < threshold)))
    threshold = max(candidates, key=lambda t: (sum(rates(t)) / 2, t))
    accepted, rejected = rates(threshold)
    return threshold, {**report, "qualified": True, "threshold": threshold, "candidate_count": len(candidates),
                       "objective_value": (accepted + rejected) / 2, "known_correct_named_rate": accepted,
                       "withheld_rejection_rate": rejected, "tie_break": "higher_threshold"}


@torch.no_grad()
def predictions(model, inputs, temperature, contract_hash, profiles, thresholds, device, export_encodings=False):
    demand(all(set(row) == {"contract_sha256", "x"} for row in inputs), "numeric-only inference input required")
    demand(all(row["contract_sha256"] == contract_hash for row in inputs), "inference contract mismatch")
    if not inputs:
        return []
    x = torch.tensor([row["x"] for row in inputs], dtype=torch.float32, device=device)
    demand(x.ndim == 2 and bool(torch.isfinite(x).all()), "nonfinite request")
    out = model(x, temperature)
    results = []
    for i in range(len(inputs)):
        p = out["origin"][i].tolist()
        p = [v / sum(p) for v in p]
        role_results = {}
        for role, involvement in (("human", p[0] + p[2]), ("ai", p[1] + p[2])):
            scores = out[f"{role}_logits"][i].softmax(-1).tolist()
            best = max(range(len(scores)), key=scores.__getitem__)
            named = bool(profiles[role]) and scores[best] >= thresholds[role]
            role_results[role] = {"profile": profiles[role][best] if named else f"other_{role}",
                                  "involvement_probability": involvement,
                                  "conditional_named_scores": dict(zip(profiles[role], scores)),
                                  "fallback_asserts_presence": False}
        share = p[1] + p[2] * float(out["conditional_mixed_share"][i])
        demand(-1e-7 <= p[1] <= share + 1e-7 <= p[1] + p[2] + 2e-7 <= 1.000001, "incoherent outputs")
        result = {"origin": dict(zip(("human_only", "model_only", "mixed"), p)),
                  "ai_confidence_percent": 100 * (p[1] + p[2]), "ai_share_percent": 100 * share,
                  "attribution": role_results}
        if export_encodings:
            result["encoding"] = out["encoding"][i].tolist()
        results.append(result)
    return results


def train(args):
    root, output = Path(args.data), Path(args.out)
    output.mkdir(exist_ok=False)
    manifest, contract = read(root / "manifest.json"), read(root / "contract.json")
    demand(sha(root / "contract.json") == manifest["contract_file_sha256"], "contract bytes changed")
    device = torch.device(args.device)
    demand(device.type != "cuda" or torch.cuda.is_available(), "CUDA required but unavailable")
    demand(args.epochs >= 0, "negative epochs")
    torch.set_num_threads(1)
    # A process can train several seeds without resetting inter-op threads.
    if torch.get_num_interop_threads() != 1:
        torch.set_num_interop_threads(1)
    torch.manual_seed(args.seed)
    torch.use_deterministic_algorithms(True)
    if device.type == "cuda":
        torch.cuda.set_per_process_memory_fraction(0.20)
        torch.cuda.reset_peak_memory_stats()
    profiles = {role: contract[f"{role}_profiles"] for role in ROLES}
    for names in profiles.values():
        demand(len(names) == len(set(names)) and all(isinstance(n, str) and n and not n.startswith("other_") for n in names), "invalid profiles")
    weighting = getattr(args, "weighting", "family")
    batches = {p: dataset(root, p, manifest, device, profiles, weighting) for p in ("train", "development", "calibration")}
    groups = {p: {r["source_group"] for r in batch["rows"]} for p, batch in batches.items()}
    demand(all(not groups[a] & groups[b] for a, b in (("train", "development"), ("train", "calibration"), ("development", "calibration"))), "family leakage")
    for role in ROLES:
        targets = set(batches["train"][role].tolist()) - {-1}
        demand(targets == set(range(len(profiles[role]))), f"{role} profiles must come from supervised Train identities")
    architecture = {"dimension": contract["input_dimension"], "human_profiles": len(profiles["human"]),
                    "ai_profiles": len(profiles["ai"]), "hidden": 64, "embedding": 32}
    demand(all(b["x"].shape[1] == architecture["dimension"] for b in batches.values()), "feature dimension mismatch")
    model = SharedA(**architecture).to(device)
    b = batches["train"]
    available = {"origin": int((b["y"] >= 0).sum()), "share": int(b["share_mask"].sum()),
                 "mixed_fractional_share": int((b["share_mask"] & (b["share"] > 0) & (b["share"] < 1)).sum()),
                 **{r: int((b[r] >= 0).sum()) for r in ROLES}}
    supervision = {k: v if ((k in ROLES and args.control != "origin_only") or
                               (k == "origin" and args.control != "attribution_only") or
                               (k in ("share", "mixed_fractional_share") and args.control == "joint")) else 0
                   for k, v in available.items()}
    demand(supervision["origin"] > 0 if args.control != "attribution_only" else
           supervision["human"] + supervision["ai"] > 0, "no training labels for selected control")
    # Untrained heads are fixed, explicitly unqualified diagnostics, with neutral interpolation.
    for head, trained in ((model.origin, supervision["origin"]),
                          (model.mixed_share, supervision["mixed_fractional_share"]),
                          (model.human, supervision["human"]), (model.ai, supervision["ai"])):
        if not trained:
            for parameter in head.parameters():
                parameter.data.zero_()
                parameter.requires_grad_(False)
    optimizer = torch.optim.AdamW(model.parameters(), lr=0.001, weight_decay=0.01)
    best, best_epoch, state = float("inf"), 0, None
    start, history = time.perf_counter(), []
    selection = "conditional_supervised_ce" if args.control == "attribution_only" else "involvement_brier"
    for epoch in range(args.epochs + 1):
        if epoch:
            model.train()
            optimizer.zero_grad(set_to_none=True)
            objective = loss(model, batches["train"], args.control)
            demand(bool(torch.isfinite(objective)), "nonfinite training objective")
            objective.backward()
            torch.nn.utils.clip_grad_norm_(model.parameters(), 5.0)
            optimizer.step()
        model.eval()
        score = criterion(model, batches["development"], control=args.control)
        if score < best - 1e-9:
            best, best_epoch = score, epoch
            state = {k: v.detach().cpu().clone() for k, v in model.state_dict().items()}
        if epoch % 25 == 0 or epoch == args.epochs:
            history.append({"epoch": epoch, f"development_{selection}": score})
            torch.save({"epoch": epoch, "model": model.state_dict(), "optimizer": optimizer.state_dict(),
                        "best_epoch": best_epoch, "best": state}, output / "recovery.pt.tmp")
            os.replace(output / "recovery.pt.tmp", output / "recovery.pt")
    model.load_state_dict(state)
    temperature = 1.0
    cal = batches["calibration"]
    origin_calibrated = bool(supervision["origin"] and best_epoch > 0 and (cal["y"] >= 0).any())
    if origin_calibrated:
        temperature = min([1.0] + [float(torch.exp(torch.tensor(k * 0.1))) for k in range(-30, 31)],
                          key=lambda t: (criterion(model, cal, t), abs(t - 1.0)))
    thresholds, threshold_reports = {}, {}
    with torch.no_grad():
        cal_out = model(cal["x"])
        for role in ROLES:
            scores, best_ids = cal_out[f"{role}_logits"].softmax(-1).max(-1)
            named = [(profiles[role][i] if profiles[role] else None, score)
                     for i, score in zip(best_ids.tolist(), scores.tolist())]
            labels = [role_label(r, role, profiles[role]) for r in cal["rows"]]
            thresholds[role], threshold_reports[role] = calibrate_named(
                named, labels, [r["source_group"] for r in cal["rows"]], bool(supervision[role] and best_epoch > 0))
    artifact = {"schema": "slop_ninja_shared_a_weights_v1", "architecture": architecture,
                "operations": ["linear", "tanh", "softmax", "sigmoid"],
                "contract_sha256": manifest["contract_sha256"], "temperature": temperature,
                "profiles": profiles, "thresholds": thresholds, "supervision": supervision,
                "origin_order": ["human_only", "model_only", "mixed"], "control": args.control,
                "weights": {k: {"shape": list(v.shape), "values": v.flatten().tolist()} for k, v in state.items()}}
    write(output / "weights.json", artifact)
    torch.save({"architecture": architecture, "state_dict": state, "temperature": temperature,
                "contract_sha256": manifest["contract_sha256"]}, output / "model.pt")
    export_encodings = getattr(args, "export_encodings", False)
    for part, batch in batches.items():
        result = predictions(model, [r["input"] for r in batch["rows"]], temperature,
                             manifest["contract_sha256"], profiles, thresholds, device, export_encodings)
        write(output / f"{part}-predictions.json", [{"id": r["id"], "prediction": p} for r, p in zip(batch["rows"], result)])
    reference_hash = None
    if export_encodings and (root / "reference.jsonl").exists():
        reference_hash = sha(root / "reference.jsonl")
        binding = next((r for r in manifest["splits"] if r["split"] == "reference"), None)
        demand(binding is None or binding["sha256"] == reference_hash, "changed reference inputs")
        refs = [json.loads(line) for line in (root / "reference.jsonl").read_text().splitlines()]
        demand(all(r.get("purpose") == "reference" for r in refs), "reference purpose required")
        demand(not {r["source_group"] for r in refs} & set.union(*groups.values()), "reference works overlap fitting queries")
        demand(len({r["id"] for r in refs}) == len(refs), "duplicate references")
        demand(not {r["id"] for r in refs} & {r["id"] for b in batches.values() for r in b["rows"]}, "reference/query id overlap")
        result = predictions(model, [r["input"] for r in refs], temperature,
                             manifest["contract_sha256"], profiles, thresholds, device, True)
        write(output / "reference-encodings.json", {"schema": ENCODING_SCHEMA,
              "contract_sha256": manifest["contract_sha256"], "weights_sha256": sha(output / "weights.json"),
              "reference_sha256": reference_hash,
              "rows": [{"id": r["id"], "encoding": p["encoding"]} for r, p in zip(refs, result)]})
    status = {"origin": "trained" if supervision["origin"] and best_epoch else "untrained",
              "share_interpolation": ("trained_fractional" if best_epoch else "untrained_initialization")
                  if supervision["mixed_fractional_share"] else "untrained_fixed_half",
              **{r: "trained" if supervision[r] and best_epoch else "untrained" for r in ROLES}}
    write(output / "fit.json", {"seed": args.seed, "control": args.control, "epochs": args.epochs, "selected_epoch": best_epoch,
          "query_weighting": weighting,
          "prevalence_scope": "declared research sampling; not deployment prevalence",
          "task_weights": task_weights(args.control),
          "development_selection": {"metric": selection, "value": best, "split": "development"},
          "development_selection_brier_uncalibrated": best if selection == "involvement_brier" else None,
          "calibration_temperature": temperature, "origin_calibrated": origin_calibrated,
          "conditional_named_calibration": threshold_reports, "training_status": status,
          "supervision": supervision, "available_train_supervision": available,
          "history": history, "elapsed_seconds": time.perf_counter()-start,
          "cuda_peak_allocated_bytes": torch.cuda.max_memory_allocated() if device.type == "cuda" else 0,
          "torch": torch.__version__, "cuda": torch.version.cuda, "device": str(device),
          "training_source_sha256": sha(__file__), "data_manifest_sha256": sha(root/"manifest.json"),
          "weights_sha256": sha(output/"weights.json"), "checkpoint_sha256": sha(output/"model.pt"),
          "encoding_export": {"enabled": export_encodings, "schema": ENCODING_SCHEMA,
                              "reference_sha256": reference_hash, "reference_used_for_fit_or_normalization": False},
          "limitations": ["untrained heads and interpolation are diagnostics only", "named confidence does not assert role presence"],
          "final_test_opened": False, "clean_holdout": manifest.get("clean_holdout", False)})
    print(json.dumps({"out": str(output), "selected_epoch": best_epoch, "supervision": supervision}))


def infer(args):
    artifact, request = read(args.artifact), read(args.request)
    model = SharedA(**artifact["architecture"])
    model.load_state_dict({k: torch.tensor(v["values"]).reshape(v["shape"]) for k, v in artifact["weights"].items()})
    model.eval()
    write(args.out, predictions(model, request["inputs"], artifact["temperature"], artifact["contract_sha256"],
                               artifact["profiles"], artifact["thresholds"], torch.device("cpu"),
                               getattr(args, "export_encodings", False)))


def frozen_qualification(artifact, fit=None):
    """Control-aware metadata only: never change or qualify frozen parameters."""
    fit = fit or {}
    control = artifact["control"]
    supervision = artifact["supervision"]
    declared = fit.get("training_status", {})
    origin_trained = control != "attribution_only" and supervision.get("origin", 0) > 0
    origin_status = declared.get("origin", "supervised_training_reported") if origin_trained else "untrained"
    if fit.get("selected_epoch") == 0:
        origin_status = "untrained"
    calibrated = (origin_status != "untrained" and fit.get("origin_calibrated", True) and
                  fit.get("calibration_temperature") == artifact["temperature"])
    return {"origin": {"training_status": origin_status,
                       "calibration": "frozen_CAL_temperature" if calibrated else "not_documented_or_untrained",
                       "temperature": artifact["temperature"],
                       "quality": "inference_does_not_establish_quality_or_calibration_generalization"},
            "attribution": {role: (declared.get(role, "supervised_training_reported")
                                   if control != "origin_only" and supervision.get(role, 0) > 0
                                   and fit.get("selected_epoch") != 0 else "untrained") for role in ROLES},
            "share_interpolation": (declared.get("share_interpolation", "fractional_supervision_reported")
                                    if control == "joint" and supervision.get("mixed_fractional_share", 0) > 0
                                    and fit.get("selected_epoch") != 0 else "untrained"),
            "named_thresholds": "frozen_from_artifact_never_gated_by_predicted_involvement",
            "conditional_named_calibration": fit.get("conditional_named_calibration"),
            "legacy_missing_metadata": not bool(fit)}


@torch.inference_mode()
def predict_split(args):
    """Read the authorized frozen Test export only on explicit invocation.

    No Train/Dev/CAL/reference files, labels, normalization, selection, optimizer,
    or calibration functions are used here. Rust already produced numeric x.
    --out is a new JSON file; its .provenance.json sidecar binds exact bytes.
    """
    demand(args.split == "test", "predict-split only accepts frozen Test inference")
    root, artifact_path, output = Path(args.data), Path(args.artifact), Path(args.out)
    provenance_path = Path(str(output) + ".provenance.json")
    demand(not output.exists() and not provenance_path.exists(), "prediction output or provenance exists")
    artifact_bytes = artifact_path.read_bytes()
    artifact_hash = hashlib.sha256(artifact_bytes).hexdigest()
    artifact = json.loads(artifact_bytes)
    demand(artifact["schema"] == "slop_ninja_shared_a_weights_v1", "unsupported frozen weights schema")
    demand(artifact["control"] in ("joint", "origin_only", "attribution_only"), "unknown frozen control")
    demand(artifact["operations"] == ["linear", "tanh", "softmax", "sigmoid"] and
           artifact["origin_order"] == ["human_only", "model_only", "mixed"], "unsupported frozen operations/order")
    temperature = artifact["temperature"]
    demand(isinstance(temperature, (int, float)) and 0 < temperature < float("inf"), "invalid frozen temperature")
    manifest_bytes = (root / "manifest.json").read_bytes()
    manifest = json.loads(manifest_bytes)
    contract_bytes = (root / "contract.json").read_bytes()
    contract = json.loads(contract_bytes)
    contract_hash = hashlib.sha256(contract_bytes).hexdigest()
    demand(contract_hash == manifest["contract_file_sha256"], "changed frozen contract bytes")
    demand(manifest["contract_sha256"] == artifact["contract_sha256"], "frozen model/contract mismatch")
    demand(contract["input_dimension"] == artifact["architecture"]["dimension"], "frozen input dimension mismatch")
    for role in ROLES:
        names = artifact["profiles"][role]
        demand(names == contract[f"{role}_profiles"] and len(names) == artifact["architecture"][f"{role}_profiles"],
               "frozen profile roster mismatch")
        demand(0 <= artifact["thresholds"][role] <= 1, "invalid frozen threshold")
    bindings = [b for b in manifest["splits"] if b["split"] == "test"]
    demand(len(bindings) == 1, "exactly one frozen Test binding required")
    fit_path, fit, fit_hash = artifact_path.parent / "fit.json", None, None
    if fit_path.exists():
        fit_bytes = fit_path.read_bytes()
        fit_hash = hashlib.sha256(fit_bytes).hexdigest()
        fit = json.loads(fit_bytes)
        demand(fit["weights_sha256"] == artifact_hash, "fit metadata belongs to different weights")
        demand(fit["final_test_opened"] is False, "fit metadata reports Test access")
    torch.set_num_threads(1)
    model = SharedA(**artifact["architecture"])
    state = {k: torch.tensor(v["values"], dtype=torch.float32).reshape(v["shape"])
             for k, v in artifact["weights"].items()}
    demand(all(bool(torch.isfinite(t).all()) for t in state.values()), "nonfinite frozen weights")
    model.load_state_dict(state, strict=True)
    model.requires_grad_(False)
    model.eval()
    # Test is first opened here, after the frozen artifact/contract checks.
    test_bytes = (root / "test.jsonl").read_bytes()
    test_hash = hashlib.sha256(test_bytes).hexdigest()
    demand(test_hash == bindings[0]["sha256"], "changed frozen Test inputs")
    rows = [json.loads(line) for line in test_bytes.splitlines()]
    demand(bool(rows), "empty frozen Test split")
    demand(bindings[0].get("rows", len(rows)) == len(rows), "frozen Test row count mismatch")
    ids = [row["id"] for row in rows]
    demand(all(isinstance(i, str) and i for i in ids) and len(ids) == len(set(ids)), "invalid or duplicate Test ids")
    demand(all(row.get("purpose", "query") == "query" and row.get("frozen_split", "test") == "test"
               for row in rows), "only Test queries may be inferred")
    inputs = [row["input"] for row in rows]
    demand(all(set(x) == {"contract_sha256", "x"} and isinstance(x["x"], list) and
               len(x["x"]) == artifact["architecture"]["dimension"] and
               all(type(v) in (int, float) for v in x["x"]) for x in inputs), "numeric-only frozen inputs required")
    result = predictions(model, inputs, temperature, artifact["contract_sha256"],
                         artifact["profiles"], artifact["thresholds"], torch.device("cpu"), True)
    output_bytes = (json.dumps([{"id": i, "prediction": p} for i, p in zip(ids, result)],
                              allow_nan=False, indent=2) + "\n").encode()
    provenance = {"schema": "slop_ninja_shared_a_frozen_split_inference_v1", "split": "test",
                  "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
                  "contract_file_sha256": contract_hash, "contract_sha256": artifact["contract_sha256"],
                  "test_sha256": test_hash, "weights_sha256": artifact_hash, "fit_file_sha256": fit_hash,
                  "prediction_file_sha256": hashlib.sha256(output_bytes).hexdigest(), "documents": len(rows),
                  "encoding_schema": ENCODING_SCHEMA, "device": "cpu", "torch": torch.__version__,
                  "implementation_sha256": sha(__file__), "control": artifact["control"],
                  "qualification": frozen_qualification(artifact, fit),
                  "fitting_performed": False, "normalization_fitted": False, "calibration_fitted": False,
                  "test_used_for_selection": False, "test_opened_for_frozen_inference": True,
                  "labels_used_for_inference": False}
    with output.open("xb") as handle:
        handle.write(output_bytes)
    write(provenance_path, provenance)
    print(json.dumps({"out": str(output), "provenance": str(provenance_path), "documents": len(rows)}))


def main(argv=None):
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("train")
    for name in ("data", "out"):
        p.add_argument(f"--{name}", required=True)
    p.add_argument("--device", default="cuda")
    p.add_argument("--seed", type=int, default=17)
    p.add_argument("--epochs", type=int, default=250)
    p.add_argument("--control", choices=("joint", "origin_only", "attribution_only"), default="joint")
    p.add_argument("--export-encodings", action="store_true")
    p.add_argument("--weighting", choices=("family", "family_origin_balanced"), default="family")
    p.set_defaults(func=train)
    p = sub.add_parser("predict")
    for name in ("artifact", "request", "out"):
        p.add_argument(f"--{name}", required=True)
    p.add_argument("--export-encodings", action="store_true")
    p.set_defaults(func=infer)
    p = sub.add_parser("predict-split", help="CPU inference only from frozen numeric Test export; never fit")
    for name in ("data", "artifact", "out"):
        p.add_argument(f"--{name}", required=True)
    p.add_argument("--split", choices=("test",), required=True)
    p.set_defaults(func=predict_split)
    args = parser.parse_args(argv)
    args.func(args)


if __name__ == "__main__":
    main()

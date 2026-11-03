"""Function-preserving V6 residual A; numeric preparation/replay stay in Rust.

qualify --text NUMERIC/text --combined NUMERIC/combined --init INIT --out RECEIPT
train --data NUMERIC/text --init INIT --equivalence RECEIPT --seed 17 --out NEW

Joint is the default; nonjoint adaptation controls require a separate frozen
control protocol. All use 250 epochs/seeds17,29,41. Both modalities must pass
the complete manifest-bound initial-origin gate before a fit can begin. The
original experiment requires 929 rows; an explicit Train extension verifies all
original numeric values and targets against that frozen parent. Test paths are
never opened. Existing shared_a training semantics are reused, not modified.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import time

import torch
from torch import nn
import shared_a as shared
from shared_a import demand, read, sha, write


INIT_SCHEMA = "slop_ninja_v6_classifier_init_v1"
SCHEMA = "slop_ninja_v6_residual_a_weights_v1"
GATE_SCHEMA = "slop_ninja_v6_residual_initial_equivalence_v1"
SCALE_SCHEMA = "slop_ninja_v6_residual_train_scale_v1"
SCALE_GATE_SCHEMA = "slop_ninja_v6_residual_initial_equivalence_v2"
MIXTURE_SCHEMA = "slop_ninja_v6_residual_cohort_mixture_v1"
SOURCE_SIZE_SCHEMA = "slop_ninja_a_source_size_sampling_v1"
SOURCE_STRATA = ("cmu-books-historical", "hc3-wiki-historical", "plos_one", "wikinews",
                 "project_gutenberg_historical_novel")
BASE_TEMPERATURE = 1.8776105792643432
ORIGIN_ORDER = ["human_only", "model_only", "mixed"]
PARTS = ("train", "development", "calibration", "reference")
OPERATIONS = ["linear", "tanh", "add", "subtract", "divide", "softmax", "sigmoid"]
EXPECTED_ROWS = 929
TOLERANCE = 1e-6
CONTROLS = ("joint", "origin_only", "attribution_only")
CONTROL_SCHEMA = "slop_ninja_v6_residual_adaptation_controls_v1"


def control_protocol(path, control, contract, binding, init_path, equivalence_path):
    """Bind a separately selected adaptation comparison; never change joint FIT."""
    demand(control in CONTROLS, "unsupported adaptation control")
    if control == "joint":
        demand(path is None, "joint uses its existing protocol, not a control protocol")
        return None
    demand(path is not None, "nonjoint adaptation requires --control-protocol")
    raw = Path(path).read_bytes()
    protocol = json.loads(raw)
    required = {
        "schema": CONTROL_SCHEMA, "status": "frozen",
        "controls": ["origin_only", "attribution_only"], "seeds": [17, 29, 41], "epochs": 250,
        "optimizer": {"name": "AdamW", "learning_rate": .001, "weight_decay": .01, "gradient_clip_norm": 5},
        "architecture": {"dimension": 2588, "raw_text_dimension": 768, "hidden": 64, "embedding": 768,
                         **{f"{r}_profiles": len(contract[f"{r}_profiles"]) for r in shared.ROLES}},
        "query_weighting": "family_origin_balanced",
        "selection": {"origin_only": "development_involvement_brier",
                      "attribution_only": "development_conditional_supervised_ce"},
        "calibration": {"origin_only": "CAL_origin_temperature_only",
                        "attribution_only": "CAL_named_thresholds_only"},
        "adaptation_scope": "frozen_V6_origin_supervised_initialization_not_origin_free_pretraining",
        "trainable_parameters": "residual_and_selected_heads", "share_interpolation": "untrained_fixed_half",
        "upstream_encoder_frozen": True, "selected_by": "joint_Development_only",
        "final_test_opened": False, "control_based_test_selection": False,
        "input_mode": contract["ablation"], "data_manifest_sha256": binding["manifest_sha256"],
        "contract_sha256": binding["contract_sha256"], "initialization_sha256": sha(init_path),
        "equivalence_receipt_sha256": sha(equivalence_path),
    }
    for key, expected in required.items():
        demand(protocol.get(key) == expected, f"control protocol differs: {key}")
    return {"sha256": hashlib.sha256(raw).hexdigest(), "bytes": raw}


def adaptation_supervision(model, batch, control):
    """Keep the inherited origin head; neutralize only unused random heads."""
    demand(control in CONTROLS, "unsupported adaptation control")
    available = {"origin": int((batch["y"] >= 0).sum()), "share": int(batch["share_mask"].sum()),
                 "mixed_fractional_share": int((batch["share_mask"] & (batch["share"] > 0) & (batch["share"] < 1)).sum()),
                 **{r: int((batch[r] >= 0).sum()) for r in shared.ROLES}}
    used = {k: v if ((k in shared.ROLES and control != "origin_only") or
                    (k == "origin" and control != "attribution_only") or
                    (k in ("share", "mixed_fractional_share") and control == "joint")) else 0
            for k, v in available.items()}
    if control == "joint":
        demand(all(v > 0 for v in used.values()), "joint warm start requires every head supervised")
        return available, used
    demand(used["origin"] > 0 if control == "origin_only" else used["human"] + used["ai"] > 0,
           "no training labels for selected adaptation control")
    for name, count in (("origin", used["origin"]), ("mixed_share", used["mixed_fractional_share"]),
                        *((r, used[r]) for r in shared.ROLES)):
        if not count:
            head = getattr(model, name)
            # A uniform singleton would pass the v1 >=1 threshold. The frozen
            # profile contract admits zero or >=2 names, never one identity.
            if name in shared.ROLES:
                demand(head.out_features != 1, "untrained attribution requires at least two frozen profiles")
            for parameter in head.parameters():
                if name != "origin":
                    with torch.no_grad():
                        parameter.zero_()
                parameter.requires_grad_(False)
    return available, used


def training_status(control, supervision, best_epoch):
    origin = "fine_tuned" if supervision["origin"] and best_epoch else "inherited_V6_initialization"
    if control == "attribution_only" and best_epoch:
        origin = "frozen_V6_head_on_adapted_encoding"
    return {"origin": origin,
            "share_interpolation": ("trained_fractional" if best_epoch else "untrained_initialization")
                if control == "joint" else "untrained_fixed_half",
            **{r: "trained" if supervision[r] and best_epoch else "untrained" for r in shared.ROLES}}


def finalize_adaptation(model, control, best_epoch):
    # If Development selects the initial checkpoint, neither random attribution
    # head is trained. Uniform logits make trained=False/threshold=1 a strict
    # fallback even when the adapted representation has very large coordinates.
    if control != "joint" and best_epoch == 0:
        with torch.no_grad():
            for role in shared.ROLES:
                demand(getattr(model, role).out_features > 1, "untrained attribution profile count")
                for parameter in getattr(model, role).parameters():
                    parameter.zero_()


def calibrate(model, cal, profiles, supervision, best_epoch):
    """No origin-label selection/calibration when origin adaptation is disabled."""
    calibrated = bool(supervision["origin"] and best_epoch > 0 and (cal["y"] >= 0).any())
    temperature = min([1.] + [float(torch.exp(torch.tensor(k * .1))) for k in range(-30, 31)],
                      key=lambda t: (shared.criterion(model, cal, t), abs(t - 1.))) if calibrated else 1.
    thresholds, reports = {}, {}
    with torch.no_grad():
        out = model(cal["x"])
        for role in shared.ROLES:
            scores, best_ids = out[f"{role}_logits"].softmax(-1).max(-1)
            named = [(profiles[role][i] if profiles[role] else None, s)
                     for i, s in zip(best_ids.tolist(), scores.tolist())]
            thresholds[role], reports[role] = shared.calibrate_named(named,
                [shared.role_label(r, role, profiles[role]) for r in cal["rows"]],
                [r["source_group"] for r in cal["rows"]], bool(supervision[role] and best_epoch > 0))
    return temperature, calibrated, thresholds, reports


def tensor(value, shape):
    demand(value["shape"] == list(shape), "tensor shape mismatch")
    values = value["values"]
    demand(len(values) == math.prod(shape) and
           all(type(v) in (int, float) and math.isfinite(v) for v in values), "invalid tensor values")
    return torch.tensor(values, dtype=torch.float32).reshape(shape)


def initializer(path):
    init = read(path)
    demand(init["schema"] == INIT_SCHEMA and init["dimension"] == 768 and
           init["origin_order"] == ORIGIN_ORDER and init["base_temperature"] == BASE_TEMPERATURE,
           "unsupported initializer")
    demand(init["original_dependency_and_reference_checks_passed"] is True and
           init["weight_file"] == "classifier/model.safetensors" and
           init["weight_file_sha256"] == "88bb0eda753ae3c6a9dc31c266e4ec24459937b77d07c27a609aa11ccdba36c0",
           "unqualified initializer provenance")
    demand(set(init["weights"]) == {"origin.weight", "origin.bias"}, "classifier-only initializer required")
    tensor(init["weights"]["origin.weight"], (3, 768))
    tensor(init["weights"]["origin.bias"], (3,))
    return init


class WarmStartA(nn.Module):
    def __init__(self, init, mean, scale, human_profiles, ai_profiles, input_mode):
        super().__init__()
        demand(input_mode in ("text", "combined"), "unsupported input mode")
        demand(len(mean) == len(scale) == 768 and all(math.isfinite(v) for v in mean) and
               all(math.isfinite(v) and v > 0 for v in scale), "invalid frozen normalization")
        self.input_mode = input_mode
        self.base_temperature = init["base_temperature"]
        self.register_buffer("text_mean", torch.tensor(mean, dtype=torch.float32))
        self.register_buffer("text_scale", torch.tensor(scale, dtype=torch.float32))
        self.residual = nn.Sequential(nn.Linear(2588, 64), nn.Tanh(), nn.Linear(64, 768))
        self.origin = nn.Linear(768, 3)
        self.mixed_share = nn.Linear(768, 1)
        self.human = nn.Linear(768, max(1, human_profiles))
        self.ai = nn.Linear(768, max(1, ai_profiles))
        with torch.no_grad():
            self.residual[2].weight.zero_()
            self.residual[2].bias.zero_()
            self.origin.weight.copy_(tensor(init["weights"]["origin.weight"], (3, 768)))
            self.origin.bias.copy_(tensor(init["weights"]["origin.bias"], (3,)))

    def forward(self, x, temperature=1.0):
        demand(x.ndim == 2 and x.shape[1] == 2588, "feature dimension mismatch")
        h, cfg = x[:, :768], x[:, 768:]
        if self.input_mode == "text":
            cfg = torch.zeros_like(cfg)
        u = torch.cat(((h - self.text_mean) / self.text_scale, cfg), dim=1)
        z = h + self.residual(u)
        logits = self.origin(z) / self.base_temperature
        p = (logits / temperature).softmax(-1)
        q = self.mixed_share(z).squeeze(-1).sigmoid()
        return {"encoding": z, "origin_logits": logits, "origin": p,
                "conditional_mixed_share": q, "share": p[:, 1] + p[:, 2] * q,
                "human_logits": self.human(z), "ai_logits": self.ai(z)}


def configure(device, seed):
    torch.set_num_threads(1)
    if torch.get_num_interop_threads() != 1:
        torch.set_num_interop_threads(1)
    torch.manual_seed(seed)
    torch.use_deterministic_algorithms(True)
    demand(device.type in ("cpu", "cuda"), "unsupported device")
    if device.type == "cuda":
        demand(torch.cuda.is_available(), "CUDA unavailable")
        demand(os.environ.get("CUBLAS_WORKSPACE_CONFIG") in (":4096:8", ":16:8"),
               "set CUBLAS_WORKSPACE_CONFIG before CUDA startup")
        # CUDA_VISIBLE_DEVICES selects the task GPU; an unindexed torch.device
        # is rejected by this particular API, unlike Tensor.to(device).
        torch.cuda.set_per_process_memory_fraction(.20)
        torch.backends.cuda.matmul.allow_tf32 = False
        torch.backends.cudnn.allow_tf32 = False
        torch.cuda.reset_peak_memory_stats()


def numeric(root):
    """Validate the four explicitly permitted files before any model work."""
    root = Path(root).resolve(strict=True)
    manifest, contract = read(root / "manifest.json"), read(root / "contract.json")
    demand(sha(root / "contract.json") == manifest["contract_file_sha256"], "changed contract")
    demand(contract["warm_start_schema"] == "slop_ninja_raw_v6_cfg_residual_input_v1" and
           contract["input_dimension"] == 2588 and contract["ablation"] in ("text", "combined"),
           "raw warm-start input contract required")
    demand(manifest["final_test_opened"] is False and manifest["test_used_for_selection"] is False,
           "Test inputs forbidden")
    splits = manifest["splits"]
    demand(len(splits) == 4 and {s["split"] for s in splits} == set(PARTS), "non-Test splits required")
    rows, files, ids, groups = {}, {}, set(), {}
    for part in PARTS:
        path = root / f"{part}.jsonl"
        binding = next(s for s in splits if s["split"] == part)
        files[path.name] = sha(path)
        demand(files[path.name] == binding["sha256"], "changed numeric split")
        rows[part] = [json.loads(line) for line in path.read_text().splitlines()]
        demand(len(rows[part]) == binding["rows"] and bool(rows[part]), "numeric row count")
        groups[part] = {r["source_group"] for r in rows[part]}
        for row in rows[part]:
            demand(row["id"] not in ids, "duplicate numeric id")
            ids.add(row["id"])
            demand(row["purpose"] == ("reference" if part == "reference" else "query") and
                   row["frozen_split"] == ("train" if part == "reference" else part), "purpose/partition mismatch")
            inp = row["input"]
            demand(set(inp) == {"contract_sha256", "x"} and
                   inp["contract_sha256"] == manifest["contract_sha256"], "numeric-only contract mismatch")
            demand(len(inp["x"]) == 2588 and all(type(v) in (int, float) and math.isfinite(v) for v in inp["x"]),
                   "invalid numeric features")
            if contract["ablation"] == "text":
                demand(all(v == 0 for v in inp["x"][768:]), "text mode CFG must be zero")
    demand(all(not groups[a] & groups[b] for i, a in enumerate(PARTS) for b in PARTS[i+1:]), "source family leakage")
    norm = contract["embedding_normalization"]
    demand(norm["fit"] == "train_query_only_equal_source_family_weight" and all(
        norm[k] is False for k in ("calibration_used_for_fit", "development_used_for_fit",
                                  "reference_used_for_fit", "labels_used_for_fit", "final_test_opened")),
        "normalization must remain frozen from Train queries")
    baseline_path = root / "baseline-origin-reference.jsonl"
    files[baseline_path.name] = sha(baseline_path)
    demand(files[baseline_path.name] == manifest["baseline_origin_reference_sha256"], "changed V6 baseline")
    baseline = [json.loads(line) for line in baseline_path.read_text().splitlines()]
    extension = manifest.get("training_extension")
    if extension:
        demand(manifest["dataset_manifest_file"] == "training-population.json", "population union binding required")
        files["training-population.json"] = sha(root / "training-population.json")
        demand(files["training-population.json"] == manifest["dataset_manifest_sha256"], "population union changed")
        for filename, key in (("extension-dataset-manifest.json", "extension_dataset_manifest_sha256"),
                              ("generation-protocol.md", "generation_protocol_sha256"),
                              ("fit-protocol.json", "protocol_sha256")):
            files[filename] = sha(root / filename)
            demand(files[filename] == extension[key], "Train admission snapshot changed")
        validate_admission(read(root / "fit-protocol.json"), read(root / "extension-dataset-manifest.json"),
                           extension, rows["train"][-extension["added_train_rows"]:])
    expected = validate_extension(manifest, contract, rows, baseline) if extension else EXPECTED_ROWS
    demand(len(baseline) == len(ids) == expected and {r["id"] for r in baseline} == ids,
           "complete manifest-bound baseline roster required" if extension else "complete 929-row baseline roster required")
    binding = {"root": str(root), "manifest_sha256": sha(root / "manifest.json"),
               "contract_file_sha256": sha(root / "contract.json"), "contract_sha256": manifest["contract_sha256"],
               "files_sha256": files, "row_count": expected, "train_extension": bool(extension)}
    return manifest, contract, rows, baseline, binding


def validate_admission(protocol, dataset, ext, added):
    demand(protocol["schema"] == SCALE_SCHEMA and dataset["schema"] == "slop_ninja_a_supervised_dataset_v1" and
           dataset["complete"] is True and dataset["train_extension_only"] is True and
           dataset["test_text_opened"] is False and dataset["test_ids"] == [], "unqualified Train admission")
    demand(protocol["selected_roots_manifest_sha256"] == dataset["source_manifest_sha256"] == ext["selected_roots_manifest_sha256"] and
           protocol["generation_protocol_sha256"] == ext["generation_protocol_sha256"], "frozen roots/generation protocol differs")
    roots, tasks = protocol["requested_new_roots"], protocol["requested_new_generations"]
    demand(type(roots) is int and roots > 0 and type(tasks) is int and tasks >= 0 and
           dataset["requested_human_records"] == roots and dataset["requested_generation_records"] == tasks and
           dataset["requested_records"] == roots + tasks and dataset["records"] == len(added), "admission counts differ")
    actual = {r["id"] for r in added}
    demand(len(actual) == len(added) == len(dataset["non_test_ids"]) and
           actual == set(dataset["non_test_ids"]), "admitted roster differs")
    demand(all(v == "train" for v in dataset["split_assignments"].values()) and all(
        r["frozen_split"] == "train" and r["purpose"] == "query" and
        dataset["split_assignments"].get(r["source_group"]) == "train" for r in added), "admission scope differs")
    requested = accepted = failed = 0
    accounted = set(actual)
    for report in dataset["generation_reports"]:
        a, n, f = (report[k] for k in ("accepted", "requested", "failed"))
        demand(report["complete"] is True and all(type(v) is int and v >= 0 for v in (a,n,f)) and
               a + f == n and len(report["failures"]) == f, "unaccounted generation tasks")
        for failure in report["failures"]:
            demand(failure["id"] not in accounted, "duplicate failure ID")
            accounted.add(failure["id"])
        requested += n; accepted += a; failed += f
    for exclusion in dataset["excluded"]:
        demand(exclusion["id"] not in accounted, "duplicate exclusion ID")
        accounted.add(exclusion["id"])
    demand(requested == tasks and dataset["accepted_before_exclusions"] == roots + accepted and
           roots + accepted == len(added) + len(dataset["excluded"]) and
           roots + tasks == len(added) + len(dataset["excluded"]) + failed, "incomplete admission accounting")


def validate_extension(manifest, contract, rows, baseline):
    """Validate numeric ML inputs; never load corpus text or new Test inputs."""
    ext = manifest["training_extension"]
    demand(ext == contract.get("training_extension") and ext["schema"] == SCALE_SCHEMA and
           ext["original_rows"] == EXPECTED_ROWS and type(ext["added_train_rows"]) is int and
           ext["added_train_rows"] > 0 and ext["total_rows"] == EXPECTED_ROWS + ext["added_train_rows"] and
           ext["original_numeric_values_and_targets_preserved"] is True and
           all(ext[k] is False for k in ("normalization_fitted", "profiles_changed", "final_test_opened")),
           "invalid Train extension binding")
    parent = Path(ext["parent_root"]).resolve(strict=True)
    demand(sha(parent / "manifest.json") == ext["parent_manifest_sha256"] and
           sha(parent / "contract.json") == ext["parent_contract_file_sha256"] and
           sha(parent / "population.json") == ext["parent_population_sha256"], "Train extension parent changed")
    pm, pc = read(parent / "manifest.json"), read(parent / "contract.json")
    demand(pm.get("training_extension") is None and pm["contract_sha256"] == ext["parent_contract_sha256"] and
           pm["contract_file_sha256"] == ext["parent_contract_file_sha256"] and
           pm["final_test_opened"] is False and pm["test_used_for_selection"] is False,
           "invalid original extension parent")
    original_contract = dict(contract)
    original_contract.pop("training_extension")
    original_contract["experiment"] = pc["experiment"]
    demand(original_contract == pc, "Train extension changed frozen feature contract")
    experiment = contract["experiment"]
    demand(experiment["schema"] == SCALE_SCHEMA and experiment["epochs"] == 250 and
           experiment["seeds"] == [17, 29, 41] and experiment["controls"] == ["text", "combined"] and
           experiment["protocol_sha256"] == ext["protocol_sha256"] and experiment["final_test_opened"] is False,
           "Train extension protocol mismatch")
    original_ids, original_groups, original_count = set(), set(), 0
    for part in PARTS:
        path = parent / f"{part}.jsonl"
        part_binding = next(s for s in pm["splits"] if s["split"] == part)
        declared = ext["original_partitions"][part]
        demand(sha(path) == part_binding["sha256"] == declared["file_sha256"], "parent numeric split changed")
        old = [json.loads(line) for line in path.read_text().splitlines()]
        demand(len(old) == part_binding["rows"] == declared["rows"], "parent row count changed")
        demand(len(rows[part]) == len(old) + (ext["added_train_rows"] if part == "train" else 0),
               "extension changed a protected partition")
        for prior, current in zip(old, rows[part]):
            restored = {**current, "input": {**current["input"], "contract_sha256": pm["contract_sha256"]}}
            demand(restored == prior, "extension changed original numeric values or targets")
            demand(prior["id"] not in original_ids, "duplicate original ID")
            original_ids.add(prior["id"])
            original_groups.add(prior["source_group"])
        original_count += len(old)
    demand(original_count == EXPECTED_ROWS, "extension parent population differs")
    added = [r for r in rows["train"] if r["id"] not in original_ids]
    demand(len(added) == ext["added_train_rows"] and
           all(r["source_group"] not in original_groups for r in added), "extension reused a source family")
    path = parent / "baseline-origin-reference.jsonl"
    demand(sha(path) == pm["baseline_origin_reference_sha256"], "parent baseline changed")
    old_baseline = [json.loads(line) for line in path.read_text().splitlines()]
    indexed = {r["id"]: r for r in baseline}
    demand(len(old_baseline) == EXPECTED_ROWS and all(indexed.get(r["id"]) == r for r in old_baseline),
           "extension changed original baseline predictions")
    return ext["total_rows"]


def make_model(init, contract, device):
    norm = contract["embedding_normalization"]
    return WarmStartA(init, norm["mean"], norm["scale"], len(contract["human_profiles"]),
                      len(contract["ai_profiles"]), contract["ablation"]).to(device)


@torch.no_grad()
def equivalence(model, rows, baseline, artifact_id, device):
    by_id = {r["id"]: r for part in PARTS for r in rows[part]}
    demand(len(by_id) == len(baseline) and {r["id"] for r in baseline} == set(by_id), "baseline ID mismatch")
    demand(all(r["artifact_id"] == artifact_id for r in baseline), "baseline artifact mismatch")
    expected = torch.tensor([r["probabilities"] for r in baseline], dtype=torch.float64, device=device)
    demand(expected.shape == (len(baseline), 3) and bool(torch.isfinite(expected).all()) and
           bool(((expected >= 0) & (expected <= 1)).all()) and
           bool((expected.sum(1) - 1).abs().le(1e-12).all()), "invalid baseline probabilities")
    x = torch.tensor([by_id[r["id"]]["input"]["x"] for r in baseline], dtype=torch.float32, device=device)
    model.eval()
    out = model(x)
    demand(torch.equal(out["encoding"], x[:, :768]), "initial residual is not identity")
    error = float((out["origin"].double() - expected).abs().max())
    demand(math.isfinite(error) and error <= TOLERANCE, f"initial V6 probability mismatch: {error} > {TOLERANCE}")
    return {"rows": len(baseline), "max_absolute_probability_error": error,
            "tolerance": TOLERANCE, "passed": True, "device": str(device)}


def implementation():
    return {"trainer_sha256": sha(__file__), "shared_helpers_sha256": sha(shared.__file__)}


def qualify(args):
    demand(not Path(args.out).exists(), "equivalence receipt exists")
    configure(torch.device("cpu"), 17)
    init, results, bindings, norms, rosters = initializer(args.init), {}, {}, [], []
    for mode in ("text", "combined"):
        _, contract, rows, baseline, binding = numeric(getattr(args, mode))
        demand(contract["ablation"] == mode, "modality mismatch")
        model = make_model(init, contract, torch.device("cpu"))
        results[mode] = equivalence(model, rows, baseline, init["artifact_id"], torch.device("cpu"))
        bindings[mode] = binding
        norms.append(contract["embedding_normalization"])
        rosters.append([(r["id"], r["text_sha256"], r["probabilities"]) for r in baseline])
    demand(norms[0] == norms[1] and rosters[0] == rosters[1], "unmatched modality normalization/baseline")
    demand(bindings["text"]["row_count"] == bindings["combined"]["row_count"] and
           bindings["text"]["train_extension"] == bindings["combined"]["train_extension"], "unmatched extension population")
    write(args.out, {"schema": SCALE_GATE_SCHEMA if bindings["text"]["train_extension"] else GATE_SCHEMA,
                    "initialization_sha256": sha(args.init),
                    "implementations": implementation(), "bindings": bindings, "results": results,
                    "rows": bindings["text"]["row_count"], "passed": True, "fitting_performed": False,
                    "labels_used_for_equivalence": False, "final_test_opened": False})
    print(json.dumps(results), flush=True)


def validate_gate(path, init_path, binding, mode):
    gate = read(path)
    expected_rows = binding["row_count"]
    schema = SCALE_GATE_SCHEMA if binding["train_extension"] else GATE_SCHEMA
    demand(gate["schema"] == schema and gate["passed"] is True and gate["rows"] == expected_rows and
           gate["final_test_opened"] is False and gate["initialization_sha256"] == sha(init_path) and
           gate["implementations"] == implementation(), "stale/failed equivalence receipt")
    demand(set(gate["results"]) == {"text", "combined"}, "both candidates must qualify")
    for result in gate["results"].values():
        demand(result["passed"] is True and result["rows"] == expected_rows and
               result["tolerance"] == TOLERANCE and 0 <= result["max_absolute_probability_error"] <= TOLERANCE,
               "incomplete/failed equivalence gate")
    demand(gate["bindings"][mode] == binding, "numeric inputs changed after equivalence")
    for peer in gate["bindings"].values():
        root = Path(peer["root"])
        for filename, expected in {"manifest.json": peer["manifest_sha256"],
                                   "contract.json": peer["contract_file_sha256"], **peer["files_sha256"]}.items():
            demand(filename in {"manifest.json", "contract.json", "baseline-origin-reference.jsonl",
                                 "training-population.json",
                                 "extension-dataset-manifest.json", "generation-protocol.md", "fit-protocol.json",
                                 *(f"{p}.jsonl" for p in PARTS)}, "unexpected gate file")
            demand(sha(root / filename) == expected, "peer inputs changed after equivalence")


def artifact(model, contract, manifest, init_path, temperature, profiles, thresholds, supervision,
             control="joint", protocol_sha256=None):
    demand(control in CONTROLS and (control == "joint" or
           (isinstance(protocol_sha256, str) and len(protocol_sha256) == 64 and
            all(c in "0123456789abcdef" for c in protocol_sha256))), "missing control protocol binding")
    return {"schema": SCHEMA, "architecture": {"dimension": 2588, "raw_text_dimension": 768,
            "hidden": 64, "embedding": 768, "human_profiles": len(profiles["human"]),
            "ai_profiles": len(profiles["ai"])}, "operations": OPERATIONS,
            "input_mode": contract["ablation"], "contract_sha256": manifest["contract_sha256"],
            "initialization_sha256": sha(init_path), "base_temperature": model.base_temperature,
            "temperature": temperature, "text_mean": model.text_mean.tolist(), "text_scale": model.text_scale.tolist(),
            "profiles": profiles, "thresholds": thresholds, "supervision": supervision,
            "origin_order": ORIGIN_ORDER, "control": control,
            **({"control_protocol_sha256": protocol_sha256} if control != "joint" else {}),
            "weights": {k: {"shape": list(v.shape), "values": v.detach().cpu().flatten().tolist()}
                        for k, v in model.named_parameters()}}


def cohort_mixture(path, manifest, binding, mode, rows, init_path):
    """Training loss weights only; numeric features and held-out weights are unchanged.

    numeric() has already verified the immutable original-prefix rows against
    their parent, including every feature/target and disjoint added families.
    """
    protocol = read(path)
    ext = manifest.get("training_extension")
    demand(ext is not None and binding["train_extension"], "mixture requires verified Train extension")
    demand(protocol["schema"] == MIXTURE_SCHEMA and
           protocol["original_cohort_mass"] == .5 and
           protocol["within_cohort"] == "family_origin_balanced" and
           protocol["apply_to"] == "train_only" and protocol["epochs"] == 250 and
           protocol["seeds"] == [17, 29, 41] and protocol["controls"] == ["text", "combined"] and
           protocol["final_test_opened"] is False, "unsupported cohort mixture protocol")
    demand(protocol["numeric_manifest_sha256"][mode] == binding["manifest_sha256"] and
           protocol["parent_manifest_sha256"][mode] == ext["parent_manifest_sha256"] and
           protocol["initialization_sha256"] == sha(init_path), "mixture input binding differs")
    n = ext["original_partitions"]["train"]["rows"]
    demand(type(n) is int and 0 < n < len(rows) and len(rows) - n == ext["added_train_rows"],
           "invalid mixture cohort sizes")
    cohorts = (rows[:n], rows[n:])
    groups = [{r["source_group"] for r in cohort} for cohort in cohorts]
    demand(not groups[0] & groups[1] and len({r["id"] for r in rows}) == len(rows) and all(
        r["purpose"] == "query" and r["frozen_split"] == "train" for r in rows),
        "mixture must contain disjoint Train query families")
    weights = [w * .5 for cohort in cohorts for w in shared.query_weights(cohort, "family_origin_balanced")]
    demand(all(math.isfinite(w) and w > 0 for w in weights) and
           abs(sum(weights[:n]) - .5) < 1e-10 and abs(sum(weights[n:]) - .5) < 1e-10,
           "cohort mass invariant failed")
    # Each supervised loss renormalizes after masking. Record the resulting
    # cohort shares, especially human identity, absent from the added sources.
    supervision_mass = {}
    for target in ("origin", "share", "human", "ai"):
        masses = [sum(weights[i] for i in indices if rows[i]["targets"].get(target) is not None)
                  for indices in (range(n), range(n, len(rows)))]
        total = sum(masses)
        supervision_mass[target] = {"original": masses[0] / total if total else None,
                                    "added": masses[1] / total if total else None}
    return weights, {"schema": MIXTURE_SCHEMA, "protocol_sha256": sha(path),
                     "original_rows": n, "added_rows": len(rows) - n,
                     "original_families": len(groups[0]), "added_families": len(groups[1]),
                     "original_cohort_mass": .5, "added_cohort_mass": .5,
                     "within_cohort": "family_origin_balanced", "apply_to": "train_only",
                     "original_mass_under_uniform_families": len(groups[0]) / sum(map(len, groups)),
                     "masked_task_cohort_mass": supervision_mass,
                     "weights_sha256": hashlib.sha256(json.dumps(weights, separators=(",", ":")).encode()).hexdigest(),
                     "normalization_changed": False, "development_calibration_weights_changed": False,
                     "final_test_opened": False}


def source_size_sampling(path, manifest, binding, mode, batch, init_path):
    """Consume Rust's frozen numeric-ID selection; never discover/read corpus text."""
    p = read(path)
    ext = manifest.get("training_extension")
    demand(ext is not None and binding["train_extension"], "source-size comparison requires extension")
    required = {"schema": SOURCE_SIZE_SCHEMA, "apply_to": "train_only", "epochs": 250,
                "seeds": [17, 29, 41], "source_strata": list(SOURCE_STRATA),
                "original_cohort_mass": .5, "added_stratum_mass": .1,
                "within_stratum": "family_origin_balanced", "normalization_fitted": False,
                "profiles_changed": False, "final_test_opened": False,
                "rank_domain": "slop_ninja_a_source_size_curve_v1"}
    demand(all(p.get(k) == v for k, v in required.items()), "unsupported source-size protocol")
    level = p.get("level")
    demand(level in ("12", "24", "all") and
           p["allowed_modes"] == (["text", "combined"] if level == "all" else ["text"]) and
           mode in p["allowed_modes"], "unplanned source-size fit")
    demand(p["numeric_manifest_sha256"][mode] == binding["manifest_sha256"] and
           p["parent_manifest_sha256"][mode] == ext["parent_manifest_sha256"] and
           p["extension_dataset_manifest_sha256"] == ext["extension_dataset_manifest_sha256"] and
           p["initialization_sha256"] == sha(init_path), "source-size input binding differs")
    rows, selections = batch["rows"], p["rows"]
    n = ext["original_partitions"]["train"]["rows"]
    demand(type(n) is int and 0 < n < len(rows) and p["original_rows"] == n and
           p["total_train_rows"] == len(rows) and len(selections) == len(rows), "source-size row counts")
    families, strata = {}, ("original", *SOURCE_STRATA)
    for i, (r, q) in enumerate(zip(rows, selections)):
        demand(q["id"] == r["id"] and q["source_group"] == r["source_group"] and
               r["frozen_split"] == "train" and r["purpose"] == "query" and
               q["stratum"] in strata and type(q["selected"]) is bool and
               ((q["stratum"] == "original") == (i < n)), "source-size row scope")
        g, st = r["source_group"], q["stratum"]
        demand(families.setdefault(g, st) == st, "family assigned to multiple strata")
    chosen = {g for g, st in families.items() if st == "original"}
    for st in SOURCE_STRATA:
        groups = sorted((g for g, value in families.items() if value == st),
                        key=lambda g: (hashlib.sha256((p["rank_domain"] + "\0" + g).encode()).hexdigest(), g))
        count = len(groups) if level == "all" else int(level)
        demand(0 < count <= len(groups), "source stratum shortfall")
        chosen.update(groups[:count])
    demand(set(p["selected_added_families"]) == {g for g in chosen if families[g] != "original"},
           "selected added family roster")
    expected = [0.] * len(rows)
    for st in strata:
        indices = [i for i, r in enumerate(rows) if families[r["source_group"]] == st and r["source_group"] in chosen]
        weights = shared.query_weights([rows[i] for i in indices], "family_origin_balanced")
        for i, weight in zip(indices, weights):
            expected[i] = weight * (.5 if st == "original" else .1)
    for q, r, w in zip(selections, rows, expected):
        demand(q["selected"] == (r["source_group"] in chosen) and type(q["weight"]) in (int, float) and
               math.isfinite(q["weight"]) and abs(q["weight"] - w) < 1e-12, "source-size weight differs")
    indices = [i for i, w in enumerate(expected) if w > 0]
    selected_batch = {k: ([v[i] for i in indices] if k == "rows" else v[indices])
                      for k, v in batch.items()}
    selected_batch["weight"] = torch.tensor([expected[i] for i in indices], device=batch["x"].device,
                                             dtype=batch["weight"].dtype)
    masked = {}
    for target in ("origin", "share", "human", "ai"):
        masses = {st: sum(expected[i] for i in indices if selections[i]["stratum"] == st and
                         rows[i]["targets"].get(target) is not None) for st in strata}
        total = sum(masses.values())
        masked[target] = {st: value / total if total else None for st, value in masses.items()}
    return selected_batch, {"schema": SOURCE_SIZE_SCHEMA, "level": level, "protocol_sha256": sha(path),
                            "selected_train_rows": len(indices), "selected_train_families": len(chosen),
                            "available_train_rows": len(rows), "masked_task_stratum_mass": masked,
                            "unselected_rows_used_for_optimizer": False, "normalization_changed": False,
                            "development_calibration_weights_changed": False, "final_test_opened": False}


def train(args):
    demand(args.seed in (17, 29, 41), "only frozen seeds17/29/41")
    control = getattr(args, "control", "joint")
    protocol_path = getattr(args, "control_protocol", None)
    demand(control in CONTROLS and (control == "joint" or protocol_path is not None),
           "nonjoint adaptation requires --control-protocol")
    demand(control == "joint" or not getattr(args, "mixture_protocol", None),
           "cohort-mixture adaptation controls require a separately qualified protocol")
    sampling_path = getattr(args, "sampling_protocol", None)
    demand(not sampling_path or (control == "joint" and not getattr(args, "mixture_protocol", None)),
           "source-size sampling is a distinct joint-only weighting protocol")
    root, output = Path(args.data), Path(args.out)
    manifest, contract, rows, baseline, binding = numeric(root)
    validate_gate(args.equivalence, args.init, binding, contract["ablation"])
    protocol = control_protocol(protocol_path, control, contract, binding, args.init, args.equivalence)
    device = torch.device(args.device)
    configure(device, args.seed)
    init = initializer(args.init)
    model = make_model(init, contract, device)
    # Check the actual training device before constructing the optimizer.
    initial_check = equivalence(model, rows, baseline, init["artifact_id"], device)
    profiles = {r: contract[f"{r}_profiles"] for r in shared.ROLES}
    demand(all(len(v) == len(set(v)) and all(isinstance(n, str) and n and not n.startswith("other_") for n in v)
               for v in profiles.values()), "invalid profile roster")
    batches = {p: shared.dataset(root, p, manifest, device, profiles, "family_origin_balanced") for p in PARTS[:3]}
    mixture = None
    if getattr(args, "mixture_protocol", None):
        weights, mixture = cohort_mixture(args.mixture_protocol, manifest, binding,
                                         contract["ablation"], rows["train"], args.init)
        batches["train"]["weight"] = torch.tensor(weights, device=device,
                                                   dtype=batches["train"]["weight"].dtype)
    sampling = None
    if sampling_path:
        batches["train"], sampling = source_size_sampling(sampling_path, manifest, binding,
                                                         contract["ablation"], batches["train"], args.init)
    for role in shared.ROLES:
        demand(set(batches["train"][role].tolist()) - {-1} == set(range(len(profiles[role]))),
               "profiles must come from supervised Train identities")
    b = batches["train"]
    available, supervision = adaptation_supervision(model, b, control)
    output.mkdir(exist_ok=False)
    if protocol:
        with (output / "control-protocol.json").open("xb") as f:
            f.write(protocol["bytes"])
            f.flush()
            os.fsync(f.fileno())
    write(output / "initial-equivalence.json", initial_check)
    if sampling:
        with (output / "sampling-protocol.json").open("xb") as handle:
            handle.write(Path(sampling_path).read_bytes())
        demand(sha(output / "sampling-protocol.json") == sampling["protocol_sha256"],
               "sampling protocol changed during admission")
        write(output / "source-size-sampling.json", sampling)
    if mixture:
        with (output / "mixture-protocol.json").open("xb") as handle:
            handle.write(Path(args.mixture_protocol).read_bytes())
        demand(sha(output / "mixture-protocol.json") == mixture["protocol_sha256"],
               "mixture protocol changed during admission")
        write(output / "cohort-mixture.json", mixture)
    optimizer = torch.optim.AdamW(model.parameters(), lr=.001, weight_decay=.01)
    best, best_epoch, state = float("inf"), 0, None
    start, history = time.perf_counter(), []
    selection = "conditional_supervised_ce" if control == "attribution_only" else "involvement_brier"
    for epoch in range(251):
        if epoch:
            model.train()
            optimizer.zero_grad(set_to_none=True)
            objective = shared.loss(model, b, control)
            demand(bool(torch.isfinite(objective)), "nonfinite training objective")
            objective.backward()
            torch.nn.utils.clip_grad_norm_(model.parameters(), 5.)
            optimizer.step()
        model.eval()
        score = shared.criterion(model, batches["development"], control=control)
        demand(math.isfinite(score), "nonfinite Development score")
        if score < best - 1e-9:
            best, best_epoch = score, epoch
            state = {k: v.detach().cpu().clone() for k, v in model.state_dict().items()}
        if epoch % 25 == 0:
            history.append({"epoch": epoch, f"development_{selection}": score,
                            f"train_{selection}": shared.criterion(model, b, control=control),
                            f"train_{'joint' if control == 'joint' else control}_loss":
                                float(shared.loss(model, b, control).detach())})
            torch.save({"epoch": epoch, "model": model.state_dict(), "optimizer": optimizer.state_dict(),
                        "best_epoch": best_epoch, "best": state}, output / "recovery.pt.tmp")
            os.replace(output / "recovery.pt.tmp", output / "recovery.pt")
            print(json.dumps(history[-1]), flush=True)
    model.load_state_dict(state)
    if control != "joint":
        finalize_adaptation(model, control, best_epoch)
        state = {k: v.detach().cpu().clone() for k, v in model.state_dict().items()}
    cal = batches["calibration"]
    temperature, calibrated, thresholds, reports = calibrate(model, cal, profiles, supervision, best_epoch)
    write(output / "weights.json", artifact(model, contract, manifest, args.init, temperature, profiles, thresholds,
          supervision, control, protocol["sha256"] if protocol else None))
    torch.save({"state_dict": state, "temperature": temperature, "contract_sha256": manifest["contract_sha256"]}, output / "model.pt")
    for part, batch in batches.items():
        preds = shared.predictions(model, [r["input"] for r in batch["rows"]], temperature,
            manifest["contract_sha256"], profiles, thresholds, device, True)
        write(output / f"{part}-predictions.json", [{"id": r["id"], "prediction": p} for r, p in zip(batch["rows"], preds)])
    refs = rows["reference"]
    preds = shared.predictions(model, [r["input"] for r in refs], temperature,
        manifest["contract_sha256"], profiles, thresholds, device, True)
    write(output / "reference-encodings.json", {"schema": shared.ENCODING_SCHEMA,
          "contract_sha256": manifest["contract_sha256"], "weights_sha256": sha(output / "weights.json"),
          "reference_sha256": binding["files_sha256"]["reference.jsonl"],
          "rows": [{"id": r["id"], "encoding": p["encoding"]} for r, p in zip(refs, preds)]})
    write(output / "fit.json", {"schema": "slop_ninja_v6_residual_a_fit_v1", "seed": args.seed,
          "control": control, "epochs": 250, "selected_epoch": best_epoch,
          "query_weighting": "family_origin_balanced", "task_weights": shared.task_weights(control),
          "cohort_mixture": mixture,
          "source_size_sampling": sampling,
          "development_selection": {"metric": selection, "value": best, "split": "development"},
          "development_selection_brier_uncalibrated": best if control != "attribution_only" else None,
          "calibration_temperature": temperature,
          "origin_calibrated": calibrated, "conditional_named_calibration": reports,
          "base_temperature": BASE_TEMPERATURE, "supervision": supervision, "available_train_supervision": available,
          "training_status": training_status(control, supervision, best_epoch),
          **({"control_protocol_sha256": protocol["sha256"], "adaptation_control": True,
              "outputs_diagnostic_only": True, "control_based_test_selection": False,
              "initialization_scope": "frozen V6 already origin-supervised; not origin-free pretraining"}
             if protocol else {}),
          "history": history, "elapsed_seconds": time.perf_counter() - start,
          "device": str(device), "torch": torch.__version__, "cuda": torch.version.cuda,
          "cuda_peak_allocated_bytes": torch.cuda.max_memory_allocated(device) if device.type == "cuda" else 0,
          "cuda_memory_fraction_limit": .20, "implementations": implementation(),
          "initialization_sha256": sha(args.init), "equivalence_receipt_sha256": sha(args.equivalence),
          "initial_equivalence": initial_check, "input_binding": binding,
          "data_manifest_sha256": sha(root / "manifest.json"), "weights_sha256": sha(output / "weights.json"),
          "checkpoint_sha256": sha(output / "model.pt"),
          "encoding_export": {"enabled": True, "schema": shared.ENCODING_SCHEMA,
                              "reference_sha256": binding["files_sha256"]["reference.jsonl"],
                              "reference_used_for_fit_or_normalization": False},
          "limitations": ["pretraining exposure unknown; historical author labels are proxies",
                          "untrained initial attribution/share heads are diagnostics only",
                          "named confidence does not assert role presence"],
          "final_test_opened": False, "clean_holdout": False})
    print(json.dumps({"out": str(output), "selected_epoch": best_epoch}), flush=True)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    q = commands.add_parser("qualify")
    q.add_argument("--text", required=True)
    q.add_argument("--combined", required=True)
    t = commands.add_parser("train")
    t.add_argument("--data", required=True)
    t.add_argument("--equivalence", required=True)
    t.add_argument("--seed", required=True, type=int, choices=(17, 29, 41))
    t.add_argument("--device", default="cuda", choices=("cpu", "cuda"))
    t.add_argument("--mixture-protocol", help="Frozen, input-bound 50/50 original/added Train loss weighting")
    t.add_argument("--sampling-protocol", help="Rust-prepared nested source-family selection and fixed stratum weights")
    t.add_argument("--control", default="joint", choices=CONTROLS)
    t.add_argument("--control-protocol", help="required separate frozen admission protocol for nonjoint adaptation")
    for p in (q, t):
        p.add_argument("--init", required=True)
        p.add_argument("--out", required=True)
    args = parser.parse_args(argv)
    (qualify if args.command == "qualify" else train)(args)


if __name__ == "__main__":
    main()

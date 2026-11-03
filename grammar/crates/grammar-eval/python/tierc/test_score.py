"""Uncalibrated raw predictions of a frozen A fit on the sealed Test scoring
rows (Tier C), through the same model construction and forward as the fit's
CAL raw run:

  faux  export_full_a.py raw   (completed full-A v2 fit; release trainer)
  v6    rc-work/tools/v6_raw.py (untrained full-A start, evaluation 0)
  s3a   tierc/stage3_raw.py     (frozen stage-3 adapted A; reference replay first)
  e0    tierc/train_e0_a.py raw (E0 control; Dev-iter full-text vs prefix replay first)

The fit stays bound to the trainer bundle it was trained on (--data); the
rows scored come from a scoring-rows directory (`slop_ninja_a_assemble
scoring-rows`: the trainer bundle's row projection of one build split), read
by TestPopulation, which tokenizes and labels each row exactly as
full_a_bundle.Population does for an evaluation split. Execution failures do
not stop the run: an input over 2,048 encoder tokens (or empty), a crash of
one document's forward, or a non-finite output is written to failures.jsonl
as {"id", "failure"} and the document is absent from raw.jsonl and
predictions.jsonl (a_comp_test_calibrate turns it into a failure line).

Test rows are scored only after the written seal: --seal and --seal-sha256
must match and SEAL.json must bind this file's sha256. CAL scoring rows are
accepted only as the mechanical identity check (their manifest gate), and the
result must equal the fit's CAL raw run byte for byte. Run with the release
trainer modules (grammar-eval/python at 2af4baa5) and this tierc directory
on PYTHONPATH:
  test_score.py KIND --data TB --rows SCORING_ROWS --out OUT [--seal S --seal-sha256 H] KIND-ARGS
"""
from __future__ import annotations
import argparse
import hashlib
import json
import math
import os
import sys
from array import array
from pathlib import Path

if len(sys.argv) > 1 and sys.argv[1] == "faux":
    # As export_full_a.py, which sets these before importing torch.
    os.environ.setdefault("MKL_CBWR", "COMPATIBLE")
    os.environ.setdefault("ATEN_CPU_CAPABILITY", "default")

import numpy as np
import torch
from safetensors.torch import load_file

import full_a, full_a_bundle as bundle, shared_a as shared, train_full_a as trainer, v6_embeddings as bridge
import last_block_a as base
from warm_start_a import WarmStartA, initializer

SCHEMA = "slop_ninja_a_comp_test_raw_v1"
SCORING_ROWS_SCHEMA = "slop_ninja_a_scoring_rows_v1"
LOGIT_SEMANTICS = "log(p_model+p_mixed)-log(p_human) of the final uncalibrated origin"
OVERLENGTH = "input_over_2048_encoder_tokens"
MAX_CONSECUTIVE_CRASHES = 3
V6_UNTRAINED_HEAD_SEED = 0


def read(p):
    return json.loads(Path(p).read_bytes())


def sha256_file(p):
    return bridge.sha256(p)


# ------------------------------------------------------------------ rows

class TestPopulation:
    """One split of a scoring-rows file, tokenized as full_a_bundle.Population
    tokenizes an evaluation split (cohort_weighted=False); rows whose input is
    unsupported are recorded in `failures` instead of stopping the run."""

    batch = bundle.Population.batch

    def __init__(self, root, split, tokenize, special_ids, ai_profiles):
        self.split = bundle.canonical_split(split)
        self.ai_profiles = list(ai_profiles)
        self.ai_index = {k: i for i, k in enumerate(self.ai_profiles)}
        self.meta, self.failures, self.unrostered, self.order = [], [], {}, []
        ids, labels, flags = array("i"), array("b"), array("B")
        starts = array("q", [0])
        texts = bundle.TextSource(root)
        seen = set()
        try:
            with open(Path(root) / bundle.ROWS, "rb") as f:
                for n, line in enumerate(f, 1):
                    if not line.strip():
                        continue
                    m, raw = bundle.parse_row(json.loads(line), n)
                    shared.demand(m.split == self.split, f"row {m.id}: split {m.split}, not {self.split}")
                    shared.demand(m.id not in seen, f"duplicate id {m.id}")
                    seen.add(m.id)
                    self.order.append(m.id)
                    if m.label is not None and m.label not in self.ai_index:
                        self.unrostered[m.label] = self.unrostered.get(m.label, 0) + 1
                    text = texts.text(raw, m)
                    try:
                        tok_ids, offsets = tokenize(text)
                    except ValueError as e:
                        msg = str(e)
                        if msg.startswith("overlength input unsupported"):
                            self.failures.append({"id": m.id, "failure": OVERLENGTH})
                            continue
                        if msg.startswith("empty input unsupported"):
                            self.failures.append({"id": m.id, "failure": "empty_input_unsupported"})
                            continue
                        raise
                    lab = full_a.token_labels(text, tok_ids, offsets, special_ids, m.origin, m.spans)
                    content = full_a.content_mask_for(tok_ids, special_ids)
                    words = full_a.word_mask_for(text, tok_ids, offsets, special_ids)
                    ids.extend(tok_ids)
                    labels.extend(lab)
                    flags.extend((bundle.FLAG_CONTENT if c else 0) | (bundle.FLAG_WORD if w else 0)
                                 for c, w in zip(content, words))
                    starts.append(starts[-1] + len(tok_ids))
                    m.spans = None
                    self.meta.append(m)
                    del text, raw
        finally:
            texts.close()
        shared.demand(self.order, f"empty {self.split} rows")
        self.ids = np.frombuffer(ids, dtype=np.int32).copy()
        self.labels = np.frombuffer(labels, dtype=np.int8).copy()
        self.flags = np.frombuffer(flags, dtype=np.uint8).copy()
        self.starts = np.frombuffer(starts, dtype=np.int64).copy()
        self.lengths = np.diff(self.starts)

    def __len__(self):
        return len(self.meta)


def finite(v):
    if isinstance(v, float):
        return math.isfinite(v)
    if isinstance(v, dict):
        return all(finite(x) for x in v.values())
    if isinstance(v, (list, tuple)):
        return all(finite(x) for x in v)
    return True


def gate(args, rows_dir):
    """Test after the written seal (binding this file); CAL only as the mechanical check."""
    m = read(rows_dir / "MANIFEST.json")
    shared.demand(m["schema"] == SCORING_ROWS_SCHEMA, "scoring-rows directory required")
    rows_sha = sha256_file(rows_dir / bundle.ROWS)
    shared.demand(m["files"]["rows.jsonl"] == rows_sha, "scoring rows differ from their manifest")
    split = m["split"]
    if split == "test":
        shared.demand(args.seal is not None and args.seal_sha256 is not None, "Test needs --seal and --seal-sha256")
        seal = Path(args.seal)
        shared.demand(seal.is_file(), "the seal does not exist: Test is sealed")
        b = seal.read_bytes()
        shared.demand(hashlib.sha256(b).hexdigest() == args.seal_sha256, "SEAL.json differs from the stated sha256")
        shared.demand(json.loads(b).get("sealed") is True, "the seal does not say sealed")
        me = sha256_file(__file__)
        shared.demand(me in b.decode("utf-8", "replace"), f"the seal does not bind this scorer ({me})")
        shared.demand(m["gate"]["seal"]["sha256"] == args.seal_sha256, "scoring rows made under another seal")
        return split, rows_sha, {"path": str(seal), "sha256": args.seal_sha256, "scorer_sha256_bound": me}
    shared.demand(split == "cal" and m["gate"]["purpose"] == "mechanical_identity_check_of_the_test_scoring_path",
                  "only Test, or CAL for the mechanical identity check, is scored here")
    return split, rows_sha, None


# ------------------------------------------------------------------ models (as each CAL raw path)

def faux_model(args, device):
    import export_full_a as exporter
    root, config, fit = (Path(p).resolve() for p in (args.data, args.config, args.fit))
    digests, report, binding, saved = exporter.completed_fit(root, config, fit, args.init)
    trainer.configure(device, trainer.validate_protocol(read(config / "protocol.json"))["cuda_memory_fraction"])
    model, tokenizer, contract, protocol, arm, profiles = exporter.restore_model(args, root, config, binding, saved, device)
    info = {"system": "full-A v2 fit (export_full_a.py raw path)", "arm": binding["arm"], "seed": binding["seed"],
            "share_mode": arm["share_mode"], "bundle_sha256": digests, "contract_sha256": binding["contract_sha256"],
            "fit_sha256": bridge.sha256(fit / "fit.json"), "selected_checkpoint_sha256": report["selected_checkpoint_sha256"],
            "exporter_sha256": bridge.sha256(exporter.__file__)}
    return model, tokenizer, profiles["ai"], info


def v6_model(args, device):
    root, config = Path(args.data).resolve(), Path(args.config).resolve()
    digests = bundle.bundle_digests(root)
    contract = trainer.validate_contract(read(config / "contract.json"), digests[bundle.MANIFEST])
    protocol = trainer.validate_protocol(read(config / "protocol.json"))
    arm_name = next(iter(protocol["arms"]))
    arm = protocol["arms"][arm_name]
    trainer.configure(device, protocol["cuda_memory_fraction"])
    a = type("A", (), {"artifact": args.artifact})()
    encoder, tokenizer, parent = trainer.load(a, protocol, device)
    shared.demand(parent["artifact_id"] == contract["parent_artifact_id"], "parent differs")
    # v6_raw.py builds without a seed: the warm-start origin path is deterministic, but the
    # untrained share, AI and human heads are random draws. Seed them so this run repeats;
    # involvement (origin logits) is unaffected, and its CAL map reads only the origin.
    torch.manual_seed(V6_UNTRAINED_HEAD_SEED)
    model, profiles = trainer.build(encoder, contract, initializer(args.init), arm, protocol, device)
    model.token_loss_weight = 0.
    model.eval()
    info = {"system": "V6 (untrained full-A start, evaluation 0; v6_raw.py path)", "arm": arm_name, "seed": 0,
            "untrained_heads_seed": V6_UNTRAINED_HEAD_SEED,
            "untrained_heads_note": "share, AI and human heads are untrained random initialisations; CAL used an unseeded draw",
            "share_mode": arm["share_mode"], "bundle_sha256": digests,
            "contract_sha256": bridge.sha256(config / "contract.json"), "init_sha256": bridge.sha256(args.init)}
    return model, tokenizer, profiles["ai"], info


def s3a_model(args, device):
    root, fit, export = Path(args.data).resolve(), Path(args.fit).resolve(), Path(args.export).resolve()
    digests = bundle.bundle_digests(root)
    report = read(fit / "fit.json")
    b = report["binding"]
    shared.demand(report["schema"] == "slop_ninja_last_block_a_fit_v1" and report["complete"] is True and
                  report["final_test_opened"] is False and b["condition"] == "adapt_last_block",
                  "complete adapted stage-3 fit required")
    shared.demand(bridge.sha256(fit / "last-layer.safetensors") == report["last_layer_sha256"] and
                  bridge.sha256(fit / "shared-heads.safetensors") == report["shared_heads_sha256"] and
                  bridge.sha256(fit / "selected.pt") == report["selected_checkpoint_sha256"],
                  "stage-3 weights differ from fit.json")
    contract = read(args.s3contract)
    shared.demand(bridge.sha256(args.s3contract) == b["contract_sha256"], "stage-3 contract differs from the fit binding")
    shared.demand(bridge.sha256(args.init) == b["initializer_sha256"], "initializer differs from the fit binding")
    sel = read(args.selection)
    shared.demand(sel["schema"] == "slop_ninja_last_block_condition_selection_v1" and sel["complete"] is True and
                  sel["selected_condition"] == "adapt_last_block" and sel["calibration_read_for_selection"] is False and
                  sel["final_test_opened"] is False, "sealed stage-3 Development selection required")
    emanifest = read(export / "manifest.json")
    shared.demand(emanifest["fit_binding"] == b and emanifest["last_layer"]["sha256"] == report["last_layer_sha256"],
                  "stage-3 export is not of this fit")
    trainer.configure(device, 0.45)
    encoder, tokenizer, parent, _ = base.load_encoder(args.artifact, device)
    shared.demand(parent["artifact_id"] == contract["parent_artifact_id"] == b["parent_artifact_id"], "parent differs")
    norm = contract["embedding_normalization"]
    profiles = {k: list(contract[k + "_profiles"]) for k in ("human", "ai")}
    heads = WarmStartA(initializer(args.init), norm["mean"], norm["scale"], len(profiles["human"]), len(profiles["ai"]), "text")
    encoder.model.layers[base.LAST_LAYER].load_state_dict(load_file(str(fit / "last-layer.safetensors")), strict=True)
    heads.load_state_dict(load_file(str(fit / "shared-heads.safetensors")), strict=True)
    model = full_a.FullA(encoder, heads, None, "head", None).to(device)
    model.token_loss_weight = 0.
    model.eval()
    qual = []
    refs = {r["id"]: r for r in read(export / "reference-predictions.json")}
    with torch.no_grad():
        for line in (export / "reference-requests.jsonl").read_text().splitlines():
            q = json.loads(line)
            ref = refs[q["id"]]
            ids = tokenizer(q["text"], add_special_tokens=True, truncation=False, padding=False)["input_ids"]
            shared.demand(ids == ref["token_ids"], f"reference {q['id']}: token ids differ")
            t = torch.tensor([ids], dtype=torch.long, device=device)
            o = model(t, torch.ones_like(t), torch.ones_like(t, dtype=torch.bool), autocast=False)
            eh = float((o["h"][0].cpu().double() - torch.tensor(ref["h_raw_f32"], dtype=torch.float64)).abs().max())
            ez = float((o["encoding"][0].cpu().double() - torch.tensor(ref["prediction"]["encoding"], dtype=torch.float64)).abs().max())
            qual.append({"id": q["id"], "tokens": len(ids), "h_max_abs_error": eh, "encoding_max_abs_error": ez})
            shared.demand(eh <= 1e-5 and ez <= 1e-5, f"reference {q['id']}: replay error {eh} / {ez}")
    info = {"system": "frozen stage-3 adapted A (adapt_last_block; stage3_raw.py path)", "seed": b["seed"],
            "share_mode": "head", "bundle_sha256": digests, "stage3_fit_sha256": bridge.sha256(fit / "fit.json"),
            "stage3_selected_checkpoint_sha256": report["selected_checkpoint_sha256"],
            "last_layer_sha256": report["last_layer_sha256"], "shared_heads_sha256": report["shared_heads_sha256"],
            "stage3_contract_sha256": b["contract_sha256"], "stage3_selection_sha256": bridge.sha256(args.selection),
            "stage3_export_manifest_sha256": bridge.sha256(export / "manifest.json"), "init_sha256": bridge.sha256(args.init),
            "human_profiles": profiles["human"], "reference_replay": {"tolerance": 1e-5, "cases": qual}}
    return model, tokenizer, profiles["ai"], info


def e0_model(args, device):
    import train_e0_a as e0
    root, digests, contract, recipe, device = e0.common(args)
    fitdir = Path(args.fit).resolve(strict=True)
    report = read(fitdir / "fit.json")
    shared.demand(report["schema"] == e0.FIT_SCHEMA and report["final_test_opened"] is False and
                  report["binding"]["bundle_sha256"] == digests and
                  report["binding"]["contract_sha256"] == bridge.sha256(args.contract) and
                  report["binding"]["recipe_sha256"] == bridge.sha256(args.recipe), "E0 fit of this bundle and recipe")
    for name, key in (("last-layer.safetensors", "last_layer_sha256"), ("shared-heads.safetensors", "shared_heads_sha256"),
                      ("selected.pt", "selected_checkpoint_sha256")):
        shared.demand(bridge.sha256(fitdir / name) == report[key], f"{name} differs from fit.json")
    init = initializer(args.init)
    encoder, tokenizer, parent, heads = e0.load_model(args, contract, device, init)
    encoder.model.layers[21].load_state_dict(load_file(str(fitdir / "last-layer.safetensors")), strict=True)
    heads.load_state_dict(load_file(str(fitdir / "shared-heads.safetensors")), strict=True)
    joint = e0.architecture.LastBlockA(encoder, heads, True).eval()
    model = full_a.FullA(encoder, heads, None, "head", None).to(device).eval()
    model.token_loss_weight = 0.
    ai = list(contract["ai_profiles"])
    tokenize, special, pad = full_a.hf_tokenize(tokenizer), set(tokenizer.all_special_ids), tokenizer.pad_token_id
    dev = bundle.Population(root, e0.SELECTION_SPLIT, tokenize, special, ai, cohort_weighted=False)
    worst, inv = 0., []
    with torch.no_grad():
        for i in range(len(dev)):
            ids, mask, content, _, words = dev.batch([i], device, pad)
            o = model(ids, mask, content, autocast=False, words=words)
            prefix, _ = e0.architecture.capture_prefix(encoder, ids)
            q = joint.from_prefix(prefix)
            worst = max(worst, float((o["origin"] - q["origin"]).abs().max()), float((o["ai_logits"] - q["ai_logits"]).abs().max()))
            inv.append(float(q["origin"][0, 1:].sum()))
    score = e0.balanced_brier(inv, dev)
    shared.demand(worst <= 1e-5, f"full-text and prefix paths differ by {worst}")
    shared.demand(abs(score - report["dev_iter_uncalibrated_balanced_involvement_brier"]) <= 1e-9,
                  "selected Dev-iter score does not reproduce")
    info = {"system": "E0 (stage-3 final-block recipe on the new Train; train_e0_a.py raw path)",
            "seed": report["binding"]["seed"], "share_mode": "head", "bundle_sha256": digests,
            "e0_fit_sha256": bridge.sha256(fitdir / "fit.json"), "selected_checkpoint_sha256": report["selected_checkpoint_sha256"],
            "contract_sha256": bridge.sha256(args.contract), "recipe_sha256": bridge.sha256(args.recipe),
            "init_sha256": bridge.sha256(args.init), "e0_module_sha256": bridge.sha256(e0.__file__),
            "dev_iter_replay": {"full_text_vs_prefix_max_abs_error": worst, "selected_score": score}}
    return model, tokenizer, ai, info


MODELS = {"faux": faux_model, "v6": v6_model, "s3a": s3a_model, "e0": e0_model}


@torch.no_grad()
def main(args):
    rows_dir, out = Path(args.rows).resolve(), Path(args.out)
    shared.demand(not out.exists(), "new output required")
    split, rows_sha, seal = gate(args, rows_dir)
    device = torch.device(args.device)
    model, tokenizer, ai, info = MODELS[args.kind](args, device)
    pad = tokenizer.pad_token_id
    population = TestPopulation(rows_dir, split, full_a.hf_tokenize(tokenizer), set(tokenizer.all_special_ids), ai)
    failures = list(population.failures)
    tmp = out.with_name(out.name + ".partial")
    if tmp.exists():
        for p in tmp.iterdir():
            p.unlink()
        tmp.rmdir()
    tmp.mkdir(parents=True)
    consecutive = 0
    with (tmp / "raw.jsonl").open("xb") as f, (tmp / "predictions.jsonl").open("xb") as g:
        for i, meta in enumerate(population.meta):
            try:
                ids, mask, content, _, words = population.batch([i], device, pad)
                o = model(ids, mask, content, autocast=False, words=words)
                r = trainer.raw_prediction(meta, o, population.lengths[i])
                p = trainer.contract_prediction(meta, o, ai)
            except (RuntimeError, ValueError) as e:  # one document's forward failed: an execution failure
                failures.append({"id": meta.id, "failure": "crash:" + type(e).__name__})
                consecutive += 1
                # Repeated crashes mean a broken process, not unsupported documents: stop, the driver reruns.
                shared.demand(consecutive < MAX_CONSECUTIVE_CRASHES, f"{consecutive} consecutive forward crashes; run stopped")
                if device.type == "cuda":
                    torch.cuda.empty_cache()
                continue
            consecutive = 0
            if not (finite(r) and finite(p)):
                failures.append({"id": meta.id, "failure": "non_finite_output"})
                continue
            f.write(bridge.canonical(r) + b"\n")
            g.write(bridge.canonical(p) + b"\n")
    order = {k: i for i, k in enumerate(population.order)}
    failures.sort(key=lambda x: order[x["id"]])
    with (tmp / "failures.jsonl").open("xb") as h:
        for x in failures:
            h.write(bridge.canonical(x) + b"\n")
    counts = {}
    for x in failures:
        counts[x["failure"]] = counts.get(x["failure"], 0) + 1
    manifest = dict(info)
    manifest.update({"schema": SCHEMA, "complete": True, "partition": split, "kind": args.kind,
        "rows": len(population.order), "scored": len(population.order) - len(failures),
        "failures": len(failures), "failures_by_reason": counts, "ai_profiles": ai,
        "unrostered_generator_labels": population.unrostered, "scoring_rows_sha256": rows_sha,
        "raw_sha256": bridge.sha256(tmp / "raw.jsonl"), "predictions_sha256": bridge.sha256(tmp / "predictions.jsonl"),
        "failures_sha256": bridge.sha256(tmp / "failures.jsonl"), "implementation_sha256": bridge.sha256(__file__),
        "trainer_sha256": bridge.sha256(trainer.__file__), "full_a_sha256": bridge.sha256(full_a.__file__),
        "bundle_reader_sha256": bridge.sha256(bundle.__file__),
        "involvement_logit_semantics": LOGIT_SEMANTICS,
        "origin_logits_semantics": "original_classifier_linear_divided_by_base_temperature; no CAL map",
        "failure_rule": "input over 2,048 encoder tokens, empty input, a crash of one document's forward or a non-finite output is an execution failure (protocol failures_and_missing.a_execution_failure)",
        "seal": seal, "final_test_opened": split == "test", "calibrated": False,
        "device": str(device), "torch": torch.__version__})
    bridge.write_json(tmp / "manifest.json", manifest)
    tmp.rename(out)
    print(json.dumps({"complete": True, "partition": split, "kind": args.kind, "rows": manifest["rows"],
                      "failures": counts}), flush=True)


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("kind", choices=sorted(MODELS))
    for k in ("data", "rows", "out", "artifact", "init"):
        p.add_argument("--" + k, required=True)
    for k in ("config", "fit", "contract", "recipe", "s3contract", "selection", "export", "seal", "seal-sha256"):
        p.add_argument("--" + k, default=None)
    p.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
    main(p.parse_args())

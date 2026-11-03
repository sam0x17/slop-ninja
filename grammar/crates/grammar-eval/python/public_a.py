"""Public A training recipe: an A artifact from ModernBERT-base and published data only.

Steps (docs/testnet/training-guide.md gives the full commands):

  parent    build the starting model directory from a downloaded ModernBERT-base snapshot, or
            with --encoder from a published reference's encoder (R_0's training companion)
  embed     the parent's h for the Train rows planned by `fit_full_a_normalization requests`
  train     run train_full_a.py with the public parent in place of the V6 parent
  raw       run `export_full_a.py raw` (uncalibrated CAL heads for the Rust calibrator)
  select    record the fit's Dev-iter checkpoint choice for `calibrate_full_a`
  release   write the release layout that export_a_onnx.py reads, in the artifact spec's
            schemas (heads v1, export manifest v2, the Rust calibration v2)
  assemble  collect the six artifact files for `slop_ninja_miner pack --dir`

The release trainer and exporter were written for the V6 parent: their loader accepts only
the frozen V6 artifact and its classifier initializer. This wrapper replaces exactly those
two functions (`last_block_a.load_encoder` and the trainer's and exporter's `initializer`)
with loaders for the public parent built by `parent`, and leaves every other check of the
trainer and exporter in force. The fit binding keeps the trainer's field names, so its
`artifact_loader_sha256` names v6_embeddings.py; the release manifest's limits record this
file's digest as the loader actually used.

The parent is ModernBERT-base at the revision pinned in public_a/recipe.json, with a
three-label classifier head from transformers' own initialization under a fixed seed. Its
tokenizer is the upstream tokenizer re-serialized by tokenizers 0.22.2, which reproduces the
artifact spec's pinned tokenizer.json byte for byte; `parent` refuses any other bytes.

No text is printed. Test splits and reference rows are never read: the bundle builder
(`public_a_bundle`) does not open them.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile

os.environ.setdefault("HF_HUB_OFFLINE", "1")
os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")
os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")

HERE = Path(__file__).resolve().parent
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))

RECIPE = HERE / "public_a" / "recipe.json"
PARENT_SCHEMA = "slop_ninja_a_public_parent_v1"
PARENT_PROFILE = "public_modernbert_base_parent_v1"
INIT_SOURCE = "public_parent_classifier_v1"
# The run format that `fit_full_a_normalization fit` reads (named for its first parent, V6).
EMBED_SCHEMA = "slop_ninja_v6_embeddings_run_v1"
H_CONTRACT_SCHEMA = "slop_ninja_a_public_parent_h_contract_v1"
SELECTION_SCHEMA = "slop_ninja_full_a_selection_v2"
HEADS_V1 = "slop_ninja_full_a_heads_v1"
EXPORT_V2 = "slop_ninja_full_a_export_v2"
CALIBRATION_V2 = "slop_ninja_full_a_calibration_v2"
LABELS = ["human_only", "model_only", "mixed"]
DIMENSION = 768
# The artifact spec's export manifest v2 and its fit binding (subnet/crates/artifact-check
# src/roles.rs ExportManifestV2 and FitBinding): exactly these fields.
FIT_BINDING_FIELDS = ("architecture_sha256", "arm", "arm_definition", "artifact_loader_sha256", "aux_sha256",
                      "bundle_reader_sha256", "bundle_sha256", "contract_sha256", "cuda", "dev_iter_rows_sha256",
                      "device", "initializer_sha256", "model_sha256", "parent_artifact_id", "protocol_sha256",
                      "schema", "seed", "shared_heads_implementation_sha256", "shared_loss_sha256", "torch",
                      "trainer_sha256", "transformers_model_source_sha256")
EXPORT_FIELDS = ("arm", "calibrated", "calibration", "complete", "contract", "encoder", "exporter_sha256",
                 "final_test_opened", "fit_binding", "heads", "limits", "parent_artifact_id",
                 "parent_manifest_sha256", "production_qualified", "reference_device", "reference_predictions",
                 "reference_requests", "runtime", "schema", "seed", "selected_checkpoint_sha256",
                 "selected_evaluation")
HEADS_V2_ONLY = ("token_head", "span_emission")
ARTIFACT_FILES = ("a-fp32band.onnx", "heads.json", "contract.json", "calibration.json", "manifest.json",
                  "tokenizer.json")


def demand(ok, message):
    if not ok:
        raise SystemExit("refused: " + message)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def canonical(value):
    """The release writers' layout (v6_embeddings.canonical): compact, sorted, ASCII."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True, allow_nan=False).encode()


def read(path):
    return json.loads(Path(path).read_bytes())


def write_new(path, data):
    with open(path, "xb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())


def recipe(path=None):
    r = read(path or RECIPE)
    demand(r.get("schema") == "slop_ninja_a_public_recipe_v1", "public recipe schema")
    return r


def roster_names(r, path=None):
    names = read(Path(path or RECIPE).parent / r["pinned"]["ai_roster_file"])
    digest = hashlib.sha256(json.dumps(names, separators=(",", ":"), ensure_ascii=False).encode()).hexdigest()
    demand(digest == r["pinned"]["ai_roster_sha256"], "AI roster file differs from the recipe pin")
    return names


# ------------------------------------------------------------------ parent

def parent(args):
    import torch
    from safetensors.torch import load_file, save_file
    from transformers import AutoModelForSequenceClassification, AutoTokenizer
    import transformers
    import tokenizers
    from warm_start_a import BASE_TEMPERATURE, INIT_SCHEMA, ORIGIN_ORDER

    r = recipe(args.recipe)
    up = Path(args.upstream).resolve(strict=True)
    out = Path(args.out)
    demand(not out.exists(), "the parent needs a new directory")
    for name, expected in sorted(r["base_model"]["files"].items()):
        demand(sha256(up / name) == expected, f"upstream {name} differs from the pinned revision")
    cls = out / "classifier"
    cls.mkdir(parents=True)
    tok = AutoTokenizer.from_pretrained(up, local_files_only=True)
    tok.save_pretrained(cls)
    pinned = r["pinned"]
    demand(sha256(cls / "tokenizer.json") == pinned["tokenizer_sha256"] and
           (cls / "tokenizer.json").stat().st_size == pinned["tokenizer_bytes"],
           f"re-serialized tokenizer differs from the pinned tokenizer (tokenizers {tokenizers.__version__}; "
           "the recipe needs 0.22.2)")
    torch.manual_seed(args.seed)
    model = AutoModelForSequenceClassification.from_pretrained(
        up, local_files_only=True, num_labels=3, id2label=dict(enumerate(LABELS)),
        label2id={k: i for i, k in enumerate(LABELS)}, attn_implementation="eager", dtype=torch.float32)
    model.eval()
    model.save_pretrained(cls, safe_serialization=True)
    demand(sha256(cls / "config.json") == pinned["classifier_config_sha256"],
           f"classifier config differs from the pinned config (transformers {transformers.__version__}; "
           "the recipe needs 4.57.6)")
    # Every upstream encoder and prediction-head tensor is carried bit for bit; the masked-LM
    # decoder is dropped and the classifier is new.
    upstream = load_file(str(up / "model.safetensors"))
    saved = load_file(str(cls / "model.safetensors"))
    carried = {k for k in upstream if not k.startswith("decoder.")}
    demand(set(saved) == carried | {"classifier.weight", "classifier.bias"}, "parent tensor roster")
    demand(all(torch.equal(saved[k], upstream[k]) for k in carried), "upstream tensors changed")
    demand(all(t.dtype == torch.float32 for t in saved.values()), "FP32 parent required")
    encoder = None
    if args.encoder:
        saved, encoder = warm_start_encoder(cls, saved, Path(args.encoder))
    w, b = saved["classifier.weight"], saved["classifier.bias"]
    init = {"schema": INIT_SCHEMA, "source": INIT_SOURCE, "parent_schema": PARENT_SCHEMA, "dimension": DIMENSION,
            "origin_order": ORIGIN_ORDER, "base_temperature": BASE_TEMPERATURE,
            "weights": {"origin.weight": {"shape": [3, DIMENSION], "values": w.flatten().tolist()},
                        "origin.bias": {"shape": [3], "values": b.flatten().tolist()}},
            "weight_file": "classifier/model.safetensors",
            "weight_file_sha256": sha256(cls / "model.safetensors"),
            "rule": "the parent classifier's own weights, so the step-0 origin head equals it"}
    if encoder:
        init["warm_start_encoder_sha256"] = encoder["sha256"]
    write_new(out / "init.json", canonical(init) + b"\n")
    files = {p.relative_to(out).as_posix(): sha256(p) for p in sorted(out.rglob("*")) if p.is_file()}
    body = {"schema": PARENT_SCHEMA, "base_model": r["base_model"], "files": files, "labels": LABELS,
            "max_tokens": 2048,
            "classifier_init": {"seed": args.seed, "rule": "transformers ModernBertForSequenceClassification."
                                "from_pretrained initialization of the new classifier after torch.manual_seed(seed)"},
            "versions": {"torch": torch.__version__, "transformers": transformers.__version__,
                         "tokenizers": tokenizers.__version__},
            "builder_sha256": sha256(__file__)}
    if encoder:
        body["warm_start_encoder"] = encoder
        body["classifier_init"] = {"rule": "the warm-start encoder's own classifier tensors"}
    manifest = dict(body, artifact_id=hashlib.sha256(canonical(body)).hexdigest())
    write_new(out / "manifest.json", canonical(manifest) + b"\n")
    print(json.dumps({"complete": True, "artifact_id": manifest["artifact_id"]}))


def warm_start_encoder(cls, saved, path):
    """Replace the parent classifier's tensors with a published encoder's (R_0's training companion,
    `model/encoder.safetensors`): the same tensor roster, shapes and FP32 type, copied byte for byte.
    Returns the new tensors and the record the parent manifest keeps."""
    import torch
    from safetensors.torch import load_file

    path = Path(path).resolve(strict=True)
    encoder = load_file(str(path))
    demand(set(encoder) == set(saved), "the warm-start encoder's tensor roster differs from the parent's")
    demand(all(encoder[k].shape == saved[k].shape and encoder[k].dtype == torch.float32 for k in saved),
           "the warm-start encoder's tensor shapes or types differ from the parent's")
    target = Path(cls) / "model.safetensors"
    shutil.copyfile(path, target)
    copied = load_file(str(target))
    demand(set(copied) == set(encoder) and all(torch.equal(copied[k], encoder[k]) for k in encoder),
           "warm-start encoder copy")
    return copied, {"sha256": sha256(path), "bytes": path.stat().st_size,
                    "rule": "every encoder, prediction-head and classifier tensor taken from this file"}


def load_parent(artifact, device):
    """last_block_a.load_encoder for the public parent: the same model checks."""
    import torch
    from transformers import AutoModelForSequenceClassification, AutoTokenizer
    import last_block_a as base

    artifact = Path(artifact).resolve(strict=True)
    manifest = read(artifact / "manifest.json")
    demand(manifest.get("schema") == PARENT_SCHEMA, "a public parent directory (from `public_a.py parent`) required")
    body = {k: v for k, v in manifest.items() if k != "artifact_id"}
    demand(hashlib.sha256(canonical(body)).hexdigest() == manifest["artifact_id"], "parent identity mismatch")
    for name, expected in manifest["files"].items():
        rel = Path(name)
        demand(not rel.is_absolute() and ".." not in rel.parts, "unsafe parent member")
        demand(sha256(artifact / rel) == expected, "parent member changed: " + name)
    model = AutoModelForSequenceClassification.from_pretrained(
        artifact / "classifier", local_files_only=True, attn_implementation="eager",
        dtype=torch.float32, reference_compile=False).to(device).eval()
    tokenizer = AutoTokenizer.from_pretrained(artifact / "classifier", local_files_only=True)
    demand(model.__class__.__name__ == "ModernBertForSequenceClassification" and
           len(model.model.layers) == base.LAST_LAYER + 1 and model.config.hidden_size == DIMENSION and
           model.config.classifier_pooling == "mean", "unsupported parent architecture")
    demand(all(p.dtype == torch.float32 for p in model.parameters()), "FP32 required")
    demand(all(float(getattr(model.config, name)) == 0.0 for name in
               ("attention_dropout", "embedding_dropout", "mlp_dropout", "classifier_dropout")),
           "zero-dropout parent required")
    model.requires_grad_(False)
    return model, tokenizer, manifest, PARENT_PROFILE


def parent_initializer(path):
    """warm_start_a.initializer for the public parent's init.json."""
    import warm_start_a as ws
    init = read(path)
    demand(init.get("schema") == ws.INIT_SCHEMA and init.get("source") == INIT_SOURCE and
           init.get("parent_schema") == PARENT_SCHEMA and init.get("dimension") == DIMENSION and
           init.get("origin_order") == ws.ORIGIN_ORDER and init.get("base_temperature") == ws.BASE_TEMPERATURE,
           "a public parent init.json required")
    demand(set(init["weights"]) == {"origin.weight", "origin.bias"}, "classifier-only initializer required")
    ws.tensor(init["weights"]["origin.weight"], (3, DIMENSION))
    ws.tensor(init["weights"]["origin.bias"], (3,))
    return init


def install():
    """Route the trainer's and exporter's parent loading to the public parent."""
    import export_full_a
    import last_block_a
    import train_full_a
    last_block_a.load_encoder = load_parent
    train_full_a.initializer = parent_initializer
    export_full_a.initializer = parent_initializer
    return train_full_a, export_full_a


# ------------------------------------------------------------------ embed

def embed(args):
    import platform
    import torch
    import transformers
    import full_a
    import last_block_a as base

    device = torch.device(args.device)
    if device.type == "cuda":
        torch.backends.cuda.matmul.allow_tf32 = False
        torch.backends.cudnn.allow_tf32 = False
    model, tokenizer, manifest, profile = load_parent(args.parent, device)
    tokenize = full_a.hf_tokenize(tokenizer)
    out = Path(args.output)
    demand(not out.exists(), "the run needs a new directory")
    out.mkdir(parents=True)
    shutil.copyfile(args.requests, out / "requests.jsonl")
    contract = {"schema": H_CONTRACT_SCHEMA, "dimension": DIMENSION, "artifact_id": manifest["artifact_id"],
                "profile": profile, "dtype": "float32", "device": device.type, "batch_size": 1, "truncation": False,
                "max_tokens_including_special_tokens": full_a.MAX_TOKENS,
                "hook": "attention-mask mean pooling -> prediction head -> eval dropout (the classifier input h)",
                "extractor_sha256": sha256(__file__), "torch": torch.__version__,
                "transformers": transformers.__version__, "python": platform.python_version()}
    n = ok = 0
    with (out / "requests.jsonl").open("rb") as f, (out / "rows.jsonl").open("xb") as g, torch.no_grad():
        for line in f:
            if not line.strip():
                continue
            req = json.loads(line)
            n += 1
            row = {"id": req["id"], "text_sha256": hashlib.sha256(req["text"].encode("utf-8")).hexdigest(),
                   "status": "unsupported_input", "embedding": None}
            try:
                ids, _ = tokenize(req["text"])
            except ValueError as error:
                row["reason"] = str(error)
            else:
                h = base.full_encoding(model, torch.tensor([ids], dtype=torch.long, device=device))
                row.update(status="ok", embedding=h[0].double().cpu().tolist(), token_count=len(ids))
                ok += 1
            g.write(canonical(row) + b"\n")
    state = {"schema": EMBED_SCHEMA, "status": "complete", "dimension": DIMENSION, "requested": n, "supported": ok,
             "unsupported": n - ok, "processed": n, "requests_sha256": sha256(out / "requests.jsonl"),
             "rows_sha256": sha256(out / "rows.jsonl"), "representation_contract": contract,
             "representation_contract_sha256": hashlib.sha256(canonical(contract)).hexdigest()}
    write_new(out / "manifest.json", canonical(state) + b"\n")
    print(json.dumps({"complete": True, "rows": n, "unsupported": n - ok}))


# ------------------------------------------------------------------ train, raw, select

def train(args):
    trainer, _ = install()
    trainer.run(argparse.Namespace(data=args.data, config=args.config, artifact=args.parent, init=args.init,
                                   out=args.out, arm=args.arm, seed=args.seed, device=args.device,
                                   report_split=None, consume_sealed_split=None, use_record=None,
                                   train_subset_rows=None, init_fit=None))


def raw(args):
    _, exporter = install()
    exporter.raw(argparse.Namespace(data=args.data, config=args.config, fit=args.fit, artifact=args.parent,
                                    init=args.init, out=args.out, part=args.part, device=args.device,
                                    allow_diagnostic_subset=False))


def select(args):
    import full_a_bundle as bundle
    fit = Path(args.fit)
    report = read(fit / "fit.json")
    b = report["binding"]
    demand(report["complete"] is True and report["final_test_opened"] is False and
           report.get("diagnostic_train_subset") is None, "a complete, non-diagnostic fit required")
    # Whole files, or the blocks the fit read when it never opened a Dev-select or CAL block (binding_digests).
    digests = bundle.binding_digests(args.data, b["bundle_sha256"])
    demand(b["bundle_sha256"] == digests, "the fit is bound to another bundle")
    record = {"schema": SELECTION_SCHEMA, "complete": True, "final_test_opened": False,
              "calibration_read_for_selection": False, "selection_split": "dev_iter",
              "rule": "the recipe's single declared fit; Dev-iter early stopping chose its checkpoint",
              "bundle_sha256": digests,
              "runs": [{"arm": b["arm"], "seed": b["seed"], "fit_sha256": sha256(fit / "fit.json"),
                        "selected_checkpoint_sha256": report["selected_checkpoint_sha256"]}]}
    write_new(args.out, canonical(record) + b"\n")
    print(json.dumps({"complete": True, "selected_evaluation": report["selected_evaluation"]}))


# ------------------------------------------------------------------ release

def calibration_v2(path, fit_report):
    c = read(path)
    demand(c.get("schema") == CALIBRATION_V2 and c.get("complete") is True and c.get("fit_split") == "cal" and
           c.get("final_test_opened") is False, "a complete Rust CAL calibration (calibration v2) required")
    demand(c["arm"] == fit_report["binding"]["arm"] and c["seed"] == fit_report["binding"]["seed"] and
           c["selected_checkpoint_sha256"] == fit_report["selected_checkpoint_sha256"],
           "the calibration belongs to another fit")
    demand(c["roles"]["ai"].get("qualified") is True and c["roles"]["human"].get("roster_empty") is True,
           "the AI role threshold is not qualified on CAL (CAL needs positives of roster and non-roster generators)")
    return c


def heads_v1(h3):
    """The exporter's heads v2 of a document-head fit, in the spec's heads v1 schema."""
    demand(all(h3.get(k) is None for k in HEADS_V2_ONLY) and h3.get("span_head") is None,
           "the artifact spec admits document heads only (no span or token head)")
    # The trainer's supervision flags record which targets the Train population carries (R0's
    # heads v1 has "token" = 1 without a token head too). Heads v1 has no "token_head" flag.
    supervision = {k: v for k, v in h3["supervision"].items() if k != "token_head"}
    heads = {k: v for k, v in h3.items() if k not in HEADS_V2_ONLY}
    heads.update(schema=HEADS_V1, supervision=supervision)
    return heads


def manifest_v2(m3, members, exporter_sha256, limits):
    """The exporter's manifest v3 in the spec's export manifest v2: exactly its fields, with
    the fit binding cut to the v2 binding fields. Returns (manifest, omitted binding keys)."""
    binding = m3["fit_binding"]
    demand(all(k in binding for k in FIT_BINDING_FIELDS), "the fit binding lacks an export-v2 field")
    manifest = {"schema": EXPORT_V2, "complete": True, "calibrated": True,
                "fit_binding": {k: binding[k] for k in FIT_BINDING_FIELDS},
                "exporter_sha256": exporter_sha256, "limits": limits, **members}
    for k in ("arm", "seed", "selected_evaluation", "parent_artifact_id", "parent_manifest_sha256",
              "final_test_opened", "production_qualified", "reference_device", "runtime",
              "selected_checkpoint_sha256"):
        manifest[k] = m3[k]
    demand(sorted(manifest) == sorted(EXPORT_FIELDS), "export manifest fields")
    return manifest, sorted(set(binding) - set(FIT_BINDING_FIELDS))


def release(args):
    trainer, exporter = install()
    fit = Path(args.fit)
    report = read(fit / "fit.json")
    cal_path = Path(args.calibration)
    cal = calibration_v2(cal_path, report)
    out = Path(args.out)
    demand(not out.exists(), "the release needs a new directory")
    out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=out.parent, prefix=".release-") as tmp:
        tmp = Path(tmp)
        # The exporter writes export v3 and reads calibration v3; for a fit without a token
        # head, v3 is v2 plus null span fields. It gets the Rust calibration with them added.
        shim = tmp / "calibration-v3.json"
        shim.write_bytes(canonical(dict(cal, schema=exporter.CALIBRATION_SCHEMA, spans=None)))
        v3 = tmp / "export-v3"
        exporter.export(argparse.Namespace(data=args.data, config=args.config, fit=args.fit, artifact=args.parent,
                                           init=args.init, out=str(v3), device=args.device, calibration=str(shim),
                                           local_references=0, allow_diagnostic_subset=False))
        m3, h3 = read(v3 / "manifest.json"), read(v3 / "heads.json")
        demand(m3["schema"] == exporter.EXPORT_SCHEMA and m3["calibrated"] is True and m3["token_head"] is None,
               "a calibrated export without a token head required")
        heads = heads_v1(h3)
        demand(heads["involvement_calibration"] == cal["involvement"]["selected"], "heads and calibration maps differ")
        model_dir, base_dir = tmp / "release" / "model", tmp / "release" / "runtime-base"
        model_dir.mkdir(parents=True)
        base_dir.mkdir()
        os.replace(v3 / "encoder.safetensors", model_dir / "encoder.safetensors")
        for name in ("contract.json", "reference-requests.jsonl", "reference-predictions.json"):
            shutil.copyfile(v3 / name, model_dir / name)
        write_new(model_dir / "heads.json", canonical(heads) + b"\n")
        shutil.copyfile(cal_path, model_dir / "calibration.json")
        member = lambda name: {"file": name, "sha256": sha256(model_dir / name)}  # noqa: E731
        for name in ("contract.json", "encoder.safetensors", "reference-requests.jsonl", "reference-predictions.json"):
            demand(member(name)["sha256"] == m3[{"contract.json": "contract", "encoder.safetensors": "encoder",
                                                  "reference-requests.jsonl": "reference_requests",
                                                  "reference-predictions.json": "reference_predictions"}[name]]["sha256"],
                   name + " changed while it was copied")
        omitted = sorted(set(m3["fit_binding"]) - set(FIT_BINDING_FIELDS))
        roster = report.get("ai_roster") or {}
        limits = [
            "Public recipe artifact: trained from ModernBERT-base on published datasets only; not R0 and not a "
            "reproduction of R0.",
            f"Written by public_a.py release (sha256 {sha256(__file__)}) from export_full_a.py export "
            f"(sha256 {m3['exporter_sha256']}); fit_binding keeps the export-v2 binding fields and omits "
            f"{omitted}.",
            "Parent loading used public_a.py, not v6_embeddings.py; fit_binding keeps the trainer's field names "
            "(artifact_loader_sha256, initializer_sha256).",
            f"AI roster rule {roster.get('rule')}: Train supervised {roster.get('supervised')}; the other roster "
            "names have no Train rows. The contract's ai_roster_rule text is pinned by the artifact spec.",
            "Rust full-encoder replay and runtime measurements still required.",
            "No Workers/Wasm or Container qualification follows from numeric export."]
        members = {"heads": member("heads.json"), "calibration": member("calibration.json"),
                   "contract": member("contract.json"), "encoder": member("encoder.safetensors"),
                   "reference_requests": member("reference-requests.jsonl"),
                   "reference_predictions": member("reference-predictions.json")}
        manifest, _ = manifest_v2(m3, members, sha256(__file__), limits)
        write_new(model_dir / "manifest.json", canonical(manifest) + b"\n")
        cls = Path(args.parent) / "classifier"
        for name in ("config.json", "tokenizer.json", "tokenizer_config.json", "special_tokens_map.json"):
            shutil.copyfile(cls / name, base_dir / name)
        os.replace(tmp / "release", out)
    print(json.dumps({"complete": True, "heads_sha256": sha256(out / "model" / "heads.json"),
                      "manifest_sha256": sha256(out / "model" / "manifest.json")}))


def assemble(args):
    r = recipe(args.recipe)
    rel, out = Path(args.release), Path(args.out)
    demand(not out.exists(), "the artifact needs a new directory")
    m = read(rel / "model" / "manifest.json")
    demand(m.get("schema") == EXPORT_V2 and m.get("calibrated") is True, "a release from `public_a.py release`")
    for name, key in (("heads.json", "heads"), ("contract.json", "contract"), ("calibration.json", "calibration")):
        demand(sha256(rel / "model" / name) == m[key]["sha256"], name + " differs from the export manifest")
    demand(sha256(rel / "runtime-base" / "tokenizer.json") == r["pinned"]["tokenizer_sha256"], "tokenizer")
    record = read(args.record)
    graph = Path(args.graph)
    demand(record.get("schema") == "slop_ninja_a_onnx_export_v1" and record.get("qualified") is True and
           record.get("variant") == "plain" and record.get("graph_sha256") == sha256(graph) and
           record["inputs"]["model/heads.json"] == m["heads"]["sha256"],
           "a qualified plain export_a_onnx.py record of this release and graph required")
    out.mkdir(parents=True)
    try:
        os.link(graph, out / "a-fp32band.onnx")
    except OSError:
        shutil.copyfile(graph, out / "a-fp32band.onnx")
    for name in ("heads.json", "contract.json", "calibration.json", "manifest.json"):
        shutil.copyfile(rel / "model" / name, out / name)
    shutil.copyfile(rel / "runtime-base" / "tokenizer.json", out / "tokenizer.json")
    demand(sorted(p.name for p in out.iterdir()) == sorted(ARTIFACT_FILES), "artifact files")
    print(json.dumps({"complete": True, "files": sorted(ARTIFACT_FILES)}))


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    a = sub.add_parser("parent")
    a.add_argument("--upstream", required=True, help="downloaded ModernBERT-base files at the pinned revision")
    a.add_argument("--encoder", help="warm start: a published encoder (R_0's training companion "
                   "model/encoder.safetensors) in place of the upstream tensors")
    a.add_argument("--out", required=True)
    a.add_argument("--seed", type=int, default=17)
    a.add_argument("--recipe", default=str(RECIPE))
    a.set_defaults(func=parent)
    a = sub.add_parser("embed")
    for k in ("parent", "requests", "output"):
        a.add_argument("--" + k, required=True)
    a.add_argument("--device", choices=("cpu", "cuda"), default="cpu")
    a.set_defaults(func=embed)
    a = sub.add_parser("train")
    for k in ("data", "config", "parent", "init", "out", "arm"):
        a.add_argument("--" + k, required=True)
    a.add_argument("--seed", type=int, required=True)
    a.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
    a.set_defaults(func=train)
    a = sub.add_parser("raw")
    for k in ("data", "config", "fit", "parent", "init", "out"):
        a.add_argument("--" + k, required=True)
    a.add_argument("--part", choices=("dev_iter", "cal"), default="cal")
    a.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
    a.set_defaults(func=raw)
    a = sub.add_parser("select")
    for k in ("data", "fit", "out"):
        a.add_argument("--" + k, required=True)
    a.set_defaults(func=select)
    a = sub.add_parser("release")
    for k in ("data", "config", "fit", "parent", "init", "calibration", "out"):
        a.add_argument("--" + k, required=True)
    a.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
    a.set_defaults(func=release)
    a = sub.add_parser("assemble")
    for k in ("release", "graph", "record", "out"):
        a.add_argument("--" + k, required=True)
    a.add_argument("--recipe", default=str(RECIPE))
    a.set_defaults(func=assemble)
    args = p.parse_args(argv)
    args.func(args)


if __name__ == "__main__":
    main()

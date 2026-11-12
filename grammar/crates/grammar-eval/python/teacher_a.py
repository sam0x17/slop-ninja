"""Teacher classes for the full-A trainer: a larger pretrained encoder fitted with the base recipe, never exported.

A teacher class answers a capacity question at fixed data and recipe and supplies soft targets for distillation
into the served graph class. It is never packaged. A teacher fit binds no initializer, so every `export_full_a.py`
route (export, raw, score-rows) refuses it at the exporter's own lineage check, and the exporter's bytes stay
unchanged. `raw` here scores a teacher fit's Dev-iter or CAL block in the exporter's raw format (the input of the
Rust CAL calibration), and `raw_full_a.py` scores its Train block or a rows file; both check the fit as the exporter
checks it (`completed_fit`), with the teacher module's digest in place of an initializer.

A protocol and its contract name the same class in `teacher_class`; with the field absent, every path of the
trainer and exporter is the base one, unchanged. With it:

- the trainer's `--artifact` is a local snapshot of the class's pinned upstream revision, holding exactly the
  pinned files, each checked against its SHA-256 at every load; there is no `--init`;
- the encoder is that snapshot's ModernBERT with transformers' sequence-classification wrapper (mean pooling,
  the pretrained prediction head, zero dropout, eager attention, FP32 weights); the new three-label classifier
  that the wrapper adds is never trained and never read except by the initial gate's record;
- the stage-3 heads (`TeacherHeads`) have WarmStartA's structure and forward at the encoder's width (read from
  its config), with the same zero CFG coordinates and logit scale, but every head starts fresh: no V6 classifier
  is copied, so the trainer's step-0 V6 gate is skipped (padded-batch checks still run);
- the contract's `raw_dimension` and normalization dimension are the class width, and the normalization is the
  base rule (`train_query_only_equal_source_family_weight`) fitted on the class's pretrained h of the bundle's
  Train rows (`embed` here, then `fit_full_a_normalization fit` and `prepare_full_a_contract --teacher-class`);
- arms train document heads only (no span head, token head, frozen document model or S2 mechanism), and
  `--init-fit` is refused.

Commands:
  id      print the class's snapshot identity (the contract's `parent_artifact_id`) and pins
  check   verify a downloaded snapshot directory against the class's pins (CPU; no model load)
  embed   the pretrained h of planned requests, in the run format `fit_full_a_normalization fit` reads
  raw     uncalibrated raw heads of a teacher fit on its Dev-iter or CAL block (`export_full_a.py raw`'s format)

No network access; tests use a tiny synthetic snapshot.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shutil

os.environ.setdefault("HF_HUB_OFFLINE", "1")
os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")
os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")

import torch
from torch import nn

import full_a
import full_a_bundle as bundle
import shared_a as shared
import v6_embeddings as bridge
from warm_start_a import BASE_TEMPERATURE, ORIGIN_ORDER


SNAPSHOT_SCHEMA = "slop_ninja_a_teacher_snapshot_v1"
BINDING_SCHEMA = "slop_ninja_a_teacher_binding_v1"
H_CONTRACT_SCHEMA = "slop_ninja_a_teacher_h_contract_v1"
# The run format `fit_full_a_normalization fit` reads (named for its first encoder, V6).
EMBED_SCHEMA = "slop_ninja_v6_embeddings_run_v1"
EMBED_WINDOW = 256
BASE_WIDTH = 768
RESIDUAL_HIDDEN = 64
HEADS_RULE = ("teacher_heads_v1: WarmStartA's structure and forward at the encoder width (residual "
              "Linear(width+1820,64)-Tanh-Linear(64,width) with its output layer zero; origin, mixed-share, human and "
              "AI linear heads on z; logits divided by the stage-3 base temperature); every other tensor at PyTorch's "
              "default initialization, drawn right after torch.manual_seed(fit seed) in a forked CPU generator; "
              "no V6 classifier copied")
CLASSES = {
    "modernbert_large_v1": {
        "repository": "answerdotai/ModernBERT-large",
        "revision": "45bb4654a4d5aaff24dd11d4781fa46d39bf8c13",
        "license": "Apache-2.0",
        "hidden_size": 1024,
        "layers": 28,
        # The upstream files the trainer reads. tokenizer.json, tokenizer_config.json and special_tokens_map.json
        # are byte-identical to ModernBERT-base's upstream files (public_a/recipe.json), so the tokenizer is the
        # artifact spec's pinned one.
        "files": {
            "config.json": {"sha256": "55dca68059e98f0debf8eb63ab355c451c65b1f5d90fdca4754113fdaadd2cca",
                            "bytes": 1194},
            "model.safetensors": {"sha256": "44510fec5d3a81a1877f225637b869495f18e55f6f23a09abb9be0acc030295f",
                                  "bytes": 1583544840},
            "special_tokens_map.json": {"sha256": "ea97ecdbcc73713039d8d64dbb05e3689495c96657fbd9a18f5bed381be81049",
                                        "bytes": 694},
            "tokenizer.json": {"sha256": "9fd55248d51d33976b324fc11592e28071da7d41e0e9401dfb7082e30574b7b1",
                               "bytes": 2132967},
            "tokenizer_config.json": {"sha256": "3cd2017ff46d0a527e5d39cae39272eccfa1f19bb9f89b05d166aab2e38354e2",
                                      "bytes": 20810},
        },
    },
}


def demand(ok, message):
    if not ok:
        raise ValueError(message)


def validate_class(name):
    demand(isinstance(name, str) and name in CLASSES, f"unknown teacher class {name!r}")
    return CLASSES[name]


def artifact_id(name):
    """The class's snapshot identity: SHA-256 of its canonical pin record (the contract's parent_artifact_id)."""
    return hashlib.sha256(bridge.canonical({"schema": SNAPSHOT_SCHEMA, "class": name,
                                            **validate_class(name)})).hexdigest()


def contract_width(contract):
    """The raw dimension a contract must declare: the class width with a teacher class, else V6's 768."""
    name = contract.get("teacher_class")
    return BASE_WIDTH if name is None else validate_class(name)["hidden_size"]


def check_arm(name, arm):
    """A teacher class trains document heads only."""
    demand(arm["span_head"] is None and arm.get("token_head") is None and arm.get("freeze", "none") == "none" and
           arm.get("s2") is None, "a teacher class trains document heads only: " + name)


def is_teacher_binding(binding):
    return isinstance(binding, dict) and binding.get("teacher_class") is not None


def verify_snapshot(snapshot, name):
    """Every pinned file present with its digest and size, and no other top-level entry (hidden entries such as a
    download cache aside), so the loader can read nothing unpinned. Returns {file: sha256}."""
    pins = validate_class(name)["files"]
    root = Path(snapshot).resolve(strict=True)
    present = sorted(p.name for p in root.iterdir() if not p.name.startswith("."))
    demand(present == sorted(pins), f"teacher snapshot holds {present}, expected exactly {sorted(pins)}")
    for file, pin in sorted(pins.items()):
        path = root / file
        demand(path.is_file() and path.stat().st_size == pin["bytes"], f"teacher snapshot {file}: size differs")
        demand(bridge.sha256(path) == pin["sha256"], f"teacher snapshot {file}: digest differs from the pin")
    return {file: pin["sha256"] for file, pin in sorted(pins.items())}


def load(snapshot, name, device):
    """The class's encoder and tokenizer from a verified snapshot (trainer.load's teacher route)."""
    from transformers import AutoModelForSequenceClassification, AutoTokenizer
    cls = validate_class(name)
    verify_snapshot(snapshot, name)
    root = Path(snapshot).resolve(strict=True)
    model = AutoModelForSequenceClassification.from_pretrained(
        root, local_files_only=True, use_safetensors=True, num_labels=3,
        id2label=dict(enumerate(ORIGIN_ORDER)), label2id={k: i for i, k in enumerate(ORIGIN_ORDER)},
        attn_implementation="eager", dtype=torch.float32, reference_compile=False).to(device).eval()
    vocab = AutoTokenizer.from_pretrained(root, local_files_only=True)
    demand(model.__class__.__name__ == "ModernBertForSequenceClassification" and
           model.config.hidden_size == cls["hidden_size"] and len(model.model.layers) == cls["layers"] and
           model.config.classifier_pooling == "mean", "unsupported teacher architecture")
    demand(all(p.dtype == torch.float32 for p in model.parameters()), "FP32 required")
    demand(all(float(getattr(model.config, k)) == 0.0 for k in
               ("attention_dropout", "embedding_dropout", "mlp_dropout", "classifier_dropout")),
           "zero-dropout teacher required")
    demand(vocab.pad_token_id is not None, "the teacher tokenizer needs a pad token")
    model.requires_grad_(False)
    return model, vocab, {"artifact_id": artifact_id(name), "teacher_class": name}


def width_of(encoder):
    return int(encoder.config.hidden_size)


class TeacherHeads(nn.Module):
    """The stage-3 heads at width W: WarmStartA's tensors, names and forward (text mode), every head fresh.

    z = h + residual([(h - mean) / scale, 0_cfg]); origin logits = origin(z) / base temperature; share =
    p_model + p_mixed * sigmoid(mixed_share(z)); human and AI role logits from z. The residual's output layer
    starts at zero, as in WarmStartA; WarmStartA alone then copies V6's classifier into the origin head."""

    def __init__(self, width, mean, scale, human_profiles, ai_profiles):
        super().__init__()
        demand(type(width) is int and width >= 1, "teacher head width")
        demand(len(mean) == len(scale) == width and all(math.isfinite(v) for v in mean) and
               all(math.isfinite(v) and v > 0 for v in scale), "invalid frozen normalization")
        self.input_mode = "text"
        self.width = width
        self.base_temperature = BASE_TEMPERATURE
        self.register_buffer("text_mean", torch.tensor(mean, dtype=torch.float32))
        self.register_buffer("text_scale", torch.tensor(scale, dtype=torch.float32))
        self.residual = nn.Sequential(nn.Linear(width + full_a.CFG_ZEROS, RESIDUAL_HIDDEN), nn.Tanh(),
                                      nn.Linear(RESIDUAL_HIDDEN, width))
        self.origin = nn.Linear(width, 3)
        self.mixed_share = nn.Linear(width, 1)
        self.human = nn.Linear(width, max(1, human_profiles))
        self.ai = nn.Linear(width, max(1, ai_profiles))
        with torch.no_grad():
            self.residual[2].weight.zero_()
            self.residual[2].bias.zero_()

    def forward(self, x, temperature=1.0):
        demand(x.ndim == 2 and x.shape[1] == self.width + full_a.CFG_ZEROS, "feature dimension mismatch")
        h = x[:, :self.width]
        u = torch.cat(((h - self.text_mean) / self.text_scale, torch.zeros_like(x[:, self.width:])), dim=1)
        z = h + self.residual(u)
        logits = self.origin(z) / self.base_temperature
        p = (logits / temperature).softmax(-1)
        q = self.mixed_share(z).squeeze(-1).sigmoid()
        return {"encoding": z, "origin_logits": logits, "origin": p,
                "conditional_mixed_share": q, "share": p[:, 1] + p[:, 2] * q,
                "human_logits": self.human(z), "ai_logits": self.ai(z)}


def fresh_heads(width, normalization, human_profiles, ai_profiles, seed):
    """TeacherHeads drawn from the fit seed alone (a forked CPU generator: the caller's RNG state is unchanged)."""
    demand(type(seed) is int, "a teacher's heads need the fit seed")
    with torch.random.fork_rng(devices=[]):
        torch.manual_seed(seed)
        return TeacherHeads(width, normalization["mean"], normalization["scale"], human_profiles, ai_profiles)


def binding_record(name, encoder):
    cls = validate_class(name)
    demand(width_of(encoder) == cls["hidden_size"], "teacher width differs from its class")
    return {"schema": BINDING_SCHEMA, "class": name, "artifact_id": artifact_id(name),
            "repository": cls["repository"], "revision": cls["revision"], "width": cls["hidden_size"],
            "layers": cls["layers"], "heads_init": HEADS_RULE, "module_sha256": bridge.sha256(__file__),
            "exportable": False}


# ------------------------------------------------------------------ fit lineage and raw scoring

RAW_PARTS = ("dev_iter", "cal")


def read(path):
    return json.loads(Path(path).read_bytes())


def completed_fit(root, config, fit, init, allow_subset=False):
    """`export_full_a.completed_fit` for any fit. A base fit goes to the exporter unchanged (its `--init` is
    required). A teacher fit gets the same checks, with no initializer and the teacher module's digest; the
    exporter itself refuses it, since it binds no initializer."""
    import export_full_a as exporter
    import train_full_a as trainer
    report = read(fit / "fit.json")
    binding = report["binding"]
    if not is_teacher_binding(binding):
        demand(init is not None, "--init (the fit's V6 initializer) is required for a base fit")
        return exporter.completed_fit(root, config, fit, init, allow_subset)
    demand(init is None and binding["initializer_sha256"] is None, "a teacher fit takes no --init")
    digests = bundle.binding_digests(root, binding["bundle_sha256"])
    demand(report["schema"] == trainer.FIT_SCHEMA and report["complete"] is True and
           report["final_test_opened"] is False and binding == read(fit / "binding.json") and
           binding["bundle_sha256"] == digests and
           binding["contract_sha256"] == bridge.sha256(config / "contract.json") and
           binding["protocol_sha256"] == bridge.sha256(config / "protocol.json") and
           report.get("teacher_class") == binding["teacher_class"],
           "completed teacher fit on this bundle and config required")
    demand(allow_subset or (report.get("diagnostic_train_subset") is None and
                            binding.get("diagnostic_train_subset") is None),
           "diagnostic Train-subset fits are never scored here (smoke runs need --allow-diagnostic-subset)")
    log = report["cohort_weight_log"]
    demand(log["complete"] is True and bridge.sha256(fit / log["file"]) == log["sha256"] and
           read(fit / log["file"])["complete"] is True, "a fit without its cohort weight log is invalid")
    for module, key in ((full_a, "model_sha256"), (exporter.base, "architecture_sha256"),
                        (shared, "shared_loss_sha256"), (bridge, "artifact_loader_sha256"),
                        (bundle, "bundle_reader_sha256"), (exporter.spans_rule, "spans_rule_sha256")):
        exporter.require(module.__file__, binding[key])
    exporter.require(__file__, binding["teacher_class"]["module_sha256"])
    exporter.require(fit / "selected.pt", report["selected_checkpoint_sha256"])
    saved = torch.load(fit / "selected.pt", map_location="cpu", weights_only=False)
    demand(saved["binding"] == binding and saved["evaluation"] == report["selected_evaluation"],
           "selected checkpoint lineage differs")
    return digests, report, binding, saved


def restore_model(args, root, config, binding, saved, device):
    """`export_full_a.restore_model` for any fit: a teacher's model is rebuilt from its snapshot (`--artifact`)
    and fresh heads from the fit seed, then every tensor comes from the checkpoint."""
    import export_full_a as exporter
    import train_full_a as trainer
    if not is_teacher_binding(binding):
        return exporter.restore_model(args, root, config, binding, saved, device)
    contract = trainer.validate_contract(read(config / "contract.json"), binding["bundle_sha256"][bundle.MANIFEST])
    protocol = trainer.validate_protocol(read(config / "protocol.json"))
    arm = protocol["arms"][binding["arm"]]
    demand(arm == binding["arm_definition"], "arm definition differs")
    demand(protocol.get("teacher_class") == contract.get("teacher_class") == binding["teacher_class"]["class"],
           "teacher class differs")
    encoder, tokenizer, parent = trainer.load(args, protocol, device)
    demand(parent["artifact_id"] == contract["parent_artifact_id"] == binding["parent_artifact_id"],
           "parent differs")
    model, profiles = trainer.build(encoder, contract, None, arm, protocol, device, binding["seed"])
    trainer.restore(model, saved["model"])
    model.token_loss_weight = model.token_head_weight = 0.
    return model.eval(), tokenizer, contract, protocol, arm, profiles


@torch.no_grad()
def raw(args):
    """`export_full_a.py raw` for a teacher fit: the same rows, predictions and manifest fields (plus the teacher
    record) for its Dev-iter or CAL block. Dev-select and Test are refused."""
    import export_full_a as exporter
    import train_full_a as trainer
    root, config, fit, out = (Path(p).resolve() for p in (args.data, args.config, args.fit, args.out))
    demand(args.part in RAW_PARTS, "a teacher's raw route reads the Dev-iter or CAL block only")
    demand(not out.exists(), "new output required")
    demand(is_teacher_binding(read(fit / "fit.json")["binding"]), "not a teacher fit (use export_full_a.py raw)")
    digests, report, binding, saved = completed_fit(root, config, fit, None,
                                                    getattr(args, "allow_diagnostic_subset", False))
    device = torch.device(args.device)
    trainer.configure(device, trainer.validate_protocol(read(config / "protocol.json"))["cuda_memory_fraction"])
    model, tokenizer, contract, protocol, arm, profiles = restore_model(args, root, config, binding, saved, device)
    population = bundle.Population(root, args.part, full_a.hf_tokenize(tokenizer), set(tokenizer.all_special_ids),
                                   profiles["ai"], cohort_weighted=False)
    out.mkdir()
    with (out / "raw.jsonl").open("xb") as f, (out / "predictions.jsonl").open("xb") as g:
        for i, meta in enumerate(population.meta):
            ids, mask, content, _, words = population.batch([i], device, tokenizer.pad_token_id)
            o = model(ids, mask, content, autocast=False, words=words)
            f.write(bridge.canonical(trainer.raw_prediction(meta, o, population.lengths[i])) + b"\n")
            g.write(bridge.canonical(trainer.contract_prediction(meta, o, profiles["ai"])) + b"\n")
    bridge.write_json(out / "manifest.json", {"schema": exporter.RAW_SCHEMA, "complete": True, "partition": args.part,
        "rows": len(population), "arm": binding["arm"], "seed": binding["seed"],
        "share_mode": arm["share_mode"], "bundle_sha256": digests, "contract_sha256": binding["contract_sha256"],
        "fit_sha256": bridge.sha256(fit / "fit.json"), "selected_checkpoint_sha256": report["selected_checkpoint_sha256"],
        "raw_sha256": bridge.sha256(out / "raw.jsonl"), "predictions_sha256": bridge.sha256(out / "predictions.jsonl"),
        "token_logits_sha256": None, "implementation_sha256": bridge.sha256(__file__),
        "final_test_opened": False, "calibrated": False, "device": str(device), "torch": torch.__version__,
        "involvement_logit_semantics": "log(p_model+p_mixed)-log(p_human) of the final uncalibrated origin",
        "origin_logits_semantics": "teacher origin head (fresh at the fit's start) divided by the base temperature; "
                                   "no CAL map",
        "teacher_class": binding["teacher_class"]})
    print(json.dumps({"complete": True, "partition": args.part, "rows": len(population)}), flush=True)


# ------------------------------------------------------------------ embed

def batches(lengths, limit):
    """Index groups over length-sorted rows (ties by index): a group grows while its padded tokens (longest x rows)
    stay within limit; limit 0 means one unpadded request per forward."""
    groups, current, width = [], [], 0
    for i in sorted(range(len(lengths)), key=lambda k: (lengths[k], k)):
        w = max(width, lengths[i])
        if current and (limit == 0 or w * (len(current) + 1) > limit):
            groups.append(current)
            current, w = [], lengths[i]
        current.append(i)
        width = w
    if current:
        groups.append(current)
    return groups


@torch.no_grad()
def encode_group(model, token_lists, device, pad_id):
    """h = eval dropout(prediction head(attention-mask mean of the last hidden states)), FP32, as FullA.encode."""
    n = max(len(t) for t in token_lists)
    ids = torch.full((len(token_lists), n), pad_id, dtype=torch.long)
    mask = torch.zeros((len(token_lists), n), dtype=torch.long)
    for i, t in enumerate(token_lists):
        ids[i, :len(t)] = torch.tensor(t, dtype=torch.long)
        mask[i, :len(t)] = 1
    ids, mask = ids.to(device), mask.to(device)
    hidden = model.model(input_ids=ids, attention_mask=mask, return_dict=True).last_hidden_state
    m = mask.unsqueeze(-1).to(hidden.dtype)
    return model.drop(model.head((hidden * m).sum(1) / m.sum(1)))


def embed(args):
    import transformers
    name = args.teacher_class
    device = torch.device(args.device)
    if device.type == "cuda":
        torch.backends.cuda.matmul.allow_tf32 = False
        torch.backends.cudnn.allow_tf32 = False
    demand(type(args.max_padded) is int and (args.max_padded == 0 or args.max_padded >= full_a.MAX_TOKENS),
           "max padded tokens per forward: 0 (one request per forward) or at least 2,048")
    out = Path(args.output)
    demand(not out.exists(), "the run needs a new directory")
    model, tokenizer, parent = load(args.artifact, name, device)
    width = width_of(model)
    encode_text = full_a.hf_tokenize(tokenizer)
    limit = full_a.MAX_TOKENS
    contract = {"schema": H_CONTRACT_SCHEMA, "dimension": width, "artifact_id": parent["artifact_id"],
                "profile": name, "dtype": "float32", "device": device.type,
                "batching": {"max_padded": args.max_padded, "window": EMBED_WINDOW,
                             "rule": "windows of consecutive requests; within a window, length-sorted padded groups "
                                     "of at most max_padded padded tokens (0: one unpadded request per forward); "
                                     "rows written in request order"},
                "truncation": False, "max_tokens_including_special_tokens": limit,
                "hook": "attention-mask mean pooling -> pretrained prediction head -> eval dropout (the stage-3 "
                        "heads' input h)",
                "extractor_sha256": bridge.sha256(__file__), "torch": torch.__version__,
                "transformers": transformers.__version__, "python": platform.python_version()}
    out.mkdir(parents=True)
    shutil.copyfile(args.requests, out / "requests.jsonl")
    n = ok = 0
    with (out / "requests.jsonl").open("rb") as f, (out / "rows.jsonl").open("xb") as g:
        window = []

        def flush():
            nonlocal ok
            ready = [k for k, (_, _, ids) in enumerate(window) if ids is not None]
            rows = {}
            for group in batches([len(window[k][2]) for k in ready], args.max_padded):
                h = encode_group(model, [window[ready[j]][2] for j in group], device, tokenizer.pad_token_id)
                for j, v in zip(group, h):
                    rows[ready[j]] = v.double().cpu().tolist()
            for k, (rid, digest, ids) in enumerate(window):
                row = {"id": rid, "text_sha256": digest, "status": "unsupported_input", "embedding": None}
                if ids is None:
                    row["reason"] = "unsupported input (empty or over 2,048 tokens)"
                else:
                    row.update(status="ok", embedding=rows[k], token_count=len(ids))
                    ok += 1
                g.write(bridge.canonical(row) + b"\n")
            window.clear()

        for line in f:
            if not line.strip():
                continue
            req = json.loads(line)
            n += 1
            try:
                ids, _ = encode_text(req["text"])
            except ValueError:
                ids = None
            window.append((req["id"], hashlib.sha256(req["text"].encode("utf-8")).hexdigest(), ids))
            if len(window) >= EMBED_WINDOW:
                flush()
        flush()
    state = {"schema": EMBED_SCHEMA, "status": "complete", "dimension": width, "requested": n, "supported": ok,
             "unsupported": n - ok, "processed": n, "requests_sha256": bridge.sha256(out / "requests.jsonl"),
             "rows_sha256": bridge.sha256(out / "rows.jsonl"), "representation_contract": contract,
             "representation_contract_sha256": hashlib.sha256(bridge.canonical(contract)).hexdigest()}
    bridge.write_json(out / "manifest.json", state)
    print(json.dumps({"complete": True, "rows": n, "unsupported": n - ok, "dimension": width}), flush=True)


# ------------------------------------------------------------------ id, check

def identity(args):
    cls = validate_class(args.teacher_class)
    print(json.dumps({"teacher_class": args.teacher_class, "artifact_id": artifact_id(args.teacher_class),
                      "repository": cls["repository"], "revision": cls["revision"], "width": cls["hidden_size"],
                      "files": cls["files"]}, sort_keys=True))


def check(args):
    digests = verify_snapshot(args.artifact, args.teacher_class)
    print(json.dumps({"complete": True, "teacher_class": args.teacher_class,
                      "artifact_id": artifact_id(args.teacher_class), "files": digests}, sort_keys=True))


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    a = sub.add_parser("id")
    a.add_argument("--teacher-class", required=True)
    a.set_defaults(func=identity)
    a = sub.add_parser("check")
    a.add_argument("--teacher-class", required=True)
    a.add_argument("--artifact", required=True, help="the downloaded snapshot directory")
    a.set_defaults(func=check)
    a = sub.add_parser("embed")
    a.add_argument("--teacher-class", required=True)
    for k in ("artifact", "requests", "output"):
        a.add_argument("--" + k, required=True)
    a.add_argument("--device", choices=("cpu", "cuda"), default="cpu")
    a.add_argument("--max-padded", type=int, default=0,
                   help="padded tokens per forward (0: one unpadded request per forward)")
    a.set_defaults(func=embed)
    a = sub.add_parser("raw")
    for k in ("data", "config", "fit", "artifact", "out"):
        a.add_argument("--" + k, required=True)
    a.add_argument("--part", choices=RAW_PARTS, required=True)
    a.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
    a.add_argument("--allow-diagnostic-subset", action="store_true",
                   help="smoke only: accept a diagnostic Train-subset fit")
    a.set_defaults(func=raw)
    args = p.parse_args(argv)
    args.func(args)


if __name__ == "__main__":
    main()

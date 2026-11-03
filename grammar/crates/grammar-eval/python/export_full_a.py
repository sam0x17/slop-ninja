"""Raw Development/CAL predictions and Rust exports for a completed full-A fit.

raw:    uncalibrated raw heads for one non-Test partition (input to Rust CAL),
        plus per-row token-head logits when the fit has a token head.
export: full encoder safetensors + numeric heads JSON (v2: optional spans-v1
        token head and span emission spec) (+ frozen Rust CAL map) + synthetic
        reference requests (the built-in cases, then any `--synthetic-cases`
        file) and their Python reference outputs (document outputs,
        token offsets and probabilities, words, sentences and spans), and
        optionally a stratified local Dev-iter reference set (private text,
        never published). Export v3 (heads v2) for every fit without a span
        tower, unchanged; export v4 (heads v3 plus `tower.safetensors`) for a
        spans-S2 span-tower fit (S2c): the tower copies of the top encoder
        layers and of the final norm, and the token head that reads them. The
        encoder and document heads of a tower fit are the init fit's, checked
        bit for bit.

No corpus discovery, calibration fitting, selection or Test access here. The
Test route needs a separate sealed implementation and is refused.
"""
from __future__ import annotations

import argparse
import contextlib
import copy
import json
import os
from pathlib import Path
import shutil

os.environ.setdefault("MKL_CBWR", "COMPATIBLE")
os.environ.setdefault("ATEN_CPU_CAPABILITY", "default")

import torch
from safetensors.torch import load_file, save_file

import hashlib

import full_a
import full_a_bundle as bundle
import full_a_s2 as s2
import full_a_spans as spans_rule
import last_block_a as base
import shared_a as shared
import train_full_a as trainer
import v6_embeddings as bridge
from warm_start_a import initializer


RAW_SCHEMA = "slop_ninja_full_a_raw_predictions_v2"
EXPORT_SCHEMA = "slop_ninja_full_a_export_v3"
CALIBRATION_SCHEMA = "slop_ninja_full_a_calibration_v3"
# spans-S2 span-tower export: heads v3 with the tower declaration; tower weights in their own FP32 file.
EXPORT_SCHEMA_TOWER = "slop_ninja_full_a_export_v4"
HEADS_SCHEMA_TOWER = "slop_ninja_full_a_heads_v3"
TOWER_FILE = "tower.safetensors"
TOWER_PREFIX = "span_tower."
TOWER_INPUT = "span_tower_final_norm_token_states"
TOWER_LAYER_TENSORS = ("attn_norm.weight", "attn.Wqkv.weight", "attn.Wo.weight", "mlp_norm.weight", "mlp.Wi.weight",
                       "mlp.Wo.weight")
TOWER_CAPTURE = "hidden_state_entering_source_layer"
LOCAL_REFERENCE_KEY = "slopninja-a-spans-v1:local-reference:"
SYNTHETIC = [
    {"id": "synthetic-short", "text": "The gardener carried a small basket through the quiet village."},
    {"id": "synthetic-unicode", "text": "“Café,” said the traveller. Rain fell at 3:15.\n\nWe stayed indoors."},
    {"id": "synthetic-long", "text": "The experiment measured rainfall, temperature, and plant growth. " * 48},
]
# spans-v1 synthetic span cases (R1/R2): programmatic text only.
SPAN_SYNTHETIC = [
    {"id": "span-crlf", "text": "First line here.\r\nSecond line follows.\r\n\r\nA new paragraph after a blank line.\r"
                                "Old style break.\r\n\r\n  Indented closing words.\r\n"},
    {"id": "span-nfc", "text": "Cafe\u0301 au lait was served. The re\u0301sume\u0301 and the Ångstro\u0308m "
                               "note were ready.\nA\u030a final line."},
    {"id": "span-emoji", "text": "We cheered 🎉🎉 loudly! The 👩\u200d👩\u200d👧 family waved 👋🏽. Done 👍."},
    {"id": "span-combining", "text": "Z\u0335a\u0336l\u0337g\u0338o text appears here. Na\u0303o e\u0302 isto? "
                                     "Plain words close the case."},
    {"id": "span-zero-width", "text": "Zero\u200bwidth spaces\u200d and non\u200cjoiners stay inside words. "
                                      "A soft\u00adhyphen too.\ufeff The end."},
    {"id": "span-single-word", "text": "Hello"},
    {"id": "span-one-word-lines", "text": "Apples\nPears\nPlums\n\nFigs\nDates\n"},
    {"id": "span-no-punctuation", "text": "this text has no sentence punctuation at all and it just keeps going on "
                                          "and on without any stop or capital letter"},
    {"id": "span-quotes-brackets", "text": "\"Is it done?\" she asked. (Yes.) [Maybe!] 'Fine.' «Oui.» {Ok.} "
                                           "The end… “Really?” he said."},
    {"id": "span-unicode-spaces", "text": "Non\u00a0breaking\u2003em space\u3000ideographic text.\u2028Line "
                                          "separator here. Unit\u001fseparator and\u001crecord marks. Tabs\tand  "
                                          "double  spaces."},
    {"id": "span-mixed-script", "text": "Ελληνικά κείμενα εδώ. Русский текст тоже! 中文句子在这里。日本語の文です。 "
                                        "Done now."},
]
LONG_CASE_ID = "span-2048-tokens"
REFERENCE_PRECISION = "fp64"
# The FP64 gate reference is the trained function evaluated in float64: HF's own
# ModernBertRotaryEmbedding (float32 angles and cos/sin, autocast disabled, then
# cast to float64), never angles recomputed in float64 (protocol v1.1 A1).
REFERENCE_ROTARY = "hf_float32_tables_widened_v1"
REFERENCE_FUNCTION = ("trained FP32 function in float64: FP32 weights widened exactly; HF rotary tables unchanged "
                      "(float32 angles and cos/sin, widened); attention softmax in float64 instead of HF's float32 "
                      "upcast (rounding only: identical to HF for float32 inputs); heads and token states in float64")


def eager_attention_reference(module, qkv, attention_mask, sliding_window_mask, position_ids, local_attention, bs,
                              dim, output_attentions=False, **_kwargs):
    """HF ModernBERT eager attention with the softmax in the query dtype (HF
    upcasts to float32 even for float64, which also turns a float64 mask
    minimum into -inf). Identical to HF for float32 inputs; for the float64
    reference the difference is rounding only (the same softmax, computed in
    float64). Rotary tables are HF's own. Inference only."""
    from transformers.models.modernbert import modeling_modernbert as mb
    shared.demand(not output_attentions and not module.training, "reference attention: inference only")
    cos, sin = module.rotary_emb(qkv, position_ids=position_ids)
    query, key, value = qkv.transpose(3, 1).unbind(dim=2)
    query, key = mb.apply_rotary_pos_emb(query, key, cos, sin)
    weights = torch.matmul(query, key.transpose(2, 3)) * module.head_dim ** -0.5
    if local_attention != (-1, -1):
        attention_mask = sliding_window_mask
    weights = torch.nn.functional.softmax(weights + attention_mask, dim=-1, dtype=query.dtype)
    return (torch.matmul(weights, value).transpose(1, 2).contiguous().view(bs, -1, dim),)


@contextlib.contextmanager
def reference_attention():
    """Swap the eager attention for eager_attention_reference while FP64
    references are computed; restored afterwards."""
    from transformers.models.modernbert import modeling_modernbert as mb
    table = mb.MODERNBERT_ATTENTION_FUNCTION
    original = table["eager"]
    table["eager"] = eager_attention_reference
    try:
        yield
    finally:
        table["eager"] = original


def fp64_reference(model):
    """A float64 copy of the restored model for the replay gate reference: FP32
    weights widened exactly, heads and token states kept in float64. The rotary
    embedding is HF's own (default rope only): its float32 angle and cos/sin
    tables are the trained function's and are only widened. The FP32 model's
    outputs are exported as the FP32 drift diagnostic. Returns (model, number
    of rotary embeddings checked)."""
    m = copy.deepcopy(model).double()
    m.output_dtype = torch.float64
    rotary = 0
    for module in m.encoder.modules():
        if type(module).__name__ == "ModernBertRotaryEmbedding":
            shared.demand(getattr(module, "rope_type", "default") == "default", "default rope only")
            shared.demand(type(module).forward is module.forward.__func__, "rotary forward must be HF's own")
            rotary += 1
    shared.demand(rotary > 0, "no rotary embedding found")
    return m.eval(), rotary


# ------------------------------------------------------------------ span tower (export v4)

def is_tower_fit(binding):
    return trainer.s2_mechanism(binding["arm_definition"]) == "span_tower"


def tower_rotary(model):
    """The tower copies' rotary embeddings: HF's own default rope, one per copied layer, each with its source
    encoder layer's rotary base (inv_freq and attention scaling equal, so local and global bases stay apart)."""
    count = 0
    for module in model.span_tower.modules():
        if type(module).__name__ == "ModernBertRotaryEmbedding":
            shared.demand(getattr(module, "rope_type", "default") == "default", "default rope only")
            shared.demand(type(module).forward is module.forward.__func__, "rotary forward must be HF's own")
            count += 1
    shared.demand(count == len(model.span_tower.layers), "one rotary embedding per tower layer")
    encoder_layers = model.encoder.model.layers
    first = len(encoder_layers) - len(model.span_tower.layers)
    for j, layer in enumerate(model.span_tower.layers):
        mine, source = layer.attn.rotary_emb, encoder_layers[first + j].attn.rotary_emb
        shared.demand(torch.equal(mine.inv_freq, source.inv_freq) and
                      mine.attention_scaling == source.attention_scaling,
                      "tower copy's rotary base differs from its source layer's")
    return count


def tower_declaration(model):
    """Heads v3 `span_tower`: the encoder layers the tower copies, each copy's attention kind (HF's rule:
    global when layer_id % global_attn_every_n_layers == 0, else local with the encoder's window), the capture
    point and the weights file. The copies read the hidden state entering the first copied layer with the
    encoder's own masks and positions, and the token head reads the copied final norm's output."""
    config = model.encoder.config
    n, k = config.num_hidden_layers, model.tower_layers
    layers = list(range(n - k, n))
    attention = ["global" if i % config.global_attn_every_n_layers == 0 else "local" for i in layers]
    shared.demand(len(model.span_tower.layers) == k, "tower depth differs from the fit")
    for layer, i, kind in zip(model.span_tower.layers, layers, attention):
        window = (-1, -1) if kind == "global" else (config.local_attention // 2,) * 2
        shared.demand(layer.attn.layer_id == i and tuple(layer.attn.local_attention) == window,
                      "tower copy differs from its encoder layer's attention")
    return {"rule": s2.TOWER_RULE, "encoder_layers": n, "source_layer": n - k, "layers": layers,
            "attention": attention, "global_attn_every_n_layers": config.global_attn_every_n_layers,
            "local_attention": config.local_attention, "capture": TOWER_CAPTURE, "masks_and_positions": "encoder",
            "final_norm": "copy", "weights": TOWER_FILE, "tensor_prefix": TOWER_PREFIX}


def tower_tensor_names(layers):
    return sorted([f"{TOWER_PREFIX}layers.{j}.{t}" for j in range(layers) for t in TOWER_LAYER_TENSORS] +
                  [TOWER_PREFIX + "norm.weight"])


def tower_tensors(model):
    """The restored tower's tensors (CPU, FP32, contiguous) under the trainer's heads.safetensors names."""
    out = {TOWER_PREFIX + k: v.detach().to("cpu").contiguous() for k, v in model.span_tower.state_dict().items()}
    shared.demand(sorted(out) == tower_tensor_names(len(model.span_tower.layers)), "tower tensor roster")
    shared.demand(all(v.dtype == torch.float32 and bool(torch.isfinite(v).all()) for v in out.values()),
                  "tower tensors must be finite FP32")
    return out


def tower_fit_binding(fit, report, binding):
    """Hashes binding a span-tower fit, and the bitwise checks that its encoder and document heads are the init
    fit's (for seed 17, R0's): the encoder file is the init fit's file, and every document tensor of the fit's
    heads.safetensors equals the init fit's. Returns the record written into the v4 manifest."""
    s = binding["s2"]
    shared.demand(s["mechanism"] == "span_tower" and s["rule"] == s2.TOWER_RULE and
                  s["module_sha256"] == bridge.sha256(s2.__file__), "span tower module or rule differs from the fit")
    check = report["frozen_document_check"]
    shared.demand(check["encoder_and_document_heads_bitwise_equal_to_init"] is True and
                  check["selected_dev_iter_document_raw_vs_init"]["document_raw_max_abs_error"] == 0.,
                  "tower fit without its frozen-document check")
    require(fit / "heads.safetensors", report["heads_sha256"])
    init = binding["init_fit"]
    init_dir = Path(init["fit_dir"])
    require(init_dir / "fit.json", init["fit_sha256"])
    init_report = read(init_dir / "fit.json")
    shared.demand(init_report["selected_checkpoint_sha256"] == init["selected_checkpoint_sha256"],
                  "init fit checkpoint differs")
    require(init_dir / "encoder.safetensors", init_report["encoder_sha256"])
    require(init_dir / "heads.safetensors", init_report["heads_sha256"])
    shared.demand(report["encoder_sha256"] == init_report["encoder_sha256"], "tower fit encoder differs from the init fit")
    own, base_heads = load_file(str(fit / "heads.safetensors")), load_file(str(init_dir / "heads.safetensors"))
    document = sorted(k for k in own if not k.startswith(("token_head.", TOWER_PREFIX)))
    shared.demand(document and document == sorted(k for k in base_heads if not k.startswith("token_head.")) and
                  all(own[k].dtype == base_heads[k].dtype and torch.equal(own[k], base_heads[k]) for k in document),
                  "tower fit document heads differ from the init fit")
    return {"fit_json_sha256": bridge.sha256(fit / "fit.json"), "binding_json_sha256": bridge.sha256(fit / "binding.json"),
            "encoder_sha256": report["encoder_sha256"], "heads_sha256": report["heads_sha256"],
            "selected_checkpoint_sha256": report["selected_checkpoint_sha256"],
            "s2_module_sha256": s["module_sha256"],
            "init_fit": {"fit_dir": str(init_dir), "fit_sha256": init["fit_sha256"],
                         "selected_checkpoint_sha256": init["selected_checkpoint_sha256"],
                         "encoder_sha256": init_report["encoder_sha256"], "heads_sha256": init_report["heads_sha256"]},
            "encoder_equal_to_init_fit": True, "document_heads_equal_to_init_fit": True,
            "document_head_tensors": len(document), "frozen_document_check": check}, own


def read(p):
    return json.loads(Path(p).read_bytes())


def require(path, expected):
    shared.demand(bridge.sha256(path) == expected, "changed bound file: " + str(path))


def long_case(tokenize):
    """Deterministic synthetic document of exactly 2,048 tokens (or the
    longest below it): seeded word sentences, trimmed by whole words."""
    import random
    import synthetic_full_a
    rng = random.Random(20261030)
    text = synthetic_full_a.synthetic_text(rng, 3000)
    words = text.split(" ")
    lo, hi = 1, len(words)
    shared.demand(len(tokenize(" ".join(words[:1]))[0]) <= full_a.MAX_TOKENS, "long case seed")
    while lo < hi:  # largest prefix of whole words within MAX_TOKENS
        mid = (lo + hi + 1) // 2
        ids = tokenize_count(tokenize, " ".join(words[:mid]))
        if ids <= full_a.MAX_TOKENS:
            lo = mid
        else:
            hi = mid - 1
    return {"id": LONG_CASE_ID, "text": " ".join(words[:lo])}


def tokenize_count(tokenize, text):
    try:
        return len(tokenize(text)[0])
    except ValueError:
        return full_a.MAX_TOKENS + 1


def synthetic_cases(tokenize, token_head, extra=()):
    """Release synthetic cases, plus the spans-v1 span cases with a token head, then `extra` (case_file) in
    file order."""
    return SYNTHETIC + (SPAN_SYNTHETIC + [long_case(tokenize)] if token_head else []) + list(extra)


CASE_KEYS = {"id", "text"}


def case_file(path, tokenize, taken):
    """Synthetic reference cases from a JSONL file, one {"id", "text"} object per line: text written for
    publication, never corpus or evaluation text. Every id is unique, differs from `taken` (the built-in
    cases) and never starts with the private `local-` prefix; every text is non-empty and tokenizes within
    MAX_TOKENS without truncation. The cases join reference-requests.jsonl after the built-in ones."""
    cases, ids = [], set(taken)
    with open(path, "rb") as f:
        for n, line in enumerate(f, 1):
            shared.demand(line.endswith(b"\n") and line.strip(), f"synthetic case line {n}: one object per line")
            row = json.loads(line)
            shared.demand(isinstance(row, dict) and set(row) == CASE_KEYS and isinstance(row["id"], str) and
                          row["id"] and isinstance(row["text"], str), f"synthetic case line {n}: id and text only")
            shared.demand(row["id"] not in ids and not row["id"].startswith("local-"),
                          f"synthetic case line {n}: duplicate or reserved id")
            tokenize(row["text"])
            ids.add(row["id"])
            cases.append({"id": row["id"], "text": row["text"]})
    shared.demand(cases, "synthetic case file has no cases")
    return cases


def local_cases(root, tokenize, special, ai_profiles, count):
    """`count` Dev-iter rows stratified by (origin, domain): round robin over
    strata ordered by name, each stratum in ascending sha256(key + id).
    Private text: written only into the export's local reference files."""
    if not count:
        return []
    strata = {}
    texts = bundle.TextSource(root)
    try:
        for m, raw in bundle.stream(root, ["dev_iter"]):
            key = hashlib.sha256((LOCAL_REFERENCE_KEY + m.id).encode()).hexdigest()
            strata.setdefault((m.origin, m.domain), []).append((key, m, raw))
        for v in strata.values():
            v.sort(key=lambda x: x[0])
        chosen, depth = [], 0
        while len(chosen) < count and any(depth < len(v) for v in strata.values()):
            for k in sorted(strata, key=lambda k: (k[0], k[1])):
                if depth < len(strata[k]) and len(chosen) < count:
                    chosen.append(strata[k][depth])
            depth += 1
        return [{"id": "local-" + m.id, "text": texts.text(raw, m)} for _, m, raw in chosen]
    finally:
        texts.close()


def completed_fit(root, config, fit, init, allow_subset=False):
    report = read(fit / "fit.json")
    binding = report["binding"]
    # The digests the fit bound, recomputed: whole files, or the blocks it read when the bundle holds a Dev-select
    # or CAL block it never opened (full_a_bundle.binding_digests; that block is not read here either).
    digests = bundle.binding_digests(root, binding["bundle_sha256"])
    shared.demand(report["schema"] == trainer.FIT_SCHEMA and report["complete"] is True and
                  report["final_test_opened"] is False and binding == read(fit / "binding.json") and
                  binding["bundle_sha256"] == digests and
                  binding["contract_sha256"] == bridge.sha256(config / "contract.json") and
                  binding["protocol_sha256"] == bridge.sha256(config / "protocol.json"),
                  "completed fit on this bundle and config required")
    # A diagnostic Train-subset fit is never admissible for export or calibration.
    shared.demand(allow_subset or (report.get("diagnostic_train_subset") is None and
                                   binding.get("diagnostic_train_subset") is None),
                  "diagnostic Train-subset fits are never exported (smoke exports need --allow-diagnostic-subset)")
    log = report["cohort_weight_log"]
    shared.demand(log["complete"] is True and bridge.sha256(fit / log["file"]) == log["sha256"] and
                  read(fit / log["file"])["complete"] is True, "a fit without its cohort weight log is invalid")
    require(init, binding["initializer_sha256"])
    for module, key in ((full_a, "model_sha256"), (base, "architecture_sha256"), (shared, "shared_loss_sha256"),
                        (bridge, "artifact_loader_sha256"), (bundle, "bundle_reader_sha256"),
                        (spans_rule, "spans_rule_sha256")):
        require(module.__file__, binding[key])
    require(fit / "selected.pt", report["selected_checkpoint_sha256"])
    saved = torch.load(fit / "selected.pt", map_location="cpu", weights_only=False)
    shared.demand(saved["binding"] == binding and saved["evaluation"] == report["selected_evaluation"],
                  "selected checkpoint lineage differs")
    return digests, report, binding, saved


def restore_model(args, root, config, binding, saved, device):
    contract = trainer.validate_contract(read(config / "contract.json"), binding["bundle_sha256"][bundle.MANIFEST])
    protocol = trainer.validate_protocol(read(config / "protocol.json"))
    arm = protocol["arms"][binding["arm"]]
    shared.demand(arm == binding["arm_definition"], "arm definition differs")
    encoder, tokenizer, parent = trainer.load(args, protocol, device)
    shared.demand(parent["artifact_id"] == contract["parent_artifact_id"] == binding["parent_artifact_id"],
                  "parent differs")
    model, profiles = trainer.build(encoder, contract, initializer(args.init), arm, protocol, device)
    if trainer.s2_mechanism(arm) == "span_tower":
        # spans-S2 tower: the module is created, then every tensor comes from the checkpoint.
        model.attach_tower()
    trainer.restore(model, saved["model"])
    model.token_loss_weight = 0. if arm["span_head"] is None else arm["span_head"]["token_loss_weight"]
    th = trainer.span_fields(arm)["token_head"]
    model.token_head_weight = 0. if th is None else th["loss_weight"]
    return model.eval(), tokenizer, contract, protocol, arm, profiles


RAW_SPLITS = ("dev_iter",) + trainer.REPORT_SPLITS


@torch.no_grad()
def raw(args):
    root, config, fit, out = (Path(p).resolve() for p in (args.data, args.config, args.fit, args.out))
    shared.demand(args.part in RAW_SPLITS, "Test cannot enter the non-Test route")
    shared.demand(not out.exists(), "new output required")
    digests, report, binding, saved = completed_fit(root, config, fit, args.init,
                                                    getattr(args, "allow_diagnostic_subset", False))
    device = torch.device(args.device)
    trainer.configure(device, trainer.validate_protocol(read(config / "protocol.json"))["cuda_memory_fraction"])
    model, tokenizer, contract, protocol, arm, profiles = restore_model(args, root, config, binding, saved, device)
    population = bundle.Population(root, args.part, full_a.hf_tokenize(tokenizer), set(tokenizer.all_special_ids),
                                   profiles["ai"], cohort_weighted=False)
    out.mkdir()
    tokens = (out / "token-logits.jsonl").open("xb") if model.token_head is not None else None
    with (out / "raw.jsonl").open("xb") as f, (out / "predictions.jsonl").open("xb") as g:
        for i, meta in enumerate(population.meta):
            ids, mask, content, _, words = population.batch([i], device, tokenizer.pad_token_id)
            o = model(ids, mask, content, autocast=False, words=words)
            f.write(bridge.canonical(trainer.raw_prediction(meta, o, population.lengths[i])) + b"\n")
            g.write(bridge.canonical(trainer.contract_prediction(meta, o, profiles["ai"])) + b"\n")
            if tokens is not None:
                tokens.write(bridge.canonical({"schema": trainer.TOKEN_DUMP_SCHEMA, "id": meta.id,
                                               "text_sha256": meta.text_sha256, "tokens": int(population.lengths[i]),
                                               "token_logits": o["span_logits"][0].double().tolist()}) + b"\n")
    if tokens is not None:
        tokens.close()
    bridge.write_json(out / "manifest.json", {"schema": RAW_SCHEMA, "complete": True, "partition": args.part,
        "rows": len(population), "arm": binding["arm"], "seed": binding["seed"],
        "share_mode": arm["share_mode"], "bundle_sha256": digests, "contract_sha256": binding["contract_sha256"],
        "fit_sha256": bridge.sha256(fit / "fit.json"), "selected_checkpoint_sha256": report["selected_checkpoint_sha256"],
        "raw_sha256": bridge.sha256(out / "raw.jsonl"), "predictions_sha256": bridge.sha256(out / "predictions.jsonl"),
        "token_logits_sha256": bridge.sha256(out / "token-logits.jsonl") if model.token_head is not None else None,
        "implementation_sha256": bridge.sha256(__file__),
        "final_test_opened": False, "calibrated": False, "device": str(device), "torch": torch.__version__,
        "involvement_logit_semantics": "log(p_model+p_mixed)-log(p_human) of the final uncalibrated origin",
        "origin_logits_semantics": "original_classifier_linear_divided_by_base_temperature; no CAL map"})
    print(json.dumps({"complete": True, "partition": args.part, "rows": len(population)}), flush=True)


OFFSETS_SCHEMA = "slop_ninja_full_a_span_offsets_v1"
SCORE_SCHEMA = "slop_ninja_full_a_scored_rows_v1"
# Row populations outside the fit bundle that score-rows may read. Test is the
# consumed Tier C Test, scored only for the labelled retrospective report.
SCORE_POPULATIONS = {"dev_iter_span_eval": "dev_iter", "test_retrospective": "test"}
RETROSPECTIVE_LABEL = "retrospective, Test consumed; not selection evidence"


def span_offsets_row(meta, text, ids, offsets, special):
    """Text-free span inputs of one row: char offsets, the word-token mask, the
    trainer's token labels and the share-word labels (spans-v1.1 evaluator input)."""
    ws = spans_rule.words(text)
    if meta.origin == 2:
        wl = spans_rule.word_labels(text, ws, meta.spans) if meta.spans is not None else [-1] * len(ws)
    else:
        wl = [0 if meta.origin == 0 else 1] * len(ws)
    return {"schema": OFFSETS_SCHEMA, "id": meta.id, "text_sha256": meta.text_sha256, "tokens": len(ids),
            "token_offsets": [list(o) for o in offsets],
            "word_mask": [int(x) for x in full_a.word_mask_for(text, ids, offsets, special)],
            "token_labels": full_a.token_labels(text, ids, offsets, special, meta.origin, meta.spans),
            "words": len(ws), "word_labels": wl}


def rows_of_file(path, split):
    """(Meta, text) for every row of a trainer-row JSONL file outside a fit bundle."""
    texts = bundle.TextSource(Path(path).parent)
    try:
        with open(path, "rb") as f:
            for n, line in enumerate(f, 1):
                if not line.strip():
                    continue
                m, raw_row = bundle.parse_row(json.loads(line), n)
                shared.demand(m.split == split, f"row {m.id}: split {m.split}, expected {split}")
                yield m, texts.text(raw_row, m)
    finally:
        texts.close()


def offsets(args):
    """Tokenizer-only span inputs for one bundle split (dev_iter or cal) or a rows file."""
    from transformers import AutoTokenizer
    out = Path(args.out)
    shared.demand(not out.exists(), "new output required")
    tokenizer = AutoTokenizer.from_pretrained(Path(args.artifact) / "classifier", local_files_only=True)
    tokenize, special = full_a.hf_tokenize(tokenizer), set(tokenizer.all_special_ids)
    if args.rows is not None:
        shared.demand(args.population in SCORE_POPULATIONS, "rows-file population")
        source, digest = rows_of_file(args.rows, SCORE_POPULATIONS[args.population]), bridge.sha256(args.rows)
    else:
        shared.demand(args.part in ("dev_iter", "cal"), "bundle offsets are for dev_iter or cal")
        texts = bundle.TextSource(Path(args.data))
        source = ((m, texts.text(r, m)) for m, r in bundle.stream(args.data, [args.part]))
        digest = bridge.sha256(Path(args.data) / bundle.ROWS)
    n = 0
    with out.open("xb") as f:
        for m, text in source:
            ids, offs = tokenize(text)
            f.write(bridge.canonical(span_offsets_row(m, text, ids, offs, special)) + b"\n")
            n += 1
    bridge.write_json(out.with_suffix(".manifest.json"), {"schema": OFFSETS_SCHEMA + "_manifest", "rows": n,
        "source_sha256": digest, "part": args.part, "population": args.population, "offsets_sha256": bridge.sha256(out),
        "implementation_sha256": bridge.sha256(__file__), "tokenizer": str(Path(args.artifact) / "classifier")})
    print(json.dumps({"complete": True, "rows": n}), flush=True)


@torch.no_grad()
def score_rows(args):
    """Raw heads, token logits and span inputs of a completed token-head fit on a
    rows file outside its bundle: the Dev-iter span-evaluation halves, or the
    consumed Tier C Test for the labelled retrospective report only."""
    root, config, fit, out = (Path(p).resolve() for p in (args.data, args.config, args.fit, args.out))
    shared.demand(args.population in SCORE_POPULATIONS, "score-rows population")
    shared.demand(not out.exists(), "new output required")
    split = SCORE_POPULATIONS[args.population]
    if split == "test":
        record = read(args.test_consumed_record)
        shared.demand(record.get("schema") == "slop_ninja_a_comp_tierc_test_consumed_v1" and
                      record.get("consumed") is True and record.get("split") == "test",
                      "the retrospective Test route needs the TEST-CONSUMED record")
    else:
        shared.demand(args.test_consumed_record is None, "a Test record only with test_retrospective")
    digests, report, binding, saved = completed_fit(root, config, fit, args.init)
    device = torch.device(args.device)
    trainer.configure(device, trainer.validate_protocol(read(config / "protocol.json"))["cuda_memory_fraction"])
    model, tokenizer, contract, protocol, arm, profiles = restore_model(args, root, config, binding, saved, device)
    shared.demand(model.token_head is not None, "score-rows needs a token-head fit")
    tokenize, special = full_a.hf_tokenize(tokenizer), set(tokenizer.all_special_ids)
    out.mkdir()
    n = 0
    names = ("raw.jsonl", "predictions.jsonl", "token-logits.jsonl", "offsets.jsonl")
    files = [(out / k).open("xb") for k in names]
    try:
        for m, text in rows_of_file(args.rows, split):
            ids, offs = tokenize(text)
            content = full_a.content_mask_for(ids, special)
            words = full_a.word_mask_for(text, ids, offs, special)
            t = lambda v, dt: torch.tensor([v], dtype=dt, device=device)  # noqa: E731
            o = model(t(ids, torch.long), t([1] * len(ids), torch.long), t(content, torch.bool),
                      autocast=False, words=t(words, torch.bool))
            rows = (trainer.raw_prediction(m, o, len(ids)), trainer.contract_prediction(m, o, profiles["ai"]),
                    {"schema": trainer.TOKEN_DUMP_SCHEMA, "id": m.id, "text_sha256": m.text_sha256, "tokens": len(ids),
                     "token_logits": o["span_logits"][0].double().tolist()},
                    span_offsets_row(m, text, ids, offs, special))
            for f, r in zip(files, rows):
                f.write(bridge.canonical(r) + b"\n")
            n += 1
    finally:
        for f in files:
            f.close()
    bridge.write_json(out / "manifest.json", {"schema": SCORE_SCHEMA, "complete": True, "population": args.population,
        "label": RETROSPECTIVE_LABEL if split == "test" else "span evaluation only; never a Train input",
        "rows": n, "rows_file": str(Path(args.rows).resolve()), "rows_sha256": bridge.sha256(args.rows),
        "arm": binding["arm"], "seed": binding["seed"], "bundle_sha256": digests,
        "contract_sha256": binding["contract_sha256"], "fit_sha256": bridge.sha256(fit / "fit.json"),
        "selected_checkpoint_sha256": report["selected_checkpoint_sha256"],
        **{k.replace(".jsonl", "_sha256").replace("-", "_"): bridge.sha256(out / k) for k in names},
        "implementation_sha256": bridge.sha256(__file__), "calibrated": False, "device": str(device),
        "torch": torch.__version__,
        "test_consumed_record_sha256": bridge.sha256(args.test_consumed_record) if split == "test" else None})
    print(json.dumps({"complete": True, "population": args.population, "rows": n}), flush=True)


def calibration(path, binding, checkpoint_hash, contract_hash, token_head=False):
    """(involvement map, role thresholds, calibrated, calibration document, span spec).

    v3 adds "spans": the span emission spec (decoder and the CAL sentence/span
    confidence maps) for a token-head fit, null otherwise. Uncalibrated exports
    use identity maps and the declared diagnostic decoder."""
    if path is None:
        spec = spans_rule.spec_for() if token_head else None
        return {"method": "identity_v1"}, {"human": 1., "ai": 1.}, False, None, spec
    value = read(path)
    shared.demand(value["schema"] == CALIBRATION_SCHEMA and value["complete"] is True and
                  value["fit_split"] == "cal" and value["final_test_opened"] is False and
                  value["arm"] == binding["arm"] and value["seed"] == binding["seed"] and
                  value["selected_checkpoint_sha256"] == checkpoint_hash and value["contract_sha256"] == contract_hash and
                  value["involvement"]["selected"]["method"] in full_a.CALIBRATION_METHODS[1:] and
                  all(value["roles"][r]["qualified"] is True or value["roles"][r].get("roster_empty") is True
                      for r in shared.ROLES),
                  "qualified CAL-only lineage required")
    shared.demand("spans" in value and (value["spans"] is None) == (not token_head),
                  "calibration v3 needs the span emission spec exactly when the fit has a token head")
    spec = None if value["spans"] is None else spans_rule.validate_spec(value["spans"])
    # An empty roster has no named threshold; its role always returns other_<role>.
    return (value["involvement"]["selected"],
            {r: 1. if value["roles"][r].get("roster_empty") is True else value["roles"][r]["threshold"]
             for r in shared.ROLES}, True, value, spec)


def tensor_json(t):
    return {"shape": list(t.shape), "values": t.detach().double().cpu().flatten().tolist()}


def heads_artifact(model, arm, contract, binding, init_path, description, thresholds, supervision, span_spec=None,
                   tower=None):
    """Heads v2; with a span-tower declaration `tower`, heads v3: the same document heads and token head, the
    token head reading the tower's normed states, and the `span_tower` declaration (weights in tower.safetensors)."""
    artifact = heads_artifact_v2(model, arm, contract, binding, init_path, description, thresholds, supervision,
                                 span_spec)
    if tower is None:
        return artifact
    shared.demand(artifact["token_head"] is not None and getattr(model, "span_tower", None) is not None,
                  "the span tower feeds the token head")
    artifact["schema"] = HEADS_SCHEMA_TOWER
    artifact["token_head"]["input"] = TOWER_INPUT
    artifact["span_tower"] = tower
    return artifact


def heads_artifact_v2(model, arm, contract, binding, init_path, description, thresholds, supervision, span_spec=None):
    heads = model.heads
    weights = {k: tensor_json(v) for k, v in heads.named_parameters()}
    if model.span is not None:
        weights.update({"span." + k: tensor_json(v) for k, v in model.span.named_parameters() if k != "gate"})
    token_head = getattr(model, "token_head", None)
    shared.demand((token_head is None) == (span_spec is None), "span emission spec iff token head")
    if token_head is not None:
        weights.update({"token_head." + k: tensor_json(v) for k, v in model.token_head.named_parameters()})
    # An empty roster's head is a one-row training placeholder that no output
    # reads; the artifact carries it as a zero-row head, as the architecture says.
    for role in shared.ROLES:
        if not contract[role + "_profiles"]:
            weights[role + ".weight"] = {"shape": [0, 768], "values": []}
            weights[role + ".bias"] = {"shape": [0], "values": []}
    return {"schema": full_a.HEADS_SCHEMA,
            "architecture": {"dimension": 2588, "raw_text_dimension": 768, "hidden": 64, "embedding": 768,
                             "human_profiles": len(contract["human_profiles"]),
                             "ai_profiles": len(contract["ai_profiles"])},
            "span_head": None if arm["span_head"] is None else {"pooling": arm["span_head"]["pooling"],
                                                                 "gate": float(model.span.gate.detach())},
            "token_head": None if token_head is None else {
                "hidden": model.token_head.width, "activation": full_a.TOKEN_HEAD_ACTIVATION,
                "rule": full_a.TOKEN_HEAD_RULE, "input": "final_norm_token_states", "label_policy": full_a.SPAN_LABEL_POLICY,
                "feeds_document_heads": False},
            "span_emission": span_spec,
            "share_mode": arm["share_mode"], "contract_sha256": binding["contract_sha256"],
            "initialization_sha256": bridge.sha256(init_path), "base_temperature": heads.base_temperature,
            "text_mean": heads.text_mean.double().tolist(), "text_scale": heads.text_scale.double().tolist(),
            "profiles": {r: contract[r + "_profiles"] for r in shared.ROLES}, "thresholds": thresholds,
            "involvement_calibration": description, "supervision": supervision,
            "origin_order": ["human_only", "model_only", "mixed"], "weights": weights}


def roles(out, involvement, profiles, thresholds):
    """Conditional role results; an empty roster always returns other_<role>."""
    result = {}
    for role, p in (("human", involvement["human_involvement_probability"]),
                    ("ai", involvement["ai_involvement_probability"])):
        names = profiles[role]
        if not names:
            result[role] = {"profile": f"other_{role}", "involvement_probability": p,
                            "conditional_named_scores": {}, "fallback_asserts_presence": False}
            continue
        scores = out[f"{role}_logits"][0].double().softmax(-1).tolist()
        best = max(range(len(scores)), key=lambda i: (scores[i], -i))
        named = scores[best] >= thresholds[role]
        result[role] = {"profile": names[best] if named else f"other_{role}", "involvement_probability": p,
                        "conditional_named_scores": dict(zip(names, scores)),
                        "fallback_asserts_presence": False}
    return result


@torch.no_grad()
def export(args):
    root, config, fit, out = (Path(p).resolve() for p in (args.data, args.config, args.fit, args.out))
    shared.demand(not out.exists(), "new export required")
    subset = getattr(args, "allow_diagnostic_subset", False)
    digests, report, binding, saved = completed_fit(root, config, fit, args.init, subset)
    has_token_head = trainer.span_fields(binding["arm_definition"])["token_head"] is not None
    # A spans-S2 span-tower fit (S2c) takes export v4; every other fit takes export v3, unchanged.
    tower_fit = is_tower_fit(binding)
    if tower_fit:
        shared.demand(has_token_head, "the span tower feeds the token head")
        fit_hashes, fit_heads = tower_fit_binding(fit, report, binding)
    description, thresholds, calibrated, cal, span_spec = calibration(
        args.calibration, binding, report["selected_checkpoint_sha256"], binding["contract_sha256"], has_token_head)
    shared.demand(not (subset and calibrated), "diagnostic Train-subset exports are never calibrated")
    require(fit / "encoder.safetensors", report["encoder_sha256"])
    device = torch.device(args.device)
    trainer.configure(device, trainer.validate_protocol(read(config / "protocol.json"))["cuda_memory_fraction"])
    model, tokenizer, contract, protocol, arm, profiles = restore_model(args, root, config, binding, saved, device)
    trained = report["trained_selected_heads"]
    # The shared-contract bundle carries no human identities (empty human roster).
    shared.demand(not calibrated or all(trained[k] for k in ("origin", "share", "ai")),
                  "calibrated A needs supervised fitted heads")
    supervision = {k: int(bool(v)) for k, v in trained.items()}
    tower, tower_weights = None, None
    if tower_fit:
        tower = tower_declaration(model)
        tower_weights = tower_tensors(model)
        # The restored tower and token head are the fit's heads.safetensors tensors, bit for bit.
        shared.demand(all(torch.equal(v, fit_heads[k]) for k, v in tower_weights.items()) and
                      all(torch.equal(v.detach().cpu(), fit_heads["token_head." + k])
                          for k, v in model.token_head.state_dict().items()),
                      "restored tower or token head differs from the fit's heads.safetensors")
    artifact = heads_artifact(model, arm, contract, binding, args.init, description, thresholds, supervision,
                              span_spec, tower)
    tokenize = full_a.hf_tokenize(tokenizer)
    special = set(tokenizer.all_special_ids)

    model64, rotary_modules = fp64_reference(model)
    tower_rotary_modules = tower_rotary(model64) if tower_fit else None

    def reference(case, m, precision):
        ids, offsets = tokenize(case["text"])
        t = torch.tensor([ids], device=device)
        mask = torch.ones_like(t)
        content = torch.tensor([full_a.content_mask_for(ids, special)], device=device)
        word_mask = full_a.word_mask_for(case["text"], ids, offsets, special)
        words = torch.tensor([word_mask], device=device)
        o = m(t, mask, content, autocast=False, words=words)
        meta = bundle.Meta()
        meta.id, meta.text_sha256 = case["id"], bridge.digest(case["text"].encode())
        r = trainer.raw_prediction(meta, o, len(ids))
        outputs = full_a.calibrated_outputs(r, description, arm["share_mode"])
        prediction = {"origin": outputs["origin"], "ai_confidence_percent": outputs["ai_confidence_percent"],
                      "ai_share_percent": outputs["ai_share_percent"],
                      "attribution": roles(o, outputs, profiles, thresholds)}
        row = {"id": case["id"], "text_sha256": r["text_sha256"], "token_ids": ids, "word_mask": word_mask,
               "token_offsets": [list(x) for x in offsets], "reference_precision": precision,
               "h_raw": o["h"][0].double().cpu().tolist(), "z_shared": o["encoding"][0].double().cpu().tolist(),
               "raw": r, "prediction": prediction, "calibrated": calibrated, "span_emission": None}
        if model.token_head is not None:
            logits = o["span_logits"][0].double().cpu().tolist()
            try:
                e = spans_rule.emit(case["text"], offsets, word_mask, logits, outputs["ai_share_percent"] / 100.,
                                    outputs["ai_confidence_percent"] / 100., span_spec, diagnostics=True)
            except ValueError as error:
                prediction.update(sentences=None, spans=None)
                row["span_emission"] = {"status": "unsupported", "reason": str(error)}
            else:
                prediction.update(sentences=e.pop("sentences"), spans=e.pop("spans"))
                row["span_emission"] = {"status": "ok", **e}
            row["token_logits"] = logits
        return row

    cases = synthetic_cases(tokenize, model.token_head is not None)
    if getattr(args, "synthetic_cases", None):
        cases = synthetic_cases(tokenize, model.token_head is not None,
                                case_file(args.synthetic_cases, tokenize, {c["id"] for c in cases}))
    with reference_attention():
        references = [reference(case, model64, "fp64") for case in cases]
    references32 = [reference(case, model, "fp32") for case in cases]
    local = local_cases(root, tokenize, special, profiles["ai"], getattr(args, "local_references", 0) or 0)
    with reference_attention():
        local_refs = [reference(case, model64, "fp64") for case in local]
    local_refs32 = [reference(case, model, "fp32") for case in local]
    out.mkdir()
    shutil.copyfile(config / "contract.json", out / "contract.json")
    shutil.copyfile(fit / "encoder.safetensors", out / "encoder.safetensors")
    # The full roster must equal the parent's: same names, shapes and dtype.
    parent = load_file(str(Path(args.artifact) / "classifier" / "model.safetensors"))
    exported = load_file(str(out / "encoder.safetensors"))
    shared.demand(set(parent) == set(exported) and all(parent[k].shape == exported[k].shape and
                  exported[k].dtype == torch.float32 for k in parent), "exported encoder roster differs from V6")
    shared.demand(all(torch.equal(parent[k], exported[k]) for k in parent if k.startswith("classifier.")),
                  "V6 classifier must remain unchanged")
    bridge.write_json(out / "heads.json", artifact)
    if tower_fit:
        save_file(tower_weights, str(out / TOWER_FILE))
        back = load_file(str(out / TOWER_FILE))
        shared.demand(sorted(back) == sorted(tower_weights) and
                      all(back[k].dtype == torch.float32 and torch.equal(back[k], v) for k, v in tower_weights.items()),
                      "written tower weights differ")
    with (out / "reference-requests.jsonl").open("xb") as f:
        for case in cases:
            f.write(bridge.canonical(case) + b"\n")
    bridge.write_json(out / "reference-predictions.json", references)
    bridge.write_json(out / "reference-predictions-fp32.json", references32)
    if local:
        with (out / "local-reference-requests.jsonl").open("xb") as f:
            for case in local:
                f.write(bridge.canonical(case) + b"\n")
        bridge.write_json(out / "local-reference-predictions.json", local_refs)
        bridge.write_json(out / "local-reference-predictions-fp32.json", local_refs32)
    if cal:
        shutil.copyfile(args.calibration, out / "calibration.json")
    member = lambda name: {"file": name, "sha256": bridge.sha256(out / name)}
    manifest = {"schema": EXPORT_SCHEMA, "complete": True, "arm": binding["arm"],
        "seed": binding["seed"], "selected_evaluation": report["selected_evaluation"],
        "parent_artifact_id": binding["parent_artifact_id"],
        "parent_manifest_sha256": bridge.sha256(Path(args.artifact) / "manifest.json"),
        "contract": member("contract.json"), "encoder": member("encoder.safetensors"), "heads": member("heads.json"),
        "reference_requests": member("reference-requests.jsonl"),
        "reference_predictions": member("reference-predictions.json"),
        "reference_precision": REFERENCE_PRECISION, "reference_rotary": REFERENCE_ROTARY,
        "reference_function": REFERENCE_FUNCTION,
        "reference_rotary_modules": rotary_modules,
        "reference_predictions_fp32": {**member("reference-predictions-fp32.json"), "role": "fp32_drift_diagnostic"},
        "local_reference_requests": {**member("local-reference-requests.jsonl"), "contains_private_text": True,
                                     "split": "dev_iter", "rows": len(local),
                                     "rule": "round robin over (origin, domain) strata, ascending sha256(key+id)"}
        if local else None,
        "local_reference_predictions": member("local-reference-predictions.json") if local else None,
        "local_reference_predictions_fp32": {**member("local-reference-predictions-fp32.json"),
                                             "role": "fp32_drift_diagnostic"} if local else None,
        "token_head": artifact["token_head"], "span_emission": span_spec,
        "diagnostic_train_subset": report.get("diagnostic_train_subset"),
        "calibration": member("calibration.json") if cal else None, "calibrated": calibrated,
        "selected_checkpoint_sha256": report["selected_checkpoint_sha256"], "fit_binding": binding,
        "exporter_sha256": bridge.sha256(__file__), "final_test_opened": False, "reference_device": str(device),
        "production_qualified": False, "runtime": {"torch": torch.__version__, "cuda": torch.version.cuda},
        "limits": ["Rust full-encoder replay and runtime measurements still required.",
                   "Uncalibrated exports use identity_v1 and threshold 1; diagnostics only.",
                   "Uncalibrated token-head exports use identity sentence/span maps and the diagnostic decoder.",
                   "No Workers/Wasm or Container qualification follows from numeric export."] +
                  (["Diagnostic Train-subset (smoke) export: never calibrated, never released."] if subset else [])}
    if tower_fit:
        manifest["schema"] = EXPORT_SCHEMA_TOWER
        manifest["span_tower"] = {**member(TOWER_FILE), "declaration": tower, "tensors": len(tower_weights),
                                  "parameters": sum(v.numel() for v in tower_weights.values()), "dtype": "F32"}
        manifest["fit_hashes"] = fit_hashes
        manifest["reference_tower_rotary_modules"] = tower_rotary_modules
        manifest["limits"] = manifest["limits"] + [
            f"The span tower adds {len(tower['layers'])} ModernBERT layers per request for the token head only; "
            "document outputs never read it."]
    bridge.write_json(out / "manifest.json", manifest)
    print(json.dumps({"complete": True, "calibrated": calibrated, "references": len(references),
                      "local_references": len(local_refs), "schema": manifest["schema"]}), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    for name, func in (("raw", raw), ("export", export)):
        p = sub.add_parser(name)
        for key in ("data", "config", "fit", "artifact", "init", "out"):
            p.add_argument("--" + key, required=True)
        p.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
        p.add_argument("--allow-diagnostic-subset", action="store_true",
                       help="smoke only: accept a diagnostic Train-subset fit (never calibrated)")
        if name == "raw":
            p.add_argument("--part", choices=RAW_SPLITS, required=True)
        else:
            p.add_argument("--calibration")
            p.add_argument("--local-references", type=int, default=0,
                           help="stratified Dev-iter rows added as private local reference cases")
            p.add_argument("--synthetic-cases",
                           help="JSONL of {id, text} synthetic cases appended to the reference requests")
        p.set_defaults(func=func)
    p = sub.add_parser("offsets")
    p.add_argument("--artifact", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--data", default=None)
    p.add_argument("--part", choices=("dev_iter", "cal"), default=None)
    p.add_argument("--rows", default=None)
    p.add_argument("--population", choices=sorted(SCORE_POPULATIONS), default=None)
    p.set_defaults(func=offsets)
    p = sub.add_parser("score-rows")
    for key in ("data", "config", "fit", "artifact", "init", "out", "rows"):
        p.add_argument("--" + key, required=True)
    p.add_argument("--population", choices=sorted(SCORE_POPULATIONS), required=True)
    p.add_argument("--test-consumed-record", default=None)
    p.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
    p.set_defaults(func=score_rows)
    a = parser.parse_args()
    a.func(a)

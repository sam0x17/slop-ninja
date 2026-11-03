"""Full fine-tuning of V6 A on a shared-contract bundle, with training-only
style-invariance objectives and cohort-weighted generator attribution.

Inputs: a bundle directory (`rows.jsonl`, `weights.json`, `MANIFEST.json`,
assembled in Rust) and a config directory with the frozen `contract.json`
(parent artifact, rosters, Train-only V6 embedding normalization, bound to
the bundle's MANIFEST digest) and `protocol.json`. No corpus discovery,
normalization or calibration fitting, detector calls or Test input.

Arms combine: the E1 document heads or the E2 token-span head; random or
voice-mixed batching; an optional style adversary (gradient reversal on the
shared encoding, predicting voice cluster and style axes); an optional
supervised contrastive loss (same generator in other voices as positives,
other generators in the same voice and humans as negatives). The attribution
head is the conditional AI-role head over the canonical generator labels,
trained with family/origin-balanced weights times the declared cohort weights;
the per-cohort, per-label weight log is written before training and a fit
without it is incomplete. Adversary and projection heads are never exported.

Schedule: AdamW, linear warmup over a fraction of the total scheduled steps
(max_epochs x steps per epoch), then linear or cosine decay over a horizon of
H epochs (floor afterwards), optional layer-wise LR decay, bf16 autocast with
FP32 master weights (FP32 on CPU: bf16 autocast on CPU is pathologically
slow), gradient checkpointing, padded token-budget microbatches. Gradients
are clipped separately for the main model (encoder, exported heads, span
head) and for the training-only auxiliary heads, and both pre-clip norms are
logged. Share supervision: the document share MSE, an optional conditional
share BCE on mixed rows and an optional training-only share-bin head. Dev-iter (uncalibrated balanced involvement Brier, FP32,
unpadded) selects the checkpoint and stops early. Resumable at evaluation
boundaries under an exact binding.

spans-v1 (protocol spans-v1): an arm may add the token head (`token_head`),
`freeze` (`encoder_and_document_heads` trains the token head only, so every
document output stays bit-identical to the initial fit) and
`token_weighting`. `--init-fit DIR` starts from a completed fit's
`selected.pt` (encoder, document heads and any token head; training-only
auxiliary heads are not in selected.pt and start fresh). Span selection
metrics are the raw (untilted) Dev-iter mixed-document word Brier, optionally
guarded by the document involvement Brier. Token logits of the selected
checkpoint are dumped for every evaluated split.

Round-2 options (protocol A2 D2 and D3; both off by default, and with both off
the training computation is unchanged):
- `--train-allowlist FILE` trains on the Train rows of a row allow-list
  (`slop_ninja_a_row_allowlist_v1`) only. Kept rows keep their bundle weights
  (no refill, no rebalancing); the protocol's train weighting then normalises
  and caps over them. weights.json is still checked over every Train row, and
  roster labels left without Train rows stay unsupervised (fit.json lists them).
- `--final-step` takes the model at its final optimizer step, with no
  checkpoint selection and no early stopping. Dev-iter is scored once, after
  the last step; boundary checkpoints (for resume) and per-boundary training
  terms (`train-log.json`) are kept without scoring it.
A trainer bundle is read by block (full_a_bundle), so a fit never parses a
Dev-select or CAL block it does not open; the binding then carries block
digests in place of the whole rows file's.
"""
from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import time

import torch
from safetensors.torch import save_file

import full_a
import full_a_aux as aux
import full_a_bundle as bundle
import full_a_s2 as s2
import full_a_spans as spans_rule
import last_block_a as base
import shared_a as shared
import v6_embeddings as bridge
import warm_start_a
from warm_start_a import WarmStartA, initializer


CONTRACT_SCHEMA = "slop_ninja_full_a_contract_v2"
PROTOCOL_SCHEMA = "slop_ninja_full_a_protocol_v2"
BINDING_SCHEMA = "slop_ninja_full_a_fit_binding_v2"
FIT_SCHEMA = "slop_ninja_full_a_fit_v2"
PREDICTION_SCHEMA = "slop_ninja_a_prediction_v1"
TASKS = ("origin", "share", "human", "ai", "token", "token_head")
# Objective terms logged per step and per Dev-iter evaluation (full_a.objective parts).
LOSS_PARTS = ("origin", "share", "conditional_share", "attribution", "token", "token_head")
TOKEN_DUMP_SCHEMA = "slop_ninja_full_a_token_logits_v1"
INVOLVEMENT_METRIC = "dev_iter_uncalibrated_balanced_involvement_brier"
WORD_METRIC = "dev_iter_raw_mixed_word_brier"
GUARDED_WORD_METRIC = "dev_iter_raw_mixed_word_brier_document_guarded"
SELECTION_METRICS = (INVOLVEMENT_METRIC, WORD_METRIC, GUARDED_WORD_METRIC)
SPAN_ARM_FIELDS = {"token_head", "freeze", "token_weighting"}
S2_ARM_FIELDS = {"s2"}  # protocol spans-S2 (full_a_s2); absent = no S2 mechanism
SELECTION_SPLIT = "dev_iter"
REPORT_SPLITS = ("dev_select", "cal")
REPORT_SCHEMA = "slop_ninja_full_a_report_split_v2"
SCHEDULE = "warmup_decay_horizon"
# Optional protocol "ai_roster": {"rule": FIXED_ROSTER, "profiles_sha256": hex}. The
# contract's AI roster is then a fixed list (an allow-listed graph fixes its size and
# names) rather than the Train generator labels: every Train label must be in it,
# roster entries without Train rows stay unsupervised, and fit.json lists both.
FIXED_ROSTER = "fixed_v1"
# --final-step (protocol A2 D2's P0): no checkpoint selection.
FINAL_STEP_RULE = ("final_step_v1: the model after the last optimizer step of max_epochs; no checkpoint selection, "
                   "no early stopping; Dev-iter scored once, after the last step")


def roster_sha256(names):
    """SHA-256 of the compact JSON array of the roster names (the artifact spec's pin)."""
    return hashlib.sha256(json.dumps(names, separators=(",", ":"), ensure_ascii=False).encode()).hexdigest()


def read(p):
    return json.loads(Path(p).read_bytes())


def atomic_save(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("wb") as f:
        torch.save(value, f)
        f.flush()
        os.fsync(f.fileno())
    os.replace(temporary, path)


def number(v, lo=None, hi=None, positive=False):
    ok = type(v) in (int, float) and math.isfinite(v)
    ok = ok and (lo is None or v >= lo) and (hi is None or v <= hi) and (not positive or v > 0)
    return ok


def configure(device, fraction):
    """Deterministic single-thread host setup; CUDA capped per process."""
    torch.set_num_threads(1)
    if torch.get_num_interop_threads() != 1:
        torch.set_num_interop_threads(1)
    torch.use_deterministic_algorithms(True)
    if device.type == "cuda":
        shared.demand(torch.cuda.is_available(), "CUDA unavailable")
        shared.demand(os.environ.get("CUBLAS_WORKSPACE_CONFIG") in (":4096:8", ":16:8"), "CUBLAS workspace config")
        torch.cuda.set_per_process_memory_fraction(fraction)
        torch.backends.cuda.matmul.allow_tf32 = False
        torch.backends.cudnn.allow_tf32 = False
        torch.cuda.reset_peak_memory_stats()


# ------------------------------------------------------------------ protocol

def span_fields(arm):
    """spans-v1 arm fields with their defaults (absent = no token head)."""
    return {"token_head": arm.get("token_head"), "freeze": arm.get("freeze", "none"),
            "token_weighting": arm.get("token_weighting", "row_weight")}


def validate_arm(name, arm):
    base_fields = {"span_head", "share_mode", "share_supervision", "batching", "style_adversary", "contrastive"}
    shared.demand(set(arm) in (base_fields, base_fields | SPAN_ARM_FIELDS,
                               base_fields | SPAN_ARM_FIELDS | S2_ARM_FIELDS) and
                  arm["share_mode"] in full_a.SHARE_MODES, "arm fields: " + name)
    s2.validate(name, arm.get("s2"), arm)
    sp = span_fields(arm)
    th = sp["token_head"]
    if th is not None:
        shared.demand(isinstance(th, dict) and set(th) == {"hidden", "loss_weight", "learning_rate"} and
                      type(th["hidden"]) is int and 1 <= th["hidden"] <= 4096 and
                      number(th["loss_weight"], positive=True) and number(th["learning_rate"], positive=True),
                      "token head: " + name)
    shared.demand(sp["freeze"] in full_a.FREEZE_MODES and sp["token_weighting"] in bundle.TOKEN_WEIGHTINGS,
                  "freeze / token weighting: " + name)
    if sp["freeze"] == "encoder_and_document_heads":
        shared.demand(th is not None and arm["span_head"] is None and arm["style_adversary"] is None and
                      arm["contrastive"] is None and arm["share_supervision"]["bins"] is None,
                      "a frozen document model trains the token head only: " + name)
    sv = arm["share_supervision"]
    shared.demand(isinstance(sv, dict) and set(sv) == {"conditional_loss_weight", "bins"} and
                  number(sv["conditional_loss_weight"], lo=0), "share supervision: " + name)
    if sv["bins"] is not None:
        shared.demand(isinstance(sv["bins"], dict) and set(sv["bins"]) == {"loss_weight", "hidden"} and
                      number(sv["bins"]["loss_weight"], positive=True) and type(sv["bins"]["hidden"]) is int and
                      sv["bins"]["hidden"] >= 0, "share bins: " + name)
    span = arm["span_head"]
    if span is None:
        shared.demand(arm["share_mode"] == "head", "token share requires a span head: " + name)
    else:
        shared.demand(set(span) == {"pooling", "token_loss_weight"} and span["pooling"] in full_a.POOLINGS and
                      number(span["token_loss_weight"], lo=0), "span head: " + name)
    b = arm["batching"]
    shared.demand(isinstance(b, dict) and b.get("kind") in ("random", "voice_mixed"), "batching: " + name)
    if b["kind"] == "random":
        shared.demand(set(b) == {"kind"}, "random batching fields: " + name)
    else:
        shared.demand(set(b) == {"kind", "per_generator"} and type(b["per_generator"]) is int and
                      b["per_generator"] >= 1, "voice-mixed batching: " + name)
    a = arm["style_adversary"]
    if a is not None:
        shared.demand(set(a) == {"loss_weight", "hidden", "min_cluster_rows", "axes", "reversal"} and
                      number(a["loss_weight"], positive=True) and type(a["hidden"]) is int and a["hidden"] >= 0 and
                      type(a["min_cluster_rows"]) is int and a["min_cluster_rows"] >= 1 and
                      type(a["axes"]) is bool, "style adversary: " + name)
        aux.validate_reversal(a["reversal"])
    c = arm["contrastive"]
    if c is not None:
        shared.demand(set(c) == {"loss_weight", "temperature", "projection_hidden", "projection_dim"} and
                      number(c["loss_weight"], positive=True) and number(c["temperature"], positive=True) and
                      type(c["projection_hidden"]) is int and c["projection_hidden"] >= 1 and
                      type(c["projection_dim"]) is int and c["projection_dim"] >= 2, "contrastive: " + name)
        shared.demand(b["kind"] == "voice_mixed", "contrastive needs voice-mixed batching: " + name)


def validate_protocol(p):
    """Frozen recipe. Every field is required; unknown arms are refused."""
    shared.demand(p.get("schema") == PROTOCOL_SCHEMA and p.get("status") == "frozen" and
                  p.get("final_test_opened") is False, "frozen non-Test full-A protocol required")
    shared.demand(isinstance(p["seeds"], list) and p["seeds"] and len(set(p["seeds"])) == len(p["seeds"]) and
                  all(type(s) is int for s in p["seeds"]), "seeds")
    shared.demand(type(p["max_epochs"]) is int and 1 <= p["max_epochs"] <= 100, "max_epochs")
    shared.demand(type(p["logical_batch_size"]) is int and 1 <= p["logical_batch_size"] <= 4096, "logical batch")
    shared.demand(type(p["microbatch_token_budget"]) is int and
                  full_a.MAX_TOKENS <= p["microbatch_token_budget"] <= 1 << 20, "microbatch token budget")
    shared.demand(p["max_tokens"] == full_a.MAX_TOKENS, "2,048-token maximum")
    o = p["optimizer"]
    shared.demand(o["name"] == "AdamW" and number(o["encoder_peak_learning_rate"], positive=True) and
                  number(o["layer_lr_decay"], hi=1, positive=True) and
                  number(o["heads_learning_rate"], positive=True) and number(o["weight_decay"], lo=0) and
                  isinstance(o["betas"], list) and len(o["betas"]) == 2 and
                  all(number(b, lo=0, hi=.999999) for b in o["betas"]) and number(o["eps"], positive=True) and
                  number(o["gradient_clip_norm"], positive=True) and
                  number(o["aux_gradient_clip_norm"], positive=True), "optimizer")
    s = p["schedule"]
    shared.demand(set(s) == {"name", "warmup_fraction_of_total_steps", "shape", "horizon_epochs",
                             "floor_fraction"} and
                  s["name"] == SCHEDULE and s["shape"] in full_a.DECAY_SHAPES and
                  number(s["warmup_fraction_of_total_steps"], lo=0, hi=.5) and
                  number(s["horizon_epochs"], positive=True) and
                  s["horizon_epochs"] <= 100 and number(s["floor_fraction"], lo=0, hi=.5), "schedule")
    shared.demand(p["precision"] == {"train": "bf16_autocast", "evaluate": "fp32"} or
                  p["precision"] == {"train": "fp32", "evaluate": "fp32"}, "precision")
    shared.demand(type(p["gradient_checkpointing"]) is bool, "gradient checkpointing flag")
    # Only eager is qualified: ModernBERT ignores a dynamic SDPA switch (measured).
    shared.demand(p["attention_implementation"] == "eager", "attention implementation")
    e = p["early_stopping"]
    shared.demand(set(e) == {"metric", "split", "evaluations_per_epoch", "patience_evaluations", "min_delta"} and
                  e["split"] == SELECTION_SPLIT and e["metric"] in SELECTION_METRICS and
                  type(e["evaluations_per_epoch"]) is int and 1 <= e["evaluations_per_epoch"] <= 20 and
                  type(e["patience_evaluations"]) is int and e["patience_evaluations"] >= 1 and
                  number(e["min_delta"], lo=0), "early stopping")
    # The guarded word metric needs the document guard (involvement Brier
    # allowance over evaluation 0); the other metrics take none.
    guard = p.get("document_guard")
    shared.demand((e["metric"] == GUARDED_WORD_METRIC) == (guard is not None) and
                  (guard is None or number(guard, lo=0, hi=1)), "document guard")
    # spans-v1.1 A3: optional Dev-iter rows excluded from the span metrics
    # (document metrics keep them): {"file", "sha256", "split": "dev_iter"}.
    ex = p.get("span_metric_exclusions")
    shared.demand(ex is None or (isinstance(ex, dict) and set(ex) == {"file", "sha256", "split"} and
                                 ex["split"] == SELECTION_SPLIT and isinstance(ex["file"], str) and
                                 isinstance(ex["sha256"], str) and len(ex["sha256"]) == 64), "span metric exclusions")
    shared.demand(number(p["cuda_memory_fraction"], hi=.95, positive=True), "CUDA memory cap")
    r = p.get("ai_roster")
    shared.demand(r is None or (isinstance(r, dict) and set(r) == {"rule", "profiles_sha256"} and
                                r["rule"] == FIXED_ROSTER and isinstance(r["profiles_sha256"], str) and
                                len(r["profiles_sha256"]) == 64), "ai roster")
    shared.demand(set(p["attribution"]) == {"loss_weight"} and number(p["attribution"]["loss_weight"], lo=0),
                  "attribution")
    bundle.validate_weighting(p["train_weighting"])
    shared.demand(isinstance(p["arms"], dict) and p["arms"], "arms")
    for name, arm in p["arms"].items():
        validate_arm(name, arm)
        shared.demand(e["metric"] == INVOLVEMENT_METRIC or span_fields(arm)["token_head"] is not None,
                      "word-Brier selection needs a token head: " + name)
    return p


def validate_contract(c, manifest_sha256):
    shared.demand(c["schema"] == CONTRACT_SCHEMA and c["input_mode"] == "text" and c["raw_dimension"] == 768 and
                  c["zero_cfg_coordinates"] == full_a.CFG_ZEROS and
                  c["maximum_tokens_including_specials"] == full_a.MAX_TOKENS and c["token_truncation"] is False and
                  c["origin_order"] == ["human_only", "model_only", "mixed"] and
                  c["span_label_policy"] == full_a.SPAN_LABEL_POLICY and
                  c["span_coordinates"] == "utf8_byte_offsets_in_exact_final_text", "full-A contract meaning")
    shared.demand(c["bundle_manifest_sha256"] == manifest_sha256, "contract is bound to another bundle")
    n = c["embedding_normalization"]
    shared.demand(n["fit"] == "train_query_only_equal_source_family_weight" and n["dimension"] == 768 and
                  n["calibration_used_for_fit"] is False and n["development_used_for_fit"] is False and
                  n["reference_used_for_fit"] is False and n["final_test_opened"] is False,
                  "normalization must be Train-only")
    # The shared bundle carries no human identities: the human role always
    # returns other_human. Generator labels are the canonical AI roster.
    shared.demand(c["human_profiles"] == [], "human roster must be empty for shared-contract bundles")
    names = c["ai_profiles"]
    shared.demand(len(names) >= 2 and names == sorted(set(names)) and
                  all(isinstance(x, str) and x and not x.startswith("other_") for x in names),
                  "AI roster: >=2 sorted unique canonical generator labels")
    return c


def arm_of(protocol, name):
    return protocol["arms"][name]


# ------------------------------------------------------------------ model

class Aux:
    """Training-only heads and their vocabularies (Train-derived)."""

    def __init__(self, arm, train, device):
        self.adversary = self.projection = self.share_bins = None
        self.cluster_index, self.axes_index = {}, {}
        a, c = arm["style_adversary"], arm["contrastive"]
        bins = arm["share_supervision"]["bins"]
        if bins is not None:
            shared.demand(any(m.origin == 2 for m in train.meta), "share-bin head needs mixed Train rows")
            self.share_bins = aux.ShareBins(768, bins["hidden"]).to(device)
        if a is not None:
            counts = {}
            for m in train.meta:
                if m.label is not None and m.voice_cluster is not None:
                    counts[m.voice_cluster] = counts.get(m.voice_cluster, 0) + 1
            clusters = sorted(k for k, n in counts.items() if n >= a["min_cluster_rows"])
            axes = sorted({x for m in train.meta if m.label is not None for x in m.axes}) if a["axes"] else []
            self.cluster_index = {k: i for i, k in enumerate(clusters)}
            self.axes_index = {k: i for i, k in enumerate(axes)}
            shared.demand(len(clusters) >= 2 or axes, "style adversary has no Train targets")
            self.adversary = aux.StyleAdversary(len(clusters), len(axes), a["hidden"]).to(device)
        self.keys = None
        if c is not None:
            self.projection = aux.Projection(768, c["projection_hidden"], c["projection_dim"]).to(device)
            self.keys = contrastive_keys(train)  # (generator key, voice cluster) per Train row

    def modules(self):
        return {k: v for k, v in (("adversary", self.adversary), ("projection", self.projection),
                                  ("share_bins", self.share_bins)) if v is not None}

    @staticmethod
    def share_bin_target(m):
        return aux.share_bin(m.share) if m.origin == 2 else None

    def cluster_target(self, m):
        if m.label is None or m.voice_cluster is None:
            return None
        return self.cluster_index.get(m.voice_cluster, -1)

    def axes_target(self, m):
        if m.label is None or not self.axes_index or not m.axes:
            return None
        row = [0.] * len(self.axes_index)
        for x in m.axes:
            if x in self.axes_index:
                row[self.axes_index[x]] = 1.
        return row

    def denominators(self, train):
        d = {"cluster": 0., "axes": 0., "share_bins": 0.}
        for m, w in zip(train.meta, train.weights):
            if self.share_bins is not None and m.origin == 2:
                d["share_bins"] += w
            t = self.cluster_target(m)
            if t is not None and t >= 0:
                d["cluster"] += w
            if self.axes_target(m) is not None:
                d["axes"] += w
        return d

    def vocab(self):
        return {"voice_clusters": len(self.cluster_index), "style_axes": sorted(self.axes_index)}


def load(args, protocol, device):
    encoder, tokenizer, parent, _ = base.load_encoder(args.artifact, device)
    shared.demand(encoder.config._attn_implementation == protocol["attention_implementation"] == "eager",
                  "eager attention required")
    return encoder, tokenizer, parent


def build(encoder, contract, init, arm, protocol, device):
    profiles = {"human": contract["human_profiles"], "ai": contract["ai_profiles"]}
    norm = contract["embedding_normalization"]
    heads = WarmStartA(init, norm["mean"], norm["scale"], len(profiles["human"]), len(profiles["ai"]), "text")
    span = None if arm["span_head"] is None else full_a.SpanHead(arm["span_head"]["pooling"])
    th = span_fields(arm)["token_head"]
    token_head = None if th is None else full_a.TokenHead(width=th["hidden"])
    dtype = torch.bfloat16 if protocol["precision"]["train"] == "bf16_autocast" else None
    if s2_mechanism(arm) == "span_tower":
        # The tower is copied from the encoder after the init fit is loaded (attach_tower).
        model = s2.TowerFullA(encoder, heads, arm["share_mode"], dtype, token_head, arm["s2"]["layers"])
        return model.to(device), profiles
    return full_a.FullA(encoder, heads, span, arm["share_mode"], dtype, token_head).to(device), profiles


def s2_mechanism(arm):
    return None if arm.get("s2") is None else arm["s2"]["mechanism"]


def model_state(model, extra=None):
    value = {"encoder": {k: v.detach().cpu().clone() for k, v in model.encoder.state_dict().items()},
             "heads": {k: v.detach().cpu().clone() for k, v in model.heads.state_dict().items()}}
    if model.span is not None:
        value["span"] = {k: v.detach().cpu().clone() for k, v in model.span.state_dict().items()}
    if model.token_head is not None:
        value["token_head"] = {k: v.detach().cpu().clone() for k, v in model.token_head.state_dict().items()}
    if getattr(model, "span_tower", None) is not None:
        value["span_tower"] = {k: v.detach().cpu().clone() for k, v in model.span_tower.state_dict().items()}
    for name, module in (extra or {}).items():
        value[name] = {k: v.detach().cpu().clone() for k, v in module.state_dict().items()}
    return value


def restore(model, saved, extra=None):
    model.encoder.load_state_dict(saved["encoder"], strict=True)
    model.heads.load_state_dict(saved["heads"], strict=True)
    shared.demand((model.span is None) == ("span" not in saved), "span head presence differs")
    if model.span is not None:
        model.span.load_state_dict(saved["span"], strict=True)
    shared.demand((model.token_head is None) == ("token_head" not in saved), "token head presence differs")
    if model.token_head is not None:
        model.token_head.load_state_dict(saved["token_head"], strict=True)
    tower = getattr(model, "span_tower", None)
    shared.demand((tower is None) == ("span_tower" not in saved), "span tower presence differs")
    if tower is not None:
        tower.load_state_dict(saved["span_tower"], strict=True)
    for name, module in (extra or {}).items():
        module.load_state_dict(saved[name], strict=True)


INIT_FIT_SCHEMA = "slop_ninja_full_a_init_fit_v1"


def load_init_fit(model, path, seed, contract):
    """Restore encoder, document heads (and an E2 span head) from a completed
    fit's selected.pt; a token head is restored when both carry one, else it
    keeps its fresh initialization. Returns the binding record."""
    fit_dir = Path(path).resolve(strict=True)
    report = read(fit_dir / "fit.json")
    shared.demand(report["schema"] == FIT_SCHEMA and report["complete"] is True and
                  report["final_test_opened"] is False and report.get("diagnostic_train_subset") is None,
                  "--init-fit needs a complete, non-diagnostic fit")
    shared.demand(bridge.sha256(fit_dir / "selected.pt") == report["selected_checkpoint_sha256"],
                  "init fit selected checkpoint changed")
    shared.demand(report["binding"]["seed"] == seed, "init fit seed differs from this fit's seed")
    saved = torch.load(fit_dir / "selected.pt", map_location="cpu", weights_only=False)
    m = saved["model"]
    model.encoder.load_state_dict(m["encoder"], strict=True)
    model.heads.load_state_dict(m["heads"], strict=True)
    shared.demand((model.span is None) == ("span" not in m), "init fit span head presence differs")
    if model.span is not None:
        model.span.load_state_dict(m["span"], strict=True)
    token_head = model.token_head is not None and "token_head" in m
    if token_head:
        model.token_head.load_state_dict(m["token_head"], strict=True)
    norm = contract["embedding_normalization"]
    shared.demand(torch.equal(model.heads.text_mean.cpu(), torch.tensor(norm["mean"], dtype=torch.float32)) and
                  torch.equal(model.heads.text_scale.cpu(), torch.tensor(norm["scale"], dtype=torch.float32)),
                  "init fit normalization differs from this contract")
    return {"schema": INIT_FIT_SCHEMA, "fit_dir": str(fit_dir), "fit_sha256": bridge.sha256(fit_dir / "fit.json"),
            "selected_checkpoint_sha256": report["selected_checkpoint_sha256"],
            "init_arm": report["binding"]["arm"], "init_seed": report["binding"]["seed"],
            "init_selected_evaluation": report["selected_evaluation"],
            "dev_iter_raw_sha256": report["dev_iter_raw_sha256"], "token_head_restored": token_head,
            "aux_heads": "fresh (selected.pt carries no training-only heads)"}, saved, fit_dir


def init_raw_gate(raw, init_dir, subset=False):
    """Evaluation-0 Dev-iter document outputs versus the init fit's own dump (a diagnostic Dev-iter subset
    compares its own rows only)."""
    ref = {}
    with (init_dir / "dev_iter-raw.jsonl").open("rb") as f:
        for line in f:
            r = json.loads(line)
            ref[r["id"]] = r
    worst = 0.
    for r in raw:
        q = ref.get(r["id"])
        shared.demand(q is not None and q["text_sha256"] == r["text_sha256"], "Dev-iter row absent from init fit")
        for key in ("origin_logits", "human_logits", "ai_logits"):
            worst = max([worst] + [abs(a - b) for a, b in zip(r[key], q[key])])
        for key in ("involvement_logit", "conditional_mixed_share", "uncalibrated_share"):
            worst = max(worst, abs(r[key] - q[key]))
    shared.demand((subset or len(raw) == len(ref)) and worst <= 1e-6, f"evaluation 0 differs from the init fit: {worst}")
    return {"rows": len(raw), "document_raw_max_abs_error": worst}


def _sigmoid(x):
    return 1. / (1. + math.exp(-x)) if x >= 0 else math.exp(x) / (1. + math.exp(x))


def init_token_gate(token_rows, init_dir):
    """Evaluation-0 token logits versus the init fit's Dev-iter token dump (recorded, not gated here)."""
    path = init_dir / "dev_iter-token-logits.jsonl"
    if not token_rows or not path.exists():
        return None
    ref = {}
    with path.open("rb") as f:
        for line in f:
            r = json.loads(line)
            ref[r["id"]] = r["token_logits"]
    logit = prob = 0.
    for r in token_rows:
        q = ref.get(r["id"])
        shared.demand(q is not None and len(q) == len(r["token_logits"]), "Dev-iter token row absent from init fit")
        for a, b in zip(r["token_logits"], q):
            logit = max(logit, abs(a - b))
            prob = max(prob, abs(_sigmoid(a) - _sigmoid(b)))
    return {"rows": len(token_rows), "token_logit_max_abs_error": logit, "token_probability_max_abs_error": prob}


def frozen_equal(model, saved):
    """Bitwise: encoder and document heads equal the init fit's tensors."""
    m = saved["model"]
    for part, module in (("encoder", model.encoder), ("heads", model.heads)):
        state = module.state_dict()
        if set(state) != set(m[part]) or not all(torch.equal(state[k].cpu(), m[part][k]) for k in state):
            return False
    return True


# ------------------------------------------------------------------ batches

def microbatches(indices, lengths, budget):
    """Length-sorted padded groups with padded tokens <= budget (one doc minimum)."""
    order = sorted(indices, key=lambda i: (int(lengths[i]), i))
    groups, current = [], []
    for i in order:
        width = max([int(lengths[j]) for j in current] + [int(lengths[i])])
        if current and width * (len(current) + 1) > budget:
            groups.append(current)
            current = []
        current.append(i)
    if current:
        groups.append(current)
    return groups


def contrastive_keys(population):
    gens = [aux.HUMAN if m.label is None else m.label for m in population.meta]
    voices = [m.voice_cluster for m in population.meta]
    return gens, voices


def epoch_order(arm, population, batch_size, generator, seed, epoch):
    if arm["batching"]["kind"] == "random":
        return torch.randperm(len(population), generator=generator).tolist()
    gens, voices = contrastive_keys(population)
    # Seeded per (seed, epoch) so a resumed run regenerates the same order.
    return aux.voice_mixed_order(gens, voices, batch_size, arm["batching"]["per_generator"],
                                 seed * 1000003 + epoch)


# ------------------------------------------------------------------ evaluation

def contract_prediction(meta, out, ai_profiles):
    """Shared-contract prediction row (uncalibrated). Attribution is the
    conditional distribution over the AI roster given AI involvement."""
    o = out["origin"][0].double().tolist()
    a = out["ai_logits"][0].double().softmax(-1).tolist()
    # FP32 share can exceed the FP64 sum of its bounds by rounding; restore the invariant.
    share = min(max(float(out["share"][0]), o[1]), o[1] + o[2])
    return {"id": meta.id, "p_human": o[0], "p_model_only": o[1], "p_mixed": o[2],
            "share": share, "attribution": dict(zip(ai_profiles, a)),
            "attribution_basis": "conditional_on_ai_involvement", "calibrated": False}


def raw_prediction(meta, out, tokens):
    """Uncalibrated raw outputs; Rust CAL fits from involvement_logit only."""
    p = out["origin"][0].double().tolist()
    return {"id": meta.id, "text_sha256": meta.text_sha256, "tokens": int(tokens),
            "origin_logits": out["origin_logits"][0].double().tolist(),
            "involvement_logit": float(out["involvement_logit"][0]),
            "conditional_mixed_share": float(out["conditional_mixed_share"][0]),
            "token_mean": None if out["token_mean"] is None else float(out["token_mean"][0]),
            "pooled_token_logit": None if out["pooled"] is None else float(out["pooled"][0]),
            "uncalibrated_origin": dict(zip(("human_only", "model_only", "mixed"), p)),
            "uncalibrated_share": float(out["share"][0]),
            "human_logits": out["human_logits"][0].double().tolist(),
            "ai_logits": out["ai_logits"][0].double().tolist(), "calibrated": False}


class SpanStats:
    """Token-head metrics from raw (untilted) probabilities; document masses are
    the population's balanced weights, words equal within a document."""

    def __init__(self):
        self.mass = {k: 0. for k in ("mixed_word", "all_word", "mixed_token", "all_token")}
        self.value = dict(self.mass)
        self.human_docs = self.human_docs_any = 0
        self.human_words = self.human_words_flagged = 0
        self.consistency = []
        self.excluded = 0

    def add(self, w, origin, p_tok, labels, index, share):
        keep = labels >= 0
        if bool(keep.any()):
            b = float(((p_tok[keep] - labels[keep].double()) ** 2).mean())
            for key in (("mixed_token", "all_token") if origin == 2 else ("all_token",)):
                self.value[key] += w * b
                self.mass[key] += w
        if index is None:
            return None
        tok, wid, lab = index
        n = int(lab.numel())
        sums = torch.zeros(n, dtype=torch.float64, device=p_tok.device).index_add_(0, wid, p_tok[tok])
        counts = torch.zeros(n, dtype=torch.float64, device=p_tok.device).index_add_(
            0, wid, torch.ones_like(wid, dtype=torch.float64))
        pw = sums / counts
        self.consistency.append(abs(float(pw.mean()) - share))
        lk = lab >= 0
        if bool(lk.any()):
            b = float(((pw[lk] - lab[lk].double()) ** 2).mean())
            for key in (("mixed_word", "all_word") if origin == 2 else ("all_word",)):
                self.value[key] += w * b
                self.mass[key] += w
        if origin == 0:
            flagged = int((pw >= .5).sum())
            self.human_docs += 1
            self.human_docs_any += flagged > 0
            self.human_words += n
            self.human_words_flagged += flagged
        return pw

    def result(self):
        c = sorted(self.consistency)
        out = {k + "_brier_raw": (self.value[k] / self.mass[k] if self.mass[k] > 0 else None) for k in self.mass}
        out.update({"human_documents": self.human_docs, "human_documents_any_word_ge_0_5": self.human_docs_any,
                    "human_words": self.human_words, "human_words_ge_0_5": self.human_words_flagged,
                    "raw_mean_word_probability_minus_share_mae": sum(c) / len(c) if c else None,
                    "raw_mean_word_probability_minus_share_p90": c[min(len(c) - 1, int(.9 * len(c)))] if c else None,
                    "excluded_rows": self.excluded,
                    "basis": "raw sigmoid token probabilities; word = mean over overlapping word tokens; "
                             "documents weighted by the balanced population weights; untilted"})
        return out


@torch.no_grad()
def evaluate(model, population, device, pad_id, role_weight, export=False):
    """FP32, one unpadded document per forward, as in Rust replay.

    Returns the selection score (balanced involvement Brier), the joint loss
    and text-free diagnostics: human flags at 0.5 and conditional attribution
    accuracy per generator label, overall and on held-out-style rows. With a
    token head, diagnostics["span"] holds the raw word and token Briers and
    export also returns per-row token logits.
    """
    model.eval()
    score, loss, raw, preds = 0., 0., [], []
    span_stats = SpanStats() if model.token_head is not None else None
    span_exclude = getattr(population, "span_exclude", frozenset())
    token_rows = []
    token_weight = getattr(model, "token_loss_weight", 0.)
    attr = {}
    flags = humans = positives = hits = 0
    parts = {k: 0. for k in LOSS_PARTS}
    domains = {}
    profiles = population.ai_profiles
    for i, (m, w) in enumerate(zip(population.meta, population.weights)):
        ids, mask, content, labels, words = population.batch([i], device, pad_id)
        out = model(ids, mask, content, autocast=False, words=words)
        t = population.targets(i)
        p_ai = float(out["origin"][0, 1:].sum())
        score += w / population.denominators["origin"] * (p_ai - float(t["origin"] != 0)) ** 2
        loss += float(full_a.objective(out, [t], [w], population.denominators, labels, token_weight, role_weight,
                                       getattr(model, "conditional_share_weight", 0.), model.share_mode, parts,
                                       getattr(model, "token_head_weight", 0.), [population.token_weights[i]],
                                       not model.frozen_document))
        if span_stats is not None:
            p_tok = out["span_logits"][0].double().sigmoid()
            if m.id in span_exclude:
                pw = None
                span_stats.excluded += 1
            else:
                pw = span_stats.add(w, t["origin"], p_tok, labels[0], population.word_index(i, device),
                                    float(out["share"][0]))
            if export:
                token_rows.append({"schema": TOKEN_DUMP_SCHEMA, "id": m.id, "text_sha256": m.text_sha256,
                                   "tokens": int(population.lengths[i]),
                                   "token_logits": out["span_logits"][0].double().tolist(),
                                   "raw_mean_word_probability": None if pw is None else float(pw.mean()),
                                   "words": None if pw is None else int(pw.numel())})
        d = domains.setdefault(m.domain, [0, 0, 0, 0, 0.])  # humans, flags, positives, hits, Brier mass
        d[4] += w / population.denominators["origin"] * (p_ai - float(t["origin"] != 0)) ** 2
        if t["origin"] == 0:
            humans += 1
            flags += p_ai >= .5
            d[0] += 1
            d[1] += p_ai >= .5
        else:
            positives += 1
            hits += p_ai >= .5
            d[2] += 1
            d[3] += p_ai >= .5
        if t["ai"] is not None:
            hit = int(out["ai_logits"][0].argmax()) == t["ai"]
            for key in (m.label, "__all__") + (("__held_out_style__",) if m.held_out_style else ()):
                n, k = attr.get(key, (0, 0))
                attr[key] = (n + 1, k + hit)
        if export:
            raw.append(raw_prediction(m, out, population.lengths[i]))
            preds.append(contract_prediction(m, out, profiles))
    diagnostics = {"human_rows": humans, "human_flags_at_0_5": flags,
                   "positive_rows": positives, "positive_flags_at_0_5": hits,
                   "tpr_at_0_5": hits / positives if positives else None,
                   "loss_parts": parts,
                   "by_domain": {k: {"human_rows": v[0], "human_flags_at_0_5": v[1], "positive_rows": v[2],
                                     "positive_flags_at_0_5": v[3], "brier_contribution": v[4]}
                                 for k, v in sorted(domains.items())},
                   "rows_outside_ai_roster": dict(sorted(population.unrostered.items())),
                   "attribution_accuracy": {k: {"rows": n, "accuracy": c / n} for k, (n, c) in sorted(attr.items())},
                   "span": None if span_stats is None else span_stats.result()}
    if export:
        return score, loss, diagnostics, raw, preds, token_rows
    return score, loss, diagnostics, raw, preds


def balanced_brier(raw, population):
    """Uncalibrated balanced involvement Brier from exported raw predictions."""
    total = 0.
    for p, m, w in zip(raw, population.meta, population.weights):
        o = p["uncalibrated_origin"]
        total += w / population.denominators["origin"] * (o["model_only"] + o["mixed"] - float(m.origin != 0)) ** 2
    return total


def gate_indices(population, extra=8):
    """The first 4 rows, the longest and shortest documents, and `extra` rows at
    evenly spaced length quantiles (distinct, deterministic)."""
    lengths = [int(x) for x in population.lengths]
    order = sorted(range(len(lengths)), key=lambda i: (lengths[i], i))
    chosen = list(range(min(4, len(order))))
    picks = [order[-1], order[0]] + [order[round(q * (len(order) - 1) / (extra + 1))] for q in range(1, extra + 1)]
    for i in picks:
        if i not in chosen:
            chosen.append(i)
    return chosen


def initial_gate(model, population, device, pad_id, budget, init_fit=False):
    """Step-0 function preservation against V6's own classifier (FP32,
    unpadded; skipped for --init-fit starts, which the evaluation-0 raw gate
    checks instead), then padded agreement for the same documents grouped as
    training microbatches (origin and token-head logits)."""
    model.eval()
    worst, padded = 0., 0.
    indices = gate_indices(population)
    token_singles = {}
    with torch.no_grad():
        singles = {}
        for i in indices:
            ids, mask, content, _, words = population.batch([i], device, pad_id)
            out = model(ids, mask, content, autocast=False, words=words)
            expected = (model.encoder.classifier(out["h"]) / model.heads.base_temperature).softmax(-1)
            worst = max(worst, float((out["origin"] - expected).abs().max()))
            singles[i] = out["origin"][0]
            if out["span_logits"] is not None:
                token_singles[i] = out["span_logits"][0]
        groups = microbatches(indices, population.lengths, budget)
        # Token head: compared on the probability scale (sigmoid of the logits);
        # the logit difference is recorded as a diagnostic.
        token_padded = token_padded_logit = 0.
        for group in groups:
            ids, mask, content, _, words = population.batch(group, device, pad_id)
            out = model(ids, mask, content, autocast=False, words=words)
            padded = max([padded] + [float((out["origin"][j] - singles[i]).abs().max()) for j, i in enumerate(group)])
            if out["span_logits"] is not None:
                for j, i in enumerate(group):
                    n = int(population.lengths[i])
                    a, b = out["span_logits"][j, :n], token_singles[i]
                    token_padded = max(token_padded, float((a.sigmoid() - b.sigmoid()).abs().max()))
                    token_padded_logit = max(token_padded_logit, float((a - b).abs().max()))
    if not init_fit:
        shared.demand(worst <= 1e-6, f"initial A differs from V6: {worst}")
    shared.demand(padded <= 1e-4, f"padded microbatch disagrees with unpadded: {padded}")
    # Token probabilities are per-token states, more sensitive than the pooled origin: a trained token head
    # measured 1.6e-4 (FP32 padded vs unpadded); a padding-mask defect gives errors of order 0.1.
    shared.demand(token_padded <= 1e-3, f"padded token probabilities disagree with unpadded: {token_padded}")
    return {"rows": len(indices), "row_indices": indices, "row_tokens": [int(population.lengths[i]) for i in indices],
            "padded_groups": [len(g) for g in groups],
            "initial_origin_max_abs_error": None if init_fit else worst,
            "initial_reference": "init_fit (evaluation-0 Dev-iter raw gate)" if init_fit else "V6 classifier",
            "padded_vs_unpadded_origin_max_abs_error": padded,
            "padded_vs_unpadded_token_probability_max_abs_error": token_padded if model.token_head is not None else None,
            "padded_vs_unpadded_token_logit_max_abs_error": token_padded_logit if model.token_head is not None else None}


def disjoint(a, b):
    shared.demand(not {m.source_group for m in a.meta} & {m.source_group for m in b.meta},
                  f"family leakage {a.split}/{b.split}")
    shared.demand(not {m.id for m in a.meta} & {m.id for m in b.meta}, f"duplicate {a.split}/{b.split} id")


def write_jsonl(path, rows):
    with path.open("wb") as f:
        for r in rows:
            f.write(bridge.canonical(r) + b"\n")


# ------------------------------------------------------------------ training step

def rng_state(device):
    return torch.get_rng_state(), (torch.cuda.get_rng_state_all() if torch.device(device).type == "cuda" else None)


def set_rng_state(state, device):
    torch.set_rng_state(state[0])
    if torch.device(device).type == "cuda":
        torch.cuda.set_rng_state_all(state[1])


def train_step(model, heads, arm, train, indices, device, pad_id, budget, role_weight, scale_reversal, aux_den,
               s2_state=None):
    """One logical batch: optional exact contrastive pre-pass (gradient cache),
    then per-microbatch forward/backward of the joint, adversary and injected
    contrastive gradients. Returns per-term values for logging.

    spans-S2 (s2_state): `teacher` replaces every document label term with the
    anchor objective against the frozen teacher; `pair_contrast` adds one parent
    entry per spliced child to the logical batch (document terms only) and
    scales human document weights. Without s2_state the computation is the
    spans-v1 one."""
    extra = heads.modules()
    pc = None if s2_state is None else s2_state.get("pair_contrast")
    teacher = None if s2_state is None else s2_state.get("teacher")
    if pc is None:
        entries = None
        groups = microbatches(indices, train.lengths, budget)
    else:
        entries = pc.entries(indices, train)
        groups = s2.entry_groups(entries, train.lengths, budget)

    def unpack(group):
        """(rows, document weights, token weights, contrast flags) of a microbatch."""
        if entries is None:
            return group, [train.weights[i] for i in group], [train.token_weights[i] for i in group], None
        return ([entries[k][0] for k in group], [entries[k][1] for k in group], [entries[k][2] for k in group],
                [entries[k][3] for k in group])
    scale = len(train) / len(indices)
    terms = {"joint": 0., "adversary": 0., "contrastive": 0., "share_bins": 0., "anchors": 0}
    parts = {k: 0. for k in LOSS_PARTS}
    injected = None
    c = arm["contrastive"]
    if c is not None:
        # Gradient cache: z for the whole logical batch without graph, the
        # contrastive loss and dL/dz on those, then the same forwards again
        # (same RNG draws) with dL/dz injected. Exact for the logical batch.
        state = rng_state(device)
        z = {}
        with torch.no_grad():
            for group in groups:
                rows, _, _, contrast = unpack(group)
                ids, mask, content, _, words = train.batch(rows, device, pad_id)
                out = model(ids, mask, content, words=words)
                for j, i in enumerate(rows):
                    if contrast is None or not contrast[j]:
                        z[i] = out["encoding"][j].float()
        set_rng_state(state, device)
        gens, voices = getattr(heads, "keys", None) or contrastive_keys(train)
        pos, neg = aux.contrastive_sets([gens[i] for i in indices], [voices[i] for i in indices])
        cached = torch.stack([z[i] for i in indices]).requires_grad_(True)
        loss, anchors = aux.supervised_contrastive(extra["projection"](cached), pos, neg,
                                                   [train.weights[i] for i in indices], c["temperature"])
        if anchors:
            (c["loss_weight"] * loss).backward()
            injected = {i: cached.grad[k].detach() for k, i in enumerate(indices)}
        terms["contrastive"], terms["anchors"] = float(loss.detach()), anchors
    a = arm["style_adversary"]
    for group in groups:
        rows, doc_weights, token_weights, contrast = unpack(group)
        ids, mask, content, labels, words = train.batch(rows, device, pad_id)
        out = model(ids, mask, content, words=words)
        group_parts = {}
        loss = full_a.objective(out, [train.targets(i) for i in rows], doc_weights,
                                train.denominators, labels, model.token_loss_weight, role_weight,
                                getattr(model, "conditional_share_weight", 0.), model.share_mode, group_parts,
                                model.token_head_weight, token_weights,
                                not model.frozen_document and teacher is None) * scale
        if teacher is not None:
            t = s2.teacher_outputs(teacher, ids, mask, content, words)
            loss = loss + s2.anchor_objective(out, t, doc_weights, train.denominators["origin"],
                                              getattr(model, "conditional_share_weight", 0.), role_weight,
                                              group_parts) * scale
        for k, v in group_parts.items():
            parts[k] = parts.get(k, 0.) + v * scale
        terms["joint"] += float(loss.detach())
        if "share_bins" in extra:
            sb = aux.share_bin_loss(extra["share_bins"], out["encoding"],
                                    [heads.share_bin_target(train.meta[i]) for i in rows],
                                    [train.weights[i] for i in rows], aux_den["share_bins"]) * \
                (arm["share_supervision"]["bins"]["loss_weight"] * scale)
            terms["share_bins"] += float(sb.detach())
            loss = loss + sb
        if a is not None:
            metas = [train.meta[i] for i in rows]
            adv = aux.adversary_loss(extra["adversary"], out["encoding"], scale_reversal,
                                     [heads.cluster_target(m) for m in metas],
                                     [heads.axes_target(m) for m in metas],
                                     [train.weights[i] for i in rows], aux_den) * (a["loss_weight"] * scale)
            terms["adversary"] += float(adv.detach())
            loss = loss + adv
        if injected is not None:
            zero = torch.zeros_like(next(iter(injected.values())))
            grads = [zero if contrast is not None and contrast[j] else injected[i] for j, i in enumerate(rows)]
            loss = loss + (out["encoding"] * torch.stack(grads)).sum()
        shared.demand(bool(torch.isfinite(loss)), "nonfinite loss")
        loss.backward()
    terms.update({"loss_" + k: v for k, v in parts.items()})
    return terms


# ------------------------------------------------------------------ fit

def trained_heads(best, train, frozen, init_dir):
    """Heads with supervised training in the selected weights; a document head
    trained by the init fit stays trained (frozen arms train only the token head)."""
    done = {k: best["global_step"] > 0 and train.denominators[k] > 0 and (not frozen or k == "token_head")
            for k in TASKS}
    if init_dir is not None:
        earlier = read(init_dir / "fit.json")["trained_selected_heads"]
        done = {k: v or (k != "token_head" and bool(earlier.get(k))) for k, v in done.items()}
    return done


def run(args):
    output = Path(args.out)
    output.mkdir(exist_ok=True)
    with (output / ".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        fit(args, Path(args.data).resolve(strict=True), Path(args.config).resolve(strict=True), output)


def fit(args, root, config, output):
    report_split = getattr(args, "report_split", None)
    # Checked before any bundle read: a refused report split is never hashed or parsed.
    shared.demand(report_split is None or report_split in REPORT_SPLITS,
                  "report split must be dev_select or cal (never Test, never the selection split)")
    # Dev-select is single-use and CAL is for calibration: a routine arm run
    # never reads them. Consuming one needs the explicit flag naming it and a
    # new one-time use record, created before the split is opened.
    shared.demand(report_split is None or (getattr(args, "consume_sealed_split", None) == report_split and
                                           getattr(args, "use_record", None) and
                                           not Path(args.use_record).exists()),
                  "--report-split needs --consume-sealed-split naming the same split and a new --use-record")
    shared.demand(report_split is not None or not getattr(args, "consume_sealed_split", None),
                  "--consume-sealed-split without --report-split")
    # A trainer bundle's unopened Dev-select or CAL block is neither parsed nor hashed (full_a_bundle).
    digests = bundle.bound_digests(root, ["train", SELECTION_SPLIT] + ([report_split] if report_split else []))
    protocol_path, contract_path = config / "protocol.json", config / "contract.json"
    contract = validate_contract(read(contract_path), digests[bundle.MANIFEST])
    protocol = validate_protocol(read(protocol_path))
    shared.demand(args.seed in protocol["seeds"] and args.arm in protocol["arms"], "seed/arm outside protocol")
    arm = arm_of(protocol, args.arm)
    role_weight = protocol["attribution"]["loss_weight"]
    device = torch.device(args.device)
    configure(device, protocol["cuda_memory_fraction"])
    torch.manual_seed(args.seed)
    encoder, tokenizer, parent = load(args, protocol, device)
    shared.demand(contract["parent_artifact_id"] == parent["artifact_id"], "encoder parent changed")
    init = initializer(args.init)
    model, profiles = build(encoder, contract, init, arm, protocol, device)
    model.token_loss_weight = 0. if arm["span_head"] is None else arm["span_head"]["token_loss_weight"]
    model.conditional_share_weight = arm["share_supervision"]["conditional_loss_weight"]
    sp = span_fields(arm)
    th = sp["token_head"]
    model.token_head_weight = 0. if th is None else th["loss_weight"]
    init_record = init_saved = init_dir = None
    if getattr(args, "init_fit", None):
        init_record, init_saved, init_dir = load_init_fit(model, args.init_fit, args.seed, contract)
    frozen = sp["freeze"] == "encoder_and_document_heads"
    shared.demand(not frozen or init_record is not None, "a frozen document model needs --init-fit")
    mechanism = s2_mechanism(arm)
    shared.demand(mechanism is None or init_record is not None, "S2 arms start from --init-fit")
    teacher = None
    if mechanism == "span_tower":
        model.attach_tower()
    elif mechanism == "anchor":
        teacher = s2.make_teacher(model)
    if frozen:
        model.freeze_document()
    if protocol["gradient_checkpointing"] and not frozen:
        encoder.gradient_checkpointing_enable(gradient_checkpointing_kwargs={"use_reentrant": False})
    final_step = bool(getattr(args, "final_step", False))
    # Evaluation 0 of an --init-fit start scores Dev-iter for its gate; a final-step fit scores it once, at the end.
    shared.demand(not final_step or init_record is None, "--final-step takes no --init-fit")
    special = set(tokenizer.all_special_ids)
    tokenize = full_a.hf_tokenize(tokenizer)
    fixed = protocol.get("ai_roster")
    train_labels = bundle.ai_roster(root)
    if fixed is None:
        shared.demand(train_labels == profiles["ai"], "AI roster differs from the Train generator labels")
    else:
        shared.demand(roster_sha256(profiles["ai"]) == fixed["profiles_sha256"],
                      "contract AI roster differs from the protocol's fixed roster")
        shared.demand(set(train_labels) <= set(profiles["ai"]), "a Train generator label is outside the fixed roster")
    loaded = time.monotonic()
    subset_rows = getattr(args, "train_subset_rows", None)
    subset_rule = getattr(args, "train_subset_rule", None) or "stratified"
    dev_rows = getattr(args, "diagnostic_dev_subset_rows", None)
    shared.demand(subset_rule in ("stratified", "coarse") and (subset_rule == "stratified" or subset_rows is not None),
                  "train subset rule")
    shared.demand(dev_rows is None or subset_rows is not None, "a Dev-iter subset is for diagnostic subset fits only")
    allowlist = getattr(args, "train_allowlist", None)
    shared.demand(allowlist is None or subset_rows is None, "--train-allowlist and --train-subset-rows are exclusive")
    full_meta = full_dropped = subset = dev_keep = keep = allow_record = None
    if allowlist is not None:
        allow_record, allow_ids = bundle.read_allowlist(allowlist, root)
        full_meta, full_dropped = bundle.train_metadata(root)
        missing = len(set(allow_ids) - {m.id for m in full_meta})
        shared.demand(missing == 0, f"{missing} allow-listed ids are not positive-weight Train rows of this bundle")
        keep = {x: 1. for x in allow_ids}
    if subset_rows is not None:
        # Diagnostic ablations only: a stratified Train subset (seeded by the fit
        # seed); the full Train metadata still checks weights.json.
        full_meta, full_dropped = bundle.train_metadata(root)
        if subset_rule == "stratified":
            subset = bundle.stratified_subset(full_meta, subset_rows, args.seed)
        else:
            subset = s2.coarse_subset(full_meta, subset_rows, args.seed)
        if mechanism == "pair_contrast":
            subset = s2.close_pairs(subset, full_meta)
        keep = subset
    if dev_rows is not None:
        dev_keep = s2.dev_subset(root, dev_rows)
    train = bundle.Population(root, "train", tokenize, special, profiles["ai"], cohort_weighted=True,
                              weighting=protocol["train_weighting"], keep=keep, token_weighting=sp["token_weighting"])
    dev = bundle.Population(root, SELECTION_SPLIT, tokenize, special, profiles["ai"], cohort_weighted=False,
                            span_words=th is not None, keep=dev_keep)
    load_seconds = time.monotonic() - loaded
    if allow_record is not None:
        shared.demand(len(train) == allow_record["count"], "allow-listed Train rows differ from the allow-list")
    pair_contrast = s2.PairContrast(train, require_all=keep is None) if mechanism == "pair_contrast" else None
    teacher_check = None
    if teacher is not None:
        teacher_check = s2.teacher_gate(teacher, dev, device, tokenizer.pad_token_id, init_dir)
    s2_state = None if mechanism is None else {"pair_contrast": pair_contrast, "teacher": teacher}
    disjoint(train, dev)
    exclusions = protocol.get("span_metric_exclusions")
    if exclusions is not None:
        shared.demand(bridge.sha256(exclusions["file"]) == exclusions["sha256"], "span metric exclusion file changed")
        flagged = {r["id"] for r in read(exclusions["file"])["flagged"] if r["split"] == SELECTION_SPLIT}
        shared.demand(dev_keep is not None or flagged <= {m.id for m in dev.meta}, "excluded span rows outside Dev-iter")
        dev.span_exclude = frozenset(flagged)
    supervised_ai = {train.targets(i)["ai"] for i in range(len(train))} - {None}
    if fixed is None and allow_record is None:
        shared.demand(supervised_ai == set(range(len(profiles["ai"]))), "AI roster not established by Train supervision")
    elif fixed is None:
        # The roster stays the bundle's Train labels; labels without allow-listed rows stay unsupervised.
        shared.demand(len(supervised_ai) >= 2, "allow-listed Train rows supervise fewer than two AI profiles")
    else:
        shared.demand(len(supervised_ai) >= 2, "a fixed roster needs Train supervision for at least two profiles")
    roster_record = {"rule": "train_labels" if fixed is None else FIXED_ROSTER,
                     "profiles_sha256": roster_sha256(profiles["ai"]),
                     "supervised": [profiles["ai"][i] for i in sorted(supervised_ai)],
                     "unsupervised": [x for i, x in enumerate(profiles["ai"]) if i not in supervised_ai]}
    shared.demand(arm["span_head"] is None or train.denominators["token"] > 0, "span arm needs token supervision")
    shared.demand(th is None or train.denominators["token_head"] > 0, "token-head arm needs token supervision")
    weights_doc = read(root / bundle.WEIGHTS)
    weight_log = bundle.finish_weight_log(
        bundle.validate_weights(weights_doc, train.meta if keep is None else full_meta,
                                train.dropped if keep is None else full_dropped,
                                scheme=protocol["train_weighting"]["scheme"]),
        train, weights_doc.get("overrides"))
    aux_heads = Aux(arm, train, device)
    extra = aux_heads.modules()
    aux_den = aux_heads.denominators(train)
    if arm["style_adversary"] is not None:
        shared.demand(aux_den["cluster"] > 0 or aux_den["axes"] > 0, "style adversary has no supervised mass")
    pad_id = tokenizer.pad_token_id
    shared.demand(protocol["precision"]["train"] == "fp32" or device.type == "cuda",
                  "CPU runs must use FP32 (bf16 autocast on CPU is pathologically slow)")
    dev_digest = dev.rows_digest()
    o, schedule = protocol["optimizer"], protocol["schedule"]
    groups = full_a.parameter_groups(model, o["encoder_peak_learning_rate"], o["layer_lr_decay"],
                                     o["heads_learning_rate"], o["weight_decay"],
                                     None if th is None else th["learning_rate"])
    if mechanism == "span_tower":
        groups += s2.tower_parameter_group(model, arm["s2"]["learning_rate"], o["weight_decay"])
    for name, module in extra.items():
        groups.append({"params": list(module.parameters()), "lr": o["heads_learning_rate"],
                       "weight_decay": o["weight_decay"], "name": "aux." + name})
    optimizer = torch.optim.AdamW(groups, betas=tuple(o["betas"]), eps=o["eps"])
    lb = protocol["logical_batch_size"]
    steps_per_epoch = math.ceil(len(train) / lb)
    horizon = max(1, round(schedule["horizon_epochs"] * steps_per_epoch))
    total_steps = protocol["max_epochs"] * steps_per_epoch
    # Warmup is a fraction of the total scheduled steps, independent of H.
    warmup = int(schedule["warmup_fraction_of_total_steps"] * total_steps)
    shared.demand(warmup < horizon, "warmup must end before the decay horizon")
    scheduler = torch.optim.lr_scheduler.LambdaLR(
        optimizer, full_a.warmup_horizon(horizon, warmup, schedule["shape"], schedule["floor_fraction"]))
    stop = protocol["early_stopping"]
    k = stop["evaluations_per_epoch"]
    eval_points = sorted({math.ceil(steps_per_epoch * (j + 1) / k) for j in range(k)})
    binding = {"schema": BINDING_SCHEMA, "arm": args.arm, "arm_definition": arm, "seed": args.seed,
               "bundle_sha256": digests, "contract_sha256": bridge.sha256(contract_path),
               "protocol_sha256": bridge.sha256(protocol_path),
               "initializer_sha256": bridge.sha256(args.init), "parent_artifact_id": parent["artifact_id"],
               "trainer_sha256": bridge.sha256(__file__), "model_sha256": bridge.sha256(full_a.__file__),
               "aux_sha256": bridge.sha256(aux.__file__), "bundle_reader_sha256": bridge.sha256(bundle.__file__),
               "architecture_sha256": bridge.sha256(base.__file__), "shared_loss_sha256": bridge.sha256(shared.__file__),
               "shared_heads_implementation_sha256": bridge.sha256(warm_start_a.__file__),
               "artifact_loader_sha256": bridge.sha256(bridge.__file__),
               "spans_rule_sha256": bridge.sha256(spans_rule.__file__), "init_fit": init_record,
               "transformers_model_source_sha256": bridge.sha256(__import__(encoder.__module__, fromlist=["x"]).__file__),
               "torch": torch.__version__, "cuda": torch.version.cuda, "device": str(device),
               "dev_iter_rows_sha256": dev_digest}
    if subset is not None:
        binding["diagnostic_train_subset"] = {
            "rows": len(subset), "seed": args.seed, "rule": "strata (cohort, label, origin, domain); largest "
            "remainder, >= 1 per stratum; ascending sha256(seed NUL id); weight x N_stratum/n_stratum",
            "ids_sha256": hashlib.sha256("\n".join(sorted(subset)).encode()).hexdigest()}
        if subset_rule == "coarse":
            binding["diagnostic_train_subset"]["rule"] = s2.COARSE_SUBSET_RULE + \
                ("; parents of kept spliced rows added (factor 1)" if mechanism == "pair_contrast" else "")
    if dev_keep is not None:
        binding["diagnostic_dev_subset"] = {"rows": len(dev_keep), "key": s2.DEV_SUBSET_KEY,
                                            "ids_sha256": hashlib.sha256("\n".join(sorted(dev_keep)).encode()).hexdigest()}
    if allow_record is not None:
        binding["train_allowlist"] = allow_record
    if final_step:
        binding["checkpoint_rule"] = FINAL_STEP_RULE
    if mechanism is not None or subset_rule == "coarse" or dev_keep is not None:
        binding["s2"] = {"mechanism": mechanism, "module_sha256": bridge.sha256(s2.__file__),
                         "rule": {"anchor": s2.ANCHOR_RULE, "pair_contrast": s2.CONTRAST_RULE,
                                  "span_tower": s2.TOWER_RULE}.get(mechanism)}
    recovery, best_path = output / "recovery.pt", output / "best.pt"
    order_rng = torch.Generator().manual_seed(args.seed)
    state = {"epoch": 1, "next_step": 0, "order": None, "evaluations": 0, "best": float("inf"),
             "best_evaluation": None, "bad": 0, "history": [], "stopped": False, "global_step": 0,
             "train_terms": [], "document_reference": None, "init_raw_gate": None}
    if recovery.exists():
        saved = torch.load(recovery, map_location="cpu", weights_only=False)
        shared.demand(saved["binding"] == binding, "resume binding differs")
        restore(model, saved["model"], extra)
        optimizer.load_state_dict(saved["optimizer"])
        scheduler.load_state_dict(saved["scheduler"])
        order_rng.set_state(saved["order_rng"])
        torch.set_rng_state(saved["torch_rng"])
        if device.type == "cuda":
            torch.cuda.set_rng_state_all(saved["cuda_rng"])
        state = saved["state"]
        gate = read(output / "initial-gate.json")
    else:
        shared.demand(not (output / "binding.json").exists(), "incomplete first evaluation; use a new output directory")
        bridge.write_json(output / "binding.json", binding)
        # The per-cohort, per-label weight log precedes any optimizer step.
        bridge.write_json(output / "cohort-weights.json", weight_log)
        gate = initial_gate(model, train, device, pad_id, protocol["microbatch_token_budget"],
                            init_fit=init_record is not None)
        bridge.write_json(output / "initial-gate.json", gate)
    shared.demand(read(output / "cohort-weights.json") == json.loads(json.dumps(weight_log)),
                  "cohort weight log differs from this bundle")
    started = time.monotonic()

    def checkpoint():
        atomic_save(recovery, {"binding": binding, "model": model_state(model, extra),
                               "optimizer": optimizer.state_dict(),
                               "scheduler": scheduler.state_dict(), "order_rng": order_rng.get_state(),
                               "torch_rng": torch.get_rng_state(),
                               "cuda_rng": torch.cuda.get_rng_state_all() if device.type == "cuda" else [],
                               "state": state})

    metric, guard = stop["metric"], protocol.get("document_guard")

    def evaluation(epoch, step):
        first = state["evaluations"] == 0
        result = evaluate(model, dev, device, pad_id, role_weight, export=first and init_dir is not None)
        score, loss, diagnostics = result[:3]
        shared.demand(math.isfinite(score) and math.isfinite(loss), "nonfinite Dev-iter result")
        if first and init_dir is not None:
            state["init_raw_gate"] = init_raw_gate(result[3], init_dir, subset=dev_keep is not None)
            bridge.write_json(output / "init-raw-gate.json", state["init_raw_gate"])
            if mechanism is not None:
                state["init_token_gate"] = init_token_gate(result[5], init_dir)
                bridge.write_json(output / "init-token-gate.json", state["init_token_gate"])
        if first:
            state["document_reference"] = score
        if metric == INVOLVEMENT_METRIC:
            value, eligible = score, True
        else:
            value = diagnostics["span"]["mixed_word_brier_raw"]
            shared.demand(value is not None and math.isfinite(value), "no Dev-iter mixed-document word Brier")
            eligible = metric == WORD_METRIC or score <= state["document_reference"] + guard
        improved = eligible and value < state["best"] - stop["min_delta"]
        if improved or state["best_evaluation"] is None:
            state["best"], state["best_evaluation"], state["bad"] = value, state["evaluations"], 0
            atomic_save(best_path, {"binding": binding, "evaluation": state["evaluations"], "epoch": epoch,
                                    "step": step, "global_step": state["global_step"], "model": model_state(model)})
        else:
            state["bad"] += 1
        terms = state["train_terms"]
        mean = {key: sum(t[key] for t in terms) / len(terms) for key in terms[0]} if terms else None
        state["train_terms"] = []
        row = {"evaluation": state["evaluations"], "epoch": epoch, "step_in_epoch": step,
               "global_step": state["global_step"], "dev_iter_involvement_brier": score,
               "selection_metric": metric, "selection_value": value, "selection_eligible": eligible,
               "dev_iter_joint_loss": loss, "dev_iter_diagnostics": diagnostics, "improved": improved,
               "learning_rates": scheduler.get_last_lr()[:1] + scheduler.get_last_lr()[-1:],
               "reversal_scale": aux.reversal_scale(arm["style_adversary"]["reversal"], state["global_step"],
                                                    steps_per_epoch) if arm["style_adversary"] else None,
               "train_terms_mean_since_last_evaluation": mean,
               "elapsed_seconds_this_invocation": time.monotonic() - started}
        state["history"].append(row)
        state["evaluations"] += 1
        state["stopped"] = state["bad"] >= stop["patience_evaluations"]
        bridge.write_json(output / "history.json", state["history"])
        print(json.dumps(row), flush=True)

    def boundary(epoch, step):
        """--final-step: an evaluation boundary without Dev-iter (training terms only)."""
        terms = state["train_terms"]
        state["train_terms"] = []
        row = {"boundary": len(state["train_log"]), "epoch": epoch, "step_in_epoch": step,
               "global_step": state["global_step"],
               "learning_rates": scheduler.get_last_lr()[:1] + scheduler.get_last_lr()[-1:],
               "reversal_scale": aux.reversal_scale(arm["style_adversary"]["reversal"], state["global_step"],
                                                    steps_per_epoch) if arm["style_adversary"] else None,
               "train_terms_mean_since_last_boundary":
                   {key: sum(t[key] for t in terms) / len(terms) for key in terms[0]} if terms else None,
               "elapsed_seconds_this_invocation": time.monotonic() - started}
        state["train_log"].append(row)
        bridge.write_json(output / "train-log.json", state["train_log"])
        print(json.dumps(row), flush=True)

    if final_step:
        state.setdefault("train_log", [])
        if not recovery.exists():
            checkpoint()
    elif state["evaluations"] == 0:
        evaluation(0, 0)
        checkpoint()
    clip, aux_clip = o["gradient_clip_norm"], o["aux_gradient_clip_norm"]
    # Separate clipping: a large auxiliary-head gradient never shrinks the
    # main-objective update (the reversed adversary gradient that reaches the
    # encoder is part of the main group by design).
    main_params = [p for p in model.parameters() if p.requires_grad]
    aux_params = [p for m in extra.values() for p in m.parameters()]
    structure = None
    while not state["stopped"] and state["epoch"] <= protocol["max_epochs"]:
        epoch = state["epoch"]
        if state["order"] is None:
            state["order"] = epoch_order(arm, train, lb, order_rng, args.seed, epoch)
        order = state["order"]
        if structure is None and arm["batching"]["kind"] == "voice_mixed":
            structure = aux.batch_structure(order, *contrastive_keys(train), lb)
        while state["next_step"] < steps_per_epoch and not state["stopped"]:
            step = state["next_step"]
            indices = order[step * lb:(step + 1) * lb]
            model.train()
            for m in extra.values():
                m.train()
            optimizer.zero_grad(set_to_none=True)
            lam = aux.reversal_scale(arm["style_adversary"]["reversal"], state["global_step"], steps_per_epoch) \
                if arm["style_adversary"] else 0.
            terms = train_step(model, aux_heads, arm, train, indices, device, pad_id,
                               protocol["microbatch_token_budget"], role_weight, lam, aux_den, s2_state)
            terms["grad_norm_main"] = float(torch.nn.utils.clip_grad_norm_(main_params, clip, error_if_nonfinite=True))
            terms["grad_norm_aux"] = float(torch.nn.utils.clip_grad_norm_(aux_params, aux_clip,
                                                                         error_if_nonfinite=True)) if aux_params else 0.
            state["train_terms"].append(terms)
            optimizer.step()
            scheduler.step()
            state["next_step"] += 1
            state["global_step"] += 1
            if state["next_step"] in eval_points:
                if final_step:
                    boundary(epoch, state["next_step"])
                else:
                    evaluation(epoch, state["next_step"])
                finished = state["next_step"] == steps_per_epoch
                if finished:
                    state["epoch"], state["next_step"], state["order"] = epoch + 1, 0, None
                checkpoint()
                if finished:
                    break
    if final_step:
        shared.demand(state["global_step"] == total_steps and not state["stopped"],
                      "a final-step fit ends after its last scheduled step")
        best = {"evaluation": 0, "epoch": protocol["max_epochs"], "step": steps_per_epoch,
                "global_step": state["global_step"], "model": model_state(model)}
    else:
        best = torch.load(best_path, map_location="cpu", weights_only=False)
        shared.demand(best["binding"] == binding, "best checkpoint binding differs")
        restore(model, best["model"])
    frozen_check = None
    if frozen:
        # S0 arms leave every document output bit-identical: verified on tensors.
        shared.demand(frozen_equal(model, init_saved), "frozen encoder/document heads changed")
        frozen_check = {"encoder_and_document_heads_bitwise_equal_to_init": True}
    atomic_save(output / "selected.pt", {"binding": binding,
                                         **{k: best[k] for k in ("evaluation", "epoch", "step", "global_step")},
                                         "model": best["model"]})
    save_file({k: v.contiguous() for k, v in best["model"]["encoder"].items()}, str(output / "encoder.safetensors"))
    heads = dict(best["model"]["heads"])
    heads.update({"span." + k: v for k, v in best["model"].get("span", {}).items()})
    heads.update({"token_head." + k: v for k, v in best["model"].get("token_head", {}).items()})
    heads.update({"span_tower." + k: v for k, v in best["model"].get("span_tower", {}).items()})
    save_file({k: v.contiguous() for k, v in heads.items()}, str(output / "heads.safetensors"))
    score, loss, diagnostics, raw, preds, token_rows = evaluate(model, dev, device, pad_id, role_weight, export=True)
    if final_step:
        # The one Dev-iter evaluation of a final-step fit: recorded, never used to choose anything.
        shared.demand(math.isfinite(score) and math.isfinite(loss), "nonfinite Dev-iter result")
        state["history"] = [{"evaluation": 0, "epoch": best["epoch"], "step_in_epoch": best["step"],
                             "global_step": best["global_step"], "dev_iter_involvement_brier": score,
                             "selection_metric": None, "selection_value": None, "selection_eligible": False,
                             "dev_iter_joint_loss": loss, "dev_iter_diagnostics": diagnostics, "improved": None,
                             "learning_rates": scheduler.get_last_lr()[:1] + scheduler.get_last_lr()[-1:],
                             "reversal_scale": None, "train_terms_mean_since_last_evaluation": None,
                             "elapsed_seconds_this_invocation": time.monotonic() - started}]
        state["evaluations"] = 1
        bridge.write_json(output / "history.json", state["history"])
    write_jsonl(output / "dev_iter-raw.jsonl", raw)
    write_jsonl(output / "dev_iter-predictions.jsonl", preds)
    if th is not None:
        write_jsonl(output / "dev_iter-token-logits.jsonl", token_rows)
    if frozen:
        frozen_check["selected_dev_iter_document_raw_vs_init"] = init_raw_gate(raw, init_dir, subset=dev_keep is not None)
    report = None
    if report_split is not None:
        # Opened only now: the checkpoint and the selection record are already fixed.
        use_record = Path(args.use_record)
        with use_record.open("x") as f:
            f.write(json.dumps({"schema": "slop_ninja_full_a_sealed_split_use_v1", "split": report_split,
                                "fit_output": str(output), "binding_sha256": bridge.sha256(output / "binding.json"),
                                "selected_checkpoint_sha256": bridge.sha256(output / "selected.pt"),
                                "opened_after_selection": True}, sort_keys=True) + "\n")
            f.flush()
            os.fsync(f.fileno())
        rep = bundle.Population(root, report_split, tokenize, special, profiles["ai"], cohort_weighted=False,
                                span_words=th is not None)
        disjoint(train, rep)
        disjoint(dev, rep)
        _, _, rep_diag, rep_raw, rep_preds, rep_tokens = evaluate(model, rep, device, pad_id, role_weight, export=True)
        write_jsonl(output / f"report-{report_split}-raw.jsonl", rep_raw)
        write_jsonl(output / f"report-{report_split}-predictions.jsonl", rep_preds)
        if th is not None:
            write_jsonl(output / f"report-{report_split}-token-logits.jsonl", rep_tokens)
        report = {"schema": REPORT_SCHEMA, "split": report_split, "read_after_selection": True,
                  "use_record": str(use_record), "use_record_sha256": bridge.sha256(use_record),
                  "used_for_selection": False, "rows": len(rep),
                  "uncalibrated_balanced_involvement_brier": balanced_brier(rep_raw, rep), "diagnostics": rep_diag,
                  "predictions_sha256": bridge.sha256(output / f"report-{report_split}-predictions.jsonl")}
    record = {"schema": FIT_SCHEMA, "complete": True, "binding": binding,
        "selected_evaluation": best["evaluation"], "selected_epoch": best["epoch"], "selected_step": best["step"],
        "selected_global_step": best["global_step"], "selection_split": SELECTION_SPLIT,
        "selection_metric": metric, "selection_value": state["best"], "document_guard": guard,
        "selection_split_involvement_brier": state["history"][best["evaluation"]]["dev_iter_involvement_brier"],
        "selection_score_is_biased": True, "init_fit": init_record, "init_raw_gate": state["init_raw_gate"],
        "frozen_document_check": frozen_check,
        "s2": None if mechanism is None else {
            "mechanism": mechanism, "arm_s2": arm["s2"], "init_token_gate": state.get("init_token_gate"),
            "pair_contrast": None if pair_contrast is None else pair_contrast.record(),
            "teacher": None if teacher is None else {"source": "init_fit", "precision": "fp32",
                                                     "selected_checkpoint_sha256": init_record["selected_checkpoint_sha256"],
                                                     "gate": teacher_check},
            "span_tower": None if mechanism != "span_tower" else {"layers": arm["s2"]["layers"],
                                                                  "learning_rate": arm["s2"]["learning_rate"]}},
        "diagnostic_dev_subset": binding.get("diagnostic_dev_subset"),
        "token_head": None if th is None else {**th, "rule": full_a.TOKEN_HEAD_RULE, "freeze": sp["freeze"],
                                               "token_weighting": sp["token_weighting"],
                                               "label_policy": full_a.SPAN_LABEL_POLICY},
        "dev_iter_token_logits_sha256": bridge.sha256(output / "dev_iter-token-logits.jsonl") if th is not None
        else None,
        "dev_iter_rows_sha256": dev_digest,
        "selected_dev_iter_diagnostics": diagnostics, "report": report,
        "stopped_early": state["stopped"], "evaluations": state["evaluations"], "steps_per_epoch": steps_per_epoch,
        "schedule": {"horizon_steps": horizon, "warmup_steps": warmup, "total_scheduled_steps": total_steps,
                     "warmup_basis": "fraction_of_max_epochs_x_steps_per_epoch", "shape": schedule["shape"],
                     "floor_fraction": schedule["floor_fraction"], "max_epochs": protocol["max_epochs"]},
        "gradient_clipping": {"main": clip, "aux": aux_clip, "groups": "separate main (encoder, heads, span) "
                              "and auxiliary (adversary, projection, share bins) norms; pre-clip norms in history"},
        "evaluation_points_per_epoch": eval_points, "initial_gate": gate,
        "cohort_weight_log": {"file": "cohort-weights.json", "sha256": bridge.sha256(output / "cohort-weights.json"),
                              "complete": True},
        "aux_heads": {"style_adversary": aux_heads.vocab() if arm["style_adversary"] else None,
                      "contrastive": arm["contrastive"] is not None,
                      "share_bins": None if aux_heads.share_bins is None else list(aux.SHARE_BIN_EDGES),
                      "exported": False,
                      "batch_structure": structure, "supervised_mass": aux_den},
        "real_data_fit": True, "calibrated": False, "final_test_opened": False, "ai_roster": roster_record,
        "diagnostic_train_subset": binding.get("diagnostic_train_subset"),
        "trained_selected_heads": trained_heads(best, train, frozen, init_dir),
        "train_population": train.token_counts(), "dev_iter_population": dev.token_counts(),
        "train_counts": train.counts(), "dev_iter_counts": dev.counts(),
        "train_dropped_zero_weight": {f"{c}|{k}": n for (c, k), n in sorted(train.dropped.items())},
        "load_seconds": load_seconds, "peak_rss_bytes": bundle.peak_rss_bytes(),
        "peak_cuda_allocated_bytes": torch.cuda.max_memory_allocated() if device.type == "cuda" else None,
        "selected_checkpoint_sha256": bridge.sha256(output / "selected.pt"),
        "encoder_sha256": bridge.sha256(output / "encoder.safetensors"),
        "heads_sha256": bridge.sha256(output / "heads.safetensors"),
        "dev_iter_predictions_sha256": bridge.sha256(output / "dev_iter-predictions.jsonl"),
        "dev_iter_raw_sha256": bridge.sha256(output / "dev_iter-raw.jsonl"),
        "limitations": ["The Dev-iter score chose the checkpoint and is optimistic; report it only beside an "
                        "independent split's score, never as an independent result.",
                        "Dev-iter iteration evidence, not final Test.",
                        "CAL calibration (Rust) and Rust full-encoder replay still required.",
                        "Evaluation-0 outputs are the V6 initialization; share/attribution heads untrained there.",
                        "Style adversary and contrastive projection are training-only and not exported."] +
                       (["Token-head metrics here are raw (untilted) Dev-iter values; span emission (tilt, "
                         "decoder, sentence/span maps) is applied in export and the Rust runtime."]
                        if th is not None else [])}
    if allow_record is not None:
        record["train_allowlist"] = allow_record
    if final_step:
        record.update({"checkpoint_rule": FINAL_STEP_RULE, "selection_split": None, "selection_metric": None,
                       "selection_value": None, "document_guard": None, "selection_score_is_biased": False,
                       "train_log": {"file": "train-log.json", "sha256": bridge.sha256(output / "train-log.json"),
                                     "boundaries": len(state["train_log"])}})
        record["limitations"] = [
            "Final-step checkpoint: no Dev-iter selection; Dev-iter was scored once, after the last step.",
            "Dev-iter iteration evidence, not final Test."] + \
            [x for x in record["limitations"][2:] if not x.startswith("Evaluation-0")]
    bridge.write_json(output / "fit.json", record)
    print(json.dumps({"complete": True, "selected_evaluation": best["evaluation"],
                      "best": None if final_step else state["best"]}), flush=True)


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    for key in ("data", "config", "artifact", "init", "out", "arm"):
        p.add_argument("--" + key, required=True)
    p.add_argument("--seed", type=int, required=True)
    p.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
    p.add_argument("--report-split", choices=REPORT_SPLITS, default=None,
                   help="non-selection split evaluated once after the checkpoint is frozen")
    p.add_argument("--consume-sealed-split", choices=REPORT_SPLITS, default=None,
                   help="explicit consent to read the sealed --report-split (must name the same split)")
    p.add_argument("--use-record", default=None, help="new file recording the one-time sealed-split use")
    p.add_argument("--train-subset-rows", type=int, default=None,
                   help="diagnostic ablations only: train on a stratified Train subset of this many rows")
    p.add_argument("--train-subset-rule", choices=("stratified", "coarse"), default="stratified",
                   help="diagnostic subset strata: (cohort, label, origin, domain) or the S2 smoke's (cohort, origin)")
    p.add_argument("--diagnostic-dev-subset-rows", type=int, default=None,
                   help="diagnostic subset fits only: evaluate on a keyed Dev-iter subset (a multiple of 3)")
    p.add_argument("--init-fit", default=None,
                   help="completed fit directory whose selected.pt initializes encoder, document heads (and token head)")
    p.add_argument("--train-allowlist", default=None,
                   help="slop_ninja_a_row_allowlist_v1 file: train on its Train rows only (weights kept, no refill)")
    p.add_argument("--final-step", action="store_true",
                   help="take the final step's model: no checkpoint selection; Dev-iter scored once, after training")
    run(p.parse_args())

"""Document-model training options for full A (protocol doc-r2; each off by default).

With none of them in the protocol the trainer's computation is unchanged: it
builds the same populations and calls `full_a.objective` as before.

- Share-bin balance (`share_supervision.bin_balance`, rule
  `slop_ninja_share_bin_balance_v1`). Mixed Train rows fall into the share bins
  of `full_a_aux.SHARE_BIN_EDGES` (seven bins; a share of exactly 1 has its
  own). With W_b the summed final Train weight of the mixed rows in bin b,
  W = sum_b W_b and K the number of bins with W_b > 0, the raw multiplier is
  W / (K W_b), clipped to [1/r, r] (r = `max_ratio`), and every multiplier is
  then scaled by one factor so that sum_b c_b W_b = W. A mixed row of bin b
  uses c_b times its weight in the document share term and the conditional
  share term only. Human and model-only rows and every other term keep their
  weights, and both share denominators are unchanged. The multipliers are
  computed once from the Train population and written to fit.json.
- Attribution mask (`attribution.unsupervised_cohort_ids`). Rows of a listed
  cohort keep their generator label (the row contract requires it) but get no
  AI attribution target and no AI attribution denominator mass, and their
  generator key is None in the trainer's contrastive keys: they are never a
  contrastive anchor, positive or negative, and voice-mixed batching deals
  them as rows outside the generator structure. Every AI word still counts
  toward share and involvement. Each listed cohort must have Train rows.
- Seed-ensemble distillation (arm block `distill`, rule
  `slop_ninja_doc_distill_v1`, with `--distill-targets FILE`). Each document
  term becomes (1 - alpha) hard + alpha soft against the teacher-mean targets
  of `seed_mean_full_a distill-targets`: origin cross-entropy to the teacher
  origin distribution plus the involvement Brier to the teacher involvement;
  share squared error to the teacher share; on mixed rows, the conditional
  share BCE to the teacher q; and, only on rows whose attribution is
  supervised, attribution cross-entropy to the teacher's conditional AI
  distribution. A temperature T other than 1 softens the two cross-entropies
  (student log-softmax of its log-probabilities over T, teacher distribution
  renormalised from its probabilities to the power 1/T) and scales them by
  T^2; the Brier, squared-error and BCE terms use the teacher means directly.
  The share-bin multipliers apply to the hard and soft share terms alike.
  alpha = 0 adds no soft term, so the fit is the fit without `distill`. The
  targets file is bound by its digest; it must hold every Train-block row
  exactly once (zero-weight rows included), with the row's text digest, and
  nothing else.

The balanced and distilled objective (`objective`) is used only when share-bin
balance or distillation is on, and only in document-head arms (no span head,
token head or spans-S2 mechanism).
"""
from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path

import torch
from torch.nn import functional as F

import full_a
import full_a_aux as aux
import full_a_bundle as bundle


BIN_BALANCE_RULE = "slop_ninja_share_bin_balance_v1"
DISTILL_RULE = "slop_ninja_doc_distill_v1"
DISTILL_TARGETS_SCHEMA = "slop_ninja_full_a_distill_targets_v1"
MASK_RULE = ("slop_ninja_attribution_mask_v1: rows of the listed cohorts keep their generator label but get no AI "
             "attribution target, no AI attribution denominator mass and generator key None in the contrastive "
             "keys (never an anchor, positive or negative; dealt outside the voice-mixed generator chunks)")
BIN_BALANCE_TEXT = ("slop_ninja_share_bin_balance_v1: mixed Train rows binned by SHARE_BIN_EDGES; W_b = summed final "
                    "Train weight per bin, W = sum W_b, K = bins with W_b > 0; c_b = clip(W / (K W_b), 1/r, r), then "
                    "scaled so sum c_b W_b = W; c_b multiplies a mixed row's weight in the share and conditional "
                    "share terms only (hard and soft)")
DISTILL_TEXT = ("slop_ninja_doc_distill_v1: each document term = (1 - alpha) hard + alpha soft; soft origin = T^2 "
                "CE(teacher origin^(1/T) renormalised, log_softmax(log origin / T)) + (P(involved) - teacher "
                "involvement)^2; soft share = (share - teacher share)^2; soft conditional share (mixed rows) = "
                "BCE(q, teacher q); soft attribution (supervised rows only) = T^2 CE(teacher AI distribution^(1/T) "
                "renormalised, log_softmax(AI logits / T)); alpha = 0 adds no soft term")
PROBABILITY_SUM_TOLERANCE = 1e-6
# Logged soft-term parts (alpha x soft, as included in the loss) when distillation adds soft terms.
DISTILL_PARTS = ("distill_soft_origin", "distill_soft_share", "distill_soft_conditional_share",
                 "distill_soft_attribution")


def demand(ok, message):
    if not ok:
        raise ValueError(message)


def _number(v, lo=None, hi=None, positive=False):
    ok = type(v) in (int, float) and math.isfinite(v)
    return ok and (lo is None or v >= lo) and (hi is None or v <= hi) and (not positive or v > 0)


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


# ------------------------------------------------------------------ protocol

def validate_bin_balance(spec, name):
    ok = isinstance(spec, dict) and set(spec) == {"rule", "max_ratio"} and spec["rule"] == BIN_BALANCE_RULE
    ok = ok and _number(spec["max_ratio"]) and spec["max_ratio"] >= 1
    demand(ok, "share bin balance: " + name)


def validate_distill(spec, name):
    demand(isinstance(spec, dict) and set(spec) == {"rule", "alpha", "temperature"} and spec["rule"] == DISTILL_RULE
           and _number(spec["alpha"], lo=0, hi=1) and _number(spec["temperature"], positive=True),
           "distill: " + name)


def validate_mask(cohorts):
    demand(isinstance(cohorts, list) and cohorts and all(isinstance(c, str) and c for c in cohorts) and
           len(set(cohorts)) == len(cohorts), "attribution unsupervised_cohort_ids: a non-empty list of distinct ids")


def options(arm):
    """(bin balance spec or None, distill spec or None) of an arm."""
    return arm["share_supervision"].get("bin_balance"), arm.get("distill")


def validate_document_arm(name, arm, span):
    """Share-bin balance and distillation run in document-head arms only."""
    balance, distill = options(arm)
    if balance is not None:
        validate_bin_balance(balance, name)
    if distill is not None:
        validate_distill(distill, name)
    if balance is not None or distill is not None:
        demand(arm["span_head"] is None and arm["share_mode"] == "head" and span["token_head"] is None and
               span["freeze"] == "none" and arm.get("s2") is None,
               "share bin balance and distillation need a document-head arm (no span head, token head or S2): " + name)


# ------------------------------------------------------------------ attribution mask

class MaskedPopulation(bundle.Population):
    """A Train population whose listed cohorts carry no AI attribution supervision (MASK_RULE)."""

    def __init__(self, *args, unsupervised_cohorts, **kwargs):
        super().__init__(*args, **kwargs)
        cohorts = frozenset(unsupervised_cohorts)
        present = {m.cohort_id for m in self.meta}
        missing = sorted(cohorts - present)
        demand(not missing, f"unsupervised attribution cohorts without Train rows: {missing}")
        self.unsupervised_cohorts = cohorts
        self.attribution_masked = [m.cohort_id in cohorts for m in self.meta]
        # Same order and arithmetic as Population's denominator, without the masked rows.
        ai = 0.
        for m, w, masked in zip(self.meta, self.weights, self.attribution_masked):
            if m.label in self.ai_index and not masked:
                ai += w
        self.denominators["ai"] = ai

    def targets(self, i):
        t = self.meta[i].targets(self.ai_index)
        if self.attribution_masked[i]:
            t["ai"] = None
        return t

    def mask_record(self):
        rows = {c: {"rows": 0, "ai_rows": 0, "mass": 0.} for c in sorted(self.unsupervised_cohorts)}
        for m, w, masked in zip(self.meta, self.weights, self.attribution_masked):
            if masked:
                e = rows[m.cohort_id]
                e["rows"] += 1
                e["ai_rows"] += m.label is not None
                e["mass"] += w
        return {"rule": MASK_RULE, "cohorts": rows, "supervised_ai_mass": self.denominators["ai"]}


# ------------------------------------------------------------------ share-bin balance

def bin_multipliers(meta, weights, max_ratio):
    """The rule's multipliers from the Train population (text-free record)."""
    bins = len(aux.SHARE_BIN_EDGES) + 1
    members = [[] for _ in range(bins)]
    for m, w in zip(meta, weights):
        if m.origin == 2:
            members[aux.share_bin(m.share)].append(w)
    rows = [len(x) for x in members]
    mass = [math.fsum(x) for x in members]
    total = math.fsum(mass)
    populated = [b for b in range(bins) if mass[b] > 0]
    demand(populated and total > 0, "the share-bin multipliers need mixed Train rows with weight")
    k = len(populated)
    raw = [total / (k * mass[b]) if mass[b] > 0 else None for b in range(bins)]
    clipped = [None if r is None else min(max(r, 1. / max_ratio), max_ratio) for r in raw]
    scale = total / math.fsum(clipped[b] * mass[b] for b in populated)
    c = [None if x is None else x * scale for x in clipped]
    check = math.fsum(c[b] * mass[b] for b in populated)
    demand(abs(check - total) <= 1e-9 * total, "the share-bin multipliers do not preserve the mixed share mass")
    return {"rule": BIN_BALANCE_TEXT, "edges": list(aux.SHARE_BIN_EDGES), "max_ratio": max_ratio,
            "rows": rows, "mass": mass, "total_mixed_mass": total, "populated_bins": k, "raw": raw,
            "clipped": clipped, "scale": scale, "multipliers": c, "balanced_mixed_mass": check}


def row_factors(meta, record):
    """Per Train row: c_b for a mixed row, None for every other row."""
    c = record["multipliers"]
    return [c[aux.share_bin(m.share)] if m.origin == 2 else None for m in meta]


# ------------------------------------------------------------------ distillation targets

def train_block_digests(root):
    """{id: text_sha256} of every Train-block row (zero-weight rows included; text-free)."""
    out = {}
    for m, _ in bundle.stream(root, ["train"]):
        demand(m.id not in out, f"duplicate Train id {m.id}")
        out[m.id] = m.text_sha256
    return out


def _probabilities(v, n, what):
    demand(isinstance(v, list) and len(v) == n and all(_number(x, lo=0, hi=1) for x in v) and
           abs(math.fsum(v) - 1.) <= PROBABILITY_SUM_TOLERANCE, what)
    return [float(x) for x in v]


def read_targets(path, ai_profiles, block):
    """(header, {id: target}) of a distillation targets file checked against the Train block digests."""
    path = Path(path)
    rows, header = {}, None
    with path.open("rb") as f:
        for n, line in enumerate(f, 1):
            if not line.strip():
                continue
            r = json.loads(line)
            if header is None:
                demand(isinstance(r, dict) and r.get("schema") == DISTILL_TARGETS_SCHEMA and
                       r.get("partition") == "train", "distillation targets: header schema")
                demand(r.get("ai_profiles") == list(ai_profiles),
                       "distillation targets: AI roster differs from the contract's")
                header = r
                continue
            demand(isinstance(r, dict) and set(r) == {"id", "text_sha256", "origin", "share",
                                                      "conditional_mixed_share", "ai"}, f"target line {n}: fields")
            rid = r["id"]
            demand(isinstance(rid, str) and rid not in rows, f"target line {n}: duplicate or invalid id")
            demand(rid in block, f"target line {n}: id outside the Train block")
            demand(r["text_sha256"] == block[rid], f"target {rid}: text digest differs from the Train row")
            demand(_number(r["share"], lo=0, hi=1) and _number(r["conditional_mixed_share"], lo=0, hi=1),
                   f"target {rid}: share")
            rows[rid] = {"origin": _probabilities(r["origin"], 3, f"target {rid}: origin"),
                         "share": float(r["share"]), "q": float(r["conditional_mixed_share"]),
                         "ai": _probabilities(r["ai"], len(ai_profiles), f"target {rid}: AI distribution")}
    demand(header is not None, "distillation targets: empty file")
    demand(header.get("rows") == len(rows), "distillation targets: header row count differs")
    missing = len(set(block) - set(rows))
    demand(missing == 0, f"distillation targets: {missing} Train rows have no target")
    return header, rows


def distill_state(spec, path, population, root, ai_profiles):
    """Soft targets per Train population row and the binding record of the targets file."""
    digest = sha256_file(path)
    header, rows = read_targets(path, ai_profiles, train_block_digests(root))
    soft = [rows[m.id] for m in population.meta]
    record = {"rule": DISTILL_TEXT, "alpha": spec["alpha"], "temperature": spec["temperature"],
              "targets_sha256": digest, "targets_rows": len(rows),
              "teachers": header.get("teachers"), "targets_rule": header.get("rule")}
    return soft, record


# ------------------------------------------------------------------ objective

CONDITIONAL_SHARE_EPS = full_a.CONDITIONAL_SHARE_EPS


def _soft_ce(log_probs, teacher, temperature):
    """T^2 x cross-entropy of a teacher distribution (list) against log-probabilities (1-D tensor)."""
    p = log_probs.new_tensor(teacher)
    if temperature == 1:
        return -(p * log_probs).sum()
    pt = torch.softmax(torch.log(p.double()) / temperature, -1).to(log_probs.dtype)
    return -(pt * torch.log_softmax(log_probs / temperature, -1)).sum() * (temperature * temperature)


def objective(out, targets, weights, denominators, role_weight=.25, conditional_weight=0., share_mode="head",
              parts=None, share_factors=None, soft=None, alpha=0., temperature=1.):
    """Document terms of full_a.objective with per-row share multipliers and distillation.

    share_factors: per row c_b (mixed rows) or None (the row weight unchanged);
    soft: per row teacher targets (distill_state) or None; alpha = 0 adds no
    soft term. With share_factors and soft absent this is full_a.objective's
    document computation, term for term."""
    demand(share_mode == "head", "the document objective needs share mode head")
    device = out["origin"].device
    loss = out["encoding"].sum() * 0
    use_soft = soft is not None and alpha > 0
    hard_scale = 1. - alpha
    for i, (t, w) in enumerate(zip(targets, weights)):
        c = None if share_factors is None else share_factors[i]
        ws = w if c is None else w * c
        s = soft[i] if use_soft else None
        y = t.get("origin")
        if y is not None:
            target = torch.tensor([y], dtype=torch.long, device=device)
            value = F.nll_loss(out["log_origin"][i:i + 1], target) + \
                (out["origin"][i, 1:].sum() - float(y != 0)).square()
            if s is not None:
                involvement = s["origin"][1] + s["origin"][2]
                sv = _soft_ce(out["log_origin"][i], s["origin"], temperature) + \
                    (out["origin"][i, 1:].sum() - involvement).square()
                _add(parts, "distill_soft_origin", alpha * sv * (w / denominators["origin"]))
                value = hard_scale * value + alpha * sv
            term = value * (w / denominators["origin"])
            _add(parts, "origin", term)
            loss = loss + term
        y = t.get("share")
        if y is not None:
            value = (out["share"][i] - y).square()
            if s is not None:
                sv = (out["share"][i] - s["share"]).square()
                _add(parts, "distill_soft_share", alpha * sv * (ws / denominators["share"]))
                value = hard_scale * value + alpha * sv
            term = value * (ws / denominators["share"])
            _add(parts, "share", term)
            loss = loss + term
            if conditional_weight and t.get("origin") == 2:
                q = full_a.conditional_share(out, share_mode)[i].clamp(CONDITIONAL_SHARE_EPS,
                                                                        1. - CONDITIONAL_SHARE_EPS)
                value = F.binary_cross_entropy(q, q.new_tensor(float(y)))
                if s is not None:
                    sv = F.binary_cross_entropy(q, q.new_tensor(s["q"]))
                    _add(parts, "distill_soft_conditional_share",
                         alpha * conditional_weight * sv * (ws / denominators["conditional_share"]))
                    value = hard_scale * value + alpha * sv
                term = conditional_weight * value * (ws / denominators["conditional_share"])
                _add(parts, "conditional_share", term)
                loss = loss + term
        for role in ("human", "ai"):
            y = t.get(role)
            if y is not None:
                target = torch.tensor([y], dtype=torch.long, device=device)
                value = F.cross_entropy(out[f"{role}_logits"][i:i + 1], target)
                if s is not None and role == "ai":
                    sv = _soft_ce(torch.log_softmax(out["ai_logits"][i], -1), s["ai"], temperature)
                    _add(parts, "distill_soft_attribution", alpha * role_weight * sv * (w / denominators[role]))
                    value = hard_scale * value + alpha * sv
                term = role_weight * value * (w / denominators[role])
                _add(parts, "attribution", term)
                loss = loss + term
    return loss


def _add(parts, key, term):
    if parts is not None:
        parts[key] = parts.get(key, 0.) + float(term.detach())

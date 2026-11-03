"""Protocol spans-S2 mechanisms for the full-A trainer (all optional; absent
options leave the trainer's computation unchanged).

S2 arms keep the token head of spans-v1 and change how the document side
reacts to the spliced rows:

- `anchor`: every document label term is replaced, on every Train row, by the
  same term against a frozen teacher's outputs (the init fit's encoder and
  document heads, FP32, no gradient). The document heads follow the teacher
  while the encoder trains jointly for the token head.
- `pair_contrast`: whenever a spliced row is in a logical batch, its untouched
  parent (`pair_id`) joins the batch as an extra human entry carrying the
  child's document weight, document terms only. Every human document weight
  is scaled by H / (H + dH) so the total human mass is unchanged.
- `span_tower`: the encoder and document heads stay frozen; copies of the
  encoder's top K layers and final norm (initialised from the frozen
  weights) read the hidden state entering layer L - K and feed the token head.
  Document outputs are bit-identical to the init fit.

Also the coarse diagnostic Train subset (strata (cohort, label, origin)) and the
keyed diagnostic Dev-iter subset used by the S2 smoke.
"""
from __future__ import annotations

import copy
import hashlib

import torch
from torch import nn
import torch.nn.functional as F

import full_a
import full_a_bundle as bundle

MECHANISMS = ("anchor", "pair_contrast", "span_tower")
SPLICED_COHORT = "spliced_rewrite_v1"
ANCHOR_RULE = ("slop_ninja_s2_anchor_v1: CE(pi0, pi) + (inv - inv0)^2 + (s - s0)^2 + c*BCE(q, q0) + "
               "rho*CE(softmax(a0), softmax(a)); weight w / sum(w); teacher = init fit, FP32, no gradient")
CONTRAST_RULE = ("slop_ninja_s2_pair_contrast_v1: parent entry per spliced child in the same logical batch, child "
                 "document weight, document terms only; human document weights x H/(H+dH)")
TOWER_RULE = "slop_ninja_s2_span_tower_v1: token_head(final_norm_copy(layers_copy[L-K..L-1](h_in(L-K))))"
COARSE_SUBSET_RULE = ("strata (cohort, label, origin); largest remainder, >= 1 per stratum; ascending "
                      "sha256(seed NUL id); weight x N_stratum/n_stratum")
DEV_SUBSET_KEY = "slopninja-a-spans-s2-smoke:"


def demand(ok, message):
    full_a.demand(ok, message)


def validate(name, s2, arm):
    """The arm's optional `s2` field. Returns the mechanism (or None)."""
    if s2 is None:
        return None
    demand(isinstance(s2, dict) and s2.get("mechanism") in MECHANISMS, "s2 mechanism: " + name)
    th = arm.get("token_head")
    demand(th is not None, "S2 arms train the token head: " + name)
    m = s2["mechanism"]
    if m == "anchor":
        demand(set(s2) == {"mechanism", "teacher", "teacher_precision"} and s2["teacher"] == "init_fit" and
               s2["teacher_precision"] == "fp32" and arm.get("freeze", "none") == "none", "s2 anchor: " + name)
    elif m == "pair_contrast":
        demand(set(s2) == {"mechanism", "cohort", "class_balance"} and s2["cohort"] == SPLICED_COHORT and
               s2["class_balance"] == "fixed_human_mass" and arm.get("freeze", "none") == "none",
               "s2 pair contrast: " + name)
    else:
        demand(set(s2) == {"mechanism", "layers", "learning_rate"} and type(s2["layers"]) is int and
               1 <= s2["layers"] <= 64 and type(s2["learning_rate"]) in (int, float) and s2["learning_rate"] > 0 and
               arm.get("freeze") == "encoder_and_document_heads", "s2 span tower: " + name)
    return m


# ------------------------------------------------------------------ anchor

def make_teacher(model):
    """Frozen FP32 copy of the init fit's encoder and document heads (no span or token head)."""
    teacher = full_a.FullA(copy.deepcopy(model.encoder), copy.deepcopy(model.heads), None, model.share_mode, None, None)
    teacher.requires_grad_(False)
    return teacher.eval()


@torch.no_grad()
def teacher_outputs(teacher, ids, mask, content, words):
    teacher.eval()
    out = teacher(ids, mask, content, autocast=False, words=words)
    return {"origin": out["origin"].float(), "share": out["share"].float(),
            "conditional_mixed_share": out["conditional_mixed_share"].float(), "ai_logits": out["ai_logits"].float()}


@torch.no_grad()
def teacher_gate(teacher, population, device, pad_id, init_dir):
    """The teacher's FP32 unpadded outputs on the evaluation rows against the init fit's own Dev-iter dump
    (origin probabilities, share, conditional share and attribution logits): the teacher is the init fit."""
    import json
    ref = {}
    with (init_dir / "dev_iter-raw.jsonl").open("rb") as f:
        for line in f:
            r = json.loads(line)
            ref[r["id"]] = r
    worst = 0.
    for i, m in enumerate(population.meta):
        ids, mask, content, _, words = population.batch([i], device, pad_id)
        t = teacher_outputs(teacher, ids, mask, content, words)
        q = ref.get(m.id)
        demand(q is not None and q["text_sha256"] == m.text_sha256, "teacher gate row absent from the init fit")
        o = q["uncalibrated_origin"]
        pairs = list(zip(t["origin"][0].tolist(), (o["human_only"], o["model_only"], o["mixed"])))
        pairs += [(float(t["share"][0]), q["uncalibrated_share"]),
                  (float(t["conditional_mixed_share"][0]), q["conditional_mixed_share"])]
        pairs += list(zip(t["ai_logits"][0].tolist(), q["ai_logits"]))
        worst = max([worst] + [abs(a - b) for a, b in pairs])
    demand(worst <= 1e-6, f"teacher differs from the init fit: {worst}")
    return {"rows": len(population), "max_abs_error": worst, "reference": "init fit dev_iter-raw.jsonl"}


def anchor_objective(out, teacher, weights, denominator, conditional_weight, role_weight, parts=None):
    """Soft-target document objective (see ANCHOR_RULE); `weights` are row document weights, `denominator`
    the total document weight (origin denominator)."""
    w = torch.tensor(weights, dtype=torch.float32, device=out["origin"].device) / denominator
    p0 = teacher["origin"]
    ce = -(p0 * out["log_origin"].float()).sum(-1)
    inv = (out["origin"][:, 1:].float().sum(-1) - p0[:, 1:].sum(-1)).square()
    share = (out["share"].float() - teacher["share"]).square()
    eps = full_a.CONDITIONAL_SHARE_EPS
    q = out["conditional_mixed_share"].float().clamp(eps, 1. - eps)
    cond = F.binary_cross_entropy(q, teacher["conditional_mixed_share"].clamp(eps, 1. - eps), reduction="none")
    a0 = teacher["ai_logits"].softmax(-1)
    attr = -(a0 * out["ai_logits"].float().log_softmax(-1)).sum(-1)
    terms = {"anchor_origin": (w * (ce + inv)).sum(), "anchor_share": (w * share).sum(),
             "anchor_conditional_share": conditional_weight * (w * cond).sum(),
             "anchor_attribution": role_weight * (w * attr).sum()}
    if parts is not None:
        for k, v in terms.items():
            parts[k] = parts.get(k, 0.) + float(v.detach())
    return sum(terms.values())


# ------------------------------------------------------------------ pair contrast

class PairContrast:
    """Child index -> parent index over a Train population, the human scale s_h and the adjusted document weights."""

    def __init__(self, train, require_all=True):
        index = {m.id: i for i, m in enumerate(train.meta)}
        self.parent, self.missing = {}, 0
        for i, m in enumerate(train.meta):
            if m.cohort_id != SPLICED_COHORT:
                continue
            demand(m.pair_id is not None and m.origin == 2, f"spliced row {m.id} without pair_id")
            j = index.get(m.pair_id)
            if j is None:
                self.missing += 1
                continue
            demand(train.meta[j].origin == 0, f"parent of {m.id} is not human-only")
            self.parent[i] = j
        demand(not require_all or self.missing == 0, f"{self.missing} spliced parents absent from Train")
        demand(self.parent, "pair contrast needs spliced rows with parents")
        human = sum(w for m, w in zip(train.meta, train.weights) if m.origin == 0)
        added = sum(train.weights[i] for i in self.parent)
        demand(human > 0 and added > 0, "pair contrast masses")
        self.human_mass, self.added_mass = human, added
        self.scale = human / (human + added)
        self.doc_weights = [w * self.scale if m.origin == 0 else w for m, w in zip(train.meta, train.weights)]
        after = sum(w for m, w in zip(train.meta, self.doc_weights) if m.origin == 0) + self.scale * added
        self.mass_relative_error = abs(after - human) / human
        demand(self.mass_relative_error <= 1e-12, "human mass changed under pair contrast")

    def entries(self, indices, train):
        """(row, document weight, token weight, contrast) per entry: the batch rows, then one parent entry per
        spliced child in the batch."""
        out = [(i, self.doc_weights[i], train.token_weights[i], False) for i in indices]
        out += [(self.parent[i], self.scale * train.weights[i], 0., True) for i in indices if i in self.parent]
        return out

    def record(self):
        return {"rule": CONTRAST_RULE, "pairs": len(self.parent), "missing_parents": self.missing,
                "human_mass": self.human_mass, "added_mass": self.added_mass, "human_scale": self.scale,
                "human_mass_relative_error": self.mass_relative_error}


def entry_groups(entries, lengths, budget):
    """Padded token-budget microbatches over entries (same rule as the trainer's `microbatches`)."""
    order = sorted(range(len(entries)), key=lambda k: (int(lengths[entries[k][0]]), entries[k][0], k))
    groups, current = [], []
    for k in order:
        width = max([int(lengths[entries[j][0]]) for j in current] + [int(lengths[entries[k][0]])])
        if current and width * (len(current) + 1) > budget:
            groups.append(current)
            current = []
        current.append(k)
    if current:
        groups.append(current)
    return groups


# ------------------------------------------------------------------ span tower

class SpanTower(nn.Module):
    def __init__(self, layers, norm):
        super().__init__()
        self.layers = nn.ModuleList(layers)
        self.norm = norm

    def forward(self, hidden, kwargs):
        for layer in self.layers:
            hidden = layer(hidden, **kwargs)[0]
        return self.norm(hidden)


class TowerFullA(full_a.FullA):
    """FullA whose token head reads a separate span tower (S2c); the document path is the frozen FullA."""

    def __init__(self, encoder, heads, share_mode="head", autocast_dtype=None, token_head=None, tower_layers=4):
        super().__init__(encoder, heads, None, share_mode, autocast_dtype, token_head)
        demand(token_head is not None, "the span tower feeds the token head")
        n = len(encoder.model.layers)
        demand(1 <= tower_layers < n, "tower depth")
        self.tower_layers = tower_layers
        self.span_tower = None
        self._captured = None
        self._hook = None

    def _capture(self, module, args, kwargs):
        self._captured = (args[0], dict(kwargs))

    def attach_tower(self):
        """Copy the top layers and the final norm from the (already loaded) encoder, then hook the input of the
        first copied layer. The hook is registered after copying so no copy carries it."""
        demand(self.span_tower is None and self._hook is None, "tower already attached")
        n = len(self.encoder.model.layers)
        layers = [copy.deepcopy(self.encoder.model.layers[i]) for i in range(n - self.tower_layers, n)]
        self.span_tower = SpanTower(layers, copy.deepcopy(self.encoder.model.final_norm))
        self.span_tower.requires_grad_(True)
        for layer in self.span_tower.layers:
            layer.gradient_checkpointing = False
        self._hook = self.encoder.model.layers[n - self.tower_layers].register_forward_pre_hook(
            self._capture, with_kwargs=True)

    def forward(self, ids, mask, content, autocast=True, words=None):
        demand(self.span_tower is not None, "tower not attached")
        head, self.token_head = self.token_head, None
        try:
            result = super().forward(ids, mask, content, autocast=autocast, words=words)
        finally:
            self.token_head = head
        hidden, kwargs = self._captured
        self._captured = None
        dtype = self.autocast_dtype if autocast else None
        if dtype is None:
            states = self.span_tower(hidden, kwargs)
        else:
            with torch.autocast(ids.device.type, dtype=dtype):
                states = self.span_tower(hidden, kwargs)
        result["span_logits"] = self.token_head(states.to(self.output_dtype))
        return result


def tower_parameter_group(model, learning_rate, weight_decay):
    decay, plain = [], []
    for p in model.span_tower.parameters():
        (decay if p.ndim >= 2 else plain).append(p)
    return [{"params": decay, "lr": learning_rate, "weight_decay": weight_decay, "name": f"span_tower.wd{weight_decay}"},
            {"params": plain, "lr": learning_rate, "weight_decay": 0., "name": "span_tower.wd0.0"}]


# ------------------------------------------------------------------ diagnostic subsets

def coarse_subset(meta, rows, seed):
    """Diagnostic Train subset over strata (cohort, generator label, origin), so every roster label keeps Train
    supervision; same allocation and order as full_a_bundle.stratified_subset. Returns {id: N_stratum / n_stratum}."""
    strata = {}
    for m in meta:
        strata.setdefault((m.cohort_id, bundle.label_key(m.label), m.origin), []).append(m.id)
    demand(type(rows) is int and len(strata) <= rows < len(meta), f"subset rows must lie in [{len(strata)}, {len(meta)})")
    total = len(meta)
    keys = sorted(strata, key=lambda k: tuple(map(str, k)))
    quota = {k: max(1., rows * len(strata[k]) / total) for k in keys}
    alloc = {k: min(len(strata[k]), int(quota[k])) for k in keys}
    left = rows - sum(alloc.values())
    order = sorted(keys, key=lambda k: (-(quota[k] - int(quota[k])), tuple(map(str, k))))
    while left > 0:
        moved = False
        for k in order:
            if left > 0 and alloc[k] < len(strata[k]):
                alloc[k] += 1
                left -= 1
                moved = True
        demand(moved, "subset allocation cannot place every row")
    while left < 0:
        k = max(keys, key=lambda k: (alloc[k], tuple(map(str, k))))
        demand(alloc[k] > 1, "subset allocation cannot honour one row per stratum")
        alloc[k] -= 1
        left += 1
    out = {}
    for k in keys:
        for x in sorted(strata[k], key=lambda x: bundle.subset_key(seed, x))[:alloc[k]]:
            out[x] = len(strata[k]) / alloc[k]
    return out


def close_pairs(keep, meta):
    """Add each kept spliced row's parent (factor 1) to a diagnostic subset."""
    by_id = {m.id: m for m in meta}
    out = dict(keep)
    for x in list(keep):
        m = by_id[x]
        if m.cohort_id == SPLICED_COHORT and m.pair_id in by_id and m.pair_id not in out:
            out[m.pair_id] = 1.
    return out


def dev_subset(root, rows):
    """Keyed diagnostic Dev-iter subset: rows/3 per origin class by ascending sha256(key + id)."""
    demand(type(rows) is int and rows >= 3 and rows % 3 == 0, "Dev-iter subset rows: a positive multiple of 3")
    by_origin = {0: [], 1: [], 2: []}
    for m, _ in bundle.stream(root, ["dev_iter"]):
        by_origin[m.origin].append(m.id)
    out = {}
    for ids in by_origin.values():
        ids.sort(key=lambda x: hashlib.sha256((DEV_SUBSET_KEY + x).encode()).hexdigest())
        demand(len(ids) >= rows // 3, "Dev-iter origin class smaller than the subset share")
        out.update({x: 1. for x in ids[:rows // 3]})
    return out

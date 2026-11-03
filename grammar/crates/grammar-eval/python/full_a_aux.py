"""Training-only auxiliary objectives for full A (never exported or replayed).

- Style adversary: gradient reversal on the shared encoding z, predicting the
  voice cluster id and the style axes of styled positive rows, with a
  scheduled reversal scale.
- Supervised contrastive loss on a projection of z: positives are documents
  of the same generator in a different voice; negatives are other generators
  in the same voice and humans.
- Voice-mixed batching within generators so logical batches contain both.

Neither head reads anything the replayed model needs. At initialization they
change no output (they only read z), so the step-0 function-preservation gate
is unaffected. The replayed model keeps the involvement, share and
attribution heads only.
"""
from __future__ import annotations

from collections import defaultdict, deque
from itertools import islice
import math
import random

import torch
from torch import nn
from torch.nn import functional as F


def demand(ok, message):
    if not ok:
        raise ValueError(message)


# ------------------------------------------------------------ gradient reversal

class _Reverse(torch.autograd.Function):
    @staticmethod
    def forward(ctx, x, scale):
        ctx.scale = scale
        return x.view_as(x)

    @staticmethod
    def backward(ctx, grad):
        return -ctx.scale * grad, None


def reverse_gradient(x, scale):
    """Identity forward; gradient multiplied by -scale backward."""
    demand(math.isfinite(scale) and scale >= 0, "reversal scale")
    return _Reverse.apply(x, float(scale))


REVERSAL_KINDS = ("constant", "linear_ramp", "dann")


def validate_reversal(s):
    demand(isinstance(s, dict) and s.get("kind") in REVERSAL_KINDS, "reversal schedule kind")
    demand(set(s) <= {"kind", "max", "ramp_epochs", "delay_epochs", "gamma"}, "reversal schedule fields")
    demand(isinstance(s.get("max"), (int, float)) and 0 <= s["max"] <= 10, "reversal max")
    demand(isinstance(s.get("delay_epochs", 0), (int, float)) and s.get("delay_epochs", 0) >= 0, "reversal delay")
    if s["kind"] != "constant":
        demand(isinstance(s.get("ramp_epochs"), (int, float)) and s["ramp_epochs"] > 0, "reversal ramp")
    if s["kind"] == "dann":
        demand(isinstance(s.get("gamma"), (int, float)) and s["gamma"] > 0, "dann gamma")
    return s


def reversal_scale(schedule, global_step, steps_per_epoch):
    """Reversal scale lambda at a global step (before the optimizer step).

    constant: max after the delay; linear_ramp: max * p; dann: max *
    (2 / (1 + exp(-gamma p)) - 1), where p = clamp((step - delay) / ramp) in
    epochs converted to steps. Zero during the delay.
    """
    delay = schedule.get("delay_epochs", 0) * steps_per_epoch
    if global_step < delay:
        return 0.
    if schedule["kind"] == "constant":
        return float(schedule["max"])
    p = min(1., (global_step - delay) / max(1., schedule["ramp_epochs"] * steps_per_epoch))
    if schedule["kind"] == "linear_ramp":
        return float(schedule["max"]) * p
    return float(schedule["max"]) * (2. / (1. + math.exp(-schedule["gamma"] * p)) - 1.)


# --------------------------------------------------------------- style adversary

class StyleAdversary(nn.Module):
    """Predicts voice cluster (softmax) and style axes (independent sigmoids)
    from a gradient-reversed copy of z. hidden 0 means linear heads."""

    def __init__(self, clusters, axes, hidden=256, dimension=768):
        super().__init__()
        demand(clusters >= 2 or axes >= 1, "adversary needs >=2 clusters or >=1 axis")
        self.trunk = nn.Sequential(nn.Linear(dimension, hidden), nn.GELU()) if hidden else nn.Identity()
        width = hidden or dimension
        self.cluster = nn.Linear(width, clusters) if clusters >= 2 else None
        self.axes = nn.Linear(width, axes) if axes >= 1 else None

    def forward(self, z, scale):
        u = self.trunk(reverse_gradient(z, scale))
        return (None if self.cluster is None else self.cluster(u),
                None if self.axes is None else self.axes(u))


def adversary_loss(adversary, z, scale, cluster_targets, axes_targets, weights, denominators):
    """Weighted CE over rows with a cluster target (>=0) plus weighted mean BCE
    over rows with axes targets (a 0/1 row, or None). Each term divides by its
    global supervised mass, like the main objective."""
    cluster_logits, axes_logits = adversary(z, scale)
    loss = z.sum() * 0
    if cluster_logits is not None:
        for i, (y, w) in enumerate(zip(cluster_targets, weights)):
            if y is not None and y >= 0:
                t = torch.tensor([y], dtype=torch.long, device=z.device)
                loss = loss + F.cross_entropy(cluster_logits[i:i + 1], t) * (w / denominators["cluster"])
    if axes_logits is not None:
        for i, (y, w) in enumerate(zip(axes_targets, weights)):
            if y is not None:
                t = torch.tensor(y, dtype=torch.float32, device=z.device)
                loss = loss + F.binary_cross_entropy_with_logits(axes_logits[i], t) * (w / denominators["axes"])
    return loss


# ------------------------------------------------------------ share-bin head

# Realised-share bins of mixed rows, as the evaluator reports them: (0,10),
# [10,25), [25,50), [50,75), [75,90), [90,100) and exactly 100 (full rewrite).
SHARE_BIN_EDGES = (.10, .25, .50, .75, .90, 1.)


def share_bin(share):
    """Bin index 0..6 of a mixed row's realised share in (0,1]."""
    demand(0 < share <= 1, "mixed share outside (0,1]")
    if share == 1:
        return len(SHARE_BIN_EDGES)
    return next(k for k, e in enumerate(SHARE_BIN_EDGES) if share < e)


class ShareBins(nn.Module):
    """Training-only realised-share-bin classifier on z (no reversal): it makes
    the shared encoding carry the share level the exported q head reads."""

    def __init__(self, dimension=768, hidden=256):
        super().__init__()
        bins = len(SHARE_BIN_EDGES) + 1
        self.net = nn.Sequential(nn.Linear(dimension, hidden), nn.GELU(), nn.Linear(hidden, bins)) if hidden \
            else nn.Linear(dimension, bins)

    def forward(self, z):
        return self.net(z)


def share_bin_loss(head, z, targets, weights, denominator):
    """Weighted CE over rows with a bin target (mixed rows), / global mixed mass."""
    loss = z.sum() * 0
    if denominator <= 0:
        return loss
    logits = head(z)
    for i, (y, w) in enumerate(zip(targets, weights)):
        if y is not None:
            t = torch.tensor([y], dtype=torch.long, device=z.device)
            loss = loss + F.cross_entropy(logits[i:i + 1], t) * (w / denominator)
    return loss


# -------------------------------------------------- supervised contrastive loss

class Projection(nn.Module):
    """Training-only projection of z for the contrastive loss, L2-normalized."""

    def __init__(self, dimension=768, hidden=512, out=128):
        super().__init__()
        self.net = nn.Sequential(nn.Linear(dimension, hidden), nn.GELU(), nn.Linear(hidden, out))

    def forward(self, z):
        return F.normalize(self.net(z), dim=-1)


SCAN = 64  # voices scanned per chosen generator when looking for shared voices
HUMAN = "\x00human"  # generator key for human-only rows: never an anchor, always a negative


def contrastive_sets(generators, voices):
    """Positive and negative masks for the generator/voice contrastive rule.

    generators: per row a label, or HUMAN for human-only rows (None: excluded).
    voices: per row a voice cluster id or None (unstyled).
    Anchor i (an AI row): P(i) = same generator, different voice (an
    unstyled row's voice differs from every styled voice and from other
    unstyled rows); N(i) = other generators in the same voice, plus humans.
    Same generator in the same voice is neither. A row with no positive or no
    negative is not an anchor.
    """
    n = len(generators)
    pos = torch.zeros((n, n), dtype=torch.bool)
    neg = torch.zeros((n, n), dtype=torch.bool)
    for i in range(n):
        g, v = generators[i], voices[i]
        if g is None or g == HUMAN:
            continue
        for j in range(n):
            if i == j or generators[j] is None:
                continue
            if generators[j] == HUMAN:
                neg[i, j] = True
            elif generators[j] == g:
                pos[i, j] = v is None or voices[j] is None or voices[j] != v
            elif v is not None and voices[j] == v:
                neg[i, j] = True
    return pos, neg


def supervised_contrastive(p, pos, neg, weights, temperature):
    """Weighted SupCon over anchors with >=1 positive and >=1 negative.

    For anchor i: -mean_{j in P(i)} log(exp(s_ij/t) / sum_{a in P(i)|N(i)} exp(s_ia/t)).
    Returns (loss, anchors). weights are per-row anchor weights.
    """
    demand(temperature > 0, "contrastive temperature")
    anchors = pos.any(1) & neg.any(1)
    if not bool(anchors.any()):
        return p.sum() * 0, 0
    s = p @ p.t() / temperature
    allowed = (pos | neg).to(s.device)
    logits = s.masked_fill(~allowed, float("-inf"))
    log_prob = logits - torch.logsumexp(logits, dim=1, keepdim=True)
    posd = pos.to(s.device)
    per = -(log_prob.masked_fill(~posd, 0.).sum(1) / posd.sum(1).clamp(min=1))
    a = anchors.to(s.device)
    w = torch.as_tensor(weights, dtype=s.dtype, device=s.device)
    w = torch.where(a, w, torch.zeros_like(w))
    demand(bool((w >= 0).all()) and float(w.sum()) > 0, "contrastive anchor weights")
    return (per.masked_fill(~a, 0.) * w).sum() / w.sum(), int(a.sum())


# ------------------------------------------------- voice-mixed batching

def voice_mixed_order(generators, voices, batch_size, per_generator, seed):
    """Epoch order whose consecutive logical batches mix voices within each
    generator and repeat voices across generators. Every row appears exactly
    once and every batch except the last has exactly batch_size rows, so the
    epoch objective is unchanged.

    generators: per row a label, HUMAN, or None (rows outside the structure).

    Stationarity: humans and other rows take their proportional share of each
    batch (cumulative rounding, never negative). The AI slots are dealt in
    chunks of `per_generator` rows by earliest deadline: the next chunk goes
    to the live generator whose chunk midpoint (taken + chunk/2) / total is
    smallest (seeded tie key), so every generator, small frontier labels
    included, is spread evenly through the epoch in proportion to its size.
    A generator may receive several chunks in one batch.

    Within a batch: voices shared by several generators of the batch are taken
    first, one row per generator per voice; then distinct unused voices; a
    generator with fewer voices than its count repeats a voice. Humans and
    other rows fill a batch only when no AI row is left.
    """
    demand(batch_size >= 2 and 1 <= per_generator <= batch_size, "voice-mixed batch shape")
    rng = random.Random(seed)
    n = len(generators)
    humans = [i for i in range(n) if generators[i] == HUMAN]
    other = [i for i in range(n) if generators[i] is None]
    rng.shuffle(humans)
    rng.shuffle(other)
    pools = defaultdict(dict)  # generator -> voice -> deque (seeded insertion order)
    unvoiced = 0
    for i in rng.sample(range(n), n):
        g = generators[i]
        if g is None or g == HUMAN:
            continue
        v = voices[i]
        if v is None:
            v = ("\x00unvoiced", unvoiced)  # every unstyled row is its own voice
            unvoiced += 1
        pools[g].setdefault(v, deque()).append(i)
    total = {g: sum(map(len, vs.values())) for g, vs in pools.items()}
    taken = {g: 0 for g in pools}
    tie = {g: rng.random() for g in sorted(pools)}
    ai_left = sum(total.values())
    batches = max(1, math.ceil(n / batch_size))
    human_per, other_per = len(humans) / batches, len(other) / batches
    order, taken_h, taken_o, b = [], 0, 0, 0

    def deal(slots):
        """Rows per generator for this batch by earliest-deadline chunks."""
        counts = {}
        while slots > 0:
            best, key = None, None
            for g in pools:
                left = total[g] - taken[g] - counts.get(g, 0)
                if left <= 0:
                    continue
                c = min(per_generator, left, slots)
                k = ((taken[g] + counts.get(g, 0) + c / 2) / total[g], tie[g])
                if key is None or k < key:
                    best, key, chunk = g, k, c
            if best is None:
                break
            counts[best] = counts.get(best, 0) + chunk
            slots -= chunk
        return counts

    def pick(counts):
        quota, used, picked = dict(counts), {g: set() for g in counts}, []

        def take(g, v):
            q = pools[g][v]
            picked.append(q.popleft())
            if not q:
                del pools[g][v]
            used[g].add(v)
            quota[g] -= 1
            taken[g] += 1

        chosen = sorted(counts, key=lambda g: tie[g])
        # Candidate shared voices: a bounded scan of each chosen pool (its
        # voice order is the seeded insertion order), then dict lookups.
        share = {}
        for g in chosen:
            for v in islice(pools[g], SCAN):
                if v not in share:
                    share[v] = [h for h in chosen if v in pools[h]]
        for v, gs in sorted(share.items(), key=lambda kv: (-len(kv[1]), rng.random())):
            if len(gs) < 2:
                continue
            for g in gs:
                if quota[g] and v not in used[g] and v in pools[g]:
                    take(g, v)
        for g in chosen:
            # Among the first quota+used voices at least `quota` are unused.
            for v in list(islice(pools[g], quota[g] + len(used[g]))):
                if not quota[g]:
                    break
                if v not in used[g]:
                    take(g, v)
            while quota[g]:  # fewer voices than the count: repeat a voice
                take(g, next(iter(pools[g])))
        return picked

    while len(order) < n:
        b += 1
        size = min(batch_size, n - len(order))
        want_h = max(0, min(len(humans), round(b * human_per)) - taken_h)
        want_o = max(0, min(len(other), round(b * other_per)) - taken_o)
        want_h = min(want_h, size)
        want_o = min(want_o, size - want_h)
        batch = humans[taken_h:taken_h + want_h] + other[taken_o:taken_o + want_o]
        taken_h += want_h
        taken_o += want_o
        slots = min(size - len(batch), ai_left)
        if slots:
            picked = pick(deal(slots))
            demand(len(picked) == slots, "voice-mixed AI deal")
            ai_left -= slots
            batch += picked
        # No AI row left: fill with the remaining humans, then others.
        while len(batch) < size and taken_h < len(humans):
            batch.append(humans[taken_h])
            taken_h += 1
        while len(batch) < size and taken_o < len(other):
            batch.append(other[taken_o])
            taken_o += 1
        demand(len(batch) == size, "voice-mixed batch size")
        rng.shuffle(batch)
        order += batch
    demand(len(order) == n and sorted(order) == list(range(n)), "voice-mixed order must be a permutation")
    return order


def batch_structure(order, generators, voices, batch_size):
    """Fraction of AI rows that are contrastive anchors in their logical batch."""
    anchors = ai = 0
    for s in range(0, len(order), batch_size):
        idx = order[s:s + batch_size]
        pos, neg = contrastive_sets([generators[i] for i in idx], [voices[i] for i in idx])
        anchors += int((pos.any(1) & neg.any(1)).sum())
        ai += sum(generators[i] not in (None, HUMAN) for i in idx)
    return {"ai_rows": ai, "anchor_rows": anchors, "anchor_fraction": anchors / ai if ai else None}

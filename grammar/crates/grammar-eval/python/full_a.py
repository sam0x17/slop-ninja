"""Full-encoder V6 A (E1) with an optional token-span involvement head (E2)
and an optional span-localization token head (spans-v1).

Only the Torch model boundary lives here: tokenization offsets, token labels
from Rust-supplied exact replacement byte spans, the model, its masked loss and
the frozen involvement-calibration replay used for reference outputs. Corpus
admission, splits, normalization fitting, calibration fitting and statistics
stay in Rust. No truncation; 2,048 tokens including specials at most.

Function preservation: with the span head absent, or present with gate 0, the
initial origin probabilities are exactly the shared stage-3 heads on the V6
classifier input, so the step-0 model reproduces V6 within float tolerance.

The spans-v1 token head (TokenHead) is separate from the E2 span head: it reads
the final-norm token states and never feeds the document involvement logit, so
the document heads keep their exact meaning. Its per-token AI logits are
decoded to words, sentences and spans by full_a_spans (reference) and Rust.
"""
from __future__ import annotations

import math

import torch
from torch import nn
from torch.nn import functional as F


MAX_TOKENS = 2048
CFG_ZEROS = 1820
SPAN_LABEL_POLICY = "slop_ninja_token_span_label_v1"
POOLINGS = ("attention", "max")
# token_mean_projected (v1, legacy): mean over every content token, including
# whitespace-only tokens. word_token_mean_projected (v2): mean over content
# tokens with at least one non-whitespace character (WORD_TOKEN_RULE).
SHARE_MODES = ("head", "token_mean_projected", "word_token_mean_projected")
WORD_TOKEN_RULE = "slop_ninja_word_token_mask_v1"
HEADS_SCHEMA = "slop_ninja_full_a_heads_v2"
TOKEN_HEAD_RULE = "slop_ninja_token_head_v1:second(gelu_tanh(first(final_norm_token_state)))"
TOKEN_HEAD_ACTIVATION = "gelu_tanh"
FREEZE_MODES = ("none", "encoder_and_document_heads")
CALIBRATION_METHODS = ("identity_v1", "involvement_temperature_v1", "isotonic_v1", "isotonic_bounded_v2",
                       "origin_temperature_v1", "isotonic_v1_clipped")
# Protocol v1.2 maps read the origin logits (temperature first), not only g.
ORIGIN_CALIBRATION_METHODS = ("origin_temperature_v1", "isotonic_v1_clipped")
CLIPPED_RULE = ("lo=1/(2*CAL human documents), hi=1-1/(2*CAL positive documents); "
                "every PAV level and every output clamped to [lo,hi]")
BOUNDED_RULE = ("epsilon=1/(2*(min(human_rows,positive_rows)+1)) over the fitting rows; "
                "every PAV level clamped to [epsilon,1-epsilon]")


def demand(ok, message):
    if not ok:
        raise ValueError(message)


# ---------------------------------------------------------------- token labels

def byte_starts(text):
    """UTF-8 byte offset of every character boundary, len(text)+1 entries."""
    starts, total = [0], 0
    for c in text:
        total += len(c.encode("utf-8"))
        starts.append(total)
    return starts


def validate_spans(text, spans):
    """Exact [start,end) UTF-8 byte spans on character boundaries, sorted, disjoint."""
    starts = byte_starts(text)
    boundaries = set(starts)
    demand(isinstance(spans, list), "ai_byte_spans must be a list")
    previous = 0
    for span in spans:
        demand(isinstance(span, list) and len(span) == 2 and all(type(v) is int for v in span),
               "span must be [start,end] integers")
        s, e = span
        demand(previous <= s < e <= starts[-1], "spans must be sorted, disjoint, nonempty and in bounds")
        demand(s in boundaries and e in boundaries, "span splits a UTF-8 character")
        previous = e
    return starts


def char_inside(text, spans):
    """Per character: True when its UTF-8 bytes lie inside a replacement span."""
    starts = validate_spans(text, spans)
    inside, j = [], 0
    for i in range(len(text)):
        while j < len(spans) and spans[j][1] <= starts[i]:
            j += 1
        inside.append(j < len(spans) and spans[j][0] <= starts[i] and starts[i + 1] <= spans[j][1])
    return inside


def token_labels(text, ids, offsets, special_ids, origin, spans):
    """Policy slop_ninja_token_span_label_v1; -1 means no token supervision.

    A token is labelled from its non-whitespace characters only: all inside
    model-produced replacement spans -> 1, all outside -> 0, straddling -> -1.
    Special tokens and whitespace-only tokens are -1. Human-only rows are all
    0 and model-only rows all 1 when spans are absent; supplied spans must agree.
    Mixed rows without measured spans carry no token supervision.
    """
    demand(len(ids) == len(offsets), "token/offset length mismatch")
    demand(origin in (None, 0, 1, 2), "origin target")
    if origin is None or (origin == 2 and spans is None):
        return [-1] * len(ids)
    content = [c for c in range(len(text)) if not text[c].isspace()]
    if spans is None:
        inside = [origin == 1] * len(text)
    else:
        inside = char_inside(text, spans)
        covered = sum(inside[c] for c in content)
        if origin == 0:
            demand(covered == 0, "human-only row cannot carry model spans")
        elif origin == 1:
            demand(covered == len(content), "model-only spans must cover all content")
        else:
            demand(covered > 0, "mixed row spans cover no content")
    labels = []
    for token, (a, b) in zip(ids, offsets):
        demand(0 <= a <= b <= len(text), "token offset out of range")
        chars = [c for c in range(a, b) if not text[c].isspace()]
        if token in special_ids or not chars:
            labels.append(-1)
            continue
        n = sum(inside[c] for c in chars)
        labels.append(1 if n == len(chars) else 0 if n == 0 else -1)
    return labels


def hf_tokenize(tokenizer):
    """Exact-text tokenization with character offsets; never truncates or pads."""
    def tokenize(text):
        demand(isinstance(text, str) and text.strip(), "empty input unsupported")
        enc = tokenizer(text, add_special_tokens=True, truncation=False, padding=False,
                        return_offsets_mapping=True)
        ids, offsets = enc["input_ids"], [tuple(o) for o in enc["offset_mapping"]]
        demand(0 < len(ids) <= MAX_TOKENS, "overlength input unsupported; no truncation")
        return ids, offsets
    return tokenize


def is_space(c):
    """Python str.isspace: Unicode White_Space plus U+001C..U+001F (Rust mirrors this)."""
    return c.isspace()


def word_mask_for(text, ids, offsets, special_ids):
    """slop_ninja_word_token_mask_v1: content positions (CLS/SEP frame excluded,
    positional rule) whose character offsets cover at least one non-whitespace
    character of the exact text. Whitespace-only tokens such as newline runs are
    excluded; they carry no token label and are not words of the share target."""
    content = content_mask_for(ids, special_ids)
    demand(len(offsets) == len(ids), "token/offset length mismatch")
    out = []
    for keep, (a, b) in zip(content, offsets):
        demand(0 <= a <= b <= len(text), "token offset out of range")
        out.append(keep and any(not is_space(text[c]) for c in range(a, b)))
    demand(any(out), "no word-bearing content tokens")
    return out


def content_mask_for(ids, special_ids):
    """Pooling covers every position except the CLS/SEP frame (positional rule,
    identical in Rust even if literal special-token text occurs inside)."""
    demand(len(ids) >= 3 and ids[0] in special_ids and ids[-1] in special_ids,
           "exactly CLS ... SEP framing required")
    return [False] + [True] * (len(ids) - 2) + [False]


# ----------------------------------------------------------------------- model

class SpanHead(nn.Module):
    """Linear token involvement logits plus masked attention or max pooling.

    gate starts at zero so the document involvement is unchanged at init.
    """

    def __init__(self, pooling, hidden=768):
        super().__init__()
        demand(pooling in POOLINGS, "unsupported pooling")
        self.pooling = pooling
        self.token = nn.Linear(hidden, 1)
        self.attention = nn.Linear(hidden, 1) if pooling == "attention" else None
        self.gate = nn.Parameter(torch.zeros(()))
        with torch.no_grad():
            nn.init.normal_(self.token.weight, std=.02)
            self.token.bias.zero_()
            if self.attention is not None:
                self.attention.weight.zero_()
                self.attention.bias.zero_()

    def forward(self, hidden, content, share=None):
        """Pooling over content; the token mean over `share` (default content)."""
        demand(hidden.ndim == 3 and content.shape == hidden.shape[:2] and content.dtype == torch.bool,
               "token states and content mask")
        demand(bool(content.any(-1).all()), "every document needs content tokens")
        if share is None:
            share = content
        demand(share.shape == content.shape and share.dtype == torch.bool and not bool((share & ~content).any()) and
               bool(share.any(-1).all()), "share mask must be a nonempty subset of content")
        t = self.token(hidden).squeeze(-1)
        if self.attention is not None:
            a = self.attention(hidden).squeeze(-1).masked_fill(~content, float("-inf"))
            pooled = (a.softmax(-1) * t.masked_fill(~content, 0.)).sum(-1)
        else:
            pooled = t.masked_fill(~content, float("-inf")).amax(-1)
        m = share.to(t.dtype)
        mean = (t.sigmoid() * m).sum(-1) / m.sum(-1)
        return t, pooled, mean


class TokenHead(nn.Module):
    """spans-v1 token AI logit t = w2 . gelu_tanh(W1 x + b1) + b2 on final-norm
    token states x. W1, w2 ~ N(0, 0.02^2), biases 0. The tanh GELU form is
    computed identically in f64 by Rust. Never feeds the document heads."""

    def __init__(self, hidden=768, width=256):
        super().__init__()
        demand(type(width) is int and 1 <= width <= 4096, "token head width")
        self.width = width
        self.first = nn.Linear(hidden, width)
        self.second = nn.Linear(width, 1)
        with torch.no_grad():
            nn.init.normal_(self.first.weight, std=.02)
            nn.init.normal_(self.second.weight, std=.02)
            self.first.bias.zero_()
            self.second.bias.zero_()

    def forward(self, hidden):
        return self.second(F.gelu(self.first(hidden), approximate="tanh")).squeeze(-1)


def combine(origin_logits, gate=None, pooled=None):
    """Final log origin and involvement logit from base origin logits.

    g0 = log(p_model + p_mixed) - log(p_human). With token evidence,
    g = g0 + gate * pooled; P(involved) = sigmoid(g) and the model-only/mixed
    ratio stays the origin head's. gate absent means exactly log_softmax.
    """
    lp = origin_logits.log_softmax(-1)
    ai = torch.logsumexp(lp[:, 1:], -1)
    g = ai - lp[:, 0]
    if gate is None:
        return lp, g
    g = g + gate * pooled
    conditional = lp[:, 1:] - ai.unsqueeze(-1)
    log_origin = torch.cat((F.logsigmoid(-g).unsqueeze(-1), F.logsigmoid(g).unsqueeze(-1) + conditional), -1)
    return log_origin, g


def projected_share(origin, mean):
    """Euclidean projection of the token mean onto [P(model_only), P(involvement)]."""
    lo = origin[:, 1]
    return torch.minimum(torch.maximum(mean, lo), origin[:, 1] + origin[:, 2])


class FullA(nn.Module):
    """Full ModernBERT encoder plus the unchanged stage-3 shared heads.

    The V6 classifier stays frozen and is used only for the initial check.
    """

    def __init__(self, encoder, heads, span=None, share_mode="head", autocast_dtype=None, token_head=None):
        super().__init__()
        demand(share_mode in SHARE_MODES, "unsupported share mode")
        demand(share_mode == "head" or span is not None, "token share needs the span head")
        demand(autocast_dtype in (None, torch.bfloat16), "bf16 autocast or fp32 only")
        self.encoder, self.heads, self.span = encoder, heads, span
        self.token_head = token_head
        self.share_mode, self.autocast_dtype = share_mode, autocast_dtype
        self.frozen_document = False
        self.token_head_weight = 0.
        # Head and token-state precision; float64 only for export reference replay.
        self.output_dtype = torch.float32
        encoder.requires_grad_(True)
        encoder.classifier.requires_grad_(False)
        if share_mode != "head":
            # The conditional share head is unused when share comes from tokens.
            heads.mixed_share.requires_grad_(False)

    def freeze_document(self):
        """spans-v1 S0 arms: the encoder, document heads and any E2 span head stay
        at their initial values; only the token head trains. Their modules run in
        eval mode even while training, so document outputs are unchanged."""
        demand(self.token_head is not None, "freezing the document model needs a token head")
        self.encoder.requires_grad_(False)
        self.heads.requires_grad_(False)
        if self.span is not None:
            self.span.requires_grad_(False)
        self.frozen_document = True

    def train(self, mode=True):
        super().train(mode)
        if self.frozen_document:
            self.encoder.eval()
            self.heads.eval()
            if self.span is not None:
                self.span.eval()
        return self

    def encode(self, ids, mask):
        out = self.encoder.model(input_ids=ids, attention_mask=mask, return_dict=True)
        hidden = out.last_hidden_state
        m = mask.unsqueeze(-1).to(hidden.dtype)
        # Same sum/divide order as the frozen V6 classifier input.
        pooled = (hidden * m).sum(1) / m.sum(1)
        return hidden, self.encoder.drop(self.encoder.head(pooled))

    def forward(self, ids, mask, content, autocast=True, words=None):
        """autocast=False forces FP32 (evaluation, export, the initial gate).

        words is the word-token mask; required for word_token_mean_projected."""
        demand(self.share_mode != "word_token_mean_projected" or words is not None,
               "word-token share needs the word mask")
        dtype = self.autocast_dtype if autocast else None
        with torch.set_grad_enabled(torch.is_grad_enabled() and not self.frozen_document):
            if dtype is None:
                hidden, h = self.encode(ids, mask)
            else:
                with torch.autocast(ids.device.type, dtype=dtype):
                    hidden, h = self.encode(ids, mask)
        hidden, h = hidden.to(self.output_dtype), h.to(self.output_dtype)
        out = self.heads(torch.cat((h, h.new_zeros((h.shape[0], CFG_ZEROS))), 1))
        result = {"h": h, "encoding": out["encoding"], "origin_logits": out["origin_logits"],
                  "human_logits": out["human_logits"], "ai_logits": out["ai_logits"],
                  "conditional_mixed_share": out["conditional_mixed_share"]}
        if self.span is None:
            log_origin, g = combine(out["origin_logits"])
            result.update(token_logits=None, token_mean=None, pooled=None)
        else:
            c = content & mask.bool()
            share_mask = (words & c) if self.share_mode == "word_token_mean_projected" else None
            t, pooled, mean = self.span(hidden, c, share_mask)
            log_origin, g = combine(out["origin_logits"], self.span.gate, pooled)
            result.update(token_logits=t, token_mean=mean, pooled=pooled)
        origin = log_origin.exp()
        if self.share_mode == "head":
            share = origin[:, 1] + origin[:, 2] * out["conditional_mixed_share"]
        else:
            share = projected_share(origin, result["token_mean"])
        result.update(log_origin=log_origin, origin=origin, involvement_logit=g, share=share)
        # spans-v1 token head: FP32 on the FP32 final-norm token states (outside autocast).
        result["span_logits"] = None if self.token_head is None else self.token_head(hidden)
        return result

    def trainable_encoder_names(self):
        return [n for n, p in self.encoder.named_parameters() if p.requires_grad]


# ----------------------------------------------------------------- optimizer

def layer_of(name, layers):
    """0 embeddings, 1..L blocks, L+1 final norm/prediction head."""
    if name.startswith("model.embeddings."):
        return 0
    if name.startswith("model.layers."):
        return int(name.split(".")[2]) + 1
    demand(name.startswith(("model.final_norm.", "head.")), "unexpected trainable encoder tensor " + name)
    return layers + 1


def parameter_groups(model, encoder_lr, layer_decay, heads_lr, weight_decay, token_head_lr=None):
    """AdamW groups. LLRD: depth d gets encoder_lr * layer_decay**(L+1-d).

    Matrices decay; norm weights, biases and the scalar gate do not. The
    spans-v1 token head has its own learning rate (matrices decay, biases do
    not). Frozen tensors (requires_grad False) are in no group.
    """
    demand(0 < layer_decay <= 1 and encoder_lr > 0 and heads_lr > 0 and weight_decay >= 0, "optimizer")
    layers = len(model.encoder.model.layers)
    groups = {}
    for name, p in model.encoder.named_parameters():
        if not p.requires_grad:
            continue
        depth = layer_of(name, layers)
        decay = weight_decay if p.ndim >= 2 else 0.
        key = ("encoder", depth, decay)
        groups.setdefault(key, {"params": [], "lr": encoder_lr * layer_decay ** (layers + 1 - depth),
                                "weight_decay": decay, "name": f"encoder.depth{depth}.wd{decay}"})["params"].append(p)
    heads = [(n, p) for n, p in model.heads.named_parameters() if p.requires_grad]
    if model.span is not None:
        heads += [("span." + n, p) for n, p in model.span.named_parameters()]
    for name, p in heads:
        # Stage-3 heads decayed every tensor; keep that, but never the scalar gate.
        decay = 0. if name == "span.gate" else weight_decay
        key = ("heads", decay)
        groups.setdefault(key, {"params": [], "lr": heads_lr, "weight_decay": decay,
                                "name": f"heads.wd{decay}"})["params"].append(p)
    if model.token_head is not None:
        demand(token_head_lr is not None and token_head_lr > 0, "token head learning rate")
        for name, p in model.token_head.named_parameters():
            decay = weight_decay if p.ndim >= 2 else 0.
            key = ("token_head", decay)
            groups.setdefault(key, {"params": [], "lr": token_head_lr, "weight_decay": decay,
                                    "name": f"token_head.wd{decay}"})["params"].append(p)
    return list(groups.values())


def warmup_linear(total_steps, warmup_steps):
    demand(total_steps > 0 and 0 <= warmup_steps < total_steps, "schedule steps")
    def factor(step):
        if step < warmup_steps:
            return (step + 1) / warmup_steps
        return max(0., (total_steps - step) / max(1, total_steps - warmup_steps))
    return factor


DECAY_SHAPES = ("linear", "cosine")


def warmup_horizon(horizon_steps, warmup_steps, shape, floor=0.):
    """Linear warmup, then linear or cosine decay over a horizon of H steps.

    The factor reaches `floor` at step H (the end of the decay horizon) and
    stays there, so a run may continue past H at the floor rate (or stop early
    before H). With H equal to the total scheduled steps and floor 0, the
    linear shape is exactly warmup_linear.
    """
    demand(shape in DECAY_SHAPES and 0. <= floor < 1., "decay shape/floor")
    demand(horizon_steps > 0 and 0 <= warmup_steps < horizon_steps, "schedule steps")
    span = max(1, horizon_steps - warmup_steps)
    def factor(step):
        if step < warmup_steps:
            return (step + 1) / warmup_steps
        progress = min(1., max(0., (step - warmup_steps) / span))
        if shape == "linear":
            remaining = max(0., (horizon_steps - step) / span)
        else:
            remaining = .5 * (1. + math.cos(math.pi * progress))
        return floor + (1. - floor) * remaining
    return factor


# ---------------------------------------------------------------------- loss

CONDITIONAL_SHARE_EPS = 1e-6


def conditional_share(out, share_mode):
    """The share estimate given that the document is mixed: the mixed-share
    head q in head mode, the (word-)token mean in the token modes."""
    return out["conditional_mixed_share"] if share_mode == "head" else out["token_mean"]


def objective(out, targets, weights, denominators, token_labels=None, token_weight=0., role_weight=.25,
              conditional_weight=0., share_mode="head", parts=None, token_head_weight=0., token_weights=None,
              document=True):
    """Stage-3 joint loss on the final origin, plus optional token BCE and an
    optional conditional share term.

    role_weight scales the conditional role (attribution) cross-entropy; the
    stage-3 value is 0.25.

    conditional_weight scales a BCE between the conditional share (see
    conditional_share) and the realised share on mixed rows only, divided by
    the mixed supervised mass. The document share MSE reaches q only through
    P(mixed), so a model unsure between model-only and mixed barely learns q;
    this term supervises it directly, including full rewrites (mixed, share 1).

    Each task term is weight/global denominator, so microbatch boundaries never
    change the epoch objective. Token BCE is the per-document mean over labelled
    tokens, weighted like the other tasks (denominator = token-supervised mass).

    parts (optional dict) accumulates each term's weighted value (floats, as
    included in the loss): origin, share, conditional_share, attribution
    (human and AI roles), token, token_head.

    spans-v1 token head (token_head_weight > 0): BCE on labelled tokens
    (slop_ninja_token_span_label_v1), per-document mean, times
    token_head_weight x the row's token weight / the token-supervised token
    weight mass (denominators["token_head"]). document=False (frozen document
    model) omits every document term.
    """
    device = out["origin"].device
    loss = out["encoding"].sum() * 0
    if token_head_weight and out.get("span_logits") is not None and token_labels is not None:
        demand(token_weights is not None and len(token_weights) == len(targets), "token weights per row")
        loss = loss + out["span_logits"].sum() * 0
        for i, tw in enumerate(token_weights):
            labels = token_labels[i]
            keep = labels >= 0
            if tw > 0 and bool(keep.any()):
                bce = F.binary_cross_entropy_with_logits(out["span_logits"][i][keep], labels[keep].float())
                term = token_head_weight * bce * (tw / denominators["token_head"])
                _add(parts, "token_head", term)
                loss = loss + term
    if not document:
        return loss
    for i, (t, w) in enumerate(zip(targets, weights)):
        y = t.get("origin")
        if y is not None:
            target = torch.tensor([y], dtype=torch.long, device=device)
            value = F.nll_loss(out["log_origin"][i:i + 1], target) + \
                (out["origin"][i, 1:].sum() - float(y != 0)).square()
            term = value * (w / denominators["origin"])
            _add(parts, "origin", term)
            loss = loss + term
        y = t.get("share")
        if y is not None:
            term = (out["share"][i] - y).square() * (w / denominators["share"])
            _add(parts, "share", term)
            loss = loss + term
            if conditional_weight and t.get("origin") == 2:
                q = conditional_share(out, share_mode)[i].clamp(CONDITIONAL_SHARE_EPS, 1. - CONDITIONAL_SHARE_EPS)
                target = q.new_tensor(float(y))
                term = conditional_weight * F.binary_cross_entropy(q, target) * \
                    (w / denominators["conditional_share"])
                _add(parts, "conditional_share", term)
                loss = loss + term
        for role in ("human", "ai"):
            y = t.get(role)
            if y is not None:
                target = torch.tensor([y], dtype=torch.long, device=device)
                term = role_weight * F.cross_entropy(out[f"{role}_logits"][i:i + 1], target) * \
                    (w / denominators[role])
                _add(parts, "attribution", term)
                loss = loss + term
        if token_labels is not None and token_weight and out["token_logits"] is not None:
            labels = token_labels[i]
            keep = labels >= 0
            if bool(keep.any()):
                bce = F.binary_cross_entropy_with_logits(out["token_logits"][i][keep], labels[keep].float())
                term = token_weight * bce * (w / denominators["token"])
                _add(parts, "token", term)
                loss = loss + term
    return loss


def _add(parts, key, term):
    if parts is not None:
        parts[key] = parts.get(key, 0.) + float(term.detach())


def involvement_brier(out, targets, weights, denominator):
    total = 0.
    for i, (t, w) in enumerate(zip(targets, weights)):
        y = t.get("origin")
        if y is not None:
            total += w / denominator * float((out["origin"][i, 1:].sum() - float(y != 0)).square())
    return total


# ---------------------------------------------------------- calibration replay

def apply_calibration(description, involvement_logit):
    """Replay a frozen Rust CAL map on one uncalibrated involvement logit.

    The fitted maps are produced in Rust (involvement_calibration.rs); this is
    only the reference replay for exports and must match it to float tolerance.
    """
    method = description["method"]
    demand(method in CALIBRATION_METHODS, "unsupported calibration method")
    demand(method not in ORIGIN_CALIBRATION_METHODS, "this map needs the origin logits (apply_origin_calibration)")
    g = float(involvement_logit)
    demand(math.isfinite(g), "nonfinite involvement logit")
    if method == "identity_v1":
        return 1. / (1. + math.exp(-g)) if g >= 0 else math.exp(g) / (1. + math.exp(g))
    if method == "involvement_temperature_v1":
        t = description["temperature"]
        demand(isinstance(t, (int, float)) and t > 0 and math.isfinite(t), "temperature")
        g = g / t
        return 1. / (1. + math.exp(-g)) if g >= 0 else math.exp(g) / (1. + math.exp(g))
    knots = description["knots"]
    lo, hi = 0., 1.
    if method == "isotonic_bounded_v2":
        e = description["epsilon"]
        demand(description["bound"] == BOUNDED_RULE and isinstance(e, float) and 0 < e < .5 and
               all(e <= k["involvement"] <= 1 - e for k in knots), "bounded isotonic levels")
        lo, hi = e, 1. - e
    x = 1. / (1. + math.exp(-g)) if g >= 0 else math.exp(g) / (1. + math.exp(g))
    xs, ys = [k["score"] for k in knots], [k["involvement"] for k in knots]
    demand(xs and all(a < b for a, b in zip(xs, xs[1:])) and all(a <= b for a, b in zip(ys, ys[1:])),
           "isotonic knots must be sorted and monotone")
    if x <= xs[0]:
        return min(hi, max(lo, ys[0]))
    if x >= xs[-1]:
        return min(hi, max(lo, ys[-1]))
    a, b = 0, len(xs) - 1
    while b - a > 1:
        mid = (a + b) // 2
        if xs[mid] <= x:
            a = mid
        else:
            b = mid
    return min(hi, max(lo, ys[a] + (ys[b] - ys[a]) * ((x - xs[a]) / (xs[b] - xs[a]))))


def _sigmoid(g):
    return 1. / (1. + math.exp(-g)) if g >= 0 else math.exp(g) / (1. + math.exp(g))


def _lse2(a, b):
    m = max(a, b)
    return m + math.log(math.exp(a - m) + math.exp(b - m))


def _isotonic(knots, x, lo, hi):
    xs, ys = [k["score"] for k in knots], [k["involvement"] for k in knots]
    demand(xs and all(a < b for a, b in zip(xs, xs[1:])) and all(a <= b for a, b in zip(ys, ys[1:])),
           "isotonic knots must be sorted and monotone")
    if x <= xs[0]:
        return min(hi, max(lo, ys[0]))
    if x >= xs[-1]:
        return min(hi, max(lo, ys[-1]))
    a, b = 0, len(xs) - 1
    while b - a > 1:
        mid = (a + b) // 2
        if xs[mid] <= x:
            a = mid
        else:
            b = mid
    return min(hi, max(lo, ys[a] + (ys[b] - ys[a]) * ((x - xs[a]) / (xs[b] - xs[a]))))


def apply_origin_calibration(description, origin_logits, involvement_logit):
    """Replay a frozen protocol v1.2 map: (calibrated involvement p, model-only
    share r of the AI mass), both from the temperature-scaled origin logits.

    T scales the three origin logits; g_T = logsumexp(l1/T, l2/T) - l0/T plus
    (g - (logsumexp(l1, l2) - l0))/T, the span-gate term of E2 (0 for E1).
    origin_temperature_v1 returns sigmoid(g_T); isotonic_v1_clipped the clipped
    isotonic map of sigmoid(g_T). Must match involvement_calibration.rs.
    """
    method = description["method"]
    demand(method in ORIGIN_CALIBRATION_METHODS, "not a protocol v1.2 origin map")
    t = description["temperature"]
    demand(isinstance(t, (int, float)) and math.isfinite(t) and t > 0, "temperature")
    logits = [float(v) for v in origin_logits]
    g = float(involvement_logit)
    demand(len(logits) == 3 and all(math.isfinite(v) for v in logits) and math.isfinite(g), "origin logits")
    base = _lse2(logits[1], logits[2]) - logits[0]
    s = [v / t for v in logits]
    gt = _lse2(s[1], s[2]) - s[0] + (g - base) / t
    m = max(s[1], s[2])
    a, b = math.exp(s[1] - m), math.exp(s[2] - m)
    r = a / (a + b)
    demand(math.isfinite(gt) and math.isfinite(r), "nonfinite scaled involvement")
    x = _sigmoid(gt)
    if method == "origin_temperature_v1":
        return x, r
    lo, hi = description["lo"], description["hi"]
    demand(description["bound"] == CLIPPED_RULE and isinstance(lo, float) and isinstance(hi, float) and
           0 < lo <= .5 <= hi < 1 and all(lo <= k["involvement"] <= hi for k in description["knots"]),
           "clipped isotonic levels")
    return _isotonic(description["knots"], x, lo, hi), r


def calibrated_outputs(raw, description, share_mode):
    """Document outputs from raw heads and a frozen involvement calibration.

    raw has origin_logits, involvement_logit and conditional_mixed_share or
    token_mean (the arm's token share rule). The model-only/mixed split is the origin head's ratio, so share
    stays within [P(model_only), P(involvement)] after calibration as well.
    """
    logits = [float(v) for v in raw["origin_logits"]]
    if description["method"] in ORIGIN_CALIBRATION_METHODS:
        p, r = apply_origin_calibration(description, logits, raw["involvement_logit"])
    else:
        a, b = logits[1], logits[2]
        m = max(a, b)
        r = math.exp(a - m) / (math.exp(a - m) + math.exp(b - m))
        p = apply_calibration(description, raw["involvement_logit"])
    origin = [1. - p, p * r, p * (1. - r)]
    if share_mode == "head":
        q = float(raw["conditional_mixed_share"])
        share = min(max(origin[1] + origin[2] * q, origin[1]), p)
    else:
        share = min(max(float(raw["token_mean"]), origin[1]), p)
    demand(-1e-12 <= origin[1] <= share <= p + 1e-12 <= 1 + 1e-12, "output bounds")
    return {"origin": dict(zip(("human_only", "model_only", "mixed"), origin)),
            "ai_confidence_percent": 100 * p, "ai_share_percent": 100 * share,
            "human_involvement_probability": origin[0] + origin[2], "ai_involvement_probability": p}

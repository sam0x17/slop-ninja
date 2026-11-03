"""Python reference for A span localization (protocol spans-v1, Section 4.2).

Pure-Python float64 reference of the emission rule that the Rust runtime
(`grammar-a-baseline/src/full_a_spans.rs`) implements: words, the sealed
sentence splitter, token-to-word grouping, word probabilities, the share tilt,
the hysteresis span decoder and the sentence/span confidence maps. The export
uses it for reference predictions and golden vectors; decoding and metrics for
selection run in Rust. No model, tokenizer, corpus or network access here.

Offsets are into the exact request text: `*_char` are Unicode scalar indices,
`*_byte` UTF-8 byte offsets, both half-open [start, end).

Rules (identifiers are bound into exports and receipts):
- Words: maximal runs of characters that are not Python `str.isspace`
  (Unicode White_Space plus U+001C..U+001F); the words of the share target.
- Word probability: mean of sigmoid(token logit) over the word-bearing tokens
  (`slop_ninja_word_token_mask_v1`) whose non-whitespace characters overlap
  the word. Every word must have one; otherwise span emission is unsupported.
- Share tilt: b with mean_w sigmoid(logit(p_w) + b) = s, s the emitted
  document share clamped to [1e-6, 1-1e-6], p_w clamped to [1e-9, 1-1e-9];
  bisection on [-40, 40], 200 iterations; q_w = sigmoid(logit(p_w) + b).
- Sentences: the sealed `a_comp_sentences_v1` splitter (Rust
  `char::is_whitespace`, i.e. Python isspace without U+001C..U+001F). A word
  belongs to the sentence that contains it; sentences without words are not
  emitted.
- Sentence share = mean q_w over its words; sentence confidence =
  min(map_sentence(max q_w), document confidence).
- Spans: decoder `slop_ninja_span_decoder_v1` on q_w; span share = mean q_w
  over its words; span confidence = min(map_span(mean q_w), document
  confidence). Offsets run from the first character of the first word to the
  end of the last word.
Sums are sequential left-to-right float64 additions (as in Rust); Python's
compensated `sum()` is never used for emitted values.
"""
from __future__ import annotations

import bisect
import json
import math


WORD_RULE = "slop_ninja_share_words_v1:maximal_runs_of_non_python_isspace"
WORD_PROBABILITY_RULE = "slop_ninja_word_probability_v1:mean_sigmoid_token_logit_over_overlapping_word_tokens"
TILT_RULE = ("slop_ninja_share_tilt_v1:mean_w_sigmoid(logit(clamp(p_w,1e-9))+b)=clamp(s,1e-6);"
             "bisection_b_in_[-40,40]_200_iterations")
SPLITTER_ID = "a_comp_sentences_v1:terminal_punct_closers_then_whitespace_or_blank_line_or_end"
DECODER_RULE = "slop_ninja_span_decoder_v1"
EMISSION_RULE = "slop_ninja_span_emission_v1"
SCORE_MAP_METHODS = ("identity_v1", "isotonic_score_v1_clipped")
SCORE_MAP_BOUND = "every PAV level and every output clamped to [lo,hi]"
P_CLAMP, S_CLAMP, TILT_BOUND, TILT_ITERATIONS = 1e-9, 1e-6, 40., 200
THRESHOLD_MARGIN = 1e-6
# Uncalibrated diagnostic exports only; the protocol decoder is chosen on Dev-iter.
DIAGNOSTIC_DECODER = {"rule": DECODER_RULE, "tau_hi": .5, "delta": 0., "min_words": 1, "gap": 0, "snap": None}
TERMINAL = frozenset(".!?…")
CLOSERS = frozenset("\"')]”’»}")


def demand(ok, message):
    if not ok:
        raise ValueError(message)


def is_space(c):
    """Python str.isspace (the share-word rule; Rust full_a_spans::is_space)."""
    return c.isspace()


def is_rust_whitespace(c):
    """Rust char::is_whitespace (Unicode White_Space): Python isspace minus U+001C..U+001F."""
    return c.isspace() and not "\x1c" <= c <= "\x1f"


def sigmoid(x):
    if x >= 0:
        return 1. / (1. + math.exp(-x))
    e = math.exp(x)
    return e / (1. + e)


def logit(p):
    return math.log(p) - math.log1p(-p)


def seq_sum(values):
    total = 0.
    for v in values:
        total += v
    return total


def byte_starts(text):
    starts, total = [0], 0
    for c in text:
        total += len(c.encode("utf-8"))
        starts.append(total)
    return starts


# ---------------------------------------------------------------- segmentation

def words(text):
    """[(start_char, end_char)] of maximal non-isspace runs."""
    out, start = [], None
    for i, c in enumerate(text):
        if is_space(c):
            if start is not None:
                out.append((start, i))
                start = None
        elif start is None:
            start = i
    if start is not None:
        out.append((start, len(text)))
    return out


def sentences(text):
    """Sealed splitter a_comp_sentences_v1 on characters: [(start, end, punctuated)].

    Character-index port of grammar-a-positives `text::sentences` (byte based)."""
    out, start, last, i, n = [], None, 0, 0, len(text)
    while i < n:
        c = text[i]
        if is_rust_whitespace(c):
            if c == "\n" and start is not None:
                k, blank = i + 1, False
                while k < n and is_rust_whitespace(text[k]):
                    if text[k] == "\n":
                        blank = True
                        break
                    k += 1
                if blank:
                    out.append((start, last, False))
                    start = None
            i += 1
            continue
        if start is None:
            start = i
        last = i + 1
        if c in TERMINAL:
            k = i + 1
            while k < n and (text[k] in TERMINAL or text[k] in CLOSERS):
                k += 1
            if k == n or is_rust_whitespace(text[k]):
                out.append((start, k, True))
                start = None
            last, i = k, k
            continue
        i += 1
    if start is not None:
        out.append((start, last, False))
    return out


def word_groups(text, offsets, word_mask, spans_of_words):
    """Per word, the positions of word-bearing tokens overlapping its characters."""
    demand(len(offsets) == len(word_mask), "token/offset length mismatch")
    owner = [-1] * len(text)
    for w, (a, b) in enumerate(spans_of_words):
        for c in range(a, b):
            owner[c] = w
    groups = [[] for _ in spans_of_words]
    for j, ((a, b), keep) in enumerate(zip(offsets, word_mask)):
        demand(0 <= a <= b <= len(text), "token offset out of range")
        if not keep:
            continue
        seen = []
        for c in range(a, b):
            w = owner[c]
            if w >= 0 and w not in seen:
                seen.append(w)
        for w in seen:
            groups[w].append(j)
    demand(all(groups), "a word has no word-bearing token (span emission unsupported)")
    return groups


def sentence_word_ranges(sents, spans_of_words):
    """Per emitted sentence (those with words): (sentence index, first word, end word)."""
    out, w = [], 0
    for s, (a, b, _) in enumerate(sents):
        first = w
        while w < len(spans_of_words) and spans_of_words[w][0] < b:
            demand(spans_of_words[w][0] >= a and spans_of_words[w][1] <= b, "word outside its sentence")
            w += 1
        if w > first:
            out.append((s, first, w))
    demand(w == len(spans_of_words), "word outside every sentence")
    return out


# ------------------------------------------------------------------ numerics

def word_probabilities(token_logits, groups):
    out = []
    for g in groups:
        out.append(seq_sum(sigmoid(float(token_logits[j])) for j in g) / len(g))
    return out


def tilt(p_words, share):
    demand(p_words and all(math.isfinite(p) and 0 <= p <= 1 for p in p_words), "word probabilities")
    demand(math.isfinite(share), "document share")
    s = min(max(share, S_CLAMP), 1. - S_CLAMP)
    lg = [logit(min(max(p, P_CLAMP), 1. - P_CLAMP)) for p in p_words]
    lo, hi = -TILT_BOUND, TILT_BOUND
    for _ in range(TILT_ITERATIONS):
        mid = .5 * (lo + hi)
        f = seq_sum(sigmoid(v + mid) for v in lg) / len(lg)
        if f < s:
            lo = mid
        else:
            hi = mid
    b = .5 * (lo + hi)
    return b, s, [sigmoid(v + b) for v in lg]


def validate_decoder(d):
    demand(isinstance(d, dict) and set(d) == {"rule", "tau_hi", "delta", "min_words", "gap", "snap"} and
           d["rule"] == DECODER_RULE, "decoder fields")
    hi, delta = d["tau_hi"], d["delta"]
    demand(type(hi) is float and 0 < hi < 1 and type(delta) is float and 0 <= delta < hi, "decoder thresholds")
    demand(type(d["min_words"]) is int and d["min_words"] >= 1 and type(d["gap"]) is int and d["gap"] >= 0,
           "decoder lengths")
    demand(d["snap"] is None or (type(d["snap"]) is float and 0 < d["snap"] <= 1), "decoder snap")
    return d


def decode(q, sentence_ranges, d):
    """slop_ninja_span_decoder_v1 -> [(first word, end word)] sorted, disjoint, non-adjacent.

    1. runs: maximal intervals of q >= tau_lo containing a word with q >= tau_hi;
    2. consecutive runs with at most `gap` words between them are merged;
    3. runs with fewer than `min_words` words are dropped;
    4. snap: a run covering at least snap x a sentence's words (and at least one)
       is extended to that whole sentence; then overlapping or adjacent runs merge.
    """
    validate_decoder(d)
    hi = d["tau_hi"]
    lo = hi - d["delta"]
    n = len(q)
    runs, i = [], 0
    while i < n:
        if q[i] >= lo:
            j = i
            while j < n and q[j] >= lo:
                j += 1
            if any(q[k] >= hi for k in range(i, j)):
                runs.append([i, j])
            i = j
        else:
            i += 1
    merged = []
    for r in runs:
        if merged and r[0] - merged[-1][1] <= d["gap"]:
            merged[-1][1] = r[1]
        else:
            merged.append(r)
    runs = [r for r in merged if r[1] - r[0] >= d["min_words"]]
    if d["snap"] is not None and runs:
        covered = [False] * n
        for a, b in runs:
            for k in range(a, b):
                covered[k] = True
        for _, sa, sb in sentence_ranges:
            size = sb - sa
            for a, b in runs:
                c = min(b, sb) - max(a, sa)
                if c > 0 and c >= d["snap"] * size:
                    for k in range(sa, sb):
                        covered[k] = True
        runs, i = [], 0
        while i < n:
            if covered[i]:
                j = i
                while j < n and covered[j]:
                    j += 1
                runs.append([i, j])
                i = j
            else:
                i += 1
    return [tuple(r) for r in runs]


def near_threshold(q, d):
    """True when a decision value lies within 1e-6 of tau_hi or tau_lo (R1 exclusion)."""
    hi = d["tau_hi"]
    lo = hi - d["delta"]
    return any(abs(v - hi) < THRESHOLD_MARGIN or abs(v - lo) < THRESHOLD_MARGIN for v in q)


def validate_score_map(m):
    demand(isinstance(m, dict) and m.get("method") in SCORE_MAP_METHODS, "score map method")
    if m["method"] == "identity_v1":
        demand(set(m) == {"method"}, "identity map fields")
        return m
    demand(set(m) == {"method", "input", "target", "bound", "lo", "hi", "knots"} and m["bound"] == SCORE_MAP_BOUND,
           "clipped score map fields")
    lo, hi, knots = m["lo"], m["hi"], m["knots"]
    demand(type(lo) is float and type(hi) is float and 0 < lo <= .5 <= hi < 1, "score map bounds")
    demand(isinstance(knots, list) and knots and all(set(k) == {"score", "value"} for k in knots), "score map knots")
    xs, ys = [k["score"] for k in knots], [k["value"] for k in knots]
    demand(all(a < b for a, b in zip(xs, xs[1:])) and all(a <= b for a, b in zip(ys, ys[1:])) and
           all(lo <= y <= hi for y in ys), "score map knots must be sorted, monotone and inside [lo,hi]")
    return m


def apply_score_map(m, x):
    if m["method"] == "identity_v1":
        return x
    xs, ys = [k["score"] for k in m["knots"]], [k["value"] for k in m["knots"]]
    if x <= xs[0]:
        y = ys[0]
    elif x >= xs[-1]:
        y = ys[-1]
    else:
        upper = bisect.bisect_right(xs, x)
        a, b = upper - 1, upper
        y = min(1., max(0., ys[a] + (ys[b] - ys[a]) * ((x - xs[a]) / (xs[b] - xs[a]))))
    return min(m["hi"], max(m["lo"], y))


def validate_spec(spec):
    demand(isinstance(spec, dict) and set(spec) == {"rule", "word_rule", "word_probability_rule", "tilt_rule",
                                                    "splitter", "decoder", "sentence_confidence",
                                                    "span_confidence"}, "span emission fields")
    demand(spec["rule"] == EMISSION_RULE and spec["word_rule"] == WORD_RULE and
           spec["word_probability_rule"] == WORD_PROBABILITY_RULE and spec["tilt_rule"] == TILT_RULE and
           spec["splitter"] == SPLITTER_ID, "span emission rule identifiers")
    validate_decoder(spec["decoder"])
    validate_score_map(spec["sentence_confidence"])
    validate_score_map(spec["span_confidence"])
    return spec


def spec_for(decoder=None, sentence_map=None, span_map=None):
    return validate_spec({"rule": EMISSION_RULE, "word_rule": WORD_RULE, "word_probability_rule": WORD_PROBABILITY_RULE,
                          "tilt_rule": TILT_RULE, "splitter": SPLITTER_ID,
                          "decoder": dict(decoder or DIAGNOSTIC_DECODER),
                          "sentence_confidence": sentence_map or {"method": "identity_v1"},
                          "span_confidence": span_map or {"method": "identity_v1"}})


# ------------------------------------------------------------------ emission

def emit(text, offsets, word_mask, token_logits, share, confidence, spec, diagnostics=False):
    """Spans and sentences for one request from per-position token logits.

    share / confidence are the emitted document share and AI confidence (0..1)."""
    validate_spec(spec)
    demand(len(token_logits) == len(offsets) == len(word_mask), "token arrays differ in length")
    demand(math.isfinite(confidence) and 0 <= confidence <= 1, "document confidence")
    ws = words(text)
    demand(ws, "no words")
    groups = word_groups(text, offsets, word_mask, ws)
    p = word_probabilities(token_logits, groups)
    b, target, q = tilt(p, share)
    sents = sentences(text)
    ranges = sentence_word_ranges(sents, ws)
    starts = byte_starts(text)
    smap, pmap = spec["sentence_confidence"], spec["span_confidence"]
    sentence_rows = []
    for s, a, e in ranges:
        sa, sb, punctuated = sents[s]
        mean = seq_sum(q[a:e]) / (e - a)
        conf = min(apply_score_map(smap, max(q[a:e])), confidence)
        sentence_rows.append({"start_char": sa, "end_char": sb, "start_byte": starts[sa], "end_byte": starts[sb],
                              "words": e - a, "punctuated": punctuated, "ai_share_percent": 100. * mean,
                              "ai_confidence_percent": 100. * conf})
    span_rows = []
    for a, e in decode(q, ranges, spec["decoder"]):
        mean = seq_sum(q[a:e]) / (e - a)
        conf = min(apply_score_map(pmap, mean), confidence)
        ca, cb = ws[a][0], ws[e - 1][1]
        span_rows.append({"start_char": ca, "end_char": cb, "start_byte": starts[ca], "end_byte": starts[cb],
                          "words": e - a, "ai_share_percent": 100. * mean, "ai_confidence_percent": 100. * conf})
    out = {"rule": EMISSION_RULE, "words": len(ws), "tilt_bias": b, "share_target": target,
           "near_threshold": near_threshold(q, spec["decoder"]), "sentences": sentence_rows, "spans": span_rows}
    if diagnostics:
        out["diagnostics"] = {
            "word_offsets": [[a, e] for a, e in ws],
            "token_probabilities": [sigmoid(float(token_logits[j])) if word_mask[j] else None
                                    for j in range(len(word_mask))],
            "word_probabilities_raw": p, "word_probabilities": q}
    return out


def word_labels(text, spans_of_words, ai_byte_spans):
    """Per word: 1 inside model-produced spans, 0 outside, -1 partially inside."""
    if ai_byte_spans is None:
        return None
    starts = byte_starts(text)
    labels = []
    j = 0
    spans = sorted(ai_byte_spans)
    for a, b in spans_of_words:
        inside = 0
        for c in range(a, b):
            while j < len(spans) and spans[j][1] <= starts[c]:
                j += 1
            k = j
            hit = k < len(spans) and spans[k][0] <= starts[c] and starts[c + 1] <= spans[k][1]
            inside += hit
        labels.append(1 if inside == b - a else 0 if inside == 0 else -1)
    return labels


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False).encode()


# ------------------------------------------------------------- golden vectors

GOLDEN_SCHEMA = "slop_ninja_span_emission_golden_v1"
GOLDEN_TEXTS = [
    ("plain", "The cat sat. The dog ran far away! Did it stop?"),
    ("crlf-blank-lines", "Line one.\r\nLine two\r\n\r\nNew para here.\n\n\nLast words now."),
    ("unicode", "“Café,” said Zoë. Ελληνικά! "
                "中文。 \U0001f469‍\U0001f469‍\U0001f467 ok."),
    ("python-only-space", "Unit\x1fsep and nbsp here. Ideo　space\x1c\x1d done."),
    ("closers", "\"Yes.\" (No!) [Maybe?] 'Fine.' End… More text follows here."),
    ("no-punctuation", "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi"),
    ("single-word", "Hello"),
    ("edges", "  leading words then a stop.  Trailing words  \n"),
    ("one-word-lines", "Apples\nPears\nPlums\n\nFigs\nDates"),
]
GOLDEN_DECODERS = [
    {"rule": DECODER_RULE, "tau_hi": .5, "delta": 0., "min_words": 1, "gap": 0, "snap": None},
    {"rule": DECODER_RULE, "tau_hi": .6, "delta": .2, "min_words": 3, "gap": 2, "snap": .8},
    {"rule": DECODER_RULE, "tau_hi": .7, "delta": .1, "min_words": 1, "gap": 0, "snap": .8},
    {"rule": DECODER_RULE, "tau_hi": .5, "delta": .2, "min_words": 5, "gap": 2, "snap": None},
]
GOLDEN_MAP = {"method": "isotonic_score_v1_clipped", "input": "max_or_mean_tilted_word_probability",
              "target": "synthetic", "bound": SCORE_MAP_BOUND, "lo": .01, "hi": .97,
              "knots": [{"score": .1, "value": .02}, {"score": .4, "value": .3}, {"score": .55, "value": .3},
                        {"score": .9, "value": .95}]}


def golden_tokenize(text):
    """Synthetic offsets: CLS, newline runs, other whitespace+up to 3 non-space
    characters, trailing non-newline whitespace runs, SEP. Not a real tokenizer."""
    import re
    offsets = [(0, 0)]
    for m in re.finditer(r"\n+|[^\S\n]*\S{1,3}|[^\S\n]+", text):
        offsets.append((m.start(), m.end()))
    offsets.append((0, 0))
    mask = [False] + [any(not is_space(text[c]) for c in range(a, b)) for a, b in offsets[1:-1]] + [False]
    return offsets, mask


def golden_cases():
    cases = []
    for t, (name, text) in enumerate(GOLDEN_TEXTS):
        offsets, mask = golden_tokenize(text)
        for v, decoder in enumerate(GOLDEN_DECODERS):
            k = t * len(GOLDEN_DECODERS) + v
            # Smooth runs of high/low logits; one saturated case per text.
            amp, shift = (30., 0.) if v == 3 and t % 3 == 0 else (3.5, .4 * (v - 1.5))
            logits = [0. if not keep else amp * math.sin(.45 * j + .9 * t + .3 * v) + shift
                      for j, keep in enumerate(mask)]
            share = (0., 1., .35, .62)[k % 4] if t % 4 == 1 else .15 + .7 * ((k * 37) % 11) / 10.
            confidence = .97 if k % 3 else .41
            spec = spec_for(decoder, GOLDEN_MAP if v % 2 else None, GOLDEN_MAP if v >= 2 else None)
            out = emit(text, offsets, mask, logits, share, confidence, spec, diagnostics=True)
            demand(not out["near_threshold"], "golden case lies near a decoder threshold")
            cases.append({"id": f"{name}-{v}", "text": text, "token_offsets": [list(o) for o in offsets],
                          "word_mask": mask, "token_logits": logits, "share": share, "confidence": confidence,
                          "spec": spec, "expected": out})
    return {"schema": GOLDEN_SCHEMA, "generator": "full_a_spans.py golden_cases (synthetic text and logits only)",
            "rules": {"emission": EMISSION_RULE, "words": WORD_RULE, "word_probability": WORD_PROBABILITY_RULE,
                      "tilt": TILT_RULE, "splitter": SPLITTER_ID, "decoder": DECODER_RULE}, "cases": cases}


def golden_bytes():
    return json.dumps(golden_cases(), sort_keys=True, indent=1, ensure_ascii=False, allow_nan=False).encode() + b"\n"


if __name__ == "__main__":
    import sys
    demand(len(sys.argv) == 3 and sys.argv[1] == "golden", "usage: full_a_spans.py golden OUT.json")
    with open(sys.argv[2], "wb") as f:
        f.write(golden_bytes())

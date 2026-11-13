"""Streaming reader for the shared-contract A training bundle.

A bundle directory holds `rows.jsonl` (one row per document), `weights.json`
(the declared weighting) and `MANIFEST.json` (inputs and counts); Rust
assembles all three. This module only reads them: it streams `rows.jsonl`
line by line, keeps per-row metadata and packs token ids, token labels and
masks into flat NumPy arrays, so the exact text is never retained after
tokenization and 100k+ rows fit in a few GB of RAM.

Rows of a split that is not requested are skipped before their text is
resolved; a bundle containing any Test row is refused outright.

Trainer bundles (`MANIFEST.json` schema `slop_ninja_a_trainer_bundle_manifest_v1`)
are read by block: the Rust writer puts each split's rows in one block, in
`BLOCK_ORDER`, with `rows_by_split` counting them. Only the lines of the
requested blocks are parsed, and reading stops after the last requested
block, so a later block (an old Dev-select or CAL block) is never parsed or
hashed; at most one read buffer past the last requested block is loaded and
dropped. A requested block behind an unrequested sealed block is refused.
Block boundaries are checked from each kept line's bytes before it is parsed
or hashed: every line of a requested block, and the last line of an
unrequested block before a requested one, must name its block's split (its
`"split"` key, read without parsing; in a valid row it is the only such key).
A count that overstates a block is therefore refused on the first line of the
next block before that line is parsed or hashed. A count that falls short of
an unread block's start cannot be seen without reading that block.

`binding_digests` recomputes, on a bundle, the digests a fit binding recorded
(block digests or whole files), so export and selection check a block-bound
fit without reading its unopened blocks.
"""
from __future__ import annotations

from array import array
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import re

import numpy as np
import torch

import full_a
import full_a_spans as spans_rule


ROWS, WEIGHTS, MANIFEST = "rows.jsonl", "weights.json", "MANIFEST.json"
SPLITS = ("train", "train_mining", "dev_iter", "dev_select", "cal", "test")
SPLIT_ALIASES = {"development_iter": "dev_iter", "dev-iter": "dev_iter", "development_select": "dev_select",
                 "dev-select": "dev_select", "calibration": "cal", "train-mining": "train_mining"}
ORIGINS = {"human_only": 0, "model_only": 1, "mixed": 2}
HUMAN_LABEL_KEY = "human"  # weights.json per-label key for rows whose generator label is null
ROW_FIELDS = {"id", "split", "text", "text_ref", "text_sha256", "origin", "share", "ai_byte_spans",
              "generator_label", "cohort_id", "cohort_weight", "source_group", "domain", "genre", "voice_id",
              "voice_cluster_id", "voice_origin", "style_axes", "operation", "held_out_style", "provenance_sha256",
              "token_weight", "pair_id", "source_kind", "edit_history_sha256"}
# spans-v1 assembler fields (optional): token_weight (token-loss mass, row-weight
# scale), pair_id (the paired untouched parent), source_kind and the edit history digest.
SPLICED_FIELDS = {"token_weight", "pair_id", "source_kind", "edit_history_sha256"}
REQUIRED = ROW_FIELDS - {"text", "text_ref", "genre", "voice_id", "voice_cluster_id", "voice_origin",
                         "style_axes", "operation", "ai_byte_spans"} - SPLICED_FIELDS
TOKEN_WEIGHTINGS = ("row_weight", "bundle_token_weight")
# Span-target floors (protocol field `span_target_floor`): policy -> minimum gold-span length in share words.
SPAN_TARGET_FLOORS = {"slop_ninja_span_target_floor24_v1": 24}
FLAG_CONTENT, FLAG_WORD = 1, 2


def demand(ok, message):
    if not ok:
        raise ValueError(message)


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def canonical_split(value):
    s = SPLIT_ALIASES.get(value, value)
    demand(s in SPLITS, f"unknown split {value!r}")
    return s


def axes_of(value):
    """style_axes: null or a list of persona-axis names (the assembler's
    projection); returns the sorted distinct names. Objects are refused: their
    keys mix axes with non-axis metadata and would become adversary targets."""
    if value is None:
        return ()
    demand(isinstance(value, list) and all(isinstance(k, str) and k for k in value),
           "style_axes must be null or a list of axis names")
    return tuple(sorted(set(value)))


class Meta:
    """Per-row metadata kept in memory (no text)."""
    __slots__ = ("id", "split", "text_sha256", "origin", "share", "label", "cohort_id", "cohort_weight",
                 "source_group", "domain", "genre", "voice_id", "voice_cluster", "voice_origin", "axes",
                 "operation", "held_out_style", "provenance_sha256", "spans", "token_weight", "pair_id",
                 "source_kind")

    def targets(self, ai_index):
        return {"origin": self.origin, "share": self.share,
                "ai": None if self.label is None else ai_index.get(self.label), "human": None}


def parse_row(raw, line_no):
    """Validate one shared-contract row; returns (Meta, row dict). Text is not resolved here."""
    demand(isinstance(raw, dict), f"row {line_no}: object required")
    unknown = set(raw) - ROW_FIELDS
    demand(not unknown, f"row {line_no}: unknown fields {sorted(unknown)}")
    missing = REQUIRED - set(raw)
    demand(not missing, f"row {line_no}: missing fields {sorted(missing)}")
    demand(("text" in raw) != ("text_ref" in raw), f"row {line_no}: exactly one of text/text_ref")
    m = Meta()
    m.id = raw["id"]
    demand(isinstance(m.id, str) and m.id, f"row {line_no}: id")
    m.split = canonical_split(raw["split"])
    demand(raw["origin"] in ORIGINS, f"row {m.id}: origin class")
    m.origin = ORIGINS[raw["origin"]]
    s = raw["share"]
    demand(type(s) in (int, float) and math.isfinite(s) and 0 <= s <= 1, f"row {m.id}: share")
    m.share = float(s)
    demand(m.origin != 0 or m.share == 0, f"row {m.id}: human-only share must be 0")
    demand(m.origin != 1 or m.share == 1, f"row {m.id}: model-only share must be 1")
    # A full rewrite of a human source is mixed with share 1 (AI wording of human content).
    demand(m.origin != 2 or 0 < m.share <= 1, f"row {m.id}: mixed share must be in (0,1]")
    m.label = raw["generator_label"]
    demand((m.label is None) == (m.origin == 0), f"row {m.id}: generator label iff AI involvement")
    demand(m.label is None or (isinstance(m.label, str) and m.label and not m.label.startswith("other_")),
           f"row {m.id}: generator label")
    m.cohort_id = raw["cohort_id"]
    demand(isinstance(m.cohort_id, str) and m.cohort_id, f"row {m.id}: cohort id")
    w = raw["cohort_weight"]
    demand(type(w) in (int, float) and math.isfinite(w) and w >= 0, f"row {m.id}: cohort weight")
    m.cohort_weight = float(w)
    for key in ("source_group", "domain", "text_sha256", "provenance_sha256"):
        demand(isinstance(raw[key], str) and raw[key], f"row {m.id}: {key}")
    m.source_group, m.domain = raw["source_group"], raw["domain"]
    m.text_sha256, m.provenance_sha256 = raw["text_sha256"], raw["provenance_sha256"]
    m.genre = raw.get("genre")
    m.voice_id = raw.get("voice_id")
    m.voice_cluster = raw.get("voice_cluster_id")
    m.voice_origin = raw.get("voice_origin")
    demand(m.voice_cluster is None or m.voice_id is not None, f"row {m.id}: voice cluster without voice id")
    m.axes = axes_of(raw.get("style_axes"))
    m.operation = raw.get("operation")
    demand(type(raw["held_out_style"]) is bool, f"row {m.id}: held_out_style")
    m.held_out_style = raw["held_out_style"]
    m.spans = raw.get("ai_byte_spans")
    tw = raw.get("token_weight")
    demand(tw is None or (type(tw) in (int, float) and math.isfinite(tw) and tw >= 0), f"row {m.id}: token weight")
    m.token_weight = None if tw is None else float(tw)
    m.pair_id, m.source_kind = raw.get("pair_id"), raw.get("source_kind")
    for key in ("pair_id", "source_kind", "edit_history_sha256"):
        demand(raw.get(key) is None or (isinstance(raw[key], str) and raw[key]), f"row {m.id}: {key}")
    return m, raw


class TextSource:
    """Exact text from the row, or from text_ref {path, offset, length} relative to the bundle."""

    def __init__(self, root):
        self.root, self.files = Path(root), {}

    def text(self, raw, meta):
        if "text" in raw:
            value = raw["text"]
            demand(isinstance(value, str), f"row {meta.id}: text")
            data = value.encode("utf-8")
        else:
            ref = raw["text_ref"]
            demand(isinstance(ref, dict) and set(ref) == {"path", "offset", "length"} and
                   isinstance(ref["path"], str) and type(ref["offset"]) is int and type(ref["length"]) is int and
                   ref["offset"] >= 0 and ref["length"] > 0, f"row {meta.id}: text_ref")
            path = Path(ref["path"])
            path = path if path.is_absolute() else self.root / path
            f = self.files.get(path)
            if f is None:
                f = self.files[path] = open(path, "rb")
            f.seek(ref["offset"])
            data = f.read(ref["length"])
            demand(len(data) == ref["length"], f"row {meta.id}: text_ref beyond file end")
            value = data.decode("utf-8")
        demand(hashlib.sha256(data).hexdigest() == meta.text_sha256, f"row {meta.id}: exact text binding differs")
        return value

    def close(self):
        for f in self.files.values():
            f.close()
        self.files.clear()


TRAINER_MANIFEST_SCHEMA = "slop_ninja_a_trainer_bundle_manifest_v1"
# The Rust trainer-bundle writer's split order (grammar-a-assemble trainer.rs), one block per split.
BLOCK_ORDER = ("train", "train_mining", "dev_iter", "dev_select", "cal")
SEALED_SPLITS = ("dev_select", "cal")
BLOCKS_READ = "rows.jsonl_blocks_read"
# A row line's split key, read from its bytes. Row values are scalars or lists of scalars and quotes inside
# strings are escaped, so in a valid row the only `"split":` key is the row's own.
SPLIT_BYTES = re.compile(rb'"split"\s*:\s*"([^"\\]*)"')
BLOCK_DIGEST_RULE = ("trainer bundle with an unrequested sealed block: MANIFEST.json and weights.json hashed; "
                     "rows.jsonl not hashed whole (its digest is the MANIFEST's record, not recomputed); each "
                     "requested block hashed over its exact lines; no line after the last requested block is "
                     "parsed or hashed")


def layout(root):
    """[(split, rows)] in file order for a trainer bundle, or None for any other bundle (read whole)."""
    m = json.loads((Path(root) / MANIFEST).read_bytes())
    if not isinstance(m, dict) or m.get("schema") != TRAINER_MANIFEST_SCHEMA:
        return None
    counts = m.get("rows_by_split")
    demand(isinstance(counts, dict) and "test" not in counts, "Test rows never enter training or iteration")
    demand(counts and set(counts) <= set(BLOCK_ORDER) and
           all(type(n) is int and n >= 0 for n in counts.values()),
           f"trainer MANIFEST rows_by_split: counts of {list(BLOCK_ORDER)}")
    return [(s, counts[s]) for s in BLOCK_ORDER if counts.get(s)]


def read_plan(blocks, wanted):
    """(split, rows, requested) for every block through the last requested one."""
    last = max((i for i, (s, _) in enumerate(blocks) if s in wanted), default=-1)
    plan = []
    for s, n in blocks[:last + 1]:
        demand(s in wanted or s not in SEALED_SPLITS,
               f"reading {sorted(wanted)} would pass the unrequested sealed {s} block")
        plan.append((s, n, s in wanted))
    return plan, last == len(blocks) - 1


def line_split(line):
    """The canonical split a row line names in its bytes (no parse), or None."""
    m = SPLIT_BYTES.search(line)
    try:
        return None if m is None else canonical_split(m.group(1).decode("ascii"))
    except (UnicodeDecodeError, ValueError):
        return None


def block_lines(f, plan):
    """Yield (split, line number, line bytes or None, requested) through the plan.
    Lines of an unrequested block are read past without being kept, except its
    last line, which is returned for the boundary check (an unrequested block
    before a requested one is never sealed). Every kept line must name its
    block's split in its bytes (line_split) before it is yielded, so no caller
    parses or hashes a line of another block."""
    n = 0
    for split, rows, requested in plan:
        for i in range(rows):
            line = f.readline()
            n += 1
            demand(line.endswith(b"\n"), f"rows.jsonl ends inside the {split} block (line {n})")
            keep = requested or i == rows - 1
            if keep:
                named = line_split(line)
                demand(named == split, f"line {n}: a {named or 'split-less'} line in the MANIFEST's {split} block "
                                       "(read from its split bytes, before any parse or hash)")
            yield split, n, line if keep else None, requested


def unread_sealed(blocks, splits):
    wanted = {canonical_split(s) for s in splits}
    return [s for s, _ in blocks if s in SEALED_SPLITS and s not in wanted]


def bound_digests(root, splits):
    """Digests a fit binds when it reads `splits`. Any bundle without an
    unrequested sealed block binds bundle_digests (the whole files). A trainer
    bundle with one (an old Dev-select or CAL block) is neither parsed nor
    hashed there: see BLOCK_DIGEST_RULE."""
    root = Path(root)
    wanted = {canonical_split(s) for s in splits}
    demand("test" not in wanted, "Test rows never enter training or iteration")
    blocks = layout(root)
    if blocks is None or not unread_sealed(blocks, wanted):
        return bundle_digests(root)
    recorded = (read_json(root / MANIFEST).get("files") or {}).get(ROWS)
    demand(isinstance(recorded, str) and len(recorded) == 64, "trainer MANIFEST records no rows.jsonl digest")
    plan, _ = read_plan(blocks, wanted)
    hashes = {s: hashlib.sha256() for s, _, requested in plan if requested}
    with open(root / ROWS, "rb") as f:
        for split, _, line, requested in block_lines(f, plan):
            if requested:
                hashes[split].update(line)
    return {MANIFEST: sha256_file(root / MANIFEST), WEIGHTS: sha256_file(root / WEIGHTS),
            "rows.jsonl_recorded_in_manifest": recorded,
            BLOCKS_READ: {s: {"rows": n, "sha256": hashes[s].hexdigest()} for s, n, requested in plan if requested},
            "rule": BLOCK_DIGEST_RULE}


def binding_digests(root, recorded):
    """The bundle digests a fit binding recorded (`recorded`, its
    bundle_sha256), recomputed on `root`: bound_digests over the blocks it read
    when it holds block digests, else the whole-file digests. They equal
    `recorded` exactly when the bundle still holds what the fit bound; a
    block-bound fit is checked without reading its unopened blocks."""
    blocks = recorded.get(BLOCKS_READ) if isinstance(recorded, dict) else None
    if blocks is None:
        return bundle_digests(root)
    demand(isinstance(blocks, dict) and blocks and set(blocks) <= set(BLOCK_ORDER), "the binding's block digests")
    return bound_digests(root, sorted(blocks))


def stream(root, splits):
    """Yield (Meta, raw) for rows of `splits`; refuses any Test row anywhere.
    A trainer bundle is read by block (module docstring)."""
    wanted = {canonical_split(s) for s in splits}
    demand("test" not in wanted, "Test rows never enter training or iteration")
    blocks = layout(root)
    if blocks is not None:
        plan, final = read_plan(blocks, wanted)
        with open(Path(root) / ROWS, "rb") as f:
            for split, n, line, requested in block_lines(f, plan):
                if not requested:
                    continue
                raw = json.loads(line)
                got = canonical_split(raw.get("split")) if isinstance(raw, dict) else None
                demand(got == split, f"row {n}: a {got} row in the MANIFEST's {split} block")
                yield parse_row(raw, n)
            demand(not final or f.read(1) == b"", "rows.jsonl holds lines after the MANIFEST's blocks")
        return
    with open(Path(root) / ROWS, "rb") as f:
        for n, line in enumerate(f, 1):
            if not line.strip():
                continue
            raw = json.loads(line)
            split = canonical_split(raw.get("split")) if isinstance(raw, dict) else None
            demand(split != "test", f"row {n}: a training bundle must not contain Test rows")
            if split not in wanted:
                continue
            yield parse_row(raw, n)


ROW_WEIGHT_SCHEME = "assembler_row_weight_v1"
LABEL_DOMAIN_SCHEME = "label_domain_balanced_v1"
WEIGHTING_SCHEMES = ("family_origin_balanced", "class_balanced", ROW_WEIGHT_SCHEME, LABEL_DOMAIN_SCHEME)
# Bundles whose weights.json carries per-row assembler weights (row form).
ROW_FORM_SCHEMES = (ROW_WEIGHT_SCHEME, LABEL_DOMAIN_SCHEME)
LABEL_DOMAIN_FIELDS = {"scheme", "positive_mass", "positive_free_domain_human_share", "max_row_weight_ratio"}
# Optional for both row-form schemes: a cap relative to the mean weight of ALL Train rows.
GLOBAL_CAP_FIELD = "max_row_weight_global_ratio"
# Optional for both row-form schemes: {cohort_id prefix: largest share of the AI-involved mass}.
COHORT_CAP_FIELD = "cohort_positive_mass_caps"
OPTIONAL_ROW_FORM_FIELDS = {GLOBAL_CAP_FIELD, COHORT_CAP_FIELD}
# assembler_row_weight_v1 takes an optional per-origin-class row cap (and the global cap).
ROW_WEIGHT_FIELDS = ({"scheme", "positive_mass"}, {"scheme", "positive_mass", "max_row_weight_ratio"})


def validate_weighting(w):
    """Train weighting: {"scheme": "family_origin_balanced", "positive_mass": null},
    {"scheme": "class_balanced", "positive_mass": m in (0,1)},
    {"scheme": "assembler_row_weight_v1", "positive_mass": null} (each Train
    row's cohort_weight is its final assembler weight, used as is; an optional
    "max_row_weight_ratio": null or r >= 1 caps rows per origin class) or
    {"scheme": "label_domain_balanced_v1", "positive_mass": m in (0,1),
    "positive_free_domain_human_share": null or s in [0,1],
    "max_row_weight_ratio": null or r >= 1} (see label_domain_balanced_weights).
    Both row-form schemes also accept "max_row_weight_global_ratio": null or
    g >= 1, which bounds every row at g x the mean weight of all Train rows
    (see apply_global_cap) and "cohort_positive_mass_caps": null or
    {prefix: c in (0,1)}, which bounds the share of the AI-involved mass held by
    rows whose cohort_id starts with the prefix (see apply_cohort_caps)."""
    demand(isinstance(w, dict) and w.get("scheme") in WEIGHTING_SCHEMES, "train weighting")
    g = w.get(GLOBAL_CAP_FIELD)
    demand(g is None or (type(g) in (int, float) and math.isfinite(g) and g >= 1),
           f"{GLOBAL_CAP_FIELD}: null or >= 1")
    caps = w.get(COHORT_CAP_FIELD)
    demand(caps is None or (isinstance(caps, dict) and all(
        isinstance(k, str) and k and type(v) in (int, float) and math.isfinite(v) and 0 < v < 1
        for k, v in caps.items())), f"{COHORT_CAP_FIELD}: null or {{cohort prefix: share in (0,1)}}")
    extra = set(w) & OPTIONAL_ROW_FORM_FIELDS
    demand(not extra or w["scheme"] in ROW_FORM_SCHEMES, f"{sorted(extra)} need a row-form scheme")
    if w["scheme"] == LABEL_DOMAIN_SCHEME:
        demand(set(w) - OPTIONAL_ROW_FORM_FIELDS == LABEL_DOMAIN_FIELDS,
               f"{LABEL_DOMAIN_SCHEME} fields: {sorted(LABEL_DOMAIN_FIELDS)} plus optional {sorted(OPTIONAL_ROW_FORM_FIELDS)}")
        m, s, r = w["positive_mass"], w["positive_free_domain_human_share"], w["max_row_weight_ratio"]
        demand(type(m) in (int, float) and math.isfinite(m) and 0 < m < 1, "positive mass in (0,1)")
        demand(s is None or (type(s) in (int, float) and math.isfinite(s) and 0 <= s <= 1),
               "positive_free_domain_human_share: null or in [0,1]")
        demand(r is None or (type(r) in (int, float) and math.isfinite(r) and r >= 1),
               "max_row_weight_ratio: null or >= 1")
        return w
    if w["scheme"] == ROW_WEIGHT_SCHEME:
        demand(set(w) - OPTIONAL_ROW_FORM_FIELDS in ROW_WEIGHT_FIELDS,
               f"{ROW_WEIGHT_SCHEME} fields: scheme, positive_mass[, max_row_weight_ratio] plus optional {sorted(OPTIONAL_ROW_FORM_FIELDS)}")
        r = w.get("max_row_weight_ratio")
        demand(r is None or (type(r) in (int, float) and math.isfinite(r) and r >= 1),
               "max_row_weight_ratio: null or >= 1")
    else:
        demand(set(w) == {"scheme", "positive_mass"}, "train weighting")
    if w["scheme"] in ("family_origin_balanced", ROW_WEIGHT_SCHEME):
        demand(w["positive_mass"] is None, f"{w['scheme']} takes no positive mass")
    else:
        m = w["positive_mass"]
        demand(type(m) in (int, float) and math.isfinite(m) and 0 < m < 1, "positive mass in (0,1)")
    return w


def family_origin_weights(meta):
    """1 / (groups x origin strata in the group x rows in the group/origin cell)."""
    groups = Counter(m.source_group for m in meta)
    cells = Counter((m.source_group, m.origin) for m in meta)
    strata = Counter(g for g, _ in cells)
    return [1.0 / (len(groups) * strata[m.source_group] * cells[(m.source_group, m.origin)]) for m in meta]


def train_weights(meta, weighting):
    """Train row weights (summing to 1 over rows with mass).

    family_origin_balanced: the balanced weights times the cohort weight. Its
    class mass follows the group structure: positives that sit in few source
    groups get little mass (measured: 1.2% of Train weight for 9,987 of 53,226
    rows in 694 of 35,872 groups).
    class_balanced: humans and AI-involved rows are balanced separately (groups
    equal within the class, origin cells equal within a group), each row times
    its cohort weight, then each class is scaled to its declared mass
    (positive_mass for AI-involved rows, 1 - positive_mass for humans).
    assembler_row_weight_v1: the cohort weight is the row's final weight
    (origin shares, group balance and frontier label balance already applied
    by the assembler); it is only normalized, never rebalanced. With
    `max_row_weight_ratio` r, no row weighs more than r x the mean weight of
    its origin class (human_only, model_only, mixed); cap_weights moves the
    excess to the uncapped rows of the same class, so each class keeps the
    assembler's declared mass.
    label_domain_balanced_v1: see label_domain_balanced_weights.
    """
    if weighting["scheme"] == LABEL_DOMAIN_SCHEME:
        return label_domain_balanced_weights(meta, weighting)
    if weighting["scheme"] == ROW_WEIGHT_SCHEME:
        raw = [m.cohort_weight for m in meta]
        total = sum(raw)
        demand(total > 0, "no Train weight")
        out = [w / total for w in raw]
        ratio = weighting.get("max_row_weight_ratio")
        if ratio is not None:
            for origin in sorted({m.origin for m in meta}):
                idx = [i for i, m in enumerate(meta) if m.origin == origin]
                for i, w in zip(idx, cap_weights([out[i] for i in idx], ratio)):
                    out[i] = w
        groups = [[i for i, m in enumerate(meta) if m.origin == o] for o in sorted({m.origin for m in meta})]
        out = apply_global_cap(out, groups, weighting.get(GLOBAL_CAP_FIELD))
        return apply_cohort_caps(meta, out, groups, ratio, weighting)
    if weighting["scheme"] == "family_origin_balanced":
        raw = [b * m.cohort_weight for b, m in zip(family_origin_weights(meta), meta)]
        total = sum(raw)
        demand(total > 0, "no Train weight")
        return [w / total for w in raw]
    out = [0.] * len(meta)
    for cls, mass in ((False, 1. - weighting["positive_mass"]), (True, weighting["positive_mass"])):
        idx = [i for i, m in enumerate(meta) if (m.origin != 0) == cls]
        demand(idx, "class_balanced weighting needs both human and AI-involved Train rows")
        sub = [meta[i] for i in idx]
        raw = [b * m.cohort_weight for b, m in zip(family_origin_weights(sub), sub)]
        total = sum(raw)
        demand(total > 0, "a class has no Train weight")
        for i, w in zip(idx, raw):
            out[i] = mass * w / total
    return out


def cap_weights(weights, ratio):
    """Water-filling cap: no weight exceeds `ratio` x the mean of `weights`;
    the excess goes to the uncapped weights in proportion, so the total and
    the order of weights are kept. Exact in at most len(weights) rounds."""
    n = len(weights)
    total = sum(weights)
    if ratio is None or n == 0 or total <= 0:
        return list(weights)
    cap = ratio * total / n
    out, capped = list(weights), [False] * n
    while True:
        free = [i for i in range(n) if not capped[i]]
        fixed = sum(cap for i in range(n) if capped[i])
        rest = sum(weights[i] for i in free)
        if not free or rest <= 0:
            break
        f = (total - fixed) / rest
        over = {i for i in free if weights[i] * f > cap * (1 + 1e-12)}
        for i in free:
            out[i] = cap if i in over else weights[i] * f
        if not over:
            break
        for i in over:
            capped[i] = True
    return out


def label_domain_balanced_weights(meta, weighting):
    """label_domain_balanced_v1 Train weights (summing to 1).

    AI-involved rows carry `positive_mass`, humans the rest.
    - Positives: every generator label gets equal mass; within a label every
      domain it occurs in equal mass; within (label, domain) the mixed and
      model-only cells equal mass; within a cell every source group equal
      mass and every row of a group equal mass. Row weights from the
      assembler are not used for positives (only their zero/non-zero
      admission), so no cohort or domain with few source groups can starve a
      generator, and the attribution roster is balanced by construction.
    - Humans: the assembler row weights (domain mass by source groups, group
      balance, cohort factors), renormalised. With
      `positive_free_domain_human_share` s, the humans of domains that hold
      no AI-involved Train row together get the fraction s of the human mass
      (the other humans 1 - s), which bounds the "this domain is always human"
      shortcut; null keeps the assembler's split.
    - With `max_row_weight_ratio` r, no row weighs more than r x its class
      mean (cap_weights), applied per class after the steps above.
    """
    n = len(meta)
    pos = [i for i, m in enumerate(meta) if m.origin != 0]
    hum = [i for i, m in enumerate(meta) if m.origin == 0]
    demand(pos and hum, f"{LABEL_DOMAIN_SCHEME} needs human and AI-involved Train rows")
    pm, ratio = float(weighting["positive_mass"]), weighting["max_row_weight_ratio"]
    out = [0.] * n
    tree = {}
    for i in pos:
        m = meta[i]
        demand(m.label is not None, f"row {m.id}: AI-involved row without generator label")
        tree.setdefault(m.label, {}).setdefault(m.domain, {}).setdefault(m.origin, {}) \
            .setdefault(m.source_group, []).append(i)
    for domains in tree.values():
        lm = 1. / len(tree)
        for origins in domains.values():
            dm = lm / len(domains)
            for groups in origins.values():
                om = dm / len(origins)
                for rows in groups.values():
                    gm = om / len(groups)
                    for i in rows:
                        out[i] = gm / len(rows)
    raw = {i: meta[i].cohort_weight for i in hum}
    demand(sum(raw.values()) > 0, "no human Train weight")
    share = weighting["positive_free_domain_human_share"]
    covered = {meta[i].domain for i in pos}
    parts = [[i for i in hum if meta[i].domain not in covered], [i for i in hum if meta[i].domain in covered]]
    if share is not None and all(parts) and all(sum(raw[i] for i in p) > 0 for p in parts):
        for p, mass in zip(parts, (share, 1. - share)):
            t = sum(raw[i] for i in p)
            for i in p:
                out[i] = mass * raw[i] / t
    else:
        t = sum(raw.values())
        for i in hum:
            out[i] = raw[i] / t
    for idx, mass in ((pos, pm), (hum, 1. - pm)):
        total = sum(out[i] for i in idx)
        capped = cap_weights([out[i] / total for i in idx], ratio)
        for i, w in zip(idx, capped):
            out[i] = mass * w
    out = apply_global_cap(out, [pos, hum], weighting.get(GLOBAL_CAP_FIELD))
    return apply_cohort_caps(meta, out, [pos, hum], ratio, weighting)


def apply_cohort_caps(meta, out, groups, ratio, weighting, rounds=1000):
    """cohort_positive_mass_caps {prefix: c}: rows whose cohort_id starts with
    the prefix hold at most the fraction c of the AI-involved mass. An
    over-cap prefix is scaled down to c and the other AI-involved rows up in
    proportion (the AI-involved total is kept); the row caps (class ratio,
    then global ratio, within `groups`) are then reapplied, and the two steps
    alternate until both hold. Human rows are never moved."""
    caps = weighting.get(COHORT_CAP_FIELD)
    if not caps:
        return out
    gratio = weighting.get(GLOBAL_CAP_FIELD)
    pos = [i for i, m in enumerate(meta) if m.origin != 0 and out[i] > 0]

    def recap(w):
        for idx in groups:
            for i, x in zip(idx, cap_weights([w[i] for i in idx], ratio)):
                w[i] = x
        return apply_global_cap(w, groups, gratio)

    w = list(out)
    for _ in range(rounds):
        over = False
        for prefix, cap in sorted(caps.items()):
            sel = [i for i in pos if meta[i].cohort_id.startswith(prefix)]
            total = sum(w[i] for i in pos)
            share = sum(w[i] for i in sel) / total if total > 0 else 0.
            if share > cap * (1 + 1e-9):
                over = True
                rest = [i for i in pos if not meta[i].cohort_id.startswith(prefix)]
                demand(rest and share < 1, f"cohort prefix {prefix} holds every AI-involved row")
                for i in sel:
                    w[i] *= cap / share
                for i in rest:
                    w[i] *= (1 - cap) / (1 - share)
        if not over:
            return w
        w = recap(w)
    demand(False, f"{COHORT_CAP_FIELD} and the row caps did not converge in {rounds} rounds")


def apply_global_cap(out, groups, ratio):
    """With a global ratio g, no row weighs more than g x the mean weight of
    all rows with mass (weights sum to 1). Each class of `groups` is
    water-filled separately (cap_weights), so every class keeps its mass and
    the order of its weights; a class whose own mean already exceeds the cap
    cannot keep its mass under it and is refused."""
    if ratio is None:
        return out
    n = sum(1 for w in out if w > 0)
    total = sum(out)
    demand(n > 0 and total > 0, "no Train weight")
    cap = ratio * total / n
    out = list(out)
    for idx in groups:
        live = [i for i in idx if out[i] > 0]
        mass = sum(out[i] for i in live)
        if not live or mass <= 0:
            continue
        rel = cap / (mass / len(live))
        demand(rel >= 1, f"{GLOBAL_CAP_FIELD} {ratio} is below a class mean ({1 / rel * ratio:.3f} x global)")
        for i, w in zip(live, cap_weights([out[i] for i in live], rel)):
            out[i] = w
    return out


def subset_key(seed, row_id):
    return hashlib.sha256(f"{seed}\0{row_id}".encode()).hexdigest()


def stratified_subset(meta, rows, seed):
    """Diagnostic Train subset of `rows` rows: strata (cohort, label, origin,
    domain), proportional allocation by largest remainder with at least one row
    per stratum, rows chosen by ascending sha256(seed NUL id). Returns
    {id: N_stratum / n_stratum}, the inverse inclusion fraction that keeps each
    stratum's assembler weight mass when multiplied into the row weight."""
    strata = {}
    for m in meta:
        strata.setdefault((m.cohort_id, label_key(m.label), m.origin, m.domain), []).append(m.id)
    demand(type(rows) is int and len(strata) <= rows < len(meta),
           f"subset rows must lie in [{len(strata)}, {len(meta)})")
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
    while left < 0:  # minimum-one strata pushed the total over: trim the largest
        k = max(keys, key=lambda k: (alloc[k], tuple(map(str, k))))
        demand(alloc[k] > 1, "subset allocation cannot honour one row per stratum")
        alloc[k] -= 1
        left += 1
    out = {}
    for k in keys:
        ids = sorted(strata[k], key=lambda x: subset_key(seed, x))[:alloc[k]]
        for x in ids:
            out[x] = len(strata[k]) / alloc[k]
    return out


ALLOWLIST_SCHEMA = "slop_ninja_a_row_allowlist_v1"
ALLOWLIST_RULE = ("Train rows restricted to the allow-list; kept rows keep their bundle weights (factor 1: no "
                  "refill, no rebalancing), then the protocol's train_weighting normalises and caps over them")


def read_allowlist(path, root):
    """A Train row allow-list (`slop_ninja_a_row_allowlist_v1`, written by
    grammar-a-assemble's `a_r2_rights snapshot --allowlist`): strictly
    ascending unique ids, their count and the SHA-256 of the ids each followed
    by a newline. Its population must be this bundle's Train source
    (MANIFEST `source_splits.train.rows_sha256`). Returns (record, ids)."""
    data = Path(path).read_bytes()
    a = json.loads(data)
    demand(isinstance(a, dict) and a.get("schema") == ALLOWLIST_SCHEMA and a.get("split") == "train",
           f"a {ALLOWLIST_SCHEMA} Train allow-list is required")
    ids = a.get("ids")
    demand(isinstance(ids, list) and ids and all(isinstance(x, str) and x for x in ids), "allow-list ids")
    demand(all(x < y for x, y in zip(ids, ids[1:])), "allow-list ids must be strictly ascending")
    joined = "".join(x + "\n" for x in ids).encode()
    demand(a.get("count") == len(ids) and a.get("ids_sha256") == hashlib.sha256(joined).hexdigest(),
           "allow-list count or ids digest differs from its ids")
    population = (a.get("rows") or {}).get("sha256")
    m = read_json(Path(root) / MANIFEST)
    source = ((m.get("source_splits") or {}).get("train") or {}).get("rows_sha256") if isinstance(m, dict) else None
    demand(isinstance(population, str) and population == source,
           "the allow-list's population is not this bundle's Train source (MANIFEST source_splits.train.rows_sha256)")
    return {"file": str(Path(path).resolve()), "sha256": hashlib.sha256(data).hexdigest(), "schema": ALLOWLIST_SCHEMA,
            "filter": a.get("filter"), "population": a.get("population"), "population_rows_sha256": population,
            "count": len(ids), "ids_sha256": a["ids_sha256"], "rule": ALLOWLIST_RULE}, ids


def train_metadata(root):
    """Text-free Train metadata: (rows with positive cohort weight, zero-weight
    counts by (cohort, label)), as Population(train) would keep and drop them."""
    kept, dropped = [], Counter()
    for m, _ in stream(root, ["train"]):
        if m.cohort_weight == 0:
            dropped[(m.cohort_id, label_key(m.label))] += 1
        else:
            kept.append(m)
    return kept, dropped


class Population:
    """One split, tokenized into flat arrays; per-row Meta; balanced weights.

    weights: Train uses `train_weights` under the declared weighting (cohort
    weights included); evaluation splits use the family/origin balanced
    weights alone, so the selection metric is the protocol's.

    keep (diagnostics only): {id: factor} from stratified_subset; other rows
    are skipped before their text is read, and each kept row's cohort weight
    is multiplied by its factor (stratum mass kept).

    token_weighting (spans-v1 token head): "row_weight" uses the row weights
    for the token loss; "bundle_token_weight" uses each row's `token_weight`
    (required on every row; times the subset factor). token_weights is
    normalised over token-supervised rows; denominators["token_head"] is its
    sum there (1, or 0 without supervision).

    span_words (evaluation populations): per row the share words
    (full_a_spans.words), each word's overlapping word-bearing token positions
    and its label from ai_byte_spans (1 inside, 0 outside, -1 partial; human
    rows 0, model-only rows 1, mixed rows without spans -1), as flat arrays.

    span_target_floor (protocol `span_target_floor`, None = off): AI token and
    span-word labels of gold spans under the floor's length become -1
    (floor_span_targets); span_target_floor_record counts what was masked.
    """

    def __init__(self, root, split, tokenize, special_ids, ai_profiles, cohort_weighted, drop_zero_weight=True,
                 weighting=None, keep=None, token_weighting="row_weight", span_words=False,
                 span_target_floor=None):
        self.split = canonical_split(split)
        self.ai_profiles = list(ai_profiles)
        self.ai_index = {k: i for i, k in enumerate(self.ai_profiles)}
        self.meta, self.dropped, self.unrostered = [], Counter(), Counter()
        ids, labels, flags = array("i"), array("b"), array("B")
        starts = array("q", [0])
        demand(token_weighting in TOKEN_WEIGHTINGS, "token weighting")
        self.token_weighting, self.span_words = token_weighting, span_words
        self.span_target_floor = validate_span_target_floor(span_target_floor)
        floor_rec = Counter()
        word_tokens, word_ptr, word_labels = array("i"), array("q", [0]), array("b")
        row_words = array("q", [0])
        self.span_word_failures = Counter()
        texts = TextSource(root)
        seen = set()
        try:
            for m, raw in stream(root, [self.split]):
                demand(m.id not in seen, f"duplicate id {m.id}")
                seen.add(m.id)
                if self.split == "train":
                    demand(not m.held_out_style, f"row {m.id}: held-out styles never enter Train")
                if cohort_weighted and drop_zero_weight and m.cohort_weight == 0:
                    self.dropped[(m.cohort_id, label_key(m.label))] += 1
                    continue
                if keep is not None:
                    if m.id not in keep:
                        continue
                    m.cohort_weight *= keep[m.id]
                    if m.token_weight is not None:
                        m.token_weight *= keep[m.id]
                if m.label is not None and m.label not in self.ai_index:
                    # Train defines the roster; an evaluation row of another
                    # generator keeps its involvement target but no attribution target.
                    demand(self.split != "train", f"row {m.id}: label outside the AI roster")
                    self.unrostered[m.label] += 1
                text = texts.text(raw, m)
                tok_ids, offsets = tokenize(text)
                lab = full_a.token_labels(text, tok_ids, offsets, special_ids, m.origin, m.spans)
                floored_words = None
                if self.span_target_floor is not None:
                    had_target = any(x >= 0 for x in lab)
                    had_ai = any(x == 1 for x in lab)
                    lab, floored_words, r = floor_span_targets(text, offsets, m.origin, m.spans, lab,
                                                               self.span_target_floor["min_words"])
                    floor_rec.update(r)
                    if had_ai and not any(x == 1 for x in lab):
                        floor_rec["rows_left_without_ai_token_target"] += 1
                    if had_target and not any(x >= 0 for x in lab):
                        floor_rec["rows_left_without_token_target"] += 1
                    floor_rec["rows_with_masked_tokens"] += r["masked_ai_tokens"] > 0
                content = full_a.content_mask_for(tok_ids, special_ids)
                words = full_a.word_mask_for(text, tok_ids, offsets, special_ids)
                if span_words:
                    ws = spans_rule.words(text)
                    try:
                        groups = spans_rule.word_groups(text, offsets, words, ws)
                    except ValueError:
                        groups = None
                        self.span_word_failures["word_without_token"] += 1
                    if groups is None:
                        groups, wl = [], []
                    elif floored_words is not None:
                        wl = floored_words
                    elif m.origin == 2:
                        wl = spans_rule.word_labels(text, ws, m.spans) if m.spans is not None else [-1] * len(ws)
                    else:
                        wl = [0 if m.origin == 0 else 1] * len(ws)
                    for g in groups:
                        word_tokens.extend(g)
                        word_ptr.append(word_ptr[-1] + len(g))
                    word_labels.extend(wl)
                    row_words.append(row_words[-1] + len(groups))
                ids.extend(tok_ids)
                labels.extend(lab)
                flags.extend((FLAG_CONTENT if c else 0) | (FLAG_WORD if w else 0) for c, w in zip(content, words))
                starts.append(starts[-1] + len(tok_ids))
                m.spans = None  # validated into token labels; not retained
                self.meta.append(m)
                del text, raw
        finally:
            texts.close()
        demand(self.meta, f"empty {self.split} population")
        self.span_target_floor_record = None if self.span_target_floor is None else {
            **self.span_target_floor, **{k: int(floor_rec[k]) for k in (
                "gold_spans", "short_spans", "masked_ai_tokens", "masked_ai_words", "rows_with_masked_tokens",
                "rows_left_without_ai_token_target", "rows_left_without_token_target")}}
        self.ids = np.frombuffer(ids, dtype=np.int32).copy()
        self.labels = np.frombuffer(labels, dtype=np.int8).copy()
        self.flags = np.frombuffer(flags, dtype=np.uint8).copy()
        self.starts = np.frombuffer(starts, dtype=np.int64).copy()
        self.lengths = np.diff(self.starts)
        if span_words:
            self.word_tokens = np.frombuffer(word_tokens, dtype=np.int32).copy()
            self.word_ptr = np.frombuffer(word_ptr, dtype=np.int64).copy()
            self.word_labels = np.frombuffer(word_labels, dtype=np.int8).copy()
            self.row_words = np.frombuffer(row_words, dtype=np.int64).copy()
        base = family_origin_weights(self.meta)
        self.base_weights = base
        self.weighting = validate_weighting(weighting or {"scheme": "family_origin_balanced", "positive_mass": None})
        self.weights = train_weights(self.meta, self.weighting) if cohort_weighted else list(base)
        self.denominators = {k: 0. for k in ("origin", "share", "human", "ai", "token", "conditional_share",
                                             "token_head")}
        supervised = [bool((self.labels[self.starts[i]:self.starts[i + 1]] >= 0).any()) for i in range(len(self.meta))]
        for i, (m, w) in enumerate(zip(self.meta, self.weights)):
            self.denominators["origin"] += w
            self.denominators["share"] += w
            if m.origin == 2:
                self.denominators["conditional_share"] += w
            if m.label in self.ai_index:
                self.denominators["ai"] += w
            if supervised[i]:
                self.denominators["token"] += w
        demand(self.denominators["origin"] > 0, "origin supervision required")
        if token_weighting == "row_weight":
            raw = [w if s else 0. for w, s in zip(self.weights, supervised)]
        else:
            demand(all(m.token_weight is not None for m in self.meta),
                   "bundle_token_weight needs token_weight on every row")
            raw = [m.token_weight if s else 0. for m, s in zip(self.meta, supervised)]
        total = sum(raw)
        self.token_weights = [w / total for w in raw] if total > 0 else raw
        self.denominators["token_head"] = sum(self.token_weights)

    def __len__(self):
        return len(self.meta)

    def rows_digest(self):
        """SHA-256 of the population's labelled row identities (text-free):
        sorted canonical (id, text_sha256, origin, share, generator_label,
        source_group) lines. Equal digests mean the same rows and labels."""
        names = {v: k for k, v in ORIGINS.items()}
        h = hashlib.sha256()
        for m in sorted(self.meta, key=lambda m: m.id):
            h.update(json.dumps([m.id, m.text_sha256, names[m.origin], m.share, m.label, m.source_group],
                                separators=(",", ":"), ensure_ascii=False).encode() + b"\n")
        return h.hexdigest()

    def targets(self, i):
        return self.meta[i].targets(self.ai_index)

    def row_ids(self, i):
        return self.ids[self.starts[i]:self.starts[i + 1]]

    def word_index(self, i, device):
        """(token positions, word ids, word labels) for row i, or None without span words."""
        if not self.span_words or self.row_words[i] == self.row_words[i + 1]:
            return None
        a, b = int(self.row_words[i]), int(self.row_words[i + 1])
        ptr = self.word_ptr[a:b + 1]
        tok = torch.from_numpy(self.word_tokens[ptr[0]:ptr[-1]].astype(np.int64))
        wid = torch.repeat_interleave(torch.arange(b - a), torch.from_numpy(np.diff(ptr).astype(np.int64)))
        lab = torch.from_numpy(self.word_labels[a:b].astype(np.int64))
        return tok.to(device), wid.to(device), lab.to(device)

    def batch(self, indices, device, pad_id):
        n = int(max(self.lengths[i] for i in indices))
        k = len(indices)
        ids = torch.full((k, n), pad_id, dtype=torch.long)
        mask = torch.zeros((k, n), dtype=torch.long)
        content = torch.zeros((k, n), dtype=torch.bool)
        labels = torch.full((k, n), -1, dtype=torch.long)
        words = torch.zeros((k, n), dtype=torch.bool)
        for j, i in enumerate(indices):
            a, b = self.starts[i], self.starts[i + 1]
            L = b - a
            ids[j, :L] = torch.from_numpy(self.ids[a:b].astype(np.int64))
            mask[j, :L] = 1
            f = torch.from_numpy(self.flags[a:b].astype(np.int64))
            content[j, :L] = (f & FLAG_CONTENT) > 0
            words[j, :L] = (f & FLAG_WORD) > 0
            labels[j, :L] = torch.from_numpy(self.labels[a:b].astype(np.int64))
        return ids.to(device), mask.to(device), content.to(device), labels.to(device), words.to(device)

    def token_counts(self):
        starts = self.starts
        supervised = sum(bool((self.labels[starts[i]:starts[i + 1]] >= 0).any()) for i in range(len(self)))
        return {"documents": len(self), "tokens": int(self.lengths.sum()), "max_tokens": int(self.lengths.max()),
                "labelled_ai_tokens": int((self.labels == 1).sum()),
                "labelled_human_tokens": int((self.labels == 0).sum()),
                "unlabelled_tokens": int((self.labels < 0).sum()),
                "word_tokens": int(((self.flags & FLAG_WORD) > 0).sum()),
                "token_supervised_documents": supervised,
                "share_words": int(self.row_words[-1]) if self.span_words else None,
                "labelled_ai_words": int((self.word_labels == 1).sum()) if self.span_words else None,
                "labelled_human_words": int((self.word_labels == 0).sum()) if self.span_words else None,
                "unlabelled_words": int((self.word_labels < 0).sum()) if self.span_words else None,
                "span_word_failures": dict(self.span_word_failures) if self.span_words else None,
                "array_bytes": int(self.ids.nbytes + self.labels.nbytes + self.flags.nbytes + self.starts.nbytes)}

    def counts(self):
        """split x domain x origin x generator counts (text-free)."""
        c = Counter((self.split, m.domain, m.origin, m.label) for m in self.meta)
        names = {v: k for k, v in ORIGINS.items()}
        return [{"split": s, "domain": d, "origin": names[o], "generator_label": g, "rows": n}
                for (s, d, o, g), n in sorted(c.items(), key=lambda kv: tuple(map(str, kv[0])))]


# ------------------------------------------------------------- cohort weights

def validate_span_target_floor(f):
    """None, or {"policy", "min_words"} naming a known floor and its length."""
    demand(f is None or (isinstance(f, dict) and set(f) == {"policy", "min_words"} and
                         f["policy"] in SPAN_TARGET_FLOORS and type(f["min_words"]) is int and
                         f["min_words"] == SPAN_TARGET_FLOORS[f["policy"]]), "span target floor")
    return f


def floor_span_targets(text, offsets, origin, spans, token_labels, min_words):
    """Policy slop_ninja_span_target_floor24_v1 (2026-11-06 rule: never train or
    evaluate spans under 24 words). A gold span is a maximal run of share words
    labelled AI (full_a_spans.word_labels for mixed rows with spans; every word of
    a model-only row); a partly AI word is unlabelled and ends a run. An AI token
    (token label 1) that overlaps no word of a run of min_words or more words is
    unsupervised (-1), never relabelled human; human (0) and unlabelled tokens are
    unchanged. Word labels of short-run words become -1 (span metrics). Rows
    without AI token targets (human-only, mixed without spans) are unchanged.
    Document targets (origin, share, attribution) never depend on this.

    Returns (token_labels, word_labels or None, record) where record counts
    gold spans, short spans, masked AI tokens and masked AI words."""
    rec = {"gold_spans": 0, "short_spans": 0, "masked_ai_tokens": 0, "masked_ai_words": 0}
    if origin not in (1, 2) or (origin == 2 and spans is None):
        return token_labels, None, rec
    ws = spans_rule.words(text)
    wl = spans_rule.word_labels(text, ws, spans) if origin == 2 else [1] * len(ws)
    long_char = bytearray(len(text))
    i, n = 0, len(ws)
    out_words = list(wl)
    while i < n:
        if wl[i] != 1:
            i += 1
            continue
        j = i
        while j < n and wl[j] == 1:
            j += 1
        rec["gold_spans"] += 1
        if j - i >= min_words:
            for a, b in ws[i:j]:
                long_char[a:b] = b"\x01" * (b - a)
        else:
            rec["short_spans"] += 1
            rec["masked_ai_words"] += j - i
            for w in range(i, j):
                out_words[w] = -1
        i = j
    labels = list(token_labels)
    for k, (lab, (a, b)) in enumerate(zip(token_labels, offsets)):
        if lab == 1 and not any(long_char[a:b]):
            labels[k] = -1
            rec["masked_ai_tokens"] += 1
    return labels, out_words, rec


def label_key(label):
    return HUMAN_LABEL_KEY if label is None else label


def _close(a, b, rel=1e-9):
    return abs(a - b) <= rel * max(1., abs(a), abs(b))


def validate_weights(weights_doc, train_meta, dropped, overrides=None, scheme="family_origin_balanced"):
    """Check every Train row's cohort weight against weights.json and build the
    per-cohort, per-label log a valid run must carry.

    Two weights.json forms, matched to the protocol's weighting scheme:
    - base_weighting "family_origin_balanced" (family_origin_balanced or
      class_balanced runs): {"cohorts": {cohort_id: {"policy": str,
      "default": bool, "labels": {label|"human": {"records": n, "weight": w}}}}};
      every row of a cohort/label carries the one declared factor w.
    - base_weighting "assembler_row_weight_v1" (assembler_row_weight_v1 runs):
      each label entry is {"records": n, "weight_sum", "weight_min",
      "weight_max"} over that cohort/label's Train rows, and the top-level
      "train_weight_sum" is the total; rows are checked by count, sum, minimum
      and maximum, since each row carries its own final weight.
    Counts are Train rows (zero-weight rows included). overrides (protocol) are
    applied by the assembler, never here: an override must already be
    reflected in the rows and is only recorded.
    """
    row_form = scheme in ROW_FORM_SCHEMES
    expected = ROW_WEIGHT_SCHEME if row_form else "family_origin_balanced"
    demand(isinstance(weights_doc, dict) and isinstance(weights_doc.get("cohorts"), dict), "weights.json shape")
    demand(weights_doc.get("base_weighting") == expected,
           f"weights.json base_weighting {weights_doc.get('base_weighting')!r} does not match the protocol's "
           f"train weighting {scheme!r} (needs {expected!r})")
    declared = weights_doc["cohorts"]
    observed = Counter((m.cohort_id, label_key(m.label)) for m in train_meta)
    for (cohort, key), n in dropped.items():
        observed[(cohort, key)] += n
    if row_form:
        # Zero-weight rows (a cohort the assembler weighted out) are dropped
        # before training but still count: weight 0 enters the sum and minimum.
        sums, lows, highs = Counter(), {k: 0. for k in dropped}, {k: 0. for k in dropped}
        for m in train_meta:
            k = (m.cohort_id, label_key(m.label))
            demand(math.isfinite(m.cohort_weight) and m.cohort_weight > 0, f"row {m.id}: row weight must be positive")
            sums[k] += m.cohort_weight
            lows[k] = min(lows.get(k, math.inf), m.cohort_weight)
            highs[k] = max(highs.get(k, -math.inf), m.cohort_weight)
        total = weights_doc.get("train_weight_sum")
        demand(type(total) in (int, float) and _close(float(total), sum(sums.values())),
               f"Train weight sum {sum(sums.values())} differs from weights.json train_weight_sum {total}")
    log = {}
    for (cohort, key), n in sorted(observed.items()):
        demand(cohort in declared, f"cohort {cohort} absent from weights.json")
        c = declared[cohort]
        demand(isinstance(c.get("labels"), dict) and key in c["labels"], f"cohort {cohort} label {key} undeclared")
        entry = c["labels"][key]
        demand(entry.get("records") == n, f"cohort {cohort} label {key}: {n} Train rows, weights.json says "
               f"{entry.get('records')}")
        demand(type(c.get("default")) is bool and isinstance(c.get("policy"), str), f"cohort {cohort} policy/default")
        log.setdefault(cohort, {"policy": c["policy"], "default": c["default"], "labels": {}})
        if row_form:
            for f, got in (("weight_sum", sums[(cohort, key)]), ("weight_min", lows[(cohort, key)]),
                           ("weight_max", highs[(cohort, key)])):
                v = entry.get(f)
                demand(type(v) in (int, float) and _close(float(v), got),
                       f"cohort {cohort} label {key}: Train {f} {got}, weights.json says {v}")
            log[cohort]["labels"][key] = {"records": n, "weight_sum": float(entry["weight_sum"]),
                                          "weight_min": float(entry["weight_min"]),
                                          "weight_max": float(entry["weight_max"]),
                                          "trained_records": 0, "effective_mass": 0.}
            continue
        log[cohort]["labels"][key] = {"records": n, "weight": float(entry["weight"]),
                                      "trained_records": 0, "effective_mass": 0.}
    for m in ([] if row_form else train_meta):
        w = log[m.cohort_id]["labels"][label_key(m.label)]
        demand(abs(w["weight"] - m.cohort_weight) <= 1e-12 * max(1., abs(w["weight"])),
               f"row {m.id}: cohort weight differs from weights.json")
    for cohort in declared:
        demand(cohort in log, f"weights.json cohort {cohort} has no Train rows")
    return log


def finish_weight_log(log, population, overrides):
    """Add trained counts and the share of origin-loss mass per cohort/label."""
    total = sum(population.weights)
    for m, w in zip(population.meta, population.weights):
        e = log[m.cohort_id]["labels"][label_key(m.label)]
        e["trained_records"] += 1
        e["effective_mass"] += w / total
    for cohort, c in log.items():
        c["records"] = sum(e["records"] for e in c["labels"].values())
        c["effective_mass"] = sum(e["effective_mass"] for e in c["labels"].values())
        c["override"] = (overrides or {}).get(cohort)
    names = {v: k for k, v in ORIGINS.items()}
    by_origin = Counter()
    for m, w in zip(population.meta, population.weights):
        by_origin[names[m.origin]] += w / total
    return {"schema": "slop_ninja_full_a_cohort_weight_log_v2", "complete": True,
            "base_weighting": (population.weighting["scheme"] if population.weighting["scheme"] in ROW_FORM_SCHEMES
                               else population.weighting["scheme"] + "_times_cohort_weight"),
            "weighting": population.weighting, "effective_mass_by_origin": dict(sorted(by_origin.items())),
            "cohorts": log}


def ai_roster(root):
    """Sorted canonical generator labels among Train rows (streamed, text-free)."""
    labels = set()
    for m, _ in stream(root, ["train"]):
        if m.label is not None and m.cohort_weight > 0:
            labels.add(m.label)
    return sorted(labels)


def bundle_digests(root):
    root = Path(root)
    return {name: sha256_file(root / name) for name in (ROWS, WEIGHTS, MANIFEST)}


def read_json(path):
    return json.loads(Path(path).read_bytes())


def peak_rss_bytes():
    try:
        import resource
        return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * (1 if os.uname().sysname == "Darwin" else 1024)
    except Exception:  # pragma: no cover - platform without resource
        return None

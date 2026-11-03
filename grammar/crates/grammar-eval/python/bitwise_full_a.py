"""Bitwise smoke harness for the full-A trainer (protocol A2 D3).

D3 requires that the trainer, with every new option off, reproduce R0's
training computation bitwise on R0's bundle, on at most 1% of its rows. The
harness runs R0's own trainer code and the candidate trainer on the same
smoke input, seed, device and environment, and compares their computation:

- `extract`: the trainer modules of one commit (`git archive`) into a new
  directory. With `--binding`, every module digest a fit binding records
  (R0's `binding.json`) must match, so the reference is R0's trainer by
  digest.
- `smoke-bundle`: a new trainer bundle from the Train block of a trainer
  bundle. Only `MANIFEST.json`, `weights.json` and exactly the first
  `rows_by_split.train` lines of `rows.jsonl` are read; no later line is
  parsed or hashed, so the Dev-iter, Dev-select and CAL blocks stay unread.
  Rows are chosen by an unkeyed rule (`SMOKE_RULE`), at most `fraction` of
  the Train lines in all. The smoke's Dev-iter is a carve of whole Train
  source groups (cohort weight 1), as in the Rust trainer bundle's smoke
  carve, so no evaluation row is read or scored. Train rows keep their exact
  lines. The bundle is marked `smoke_only`. The config directory holds the
  given contract, rebound to the smoke MANIFEST, and the given protocol
  bytes unchanged.
- `compare`: each trainer runs in its own process (`trace`), importing only
  its own tree, with its optimizer step terms recorded (the joint loss, every
  loss part and both pre-clip gradient norms, as exact floats). It then
  compares every step's terms, the Dev-iter history, the initial gate, the
  cohort weight log, the selected checkpoint tensors, the exported encoder
  and heads, the raw and prediction dumps, fit.json's computed fields and the
  binding's input digests. Floats compare by their exact bits. A key that
  only one trainer writes must carry a neutral value (0, null, false, empty);
  the record-only keys in `RECORD_ONLY` are listed, not compared. The report
  (`BITWISE.json`) holds counts, digests and key paths only, never text or
  row ids.

A D3 pass (`d3_pass`) is `bitwise_equal` and every D3 condition: the smoke
bundle rebuilt byte for byte from `--source`'s Train block, at most 1% of
its lines; `--r0-binding` (R0's F-17 binding.json) equal to its expected
digest, the reference tree extracted against that file and its module files
equal to the binding's digests now; both runs' torch, CUDA, transformers
model source and device equal to that binding's; the source MANIFEST,
weights.json, Train block, contract (`--source-contract`, rebound only in
`bundle_manifest_sha256`) and protocol equal to their expected digests, and
the first four of those equal to the binding's records; and the real
encoder (no synthetic fixture). Each expected digest is an argument and is
recorded with what was observed. `compare` exits 0 on a D3 pass, 1 when the
runs differ, and 3 when they are bitwise equal but a D3 condition fails.

Row lines read from a source bundle must name the Train split in their bytes
before they are parsed (as full_a_bundle's block reader checks), so an
overstated Train count is refused on the first line past the block without
parsing it.

Stdlib only at import: the trainer trees are imported in the `trace`
subprocesses only.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

TRAINER_MANIFEST_SCHEMA = "slop_ninja_a_trainer_bundle_manifest_v1"
TRAINER_WEIGHTS_SCHEMA = "slop_ninja_a_trainer_weights_v1"
ROW_WEIGHT_SCHEME = "assembler_row_weight_v1"
FACTOR_SCHEME = "family_origin_balanced"
SMOKE_SCHEMA = "slop_ninja_full_a_bitwise_smoke_bundle_v1"
REFERENCE_SCHEMA = "slop_ninja_full_a_bitwise_trainer_tree_v1"
REPORT_SCHEMA = "slop_ninja_full_a_bitwise_smoke_v1"
PYTHON_DIR = "grammar/crates/grammar-eval/python"
SMOKE_KEY = "slop_ninja_a_r2_d3_smoke_v1:"
D3_FRACTION = 0.01
SMOKE_RULE = (
    "D3 smoke v1 over the Train block's positive-weight rows: source groups ordered by "
    "sha256(SMOKE_KEY + 'group:' + group); the shortest prefix of groups holding all three origin classes is "
    "carved into dev_iter (cohort weight 1); the other rows are stratified by (cohort, label, origin), "
    "floor(fraction x Train lines) - carved rows in all, largest remainder with at least one per stratum, rows "
    "by ascending sha256(SMOKE_KEY + 'row:' + id); Train lines kept byte for byte; each block sorted by id")
# Fit binding fields -> the module whose file digest they record.
MODULE_DIGESTS = {"trainer_sha256": "train_full_a.py", "model_sha256": "full_a.py", "aux_sha256": "full_a_aux.py",
                  "bundle_reader_sha256": "full_a_bundle.py", "architecture_sha256": "last_block_a.py",
                  "shared_loss_sha256": "shared_a.py", "shared_heads_implementation_sha256": "warm_start_a.py",
                  "artifact_loader_sha256": "v6_embeddings.py", "spans_rule_sha256": "full_a_spans.py"}
# Written by one trainer version as a record of the run, not as a computed value.
RECORD_ONLY = ("elapsed_seconds_this_invocation", "selection_metric", "selection_value", "selection_eligible",
               "initial_reference")
# Binding fields that must be equal: the run's inputs and environment (code digests may differ).
BINDING_INPUTS = ("schema", "arm", "arm_definition", "seed", "bundle_sha256", "contract_sha256", "protocol_sha256",
                  "initializer_sha256", "parent_artifact_id", "transformers_model_source_sha256", "torch", "cuda",
                  "device", "dev_iter_rows_sha256")
# fit.json fields computed by the fit (the rest are records, timings or paths).
FIT_COMPUTED = ("selected_evaluation", "selected_epoch", "selected_step", "selected_global_step",
                "selection_split_involvement_brier", "selected_dev_iter_diagnostics", "stopped_early", "evaluations",
                "steps_per_epoch", "schedule", "evaluation_points_per_epoch", "trained_selected_heads",
                "train_population", "dev_iter_population", "train_counts", "dev_iter_counts",
                "train_dropped_zero_weight", "dev_iter_rows_sha256", "aux_heads", "diagnostic_train_subset")
# compare's expected digests (--expect-NAME) and the R0 binding fields that pin the run's environment.
D3_EXPECT = ("r0_binding_sha256", "source_manifest_sha256", "source_weights_sha256", "train_block_sha256",
             "contract_sha256", "protocol_sha256")
ENVIRONMENT = ("torch", "cuda", "transformers_model_source_sha256", "device")
EXIT_D3_PASS, EXIT_DIFFERENT, EXIT_EQUAL_NOT_D3 = 0, 1, 3
# A row line's split key, read from its bytes (full_a_bundle.SPLIT_BYTES).
SPLIT_BYTES = re.compile(rb'"split"\s*:\s*"([^"\\]*)"')
TRAINER_FILES = ("binding.json", "cohort-weights.json", "initial-gate.json", "history.json", "fit.json",
                 "selected.pt", "encoder.safetensors", "heads.safetensors", "dev_iter-raw.jsonl",
                 "dev_iter-predictions.jsonl")


def demand(ok, message):
    if not ok:
        raise ValueError(message)


def sha256_bytes(b):
    return hashlib.sha256(b).hexdigest()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def key(*parts):
    return hashlib.sha256((SMOKE_KEY + ":".join(parts)).encode()).hexdigest()


def write_new(path, data):
    with open(path, "xb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())


def pretty(value):
    return (json.dumps(value, indent=1, sort_keys=True, ensure_ascii=False) + "\n").encode()


def new_dir(out):
    """A new output directory, created complete: work in a sibling temporary directory, rename at the end."""
    out = Path(out)
    demand(not out.exists(), f"{out} exists; outputs are written once")
    out.parent.mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(prefix="." + out.name + ".", dir=out.parent)), out


# ------------------------------------------------------------------ trees

def extract(repo, commit, out, binding=None):
    """Trainer modules of `commit` (git archive of PYTHON_DIR) into `out`/tree, verified against a fit binding."""
    work, out = new_dir(out)
    try:
        data = subprocess.run(["git", "-C", str(repo), "archive", "--format=tar", commit, PYTHON_DIR],
                              check=True, capture_output=True).stdout
        resolved = subprocess.run(["git", "-C", str(repo), "rev-parse", commit + "^{commit}"], check=True,
                                  capture_output=True, text=True).stdout.strip()
        tree = work / "tree"
        tree.mkdir()
        with tarfile.open(fileobj=io.BytesIO(data)) as tar:
            for m in tar.getmembers():
                name = Path(m.name)
                demand(not name.is_absolute() and ".." not in name.parts, "archive member outside its directory")
                if not m.isfile() or name.parent.as_posix() != PYTHON_DIR or name.suffix != ".py":
                    continue
                write_new(tree / name.name, tar.extractfile(m).read())
        digests = {f.name: sha256_file(f) for f in sorted(tree.iterdir())}
        checked = {}
        if binding is not None:
            b = json.loads(Path(binding).read_bytes())
            for field, module in MODULE_DIGESTS.items():
                if field in b:
                    demand(digests.get(module) == b[field], f"{module} at {resolved} differs from the binding's {field}")
                    checked[field] = b[field]
            demand(checked, "the binding records no module digest")
        record = {"schema": REFERENCE_SCHEMA, "commit": resolved, "python_dir": PYTHON_DIR, "files": digests,
                  "binding": None if binding is None else {"file": str(Path(binding).resolve()),
                                                           "sha256": sha256_file(binding), "checked": checked}}
        write_new(work / "TREE.json", pretty(record))
        os.rename(work, out)
        return record
    except BaseException:
        shutil.rmtree(work, ignore_errors=True)
        raise


# ------------------------------------------------------------------ smoke bundle

def read_train_block(source):
    """(MANIFEST, weights.json, [(line bytes, row)]) from the Train block only, with digests."""
    source = Path(source)
    mbytes = (source / "MANIFEST.json").read_bytes()
    m = json.loads(mbytes)
    demand(isinstance(m, dict) and m.get("schema") == TRAINER_MANIFEST_SCHEMA and m.get("smoke_only") is False,
           f"a non-smoke {TRAINER_MANIFEST_SCHEMA} bundle is required")
    n = (m.get("rows_by_split") or {}).get("train")
    demand(type(n) is int and n > 0, "the MANIFEST records no Train rows")
    wbytes = (source / "weights.json").read_bytes()
    rows, h = [], hashlib.sha256()
    with open(source / "rows.jsonl", "rb") as f:
        for i in range(1, n + 1):
            line = f.readline()
            demand(line.endswith(b"\n"), f"rows.jsonl ends inside the Train block (line {i})")
            named = SPLIT_BYTES.search(line)
            demand(named is not None and named.group(1) == b"train",
                   f"line {i} does not name the Train split (read from its bytes, before any parse or hash)")
            h.update(line)
            r = json.loads(line)
            demand(isinstance(r, dict) and r.get("split") == "train", f"line {i} is not a Train row")
            rows.append((line, r))
    return m, json.loads(wbytes), rows, {"manifest_sha256": sha256_bytes(mbytes), "weights_sha256": sha256_bytes(wbytes),
                                         "train_block": {"lines": n, "sha256": h.hexdigest()}}


def select_smoke(rows, fraction):
    """(train rows, carved dev rows, record) under SMOKE_RULE."""
    demand(type(fraction) in (int, float) and 0 < fraction < 1, "fraction in (0, 1)")
    total = math.floor(fraction * len(rows))
    live = [(line, r) for line, r in rows if r["cohort_weight"] > 0]
    groups = {}
    for x in live:
        groups.setdefault(x[1]["source_group"], []).append(x)
    carved, origins = [], set()
    for g in sorted(groups, key=lambda g: key("group", g)):
        if origins >= {"human_only", "model_only", "mixed"}:
            break
        carved.append(g)
        origins |= {r["origin"] for _, r in groups[g]}
    demand(origins >= {"human_only", "model_only", "mixed"}, "the Train block lacks an origin class")
    dev = [x for g in carved for x in groups[g]]
    cut = set(carved)
    pool = [x for x in live if x[1]["source_group"] not in cut]
    labels = {r["generator_label"] for _, r in live} - {None}
    demand(labels <= {r["generator_label"] for _, r in pool}, "the carve removes every Train row of a label")
    strata = {}
    for x in pool:
        r = x[1]
        strata.setdefault((r["cohort_id"], r["generator_label"] or "human", r["origin"]), []).append(x)
    want = total - len(dev)
    demand(len(strata) <= want <= len(pool),
           f"{want} smoke Train rows cannot hold one row of each of {len(strata)} strata")
    keys = sorted(strata)
    quota = {k: max(1., want * len(strata[k]) / len(pool)) for k in keys}
    alloc = {k: min(len(strata[k]), int(quota[k])) for k in keys}
    left = want - sum(alloc.values())
    order = sorted(keys, key=lambda k: (-(quota[k] - int(quota[k])), k))
    while left > 0:
        moved = False
        for k in order:
            if left > 0 and alloc[k] < len(strata[k]):
                alloc[k] += 1
                left -= 1
                moved = True
        demand(moved, "smoke allocation cannot place every row")
    while left < 0:
        k = max(keys, key=lambda k: (alloc[k], k))
        demand(alloc[k] > 1, "smoke allocation cannot keep one row per stratum")
        alloc[k] -= 1
        left += 1
    train = [x for k in keys for x in sorted(strata[k], key=lambda x: key("row", x[1]["id"]))[:alloc[k]]]
    return train, dev, {"train_lines": len(rows), "positive_weight_rows": len(live), "fraction": fraction,
                        "rows_allowed": total, "carved_groups": len(carved), "dev_iter_rows": len(dev),
                        "train_rows": len(train), "strata": len(strata), "labels": len(labels)}


def smoke_weights(source_weights, train):
    base = source_weights.get("base_weighting")
    demand(base in (ROW_WEIGHT_SCHEME, FACTOR_SCHEME), "source weights.json form")
    cohorts, total = {}, 0.
    for _, r in train:
        decl = (source_weights.get("cohorts") or {}).get(r["cohort_id"])
        demand(isinstance(decl, dict), f"cohort {r['cohort_id']} absent from the source weights.json")
        c = cohorts.setdefault(r["cohort_id"], {k: decl[k] for k in ("policy", "default", "cohort_weight")
                                                if k in decl} | {"labels": {}})
        label, w = r["generator_label"] or "human", float(r["cohort_weight"])
        if base == ROW_WEIGHT_SCHEME:
            e = c["labels"].setdefault(label, {"records": 0, "weight_sum": 0., "weight_min": math.inf,
                                               "weight_max": -math.inf})
            e["records"] += 1
            e["weight_sum"] += w
            e["weight_min"], e["weight_max"] = min(e["weight_min"], w), max(e["weight_max"], w)
            total += w
        else:
            e = c["labels"].setdefault(label, {"records": 0, "weight": w})
            demand(e["weight"] == w, f"cohort {r['cohort_id']} label {label}: factor differs within the label")
            e["records"] += 1
    out = {"schema": TRAINER_WEIGHTS_SCHEMA, "base_weighting": base, "rule": "D3 smoke: the smoke Train rows' "
           "weights, unchanged; counts, sums and extremes recomputed over them", "cohorts": cohorts}
    if base == ROW_WEIGHT_SCHEME:
        out["train_weight_sum"] = total
    return out


def dev_line(r):
    d = dict(r)
    d["split"], d["cohort_weight"] = "dev_iter", 1.0
    return (json.dumps(d, ensure_ascii=False, separators=(",", ":")) + "\n").encode()


def smoke_body(train, dev):
    """The smoke rows.jsonl: Train lines byte for byte, then the carved Dev-iter rows, each block sorted by id."""
    body = b"".join(line for line, _ in sorted(train, key=lambda x: x[1]["id"]))
    return body + b"".join(dev_line(r) for _, r in sorted(dev, key=lambda x: x[1]["id"]))


def smoke_bundle(source, contract, protocol, out, fraction=D3_FRACTION):
    """Write `out`/bundle and `out`/config (see the module docstring); returns the SMOKE.json record."""
    m, weights, rows, src = read_train_block(source)
    train, dev, selection = select_smoke(rows, fraction)
    work, out = new_dir(out)
    try:
        b, c = work / "bundle", work / "config"
        b.mkdir()
        c.mkdir()
        body = smoke_body(train, dev)
        write_new(b / "rows.jsonl", body)
        write_new(b / "weights.json", pretty(smoke_weights(weights, train)))
        manifest = {"schema": TRAINER_MANIFEST_SCHEMA, "smoke_only": True, "smoke_schema": SMOKE_SCHEMA,
                    "rule": SMOKE_RULE, "smoke_key": SMOKE_KEY,
                    "rows_by_split": {"train": len(train), "dev_iter": len(dev)},
                    "files": {"rows.jsonl": sha256_bytes(body), "weights.json": sha256_file(b / "weights.json")},
                    "source": src, "row_fields": m.get("row_fields"), "selection": selection,
                    "detector_outputs_used": False, "test_opened": False,
                    "dev_iter": "carved Train source groups; never a protocol Dev-iter"}
        write_new(b / "MANIFEST.json", pretty(manifest))
        msha = sha256_file(b / "MANIFEST.json")
        k = json.loads(Path(contract).read_bytes())
        demand(isinstance(k, dict) and "bundle_manifest_sha256" in k, "a full-A contract is required")
        k["bundle_manifest_sha256"] = msha
        write_new(c / "contract.json", pretty(k))
        write_new(c / "protocol.json", Path(protocol).read_bytes())
        record = {"schema": SMOKE_SCHEMA, "source": {"dir": str(Path(source).resolve()), **src},
                  "contract": {"source_sha256": sha256_file(contract), "smoke_sha256": sha256_file(c / "contract.json")},
                  "protocol_sha256": sha256_file(protocol), "manifest_sha256": msha,
                  "rows_sha256": manifest["files"]["rows.jsonl"], "selection": selection, "rule": SMOKE_RULE}
        write_new(work / "SMOKE.json", pretty(record))
        os.rename(work, out)
        return record
    except BaseException:
        shutil.rmtree(work, ignore_errors=True)
        raise


def verify_smoke(source, smoke):
    """Whether the smoke bundle `smoke` is exactly what smoke_bundle builds from `source` at its recorded
    fraction (rows.jsonl, weights.json and the MANIFEST's records), with the source digests recomputed."""
    smoke = Path(smoke)
    m = read_json(smoke / "MANIFEST.json")
    _, weights, rows, src = read_train_block(source)
    recorded = m.get("selection") or {}
    train, dev, selection = select_smoke(rows, recorded.get("fraction"))
    body = smoke_body(train, dev)
    files = m.get("files") or {}
    ok = (m.get("smoke_only") is True and m.get("source") == src and selection == recorded and
          m.get("rows_by_split") == {"train": len(train), "dev_iter": len(dev)} and
          sha256_bytes(body) == files.get("rows.jsonl") == sha256_file(smoke / "rows.jsonl") and
          pretty(smoke_weights(weights, train)) == (smoke / "weights.json").read_bytes() and
          files.get("weights.json") == sha256_file(smoke / "weights.json"))
    return ok, src


def pinned(observed, *expected):
    """observed is a SHA-256 and every expected value equals it (a missing expectation never pins)."""
    return isinstance(observed, str) and len(observed) == 64 and all(e == observed for e in expected)


def d3_check(args, work, ref_record):
    """(conditions, observed, expected, rows fraction) of a compare run whose outputs are in `work`."""
    expect = {k: getattr(args, "expect_" + k, None) for k in D3_EXPECT}
    data, config = Path(args.data), Path(args.config)
    smoke = read_json(data / "MANIFEST.json")
    if getattr(args, "source", None):
        rebuilt, src = verify_smoke(args.source, data)
    else:
        rebuilt, src = False, smoke.get("source") or {}
    counts = smoke.get("rows_by_split") or {}
    lines = (src.get("train_block") or {}).get("lines")
    fraction = (counts.get("train", 0) + counts.get("dev_iter", 0)) / lines if lines else None
    r0_path = getattr(args, "r0_binding", None)
    r0 = read_json(r0_path) if r0_path else {}
    bound = r0.get("bundle_sha256") if isinstance(r0.get("bundle_sha256"), dict) else {}
    tree = Path(args.reference_tree)
    modules = {f: (sha256_file(tree / m) if (tree / m).exists() else None)
               for f, m in MODULE_DIGESTS.items() if f in r0}
    runs = {n: read_json(work / n / "binding.json") for n in ("reference", "candidate")}
    contract_path = getattr(args, "source_contract", None)
    rebound = None
    if contract_path:
        rebound = dict(read_json(contract_path), bundle_manifest_sha256=sha256_file(data / "MANIFEST.json"))
    observed = {"r0_binding_sha256": sha256_file(r0_path) if r0_path else None,
                "reference_tree_binding_sha256": (ref_record.get("binding") or {}).get("sha256"),
                "reference_modules": modules,
                "environment": {n: {k: b.get(k) for k in ENVIRONMENT} for n, b in runs.items()},
                "source_manifest_sha256": src.get("manifest_sha256"),
                "source_weights_sha256": src.get("weights_sha256"),
                "train_block_sha256": (src.get("train_block") or {}).get("sha256"),
                "contract_sha256": sha256_file(contract_path) if contract_path else None,
                "smoke_contract_is_the_source_contract_rebound": rebound is not None and
                                                                  read_json(config / "contract.json") == rebound,
                "protocol_sha256": sha256_file(config / "protocol.json")}
    r0_pinned = bool(r0) and pinned(observed["r0_binding_sha256"], expect["r0_binding_sha256"])
    conditions = {
        "smoke_rebuilt_from_source": rebuilt,
        "rows_at_most_1_percent": fraction is not None and fraction <= D3_FRACTION,
        "r0_binding_pinned": r0_pinned,
        "reference_matches_r0_binding": r0_pinned and r0.get("trainer_sha256") is not None and
                                        observed["reference_tree_binding_sha256"] == observed["r0_binding_sha256"] and
                                        all(modules[f] == r0[f] for f in modules),
        "environment_matches_r0_binding": r0_pinned and all(b.get(k) == r0.get(k) for b in runs.values()
                                                            for k in ENVIRONMENT),
        "source_matches_expected": rebuilt and r0_pinned and
                                   pinned(observed["source_manifest_sha256"], expect["source_manifest_sha256"],
                                          bound.get("MANIFEST.json")) and
                                   pinned(observed["source_weights_sha256"], expect["source_weights_sha256"],
                                          bound.get("weights.json")) and
                                   pinned(observed["train_block_sha256"], expect["train_block_sha256"]),
        "contract_matches_expected": r0_pinned and observed["smoke_contract_is_the_source_contract_rebound"] and
                                     pinned(observed["contract_sha256"], expect["contract_sha256"],
                                            r0.get("contract_sha256")),
        "protocol_matches_expected": r0_pinned and pinned(observed["protocol_sha256"], expect["protocol_sha256"],
                                                          r0.get("protocol_sha256")),
        "real_encoder": not args.synthetic_fixture}
    return conditions, observed, expect, fraction


# ------------------------------------------------------------------ trace (subprocess)

def exact(v):
    if type(v) is float:
        return v.hex()
    if isinstance(v, (bool, int, str)) or v is None:
        return v
    return repr(v)


def trace(args):
    """Run one trainer tree's fit with its optimizer step terms recorded (in its own process)."""
    sys.path.insert(0, str(Path(args.tree).resolve()))
    import train_full_a as trainer
    steps, real = [], trainer.train_step

    def traced(*a, **k):
        terms = real(*a, **k)
        steps.append(terms)  # fit() adds both pre-clip gradient norms to this dict after the call
        return terms
    trainer.train_step = traced
    fit_args = argparse.Namespace(
        data=args.data, config=args.config, artifact=args.artifact, init=args.init, out=args.out, arm=args.arm,
        seed=args.seed, device=args.device, report_split=None, consume_sealed_split=None, use_record=None,
        train_subset_rows=None, train_subset_rule=None, diagnostic_dev_subset_rows=None, init_fit=None,
        train_allowlist=None, final_step=False)
    if args.synthetic_fixture:
        # Offline tests only: the trees' own tiny random ModernBERT and whitespace tokenizer (test_full_a).
        from unittest import mock
        import test_full_a as fixture
        encoder = fixture.tiny_encoder(11)
        init = fixture.init_for(encoder)
        with mock.patch.object(trainer, "load", lambda a, p, d: (encoder, fixture.FakeTokenizer(),
                                                                 {"artifact_id": "synthetic-parent"})), \
                mock.patch.object(trainer, "initializer", lambda path: init):
            trainer.run(fit_args)
    else:
        trainer.run(fit_args)
    write_new(Path(args.trace), pretty({"steps": [{k: exact(v) for k, v in t.items()} for t in steps],
                                        "trainer": trainer.__file__}))


# ------------------------------------------------------------------ comparison

def neutral(v):
    return v is None or v is False or v == 0 or v == [] or v == {} or v == "0x0.0p+0"


def same(a, b):
    """Equal values of equal types; floats by their exact bits."""
    if type(a) is not type(b):
        return False
    return a.hex() == b.hex() if type(a) is float else a == b


def diff(a, b, path="", out=None):
    """Differences between two JSON values: key paths of differing values and of one-side keys that carry a
    non-neutral value. RECORD_ONLY keys are skipped."""
    out = [] if out is None else out
    if isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b)):
            if k in RECORD_ONLY:
                continue
            p = f"{path}/{k}"
            if k not in b:
                if not neutral(a[k]):
                    out.append(p + " (reference only)")
            elif k not in a:
                if not neutral(b[k]):
                    out.append(p + " (candidate only)")
            else:
                diff(a[k], b[k], p, out)
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            out.append(f"{path} (lengths {len(a)} / {len(b)})")
        for i, (x, y) in enumerate(zip(a, b)):
            diff(x, y, f"{path}/{i}", out)
    elif not same(a, b):
        out.append(path)
    return out


def section(differences, compared):
    return {"compared": compared, "differences": len(differences), "first": differences[:20]}


def read_json(path):
    return json.loads(Path(path).read_bytes())


def jsonl(path):
    return [json.loads(x) for x in Path(path).read_bytes().splitlines() if x.strip()]


def tensors(saved):
    return {f"{part}.{k}": v for part, state in saved["model"].items() for k, v in state.items()}


def compare_outputs(ref, cand, ref_trace, cand_trace):
    """Section results for two trainer output directories and their step traces."""
    ref, cand = Path(ref), Path(cand)
    for d in (ref, cand):
        for f in TRAINER_FILES:
            demand((d / f).exists(), f"{d.name}/{f} missing")
    out = {}
    a, b = read_json(ref_trace)["steps"], read_json(cand_trace)["steps"]
    demand(a, "the reference made no optimizer step")
    out["steps"] = section(diff(a, b, "/steps"), len(a)) | {
        "steps_differing": sum(1 for x, y in zip(a, b) if diff(x, y)) + abs(len(a) - len(b))}
    out["history"] = section(diff(read_json(ref / "history.json"), read_json(cand / "history.json"), "/history"),
                             len(read_json(ref / "history.json")))
    for name in ("initial-gate.json", "cohort-weights.json"):
        out[name] = section(diff(read_json(ref / name), read_json(cand / name), "/" + name), 1)
    fa, fb = read_json(ref / "fit.json"), read_json(cand / "fit.json")
    out["fit"] = section(diff({k: fa.get(k) for k in FIT_COMPUTED}, {k: fb.get(k) for k in FIT_COMPUTED}, "/fit"),
                         len(FIT_COMPUTED))
    ba, bb = read_json(ref / "binding.json"), read_json(cand / "binding.json")
    out["binding_inputs"] = section(diff({k: ba.get(k) for k in BINDING_INPUTS}, {k: bb.get(k) for k in BINDING_INPUTS},
                                         "/binding"), len(BINDING_INPUTS))
    for name in ("dev_iter-raw.jsonl", "dev_iter-predictions.jsonl"):
        ra, rb = jsonl(ref / name), jsonl(cand / name)
        # Rows are compared by position; ids are not written to the report.
        out[name] = section(diff([{k: v for k, v in r.items() if k != "id"} for r in ra],
                                 [{k: v for k, v in r.items() if k != "id"} for r in rb], "/" + name) +
                            ([] if [r.get("id") for r in ra] == [r.get("id") for r in rb] else ["/" + name + " (row order)"]),
                            len(ra))
    import torch
    sa = torch.load(ref / "selected.pt", map_location="cpu", weights_only=False)
    sb = torch.load(cand / "selected.pt", map_location="cpu", weights_only=False)
    ta, tb = tensors(sa), tensors(sb)
    bad = [f"/selected/{k} (reference only)" for k in sorted(set(ta) - set(tb))] + \
          [f"/selected/{k} (candidate only)" for k in sorted(set(tb) - set(ta))] + \
          [f"/selected/{k}" for k in sorted(set(ta) & set(tb))
           if ta[k].dtype != tb[k].dtype or ta[k].shape != tb[k].shape or not torch.equal(ta[k], tb[k])] + \
          [f"/selected/{k}" for k in ("evaluation", "epoch", "step", "global_step") if sa.get(k) != sb.get(k)]
    out["selected_checkpoint"] = section(bad, len(ta))
    from safetensors.torch import load_file
    for name in ("encoder.safetensors", "heads.safetensors"):
        la, lb = load_file(str(ref / name)), load_file(str(cand / name))
        bad = [f"/{name}/{k}" for k in sorted(set(la) ^ set(lb))] + \
              [f"/{name}/{k}" for k in sorted(set(la) & set(lb)) if not torch.equal(la[k], lb[k])]
        out[name] = section(bad, len(la)) | {"bytes_equal": sha256_file(ref / name) == sha256_file(cand / name)}
    return out


def compare(args):
    smoke = read_json(Path(args.data) / "MANIFEST.json")
    trees = {"reference": Path(args.reference_tree), "candidate": Path(args.candidate_tree)}
    records = {}
    for name, tree in trees.items():
        t = tree.parent / "TREE.json"
        records[name] = read_json(t) if t.exists() else {"files": {f.name: sha256_file(f) for f in sorted(tree.glob("*.py"))}}
    work, out = new_dir(args.out)
    try:
        for name, tree in trees.items():
            cmd = [sys.executable, "-I", "-B", str(Path(__file__).resolve()), "trace", "--tree", str(tree.resolve()),
                   "--data", str(Path(args.data).resolve()), "--config", str(Path(args.config).resolve()),
                   "--artifact", args.artifact, "--init", str(Path(args.init).resolve()), "--arm", args.arm,
                   "--seed", str(args.seed), "--device", args.device, "--out", str(work / name),
                   "--trace", str(work / f"{name}-steps.json")] + (["--synthetic-fixture"] if args.synthetic_fixture else [])
            with open(work / f"{name}.stdout", "wb") as so, open(work / f"{name}.stderr", "wb") as se:
                rc = subprocess.run(cmd, stdout=so, stderr=se, cwd=str(work)).returncode
            demand(rc == 0, f"the {name} trainer exited {rc} (see {name}.stderr)")
        sections = compare_outputs(work / "reference", work / "candidate", work / "reference-steps.json",
                                   work / "candidate-steps.json")
        equal = all(s["differences"] == 0 for s in sections.values())
        sel = smoke.get("selection") or {}
        conditions, observed, expect, fraction = d3_check(args, work, records["reference"])
        d3_pass = equal and all(conditions.values())
        report = {"schema": REPORT_SCHEMA, "bitwise_equal": equal, "d3_pass": d3_pass, "sections": sections,
                  "reference": {"commit": records["reference"].get("commit"), "files": records["reference"]["files"],
                                "binding_checked": records["reference"].get("binding")},
                  "candidate": {"commit": records["candidate"].get("commit"), "files": records["candidate"]["files"]},
                  "candidate_options": {"train_allowlist": None, "final_step": False, "train_subset_rows": None,
                                        "init_fit": None, "report_split": None},
                  "smoke": {"manifest_sha256": sha256_file(Path(args.data) / "MANIFEST.json"),
                            "smoke_only": smoke.get("smoke_only"), "selection": sel, "rows_fraction": fraction},
                  "run": {"arm": args.arm, "seed": args.seed, "device": args.device,
                          "synthetic_fixture": bool(args.synthetic_fixture),
                          "contract_sha256": sha256_file(Path(args.config) / "contract.json"),
                          "protocol_sha256": sha256_file(Path(args.config) / "protocol.json"),
                          "init_sha256": sha256_file(args.init)},
                  "d3_conditions": conditions, "d3_expected": expect, "d3_observed": observed,
                  "rule": "bitwise_equal: every section has zero differences (exact float bits; one-side keys "
                          "neutral; RECORD_ONLY keys not compared); d3_pass: bitwise_equal and every D3 condition",
                  "record_only": list(RECORD_ONLY)}
        write_new(work / "BITWISE.json", pretty(report))
        os.rename(work, out)
        print(json.dumps({"bitwise_equal": equal, "d3_pass": d3_pass,
                          "differences": {k: v["differences"] for k, v in sections.items()},
                          "d3_conditions": conditions}), flush=True)
        return report
    except BaseException:
        # Kept for inspection beside the requested output, which stays absent.
        failed = out.with_name(out.name + ".failed")
        if failed.exists():
            shutil.rmtree(work, ignore_errors=True)
        else:
            os.rename(work, failed)
        raise


def main(argv=None):
    os.umask(0o077)
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    e = sub.add_parser("extract", help="trainer modules of a commit, verified against a fit binding")
    e.add_argument("--repo", required=True)
    e.add_argument("--commit", required=True)
    e.add_argument("--binding", default=None)
    e.add_argument("--out", required=True)
    s = sub.add_parser("smoke-bundle", help="smoke trainer bundle from a trainer bundle's Train block")
    s.add_argument("--source", required=True)
    s.add_argument("--contract", required=True)
    s.add_argument("--protocol", required=True)
    s.add_argument("--fraction", type=float, default=D3_FRACTION)
    s.add_argument("--out", required=True)
    for name in ("trace", "compare"):
        c = sub.add_parser(name)
        if name == "trace":
            c.add_argument("--tree", required=True)
            c.add_argument("--trace", required=True)
        else:
            c.add_argument("--reference-tree", required=True)
            c.add_argument("--candidate-tree", required=True)
        for k in ("data", "config", "artifact", "init", "arm", "out"):
            c.add_argument("--" + k, required=True)
        c.add_argument("--seed", type=int, required=True)
        c.add_argument("--device", choices=("cpu", "cuda"), default="cuda")
        c.add_argument("--synthetic-fixture", action="store_true", help="offline tests only")
        if name == "compare":
            c.add_argument("--source", default=None, help="the trainer bundle the smoke was built from (rebuilt)")
            c.add_argument("--source-contract", default=None, help="the contract the smoke config was rebound from")
            c.add_argument("--r0-binding", default=None, help="R0's F-17 binding.json")
            for k in D3_EXPECT:
                c.add_argument("--expect-" + k.replace("_", "-"), dest="expect_" + k, default=None)
    a = p.parse_args(argv)
    if a.cmd == "extract":
        r = extract(a.repo, a.commit, a.out, a.binding)
        print(json.dumps({"commit": r["commit"], "files": len(r["files"]), "checked": sorted(r["binding"]["checked"])
                          if r["binding"] else []}))
    elif a.cmd == "smoke-bundle":
        r = smoke_bundle(a.source, a.contract, a.protocol, a.out, a.fraction)
        print(json.dumps({"manifest_sha256": r["manifest_sha256"], "selection": r["selection"]}))
    elif a.cmd == "trace":
        trace(a)
    else:
        r = compare(a)
        sys.exit(EXIT_D3_PASS if r["d3_pass"] else EXIT_EQUAL_NOT_D3 if r["bitwise_equal"] else EXIT_DIFFERENT)


if __name__ == "__main__":
    main()

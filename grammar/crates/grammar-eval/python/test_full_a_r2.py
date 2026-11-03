"""Offline protocol A2 trainer checks (B19): the block reader of trainer bundles,
export and selection of block-bound fits, the Train row allow-list, the
final-step mode, and the D3 bitwise smoke harness (with R0's own trainer as
the reference when its commit is in the clone).

Synthetic word sequences and a tiny random ModernBERT only. No corpus, artifact,
provider, detector or network access. CPU only.
"""
import argparse
import copy
import hashlib
import json
import math
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import torch

import bitwise_full_a as harness
import export_full_a as exporter
import full_a_bundle as bundle
import public_a
import synthetic_full_a as synthetic
import test_full_a as base
import train_full_a as trainer
import v6_embeddings as bridge

HERE = Path(__file__).resolve().parent
GARBAGE = b"\xff\xfe sealed bytes the reader must never parse\n"
POPULATION = "ab" * 32
# R0 (F-17): its trainer commit and the module digests its fit binding records (spans_rule_sha256 null: the
# commit has no full_a_spans.py), and its F protocol's Train weighting.
R0_TRAINER_COMMIT = "23f24262dbc02d75b79d1dad1cbef2948cdc6a6a"
R0_MODULE_DIGESTS = {
    "trainer_sha256": "a16ab860b3ea7c795a9dc02e00d8fd9240a945729fc2cebe6be3535fd4eaef94",
    "model_sha256": "e8ab5a6a2a4e57774f6e3fe448e42ef45b7e1c3dddd6b44131d65b0fe80732ee",
    "aux_sha256": "70292cea49f2ac84ef3eef54cabe57ad58f1752dbb3fa4191494eb6d88c3160f",
    "bundle_reader_sha256": "933d0dfd6dc033c5085d9388f9c2657edc52299869ede736f5880396529ceafb",
    "architecture_sha256": "29dd3afe542459497b4b662f894df5d82b271f10eb0371f1b0f5155a7ace53d4",
    "shared_loss_sha256": "f149d8f44f474483240b2085095b689e9c08603f2c6c84acb8bfa0a357132480",
    "shared_heads_implementation_sha256": "703be27ac6968eeb0a70cd9338690ec9f4b980db4971b93d82bc1ba022e9f927",
    "artifact_loader_sha256": "7fef2bdc62a895e34bf6ca3c81b36ebd9a6ca8949d3fa018f9aa47449015cbab",
    "spans_rule_sha256": None}
R0_WEIGHTING = {"scheme": bundle.ROW_WEIGHT_SCHEME, "positive_mass": None, "max_row_weight_ratio": 5.0,
                "max_row_weight_global_ratio": 5.0, "cohort_positive_mass_caps": {"raid-v1-": 0.2}}


def sha(b):
    return hashlib.sha256(b).hexdigest()


def weighted_rows(groups=2, splits=("train", "dev_iter")):
    rows = synthetic.factorial_rows(groups=groups, splits=splits)
    for i, r in enumerate(rows):
        if r["split"] == "train":
            r["cohort_weight"] = 0.5 + (i % 5)
    return rows


def write_trainer_bundle(root, rows, sealed=None):
    """A trainer bundle in the Rust writer's layout: blocks in BLOCK_ORDER, ids sorted within a block, then
    `sealed` {split: rows} blocks of unparseable bytes (they stand in for old Dev-select and CAL)."""
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)
    order = {s: i for i, s in enumerate(bundle.BLOCK_ORDER)}
    rows = sorted(rows, key=lambda r: (order[r["split"]], r["id"]))
    counts = {}
    body = b""
    for r in rows:
        counts[r["split"]] = counts.get(r["split"], 0) + 1
        body += json.dumps(r).encode() + b"\n"
    for split in bundle.BLOCK_ORDER:
        n = (sealed or {}).get(split)
        if n:
            counts[split] = n
            body += GARBAGE * n
    (root / bundle.ROWS).write_bytes(body)
    (root / bundle.WEIGHTS).write_text(json.dumps(synthetic.row_weights_doc(rows)))
    (root / bundle.MANIFEST).write_text(json.dumps({
        "schema": bundle.TRAINER_MANIFEST_SCHEMA, "smoke_only": False, "rows_by_split": counts,
        "files": {"rows.jsonl": sha(body), "weights.json": sha((root / bundle.WEIGHTS).read_bytes())},
        "source_splits": {"train": {"rows_sha256": POPULATION}}}))
    return rows


def r0_shaped_rows(groups=4):
    """Train weights under which each of R0's caps moves weight: a mixed outlier above 5x its class mean (and
    below the global cap), a human above 5x the global mean, and a raid-v1- cohort over 20% of AI mass."""
    rows = weighted_rows(groups=groups)
    humans, outlier = 0, False
    for r in rows:
        if r["split"] != "train":
            continue
        if r["origin"] == "human_only":
            r["cohort_weight"] = 40. if humans == 0 else 10.
            humans += 1
        elif r["generator_label"] == "claude-x":
            r["cohort_id"], r["cohort_weight"] = "raid-v1-synthetic", 3.
        elif r["origin"] == "mixed" and not outlier:
            r["cohort_weight"], outlier = 30., True
        else:
            r["cohort_weight"] = 1. + (len(r["id"]) % 3) / 7
    return rows


def run_environment():
    """The environment fields a CPU fit with test_full_a's encoder records in its binding."""
    encoder = base.tiny_encoder(11)
    return {"torch": torch.__version__, "cuda": torch.version.cuda, "device": "cpu",
            "transformers_model_source_sha256": bridge.sha256(__import__(encoder.__module__, fromlist=["x"]).__file__)}


def row_protocol():
    p = synthetic.protocol_template()
    p["train_weighting"] = {"scheme": bundle.ROW_WEIGHT_SCHEME, "positive_mass": None}
    return p


def write_config(config, root, rows, protocol=None):
    labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
    synthetic.write_config(config, sha((Path(root) / bundle.MANIFEST).read_bytes()), labels, protocol or row_protocol())
    (Path(config) / "init.json").write_text("{}")
    return labels


def write_allowlist(path, ids, population=POPULATION, **override):
    ids = sorted(ids)
    doc = {"schema": bundle.ALLOWLIST_SCHEMA, "filter": "jitem_filter_v1", "excludes": ["J1", "J2", "J4"],
           "population": "train0", "split": "train", "rows": {"path": "synthetic", "sha256": population, "rows": 0},
           "rules_sha256": "cd" * 32, "kept": {}, "count": len(ids),
           "ids_sha256": sha("".join(x + "\n" for x in ids).encode()), "ids": ids}
    doc.update(override)
    Path(path).write_text(json.dumps(doc, indent=1))
    return path


def fit(root, config, out, arm="E1", interrupt_after=None, **extra):
    """One tiny CPU fit through the real trainer (test_full_a's encoder and tokenizer)."""
    encoder = base.tiny_encoder(11)
    init = base.init_for(encoder)
    saves = {"n": 0}
    real_save = trainer.atomic_save

    def save(path, value):
        real_save(path, value)
        if path.name == "recovery.pt":
            saves["n"] += 1
            if interrupt_after is not None and saves["n"] == interrupt_after:
                raise KeyboardInterrupt("synthetic interruption")
    fields = {"data": str(root), "config": str(config), "artifact": "unused", "init": str(Path(config) / "init.json"),
              "out": str(out), "arm": arm, "seed": 17, "device": "cpu", "report_split": None,
              "consume_sealed_split": None, "use_record": None, "train_subset_rows": None, "init_fit": None,
              "train_allowlist": None, "final_step": False}
    fields.update(extra)
    with mock.patch.object(trainer, "load", lambda a, p, d: (encoder, base.FakeTokenizer(),
                                                             {"artifact_id": "synthetic-parent"})), \
            mock.patch.object(trainer, "initializer", lambda path: init), \
            mock.patch.object(trainer, "atomic_save", save):
        trainer.run(type("Args", (), fields))


def read(path):
    return json.loads(Path(path).read_text())


def model_tensors(path, key="model"):
    saved = torch.load(path, map_location="cpu", weights_only=False)[key]
    return {f"{p}.{k}": v for p, s in saved.items() for k, v in s.items()}


class BlockReader(unittest.TestCase):
    def test_requested_blocks_only_and_sealed_blocks_never_parsed_or_hashed(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "b"
            rows = write_trainer_bundle(root, weighted_rows(), sealed={"dev_select": 3, "cal": 2})
            self.assertEqual(bundle.layout(root), [("train", 20), ("dev_iter", 20), ("dev_select", 3), ("cal", 2)])
            for split in ("train", "dev_iter"):
                got = [m.id for m, _ in bundle.stream(root, [split])]
                self.assertEqual(got, [r["id"] for r in rows if r["split"] == split])
            digests = bundle.bound_digests(root, ["train", "dev_iter"])
            self.assertNotIn(bundle.ROWS, digests)
            body = (root / bundle.ROWS).read_bytes().splitlines(keepends=True)
            self.assertEqual(digests["rows.jsonl_blocks_read"], {
                "train": {"rows": 20, "sha256": sha(b"".join(body[:20]))},
                "dev_iter": {"rows": 20, "sha256": sha(b"".join(body[20:40]))}})
            self.assertEqual(digests["rows.jsonl_recorded_in_manifest"], read(root / bundle.MANIFEST)["files"]["rows.jsonl"])
            self.assertEqual(digests[bundle.MANIFEST], sha((root / bundle.MANIFEST).read_bytes()))
            # Other sealed bytes leave every bound digest unchanged; the whole-file digest would move.
            whole = bundle.bundle_digests(root)
            (root / bundle.ROWS).write_bytes(b"".join(body[:40]) + b"\x00 other sealed bytes\n" * 5)
            self.assertEqual(bundle.bound_digests(root, ["train", "dev_iter"]), digests)
            self.assertNotEqual(bundle.bundle_digests(root)[bundle.ROWS], whole[bundle.ROWS])

    def test_whole_file_digests_without_an_unrequested_sealed_block(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "b"
            write_trainer_bundle(root, weighted_rows())
            self.assertEqual(bundle.bound_digests(root, ["train", "dev_iter"]), bundle.bundle_digests(root))
            plain = Path(d) / "plain"
            rows = weighted_rows()
            synthetic.write_bundle(plain, rows, synthetic.row_weights_doc(rows))
            self.assertIsNone(bundle.layout(plain))
            self.assertEqual(bundle.bound_digests(plain, ["train", "dev_iter"]), bundle.bundle_digests(plain))

    def test_layout_refusals(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "b"
            write_trainer_bundle(root, weighted_rows(), sealed={"dev_select": 3, "cal": 2})
            with self.assertRaisesRegex(ValueError, "unrequested sealed dev_select"):
                list(bundle.stream(root, ["cal"]))
            with self.assertRaisesRegex(ValueError, "unrequested sealed dev_select"):
                bundle.bound_digests(root, ["train", "cal"])
            write_trainer_bundle(root, weighted_rows())
            m = read(root / bundle.MANIFEST)
            for counts, message in (({"train": 21, "dev_iter": 19}, "line 21: a dev_iter line in the MANIFEST's train block"),
                                    ({"train": 19, "dev_iter": 21}, "line 20: a train line in the MANIFEST's dev_iter block"),
                                    ({"train": 20, "dev_iter": 30}, "ends inside the dev_iter block"),
                                    ({"train": 20, "dev_iter": 20, "test": 5}, "Test rows"),
                                    ({"train": 20, "dev_iter": 20, "other": 5}, "rows_by_split")):
                (root / bundle.MANIFEST).write_text(json.dumps(dict(m, rows_by_split=counts)))
                with self.assertRaisesRegex(ValueError, message):
                    list(bundle.stream(root, ["dev_iter"]))
            # Lines after the last declared block are refused when that block is read.
            write_trainer_bundle(root, weighted_rows())
            with (root / bundle.ROWS).open("ab") as f:
                f.write(b'{"id": "extra"}\n')
            with self.assertRaisesRegex(ValueError, "after the MANIFEST's blocks"):
                list(bundle.stream(root, ["dev_iter"]))
            self.assertEqual(len(list(bundle.stream(root, ["train"]))), 20)

    def test_an_overstated_count_is_refused_before_a_sealed_line_is_parsed_or_hashed(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "b"
            write_trainer_bundle(root, weighted_rows(splits=("train", "dev_iter", "dev_select")))
            m = read(root / bundle.MANIFEST)
            # Dev-iter overstated by one: its last counted line is the first Dev-select row (valid JSON here).
            (root / bundle.MANIFEST).write_text(json.dumps(dict(m, rows_by_split={"train": 20, "dev_iter": 21,
                                                                                  "dev_select": 19})))
            real, parsed = json.loads, []

            def spy(b, *a, **k):
                parsed.append(b)
                return real(b, *a, **k)
            hashed = []
            real_sha = bundle.hashlib.sha256

            class Spy:
                def __init__(self, *a):
                    self.h = real_sha(*a)

                def update(self, b):
                    hashed.append(b)
                    self.h.update(b)

                def hexdigest(self):
                    return self.h.hexdigest()
            with mock.patch.object(bundle.json, "loads", spy), mock.patch.object(bundle.hashlib, "sha256", Spy):
                with self.assertRaisesRegex(ValueError, "line 41: a dev_select line in the MANIFEST's dev_iter block"):
                    list(bundle.stream(root, ["dev_iter"]))
                with self.assertRaisesRegex(ValueError, "line 41: a dev_select line in the MANIFEST's dev_iter block"):
                    bundle.bound_digests(root, ["train", "dev_iter"])
            sealed = [b for b in parsed + hashed if isinstance(b, bytes) and b'"split": "dev_select"' in b]
            self.assertEqual(sealed, [])
            self.assertTrue(any(isinstance(b, bytes) and b'"split": "dev_iter"' in b for b in parsed))
            self.assertTrue(any(b'"split": "dev_iter"' in b for b in hashed))
            # The byte check reads a row's own split key only.
            self.assertEqual(bundle.line_split(b'{"text": "a \\"split\\": \\"cal\\" b", "split": "train"}\n'), "train")
            self.assertEqual(bundle.line_split(b'{"source_split": "cal", "split" : "dev-iter"}\n'), "dev_iter")
            self.assertIsNone(bundle.line_split(GARBAGE))

    def test_fit_on_a_bundle_with_sealed_blocks_binds_block_digests(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "b", Path(d) / "config"
            rows = write_trainer_bundle(root, weighted_rows(), sealed={"dev_select": 4, "cal": 3})
            write_config(config, root, rows)
            fit(root, config, Path(d) / "fit")
            b = read(Path(d) / "fit" / "fit.json")["binding"]
            self.assertEqual(b["bundle_sha256"], bundle.bound_digests(root, ["train", "dev_iter"]))
            self.assertEqual(set(b["bundle_sha256"]["rows.jsonl_blocks_read"]), {"train", "dev_iter"})
            # The same rows without the sealed blocks train the same weights.
            plain, pconf = Path(d) / "plain", Path(d) / "pconf"
            write_trainer_bundle(plain, weighted_rows())
            write_config(pconf, plain, rows)
            fit(plain, pconf, Path(d) / "pfit")
            self.assertEqual((Path(d) / "fit" / "encoder.safetensors").read_bytes(),
                             (Path(d) / "pfit" / "encoder.safetensors").read_bytes())


class BlockBoundFits(unittest.TestCase):
    def test_export_and_selection_recompute_the_bound_digests(self):
        with tempfile.TemporaryDirectory() as d:
            root, config, out = Path(d) / "b", Path(d) / "config", Path(d) / "fit"
            rows = write_trainer_bundle(root, weighted_rows(), sealed={"dev_select": 3, "cal": 2})
            write_config(config, root, rows)
            fit(root, config, out)
            bound = read(out / "binding.json")["bundle_sha256"]
            self.assertIn(bundle.BLOCKS_READ, bound)
            self.assertEqual(bundle.binding_digests(root, bound), bound)
            digests = exporter.completed_fit(root, config, out, config / "init.json")[0]
            self.assertEqual(digests, bound)
            public_a.select(argparse.Namespace(fit=str(out), data=str(root), out=str(Path(d) / "sel1.json")))
            self.assertEqual(read(Path(d) / "sel1.json")["bundle_sha256"], bound)
            # The unopened blocks are never read: other sealed bytes leave both checks passing.
            body = (root / bundle.ROWS).read_bytes().splitlines(keepends=True)
            (root / bundle.ROWS).write_bytes(b"".join(body[:40]) + b"\x00 other sealed bytes\n" * 5)
            self.assertEqual(exporter.completed_fit(root, config, out, config / "init.json")[0], bound)
            # A changed Train line is refused by both; so is a binding that names no block the fit read.
            changed = body[0].replace(b'"cohort_weight": ', b'"cohort_weight":  ', 1)
            self.assertNotEqual(changed, body[0])
            (root / bundle.ROWS).write_bytes(changed + b"".join(body[1:]))
            with self.assertRaisesRegex(ValueError, "completed fit on this bundle and config required"):
                exporter.completed_fit(root, config, out, config / "init.json")
            with self.assertRaisesRegex(SystemExit, "bound to another bundle"):
                public_a.select(argparse.Namespace(fit=str(out), data=str(root), out=str(Path(d) / "sel2.json")))
            with self.assertRaisesRegex(ValueError, "block digests"):
                bundle.binding_digests(root, dict(bound, **{bundle.BLOCKS_READ: {}}))
            # CAL lies behind the unopened Dev-select block: no route reads it from this bundle.
            with self.assertRaisesRegex(ValueError, "unrequested sealed dev_select"):
                list(bundle.stream(root, ["cal"]))
            # A whole-file fit is checked on the whole files, as before.
            plain, pconf, pout = Path(d) / "plain", Path(d) / "pconf", Path(d) / "pfit"
            write_trainer_bundle(plain, weighted_rows())
            write_config(pconf, plain, rows)
            fit(plain, pconf, pout)
            self.assertEqual(read(pout / "binding.json")["bundle_sha256"], bundle.bundle_digests(plain))
            self.assertEqual(exporter.completed_fit(plain, pconf, pout, pconf / "init.json")[0],
                             bundle.bundle_digests(plain))

    def test_a_refused_report_split_reads_no_bundle_line(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "b", Path(d) / "config"
            rows = write_trainer_bundle(root, weighted_rows(), sealed={"dev_select": 3, "cal": 2})
            write_config(config, root, rows)
            used = Path(d) / "used.json"
            used.write_text("{}")
            seen, real = [], bundle.block_lines

            def spy(f, plan):
                for item in real(f, plan):
                    seen.append(item[0])
                    yield item
            with mock.patch.object(bundle, "block_lines", spy):
                for extra, message in (
                        ({"report_split": "dev_select"}, "needs --consume-sealed-split"),
                        ({"report_split": "dev_select", "consume_sealed_split": "dev_select", "use_record": str(used)},
                         "needs --consume-sealed-split"),
                        ({"report_split": "cal", "consume_sealed_split": "dev_select",
                          "use_record": str(Path(d) / "new.json")}, "needs --consume-sealed-split"),
                        ({"report_split": "test"}, "report split must be"),
                        ({"consume_sealed_split": "cal"}, "without --report-split")):
                    with self.assertRaisesRegex(ValueError, message):
                        fit(root, config, Path(d) / "x", **extra)
            self.assertEqual(seen, [])
            self.assertFalse((Path(d) / "new.json").exists())


class AllowList(unittest.TestCase):
    def test_reader_validation(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "b"
            write_trainer_bundle(root, weighted_rows())
            good = write_allowlist(Path(d) / "a.json", ["train-h0", "train-gen-a-0-0"])
            record, ids = bundle.read_allowlist(good, root)
            self.assertEqual(ids, ["train-gen-a-0-0", "train-h0"])
            self.assertEqual((record["count"], record["population_rows_sha256"]), (2, POPULATION))
            self.assertEqual(record["sha256"], sha(Path(good).read_bytes()))
            for override, message in (({"ids": ["train-h0", "train-gen-a-0-0"]}, "ascending"),
                                      ({"ids": ["train-h0", "train-h0"]}, "ascending"),
                                      ({"count": 3}, "count or ids digest"),
                                      ({"ids_sha256": "00" * 32}, "count or ids digest"),
                                      ({"schema": "other"}, "allow-list is required"),
                                      ({"split": "dev_iter"}, "allow-list is required"),
                                      ({"ids": []}, "allow-list ids"),
                                      ({"rows": {"sha256": "ef" * 32}}, "Train source")):
                doc = read(good)
                doc.update(override)
                bad = Path(d) / "bad.json"
                bad.write_text(json.dumps(doc))
                with self.assertRaisesRegex(ValueError, message):
                    bundle.read_allowlist(bad, root)

    def test_allowlisted_fit_trains_kept_rows_at_their_weights_and_keeps_the_roster(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "b", Path(d) / "config"
            rows = write_trainer_bundle(root, weighted_rows(), sealed={"dev_select": 2})
            labels = write_config(config, root, rows)
            train = [r for r in rows if r["split"] == "train"]
            kept = [r for r in train if r["generator_label"] != "claude-x" and r["id"] != "train-h1"]
            allow = write_allowlist(Path(d) / "a.json", [r["id"] for r in kept])
            out = Path(d) / "fit"
            fit(root, config, out, train_allowlist=str(allow))
            f = read(out / "fit.json")
            self.assertEqual(f["train_population"]["documents"], len(kept))
            self.assertEqual(f["train_allowlist"], f["binding"]["train_allowlist"])
            self.assertEqual(f["train_allowlist"]["count"], len(kept))
            self.assertEqual(f["ai_roster"]["rule"], "train_labels")
            self.assertEqual(f["ai_roster"]["unsupervised"], ["claude-x"])
            self.assertEqual(f["ai_roster"]["supervised"], [x for x in labels if x != "claude-x"])
            log = read(out / "cohort-weights.json")
            counted = sum(e["records"] for c in log["cohorts"].values() for e in c["labels"].values())
            trained = sum(e["trained_records"] for c in log["cohorts"].values() for e in c["labels"].values())
            self.assertEqual((counted, trained), (len(train), len(kept)))
            total = sum(r["cohort_weight"] for r in kept)
            human = sum(r["cohort_weight"] for r in kept if r["origin"] == "human_only") / total
            self.assertAlmostEqual(log["effective_mass_by_origin"]["human_only"], human, places=12)
            preds = [json.loads(x) for x in (out / "dev_iter-predictions.jsonl").read_text().splitlines()]
            self.assertTrue(all(sorted(p["attribution"]) == labels for p in preds))

    def test_full_allowlist_trains_exactly_as_no_allowlist(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "b", Path(d) / "config"
            rows = write_trainer_bundle(root, weighted_rows())
            write_config(config, root, rows)
            allow = write_allowlist(Path(d) / "a.json", [r["id"] for r in rows if r["split"] == "train"])
            fit(root, config, Path(d) / "plain")
            fit(root, config, Path(d) / "allow", train_allowlist=str(allow))
            for name in ("encoder.safetensors", "heads.safetensors", "dev_iter-raw.jsonl"):
                self.assertEqual((Path(d) / "plain" / name).read_bytes(), (Path(d) / "allow" / name).read_bytes())
            a, b = read(Path(d) / "plain" / "history.json"), read(Path(d) / "allow" / "history.json")
            self.assertEqual([h["train_terms_mean_since_last_evaluation"] for h in a],
                             [h["train_terms_mean_since_last_evaluation"] for h in b])
            self.assertNotIn("train_allowlist", read(Path(d) / "plain" / "binding.json"))

    def test_refusals(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "b", Path(d) / "config"
            rows = write_trainer_bundle(root, weighted_rows())
            write_config(config, root, rows)
            ids = [r["id"] for r in rows if r["split"] == "train"]
            with self.assertRaisesRegex(ValueError, "not positive-weight Train rows"):
                fit(root, config, Path(d) / "x1", train_allowlist=str(write_allowlist(Path(d) / "a1.json",
                                                                                      ids[:3] + ["train-unknown"])))
            with self.assertRaisesRegex(ValueError, "not positive-weight Train rows"):
                fit(root, config, Path(d) / "x2", train_allowlist=str(write_allowlist(Path(d) / "a2.json",
                                                                                      ids[:3] + ["dev_iter-h0"])))
            with self.assertRaisesRegex(ValueError, "exclusive"):
                fit(root, config, Path(d) / "x3", train_allowlist=str(write_allowlist(Path(d) / "a3.json", ids)),
                    train_subset_rows=10)
            humans = [r["id"] for r in rows if r["split"] == "train" and r["origin"] == "human_only"]
            one = [r["id"] for r in rows if r["split"] == "train" and r["generator_label"] == "gen-a"]
            with self.assertRaisesRegex(ValueError, "fewer than two AI profiles"):
                fit(root, config, Path(d) / "x4", train_allowlist=str(write_allowlist(Path(d) / "a4.json", humans + one)))


class FinalStep(unittest.TestCase):
    def test_final_step_equals_the_selection_run_at_its_last_step_and_scores_dev_iter_once(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "b", Path(d) / "config"
            rows = write_trainer_bundle(root, weighted_rows())
            write_config(config, root, rows)
            selection, final = Path(d) / "selection", Path(d) / "final"
            fit(root, config, selection)
            real, calls = trainer.evaluate, []

            def counted(model, population, *a, **k):
                calls.append((population.split, k.get("export", False)))
                return real(model, population, *a, **k)
            with mock.patch.object(trainer, "evaluate", counted):
                fit(root, config, final, final_step=True)
            self.assertEqual(calls, [("dev_iter", True)])
            # Training is unchanged: the final model equals the selection run's last state.
            a, b = model_tensors(selection / "recovery.pt"), model_tensors(final / "selected.pt")
            self.assertEqual(set(a), set(b))
            self.assertTrue(all(torch.equal(a[k], b[k]) for k in a))
            hs, log = read(selection / "history.json"), read(final / "train-log.json")
            self.assertEqual([h["train_terms_mean_since_last_evaluation"] for h in hs[1:]],
                             [r["train_terms_mean_since_last_boundary"] for r in log])
            self.assertEqual([h["learning_rates"] for h in hs[1:]], [r["learning_rates"] for r in log])
            self.assertEqual([h["global_step"] for h in hs[1:]], [r["global_step"] for r in log])
            history = read(final / "history.json")
            self.assertEqual(len(history), 1)
            self.assertEqual(history[0]["dev_iter_involvement_brier"].hex(), hs[-1]["dev_iter_involvement_brier"].hex())
            f = read(final / "fit.json")
            self.assertEqual(f["checkpoint_rule"], trainer.FINAL_STEP_RULE)
            self.assertEqual(f["binding"]["checkpoint_rule"], trainer.FINAL_STEP_RULE)
            self.assertEqual((f["selection_split"], f["selection_metric"], f["selection_value"]), (None, None, None))
            self.assertFalse(f["selection_score_is_biased"])
            self.assertEqual((f["evaluations"], f["stopped_early"], f["selected_evaluation"]), (1, False, 0))
            self.assertEqual(f["selected_global_step"], f["schedule"]["total_scheduled_steps"])
            self.assertEqual(f["selected_epoch"], 2)
            self.assertEqual(f["train_log"]["boundaries"], len(log))
            self.assertFalse((final / "best.pt").exists())
            self.assertEqual(torch.load(final / "selected.pt", map_location="cpu", weights_only=False)["evaluation"], 0)
            self.assertTrue(f["limitations"][0].startswith("Final-step checkpoint"))
            self.assertNotIn("checkpoint_rule", read(selection / "fit.json"))

    def test_resume_and_refusals(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "b", Path(d) / "config"
            rows = write_trainer_bundle(root, weighted_rows())
            write_config(config, root, rows)
            whole, broken = Path(d) / "whole", Path(d) / "broken"
            fit(root, config, whole, final_step=True)
            with self.assertRaises(KeyboardInterrupt):
                fit(root, config, broken, final_step=True, interrupt_after=3)
            self.assertFalse((broken / "history.json").exists())
            fit(root, config, broken, final_step=True)
            for name in ("encoder.safetensors", "heads.safetensors", "dev_iter-raw.jsonl", "train-log.json"):
                if name == "train-log.json":
                    strip = lambda rows: [{k: v for k, v in r.items() if k != "elapsed_seconds_this_invocation"}
                                          for r in rows]
                    self.assertEqual(strip(read(whole / name)), strip(read(broken / name)))
                else:
                    self.assertEqual((whole / name).read_bytes(), (broken / name).read_bytes())
            # A resume never mixes modes: the binding records the checkpoint rule.
            with self.assertRaisesRegex(ValueError, "resume binding differs"):
                with self.assertRaises(KeyboardInterrupt):
                    fit(root, config, Path(d) / "mixed", final_step=True, interrupt_after=2)
                fit(root, config, Path(d) / "mixed")
            with self.assertRaisesRegex(ValueError, "--final-step takes no --init-fit"):
                fit(root, config, Path(d) / "init", final_step=True, init_fit=str(whole))


class P0Shape(unittest.TestCase):
    def test_allowlist_final_step_fit_on_a_bundle_with_sealed_blocks(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "b", Path(d) / "config"
            rows = write_trainer_bundle(root, weighted_rows(groups=3), sealed={"dev_select": 5, "cal": 5})
            write_config(config, root, rows)
            kept = [r["id"] for r in rows if r["split"] == "train" and r["generator_label"] != "claude-x"]
            out = Path(d) / "p0"
            fit(root, config, out, train_allowlist=str(write_allowlist(Path(d) / "a.json", kept)), final_step=True)
            f = read(out / "fit.json")
            self.assertEqual((f["train_population"]["documents"], f["evaluations"]), (len(kept), 1))
            self.assertEqual(set(f["binding"]["bundle_sha256"]["rows.jsonl_blocks_read"]), {"train", "dev_iter"})
            self.assertEqual(f["dev_iter_population"]["documents"], sum(r["split"] == "dev_iter" for r in rows))


class Bitwise(unittest.TestCase):
    def source_bundle(self, d, groups=4):
        root, config = Path(d) / "source", Path(d) / "source-config"
        rows = write_trainer_bundle(root, weighted_rows(groups=groups, splits=("train", "dev_iter", "dev_select")))
        # Every block after Train is overwritten with unparseable bytes: the builder reads the Train block only.
        n = sum(r["split"] == "train" for r in rows)
        body = (root / bundle.ROWS).read_bytes().splitlines(keepends=True)
        (root / bundle.ROWS).write_bytes(b"".join(body[:n]) + GARBAGE * (len(body) - n))
        write_config(config, root, rows)
        return root, config, rows

    def test_smoke_bundle_reads_the_train_block_only_and_keeps_its_lines(self):
        with tempfile.TemporaryDirectory() as d:
            root, config, rows = self.source_bundle(d)
            out = Path(d) / "smoke"
            rec = harness.smoke_bundle(root, config / "contract.json", config / "protocol.json", out, fraction=0.5)
            train_lines = (root / bundle.ROWS).read_bytes().splitlines(keepends=True)[:40]
            sel = rec["selection"]
            self.assertEqual(sel["train_lines"], 40)
            self.assertLessEqual(sel["train_rows"] + sel["dev_iter_rows"], math.floor(0.5 * 40))
            m = read(out / "bundle" / "MANIFEST.json")
            self.assertTrue(m["smoke_only"])
            self.assertEqual(m["source"]["train_block"]["sha256"], sha(b"".join(train_lines)))
            smoke_rows = (out / "bundle" / bundle.ROWS).read_bytes().splitlines(keepends=True)
            t, v = m["rows_by_split"]["train"], m["rows_by_split"]["dev_iter"]
            self.assertTrue(set(smoke_rows[:t]) <= set(train_lines))
            dev = [json.loads(x) for x in smoke_rows[t:t + v]]
            train = [json.loads(x) for x in smoke_rows[:t]]
            self.assertEqual({r["origin"] for r in dev}, {"human_only", "model_only", "mixed"})
            self.assertTrue(all(r["split"] == "dev_iter" and r["cohort_weight"] == 1.0 for r in dev))
            self.assertFalse({r["source_group"] for r in dev} & {r["source_group"] for r in train})
            self.assertEqual({r["generator_label"] for r in train} - {None},
                             {r["generator_label"] for r in rows} - {None})
            self.assertEqual([r["id"] for r in train], sorted(r["id"] for r in train))
            k = read(out / "config" / "contract.json")
            self.assertEqual(k["bundle_manifest_sha256"], sha((out / "bundle" / "MANIFEST.json").read_bytes()))
            self.assertEqual((out / "config" / "protocol.json").read_bytes(), (config / "protocol.json").read_bytes())
            # The trainer accepts the smoke bundle (weights.json, roster, carve disjointness).
            (out / "config" / "init.json").write_text("{}")
            fit(out / "bundle", out / "config", Path(d) / "fit")
            self.assertEqual(read(Path(d) / "fit" / "fit.json")["train_population"]["documents"], t)
            # Deterministic, and refused when the rows cannot hold one per stratum or the output exists.
            again = harness.smoke_bundle(root, config / "contract.json", config / "protocol.json", Path(d) / "s2", 0.5)
            self.assertEqual(again["manifest_sha256"], rec["manifest_sha256"])
            with self.assertRaisesRegex(ValueError, "strata"):
                harness.smoke_bundle(root, config / "contract.json", config / "protocol.json", Path(d) / "s3", 0.3)
            with self.assertRaisesRegex(ValueError, "written once"):
                harness.smoke_bundle(root, config / "contract.json", config / "protocol.json", out, 0.5)

    def compare(self, d, reference, smoke, name, data=None, config=None, extra=()):
        data, config = data or smoke / "bundle", config or smoke / "config"
        cmd = [sys.executable, "-I", "-B", str(HERE / "bitwise_full_a.py"), "compare", "--reference-tree", str(reference),
               "--candidate-tree", str(HERE), "--data", str(data), "--config", str(config),
               "--artifact", "unused", "--init", str(config / "init.json"), "--arm", "E1", "--seed", "17",
               "--device", "cpu", "--out", str(Path(d) / name), "--synthetic-fixture", *extra]
        rc = subprocess.run(cmd, capture_output=True, timeout=900).returncode
        return rc, read(Path(d) / name / "BITWISE.json")

    def test_compare_equal_trees_and_a_perturbed_tree(self):
        with tempfile.TemporaryDirectory() as d:
            root, config, _ = self.source_bundle(d)
            smoke = Path(d) / "smoke"
            harness.smoke_bundle(root, config / "contract.json", config / "protocol.json", smoke, fraction=0.5)
            (smoke / "config" / "init.json").write_text("{}")
            rc, report = self.compare(d, HERE, smoke, "same")
            # Bitwise equal but not a D3 pass: nothing is pinned, 50% of rows, synthetic encoder.
            self.assertEqual(rc, harness.EXIT_EQUAL_NOT_D3)
            self.assertTrue(report["bitwise_equal"])
            self.assertFalse(report["d3_pass"])
            self.assertTrue(all(s["differences"] == 0 for s in report["sections"].values()))
            self.assertGreater(report["sections"]["steps"]["compared"], 0)
            self.assertFalse(any(report["d3_conditions"].values()))
            self.assertNotIn("train-", json.dumps(report))  # no row ids in the report
            # A changed loss scale in the reference shows in every step and in the weights.
            perturbed = Path(d) / "perturbed"
            perturbed.mkdir()
            for f in HERE.glob("*.py"):
                shutil.copy(f, perturbed / f.name)
            src = (perturbed / "train_full_a.py").read_text()
            target = "    scale = len(train) / len(indices)\n"
            self.assertEqual(src.count(target), 1)
            (perturbed / "train_full_a.py").write_text(src.replace(target, "    scale = 1.001 * len(train) / len(indices)\n"))
            rc, report = self.compare(d, perturbed, smoke, "perturbed-report")
            self.assertEqual(rc, harness.EXIT_DIFFERENT)
            self.assertFalse(report["bitwise_equal"])
            self.assertEqual(report["sections"]["steps"]["steps_differing"], report["sections"]["steps"]["compared"])
            self.assertGreater(report["sections"]["selected_checkpoint"]["differences"], 0)
            self.assertEqual(report["sections"]["binding_inputs"]["differences"], 0)

    def test_d3_pass_needs_every_input_pinned(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            # A source whose 1% holds a carved group and one row per stratum; the blocks after Train are unparseable.
            root, config = d / "source", d / "source-config"
            rows = write_trainer_bundle(root, weighted_rows(groups=180, splits=("train",)),
                                        sealed={"dev_iter": 4, "dev_select": 4})
            write_config(config, root, rows)
            smoke = d / "smoke"
            harness.smoke_bundle(root, config / "contract.json", config / "protocol.json", smoke)
            (smoke / "config" / "init.json").write_text("{}")
            self.assertEqual(read(smoke / "bundle" / "MANIFEST.json")["selection"]["rows_allowed"], 18)
            # The reference tree (this tree's files) extracted against an R0-like binding of this environment.
            tree = d / "ref" / "tree"
            tree.mkdir(parents=True)
            for f in HERE.glob("*.py"):
                shutil.copy(f, tree / f.name)
            modules = {k: sha((tree / m).read_bytes()) if (tree / m).exists() else None
                       for k, m in harness.MODULE_DIGESTS.items()}
            src = read(smoke / "bundle" / "MANIFEST.json")["source"]
            r0 = dict(modules, **run_environment(), contract_sha256=sha((config / "contract.json").read_bytes()),
                      protocol_sha256=sha((config / "protocol.json").read_bytes()),
                      bundle_sha256={bundle.MANIFEST: src["manifest_sha256"], bundle.ROWS: "ee" * 32,
                                     bundle.WEIGHTS: src["weights_sha256"]})
            binding = d / "r0-binding.json"
            binding.write_text(json.dumps(r0))
            (d / "ref" / "TREE.json").write_text(json.dumps({"files": {}, "binding": {"sha256": sha(binding.read_bytes()),
                                                                                       "checked": modules}}))
            expect = {"r0_binding_sha256": sha(binding.read_bytes()), "source_manifest_sha256": src["manifest_sha256"],
                      "source_weights_sha256": src["weights_sha256"], "train_block_sha256": src["train_block"]["sha256"],
                      "contract_sha256": r0["contract_sha256"], "protocol_sha256": r0["protocol_sha256"]}
            extra = ["--source", str(root), "--source-contract", str(config / "contract.json"), "--r0-binding",
                     str(binding)] + [x for k, v in expect.items() for x in ("--expect-" + k.replace("_", "-"), v)]
            rc, report = self.compare(d, tree, smoke, "d3", extra=extra)
            self.assertTrue(report["bitwise_equal"])
            self.assertEqual({k for k, v in report["d3_conditions"].items() if not v}, {"real_encoder"})
            self.assertEqual((rc, report["d3_pass"]), (harness.EXIT_EQUAL_NOT_D3, False))
            self.assertEqual(report["d3_expected"], expect)
            # The same outputs, checked as if from the real encoder, pass; every pinned input can fail it.
            ns = dict(data=smoke / "bundle", config=smoke / "config", reference_tree=tree, source=root,
                      source_contract=config / "contract.json", r0_binding=binding, synthetic_fixture=False,
                      **{"expect_" + k: v for k, v in expect.items()})
            ref = read(d / "ref" / "TREE.json")

            def failing(**change):
                conditions = harness.d3_check(argparse.Namespace(**dict(ns, **change)), d / "d3", ref)[0]
                return {k for k, v in conditions.items() if not v}
            self.assertEqual(failing(), set())
            self.assertEqual(failing(synthetic_fixture=True), {"real_encoder"})
            pinned_by = {"source_manifest_sha256": {"source_matches_expected"},
                         "source_weights_sha256": {"source_matches_expected"},
                         "train_block_sha256": {"source_matches_expected"},
                         "contract_sha256": {"contract_matches_expected"},
                         "protocol_sha256": {"protocol_matches_expected"}}
            for k, conditions in pinned_by.items():
                self.assertEqual(failing(**{"expect_" + k: "00" * 64}), conditions, k)
                self.assertEqual(failing(**{"expect_" + k: None}), conditions, k)
            self.assertEqual(failing(expect_r0_binding_sha256="00" * 64),
                             {"r0_binding_pinned", "reference_matches_r0_binding", "environment_matches_r0_binding",
                              "source_matches_expected", "contract_matches_expected", "protocol_matches_expected"})
            self.assertEqual(failing(source=None), {"smoke_rebuilt_from_source", "source_matches_expected"})
            self.assertEqual(failing(source_contract=None), {"contract_matches_expected"})
            # Another environment, or another module digest, in a binding pinned as expected.
            for field, value, condition in (("torch", "0.0.0", "environment_matches_r0_binding"),
                                            ("device", "cuda", "environment_matches_r0_binding"),
                                            ("trainer_sha256", "11" * 32, "reference_matches_r0_binding")):
                other = d / f"r0-{field}.json"
                other.write_text(json.dumps(dict(r0, **{field: value})))
                tree_ref = dict(ref, binding=dict(ref["binding"], sha256=sha(other.read_bytes())))
                conditions = harness.d3_check(argparse.Namespace(**dict(ns, r0_binding=other,
                                              expect_r0_binding_sha256=sha(other.read_bytes()))), d / "d3", tree_ref)[0]
                self.assertEqual({k for k, v in conditions.items() if not v}, {condition}, field)
            # A smoke above 1% of the Train lines, and a smoke that is not the rebuild of its source.
            wide = d / "wide"
            harness.smoke_bundle(root, config / "contract.json", config / "protocol.json", wide, fraction=0.02)
            self.assertEqual(failing(data=wide / "bundle", config=wide / "config"), {"rows_at_most_1_percent"})
            (wide / "bundle" / "weights.json").write_bytes((wide / "bundle" / "weights.json").read_bytes() + b"\n")
            self.assertEqual(failing(data=wide / "bundle", config=wide / "config"),
                             {"rows_at_most_1_percent", "smoke_rebuilt_from_source", "source_matches_expected"})
            # A reference tree changed after its extraction.
            with (tree / "train_full_a.py").open("a") as f:
                f.write("# changed\n")
            self.assertEqual(failing(), {"reference_matches_r0_binding"})

    @unittest.skipUnless(shutil.which("git"), "git unavailable")
    def test_r0_trainer_and_this_tree_agree_bitwise_with_r0s_caps_binding(self):
        top = subprocess.run(["git", "-C", str(HERE), "rev-parse", "--show-toplevel"], capture_output=True, text=True)
        if top.returncode != 0:
            self.skipTest("not a git checkout")
        repo = top.stdout.strip()
        if subprocess.run(["git", "-C", repo, "cat-file", "-e", R0_TRAINER_COMMIT + "^{commit}"],
                          capture_output=True).returncode != 0:
            self.skipTest("R0's trainer commit is not in this clone")
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            binding = d / "r0-modules.json"
            binding.write_text(json.dumps(R0_MODULE_DIGESTS))
            rec = harness.extract(repo, R0_TRAINER_COMMIT, d / "r0", binding)
            self.assertEqual(rec["binding"]["checked"], R0_MODULE_DIGESTS)
            root, config = d / "b", d / "config"
            rows = write_trainer_bundle(root, r0_shaped_rows())
            protocol = synthetic.protocol_template()
            protocol["train_weighting"] = copy.deepcopy(R0_WEIGHTING)
            write_config(config, root, rows, protocol)
            # Each of R0's three caps moves weight on these rows.
            meta, _ = bundle.train_metadata(root)
            full = bundle.train_weights(meta, R0_WEIGHTING)
            for field in ("max_row_weight_ratio", bundle.GLOBAL_CAP_FIELD, bundle.COHORT_CAP_FIELD):
                other = bundle.train_weights(meta, dict(R0_WEIGHTING, **{field: None}))
                self.assertGreater(max(abs(a - b) for a, b in zip(full, other)), 1e-3, field)
            r0 = dict(R0_MODULE_DIGESTS, **run_environment(), protocol_sha256=sha((config / "protocol.json").read_bytes()))
            pinned = d / "r0-binding.json"
            pinned.write_text(json.dumps(r0))
            # The tree record names the binding it was extracted against; this one adds the run's environment.
            tree = json.loads((d / "r0" / "TREE.json").read_bytes())
            tree["binding"]["sha256"] = sha(pinned.read_bytes())
            (d / "r0" / "TREE.json").write_text(json.dumps(tree))
            rc, report = self.compare(d, d / "r0" / "tree", None, "r0-compare", data=root, config=config, extra=[
                "--r0-binding", str(pinned), "--expect-r0-binding-sha256", sha(pinned.read_bytes()),
                "--expect-protocol-sha256", r0["protocol_sha256"]])
            self.assertTrue(report["bitwise_equal"], {k: v["first"] for k, v in report["sections"].items()})
            self.assertGreater(report["sections"]["steps"]["compared"], 0)
            self.assertEqual(rc, harness.EXIT_EQUAL_NOT_D3)
            c = report["d3_conditions"]
            self.assertTrue(c["reference_matches_r0_binding"] and c["environment_matches_r0_binding"] and
                            c["protocol_matches_expected"])
            self.assertEqual(report["reference"]["commit"], R0_TRAINER_COMMIT)

    def test_diff_rules(self):
        a = {"x": 1.0, "y": [1, 2], "elapsed_seconds_this_invocation": 3., "old": None}
        self.assertEqual(harness.diff(a, dict(a, elapsed_seconds_this_invocation=9., new=0.)), [])
        self.assertEqual(harness.diff(a, dict(a, new="0x0.0p+0")), [])
        self.assertEqual(harness.diff(a, dict(a, x=1.0000000000000002)), ["/x"])
        self.assertEqual(harness.diff(a, dict(a, x=1)), ["/x"])
        self.assertEqual(harness.diff(a, dict(a, new=0.5)), ["/new (candidate only)"])
        self.assertEqual(harness.diff(dict(a, gone=True), a), ["/gone (reference only)"])
        self.assertEqual(harness.diff(a, dict(a, y=[1])), ["/y (lengths 2 / 1)"])
        self.assertEqual(harness.diff({"v": -0.0}, {"v": 0.0}), ["/v"])

    @unittest.skipUnless(shutil.which("git"), "git unavailable")
    def test_extract_checks_the_binding_digests(self):
        top = subprocess.run(["git", "-C", str(HERE), "rev-parse", "--show-toplevel"], capture_output=True, text=True)
        if top.returncode != 0:
            self.skipTest("not a git checkout")
        repo = top.stdout.strip()
        with tempfile.TemporaryDirectory() as d:
            files = {}
            for field, module in harness.MODULE_DIGESTS.items():
                show = subprocess.run(["git", "-C", repo, "show", f"HEAD:{harness.PYTHON_DIR}/{module}"],
                                      capture_output=True)
                if show.returncode == 0:
                    files[field] = sha(show.stdout)
            binding = Path(d) / "binding.json"
            binding.write_text(json.dumps(files))
            rec = harness.extract(repo, "HEAD", Path(d) / "ref", binding)
            self.assertEqual(rec["binding"]["checked"], files)
            self.assertTrue((Path(d) / "ref" / "tree" / "train_full_a.py").exists())
            self.assertEqual(read(Path(d) / "ref" / "TREE.json")["commit"], rec["commit"])
            binding.write_text(json.dumps(dict(files, trainer_sha256="00" * 32)))
            with self.assertRaisesRegex(ValueError, "differs from the binding"):
                harness.extract(repo, "HEAD", Path(d) / "ref2", binding)
            self.assertFalse((Path(d) / "ref2").exists())


if __name__ == "__main__":
    unittest.main()

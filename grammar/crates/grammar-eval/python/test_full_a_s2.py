"""Offline protocol spans-S2 checks: the anchor objective and its teacher, pair
contrast entries and the fixed human mass, the span tower, the diagnostic
subsets, arm validation, and tiny S2a/S2b/S2c fits through the real trainer.

No corpus, artifact, provider, detector or network access. CPU only.
"""
import copy
import json
import math
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import torch

import full_a
import full_a_bundle as bundle
import full_a_s2 as s2
import synthetic_full_a as synthetic
import test_full_a as base
import test_full_a_spans as spans_tests
import train_full_a as trainer


def with_token_head(model, width=16):
    model.token_head = full_a.TokenHead(width=width)
    return model


class Anchor(unittest.TestCase):
    def test_anchor_is_stationary_at_the_teacher_and_teacher_is_frozen(self):
        ids, mask, content = base.batch_of(base.TEXTS)
        model = with_token_head(base.make_model(seed=4)).eval()
        teacher = s2.make_teacher(model)
        self.assertTrue(all(not p.requires_grad for p in teacher.parameters()))
        self.assertIsNone(teacher.token_head)
        out = model(ids, mask, content, autocast=False)
        t = s2.teacher_outputs(teacher, ids, mask, content, None)
        self.assertTrue(torch.equal(t["origin"], out["origin"].detach()))
        parts = {}
        loss = s2.anchor_objective(out, t, [1., 2., 1.], 4., 1., .25, parts)
        loss.backward()
        worst = max(float(p.grad.abs().max()) for p in model.heads.parameters() if p.grad is not None)
        self.assertLess(worst, 1e-6)
        self.assertEqual(set(parts), {"anchor_origin", "anchor_share", "anchor_conditional_share",
                                      "anchor_attribution"})
        # A moved student pays a positive anchor loss above the teacher's own entropy floor.
        floor = float(loss.detach())
        with torch.no_grad():
            for p in model.heads.parameters():
                p.add_(.05 * torch.randn_like(p))
        moved = s2.anchor_objective(model(ids, mask, content, autocast=False), t, [1., 2., 1.], 4., 1., .25)
        self.assertGreater(float(moved.detach()), floor)
        # The teacher is a copy: moving the student never moves it.
        again = s2.teacher_outputs(teacher, ids, mask, content, None)
        self.assertTrue(torch.equal(again["origin"], t["origin"]))


def fake_population(rows):
    meta = [SimpleNamespace(id=i, cohort_id=c, pair_id=p, origin=o) for i, c, p, o, _ in rows]
    weights = [w for *_, w in rows]
    total = sum(weights)
    return SimpleNamespace(meta=meta, weights=weights, token_weights=[w / total for w in weights])


class PairContrastEntries(unittest.TestCase):
    ROWS = [("h1", "humans", None, 0, 2.), ("h2", "humans", None, 0, 1.), ("m1", "local", None, 1, 1.5),
            ("c1", s2.SPLICED_COHORT, "h1", 2, .5), ("c2", s2.SPLICED_COHORT, "h1", 2, .25),
            ("c3", s2.SPLICED_COHORT, "h2", 2, .25)]

    def test_entries_scale_and_fixed_human_mass(self):
        train = fake_population(self.ROWS)
        pc = s2.PairContrast(train)
        self.assertEqual(pc.parent, {3: 0, 4: 0, 5: 1})
        self.assertAlmostEqual(pc.scale, 3. / 4.)
        self.assertEqual(pc.doc_weights, [1.5, .75, 1.5, .5, .25, .25])
        self.assertLessEqual(pc.mass_relative_error, 1e-12)
        entries = pc.entries([0, 3, 4, 2], train)
        self.assertEqual([(e[0], e[3]) for e in entries], [(0, False), (3, False), (4, False), (2, False),
                                                          (0, True), (0, True)])
        self.assertEqual([e[1] for e in entries[4:]], [.375, .1875])
        self.assertEqual([e[2] for e in entries[4:]], [0., 0.])
        human = sum(w for m, w in zip(train.meta, pc.doc_weights) if m.origin == 0) + pc.scale * pc.added_mass
        self.assertAlmostEqual(human, 3.)
        groups = s2.entry_groups(entries, [5, 9, 9, 3, 5, 5], 20)
        self.assertEqual(sorted(k for g in groups for k in g), list(range(len(entries))))

    def test_missing_or_non_human_parent_refused(self):
        rows = self.ROWS + [("c4", s2.SPLICED_COHORT, "gone", 2, .1)]
        with self.assertRaisesRegex(ValueError, "absent"):
            s2.PairContrast(fake_population(rows))
        self.assertEqual(s2.PairContrast(fake_population(rows), require_all=False).missing, 1)
        bad = self.ROWS + [("c5", s2.SPLICED_COHORT, "m1", 2, .1)]
        with self.assertRaisesRegex(ValueError, "not human-only"):
            s2.PairContrast(fake_population(bad))


class SpanTower(unittest.TestCase):
    def test_tower_starts_equal_and_trains_alone(self):
        ids, mask, content = base.batch_of(base.TEXTS)
        plain = with_token_head(base.make_model(seed=4))
        encoder = base.tiny_encoder(4)
        tower = s2.TowerFullA(encoder, base.heads_for(encoder), token_head=copy.deepcopy(plain.token_head),
                              tower_layers=1)
        tower.attach_tower()
        self.assertFalse(tower.span_tower.layers[0]._forward_pre_hooks)
        a = plain(ids, mask, content, autocast=False)
        b = tower(ids, mask, content, autocast=False)
        for key in ("origin_logits", "involvement_logit", "share", "ai_logits"):
            self.assertTrue(torch.equal(a[key], b[key]), key)
        self.assertTrue(torch.equal(a["span_logits"], b["span_logits"]))
        tower.freeze_document()
        tower.train()
        self.assertFalse(tower.encoder.training)
        self.assertTrue(tower.span_tower.training)
        out = tower(ids, mask, content)
        labels = torch.where(content, torch.ones_like(ids), torch.full_like(ids, -1))
        loss = full_a.objective(out, [{"origin": 1, "share": 1., "human": None, "ai": 0}] * 3, [1.] * 3,
                                {"origin": 3., "share": 3., "ai": 3., "token_head": 3.}, labels,
                                token_head_weight=1., token_weights=[1.] * 3, document=False)
        loss.backward()
        self.assertTrue(all(p.grad is None for p in tower.encoder.parameters()))
        self.assertTrue(all(p.grad is None for p in tower.heads.parameters()))
        self.assertTrue(any(p.grad is not None and float(p.grad.abs().sum()) > 0
                            for p in tower.span_tower.parameters()))
        groups = full_a.parameter_groups(tower, 1e-5, 1., 1e-4, .01, 1e-3) + \
            s2.tower_parameter_group(tower, 3e-5, .01)
        names = {g["name"] for g in groups if g["params"]}
        self.assertEqual(names, {"token_head.wd0.01", "token_head.wd0.0", "span_tower.wd0.01", "span_tower.wd0.0"})


class Subsets(unittest.TestCase):
    def test_coarse_subset_allocation(self):
        meta = [SimpleNamespace(id=f"r{i}", cohort_id=c, origin=o, label=None if o == 0 else "gen-a") for i, (c, o) in
                enumerate([("a", 0)] * 50 + [("a", 2)] * 30 + [("b", 1)] * 15 + [("c", 2)] * 5)]
        keep = s2.coarse_subset(meta, 10, 17)
        self.assertEqual(len(keep), 10)
        strata = {}
        for m in meta:
            if m.id in keep:
                strata.setdefault((m.cohort_id, m.label, m.origin), []).append(keep[m.id])
        self.assertEqual(len(strata), 4)
        for k, f in strata.items():
            n = sum(1 for m in meta if (m.cohort_id, m.label, m.origin) == k)
            self.assertEqual(len(set(f)), 1)
            self.assertAlmostEqual(f[0] * len(f), n)
        self.assertEqual(keep, s2.coarse_subset(meta, 10, 17))
        self.assertNotEqual(set(keep), set(s2.coarse_subset(meta, 10, 18)))
        with self.assertRaises(ValueError):
            s2.coarse_subset(meta, 3, 17)

    def test_pair_closure(self):
        meta = [SimpleNamespace(id="h", cohort_id="humans", pair_id=None),
                SimpleNamespace(id="c", cohort_id=s2.SPLICED_COHORT, pair_id="h")]
        self.assertEqual(s2.close_pairs({"c": 3.}, meta), {"c": 3., "h": 1.})


def s2_protocol():
    p = spans_tests.span_protocol()
    head = {"hidden": 16, "loss_weight": 1., "learning_rate": 1e-2}
    joint = {"span_head": None, "share_mode": "head", "share_supervision": {"conditional_loss_weight": 1., "bins": None},
             "batching": {"kind": "random"}, "style_adversary": None, "contrastive": None, "token_head": head,
             "freeze": "none", "token_weighting": "row_weight"}
    p["arms"] = {"S2a": dict(joint, s2={"mechanism": "anchor", "teacher": "init_fit", "teacher_precision": "fp32"}),
                 "S2b": dict(joint, s2={"mechanism": "pair_contrast", "cohort": s2.SPLICED_COHORT,
                                        "class_balance": "fixed_human_mass"})}
    p["early_stopping"]["metric"] = trainer.GUARDED_WORD_METRIC
    p["document_guard"] = 1.
    return p


def tower_protocol():
    p = spans_tests.span_protocol()
    p["arms"] = {"S2c": dict(p["arms"]["S0"], s2={"mechanism": "span_tower", "layers": 1, "learning_rate": 1e-3})}
    p["early_stopping"]["metric"] = trainer.WORD_METRIC
    return p


class Validation(unittest.TestCase):
    def test_s2_arm_fields(self):
        for p in (s2_protocol(), tower_protocol()):
            trainer.validate_protocol(p)
        a = s2_protocol()["arms"]["S2a"]
        for bad in (dict(a, s2={"mechanism": "anchor", "teacher": "other", "teacher_precision": "fp32"}),
                    dict(a, s2={"mechanism": "anchor", "teacher": "init_fit", "teacher_precision": "fp32"},
                         token_head=None),
                    dict(a, s2={"mechanism": "span_tower", "layers": 1, "learning_rate": 1e-3}),
                    dict(a, s2={"mechanism": "nope"})):
            with self.assertRaises(ValueError):
                trainer.validate_arm("bad", bad)
        c = tower_protocol()["arms"]["S2c"]
        with self.assertRaises(ValueError):
            trainer.validate_arm("bad", dict(c, freeze="none"))


def spliced_rows():
    """Synthetic rows with train mixed rows recast as spliced children of their group's human."""
    rows = synthetic.factorial_rows(groups=2)
    humans = {r["source_group"]: r["id"] for r in rows if r["origin"] == "human_only"}
    n = 0
    for r in rows:
        if r["split"] == "train" and r["origin"] == "mixed" and n < 4:
            r["cohort_id"], r["pair_id"] = s2.SPLICED_COHORT, humans[r["source_group"]]
            r["source_kind"] = "source_conditioned_mixed_rewrite"
            n += 1
    return rows


class TinyFits(unittest.TestCase):
    """E1 init fit, then S2a, S2b and S2c from it through the real trainer."""

    def test_three_mechanisms(self):
        fitter = base.EndToEnd()
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "bundle", Path(d) / "config"
            rows = spliced_rows()
            digests = synthetic.write_bundle(root, rows)
            labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
            synthetic.write_config(config, digests[bundle.MANIFEST], labels)
            (config / "init.json").write_text("{}")
            e1 = Path(d) / "e1"
            real_evaluate, calls = trainer.evaluate, []

            def trained_selection(*args, **kwargs):
                result = real_evaluate(*args, **kwargs)
                calls.append(None)
                return (1. / (1 + len(calls)),) + tuple(result[1:])
            with mock.patch.object(trainer, "evaluate", trained_selection):
                fitter.fit(root, config, e1, "E1")
            real = trainer.load_init_fit
            for name, protocol in (("S2a", s2_protocol()), ("S2b", s2_protocol()), ("S2c", tower_protocol())):
                conf = Path(d) / f"conf-{name}"
                synthetic.write_config(conf, digests[bundle.MANIFEST], labels, protocol)
                (conf / "init.json").write_text("{}")
                out = Path(d) / name
                with mock.patch.object(trainer, "load_init_fit", lambda m, path, seed, c: real(m, e1, seed, c)):
                    spans_tests.fitter_args_fit(fitter, root, conf, out, name)
                fit = json.loads((out / "fit.json").read_text())
                history = json.loads((out / "history.json").read_text())
                self.assertEqual(fit["s2"]["mechanism"], {"S2a": "anchor", "S2b": "pair_contrast",
                                                          "S2c": "span_tower"}[name])
                self.assertEqual(fit["init_raw_gate"]["document_raw_max_abs_error"], 0.)
                terms = [h["train_terms_mean_since_last_evaluation"] for h in history[1:]]
                self.assertTrue(all(math.isfinite(v) for t in terms for v in t.values()))
                if name == "S2a":
                    self.assertTrue(all(t["loss_anchor_origin"] >= 0 for t in terms))
                    self.assertTrue(all(t["loss_origin"] == 0 for t in terms))
                    self.assertEqual(fit["s2"]["teacher"]["precision"], "fp32")
                    self.assertLessEqual(fit["s2"]["teacher"]["gate"]["max_abs_error"], 1e-6)
                    self.assertEqual(fit["s2"]["teacher"]["gate"]["rows"], sum(1 for r in rows if r["split"] == "dev_iter"))
                if name == "S2b":
                    pc = fit["s2"]["pair_contrast"]
                    self.assertEqual((pc["pairs"], pc["missing_parents"]), (4, 0))
                    self.assertLessEqual(pc["human_mass_relative_error"], 1e-12)
                    self.assertGreater(pc["added_mass"], 0)
                if name == "S2c":
                    self.assertTrue(fit["frozen_document_check"]["encoder_and_document_heads_bitwise_equal_to_init"])
                    from safetensors.torch import load_file
                    heads = load_file(str(out / "heads.safetensors"))
                    self.assertTrue(any(k.startswith("span_tower.") for k in heads))
                    a = [json.loads(x) for x in (e1 / "dev_iter-raw.jsonl").read_text().splitlines()]
                    b = [json.loads(x) for x in (out / "dev_iter-raw.jsonl").read_text().splitlines()]
                    self.assertEqual([r["origin_logits"] for r in a], [r["origin_logits"] for r in b])


if __name__ == "__main__":
    unittest.main()

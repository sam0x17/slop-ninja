"""Offline checks of the doc-r2 trainer options (full_a_doc), the Train raw-heads route (raw_full_a) and the
bitwise harness's KD mode: share-bin balance, the attribution mask, seed-ensemble distillation (alpha 0 equals
no distillation, bit for bit), and every option absent leaving the trainer's computation unchanged.

Synthetic word sequences and a tiny random ModernBERT only. No corpus, artifact, provider, detector or network
access. CPU only.
"""
import copy
import hashlib
import json
import math
from pathlib import Path
import random
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from unittest import mock

import torch

import bitwise_full_a as harness
import export_full_a as exporter
import full_a
import full_a_aux as aux
import full_a_bundle as bundle
import full_a_doc as doc
import raw_full_a
import test_full_a as base
import test_full_a_r2 as r2
import train_full_a as trainer

HERE = Path(__file__).resolve().parent
# The public commit this change starts from: its trainer is the reference for "every option absent".
BASE_COMMIT = "2a450da28a91b24822bce74091e55fce0a0258de"
BALANCE = {"rule": doc.BIN_BALANCE_RULE,
           "max_ratio": 5.0}
MASKED = "hosted_frontier_v1"  # synthetic_full_a's claude-x cohort


def sha(b):
    return hashlib.sha256(b).hexdigest()


def protocol(arm="E1", balance=None, distill=None, mask=None, base_protocol=None):
    p = copy.deepcopy(base_protocol or r2.row_protocol())
    a = p["arms"][arm]
    if balance is not None:
        a["share_supervision"] = dict(a["share_supervision"], bin_balance=balance)
    if distill is not None:
        a["distill"] = distill
    if mask is not None:
        p["attribution"] = dict(p["attribution"], unsupervised_cohort_ids=mask)
    return p


def spread_rows(groups=3):
    """Weighted rows whose mixed shares cover several share bins (one exactly 1)."""
    rows = r2.weighted_rows(groups=groups)
    shares = (0.05, 0.3, 0.6, 0.95, 1.0, 0.12)
    k = 0
    for r in rows:
        if r["origin"] == "mixed":
            r["share"] = shares[k % len(shares)]
            k += 1
    return rows


def bundle_with(d, rows, name="b"):
    root, config = Path(d) / name, Path(d) / (name + "-config")
    rows = r2.write_trainer_bundle(root, rows)
    r2.write_config(config, root, rows)
    return root, config, rows


def config_with(d, root, rows, p, name):
    config = Path(d) / name
    r2.write_config(config, root, rows, p)
    return config


def tensors_equal(a, b):
    return set(a) == set(b) and all(torch.equal(a[k], b[k]) for k in a)


def raw_train(root, config, fit_dir, out, **extra):
    encoder = base.tiny_encoder(11)
    args = types.SimpleNamespace(data=str(root), config=str(config), fit=str(fit_dir), artifact="unused",
                                 init=str(Path(config) / "init.json"), out=str(out), device="cpu",
                                 allow_diagnostic_subset=False, **extra)
    with mock.patch.object(trainer, "load", lambda a, p, d: (encoder, base.FakeTokenizer(),
                                                             {"artifact_id": "synthetic-parent"})), \
            mock.patch.object(exporter, "initializer", lambda path: base.init_for(encoder)):
        (raw_full_a.rows if "rows" in extra else raw_full_a.train)(args)


def targets_from_raw(raw_dirs, out, edit=None):
    """The targets file `seed_mean_full_a distill-targets` writes (Python mirror for these tests)."""
    manifests = [r2.read(Path(d) / "manifest.json") for d in raw_dirs]
    tables = [[json.loads(x) for x in (Path(d) / "raw.jsonl").read_bytes().splitlines()] for d in raw_dirs]
    n = len(raw_dirs)
    rows = []
    for k, first in enumerate(tables[0]):
        group = [t[k] for t in tables]
        ai = [torch.tensor(r["ai_logits"], dtype=torch.float64).softmax(-1).tolist() for r in group]
        rows.append({"id": first["id"], "text_sha256": first["text_sha256"],
                     "origin": [sum(r["uncalibrated_origin"][o] for r in group) / n
                                for o in ("human_only", "model_only", "mixed")],
                     "share": sum(r["uncalibrated_share"] for r in group) / n,
                     "conditional_mixed_share": sum(r["conditional_mixed_share"] for r in group) / n,
                     "ai": [sum(a[j] for a in ai) / n for j in range(len(ai[0]))]})
    header = {"schema": doc.DISTILL_TARGETS_SCHEMA, "partition": "train", "rows": len(rows),
              "ai_profiles": manifests[0]["ai_profiles"], "teachers": [{"fit_sha256": m["fit_sha256"]} for m in manifests],
              "rule": "test mirror"}
    if edit is not None:
        header, rows = edit(header, rows)
    Path(out).write_bytes(b"".join(json.dumps(x).encode() + b"\n" for x in [header] + rows))
    return out


def fake_out(seed=0, n=6, roster=3):
    """Model outputs shaped like FullA's, as leaf tensors with gradients."""
    g = torch.Generator().manual_seed(seed)
    origin_logits = torch.randn((n, 3), generator=g).requires_grad_(True)
    q_logit = torch.randn((n,), generator=g).requires_grad_(True)
    ai_logits = torch.randn((n, roster), generator=g).requires_grad_(True)
    human_logits = torch.zeros((n, 0)).requires_grad_(True)
    encoding = torch.randn((n, 4), generator=g).requires_grad_(True)
    log_origin, gi = full_a.combine(origin_logits)
    origin = log_origin.exp()
    q = torch.sigmoid(q_logit)
    out = {"origin_logits": origin_logits, "log_origin": log_origin, "origin": origin, "involvement_logit": gi,
           "conditional_mixed_share": q, "share": origin[:, 1] + origin[:, 2] * q, "ai_logits": ai_logits,
           "human_logits": human_logits, "encoding": encoding, "token_logits": None, "span_logits": None}
    leaves = (origin_logits, q_logit, ai_logits, encoding)
    return out, leaves


ROWS = [  # (origin, share, ai target)
    (0, 0., None), (1, 1., 0), (2, 0.05, 1), (2, 0.6, 2), (2, 1.0, None), (0, 0., None)]


def fake_targets():
    return [{"origin": o, "share": s, "ai": a, "human": None} for o, s, a in ROWS]


def fake_soft(seed=1, roster=3):
    rng = random.Random(seed)
    soft = []
    for _ in ROWS:
        o = [rng.random() + .1 for _ in range(3)]
        a = [rng.random() + .1 for _ in range(roster)]
        soft.append({"origin": [x / sum(o) for x in o], "share": rng.random(), "q": rng.random(),
                     "ai": [x / sum(a) for x in a]})
    return soft


WEIGHTS = [.2, .1, .15, .25, .1, .2]
DENOMINATORS = {"origin": 1., "share": 1., "conditional_share": .5, "ai": .6, "human": 0.}


def loss_and_grads(fn, seed=0):
    out, leaves = fake_out(seed)
    loss = fn(out)
    grads = torch.autograd.grad(loss, leaves, allow_unused=True)
    return loss.detach(), [None if g is None else g.detach() for g in grads]


class BinBalanceRule(unittest.TestCase):
    def meta(self, rows):
        return [types.SimpleNamespace(origin=o, share=s) for o, s in rows]

    def test_multipliers_preserve_mixed_mass_and_clip(self):
        meta = self.meta([(2, .05), (2, .07), (2, .3), (2, .95), (0, 0.), (1, 1.), (2, 1.)])
        w = [.3, .2, .3, .002, .5, .4, .1]
        rec = doc.bin_multipliers(meta, w, 5.)
        mass = rec["mass"]
        self.assertEqual(rec["rows"], [2, 0, 1, 0, 0, 1, 1])
        self.assertEqual(mass[0], .5)
        self.assertEqual(rec["populated_bins"], 4)
        self.assertEqual(rec["total_mixed_mass"], math.fsum([.3, .2, .3, .002, .1]))
        c = rec["multipliers"]
        self.assertEqual([x is None for x in c], [False, True, False, True, True, False, False])
        # The sparse 90-100% bin is clipped at 5 before the common rescaling; the rescaling keeps the ratios.
        self.assertEqual(rec["clipped"][5], 5.)
        self.assertGreater(rec["raw"][5], 5.)
        self.assertAlmostEqual(c[5] / c[0], 5. / rec["raw"][0], places=12)
        self.assertAlmostEqual(math.fsum(c[b] * mass[b] for b in (0, 2, 5, 6)), rec["total_mixed_mass"], places=15)
        # Share exactly 1 has its own bin; human and model-only rows are never balanced.
        self.assertEqual(aux.share_bin(1.), 6)
        self.assertEqual(doc.row_factors(meta, rec), [c[0], c[0], c[2], c[5], None, None, c[6]])

    def test_one_populated_bin_is_neutral_and_refusals(self):
        rec = doc.bin_multipliers(self.meta([(2, .3), (2, .4), (0, 0.)]), [.25, .35, .4], 5.)
        self.assertEqual(rec["multipliers"][2], 1.)
        self.assertEqual(rec["scale"], 1.)
        with self.assertRaisesRegex(ValueError, "mixed Train rows"):
            doc.bin_multipliers(self.meta([(0, 0.), (1, 1.)]), [.5, .5], 5.)
        rule = doc.BIN_BALANCE_RULE
        for bad in ({"rule": "other", "max_ratio": 5.}, {"rule": rule, "max_ratio": .5}, {"rule": rule},
                    {"rule": rule, "max_ratio": 5., "x": 1}):
            with self.assertRaisesRegex(ValueError, "share bin balance"):
                trainer.validate_protocol(protocol(balance=bad))

    def test_document_arms_only(self):
        for arm in ("E2w", "E2wshare"):
            with self.assertRaisesRegex(ValueError, "document-head arm"):
                trainer.validate_protocol(protocol(arm=arm, balance=BALANCE))
        p = protocol(balance=BALANCE)
        p["arms"]["E1"].update(token_head={"hidden": 8, "loss_weight": 1., "learning_rate": 1e-3}, freeze="none",
                               token_weighting="row_weight")
        with self.assertRaisesRegex(ValueError, "document-head arm"):
            trainer.validate_protocol(p)
        trainer.validate_protocol(protocol(balance=BALANCE))


class DocumentObjective(unittest.TestCase):
    def full(self, out, parts=None):
        return full_a.objective(out, fake_targets(), WEIGHTS, DENOMINATORS, None, 0., .25, 1., "head", parts)

    def test_neutral_options_reproduce_full_a_bitwise(self):
        ref, ref_g = loss_and_grads(self.full)
        for factors in (None, [None, None, 1., 1., 1., None]):
            for soft, alpha in ((None, 0.), (fake_soft(), 0.)):
                loss, g = loss_and_grads(lambda out: doc.objective(out, fake_targets(), WEIGHTS, DENOMINATORS, .25, 1.,
                                                                   "head", None, factors, soft, alpha, 1.))
                self.assertEqual(loss.item().hex(), ref.item().hex())
                self.assertTrue(all((a is None and b is None) or torch.equal(a, b) for a, b in zip(g, ref_g)))
        a, b = {}, {}
        out, _ = fake_out()
        self.full(out, a)
        doc.objective(out, fake_targets(), WEIGHTS, DENOMINATORS, .25, 1., "head", b)
        self.assertEqual(a, {k: v for k, v in b.items()})

    def test_share_factors_scale_the_mixed_share_terms_only(self):
        c = [None, None, 2., .5, 3., None]
        out, _ = fake_out()
        parts_ref, parts = {}, {}
        ref = self.full(out, parts_ref)
        loss = doc.objective(out, fake_targets(), WEIGHTS, DENOMINATORS, .25, 1., "head", parts, c)
        extra = 0.
        for i, (o, s, _) in enumerate(ROWS):
            if c[i] is None:
                continue
            q = out["conditional_mixed_share"][i].clamp(1e-6, 1 - 1e-6)
            extra += (c[i] - 1) * WEIGHTS[i] * (float((out["share"][i] - s) ** 2) / DENOMINATORS["share"] +
                                                float(torch.nn.functional.binary_cross_entropy(q, q.new_tensor(s))) /
                                                DENOMINATORS["conditional_share"])
        self.assertAlmostEqual(float(loss - ref), extra, places=6)
        for k in ("origin", "attribution"):
            self.assertEqual(parts[k], parts_ref[k])
        self.assertNotEqual(parts["share"], parts_ref["share"])

    def manual_soft(self, out, soft, temperature=1.):
        """The soft terms of DISTILL_TEXT, written out directly (alpha 1)."""
        total = 0.
        for i, ((o, s, a), w, t) in enumerate(zip(ROWS, WEIGHTS, soft)):
            p = torch.tensor(t["origin"])
            lo = out["log_origin"][i]
            if temperature == 1:
                ce = -(p * lo).sum()
            else:
                pt = p ** (1 / temperature)
                pt = pt / pt.sum()
                ce = -(pt * torch.log_softmax(lo / temperature, -1)).sum() * temperature ** 2
            total += float(ce + (out["origin"][i, 1:].sum() - (t["origin"][1] + t["origin"][2])) ** 2) * w
            total += float((out["share"][i] - t["share"]) ** 2) * w / DENOMINATORS["share"]
            if o == 2:
                q = out["conditional_mixed_share"][i].clamp(1e-6, 1 - 1e-6)
                total += float(torch.nn.functional.binary_cross_entropy(q, q.new_tensor(t["q"]))) * w / \
                    DENOMINATORS["conditional_share"]
            if a is not None:
                pa = torch.tensor(t["ai"])
                if temperature == 1:
                    ce = -(pa * torch.log_softmax(out["ai_logits"][i], -1)).sum()
                else:
                    pt = pa ** (1 / temperature)
                    pt = pt / pt.sum()
                    ce = -(pt * torch.log_softmax(out["ai_logits"][i] / temperature, -1)).sum() * temperature ** 2
                total += .25 * float(ce) * w / DENOMINATORS["ai"]
        return total

    def test_soft_terms_and_alpha_mix(self):
        soft = fake_soft()
        out, _ = fake_out()
        for temperature in (1., 2.):
            hard = float(doc.objective(out, fake_targets(), WEIGHTS, DENOMINATORS, .25, 1., "head", None))
            parts = {}
            pure = float(doc.objective(out, fake_targets(), WEIGHTS, DENOMINATORS, .25, 1., "head", parts, None, soft,
                                       1., temperature))
            self.assertAlmostEqual(pure, self.manual_soft(out, soft, temperature), places=5)
            self.assertTrue({"distill_soft_origin", "distill_soft_share", "distill_soft_conditional_share",
                             "distill_soft_attribution"} <= set(parts))
            half = float(doc.objective(out, fake_targets(), WEIGHTS, DENOMINATORS, .25, 1., "head", None, None, soft,
                                       .5, temperature))
            self.assertAlmostEqual(half, .5 * hard + .5 * pure, places=5)
        # Rows without an attribution target (masked or unsupervised) get no soft attribution term.
        targets = [dict(t, ai=None) for t in fake_targets()]
        parts = {}
        doc.objective(out, targets, WEIGHTS, DENOMINATORS, .25, 1., "head", parts, None, soft, 1., 1.)
        self.assertNotIn("attribution", parts)
        self.assertNotIn("distill_soft_attribution", parts)
        # The share multipliers apply to the soft share terms too.
        c = [None, None, 2., 2., 2., None]
        a = float(doc.objective(out, fake_targets(), WEIGHTS, DENOMINATORS, .25, 1., "head", None, c, soft, 1.))
        b = float(doc.objective(out, fake_targets(), WEIGHTS, DENOMINATORS, .25, 1., "head", None, None, soft, 1.))
        extra = 0.
        for i in (2, 3, 4):
            q = out["conditional_mixed_share"][i].clamp(1e-6, 1 - 1e-6)
            extra += WEIGHTS[i] * (float((out["share"][i] - soft[i]["share"]) ** 2) +
                                   float(torch.nn.functional.binary_cross_entropy(q, q.new_tensor(soft[i]["q"]))) /
                                   DENOMINATORS["conditional_share"])
        self.assertAlmostEqual(a - b, extra, places=5)

    def test_distill_validation(self):
        for bad in ({"rule": doc.DISTILL_RULE, "alpha": 1.5, "temperature": 1.},
                    {"rule": doc.DISTILL_RULE, "alpha": -.1, "temperature": 1.},
                    {"rule": doc.DISTILL_RULE, "alpha": .5, "temperature": 0.},
                    {"rule": "other", "alpha": .5, "temperature": 1.},
                    {"rule": doc.DISTILL_RULE, "alpha": .5}):
            with self.assertRaisesRegex(ValueError, "distill"):
                trainer.validate_protocol(protocol(distill=bad))
        good = {"rule": doc.DISTILL_RULE, "alpha": .5, "temperature": 1.}
        trainer.validate_protocol(protocol(distill=good))
        with self.assertRaisesRegex(ValueError, "document-head arm"):
            trainer.validate_protocol(protocol(arm="E2w", distill=good))
        p = protocol(distill=good)
        p["arms"]["E1"]["s2"] = None
        with self.assertRaisesRegex(ValueError, "arm fields"):
            trainer.validate_protocol(p)
        for bad in ([], [""], ["a", "a"], "cohort"):
            with self.assertRaisesRegex(ValueError, "unsupervised_cohort_ids"):
                trainer.validate_protocol(protocol(mask=bad))
        p = protocol(mask=["x"])
        p["attribution"]["other"] = 1
        with self.assertRaisesRegex(ValueError, "attribution"):
            trainer.validate_protocol(p)


class AttributionMask(unittest.TestCase):
    def populations(self, d, cohorts=(MASKED,)):
        root, config, rows = bundle_with(d, r2.weighted_rows(groups=3))
        labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
        tokenize = full_a.hf_tokenize(base.FakeTokenizer())
        special = {base.PAD, base.CLS, base.SEP}
        w = r2.row_protocol()["train_weighting"]
        plain = bundle.Population(root, "train", tokenize, special, labels, cohort_weighted=True, weighting=w)
        masked = doc.MaskedPopulation(root, "train", tokenize, special, labels, cohort_weighted=True, weighting=w,
                                      unsupervised_cohorts=list(cohorts))
        return plain, masked

    def test_targets_denominators_and_keys(self):
        with tempfile.TemporaryDirectory() as d:
            plain, masked = self.populations(d)
            flags = masked.attribution_masked
            self.assertTrue(any(flags) and not all(flags))
            for i, m in enumerate(masked.meta):
                self.assertEqual(masked.targets(i)["ai"] is None, m.label is None or flags[i])
                self.assertEqual(flags[i], m.cohort_id == MASKED)
                if flags[i]:
                    self.assertIsNotNone(m.label)  # the label stays (row contract)
                else:
                    self.assertEqual(masked.targets(i), plain.targets(i))
            expected = sum(w for m, w, x in zip(masked.meta, masked.weights, flags) if m.label is not None and not x)
            self.assertAlmostEqual(masked.denominators["ai"], expected, places=15)
            self.assertLess(masked.denominators["ai"], plain.denominators["ai"])
            self.assertEqual({k: v for k, v in masked.denominators.items() if k != "ai"},
                             {k: v for k, v in plain.denominators.items() if k != "ai"})
            self.assertEqual(masked.weights, plain.weights)
            gens, voices = trainer.contrastive_keys(masked)
            pgens, pvoices = trainer.contrastive_keys(plain)
            self.assertEqual(voices, pvoices)
            self.assertEqual([g for g, x in zip(gens, flags) if not x], [g for g, x in zip(pgens, flags) if not x])
            self.assertTrue(all(g is None for g, x in zip(gens, flags) if x))
            rec = masked.mask_record()
            self.assertEqual(rec["cohorts"][MASKED]["rows"], sum(flags))
            with self.assertRaisesRegex(ValueError, "without Train rows"):
                self.populations(Path(d) / "x", cohorts=(MASKED, "no-such-cohort"))

    def test_masked_rows_are_never_anchors_positives_or_negatives(self):
        with tempfile.TemporaryDirectory() as d:
            _, masked = self.populations(d)
            gens, voices = trainer.contrastive_keys(masked)
            rng = random.Random(3)
            n = len(gens)
            for _ in range(200):
                idx = rng.sample(range(n), rng.randint(2, min(n, 12)))
                pos, neg = aux.contrastive_sets([gens[i] for i in idx], [voices[i] for i in idx])
                for k, i in enumerate(idx):
                    if masked.attribution_masked[i]:
                        self.assertFalse(bool(pos[k].any() or neg[k].any() or pos[:, k].any() or neg[:, k].any()))

    def test_voice_mixed_order_deals_masked_rows_outside_the_generator_chunks(self):
        with tempfile.TemporaryDirectory() as d:
            _, masked = self.populations(d)
            gens, voices = trainer.contrastive_keys(masked)
            n, bs = len(gens), 6
            order = aux.voice_mixed_order(gens, voices, bs, 2, 17)
            self.assertEqual(sorted(order), list(range(n)))
            flagged = [i for i in range(n) if masked.attribution_masked[i]]
            batches = math.ceil(n / bs)
            per = len(flagged) / batches
            cum, humans = 0, sum(g == aux.HUMAN for g in gens)
            ai_rows = sum(g not in (None, aux.HUMAN) for g in gens)
            dealt_ai = 0
            for b in range(batches):
                batch = order[b * bs:(b + 1) * bs]
                cum += sum(masked.attribution_masked[i] for i in batch)
                dealt_ai += sum(gens[i] not in (None, aux.HUMAN) for i in batch)
                if dealt_ai < ai_rows:
                    # While AI rows remain, masked rows arrive at their proportional share of each batch (the
                    # rule for rows outside the structure), never in per-generator chunks.
                    self.assertLessEqual(abs(cum - min(len(flagged), round((b + 1) * per))), 1)
            self.assertGreater(humans, 0)
            structure = aux.batch_structure(order, gens, voices, bs)
            self.assertEqual(structure["ai_rows"], ai_rows)


class DocumentFits(unittest.TestCase):
    def test_bin_balance_with_one_bin_trains_as_the_plain_fit(self):
        with tempfile.TemporaryDirectory() as d:
            rows = r2.weighted_rows(groups=2)
            for r in rows:
                if r["origin"] == "mixed":
                    r["share"] = .3
            root, config, rows = bundle_with(d, rows)
            arm = "E1share"
            balanced = config_with(d, root, rows, protocol(arm, balance=BALANCE), "balanced")
            plain = config_with(d, root, rows, protocol(arm), "plain")
            r2.fit(root, plain, Path(d) / "plain", arm=arm, final_step=True)
            r2.fit(root, balanced, Path(d) / "balanced", arm=arm, final_step=True)
            self.assertTrue(tensors_equal(r2.model_tensors(Path(d) / "plain" / "selected.pt"),
                                          r2.model_tensors(Path(d) / "balanced" / "selected.pt")))
            f = r2.read(Path(d) / "balanced" / "fit.json")
            rec = f["document_options"]["share_bin_balance"]
            self.assertEqual(rec["populated_bins"], 1)
            self.assertEqual(f["binding"]["document_options"]["share_bin_balance"], BALANCE)
            self.assertNotIn("document_options", r2.read(Path(d) / "plain" / "fit.json"))
            self.assertNotIn("document_options", r2.read(Path(d) / "plain" / "binding.json"))

    def test_bin_balance_and_mask_fit(self):
        with tempfile.TemporaryDirectory() as d:
            root, config, rows = bundle_with(d, spread_rows())
            arm = "E1con"
            both = config_with(d, root, rows, protocol(arm, balance=BALANCE, mask=[MASKED]), "both")
            plain = config_with(d, root, rows, protocol(arm), "plain")
            r2.fit(root, both, Path(d) / "both", arm=arm, final_step=True)
            r2.fit(root, plain, Path(d) / "plain", arm=arm, final_step=True)
            f, g = r2.read(Path(d) / "both" / "fit.json"), r2.read(Path(d) / "plain" / "fit.json")
            rec = f["document_options"]["share_bin_balance"]
            self.assertGreater(rec["populated_bins"], 3)
            self.assertAlmostEqual(rec["balanced_mixed_mass"], rec["total_mixed_mass"], places=12)
            self.assertIn("claude-x", f["ai_roster"]["unsupervised"])
            self.assertNotIn("claude-x", g["ai_roster"]["unsupervised"])
            self.assertEqual(f["document_options"]["attribution_mask"]["cohorts"][MASKED]["ai_rows"],
                             sum(r["split"] == "train" and r["cohort_id"] == MASKED for r in rows))
            self.assertLess(f["aux_heads"]["batch_structure"]["ai_rows"], g["aux_heads"]["batch_structure"]["ai_rows"])
            self.assertEqual(f["binding"]["document_options"]["attribution_unsupervised_cohort_ids"], [MASKED])
            self.assertEqual(f["train_population"], g["train_population"])
            bad = config_with(d, root, rows, protocol(arm, mask=["no-such-cohort"]), "bad")
            with self.assertRaisesRegex(ValueError, "without Train rows"):
                r2.fit(root, bad, Path(d) / "bad", arm=arm, final_step=True)

    def test_distillation_alpha_zero_equals_no_distillation_and_refusals(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            rows = spread_rows(groups=2)
            zero = next(r for r in rows if r["split"] == "train" and r["origin"] == "model_only")
            zero["cohort_weight"] = 0.  # a zero-weight Train row still has a target
            root, config, rows = bundle_with(d, rows)
            arm = "E1share"
            plain = config_with(d, root, rows, protocol(arm, balance=BALANCE), "plain")
            r2.fit(root, plain, d / "teacher", arm=arm, final_step=True)
            raw_train(root, plain, d / "teacher", d / "raw")
            m = r2.read(d / "raw" / "manifest.json")
            self.assertEqual(m["rows"], sum(r["split"] == "train" for r in rows))
            targets = targets_from_raw([d / "raw"], d / "targets.jsonl")
            spec = {"rule": doc.DISTILL_RULE, "alpha": 0., "temperature": 1.}
            kd0 = config_with(d, root, rows, protocol(arm, balance=BALANCE, distill=spec), "kd0")
            r2.fit(root, plain, d / "plain", arm=arm, final_step=True)
            r2.fit(root, kd0, d / "kd0", arm=arm, final_step=True, distill_targets=str(targets))
            self.assertTrue(tensors_equal(r2.model_tensors(d / "plain" / "selected.pt"),
                                          r2.model_tensors(d / "kd0" / "selected.pt")))
            strip = lambda rows: [{k: v for k, v in r.items() if k != "elapsed_seconds_this_invocation"} for r in rows]
            self.assertEqual(strip(r2.read(d / "plain" / "train-log.json")), strip(r2.read(d / "kd0" / "train-log.json")))
            self.assertEqual((d / "plain" / "dev_iter-raw.jsonl").read_bytes(), (d / "kd0" / "dev_iter-raw.jsonl").read_bytes())
            b = r2.read(d / "kd0" / "binding.json")["document_options"]["distill"]
            self.assertEqual(b["targets_sha256"], sha(targets.read_bytes()))
            self.assertFalse(b["soft_terms"])
            # alpha 0.5 trains differently and logs the soft terms.
            half = config_with(d, root, rows, protocol(arm, balance=BALANCE, distill=dict(spec, alpha=.5)), "kd5")
            r2.fit(root, half, d / "kd5", arm=arm, final_step=True, distill_targets=str(targets))
            self.assertFalse(tensors_equal(r2.model_tensors(d / "plain" / "selected.pt"),
                                           r2.model_tensors(d / "kd5" / "selected.pt")))
            terms = r2.read(d / "kd5" / "train-log.json")[0]["train_terms_mean_since_last_boundary"]
            self.assertGreater(terms["loss_distill_soft_origin"], 0.)
            self.assertEqual(r2.read(d / "kd5" / "fit.json")["document_options"]["distill"]["targets_sha256"],
                             sha(targets.read_bytes()))

            def refused(message, edit=None, cfg=kd0, path=True):
                t = targets_from_raw([d / "raw"], d / f"t-{len(list(d.iterdir()))}.jsonl", edit) if edit else targets
                with self.assertRaisesRegex(ValueError, message):
                    r2.fit(root, cfg, d / f"refused-{len(list(d.iterdir()))}", arm=arm, final_step=True,
                           distill_targets=str(t) if path else None)
            refused("Train rows have no target", lambda h, r: (dict(h, rows=len(r) - 1), r[1:]))
            refused("outside the Train block", lambda h, r: (dict(h, rows=len(r) + 1), r + [dict(r[0], id="extra")]))
            refused("duplicate", lambda h, r: (dict(h, rows=len(r) + 1), r + [r[0]]))
            refused("text digest", lambda h, r: (h, [dict(r[0], text_sha256="00" * 32)] + r[1:]))
            refused("AI roster", lambda h, r: (dict(h, ai_profiles=h["ai_profiles"][::-1]), r))
            refused("header row count", lambda h, r: (dict(h, rows=len(r) + 5), r))
            refused("origin", lambda h, r: (h, [dict(r[0], origin=[.5, .5, .5])] + r[1:]))
            refused("header schema", lambda h, r: (dict(h, schema="other"), r))
            refused("--distill-targets goes with", path=False)
            refused("--distill-targets goes with", cfg=plain)

    def test_raw_routes(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            root, config, rows = bundle_with(d, r2.weighted_rows(groups=2))
            r2.fit(root, config, d / "fit", final_step=True)
            raw_train(root, config, d / "fit", d / "train")
            got = [json.loads(x) for x in (d / "train" / "raw.jsonl").read_bytes().splitlines()]
            self.assertEqual([r["id"] for r in got], sorted(r["id"] for r in rows if r["split"] == "train"))
            m = r2.read(d / "train" / "manifest.json")
            self.assertEqual((m["schema"], m["partition"], m["source"]), (raw_full_a.RAW_HEADS_SCHEMA, "train",
                                                                          "bundle_train_block"))
            self.assertEqual(m["raw_sha256"], sha((d / "train" / "raw.jsonl").read_bytes()))
            self.assertEqual(m["ai_profiles"], r2.read(config / "contract.json")["ai_profiles"])
            # A rows file of the Dev-iter rows gives the exporter's Dev-iter raw rows byte for byte.
            dev = d / "rows" / "dev.jsonl"
            dev.parent.mkdir()
            dev.write_bytes(b"".join(json.dumps(r).encode() + b"\n" for r in rows if r["split"] == "dev_iter"))
            raw_train(root, config, d / "fit", d / "dev-raw", rows=str(dev), split="dev_iter")
            encoder = base.tiny_encoder(11)
            args = types.SimpleNamespace(data=str(root), config=str(config), fit=str(d / "fit"), artifact="unused",
                                         init=str(config / "init.json"), out=str(d / "export-raw"), device="cpu",
                                         part="dev_iter", allow_diagnostic_subset=False)
            with mock.patch.object(trainer, "load", lambda a, p, dv: (encoder, base.FakeTokenizer(),
                                                                      {"artifact_id": "synthetic-parent"})), \
                    mock.patch.object(exporter, "initializer", lambda path: base.init_for(encoder)):
                exporter.raw(args)
            ours = {json.loads(x)["id"]: x for x in (d / "dev-raw" / "raw.jsonl").read_bytes().splitlines()}
            theirs = {json.loads(x)["id"]: x for x in (d / "export-raw" / "raw.jsonl").read_bytes().splitlines()}
            self.assertEqual(ours, theirs)
            # Refusals: a row of another split, sealed splits, an existing output.
            with self.assertRaisesRegex(ValueError, "expected train"):
                raw_train(root, config, d / "fit", d / "wrong", rows=str(dev), split="train")
            for split in ("dev_select", "test"):
                with self.assertRaisesRegex(ValueError, "rows-file split"):
                    raw_train(root, config, d / "fit", d / ("sealed-" + split), rows=str(dev), split=split)
            with self.assertRaisesRegex(ValueError, "new output"):
                raw_train(root, config, d / "fit", d / "train")


class Harness(unittest.TestCase):
    def compare(self, d, data, config, name, extra=(), reference=HERE, arm="E1"):
        cmd = [sys.executable, "-I", "-B", str(HERE / "bitwise_full_a.py"), "compare", "--reference-tree",
               str(reference), "--candidate-tree", str(HERE), "--data", str(data), "--config", str(config),
               "--artifact", "unused", "--init", str(Path(config) / "init.json"), "--arm", arm, "--seed", "17",
               "--device", "cpu", "--out", str(Path(d) / name), "--synthetic-fixture", *extra]
        rc = subprocess.run(cmd, capture_output=True, timeout=900).returncode
        return rc, r2.read(Path(d) / name / "BITWISE.json")

    def test_kd_mode(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            root, config, rows = bundle_with(d, spread_rows(groups=2))
            r2.fit(root, config, d / "teacher")
            raw_train(root, config, d / "teacher", d / "raw")
            targets = targets_from_raw([d / "raw"], d / "targets.jsonl")
            spec = {"rule": doc.DISTILL_RULE, "alpha": 0., "temperature": 1.}
            kd0 = config_with(d, root, rows, protocol(distill=spec), "kd0")
            rc, report = self.compare(d, root, config, "kd0-report",
                                      ["--candidate-config", str(kd0), "--candidate-distill-targets", str(targets)])
            self.assertEqual(rc, 0, {k: v["first"] for k, v in report["sections"].items()})
            self.assertTrue(report["kd"]["kd_pass"] and report["bitwise_equal"] and report["kd"]["same_tree"])
            self.assertEqual(report["kd"]["targets_sha256"], sha(targets.read_bytes()))
            self.assertGreater(report["sections"]["steps"]["compared"], 0)
            kd5 = config_with(d, root, rows, protocol(distill=dict(spec, alpha=.5)), "kd5")
            rc, report = self.compare(d, root, config, "kd5-report",
                                      ["--candidate-config", str(kd5), "--candidate-distill-targets", str(targets)])
            self.assertEqual(rc, harness.EXIT_DIFFERENT)
            self.assertFalse(report["kd"]["kd_pass"])
            self.assertGreater(report["sections"]["steps"]["steps_differing"], 0)
            other = protocol(distill=spec)
            other["max_epochs"] = 1
            kdx = config_with(d, root, rows, other, "kdx")
            rc, report = self.compare(d, root, config, "kdx-report",
                                      ["--candidate-config", str(kdx), "--candidate-distill-targets", str(targets)])
            self.assertEqual(rc, harness.EXIT_DIFFERENT)
            self.assertEqual(report["kd"]["protocol"]["first"], ["/protocol/max_epochs"])

    @unittest.skipUnless(shutil.which("git"), "git unavailable")
    def test_options_absent_equal_the_base_trainer_bitwise(self):
        top = subprocess.run(["git", "-C", str(HERE), "rev-parse", "--show-toplevel"], capture_output=True, text=True)
        if top.returncode != 0:
            self.skipTest("not a git checkout")
        repo = top.stdout.strip()
        if subprocess.run(["git", "-C", repo, "cat-file", "-e", BASE_COMMIT + "^{commit}"],
                          capture_output=True).returncode != 0:
            self.skipTest("the base commit is not in this clone")
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            harness.extract(repo, BASE_COMMIT, d / "base")
            root, config, rows = bundle_with(d, spread_rows(groups=3))
            # Voice-mixed batching, the contrastive term, the adversary and the share-bin head, all trainer paths
            # the options touch, on row weights.
            p = copy.deepcopy(r2.row_protocol())
            arm = p["arms"]["E2wall"]
            arm.update(span_head=None, share_mode="head")
            p["arms"] = {"E1all": arm}
            cfg = config_with(d, root, rows, p, "all")
            rc, report = self.compare(d, root, cfg, "base-report", reference=d / "base" / "tree", arm="E1all")
            self.assertTrue(report["bitwise_equal"], {k: v["first"] for k, v in report["sections"].items()})
            self.assertEqual(rc, harness.EXIT_EQUAL_NOT_D3)
            self.assertGreater(report["sections"]["steps"]["compared"], 0)


if __name__ == "__main__":
    unittest.main()

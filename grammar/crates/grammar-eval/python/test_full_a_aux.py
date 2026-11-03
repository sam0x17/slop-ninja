"""Offline checks for the full-A training-only objectives, schedules and the
streaming shared-contract bundle reader. Synthetic tensors and text; CPU only.
"""
import copy
import hashlib
import json
import math
from pathlib import Path
import random
import tempfile
import unittest

import torch

import export_full_a
import mine_full_a
import full_a
import full_a_aux as aux
import full_a_bundle as bundle
import synthetic_full_a as synthetic
import train_full_a as trainer
from test_full_a import CLS, PAD, SEP, FakeTokenizer, make_model


class Reversal(unittest.TestCase):
    def test_forward_identity_backward_negated(self):
        x = torch.randn(3, 4, requires_grad=True)
        y = aux.reverse_gradient(x, .7)
        torch.testing.assert_close(y, x)
        y.sum().backward()
        torch.testing.assert_close(x.grad, torch.full_like(x, -.7))
        with self.assertRaises(ValueError):
            aux.reverse_gradient(x, -1.)

    def test_schedules(self):
        steps = 10
        const = {"kind": "constant", "max": .3, "delay_epochs": 1}
        self.assertEqual([aux.reversal_scale(const, s, steps) for s in (0, 9, 10, 50)], [0., 0., .3, .3])
        ramp = aux.validate_reversal({"kind": "linear_ramp", "max": 1., "ramp_epochs": 2})
        self.assertEqual([aux.reversal_scale(ramp, s, steps) for s in (0, 10, 20, 40)], [0., .5, 1., 1.])
        dann = aux.validate_reversal({"kind": "dann", "max": 1., "ramp_epochs": 1, "gamma": 10.})
        values = [aux.reversal_scale(dann, s, steps) for s in range(0, 11)]
        self.assertEqual(values[0], 0.)
        self.assertTrue(all(a < b for a, b in zip(values, values[1:])))
        self.assertAlmostEqual(values[-1], 2 / (1 + math.exp(-10)) - 1)
        for bad in ({"kind": "step", "max": 1.}, {"kind": "dann", "max": 1., "ramp_epochs": 1},
                    {"kind": "constant", "max": -1}, {"kind": "constant", "max": 1, "extra": 1}):
            with self.assertRaises(ValueError):
                aux.validate_reversal(bad)


class Adversary(unittest.TestCase):
    def test_head_learns_while_encoding_gradient_is_reversed(self):
        torch.manual_seed(0)
        adv = aux.StyleAdversary(3, 2, hidden=8, dimension=6)
        z = torch.randn(4, 6, requires_grad=True)
        clusters, axes = [0, 2, None, -1], [[1., 0.], None, [0., 1.], [1., 1.]]
        w, den = [1., 2., 1., 1.], {"cluster": 3., "axes": 3.}
        loss = aux.adversary_loss(adv, z, .5, clusters, axes, w, den)
        loss.backward()
        reversed_grad = z.grad.clone()
        head_grad = {n: p.grad.clone() for n, p in adv.named_parameters()}
        z.grad = None
        adv.zero_grad()
        # Reference without reversal: an outer reversal cancels the inner one.
        plain = aux.adversary_loss(adv, aux._Reverse.apply(z, 1.), 1., clusters, axes, w, den)
        plain.backward()
        torch.testing.assert_close(reversed_grad, -.5 * z.grad)
        for n, p in adv.named_parameters():
            torch.testing.assert_close(head_grad[n], p.grad)
        manual = (torch.nn.functional.cross_entropy(adv(z, 0)[0][:1], torch.tensor([0])) * 1 / 3 +
                  torch.nn.functional.cross_entropy(adv(z, 0)[0][1:2], torch.tensor([2])) * 2 / 3)
        logits = adv(z, 0)[1]
        for i in (0, 2, 3):
            manual = manual + torch.nn.functional.binary_cross_entropy_with_logits(
                logits[i], torch.tensor(axes[i])) * w[i] / 3
        self.assertAlmostEqual(float(loss), float(manual), places=5)


class Contrastive(unittest.TestCase):
    def test_positive_and_negative_rule(self):
        H = aux.HUMAN
        gens = ["a", "a", "b", "b", H, "a", None]
        voices = ["v1", "v2", "v1", "v3", None, "v1", "v1"]
        pos, neg = aux.contrastive_sets(gens, voices)
        # a/v1: positive a/v2 (other voice), not a/v1 (same voice); negatives b/v1 (same voice) and the human.
        self.assertEqual(pos[0].nonzero().flatten().tolist(), [1])
        self.assertEqual(neg[0].nonzero().flatten().tolist(), [2, 4])
        # b/v3 has no same-voice other generator: only the human is a negative; its positive is b/v1.
        self.assertEqual(pos[3].nonzero().flatten().tolist(), [2])
        self.assertEqual(neg[3].nonzero().flatten().tolist(), [4])
        self.assertFalse(pos[4].any() or neg[4].any())  # humans are never anchors
        self.assertFalse(pos[:, 6].any() or neg[:, 6].any())  # excluded rows take no part

    def test_loss_matches_formula_and_is_zero_without_anchors(self):
        torch.manual_seed(1)
        p = torch.nn.functional.normalize(torch.randn(5, 4), dim=-1).requires_grad_(True)
        gens = ["a", "a", "b", "b", aux.HUMAN]
        voices = ["v1", "v2", "v1", "v2", None]
        pos, neg = aux.contrastive_sets(gens, voices)
        w = [1., 2., 1., 1., 5.]
        loss, anchors = aux.supervised_contrastive(p, pos, neg, w, .5)
        self.assertEqual(anchors, 4)
        s = (p @ p.t() / .5).detach()
        total, mass = 0., 0.
        for i in range(4):
            allowed = [j for j in range(5) if pos[i, j] or neg[i, j]]
            den = math.log(sum(math.exp(s[i, j]) for j in allowed))
            ps = [j for j in range(5) if pos[i, j]]
            total += w[i] * -sum(float(s[i, j]) - den for j in ps) / len(ps)
            mass += w[i]
        self.assertAlmostEqual(float(loss), total / mass, places=5)
        loss.backward()
        self.assertTrue(bool(p.grad.abs().sum() > 0))
        none, n = aux.supervised_contrastive(p, *aux.contrastive_sets(["a", "a"], ["v", "v"]), [1., 1.], .5)
        self.assertEqual((float(none), n), (0., 0))


class VoiceMixedOrder(unittest.TestCase):
    def setUp(self):
        rng = random.Random(3)
        self.gens, self.voices = [], []
        for g in range(6):
            for v in range(12):
                for _ in range(rng.randint(1, 3)):
                    self.gens.append(f"g{g}")
                    self.voices.append(f"v{v}")
        for _ in range(60):
            self.gens.append(aux.HUMAN)
            self.voices.append(None)
        for _ in range(7):
            self.gens.append(f"g{rng.randint(0, 5)}")
            self.voices.append(None)

    def test_permutation_deterministic_and_structured(self):
        order = aux.voice_mixed_order(self.gens, self.voices, 16, 3, seed=9)
        self.assertEqual(sorted(order), list(range(len(self.gens))))
        # Batches are exactly 16 rows, so the flat order chunks align with them.
        self.assertTrue(all(len(set(order[s:s + 16])) == len(order[s:s + 16]) for s in range(0, len(order), 16)))
        self.assertEqual(order, aux.voice_mixed_order(self.gens, self.voices, 16, 3, seed=9))
        self.assertNotEqual(order, aux.voice_mixed_order(self.gens, self.voices, 16, 3, seed=10))
        mixed = aux.batch_structure(order, self.gens, self.voices, 16)
        shuffled = list(range(len(self.gens)))
        random.Random(9).shuffle(shuffled)
        plain = aux.batch_structure(shuffled, self.gens, self.voices, 16)
        self.assertGreater(mixed["anchor_fraction"], plain["anchor_fraction"])
        self.assertGreater(mixed["anchor_fraction"], .6)
        # Humans are spread across batches rather than bunched at the end.
        per_batch = [sum(self.gens[i] == aux.HUMAN for i in order[s:s + 16]) for s in range(0, len(order), 16)]
        self.assertLessEqual(max(per_batch[:-1]) - min(per_batch[:-1]), 2)


class VoiceMixedImbalance(unittest.TestCase):
    """Property checks over imbalanced generators (the configurations that once
    duplicated humans) and stationarity of small labels within the epoch."""

    def test_permutation_and_exact_batch_sizes_over_random_configurations(self):
        for trial in range(200):
            r = random.Random(trial)
            gens, voices = [], []
            for g in range(r.randint(1, 12)):
                for _ in range(r.choice([1, 2, 5, 30, 200])):
                    gens.append(f"g{g}")
                    voices.append(None if r.random() < .3 else f"v{r.randint(0, 30)}")
            for _ in range(r.randint(0, 400)):
                gens.append(aux.HUMAN)
                voices.append(None)
            for _ in range(r.randint(0, 5)):
                gens.append(None)
                voices.append(None)
            bs = r.choice([2, 3, 7, 16, 33, 64, 128])
            pg = r.randint(1, min(bs, 5))
            order = aux.voice_mixed_order(gens, voices, bs, pg, trial)
            self.assertEqual(sorted(order), list(range(len(gens))), (trial, bs, pg))
            # Every logical batch but the last has exactly bs rows: the trainer
            # slices the flat order, so this is checked through human spreading.
            humans = sum(g == aux.HUMAN for g in gens)
            n_batches = math.ceil(len(gens) / bs)
            if humans and n_batches > 2:
                batches = [order[k:k + bs] for k in range(0, len(order), bs)]
                per = [sum(gens[i] == aux.HUMAN for i in b) for b in batches]
                with_ai = [k for k, b in enumerate(batches) if any(gens[i] not in (None, aux.HUMAN) for i in b)]
                # Before the last batch holding AI rows, no batch takes more
                # humans than its proportional share plus rounding.
                share = humans / n_batches
                for k in range(with_ai[-1] if with_ai else 0):
                    self.assertLessEqual(per[k], math.ceil(share) + 1, (trial, bs, pg, k))

    def test_small_labels_are_spread_through_the_epoch(self):
        gens, voices = [], []
        for g in range(4):
            gens += [f"local{g}"] * 3000
            voices += [f"v{i % 200}" for i in range(3000)]
        for g in range(6):
            gens += [f"frontier{g}"] * 300
            voices += [f"v{i % 200}" for i in range(300)]
        gens += [aux.HUMAN] * 15000
        voices += [None] * 15000
        for pg in (2, 4):
            order = aux.voice_mixed_order(gens, voices, 64, pg, seed=1)
            positions = {}
            for k, i in enumerate(order):
                positions.setdefault(gens[i], []).append(k / len(order))
            for g, pos in positions.items():
                if g == aux.HUMAN:
                    continue
                self.assertAlmostEqual(sum(pos) / len(pos), .5, delta=.02, msg=g)
                deciles = [sum(1 for x in pos if d / 10 <= x < (d + 1) / 10) for d in range(10)]
                expected = len(pos) / 10
                self.assertTrue(all(abs(c - expected) <= .15 * expected + pg for c in deciles), (g, deciles))
            # Contrastive structure survives: most AI rows are anchors.
            self.assertGreater(aux.batch_structure(order[:64 * 100], gens, voices, 64)["anchor_fraction"], .8)


class ShareSupervision(unittest.TestCase):
    def test_share_bins(self):
        self.assertEqual([aux.share_bin(x) for x in (.01, .1, .3, .5, .8, .95, 1.)], [0, 1, 2, 3, 4, 5, 6])
        for bad in (0., 1.2):
            with self.assertRaises(ValueError):
                aux.share_bin(bad)
        head = aux.ShareBins(6, 4)
        z = torch.randn(3, 6, requires_grad=True)
        loss = aux.share_bin_loss(head, z, [6, None, 2], [1., 5., 1.], 2.)
        loss.backward()
        self.assertTrue(bool(z.grad[0].abs().sum() > 0) and bool(z.grad[1].abs().sum() == 0))

    def test_conditional_share_reaches_q_when_p_mixed_is_small(self):
        q = torch.tensor([.3], requires_grad=True)
        origin = torch.tensor([[.05, .94, .01]])

        def out():
            return {"origin": origin, "log_origin": origin.log(), "encoding": torch.zeros(1, 4),
                    "share": origin[:, 1] + origin[:, 2] * q, "conditional_mixed_share": q,
                    "human_logits": torch.zeros(1, 1), "ai_logits": torch.zeros(1, 2), "token_logits": None}
        targets = [{"origin": 2, "share": 1., "human": None, "ai": None}]
        den = {"origin": 1., "share": 1., "ai": 1., "token": 1., "conditional_share": 1.}
        plain = torch.autograd.grad(full_a.objective(out(), targets, [1.], den), q)[0]
        with_term = torch.autograd.grad(full_a.objective(out(), targets, [1.], den, conditional_weight=1.), q)[0]
        self.assertLess(abs(float(plain)), .01)  # the document MSE barely moves q
        self.assertLess(float(with_term), -1.)  # the conditional BCE pushes q toward the full-rewrite share
        model_only = [{"origin": 1, "share": 1., "human": None, "ai": None}]
        g = torch.autograd.grad(full_a.objective(out(), model_only, [1.], den, conditional_weight=1.), q,
                                allow_unused=True)[0]
        self.assertLess(abs(float(g)), .02)  # only mixed rows carry the conditional term


class BundleContract(unittest.TestCase):
    def test_style_axes_list_only(self):
        self.assertEqual(bundle.axes_of(["b", "a", "b"]), ("a", "b"))
        self.assertEqual(bundle.axes_of(None), ())
        with self.assertRaises(ValueError):
            bundle.axes_of({"persona_axis": "age", "inventor": "mistral7b"})

    def test_class_balanced_weighting_sets_class_masses(self):
        def meta(group, origin, weight=1.):
            m = bundle.Meta()
            m.source_group, m.origin, m.cohort_weight = group, origin, weight
            return m
        # 30 human groups, positives concentrated in 2 groups (the measured imbalance).
        rows = [meta(f"h{g}", 0) for g in range(30)] + [meta("p0", 0), meta("p0", 1), meta("p0", 2, 2.),
                                                         meta("p1", 1), meta("p1", 1)]
        fob = bundle.train_weights(rows, {"scheme": "family_origin_balanced", "positive_mass": None})
        self.assertAlmostEqual(sum(fob), 1.)
        self.assertLess(sum(w for w, m in zip(fob, rows) if m.origin), .1)
        cb = bundle.train_weights(rows, bundle.validate_weighting({"scheme": "class_balanced", "positive_mass": .4}))
        self.assertAlmostEqual(sum(w for w, m in zip(cb, rows) if m.origin), .4)
        self.assertAlmostEqual(sum(w for w, m in zip(cb, rows) if not m.origin), .6)
        # Within the AI class: groups equal, origin cells equal in a group, cohort weight applied.
        p0_model, p0_mixed, p1a = cb[31], cb[32], cb[33]
        self.assertAlmostEqual(p0_mixed, 2 * p0_model)
        self.assertAlmostEqual(cb[33], cb[34])
        for bad in ({"scheme": "class_balanced", "positive_mass": 1.}, {"scheme": "class_balanced", "positive_mass": None},
                    {"scheme": "family_origin_balanced", "positive_mass": .5}, {"scheme": "other", "positive_mass": None}):
            with self.assertRaises(ValueError):
                bundle.validate_weighting(bad)
        with self.assertRaises(ValueError):
            bundle.train_weights(rows[:30], {"scheme": "class_balanced", "positive_mass": .5})

    def test_rows_digest_tracks_rows_and_labels(self):
        with tempfile.TemporaryDirectory() as d:
            rows = synthetic.factorial_rows(groups=2, splits=("train", "dev_iter"))
            root = Path(d) / "b"
            synthetic.write_bundle(root, rows)
            labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
            dev = lambda root: bundle.Population(root, "dev_iter", full_a.hf_tokenize(FakeTokenizer()),
                                                 {PAD, CLS, SEP}, labels, cohort_weighted=False)
            a = dev(root).rows_digest()
            self.assertEqual(a, dev(root).rows_digest())
            changed = copy.deepcopy(rows)
            k = next(i for i, r in enumerate(changed) if r["split"] == "dev_iter" and r["generator_label"])
            changed[k]["generator_label"] = [x for x in labels if x != changed[k]["generator_label"]][0]
            other = Path(d) / "c"
            synthetic.write_bundle(other, changed)
            self.assertNotEqual(a, dev(other).rows_digest())


class MiningBinding(unittest.TestCase):
    def test_resume_refuses_another_binding_and_drops_a_partial_line(self):
        with tempfile.TemporaryDirectory() as d:
            out = Path(d)
            fit = out / "fit"
            fit.mkdir()
            (fit / "fit.json").write_text("{}")
            report = {"selected_checkpoint_sha256": "ck"}
            b = mine_full_a.score_binding(fit, report, {"rows.jsonl": "r"}, "pool", ("s", 1, 4), "fp32", 4096)
            mine_full_a.bind(out, b)
            mine_full_a.bind(out, b)  # same binding resumes
            for other in (mine_full_a.score_binding(fit, report, {"rows.jsonl": "r"}, "pool", ("s", 1, 8), "fp32", 4096),
                          mine_full_a.score_binding(fit, {"selected_checkpoint_sha256": "x"}, {"rows.jsonl": "r"},
                                                    "pool", ("s", 1, 4), "fp32", 4096),
                          mine_full_a.score_binding(fit, report, {"rows.jsonl": "r"}, "pool2", ("s", 1, 4), "fp32", 4096)):
                with self.assertRaisesRegex(ValueError, "binding differs"):
                    mine_full_a.bind(out, other)
            (fit / "fit.json").write_text('{"changed": 1}')
            with self.assertRaisesRegex(ValueError, "binding differs"):
                mine_full_a.bind(out, mine_full_a.score_binding(fit, report, {"rows.jsonl": "r"}, "pool",
                                                                ("s", 1, 4), "fp32", 4096))
            scores = out / "scores.jsonl"
            scores.write_bytes(b'{"id": "a"}\n{"id": "b"}\n{"id": "c", "p_an')
            self.assertEqual(mine_full_a.completed_ids(scores), {"a", "b"})
            self.assertEqual(scores.read_bytes(), b'{"id": "a"}\n{"id": "b"}\n')
            fresh = Path(d) / "fresh"
            fresh.mkdir()
            (fresh / "scores.jsonl").write_text("")
            with self.assertRaisesRegex(ValueError, "without a binding"):
                mine_full_a.bind(fresh, b)


class Schedule(unittest.TestCase):
    def test_linear_horizon_equals_warmup_linear_and_cosine_floor(self):
        for total, warm in ((10, 2), (7, 0), (33, 3)):
            a, b = full_a.warmup_linear(total, warm), full_a.warmup_horizon(total, warm, "linear")
            for s in range(total + 1):
                self.assertAlmostEqual(a(s), b(s))
        f = full_a.warmup_horizon(10, 2, "cosine", .1)
        self.assertEqual(round(f(0), 6), .5)
        self.assertEqual(f(2), 1.)
        self.assertAlmostEqual(f(6), .1 + .9 * .5)
        self.assertAlmostEqual(f(10), .1)
        self.assertAlmostEqual(f(25), .1)  # beyond the horizon: floor
        g = full_a.warmup_horizon(10, 0, "linear", .2)
        self.assertAlmostEqual(g(5), .2 + .8 * .5)
        for bad in ((10, 10, "linear"), (10, 2, "step"), (0, 0, "cosine")):
            with self.assertRaises(ValueError):
                full_a.warmup_horizon(*bad)


class GradientCache(unittest.TestCase):
    def test_cached_contrastive_gradient_equals_direct_backprop(self):
        with tempfile.TemporaryDirectory() as d:
            rows = synthetic.factorial_rows(groups=2, splits=("train",), held_out_dev=False)
            synthetic.write_bundle(Path(d), rows)
            labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
            pop = bundle.Population(Path(d), "train", full_a.hf_tokenize(FakeTokenizer()), {PAD, CLS, SEP},
                                    labels, cohort_weighted=True)
        arm = dict(synthetic.protocol_template()["arms"]["E1con"])
        model = make_model()
        model.heads.ai = torch.nn.Linear(768, len(labels))
        model.token_loss_weight = 0.
        heads = type("H", (), {})()
        torch.manual_seed(4)
        projection = aux.Projection(768, 16, 8)
        heads.modules = lambda: {"projection": projection}
        indices = list(range(len(pop)))
        params = [p for p in model.parameters() if p.requires_grad] + list(projection.parameters())

        def grads():
            g = {id(p): p.grad.clone() for p in params if p.grad is not None}
            for p in params:
                p.grad = None
            return g

        model.train()
        for budget in (8192, 64):  # one microbatch, then one document per microbatch
            trainer.train_step(model, heads, arm, pop, indices, "cpu", PAD, budget, 1., 0., {})
            cached = grads()
            # Direct: one graph over the whole logical batch.
            ids, mask, content, labels_t, words = pop.batch(indices, "cpu", PAD)
            out = model(ids, mask, content, words=words)
            joint = full_a.objective(out, [pop.targets(i) for i in indices], [pop.weights[i] for i in indices],
                                     pop.denominators, labels_t, 0., 1.)
            pos, neg = aux.contrastive_sets(*[[x[i] for i in indices] for x in trainer.contrastive_keys(pop)])
            c, n = aux.supervised_contrastive(projection(out["encoding"]), pos, neg,
                                              [pop.weights[i] for i in indices], arm["contrastive"]["temperature"])
            self.assertGreater(n, 0)
            (joint + arm["contrastive"]["loss_weight"] * c).backward()
            direct = grads()
            self.assertEqual(set(cached), set(direct))
            for k in direct:
                torch.testing.assert_close(cached[k], direct[k], atol=2e-5, rtol=1e-4)


class BundleReader(unittest.TestCase):
    def tokenize(self):
        return full_a.hf_tokenize(FakeTokenizer())

    def test_text_ref_aliases_counts_and_compact_arrays(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            rows = synthetic.factorial_rows(groups=2, splits=("train", "development_iter"))
            store = root / "texts.bin"
            with store.open("wb") as f:
                for r in rows[::2]:
                    data = r.pop("text").encode()
                    r["text_ref"] = {"path": "texts.bin", "offset": f.tell(), "length": len(data)}
                    f.write(data)
            synthetic.write_bundle(root, rows)
            labels = bundle.ai_roster(root)
            self.assertEqual(labels, ["claude-x", "gen-a", "gen-b"])
            pop = bundle.Population(root, "dev_iter", self.tokenize(), {PAD, CLS, SEP}, labels, cohort_weighted=False)
            self.assertEqual(len(pop), 20)
            self.assertEqual(pop.ids.dtype.itemsize + pop.labels.dtype.itemsize + pop.flags.dtype.itemsize, 6)
            self.assertEqual(int(pop.starts[-1]), int(pop.lengths.sum()))
            self.assertAlmostEqual(sum(pop.weights), 1.)
            with self.assertRaisesRegex(ValueError, "outside the AI roster"):
                bundle.Population(root, "train", self.tokenize(), {PAD, CLS, SEP}, labels[1:], cohort_weighted=True)
            narrow = bundle.Population(root, "dev_iter", self.tokenize(), {PAD, CLS, SEP}, labels[1:],
                                       cohort_weighted=False)
            self.assertEqual(dict(narrow.unrostered), {"claude-x": 6})
            self.assertTrue(all(narrow.targets(i)["ai"] is None for i, m in enumerate(narrow.meta)
                                if m.label == "claude-x"))
            counts = pop.counts()
            self.assertEqual(sum(c["rows"] for c in counts), 20)
            self.assertTrue(all(c["split"] == "dev_iter" for c in counts))
            # A changed text_ref target fails the exact binding.
            raw = store.read_bytes()
            store.write_bytes(raw[:3] + (b"X" if raw[3:4] != b"X" else b"Y") + raw[4:])
            with self.assertRaisesRegex(ValueError, "exact text binding"):
                bundle.Population(root, "train", self.tokenize(), {PAD, CLS, SEP}, labels, cohort_weighted=True)

    def test_row_validation(self):
        rows = synthetic.factorial_rows(groups=1, splits=("train",))
        good = rows[1]
        bundle.parse_row(good, 1)
        for change in ({"origin": "unknown"}, {"share": 1.2}, {"generator_label": None},
                       {"split": "holdout"}, {"cohort_weight": -1}, {"held_out_style": 1},
                       {"text_ref": {"path": "x", "offset": 0, "length": 1}}, {"extra": 1}):
            with self.assertRaises(ValueError):
                bundle.parse_row(dict(good, **change), 1)
        human = rows[0]
        with self.assertRaises(ValueError):
            bundle.parse_row(dict(human, generator_label="gen-a"), 1)
        with self.assertRaises(ValueError):
            bundle.parse_row(dict(human, share=.5), 1)
        # A full rewrite of a human source is mixed with share 1; share 0 is never mixed.
        mixed = dict(good, origin="mixed", share=1.)
        self.assertEqual(bundle.parse_row(mixed, 1)[0].share, 1.)
        with self.assertRaises(ValueError):
            bundle.parse_row(dict(mixed, share=0.), 1)

    def test_weights_validation_and_zero_weight_cohort_removal(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            rows = synthetic.factorial_rows(groups=2, splits=("train",))
            for r in rows:
                if r["cohort_id"] == "hosted_frontier_v1":
                    r["cohort_weight"] = 0.
            synthetic.write_bundle(root, rows)
            labels = ["gen-a", "gen-b"]
            self.assertEqual(bundle.ai_roster(root), labels)
            pop = bundle.Population(root, "train", self.tokenize(), {PAD, CLS, SEP}, labels, cohort_weighted=True)
            self.assertEqual(sum(pop.dropped.values()), 6)
            doc = synthetic.weights_doc(rows)
            log = bundle.finish_weight_log(bundle.validate_weights(doc, pop.meta, pop.dropped), pop, None)
            frontier = log["cohorts"]["hosted_frontier_v1"]
            self.assertEqual((frontier["records"], frontier["labels"]["claude-x"]["trained_records"]), (6, 0))
            self.assertAlmostEqual(sum(c["effective_mass"] for c in log["cohorts"].values()), 1.)
            wrong = copy.deepcopy(doc)
            wrong["cohorts"]["local_v1"]["labels"]["gen-a"]["records"] += 1
            with self.assertRaisesRegex(ValueError, "Train rows"):
                bundle.validate_weights(wrong, pop.meta, pop.dropped)
            missing = copy.deepcopy(doc)
            del missing["cohorts"]["humans_v1"]
            with self.assertRaisesRegex(ValueError, "absent"):
                bundle.validate_weights(missing, pop.meta, pop.dropped)

    def test_assembler_row_weights_are_used_as_is_and_checked_by_counts_and_sums(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            rows = synthetic.factorial_rows(groups=2, splits=("train",))
            # Final per-row assembler weights: distinct within a cohort/label.
            for i, r in enumerate(rows):
                r["cohort_weight"] = 0.25 + (i % 7) * 0.5
            doc = synthetic.row_weights_doc(rows)
            synthetic.write_bundle(root, rows, doc)
            scheme = bundle.validate_weighting({"scheme": bundle.ROW_WEIGHT_SCHEME, "positive_mass": None})
            pop = bundle.Population(root, "train", self.tokenize(), {PAD, CLS, SEP}, bundle.ai_roster(root),
                                    cohort_weighted=True, weighting=scheme)
            total = sum(r["cohort_weight"] for r in rows)
            by_id = {r["id"]: r["cohort_weight"] / total for r in rows}
            for m, w in zip(pop.meta, pop.weights):
                self.assertAlmostEqual(w, by_id[m.id], places=14)  # no family/origin rebalancing
            log = bundle.finish_weight_log(
                bundle.validate_weights(doc, pop.meta, pop.dropped, scheme=bundle.ROW_WEIGHT_SCHEME), pop, None)
            self.assertEqual(log["base_weighting"], bundle.ROW_WEIGHT_SCHEME)
            self.assertAlmostEqual(sum(c["effective_mass"] for c in log["cohorts"].values()), 1.)
            local = doc["cohorts"]["local_v1"]["labels"]["gen-a"]
            self.assertAlmostEqual(log["cohorts"]["local_v1"]["labels"]["gen-a"]["effective_mass"],
                                   local["weight_sum"] / total)
            for path, delta, msg in ((("records",), 1, "Train rows"), (("weight_sum",), 1e-3, "weight_sum"),
                                     (("weight_min",), -1e-3, "weight_min"), (("weight_max",), 1e-3, "weight_max")):
                bad = copy.deepcopy(doc)
                bad["cohorts"]["local_v1"]["labels"]["gen-a"][path[0]] += delta
                with self.assertRaisesRegex(ValueError, msg):
                    bundle.validate_weights(bad, pop.meta, pop.dropped, scheme=bundle.ROW_WEIGHT_SCHEME)
            bad = copy.deepcopy(doc)
            bad["train_weight_sum"] += 1.
            with self.assertRaisesRegex(ValueError, "train_weight_sum"):
                bundle.validate_weights(bad, pop.meta, pop.dropped, scheme=bundle.ROW_WEIGHT_SCHEME)
            # The weights.json form must match the protocol's scheme in both directions.
            with self.assertRaisesRegex(ValueError, "does not match"):
                bundle.validate_weights(doc, pop.meta, pop.dropped, scheme="family_origin_balanced")
            with self.assertRaisesRegex(ValueError, "does not match"):
                bundle.validate_weights(synthetic.weights_doc(synthetic.factorial_rows(groups=2, splits=("train",))),
                                        pop.meta, pop.dropped, scheme=bundle.ROW_WEIGHT_SCHEME)
            with self.assertRaises(ValueError):
                bundle.validate_weighting({"scheme": bundle.ROW_WEIGHT_SCHEME, "positive_mass": .3})


class MiningPool(unittest.TestCase):
    def test_only_train_mining_humans_are_read_and_sampling_is_deterministic(self):
        def doc(i, split="train_mining", origin="human_only"):
            text = f"synthetic pool text {i}"
            return {"id": f"p{i}", "frozen_split": split, "split_assignment": {"split": split}, "origin": origin,
                    "text": text, "text_sha256": hashlib.sha256(text.encode()).hexdigest(), "source_group": "f"}
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "pool.jsonl"
            path.write_text("".join(json.dumps(doc(i)) + "\n" for i in range(200)))
            rows = list(mine_full_a.pool_rows(path))
            self.assertEqual(len(rows), 200)
            self.assertNotIn("text", rows[0][0])
            kept = [r["id"] for r, _ in mine_full_a.pool_rows(path, ("s", 1, 4))]
            self.assertEqual(kept, [r["id"] for r, _ in mine_full_a.pool_rows(path, ("s", 1, 4))])
            self.assertTrue(20 < len(kept) < 80)
            for bad in (doc(0, "cal"), doc(0, "dev_iter"), doc(0, "test"), doc(0, "train"),
                        doc(0, origin="mixed"), dict(doc(0), text="changed")):
                path.write_text(json.dumps(bad) + "\n")
                with self.assertRaises(ValueError):
                    list(mine_full_a.pool_rows(path))


class ExportRoles(unittest.TestCase):
    def test_empty_roster_returns_other_without_presence(self):
        out = {"human_logits": torch.zeros(1, 1), "ai_logits": torch.tensor([[2., 0.]])}
        r = export_full_a.roles(out, {"human_involvement_probability": .4, "ai_involvement_probability": .7},
                                {"human": [], "ai": ["gen-a", "gen-b"]}, {"human": 1., "ai": .5})
        self.assertEqual(r["human"]["profile"], "other_human")
        self.assertEqual(r["human"]["conditional_named_scores"], {})
        self.assertFalse(r["human"]["fallback_asserts_presence"])
        self.assertEqual(r["ai"]["profile"], "gen-a")

    def test_empty_roster_head_is_exported_with_zero_rows(self):
        heads = torch.nn.Module()
        heads.human, heads.ai = torch.nn.Linear(768, 1), torch.nn.Linear(768, 2)
        heads.base_temperature, heads.text_mean, heads.text_scale = 1.5, torch.zeros(768), torch.ones(768)
        model = type("M", (), {"heads": heads, "span": None})()
        arm = {"span_head": None, "share_mode": "head"}
        contract = {"human_profiles": [], "ai_profiles": ["gen-a", "gen-b"]}
        with tempfile.NamedTemporaryFile() as init:
            a = export_full_a.heads_artifact(model, arm, contract, {"contract_sha256": "c"}, init.name,
                                             {"method": "identity_v1"}, {"human": 1., "ai": 1.}, {})
        self.assertEqual(a["architecture"]["human_profiles"], 0)
        self.assertEqual(a["weights"]["human.weight"], {"shape": [0, 768], "values": []})
        self.assertEqual(a["weights"]["human.bias"], {"shape": [0], "values": []})
        self.assertEqual(a["weights"]["ai.weight"]["shape"], [2, 768])


if __name__ == "__main__":
    unittest.main()

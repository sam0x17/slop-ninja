"""Offline full-A checks on synthetic tensors and a tiny random ModernBERT.

No corpus, artifact, provider, detector or network access. CPU only.
"""
import copy
import hashlib
import json
import math
from pathlib import Path
import re
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import torch
from transformers import ModernBertConfig, ModernBertForSequenceClassification

import export_full_a
import full_a
import full_a_aux as aux
import qualify_full_a
import shared_a as shared
import full_a_bundle as bundle
import synthetic_full_a as synthetic
import train_full_a as trainer
from warm_start_a import BASE_TEMPERATURE, WarmStartA

CLS, SEP, PAD = 1, 2, 0


class FakeTokenizer:
    """Whitespace-attached word pieces with character offsets, CLS/SEP framing."""
    all_special_ids = [PAD, CLS, SEP]
    pad_token_id = PAD

    def __call__(self, text, add_special_tokens=True, truncation=False, padding=False, return_offsets_mapping=False):
        assert add_special_tokens and not truncation and not padding
        ids, offsets = [CLS], [(0, 0)]
        for m in re.finditer(r"\s*\S+|\s+$", text):
            word = m.group(0)
            ids.append(3 + int(hashlib.sha256(word.strip().encode()).hexdigest(), 16) % 60)
            offsets.append((m.start(), m.end()))
        ids.append(SEP)
        offsets.append((0, 0))
        return {"input_ids": ids, "offset_mapping": offsets}


def tiny_encoder(seed=3):
    torch.manual_seed(seed)
    config = ModernBertConfig(vocab_size=64, hidden_size=768, intermediate_size=32, num_hidden_layers=2,
                              num_attention_heads=12, max_position_embeddings=2048, local_attention=8,
                              global_attn_every_n_layers=2, pad_token_id=PAD, bos_token_id=CLS, eos_token_id=SEP,
                              cls_token_id=CLS, sep_token_id=SEP, classifier_pooling="mean", num_labels=3,
                              attention_dropout=0., embedding_dropout=0., mlp_dropout=0., classifier_dropout=0.,
                              reference_compile=False)
    config._attn_implementation = "eager"
    model = ModernBertForSequenceClassification(config).eval()
    with torch.no_grad():
        model.classifier.weight.normal_(std=.05)
        model.classifier.bias.normal_(std=.05)
    return model


def init_for(encoder):
    return {"base_temperature": BASE_TEMPERATURE,
            "weights": {"origin.weight": {"shape": [3, 768], "values": encoder.classifier.weight.flatten().tolist()},
                        "origin.bias": {"shape": [3], "values": encoder.classifier.bias.flatten().tolist()}}}


def heads_for(encoder, human=2, ai=2):
    return WarmStartA(init_for(encoder), [0.] * 768, [1.] * 768, human, ai, "text")


def make_model(span=None, share_mode="head", seed=3, dtype=None):
    encoder = tiny_encoder(seed)
    head = None if span is None else full_a.SpanHead(span)
    return full_a.FullA(encoder, heads_for(encoder), head, share_mode, dtype)


def batch_of(texts, special=frozenset((PAD, CLS, SEP))):
    tok = full_a.hf_tokenize(FakeTokenizer())
    seqs = [tok(t)[0] for t in texts]
    n = max(map(len, seqs))
    ids = torch.full((len(seqs), n), PAD)
    mask = torch.zeros((len(seqs), n), dtype=torch.long)
    content = torch.zeros((len(seqs), n), dtype=torch.bool)
    for i, s in enumerate(seqs):
        ids[i, :len(s)] = torch.tensor(s)
        mask[i, :len(s)] = 1
        content[i, :len(s)] = torch.tensor(full_a.content_mask_for(s, special))
    return ids, mask, content


TEXTS = ["The quiet garden held a basket of pears.",
         "Rain fell steadily over the old stone bridge while we waited for the coach to arrive.",
         "Short note."]


class TokenLabels(unittest.TestCase):
    def labels(self, text, origin, spans):
        ids, offsets = full_a.hf_tokenize(FakeTokenizer())(text)
        return full_a.token_labels(text, ids, offsets, {PAD, CLS, SEP}, origin, spans)

    def test_policy_by_origin_and_span(self):
        text = "Alpha beta gamma delta."
        self.assertEqual(self.labels(text, 0, None), [-1, 0, 0, 0, 0, -1])
        self.assertEqual(self.labels(text, 0, []), [-1, 0, 0, 0, 0, -1])
        self.assertEqual(self.labels(text, 1, None), [-1, 1, 1, 1, 1, -1])
        self.assertEqual(self.labels(text, 1, [[0, len(text.encode())]]), [-1, 1, 1, 1, 1, -1])
        self.assertEqual(self.labels(text, 2, None), [-1] * 6)
        self.assertEqual(self.labels(text, None, None), [-1] * 6)
        # " beta gamma" replaced: leading whitespace never decides a label.
        self.assertEqual(self.labels(text, 2, [[5, 16]]), [-1, 0, 1, 1, 0, -1])
        # Span ending inside "delta." makes that token ambiguous, not human.
        self.assertEqual(self.labels(text, 2, [[17, 20]]), [-1, 0, 0, 0, -1, -1])

    def test_utf8_byte_spans_and_rejections(self):
        text = "Café au lait — très bien."
        start = len("Café au lait ".encode())
        end = start + len("— très".encode())
        labels = self.labels(text, 2, [[start, end]])
        self.assertEqual(labels, [-1, 0, 0, 0, 1, 1, 0, -1])
        for bad in ([[0, 4]], [[3, 2]], [[0, 2], [1, 3]], [[0, 999]], [[-1, 2]], [[0.0, 2]], [[2, 2]]):
            with self.assertRaises((ValueError, TypeError)):
                self.labels(text, 2, bad)
        with self.assertRaises(ValueError):
            self.labels(text, 0, [[0, 3]])
        with self.assertRaises(ValueError):
            self.labels(text, 1, [[0, 3]])
        with self.assertRaises(ValueError):
            self.labels("a   b", 2, [[1, 4]])

    def test_overlength_and_empty_inputs_are_refused(self):
        tok = full_a.hf_tokenize(FakeTokenizer())
        with self.assertRaises(ValueError):
            tok("   ")
        with self.assertRaises(ValueError):
            tok("w " * 2047)
        self.assertEqual(len(tok("w " * 2045 + "w")[0]), 2048)


class WordTokenShare(unittest.TestCase):
    def test_word_mask_excludes_whitespace_only_tokens(self):
        text = "Alpha beta.\n\n"
        ids, offsets = full_a.hf_tokenize(FakeTokenizer())(text)
        # CLS, "Alpha", " beta.", "\n\n" (whitespace-only), SEP
        self.assertEqual(full_a.word_mask_for(text, ids, offsets, {PAD, CLS, SEP}),
                         [False, True, True, False, False])
        labels = full_a.token_labels(text, ids, offsets, {PAD, CLS, SEP}, 0, [])
        # The word mask is exactly the tokens that can carry a label.
        self.assertEqual([v >= 0 for v in labels], full_a.word_mask_for(text, ids, offsets, {PAD, CLS, SEP}))
        # U+001C..U+001F count as whitespace, as in Python str.isspace and the Rust mirror.
        self.assertTrue(full_a.is_space("\x1c") and full_a.is_space("\u3000") and not full_a.is_space("\u200b"))

    def test_word_token_share_mean_ignores_whitespace_tokens_and_pooling_is_unchanged(self):
        texts = ["The quiet garden.\n\n", "Rain fell on the bridge.   "]
        tok = full_a.hf_tokenize(FakeTokenizer())
        encoded = [tok(t) for t in texts]
        n = max(len(i) for i, _ in encoded)
        ids = torch.full((2, n), PAD)
        mask = torch.zeros((2, n), dtype=torch.long)
        content = torch.zeros((2, n), dtype=torch.bool)
        words = torch.zeros((2, n), dtype=torch.bool)
        for j, (t, (i, o)) in enumerate(zip(texts, encoded)):
            ids[j, :len(i)] = torch.tensor(i)
            mask[j, :len(i)] = 1
            content[j, :len(i)] = torch.tensor(full_a.content_mask_for(i, {PAD, CLS, SEP}))
            words[j, :len(i)] = torch.tensor(full_a.word_mask_for(t, i, o, {PAD, CLS, SEP}))
        self.assertTrue(bool((content & ~words).any()))
        v1 = make_model("attention", "token_mean_projected").eval()
        v2 = make_model("attention", "word_token_mean_projected").eval()
        v2.load_state_dict(v1.state_dict())
        with torch.no_grad():
            v1.span.gate.fill_(.4)
            v2.span.gate.fill_(.4)
            a = v1(ids, mask, content, words=words)
            b = v2(ids, mask, content, words=words)
            with self.assertRaises(ValueError):
                v2(ids, mask, content)
        torch.testing.assert_close(a["involvement_logit"], b["involvement_logit"], atol=0, rtol=0)
        p = a["token_logits"].sigmoid()
        for j in range(2):
            torch.testing.assert_close(a["token_mean"][j], p[j][content[j]].mean())
            torch.testing.assert_close(b["token_mean"][j], p[j][words[j]].mean())
        self.assertGreater(float((a["token_mean"] - b["token_mean"]).abs().max()), 0.)


class Composition(unittest.TestCase):
    def test_gate_zero_is_exactly_log_softmax_and_ratio_preserved(self):
        torch.manual_seed(5)
        logits = torch.randn(7, 3) * 4
        lp, g = full_a.combine(logits)
        torch.testing.assert_close(lp, logits.log_softmax(-1))
        torch.testing.assert_close(g.sigmoid(), logits.softmax(-1)[:, 1:].sum(-1))
        pooled = torch.randn(7) * 3
        lo, g0 = full_a.combine(logits, torch.tensor(0.), pooled)
        torch.testing.assert_close(lo, lp, atol=1e-6, rtol=0)
        lo, g1 = full_a.combine(logits, torch.tensor(.7), pooled)
        p = lo.exp()
        torch.testing.assert_close(p.sum(-1), torch.ones(7))
        torch.testing.assert_close(g1, g + .7 * pooled)
        q = logits.softmax(-1)
        torch.testing.assert_close(p[:, 1] / p[:, 2], q[:, 1] / q[:, 2])

    def test_projected_share_bounds(self):
        origin = torch.tensor([[.5, .2, .3], [.1, .0, .9], [.9, .05, .05]])
        mean = torch.tensor([.05, .5, .99])
        s = full_a.projected_share(origin, mean)
        torch.testing.assert_close(s, torch.tensor([.2, .5, .1]))


class FunctionPreservation(unittest.TestCase):
    def test_initial_full_a_reproduces_v6_classifier_for_both_arms(self):
        ids, mask, content = batch_of(TEXTS[:1])
        for span in (None, "attention", "max"):
            # Configured for bf16 training; the explicit FP32 path must still preserve V6.
            model = make_model(span, "head" if span is None else "token_mean_projected", dtype=torch.bfloat16).eval()
            with torch.no_grad():
                out = model(ids, mask, content, autocast=False)
                approximate = model(ids, mask, content)
                expected = (model.encoder.classifier(out["h"]) / BASE_TEMPERATURE).softmax(-1)
                reference = model.encoder(input_ids=ids, attention_mask=mask).logits
            self.assertLess(float((out["origin"] - expected).abs().max()), 1e-6)
            self.assertGreater(float((approximate["h"] - out["h"]).abs().max()), 0.)
            torch.testing.assert_close(model.encoder.classifier(out["h"]), reference, atol=1e-5, rtol=0)
            p = out["origin"]
            self.assertTrue(bool(((p[:, 1] <= out["share"] + 1e-7) & (out["share"] <= p[:, 1] + p[:, 2] + 1e-7)).all()))

    def test_padded_batch_matches_single_documents(self):
        model = make_model("attention", "token_mean_projected").eval()
        ids, mask, content = batch_of(TEXTS)
        with torch.no_grad():
            joint = model(ids, mask, content)
            for i, text in enumerate(TEXTS):
                a, b, c = batch_of([text])
                single = model(a, b, c)
                n = a.shape[1]
                for key in ("origin", "share", "involvement_logit", "token_mean", "pooled"):
                    torch.testing.assert_close(joint[key][i], single[key][0], atol=2e-5, rtol=1e-5)
                torch.testing.assert_close(joint["token_logits"][i, :n][c[0]], single["token_logits"][0][c[0]],
                                           atol=2e-5, rtol=1e-5)


def synthetic_population(d, split="train"):
    rows = synthetic.factorial_rows(groups=2, splits=(split,), held_out_dev=False)[:7]
    synthetic.write_bundle(d, rows)
    labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
    return bundle.Population(d, split, full_a.hf_tokenize(FakeTokenizer()), {PAD, CLS, SEP}, labels,
                             cohort_weighted=True)


class Objective(unittest.TestCase):
    def test_microbatch_split_and_padding_preserve_loss_and_gradient(self):
        with tempfile.TemporaryDirectory() as d:
            population = synthetic_population(Path(d))
        n = len(population)
        self.assertGreater(population.denominators["token"], 0)
        model = make_model("attention", "token_mean_projected")
        with torch.no_grad():
            model.span.gate.fill_(.5)
        def run(groups):
            model.zero_grad(set_to_none=True)
            total = 0.
            for g in groups:
                ids, mask, content, labels, _ = population.batch(g, "cpu", PAD)
                loss = full_a.objective(model(ids, mask, content), [population.targets(i) for i in g],
                                        [population.weights[i] for i in g], population.denominators, labels, 1.)
                loss.backward()
                total += float(loss.detach())
            return total, {n: p.grad.clone() for n, p in model.named_parameters() if p.grad is not None}
        whole, g1 = run([list(range(n))])
        split, g2 = run([[0, 3], [1, 5], [2, 4, 6]])
        self.assertAlmostEqual(whole, split, places=5)
        self.assertEqual(set(g1), set(g2))
        for k in g1:
            torch.testing.assert_close(g1[k], g2[k], atol=1e-5, rtol=1e-4)
        self.assertNotIn("encoder.classifier.weight", g1)
        self.assertTrue(float(g1["span.gate"].abs()) > 0)

    def test_token_loss_only_where_labelled(self):
        out = {"encoding": torch.zeros(1, 2), "origin": torch.tensor([[.5, .25, .25]]),
               "log_origin": torch.tensor([[.5, .25, .25]]).log(), "share": torch.tensor([.3]),
               "human_logits": torch.zeros(1, 2), "ai_logits": torch.zeros(1, 2),
               "token_logits": torch.tensor([[9., 0., -9., 100.]])}
        labels = torch.tensor([[-1, 1, 0, -1]])
        loss = full_a.objective(out, [{"origin": None}], [1.], {"token": 2.}, labels, 3.)
        expected = 3 * .5 * float(torch.nn.functional.binary_cross_entropy_with_logits(
            torch.tensor([0., -9.]), torch.tensor([1., 0.])))
        self.assertAlmostEqual(float(loss), expected, places=6)


class Optimizer(unittest.TestCase):
    def test_layerwise_groups_cover_each_trainable_tensor_once(self):
        model = make_model("max", "token_mean_projected")
        groups = full_a.parameter_groups(model, 2e-5, .8, 3e-4, .01)
        seen = [id(p) for g in groups for p in g["params"]]
        trainable = [id(p) for p in model.parameters() if p.requires_grad]
        self.assertEqual(sorted(seen), sorted(trainable))
        self.assertNotIn(id(model.encoder.classifier.weight), seen)
        lr = {g["name"]: g["lr"] for g in groups}
        L = 2
        self.assertAlmostEqual(lr["encoder.depth0.wd0.01"], 2e-5 * .8 ** (L + 1))
        self.assertAlmostEqual(lr["encoder.depth1.wd0.01"], 2e-5 * .8 ** L)
        self.assertAlmostEqual(lr[f"encoder.depth{L + 1}.wd0.0"], 2e-5)
        gate = next(g for g in groups if any(p is model.span.gate for p in g["params"]))
        self.assertEqual(gate["weight_decay"], 0.)
        norms = next(g for g in groups if any(p is model.encoder.model.final_norm.weight for p in g["params"]))
        self.assertEqual(norms["weight_decay"], 0.)

    def test_warmup_then_linear_decay(self):
        f = full_a.warmup_linear(10, 2)
        self.assertEqual([round(f(s), 6) for s in range(11)],
                         [.5, 1., 1., .875, .75, .625, .5, .375, .25, .125, 0.])
        self.assertEqual(full_a.warmup_linear(4, 0)(0), 1.)
        with self.assertRaises(ValueError):
            full_a.warmup_linear(4, 4)

    def test_microbatches_respect_token_budget(self):
        lengths = {0: 900, 1: 2048, 2: 100, 3: 1000, 4: 1000, 5: 50}
        groups = trainer.microbatches(list(lengths), lengths, 2048)
        self.assertEqual(sorted(i for g in groups for i in g), sorted(lengths))
        for g in groups:
            self.assertTrue(len(g) == 1 or max(lengths[i] for i in g) * len(g) <= 2048)

    def test_checkpointed_bf16_step_trains_encoder_not_v6_classifier(self):
        model = make_model("attention", "token_mean_projected", dtype=torch.bfloat16)
        model.encoder.gradient_checkpointing_enable(gradient_checkpointing_kwargs={"use_reentrant": False})
        model.train()
        before = model.encoder.classifier.weight.clone()
        ids, mask, content = batch_of(TEXTS)
        out = model(ids, mask, content)
        self.assertEqual(out["origin"].dtype, torch.float32)
        loss = out["share"].sum() + out["origin"][:, 1:].sum() + out["token_logits"].sum() * 1e-3
        loss.backward()
        for n, p in model.named_parameters():
            if p.requires_grad and not n.startswith("heads.human") \
                    and not n.startswith("heads.ai") and not n.startswith("heads.residual.0"):
                self.assertIsNotNone(p.grad, n)
                self.assertTrue(bool(torch.isfinite(p.grad).all()), n)
        self.assertIsNone(model.encoder.classifier.weight.grad)
        torch.testing.assert_close(model.encoder.classifier.weight, before)


class Calibration(unittest.TestCase):
    def test_replay_methods_and_bounds(self):
        g = 0.8
        p = 1 / (1 + math.exp(-g))
        self.assertAlmostEqual(full_a.apply_calibration({"method": "identity_v1"}, g), p)
        self.assertAlmostEqual(full_a.apply_calibration({"method": "involvement_temperature_v1", "temperature": 2.}, g),
                               1 / (1 + math.exp(-g / 2)))
        iso = {"method": "isotonic_v1", "knots": [{"score": .2, "involvement": .1}, {"score": .6, "involvement": .3},
                                                  {"score": .9, "involvement": .95}]}
        self.assertAlmostEqual(full_a.apply_calibration(iso, -50.), .1)
        self.assertAlmostEqual(full_a.apply_calibration(iso, 50.), .95)
        x = .4
        self.assertAlmostEqual(full_a.apply_calibration(iso, math.log(x / (1 - x))), .2)
        bad = copy.deepcopy(iso)
        bad["knots"][1]["involvement"] = .05
        with self.assertRaises(ValueError):
            full_a.apply_calibration(bad, 0.)

    def test_bounded_isotonic_replay_never_returns_zero_or_one(self):
        e = 1 / 82
        iso = {"method": "isotonic_bounded_v2", "bound": full_a.BOUNDED_RULE, "epsilon": e,
               "knots": [{"score": .2, "involvement": e}, {"score": .6, "involvement": .3},
                         {"score": .9, "involvement": 1 - e}]}
        for g in (-1e6, -50., -2., 0., 2., 50., 1e6):
            p = full_a.apply_calibration(iso, g)
            self.assertTrue(e <= p <= 1 - e and p not in (0., 1.))
        self.assertEqual(full_a.apply_calibration(iso, -50.), e)
        self.assertEqual(full_a.apply_calibration(iso, 50.), 1 - e)
        for key, value in (("epsilon", 0.), ("bound", "other")):
            with self.assertRaises(ValueError):
                full_a.apply_calibration(dict(iso, **{key: value}), 0.)
        bad = copy.deepcopy(iso)
        bad["knots"][0]["involvement"] = 0.
        with self.assertRaises(ValueError):
            full_a.apply_calibration(bad, 0.)

    def test_identity_outputs_equal_model_and_calibrated_share_stays_bounded(self):
        for span, mode in ((None, "head"), ("attention", "token_mean_projected")):
            model = make_model(span, mode).eval()
            if span:
                with torch.no_grad():
                    model.span.gate.fill_(1.3)
            ids, mask, content = batch_of(TEXTS[1:2])
            with torch.no_grad():
                out = model(ids, mask, content)
            raw = trainer.raw_prediction(SimpleNamespace(id="x", text_sha256="0"), out, ids.shape[1])
            result = full_a.calibrated_outputs(raw, {"method": "identity_v1"}, mode)
            for k, v in zip(("human_only", "model_only", "mixed"), out["origin"][0].tolist()):
                self.assertAlmostEqual(result["origin"][k], v, places=5)
            self.assertAlmostEqual(result["ai_share_percent"], 100 * float(out["share"][0]), places=3)
            hot = full_a.calibrated_outputs(raw, {"method": "involvement_temperature_v1", "temperature": .1}, mode)
            o = hot["origin"]
            self.assertLessEqual(100 * o["model_only"] - 1e-9, hot["ai_share_percent"])
            self.assertLessEqual(hot["ai_share_percent"], hot["ai_confidence_percent"] + 1e-9)

    def test_protocol_origin_temperature_and_clipped_isotonic_replay(self):
        logits, g = [0.3, 1.1, -0.4], None
        base = math.log(math.exp(1.1) + math.exp(-0.4)) - 0.3
        # T = 1 reproduces the identity involvement and the head's model-only ratio.
        p, r = full_a.apply_origin_calibration({"method": "origin_temperature_v1", "temperature": 1.}, logits, base)
        self.assertAlmostEqual(p, 1 / (1 + math.exp(-base)), places=12)
        self.assertAlmostEqual(r, math.exp(1.1) / (math.exp(1.1) + math.exp(-0.4)), places=12)
        t = 2.
        p, r = full_a.apply_origin_calibration({"method": "origin_temperature_v1", "temperature": t}, logits, base)
        s = [v / t for v in logits]
        z = sum(math.exp(v) for v in s)
        self.assertAlmostEqual(p, (math.exp(s[1]) + math.exp(s[2])) / z, places=12)
        self.assertAlmostEqual(r, math.exp(s[1]) / (math.exp(s[1]) + math.exp(s[2])), places=12)
        # A span-gate offset on g is scaled by 1/T.
        p2, _ = full_a.apply_origin_calibration({"method": "origin_temperature_v1", "temperature": t}, logits, base + 1.)
        gt = math.log(p / (1 - p)) + .5
        self.assertAlmostEqual(p2, 1 / (1 + math.exp(-gt)), places=12)
        iso = {"method": "isotonic_v1_clipped", "temperature": 1., "bound": full_a.CLIPPED_RULE,
               "lo": .05, "hi": .9, "knots": [{"score": .2, "involvement": .05}, {"score": .8, "involvement": .9}]}
        self.assertEqual(full_a.apply_origin_calibration(iso, [0., -50., -50.], -60.)[0], .05)
        self.assertEqual(full_a.apply_origin_calibration(iso, [0., 50., 50.], 60.)[0], .9)
        mid = full_a.apply_origin_calibration(iso, [0., 0., -1e9], 0.)[0]
        self.assertAlmostEqual(mid, .05 + .85 * (.5 - .2) / .6, places=12)
        for bad in (dict(iso, lo=.0), dict(iso, hi=1.), dict(iso, bound="x"), dict(iso, temperature=0.),
                    dict(iso, knots=[{"score": .2, "involvement": .01}])):
            with self.assertRaises(ValueError):
                full_a.apply_origin_calibration(bad, logits, base)
        with self.assertRaises(ValueError):
            full_a.apply_calibration(iso, 0.)

    def test_protocol_maps_keep_share_bounds_in_calibrated_outputs(self):
        model = make_model(None, "head").eval()
        ids, mask, content = batch_of(TEXTS[1:2])
        with torch.no_grad():
            out = model(ids, mask, content)
        raw = trainer.raw_prediction(SimpleNamespace(id="x", text_sha256="0"), out, ids.shape[1])
        one = full_a.calibrated_outputs(raw, {"method": "origin_temperature_v1", "temperature": 1.}, "head")
        ident = full_a.calibrated_outputs(raw, {"method": "identity_v1"}, "head")
        for k in ("human_only", "model_only", "mixed"):
            self.assertAlmostEqual(one["origin"][k], ident["origin"][k], places=9)
        hot = full_a.calibrated_outputs(raw, {"method": "origin_temperature_v1", "temperature": 3.}, "head")
        o = hot["origin"]
        self.assertAlmostEqual(sum(o.values()), 1., places=12)
        self.assertLessEqual(100 * o["model_only"] - 1e-9, hot["ai_share_percent"])
        self.assertLessEqual(hot["ai_share_percent"], hot["ai_confidence_percent"] + 1e-9)


protocol_template = synthetic.protocol_template


class Protocol(unittest.TestCase):
    def test_selection_split_is_dev_iter_only(self):
        p = protocol_template()
        trainer.validate_protocol(p)
        for bad in ("cal", "dev_select", "test", "development"):
            p["early_stopping"]["split"] = bad
            with self.assertRaises(ValueError):
                trainer.validate_protocol(p)
        p = protocol_template()
        p["early_stopping"]["extra"] = 1
        with self.assertRaises(ValueError):
            trainer.validate_protocol(p)

    def test_gate_indices_cover_extremes_and_quantiles(self):
        import numpy as np
        population = type("P", (), {"lengths": np.array([9, 3, 40, 7, 12, 2048, 5, 30, 11, 6, 8, 25])})()
        chosen = trainer.gate_indices(population)
        self.assertEqual(chosen[:4], [0, 1, 2, 3])
        self.assertIn(5, chosen)  # longest
        self.assertIn(1, chosen)  # shortest
        self.assertEqual(len(chosen), len(set(chosen)))
        self.assertGreaterEqual(len({int(population.lengths[i]) for i in chosen}), 8)

    def test_template_and_rejections(self):
        trainer.validate_protocol(protocol_template())
        for path, value in ((("status",), "draft"), (("max_tokens",), 4096), (("final_test_opened",), True),
                            (("optimizer", "layer_lr_decay"), 1.5), (("schedule", "warmup_fraction_of_total_steps"), .9),
                            (("optimizer", "aux_gradient_clip_norm"), 0),
                            (("arms", "E1", "share_supervision", "conditional_loss_weight"), -1),
                            (("arms", "E1share", "share_supervision", "bins", "loss_weight"), 0),
                            (("arms", "E1", "share_supervision"), None),
                            (("schedule", "shape"), "step"), (("schedule", "horizon_epochs"), 0),
                            (("arms", "E1", "share_mode"), "token_mean_projected"),
                            (("arms", "E2w", "span_head", "pooling"), "mean"),
                            (("arms", "E1con", "batching"), {"kind": "random"}),
                            (("arms", "E1adv", "style_adversary", "reversal", "kind"), "step"),
                            (("arms", "E1adv", "style_adversary", "loss_weight"), 0),
                            (("attribution",), {}),
                            (("train_weighting",), {"scheme": "class_balanced", "positive_mass": 0}),
                            (("precision",), {"train": "fp16", "evaluate": "fp32"}),
                            (("ai_roster",), {"rule": "train_labels", "profiles_sha256": "0" * 64}),
                            (("ai_roster",), {"rule": trainer.FIXED_ROSTER, "profiles_sha256": "0"}),
                            (("attention_implementation",), "sdpa")):
            p = protocol_template()
            target = p
            for key in path[:-1]:
                target = target[key]
            target[path[-1]] = value
            with self.assertRaises((ValueError, KeyError)):
                trainer.validate_protocol(p)


def write_bundle(root, config, splits=("train", "dev_iter")):
    rows = synthetic.factorial_rows(groups=2, splits=splits)
    digests = synthetic.write_bundle(root, rows)
    labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
    synthetic.write_config(config, digests[bundle.MANIFEST], labels)
    (config / "init.json").write_text("{}")
    return rows


class Export(unittest.TestCase):
    def test_calibration_lineage_and_heads_artifact(self):
        value = {"schema": export_full_a.CALIBRATION_SCHEMA, "complete": True, "fit_split": "cal",
                 "final_test_opened": False, "arm": "E2", "seed": 17, "selected_checkpoint_sha256": "a" * 64,
                 "contract_sha256": "b" * 64,
                 "involvement": {"selected": {"method": "involvement_temperature_v1", "temperature": 1.2}},
                 "roles": {r: {"qualified": True, "threshold": .7} for r in ("human", "ai")}, "spans": None}
        binding = {"arm": "E2", "seed": 17}
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "calibration.json"
            path.write_text(json.dumps(value))
            self.assertTrue(export_full_a.calibration(path, binding, "a" * 64, "b" * 64)[2])
            for key, bad in (("selected_checkpoint_sha256", "c" * 64), ("fit_split", "test"), ("arm", "E1")):
                wrong = dict(value, **{key: bad})
                path.write_text(json.dumps(wrong))
                with self.assertRaises(ValueError):
                    export_full_a.calibration(path, binding, "a" * 64, "b" * 64)
            self.assertEqual(export_full_a.calibration(None, binding, "a", "b")[0], {"method": "identity_v1"})
            model = make_model("attention", "token_mean_projected")
            with torch.no_grad():
                model.span.gate.fill_(.25)
            init = Path(d) / "init.json"
            init.write_text("{}")
            contract = {"human_profiles": [], "ai_profiles": ["gen-a", "gen-b"]}
            arm = protocol_template()["arms"]["E2w"]
            art = export_full_a.heads_artifact(model, arm, contract, {"contract_sha256": "b" * 64}, init,
                                               {"method": "identity_v1"}, {"human": 1., "ai": 1.}, {"origin": 1})
            self.assertEqual(art["schema"], full_a.HEADS_SCHEMA)
            self.assertEqual(art["span_head"], {"pooling": "attention", "gate": .25})
            self.assertNotIn("span.gate", art["weights"])
            self.assertEqual(art["weights"]["span.token.weight"]["shape"], [1, 768])
            self.assertEqual(art["weights"]["residual.0.weight"]["shape"], [64, 2588])
            self.assertEqual(len(art["text_mean"]), 768)


class EndToEnd(unittest.TestCase):
    """Tiny CPU fits through the real trainer, with an interrupted resume."""

    def fit(self, root, config, out, arm, interrupt_after=None, report=None, consume="same", use_record=None,
            subset=None):
        encoder = tiny_encoder(11)
        init = init_for(encoder)
        calls = {"n": 0}
        real_save = trainer.atomic_save
        def save(path, value):
            real_save(path, value)
            if path.name == "recovery.pt":
                calls["n"] += 1
                if interrupt_after is not None and calls["n"] == interrupt_after:
                    raise KeyboardInterrupt("synthetic interruption")
        args = type("Args", (), {"data": str(root), "config": str(config), "artifact": "unused",
                                 "init": str(config / "init.json"), "out": str(out), "arm": arm, "seed": 17,
                                 "device": "cpu", "report_split": report, "train_subset_rows": subset,
                                 "consume_sealed_split": report if consume == "same" else consume,
                                 "use_record": use_record if use_record is not None or report is None
                                 else str(Path(out).parent / f"use-{Path(out).name}.json")})
        with mock.patch.object(trainer, "load", lambda a, p, d: (encoder, FakeTokenizer(), {"artifact_id": "synthetic-parent"})), \
                mock.patch.object(trainer, "initializer", lambda path: init), \
                mock.patch.object(trainer, "atomic_save", save):
            trainer.run(args)

    def test_fit_resume_is_identical_to_uninterrupted(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "bundle", Path(d) / "config"
            write_bundle(root, config)
            for arm in ("E1", "E2w", "E2wall"):
                whole, broken = Path(d) / f"whole-{arm}", Path(d) / f"broken-{arm}"
                self.fit(root, config, whole, arm)
                with self.assertRaises(KeyboardInterrupt):
                    self.fit(root, config, broken, arm, interrupt_after=2)
                self.assertFalse((broken / "fit.json").exists())
                self.fit(root, config, broken, arm)
                a, b = (json.loads((p / "fit.json").read_text()) for p in (whole, broken))
                self.assertEqual(a["selected_evaluation"], b["selected_evaluation"])
                self.assertEqual(a["selection_split_involvement_brier"], b["selection_split_involvement_brier"])
                self.assertEqual((whole / "encoder.safetensors").read_bytes(), (broken / "encoder.safetensors").read_bytes())
                self.assertEqual((whole / "heads.safetensors").read_bytes(), (broken / "heads.safetensors").read_bytes())
                self.assertEqual(a["evaluations"], 5)
                self.assertEqual(a["dev_iter_rows_sha256"], b["dev_iter_rows_sha256"])
                self.assertEqual(a["binding"]["dev_iter_rows_sha256"], a["dev_iter_rows_sha256"])
                self.assertEqual(a["schedule"]["warmup_basis"], "fraction_of_max_epochs_x_steps_per_epoch")
                self.assertLess(a["initial_gate"]["initial_origin_max_abs_error"], 1e-6)
                history = json.loads((whole / "history.json").read_text())
                self.assertEqual([h["evaluation"] for h in history], list(range(5)))
                self.assertTrue(all(math.isfinite(h["dev_iter_involvement_brier"]) for h in history))
                self.assertEqual(a["selection_split"], "dev_iter")
                self.assertIsNone(a["report"])
                log = json.loads((whole / "cohort-weights.json").read_text())
                self.assertTrue(log["complete"])
                frontier = log["cohorts"]["hosted_frontier_v1"]["labels"]["claude-x"]
                self.assertEqual((frontier["records"], frontier["weight"], frontier["trained_records"]), (6, 1.5, 6))
                self.assertIn("human", log["cohorts"]["humans_v1"]["labels"])
                self.assertAlmostEqual(sum(log["effective_mass_by_origin"].values()), 1.)
                self.assertEqual(a["cohort_weight_log"]["sha256"], hashlib.sha256((whole / "cohort-weights.json").read_bytes()).hexdigest())
                preds = [json.loads(x) for x in (whole / "dev_iter-predictions.jsonl").read_text().splitlines()]
                self.assertEqual(len(preds), 2 + 2 * 9)
                for p in preds:
                    self.assertAlmostEqual(p["p_human"] + p["p_model_only"] + p["p_mixed"], 1., places=6)
                    self.assertLessEqual(p["p_model_only"] - 1e-9, p["share"])
                    self.assertLessEqual(p["share"], p["p_model_only"] + p["p_mixed"] + 1e-9)
                    self.assertEqual(sorted(p["attribution"]), ["claude-x", "gen-a", "gen-b"])
                    self.assertAlmostEqual(sum(p["attribution"].values()), 1., places=6)
                diag = history[-1]["dev_iter_diagnostics"]["attribution_accuracy"]
                self.assertEqual(diag["__all__"]["rows"], 18)
                self.assertEqual(diag["__held_out_style__"]["rows"], 6)
                if arm == "E2wall":
                    self.assertIsNotNone(a["aux_heads"]["style_adversary"])
                    self.assertGreater(a["aux_heads"]["batch_structure"]["anchor_rows"], 0)
                    terms = [h["train_terms_mean_since_last_evaluation"] for h in history[1:]]
                    self.assertTrue(all(t["contrastive"] > 0 and t["adversary"] > 0 and t["share_bins"] > 0
                                        for t in terms))
                    self.assertTrue(all(t["grad_norm_main"] > 0 and t["grad_norm_aux"] > 0 for t in terms))
                    self.assertEqual(a["aux_heads"]["share_bins"], list(aux.SHARE_BIN_EDGES))
                    self.assertGreater(history[-1]["reversal_scale"], 0)
                    # Training-only heads never reach the exported heads.
                    from safetensors.torch import load_file
                    self.assertFalse(any(k.startswith(("adversary", "projection", "share_bins", "net"))
                                         for k in load_file(str(whole / "heads.safetensors"))))
                if arm != "E1":
                    self.assertGreater(a["train_population"]["labelled_ai_tokens"], 0)

    def test_fixed_roster_keeps_unsupervised_profiles(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "bundle", Path(d) / "config"
            rows = write_bundle(root, config)
            labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
            roster = sorted(labels + ["gen-unseen"])
            manifest = bundle.bundle_digests(root)[bundle.MANIFEST]
            protocol = protocol_template()
            synthetic.write_config(config, manifest, roster, protocol)
            with self.assertRaisesRegex(ValueError, "AI roster differs"):
                self.fit(root, config, Path(d) / "derived", "E1")
            protocol["ai_roster"] = {"rule": trainer.FIXED_ROSTER, "profiles_sha256": trainer.roster_sha256(labels)}
            synthetic.write_config(config, manifest, roster, protocol)
            with self.assertRaisesRegex(ValueError, "fixed roster"):
                self.fit(root, config, Path(d) / "wrong", "E1")
            protocol["ai_roster"]["profiles_sha256"] = trainer.roster_sha256(roster)
            synthetic.write_config(config, manifest, roster, protocol)
            self.fit(root, config, Path(d) / "fixed", "E1")
            report = json.loads((Path(d) / "fixed" / "fit.json").read_text())
            self.assertEqual(report["ai_roster"], {"rule": trainer.FIXED_ROSTER,
                                                   "profiles_sha256": trainer.roster_sha256(roster),
                                                   "supervised": labels, "unsupervised": ["gen-unseen"]})
            preds = [json.loads(x) for x in (Path(d) / "fixed" / "dev_iter-predictions.jsonl").read_text().splitlines()]
            self.assertTrue(all(sorted(p["attribution"]) == roster for p in preds))
            # Train labels must stay inside the fixed roster.
            narrow = [x for x in roster if x != "gen-a"]
            protocol["ai_roster"]["profiles_sha256"] = trainer.roster_sha256(narrow)
            synthetic.write_config(config, manifest, narrow, protocol)
            with self.assertRaisesRegex(ValueError, "outside the fixed roster"):
                self.fit(root, config, Path(d) / "narrow", "E1")

    def test_class_balanced_fit_logs_declared_class_mass(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "bundle", Path(d) / "config"
            rows = write_bundle(root, config)
            protocol = protocol_template()
            protocol["train_weighting"] = {"scheme": "class_balanced", "positive_mass": .5}
            labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
            synthetic.write_config(config, bundle.bundle_digests(root)[bundle.MANIFEST], labels, protocol)
            self.fit(root, config, Path(d) / "cb", "E1share")
            log = json.loads((Path(d) / "cb" / "cohort-weights.json").read_text())
            self.assertEqual(log["weighting"]["scheme"], "class_balanced")
            self.assertAlmostEqual(log["effective_mass_by_origin"]["human_only"], .5)
            self.assertAlmostEqual(log["effective_mass_by_origin"]["model_only"] +
                                   log["effective_mass_by_origin"]["mixed"], .5)

    def test_assembler_row_weight_fit_uses_bundle_row_weights(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "bundle", Path(d) / "config"
            rows = synthetic.factorial_rows(groups=2)
            for i, r in enumerate(rows):
                if r["split"] == "train":
                    r["cohort_weight"] = 0.5 + (i % 5)
            synthetic.write_bundle(root, rows, synthetic.row_weights_doc(rows))
            (config / "init.json").parent.mkdir(parents=True, exist_ok=True)
            (config / "init.json").write_text("{}")
            protocol = protocol_template()
            protocol["train_weighting"] = {"scheme": bundle.ROW_WEIGHT_SCHEME, "positive_mass": None}
            labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
            synthetic.write_config(config, bundle.bundle_digests(root)[bundle.MANIFEST], labels, protocol)
            self.fit(root, config, Path(d) / "rw", "E1share")
            log = json.loads((Path(d) / "rw" / "cohort-weights.json").read_text())
            self.assertEqual(log["base_weighting"], bundle.ROW_WEIGHT_SCHEME)
            train = [r for r in rows if r["split"] == "train"]
            total = sum(r["cohort_weight"] for r in train)
            human = sum(r["cohort_weight"] for r in train if r["origin"] == "human_only") / total
            self.assertAlmostEqual(log["effective_mass_by_origin"]["human_only"], human)
            # The same bundle under a rebalancing scheme is refused (weights.json form mismatch).
            protocol["train_weighting"] = {"scheme": "family_origin_balanced", "positive_mass": None}
            synthetic.write_config(config, bundle.bundle_digests(root)[bundle.MANIFEST], labels, protocol)
            with self.assertRaisesRegex(ValueError, "does not match"):
                self.fit(root, config, Path(d) / "rw2", "E1share")

    def test_refuses_test_rows_held_out_train_styles_and_weight_mismatch(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "bundle", Path(d) / "config"
            rows = write_bundle(root, config)
            for mutate, message in (
                    (lambda rs: rs.append(dict(rs[0], id="t0", split="test")), "Test"),
                    (lambda rs: rs.__setitem__(1, dict(rs[1], held_out_style=True)), "held-out"),
                    (lambda rs: rs.__setitem__(1, dict(rs[1], cohort_weight=2.)), "cohort weight")):
                bad = copy.deepcopy(rows)
                mutate(bad)
                weights = synthetic.weights_doc(rows)  # the declared weights stay those of the clean rows
                if message == "Test":
                    weights = synthetic.weights_doc(rows)
                broot = Path(d) / ("bad-" + message)
                digests = synthetic.write_bundle(broot, bad, weights)
                bconf = Path(d) / ("conf-" + message)
                synthetic.write_config(bconf, digests[bundle.MANIFEST], ["claude-x", "gen-a", "gen-b"])
                (bconf / "init.json").write_text("{}")
                with self.assertRaisesRegex(ValueError, message):
                    self.fit(broot, bconf, Path(d) / ("out-" + message), "E1")

    def test_report_split_is_read_after_selection_and_never_changes_it(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "bundle", Path(d) / "config"
            write_bundle(root, config, splits=("train", "dev_iter", "dev_select"))
            plain, reported = Path(d) / "plain", Path(d) / "reported"
            seen = []
            real = bundle.Population
            def spy(root_, split, *rest, **kw):
                seen.append(split)
                return real(root_, split, *rest, **kw)
            with mock.patch.object(bundle, "Population", spy):
                self.fit(root, config, plain, "E2w")
                self.assertEqual(seen, ["train", "dev_iter"])
                seen.clear()
                self.fit(root, config, reported, "E2w", report="dev_select")
                self.assertEqual(seen, ["train", "dev_iter", "dev_select"])
            a, b = (json.loads((p / "fit.json").read_text()) for p in (plain, reported))
            self.assertTrue(b["selection_score_is_biased"])
            self.assertEqual(a["selected_evaluation"], b["selected_evaluation"])
            self.assertEqual((plain / "encoder.safetensors").read_bytes(), (reported / "encoder.safetensors").read_bytes())
            r = b["report"]
            self.assertEqual((r["split"], r["used_for_selection"], r["rows"]), ("dev_select", False, 20))
            self.assertTrue(Path(r["use_record"]).exists())
            with self.assertRaises(ValueError):
                self.fit(root, config, Path(d) / "same", "E1", report="dev_iter")
            # A sealed split needs the explicit consent flag and a new use record.
            with self.assertRaisesRegex(ValueError, "consume-sealed-split"):
                self.fit(root, config, Path(d) / "noflag", "E1", report="dev_select", consume=None)
            with self.assertRaisesRegex(ValueError, "consume-sealed-split"):
                self.fit(root, config, Path(d) / "reused", "E1", report="dev_select", use_record=r["use_record"])


def meta_row(i, origin, label=None, domain="wikipedia", group=None, cohort="c", weight=1.):
    m = bundle.Meta()
    m.id, m.origin, m.label, m.domain = f"r{i}", origin, label, domain
    m.source_group, m.cohort_id, m.cohort_weight = group or f"g{i}", cohort, weight
    return m


class TrainWeighting(unittest.TestCase):
    """label_domain_balanced_v1, the row cap and the diagnostic subset."""

    W = {"scheme": bundle.LABEL_DOMAIN_SCHEME, "positive_mass": .5, "positive_free_domain_human_share": None,
         "max_row_weight_ratio": None}

    def test_cap_keeps_total_and_order_and_bounds_every_row(self):
        w = [100., 1., 1., 1., 2., 3., .5, 40.]
        out = bundle.cap_weights(w, 2.)
        self.assertAlmostEqual(sum(out), sum(w))
        self.assertLessEqual(max(out), 2. * sum(w) / len(w) * (1 + 1e-9))
        # Weakly monotone: capped rows tie at the cap, no row overtakes a heavier one.
        self.assertTrue(all(out[i] <= out[j] + 1e-12 for i in range(len(w)) for j in range(len(w)) if w[i] < w[j]))
        self.assertAlmostEqual(out[0], out[7])
        self.assertEqual(bundle.cap_weights([1., 2., 3.], 10.), [1., 2., 3.])
        self.assertEqual(bundle.cap_weights([1., 2.], None), [1., 2.])

    def test_positives_balance_label_then_domain_then_origin_then_group(self):
        meta = [meta_row(0, 0, domain="wikipedia", weight=3.), meta_row(1, 0, domain="fineweb", weight=1.)]
        # gen-a: one starved row in gutenberg; gen-b: many rows in two domains, both origins.
        meta.append(meta_row(2, 1, "gen-a", "project_gutenberg", weight=1e-6))
        k = 3
        for d, n in (("wikipedia", 30), ("plos", 3)):
            for o in (1, 2):
                for j in range(n):
                    meta.append(meta_row(k, o, "gen-b", d, group=f"{d}-{j % 3}", weight=50.))
                    k += 1
        w = bundle.label_domain_balanced_weights(meta, self.W)
        self.assertAlmostEqual(sum(w), 1.)
        by = lambda f: sum(x for x, m in zip(w, meta) if f(m))
        self.assertAlmostEqual(by(lambda m: m.origin == 0), .5)
        self.assertAlmostEqual(by(lambda m: m.label == "gen-a"), .25)
        self.assertAlmostEqual(by(lambda m: m.label == "gen-b"), .25)
        self.assertAlmostEqual(by(lambda m: m.label == "gen-b" and m.domain == "plos"), .125)
        self.assertAlmostEqual(by(lambda m: m.label == "gen-b" and m.domain == "plos" and m.origin == 2), .0625)
        self.assertAlmostEqual(by(lambda m: m.source_group == "plos-0" and m.origin == 1), .0625 / 3)
        # Humans keep the assembler's relative weights when no share is declared.
        self.assertAlmostEqual(w[0] / w[1], 3.)

    def test_positive_free_domain_humans_get_the_declared_share(self):
        meta = [meta_row(0, 0, domain="wikipedia"), meta_row(1, 0, domain="fineweb", weight=9.),
                meta_row(2, 0, domain="global_voices", weight=9.), meta_row(3, 1, "gen-a", "wikipedia")]
        w = bundle.label_domain_balanced_weights(meta, dict(self.W, positive_free_domain_human_share=.2))
        self.assertAlmostEqual(w[1] + w[2], .5 * .2)
        self.assertAlmostEqual(w[0], .5 * .8)
        self.assertAlmostEqual(w[3], .5)

    def test_cap_applies_per_class(self):
        meta = [meta_row(i, 0, weight=1. if i else 1000.) for i in range(10)]
        meta += [meta_row(10, 1, "gen-a")] + [meta_row(11 + j, 2, "gen-b") for j in range(9)]
        w = bundle.label_domain_balanced_weights(meta, dict(self.W, max_row_weight_ratio=2.))
        humans, pos = w[:10], w[10:]
        self.assertAlmostEqual(sum(humans), .5)
        self.assertAlmostEqual(sum(pos), .5)
        self.assertLessEqual(max(humans), 2 * .5 / 10 * (1 + 1e-9))
        self.assertLessEqual(max(pos), 2 * .5 / 10 * (1 + 1e-9))

    def test_validation_of_the_new_scheme(self):
        self.assertEqual(bundle.validate_weighting(dict(self.W)), self.W)
        for bad in (dict(self.W, positive_mass=None), dict(self.W, positive_mass=1.),
                    dict(self.W, positive_free_domain_human_share=1.5), dict(self.W, max_row_weight_ratio=.5),
                    {"scheme": bundle.LABEL_DOMAIN_SCHEME, "positive_mass": .5}):
            with self.assertRaises(ValueError):
                bundle.validate_weighting(bad)
        row = {"scheme": bundle.ROW_WEIGHT_SCHEME, "positive_mass": None, "max_row_weight_ratio": 2.}
        self.assertEqual(bundle.validate_weighting(dict(row)), row)
        for bad in (dict(row, max_row_weight_ratio=.5), dict(row, max_row_weight_ratio=float("inf")),
                    dict(row, positive_free_domain_human_share=.2)):
            with self.assertRaises(ValueError):
                bundle.validate_weighting(bad)
        meta = [meta_row(0, 0)]
        with self.assertRaisesRegex(ValueError, "human and AI-involved"):
            bundle.label_domain_balanced_weights(meta, self.W)

    def test_assembler_row_weights_cap_per_origin_class(self):
        meta = [meta_row(i, 0, weight=1. if i else 1000.) for i in range(10)]
        meta += [meta_row(10 + j, 1, "gen-a", weight=50. if j == 0 else 1.) for j in range(5)]
        meta += [meta_row(15 + j, 2, "gen-b", weight=2.) for j in range(5)]
        plain = bundle.train_weights(meta, {"scheme": bundle.ROW_WEIGHT_SCHEME, "positive_mass": None})
        capped = bundle.train_weights(meta, {"scheme": bundle.ROW_WEIGHT_SCHEME, "positive_mass": None,
                                             "max_row_weight_ratio": 2.})
        self.assertAlmostEqual(sum(capped), 1.)
        for o, n in ((0, 10), (1, 5), (2, 5)):
            idx = [i for i, m in enumerate(meta) if m.origin == o]
            self.assertAlmostEqual(sum(capped[i] for i in idx), sum(plain[i] for i in idx))
            mean = sum(plain[i] for i in idx) / n
            self.assertLessEqual(max(capped[i] for i in idx), 2 * mean * (1 + 1e-9))
        self.assertEqual(capped[15:], plain[15:])
        self.assertEqual(bundle.train_weights(meta, {"scheme": bundle.ROW_WEIGHT_SCHEME, "positive_mass": None,
                                                     "max_row_weight_ratio": None}), plain)

    def test_global_cap_bounds_rows_by_the_all_row_mean_and_keeps_class_mass(self):
        meta = [meta_row(i, 0, weight=1. if i else 30.) for i in range(20)]
        meta += [meta_row(20 + j, 1, "gen-a", weight=20. if j == 0 else 2.) for j in range(5)]
        meta += [meta_row(25 + j, 2, "gen-b", weight=4.) for j in range(5)]
        base = {"scheme": bundle.ROW_WEIGHT_SCHEME, "positive_mass": None, "max_row_weight_ratio": 5.}
        plain = bundle.train_weights(meta, base)
        both = dict(base, max_row_weight_global_ratio=2.5)
        self.assertEqual(bundle.validate_weighting(dict(both)), both)
        w = bundle.train_weights(meta, both)
        self.assertAlmostEqual(sum(w), 1.)
        self.assertLessEqual(max(w), 2.5 / len(meta) * (1 + 1e-9))
        for o in (0, 1, 2):
            idx = [i for i, m in enumerate(meta) if m.origin == o]
            self.assertAlmostEqual(sum(w[i] for i in idx), sum(plain[i] for i in idx))
        self.assertEqual(bundle.train_weights(meta, dict(base, max_row_weight_global_ratio=None)), plain)
        # A class whose mean is above the cap cannot keep its mass: refused.
        with self.assertRaisesRegex(ValueError, "below a class mean"):
            bundle.train_weights(meta, dict(base, max_row_weight_global_ratio=1.))
        ld = dict(self.W, max_row_weight_ratio=5., max_row_weight_global_ratio=3.)
        self.assertEqual(bundle.validate_weighting(dict(ld)), ld)
        w = bundle.label_domain_balanced_weights(meta, ld)
        self.assertAlmostEqual(sum(w), 1.)
        self.assertAlmostEqual(sum(w[:20]), .5)
        self.assertLessEqual(max(w), 3. / len(meta) * (1 + 1e-9))
        for bad in (dict(base, max_row_weight_global_ratio=.5), dict(base, max_row_weight_global_ratio=float("nan")),
                    {"scheme": "class_balanced", "positive_mass": .5, "max_row_weight_global_ratio": 2.}):
            with self.assertRaises(ValueError):
                bundle.validate_weighting(bad)

    def test_cohort_positive_mass_cap_bounds_a_prefix_and_keeps_humans_and_row_caps(self):
        meta = [meta_row(i, 0, weight=1.) for i in range(20)]
        meta += [meta_row(20 + j, 1, "gen-a", cohort="local:a", weight=1.) for j in range(5)]
        meta += [meta_row(25 + j, 1, f"ext-{j % 3}", cohort="raid-v1-paired", weight=3.) for j in range(15)]
        meta += [meta_row(40 + j, 2, "gen-b", cohort="local:b", weight=2.) for j in range(5)]
        for base in ({"scheme": bundle.ROW_WEIGHT_SCHEME, "positive_mass": None, "max_row_weight_ratio": 5.,
                      "max_row_weight_global_ratio": 5.},
                     dict(self.W, max_row_weight_ratio=5., max_row_weight_global_ratio=5.)):
            plain = bundle.train_weights(meta, base)
            cfg = dict(base, cohort_positive_mass_caps={"raid-v1-": .2})
            self.assertEqual(bundle.validate_weighting(dict(cfg)), cfg)
            w = bundle.train_weights(meta, cfg)
            pos = [i for i, m in enumerate(meta) if m.origin != 0]
            share = lambda v: sum(v[i] for i in pos if meta[i].cohort_id.startswith("raid-v1-")) / sum(v[i] for i in pos)
            self.assertGreater(share(plain), .2)
            self.assertLessEqual(share(w), .2 * (1 + 1e-6))
            self.assertAlmostEqual(sum(w), 1.)
            self.assertAlmostEqual(sum(w[:20]), sum(plain[:20]))
            self.assertLessEqual(max(w), 5. / len(meta) * (1 + 1e-6))
            # A cap above the current share changes nothing.
            self.assertEqual(bundle.train_weights(meta, dict(base, cohort_positive_mass_caps={"raid-v1-": .99})), plain)
        for bad in ({"scheme": bundle.ROW_WEIGHT_SCHEME, "positive_mass": None, "cohort_positive_mass_caps": {"x": 1.}},
                    {"scheme": bundle.ROW_WEIGHT_SCHEME, "positive_mass": None, "cohort_positive_mass_caps": {"": .2}},
                    {"scheme": "family_origin_balanced", "positive_mass": None, "cohort_positive_mass_caps": {"x": .2}}):
            with self.assertRaises(ValueError):
                bundle.validate_weighting(bad)

    def test_stratified_subset_is_deterministic_covers_strata_and_keeps_mass(self):
        meta = [meta_row(i, 0, domain="wikipedia") for i in range(500)]
        meta += [meta_row(500 + i, 1, "gen-a", "plos") for i in range(40)]
        meta += [meta_row(540, 2, "gen-b", "fineweb")]
        a = bundle.stratified_subset(meta, 54, 17)
        self.assertEqual(a, bundle.stratified_subset(meta, 54, 17))
        self.assertNotEqual(set(a), set(bundle.stratified_subset(meta, 54, 29)))
        self.assertEqual(len(a), 54)
        self.assertIn("r540", a)
        self.assertEqual(a["r540"], 1.)
        humans = [x for x in a if int(x[1:]) < 500]
        self.assertAlmostEqual(sum(a[x] for x in humans), 500.)
        self.assertAlmostEqual(sum(a[x] for x in a if 500 <= int(x[1:]) < 540), 40.)
        with self.assertRaises(ValueError):
            bundle.stratified_subset(meta, 2, 17)


class TrainWeightingFit(unittest.TestCase):
    fit = EndToEnd.fit

    def test_label_domain_balanced_fit_on_a_subset_logs_mass_parts_and_tpr(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "bundle", Path(d) / "config"
            rows = synthetic.factorial_rows(groups=3)
            for i, r in enumerate(rows):
                if r["split"] == "train":
                    r["cohort_weight"] = 0.5 + (i % 5)
            synthetic.write_bundle(root, rows, synthetic.row_weights_doc(rows))
            config.mkdir(parents=True, exist_ok=True)
            (config / "init.json").write_text("{}")
            protocol = protocol_template()
            protocol["train_weighting"] = {"scheme": bundle.LABEL_DOMAIN_SCHEME, "positive_mass": .4,
                                           "positive_free_domain_human_share": None, "max_row_weight_ratio": 4.}
            labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
            synthetic.write_config(config, bundle.bundle_digests(root)[bundle.MANIFEST], labels, protocol)
            train_rows = sum(r["split"] == "train" for r in rows)
            out = Path(d) / "ldb"
            self.fit(root, config, out, "E1share", subset=train_rows - 6)
            fit = json.loads((out / "fit.json").read_text())
            self.assertEqual(fit["diagnostic_train_subset"]["rows"], train_rows - 6)
            self.assertEqual(fit["binding"]["diagnostic_train_subset"], fit["diagnostic_train_subset"])
            self.assertEqual(fit["train_population"]["documents"], train_rows - 6)
            log = json.loads((out / "cohort-weights.json").read_text())
            self.assertEqual(log["base_weighting"], bundle.LABEL_DOMAIN_SCHEME)
            self.assertAlmostEqual(log["effective_mass_by_origin"]["human_only"], .6)
            # The log still counts every Train row of weights.json, trained rows are the subset.
            counted = sum(e["records"] for c in log["cohorts"].values() for e in c["labels"].values())
            trained = sum(e["trained_records"] for c in log["cohorts"].values() for e in c["labels"].values())
            self.assertEqual((counted, trained), (train_rows, train_rows - 6))
            history = json.loads((out / "history.json").read_text())
            for h in history:
                diag = h["dev_iter_diagnostics"]
                self.assertEqual(diag["positive_rows"] + diag["human_rows"], 3 * (1 + 9))
                self.assertTrue(0 <= diag["tpr_at_0_5"] <= 1)
                self.assertEqual(set(diag["loss_parts"]), set(trainer.LOSS_PARTS))
                self.assertAlmostEqual(sum(diag["loss_parts"].values()), h["dev_iter_joint_loss"], places=6)
                self.assertAlmostEqual(sum(v["brier_contribution"] for v in diag["by_domain"].values()),
                                       h["dev_iter_involvement_brier"], places=9)
            for h in history[1:]:
                t = h["train_terms_mean_since_last_evaluation"]
                parts = sum(t["loss_" + k] for k in trainer.LOSS_PARTS)
                self.assertAlmostEqual(parts, t["joint"], places=5)
                self.assertGreater(t["loss_conditional_share"], 0)


if __name__ == "__main__":
    unittest.main()

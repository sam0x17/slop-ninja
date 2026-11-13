"""Offline spans-v1 checks: emission rules, golden vectors, the token head and
a tiny frozen-document (S0) fit through the real trainer.

No corpus, artifact, provider, detector or network access. CPU only.
"""
import json
import math
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import torch

import export_full_a
import full_a
import full_a_bundle as bundle
import full_a_spans as sp
import synthetic_full_a as synthetic
import test_full_a as base
import train_full_a as trainer

GOLDEN = Path(__file__).resolve().parents[2] / "grammar-a-baseline" / "tests" / "golden" / "span-golden-v1.json"
# Unicode White_Space (PropList.txt), the set Rust char::is_whitespace uses.
WHITE_SPACE = set(range(0x9, 0xE)) | {0x20, 0x85, 0xA0, 0x1680} | set(range(0x2000, 0x200B)) | \
    {0x2028, 0x2029, 0x202F, 0x205F, 0x3000}


class Whitespace(unittest.TestCase):
    def test_python_and_rust_whitespace_rules(self):
        for cp in range(0x110000):
            if 0xD800 <= cp <= 0xDFFF:
                continue
            c = chr(cp)
            self.assertEqual(sp.is_rust_whitespace(c), cp in WHITE_SPACE, hex(cp))
            self.assertEqual(sp.is_space(c), cp in WHITE_SPACE or 0x1C <= cp <= 0x1F, hex(cp))


class Segmentation(unittest.TestCase):
    def test_words_are_maximal_non_isspace_runs(self):
        self.assertEqual(sp.words("  ab c\x1fd\n"), [(2, 4), (5, 6), (7, 8)])
        self.assertEqual(sp.words("x"), [(0, 1)])
        self.assertEqual(sp.words(" \n\t"), [])

    def test_sealed_splitter_boundaries(self):
        text = "One. Two!\" (Three?) Four\n\nFive.Six\r\n \r\nSeven"
        got = [(text[a:b], p) for a, b, p in sp.sentences(text)]
        self.assertEqual(got, [("One.", True), ("Two!\"", True), ("(Three?)", True), ("Four", False),
                               ("Five.Six", False), ("Seven", False)])
        # U+001F is not Rust whitespace: no boundary after the stop, but a Python word break.
        self.assertEqual([(a, b) for a, b, _ in sp.sentences("A.\x1fB. C")], [(0, 5), (6, 7)])

    def test_every_word_has_one_sentence_and_tokens(self):
        text = "Alpha be.\x1f Gamma\n\nDelta"
        ws = sp.words(text)
        ranges = sp.sentence_word_ranges(sp.sentences(text), ws)
        self.assertEqual([(a, b) for _, a, b in ranges], [(0, 3), (3, 4)])
        offsets, mask = sp.golden_tokenize(text)
        groups = sp.word_groups(text, offsets, mask, ws)
        self.assertTrue(all(groups))
        # Multi-token words collect every overlapping word-bearing token.
        self.assertEqual(len(groups[0]), 2)
        # A word without a word-bearing token makes emission unsupported.
        with self.assertRaises(ValueError):
            sp.word_groups(text, offsets, [False] * len(mask), ws)

    def test_word_labels(self):
        text = "ab cd ef"
        self.assertEqual(sp.word_labels(text, sp.words(text), [[3, 5]]), [0, 1, 0])
        self.assertEqual(sp.word_labels(text, sp.words(text), [[4, 8]]), [0, -1, 1])


class Numerics(unittest.TestCase):
    def test_tilt_hits_the_share_and_keeps_order(self):
        p = [.01, .2, .5, .8, .999999]
        for s in (0., 1e-7, .1, .5, .93, 1.):
            b, target, q = sp.tilt(p, s)
            self.assertAlmostEqual(sum(q) / len(q), min(max(s, 1e-6), 1 - 1e-6), delta=1e-12)
            self.assertEqual(sorted(q), q)
        b, _, q = sp.tilt([1., 1., 1.], .2)
        self.assertTrue(all(abs(v - .2) < 1e-12 for v in q))

    def test_decoder_hysteresis_gap_min_and_snap(self):
        d = dict(sp.DIAGNOSTIC_DECODER)
        q = [.1, .6, .2, .7, .7, .1, .1, .1, .55]
        one = [(0, 0, 9)]
        self.assertEqual(sp.decode(q, one, d), [(1, 2), (3, 5), (8, 9)])
        self.assertEqual(sp.decode(q, one, dict(d, gap=1)), [(1, 5), (8, 9)])
        self.assertEqual(sp.decode(q, one, dict(d, gap=3)), [(1, 9)])
        self.assertEqual(sp.decode(q, one, dict(d, min_words=2)), [(3, 5)])
        # Hysteresis: tau_lo = .2 joins the .2 word; .55 alone never reaches tau_hi = .6.
        self.assertEqual(sp.decode(q, one, dict(d, tau_hi=.6, delta=.4)), [(1, 5)])
        d = dict(sp.DIAGNOSTIC_DECODER, snap=.8)
        q = [.9, .9, .9, .9, .1, .1, .9]
        self.assertEqual(sp.decode(q, [(0, 0, 5), (1, 5, 7)], d), [(0, 5), (6, 7)])
        self.assertEqual(sp.decode(q, [(0, 0, 6), (1, 6, 7)], d), [(0, 4), (6, 7)])

    def test_clipped_score_map(self):
        m = sp.GOLDEN_MAP
        self.assertEqual(sp.apply_score_map(m, 0.), .02)
        self.assertEqual(sp.apply_score_map(m, 1.), .95)
        self.assertAlmostEqual(sp.apply_score_map(m, .25), .16)
        self.assertEqual(sp.apply_score_map(m, .5), .3)
        with self.assertRaises(ValueError):
            sp.validate_score_map(dict(m, lo=.05))


class Emission(unittest.TestCase):
    def test_sentence_shares_average_to_the_document_share(self):
        for case in json.loads(GOLDEN.read_text())["cases"]:
            e = case["expected"]
            total = sum(s["ai_share_percent"] * s["words"] for s in e["sentences"]) / e["words"] / 100.
            self.assertAlmostEqual(total, e["share_target"], delta=1e-9)
            for s in e["sentences"] + e["spans"]:
                self.assertLessEqual(s["ai_confidence_percent"], 100 * case["confidence"] + 1e-9)
                text = case["text"]
                self.assertEqual(len(text[:s["start_char"]].encode()), s["start_byte"])
                self.assertEqual(len(text[:s["end_char"]].encode()), s["end_byte"])
                self.assertFalse(text[s["start_char"]].isspace() or text[s["end_char"] - 1].isspace())

    def test_golden_vectors_are_current(self):
        self.assertEqual(GOLDEN.read_bytes(), sp.golden_bytes(),
                         "regenerate: python full_a_spans.py golden ../../grammar-a-baseline/tests/golden/span-golden-v1.json")
        cases = json.loads(GOLDEN.read_text())["cases"]
        self.assertGreaterEqual(len(cases), 36)
        self.assertTrue(any(c["expected"]["spans"] for c in cases) and any(not c["expected"]["spans"] for c in cases))


class TokenHeadModel(unittest.TestCase):
    def test_token_head_never_changes_document_outputs(self):
        ids, mask, content = base.batch_of(base.TEXTS)
        plain = base.make_model(seed=4)
        with_head = base.make_model(seed=4)
        with_head.token_head = full_a.TokenHead()
        a = plain(ids, mask, content, autocast=False)
        b = with_head(ids, mask, content, autocast=False)
        for key in ("origin_logits", "involvement_logit", "share", "ai_logits"):
            self.assertTrue(torch.equal(a[key], b[key]), key)
        self.assertIsNone(a["span_logits"])
        self.assertEqual(tuple(b["span_logits"].shape), tuple(ids.shape))
        head = with_head.token_head
        x = torch.randn(5, 768)
        manual = head.second(0.5 * head.first(x) * (1 + torch.tanh(math.sqrt(2 / math.pi) *
                             (head.first(x) + .044715 * head.first(x) ** 3)))).squeeze(-1)
        self.assertTrue(torch.allclose(head(x), manual, atol=1e-6))
        self.assertTrue(float(head.first.bias.abs().sum()) == 0 and abs(float(head.first.weight.std()) - .02) < .002)

    def test_frozen_document_trains_only_the_token_head(self):
        ids, mask, content = base.batch_of(base.TEXTS)
        model = base.make_model(seed=4)
        model.token_head = full_a.TokenHead()
        model.freeze_document()
        model.train()
        self.assertFalse(model.encoder.training or model.heads.training)
        out = model(ids, mask, content)
        labels = torch.where(content, torch.ones_like(ids), torch.full_like(ids, -1))
        loss = full_a.objective(out, [{"origin": 1, "share": 1., "human": None, "ai": 0}] * 3, [1.] * 3,
                                {"origin": 3., "share": 3., "ai": 3., "token_head": 3.}, labels,
                                token_head_weight=1., token_weights=[1.] * 3, document=False)
        loss.backward()
        self.assertTrue(all(p.grad is None for p in model.encoder.parameters()))
        self.assertTrue(all(p.grad is None for p in model.heads.parameters()))
        self.assertTrue(all(p.grad is not None for p in model.token_head.parameters()))
        groups = full_a.parameter_groups(model, 1e-5, 1., 1e-4, .01, 1e-3)
        self.assertEqual({g["name"] for g in groups}, {"token_head.wd0.01", "token_head.wd0.0"})

    def test_padding_and_specials_carry_no_token_label(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "bundle", Path(d) / "config"
            base.write_bundle(root, config)
            pop = bundle.Population(root, "dev_iter", full_a.hf_tokenize(base.FakeTokenizer()),
                                    {base.PAD, base.CLS, base.SEP}, ["claude-x", "gen-a", "gen-b"],
                                    cohort_weighted=False, span_words=True)
            idx = list(range(len(pop)))
            ids, mask, content, labels, words = pop.batch(idx, "cpu", base.PAD)
            self.assertTrue(bool((labels[mask == 0] == -1).all()))
            self.assertTrue(bool((labels[:, 0] == -1).all()))
            for j, i in enumerate(idx):
                n = int(pop.lengths[i])
                self.assertEqual(int(labels[j, n - 1]), -1)
                tok, wid, lab = pop.word_index(i, "cpu")
                self.assertTrue(bool(words[j, tok].all()))
            counts = pop.token_counts()
            self.assertGreater(counts["labelled_ai_words"], 0)
            self.assertEqual(counts["span_word_failures"], {})


class ExportCases(unittest.TestCase):
    def test_fp64_reference_widens_the_model_and_keeps_its_function(self):
        ids, mask, content = base.batch_of(base.TEXTS)
        model = base.make_model(seed=4)
        model.token_head = full_a.TokenHead()
        model.eval()
        m64, rotary = export_full_a.fp64_reference(model)
        self.assertGreater(rotary, 0)
        self.assertEqual(next(model.parameters()).dtype, torch.float32)
        # Rotary tables are HF's own: float32 angles and cos/sin, widened exactly.
        modules = [(a, b) for a, b in zip(model.encoder.modules(), m64.encoder.modules())
                   if type(a).__name__ == "ModernBertRotaryEmbedding"]
        self.assertEqual(len(modules), rotary)
        pos = torch.arange(2048)[None, :]
        for r32, r64 in modules:
            self.assertNotIn("forward", vars(r64))
            c32, s32 = r32(torch.zeros(1, dtype=torch.float32), pos)
            c64, s64 = r64(torch.zeros(1, dtype=torch.float64), pos)
            self.assertEqual(c64.dtype, torch.float64)
            self.assertTrue(torch.equal(c64, c32.double()) and torch.equal(s64, s32.double()))
            angles64 = r32.inv_freq.double()[:, None] * pos[0].double()[None, :]
            self.assertGreater(float((angles64.T.cos() - c64[0, :, :angles64.shape[0]]).abs().max()), 1e-6)
        with torch.no_grad():
            a = model(ids, mask, content, autocast=False)
            with export_full_a.reference_attention():
                same = model(ids, mask, content, autocast=False)
                b = m64(ids, mask, content, autocast=False)
        # The reference attention is HF's for float32 inputs, bit for bit.
        for key in ("origin_logits", "span_logits", "h"):
            self.assertTrue(torch.equal(a[key], same[key]), key)
        self.assertEqual(b["span_logits"].dtype, torch.float64)
        self.assertEqual(b["origin"].dtype, torch.float64)
        for key in ("origin", "span_logits", "h"):
            self.assertLess(float((a[key].double() - b[key]).abs().max()), 1e-4, key)

    def test_long_case_is_the_longest_prefix_within_2048_tokens(self):
        tok = full_a.hf_tokenize(base.FakeTokenizer())
        case = export_full_a.long_case(tok)
        n = len(tok(case["text"])[0])
        self.assertLessEqual(n, full_a.MAX_TOKENS)
        self.assertGreaterEqual(n, full_a.MAX_TOKENS - 1)
        cases = export_full_a.synthetic_cases(tok, True)
        self.assertEqual(len(cases), 3 + 12)
        self.assertEqual(len({c["id"] for c in cases}), 15)
        self.assertEqual(len(export_full_a.synthetic_cases(tok, False)), 3)

    def test_case_file_appends_checked_cases_after_the_built_in_ones(self):
        tok = full_a.hf_tokenize(base.FakeTokenizer())
        built_in = export_full_a.synthetic_cases(tok, True)
        taken = {c["id"] for c in built_in}
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "cases.jsonl"

            def write(rows):
                path.write_bytes(b"".join(json.dumps(r).encode() + b"\n" for r in rows))
                return path

            rows = [{"id": "public-b", "text": "Second case text."}, {"id": "public-a", "text": "First case."}]
            got = export_full_a.case_file(write(rows), tok, taken)
            self.assertEqual(got, rows)
            cases = export_full_a.synthetic_cases(tok, True, got)
            self.assertEqual(cases[:len(built_in)], built_in)
            self.assertEqual(cases[len(built_in):], rows)
            refused = [
                [{"id": "public-a", "text": "One."}, {"id": "public-a", "text": "Two."}],
                [{"id": "synthetic-short", "text": "Taken id."}],
                [{"id": "local-x", "text": "Reserved prefix."}],
                [{"id": "public-a", "text": "Extra key.", "label": 1}],
                [{"id": "public-a", "text": "   "}],
                [{"id": "", "text": "Empty id."}],
                [{"id": "public-long", "text": "word " * (full_a.MAX_TOKENS + 1)}],
                [],
            ]
            for bad in refused:
                with self.assertRaises(ValueError, msg=json.dumps(bad)[:80]):
                    export_full_a.case_file(write(bad), tok, taken)
            path.write_bytes(b'{"id": "public-a", "text": "No final newline."}')
            with self.assertRaises(ValueError):
                export_full_a.case_file(path, tok, taken)

    def test_public_reference_case_file(self):
        path = Path(__file__).resolve().parent / "reference_cases" / "a-public-reference-cases-v1.jsonl"
        tok = full_a.hf_tokenize(base.FakeTokenizer())
        taken = {c["id"] for c in export_full_a.synthetic_cases(tok, True)}
        cases = export_full_a.case_file(path, tok, taken)
        self.assertEqual(len(cases), 200)
        self.assertEqual(len({c["text"] for c in cases}), 200)

    def test_local_cases_round_robin_over_strata(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "bundle", Path(d) / "config"
            rows = base.write_bundle(root, config)
            dev = [r for r in rows if r["split"] == "dev_iter"]
            tok = full_a.hf_tokenize(base.FakeTokenizer())
            got = export_full_a.local_cases(root, tok, {base.PAD, base.CLS, base.SEP}, [], 5)
            self.assertEqual(len(got), 5)
            by_id = {r["id"]: r for r in dev}
            origins = [by_id[c["id"][len("local-"):]]["origin"] for c in got]
            self.assertEqual(len(set(origins)), 3)
            self.assertEqual(got, export_full_a.local_cases(root, tok, set(), [], 5))
            self.assertEqual(export_full_a.local_cases(root, tok, set(), [], 0), [])


def span_protocol():
    p = synthetic.protocol_template()
    plain = {"conditional_loss_weight": 0., "bins": None}
    head = {"hidden": 16, "loss_weight": 1., "learning_rate": 1e-2}
    s0 = {"span_head": None, "share_mode": "head", "share_supervision": plain, "batching": {"kind": "random"},
          "style_adversary": None, "contrastive": None, "token_head": head,
          "freeze": "encoder_and_document_heads", "token_weighting": "row_weight"}
    p["arms"] = {"S0": s0}
    return p


class FrozenFit(unittest.TestCase):
    """E1 fit, then an S0 fit from it: document outputs bit-identical, token head trained."""

    def test_s0_from_init_fit_keeps_document_outputs(self):
        fitter = base.EndToEnd()
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "bundle", Path(d) / "config"
            rows = base.write_bundle(root, config)
            labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
            e1 = Path(d) / "e1"
            # The init fit must be a trained checkpoint on every torch build. With the real
            # Dev-iter score, whether a few steps beat the untrained start on this tiny bundle
            # depends on the build's numerics (torch 2.14 CPU keeps evaluation 0, 2.9.1 does
            # not). A strictly falling selection score makes E1 select its last, trained
            # evaluation; every tensor, output and checkpoint is still the real trainer's.
            real_evaluate, calls = trainer.evaluate, []

            def trained_selection(*args, **kwargs):
                result = real_evaluate(*args, **kwargs)
                calls.append(None)
                return (1. / (1 + len(calls)),) + tuple(result[1:])
            with mock.patch.object(trainer, "evaluate", trained_selection):
                fitter.fit(root, config, e1, "E1")
            e1_fit = json.loads((e1 / "fit.json").read_text())
            e1_history = json.loads((e1 / "history.json").read_text())
            self.assertEqual(e1_fit["selected_evaluation"], len(e1_history) - 1)
            self.assertGreater(e1_history[-1]["global_step"], 0)
            self.assertTrue(e1_fit["trained_selected_heads"]["origin"])
            p = span_protocol()
            p["early_stopping"]["metric"] = trainer.WORD_METRIC
            sconf = Path(d) / "sconf"
            synthetic.write_config(sconf, bundle.bundle_digests(root)[bundle.MANIFEST], labels, p)
            (sconf / "init.json").write_text("{}")
            s0 = Path(d) / "s0"
            real = trainer.load_init_fit
            with mock.patch.object(trainer, "load_init_fit", lambda m, path, seed, c: real(m, e1, seed, c)):
                fitter_args_fit(fitter, root, sconf, s0, "S0")
            fit = json.loads((s0 / "fit.json").read_text())
            self.assertEqual(fit["selection_metric"], trainer.WORD_METRIC)
            self.assertTrue(fit["frozen_document_check"]["encoder_and_document_heads_bitwise_equal_to_init"])
            self.assertEqual(fit["frozen_document_check"]["selected_dev_iter_document_raw_vs_init"]
                             ["document_raw_max_abs_error"], 0.)
            self.assertEqual(fit["init_raw_gate"]["document_raw_max_abs_error"], 0.)
            self.assertTrue(fit["trained_selected_heads"]["origin"])
            history = json.loads((s0 / "history.json").read_text())
            words = [h["dev_iter_diagnostics"]["span"]["mixed_word_brier_raw"] for h in history]
            self.assertTrue(all(math.isfinite(w) for w in words))
            self.assertEqual(fit["selection_value"], min(words))
            self.assertEqual(fit["selection_split_involvement_brier"], history[0]["dev_iter_involvement_brier"])
            a = [json.loads(x) for x in (e1 / "dev_iter-raw.jsonl").read_text().splitlines()]
            b = [json.loads(x) for x in (s0 / "dev_iter-raw.jsonl").read_text().splitlines()]
            self.assertEqual([r["origin_logits"] for r in a], [r["origin_logits"] for r in b])
            tokens = [json.loads(x) for x in (s0 / "dev_iter-token-logits.jsonl").read_text().splitlines()]
            self.assertEqual(len(tokens), len(b))
            self.assertEqual([len(t["token_logits"]) for t in tokens], [t["tokens"] for t in tokens])
            from safetensors.torch import load_file
            self.assertIn("token_head.first.weight", load_file(str(s0 / "heads.safetensors")))
            # The token head enters the heads artifact with the diagnostic emission spec.
            model = base.make_model(seed=4)
            model.token_head = full_a.TokenHead(width=16)
            _, _, _, _, spec = export_full_a.calibration(None, {}, "a", "b", token_head=True)
            art = export_full_a.heads_artifact(model, p["arms"]["S0"], {"human_profiles": [], "ai_profiles": labels},
                                               {"contract_sha256": "b" * 64}, sconf / "init.json",
                                               {"method": "identity_v1"}, {"human": 1., "ai": 1.}, {"origin": 1},
                                               spec)
            self.assertEqual(art["token_head"]["hidden"], 16)
            self.assertEqual(art["weights"]["token_head.first.weight"]["shape"], [16, 768])
            self.assertEqual(art["span_emission"]["decoder"], sp.DIAGNOSTIC_DECODER)
            # The same S0 fit under the 24-word span-target floor: document outputs unchanged, the
            # masking recorded, and the floor bound through the protocol digest.
            p["span_target_floor"] = {"policy": "slop_ninja_span_target_floor24_v1", "min_words": 24}
            fconf = Path(d) / "fconf"
            synthetic.write_config(fconf, bundle.bundle_digests(root)[bundle.MANIFEST], labels, p)
            (fconf / "init.json").write_text("{}")
            s0f = Path(d) / "s0f"
            with mock.patch.object(trainer, "load_init_fit", lambda m, path, seed, c: real(m, e1, seed, c)):
                fitter_args_fit(fitter, root, fconf, s0f, "S0")
            ffit = json.loads((s0f / "fit.json").read_text())
            self.assertNotIn("span_target_floor", fit)
            rec = ffit["span_target_floor"]
            self.assertEqual(rec["train"]["policy"], "slop_ninja_span_target_floor24_v1")
            self.assertGreater(rec["train"]["masked_ai_tokens"], 0)
            self.assertEqual(rec["train"]["masked_ai_tokens"],
                             fit["train_population"]["labelled_ai_tokens"] - ffit["train_population"]["labelled_ai_tokens"])
            self.assertEqual(ffit["train_population"]["labelled_human_tokens"], fit["train_population"]["labelled_human_tokens"])
            self.assertTrue(ffit["frozen_document_check"]["encoder_and_document_heads_bitwise_equal_to_init"])
            self.assertEqual((s0f / "dev_iter-raw.jsonl").read_bytes(), (s0 / "dev_iter-raw.jsonl").read_bytes())
            self.assertNotEqual(ffit["binding"]["protocol_sha256"], fit["binding"]["protocol_sha256"])

    def test_frozen_arm_needs_init_fit_and_token_head(self):
        p = span_protocol()
        bad = dict(p["arms"]["S0"], token_head=None)
        with self.assertRaises(ValueError):
            trainer.validate_arm("bad", bad)
        p["early_stopping"]["metric"] = trainer.GUARDED_WORD_METRIC
        with self.assertRaisesRegex(ValueError, "document guard"):
            trainer.validate_protocol(p)
        p["document_guard"] = .002
        trainer.validate_protocol(p)
        p["early_stopping"]["metric"] = trainer.INVOLVEMENT_METRIC
        with self.assertRaisesRegex(ValueError, "document guard"):
            trainer.validate_protocol(p)


FLOOR24 = {"policy": "slop_ninja_span_target_floor24_v1", "min_words": 24}


class SpanTargetFloor(unittest.TestCase):
    """slop_ninja_span_target_floor24_v1: AI spans under 24 words leave the span targets."""

    @staticmethod
    def case():
        # 30 human words, a 3-word AI span, 5 human words, a 25-word AI span, 2 human words.
        h = lambda k, n: [f"h{k}{i}" for i in range(n)]
        a = lambda k, n: [f"a{k}{i}" for i in range(n)]
        parts = [(h(0, 30), 0), (a(1, 3), 1), (h(2, 5), 0), (a(3, 25), 1), (h(4, 2), 0)]
        text, spans, pos = "", [], 0
        for i, (ws, ai) in enumerate(parts):
            chunk = " ".join(ws)
            if i:
                text += " "
            start = len(text)
            text += chunk
            if ai:
                spans.append([start, len(text)])
        return text, spans

    def test_short_ai_runs_are_unsupervised_not_human(self):
        text, spans = self.case()
        tok = base.FakeTokenizer()
        ids, offs = full_a.hf_tokenize(tok)(text)
        special = {base.PAD, base.CLS, base.SEP}
        lab = full_a.token_labels(text, ids, offs, special, 2, spans)
        got, words, rec = bundle.floor_span_targets(text, offs, 2, spans, lab, 24)
        self.assertEqual(rec, {"gold_spans": 2, "short_spans": 1, "masked_ai_tokens": 3, "masked_ai_words": 3})
        for x, y, (a, b) in zip(lab, got, offs):
            piece = text[a:b].strip()
            if piece.startswith("a1"):
                self.assertEqual((x, y), (1, -1))
            else:
                self.assertEqual(x, y)
        self.assertEqual(sum(y == 1 for y in got), 25)
        self.assertEqual(sum(y == 0 for y in got), sum(x == 0 for x in lab))
        ws = sp.words(text)
        self.assertEqual([w for w, l in zip(ws, words) if l == -1], [w for w in ws if text[w[0]:w[1]].startswith("a1")])
        # A 24-word run is kept; a 23-word model-only document is unsupervised; human rows are unchanged.
        t24 = " ".join(f"m{i}" for i in range(24))
        ids, offs = full_a.hf_tokenize(tok)(t24)
        lab = full_a.token_labels(t24, ids, offs, special, 1, None)
        self.assertEqual(bundle.floor_span_targets(t24, offs, 1, None, lab, 24)[0], lab)
        t23 = " ".join(f"m{i}" for i in range(23))
        ids, offs = full_a.hf_tokenize(tok)(t23)
        lab = full_a.token_labels(t23, ids, offs, special, 1, None)
        self.assertTrue(all(x == -1 for x in bundle.floor_span_targets(t23, offs, 1, None, lab, 24)[0]))
        lab = full_a.token_labels(t23, ids, offs, special, 0, None)
        self.assertEqual(bundle.floor_span_targets(t23, offs, 0, None, lab, 24)[0], lab)

    def test_population_floor_changes_only_ai_span_targets(self):
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "bundle", Path(d) / "config"
            base.write_bundle(root, config)
            mk = lambda split, floor, **kw: bundle.Population(
                root, split, full_a.hf_tokenize(base.FakeTokenizer()), {base.PAD, base.CLS, base.SEP},
                ["claude-x", "gen-a", "gen-b"], cohort_weighted=split == "train", span_target_floor=floor, **kw)
            for split, kw in (("train", {}), ("dev_iter", {"span_words": True})):
                off, on = mk(split, None, **kw), mk(split, FLOOR24, **kw)
                self.assertIsNone(off.span_target_floor_record)
                changed = off.labels != on.labels
                self.assertTrue(bool(((off.labels[changed] == 1) & (on.labels[changed] == -1)).all()))
                self.assertEqual(int(changed.sum()), on.span_target_floor_record["masked_ai_tokens"])
                self.assertEqual([m.id for m in off.meta], [m.id for m in on.meta])
                self.assertEqual(off.weights, on.weights)
                self.assertEqual([off.targets(i) for i in range(len(off))], [on.targets(i) for i in range(len(on))])
                if kw:
                    wc = off.word_labels != on.word_labels
                    self.assertTrue(bool(((off.word_labels[wc] == 1) & (on.word_labels[wc] == -1)).all()))
                    self.assertEqual(int(wc.sum()), on.span_target_floor_record["masked_ai_words"])

    def test_only_the_declared_floor_is_accepted(self):
        bundle.validate_span_target_floor(None)
        bundle.validate_span_target_floor(dict(FLOOR24))
        for bad in ({"policy": "slop_ninja_span_target_floor24_v1", "min_words": 10}, {"min_words": 24},
                    {"policy": "other", "min_words": 24}, dict(FLOOR24, extra=1)):
            with self.assertRaises(ValueError):
                bundle.validate_span_target_floor(bad)
        p = span_protocol()
        p["span_target_floor"] = dict(FLOOR24)
        trainer.validate_protocol(p)
        p["span_target_floor"]["min_words"] = 12
        with self.assertRaisesRegex(ValueError, "span target floor"):
            trainer.validate_protocol(p)


def fitter_args_fit(fitter, root, config, out, arm):
    """EndToEnd.fit with --init-fit set (the patched loader supplies the path)."""
    real_run = trainer.run

    def run(args):
        args.init_fit = "patched"
        return real_run(args)
    with mock.patch.object(trainer, "run", run):
        fitter.fit(root, config, out, arm)


if __name__ == "__main__":
    unittest.main()

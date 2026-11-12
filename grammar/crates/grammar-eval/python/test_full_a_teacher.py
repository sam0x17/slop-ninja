"""Teacher classes (teacher_a): a tiny synthetic ModernBERT snapshot written here, never downloaded.

CPU only; no corpus, provider, detector or network access. Covers the class pins, the heads' structure against
WarmStartA, fresh-head determinism by seed, snapshot verification, a teacher fit through the real trainer and
loader, `raw` scoring of it, the export refusal, the embedding run, and the base paths with the option absent.
"""
from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
import re
import tempfile
import unittest
from unittest import mock

import torch
from safetensors.torch import load_file

import export_full_a
import full_a
import full_a_bundle as bundle
import public_a
import raw_full_a
import synthetic_full_a as synthetic
import teacher_a as teacher
import test_full_a as base_tests
import train_full_a as trainer
from warm_start_a import BASE_TEMPERATURE, WarmStartA

HERE = Path(__file__).resolve().parent
CONTRACT_TOOL = HERE.parent.parent / "grammar-a-baseline" / "examples" / "prepare_full_a_contract.rs"
TINY = "tiny_test_v1"
WIDTH = 64
SPECIALS = ["[PAD]", "[CLS]", "[SEP]", "[UNK]", "[MASK]"]


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write_snapshot(root, seed=5):
    """A tiny ModernBERT masked-LM snapshot (config, weights, fast word-level tokenizer) in the upstream layout."""
    from tokenizers import Tokenizer, models, processors
    from tokenizers.pre_tokenizers import Whitespace
    from transformers import ModernBertConfig, ModernBertForMaskedLM, PreTrainedTokenizerFast
    root = Path(root)
    root.mkdir(parents=True)
    words = sorted(set(synthetic.WORDS) | {w.capitalize() for w in synthetic.WORDS} | {".", ","})
    vocab = {t: i for i, t in enumerate(SPECIALS + words)}
    tok = Tokenizer(models.WordLevel(vocab, unk_token="[UNK]"))
    tok.pre_tokenizer = Whitespace()
    tok.post_processor = processors.TemplateProcessing(single="[CLS] $A [SEP]", special_tokens=[
        ("[CLS]", vocab["[CLS]"]), ("[SEP]", vocab["[SEP]"])])
    PreTrainedTokenizerFast(tokenizer_object=tok, unk_token="[UNK]", pad_token="[PAD]", cls_token="[CLS]",
                            sep_token="[SEP]", mask_token="[MASK]").save_pretrained(root)
    torch.manual_seed(seed)
    config = ModernBertConfig(vocab_size=len(vocab), hidden_size=WIDTH, intermediate_size=32, num_hidden_layers=2,
                              num_attention_heads=4, max_position_embeddings=2048, local_attention=8,
                              global_attn_every_n_layers=2, pad_token_id=0, cls_token_id=1, sep_token_id=2,
                              bos_token_id=1, eos_token_id=2, classifier_pooling="mean", attention_dropout=0.,
                              embedding_dropout=0., mlp_dropout=0., classifier_dropout=0., reference_compile=False)
    ModernBertForMaskedLM(config).save_pretrained(root, safe_serialization=True)
    return {p.name: {"sha256": sha(p), "bytes": p.stat().st_size} for p in sorted(root.iterdir())}


def tiny_class(files):
    return {"repository": "synthetic/tiny-modernbert", "revision": "0" * 40, "license": "synthetic",
            "hidden_size": WIDTH, "layers": 2, "files": files}


def teacher_protocol(name=TINY):
    p = synthetic.protocol_template()
    p["teacher_class"] = name
    p["arms"] = {k: p["arms"][k] for k in ("E1", "E1share", "E1adv", "E1con")}
    return p


def teacher_contract(manifest_sha, labels, name=TINY, width=WIDTH):
    c = synthetic.contract_for(manifest_sha, labels, parent=teacher.artifact_id(name))
    c["teacher_class"] = name
    c["raw_dimension"] = width
    c["embedding_normalization"].update(dimension=width, mean=[.01] * width, scale=[.5] * width)
    return c


def write_teacher_bundle(d, protocol=None):
    root, config = Path(d) / "bundle", Path(d) / "config"
    rows = synthetic.factorial_rows(groups=2)
    digests = synthetic.write_bundle(root, rows)
    labels = sorted({r["generator_label"] for r in rows if r["generator_label"]})
    config.mkdir()
    (config / "contract.json").write_text(json.dumps(teacher_contract(digests[bundle.MANIFEST], labels)))
    (config / "protocol.json").write_text(json.dumps(protocol or teacher_protocol()))
    return root, config, rows


def run_fit(root, config, out, snapshot, seed=17, arm="E1", **extra):
    fields = {"data": str(root), "config": str(config), "artifact": str(snapshot), "init": None, "out": str(out),
              "arm": arm, "seed": seed, "device": "cpu", "report_split": None, "consume_sealed_split": None,
              "use_record": None, "train_subset_rows": None, "init_fit": None, "train_allowlist": None,
              "final_step": True}
    fields.update(extra)
    trainer.run(type("Args", (), fields))


class TinyClass(unittest.TestCase):
    """A synthetic snapshot registered as a teacher class for the duration of each test."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.d = Path(self.tmp.name)
        self.snapshot = self.d / "snapshot"
        self.patch = mock.patch.dict(teacher.CLASSES, {TINY: tiny_class(write_snapshot(self.snapshot))})
        self.patch.start()

    def tearDown(self):
        self.patch.stop()
        self.tmp.cleanup()


class Pins(unittest.TestCase):
    def test_modernbert_large_pins(self):
        c = teacher.CLASSES["modernbert_large_v1"]
        self.assertTrue(re.fullmatch(r"45bb4654[0-9a-f]{32}", c["revision"]))
        self.assertEqual((c["repository"], c["hidden_size"], c["layers"]), ("answerdotai/ModernBERT-large", 1024, 28))
        # The tokenizer files are ModernBERT-base's upstream bytes, from which the artifact spec's tokenizer is made.
        upstream = public_a.recipe()["base_model"]["files"]
        for name in ("tokenizer.json", "tokenizer_config.json", "special_tokens_map.json"):
            self.assertEqual(c["files"][name]["sha256"], upstream[name])
        self.assertEqual(sorted(c["files"]), ["config.json", "model.safetensors", "special_tokens_map.json",
                                              "tokenizer.json", "tokenizer_config.json"])
        self.assertTrue(all(re.fullmatch(r"[0-9a-f]{64}", f["sha256"]) and f["bytes"] > 0 for f in c["files"].values()))
        self.assertTrue(re.fullmatch(r"[0-9a-f]{64}", teacher.artifact_id("modernbert_large_v1")))
        self.assertEqual(teacher.artifact_id("modernbert_large_v1"), teacher.artifact_id("modernbert_large_v1"))

    def test_rust_contract_tool_knows_every_class_width(self):
        table = re.search(r"TEACHER_CLASSES: &\[\(&str, usize\)\] = &\[(.*?)\];", CONTRACT_TOOL.read_text(), re.S)
        rust = dict((n, int(w)) for n, w in re.findall(r'\("([a-z0-9_]+)", (\d+)\)', table.group(1)))
        self.assertEqual(rust, {k: v["hidden_size"] for k, v in teacher.CLASSES.items()})

    def test_unknown_class_and_contract_widths(self):
        with self.assertRaisesRegex(ValueError, "unknown teacher class"):
            teacher.validate_class("modernbert_huge_v9")
        self.assertEqual(teacher.contract_width({}), 768)
        self.assertEqual(teacher.contract_width({"teacher_class": "modernbert_large_v1"}), 1024)


class Heads(unittest.TestCase):
    def test_structure_and_forward_equal_warm_start_at_768(self):
        init = base_tests.init_for(base_tests.tiny_encoder(3))
        mean, scale = [.1] * 768, [2.] * 768
        torch.manual_seed(41)
        warm = WarmStartA(init, mean, scale, 2, 3, "text")
        torch.manual_seed(41)
        fresh = teacher.TeacherHeads(768, mean, scale, 2, 3)
        a, b = warm.state_dict(), fresh.state_dict()
        self.assertEqual(list(a), list(b))
        # Same draws in the same order; only WarmStartA copies V6's classifier into the origin head.
        for k in a:
            self.assertEqual(torch.equal(a[k], b[k]), not k.startswith("origin."), k)
        self.assertEqual((warm.base_temperature, fresh.base_temperature), (BASE_TEMPERATURE, BASE_TEMPERATURE))
        fresh.load_state_dict(a)
        x = torch.randn(5, 768 + full_a.CFG_ZEROS)
        out_w, out_f = warm(x), fresh(x)
        self.assertEqual(sorted(out_w), sorted(out_f))
        for k in out_w:
            self.assertTrue(torch.equal(out_w[k], out_f[k]), k)

    def test_width_and_fresh_init_by_seed(self):
        norm = {"mean": [0.] * WIDTH, "scale": [1.] * WIDTH}
        state = torch.get_rng_state()
        a = teacher.fresh_heads(WIDTH, norm, 0, 3, 17)
        self.assertTrue(torch.equal(torch.get_rng_state(), state))  # the caller's generator is untouched
        b = teacher.fresh_heads(WIDTH, norm, 0, 3, 17)
        c = teacher.fresh_heads(WIDTH, norm, 0, 3, 29)
        for k, v in a.state_dict().items():
            self.assertTrue(torch.equal(v, b.state_dict()[k]), k)
        self.assertFalse(torch.equal(a.origin.weight, c.origin.weight))
        self.assertEqual(tuple(a.residual[0].weight.shape), (64, WIDTH + full_a.CFG_ZEROS))
        self.assertEqual(tuple(a.ai.weight.shape), (3, WIDTH))
        x = torch.randn(2, WIDTH + full_a.CFG_ZEROS)
        out = a(x)
        self.assertTrue(torch.equal(out["encoding"], x[:, :WIDTH]))  # zero residual output at init
        with self.assertRaisesRegex(ValueError, "feature dimension"):
            a(torch.randn(2, 768 + full_a.CFG_ZEROS))
        with self.assertRaisesRegex(ValueError, "normalization"):
            teacher.TeacherHeads(WIDTH, [0.] * 768, [1.] * 768, 0, 3)
        with self.assertRaisesRegex(ValueError, "seed"):
            teacher.fresh_heads(WIDTH, norm, 0, 3, None)


class Snapshot(TinyClass):
    def test_verification_and_load(self):
        digests = teacher.verify_snapshot(self.snapshot, TINY)
        self.assertEqual(sorted(digests), sorted(teacher.CLASSES[TINY]["files"]))
        encoder, tokenizer, parent = teacher.load(self.snapshot, TINY, torch.device("cpu"))
        self.assertEqual(parent, {"artifact_id": teacher.artifact_id(TINY), "teacher_class": TINY})
        self.assertEqual((teacher.width_of(encoder), len(encoder.model.layers)), (WIDTH, 2))
        self.assertFalse(any(p.requires_grad for p in encoder.parameters()))
        ids, _ = full_a.hf_tokenize(tokenizer)("The quiet river.")
        self.assertEqual((ids[0], ids[-1]), (1, 2))
        # A hidden download cache is ignored; any other entry, a changed byte or a missing file is refused.
        (self.snapshot / ".cache").mkdir()
        teacher.verify_snapshot(self.snapshot, TINY)
        (self.snapshot / "pytorch_model.bin").write_bytes(b"x")
        with self.assertRaisesRegex(ValueError, "expected exactly"):
            teacher.verify_snapshot(self.snapshot, TINY)
        (self.snapshot / "pytorch_model.bin").unlink()
        config = self.snapshot / "config.json"
        original = config.read_bytes()
        config.write_bytes(original.replace(b'"hidden_size"', b'"hidden_size" '))
        with self.assertRaisesRegex(ValueError, "size differs"):
            teacher.verify_snapshot(self.snapshot, TINY)
        config.write_bytes(original[:-1] + (b" " if original[-1:] != b" " else b"\n"))
        with self.assertRaisesRegex(ValueError, "digest differs"):
            teacher.verify_snapshot(self.snapshot, TINY)
        config.unlink()
        with self.assertRaisesRegex(ValueError, "expected exactly"):
            teacher.load(self.snapshot, TINY, torch.device("cpu"))


class Validation(unittest.TestCase):
    def test_protocol_and_contract(self):
        p = teacher_protocol("modernbert_large_v1")
        self.assertEqual(trainer.validate_protocol(p)["teacher_class"], "modernbert_large_v1")
        for arm in ("E2w", "E2wall"):
            bad = copy.deepcopy(p)
            bad["arms"][arm] = synthetic.protocol_template()["arms"][arm]
            with self.subTest(arm=arm), self.assertRaisesRegex(ValueError, "document heads only"):
                trainer.validate_protocol(bad)
        token = copy.deepcopy(p)
        token["arms"]["E1"].update(token_head={"hidden": 8, "loss_weight": 1., "learning_rate": 1e-3},
                                   freeze="none", token_weighting="row_weight")
        with self.assertRaisesRegex(ValueError, "document heads only"):
            trainer.validate_protocol(token)
        with self.assertRaisesRegex(ValueError, "unknown teacher class"):
            trainer.validate_protocol(dict(p, teacher_class="nope_v1"))
        # The base protocol and contract are accepted exactly as before.
        trainer.validate_protocol(synthetic.protocol_template())
        trainer.validate_contract(synthetic.contract_for("m", ["a", "b"]), "m")
        c = teacher_contract("m", ["a", "b"], "modernbert_large_v1", 1024)
        self.assertEqual(trainer.validate_contract(c, "m")["raw_dimension"], 1024)
        for change in ({"raw_dimension": 768}, {"teacher_class": None}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                trainer.validate_contract(dict(c, **change), "m")
        narrow = copy.deepcopy(c)
        narrow["embedding_normalization"]["dimension"] = 768
        with self.assertRaises(ValueError):
            trainer.validate_contract(narrow, "m")


class TeacherFit(TinyClass):
    def test_fit_raw_export_refusal_and_determinism(self):
        root, config, rows = write_teacher_bundle(self.d)
        fits = {}
        for name, seed, arm in (("a", 17, "E1con"), ("b", 17, "E1con"), ("c", 29, "E1con"), ("adv", 17, "E1adv")):
            out = self.d / ("fit-" + name)
            run_fit(root, config, out, self.snapshot, seed=seed, arm=arm)
            fits[name] = out
        report = json.loads((fits["a"] / "fit.json").read_text())
        b = report["binding"]
        self.assertTrue(report["complete"])
        self.assertIsNone(b["initializer_sha256"])
        self.assertEqual(b["parent_artifact_id"], teacher.artifact_id(TINY))
        self.assertEqual(b["teacher_class"], report["teacher_class"])
        self.assertEqual({k: b["teacher_class"][k] for k in ("class", "width", "layers", "exportable")},
                         {"class": TINY, "width": WIDTH, "layers": 2, "exportable": False})
        self.assertEqual(b["teacher_class"]["module_sha256"], sha(teacher.__file__))
        gate = report["initial_gate"]
        self.assertIsNone(gate["initial_origin_max_abs_error"])
        self.assertEqual(gate["initial_reference"], f"none (teacher class {TINY}: fresh heads)")
        self.assertLessEqual(gate["padded_vs_unpadded_origin_max_abs_error"], 1e-4)
        self.assertTrue(any("never exported" in x for x in report["limitations"]))
        self.assertGreater(report["aux_heads"]["batch_structure"]["anchor_rows"], 0)
        heads = load_file(str(fits["a"] / "heads.safetensors"))
        self.assertEqual(tuple(heads["residual.0.weight"].shape), (64, WIDTH + full_a.CFG_ZEROS))
        # Trained: the fresh heads and the encoder moved from their initial values.
        initial = teacher.fresh_heads(WIDTH, json.loads((config / "contract.json").read_text())
                                      ["embedding_normalization"], 0, 3, 17)
        self.assertFalse(torch.equal(heads["origin.weight"], initial.origin.weight))
        # Deterministic by seed: the same seed gives the same weights, another seed others.
        for f in ("encoder.safetensors", "heads.safetensors", "dev_iter-raw.jsonl"):
            self.assertEqual((fits["a"] / f).read_bytes(), (fits["b"] / f).read_bytes(), f)
        self.assertNotEqual((fits["a"] / "heads.safetensors").read_bytes(),
                            (fits["c"] / "heads.safetensors").read_bytes())
        self.assertIsNotNone(json.loads((fits["adv"] / "fit.json").read_text())["aux_heads"]["style_adversary"])

        # teacher_a raw scores the teacher from its snapshot, without --init, as the fit itself scored Dev-iter,
        # in the exporter's raw format (the Rust CAL calibration's input).
        raw_args = dict(data=str(root), config=str(config), fit=str(fits["a"]), artifact=str(self.snapshot),
                        init=None, part="dev_iter", device="cpu", allow_diagnostic_subset=False)
        teacher.raw(type("Args", (), dict(raw_args, out=str(self.d / "raw"))))
        self.assertEqual((self.d / "raw" / "raw.jsonl").read_bytes(), (fits["a"] / "dev_iter-raw.jsonl").read_bytes())
        self.assertEqual((self.d / "raw" / "predictions.jsonl").read_bytes(),
                         (fits["a"] / "dev_iter-predictions.jsonl").read_bytes())
        m = json.loads((self.d / "raw" / "manifest.json").read_text())
        dev = [r for r in rows if r["split"] == "dev_iter"]
        self.assertEqual((m["schema"], m["partition"], m["rows"], m["teacher_class"]),
                         (export_full_a.RAW_SCHEMA, "dev_iter", len(dev), b["teacher_class"]))
        self.assertEqual((m["fit_sha256"], m["selected_checkpoint_sha256"]),
                         (sha(fits["a"] / "fit.json"), report["selected_checkpoint_sha256"]))
        for part in ("dev_select", "test", "train"):
            with self.subTest(part=part), self.assertRaisesRegex(ValueError, "Dev-iter or CAL"):
                teacher.raw(type("Args", (), dict(raw_args, part=part, out=str(self.d / ("raw-" + part)))))
        # raw_full_a (the Train block and rows files) scores it with the same function.
        train_rows = [r for r in rows if r["split"] == "train"]
        raw_full_a.train(type("Args", (), dict(raw_args, out=str(self.d / "raw-train"))))
        mt = json.loads((self.d / "raw-train" / "manifest.json").read_text())
        self.assertEqual((mt["rows"], mt["teacher_class"]), (len(train_rows), b["teacher_class"]))
        dev_file = self.d / "dev-rows.jsonl"
        dev_file.write_text("".join(json.dumps(r) + "\n" for r in dev))
        raw_full_a.rows(type("Args", (), dict(raw_args, out=str(self.d / "raw-rows"), rows=str(dev_file),
                                              split="dev_iter")))
        self.assertEqual((self.d / "raw-rows" / "raw.jsonl").read_bytes(),
                         (fits["a"] / "dev_iter-raw.jsonl").read_bytes())
        with self.assertRaisesRegex(ValueError, "no --init"):
            raw_full_a.train(type("Args", (), dict(raw_args, init=str(config / "contract.json"),
                                                   out=str(self.d / "raw-train2"))))
        # The exporter never packages or scores a teacher: the fit binds no initializer, so its lineage check
        # refuses every route (export, raw), and nothing is written.
        for route, extra in ((export_full_a.export, dict(calibration=None, local_references=0)),
                             (export_full_a.raw, {})):
            out = self.d / ("exporter-" + route.__name__)
            with self.subTest(route=route.__name__), self.assertRaisesRegex(ValueError, "changed bound file"):
                route(type("Args", (), dict(raw_args, init=str(config / "contract.json"), out=str(out), **extra)))
            self.assertFalse(out.exists())
        # A tampered checkpoint or a changed teacher module is refused by the teacher lineage check.
        with mock.patch.object(teacher, "__file__", str(config / "contract.json")):
            with self.assertRaisesRegex(ValueError, "changed bound file"):
                teacher.raw(type("Args", (), dict(raw_args, out=str(self.d / "raw-moved"))))

    def test_refusals(self):
        root, config, _ = write_teacher_bundle(self.d)
        with self.assertRaisesRegex(ValueError, "--init"):
            run_fit(root, config, self.d / "with-init", self.snapshot, init=str(config / "contract.json"))
        with self.assertRaisesRegex(ValueError, "init-fit"):
            run_fit(root, config, self.d / "with-init-fit", self.snapshot, init_fit=str(self.d))
        # A contract without the class (or naming another) never pairs with a teacher protocol, nor the reverse.
        contract = json.loads((config / "contract.json").read_text())
        plain = self.d / "plain"
        plain.mkdir()
        (plain / "contract.json").write_text(json.dumps({k: v for k, v in contract.items() if k != "teacher_class"}))
        (plain / "protocol.json").write_text((config / "protocol.json").read_text())
        with self.assertRaises(ValueError):
            run_fit(root, plain, self.d / "plain-fit", self.snapshot)
        base_protocol = self.d / "base-protocol"
        base_protocol.mkdir()
        (base_protocol / "contract.json").write_text(json.dumps(contract))
        (base_protocol / "protocol.json").write_text(json.dumps(synthetic.protocol_template()))
        with self.assertRaisesRegex(ValueError, "different teacher classes"):
            run_fit(root, base_protocol, self.d / "base-fit", self.snapshot, init=str(config / "contract.json"))
        # A snapshot that is not the class's pinned bytes is refused at load.
        (self.snapshot / "tokenizer.json").write_bytes(b"{}")
        with self.assertRaisesRegex(ValueError, "snapshot"):
            run_fit(root, config, self.d / "changed", self.snapshot)


class Embed(TinyClass):
    def test_embedding_run_matches_the_trainer_h_with_or_without_padding(self):
        requests = self.d / "requests.jsonl"
        rows = synthetic.factorial_rows(groups=1, splits=("train",))
        with requests.open("w") as f:
            for r in rows:
                f.write(json.dumps({"id": r["id"], "text": r["text"]}) + "\n")
            f.write(json.dumps({"id": "empty", "text": "   "}) + "\n")
        runs = {}
        for limit in (0, 4096):
            out = self.d / f"run-{limit}"
            teacher.main(["embed", "--teacher-class", TINY, "--artifact", str(self.snapshot), "--requests",
                          str(requests), "--output", str(out), "--max-padded", str(limit)])
            runs[limit] = out
        m = json.loads((runs[0] / "manifest.json").read_text())
        self.assertEqual((m["schema"], m["dimension"], m["requested"], m["unsupported"]),
                         (teacher.EMBED_SCHEMA, WIDTH, len(rows) + 1, 1))
        self.assertEqual(m["representation_contract"]["artifact_id"], teacher.artifact_id(TINY))
        got = {}
        for limit, out in runs.items():
            lines = [json.loads(x) for x in (out / "rows.jsonl").read_text().splitlines()]
            self.assertEqual([r["id"] for r in lines], [r["id"] for r in rows] + ["empty"])  # request order
            got[limit] = {r["id"]: r["embedding"] for r in lines if r["status"] == "ok"}
        # Padded groups agree with one-request forwards to float tolerance.
        worst = max(abs(a - b) for k in got[0] for a, b in zip(got[0][k], got[4096][k]))
        self.assertLess(worst, 1e-4)
        # The h is the trainer's: FullA's encoding of the same text at step 0 (FP32, unpadded).
        encoder, tokenizer, _ = teacher.load(self.snapshot, TINY, torch.device("cpu"))
        heads = teacher.fresh_heads(WIDTH, {"mean": [0.] * WIDTH, "scale": [1.] * WIDTH}, 0, 2, 17)
        model = full_a.FullA(encoder, heads).eval()
        ids, _ = full_a.hf_tokenize(tokenizer)(rows[0]["text"])
        t = torch.tensor([ids])
        content = torch.tensor([full_a.content_mask_for(ids, set(tokenizer.all_special_ids))])
        with torch.no_grad():
            h = model(t, torch.ones_like(t), content, autocast=False)["h"][0].double().tolist()
        self.assertEqual(h, got[0][rows[0]["id"]])
        with self.assertRaisesRegex(ValueError, "max padded"):
            teacher.main(["embed", "--teacher-class", TINY, "--artifact", str(self.snapshot), "--requests",
                          str(requests), "--output", str(self.d / "bad"), "--max-padded", "100"])


class BasePathsUnchanged(unittest.TestCase):
    """With `teacher_class` absent the trainer builds WarmStartA from the V6 initializer, the auxiliary heads read
    768 coordinates and the initial gate is the V6 check (the bitwise harness covers the whole computation)."""

    def test_build_aux_and_gate_without_the_option(self):
        encoder = base_tests.tiny_encoder(7)
        init = base_tests.init_for(encoder)
        contract = synthetic.contract_for("m", ["gen-a", "gen-b", "claude-x"])
        protocol = synthetic.protocol_template()
        torch.manual_seed(13)
        model, _ = trainer.build(encoder, contract, init, protocol["arms"]["E1"], protocol, torch.device("cpu"))
        torch.manual_seed(13)
        expected = WarmStartA(init, [0.] * 768, [1.] * 768, 0, 3, "text")
        self.assertIsInstance(model.heads, WarmStartA)
        for k, v in expected.state_dict().items():
            self.assertTrue(torch.equal(v, model.heads.state_dict()[k]), k)
        # A seed passed to build changes nothing on the base path.
        torch.manual_seed(13)
        again, _ = trainer.build(encoder, contract, init, protocol["arms"]["E1"], protocol, torch.device("cpu"), 99)
        for k, v in again.heads.state_dict().items():
            self.assertTrue(torch.equal(v, model.heads.state_dict()[k]), k)
        with tempfile.TemporaryDirectory() as d:
            root, config = Path(d) / "bundle", Path(d) / "config"
            base_tests.write_bundle(root, config)
            out = Path(d) / "fit"
            base_tests.EndToEnd().fit(root, config, out, "E2wall")
            report = json.loads((out / "fit.json").read_text())
            self.assertNotIn("teacher_class", report)
            self.assertNotIn("teacher_class", report["binding"])
            self.assertEqual(report["initial_gate"]["initial_reference"], "V6 classifier")
            self.assertLess(report["initial_gate"]["initial_origin_max_abs_error"], 1e-6)
            self.assertEqual(report["binding"]["initializer_sha256"], sha(config / "init.json"))


if __name__ == "__main__":
    unittest.main()

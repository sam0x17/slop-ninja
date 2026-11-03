"""Offline spans-S2 span-tower export checks: export v4 of a tiny S2c fit through the real trainer and exporter
(encoder and document heads equal to the init fit's, tower weights equal to the fit's, heads v3 declaration,
FP64 references), heads v2 unchanged without a tower, and the tower golden vectors that bind the Rust runtime
branch.

No corpus, artifact, provider, detector or network access. CPU only.
"""
import hashlib
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import torch
from safetensors.torch import load_file, save_file

import export_full_a
import full_a
import full_a_bundle as bundle
import full_a_s2 as s2
import provenance_paths
import span_tower_golden as golden
import synthetic_full_a as synthetic
import test_full_a as base
import test_full_a_s2 as s2_tests
import test_full_a_spans as spans_tests
import train_full_a as trainer

GOLDEN = Path(__file__).resolve().parents[2] / "grammar-a-baseline" / "tests" / "golden" / "span-tower-golden-v1.json"


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def assert_close(test, a, b, tol, path="$"):
    """Equal structure and non-float leaves; floats within tol (torch builds may differ in the last bits)."""
    if isinstance(a, float) or isinstance(b, float):
        test.assertTrue(isinstance(a, (int, float)) and isinstance(b, (int, float)), path)
        test.assertLessEqual(abs(a - b), tol, path)
    elif isinstance(a, dict):
        test.assertEqual(sorted(a), sorted(b), path)
        for k in a:
            assert_close(test, a[k], b[k], tol, f"{path}.{k}")
    elif isinstance(a, list):
        test.assertEqual(len(a), len(b), path)
        for i, (x, y) in enumerate(zip(a, b)):
            assert_close(test, x, y, tol, f"{path}[{i}]")
    else:
        test.assertEqual(a, b, path)


class TowerGolden(unittest.TestCase):
    def test_golden_vectors_are_current(self):
        want = json.loads(GOLDEN.read_text())
        got = json.loads(golden.golden_bytes())
        assert_close(self, got, want, 1e-12)
        self.assertEqual(want["tower"]["layers"], [18, 19, 20, 21])
        self.assertEqual(want["tower"]["attention"], ["global", "local", "local", "global"])
        self.assertEqual([c["id"] for c in want["cases"]], ["one-token", "six-tokens", "twenty-tokens", "forty-tokens"])

    def test_generator_is_dyadic_and_float32_exact(self):
        for name in ("model.layers.18.attn.Wqkv.weight", "span_tower.norm.weight", "model.embeddings.tok_embeddings.weight"):
            v = golden.golden_values(name, 4096)
            self.assertEqual(torch.tensor(v, dtype=torch.float64).float().double().tolist(), v)
        self.assertNotEqual(golden.golden_values("a", 8), golden.golden_values("b", 8))
        self.assertTrue(all(0.9375 <= x < 1.0625 for x in golden.golden_values("x.norm.weight", 1000)))
        self.assertTrue(all(-0.125 <= x < 0.125 for x in golden.golden_values("x.attn.Wo.weight", 1000)))

    def test_golden_tower_reads_its_own_weights_and_the_layer_input(self):
        model = golden.build()
        n = len(model.encoder.model.layers)
        k = model.tower_layers
        # The copies keep their layers' attention but carry their own weights.
        for j, layer in enumerate(model.span_tower.layers):
            self.assertEqual(layer.attn.layer_id, n - k + j)
            self.assertFalse(torch.equal(layer.attn.Wqkv.weight, model.encoder.model.layers[n - k + j].attn.Wqkv.weight))
        ids = torch.tensor([golden.CASES[1][1]])
        content = torch.tensor([full_a.content_mask_for(golden.CASES[1][1], {0, 1, 2})])
        seen = {}
        model.encoder.model.layers[n - k].register_forward_pre_hook(
            lambda m, a, kw: seen.update(input=a[0].clone(), kwargs=dict(kw)), with_kwargs=True)
        with torch.no_grad():
            o = model(ids, torch.ones_like(ids), content, autocast=False)
            hidden = seen["input"]
            for layer in model.span_tower.layers:
                hidden = layer(hidden, **seen["kwargs"])[0]
            again = model.token_head(model.span_tower.norm(hidden))
            plain = model.token_head(model.encoder.model(input_ids=ids, attention_mask=torch.ones_like(ids)).last_hidden_state)
        # The token head reads the tower run from the first copied layer's input, not the encoder's final states.
        self.assertTrue(torch.equal(o["span_logits"], again))
        self.assertGreater(float((o["span_logits"] - plain).abs().max()), 1e-3)

    def test_tower_rotary_requires_each_copy_to_keep_its_source_base(self):
        model = golden.build()
        self.assertEqual(export_full_a.tower_rotary(model), model.tower_layers)
        # Layers 18 (global) and 19 (local) have different bases; swapping the copies' tables must be refused.
        a, b = (model.span_tower.layers[j].attn.rotary_emb for j in (0, 1))
        self.assertFalse(torch.equal(a.inv_freq, b.inv_freq))
        a.inv_freq, b.inv_freq = b.inv_freq.clone(), a.inv_freq.clone()
        with self.assertRaises(ValueError):
            export_full_a.tower_rotary(model)
        # The FP64 reference copy checks the same way.
        restored = golden.build()
        self.assertEqual(export_full_a.tower_rotary(export_full_a.fp64_reference(restored)[0]), restored.tower_layers)


def fit_e1_and_s2c(d):
    """A tiny E1 init fit, then an S2c (one-layer tower) fit from it, through the real trainer."""
    fitter = base.EndToEnd()
    root, config = Path(d) / "bundle", Path(d) / "config"
    rows = s2_tests.spliced_rows()
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
    conf = Path(d) / "conf-S2c"
    synthetic.write_config(conf, digests[bundle.MANIFEST], labels, s2_tests.tower_protocol())
    (conf / "init.json").write_text("{}")
    out = Path(d) / "S2c"
    real = trainer.load_init_fit
    with mock.patch.object(trainer, "load_init_fit", lambda m, path, seed, c: real(m, e1, seed, c)):
        spans_tests.fitter_args_fit(fitter, root, conf, out, "S2c")
    return root, conf, e1, out


def run_export(d, root, conf, fit, name):
    artifact = Path(d) / "artifact"
    if not artifact.exists():
        (artifact / "classifier").mkdir(parents=True)
        parent = base.tiny_encoder(11)
        save_file({k: v.contiguous() for k, v in parent.state_dict().items()},
                  str(artifact / "classifier" / "model.safetensors"))
        (artifact / "manifest.json").write_text("{}")
    out = Path(d) / name
    args = SimpleNamespace(data=str(root), config=str(conf), fit=str(fit), artifact=str(artifact),
                           init=str(conf / "init.json"), out=str(out), device="cpu", calibration=None,
                           local_references=0, allow_diagnostic_subset=False)
    encoder = base.tiny_encoder(11)
    with mock.patch.object(trainer, "load", lambda a, p, dev: (encoder, base.FakeTokenizer(),
                                                               {"artifact_id": "synthetic-parent"})), \
            mock.patch.object(export_full_a, "initializer", lambda path: base.init_for(encoder)):
        export_full_a.export(args)
    return out


class TowerExport(unittest.TestCase):
    def test_tower_fit_exports_v4_with_the_init_fit_encoder_and_document_heads(self):
        with tempfile.TemporaryDirectory() as d:
            root, conf, e1, fit = fit_e1_and_s2c(d)
            out = run_export(d, root, conf, fit, "export")
            m = json.loads((out / "manifest.json").read_text())
            heads = json.loads((out / "heads.json").read_text())
            report = json.loads((fit / "fit.json").read_text())
            self.assertEqual(m["schema"], export_full_a.EXPORT_SCHEMA_TOWER)
            self.assertEqual(heads["schema"], export_full_a.HEADS_SCHEMA_TOWER)
            self.assertEqual(heads["token_head"]["input"], export_full_a.TOWER_INPUT)
            self.assertEqual(heads["token_head"]["rule"], full_a.TOKEN_HEAD_RULE)
            self.assertEqual(m["token_head"], heads["token_head"])
            decl = heads["span_tower"]
            self.assertEqual(m["span_tower"]["declaration"], decl)
            # Tiny encoder: two layers, global every second layer; a one-layer tower copies layer 1 (local).
            self.assertEqual((decl["encoder_layers"], decl["source_layer"], decl["layers"], decl["attention"]),
                             (2, 1, [1], ["local"]))
            self.assertEqual(decl["rule"], s2.TOWER_RULE)
            self.assertEqual(m["span_tower"]["sha256"], sha(out / export_full_a.TOWER_FILE))
            tower = load_file(str(out / export_full_a.TOWER_FILE))
            fit_heads = load_file(str(fit / "heads.safetensors"))
            self.assertEqual(sorted(tower), export_full_a.tower_tensor_names(1))
            self.assertEqual(m["span_tower"]["tensors"], len(tower))
            for k, v in tower.items():
                self.assertEqual(v.dtype, torch.float32)
                self.assertTrue(torch.equal(v, fit_heads[k]), k)
            # The encoder and document heads are the init fit's, bit for bit.
            self.assertEqual((out / "encoder.safetensors").read_bytes(), (e1 / "encoder.safetensors").read_bytes())
            init_heads = load_file(str(e1 / "heads.safetensors"))
            document = [k for k in heads["weights"] if not k.startswith("token_head.")]
            self.assertTrue(document)
            compared = 0
            for k in document:
                w = heads["weights"][k]
                if k in init_heads and w["shape"] == list(init_heads[k].shape):
                    self.assertEqual(w["values"], init_heads[k].double().flatten().tolist(), k)
                    compared += 1
            self.assertGreaterEqual(compared, 8)
            self.assertFalse(any(k.startswith(export_full_a.TOWER_PREFIX) for k in heads["weights"]))
            fh = m["fit_hashes"]
            self.assertEqual((fh["fit_json_sha256"], fh["binding_json_sha256"], fh["heads_sha256"],
                              fh["encoder_sha256"], fh["selected_checkpoint_sha256"]),
                             (sha(fit / "fit.json"), sha(fit / "binding.json"), report["heads_sha256"],
                              report["encoder_sha256"], report["selected_checkpoint_sha256"]))
            self.assertEqual(fh["init_fit"]["fit_sha256"], sha(e1 / "fit.json"))
            # No local path reaches the manifest: the trainer recorded the init fit (outside the repository) by
            # its final component, so the published fit binding is the fit's own binding unchanged, the content
            # of the bytes binding_json_sha256 names, and no rewrite record is written.
            self.assertEqual(provenance_paths.local_paths(m), [])
            self.assertEqual((fh["init_fit"]["fit_dir"], m["fit_binding"]["init_fit"]["fit_dir"]), ("e1", "e1"))
            own = json.loads((fit / "binding.json").read_text())
            self.assertEqual(own, m["fit_binding"])
            self.assertFalse((out / "provenance-rewrite.json").exists())
            # The export found the init fit through the fit's private location file, by its digest.
            location = json.loads((fit / trainer.INIT_FIT_LOCATION).read_text())
            self.assertEqual(Path(location["fit_dir"]), e1.resolve())
            self.assertTrue(fh["encoder_equal_to_init_fit"] and fh["document_heads_equal_to_init_fit"])
            self.assertEqual(m["reference_tower_rotary_modules"], 1)
            # FP64 references carry the tower's token logits; the FP32 drift diagnostic stays close.
            r64 = json.loads((out / "reference-predictions.json").read_text())
            r32 = json.loads((out / "reference-predictions-fp32.json").read_text())
            self.assertEqual(len(r64), 15)
            for a, b in zip(r64, r32):
                self.assertEqual(a["reference_precision"], "fp64")
                self.assertEqual(len(a["token_logits"]), len(a["token_ids"]))
                self.assertLess(max(abs(x - y) for x, y in zip(a["token_logits"], b["token_logits"])), 1e-4)
            self.assertTrue(all(r["span_emission"]["status"] in ("ok", "unsupported") for r in r64))

    def test_calibration_copy_keeps_values_and_drops_local_paths(self):
        cal = {"schema": export_full_a.CALIBRATION_SCHEMA, "involvement": {"selected": {"knots": [0.125, 1e-7]}},
               "spans_provenance": {"cal_fit": "/opt/elsewhere/eval/cal-maps.json", "cal_fit_sha256": "a" * 64},
               "document_calibration_source": {"file": "/opt/elsewhere/cal/calibration.json", "note": "n"}}
        with tempfile.TemporaryDirectory() as d:
            src, dest = Path(d) / "in.json", Path(d) / "out.json"
            src.write_text(json.dumps(cal, indent=1))
            changes = export_full_a.copy_calibration(src, cal, dest)
            self.assertEqual(sorted(changes), [("/document_calibration_source/file", "calibration.json"),
                                               ("/spans_provenance/cal_fit", "cal-maps.json")])
            out = json.loads(dest.read_bytes())
            self.assertEqual(provenance_paths.local_paths(out), [])
            self.assertEqual(out["involvement"], cal["involvement"])
            self.assertEqual(out["spans_provenance"]["cal_fit_sha256"], "a" * 64)
            # Without a local path the copy is byte for byte.
            clean = provenance_paths.portable_at(cal, export_full_a.CALIBRATION_POINTERS)[0]
            src.write_text(json.dumps(clean, indent=1))
            dest.unlink()
            self.assertEqual(export_full_a.copy_calibration(src, clean, dest), [])
            self.assertEqual(dest.read_bytes(), src.read_bytes())

    def test_contract_paths_are_reported_not_changed(self):
        contract = {"embedding_normalization": {"runs": [{"run": "/opt/elsewhere/run-000"}, {"run": "n/run-001"}]}}
        with mock.patch("sys.stderr", new_callable=io.StringIO) as err:
            export_full_a.report_paths(contract, [("/fit_binding/init_fit/fit_dir", "e1")], [])
        text = err.getvalue()
        self.assertIn("provenance: manifest.json /fit_binding/init_fit/fit_dir recorded as 'e1'", text)
        self.assertIn("warning: contract.json keeps 1 absolute local path (/embedding_normalization/runs/0/run)",
                      text)
        self.assertNotIn("/opt/elsewhere", text)
        with mock.patch("sys.stderr", new_callable=io.StringIO) as err:
            export_full_a.report_paths({"runs": [{"run": "n/run-001"}]}, [], [])
        self.assertEqual(err.getvalue(), "")
        # A local path outside the known pointers is warned about by pointer, never rewritten.
        with mock.patch("sys.stderr", new_callable=io.StringIO) as err:
            export_full_a.report_paths({}, [], [], [("manifest.json", {"note": "run in /home/someone/x",
                                                                       "heads_pointer": "/heads/sha256"})])
        text = err.getvalue()
        self.assertIn("warning: manifest.json keeps 2 strings", text)
        self.assertIn("(/note, /heads_pointer)", text)
        self.assertNotIn("someone", text)

    def test_a_binding_with_absolute_paths_is_published_only_as_a_recorded_rewrite(self):
        binding = {"arm": "S2c", "contract_sha256": "c" * 64,
                   "init_fit": {"fit_dir": "/opt/elsewhere/fits/e1", "fit_sha256": "f" * 64},
                   "route": "/v1/load", "train_allowlist": {"file": "/opt/elsewhere/allow.json", "sha256": "a" * 64}}
        with self.assertRaises(ValueError) as refused:
            export_full_a.published_binding(binding, False)
        self.assertIn("--rewrite-provenance", str(refused.exception))
        self.assertNotIn("/opt/elsewhere", str(refused.exception))
        copy, changes = export_full_a.published_binding(binding, True)
        self.assertEqual(sorted(changes), [("/fit_binding/init_fit/fit_dir", "e1"),
                                           ("/fit_binding/train_allowlist/file", "allow.json")])
        # Only the provenance pointers change; the contract digest stays as trained, routes stay.
        self.assertEqual((copy["contract_sha256"], copy["route"]), (binding["contract_sha256"], "/v1/load"))
        clean = dict(binding, init_fit={"fit_dir": "e1", "fit_sha256": "f" * 64}, train_allowlist={"file": "a"})
        self.assertEqual(export_full_a.published_binding(clean, False), (clean, []))
        with tempfile.TemporaryDirectory() as d:
            fit, out = Path(d) / "fit", Path(d) / "out"
            fit.mkdir()
            out.mkdir()
            (fit / "binding.json").write_text(json.dumps(binding))
            cal = Path(d) / "cal.json"
            cal.write_text("{}")
            record = export_full_a.rewrite_record(out, fit, cal, changes, [("/spans_provenance/cal_fit", "m.json")])
            written = json.loads((out / "provenance-rewrite.json").read_text())
            self.assertEqual(record, written)
            self.assertEqual(written["schema"], export_full_a.REWRITE_SCHEMA)
            self.assertEqual(written["files"][0]["source_sha256"], sha(fit / "binding.json"))
            self.assertEqual(written["files"][0]["pointers"], [p for p, _ in changes])
            # Pointers only: no path the binding held appears in the record.
            self.assertNotIn("/opt/elsewhere", json.dumps(written))
            self.assertIsNone(export_full_a.rewrite_record(Path(d) / "none", fit, None, [], []))

    def test_heads_without_a_tower_stay_v2(self):
        model = base.make_model(seed=4)
        model.token_head = full_a.TokenHead(width=16)
        p = spans_tests.span_protocol()
        _, _, _, _, spec = export_full_a.calibration(None, {}, "a", "b", token_head=True)
        args = (model, p["arms"]["S0"], {"human_profiles": [], "ai_profiles": ["a1", "a2"]},
                {"contract_sha256": "b" * 64}, Path(__file__), {"method": "identity_v1"}, {"human": 1., "ai": 1.},
                {"origin": 1}, spec)
        a, b = export_full_a.heads_artifact(*args), export_full_a.heads_artifact_v2(*args)
        self.assertEqual(a, b)
        self.assertEqual(a["schema"], full_a.HEADS_SCHEMA)
        self.assertNotIn("span_tower", a)
        self.assertEqual(a["token_head"]["input"], "final_norm_token_states")
        # A tower declaration needs the tower on the model.
        with self.assertRaises(ValueError):
            export_full_a.heads_artifact(*args, tower={"rule": s2.TOWER_RULE})


if __name__ == "__main__":
    unittest.main()

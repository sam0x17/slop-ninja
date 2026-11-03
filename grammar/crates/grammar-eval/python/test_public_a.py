"""Public A recipe: pins against the artifact spec, schema conversions and parent loading.

CPU only, synthetic values, no model download and no network."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re
import tempfile
import unittest

import public_a
import train_full_a as trainer

REPO = Path(__file__).resolve().parents[4]
SPEC = REPO / "subnet" / "crates" / "artifact-check" / "src"


def rust_fields(struct):
    """Field names of a Rust struct in artifact-check's roles.rs that every record must have: those without
    `#[serde(default ...)]`, which mark the fields of later export and binding versions."""
    source = (SPEC / "roles.rs").read_text()
    body = re.search(r"struct " + struct + r" \{(.*?)\n\}", source, re.S).group(1)
    fields = re.finditer(r"^((?:\s+#\[[^\]]*\]\n)*)\s+([a-z_][a-z0-9_]*):", body, re.M)
    return sorted(m.group(2) for m in fields if "serde(default" not in m.group(1))


def rust_const(name):
    return re.search(name + r': &str =\s*"([0-9a-f]{64})"', (SPEC / "spec.rs").read_text()).group(1)


class Pins(unittest.TestCase):
    def test_recipe_matches_the_artifact_spec(self):
        r = public_a.recipe()
        names = public_a.roster_names(r)
        self.assertEqual(len(names), 33)
        self.assertEqual(r["pinned"]["ai_roster_sha256"], rust_const("AI_PROFILES_SHA256"))
        self.assertEqual(trainer.roster_sha256(names), r["pinned"]["ai_roster_sha256"])
        self.assertEqual(r["pinned"]["tokenizer_sha256"], rust_const("TOKENIZER_SHA256"))
        for name in ("protocol.json", "protocol-smoke.json"):
            p = trainer.validate_protocol(json.loads((public_a.RECIPE.parent / name).read_text()))
            self.assertEqual(p["ai_roster"], {"rule": trainer.FIXED_ROSTER,
                                              "profiles_sha256": r["pinned"]["ai_roster_sha256"]})
            for arm in p["arms"].values():
                self.assertIsNone(arm["span_head"])
                self.assertNotIn("token_head", arm)

    def test_export_fields_mirror_the_checker(self):
        self.assertEqual(sorted(public_a.EXPORT_FIELDS), rust_fields("ExportManifestV2"))
        self.assertEqual(sorted(public_a.FIT_BINDING_FIELDS), rust_fields("FitBinding"))


def heads_v2(**extra):
    return {"schema": "slop_ninja_full_a_heads_v2", "architecture": {}, "span_head": None, "token_head": None,
            "span_emission": None, "share_mode": "head", "supervision": {"ai": 1, "human": 0, "origin": 1,
                                                                         "share": 1, "token": 1, "token_head": 0},
            "weights": {}, **extra}


def manifest_v3():
    binding = {k: "x" for k in public_a.FIT_BINDING_FIELDS}
    binding.update(spans_rule_sha256="y", init_fit=None)
    m = {k: k for k in ("arm", "seed", "selected_evaluation", "parent_artifact_id", "parent_manifest_sha256",
                        "final_test_opened", "production_qualified", "reference_device", "runtime",
                        "selected_checkpoint_sha256")}
    m.update(schema="slop_ninja_full_a_export_v3", fit_binding=binding, token_head=None, span_emission=None,
             reference_precision="fp64")
    return m


class Conversions(unittest.TestCase):
    def test_heads_v1_drops_only_the_v2_fields(self):
        h = public_a.heads_v1(heads_v2())
        self.assertEqual(h["schema"], public_a.HEADS_V1)
        self.assertNotIn("token_head", h)
        self.assertNotIn("span_emission", h)
        self.assertEqual(sorted(h["supervision"]), ["ai", "human", "origin", "share", "token"])
        flagged = heads_v2(supervision={"ai": 1, "human": 0, "origin": 1, "share": 1, "token": 1, "token_head": 1})
        self.assertNotIn("token_head", public_a.heads_v1(flagged)["supervision"])
        for bad in (heads_v2(token_head={"hidden": 4}), heads_v2(span_head={"pooling": "max"}),
                    heads_v2(span_emission={"decoder": "x"})):
            with self.assertRaises(SystemExit):
                public_a.heads_v1(bad)

    def test_manifest_v2_has_exactly_the_spec_fields(self):
        members = {k: {"file": k, "sha256": "0" * 64} for k in ("heads", "calibration", "contract", "encoder",
                                                                 "reference_requests", "reference_predictions")}
        m, omitted = public_a.manifest_v2(manifest_v3(), members, "1" * 64, ["limit"])
        self.assertEqual(sorted(m), sorted(public_a.EXPORT_FIELDS))
        self.assertEqual(sorted(m["fit_binding"]), sorted(public_a.FIT_BINDING_FIELDS))
        self.assertEqual(omitted, ["init_fit", "spans_rule_sha256"])
        self.assertEqual((m["schema"], m["calibrated"], m["complete"]), (public_a.EXPORT_V2, True, True))
        short = manifest_v3()
        del short["fit_binding"]["trainer_sha256"]
        with self.assertRaises(SystemExit):
            public_a.manifest_v2(short, members, "1" * 64, [])


class Parent(unittest.TestCase):
    def write_parent(self, root):
        cls = root / "classifier"
        cls.mkdir(parents=True)
        (cls / "config.json").write_text("{}")
        files = {"classifier/config.json": public_a.sha256(cls / "config.json")}
        body = {"schema": public_a.PARENT_SCHEMA, "files": files}
        manifest = dict(body, artifact_id=hashlib.sha256(public_a.canonical(body)).hexdigest())
        (root / "manifest.json").write_bytes(public_a.canonical(manifest) + b"\n")

    def test_parent_identity_and_members_are_checked_before_loading(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "parent"
            self.write_parent(root)
            (root / "classifier" / "config.json").write_text('{"changed": true}')
            with self.assertRaisesRegex(SystemExit, "parent member changed"):
                public_a.load_parent(root, "cpu")
            m = json.loads((root / "manifest.json").read_text())
            m["files"]["classifier/config.json"] = "0" * 64
            (root / "manifest.json").write_text(json.dumps(m))
            with self.assertRaisesRegex(SystemExit, "identity"):
                public_a.load_parent(root, "cpu")
            (root / "manifest.json").write_text(json.dumps({"schema": "slop_ninja_v6_release"}))
            with self.assertRaisesRegex(SystemExit, "public parent"):
                public_a.load_parent(root, "cpu")

    def test_a_warm_start_encoder_replaces_every_tensor_and_must_match_the_roster(self):
        import torch
        from safetensors.torch import save_file
        saved = {"model.w": torch.zeros(2, 3), "classifier.weight": torch.zeros(3, 2),
                 "classifier.bias": torch.zeros(3)}
        with tempfile.TemporaryDirectory() as d:
            cls = Path(d) / "classifier"
            cls.mkdir()
            save_file(saved, str(cls / "model.safetensors"))
            enc = {k: torch.full_like(v, 0.5) for k, v in saved.items()}
            path = Path(d) / "encoder.safetensors"
            save_file(enc, str(path))
            got, record = public_a.warm_start_encoder(cls, saved, path)
            self.assertTrue(all(torch.equal(got[k], enc[k]) for k in enc))
            self.assertEqual(public_a.sha256(cls / "model.safetensors"), record["sha256"])
            self.assertEqual(record["sha256"], public_a.sha256(path))
            for bad in ({"model.w": torch.zeros(2, 3)},
                        dict(enc, **{"model.w": torch.zeros(3, 2)}),
                        dict(enc, **{"model.w": torch.zeros(2, 3, dtype=torch.float16)})):
                save_file(bad, str(path))
                with self.assertRaises(SystemExit):
                    public_a.warm_start_encoder(cls, saved, path)

    def test_initializer_accepts_only_a_public_parent_init(self):
        import warm_start_a as ws
        init = {"schema": ws.INIT_SCHEMA, "source": public_a.INIT_SOURCE, "parent_schema": public_a.PARENT_SCHEMA,
                "dimension": 768, "origin_order": ws.ORIGIN_ORDER, "base_temperature": ws.BASE_TEMPERATURE,
                "weights": {"origin.weight": {"shape": [3, 768], "values": [0.] * 2304},
                            "origin.bias": {"shape": [3], "values": [0.] * 3}}}
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "init.json"
            path.write_text(json.dumps(init))
            self.assertEqual(public_a.parent_initializer(path)["source"], public_a.INIT_SOURCE)
            for key, bad in (("source", "v6"), ("base_temperature", 1.0), ("parent_schema", None)):
                path.write_text(json.dumps(dict(init, **{key: bad})))
                with self.assertRaises(SystemExit):
                    public_a.parent_initializer(path)


if __name__ == "__main__":
    unittest.main()

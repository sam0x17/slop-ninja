"""Synthetic CPU tests only; no artifact loading, corpus or provider calls."""
from contextlib import redirect_stderr
import io
import json
import os
from pathlib import Path
import tempfile
import shutil
import subprocess
import importlib.util
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import torch
import shared_a as shared
import warm_start_a as warm


def init_value():
    g = torch.Generator().manual_seed(7)
    return {"schema": warm.INIT_SCHEMA, "dimension": 768, "origin_order": warm.ORIGIN_ORDER,
            "base_temperature": warm.BASE_TEMPERATURE, "artifact_id": "synthetic-artifact",
            "original_dependency_and_reference_checks_passed": True,
            "weight_file": "classifier/model.safetensors",
            "weight_file_sha256": "88bb0eda753ae3c6a9dc31c266e4ec24459937b77d07c27a609aa11ccdba36c0",
            "weights": {"origin.weight": {"shape": [3, 768],
                "values": (torch.randn(3, 768, generator=g) * .01).flatten().tolist()},
                "origin.bias": {"shape": [3], "values": [.2, -.1, .05]}}}


def model(mode="combined"):
    return warm.WarmStartA(init_value(), [.1] * 768, [2.] * 768, 2, 2, mode)


def synthetic_batch():
    return {"x": torch.randn(6, 2588), "weight": torch.tensor([.1, .2, .15, .1, .2, .25]),
            "y": torch.tensor([0, 1, 2, 0, 1, 2]), "share": torch.tensor([0., 1., .3, 0., 1., .6]),
            "share_mask": torch.ones(6, dtype=torch.bool), "human": torch.tensor([0, -1, 1, 1, -1, 0]),
            "ai": torch.tensor([-1, 0, 1, -1, 1, 0]),
            "rows": [{"source_group": f"synthetic-{i}", "roles": {
                role: {"present": True, "known": i % 2 == 0,
                       "identity": ("a" if role == "human" else "x") if i % 2 == 0 else "withheld"}
                for role in shared.ROLES}} for i in range(6)]}


class WarmStartTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        warm.configure(torch.device("cpu"), 17)

    def test_initial_identity_and_both_temperature_factors(self):
        x = torch.randn(7, 2588)
        for mode in ("text", "combined"):
            m = model(mode)
            for temperature in (.1, 1., 3.):
                out = m(x, temperature)
                self.assertTrue(torch.equal(out["encoding"], x[:, :768]))
                expected = (m.origin(x[:, :768]) / warm.BASE_TEMPERATURE / temperature).softmax(-1)
                self.assertTrue(torch.equal(out["origin"], expected))
                torch.testing.assert_close(out["origin_logits"], m.origin(x[:, :768]) / warm.BASE_TEMPERATURE)

    def test_all_heads_share_z_and_cfg_mask_survives_training(self):
        m = model("text")
        with torch.no_grad():
            m.residual[2].weight.fill_(.01)
            m.residual[2].bias.fill_(.02)
        x, y = torch.randn(4, 2588), torch.randn(4, 2588)
        y[:, :768] = x[:, :768]
        out = m(x)
        self.assertTrue(torch.equal(out["encoding"], m(y)["encoding"]))
        for name in ("human", "ai"):
            torch.testing.assert_close(out[name + "_logits"], getattr(m, name)(out["encoding"]))
        torch.testing.assert_close(out["conditional_mixed_share"], m.mixed_share(out["encoding"]).squeeze(-1).sigmoid())
        self.assertTrue(bool((out["share"] >= out["origin"][:, 1]).all()))
        self.assertTrue(bool((out["share"] <= out["origin"][:, 1:].sum(1)).all()))

    def test_joint_loss_gradients_train_residual_and_every_head(self):
        m = model()
        batch = {"x": torch.randn(6, 2588), "weight": torch.ones(6) / 6,
                 "y": torch.tensor([0, 1, 2, 0, 1, 2]), "share": torch.tensor([0., 1., .3, 0., 1., .6]),
                 "share_mask": torch.ones(6, dtype=torch.bool), "human": torch.tensor([0, -1, 1, 1, -1, 0]),
                 "ai": torch.tensor([-1, 0, 1, -1, 1, 0])}
        optimizer = torch.optim.AdamW(m.parameters(), lr=.001, weight_decay=.01)
        for _ in range(2):
            optimizer.zero_grad()
            shared.loss(m, batch, "joint").backward()
            for head in (m.origin, m.mixed_share, m.human, m.ai, m.residual[2]):
                self.assertGreater(float(head.weight.grad.abs().sum()), 0)
            optimizer.step()
        self.assertGreater(float(m.residual[0].weight.grad.abs().sum()), 0)
        self.assertFalse(m.text_mean.requires_grad)
        self.assertFalse(m.text_scale.requires_grad)

    def test_portable_export_replays_and_has_no_upstream_parameters(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "init.json"
            shared.write(path, init_value())
            m = model()
            art = warm.artifact(m, {"ablation": "combined"}, {"contract_sha256": "synthetic"}, path,
                                1.4, {"human": ["a", "b"], "ai": ["x", "y"]}, {"human": .6, "ai": .7}, {})
            self.assertEqual(art["schema"], "slop_ninja_v6_residual_a_weights_v1")
            self.assertEqual(art["initialization_sha256"], shared.sha(path))
            self.assertEqual(len(art["weights"]), 12)
            self.assertEqual(art["architecture"]["raw_text_dimension"], 768)
            replay = model()
            state = {k: warm.tensor(v, v["shape"]) for k, v in art["weights"].items()}
            state.update(text_mean=torch.tensor(art["text_mean"]), text_scale=torch.tensor(art["text_scale"]))
            replay.load_state_dict(state)
            x = torch.randn(3, 2588)
            torch.testing.assert_close(m(x, 1.4)["origin"], replay(x, 1.4)["origin"], rtol=0, atol=0)

    def fixture(self, root):
        init = root / "init.json"
        shared.write(init, init_value())
        norm = {"fit": "train_query_only_equal_source_family_weight", "mean": [.1] * 768, "scale": [2.] * 768,
                **{k: False for k in ("calibration_used_for_fit", "development_used_for_fit", "reference_used_for_fit",
                                     "labels_used_for_fit", "final_test_opened")}}
        roots = {}
        for mode in ("text", "combined"):
            folder = root / mode
            folder.mkdir()
            contract = {"warm_start_schema": "slop_ninja_raw_v6_cfg_residual_input_v1", "input_dimension": 2588,
                        "ablation": mode, "human_profiles": ["a", "b"], "ai_profiles": ["x", "y"],
                        "embedding_normalization": norm}
            shared.write(folder / "contract.json", contract)
            splits, baseline = [], []
            m = model(mode)
            for i, part in enumerate(warm.PARTS):
                x = [i / 10.] * 768 + [0. if mode == "text" else .2] * 1820
                row = {"id": part, "source_group": part, "purpose": "reference" if part == "reference" else "query",
                       "frozen_split": "train" if part == "reference" else part,
                       "input": {"contract_sha256": mode, "x": x}}
                (folder / f"{part}.jsonl").write_text(json.dumps(row) + "\n")
                splits.append({"split": part, "rows": 1, "sha256": shared.sha(folder / f"{part}.jsonl")})
                with torch.no_grad():
                    p = (m.origin(torch.tensor([x[:768]])).double() / warm.BASE_TEMPERATURE).softmax(-1)[0].tolist()
                baseline.append({"id": part, "probabilities": p, "artifact_id": "synthetic-artifact", "text_sha256": part})
            (folder / "baseline-origin-reference.jsonl").write_text("".join(json.dumps(r) + "\n" for r in baseline))
            shared.write(folder / "manifest.json", {"contract_file_sha256": shared.sha(folder / "contract.json"),
                "contract_sha256": mode, "splits": splits, "final_test_opened": False, "test_used_for_selection": False,
                "baseline_origin_reference_sha256": shared.sha(folder / "baseline-origin-reference.jsonl")})
            roots[mode] = folder
        return init, roots

    def test_complete_gate_and_stale_peer_rejection(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(warm, "EXPECTED_ROWS", 4):
            init, roots = self.fixture(Path(tmp))
            receipt = Path(tmp) / "gate.json"
            warm.qualify(SimpleNamespace(**roots, init=init, out=receipt))
            binding = warm.numeric(roots["text"])[-1]
            warm.validate_gate(receipt, init, binding, "text")
            with (roots["combined"] / "reference.jsonl").open("a") as handle:
                handle.write("\n")
            with self.assertRaisesRegex(ValueError, "peer inputs changed"):
                warm.validate_gate(receipt, init, binding, "text")

    def test_actual_gate_requires_all_929(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, roots = self.fixture(Path(tmp))
            with self.assertRaisesRegex(ValueError, "929-row"):
                warm.numeric(roots["text"])

    def extension_fixture(self, root):
        init, original = self.fixture(root)
        extended = {}
        for mode, parent in original.items():
            pc = shared.read(parent / "contract.json")
            pc["experiment"] = {"schema": "original", "epochs": 250}
            (parent / "contract.json").write_text(json.dumps(pc))
            (parent / "population.json").write_text("{}\n")
            pm = shared.read(parent / "manifest.json")
            pm["contract_file_sha256"] = shared.sha(parent / "contract.json")
            (parent / "manifest.json").write_text(json.dumps(pm))
            folder = root / (mode + "-scale")
            shutil.copytree(parent, folder)
            ext = {"schema": warm.SCALE_SCHEMA, "original_rows": 4, "added_train_rows": 1, "total_rows": 5,
                   "parent_root": str(parent), "parent_manifest_sha256": shared.sha(parent / "manifest.json"),
                   "parent_contract_file_sha256": shared.sha(parent / "contract.json"), "parent_contract_sha256": mode,
                   "parent_population_sha256": shared.sha(parent / "population.json"), "protocol_sha256": "synthetic-protocol",
                   "original_numeric_values_and_targets_preserved": True, "normalization_fitted": False,
                   "profiles_changed": False, "final_test_opened": False,
                   "original_partitions": {p: {"rows": 1, "file_sha256": shared.sha(parent / f"{p}.jsonl")} for p in warm.PARTS}}
            (folder / "generation-protocol.md").write_text("Synthetic protocol only.\n")
            protocol = {"schema": warm.SCALE_SCHEMA, "selected_roots_manifest_sha256": "roots",
                        "generation_protocol_sha256": shared.sha(folder / "generation-protocol.md"),
                        "requested_new_roots": 1, "requested_new_generations": 0}
            admission = {"schema": "slop_ninja_a_supervised_dataset_v1", "complete": True,
                         "train_extension_only": True, "test_text_opened": False, "test_ids": [],
                         "source_manifest_sha256": "roots", "requested_human_records": 1,
                         "requested_generation_records": 0, "requested_records": 1, "records": 1,
                         "non_test_ids": ["added"], "split_assignments": {"new-family": "train"},
                         "generation_reports": [], "excluded": [], "accepted_before_exclusions": 1}
            shared.write(folder / "fit-protocol.json", protocol)
            shared.write(folder / "extension-dataset-manifest.json", admission)
            ext.update(selected_roots_manifest_sha256="roots", generation_protocol_sha256=protocol["generation_protocol_sha256"],
                       extension_dataset_manifest_sha256=shared.sha(folder / "extension-dataset-manifest.json"),
                       protocol_sha256=shared.sha(folder / "fit-protocol.json"))
            c = {**pc, "training_extension": ext, "experiment": {"schema": warm.SCALE_SCHEMA,
                 "epochs": 250, "seeds": [17, 29, 41], "controls": ["text", "combined"],
                 "protocol_sha256": ext["protocol_sha256"], "final_test_opened": False}}
            (folder / "contract.json").write_text(json.dumps(c))
            m = {**pm, "training_extension": ext, "contract_file_sha256": shared.sha(folder / "contract.json")}
            (folder / "training-population.json").write_text("{\"synthetic\": true}\n")
            m["dataset_manifest_file"] = "training-population.json"
            m["dataset_manifest_sha256"] = shared.sha(folder / "training-population.json")
            new = json.loads((parent / "train.jsonl").read_text())
            new.update(id="added", source_group="new-family")
            with (folder / "train.jsonl").open("a") as f: f.write(json.dumps(new) + "\n")
            m["splits"][0].update(rows=2, sha256=shared.sha(folder / "train.jsonl"))
            baseline = [json.loads(s) for s in (parent / "baseline-origin-reference.jsonl").read_text().splitlines()]
            baseline.append({**baseline[0], "id": "added"})
            (folder / "baseline-origin-reference.jsonl").write_text("".join(json.dumps(r) + "\n" for r in baseline))
            m["baseline_origin_reference_sha256"] = shared.sha(folder / "baseline-origin-reference.jsonl")
            (folder / "manifest.json").write_text(json.dumps(m))
            extended[mode] = folder
        return init, extended

    def test_train_extension_qualifies_every_added_row(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(warm, "EXPECTED_ROWS", 4):
            init, roots = self.extension_fixture(Path(tmp))
            gate = Path(tmp) / "gate.json"
            warm.qualify(SimpleNamespace(**roots, init=init, out=gate))
            receipt = shared.read(gate)
            self.assertEqual(receipt["rows"], 5)
            self.assertEqual(receipt["schema"], warm.SCALE_GATE_SCHEMA)
            binding = warm.numeric(roots["text"])[-1]
            warm.validate_gate(gate, init, binding, "text")

    def test_extension_rejects_changed_original_features_and_targets(self):
        for key in ("input", "extra_target"):
            with self.subTest(key=key), tempfile.TemporaryDirectory() as tmp, patch.object(warm, "EXPECTED_ROWS", 4):
                _, roots = self.extension_fixture(Path(tmp))
                folder = roots["text"]
                path = folder / "development.jsonl"
                row = json.loads(path.read_text())
                if key == "input": row["input"]["x"][0] += .1
                else: row[key] = .5
                path.write_text(json.dumps(row) + "\n")
                m = shared.read(folder / "manifest.json")
                m["splits"][1]["sha256"] = shared.sha(path)
                (folder / "manifest.json").write_text(json.dumps(m))
                with self.assertRaisesRegex(ValueError, "original numeric values or targets"):
                    warm.numeric(folder)

    def test_extension_rejects_changed_frozen_normalization_and_parent(self):
        for change in ("normalization", "parent"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as tmp, patch.object(warm, "EXPECTED_ROWS", 4):
                _, roots = self.extension_fixture(Path(tmp))
                folder = roots["text"]
                c = shared.read(folder / "contract.json")
                if change == "parent":
                    (Path(c["training_extension"]["parent_root"]) / "manifest.json").write_text("{}")
                else:
                    c["embedding_normalization"]["mean"][0] += .1
                    (folder / "contract.json").write_text(json.dumps(c))
                    m = shared.read(folder / "manifest.json")
                    m["contract_file_sha256"] = shared.sha(folder / "contract.json")
                    (folder / "manifest.json").write_text(json.dumps(m))
                with self.assertRaises(ValueError): warm.numeric(folder)

    def test_admission_rejects_substituted_roster_roots_and_unaccounted_tasks(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(warm, "EXPECTED_ROWS", 4):
            _, roots = self.extension_fixture(Path(tmp))
            folder = roots["text"]
            m, _, rows, _, _ = warm.numeric(folder)
            protocol = shared.read(folder / "fit-protocol.json")
            admission = shared.read(folder / "extension-dataset-manifest.json")
            added = rows["train"][-1:]
            for key, value in (("source_manifest_sha256", "different-roots"),
                               ("non_test_ids", ["substituted"]), ("accepted_before_exclusions", 2),
                               ("excluded", [{"id": "added"}])):
                with self.subTest(key=key), self.assertRaises(ValueError):
                    warm.validate_admission(protocol, {**admission, key: value}, m["training_extension"], added)
            with self.assertRaises(ValueError):
                warm.validate_admission({**protocol, "generation_protocol_sha256": "other"}, admission,
                                        m["training_extension"], added)

    def test_bad_baseline_or_nonzero_residual_blocks_equivalence(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(warm, "EXPECTED_ROWS", 4):
            _, roots = self.fixture(Path(tmp))
            _, _, rows, baseline, _ = warm.numeric(roots["text"])
            bad = model("text")
            with torch.no_grad():
                bad.residual[2].bias[0] = .1
            with self.assertRaisesRegex(ValueError, "not identity"):
                warm.equivalence(bad, rows, baseline, "synthetic-artifact", torch.device("cpu"))
            baseline[0]["probabilities"] = [1., 0., 0.]
            with self.assertRaisesRegex(ValueError, "probability mismatch"):
                warm.equivalence(model("text"), rows, baseline, "synthetic-artifact", torch.device("cpu"))

    def test_reference_family_and_partition_leakage_rejected(self):
        for kind in ("purpose", "family", "partition", "test_manifest"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as tmp:
                _, roots = self.fixture(Path(tmp))
                folder = roots["text"]
                manifest = shared.read(folder / "manifest.json")
                if kind == "test_manifest":
                    manifest["splits"][0]["split"] = "test"
                else:
                    row = json.loads((folder / "development.jsonl").read_text())
                    if kind == "purpose": row["purpose"] = "reference"
                    if kind == "family": row["source_group"] = "train"
                    if kind == "partition": row["frozen_split"] = "train"
                    (folder / "development.jsonl").write_text(json.dumps(row) + "\n")
                    manifest["splits"][1]["sha256"] = shared.sha(folder / "development.jsonl")
                (folder / "manifest.json").write_text(json.dumps(manifest))
                with self.assertRaises(ValueError):
                    warm.numeric(folder)

    def test_cli_rejects_budget_seed_and_test_overrides(self):
        argv = ["train", "--data", "/missing", "--init", "/missing", "--equivalence", "/missing", "--out", "/missing", "--seed", "17"]
        for extra in (["--epochs", "1"], ["--seed", "99"], ["--split", "test"]):
            with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                warm.main(argv + extra)

    def test_nonjoint_requires_protocol_before_opening_data_and_cli_defaults_joint(self):
        argv = ["train", "--data", "/missing", "--init", "/missing", "--equivalence", "/missing",
                "--out", "/missing", "--seed", "17"]
        with patch.object(warm, "train") as run:
            warm.main(argv)
            self.assertEqual(run.call_args.args[0].control, "joint")
            self.assertIsNone(run.call_args.args[0].control_protocol)
        for control in ("origin_only", "attribution_only"):
            with patch.object(warm, "numeric", side_effect=AssertionError("no inputs before admission")), \
                 self.assertRaisesRegex(ValueError, "requires --control-protocol"):
                warm.main(argv + ["--control", control])

    def test_control_gradients_masks_and_unused_heads_stay_frozen(self):
        batch = synthetic_batch()
        for control in ("origin_only", "attribution_only"):
            m = model()
            inherited = {k: v.clone() for k, v in m.origin.state_dict().items()}
            _, used = warm.adaptation_supervision(m, batch, control)
            self.assertEqual(used["share"], 0)
            self.assertEqual(used["mixed_fractional_share"], 0)
            for k, v in m.origin.state_dict().items():
                self.assertTrue(torch.equal(v, inherited[k]))
            optimizer = torch.optim.AdamW(m.parameters(), lr=.001, weight_decay=.01)
            frozen = {k: v.clone() for k, v in m.named_parameters() if not v.requires_grad}
            for step in range(2):
                optimizer.zero_grad(set_to_none=True)
                shared.loss(m, batch, control).backward()
                active = (m.origin,) if control == "origin_only" else (m.human, m.ai)
                for head in (*active, m.residual[2]):
                    self.assertGreater(float(head.weight.grad.abs().sum()), 0)
                for name, parameter in m.named_parameters():
                    if name in frozen:
                        self.assertIsNone(parameter.grad)
                torch.nn.utils.clip_grad_norm_(m.parameters(), 5.)
                optimizer.step()
            for name, parameter in m.named_parameters():
                if name in frozen:
                    self.assertTrue(torch.equal(parameter, frozen[name]))
            self.assertGreater(float(m.residual[0].weight.grad.abs().sum()), 0)
            out = m(batch["x"])
            self.assertTrue(torch.equal(out["conditional_mixed_share"], torch.full((6,), .5)))
            # Unavailable role labels are excluded, not treated as a profile index.
            logits = out["human_logits"]
            if control == "attribution_only":
                logits.retain_grad()
                sum(shared.conditional_ce(out, batch)).backward()
                self.assertTrue(torch.equal(logits.grad[batch["human"] < 0], torch.zeros_like(logits.grad[batch["human"] < 0])))

    def test_loss_and_selection_ignore_inactive_targets(self):
        b = synthetic_batch()
        for control in ("origin_only", "attribution_only"):
            m = model()
            warm.adaptation_supervision(m, b, control)
            changed = {**b, "share": 1 - b["share"], "share_mask": ~b["share_mask"]}
            if control == "origin_only":
                changed.update(human=torch.full((6,), -1), ai=torch.full((6,), -1))
                out = m(b["x"])
                expected = shared.masked_mean(torch.nn.functional.cross_entropy(out["origin_logits"], b["y"], reduction="none") +
                    (out["origin"][:, 1:].sum(-1) - (b["y"] != 0).float()).square(), b["weight"], b["y"] >= 0)
            else:
                changed["y"] = torch.full((6,), -1)
                expected = sum(shared.conditional_ce(m(b["x"]), b))
            torch.testing.assert_close(shared.loss(m, b, control), expected, rtol=0, atol=0)
            torch.testing.assert_close(shared.loss(m, b, control), shared.loss(m, changed, control), rtol=0, atol=0)
            self.assertEqual(shared.criterion(m, b, control=control), shared.criterion(m, changed, control=control))
            self.assertEqual(shared.task_weights(control)["share_mse"], 0)
        b["human"] = torch.full((6,), -1)
        b["ai"] = torch.full((6,), -1)
        with self.assertRaisesRegex(ValueError, "no training labels"):
            warm.adaptation_supervision(model(), b, "attribution_only")

    def test_attribution_control_never_calibrates_origin_and_untrained_fallbacks(self):
        b, profiles = synthetic_batch(), {"human": ["a", "b"], "ai": ["x", "y"]}
        for control in ("origin_only", "attribution_only"):
            m = model()
            _, used = warm.adaptation_supervision(m, b, control)
            with patch.object(shared, "criterion", side_effect=AssertionError("origin criterion forbidden")):
                # origin_only at epoch 0, attribution_only even after adaptation.
                temperature, calibrated, thresholds, reports = warm.calibrate(m, b, profiles, used,
                    5 if control == "attribution_only" else 0)
            self.assertEqual(temperature, 1.)
            self.assertFalse(calibrated)
            if control == "origin_only":
                for role in shared.ROLES:
                    self.assertEqual(reports[role]["reason"], "untrained_head")
                inputs = [{"contract_sha256": "a" * 64, "x": (b["x"][0] * 100).tolist()}]
                result = shared.predictions(m, inputs, 1., "a" * 64, profiles, thresholds, torch.device("cpu"))[0]
                for role in shared.ROLES:
                    self.assertEqual(result["attribution"][role]["profile"], "other_" + role)
                    self.assertIn("involvement_probability", result["attribution"][role])
            warm.finalize_adaptation(m, control, 0)
            for role in shared.ROLES:
                self.assertEqual(float(getattr(m, role).weight.abs().sum()), 0)
            status = warm.training_status(control, used, 5)
            self.assertEqual(status["share_interpolation"], "untrained_fixed_half")
            if control == "attribution_only":
                self.assertEqual(status["origin"], "frozen_V6_head_on_adapted_encoding")

    def test_control_protocol_pins_capacity_optimizer_selection_and_inputs(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            init, gate, path = root / "init.json", root / "gate.json", root / "control.json"
            shared.write(init, init_value())
            shared.write(gate, {"synthetic": True})
            contract = {"ablation": "text", "human_profiles": ["a", "b"], "ai_profiles": ["x", "y"]}
            binding = {"manifest_sha256": "a" * 64, "contract_sha256": "b" * 64}
            p = {"schema": warm.CONTROL_SCHEMA, "status": "frozen", "controls": ["origin_only", "attribution_only"],
                 "seeds": [17, 29, 41], "epochs": 250,
                 "optimizer": {"name": "AdamW", "learning_rate": .001, "weight_decay": .01, "gradient_clip_norm": 5},
                 "architecture": {"dimension": 2588, "raw_text_dimension": 768, "hidden": 64, "embedding": 768,
                                  "human_profiles": 2, "ai_profiles": 2},
                 "query_weighting": "family_origin_balanced",
                 "selection": {"origin_only": "development_involvement_brier", "attribution_only": "development_conditional_supervised_ce"},
                 "calibration": {"origin_only": "CAL_origin_temperature_only", "attribution_only": "CAL_named_thresholds_only"},
                 "adaptation_scope": "frozen_V6_origin_supervised_initialization_not_origin_free_pretraining",
                 "trainable_parameters": "residual_and_selected_heads", "share_interpolation": "untrained_fixed_half",
                 "upstream_encoder_frozen": True, "selected_by": "joint_Development_only",
                 "final_test_opened": False, "control_based_test_selection": False, "input_mode": "text",
                 "data_manifest_sha256": "a" * 64, "contract_sha256": "b" * 64,
                 "initialization_sha256": shared.sha(init), "equivalence_receipt_sha256": shared.sha(gate)}
            path.write_text(json.dumps(p) + "\n")
            pin = warm.control_protocol(path, "origin_only", contract, binding, init, gate)
            self.assertEqual(pin["sha256"], shared.sha(path))
            self.assertEqual(pin["bytes"], path.read_bytes())
            for key, value in (("epochs", 249), ("seeds", [17]), ("input_mode", "combined"),
                               ("architecture", {**p["architecture"], "hidden": 32}),
                               ("optimizer", {**p["optimizer"], "weight_decay": 0}),
                               ("selection", {**p["selection"], "attribution_only": "development_involvement_brier"}),
                               ("query_weighting", "family"), ("data_manifest_sha256", "c" * 64),
                               ("initialization_sha256", "d" * 64), ("equivalence_receipt_sha256", "e" * 64),
                               ("status", "template"), ("control_based_test_selection", True)):
                path.write_text(json.dumps({**p, key: value}))
                with self.subTest(key=key), self.assertRaises(ValueError):
                    warm.control_protocol(path, "attribution_only", contract, binding, init, gate)
            self.assertIsNone(warm.control_protocol(None, "joint", contract, binding, init, gate))
            with self.assertRaises(ValueError):
                warm.control_protocol(path, "joint", contract, binding, init, gate)

    @unittest.skipUnless(os.environ.get("WARM_JOINT_REFERENCE"), "frozen joint source supplied by capped job")
    def test_joint_matches_frozen_trainer(self):
        spec = importlib.util.spec_from_file_location("frozen_joint", os.environ["WARM_JOINT_REFERENCE"])
        original = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(original)
        b = synthetic_batch()
        torch.manual_seed(41)
        old = original.WarmStartA(init_value(), [.1] * 768, [2.] * 768, 2, 2, "combined")
        torch.manual_seed(41)
        new = model()
        before = {k: v.clone() for k, v in new.state_dict().items()}
        _, used = warm.adaptation_supervision(new, b, "joint")
        warm.finalize_adaptation(new, "joint", 0)
        for name, value in new.state_dict().items():
            self.assertTrue(torch.equal(value, before[name]))
            self.assertTrue(torch.equal(value, old.state_dict()[name]))
        optimizers = [torch.optim.AdamW(m.parameters(), lr=.001, weight_decay=.01) for m in (old, new)]
        for _ in range(2):
            for m, opt in zip((old, new), optimizers):
                opt.zero_grad(set_to_none=True)
                shared.loss(m, b, "joint").backward()
                torch.nn.utils.clip_grad_norm_(m.parameters(), 5.)
                opt.step()
        for name, value in new.state_dict().items():
            self.assertTrue(torch.equal(value, old.state_dict()[name]), name)
        self.assertEqual(shared.criterion(old, b), shared.criterion(new, b, control="joint"))
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "init.json"
            shared.write(path, init_value())
            args = ({"ablation": "combined"}, {"contract_sha256": "a" * 64}, path, 1.,
                    {"human": ["a", "b"], "ai": ["x", "y"]}, {"human": 1., "ai": 1.}, used)
            self.assertEqual(original.artifact(old, *args), warm.artifact(new, *args))

    @unittest.skipUnless(os.environ.get("WARM_RUST_REPLAY_BIN"), "Rust replay binary supplied by capped job")
    def test_nonjoint_export_replays_in_rust(self):
        b, profiles = synthetic_batch(), {"human": ["a", "b"], "ai": ["x", "y"]}
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            init = root / "init.json"
            shared.write(init, init_value())
            for control in ("origin_only", "attribution_only"):
                m = model()
                _, used = warm.adaptation_supervision(m, b, control)
                optimizer = torch.optim.AdamW(m.parameters(), lr=.001, weight_decay=.01)
                for _ in range(2):
                    optimizer.zero_grad(set_to_none=True)
                    shared.loss(m, b, control).backward()
                    torch.nn.utils.clip_grad_norm_(m.parameters(), 5.)
                    optimizer.step()
                thresholds = {r: 1. if not used[r] else 0. for r in shared.ROLES}
                temperature = 1.3 if control == "origin_only" else 1.
                art = warm.artifact(m, {"ablation": "combined"}, {"contract_sha256": "a" * 64}, init,
                    temperature, profiles, thresholds, used, control, "b" * 64)
                inputs = [{"contract_sha256": "a" * 64, "x": x.tolist()} for x in b["x"][:2]]
                predictions = shared.predictions(m, inputs, temperature, "a" * 64, profiles, thresholds, torch.device("cpu"))
                weights, rows, refs, result = [root / (control + suffix) for suffix in ("-weights.json", "-rows.jsonl", "-refs.json", "-replay.json")]
                shared.write(weights, art)
                rows.write_text("".join(json.dumps({"id": str(i), "input": x}) + "\n" for i, x in enumerate(inputs)))
                shared.write(refs, [{"id": str(i), "prediction": p} for i, p in enumerate(predictions)])
                subprocess.run([os.environ["WARM_RUST_REPLAY_BIN"], str(weights), str(rows), str(refs), str(result)],
                               check=True, capture_output=True, text=True)
                replay = shared.read(result)
                self.assertLess(replay["max_absolute_delta_percentage_points"], .0001)

    def test_initializer_rejects_wrong_temperature_and_unqualified_weights(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "init.json"
            for key, value in (("base_temperature", 1.), ("weight_file_sha256", "wrong"),
                               ("original_dependency_and_reference_checks_passed", False)):
                init = init_value()
                init[key] = value
                path.write_text(json.dumps(init))
                with self.assertRaises(ValueError):
                    warm.initializer(path)

    def test_cuda_cap_uses_current_device_without_starting_cuda(self):
        with patch.dict(os.environ, {"CUBLAS_WORKSPACE_CONFIG": ":4096:8"}), \
             patch.object(torch.cuda, "is_available", return_value=True), \
             patch.object(torch.cuda, "set_per_process_memory_fraction") as cap, \
             patch.object(torch.cuda, "reset_peak_memory_stats") as reset, \
             patch.object(torch.cuda, "_lazy_init", side_effect=AssertionError("synthetic test must not start CUDA")):
            warm.configure(torch.device("cuda"), 17)
            cap.assert_called_once_with(.20)
            reset.assert_called_once_with()

    def mixture_fixture(self, root):
        init = root / "init.json"
        shared.write(init, init_value())
        protocol = {"schema": warm.MIXTURE_SCHEMA, "original_cohort_mass": .5,
                    "within_cohort": "family_origin_balanced", "apply_to": "train_only",
                    "epochs": 250, "seeds": [17, 29, 41], "controls": ["text", "combined"],
                    "final_test_opened": False, "numeric_manifest_sha256": {"text": "numeric"},
                    "parent_manifest_sha256": {"text": "parent"}, "initialization_sha256": shared.sha(init)}
        path = root / "protocol.json"
        shared.write(path, protocol)
        rows = [{"id": str(i), "source_group": group, "purpose": "query", "frozen_split": "train",
                 "targets": {"origin": origin, "share": float(origin != 0),
                             "human": 0 if i < 3 else None, "ai": 0 if origin else None}}
                for i, (group, origin) in enumerate([("a", 0), ("a", 1), ("b", 0),
                                                    ("c", 0), ("c", 1), ("c", 1), ("c", 1)])]
        manifest = {"training_extension": {"parent_manifest_sha256": "parent", "added_train_rows": 4,
                                          "original_partitions": {"train": {"rows": 3}}}}
        binding = {"train_extension": True, "manifest_sha256": "numeric"}
        return path, manifest, binding, rows, init

    def test_mixture_balances_cohorts_families_origins_and_supervised_masks(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, manifest, binding, rows, init = self.mixture_fixture(Path(tmp))
            original = json.dumps(rows, sort_keys=True)
            weights, receipt = warm.cohort_mixture(path, manifest, binding, "text", rows, init)
            for actual, expected in zip(weights, [.125, .125, .25, .25, 1/12, 1/12, 1/12]):
                self.assertAlmostEqual(actual, expected)
            self.assertEqual(json.dumps(rows, sort_keys=True), original)
            self.assertAlmostEqual(receipt["original_mass_under_uniform_families"], 2/3)
            self.assertEqual(receipt["masked_task_cohort_mass"]["human"], {"original": 1., "added": 0.})
            self.assertAlmostEqual(receipt["masked_task_cohort_mass"]["origin"]["original"], .5)
            self.assertAlmostEqual(receipt["masked_task_cohort_mass"]["ai"]["original"], 1/3)

    def test_mixture_duplicate_descendants_do_not_gain_family_or_origin_mass(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, manifest, binding, rows, init = self.mixture_fixture(Path(tmp))
            weights, _ = warm.cohort_mixture(path, manifest, binding, "text", rows, init)
            rows.extend([{**rows[-1], "id": "extra1"}, {**rows[-1], "id": "extra2"}])
            manifest["training_extension"]["added_train_rows"] += 2
            new_weights, _ = warm.cohort_mixture(path, manifest, binding, "text", rows, init)
            self.assertEqual(weights[:4], new_weights[:4])
            self.assertAlmostEqual(sum(weights[4:]), sum(new_weights[4:]))

    def test_mixture_rejects_unbound_population_leakage_and_nontrain_rows(self):
        for defect in ("manifest", "parent", "family", "duplicate", "split", "reference", "mass"):
            with self.subTest(defect=defect), tempfile.TemporaryDirectory() as tmp:
                path, manifest, binding, rows, init = self.mixture_fixture(Path(tmp))
                if defect == "manifest": binding["manifest_sha256"] = "wrong"
                if defect == "parent": manifest["training_extension"]["parent_manifest_sha256"] = "wrong"
                if defect == "family": rows[-1]["source_group"] = "a"
                if defect == "duplicate": rows[-1]["id"] = rows[0]["id"]
                if defect == "split": rows[-1]["frozen_split"] = "development"
                if defect == "reference": rows[-1]["purpose"] = "reference"
                if defect == "mass":
                    p = shared.read(path)
                    p["original_cohort_mass"] = .75
                    path.write_text(json.dumps(p))
                with self.assertRaises(ValueError):
                    warm.cohort_mixture(path, manifest, binding, "text", rows, init)

    def test_unqualified_control_and_mixture_combination_is_rejected_before_inputs(self):
        args = SimpleNamespace(seed=17, control="origin_only", control_protocol="unused",
                               mixture_protocol="unused")
        with patch.object(warm, "numeric", side_effect=AssertionError("no input reads")), \
             self.assertRaisesRegex(ValueError, "separately qualified protocol"):
            warm.train(args)


if __name__ == "__main__":
    unittest.main()

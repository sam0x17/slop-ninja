import unittest
from contextlib import ExitStack, redirect_stderr
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
from unittest.mock import patch
import torch
from shared_a import (SharedA, predictions, loss, criterion, calibrate_named, dataset,
                      train, sha, predict_split, frozen_qualification, main, task_weights, query_weights)


class SharedATests(unittest.TestCase):
    def test_balanced_origin_weight_survives_duplicate_descendants(self):
        def row(group, origin):
            return {"source_group": group, "targets": {"origin": origin}}
        rows = [row("a",0), row("a",1)] + [row("a",2)] * 12 + [row("b",0),row("b",1),row("b",2)]
        weights = query_weights(rows,"family_origin_balanced")
        for group in ("a","b"):
            for origin in range(3):
                mass = sum(w for r,w in zip(rows,weights) if r["source_group"]==group and r["targets"]["origin"]==origin)
                self.assertAlmostEqual(mass,1/6)
        self.assertAlmostEqual(sum(weights),1)
        old = query_weights(rows,"family")
        self.assertAlmostEqual(sum(w for r,w in zip(rows,old) if r["source_group"]=="a"),.5)
        self.assertGreater(sum(w for r,w in zip(rows,old) if r["targets"]["origin"]==2),1/3)

    def test_joint_task_weights_prioritize_origin_without_scaling_share(self):
        torch.manual_seed(13)
        model = SharedA(2, 2, 2, hidden=3, embedding=2)
        batch = {"x": torch.randn(3, 2), "weight": torch.tensor([.2, .3, .5]),
                 "y": torch.tensor([0, 1, 2]), "share_mask": torch.tensor([False, False, True]),
                 "share": torch.tensor([0., 0., .4]), "human": torch.tensor([0, -1, -1]),
                 "ai": torch.tensor([-1, 1, 0])}
        out = model(batch["x"])
        human_ce = torch.nn.functional.cross_entropy(out["human_logits"][:1], batch["human"][:1])
        ai_ce = (torch.nn.functional.cross_entropy(out["ai_logits"][1:], batch["ai"][1:], reduction="none")
                 * torch.tensor([.3/.8, .5/.8])).sum()
        attribution = loss(model, batch, "attribution_only")
        torch.testing.assert_close(attribution, human_ce + ai_ce)
        joint = loss(model, batch, "joint")
        expected = loss(model, batch, "origin_only") + (out["share"][2] - .4).square() + .25 * (human_ce + ai_ce)
        torch.testing.assert_close(joint, expected)
        for head in (model.human, model.ai):
            joint_grad = torch.autograd.grad(joint, head.weight, retain_graph=True)[0]
            single_grad = torch.autograd.grad(attribution, head.weight, retain_graph=True)[0]
            torch.testing.assert_close(joint_grad, .25 * single_grad)
        self.assertEqual(task_weights("joint"), {"origin_ce": 1., "involvement_brier": 1., "share_mse": 1.,
                                                "human_conditional_ce": .25, "ai_conditional_ce": .25})

    def test_all_outputs_use_shared_encoding_and_roles_remain_visible(self):
        model = SharedA(4, 2, 2, hidden=3, embedding=2)
        with torch.no_grad():
            model.origin.weight.zero_()
            model.origin.bias.copy_(torch.tensor([100., -100., -100.]))
            model.ai.weight.zero_()
            model.ai.bias.copy_(torch.tensor([10., -10.]))
        result = predictions(model, [{"contract_sha256": "test", "x": [0.] * 4}], 1., "test",
                             {"human": ["writer1", "writer2"], "ai": ["generator1", "generator2"]},
                             {"human": .9, "ai": .9}, torch.device("cpu"))[0]
        self.assertEqual(result["attribution"]["ai"]["profile"], "generator1")
        self.assertEqual(result["attribution"]["ai"]["involvement_probability"], 0.)
        self.assertIn("human", result["attribution"])

    def test_coherent_percentages_at_temperature_extremes(self):
        torch.manual_seed(3)
        model = SharedA(4)
        inputs = [{"contract_sha256": "test", "x": row} for row in torch.randn(50, 4).tolist()]
        for temperature in (.01, 1., 100.):
            for result in predictions(model, inputs, temperature, "test", {"human": [], "ai": []},
                                      {"human": 1., "ai": 1.}, torch.device("cpu")):
                self.assertAlmostEqual(sum(result["origin"].values()), 1.)
                self.assertGreaterEqual(result["ai_share_percent"], 100 * result["origin"]["model_only"])
                self.assertLessEqual(result["ai_share_percent"], result["ai_confidence_percent"])
                self.assertLessEqual(result["ai_confidence_percent"], 100.000000000001)
                self.assertEqual(result["attribution"]["human"]["profile"], "other_human")

    def test_missing_supervision_masks_gradients(self):
        model = SharedA(4)
        batch = {"x": torch.randn(3, 4), "weight": torch.ones(3) / 3,
                 "y": torch.tensor([0, 1, 2]), "share_mask": torch.zeros(3, dtype=torch.bool),
                 "share": torch.zeros(3), "human": torch.full((3,), -1), "ai": torch.full((3,), -1)}
        loss(model, batch, "joint").backward()
        self.assertIsNotNone(model.encoder[0].weight.grad)
        self.assertIsNone(model.human.weight.grad)
        self.assertIsNone(model.ai.weight.grad)
        self.assertIsNone(model.mixed_share.weight.grad)

    def test_numeric_request_rejects_metadata_and_wrong_contract(self):
        model = SharedA(2)
        for row in ({"contract_sha256": "test", "x": [0., 0.], "text": "forbidden"},
                    {"contract_sha256": "wrong", "x": [0., 0.]}):
            with self.assertRaises(ValueError):
                predictions(model, [row], 1., "test", {"human": [], "ai": []},
                            {"human": 1., "ai": 1.}, torch.device("cpu"))

    def test_nullable_origin_and_conditional_selection_ignore_origin_head(self):
        model = SharedA(4, 2, 2)
        batch = {"x": torch.randn(3, 4), "weight": torch.ones(3) / 3,
                 "y": torch.full((3,), -1), "share_mask": torch.zeros(3, dtype=torch.bool),
                 "share": torch.zeros(3), "human": torch.tensor([0, 1, -1]), "ai": torch.full((3,), -1)}
        before = criterion(model, batch, control="attribution_only")
        with torch.no_grad():
            model.origin.bias.copy_(torch.tensor([100., -100., -100.]))
        self.assertEqual(before, criterion(model, batch, control="attribution_only"))
        loss(model, batch, "joint").backward()
        self.assertIsNone(model.origin.weight.grad)
        self.assertIsNotNone(model.human.weight.grad)
        with self.assertRaises(ValueError):
            criterion(model, batch, control="joint")

    def test_threshold_requires_withheld_evidence_and_ignores_absent_roles(self):
        labels = [{"present": True, "known": True, "identity": "a"},
                  {"present": True, "known": False, "identity": "c"},
                  {"present": False, "known": False, "identity": "d"}]
        threshold, report = calibrate_named([("a", .9), ("a", .6), ("a", 1.)], labels, ["x", "y", "z"])
        self.assertGreater(threshold, .6)
        self.assertLessEqual(threshold, .9)
        self.assertEqual(report["objective_value"], 1.)
        self.assertEqual(report["withheld_rows"], 1)
        _, report = calibrate_named([("a", .9)], labels[:1], ["x"])
        self.assertFalse(report["qualified"])
        _, report = calibrate_named([("a", .9), ("a", .6)], labels[:2], ["x", "y"], False)
        self.assertEqual(report["reason"], "untrained_head")

    def test_encodings_are_optional_and_do_not_change_predictions(self):
        model = SharedA(2)
        args = (model, [{"contract_sha256": "test", "x": [0., 1.]}], 1., "test",
                {"human": [], "ai": []}, {"human": 1., "ai": 1.}, torch.device("cpu"))
        original = predictions(*args)[0]
        exported = predictions(*args, export_encodings=True)[0]
        self.assertEqual(len(exported.pop("encoding")), 32)
        self.assertEqual(original, exported)

    def test_repeated_seed_controls_and_reference_exclusion(self):
        # Synthetic numeric inputs only; Test is intentionally invalid and must never open.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            contract = {"input_dimension": 2, "human_profiles": ["a", "b"], "ai_profiles": []}
            (root / "contract.json").write_text(json.dumps(contract))
            bindings = []
            for split in ("train", "development", "calibration"):
                rows = []
                for i in range(3):
                    known = i < 2
                    rows.append({"id": f"{split}-{i}", "source_group": f"{split}-work-{i}", "purpose": "query",
                        "input": {"contract_sha256": "a" * 64, "x": [float(i), 1.]},
                        "targets": {"origin": 0 if known else None, "share": 0. if known else None,
                                    "human": i if known else None, "ai": None},
                        "roles": {"human": {"present": True, "identity": ["a", "b", "c"][i], "known": known},
                                  "ai": {"present": False, "identity": None, "known": False}}})
                path = root / f"{split}.jsonl"
                path.write_text("\n".join(json.dumps(r) for r in rows) + "\n")
                bindings.append({"split": split, "sha256": sha(path)})
            manifest = {"splits": bindings, "contract_sha256": "a" * 64,
                        "contract_file_sha256": sha(root / "contract.json"), "clean_holdout": False}
            (root / "manifest.json").write_text(json.dumps(manifest))
            (root / "test.jsonl").write_text("must not read Test")
            reference = {"id": "ref", "source_group": "independent-work", "purpose": "reference",
                         "input": {"contract_sha256": "a" * 64, "x": [10000., -10000.]}}
            (root / "reference.jsonl").write_text(json.dumps(reference) + "\n")
            hashes = {}
            for control in ("origin_only", "joint", "attribution_only"):
                for repeat in range(2):
                    out = root / f"{control}-{repeat}"
                    train(SimpleNamespace(data=root, out=out, device="cpu", seed=17, epochs=2,
                                          control=control, export_encodings=bool(repeat)))
                    weights = json.loads((out / "weights.json").read_text())
                    fit = json.loads((out / "fit.json").read_text())
                    self.assertEqual(fit["task_weights"], task_weights(control))
                    self.assertFalse(fit["final_test_opened"])
                    self.assertEqual(weights["schema"], "slop_ninja_shared_a_weights_v1")
                    self.assertEqual(set(weights), {"schema", "architecture", "operations", "contract_sha256",
                        "temperature", "profiles", "thresholds", "supervision", "origin_order", "control", "weights"})
                    self.assertEqual(fit["development_selection"]["metric"], "conditional_supervised_ce"
                                     if control == "attribution_only" else "involvement_brier")
                    if control == "origin_only":
                        self.assertEqual(fit["training_status"]["human"], "untrained")
                        self.assertEqual(fit["training_status"]["share_interpolation"], "untrained_fixed_half")
                    if repeat:
                        self.assertEqual(hashes[control], sha(out / "weights.json"))
                        self.assertTrue((out / "reference-encodings.json").exists())
                    else:
                        hashes[control] = sha(out / "weights.json")
            # Guard input roles and purpose before any fitting can occur.
            rows[0]["purpose"] = "reference"
            path = root / "calibration.jsonl"
            path.write_text("\n".join(json.dumps(r) for r in rows))
            manifest["splits"][-1]["sha256"] = sha(path)
            with self.assertRaises(ValueError):
                dataset(root, "calibration", manifest, torch.device("cpu"), {"human": ["a", "b"], "ai": []})


class FrozenSplitTests(unittest.TestCase):
    def fixture(self, root, control="origin_only"):
        data, fit = root / "data", root / "fit"
        data.mkdir()
        fit.mkdir()
        architecture = {"dimension": 2, "human_profiles": 2, "ai_profiles": 0, "hidden": 3, "embedding": 2}
        model = SharedA(**architecture).eval()
        # Legacy v1: supervision counts may describe available labels for disabled heads.
        artifact = {"schema": "slop_ninja_shared_a_weights_v1", "architecture": architecture,
                    "operations": ["linear", "tanh", "softmax", "sigmoid"], "origin_order": ["human_only", "model_only", "mixed"],
                    "contract_sha256": "a" * 64, "temperature": 1.7, "control": control,
                    "profiles": {"human": ["a", "b"], "ai": []}, "thresholds": {"human": .8, "ai": 1.},
                    "supervision": {"origin": 10, "human": 10, "ai": 0, "share": 10, "mixed_fractional_share": 10},
                    "weights": {k: {"shape": list(v.shape), "values": v.flatten().tolist()} for k, v in model.state_dict().items()}}
        (fit / "weights.json").write_text(json.dumps(artifact))
        contract = {"input_dimension": 2, "human_profiles": ["a", "b"], "ai_profiles": []}
        (data / "contract.json").write_text(json.dumps(contract))
        rows = [{"id": "synthetic-only", "purpose": "query", "frozen_split": "test",
                 "input": {"contract_sha256": "a" * 64, "x": [.3, -.2]},
                 "targets": "unused_by_inference", "roles": "unused_by_inference"}]
        (data / "test.jsonl").write_text(json.dumps(rows[0]) + "\n")
        manifest = {"contract_sha256": "a" * 64, "contract_file_sha256": sha(data / "contract.json"),
                    "splits": [{"split": "test", "rows": 1, "sha256": sha(data / "test.jsonl")}]}
        (data / "manifest.json").write_text(json.dumps(manifest))
        args = SimpleNamespace(data=data, artifact=fit / "weights.json", split="test", out=fit / "test-predictions.json")
        return args, artifact, manifest, rows, model

    def test_cli_frozen_inference_never_fits_and_preserves_old_weights(self):
        with tempfile.TemporaryDirectory() as tmp:
            args, artifact, manifest, rows, model = self.fixture(Path(tmp))
            before = {path: path.read_bytes() for path in Path(tmp).rglob("*") if path.is_file()}
            expected = predictions(model, [r["input"] for r in rows], artifact["temperature"], artifact["contract_sha256"],
                                   artifact["profiles"], artifact["thresholds"], torch.device("cpu"), True)
            with ExitStack() as stack:
                for name in ("train", "dataset", "loss", "criterion", "calibrate_named", "torch.optim.AdamW", "torch.cuda.is_available"):
                    stack.enter_context(patch("shared_a." + name, side_effect=AssertionError("fitting/GPU forbidden")))
                main(["predict-split", "--data", str(args.data), "--artifact", str(args.artifact), "--split", "test", "--out", str(args.out)])
            result = json.loads(args.out.read_text())
            self.assertEqual(result, [{"id": rows[0]["id"], "prediction": expected[0]}])
            provenance = json.loads(Path(str(args.out) + ".provenance.json").read_text())
            for key, file in (("manifest_sha256", args.data / "manifest.json"), ("contract_file_sha256", args.data / "contract.json"),
                              ("test_sha256", args.data / "test.jsonl"), ("weights_sha256", args.artifact), ("prediction_file_sha256", args.out)):
                self.assertEqual(provenance[key], sha(file))
            self.assertFalse(provenance["fitting_performed"])
            self.assertFalse(provenance["calibration_fitted"])
            self.assertFalse(provenance["normalization_fitted"])
            self.assertFalse(provenance["test_used_for_selection"])
            self.assertEqual(provenance["qualification"]["attribution"]["human"], "untrained")
            self.assertEqual(provenance["qualification"]["share_interpolation"], "untrained")
            self.assertEqual(provenance["encoding_schema"], "slop_ninja_shared_a_encoding_v1")
            for path, contents in before.items():
                self.assertEqual(path.read_bytes(), contents)
            with self.assertRaisesRegex(ValueError, "exists"):
                predict_split(args)

    def test_control_qualification_never_promotes_untrained_origin_or_attribution(self):
        with tempfile.TemporaryDirectory() as tmp:
            args, artifact, _, _, _ = self.fixture(Path(tmp))
            fit = {"weights_sha256": sha(args.artifact), "final_test_opened": False,
                   "calibration_temperature": artifact["temperature"], "selected_epoch": 2}
            q = frozen_qualification(artifact, fit)
            self.assertEqual(q["origin"]["calibration"], "frozen_CAL_temperature")
            self.assertEqual(q["attribution"]["human"], "untrained")
            artifact["control"] = "attribution_only"
            q = frozen_qualification(artifact, fit)
            self.assertEqual(q["origin"]["training_status"], "untrained")
            self.assertEqual(q["origin"]["calibration"], "not_documented_or_untrained")
            self.assertEqual(q["share_interpolation"], "untrained")
            fit["selected_epoch"] = 0
            self.assertEqual(frozen_qualification(artifact, fit)["attribution"]["human"], "untrained")

    def test_frozen_bindings_reject_changes_before_opening_test(self):
        for change in ("contract", "artifact", "missing_binding", "duplicate_binding", "fit_weights", "fit_test_access", "split"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as tmp:
                args, artifact, manifest, _, _ = self.fixture(Path(tmp))
                (args.data / "test.jsonl").unlink()  # Any early Test access raises FileNotFoundError, not ValueError.
                if change == "contract":
                    (args.data / "contract.json").write_text("{}")
                elif change == "artifact":
                    artifact["contract_sha256"] = "b" * 64
                    args.artifact.write_text(json.dumps(artifact))
                elif change in ("missing_binding", "duplicate_binding"):
                    manifest["splits"] = [] if change == "missing_binding" else manifest["splits"] * 2
                    (args.data / "manifest.json").write_text(json.dumps(manifest))
                elif change.startswith("fit_"):
                    (args.artifact.parent / "fit.json").write_text(json.dumps({
                        "weights_sha256": "wrong" if change == "fit_weights" else sha(args.artifact),
                        "final_test_opened": change == "fit_test_access"}))
                else:
                    args.split = "train"
                with self.assertRaises(ValueError):
                    predict_split(args)
                self.assertFalse(args.out.exists())

    def test_test_hash_purpose_and_numeric_input_are_enforced(self):
        for change in ("hash", "reference", "wrong_split", "prose_input", "string_coordinate", "nan", "duplicate", "dimension", "row_contract"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as tmp:
                args, _, manifest, rows, _ = self.fixture(Path(tmp))
                if change == "reference":
                    rows[0]["purpose"] = "reference"
                elif change == "wrong_split":
                    rows[0]["frozen_split"] = "train"
                elif change == "prose_input":
                    rows[0]["input"]["text"] = "synthetic prohibited input"
                elif change == "string_coordinate":
                    rows[0]["input"]["x"][0] = "0.3"
                elif change == "nan":
                    rows[0]["input"]["x"][0] = float("nan")
                elif change == "dimension":
                    rows[0]["input"]["x"].append(0.)
                elif change == "row_contract":
                    rows[0]["input"]["contract_sha256"] = "wrong"
                elif change == "duplicate":
                    rows *= 2
                (args.data / "test.jsonl").write_text("\n".join(json.dumps(r) for r in rows) + "\n\n")
                # Blank lines are invalid too; use exactly the changed rows except in the hash case.
                if change != "hash":
                    (args.data / "test.jsonl").write_text("\n".join(json.dumps(r) for r in rows) + "\n")
                    manifest["splits"][0].update(sha256=sha(args.data / "test.jsonl"), rows=len(rows))
                    (args.data / "manifest.json").write_text(json.dumps(manifest))
                with self.assertRaises(ValueError):
                    predict_split(args)
                self.assertFalse(args.out.exists())

    def test_predict_split_cli_refuses_fitting_options_without_reading_data(self):
        argv = ["predict-split", "--data", "/nonexistent", "--artifact", "/nonexistent", "--split", "test", "--out", "/nonexistent"]
        for option in ("--epochs", "--seed", "--control", "--device"):
            with self.subTest(option=option), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                main(argv + [option, "1"])


if __name__ == "__main__":
    unittest.main()

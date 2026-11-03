"""Audit CLI contract checks with synthetic loaded rows and no ML imports."""

import contextlib
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import Mock, call, patch


class AuditTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.checkpoint = self.directory / "checkpoint"
        self.checkpoint.mkdir()
        self.output = self.directory / "audit.json"
        self.tokenizer_hashes = {}
        for name in ("tokenizer.json", "tokenizer_config.json", "special_tokens_map.json", "config.json"):
            content = json.dumps({"synthetic_asset": name}).encode()
            (self.checkpoint / name).write_bytes(content)
            self.tokenizer_hashes[name] = hashlib.sha256(content).hexdigest()
        # No weight file exists: any accidental weight read fails this test.
        (self.checkpoint / "checkpoint.json").write_text(json.dumps({
            "files": {**self.tokenizer_hashes, "model.safetensors": "b" * 64}
        }))
        self.paths = {split: self.directory / f"{split}.jsonl"
                      for split in ("train", "calibration")}
        self.rows = {}
        for split in self.paths:
            self.paths[split].write_bytes(b"synthetic shard placeholder")
            self.rows[split] = [
                {"id": f"synthetic-{split}-{label}", "source_group": f"synthetic-{split}",
                 "hash": hashlib.sha256(f"synthetic-{split}-{label}".encode()).hexdigest(),
                 "ids": list(range(length)), "label": label, "evidence": "synthetic_fixture"}
                for label, length in enumerate((2, 4, 7))
            ]
        self.tokenizer = object()
        self.common = types.ModuleType("common")
        common_source = self.directory / "common.py"
        common_source.write_text("# Synthetic common module source binding.\n")
        self.common.__file__ = str(common_source)
        self.common.LABELS = ["human_only", "model_only", "mixed"]
        self.common.check_partition_separation = Mock()
        self.common.class_counts = Mock(side_effect=lambda rows: [
            sum(row["label"] == label for row in rows) for label in range(3)
        ])
        self.common.load_checkpoint_tokenizer = Mock(return_value=self.tokenizer)
        self.common.load_partition = Mock(side_effect=lambda path, split, tokenizer, limit: self.rows[split])
        self.common.sha256 = Mock(side_effect=lambda path: hashlib.sha256(Path(path).read_bytes()).hexdigest())
        self.common.write_json = Mock()
        spec = importlib.util.spec_from_file_location("audit_under_test", Path(__file__).with_name("audit.py"))
        self.audit = importlib.util.module_from_spec(spec)
        with patch.dict(sys.modules, {"common": self.common}):
            spec.loader.exec_module(self.audit)

    def run_audit(self, record_counts=False):
        self.common.load_checkpoint_tokenizer.reset_mock()
        self.common.load_partition.reset_mock()
        args = ["audit.py", "--checkpoint", str(self.checkpoint), "--max-tokens", "4",
                "--output", str(self.output)]
        for split, path in self.paths.items():
            args.extend([f"--{split}-jsonl", str(path)])
        if record_counts:
            args.append("--record-token-counts")
        with patch.object(sys, "argv", args), contextlib.redirect_stdout(io.StringIO()) as stdout:
            self.audit.main()
        self.common.load_checkpoint_tokenizer.assert_called_once_with(self.checkpoint)
        self.assertEqual(self.common.load_partition.call_args_list, [
            call(path, split, self.tokenizer, None) for split, path in self.paths.items()
        ])
        self.assertEqual(self.common.write_json.call_args.args[0], self.output)
        return copy.deepcopy(self.common.write_json.call_args.args[1]), stdout.getvalue()

    def test_default_report_retains_schema_and_omits_record_counts(self):
        report, _ = self.run_audit()
        self.assertEqual(report["schema"], "slop_ninja.encoder_shard_audit.v1")
        self.assertEqual(report["test_inspection"], "not_opened")
        self.assertFalse(report["fits_configured_token_limit"])
        self.assertEqual(report["records_dropped"], 0)
        self.assertNotIn("record_token_counts_provenance", report)
        self.assertEqual(self.common.sha256.call_args_list, [
            call(path) for path in [*self.paths.values(), self.checkpoint / "checkpoint.json"]
        ])
        for split, shard in report["shards"].items():
            self.assertNotIn("record_token_counts", shard)
            self.assertEqual(shard["records"], 3)
            self.assertEqual(shard["over_limit_records"], [
                {"id": f"synthetic-{split}-2", "token_count": 7}
            ])

    def test_opt_in_preserves_every_row_and_other_output_without_loading_again(self):
        original = copy.deepcopy(self.rows)
        default, default_stdout = self.run_audit()
        detailed, detailed_stdout = self.run_audit(record_counts=True)
        self.assertEqual(detailed.pop("record_token_counts_provenance"), {
            "audit_py_sha256": hashlib.sha256(Path(self.audit.__file__).read_bytes()).hexdigest(),
            "common_py_sha256": hashlib.sha256(Path(self.common.__file__).read_bytes()).hexdigest(),
            "tokenizer_files_sha256": self.tokenizer_hashes,
        })
        for split, shard in detailed["shards"].items():
            self.assertEqual(shard.pop("record_token_counts"), [
                {"id": f"synthetic-{split}-{label}",
                 "text_sha256": hashlib.sha256(f"synthetic-{split}-{label}".encode()).hexdigest(),
                 "token_count": length}
                for label, length in enumerate((2, 4, 7))
            ])
        self.assertEqual(json.dumps(detailed), json.dumps(default))
        self.assertEqual(detailed_stdout, default_stdout)
        self.assertEqual(self.rows, original)

    def test_changed_tokenizer_file_cannot_receive_a_false_pin_binding(self):
        (self.checkpoint / "tokenizer_config.json").write_text('{"changed": true}')
        with self.assertRaisesRegex(ValueError, "tokenizer files changed"):
            self.run_audit(record_counts=True)
        self.common.write_json.assert_not_called()

    def test_unassigned_audit_retains_overflow_without_assigning_a_partition(self):
        source = self.paths["train"]
        self.rows[None] = copy.deepcopy(self.rows["train"])
        original = copy.deepcopy(self.rows[None])
        args = ["audit.py", "--checkpoint", str(self.checkpoint), "--max-tokens", "4",
                "--output", str(self.output), "--unassigned-jsonl", str(source),
                "--record-token-counts"]
        with patch.object(sys, "argv", args), contextlib.redirect_stdout(io.StringIO()):
            self.audit.main()
        self.common.load_partition.assert_called_once_with(source, None, self.tokenizer, None)
        report = self.common.write_json.call_args.args[1]
        self.assertEqual(report["schema"], "slop_ninja.encoder_source_audit.v1")
        self.assertFalse(report["experiment_splits_assigned"])
        self.assertFalse(report["model_executed"])
        self.assertEqual(report["test_inspection"], "not_opened")
        self.assertEqual(report["records_dropped"], 0)
        self.assertEqual(report["shards"]["unassigned"]["over_limit_count"], 1)
        self.assertEqual(len(report["shards"]["unassigned"]["record_token_counts"]), 3)
        self.assertEqual(self.rows[None], original)
        with patch.object(sys, "argv", args + ["--train-jsonl", str(source)]), \
                contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            self.audit.main()


if __name__ == "__main__":
    unittest.main()

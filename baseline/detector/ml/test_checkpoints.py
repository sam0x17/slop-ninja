"""Checkpoint identity and local-load boundaries; synthetic files, no downloads or model execution."""

import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

import common
from checkpoints import CHECKPOINTS, MODEL_FILES


class CheckpointTest(unittest.TestCase):
    def fixture(self, root, model_id):
        spec = CHECKPOINTS[model_id]
        config = {"model_type": "modernbert", "vocab_size": 50368,
                  "max_position_embeddings": 8192, "classifier_pooling": "mean", **spec["config"]}
        for name in MODEL_FILES:
            (root / name).write_text(json.dumps(config) if name == "config.json" else "synthetic " + name)
        pin = {"model_id": model_id, "revision": spec["revision"], "weight_license": "Apache-2.0",
               "files": {name: common.sha256(root / name) for name in MODEL_FILES}}
        self.write_pin(root, pin)
        return pin, config

    @staticmethod
    def write_pin(root, pin):
        (root / "checkpoint.json").write_text(json.dumps(pin))

    def test_both_sizes_load_locally_with_the_same_label_contract(self):
        for model_id in CHECKPOINTS:
            with self.subTest(model=model_id), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                pin, config = self.fixture(root, model_id)
                model = Mock()
                model.config.to_dict.return_value = config
                tokenizer = object()
                with patch.object(common.AutoTokenizer, "from_pretrained", return_value=tokenizer) as tok, \
                     patch.object(common.AutoModelForSequenceClassification, "from_pretrained", return_value=model) as load:
                    self.assertEqual(common.load_checkpoint(root, 17), (tokenizer, model))
                tok.assert_called_once_with(root, local_files_only=True, trust_remote_code=False)
                kwargs = load.call_args.kwargs
                self.assertEqual(kwargs["num_labels"], 3)
                self.assertEqual(kwargs["id2label"], dict(enumerate(common.LABELS)))
                self.assertTrue(kwargs["local_files_only"])
                self.assertTrue(kwargs["use_safetensors"])
                self.assertFalse(kwargs["trust_remote_code"])
                self.assertEqual(common.read_checkpoint(root)[0], pin)

    def test_wrong_identity_architecture_and_changed_assets_fail_before_loading(self):
        model_id = "answerdotai/ModernBERT-large"
        changes = [
            lambda root, pin, config: pin.update(model_id="unapproved/model"),
            lambda root, pin, config: pin.update(revision=CHECKPOINTS["answerdotai/ModernBERT-base"]["revision"]),
            lambda root, pin, config: pin.update(weight_license="unknown"),
            lambda root, pin, config: pin["files"].pop("tokenizer.json"),
            lambda root, pin, config: (root / "model.safetensors").write_text("changed bytes"),
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(common.AutoTokenizer, "from_pretrained") as tok, \
                 patch.object(common.AutoModelForSequenceClassification, "from_pretrained") as model:
                for index, change in enumerate(changes):
                    with self.subTest(change=index):
                        pin, config = self.fixture(root, model_id)
                        change(root, pin, config)
                        self.write_pin(root, pin)
                        with self.assertRaises(ValueError):
                            common.load_checkpoint(root, 17)
                # Even a self-consistent file manifest cannot label base dimensions as large.
                pin, config = self.fixture(root, model_id)
                config.update(CHECKPOINTS["answerdotai/ModernBERT-base"]["config"])
                (root / "config.json").write_text(json.dumps(config))
                pin["files"]["config.json"] = common.sha256(root / "config.json")
                self.write_pin(root, pin)
                with self.assertRaisesRegex(ValueError, "architecture"):
                    common.load_checkpoint(root, 17)
                tok.assert_not_called()
                model.assert_not_called()

    def test_tokenizer_audit_avoids_weight_reads_but_rejects_changed_tokenizer(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fixture(root, "answerdotai/ModernBERT-large")
            (root / "model.safetensors").unlink()
            with patch.object(common.AutoTokenizer, "from_pretrained", return_value="tokenizer") as tok, \
                 patch.object(common.AutoModelForSequenceClassification, "from_pretrained") as model:
                self.assertEqual(common.load_checkpoint_tokenizer(root), "tokenizer")
                (root / "tokenizer.json").write_text("changed tokenizer")
                with self.assertRaisesRegex(ValueError, "hash mismatch"):
                    common.load_checkpoint_tokenizer(root)
                tok.assert_called_once()
                model.assert_not_called()


if __name__ == "__main__":
    unittest.main()

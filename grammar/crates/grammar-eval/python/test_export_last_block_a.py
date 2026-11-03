"""Offline export boundary checks. No model/provider calls or corpus input."""
import json
from pathlib import Path
import tempfile
import unittest

from export_last_block_a import calibration, supervision


class ExportBoundary(unittest.TestCase):
    def test_epoch_zero_cannot_claim_trained_heads(self):
        rows = [{"targets": {"origin": 2, "share": .8, "human": 0, "ai": 0}},
                {"targets": {"origin": 0, "share": None, "human": None, "ai": None}}]
        self.assertFalse(any(supervision(rows, 0).values()))
        self.assertEqual(supervision(rows, 1), {"origin": 2, "share": 1, "human": 1,
                                             "ai": 1, "mixed_fractional_share": 1})

    def test_calibration_cannot_be_reused_across_weights_or_split(self):
        value = {"schema": "slop_ninja_last_block_calibration_v1", "complete": True,
                 "fit_split": "calibration", "final_test_opened": False,
                 "condition": "adapt_last_block", "seed": 17,
                 "selected_checkpoint_sha256": "a" * 64, "contract_sha256": "b" * 64,
                 "origin_calibrated": True, "temperature": 1.2,
                 "roles": {r: {"qualified": True, "threshold": .7} for r in ("human", "ai")}}
        binding = {"condition": "adapt_last_block", "seed": 17}
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "calibration.json"
            path.write_text(json.dumps(value))
            self.assertTrue(calibration(path, binding, "a" * 64, "b" * 64)[2])
            with self.assertRaises(ValueError):
                calibration(path, binding, "c" * 64, "b" * 64)
            value["fit_split"] = "test"
            path.write_text(json.dumps(value))
            with self.assertRaises(ValueError):
                calibration(path, binding, "a" * 64, "b" * 64)


if __name__ == "__main__":
    unittest.main()

"""Offline scope checks only; no model loads, corpus reads or provider calls."""
import copy
import unittest
from predict_last_block_a import test_scope


class TestScope(unittest.TestCase):
    def setUp(self):
        self.manifest = {"schema": "slop_ninja_last_block_test_dataset_v1", "complete": True,
                         "final_test_opened": True, "test_used_for_selection": False,
                         "normalization_refit": False, "profile_enrollment_changed": False,
                         "contract_sha256": "contract", "fitting_dataset_manifest_sha256": "fit",
                         "selection_sha256": "selection"}
        self.seal = {"schema": "slop_ninja_last_block_final_test_seal_v1", "complete": True,
                     "model_reselection_permitted": False, "test_used_for_selection": False,
                     "dataset": {"sha256": "fit"}, "selection": {"sha256": "selection"}}

    def check(self, manifest=None, seal=None, part="test"):
        test_scope(manifest or self.manifest, seal or self.seal, "contract", "fit", "selection", part)

    def test_only_test_and_reference_are_permitted(self):
        self.check()
        self.check(part="reference")
        for part in ("train", "development", "calibration"):
            with self.assertRaises(ValueError):
                self.check(part=part)

    def test_no_refitting_reselection_or_contract_change(self):
        for field in ("normalization_refit", "profile_enrollment_changed", "test_used_for_selection"):
            m = copy.deepcopy(self.manifest)
            m[field] = True
            with self.assertRaises(ValueError):
                self.check(manifest=m)
        for field in ("contract_sha256", "fitting_dataset_manifest_sha256", "selection_sha256"):
            m = copy.deepcopy(self.manifest)
            m[field] = "changed"
            with self.assertRaises(ValueError):
                self.check(manifest=m)
        s = copy.deepcopy(self.seal)
        s["model_reselection_permitted"] = True
        with self.assertRaises(ValueError):
            self.check(seal=s)


if __name__ == "__main__":
    unittest.main()

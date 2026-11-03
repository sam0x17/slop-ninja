"""Offline tests of the immutable V6 comparator scope."""
import copy
import unittest
from predict_frozen_v6_head import check_scope


class Scope(unittest.TestCase):
    def test_test_requires_sealed_initializer_and_explicit_route(self):
        m = {"schema": "slop_ninja_last_block_test_fixed_comparators_v1", "complete": True,
             "final_test_opened": True, "test_used_for_selection": False, "fitting_or_calibration_performed": False,
             "original_prepared_manifest_sha256": "old", "v6_initializer_path": "init", "v6_initializer_sha256": "weights"}
        s = {"schema": "slop_ninja_last_block_final_test_seal_v1", "complete": True,
             "model_reselection_permitted": False,
             "binding_files": [{"path": "original", "sha256": "old"}, {"path": "init", "sha256": "weights"}]}
        self.assertEqual(check_scope(m, s), ("test", "reference"))
        with self.assertRaises(ValueError):
            check_scope(m)
        for field in ("v6_initializer_sha256", "original_prepared_manifest_sha256"):
            bad = copy.deepcopy(m)
            bad[field] = "changed"
            with self.assertRaises(ValueError):
                check_scope(bad, s)
        m["fitting_or_calibration_performed"] = True
        with self.assertRaises(ValueError):
            check_scope(m, s)

    def test_original_non_test_scope_remains_separate(self):
        m = {"schema": "slop_ninja_last_block_fixed_comparators_v1", "complete": True,
             "final_test_opened": False, "fitting_or_calibration_performed": False}
        self.assertNotIn("test", check_scope(m))
        m["final_test_opened"] = True
        with self.assertRaises(ValueError):
            check_scope(m)

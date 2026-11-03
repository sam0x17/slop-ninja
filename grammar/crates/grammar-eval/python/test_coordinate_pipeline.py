"""Frozen cohort bindings and complete-grid selection checks."""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

import numpy as np

from coordinate_io import load_array, validate_binding
from coordinate_model import LAMBDA_GRID, SEEDS
from frontier_common import sha256
from train_coordinates import choose_lambda, paired_interval


class CoordinatePipelineTests(unittest.TestCase):
    def test_array_loader_rejects_tampering_wrong_shapes_and_nonfinite_values(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "values.f64"
            np.array([[1.0, 2.0]], dtype="<f8").tofile(path)
            binding = {"path": path.name, "shape": [1, 2], "dtype": "float64-le", "sha256": sha256(path)}
            self.assertEqual(load_array(root, binding, (1, 2)).tolist(), [[1.0, 2.0]])
            with self.assertRaisesRegex(ValueError, "shape"):
                load_array(root, binding, (2, 1))
            np.array([[1.0, 3.0]], dtype="<f8").tofile(path)
            with self.assertRaisesRegex(ValueError, "digest"):
                load_array(root, binding, (1, 2))
            np.array([[1.0, np.nan]], dtype="<f8").tofile(path)
            binding["sha256"] = sha256(path)
            with self.assertRaisesRegex(ValueError, "nonfinite"):
                load_array(root, binding, (1, 2))
            binding["path"] = "../outside.f64"
            with self.assertRaisesRegex(ValueError, "escapes"):
                load_array(root, binding, (1, 2))

    def binding_fixture(self):
        data = {"authors": ["a", "c"], "rows": [{}, {}], "manifest": {"input_bindings": {"reference": "space-hash"}}}
        binding = {"eligible_author_ids": ["a", "c"], "excluded_author_ids": ["b"], "query_count": 2,
                   "source_summary_sha256": "summary-hash"}
        digest = hashlib.sha256(json.dumps(["a", "b", "c"], separators=(",", ":")).encode()).hexdigest()
        descriptor = {"author_ids_sha256": digest, "summary_sha256": "summary-hash"}
        return data, binding, descriptor

    def test_binding_requires_original_cohort_partition_summary_and_reference_bytes(self):
        data, binding, descriptor = self.binding_fixture()
        validate_binding(data, binding, descriptor, "space-hash")
        for key, value, expected in [("excluded_author_ids", ["d"], "author digest"),
                                     ("excluded_author_ids", ["a"], "excluded author"),
                                     ("source_summary_sha256", "other", "summary"),
                                     ("query_count", 3, "eligibility")]:
            changed = copy.deepcopy(binding)
            changed[key] = value
            with self.subTest(key=key, value=value), self.assertRaisesRegex(ValueError, expected):
                validate_binding(data, changed, descriptor, "space-hash")
        with self.assertRaisesRegex(ValueError, "reference-space"):
            validate_binding(data, binding, descriptor, "other-space")

    def runs(self):
        return [{"lambda": value, "inherited_seed": seed, "status": "complete",
                 "selected_validation": {"macro_author": {"top1": 0.5, "top5": 0.7, "mrr": 0.6}}}
                for value in LAMBDA_GRID for seed in SEEDS]

    def test_shared_lambda_uses_seed_mean_then_mrr_then_stronger_penalty(self):
        runs = self.runs()
        self.assertEqual(choose_lambda(runs)[0], 10)
        for row in runs:
            if row["lambda"] == 1:
                row["selected_validation"]["macro_author"]["mrr"] = 0.61
            if row["lambda"] == 0.01:
                row["selected_validation"]["macro_author"]["top1"] = 0.99 if row["inherited_seed"] == 0 else 0.1
        self.assertEqual(choose_lambda(runs)[0], 1)
        for row in runs:
            if row["lambda"] == 0.1:
                row["selected_validation"]["macro_author"]["top1"] = 0.51
        self.assertEqual(choose_lambda(runs)[0], 0.1)

    def test_selection_fails_for_missing_failed_duplicate_or_extra_fits(self):
        complete = self.runs()
        failed = copy.deepcopy(complete)
        failed[3]["status"] = "failed"
        duplicate = copy.deepcopy(complete)
        duplicate[-1] = duplicate[0]
        for rows in [complete[:-1], failed, duplicate, complete + [complete[0]]]:
            with self.subTest(rows=rows), self.assertRaisesRegex(ValueError, "all twelve"):
                choose_lambda(rows)

    def test_stratified_interval_weights_authors_and_preserves_gallery_sizes(self):
        def metric(value):
            return {name: value for name in ["top1", "top5", "mrr"]}
        left = {"gallery-b": {"b1": metric(0), "b2": metric(0), "b3": metric(0)},
                "gallery-a": {"a1": metric(1)}}
        right = {name: {author: metric(0) for author in rows} for name, rows in left.items()}
        result = paired_interval(left, right)
        self.assertEqual(result["gallery_order"], ["gallery-a", "gallery-b"])
        self.assertEqual(result["gallery_author_counts"], {"gallery-a": 1, "gallery-b": 3})
        self.assertEqual(result["author_count"], 4)
        self.assertEqual(result["metrics"]["top1"], {"mean_difference": 0.25, "percentile_95": [0.25, 0.25]})


if __name__ == "__main__":
    unittest.main()

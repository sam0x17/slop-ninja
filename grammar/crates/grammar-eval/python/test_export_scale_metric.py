"""Focused tests for the frozen treatment's portable inference adapter."""
import argparse
import copy
import math
from pathlib import Path
import tempfile
import unittest

import torch

from export_scale_metric import (complete_geometry, export, index_hash, portable_coordinates,
                                 selected_coordinates, validate_selection, validated_eta)


def geometry_fixture():
    axes = [{"family": family, "feature": feature, "kind": kind, "scale": scale}
            for family, feature, kind, scale in [("syntax", "a", "metric", 2.0),
                                                 ("syntax", "b", "metric", 4.0),
                                                 ("word", "a", "distribution", 1.0),
                                                 ("word", "b", "distribution", 1.0)]]
    coordinates = [{**axis, "family_index": 0 if axis["family"] == "syntax" else 1,
                    "original_family_axis_count": 2} for axis in axes]
    indices = [0, 3]
    subset = {"max_column_indices": indices, "coordinate_count": 2, "indices_sha256": index_hash(indices)}
    catalog = {"schema": "slopninja-coordinate-frontier-catalog-v1", "families": ["syntax", "word"],
               "coordinates": coordinates, "subsets": {"all_m2": subset}}
    space = {"axes": axes, "family_schemas": {"syntax": {"kind": "metric"}, "word": {"kind": "distribution"}}}
    protocol = {"coordinate_selection": {"fixed_subset": "all_m2", "fixed_subset_indices_sha256": index_hash(indices)},
                "fixed_subset_indices_sha256": index_hash(indices), "coordinate_count": 2, "maximal_coordinate_count": 4}
    return catalog, space, {"name": "all_m2", **copy.deepcopy(subset)}, protocol


class ExportScaleMetricTests(unittest.TestCase):
    def test_nonprefix_projection_keeps_inactive_numeric_geometry(self):
        catalog, space, config, protocol = geometry_fixture()
        coordinates = selected_coordinates(catalog, config, protocol)
        self.assertEqual([(axis["family"], axis["feature"]) for axis in coordinates], [("syntax", "a"), ("word", "b")])
        portable = portable_coordinates(coordinates, torch.tensor([math.log(2), math.log(0.5)], dtype=torch.float64))
        self.assertEqual([axis["multiplier"] for axis in portable], [2.0, 0.5])
        families = complete_geometry(catalog, space, coordinates)
        self.assertEqual(families[0]["numerical_axes"], [{"feature": "a", "scale": 2.0}, {"feature": "b", "scale": 4.0}])
        self.assertEqual([family["original_axis_count"] for family in families], [2, 2])
        self.assertEqual([axis["original_family_axis_count"] for axis in portable], [2, 2])

    def test_reordered_duplicate_and_stale_hash_indices_rejected(self):
        for indices in ([3, 0], [0, 0], [0, 2], [0, 4], [False, 3]):
            with self.subTest(indices=indices):
                catalog, _, config, protocol = geometry_fixture()
                catalog["subsets"]["all_m2"]["max_column_indices"] = indices
                config["max_column_indices"] = indices
                with self.assertRaises(ValueError):
                    selected_coordinates(catalog, config, protocol)

    def test_numeric_scale_denominator_and_family_changes_rejected(self):
        for key, value in (("scale", 4.0), ("original_family_axis_count", 1), ("family_index", 1)):
            with self.subTest(key=key):
                catalog, space, config, protocol = geometry_fixture()
                selected = selected_coordinates(catalog, config, protocol)
                selected[0][key] = value
                with self.assertRaises(ValueError):
                    complete_geometry(catalog, space, selected)

    def test_complete_grid_preserves_all_seeds_and_stronger_lambda_tie(self):
        protocol = {"inherited_seeds": [0, 17, 29], "lambda_grid": [0.1, 1, 10, 100], "coordinate_count": 2}
        runs = [{"lambda": regularization, "inherited_seed": seed, "status": "complete", "actual_trainable_parameters": 2,
                 "selected_validation": {"macro_author": {"top1": 0.5, "mrr": 0.6}}}
                for regularization in protocol["lambda_grid"] for seed in protocol["inherited_seeds"]]
        selection = {"schema": "slopninja-frozen-training-author-scale-v1", "inherited_seeds": [0, 17, 29],
                     "completed_new_fits": 12, "failed_new_fits": 0, "control_optimizer_steps": 0,
                     "selected_lambda": 100, "runs": runs}
        self.assertEqual([row["inherited_seed"] for row in validate_selection(selection, protocol)], [0, 17, 29])
        for bad in ([*runs[:-1], runs[0]], runs[:-1]):
            with self.assertRaises(ValueError):
                validate_selection({**selection, "runs": bad}, protocol)
        with self.assertRaises(ValueError):
            validate_selection({**selection, "selected_lambda": 0.1}, protocol)
        with self.assertRaises(ValueError):
            validate_selection({**selection, "inherited_seeds": [0]}, protocol)

    def test_eta_checkpoint_identity_dtype_and_bounds_enforced(self):
        eta = torch.tensor([0.2, -0.3], dtype=torch.float64)
        protocol = {"coordinate_count": 2, "optimization": {"weight_bounds": [0.5, 2]}}
        selection = {"selected_lambda": 0.1, "config": {"name": "all_m2"}, "catalog_sha256": "catalog"}
        run = {"inherited_seed": 17, "selected_epoch": 19, "lambda": 0.1, "catalog_sha256": "catalog",
               "baseline_checkpoint_sha256": "baseline", "selected_validation": {"top1": 0.5}, "selected_eta": eta.tolist()}
        stored = {"schema": "slopninja-training-author-scale-checkpoint-v1", "inherited_seed": 17, "epoch": 19,
                  "lambda": 0.1, "config": selection["config"], "catalog_sha256": "catalog",
                  "baseline_checkpoint_sha256": "baseline", "validation": run["selected_validation"], "eta": eta}
        self.assertTrue(torch.equal(validated_eta(stored, run, selection, protocol), eta))
        for changes in ({"inherited_seed": 0}, {"baseline_checkpoint_sha256": "other"}, {"eta": eta.float()},
                        {"eta": torch.tensor([math.nan, 0.0], dtype=torch.float64)},
                        {"eta": torch.tensor([0.7, 0.0], dtype=torch.float64)}, {"eta": eta[:1]}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                validated_eta({**stored, **changes}, run, selection, protocol)

    def test_changed_external_selection_binding_leaves_no_export(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            selection = root / "selection.json"
            selection.write_text("{}\n")
            args = argparse.Namespace(out=root / "out", selection=selection, selection_sha256="0" * 64)
            with self.assertRaisesRegex(ValueError, "selection SHA"):
                export(args)
            self.assertFalse(args.out.exists())


if __name__ == "__main__":
    unittest.main()

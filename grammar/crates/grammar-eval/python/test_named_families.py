"""Focused matching, gradient, split, provenance and retrieval tests."""
import copy
import hashlib
import math
from pathlib import Path
import tempfile
import unittest

import numpy as np
import torch

from named_family_io import bound_array, digest_leaves, weights_by_author_date
from named_family_metrics import metric_rows, paired_interval, summarize
from named_family_model import NamedFamilyMetric, backward_panels, fit_shared_knots, initial_state
from train_named_families import complete_runs


SETTINGS = {"lexical_strength": 0.25, "grammar_strength": 0.5 / 12, "jitter_std": 0.05,
            "zero_jitter_seed": 0, "baseline_temperature": 0.1, "initial_log_slopes": 0}
BASELINE = ["grammar", "word", "word_bigram"]
FAMILIES = ["added", *BASELINE]


def model(active=FAMILIES, seed=0):
    return NamedFamilyMetric(FAMILIES, BASELINE, active, [0.2, 0.7], seed, SETTINGS)


class NamedFamilyTests(unittest.TestCase):
    def test_named_initialization_preserves_shared_coefficients_and_real_mask_size(self):
        for seed in [0, 17, 29]:
            small, large = model(BASELINE, seed), model(FAMILIES, seed)
            self.assertTrue(torch.equal(small.theta, large.theta[1:]))
            self.assertTrue(torch.allclose(small.theta.softmax(0) / small.log_tau.exp(),
                                           (large.theta.softmax(0) / large.log_tau.exp())[1:], atol=1e-14, rtol=0))
            self.assertEqual(sum(p.numel() for p in small.parameters()), 6)
            self.assertEqual(sum(p.numel() for p in large.parameters()), 7)
            self.assertEqual(small.initialization["named_theta"], large.initialization["named_theta"])
        self.assertAlmostEqual(initial_state(FAMILIES, BASELINE, BASELINE, 0, SETTINGS)["temperature"], 0.1)
        with self.assertRaises(ValueError):
            model(["missing"])

    def test_pooled_gradient_matches_manual_loss_and_finite_differences(self):
        torch.manual_seed(11)
        panels = {name: {"x": torch.rand(3, 2, 4, dtype=torch.float64) + offset,
                         "own": torch.tensor([0, 1, 0]), "loss_weight": torch.tensor([0.25, 0.5, 0.25], dtype=torch.float64),
                         "authors": ["a", "b"]} for name, offset in [("one", 0.01), ("two", 0.1), ("three", 0.05)]}
        fitted = model()
        fitted.log_slope.data.copy_(torch.tensor([0.2, -0.1], dtype=torch.float64))
        fitted.zero_grad(set_to_none=True)
        recorded = backward_panels(fitted, panels)
        analytic = [p.grad.detach().clone() for p in fitted.parameters()]

        def objective():
            return sum((torch.nn.functional.cross_entropy(fitted(data["x"]), data["own"], reduction="none") * data["loss_weight"]).sum()
                       for data in panels.values()) / 3

        self.assertAlmostEqual(recorded["training_cross_entropy"], objective().item(), places=14)
        fitted.zero_grad(set_to_none=True)
        objective().backward()
        for got, parameter in zip(analytic, fitted.parameters()):
            self.assertTrue(torch.allclose(got, parameter.grad, atol=1e-13, rtol=1e-13))
        for parameter, expected in zip(fitted.parameters(), analytic):
            for index in range(parameter.numel()):
                with torch.no_grad():
                    original = parameter.reshape(-1)[index].item()
                    parameter.reshape(-1)[index] = original + 1e-6
                    plus = objective().item()
                    parameter.reshape(-1)[index] = original - 1e-6
                    minus = objective().item()
                    parameter.reshape(-1)[index] = original
                self.assertAlmostEqual((plus - minus) / 2e-6, expected.reshape(-1)[index].item(), places=7)

    def test_knot_population_ignores_added_family_and_rejects_development(self):
        x = torch.linspace(0, 1, 240, dtype=torch.float64).reshape(20, 3, 4)
        panel = {"x": x, "families": FAMILIES, "split": "train", "manifest_sha256": "source"}
        first = fit_shared_knots({"panel": panel}, BASELINE, 5)
        changed = x.clone()
        changed[:, :, 0] = 1e12
        second = fit_shared_knots({"panel": {**panel, "x": changed}}, BASELINE, 5)
        self.assertEqual(first, second)
        self.assertEqual(first["population_count"], 180)
        self.assertEqual(len(first["knots"]), 5)
        with self.assertRaises(ValueError):
            fit_shared_knots({"panel": {**panel, "split": "dev"}}, BASELINE, 5)

    def test_exact_ties_and_author_macro_are_not_query_weighted(self):
        logits = np.array([[1] * 6, [5, 4, 3, 2, 1, 0]], dtype=float)
        rows = metric_rows(logits, np.array([0, 5]))
        np.testing.assert_allclose(rows[0], [1 / 6, 5 / 6, sum(1 / i for i in range(1, 7)) / 6])
        np.testing.assert_allclose(rows[1], [0, 0, 1 / 6])
        result = summarize([[1, 1, 1], [0, 0, 0], [0, 0, 0], [0, 0, 0]], ["a", "b", "b", "b"])
        self.assertEqual(result["macro_author"]["top1"], 0.5)
        self.assertEqual(result["micro"]["top1"], 0.25)
        with self.assertRaises(ValueError):
            metric_rows(np.array([[math.nan, 0]]), np.array([0]))

    def test_bootstrap_preserves_actual_gallery_sizes_and_old_convention(self):
        from train_coordinates import paired_interval as frozen_reference
        left = {"small": {"a": dict(top1=0.7, top5=0.8, mrr=0.75)},
                "large": {"b": dict(top1=0.2, top5=0.5, mrr=0.3), "c": dict(top1=0.9, top5=1, mrr=0.95)}}
        right = {name: {author: dict(top1=0, top5=0, mrr=0) for author in rows} for name, rows in left.items()}
        result = paired_interval(left, right)
        self.assertEqual(result, frozen_reference(left, right))
        self.assertEqual(result["gallery_author_counts"], {"large": 2, "small": 1})
        self.assertAlmostEqual(result["metrics"]["top1"]["mean_difference"], 0.6)

    def test_complete_fit_grid_rejects_missing_duplicate_or_failed_runs(self):
        protocol = {"arms": [{"name": "raw14"}, {"name": "raw15"}], "seeds": [0, 17, 29]}
        runs = [{"arm": arm, "seed": seed, "status": "complete"} for arm in protocol["arms"] for seed in protocol["seeds"]]
        self.assertEqual(complete_runs(runs, protocol), runs)
        for wrong in [runs[:-1], [*runs[:-1], runs[0]], [*runs[:-1], {**runs[-1], "status": "failed"}]]:
            with self.assertRaises(ValueError):
                complete_runs(wrong, protocol)

    def test_array_hash_shape_finiteness_and_nested_source_bindings(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "values.f64"
            np.array([0.2, 0.3], dtype="<f8").tofile(path)
            binding = {"path": path.name, "dtype": "float64-le", "shape": [1, 2, 1], "bytes": 16,
                       "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
            self.assertEqual(bound_array(root, binding, (1, 2, 1)).shape, (1, 2, 1))
            with self.assertRaises(ValueError):
                bound_array(root, {**binding, "sha256": "0" * 64}, (1, 2, 1))
            np.array([np.nan, 0.3], dtype="<f8").tofile(path)
            with self.assertRaises(ValueError):
                bound_array(root, {**binding, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}, (1, 2, 1))
        self.assertEqual(digest_leaves({"path": "/some/path", "original": {"a": "a" * 64}, "array": ["b" * 64]}), {"a" * 64, "b" * 64})

    def test_date_balancing_and_input_cache_mutation(self):
        pairs = [("a", "2004-01-01"), ("a", "2004-01-01"), ("a", "2004-01-02"), ("b", "2004-01-01")]
        rows = [{"author": author, "date": date, "source_group": f"blog:{author}:date:{date}"} for author, date in pairs]
        self.assertEqual(weights_by_author_date(rows), [0.125, 0.125, 0.25, 0.5])
        fitted = model()
        x = torch.full((1, 2, 4), 0.1, dtype=torch.float64)
        before = fitted(x).detach().clone()
        x.add_(0.3)
        after = fitted(x).detach()
        self.assertFalse(torch.equal(before, after))
        fresh = model()
        self.assertTrue(torch.equal(after, fresh(x)))


if __name__ == "__main__":
    unittest.main()

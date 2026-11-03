"""Synthetic tests; no corpus, detector or model-service calls."""
import math
import unittest

import numpy as np
import torch

from frontier_common import balanced_loss, query_metrics, summarize
from frontier_models import CATALOG, fit_transforms, make_model, original_weights
from train_frontier import choose_model, paired_difference


FAMILIES = sorted(["word", "word_bigram", "pos", "pos_tag", "pos_bigram", "pos_trigram", "dependency",
                   "dependency_path_2", "dependency_path_3", "ordered_head_frame", "clause_head_frame",
                   "morphology_bundle", "syntax_load", "syntax_sentence_load"])


class FrontierTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        torch.set_num_threads(2)
        torch.set_default_dtype(torch.float64)
        torch.manual_seed(71)
        cls.x = torch.rand(20, 4, 14)
        cls.transforms = fit_transforms(cls.x)

    def test_actual_parameter_counts_and_seed_zero_metric_prior(self):
        expected = -(self.x * original_weights(FAMILIES)).sum(-1) / 0.1
        for config in CATALOG:
            model = make_model(config, FAMILIES, self.transforms, 0)
            self.assertEqual(sum(p.numel() for p in model.parameters()), config["parameters"])
            if config["kind"] in {"tied", "diagonal", "spline"}:
                torch.testing.assert_close(model(self.x), expected, atol=1e-13, rtol=1e-13)
                torch.testing.assert_close(model.weights(), original_weights(FAMILIES), atol=1e-15, rtol=1e-15)

    def test_spline_knots_are_nested_and_warp_is_positive_monotone(self):
        prior = set()
        for config in CATALOG:
            if config["kind"] != "spline":
                continue
            model = make_model(config, FAMILIES, self.transforms, 0)
            knots = set(model.knots.tolist())
            self.assertTrue(prior < knots)
            prior = knots
            with torch.no_grad():
                model.log_slope.copy_(torch.linspace(-6, 6, len(model.log_slope)))
            probe = torch.linspace(0, 3, 1000)
            result = model.warp(probe)
            self.assertEqual(result[0].item(), 0)
            self.assertTrue(bool((result[1:] > result[:-1]).all()))

    def test_autograd_matches_finite_differences_for_each_model_kind(self):
        inputs = self.x[:2, :2]
        for name in ["tied_8", "diagonal_15", "spline_18", "linear_14", "mlp_64"]:
            config = next(row for row in CATALOG if row["name"] == name)
            model = make_model(config, FAMILIES, self.transforms, 17)
            named = list(model.named_parameters())
            function = lambda *parameters: torch.func.functional_call(model, dict(zip([key for key, _ in named], parameters)), (inputs,))
            self.assertTrue(torch.autograd.gradcheck(function, tuple(p for _, p in named), eps=1e-6, atol=1e-5, rtol=1e-4))

    def test_frozen_preprocessing_and_cached_inputs_handle_mutation(self):
        for kind in ["mlp", "spline"]:
            config = next(row for row in CATALOG if row["kind"] == kind)
            model = make_model(config, FAMILIES, self.transforms, 17)
            x = self.x.clone()
            before = model(x).detach().clone()
            x.add_(0.25)
            after = model(x).detach()
            self.assertFalse(torch.equal(before, after))
            fresh = make_model(config, FAMILIES, self.transforms, 17)
            torch.testing.assert_close(after, fresh(x), atol=0, rtol=0)
            torch.testing.assert_close(model(x[:, [1, 0, 3, 2]]), after[:, [1, 0, 3, 2]], atol=1e-14, rtol=1e-14)

    def test_seed_reproducibility_and_no_padding(self):
        for config in CATALOG:
            a = make_model(config, FAMILIES, self.transforms, 17)
            b = make_model(config, FAMILIES, self.transforms, 17)
            for left, right in zip(a.parameters(), b.parameters()):
                self.assertTrue(torch.equal(left, right))
            (a(self.x).square().sum()).backward()
            self.assertTrue(all(parameter.grad is not None and bool(torch.isfinite(parameter.grad).all()) for parameter in a.parameters()))

    def test_ties_and_author_macro_match_expected_random_order(self):
        values = np.zeros((2, 6))
        metrics = query_metrics(values, np.array([0, 4]), np.array([1, 2]))
        np.testing.assert_allclose(metrics[:, 0], 1 / 6)
        np.testing.assert_allclose(metrics[:, 1], 5 / 6)
        np.testing.assert_allclose(metrics[:, 2], sum(1 / k for k in range(1, 7)) / 6)
        np.testing.assert_allclose(metrics[:, 3], 0.5)
        result = summarize(np.array([[1, 1, 1, 1], [0, 0, 0, 0], [0, 0, 0, 0]]), ["a", "b", "b"])
        self.assertEqual(result["macro_author"]["top1"], 0.5)
        self.assertEqual(result["micro"]["top1"], 1 / 3)

    def test_training_loss_balances_authors_dates_then_posts(self):
        rows = [{"author": a, "source_group": d} for a, d in [("a", "one"), ("a", "one"), ("a", "two"), ("b", "one")]]
        self.assertEqual(balanced_loss(rows), [0.125, 0.125, 0.25, 0.5])

    def test_model_selection_uses_seed_mean_and_smaller_parameter_tiebreak(self):
        runs = []
        for config, scores in [(CATALOG[0], [0.6, 0.6, 0.6]), (CATALOG[1], [1.0, 0.3, 0.3])]:
            for seed, score in zip([0, 17, 29], scores):
                runs.append({"status": "complete", "config": config, "seed": seed,
                             "selected_development": {"macro_author": dict.fromkeys(["top1", "top5", "mrr", "own_vs_content_impostor"], score)}})
        winner, _ = choose_model(runs)
        self.assertEqual(winner, "tied_8")
        for row in runs:
            row["selected_development"]["macro_author"] = dict.fromkeys(["top1", "top5", "mrr", "own_vs_content_impostor"], 0.6)
        self.assertEqual(choose_model(runs)[0], "tied_8")

    def test_paired_bootstrap_preserves_constant_difference(self):
        names = ["top1", "top5", "mrr", "own_vs_content_impostor"]
        left = {"a": dict.fromkeys(names, 0.7), "b": dict.fromkeys(names, 0.7)}
        right = {"a": dict.fromkeys(names, 0.2), "b": dict.fromkeys(names, 0.2)}
        report = paired_difference(left, right)
        for value in report["metrics"]["top1"]["percentile_95"]:
            self.assertTrue(math.isclose(value, 0.5, abs_tol=1e-15))


if __name__ == "__main__":
    unittest.main()

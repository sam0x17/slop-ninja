"""Nested catalogs, fixed-family regularization, and complete frontier selection."""
import copy
import math
import unittest

import torch

from coordinate_frontier import select_frontier
from coordinate_frontier_models import LAMBDAS, SEEDS, FrontierMetric, configurations, expected_subsets, reset_eta
from coordinate_model import CoordinateInputs


class CoordinateFrontierTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        torch.set_default_dtype(torch.float64)
        torch.set_num_threads(2)

    def catalog(self):
        counts = {"clause_head_frame": 197, "dependency": 359, "dependency_path_2": 1229,
                  "dependency_path_3": 1476, "morphology_bundle": 91, "ordered_head_frame": 523,
                  "pos": 16, "pos_bigram": 189, "pos_tag": 49, "pos_trigram": 1023,
                  "word": 1071, "word_bigram": 1118}
        rows = [{"family": family, "feature": f"axis-{index:05}", "kind": "distribution", "support_rank": index + 1}
                for family, count in counts.items() for index in range(count)]
        rows += [{"family": family, "feature": f"axis-{index}", "kind": "rate", "support_rank": None}
                 for family in ["syntax_load", "syntax_sentence_load"] for index in range(6)]
        catalog = {"coordinates": sorted(rows, key=lambda row: (row["family"], row["feature"]))}
        catalog["subsets"] = expected_subsets(catalog)
        return catalog

    def test_nested_catalogs_are_prefixes_with_exact_aliases_and_full_numeric_axes(self):
        catalog = self.catalog()
        configs = configurations(catalog)
        self.assertEqual(len(configs), 16)
        self.assertEqual([row["coordinate_count"] for row in configs if row["block"] == "all"],
                         [972, 1864, 3557, 4638, 5929, 7353])
        self.assertEqual(catalog["subsets"]["word_m8"]["canonical_name"], "word_m4")
        self.assertEqual(catalog["subsets"]["word_supported"]["canonical_name"], "word_m4")
        previous = set()
        for suffix in ["m0p5", "m1", "m2", "m4", "m8", "supported"]:
            indices = catalog["subsets"][f"all_{suffix}"]["max_column_indices"]
            self.assertTrue(previous.issubset(indices))
            self.assertEqual(sum(catalog["coordinates"][index]["kind"] != "distribution" for index in indices), 12)
            previous = set(indices)
        catalog["subsets"]["word_m1"]["max_column_indices"][0] += 1
        with self.assertRaisesRegex(ValueError, "nested coordinate"):
            configurations(catalog)

    def state(self):
        return {"theta": torch.zeros(14), "log_tau": torch.tensor(math.log(0.1)),
                "knots": torch.tensor([0.2, 0.7]), "log_slope": torch.tensor([0.1, -0.2])}

    def test_penalty_preserves_per_coordinate_shrinkage_across_adjustable_subsets(self):
        word = FrontierMetric(torch.tensor([12, 12, 13]), self.state())
        combined = FrontierMetric(torch.tensor([0, 1, 12, 12, 13]), self.state())
        with torch.no_grad():
            word.eta.copy_(torch.tensor([0.1, 0.2, 0.3]))
            combined.eta.copy_(torch.tensor([0.0, 0.0, 0.1, 0.2, 0.3]))
        self.assertEqual(word.penalty().item(), combined.penalty().item())
        self.assertAlmostEqual(word.penalty().item(), ((0.01 + 0.04) / 2 + 0.09) / 14)
        word.penalty().backward()
        combined.penalty().backward()
        torch.testing.assert_close(word.eta.grad, combined.eta.grad[-3:], rtol=0, atol=0)
        self.assertEqual(sum(value.numel() for value in word.parameters()), 3)

    def test_conditional_resets_keep_raw_families_and_zero_restores_baseline(self):
        families = torch.tensor([0, 12, 13])
        model = FrontierMetric(families, self.state())
        with torch.no_grad():
            model.eta.copy_(torch.tensor([0.1, 0.2, 0.3]))
        reset_eta(model, "word", {12, 13})
        torch.testing.assert_close(model.eta, torch.tensor([0.0, 0.2, 0.3]), rtol=0, atol=0)
        inputs = CoordinateInputs(torch.tensor([[0.1, 0.2, 0.3]]), torch.zeros(2, 3),
                                  torch.ones(1, 2, 14), torch.tensor([0]), families)
        reset_eta(model, "neither", {12, 13})
        self.assertTrue(torch.equal(model(inputs), model.score_distances(inputs.raw_distances)))

    def runs(self):
        configs = configurations(self.catalog())
        runs = [{"status": "complete", "config": config, "lambda": value, "inherited_seed": seed,
                 "selected_validation": {"macro_author": {"top1": 0.5, "top5": 0.7, "mrr": 0.6}}}
                for config in configs for value in LAMBDAS for seed in SEEDS]
        return runs, configs

    def test_frontier_selection_uses_all_seeds_stronger_lambda_then_smaller_model(self):
        runs, configs = self.runs()
        result = select_frontier(runs, configs)
        self.assertEqual(result["winner"], "grammar_m0p5")
        self.assertTrue(all(row["selected_lambda"] == 100 for row in result["cells"]))
        for row in runs:
            if row["config"]["name"] == "all_supported":
                row["selected_validation"]["macro_author"]["top1"] = 0.99 if row["inherited_seed"] == 0 else 0.1
            if row["config"]["name"] == "word_m1" and row["lambda"] == 1:
                row["selected_validation"]["macro_author"]["top1"] = 0.51
        result = select_frontier(runs, configs)
        self.assertEqual(result["winner"], "word_m1")
        self.assertEqual(result["mask_winners"]["word"], "word_m1")

    def test_any_failed_missing_or_duplicated_fit_prevents_selection(self):
        runs, configs = self.runs()
        failed = copy.deepcopy(runs)
        failed[-1]["status"] = "failed"
        duplicate = runs[:-1] + [runs[0]]
        for rows in [failed, duplicate, runs[:-1]]:
            with self.subTest(count=len(rows)), self.assertRaisesRegex(ValueError, "all 192"):
                select_frontier(rows, configs)


if __name__ == "__main__":
    unittest.main()

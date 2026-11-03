"""Pooled-panel gradients and fixed twelve-fit selection behavior."""
import copy
import math
import unittest

import torch

from coordinate_frontier_models import LAMBDAS, SEEDS, FrontierMetric
from coordinate_model import CoordinateInputs
from coordinate_scale import backward_panels, choose_lambda, panel_cross_entropy


class CoordinateScaleTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        torch.set_default_dtype(torch.float64)
        torch.set_num_threads(2)

    def fixture(self):
        torch.manual_seed(482)
        families = torch.tensor([0, 12, 13])
        state = {"theta": torch.zeros(14), "log_tau": torch.tensor(math.log(0.1)),
                 "knots": torch.tensor([0.2, 0.7]), "log_slope": torch.tensor([0.1, -0.2])}
        model = FrontierMetric(families, state)
        with torch.no_grad():
            model.eta.copy_(torch.tensor([0.2, -0.15, 0.1]))
        panels = {}
        for panel, queries in enumerate([2, 3, 4]):
            query, target, held = torch.rand(queries, 3) * 0.2, torch.rand(2, 3) * 0.2, torch.rand(queries, 3) * 0.2
            own = torch.arange(queries) % 2
            squared = (query[:, None, :] - target[None, :, :]).square()
            squared[torch.arange(queries), own] = (query - held).square()
            raw = torch.ones(queries, 2, 14) * 0.03
            for axis, family in enumerate(families):
                raw[:, :, family] += squared[:, :, axis]
            weights = torch.tensor([1 / (2 * (own == label).sum().item()) for label in own])
            data = CoordinateInputs(query, target, raw, own, families, held, weights)
            panels[str(panel)] = {"inputs": data, "authors": [f"{panel}-a", f"{panel}-b"]}
        return model, panels

    def manual_objective(self, model, panels, regularization):
        losses = []
        for data in panels.values():
            logits = model(data["inputs"])
            row_losses = -logits.log_softmax(-1).gather(1, data["inputs"].own_indices[:, None]).squeeze(1)
            losses.append((row_losses * data["inputs"].loss_weights).sum())
        return torch.stack(losses).mean() + regularization * model.eta.square().sum() / 14

    def test_pooled_gradient_matches_manual_objective_and_finite_differences(self):
        model, panels = self.fixture()
        regularization = 0.7
        actual = backward_panels(model, panels, regularization)
        gradient = model.eta.grad.detach().clone()
        expected = self.manual_objective(model, panels, regularization)
        expected_gradient, = torch.autograd.grad(expected, model.eta)
        torch.testing.assert_close(gradient, expected_gradient, rtol=1e-12, atol=1e-12)
        self.assertAlmostEqual(actual["objective"], expected.item(), places=14)
        self.assertEqual([row["author_weight"] for row in actual["panel_losses"].values()], [1/3] * 3)
        initial = model.eta.detach().clone()
        numeric = []
        for axis in range(3):
            values = []
            for direction in [-1, 1]:
                with torch.no_grad():
                    model.eta.copy_(initial)
                    model.eta[axis] += direction * 1e-6
                    values.append(self.manual_objective(model, panels, regularization).item())
            numeric.append((values[1] - values[0]) / 2e-6)
        torch.testing.assert_close(gradient, torch.tensor(numeric), rtol=1e-6, atol=1e-9)

    def test_one_panel_reduces_to_original_single_panel_loss_and_one_penalty(self):
        model, panels = self.fixture()
        one = {"original": panels["0"]}
        objective = panel_cross_entropy(model, panels["0"]) + 10 * model.penalty()
        expected_gradient, = torch.autograd.grad(objective, model.eta)
        result = backward_panels(model, one, 10)
        self.assertAlmostEqual(result["objective"], objective.item(), places=14)
        torch.testing.assert_close(model.eta.grad, expected_gradient, rtol=0, atol=0)
        self.assertEqual(result["panel_losses"]["original"]["author_weight"], 1)

    def test_panel_batch_partition_does_not_multiply_regularization(self):
        model, panels = self.fixture()
        data = panels["0"]
        one = backward_panels(model, {"a": data}, 10)
        one_gradient = model.eta.grad.clone()
        model.zero_grad(set_to_none=True)
        three = backward_panels(model, {"a": data, "b": data, "c": data}, 10)
        self.assertAlmostEqual(one["objective"], three["objective"], places=14)
        torch.testing.assert_close(one_gradient, model.eta.grad, rtol=1e-12, atol=1e-12)

    def runs(self):
        return [{"status": "complete", "lambda": value, "inherited_seed": seed,
                 "selected_validation": {"macro_author": {"top1": 0.5, "top5": 0.7, "mrr": 0.6}}}
                for value in LAMBDAS for seed in SEEDS]

    def test_complete_twelve_fit_selection_uses_seed_mean_and_stronger_lambda(self):
        rows = self.runs()
        self.assertEqual(choose_lambda(rows)[0], 100)
        for row in rows:
            if row["lambda"] == 0.1:
                row["selected_validation"]["macro_author"]["top1"] = 0.99 if row["inherited_seed"] == 0 else 0.1
            if row["lambda"] == 1:
                row["selected_validation"]["macro_author"]["mrr"] = 0.61
        self.assertEqual(choose_lambda(rows)[0], 1)
        failed = copy.deepcopy(rows)
        failed[-1]["status"] = "failed"
        for invalid in [rows[:-1], failed, rows[:-1] + [rows[0]]]:
            with self.assertRaisesRegex(ValueError, "all twelve"):
                choose_lambda(invalid)


if __name__ == "__main__":
    unittest.main()

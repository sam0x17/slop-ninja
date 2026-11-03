"""Coordinate residual geometry and frozen-spline derivative tests."""
import math
import unittest

import torch

from coordinate_model import CoordinateInputs, CoordinateMetric, LOG_WEIGHT_BOUND


class CoordinateModelTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        torch.set_default_dtype(torch.float64)
        torch.set_num_threads(2)

    def fixture(self):
        torch.manual_seed(87)
        q = torch.rand(3, 4) * 0.2
        p = torch.rand(2, 4) * 0.2
        own = torch.rand(3, 4) * 0.2
        indices = torch.tensor([0, 1, 0])
        families = torch.tensor([0, 0, 1, 1])
        dense = (q[:, None, :] - p[None, :, :]).square()
        dense[torch.arange(3), indices] = (q - own).square()
        base = torch.stack((dense[:, :, :2].sum(-1), dense[:, :, 2:].sum(-1)), -1) + 0.05
        inputs = CoordinateInputs(q, p, base, indices, families, own)
        state = {"theta": torch.tensor([0.6, 0.4]).log(), "log_tau": torch.tensor(math.log(0.5)),
                 "knots": torch.tensor([0.1, 0.3]), "log_slope": torch.tensor([2.0, 0.5]).log()}
        return inputs, CoordinateMetric(families, state), dense

    def test_factorized_correction_matches_dense_squares_and_held_date_override(self):
        inputs, model, dense = self.fixture()
        multipliers = torch.tensor([0.5, 2.0, 1.2, 0.8])
        with torch.no_grad():
            model.eta.copy_(multipliers.log())
        correction = dense * (multipliers - 1)
        expected = inputs.raw_distances + torch.stack((correction[:, :, :2].sum(-1), correction[:, :, 2:].sum(-1)), -1)
        torch.testing.assert_close(model.adjusted_distances(inputs), expected, atol=1e-15, rtol=1e-15)
        self.assertAlmostEqual(inputs.validate_residual_tail()["minimum_unselected_tail"], 0.05)

    def test_zero_eta_exactly_preserves_complete_baseline_and_only_eta_trains(self):
        inputs, model, _ = self.fixture()
        self.assertTrue(torch.equal(model.adjusted_distances(inputs), inputs.raw_distances))
        self.assertTrue(torch.equal(model(inputs), model.score_distances(inputs.raw_distances)))
        self.assertEqual([(name, parameter.numel()) for name, parameter in model.named_parameters()], [("eta", 4)])

    def test_autograd_flows_through_frozen_spline_and_matches_finite_difference(self):
        inputs, model, _ = self.fixture()
        function = lambda eta: torch.func.functional_call(model, {"eta": eta}, (inputs,))
        self.assertTrue(torch.autograd.gradcheck(function, (model.eta,), eps=1e-6, atol=1e-6, rtol=1e-4))
        model(inputs).sum().backward()
        self.assertGreater(model.eta.grad.abs().sum().item(), 0)

    def test_crossing_a_knot_is_continuous_with_correct_one_sided_slopes(self):
        family = torch.tensor([0])
        inputs = CoordinateInputs(torch.tensor([[math.sqrt(0.4)]]), torch.zeros(1, 1),
                                  torch.tensor([[[0.4]]]), torch.tensor([0]), family)
        state = {"theta": torch.zeros(1), "log_tau": torch.tensor(0.0),
                 "knots": torch.tensor([0.5]), "log_slope": torch.tensor([math.log(2.0)])}
        model = CoordinateMetric(family, state)
        crossing = math.log(1.25)
        values, gradients = [], []
        for eta in [crossing - 1e-6, crossing + 1e-6]:
            with torch.no_grad():
                model.eta.fill_(eta)
            model.zero_grad(set_to_none=True)
            value = model(inputs).sum()
            value.backward()
            values.append(value.item())
            gradients.append(model.eta.grad.item())
        self.assertLess(abs(values[0] - values[1]), 2e-6)
        self.assertAlmostEqual(gradients[0], -0.5, places=5)
        self.assertAlmostEqual(gradients[1], -1.0, places=5)

    def test_penalty_balances_families_and_bounds_are_enforced(self):
        _, model, _ = self.fixture()
        model.penalty_groups = [torch.tensor([0, 1, 2]), torch.tensor([3])]
        with torch.no_grad():
            model.eta.copy_(torch.tensor([0.1, 0.1, 0.1, 0.3]))
        self.assertAlmostEqual(model.penalty().item(), 0.05)
        with torch.no_grad():
            model.eta.copy_(torch.tensor([-5.0, 5.0, -1.0, 1.0]))
        model.constrain()
        self.assertTrue(bool((model.eta.abs() <= LOG_WEIGHT_BOUND).all()))
        torch.testing.assert_close(model.eta.exp(), torch.tensor([0.5, 2.0, 0.5, 2.0]), atol=0, rtol=0)

    def test_invalid_tail_is_rejected_instead_of_silently_dropping_unselected_axes(self):
        inputs, _, _ = self.fixture()
        inputs.raw_distances.zero_()
        with self.assertRaisesRegex(ValueError, "exceeds"):
            inputs.validate_residual_tail()


if __name__ == "__main__":
    unittest.main()

"""Numerical and sparse-input checks for explicit-feature neural models."""

import copy
import tempfile
from pathlib import Path
import unittest

import torch
from safetensors.torch import load_model, save_model

from feature_nn import FeatureHead, dense_values, feature_gradient, margin, predict


class FeatureHeadTest(unittest.TestCase):
    def test_sparse_values_preserve_coordinates_and_reject_bad_inputs(self):
        row = {"feature_space_sha256": "a" * 64, "id": "metadata-is-not-a-feature",
               "vector": {"values": [[0, 1.0], [3, 2.5]], "unknown_coordinates": 7,
                          "unavailable_families": ["dependency"]}}
        expected = torch.tensor([[1.0, 0, 0, 2.5]])
        torch.testing.assert_close(dense_values([row], 4, "a" * 64), expected)
        for values in ([[0, 1], [0, 2]], [[-1, 1]], [[4, 1]], [[1, float("nan")]], [[True, 1]]):
            bad = copy.deepcopy(row)
            bad["vector"]["values"] = values
            with self.assertRaises(ValueError):
                dense_values([bad], 4, "a" * 64)
        with self.assertRaisesRegex(ValueError, "another coordinate space"):
            dense_values([row], 4, "b" * 64)

    def test_nonlinear_margin_gradient_matches_central_difference(self):
        torch.manual_seed(71)
        model = FeatureHead(5, 7).double()
        value = torch.tensor([[0.2, -0.3, 0.8, 0.0, 0.5]], dtype=torch.float64)
        logits, gradient = feature_gradient(model, value)
        self.assertTrue(torch.equal(logits, model(value)))
        self.assertTrue(all(p.grad is None for p in model.parameters()))
        for index in range(5):
            displacement = torch.zeros_like(value)
            displacement[0, index] = 1e-5
            with torch.no_grad():
                difference = (margin(model(value + displacement)) - margin(model(value - displacement))) / 2e-5
            self.assertAlmostEqual(float(difference), float(gradient[0, index]), places=9)

    def test_training_step_and_safetensors_reload(self):
        torch.manual_seed(9)
        values = torch.randn(6, 8)
        labels = torch.tensor([0, 1, 2, 0, 1, 2])
        model = FeatureHead(8, 5)
        optimizer = torch.optim.AdamW(model.parameters(), lr=0.001)
        loss = torch.nn.functional.cross_entropy(model(values), labels)
        loss.backward()
        optimizer.step()
        before = predict(model, values, 1)
        with tempfile.TemporaryDirectory() as temporary:
            path = str(Path(temporary) / "weights.safetensors")
            save_model(model, path)
            reloaded = FeatureHead(**model.config)
            load_model(reloaded, path, strict=True)
            torch.testing.assert_close(before, predict(reloaded, values, 1), atol=0, rtol=0)


if __name__ == "__main__":
    torch.set_num_threads(2)
    unittest.main()

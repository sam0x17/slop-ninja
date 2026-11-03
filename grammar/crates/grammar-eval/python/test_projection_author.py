"""Mathematical checks of reference pooling and whole-date gradient exclusion."""
import unittest

import torch

from projection_author import Pool, unit


class ProjectionPoolTests(unittest.TestCase):
    def fixture(self):
        rows = [{"author": author, "date": date, "split": "train"} for author, date in
                [("a", "2001-01-01"), ("a", "2001-01-01"), ("a", "2001-01-02"),
                 ("a", "2001-01-03"), ("b", "2001-01-01"), ("b", "2001-01-02")]]
        raw = torch.tensor([[1.0, 0.3], [0.2, 1.0], [0.8, 0.6], [0.6, 0.8],
                            [0.9, 0.2], [0.2, 0.9]], dtype=torch.float64, requires_grad=True)
        return Pool(rows, ["a", "b"], True), raw

    def test_date_balancing_exclusion_and_gradients(self):
        pool, raw = self.fixture()
        documents = unit(raw)
        prototypes, held = pool.prototypes(documents)
        expected_full = unit(((documents[0] + documents[1]) / 2 + documents[2] + documents[3]).unsqueeze(0) / 3)[0]
        expected_held = unit(((documents[2] + documents[3]) / 2).unsqueeze(0))[0]
        torch.testing.assert_close(prototypes[0], expected_full, atol=1e-14, rtol=1e-14)
        torch.testing.assert_close(held[0], expected_held, atol=1e-14, rtol=1e-14)
        torch.testing.assert_close(held[1], expected_held, atol=1e-14, rtol=1e-14)
        gradient = torch.autograd.grad(held[0, 0], raw)[0]
        torch.testing.assert_close(gradient[:2], torch.zeros_like(gradient[:2]), atol=1e-14, rtol=0)
        self.assertGreater(float(gradient[2:4].abs().sum()), 0)
        torch.testing.assert_close(gradient[4:], torch.zeros_like(gradient[4:]), atol=1e-14, rtol=0)
        changed = raw.detach().clone()
        changed[:2] = torch.tensor([[0.05, 3.0], [9.0, 0.2]], dtype=torch.float64)
        _, changed_held = pool.prototypes(unit(changed))
        torch.testing.assert_close(changed_held[:2], held[:2], atol=1e-14, rtol=1e-14)
        torch.testing.assert_close(pool.loss_weights, torch.tensor([1/12, 1/12, 1/6, 1/6, 1/4, 1/4], dtype=torch.float32))

    def test_query_values_never_enter_evaluation_prototype(self):
        rows = [{"author": author, "date": date, "split": split} for author, date, split in
                [("a", "2001-01-01", "reference"), ("a", "2001-01-01", "reference"),
                 ("a", "2001-01-02", "reference"), ("b", "2001-01-01", "reference"),
                 ("a", "2001-02-01", "query"), ("b", "2001-02-01", "query")]]
        pool = Pool(rows, ["a", "b"], False)
        raw = torch.tensor([[1., 0.], [0., 1.], [1., 0.], [0., 1.], [1., 1.], [1., 1.]])
        expected = unit(torch.tensor([[(0.5 + 1) / 2, 0.5 / 2], [0., 1.]]))
        targets, held = pool.prototypes(unit(raw))
        self.assertIsNone(held)
        torch.testing.assert_close(targets, expected)
        raw[4:] = torch.tensor([[9., 0.1], [0.1, 9.]])
        torch.testing.assert_close(pool.prototypes(unit(raw))[0], targets)

    def test_zero_norm_fails_instead_of_dropping_a_profile(self):
        with self.assertRaisesRegex(ValueError, "zero or nonfinite"):
            unit(torch.zeros(2, 3))


if __name__ == "__main__":
    unittest.main()

"""Offline synthetic loss/gradient qualification. No corpus or model calls."""
import unittest
import torch
import shared_a as shared
from train_last_block_a import Population


class StreamedLossTests(unittest.TestCase):
    def test_global_mask_denominators_match_full_batch_loss_and_gradient(self):
        torch.manual_seed(17)
        rows = [
            {"id": "synthetic-a", "source_group": "work-a", "targets": {"origin": 0, "share": 0., "human": 0, "ai": None}},
            {"id": "synthetic-b", "source_group": "work-a", "targets": {"origin": 2, "share": None, "human": 0, "ai": 1}},
            {"id": "synthetic-c", "source_group": "work-b", "targets": {"origin": 1, "share": 1., "human": None, "ai": 0}},
            {"id": "synthetic-d", "source_group": "work-c", "targets": {"origin": None, "share": .8, "human": 1, "ai": None}},
            {"id": "synthetic-e", "source_group": "work-c", "targets": {"origin": 2, "share": .9, "human": 1, "ai": 0}},
        ]
        population = Population.__new__(Population)
        population.rows = rows
        population.weights = shared.query_weights(rows, "family_origin_balanced")
        population.denominators = {k: sum(w for r, w in zip(rows, population.weights) if r["targets"][k] is not None)
                                   for k in ("origin", "share", "human", "ai")}
        x = torch.randn(5, 4)
        model = shared.SharedA(4, 2, 2)
        batch = {"x": x, "weight": torch.tensor(population.weights),
                 "y": torch.tensor([r["targets"]["origin"] if r["targets"]["origin"] is not None else -1 for r in rows]),
                 "share": torch.tensor([r["targets"]["share"] or 0. for r in rows]),
                 "share_mask": torch.tensor([r["targets"]["share"] is not None for r in rows])}
        for k in ("human", "ai"):
            batch[k] = torch.tensor([r["targets"][k] if r["targets"][k] is not None else -1 for r in rows])
        expected = shared.loss(model, batch, "joint")
        expected.backward()
        gradients = {n: p.grad.clone() for n, p in model.named_parameters()}
        model.zero_grad(set_to_none=True)
        observed = 0.
        for i in range(len(rows)):
            term = population.objective(model(x[i:i+1]), i)
            observed += float(term.detach())
            term.backward()
        self.assertAlmostEqual(float(expected.detach()), observed, places=6)
        for n, p in model.named_parameters():
            torch.testing.assert_close(p.grad, gradients[n], atol=1e-6, rtol=1e-5)


if __name__ == "__main__":
    unittest.main()

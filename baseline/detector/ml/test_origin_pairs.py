"""Numerical and input-boundary checks for the paired training objective."""
import json
import math
from pathlib import Path
import tempfile
import unittest

import torch
from common import sha256
from train import load_origin_pairs, paired_origin_loss


class OriginPairsTest(unittest.TestCase):
    def test_equal_predictions_reduce_to_supervised_loss(self):
        logits = torch.tensor([[2., 0., -1.], [-1., 1., 2.]], dtype=torch.float64)
        labels = torch.tensor([0, 2])
        weights = torch.ones(2, dtype=torch.float64)
        loss, js = paired_origin_loss(logits, logits.clone(), labels, weights, 0.5)
        self.assertAlmostEqual(float(js), 0.0, places=12)
        self.assertAlmostEqual(float(loss), float(torch.nn.functional.cross_entropy(logits, labels)), places=12)

    def test_symmetric_finite_divergence_updates_both_branches(self):
        a = torch.tensor([[3., -1., 0.]], dtype=torch.float64, requires_grad=True)
        b = torch.tensor([[-1., 3., 0.]], dtype=torch.float64, requires_grad=True)
        labels, weights = torch.tensor([1]), torch.ones(1, dtype=torch.float64)
        loss, js = paired_origin_loss(a, b, labels, weights, 0.5)
        swapped, reverse_js = paired_origin_loss(b, a, labels, weights, 0.5)
        self.assertAlmostEqual(float(js.detach()), float(reverse_js.detach()), places=12)
        self.assertAlmostEqual(float(loss.detach()), float(swapped.detach()), places=12)
        self.assertTrue(0 < float(js.detach()) < math.log(2))
        loss.backward()
        for value in [a.grad, b.grad]:
            self.assertTrue(torch.isfinite(value).all())
            self.assertGreater(float(value.abs().sum()), 0)
        extreme = torch.tensor([[1000., -1000., -1000.]])
        other = torch.tensor([[-1000., 1000., -1000.]])
        extreme_loss, extreme_js = paired_origin_loss(extreme, other, labels, weights.float(), 0.5)
        self.assertTrue(torch.isfinite(extreme_loss))
        self.assertAlmostEqual(float(extreme_js), math.log(2), places=6)

    def test_pair_manifest_rejects_changed_inputs_labels_and_coverage(self):
        rows = [{"id": "human", "source_group": "s", "label": 0},
                {"id": "draft", "source_group": "s", "label": 1},
                {"id": "revision", "source_group": "s", "label": 1}]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw, whitespace, path = root / "raw", root / "space", root / "pairs.json"
            raw.write_text("input binding fixture\n")
            whitespace.write_text("alternate binding fixture\n")
            manifest = {"schema": "slop_ninja_origin_pairs_v1", "train_sha256": sha256(raw),
                        "whitespace_sha256": sha256(whitespace), "records": 3,
                        "pairs": {"human": "human", "draft": "revision", "revision": "draft"},
                        "model_revision_pairs": 2, "formatting_only_pairs": 1}
            path.write_text(json.dumps(manifest))
            pairs, _ = load_origin_pairs(path, raw, whitespace, rows)
            self.assertEqual(pairs["draft"], "revision")
            for invalid in [{"human": "draft", "draft": "human", "revision": "revision"},
                            {"human": "human", "draft": "revision", "revision": "revision"}]:
                path.write_text(json.dumps({**manifest, "pairs": invalid}))
                with self.assertRaises(ValueError):
                    load_origin_pairs(path, raw, whitespace, rows)
            path.write_text(json.dumps(manifest))
            raw.write_text("changed input\n")
            with self.assertRaises(ValueError):
                load_origin_pairs(path, raw, whitespace, rows)


if __name__ == "__main__":
    unittest.main()

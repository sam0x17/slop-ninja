"""Offline checks of export_a_onnx.py's equal-weights refusal. Synthetic tensors only; no model files, no
model/provider calls or corpus input."""
from types import SimpleNamespace
import unittest

import torch

from export_a_onnx import TOWER_FILE, equal_weights, graph_weights


def module(**tensors):
    return SimpleNamespace(state_dict=lambda: dict(tensors))


class EqualWeights(unittest.TestCase):
    def test_equal_values_of_one_dtype_and_shape_are_merged(self):
        g = torch.Generator().manual_seed(17)
        a, b = torch.randn(8, generator=g), torch.randn(8, generator=g)
        self.assertEqual(equal_weights([("a", a), ("b", b)]), [])
        self.assertEqual(equal_weights([("a", a), ("b", b), ("c", a.clone())]), [("a", "c")])
        # Values are compared, not bits: 0 and -0 are equal.
        z, nz = torch.zeros(8), torch.zeros(8)
        nz[3] = -0.0
        self.assertEqual(equal_weights([("z", z), ("nz", nz)]), [("z", "nz")])
        # Another dtype or shape is another initializer.
        self.assertEqual(equal_weights([("a", a), ("d", a.double())]), [])
        m = torch.randn(4, 8, generator=g)
        self.assertEqual(equal_weights([("m", m), ("row", m[0].clone()), ("r", m.reshape(8, 4).clone())]), [])

    def test_graph_weights_name_release_files_and_skip_the_classifier(self):
        w = torch.ones(2)
        model = SimpleNamespace(encoder=module(**{"model.final_norm.weight": w, "classifier.weight": w}),
                                heads=module(**{"origin.bias": w * 2, "text_mean": w * 3}))
        tower = (module(**{"norm.weight": w.clone()}), 18)
        names = [n for n, _ in graph_weights(model, tower)]
        self.assertEqual(names, ["encoder.safetensors model.final_norm.weight", "heads.json origin.bias",
                                 "heads.json text_mean", f"{TOWER_FILE} span_tower.norm.weight"])
        self.assertEqual(equal_weights(graph_weights(model, tower)),
                         [("encoder.safetensors model.final_norm.weight", f"{TOWER_FILE} span_tower.norm.weight")])
        self.assertEqual(equal_weights(graph_weights(model, None)), [])


if __name__ == "__main__":
    unittest.main()

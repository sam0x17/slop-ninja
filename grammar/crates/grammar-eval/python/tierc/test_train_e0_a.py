"""Offline checks of the E0 control trainer (synthetic tensors; no corpus, model or GPU).
Run with the release trainer modules on PYTHONPATH."""
import copy
import json
from pathlib import Path
from types import SimpleNamespace
import unittest

import torch
from torch.nn import functional as F

import train_e0_a as e0

RECIPE = {"schema": e0.RECIPE_SCHEMA,
          "fit": {"seeds": [17, 29, 41], "max_epochs": 4, "logical_batch_size": 64, "heads_learning_rate": .0003,
                  "adapted_final_block_learning_rate": .00001, "weight_decay": .01, "gradient_clip_norm": 5.0,
                  "attribution_loss_weight": .25, "cuda_memory_fraction": .50, "selection_split": "dev_iter",
                  "train_weighting": {"scheme": "assembler_row_weight_v1", "positive_mass": None}},
          "prefix_cache": {"min_free_bytes": 420 * 10 ** 9}}


def meta(origin, share, label):
    return SimpleNamespace(origin=origin, share=share, label=label)


def output(logits, q, ai):
    logits = torch.tensor([logits], requires_grad=True)
    p = logits.softmax(-1)
    return {"origin_logits": logits, "origin": p, "share": p[:, 1] + p[:, 2] * q, "ai_logits": torch.tensor([ai])}


class RecipeTests(unittest.TestCase):
    def test_recipe_keeps_stage3_schedule_under_the_epoch_cap(self):
        e0.validate_recipe(RECIPE)
        for path, value in ((("fit", "max_epochs"), 12), (("fit", "logical_batch_size"), 32),
                            (("fit", "gradient_clip_norm"), 1.0), (("fit", "selection_split"), "dev_select")):
            r = copy.deepcopy(RECIPE)
            r[path[0]][path[1]] = value
            with self.assertRaises(ValueError):
                e0.validate_recipe(r)
        r = copy.deepcopy(RECIPE)
        r["fit"]["train_weighting"] = {"scheme": "family_origin_balanced"}
        with self.assertRaises(ValueError):
            e0.validate_recipe(r)

    def test_checked_in_recipe_is_valid(self):
        path = Path(__file__).with_name("e0-recipe-v1.json")
        if path.exists():
            e0.validate_recipe(json.loads(path.read_text()))


class ObjectiveTests(unittest.TestCase):
    def test_stage3_terms_with_row_weights_and_task_denominators(self):
        ms = [meta(0, 0., None), meta(1, 1., "g1"), meta(2, .4, "unrostered"), meta(2, .6, "g0")]
        ws = [2.0, 1.0, 0.5, 1.5]
        obj = e0.Objective(ms, ws, {"g0": 0, "g1": 1}, .25)
        self.assertEqual(obj.den, {"origin": 5.0, "share": 5.0, "ai": 2.5})
        outs = [output([.2, -.1, .3], .5, [.1, .9]), output([-1., 2., .0], .2, [.3, -.2]),
                output([.0, .5, 1.], .7, [.0, .0]), output([.4, .1, -.3], .1, [1.2, .4])]
        total = sum(float(obj(o, i)) for i, o in enumerate(outs))
        manual = 0.
        for m, w, o in zip(ms, ws, outs):
            y = torch.tensor([m.origin])
            manual += (float(F.cross_entropy(o["origin_logits"], y)) +
                       (float(o["origin"][0, 1:].sum()) - float(m.origin != 0)) ** 2) * w / 5.0
            manual += (float(o["share"][0]) - m.share) ** 2 * w / 5.0
            if m.label in ("g0", "g1"):
                manual += .25 * float(F.cross_entropy(o["ai_logits"], torch.tensor([{"g0": 0, "g1": 1}[m.label]]))) * w / 2.5
        self.assertAlmostEqual(total, manual, places=6)

    def test_balanced_brier_and_earliest_tie(self):
        pop = SimpleNamespace(meta=[meta(0, 0., None), meta(1, 1., "g")], weights=[1., 3.],
                              denominators={"origin": 4.})
        self.assertAlmostEqual(e0.balanced_brier([.5, .5], pop), .25)
        self.assertAlmostEqual(e0.balanced_brier([.0, 1.], pop), 0.)
        self.assertTrue(e0.better(.1, float("inf")))
        self.assertFalse(e0.better(.1, .1))


class BudgetTests(unittest.TestCase):
    def test_first_epoch_estimate_from_sibling_seeds(self):
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "E0-seed-29").mkdir()
            self.assertIsNone(e0.sibling_epoch_seconds(d / "E0-seed-29"))
            (d / "E0-seed-17").mkdir()
            (d / "E0-seed-17" / "history.json").write_text(json.dumps(
                [{"epoch": 0, "epoch_seconds": 30.}, {"epoch": 1, "epoch_seconds": 2000.}]))
            self.assertEqual(e0.sibling_epoch_seconds(d / "E0-seed-29"), 2000.)
            (d / "E0-seed-17" / "history.json").write_text(json.dumps(
                [{"epoch": 0, "epoch_seconds": 30.}, {"epoch": 1, "epoch_seconds": 2000.},
                 {"epoch": 2, "epoch_seconds": 1400.}, {"epoch": 3, "epoch_seconds": 1500.}]))
            self.assertEqual(e0.sibling_epoch_seconds(d / "E0-seed-29"), 1500.)


if __name__ == "__main__":
    unittest.main()

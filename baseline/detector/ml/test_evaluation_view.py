"""Offline checks for the Rust-to-ML observation selection handoff."""

import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

from common import class_counts, sha256
from train import evaluation_view, source_origin_weights


class EvaluationViewTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.shard = Path(self.temporary.name) / "calibration.jsonl"
        self.path = Path(self.temporary.name) / "view.json"
        self.rows = []
        families = []
        for group in ("synthetic-a", "synthetic-b"):
            family = {"source_group": group}
            for role, label in (("human", 0), ("draft", 1), ("model", 1), ("mixed", 2)):
                identifier = f"{group}-{role}"
                digest = hashlib.sha256(identifier.encode()).hexdigest()
                self.rows.append({"id": identifier, "source_group": group, "label": label, "hash": digest})
                if role != "draft":
                    family[role] = {"id": identifier, "text_sha256": digest}
            families.append(family)
        # These are invented tensor-handoff rows, not admitted OriginRecords.
        self.shard.write_text(json.dumps(self.rows))
        self.view = {"schema": "slop_ninja_origin_evaluation_view_v1", "split": "calibration",
                     "records_sha256": sha256(self.shard), "families": families}

    def select(self, view):
        self.path.write_text(json.dumps(view))
        return evaluation_view(self.path, self.shard, "calibration", self.rows)

    def test_ancestors_remain_available_but_do_not_change_calibration_mixture(self):
        original = copy.deepcopy(self.rows)
        selected, binding = self.select(self.view)
        self.assertEqual(class_counts(self.rows), [2, 4, 2])
        self.assertEqual(class_counts(selected), [2, 2, 2])
        self.assertEqual([row["id"] for row in selected],
                         [f"{group}-{role}" for group in ("synthetic-a", "synthetic-b")
                          for role in ("human", "model", "mixed")])
        self.assertEqual(self.rows, original)
        self.assertEqual(binding["sha256"], sha256(self.path))
        self.assertEqual(binding["selected_rows"], 6)
        # Training can still expose both model stages with equal family-origin mass.
        weights = source_origin_weights(self.rows)
        masses = [sum(weights[row["id"]] for row in self.rows if row["label"] == label)
                  for label in range(3)]
        self.assertEqual(masses, [8 / 3] * 3)

    def test_changed_shard_omitted_family_and_false_row_bindings_fail(self):
        changes = [
            lambda v: v.update(records_sha256="0" * 64),
            lambda v: v.update(split="test"),
            lambda v: v["families"].pop(),
            lambda v: v["families"].reverse(),
            lambda v: v["families"][0].update(model=v["families"][1]["model"]),
            lambda v: v["families"][0].update(model=v["families"][0]["mixed"]),
            lambda v: v["families"][0]["model"].update(text_sha256="0" * 64),
        ]
        for index, change in enumerate(changes):
            with self.subTest(change=index):
                view = copy.deepcopy(self.view)
                change(view)
                with self.assertRaises(ValueError):
                    self.select(view)


if __name__ == "__main__":
    unittest.main()

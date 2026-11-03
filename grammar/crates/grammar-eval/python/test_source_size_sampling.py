"""Synthetic numeric-only checks; never access corpus or model services."""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

import torch
import warm_start_a as warm


class SourceSizeSamplingTests(unittest.TestCase):
    def setup_protocol(self, root):
        init = root / "init.json"
        init.write_text("{}")
        rows = []
        for stratum in ("original", *warm.SOURCE_STRATA):
            for group in range(1 if stratum == "original" else 13):
                family = f"{stratum}:{group}"
                # Deliberately unequal descendants and multiple origin cells.
                for variant in range(1 + group % 3):
                    rows.append({"id": f"{family}:{variant}", "source_group": family,
                                 "frozen_split": "train", "purpose": "query",
                                 "targets": {"origin": variant, "share": variant / 2,
                                             "human": 0 if variant != 1 else None, "ai": 0 if variant else None}})
        selected = set()
        for st in warm.SOURCE_STRATA:
            groups = [f"{st}:{g}" for g in range(13)]
            groups.sort(key=lambda g: (hashlib.sha256(("slop_ninja_a_source_size_curve_v1\0" + g).encode()).hexdigest(), g))
            selected.update(groups[:12])
        q = []
        for r in rows:
            st = r["source_group"].rsplit(":", 1)[0]
            q.append({"id": r["id"], "source_group": r["source_group"], "stratum": st,
                      "selected": st == "original" or r["source_group"] in selected, "weight": 0.})
        for st in ("original", *warm.SOURCE_STRATA):
            indices = [i for i, r in enumerate(q) if r["stratum"] == st and r["selected"]]
            weights = warm.shared.query_weights([rows[i] for i in indices], "family_origin_balanced")
            for i, w in zip(indices, weights):
                q[i]["weight"] = w * (.5 if st == "original" else .1)
        p = {"schema": warm.SOURCE_SIZE_SCHEMA, "apply_to": "train_only", "epochs": 250,
             "seeds": [17, 29, 41], "source_strata": list(warm.SOURCE_STRATA),
             "original_cohort_mass": .5, "added_stratum_mass": .1, "within_stratum": "family_origin_balanced",
             "normalization_fitted": False, "profiles_changed": False, "final_test_opened": False,
             "rank_domain": "slop_ninja_a_source_size_curve_v1", "level": "12", "allowed_modes": ["text"],
             "numeric_manifest_sha256": {"text": "numeric"}, "parent_manifest_sha256": {"text": "parent"},
             "extension_dataset_manifest_sha256": "dataset", "initialization_sha256": warm.sha(init),
             "original_rows": 1, "total_train_rows": len(rows), "rows": q, "selected_added_families": sorted(selected)}
        manifest = {"training_extension": {"parent_manifest_sha256": "parent",
                                           "extension_dataset_manifest_sha256": "dataset",
                                           "original_partitions": {"train": {"rows": 1}}}}
        binding = {"train_extension": True, "manifest_sha256": "numeric"}
        batch = {"rows": rows, "x": torch.arange(len(rows) * 3, dtype=torch.float32).reshape(-1, 3),
                 "weight": torch.ones(len(rows)), "y": torch.tensor([r["targets"]["origin"] for r in rows]),
                 "share": torch.zeros(len(rows)), "share_mask": torch.ones(len(rows), dtype=torch.bool),
                 "human": torch.zeros(len(rows), dtype=torch.long), "ai": torch.zeros(len(rows), dtype=torch.long)}
        return p, manifest, binding, batch, init

    def call(self, root, p, manifest, binding, batch, init):
        path = root / "sampling.json"
        path.write_text(json.dumps(p))
        return warm.source_size_sampling(path, manifest, binding, "text", batch, init)

    def test_whole_families_and_unselected_features_never_enter_optimizer(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            p, m, b, batch, init = self.setup_protocol(root)
            actual, receipt = self.call(root, p, m, b, batch, init)
            keep = [i for i, r in enumerate(p["rows"]) if r["selected"]]
            self.assertEqual(receipt["selected_train_families"], 61)
            self.assertEqual([r["id"] for r in actual["rows"]], [batch["rows"][i]["id"] for i in keep])
            self.assertTrue(torch.equal(actual["x"], batch["x"][keep]))
            before = actual["x"].clone()
            for i, r in enumerate(p["rows"]):
                if not r["selected"]:
                    batch["x"][i] = float("nan")
            changed, _ = self.call(root, p, m, b, batch, init)
            self.assertTrue(torch.equal(changed["x"], before))
            self.assertAlmostEqual(float(actual["weight"].sum()), 1., places=6)
            self.assertAlmostEqual(float(actual["weight"][0]), .5, places=6)

    def test_changed_selection_weight_split_or_binding_is_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            p, m, b, batch, init = self.setup_protocol(root)
            for mutate in (lambda q: q["rows"][1].update(selected=not q["rows"][1]["selected"]),
                           lambda q: q["rows"][0].update(weight=.6),
                           lambda q: q["numeric_manifest_sha256"].update(text="changed"),
                           lambda q: q.update(final_test_opened=True),
                           lambda q: q.update(allowed_modes=["text", "combined"])):
                bad = copy.deepcopy(p)
                mutate(bad)
                with self.assertRaises(ValueError):
                    self.call(root, bad, m, b, batch, init)
            batch["rows"][0]["frozen_split"] = "test"
            with self.assertRaises(ValueError):
                self.call(root, p, m, b, batch, init)


if __name__ == "__main__":
    unittest.main()

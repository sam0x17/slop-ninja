import math
import unittest

from unslop.statistics import contrast


def sample(counts, total=None, docs=5):
    return {"extractor": "test", "family": "word", "counts": counts,
            "total": total if total is not None else sum(counts.values()),
            "document_counts": {k: docs for k in counts}, "group_counts": {k: docs for k in counts}}


class StatisticsTests(unittest.TestCase):
    def test_swapping_corpora_reverses_scores(self):
        a, b = sample({"rare": 1, "common": 99}), sample({"rare": 10, "common": 90})
        first = {r["feature"]: r for r in contrast(a, b)}
        second = {r["feature"]: r for r in contrast(b, a)}
        for key in first:
            self.assertAlmostEqual(first[key]["z_heuristic"], -second[key]["z_heuristic"])
            self.assertAlmostEqual(first[key]["smoothed_lift"], 1 / second[key]["smoothed_lift"])

    def test_unseen_feature_finite_and_singletons_flagged(self):
        rows = contrast(sample({"singleton": 1, "other": 100}, docs=1), sample({"other": 100}))
        rare = next(r for r in rows if r["feature"] == "singleton")
        self.assertTrue(math.isfinite(rare["z_heuristic"]))
        self.assertFalse(rare["enough_evidence"])

    def test_equal_counts_produce_zero_difference(self):
        rows = contrast(sample({"x": 10, "y": 20}), sample({"x": 10, "y": 20}))
        self.assertTrue(all(r["z_heuristic"] == 0 for r in rows))

    def test_constructions_use_sentence_denominator(self):
        a, b = sample({"passive": 3, "relative": 4}, total=5), sample({"passive": 1, "relative": 4}, total=5)
        a["family"] = b["family"] = "construction"
        passive = next(r for r in contrast(a, b) if r["feature"] == "passive")
        self.assertEqual(passive["left_per_1000"], 600)

    def test_invalid_denominator_and_version_rejected(self):
        with self.assertRaises(ValueError):
            contrast(sample({"x": 3}, total=2), sample({"x": 1}))
        with self.assertRaises(ValueError):
            contrast(sample({"x": 3}), {**sample({"x": 1}), "extractor": "other"})

    def test_many_variants_of_one_source_do_not_meet_evidence_floor(self):
        a, b = sample({"x": 90, "y": 10}, docs=100), sample({"x": 1, "y": 9}, docs=10)
        a["group_counts"] = b["group_counts"] = {"x": 1, "y": 1}
        self.assertTrue(all(not r["enough_evidence"] for r in contrast(a, b)))


if __name__ == "__main__":
    unittest.main()

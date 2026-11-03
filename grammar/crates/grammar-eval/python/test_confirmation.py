"""Confirmation protocol and eligibility checks on synthetic identities only."""
import copy
import unittest

import numpy as np

from confirm_frontier import (CHECKPOINT_POLICY, MODELS, SCHEMA, aggregate_seeds,
                              validate_eligibility, validate_protocol)


def protocol_fixture():
    return {"schema": SCHEMA, "models": MODELS.copy(), "seeds": [0, 17, 29], "split": "test",
            "checkpoint_policy": CHECKPOINT_POLICY,
            "primary": {"left": "spline_45", "right": "diagonal_15", "metric": "macro_author.top1"},
            "bootstrap": {"replicates": 2000, "seed": "0x6a09e667f3bcc909", "percentile_indices_zero_based": [49, 1949]},
            "device": "cpu", "dtype": "float64", "threads": 8,
            "cohort": {"author_ids": [f"fresh-{i:03}" for i in range(100)],
                       "excluded_prior_author_ids": [f"old-{i:03}" for i in range(300)]}}


class ConfirmationTests(unittest.TestCase):
    def test_protocol_rejects_changed_models_seeds_split_and_prior_overlap(self):
        protocol = protocol_fixture()
        validate_protocol(protocol)
        for key, value in [("models", ["spline_60", "diagonal_15", "tied_8"]),
                           ("seeds", [0]), ("split", "dev"), ("dtype", "float32")]:
            changed = copy.deepcopy(protocol)
            changed[key] = value
            with self.assertRaises(ValueError):
                validate_protocol(changed)
        changed = copy.deepcopy(protocol)
        changed["cohort"]["excluded_prior_author_ids"][0] = "fresh-000"
        changed["cohort"]["excluded_prior_author_ids"].sort()
        with self.assertRaisesRegex(ValueError, "overlap"):
            validate_protocol(changed)

    def fixture(self):
        protocol = protocol_fixture()
        authors = protocol["cohort"]["author_ids"]
        removed = {authors[-1]: ["only one eligible training date"]}
        audit = []
        for author in authors:
            for split in ["train", "dev", "test"]:
                audit.append({"post": {"id": f"{author}:{split}", "author_id": author, "split": split},
                              "status": "excluded_all_variants" if author in removed else "eligible"})
        audit[-4]["status"] = "excluded_all_variants"  # fresh-098 test
        excluded = [{"id": authors[-2] + ":test", "split": "test"}]
        eligibility = {"schema": "unslop-spline-confirmation-eligibility-v1", "protocol_sha256": "frozen",
                       "no_performance_based_exclusions": True, "eligible_author_ids": authors[:-1],
                       "excluded_authors": removed, "excluded_posts": excluded,
                       "candidate_authors": 99, "train_posts": 99, "dev_posts": 99, "test_posts": 98}
        exclusions = {"input_authors": 100, "candidate_authors": 99,
                      "eligible_author_ids": authors[:-1], "excluded_authors": removed}
        return protocol, eligibility, exclusions, audit

    def test_eligibility_requires_exact_frozen_author_and_post_exclusions(self):
        protocol, eligibility, exclusions, audit = self.fixture()
        self.assertEqual(len(validate_eligibility(protocol, "frozen", eligibility, exclusions, audit)), 99)
        for mutation in ["post", "author", "count", "hash", "duplicate"]:
            p, e, x, a = copy.deepcopy((protocol, eligibility, exclusions, audit))
            if mutation == "post":
                a[2]["status"] = "excluded_all_variants"
            elif mutation == "author":
                x["excluded_authors"] = {}
            elif mutation == "count":
                e["test_posts"] += 1
            elif mutation == "hash":
                e["protocol_sha256"] = "changed"
            else:
                a.append(copy.deepcopy(a[0]))
            with self.assertRaises(ValueError):
                validate_eligibility(p, "frozen", e, x, a)

    def test_seed_aggregation_averages_metrics_instead_of_selecting_best_seed(self):
        arrays = {0: np.ones((2, 4)), 17: np.zeros((2, 4)), 29: np.zeros((2, 4))}
        result = aggregate_seeds(arrays, ["author-a", "author-b"])
        self.assertEqual(result["macro_author"]["top1"], 1 / 3)
        del arrays[29]
        with self.assertRaises(ValueError):
            aggregate_seeds(arrays, ["author-a", "author-b"])


if __name__ == "__main__":
    unittest.main()

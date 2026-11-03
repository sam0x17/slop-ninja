"""Synthetic parameter, selection, separation and stratified-bootstrap tests."""
import copy
import tempfile
import unittest
from pathlib import Path

import numpy as np
import torch

from author_selection import select_architectures, stratified_difference
from author_selection_io import load_cohort
from author_selection_models import CONFIGS, fit_knots, included_families, make_model, prior_weights
from frontier_common import METRIC_NAMES, sha256, write_json


FAMILIES = sorted(["word", "word_bigram", "pos", "pos_tag", "pos_bigram", "pos_trigram", "dependency",
                   "dependency_path_2", "dependency_path_3", "ordered_head_frame", "clause_head_frame",
                   "morphology_bundle", "syntax_load", "syntax_sentence_load"])


class AuthorSelectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        torch.set_num_threads(2)
        torch.set_default_dtype(torch.float64)
        torch.manual_seed(8)
        cls.x = torch.rand(20, 4, 14)
        cls.knots = fit_knots(cls.x)["knots"]

    def test_subset_parameter_counts_and_prior_are_real(self):
        self.assertEqual([row["parameters"] for row in CONFIGS], [3, 33, 13, 43, 15, 45])
        for config in CONFIGS:
            model = make_model(config, FAMILIES, self.knots, 0)
            included = included_families(FAMILIES, config["subset"])
            indices = [FAMILIES.index(name) for name in included]
            expected = -(self.x[:, :, indices] * prior_weights(included)).sum(-1) / 0.1
            self.assertEqual(len(model.theta), len(included))
            self.assertEqual(sum(parameter.numel() for parameter in model.parameters()), config["parameters"])
            torch.testing.assert_close(model(self.x), expected, rtol=1e-13, atol=1e-13)
            changed = self.x.clone()
            for index, family in enumerate(FAMILIES):
                if family not in included:
                    changed[:, :, index] += 100
            torch.testing.assert_close(model(changed), model(self.x), atol=0, rtol=0)

    def test_all_six_autograd_models_match_finite_differences(self):
        for config in CONFIGS:
            model = make_model(config, FAMILIES, self.knots, 17)
            named = list(model.named_parameters())
            function = lambda *parameters: torch.func.functional_call(model, dict(zip([name for name, _ in named], parameters)), (self.x[:2, :2],))
            self.assertTrue(torch.autograd.gradcheck(function, tuple(p for _, p in named), eps=1e-6, atol=1e-5, rtol=1e-4))

    def test_knots_are_subset_specific_and_do_not_use_unrelated_columns(self):
        words = [FAMILIES.index(name) for name in included_families(FAMILIES, "words")]
        grammar = [index for index in range(14) if index not in words]
        before = fit_knots(self.x[:, :, words])
        changed = self.x.clone()
        changed[:, :, grammar] += 1000
        self.assertEqual(before, fit_knots(changed[:, :, words]))
        after = fit_knots(changed[:, :, grammar])
        self.assertNotEqual(before["knots"], after["knots"])
        self.assertEqual(len(before["knots"]), 30)
        self.assertEqual(before["knots"], sorted(set(before["knots"])))

    def test_architecture_selection_uses_three_seed_means_then_smaller_count(self):
        runs = []
        for config in CONFIGS:
            for seed, score in zip([0, 17, 29], [0.5, 0.5, 0.5] if config["kind"] == "linear" else [1.0, 0.1, 0.1]):
                runs.append({"config": config, "seed": seed, "status": "complete",
                             "selected_validation": {"macro_author": dict.fromkeys(METRIC_NAMES, score)}})
        selected, _ = select_architectures(runs)
        self.assertTrue(all(name.endswith("_linear") for name in selected.values()))
        for row in runs:
            row["selected_validation"]["macro_author"] = dict.fromkeys(METRIC_NAMES, 0.5)
        self.assertEqual(select_architectures(runs)[0], selected)
        for row in runs:
            if row["config"]["subset"] == "words" and row["config"]["kind"] == "linear":
                row["status"] = "failed"
        with self.assertRaisesRegex(ValueError, "all eighteen"):
            select_architectures(runs)

    def test_stratification_preserves_gallery_sizes_and_equal_author_weight(self):
        def row(value):
            return dict.fromkeys(METRIC_NAMES, value)
        left = {"gallery-z": {"a": row(1.0)}, "gallery-a": {"b": row(0), "c": row(0), "d": row(0)}}
        right = {gallery: {author: row(0) for author in authors} for gallery, authors in left.items()}
        result = stratified_difference(left, right)
        self.assertEqual(result["gallery_order"], ["gallery-a", "gallery-z"])
        self.assertEqual(result["gallery_author_counts"], {"gallery-a": 3, "gallery-z": 1})
        self.assertEqual(result["metrics"]["top1"], {"mean_difference": 0.25, "percentile_95": [0.25, 0.25]})
        malformed = copy.deepcopy(right)
        del malformed["gallery-a"]["d"]
        with self.assertRaises(ValueError):
            stratified_difference(left, malformed)

    def test_validation_loader_never_opens_test_queries_or_report(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            all_authors = [f"author-{index:03}" for index in range(100)]
            authors = all_authors[:2]
            removed = {author: ["synthetic insufficient support"] for author in all_authors[2:]}
            protocol = {"source_space_id": "space", "reference_space_sha256": "space-sha", "feature_schema": "features", "parser_identity": "parser"}
            descriptor = {"id": "validation", "author_ids": all_authors, "summary_sha256": "source-sha"}
            fresh_protocol = {"input_bindings": {"summary_sha256": "source-sha"},
                              "frozen_selection": {"source_space_id": "space", "source_space_sha256": "space-sha"},
                              "feature_schema": "features", "parser_identity": "parser"}
            write_json(directory / "protocol.json", fresh_protocol)
            write_json(directory / "source-summary.json", {"authors": [{"author_id": author} for author in all_authors]})
            write_json(directory / "space.json", {"family_schemas": dict.fromkeys(FAMILIES, {}), "feature_schema": "features", "parser_identity": "parser"})
            audit, queries = [], []
            for author in authors:
                for split in ["train", "dev", "test"]:
                    post = {"id": f"{author}:{split}", "author_id": author, "split": split}
                    audit.append({"post": post, "status": "eligible"})
                    if split == "dev":
                        queries.append({"post": post, "content_matched_impostor": next(other for other in authors if other != author),
                                        "unweighted_squared_family_distances": {a: dict.fromkeys(FAMILIES, 0.1) for a in authors}})
            write_json(directory / "annotation-audit.json", audit)
            write_json(directory / "author-exclusions.json", {"input_authors": 100, "candidate_authors": 2, "eligible_author_ids": authors, "excluded_authors": removed})
            import json
            (directory / "dev-queries.jsonl").write_text("\n".join(json.dumps(row) for row in queries) + "\n")
            hashes = {path.name: sha256(path) for path in directory.iterdir()}
            write_json(directory / "implementation.json", {"schema": "unslop-author-evaluation-implementation-v1", "artifacts_sha256": hashes})
            eligible = {"role": "validation", "eligible_author_ids": authors, "excluded_authors": removed, "excluded_posts": [],
                        "candidate_authors": 2, "train_posts": 2, "dev_posts": 2, "test_posts": 2,
                        "rust_implementation_sha256": sha256(directory / "implementation.json"),
                        "annotation_audit_sha256": sha256(directory / "annotation-audit.json"),
                        "author_exclusions_sha256": sha256(directory / "author-exclusions.json")}
            result = load_cohort(directory, descriptor, eligible, protocol, FAMILIES, "validation")
            self.assertEqual(result["x"].shape, (2, 2, 14))
            self.assertFalse((directory / "test-queries.jsonl").exists())
            self.assertFalse((directory / "report.json").exists())
            (directory / "dev-queries.jsonl").write_text("changed")
            with self.assertRaisesRegex(ValueError, "Rust artifact changed"):
                load_cohort(directory, descriptor, eligible, protocol, FAMILIES, "validation")


if __name__ == "__main__":
    unittest.main()

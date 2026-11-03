import copy
from pathlib import Path
import tempfile
import unittest

from unslop.features import extract
from unslop.store import add_document, add_run, connect, digest, profile, status


def document(text="A mind changes. A person remembers.", **kwargs):
    return {"text": text, "corpus": "human-sample", "source_kind": "human",
            "domain": "philosophy", "register": "essay", "group_id": "source-1", "split": "train", **kwargs}


class StoreTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.db = connect(Path(self.directory.name) / "corpus.sqlite3")

    def tearDown(self):
        self.db.close()
        self.directory.cleanup()

    def add(self, **kwargs):
        doc = document(**kwargs)
        return add_document(self.db, doc, extract(doc["text"]))

    def test_dedup_does_not_double_word_counts(self):
        first = self.add()
        again = self.add()
        self.assertEqual(first, (1, True))
        self.assertEqual(again, (1, False))
        result = profile(self.db, "human-sample")
        self.assertEqual(result["counts"]["a"], 2)
        self.assertEqual(result["total"], 6)
        self.assertEqual(self.db.execute("SELECT total FROM corpus_totals WHERE family='word'").fetchone()[0], 6)

    def test_source_family_cannot_cross_splits(self):
        self.add()
        with self.assertRaisesRegex(ValueError, "another split"):
            self.add(text="Different prose.", corpus="other-model", split="test")

    def test_identical_text_cannot_cross_splits_under_new_group(self):
        self.add()
        with self.assertRaisesRegex(ValueError, "another split"):
            self.add(group_id="pretend-new-source", corpus="other", split="test")

    def test_unverified_generator_is_not_model_training_data(self):
        with self.assertRaisesRegex(ValueError, "exact provider"):
            self.add(source_kind="model")
        with self.assertRaisesRegex(ValueError, "exploratory"):
            self.add(source_kind="experimental")

    def test_domain_filter_and_group_exclusion_change_denominators(self):
        self.add()
        self.add(text="The rat runs.", group_id="source-2", domain="biology")
        result = profile(self.db, "human-sample", domain="biology")
        self.assertEqual(result["total"], 3)
        self.assertEqual(result["groups"], 1)
        self.assertEqual(profile(self.db, "human-sample", exclude_groups=["source-1"])["counts"], result["counts"])

    def test_mixed_extractor_versions_require_selection(self):
        doc = document()
        self.add()
        different = extract(doc["text"])
        different["extractor"] = "new-version"
        add_document(self.db, doc, different)
        with self.assertRaisesRegex(ValueError, "Multiple extractor"):
            profile(self.db, "human-sample")
        self.assertEqual(profile(self.db, "human-sample", extractor="new-version")["total"], 6)

    def test_import_is_atomic_on_bad_later_document(self):
        with self.assertRaises(ValueError):
            with self.db:
                self.add()
                self.add(text="A bad split.", split="other")
        self.assertEqual(status(self.db)["corpora"], [])

    def test_repeated_detector_input_preserved_but_task_deduped(self):
        doc_id, _ = self.add()
        text = document()["text"]
        record = {"task_id": "1", "sha256": digest(text), "submitted_text": text, "result": {}}
        self.assertTrue(add_run(self.db, doc_id, record))
        self.assertFalse(add_run(self.db, doc_id, record))
        self.assertTrue(add_run(self.db, doc_id, {**record, "task_id": "2"}))
        with self.assertRaisesRegex(ValueError, "conflicting"):
            add_run(self.db, doc_id, {**record, "result": {"new": 1}})
        self.assertEqual(status(self.db)["detector_runs"], 2)

    def test_hash_mismatch_rejected(self):
        doc_id, _ = self.add()
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            add_run(self.db, doc_id, {"sha256": "fake", "submitted_text": "different"})


if __name__ == "__main__":
    unittest.main()

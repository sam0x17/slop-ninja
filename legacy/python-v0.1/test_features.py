"""Behavioral checks for count denominators, Unicode handling, and parsing."""

import importlib.util
import json
import math
import unittest
from unittest.mock import patch

from unslop.features import extract, words, _load_parser


class LexicalTests(unittest.TestCase):
    def test_unicode_equivalence_and_apostrophes(self):
        self.assertEqual(
            words("CAFÉ cafe\u0301 DON’T don't l’esprit l'esprit co-operate 123 under_score"),
            ["café", "café", "don't", "don't", "l'esprit", "l'esprit", "co", "operate", "under", "score"],
        )
        self.assertEqual(words("हिन्दी 中文 Straße"), ["हिन्दी", "中文", "straße"])
        self.assertEqual(words("'hello' rock ‘n’ roll"), ["hello", "rock", "n", "roll"])

    def test_case_and_canonical_equivalence_leave_counts_unchanged(self):
        first = extract("CAFÉ isn't ordinary. CAFÉ matters.")
        second = extract("cafe\u0301 isn’t ordinary. café matters.")
        self.assertEqual(first, second)

    def test_ngram_boundaries_and_opportunity_counts(self):
        result = extract('One two three. "Four five!"\n\nSix seven eight nine')
        self.assertEqual(result["totals"]["word"], 9)
        self.assertEqual(result["totals"]["word_bigram"], 6)
        self.assertEqual(result["totals"]["word_trigram"], 3)
        self.assertNotIn("three four", result["families"]["word_bigram"])
        self.assertNotIn("five six", result["families"]["word_bigram"])
        self.assertEqual(result["metrics"]["heuristic_sentence_count"], 3)
        self.assertEqual(result["metrics"]["paragraph_count"], 2)
        for family, counts in result["families"].items():
            self.assertEqual(sum(counts.values()), result["totals"][family])

    def test_no_cross_document_ngram_assumption(self):
        one = extract("First fragment")
        two = extract("Second fragment")
        self.assertEqual(one["totals"]["word_bigram"] + two["totals"]["word_bigram"], 2)
        self.assertNotIn("fragment second", {**one["families"]["word_bigram"], **two["families"]["word_bigram"]})

    def test_no_tokens_returns_finite_zero_metrics(self):
        for text in ("", " \n\t ", "1234 ...!? --__"):
            with self.subTest(text=text):
                result = extract(text)
                self.assertTrue(all(total == 0 for total in result["totals"].values()))
                self.assertTrue(all(not counts for counts in result["families"].values()))
                self.assertTrue(all(value == 0 for value in result["metrics"].values()))
                json.dumps(result, allow_nan=False)

    def test_lexical_mode_does_not_load_optional_dependency(self):
        with patch("unslop.features._load_parser", side_effect=AssertionError("unexpected parser load")):
            self.assertEqual(extract("Ordinary text")["totals"]["word"], 2)

    def test_grammar_dependency_error_is_not_silently_ignored(self):
        with patch("unslop.features._load_parser", side_effect=RuntimeError("model missing")):
            with self.assertRaisesRegex(RuntimeError, "model missing"):
                extract("Some text.", grammar=True)

    def test_rejects_non_string_input(self):
        with self.assertRaisesRegex(TypeError, "string"):
            extract(None)


@unittest.skipUnless(importlib.util.find_spec("spacy") and importlib.util.find_spec("en_core_web_sm"), "spaCy English model not installed")
class GrammarIntegrationTests(unittest.TestCase):
    def test_grammar_preserves_lexical_counts_and_records_versions(self):
        text = "I don't believe it. The dogs were fed by the keeper."
        lexical = extract(text)
        parsed = extract(text, grammar=True)
        for family, counts in lexical["families"].items():
            self.assertEqual(parsed["families"][family], counts)
            self.assertEqual(parsed["totals"][family], lexical["totals"][family])
        self.assertIn("spacy=", parsed["extractor"])
        self.assertIn("model=en_core_web_sm@", parsed["extractor"])
        self.assertEqual(parsed["totals"]["construction"], 2)
        self.assertEqual(parsed["families"]["construction"]["passive_sentence"], 1)
        self.assertEqual(parsed["families"]["construction"]["first_person_reference_sentence"], 1)
        self.assertIn("dog", parsed["families"]["lemma"])
        for family, counts in parsed["families"].items():
            if family == "construction":
                self.assertTrue(all(0 <= count <= parsed["totals"][family] for count in counts.values()))
            else:
                self.assertEqual(sum(counts.values()), parsed["totals"][family])
        json.dumps(parsed, allow_nan=False)

    def test_multiple_passive_clauses_count_one_sentence_opportunity(self):
        parsed = extract("The dog was fed, and the cat was washed.", grammar=True)
        self.assertEqual(parsed["totals"]["construction"], 1)
        self.assertEqual(parsed["families"]["construction"]["passive_sentence"], 1)
        self.assertEqual(parsed["families"]["construction"]["coordination_sentence"], 1)

    def test_empty_input_is_finite_with_grammar(self):
        parsed = extract("", grammar=True)
        self.assertTrue(all(total == 0 for total in parsed["totals"].values()))
        self.assertTrue(all(math.isfinite(value) for value in parsed["metrics"].values()))

    def test_invalid_model_fails_clearly(self):
        with self.assertRaisesRegex(RuntimeError, "Cannot load spaCy model"):
            _load_parser("unslop_nonexistent_model_for_test")


if __name__ == "__main__":
    unittest.main()

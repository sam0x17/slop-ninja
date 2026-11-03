import tempfile
from pathlib import Path
import unittest

from unslop.cli import read
from unslop.experiments import substitute
from unslop.store import digest


class PerturbationTests(unittest.TestCase):
    def test_one_occurrence_changes_with_exact_offsets_and_hash(self):
        source = "The scope is accordingly broad. I act accordingly."
        changed, manifest = substitute(source, "accordingly", "thus", 2)
        self.assertEqual(changed, "The scope is accordingly broad. I act thus.")
        self.assertEqual(source[manifest["start"]:manifest["end"]], "accordingly")
        self.assertEqual(manifest["candidate_sha256"], digest(changed))

    def test_partial_words_and_contractions_not_accidental_targets(self):
        with self.assertRaises(ValueError):
            substitute("It isn't minor; primarily they're revised.", "is", "was")

    def test_file_loading_preserves_crlf_for_replays(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "source.txt"
            path.write_bytes(b"A sentence.\r\nAnother sentence.\r\n")
            self.assertEqual(read(path).encode("utf-8"), path.read_bytes())

    def test_deletion_punctuation_and_multiword_changes_are_not_word_edits(self):
        for value in ("", "...", "three words here", "two-words", "42", "thus!"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                substitute("We act accordingly.", "accordingly", value)


if __name__ == "__main__":
    unittest.main()

import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from unslop.cli import main, parser, run_study
from unslop.store import digest


class FakeClient:
    submissions = 0

    def submit(self, text, model):
        FakeClient.submissions += 1
        return {"detector": "pangram", "model": model, "task_id": f"task-{self.submissions}",
                "submitted_text": text, "sha256": digest(text)}

    def poll(self, task_id, timeout):
        return {"stage": "STAGE_SUCCESS", "version": "4.0", "fraction_ai": .05,
                "fraction_ai_assisted": 0, "fraction_human": .95}


class StudyIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.root = Path(self.folder.name)
        self.baseline = self.root / "baseline.txt"
        self.candidate = self.root / "candidate.txt"
        self.baseline.write_text("A reader can follow this philosophical argument in some detail. " * 6)
        self.candidate.write_text("A reader can examine this philosophical argument in some detail. " * 6)
        FakeClient.submissions = 0

    def tearDown(self):
        self.folder.cleanup()

    def args(self, *extra):
        return parser().parse_args(["study", str(self.baseline), str(self.candidate),
            "--out", str(self.root / "study"), "--max-requests", "6", "--full-document", *extra])

    def test_complete_study_resumes_without_new_submissions_and_imports(self):
        args = self.args()
        with patch("unslop.evaluation.PangramClient", FakeClient), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            run_study(args)
            self.assertEqual(FakeClient.submissions, 6)
            args.max_requests = 0
            run_study(args)
            self.assertEqual(FakeClient.submissions, 6)
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                main(["--db", str(self.root / "test.sqlite3"), "import-pangram", args.out])
            self.assertEqual(json.loads(output.getvalue())["imported_runs"], 6)
        report = json.loads((self.root / "study" / "summary.json").read_text())
        self.assertFalse(report["observed_success"], "Missing quality audit must prevent acceptance")

    def test_invalid_inputs_fail_before_any_network(self):
        for option in (("--threshold", "nan"), ("--timeout", "0"), ("--max-requests", "1")):
            with self.subTest(option=option), patch("unslop.evaluation.PangramClient") as client:
                with self.assertRaises(ValueError):
                    run_study(self.args(*option))
                client.assert_not_called()

    def test_bad_audit_and_short_candidate_fail_before_billing(self):
        audit = self.root / "audit.json"
        audit.write_text('{"source_sha256":"wrong"}')
        with patch("unslop.evaluation.PangramClient") as client:
            with self.assertRaisesRegex(ValueError, "exact source"):
                run_study(self.args("--audit", str(audit)))
            self.candidate.write_text("Too short.")
            with self.assertRaisesRegex(ValueError, "50 words"):
                run_study(self.args())
            client.assert_not_called()

    def test_uncertain_submission_cannot_silently_be_repeated(self):
        with patch("unslop.evaluation.PangramClient") as client:
            client.return_value.submit.side_effect = RuntimeError("connection interrupted")
            with self.assertRaisesRegex(RuntimeError, "interrupted"):
                run_study(self.args())
            self.assertTrue((self.root / "study" / "01-baseline.json").exists())
            with self.assertRaisesRegex(ValueError, "uncertain"):
                run_study(self.args())
            self.assertEqual(client.return_value.submit.call_count, 1)

    def test_missing_scope_is_rejected_by_argument_parser(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            parser().parse_args(["study", "a", "b", "--out", "out", "--max-requests", "6"])

    def test_bad_cached_completion_is_rejected_before_other_paid_slots(self):
        with patch("unslop.evaluation.PangramClient", FakeClient), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            run_study(self.args())
        first = self.root / "study" / "01-baseline.json"
        record = json.loads(first.read_text())
        record["result"]["fraction_ai"] = 2
        first.write_text(json.dumps(record))
        (self.root / "study" / "01-candidate.json").unlink()
        with patch("unslop.evaluation.PangramClient") as client:
            with self.assertRaises(ValueError):
                run_study(self.args())
            client.assert_not_called()


if __name__ == "__main__":
    unittest.main()

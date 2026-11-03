import io
import json
import math
import unittest
from unittest.mock import patch
import urllib.error

from unslop.evaluation import (
    PangramClient, PangramError, PangramTaskFailed, PangramTimeout,
    paired_study, summarize_runs, text_hash,
)


def run(task, ai=.04, assisted=.01, text="Complete original document.",
        model="pangram-4", version="4.0", full=True):
    return {
        "task_id": task, "model": model, "submitted_text": text,
        "sha256": text_hash(text), "full_document": full,
        "result": {"stage": "STAGE_SUCCESS", "version": version,
                   "fraction_ai": ai, "fraction_ai_assisted": assisted,
                   "fraction_human": 1 - ai - assisted},
    }


class RepeatedEvaluationTests(unittest.TestCase):
    def test_reports_all_observations_including_worst(self):
        records = [run("a", .03), run("b", .02), run("c", .21)]
        summary = summarize_runs(records)
        group = summary["groups"][0]
        self.assertEqual(group["repeat_count"], 3)
        self.assertAlmostEqual(group["ai_fraction"]["median"], .03)
        self.assertAlmostEqual(group["ai_plus_assisted_fraction"]["max"], .22)
        self.assertFalse(group["observed_success"])
        self.assertFalse(summary["reliability_established"])

    def test_below_threshold_requires_all_three_distinct_tasks(self):
        self.assertFalse(summarize_runs([run("a"), run("b")])["all_groups_observed_success"])
        self.assertTrue(summarize_runs([run("a"), run("b"), run("c")])["all_groups_observed_success"])
        summary = summarize_runs([run("a"), run("b"), run("b")])
        self.assertEqual(summary["duplicate_records_ignored"], 1)
        self.assertFalse(summary["all_groups_observed_success"])

    def test_exact_ten_percent_fails_even_if_headline_might_be_human(self):
        records = [run(str(i), .08, .02) for i in range(3)]
        group = summarize_runs(records)["groups"][0]
        self.assertFalse(group["observed_success"])
        self.assertTrue(group["ai_only_observed_success"])

    def test_assisted_counts_toward_threshold(self):
        records = [run(str(i), 0, .25) for i in range(3)]
        group = summarize_runs(records)["groups"][0]
        self.assertFalse(group["observed_success"])
        self.assertTrue(group["ai_only_observed_success"])

    def test_input_model_version_and_detector_groups_stay_separate(self):
        records = [run("a"), run("b", text="Another document."),
                   run("c", model="default"), run("d", version="4.1")]
        other = run("a")
        other["detector"] = "other"
        records.append(other)
        summary = summarize_runs(records)
        self.assertEqual(len(summary["groups"]), 5)
        self.assertFalse(summary["all_groups_observed_success"])

    def test_rejects_hash_mismatch_and_conflicting_same_task(self):
        record = run("a")
        record["submitted_text"] += "\n"
        with self.assertRaisesRegex(ValueError, "sha256"):
            summarize_runs([record])
        with self.assertRaisesRegex(ValueError, "conflicting"):
            summarize_runs([run("a"), run("a", ai=.9)])

    def test_normalized_response_text_is_preserved_without_rejecting_input(self):
        record = run("a")
        record["result"]["text"] = "Normalized document."
        summary = summarize_runs([record])
        self.assertTrue(summary["groups"][0]["normalization_observed"])
        self.assertEqual(summary["groups"][0]["sha256"], record["sha256"])

    def test_invalid_fractions_fail_instead_of_passing_by_accident(self):
        for value in (math.nan, math.inf, -math.inf, -.1, 1.1, True, ".1", None):
            with self.subTest(value=value):
                record = run("a")
                record["result"]["fraction_ai"] = value
                with self.assertRaises(ValueError):
                    summarize_runs([record])
        record = run("a")
        record["result"]["fraction_human"] = .2
        with self.assertRaisesRegex(ValueError, "sum to 1"):
            summarize_runs([record])

    def test_missing_provenance_failed_stage_and_invalid_settings(self):
        for key in ("task_id", "model", "submitted_text"):
            record = run("a")
            del record[key]
            with self.subTest(key=key), self.assertRaises(ValueError):
                summarize_runs([record])
        record = run("a")
        record["result"]["stage"] = "STAGE_FAILED"
        with self.assertRaisesRegex(ValueError, "successful"):
            summarize_runs([record])
        for kwargs in ({"threshold": math.nan}, {"threshold": 0},
                       {"min_repeats": 0}, {"min_repeats": True}):
            with self.subTest(kwargs=kwargs), self.assertRaises(ValueError):
                summarize_runs([], **kwargs)
        self.assertFalse(summarize_runs([])["all_groups_observed_success"])


class PairedStudyTests(unittest.TestCase):
    def setUp(self):
        self.baseline = [run(f"b{i}", ai=.7) for i in range(3)]
        self.candidate = [run(f"c{i}", text="Revised complete document.") for i in range(3)]
        self.audit = {"reviewed_by_human": True, "readability": True,
                      "argumentation": True, "detail": True, "tone": True}

    def test_joint_gate_and_paired_differences(self):
        study = paired_study(self.baseline, self.candidate, self.audit)
        self.assertTrue(study["observed_success"])
        self.assertFalse(study["reliability_established"])
        delta = study["paired_deltas"][0]["candidate_minus_baseline_ai_plus_assisted"]
        self.assertAlmostEqual(delta["mean"], -.66)
        for field in self.audit:
            audit = {**self.audit, field: False}
            self.assertFalse(paired_study(self.baseline, self.candidate, audit)["observed_success"])

    def test_requires_full_document_review_and_exact_same_text_per_arm(self):
        self.candidate[0]["full_document"] = False
        with self.assertRaisesRegex(ValueError, "full_document"):
            paired_study(self.baseline, self.candidate, self.audit)
        self.candidate[0] = run("c0", text="A third document.")
        with self.assertRaisesRegex(ValueError, "one exact document"):
            paired_study(self.baseline, self.candidate, self.audit)

    def test_rejects_unmatched_models_duplicate_tasks_and_unequal_arms(self):
        self.candidate[0]["model"] = "default"
        with self.assertRaisesRegex(ValueError, "same detector"):
            paired_study(self.baseline, self.candidate, self.audit)
        with self.assertRaisesRegex(ValueError, "equally sized"):
            paired_study(self.baseline, self.candidate[:2], self.audit)
        with self.assertRaisesRegex(ValueError, "distinct submitted"):
            paired_study([self.baseline[0]] * 3, self.candidate, self.audit)

    def test_same_original_does_not_count_as_a_successful_rewrite(self):
        candidate = [run(f"c{i}") for i in range(3)]
        with self.assertRaisesRegex(ValueError, "different documents"):
            paired_study(self.baseline, candidate, self.audit)


class PangramClientTests(unittest.TestCase):
    def setUp(self):
        self.client = PangramClient(api_key="secret-key")
        self.text = "Caf\u00e9 means a place to sit and read. " * 7 + "\r\n"

    @patch("unslop.evaluation.urllib.request.urlopen")
    def test_submit_preserves_exact_text_and_disables_public_link(self, urlopen):
        urlopen.return_value = io.BytesIO(b'{"task_id":"job-1"}')
        record = self.client.submit(self.text)
        request = urlopen.call_args.args[0]
        payload = json.loads(request.data)
        self.assertEqual(request.method, "POST")
        self.assertEqual(payload["text"], self.text)
        self.assertEqual(payload["model"], "pangram-4")
        self.assertIs(payload["public_dashboard_link"], False)
        self.assertEqual(record["sha256"], text_hash(self.text))
        self.assertEqual(record["submitted_text"], self.text)
        self.assertEqual(record["task_id"], "job-1")
        self.assertNotIn("secret-key", json.dumps(record))
        self.assertEqual(urlopen.call_count, 1)

    @patch("unslop.evaluation.urllib.request.urlopen")
    def test_http_error_is_not_retried_and_does_not_echo_body(self, urlopen):
        urlopen.side_effect = urllib.error.HTTPError(
            "https://text.external-api.pangram.com/task", 402,
            "insufficient credits", {}, io.BytesIO(b"secret-key private text"),
        )
        with self.assertRaises(PangramError) as caught:
            self.client.submit(self.text)
        self.assertIn("402", str(caught.exception))
        self.assertNotIn("secret", str(caught.exception))
        self.assertEqual(urlopen.call_count, 1)

    @patch("unslop.evaluation.urllib.request.urlopen")
    def test_transport_and_malformed_json_are_not_retried(self, urlopen):
        urlopen.side_effect = urllib.error.URLError("secret-key")
        with self.assertRaises(PangramError) as caught:
            self.client.submit(self.text)
        self.assertNotIn("secret-key", str(caught.exception))
        self.assertEqual(urlopen.call_count, 1)
        urlopen.reset_mock(side_effect=True)
        urlopen.return_value = io.BytesIO(b'{"task_id": NaN}')
        with self.assertRaisesRegex(PangramError, "invalid JSON"):
            self.client.submit(self.text)
        self.assertEqual(urlopen.call_count, 1)

    @patch("unslop.evaluation.time.sleep")
    def test_poll_terminal_result_and_failure_preserve_task(self, sleep):
        responses = [
            {"stage": "STAGE_PREPROCESSING", "task_id": "job-1"},
            {"stage": "STAGE_SUCCESS", "fraction_ai": .1},
        ]
        with patch.object(self.client, "_request", side_effect=responses) as request:
            result = self.client.poll("job-1")
            self.assertEqual(result["stage"], "STAGE_SUCCESS")
            self.assertEqual(request.call_count, 2)
            self.assertTrue(all(call.args[0] == "/task/job-1" for call in request.call_args_list))
        failure = {"stage": "STAGE_FAILED", "headline": "Input error"}
        with patch.object(self.client, "_request", return_value=failure):
            with self.assertRaises(PangramTaskFailed) as caught:
                self.client.poll("job-1")
        self.assertEqual(caught.exception.task_id, "job-1")
        self.assertEqual(caught.exception.result, failure)

    def test_poll_timeout_bounds_http_wait_and_can_resume(self):
        pending = {"stage": "STAGE_PREPROCESSING"}
        with patch("unslop.evaluation.time.monotonic", side_effect=[0, 1, 6]):
            with patch.object(self.client, "_request", return_value=pending) as request:
                with self.assertRaises(PangramTimeout) as caught:
                    self.client.poll("job-1", timeout=5)
        self.assertEqual(request.call_args.kwargs["timeout"], 4)
        self.assertEqual(caught.exception.task_id, "job-1")
        self.assertEqual(caught.exception.last_response, pending)

    def test_invalid_inputs_do_not_submit(self):
        with patch.object(self.client, "_request") as request:
            for text in ("short", "", None):
                with self.assertRaises(ValueError):
                    self.client.submit(text)
            with self.assertRaises(ValueError):
                self.client.submit(self.text, model="")
            request.assert_not_called()


if __name__ == "__main__":
    unittest.main()

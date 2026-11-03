import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from urllib.error import HTTPError, URLError

from unslop.collection import CollectionError, collect_one, load_prompts


PROMPT = {"id": "p1", "prompt": "Explain this result.\nKeep its caveats.",
          "domain": "science", "register": "formal", "group_id": "source1"}


def openai_response():
    return {"id": "resp_test", "model": "returned-model", "status": "completed",
            "created_at": 100, "usage": {"input_tokens": 9, "output_tokens": 3},
            "output": [{"type": "reasoning", "summary": []},
                       {"type": "message", "role": "assistant", "status": "completed",
                        "content": [{"type": "output_text", "text": "  Exact prose.\n"}]}]}


def anthropic_response():
    return {"id": "msg_test", "model": "returned-model", "type": "message", "role": "assistant",
            "stop_reason": "end_turn", "usage": {"input_tokens": 9, "output_tokens": 3},
            "content": [{"type": "thinking", "thinking": "private reasoning"},
                        {"type": "text", "text": "  Exact prose.\n"}]}


class CollectionTests(unittest.TestCase):
    def collect(self, provider="openai", **kwargs):
        return collect_one(PROMPT, provider, "requested-model", "test-corpus",
                           api_key="test-secret", **kwargs)

    @patch("unslop.collection._post_json")
    def test_openai_provenance_and_exact_text(self, post):
        response = openai_response()
        post.return_value = (response, {"x-request-id": "req_test"})
        doc = self.collect(parameters={"reasoning": {"effort": "low"}})
        self.assertEqual(doc["text"], "  Exact prose.\n")
        self.assertEqual(doc["model"], "returned-model")
        self.assertEqual(doc["model_requested"], "requested-model")
        self.assertEqual(doc["group_id"], "source1")
        self.assertEqual(doc["source_kind"], "model")
        self.assertEqual(doc["metadata"]["response"], response)
        self.assertEqual(doc["metadata"]["request_id"], "req_test")
        self.assertEqual(doc["metadata"]["prompt"], PROMPT["prompt"])
        self.assertNotIn("test-secret", json.dumps(doc))
        endpoint, payload, headers, timeout = post.call_args.args
        self.assertEqual(endpoint, "https://api.openai.com/v1/responses")
        self.assertFalse(payload["store"])
        self.assertEqual(payload["input"], PROMPT["prompt"])
        self.assertEqual(headers["Authorization"], "Bearer test-secret")
        self.assertEqual(timeout, 120.0)

    @patch("unslop.collection._post_json")
    def test_anthropic_provenance_and_thinking_exclusion(self, post):
        post.return_value = (anthropic_response(), {"request-id": "req_anthropic"})
        doc = self.collect("anthropic")
        self.assertEqual(doc["text"], "  Exact prose.\n")
        self.assertEqual(doc["metadata"]["request_id"], "req_anthropic")
        _, payload, headers, _ = post.call_args.args
        self.assertEqual(payload["max_tokens"], 2048)
        self.assertEqual(payload["messages"], [{"role": "user", "content": PROMPT["prompt"]}])
        self.assertEqual(headers["anthropic-version"], "2023-06-01")

    @patch("unslop.collection._post_json")
    def test_openai_refusal_keeps_attempt_without_corpus_document(self, post):
        response = openai_response()
        response["output"][1]["content"] = [{"type": "refusal", "refusal": "Declined."}]
        post.return_value = (response, {})
        with self.assertRaises(CollectionError) as result:
            self.collect()
        self.assertIn("refused", str(result.exception))
        self.assertEqual(result.exception.record["response"], response)
        self.assertEqual(result.exception.record["collection_status"], "rejected")
        post.assert_called_once()

    @patch("unslop.collection._post_json")
    def test_incomplete_outputs_are_not_samples(self, post):
        for provider, response, key, status in [
            ("openai", openai_response(), "status", "incomplete"),
            ("anthropic", anthropic_response(), "stop_reason", "max_tokens"),
            ("anthropic", anthropic_response(), "stop_reason", "refusal"),
            ("anthropic", anthropic_response(), "stop_reason", "stop_sequence"),
        ]:
            with self.subTest(provider=provider, status=status):
                response[key] = status
                post.return_value = (response, {})
                with self.assertRaises(CollectionError):
                    self.collect(provider)

    @patch("unslop.collection._post_json")
    def test_missing_model_and_empty_text_rejected(self, post):
        for modification in ("missing_model", "empty", "malformed"):
            response = openai_response()
            if modification == "missing_model":
                del response["model"]
            elif modification == "empty":
                response["output"][1]["content"][0]["text"] = " \n"
            else:
                response["output"] = [None]
            with self.subTest(modification=modification):
                post.return_value = (response, {})
                with self.assertRaises(CollectionError):
                    self.collect()

    @patch("unslop.collection._post_json")
    def test_parameter_override_and_invalid_budget_fail_before_network(self, post):
        for kwargs in ({"parameters": {"model": "another"}},
                       {"parameters": {"input": "untracked"}},
                       {"parameters": {"tools": []}},
                       {"parameters": {"temperature": float("nan")}},
                       {"max_output_tokens": 0}, {"timeout": float("inf")}):
            with self.subTest(kwargs=kwargs), self.assertRaises(ValueError):
                self.collect(**kwargs)
        post.assert_not_called()

    @patch("unslop.collection._opener.open")
    def test_http_error_not_retried_and_body_not_logged(self, request):
        request.side_effect = HTTPError("https://api.openai.com/v1/responses", 429,
                                       "busy", {"x-request-id": "req_rate"},
                                       io.BytesIO(b"test-secret"))
        with self.assertRaises(CollectionError) as result:
            self.collect()
        request.assert_called_once()
        self.assertEqual(result.exception.record["http_status"], 429)
        self.assertEqual(result.exception.record["request_id"], "req_rate")
        self.assertEqual(result.exception.record["collection_status"], "request_failed")
        self.assertNotIn("test-secret", str(result.exception))
        self.assertNotIn("test-secret", json.dumps(result.exception.record))

    @patch("unslop.collection._opener.open")
    def test_ambiguous_network_failure_not_retried(self, request):
        request.side_effect = URLError("connection lost")
        with self.assertRaisesRegex(CollectionError, "may have been charged"):
            self.collect()
        request.assert_called_once()

    @patch.dict("os.environ", {}, clear=True)
    @patch("unslop.collection._post_json")
    def test_missing_credentials_before_request(self, post):
        with self.assertRaisesRegex(CollectionError, "OPENAI_API_KEY"):
            collect_one(PROMPT, "openai", "explicit-model", "corpus")
        post.assert_not_called()

    def test_prompt_manifest_is_checked_in_full(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "prompts.jsonl"
            path.write_text(json.dumps(PROMPT) + "\n", encoding="utf-8")
            self.assertEqual(load_prompts(path), [PROMPT])
            path.write_text(json.dumps(PROMPT) + "\n" + json.dumps(PROMPT), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "Duplicate prompt id"):
                load_prompts(path)
            other = {**PROMPT, "id": "p2", "split": "test"}
            path.write_text(json.dumps(PROMPT) + "\n" + json.dumps(other), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "different splits"):
                load_prompts(path)

    @patch("unslop.collection._post_json")
    def test_collection_split_values_match_store(self, post):
        from unslop.store import SPLITS, add_document, connect

        post.return_value = (openai_response(), {})
        extracted = {"extractor": "collection-integration-test", "metrics": {},
                     "families": {"word": {"exact": 1, "prose": 1}}, "totals": {"word": 2}}
        for split in SPLITS:
            with self.subTest(split=split), tempfile.TemporaryDirectory() as directory:
                doc = collect_one({**PROMPT, "split": split}, "openai", "requested-model",
                                  "test-corpus", api_key="test-secret")
                with connect(Path(directory) / "test.sqlite") as db:
                    doc_id, added = add_document(db, doc, extracted)
                    row = db.execute("SELECT * FROM documents WHERE id=?", (doc_id,)).fetchone()
                    self.assertTrue(added)
                    self.assertEqual(row["split"], split)
                    self.assertEqual(row["source_kind"], "model")
                    self.assertEqual(row["provider"], "openai")
                    self.assertEqual(row["model"], "returned-model")
                    metadata = json.loads(row["metadata_json"])
                    self.assertEqual(metadata["model_requested"], "requested-model")
                    self.assertEqual(metadata["response"]["model"], "returned-model")


if __name__ == "__main__":
    unittest.main()

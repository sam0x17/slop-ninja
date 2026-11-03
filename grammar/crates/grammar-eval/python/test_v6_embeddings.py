"""Small synthetic tests only: never load V6 or issue detector/provider calls."""

import json
import os
from pathlib import Path
import struct
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import v6_embeddings as v6

os.environ.update(v6.CPU_ENV)
import torch

torch.set_num_threads(1)


class ToyClassifier(torch.nn.Module):
    """A single tiny linear layer, with a distinguishable pre-classifier input."""

    def __init__(self):
        super().__init__()
        self.classifier = torch.nn.Linear(768, 3)
        with torch.no_grad():
            self.classifier.weight.fill_(0.001)
            self.classifier.bias.copy_(torch.tensor([-1., 0., 1.]))

    def forward(self, ids):
        pooled = torch.arange(768, dtype=torch.float32).reshape(1, 768) / 768
        features = pooled + len(ids) / 100
        return self.classifier(features)


class ToyCommon:
    torch = torch
    CPU_PROBABILITY_TOLERANCE = 1e-6

    @staticmethod
    def token_ids(tokenizer, text, max_tokens):
        if not text.strip():
            raise ValueError("empty_text")
        ids = [1] + list(text.encode("utf-8")) + [2]
        if max_tokens is not None and len(ids) > max_tokens:
            raise ValueError(f"unsupported_length:{len(ids)}>{max_tokens}")
        return ids

    @staticmethod
    @torch.inference_mode()
    def logits_for(model, tokenizer, rows, batch_size, device):
        assert batch_size == 1 and device == "cpu" and len(rows) == 1
        assert set(rows[0]) == {"ids"}
        model.eval()
        return model(rows[0]["ids"]).double()

    @staticmethod
    def software():
        return {"packages": {"torch": "toy-test-only"}}


class EmbeddingTests(unittest.TestCase):
    def setUp(self):
        self.model = ToyClassifier().eval()
        self.common = ToyCommon()

    def test_hook_captures_classifier_input_and_preserves_original_outputs(self):
        ids = [1, 4, 2]
        original = v6.forward(self.common, None, self.model, ids, 1.3, capture=False)
        hooked = v6.forward(self.common, None, self.model, ids, 1.3)
        self.assertEqual(hooked["logits"], original["logits"])
        self.assertEqual(hooked["probabilities"], original["probabilities"])
        self.assertEqual(hooked["embedding"], (torch.arange(768) / 768 + .03).tolist())
        self.assertEqual(len(self.model.classifier._forward_pre_hooks), 0)
        self.assertGreaterEqual(hooked["elapsed_forward_seconds"], 0)

    def test_hook_removed_on_failure(self):
        with self.assertRaisesRegex(RuntimeError, "synthetic"):
            with v6.capture_classifier_input(self.model, torch):
                raise RuntimeError("synthetic")
        self.assertEqual(len(self.model.classifier._forward_pre_hooks), 0)
        with self.assertRaisesRegex(ValueError, "shape"):
            with v6.capture_classifier_input(self.model, torch):
                self.model.classifier(torch.zeros(2, 768))
        self.assertEqual(len(self.model.classifier._forward_pre_hooks), 0)

    def test_supported_row_and_stable_little_endian_hash(self):
        request = {"id": "synthetic", "text": "  Synthetic café.\n"}
        row = v6.extract_row(request, self.common, None, self.model, 2., "toy")
        self.assertEqual(row["status"], "ok")
        self.assertEqual(len(row["embedding"]), 768)
        self.assertEqual(row["token_count"], len(request["text"].encode()) + 2)
        self.assertEqual(row["text_sha256"], v6.digest(request["text"].encode()))
        self.assertEqual(row["embedding_sha256"], v6.digest(struct.pack("<768f", *row["embedding"])))
        self.assertEqual(row["model_or_assistance_probability"], sum(row["probabilities"][1:]))

    def test_empty_and_over_limit_have_null_embedding_and_never_forward(self):
        for text, count, reason in [(" \n", 0, "empty_text"),
                                    ("x" * 2047, 2049, "unsupported_length:2049>2048")]:
            with self.subTest(reason=reason), patch.object(self.common, "logits_for", side_effect=AssertionError):
                row = v6.extract_row({"id": "synthetic", "text": text}, self.common, None, self.model, 1., "toy")
            self.assertEqual(row["status"], "unsupported_input")
            self.assertEqual(row["reason"], reason)
            self.assertEqual(row["token_count"], count)
            self.assertIsNone(row["embedding"])
            self.assertIsNone(row["logits"])
            self.assertIsNone(row["probabilities"])
        row = v6.extract_row({"id": "boundary", "text": "x" * 2046}, self.common, None, self.model, 1., "toy")
        self.assertEqual(row["token_count"], 2048)
        self.assertEqual(row["status"], "ok")

    def test_request_validation_and_exact_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "requests.jsonl"
            raw = b'{ "text": "  Synthetic text.\\n", "id": "a" }\r\n'
            path.write_bytes(raw)
            self.assertEqual(list(v6.requests(path)), [(1, {"id": "a", "text": "  Synthetic text.\n"}, v6.digest(raw))])
            for invalid in [b'\n', b'{"id":"a","text":"x","label":1}\n',
                            b'{"id":"a","text":"x","text":"y"}\n',
                            b'{"id":null,"text":"x"}\n', b'{"id":"a","text":null}\n',
                            b'{"id":"a","text":"\\ud800"}\n', raw + raw]:
                path.write_bytes(invalid)
                with self.subTest(raw=invalid), self.assertRaisesRegex(ValueError, "invalid request at line"):
                    list(v6.requests(path))

    def test_synthetic_reference_qualification(self):
        text = "Synthetic reference."
        ids = self.common.token_ids(None, text, 2048)
        original = v6.forward(self.common, None, self.model, ids, 1.2, capture=False)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            v6.write_json(path / "reference_vectors.json", [{"text": text, "token_count": len(ids),
                                                            "probabilities": original["probabilities"]}])
            evidence = v6.qualify_hook(path, self.common, None, self.model, 1.2)
            self.assertTrue(evidence[0]["hook_preserves_logits_and_probabilities_exactly"])
            self.assertEqual(evidence[0]["reference_probability_max_absolute_error"], 0.)

    def test_full_protocol_and_interruption_use_only_toy_model(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "toy-artifact"
            artifact.mkdir()
            (artifact / "manifest.json").write_text("{}")
            source = root / "source.jsonl"
            raw = b'{"id":"one","text":"Synthetic text."}\r\n{"id":"two","text":""}\n'
            source.write_bytes(raw)
            manifest = {"artifact_id": "toy"}
            contract = {"dimension": 768, "synthetic_test": True}
            args = SimpleNamespace(artifact=artifact, requests=source, output=root / "output")
            with patch.object(v6, "artifact_binding", return_value=(manifest, "toy")), \
                    patch.object(v6, "load_runtime", return_value=(self.common, None, self.model, 1.)), \
                    patch.object(v6, "qualify_hook", return_value=[{"toy": True}]), \
                    patch.object(v6, "representation_contract", return_value=contract):
                result = v6.run(args)
                self.assertEqual(result["status"], "complete")
                self.assertEqual((result["requested"], result["supported"], result["unsupported"]), (2, 1, 1))
                self.assertEqual((args.output / "requests.jsonl").read_bytes(), raw)
                self.assertEqual(result["requests_sha256"], v6.digest(raw))
                self.assertEqual(result["rows_sha256"], v6.sha256(args.output / "rows.jsonl"))
                self.assertEqual(result["representation_contract"], v6.read_json(args.output / "contract/manifest.json"))
                rows = [json.loads(line) for line in (args.output / "rows.jsonl").read_bytes().splitlines()]
                self.assertEqual([row["id"] for row in rows], ["one", "two"])
                with self.assertRaisesRegex(ValueError, "NEW"):
                    v6.run(args)
                args.output = root / "interrupted"
                extract = v6.extract_row

                def interrupt_second(request, *extra):
                    if request["id"] == "two":
                        raise v6.Interrupted()
                    return extract(request, *extra)

                with patch.object(v6, "extract_row", side_effect=interrupt_second), self.assertRaises(v6.Interrupted):
                    v6.run(args)
                partial = v6.read_json(args.output / "manifest.json")
                self.assertEqual(partial["status"], "interrupted")
                self.assertEqual(partial["processed"], 1)
                self.assertEqual(partial["rows_sha256"], v6.sha256(args.output / "rows.jsonl"))
                self.assertEqual(partial["representation_contract_sha256"], result["representation_contract_sha256"])

    def test_environment_refuses_already_imported_torch(self):
        with self.assertRaisesRegex(ValueError, "fresh process"):
            v6.configure_environment({"reference_runtime": {"fixed_environment": v6.CPU_ENV}})


if __name__ == "__main__":
    unittest.main()

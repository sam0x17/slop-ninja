"""Explicit frozen-artifact evaluation; no training or calibration updates."""

import argparse
import json
from pathlib import Path

from common import LABELS, configure_cpu, load_artifact, load_partition, logits_for, metrics, sha256, write_json


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--artifact", required=True, type=Path)
    inputs = p.add_mutually_exclusive_group(required=True)
    inputs.add_argument("--test-jsonl", type=Path)
    inputs.add_argument("--calibration-jsonl", type=Path,
                        help="Export frozen calibrated predictions; never refit the temperature")
    p.add_argument("--output", required=True, type=Path)
    p.add_argument("--probabilities-output", type=Path,
                   help="Write exact ID/text-hash matched probability JSONL for Rust evaluation")
    p.add_argument("--acknowledge-final-test", action="store_true",
                   help="Confirm that model selection and calibration are frozen before opening this test")
    p.add_argument("--batch-size", type=int, default=1)
    args = p.parse_args()
    if args.test_jsonl and not args.acknowledge_final_test:
        p.error("--test-jsonl requires --acknowledge-final-test")
    if args.batch_size != 1:
        p.error("the frozen CPU reference contract requires batch size 1")
    outputs = [args.output] + ([args.probabilities_output] if args.probabilities_output else [])
    if len({path.resolve() for path in outputs}) != len(outputs):
        p.error("report and probability outputs must use distinct paths")
    if any(path.exists() or path.resolve().is_relative_to(args.artifact.resolve()) for path in outputs):
        p.error("outputs must be new paths outside the immutable artifact")
    configure_cpu()
    manifest, tokenizer, model, temperature = load_artifact(args.artifact, "evaluate.py")
    split = "test" if args.test_jsonl else "calibration"
    input_path = args.test_jsonl or args.calibration_jsonl
    input_hash = sha256(input_path)
    if split == "calibration":
        training = json.loads((args.artifact / "training.json").read_text())
        if input_hash != training["partition_sha256"]["calibration"]:
            raise ValueError("calibration shard differs from the frozen artifact's calibration input")
    rows = load_partition(input_path, split, tokenizer, manifest["max_tokens"])
    logits = logits_for(model, tokenizer, rows, 1, "cpu")
    report = {"artifact_id": manifest["artifact_id"], "split": split, f"{split}_sha256": input_hash,
              "source_groups": len({r["source_group"] for r in rows}),
              "metrics": metrics(logits, [r["label"] for r in rows], temperature),
              "runtime": manifest["reference_runtime"],
              "final_test_opened": split == "test",
              "limitations": "Point estimates only; source-cluster uncertainty and preregistered false-positive operating points require the Rust evaluation stage."}
    if args.probabilities_output:
        probabilities = (logits.double() / temperature).softmax(-1).tolist()
        with args.probabilities_output.open("x") as output:
            for row, values in zip(rows, probabilities, strict=True):
                prediction = {"id": row["id"], "text_sha256": row["hash"],
                              "artifact_id": manifest["artifact_id"], "classes": LABELS,
                              "probabilities": values, "status": "ok"}
                output.write(json.dumps(prediction, allow_nan=False) + "\n")
        report["probabilities_sha256"] = sha256(args.probabilities_output)
    write_json(args.output, report)


if __name__ == "__main__":
    main()

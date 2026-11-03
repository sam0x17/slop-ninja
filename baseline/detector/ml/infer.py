"""Offline CPU reference inference. Read one JSON request per line and write one result."""

import argparse
import json
from pathlib import Path
import sys

from common import LABELS, configure_cpu, load_artifact, logits_for, token_ids


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact", required=True, type=Path)
    args = parser.parse_args()
    configure_cpu()
    manifest, tokenizer, model, temperature = load_artifact(args.artifact, "infer.py")
    for line in sys.stdin:
        request = json.loads(line)
        # Labels and assignment metadata are not accepted as model inputs.
        if set(request) - {"id", "text"}:
            raise ValueError("inference requests accept only id and text")
        result = {"id": request.get("id"), "artifact_id": manifest["artifact_id"], "labels": LABELS,
                  "runtime": "cpu_float32_eager", "probabilities": None, "model_or_assistance_probability": None}
        try:
            ids = token_ids(tokenizer, request["text"], manifest["max_tokens"])
        except ValueError as error:
            result.update(status="unsupported_input", reason=str(error))
        else:
            logits = logits_for(model, tokenizer, [{"ids": ids}], 1, "cpu")
            probabilities = (logits / temperature).softmax(-1)[0].tolist()
            result.update(status="ok", probabilities=probabilities,
                          model_or_assistance_probability=probabilities[1] + probabilities[2], token_count=len(ids))
        print(json.dumps(result, allow_nan=False), flush=True)


if __name__ == "__main__":
    main()

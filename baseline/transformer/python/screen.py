"""Optional ML accelerator bridge. Never emit canonical A evidence."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import time

os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TRANSFORMERS_OFFLINE"] = "1"
os.environ["PYTORCH_ENABLE_MPS_FALLBACK"] = "0"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--device", choices=["mps", "cuda"], required=True)
    args = parser.parse_args()
    # The trusted released package verifies its source, weights, dependencies
    # and CPU vectors before this explicitly different device experiment.
    sys.path.insert(0, str(args.artifact.resolve() / "runner"))
    from common import configure_cpu, load_artifact, logits_for, software, token_ids, torch
    configure_cpu()
    available = torch.backends.mps.is_available() if args.device == "mps" else torch.cuda.is_available()
    if not available:
        raise ValueError(f"{args.device} unavailable; no automatic device fallback")
    torch.backends.cuda.matmul.allow_tf32 = False
    torch.backends.cudnn.allow_tf32 = False
    synchronize = torch.mps.synchronize if args.device == "mps" else torch.cuda.synchronize
    start = time.perf_counter()
    manifest, tokenizer, model, temperature = load_artifact(args.artifact, "infer.py")
    model.to(args.device)
    synchronize()
    runtime = f"{args.device}_float32_eager_exploratory"
    print(json.dumps({
        "schema": "slop_ninja_b_screen_runtime_v1", "weight_artifact_id": manifest["artifact_id"],
        "runtime": runtime, "canonical_evidence": False, "software": software(),
        "bridge_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "startup_seconds": time.perf_counter() - start, "batch_size": 1,
        "cpu_fallback": False, "tf32": False,
    }, allow_nan=False), flush=True)
    for line in sys.stdin:
        request = json.loads(line)
        result = {
            "schema": "slop_ninja_b_screen_observation_v1", "id": request["id"],
            "weight_artifact_id": manifest["artifact_id"], "runtime": runtime,
            "canonical_evidence": False, "labels": manifest["labels"],
            "probabilities": None, "model_or_assistance_probability": None,
        }
        start = time.perf_counter()
        try:
            ids = token_ids(tokenizer, request["text"], manifest["max_tokens"])
        except ValueError as error:
            result.update(status="unsupported_input", reason=str(error))
        else:
            logits = logits_for(model, tokenizer, [{"ids": ids}], 1, args.device)
            synchronize()
            probabilities = (logits / temperature).softmax(-1)[0].tolist()
            result.update(status="ok", token_count=len(ids), probabilities=probabilities,
                          model_or_assistance_probability=sum(probabilities[1:]))
        result["elapsed_seconds"] = time.perf_counter() - start
        print(json.dumps(result, allow_nan=False), flush=True)


if __name__ == "__main__":
    main()

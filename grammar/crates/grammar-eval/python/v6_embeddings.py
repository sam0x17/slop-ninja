"""Extract frozen Slop Ninja V6 classifier inputs; Rust owns request preparation.

CLI (Python 3.12, artifact's runner/requirements.lock, no additional packages)::

    python v6_embeddings.py --artifact /root/task/runtime/encoder-linux-compatible \
        --requests /root/task/requests.jsonl --output /root/task/v6-embeddings-run1

Run in a durable parent-owned job with a finite memory cap and zero swap. This
CPU-only bridge accepts exactly {"id": nonempty_string, "text": string} per line.
It neither discovers corpora nor accepts labels, provenance, or fitting inputs.
Output must be NEW, outside the artifact. requests.jsonl preserves input bytes;
rows.jsonl is flushed/fsynced after every row. manifest.json binds both files and
contains the full representation_contract, also in contract/manifest.json.

SIGINT/SIGTERM preserve the completed prefix and an interrupted manifest. There
is deliberately no in-place resume: Rust may prepare the remaining IDs for a new
run and join by id/text_sha256 and the identical representation_contract_sha256.
SIGKILL/OOM may leave a running manifest or partial last line; only a manifest
whose status is complete certifies a complete run. Never infer completion from
the existence of rows.jsonl. No model loading occurs when importing this module.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import importlib.util
import inspect
import json
import math
import os
from pathlib import Path
import platform
import resource
import shutil
import signal
import struct
import sys
import time


DIMENSION = 768
MAX_TOKENS = 2048
PARENT_ID = "4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138"
PARENT_MANIFEST_SHA256 = "1f4700f2a1855ca9cec5d0b9fbbe830a2a07588a4e95a77a9a754bd6cc3de73f"
PARENT_REQUIREMENTS_SHA256 = "7abddfab826bd4ec669223c276b06449f07a159bcb542c412f5ac9aa0fd7ee24"
CPU_ENV = {"MKL_CBWR": "COMPATIBLE", "ATEN_CPU_CAPABILITY": "default",
           "OMP_NUM_THREADS": "1", "MKL_NUM_THREADS": "1"}
OFFLINE_ENV = {"HF_HUB_OFFLINE": "1", "TRANSFORMERS_OFFLINE": "1",
               "TOKENIZERS_PARALLELISM": "false"}
LABELS = ["human_only", "model_only", "mixed"]


def demand(condition, message):
    if not condition:
        raise ValueError(message)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"),
                      ensure_ascii=True, allow_nan=False).encode("utf-8")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def sha256(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def write_json(path, value):
    """Durable atomic manifest replacement; caller owns the new output directory."""
    path = Path(path)
    temporary = path.with_name(path.name + ".tmp")
    with temporary.open("wb") as handle:
        handle.write(canonical(value) + b"\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)
    directory = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def read_json(path):
    return json.loads(Path(path).read_bytes())


def artifact_binding(artifact):
    """Pin the release or its CPU packaging variant before executing runner code.

    Reconstructing the parent manifest verifies ALL original file bindings, not
    just a provenance assertion. The immutable parent need not be mounted into
    the isolated runtime. load_artifact subsequently checks every actual file.
    """
    manifest = read_json(artifact / "manifest.json")
    body = {k: v for k, v in manifest.items() if k != "artifact_id"}
    demand(digest(canonical(body)) == manifest.get("artifact_id"), "artifact identity mismatch")
    if manifest["artifact_id"] == PARENT_ID:
        demand(sha256(artifact / "manifest.json") == PARENT_MANIFEST_SHA256,
               "frozen parent manifest differs")
        profile = "cpu_float32_eager_parent_v1"
    else:
        reconstructed = json.loads(canonical(body))
        files = reconstructed["files"]
        demand("RUNTIME_VARIANT.json" in files and "runner/infer_compatible.py" in files,
               "only the frozen parent or compatible CPU variant is supported")
        del files["RUNTIME_VARIANT.json"]
        del files["runner/infer_compatible.py"]
        files["runner/requirements.lock"] = PARENT_REQUIREMENTS_SHA256
        runtime = reconstructed["reference_runtime"]
        demand(runtime.pop("fixed_environment", None) == CPU_ENV, "CPU profile differs")
        demand(runtime["packages"]["torch"] == "2.14.0+cpu", "CPU PyTorch pin differs")
        runtime["packages"]["torch"] = "2.14.0"
        demand(digest(canonical(reconstructed)) == PARENT_ID, "variant changes frozen V6 contents")
        variant = read_json(artifact / "RUNTIME_VARIANT.json")
        demand(variant.get("parent_artifact_id") == PARENT_ID and
               variant.get("parent_manifest_sha256") == PARENT_MANIFEST_SHA256 and
               variant.get("fixed_environment") == CPU_ENV and
               variant.get("entrypoint") == "runner/infer_compatible.py" and
               variant.get("cpu_kernel_profile") == "mkl-compatible-aten-default-v1",
               "variant provenance differs")
        profile = "cpu_float32_eager_mkl_compatible_aten_default_v1"
    for name in manifest["files"]:
        relative = Path(name)
        demand(not relative.is_absolute() and ".." not in relative.parts,
               "unsafe artifact member")
        demand((artifact / relative).resolve(strict=True).is_relative_to(artifact),
               "artifact member resolves outside artifact")
        # No unverified source executes. Full large-file verification is left to
        # the unmodified loader to avoid hashing the weights twice.
        if name.startswith("runner/") or name == "RUNTIME_VARIANT.json":
            demand(sha256(artifact / name) == manifest["files"][name], "runner binding differs")
    demand(manifest["max_tokens"] == MAX_TOKENS and manifest["labels"] == LABELS,
           "V6 input/output contract differs")
    return manifest, profile


def configure_environment(manifest):
    demand("torch" not in sys.modules, "start a fresh process: environment must precede torch import")
    os.environ.update(OFFLINE_ENV)
    os.environ.update({"OMP_NUM_THREADS": "1", "MKL_NUM_THREADS": "1"})
    os.environ.update(manifest["reference_runtime"].get("fixed_environment", {}))
    # Do not create __pycache__ inside the recovered artifact or its runner.
    sys.dont_write_bytecode = True


def load_runtime(artifact, expected_manifest):
    configure_environment(expected_manifest)
    spec = importlib.util.spec_from_file_location("_slop_ninja_frozen_v6_common",
                                                artifact / "runner/common.py")
    common = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(common)
    common.configure_cpu()
    # Pass the actual bundled inference entrypoint, NEVER this extractor's name.
    manifest, tokenizer, model, temperature = common.load_artifact(artifact, "infer.py")
    demand(manifest == expected_manifest, "artifact changed during loading")
    demand(model.__class__.__name__ == "ModernBertForSequenceClassification" and
           model.config.hidden_size == DIMENSION and model.config.classifier_pooling == "mean" and
           isinstance(model.classifier, common.torch.nn.Linear) and
           model.classifier.in_features == DIMENSION and model.classifier.out_features == 3,
           "unexpected classifier representation")
    demand(all(p.dtype == common.torch.float32 and p.device.type == "cpu" for p in model.parameters()),
           "model must remain CPU float32")
    model.requires_grad_(False)
    return common, tokenizer, model, temperature


@contextmanager
def capture_classifier_input(model, torch):
    """Read-only pre-hook, after mean pooling, prediction head and eval dropout."""
    captured = []

    def capture(_module, args):
        demand(len(args) == 1 and tuple(args[0].shape) == (1, DIMENSION),
               "expected one classifier input of shape [1, 768]")
        demand(args[0].dtype == torch.float32 and args[0].device.type == "cpu",
               "classifier input must be CPU float32")
        captured.append(args[0].detach().clone())
        # Returning None preserves the original classifier argument object.

    handle = model.classifier.register_forward_pre_hook(capture)
    try:
        yield captured
    finally:
        handle.remove()


def forward(common, tokenizer, model, ids, temperature, capture=True):
    started = time.perf_counter()
    if capture:
        with capture_classifier_input(model, common.torch) as values:
            logits = common.logits_for(model, tokenizer, [{"ids": ids}], 1, "cpu")
        demand(len(values) == 1, "classifier hook must fire exactly once per forward")
        embedding = values[0][0].tolist()
        demand(all(math.isfinite(v) for v in embedding), "nonfinite embedding")
    else:
        logits = common.logits_for(model, tokenizer, [{"ids": ids}], 1, "cpu")
        embedding = None
    elapsed = time.perf_counter() - started
    demand(tuple(logits.shape) == (1, 3) and bool(common.torch.isfinite(logits).all()),
           "invalid logits")
    probabilities = (logits / temperature).softmax(-1)[0].tolist()
    demand(all(math.isfinite(p) for p in probabilities), "nonfinite probabilities")
    return {"embedding": embedding, "logits": logits[0].tolist(),
            "probabilities": probabilities, "elapsed_forward_seconds": elapsed}


def qualify_hook(artifact, common, tokenizer, model, temperature):
    """In addition to load_artifact's unchanged 1e-6 bundled reference checks."""
    evidence = []
    for vector in read_json(artifact / "reference_vectors.json"):
        ids = common.token_ids(tokenizer, vector["text"], MAX_TOKENS)
        baseline = forward(common, tokenizer, model, ids, temperature, capture=False)
        hooked = forward(common, tokenizer, model, ids, temperature)
        demand(hooked["logits"] == baseline["logits"] and
               hooked["probabilities"] == baseline["probabilities"], "hook changed original outputs")
        error = max(abs(a - b) for a, b in zip(hooked["probabilities"], vector["probabilities"]))
        demand(len(ids) == vector["token_count"] and error <= common.CPU_PROBABILITY_TOLERANCE,
               "hooked CPU reference replay failed")
        evidence.append({"text_sha256": digest(vector["text"].encode("utf-8")),
                         "token_count": len(ids), "original_logits": baseline["logits"],
                         "original_probabilities": baseline["probabilities"],
                         "hook_preserves_logits_and_probabilities_exactly": True,
                         "reference_probability_max_absolute_error": error,
                         "unhooked_forward_seconds": baseline["elapsed_forward_seconds"],
                         "hooked_forward_seconds": hooked["elapsed_forward_seconds"]})
    demand(bool(evidence), "missing hook qualification references")
    return evidence


def representation_contract(manifest, profile, common, model, temperature):
    return {
        "schema": "slop_ninja_v6_penultimate_contract_v1", "dimension": DIMENSION,
        "artifact_id": manifest["artifact_id"], "parent_artifact_id": PARENT_ID,
        "parent_manifest_sha256": PARENT_MANIFEST_SHA256,
        "artifact_files_sha256": manifest["files"],
        "model": {"architecture": "ModernBertForSequenceClassification", "frozen": True,
                  "training": False, "temperature": temperature, "labels": LABELS},
        "input": {"keys": ["id", "text"], "learned_inputs": ["text"],
                  "id": "nonempty unique string; routing only", "text": "exact UTF-8, unchanged",
                  "preprocessing": manifest["preprocessing"], "truncation": False,
                  "max_tokens_including_special_tokens": MAX_TOKENS,
                  "empty": "unsupported_input; token_count=0; tokenizer not called",
                  "over_limit": "unsupported_input; token_count=full untruncated length"},
        "hook": {"module": "model.classifier", "api": "register_forward_pre_hook",
                 "argument": 0, "shape": [1, DIMENSION],
                 "semantics": "attention-mask mean pooling -> prediction head -> eval dropout -> classifier input",
                 "capture": "detach().clone(); return None; exactly once per document",
                 "normalization": "none", "projection": "none"},
        "embedding_dtype": "float32", "embedding_sha256_encoding": "768 contiguous little-endian IEEE754 float32",
        "logits": "original common.logits_for; FP32 model logits cast to float64",
        "probabilities": "original float64 softmax(logits / frozen temperature)",
        "runtime": {"profile": profile, "reference_runtime": manifest["reference_runtime"],
                    "environment": {k: os.environ.get(k) for k in sorted(CPU_ENV | OFFLINE_ENV)},
                    "device": "cpu", "attention": "eager", "batch_size": 1,
                    "autocast": False, "compile": False, "threads": 1, "interop_threads": 1,
                    "python": platform.python_version(), "system": platform.system(),
                    "machine": platform.machine(), "packages": common.software()["packages"]},
        "source": {"extractor_sha256": sha256(Path(__file__)),
                   "model_class_source_sha256": sha256(inspect.getfile(type(model))),
                   "classifier_class_source_sha256": sha256(inspect.getfile(type(model.classifier)))},
        "qualification": {"loader": "bundled common.load_artifact(artifact, 'infer.py')",
                          "bundled_probability_absolute_tolerance": 1e-6,
                          "hook_vs_unhooked_logits_and_probabilities": "exact equality on bundled references",
                          "scope": "CPU extraction only; no CUDA or other-runtime equivalence claim"},
    }


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        demand(key not in result, "duplicate JSON key")
        result[key] = value
    return result


def requests(path):
    seen = set()
    with Path(path).open("rb") as handle:
        for index, raw in enumerate(handle, 1):
            try:
                request = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_object)
                demand(isinstance(request, dict) and set(request) == {"id", "text"},
                       "requests accept exactly id and text")
                demand(isinstance(request["id"], str) and bool(request["id"]), "id must be a nonempty string")
                demand(isinstance(request["text"], str), "text must be a string")
                request["id"].encode("utf-8")
                request["text"].encode("utf-8")
                demand(request["id"] not in seen, "duplicate request id")
            except (ValueError, UnicodeError) as error:
                # Never include the private line or parser exception's document.
                raise ValueError(f"invalid request at line {index}") from None
            seen.add(request["id"])
            yield index, request, digest(raw)


def memory_snapshot():
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    result = {"process_peak_rss_bytes": int(peak * (1 if sys.platform == "darwin" else 1024))}
    if Path("/proc/self/status").exists():
        for line in Path("/proc/self/status").read_text().splitlines():
            key, _, value = line.partition(":")
            if key in {"VmRSS", "VmSwap"}:
                result[{"VmRSS": "process_rss_bytes", "VmSwap": "process_swap_bytes"}[key]] = int(value.split()[0]) * 1024
    return result


def resource_limits():
    limits = {"rlimits": {name: list(resource.getrlimit(getattr(resource, name)))
                         for name in ("RLIMIT_AS", "RLIMIT_DATA", "RLIMIT_CPU", "RLIMIT_NOFILE")},
              "cgroup_v2": [], "scheduling": "parent-owned durable job; one sequential CPU worker",
              "enforcement": "external memory/swap caps; extractor records observed limits"}
    root = Path("/sys/fs/cgroup")
    membership = Path("/proc/self/cgroup")
    if membership.exists():
        for line in membership.read_text().splitlines():
            if line.startswith("0::"):
                relative = Path(line[3:].lstrip("/"))
                current = root / relative if ".." not in relative.parts else root
                while current.is_relative_to(root):
                    values = {name: (current / name).read_text().strip()
                              for name in ("memory.max", "memory.swap.max", "cpu.max", "pids.max")
                              if (current / name).is_file()}
                    if values:
                        limits["cgroup_v2"].append({"path": str(current), **values})
                    if current == root:
                        break
                    current = current.parent
    return limits


def extract_row(request, common, tokenizer, model, temperature, artifact_id):
    started = time.perf_counter()
    row = {"id": request["id"], "text_sha256": digest(request["text"].encode("utf-8")),
           "status": "unsupported_input", "embedding": None, "embedding_sha256": None,
           "token_count": 0, "logits": None, "probabilities": None,
           "model_or_assistance_probability": None, "labels": LABELS,
           "artifact_id": artifact_id, "runtime": "cpu_float32_eager", "elapsed_forward_seconds": 0.0}
    try:
        ids = common.token_ids(tokenizer, request["text"], None)
    except ValueError as error:
        if str(error) != "empty_text":
            raise
        row["reason"] = "empty_text"
    else:
        row["token_count"] = len(ids)
        if len(ids) > MAX_TOKENS:
            row["reason"] = f"unsupported_length:{len(ids)}>{MAX_TOKENS}"
        else:
            row.update(forward(common, tokenizer, model, ids, temperature))
            row["status"] = "ok"
            row["embedding_sha256"] = digest(struct.pack("<768f", *row["embedding"]))
            row["model_or_assistance_probability"] = row["probabilities"][1] + row["probabilities"][2]
    row["elapsed_request_seconds"] = time.perf_counter() - started
    row["memory"] = memory_snapshot()
    return row


class Interrupted(Exception):
    pass


def run(args):
    artifact = args.artifact.resolve(strict=True)
    source = args.requests.resolve(strict=True)
    output = args.output.resolve()
    demand(not args.output.exists() and not args.output.is_symlink(), "output must be a NEW directory")
    demand(not output.is_relative_to(artifact), "output must be outside the immutable artifact")
    manifest, profile = artifact_binding(artifact)
    output.mkdir(parents=True, mode=0o700)
    (output / "contract").mkdir(mode=0o700)
    rows_path = output / "rows.jsonl"
    rows_path.touch(mode=0o600, exist_ok=False)
    started = time.perf_counter()
    state = {"schema": "slop_ninja_v6_embeddings_run_v1", "status": "initializing",
             "dimension": DIMENSION, "requested": 0, "supported": 0, "unsupported": 0, "processed": 0,
             "requests_sha256": None, "rows_sha256": None, "representation_contract": None,
             "representation_contract_sha256": None,
             "runtime_evidence": {"profile": profile, "artifact_path": str(artifact),
                                  "artifact_manifest_sha256": sha256(artifact / "manifest.json"),
                                  "resource_limits": resource_limits(), "memory_at_start": memory_snapshot(),
                                  "host": platform.node(), "platform": platform.platform(),
                                  "executable": sys.executable},
             "recovery": "no in-place resume; Rust may re-request remaining IDs in a new output directory"}

    def checkpoint():
        state["rows_sha256"] = sha256(rows_path)
        state["elapsed_seconds"] = time.perf_counter() - started
        state["runtime_evidence"]["memory"] = memory_snapshot()
        write_json(output / "manifest.json", state)

    old_handlers = {}

    def interrupt(signum, _frame):
        # Let a second signal retain the normal process termination behavior.
        signal.signal(signum, signal.SIG_DFL)
        raise Interrupted(f"signal_{signum}")

    try:
        for signum in (signal.SIGINT, signal.SIGTERM):
            old_handlers[signum] = signal.signal(signum, interrupt)
        with source.open("rb") as incoming, (output / "requests.jsonl").open("xb") as snapshot:
            shutil.copyfileobj(incoming, snapshot, length=1024 * 1024)
            snapshot.flush()
            os.fsync(snapshot.fileno())
        state["requests_sha256"] = sha256(output / "requests.jsonl")
        state["requested"] = sum(1 for _ in requests(output / "requests.jsonl"))
        checkpoint()
        load_started = time.perf_counter()
        common, tokenizer, model, temperature = load_runtime(artifact, manifest)
        state["runtime_evidence"]["load_and_original_reference_seconds"] = time.perf_counter() - load_started
        state["runtime_evidence"]["original_dependency_and_reference_checks_passed"] = True
        state["runtime_evidence"]["hook_reference_replay"] = qualify_hook(artifact, common, tokenizer, model, temperature)
        state["runtime_evidence"]["software"] = common.software()
        state["runtime_evidence"]["torch_build_config"] = common.torch.__config__.show()
        contract = representation_contract(manifest, profile, common, model, temperature)
        state["representation_contract"] = contract
        state["representation_contract_sha256"] = digest(canonical(contract))
        write_json(output / "contract/manifest.json", contract)
        state["status"] = "running"
        checkpoint()
        with rows_path.open("ab", buffering=0) as rows:
            for index, request, request_sha256 in requests(output / "requests.jsonl"):
                row = extract_row(request, common, tokenizer, model, temperature, manifest["artifact_id"])
                row.update(request_index=index, request_sha256=request_sha256,
                           representation_contract_sha256=state["representation_contract_sha256"])
                encoded = canonical(row) + b"\n"
                # A signal is delivered only after the complete row and counters
                # are durable. Forward computation itself remains interruptible.
                blocked = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM})
                try:
                    view = memoryview(encoded)
                    while view:
                        written = rows.write(view)
                        demand(bool(written), "failed to write row")
                        view = view[written:]
                    os.fsync(rows.fileno())
                    state["processed"] += 1
                    state["supported" if row["status"] == "ok" else "unsupported"] += 1
                finally:
                    signal.pthread_sigmask(signal.SIG_SETMASK, blocked)
        demand(sha256(output / "requests.jsonl") == state["requests_sha256"], "request snapshot changed")
        state["status"] = "complete"
    except (Interrupted, KeyboardInterrupt) as error:
        state["status"] = "interrupted"
        state["failure_type"] = type(error).__name__
        raise
    except BaseException as error:
        state["status"] = "failed"
        state["failure_type"] = type(error).__name__
        raise
    finally:
        try:
            checkpoint()
        finally:
            for signum, handler in old_handlers.items():
                signal.signal(signum, handler)
    return state


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--artifact", required=True, type=Path, help="immutable frozen V6 or compatible CPU variant")
    parser.add_argument("--requests", required=True, type=Path, help="Rust-prepared UTF-8 id/text JSONL")
    parser.add_argument("--output", required=True, type=Path, help="NEW output directory, outside artifact")
    args = parser.parse_args(argv)
    os.umask(0o077)
    try:
        state = run(args)
    except (Interrupted, KeyboardInterrupt):
        print("interrupted; inspect the run manifest and completed rows", file=sys.stderr)
        return 130
    except Exception as error:
        # Do not print arbitrary model/tokenizer exceptions containing input text.
        print(f"extraction failed ({type(error).__name__}); inspect the run manifest", file=sys.stderr)
        return 1
    print(json.dumps({k: state[k] for k in ("status", "requested", "supported", "unsupported",
                                           "requests_sha256", "rows_sha256", "representation_contract_sha256")}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

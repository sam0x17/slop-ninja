"""Pinned PyTorch reference runtime; corpus admission belongs to the Rust CLI."""

from __future__ import annotations

import hashlib
import importlib.metadata
import json
import math
import os
from pathlib import Path
import platform
import shutil

# All model loads here are local. fetch_checkpoint.py is the only network step.
os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TRANSFORMERS_OFFLINE"] = "1"
os.environ["TOKENIZERS_PARALLELISM"] = "false"

import torch
from transformers import AutoModelForSequenceClassification, AutoTokenizer
from checkpoints import (CHECKPOINTS, DEFAULT_MODEL_ID, MODEL_FILES, TOKENIZER_FILES,
                         checkpoint_spec, validate_config)

LABELS = ["human_only", "model_only", "mixed"]
# Compatibility aliases for callers referring to the original default.
MODEL_ID = DEFAULT_MODEL_ID
REVISION = CHECKPOINTS[MODEL_ID]["revision"]
SCHEMA = "slop_ninja.encoder.v1"
RUNTIME_PACKAGES = ["torch", "transformers", "tokenizers", "safetensors", "numpy"]
CPU_PROBABILITY_TOLERANCE = 1e-6
PACKAGE_RUNNER_FILES = ("common.py", "checkpoints.py", "infer.py", "evaluate.py", "train.py", "audit.py",
                        "fetch_checkpoint.py", "smoke.py", "requirements.lock")
PACKAGE_CHECKPOINT_FILES = ("LICENSE", "UPSTREAM_README.md", "checkpoint.json")


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()


def sha256(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n")


def software():
    return {
        "python": platform.python_version(),
        "platform": platform.platform(),
        "packages": {p: importlib.metadata.version(p) for p in RUNTIME_PACKAGES},
    }


def configure_cpu(threads=1):
    if threads < 1:
        raise ValueError("CPU threads must be positive")
    torch.set_num_threads(threads)
    torch.set_num_interop_threads(1)


def read_checkpoint(path, *, tokenizer_only=False):
    path = Path(path)
    pin = json.loads((path / "checkpoint.json").read_text())
    spec = checkpoint_spec(pin)
    if not set(MODEL_FILES).issubset(pin["files"]):
        raise ValueError("checkpoint manifest lacks required model/tokenizer files")
    files = ({name: pin["files"][name] for name in TOKENIZER_FILES}
             if tokenizer_only else pin["files"])
    verify_files(path, files)
    validate_config(json.loads((path / "config.json").read_text()), spec)
    return pin, spec


def load_checkpoint(path, seed):
    path = Path(path)
    _, spec = read_checkpoint(path)
    torch.manual_seed(seed)
    tokenizer = AutoTokenizer.from_pretrained(path, local_files_only=True, trust_remote_code=False)
    model = AutoModelForSequenceClassification.from_pretrained(
        path, local_files_only=True, trust_remote_code=False, num_labels=3,
        id2label=dict(enumerate(LABELS)), label2id={v: i for i, v in enumerate(LABELS)},
        attn_implementation="eager", reference_compile=False, dtype=torch.float32, use_safetensors=True,
    )
    validate_config(model.config.to_dict(), spec)
    return tokenizer, model


def load_checkpoint_tokenizer(path):
    """Verify and load tokenizer assets without loading or executing the encoder."""
    path = Path(path)
    read_checkpoint(path, tokenizer_only=True)
    return AutoTokenizer.from_pretrained(path, local_files_only=True, trust_remote_code=False)


def verify_files(directory, files):
    for name, digest in files.items():
        p = Path(name)
        if p.is_absolute() or ".." in p.parts or not p.parts:
            raise ValueError("unsafe artifact path")
        if sha256(directory / p) != digest:
            raise ValueError(f"artifact hash mismatch: {name}")


def token_ids(tokenizer, text, max_tokens):
    if not isinstance(text, str) or not text.strip():
        raise ValueError("empty_text")
    # Include special tokens in the cap. No truncation, normalization or windows.
    ids = tokenizer(text, add_special_tokens=True, truncation=False)["input_ids"]
    if max_tokens is not None and len(ids) > max_tokens:
        raise ValueError(f"unsupported_length:{len(ids)}>{max_tokens}")
    return ids


def load_partition(path, split, tokenizer, max_tokens):
    """Load an already admitted/exported Rust shard without relabeling or filtering."""
    rows = []
    with Path(path).open() as f:
        for line_number, line in enumerate(f, 1):
            row = json.loads(line)
            if row.get("schema") != "slop-ninja-origin-record-v1":
                raise ValueError(f"{path}:{line_number}: expected Rust origin-record export")
            if row["split"] != split or row["origin"] not in LABELS:
                raise ValueError(f"{path}:{line_number}: unexpected split/origin")
            digest = hashlib.sha256(row["text"].encode()).hexdigest()
            if digest != row["text_sha256"]:
                raise ValueError(f"{path}:{line_number}: text hash mismatch")
            rows.append({
                "id": row["id"], "source_group": row["source_group"], "hash": digest,
                "ids": token_ids(tokenizer, row["text"], max_tokens),
                "label": LABELS.index(row["origin"]), "evidence": row["evidence"],
                "share_alike": row["rights"]["license"].startswith("CC-BY-SA-"),
            })
    if not rows:
        raise ValueError(f"empty {split} partition")
    return rows


def check_partition_separation(partitions):
    # Recheck exported shard boundaries at the ML handoff; never create splits here.
    for field in ["id", "source_group", "hash"]:
        seen = {}
        for split, rows in partitions.items():
            for row in rows:
                previous = seen.setdefault(row[field], split)
                if previous != split:
                    raise ValueError(f"cross-partition {field} overlap: {previous}/{split}")


def class_counts(rows):
    return [sum(r["label"] == i for r in rows) for i in range(3)]


def batch_tensors(rows, tokenizer, device):
    batch = tokenizer.pad({"input_ids": [r["ids"] for r in rows]}, return_tensors="pt")
    return {k: v.to(device) for k, v in batch.items() if k in ("input_ids", "attention_mask")}


@torch.inference_mode()
def logits_for(model, tokenizer, rows, batch_size, device):
    model.eval()
    logits = []
    for start in range(0, len(rows), batch_size):
        batch = batch_tensors(rows[start:start + batch_size], tokenizer, device)
        logits.append(model(**batch).logits.cpu())
    return torch.cat(logits).double()


def metrics(logits, labels, temperature=1.0):
    probabilities = torch.softmax(logits.double() / temperature, dim=-1)
    labels = torch.tensor(labels, dtype=torch.long)
    predictions = probabilities.argmax(-1)
    confusion = [[int(((labels == i) & (predictions == j)).sum()) for j in range(3)] for i in range(3)]
    one_hot = torch.nn.functional.one_hot(labels, 3)
    return {
        "count": len(labels),
        "class_counts": [int((labels == i).sum()) for i in range(3)],
        "accuracy": float((predictions == labels).double().mean()),
        "log_loss": float(torch.nn.functional.cross_entropy(logits.double() / temperature, labels)),
        "multiclass_brier": float(((probabilities - one_hot) ** 2).sum(-1).mean()),
        "confusion_true_rows_predicted_columns": confusion,
        "per_class": {
            LABELS[i]: {
                "precision": confusion[i][i] / n if (n := sum(row[i] for row in confusion)) else None,
                "recall": confusion[i][i] / n if (n := sum(confusion[i])) else None,
            } for i in range(3)
        },
    }


def fit_temperature(logits, labels):
    """Unweighted calibration preserves the observed calibration class mixture."""
    labels = torch.tensor(labels, dtype=torch.long)
    # Deterministic bounded scalar search; no training class weights or resampling.
    grid = torch.linspace(-3.0, 3.0, 601, dtype=torch.float64)
    losses = [float(torch.nn.functional.cross_entropy(logits / math.exp(float(t)), labels)) for t in grid]
    index = min(range(len(losses)), key=losses.__getitem__)
    return {"temperature": math.exp(float(grid[index])), "log_temperature_bounds": [-3.0, 3.0],
            "log_temperature_step": 0.01, "boundary_optimum": index in (0, len(grid) - 1),
            "method": "unweighted_calibration_nll_grid", "class_counts": [int((labels == i).sum()) for i in range(3)],
            "class_priors": [float((labels == i).double().mean()) for i in range(3)]}


def runtime_requirements():
    """Keep exported install instructions consistent with the numeric runtime."""
    lines = (Path(__file__).parent / "requirements.lock").read_text().splitlines()
    pins = dict(line.split("==", 1) for line in lines if "==" in line)
    actual = software()["packages"]
    for name, version in actual.items():
        if name == "torch":
            if version not in {"2.14.0", "2.14.0+cpu"} or pins.get(name) not in {"2.14.0", "2.14.0+cpu"}:
                raise ValueError("unqualified PyTorch build; declare and verify a separate runtime profile")
        elif pins.get(name) != version:
            raise ValueError(f"runtime package {name} differs from the pinned source requirements")
    torch_lines = [index for index, line in enumerate(lines) if line.startswith("torch==")]
    if len(torch_lines) != 1:
        raise ValueError("expected one pinned PyTorch requirement")
    lines[torch_lines[0]] = f"torch=={actual['torch']}"
    cpu_index = "--extra-index-url https://download.pytorch.org/whl/cpu"
    lines = [line for line in lines if line != cpu_index]
    if actual["torch"] == "2.14.0+cpu":
        lines.insert(0, cpu_index)
    return "\n".join(lines) + "\n"


def check_package_sources(checkpoint):
    """Reject incomplete runner snapshots and checkpoint notices before fitting."""
    paths = [Path(__file__).parent / name for name in PACKAGE_RUNNER_FILES]
    paths.extend(Path(checkpoint) / name for name in PACKAGE_CHECKPOINT_FILES)
    missing = [str(path) for path in paths if not path.is_file()]
    if missing:
        raise FileNotFoundError("missing artifact package files: " + ", ".join(missing))
    runtime_requirements()


def package_artifact(output, tokenizer, model, calibration, training, max_tokens, checkpoint, data_rights=None):
    check_package_sources(checkpoint)
    pin, spec = read_checkpoint(checkpoint)
    validate_config(model.config.to_dict(), spec)
    requirements = runtime_requirements()
    output = Path(output)
    output.mkdir(parents=True, exist_ok=False)
    model.cpu().eval().save_pretrained(output / "classifier", safe_serialization=True)
    tokenizer.save_pretrained(output / "classifier")
    write_json(output / "calibration.json", calibration)
    write_json(output / "training.json", training)
    if data_rights is not None:
        write_json(output / "DATA_RIGHTS.json", data_rights)
    runner = output / "runner"
    runner.mkdir()
    for name in PACKAGE_RUNNER_FILES:
        shutil.copyfile(Path(__file__).parent / name, runner / name)
    (runner / "requirements.lock").write_text(requirements)
    if torch.get_num_threads() != 1:
        raise ValueError("package reference vectors using the single-threaded CPU runtime")
    reference_vectors = []
    for text in [".", "This synthetic sentence checks the reference runtime."]:
        ids = token_ids(tokenizer, text, None)
        if len(ids) <= max_tokens:
            logits = logits_for(model, tokenizer, [{"ids": ids}], 1, "cpu")
            reference_vectors.append({"text": text, "token_count": len(ids),
                                      "probabilities": (logits / calibration["temperature"]).softmax(-1)[0].tolist()})
    if not reference_vectors:
        raise ValueError("token limit cannot support a nonempty reference input")
    write_json(output / "reference_vectors.json", reference_vectors)
    write_json(output / "MODEL_CARD.json", {
        "status": training["status"], "architecture": f"{spec['architecture']} with a three-class classification head",
        "upstream_model": pin["model_id"], "upstream_revision": pin["revision"],
        "upstream_weight_license": spec["weight_license"],
        "upstream_url": f"https://huggingface.co/{pin['model_id']}/tree/{pin['revision']}",
        "fine_tuned_weight_license": data_rights["content"]["model_release_license"] if data_rights else None,
        "data_attribution_file": "DATA_RIGHTS.json" if data_rights else None,
        "labels": LABELS, "language_scope": "English", "max_tokens_including_special_tokens": max_tokens,
        "limitations": ["No Pangram parity claim.", "No subnet qualification claim.",
                        "Production history can be ambiguous from text alone.",
                        "Scores are document-origin probabilities, not AI word fractions.",
                        "No deployment population prior adjustment is fitted."],
    })
    # Include upstream notices shipped by the fetch step without assuming corpus licenses.
    checkpoint = Path(checkpoint)
    for name in PACKAGE_CHECKPOINT_FILES:
        shutil.copyfile(checkpoint / name, output / name)
    if data_rights and data_rights["content"]["model_release_license"] == "CC-BY-SA-4.0":
        (output / "LICENSE").rename(output / "UPSTREAM_LICENSE")
        (output / "LICENSE").write_text(
            "Fine-tuned weights: Creative Commons Attribution-ShareAlike 4.0 International.\n"
            "https://creativecommons.org/licenses/by-sa/4.0/\n"
            "Copyright Slop Ninja Research, where applicable. No warranties.\n"
            "See DATA_RIGHTS.json for source attribution and changes, and UPSTREAM_LICENSE\n"
            "for base-model terms. Runtime code retains its separate license.\n")
    metadata = {
        "schema": SCHEMA, "labels": LABELS, "max_tokens": max_tokens,
        "preprocessing": "exact_utf8_hf_tokenizer_no_truncation_v1",
        "reference_runtime": {"device": "cpu", "dtype": "float32", "probability_dtype": "float64",
                              "threads": 1, "request_batch_size": 1, "attention": "eager", "python_major_minor": [3, 12],
                              "probability_absolute_tolerance": CPU_PROBABILITY_TOLERANCE,
                              "boundary_rule": "use_cpu_reference_probabilities_no_threshold_in_runner",
                              "packages": software()["packages"]},
        "files": {str(p.relative_to(output)): sha256(p) for p in sorted(output.rglob("*")) if p.is_file()},
    }
    metadata["artifact_id"] = hashlib.sha256(canonical(metadata)).hexdigest()
    write_json(output / "manifest.json", metadata)
    return metadata


def load_artifact(path, entrypoint):
    path = Path(path)
    manifest = json.loads((path / "manifest.json").read_text())
    claimed = manifest.pop("artifact_id")
    if hashlib.sha256(canonical(manifest)).hexdigest() != claimed:
        raise ValueError("manifest content hash mismatch")
    manifest["artifact_id"] = claimed
    if manifest["schema"] != SCHEMA or manifest["labels"] != LABELS:
        raise ValueError("unsupported artifact schema or label order")
    verify_files(path, manifest["files"])
    observed_model_files = {str(p.relative_to(path)) for p in (path / "classifier").rglob("*") if p.is_file()}
    expected_model_files = {p for p in manifest["files"] if p.startswith("classifier/")}
    if observed_model_files != expected_model_files:
        raise ValueError("unexpected classifier files outside the content manifest")
    for name in ["common.py", "checkpoints.py", entrypoint]:
        if sha256(Path(__file__).parent / name) != manifest["files"][f"runner/{name}"]:
            raise ValueError("execute the bundled runner: source differs from the artifact")
    if software()["packages"] != manifest["reference_runtime"]["packages"]:
        raise ValueError("runtime package versions differ; install the bundled requirements.lock")
    contract = manifest["reference_runtime"]
    if contract["python_major_minor"] != [int(x) for x in platform.python_version_tuple()[:2]]:
        raise ValueError("Python major/minor version differs from the reference runtime")
    expected_contract = {"device": "cpu", "dtype": "float32", "probability_dtype": "float64", "threads": 1,
                         "request_batch_size": 1, "attention": "eager", "probability_absolute_tolerance": CPU_PROBABILITY_TOLERANCE}
    if any(contract.get(key) != value for key, value in expected_contract.items()) or torch.get_num_threads() != 1:
        raise ValueError("unsupported CPU reference contract")
    tokenizer = AutoTokenizer.from_pretrained(path / "classifier", local_files_only=True, trust_remote_code=False)
    model = AutoModelForSequenceClassification.from_pretrained(
        path / "classifier", local_files_only=True, trust_remote_code=False,
        attn_implementation="eager", reference_compile=False, dtype=torch.float32, use_safetensors=True,
    ).cpu().eval()
    temperature = json.loads((path / "calibration.json").read_text())["temperature"]
    if not math.isfinite(temperature) or temperature <= 0:
        raise ValueError("invalid calibration temperature")
    vectors = json.loads((path / "reference_vectors.json").read_text())
    if not vectors:
        raise ValueError("missing CPU reference vectors")
    for vector in vectors:
        ids = token_ids(tokenizer, vector["text"], manifest["max_tokens"])
        logits = logits_for(model, tokenizer, [{"ids": ids}], 1, "cpu")
        actual = (logits / temperature).softmax(-1)[0]
        expected = torch.tensor(vector["probabilities"], dtype=torch.float64)
        if len(ids) != vector["token_count"] or expected.shape != (3,) or not torch.isfinite(expected).all():
            raise ValueError("invalid CPU reference vector")
        if not torch.isfinite(actual).all() or float((actual - expected).abs().max()) > CPU_PROBABILITY_TOLERANCE:
            raise ValueError("CPU reference probabilities exceed the artifact tolerance")
    return manifest, tokenizer, model, temperature

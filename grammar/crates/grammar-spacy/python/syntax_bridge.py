"""Transport spaCy annotations for the exact input; Rust derives all features."""

import argparse
from functools import lru_cache
import hashlib
import json
import re
from pathlib import Path
import sys
from typing import Any


def _model_files_sha256(root: Path) -> str:
    """Bind model weights, tokenizer, config and loader source across installs.

    Relative file names and byte hashes are canonical; installation paths and
    generated Python bytecode do not enter the model identity.
    """
    if not root.is_dir():
        raise OSError(f"Parser model directory does not exist: {root}")
    records = []
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root)
        if not path.is_file() or "__pycache__" in relative.parts or path.suffix in {".pyc", ".pyo"}:
            continue
        digest = hashlib.sha256()
        size = 0
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
                size += len(chunk)
        records.append([relative.as_posix(), size, digest.hexdigest()])
    if not records:
        raise OSError("Parser model directory contains no hashable files")
    payload = json.dumps(
        ["grammar-spacy-model-files-v1", records], ensure_ascii=True, separators=(",", ":")
    ).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


@lru_cache(maxsize=4)
def _load_parser(model: str, paragraph_boundaries: bool = False, paragraph_content: bool = False) -> tuple[Any, str]:
    """Load a versioned, locally installed dependency parser."""
    if paragraph_boundaries and paragraph_content:
        raise ValueError("Select only one paragraph profile.")
    try:
        import spacy
    except ImportError as exc:
        raise RuntimeError(
            "Syntax annotation requires spaCy and an English model. Install "
            "spaCy and en_core_web_sm in the selected Python interpreter."
        ) from exc
    try:
        model_root = spacy.util.get_package_path(model) if spacy.util.is_package(model) else Path(model)
        model_files = _model_files_sha256(model_root)
        nlp = spacy.load(model, exclude=["ner"])
    except (OSError, ImportError) as exc:
        raise RuntimeError(
            f"Cannot load spaCy model {model!r}. Install that model with "
            "python -m spacy download en_core_web_sm (or supply an installed model)."
        ) from exc
    if nlp.path is None or not nlp.path.resolve().is_relative_to(model_root.resolve()):
        raise ValueError("Loaded parser assets are outside the fingerprinted model directory.")
    if model_files != _model_files_sha256(model_root):
        raise ValueError("Parser model files changed while loading; retry from a stable installation.")
    if nlp.lang != "en":
        raise ValueError("Syntax annotation requires an English spaCy model.")
    if "parser" not in nlp.pipe_names or "lemmatizer" not in nlp.pipe_names:
        raise ValueError("Syntax annotation requires an enabled dependency parser and lemmatizer.")
    model_name = nlp.meta.get("name", model)
    model_version = nlp.meta.get("version", "unknown")
    if model_version == "unknown":
        raise ValueError("The spaCy model must declare a version for reproducible features.")
    identity = (
        f"spacy={spacy.__version__};model={nlp.lang}_{model_name}@{model_version}"
        f";model-files-sha256={model_files}"
    )
    if paragraph_boundaries:
        identity += ";paragraph-documents-v1"
    if paragraph_content:
        identity += ";paragraph-content-v2"
    return nlp, identity

def annotate(text: str, nlp: Any, loaded_identity: str) -> dict[str, Any]:
    content_only = ";paragraph-content-v2" in loaded_identity
    if ";paragraph-documents-v1" not in loaded_identity and not content_only:
        return _annotate_whole(text, nlp, loaded_identity)
    # Parse the blocks independently, including tagging. Merely forcing sentence
    # starts after tok2vec/tagger still lets a heading affect the next subject.
    cuts = [0] + [m.end() for m in re.finditer(r"\r?\n[ \t]*\r?\n", text)]
    if cuts[-1] != len(text):
        cuts.append(len(text))
    result = {"text": text, "parser_identity": "grammar-spacy-annotation-v1;" + loaded_identity + ";raw-text-v1", "tokens": [], "sentences": []}
    for start, end in zip(cuts, cuts[1:]):
        if content_only:
            # Keep layout in result.text and source offsets, but do not ask NLP
            # to interpret paragraph-edge whitespace as a sentence or its head.
            block = text[start:end]
            start += len(block) - len(block.lstrip())
            end -= len(block) - len(block.rstrip())
            if start >= end:
                continue
        part = _annotate_whole(text[start:end], nlp, loaded_identity)
        token_offset = len(result["tokens"])
        sentence_offset = len(result["sentences"])
        for token in part["tokens"]:
            token["i"] += token_offset
            token["head"] += token_offset
            token["sentence"] += sentence_offset
            token["start_char"] += start
            token["end_char"] += start
            result["tokens"].append(token)
        for sentence in part["sentences"]:
            for field in ["root", "token_start", "token_end"]:
                sentence[field] += token_offset
            sentence["start_char"] += start
            sentence["end_char"] += start
            result["sentences"].append(sentence)
    return result

def _annotate_whole(text: str, nlp: Any, loaded_identity: str) -> dict[str, Any]:
    """Use the same annotation and identity for individual and batched inputs."""
    doc = nlp(text)
    if doc.text != text:
        raise RuntimeError("The parser changed its input text.")
    if len(doc) and any(
        not doc.has_annotation(attribute)
        for attribute in ("DEP", "POS", "TAG", "LEMMA", "SENT_START")
    ):
        raise RuntimeError("The parser omitted required syntactic annotations.")
    sentence_ids = {}
    sentences = []
    for index, sentence in enumerate(doc.sents):
        sentences.append({
            "start_char": sentence.start_char,
            "end_char": sentence.end_char,
            "root": sentence.root.i,
            "token_start": sentence.start,
            "token_end": sentence.end,
        })
        sentence_ids.update((token.i, index) for token in sentence)
    tokens = [{
        "i": token.i,
        "start_char": token.idx,
        "end_char": token.idx + len(token.text),
        "text": token.text,
        "lemma": token.lemma_,
        "pos": token.pos_,
        "tag": token.tag_,
        "dep": token.dep_,
        "head": token.head.i,
        "sentence": sentence_ids[token.i],
        "morph": {key: token.morph.get(key) for key in token.morph.to_dict()},
        "is_punct": token.is_punct,
        "is_space": token.is_space,
    } for token in doc]
    parser_identity = loaded_identity
    return {
        "text": text,
        "parser_identity": "grammar-spacy-annotation-v1;" + parser_identity + ";raw-text-v1",
        "tokens": tokens,
        "sentences": sentences,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", default="en_core_web_sm")
    parser.add_argument("--batch", action="store_true")
    parser.add_argument("--stream", action="store_true")
    profiles = parser.add_mutually_exclusive_group()
    profiles.add_argument("--paragraph-boundaries", action="store_true")
    profiles.add_argument("--paragraph-content", action="store_true")
    args = parser.parse_args()
    if args.stream:
        if args.batch:
            parser.error("Choose batch or stream transport, not both.")
        nlp, loaded_identity = _load_parser(args.model, args.paragraph_boundaries, args.paragraph_content)
        for index, line in enumerate(sys.stdin):
            source = json.loads(line)
            if not isinstance(source, str):
                raise ValueError("Stream input must contain JSON strings.")
            try:
                result = {"index": index, "status": "ok", "annotation": annotate(source, nlp, loaded_identity)}
            except Exception as exc:
                result = {"index": index, "status": "error", "text": source, "error": f"{type(exc).__name__}: {exc}"}
            print(json.dumps(result, ensure_ascii=False, allow_nan=False), flush=True)
        return
    text = sys.stdin.buffer.read().decode("utf-8")
    if args.batch:
        texts = json.loads(text)
        if not isinstance(texts, list) or any(not isinstance(item, str) for item in texts):
            raise ValueError("Batch input must be a JSON array of strings.")
        # Read the entire input before producing output, so the caller can finish
        # writing stdin before it drains stdout. Bound batch sizes at the caller.
        records = []
        if texts:
            nlp, loaded_identity = _load_parser(args.model, args.paragraph_boundaries, args.paragraph_content)
            for index, source in enumerate(texts):
                try:
                    annotation = annotate(source, nlp, loaded_identity)
                    records.append({"index": index, "status": "ok", "annotation": annotation})
                except Exception as exc:
                    records.append({
                        "index": index,
                        "status": "error",
                        "text": source,
                        "error": f"{type(exc).__name__}: {exc}",
                    })
        result = records
    else:
        nlp, loaded_identity = _load_parser(args.model, args.paragraph_boundaries, args.paragraph_content)
        result = annotate(text, nlp, loaded_identity)
    json.dump(result, sys.stdout, ensure_ascii=False, allow_nan=False)


if __name__ == "__main__":
    main()

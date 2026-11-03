"""Local corpus work, explicit corpus generation, and bounded detector studies."""

import argparse
import json
import math
from pathlib import Path
import sys

from .features import extract
from .statistics import contrast, document_profile, vector
from .store import add_document, add_run, connect, digest, now, profile, status
from .experiments import compare_features, substitute


def emit(value):
    print(json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False))


def read(path):
    # newline='' preserves submitted CRLF bytes when text is hashed/replayed.
    with Path(path).open(encoding="utf-8", newline="") as f:
        return f.read()


def write_json(path, value):
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    temporary = target.with_name(target.name + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False) + "\n", encoding="utf-8")
    temporary.replace(target)


def extraction(args, text):
    return extract(text, grammar=args.grammar, model=args.parser_model)


def filters(args):
    return dict(family=args.family, split=args.split, domain=args.domain,
                register=args.register, extractor=args.extractor)


def run_study(args):
    from .evaluation import PangramClient, PangramTaskFailed, PangramTimeout, paired_study, summarize_runs
    if args.repeats < 1:
        raise ValueError("Repeats must be positive")
    if not math.isfinite(args.threshold) or not 0 < args.threshold <= 1:
        raise ValueError("Threshold must be finite and in (0, 1]")
    if not math.isfinite(args.timeout) or args.timeout <= 0 or args.max_requests < 0:
        raise ValueError("Timeout must be positive and finite; request budget must be nonnegative")
    if not args.model.strip():
        raise ValueError("An explicit nonempty detector model selector is required")
    baseline, candidate = read(args.baseline), read(args.candidate)
    if baseline == candidate:
        raise ValueError("Candidate equals baseline")
    if min(len(baseline.split()), len(candidate.split())) < 50:
        raise ValueError("Both study inputs must contain at least 50 words of complete prose")
    audit = json.loads(read(args.audit)) if args.audit else {}
    if not isinstance(audit, dict):
        raise ValueError("Quality audit must be a JSON object")
    if audit and (audit.get("source_sha256") != digest(baseline) or audit.get("candidate_sha256") != digest(candidate)):
        raise ValueError("Quality audit must identify these exact source and candidate hashes")
    folder = Path(args.out)
    folder.mkdir(parents=True, exist_ok=True)
    manifest = {"baseline_sha256": digest(baseline), "candidate_sha256": digest(candidate),
                "model": args.model, "repeats": args.repeats,
                "full_document": args.full_document}
    manifest_path = folder / "manifest.json"
    if manifest_path.exists() and json.loads(read(manifest_path)) != manifest:
        raise ValueError("Study directory already belongs to a different study")
    write_json(manifest_path, manifest)
    order = []
    for i in range(args.repeats):
        # Counterbalance order across replicate blocks instead of always baseline first.
        roles = ("baseline", "candidate") if i % 2 == 0 else ("candidate", "baseline")
        order.extend((role, i, folder / f"{i + 1:02d}-{role}.json") for role in roles)
    missing = sum(not p.exists() for _, _, p in order)
    if missing > args.max_requests:
        raise ValueError(f"Study needs {missing} new paid requests; --max-requests is {args.max_requests}")
    # Check every cached entry before issuing any new request.
    completed = []
    for role, _, path in order:
        if path.exists():
            cached = json.loads(read(path))
            expected = baseline if role == "baseline" else candidate
            if (cached.get("sha256") != digest(expected) or cached.get("submitted_text") != expected
                    or cached.get("model") != args.model or cached.get("full_document") != args.full_document):
                raise ValueError(f"Cached record mismatch: {path}")
            if not cached.get("task_id"):
                raise ValueError(f"Submission outcome uncertain in {path}; recover its task ID before resuming. No automatic resubmission.")
            if cached.get("result", {}).get("stage") == "STAGE_FAILED":
                raise ValueError(f"Recorded task failed in {path}; inspect the stored result before starting another study")
            if cached.get("result", {}).get("stage") == "STAGE_SUCCESS":
                completed.append(cached)
    if completed:
        cached_summary = summarize_runs(completed, threshold=args.threshold)
        if cached_summary["duplicate_records_ignored"]:
            raise ValueError("Cached study slots must contain distinct detector task IDs")
    client = None
    for role, i, path in order:
        text = baseline if role == "baseline" else candidate
        if path.exists():
            record = json.loads(read(path))
            if record.get("sha256") != digest(text) or record.get("model") != args.model:
                raise ValueError(f"Cached record mismatch: {path}")
        else:
            client = client or PangramClient()
            # A transport failure or interruption may occur after billing. Leave
            # a durable intent so a rerun cannot silently submit that slot twice.
            intent = {"state": "submission_uncertain", "detector": "pangram", "model": args.model,
                      "submitted_text": text, "sha256": digest(text), "submitted_at": now(),
                      "full_document": args.full_document, "role": role, "block": i}
            write_json(path, intent)
            record = client.submit(text, model=args.model)
            record.update({"full_document": args.full_document, "role": role, "block": i})
            write_json(path, record)  # persist task before polling; reruns resume
        if record.get("result", {}).get("stage") != "STAGE_SUCCESS":
            if record.get("result", {}).get("stage") == "STAGE_FAILED":
                raise ValueError(f"Recorded task failed in {path}; inspect the stored result before starting another study")
            client = client or PangramClient()
            try:
                record["result"] = client.poll(record["task_id"], timeout=args.timeout)
            except PangramTaskFailed as exc:
                record["result"] = exc.result
                write_json(path, record)
                raise
            except PangramTimeout as exc:
                record["last_poll_response"] = exc.last_response
                write_json(path, record)
                raise
            write_json(path, record)
        summarize_runs([record], threshold=args.threshold)
        print(f"Recorded {role} repeat {i + 1}/{args.repeats}", file=sys.stderr, flush=True)
    groups = {role: [json.loads(read(p)) for r, _, p in order if r == role] for role in ("baseline", "candidate")}
    if args.full_document:
        result = paired_study(groups["baseline"], groups["candidate"], audit, threshold=args.threshold, min_repeats=max(3, args.repeats))
    else:
        result = {"scope": "section", "baseline": summarize_runs(groups["baseline"], args.threshold),
                  "candidate": summarize_runs(groups["candidate"], args.threshold),
                  "reliability_established": False,
                  "interpretation": "Section experiments require a separate full-document confirmation before acceptance."}
    write_json(folder / "summary.json", result)
    emit(result)


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--db", default="data/unslop.sqlite3")
    sub = p.add_subparsers(dest="command", required=True)
    sub.add_parser("init")
    sub.add_parser("status")
    ingest = sub.add_parser("ingest", help="Import attributed corpus JSONL atomically")
    ingest.add_argument("jsonl")
    old = sub.add_parser("import-pangram", help="Import historical runs as one exploratory source group")
    old.add_argument("directory")
    old.add_argument("--corpus", default="preface-experiments")
    old.add_argument("--group-id", default="cit-preface")
    for q in (ingest, old):
        q.add_argument("--grammar", action="store_true")
        q.add_argument("--parser-model", default="en_core_web_sm")
    for name in ("profile", "compare", "analyze"):
        q = sub.add_parser(name)
        q.add_argument("--family", default="word")
        q.add_argument("--split", choices=("train", "dev", "test", "exploratory"), default="train")
        q.add_argument("--domain")
        q.add_argument("--register")
        q.add_argument("--extractor")
        if name == "profile":
            q.add_argument("--corpus", required=True)
        else:
            q.add_argument("--limit", type=int, default=30)
            q.add_argument("--min-count", type=int, default=5)
            q.add_argument("--min-documents", type=int, default=3)
        if name == "compare":
            q.add_argument("--left", required=True)
            q.add_argument("--right", required=True)
        if name == "analyze":
            q.add_argument("text")
            q.add_argument("--reference", required=True)
            q.add_argument("--group-id", help="Exclude source family from the reference profile")
            q.add_argument("--grammar", action="store_true")
            q.add_argument("--parser-model", default="en_core_web_sm")
    pair = sub.add_parser("inspect-pair")
    pair.add_argument("source")
    pair.add_argument("candidate")
    pair.add_argument("--grammar", action="store_true")
    pair.add_argument("--parser-model", default="en_core_web_sm")
    perturb = sub.add_parser("perturb")
    perturb.add_argument("source")
    perturb.add_argument("--old", required=True)
    perturb.add_argument("--new", required=True)
    perturb.add_argument("--occurrence", type=int, default=1)
    perturb.add_argument("--out", required=True)
    summaries = sub.add_parser("runs", help="Group stored runs by exact input and detector version")
    summaries.add_argument("--corpus")
    summaries.add_argument("--threshold", type=float, default=.1)
    study = sub.add_parser("study", help="Run a bounded, resumable baseline/candidate detector study")
    study.add_argument("baseline")
    study.add_argument("candidate")
    study.add_argument("--out", required=True)
    study.add_argument("--repeats", type=int, default=3)
    study.add_argument("--max-requests", type=int, required=True)
    study.add_argument("--model", default="pangram-4")
    study.add_argument("--timeout", type=float, default=600)
    study.add_argument("--threshold", type=float, default=.1)
    scope = study.add_mutually_exclusive_group(required=True)
    scope.add_argument("--full-document", action="store_true")
    scope.add_argument("--section", action="store_true")
    study.add_argument("--audit")
    collect = sub.add_parser("collect", help="Generate attributed model corpus JSONL through a provider API")
    collect.add_argument("prompts")
    collect.add_argument("--provider", choices=("openai", "anthropic"), required=True)
    collect.add_argument("--model", required=True)
    collect.add_argument("--corpus", required=True)
    collect.add_argument("--out", required=True)
    collect.add_argument("--max-requests", type=int, required=True)
    collect.add_argument("--max-output-tokens", type=int, default=2048)
    return p


def main(argv=None):
    args = parser().parse_args(argv)
    try:
        if args.command == "study":
            return run_study(args)
        if args.command == "collect":
            from .collection import CollectionError, collect_one, load_prompts
            prompts = load_prompts(args.prompts)
            if len(prompts) > args.max_requests:
                raise ValueError("Prompt count exceeds --max-requests")
            out = Path(args.out)
            out.parent.mkdir(parents=True, exist_ok=True)
            # Exclusive create prevents a rerun from silently paying again/overwriting a corpus.
            with out.open("x", encoding="utf-8") as f:
                for prompt in prompts:
                    try:
                        doc = collect_one(prompt, args.provider, args.model, args.corpus, max_output_tokens=args.max_output_tokens)
                    except CollectionError as exc:
                        with out.with_suffix(out.suffix + ".attempts.jsonl").open("a", encoding="utf-8") as attempts:
                            attempts.write(json.dumps(exc.record, ensure_ascii=False, allow_nan=False) + "\n")
                        raise
                    f.write(json.dumps(doc, ensure_ascii=False, allow_nan=False) + "\n")
                    f.flush()
            return emit({"documents": len(prompts), "path": str(out)})
        if args.command == "perturb":
            candidate, manifest = substitute(read(args.source), args.old, args.new, args.occurrence)
            out = Path(args.out)
            out.parent.mkdir(parents=True, exist_ok=True)
            if out.exists() or out.with_suffix(out.suffix + ".json").exists():
                raise ValueError("Candidate output already exists")
            out.write_text(candidate, encoding="utf-8", newline="")
            write_json(out.with_suffix(out.suffix + ".json"), manifest)
            return emit(manifest)
        if args.command == "inspect-pair":
            a, b = read(args.source), read(args.candidate)
            return emit(compare_features(a, b, extraction(args, a), extraction(args, b)))
        with connect(args.db) as db:
            if args.command in {"init", "status"}:
                return emit(status(db))
            if args.command == "ingest":
                documents = [json.loads(line) for line in read(args.jsonl).splitlines() if line.strip()]
                added = sum(add_document(db, d, extraction(args, d["text"]))[1] for d in documents)
                return emit({"imported_extractions": added, "records": len(documents)})
            if args.command == "import-pangram":
                paths = sorted(Path(args.directory).glob("*.json"))
                if not paths:
                    raise ValueError("No result JSON files found")
                added = 0
                skipped = 0
                for path in paths:
                    record = json.loads(read(path))
                    if "submitted_text" not in record:
                        if path.name in {"manifest.json", "summary.json"}:
                            skipped += 1
                            continue
                        raise ValueError(f"Not a detector run: {path}")
                    if not record.get("task_id") or record.get("result", {}).get("stage") != "STAGE_SUCCESS":
                        raise ValueError(f"Detector run incomplete; resume study first: {path}")
                    document = {"text": record["submitted_text"], "corpus": args.corpus,
                                "source_kind": "experimental", "domain": "philosophy", "register": "book-preface",
                                "group_id": args.group_id, "split": "exploratory",
                                "metadata": {"first_import_path": str(path), "generator_provenance": "unverified"}}
                    doc_id, _ = add_document(db, document, extraction(args, document["text"]))
                    added += add_run(db, doc_id, record)
                return emit({"imported_runs": added, "files": len(paths), "non_run_files_skipped": skipped, "status": status(db)})
            if args.command == "profile":
                return emit(profile(db, args.corpus, **filters(args)))
            if args.command == "compare":
                left, right = [profile(db, name, **filters(args)) for name in (args.left, args.right)]
                values = contrast(left, right, args.min_count, args.min_documents)
                return emit({"left": {k: v for k, v in left.items() if k not in {"counts", "document_counts"}},
                             "right": {k: v for k, v in right.items() if k not in {"counts", "document_counts"}},
                             "same_stratum_names": left["strata"] == right["strata"],
                             "note": "Ranks are exploratory; match topic/register and validate across independent source groups.",
                             "features": values[:args.limit]})
            if args.command == "analyze":
                text = read(args.text)
                extracted = extraction(args, text)
                groups = {r[0] for r in db.execute("SELECT group_id FROM documents WHERE sha256=?", (digest(text),))}
                if args.group_id:
                    groups.add(args.group_id)
                ref = profile(db, args.reference, exclude_groups=sorted(groups), **filters(args))
                values = contrast(document_profile(extracted, args.family), ref, args.min_count, args.min_documents)
                return emit({"sha256": digest(text), "reference_total": ref["total"], "reference_groups": ref["groups"],
                             "excluded_groups": sorted(groups), "extractor": extracted["extractor"],
                             "metrics": extracted["metrics"], "vector": vector(extracted, args.family), "anomalies": values[:args.limit]})
            if args.command == "runs":
                from .evaluation import summarize_runs
                sql = "SELECT r.record_json FROM detector_runs r JOIN documents d ON d.id=r.document_id"
                rows = db.execute(sql + (" WHERE d.corpus=?" if args.corpus else ""), (args.corpus,) if args.corpus else ())
                return emit(summarize_runs([json.loads(r[0]) for r in rows], threshold=args.threshold))
    except (ValueError, RuntimeError, KeyError, OSError) as exc:
        print(f"unslop: {exc}", file=sys.stderr)
        raise SystemExit(1) from None


if __name__ == "__main__":
    main()

"""SQLite corpus provenance, per-document counts, and immutable detector records."""

from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import sqlite3

SCHEMA_VERSION = "1"
KINDS = {"human", "model", "experimental", "unknown", "synthetic_fixture"}
SPLITS = {"train", "dev", "test", "exploratory"}


def digest(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def now():
    return datetime.now(timezone.utc).isoformat()


def connect(path):
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(path)
    db.row_factory = sqlite3.Row
    db.execute("PRAGMA foreign_keys = ON")
    db.executescript("""
    CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS documents(
        id INTEGER PRIMARY KEY, corpus TEXT NOT NULL, sha256 TEXT NOT NULL,
        text TEXT NOT NULL, source_kind TEXT NOT NULL, provider TEXT,
        model TEXT, domain TEXT NOT NULL, register TEXT NOT NULL,
        group_id TEXT NOT NULL, split TEXT NOT NULL,
        metadata_json TEXT NOT NULL, created_at TEXT NOT NULL,
        UNIQUE(corpus, sha256)
    );
    CREATE INDEX IF NOT EXISTS document_groups ON documents(group_id, split);
    CREATE INDEX IF NOT EXISTS document_hashes ON documents(sha256, split);
    CREATE TABLE IF NOT EXISTS extractions(
        document_id INTEGER NOT NULL REFERENCES documents(id),
        extractor TEXT NOT NULL, metrics_json TEXT NOT NULL,
        PRIMARY KEY(document_id, extractor)
    );
    CREATE TABLE IF NOT EXISTS totals(
        document_id INTEGER NOT NULL, extractor TEXT NOT NULL,
        family TEXT NOT NULL, total INTEGER NOT NULL CHECK(total >= 0),
        PRIMARY KEY(document_id, extractor, family),
        FOREIGN KEY(document_id, extractor) REFERENCES extractions(document_id, extractor)
    );
    CREATE TABLE IF NOT EXISTS features(
        document_id INTEGER NOT NULL, extractor TEXT NOT NULL,
        family TEXT NOT NULL, feature TEXT NOT NULL,
        count INTEGER NOT NULL CHECK(count > 0),
        PRIMARY KEY(document_id, extractor, family, feature),
        FOREIGN KEY(document_id, extractor, family)
            REFERENCES totals(document_id, extractor, family)
    );
    CREATE TABLE IF NOT EXISTS detector_runs(
        id INTEGER PRIMARY KEY, detector TEXT NOT NULL, run_key TEXT NOT NULL,
        document_id INTEGER NOT NULL REFERENCES documents(id),
        record_json TEXT NOT NULL, imported_at TEXT NOT NULL,
        UNIQUE(detector, run_key)
    );
    CREATE VIEW IF NOT EXISTS word_counts AS
        SELECT d.corpus, d.split, f.extractor, f.feature AS word,
               SUM(f.count) AS occurrences, COUNT(*) AS document_count
        FROM features f JOIN documents d ON d.id=f.document_id
        WHERE f.family='word'
        GROUP BY d.corpus, d.split, f.extractor, f.feature;
    CREATE VIEW IF NOT EXISTS corpus_totals AS
        SELECT d.corpus, d.split, t.extractor, t.family,
               SUM(t.total) AS total, COUNT(*) AS document_count
        FROM totals t JOIN documents d ON d.id=t.document_id
        GROUP BY d.corpus, d.split, t.extractor, t.family;
    """)
    old = db.execute("SELECT value FROM settings WHERE key='schema_version'").fetchone()
    if old and old[0] != SCHEMA_VERSION:
        raise ValueError(f"Unsupported database schema {old[0]}")
    db.execute("INSERT OR IGNORE INTO settings VALUES ('schema_version', ?)", (SCHEMA_VERSION,))
    db.commit()
    return db


def add_document(db, document, extracted):
    """Caller owns transaction. Duplicate imports never inflate counts."""
    required = ("text", "corpus", "source_kind", "domain", "register", "group_id", "split")
    for field in required:
        if not isinstance(document.get(field), str) or not document[field].strip():
            raise ValueError(f"Document requires nonempty {field}")
    if document["source_kind"] not in KINDS or document["split"] not in SPLITS:
        raise ValueError("Unknown source_kind or split")
    if document["source_kind"] == "model" and not all(document.get(k) for k in ("provider", "model")):
        raise ValueError("Model corpus requires exact provider and returned model")
    if document["source_kind"] in {"experimental", "unknown", "synthetic_fixture"} and document["split"] != "exploratory":
        raise ValueError("Unverified or experimental documents must use exploratory split")
    sha = digest(document["text"])
    leak = db.execute("""SELECT id FROM documents
        WHERE (group_id=? OR sha256=?) AND split<>? LIMIT 1""",
        (document["group_id"], sha, document["split"])).fetchone()
    if leak:
        raise ValueError("Source group or identical text already belongs to another split")
    fields = ("corpus", "source_kind", "provider", "model", "domain", "register", "group_id", "split")
    metadata = dict(document.get("metadata", {}))
    metadata.update({k: v for k, v in document.items() if k not in {*fields, "text", "metadata"}})
    existing = db.execute("SELECT * FROM documents WHERE corpus=? AND sha256=?", (document["corpus"], sha)).fetchone()
    if existing:
        if any(existing[k] != document.get(k) for k in fields):
            raise ValueError("Duplicate text has conflicting provenance")
        doc_id = existing["id"]
    else:
        cursor = db.execute("""INSERT INTO documents
            (corpus, sha256, text, source_kind, provider, model, domain, register,
             group_id, split, metadata_json, created_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)""",
            (document["corpus"], sha, document["text"], document["source_kind"],
             document.get("provider"), document.get("model"), document["domain"],
             document["register"], document["group_id"], document["split"],
             json.dumps(metadata, sort_keys=True), now()))
        doc_id = cursor.lastrowid
    extractor = extracted["extractor"]
    if db.execute("SELECT 1 FROM extractions WHERE document_id=? AND extractor=?", (doc_id, extractor)).fetchone():
        return doc_id, False
    db.execute("INSERT INTO extractions VALUES (?,?,?)", (doc_id, extractor, json.dumps(extracted["metrics"])))
    for family, counts in extracted["families"].items():
        total = extracted["totals"][family]
        if not isinstance(total, int) or total < 0:
            raise ValueError("Invalid feature denominator")
        if family != "construction" and sum(counts.values()) != total:
            raise ValueError("Non-construction counts must sum to their family denominator")
        db.execute("INSERT INTO totals VALUES (?,?,?,?)", (doc_id, extractor, family, total))
        for feature, count in counts.items():
            if not isinstance(count, int) or count < 0 or count > total:
                raise ValueError("Feature count must fit its opportunity denominator")
            if count:
                db.execute("INSERT INTO features VALUES (?,?,?,?,?)", (doc_id, extractor, family, feature, count))
    return doc_id, True


def add_run(db, document_id, record):
    """Retain every unique request, including repeated input and full API output."""
    if digest(record["submitted_text"]) != record["sha256"]:
        raise ValueError("Detector record submitted hash mismatch")
    doc = db.execute("SELECT sha256 FROM documents WHERE id=?", (document_id,)).fetchone()
    if not doc or doc[0] != record["sha256"]:
        raise ValueError("Detector record does not match document")
    detector = record.get("detector", "pangram")
    key = record.get("task_id") or digest(json.dumps(record, sort_keys=True))
    old = db.execute("SELECT record_json FROM detector_runs WHERE detector=? AND run_key=?", (detector, key)).fetchone()
    encoded = json.dumps(record, sort_keys=True)
    if old:
        if json.loads(old[0]) != record:
            raise ValueError("Existing task ID has conflicting result or provenance")
        return False
    db.execute("INSERT INTO detector_runs(detector,run_key,document_id,record_json,imported_at) VALUES (?,?,?,?,?)",
               (detector, key, document_id, encoded, now()))
    return True


def profile(db, corpus, family="word", split="train", domain=None, register=None, extractor=None, exclude_groups=()):
    filters = ["d.corpus=?", "d.split=?", "t.family=?"]
    params = [corpus, split, family]
    for field, value in (("domain", domain), ("register", register)):
        if value is not None:
            filters.append(f"d.{field}=?")
            params.append(value)
    if extractor:
        filters.append("t.extractor=?")
        params.append(extractor)
    if exclude_groups:
        filters.append("d.group_id NOT IN (" + ",".join("?" for _ in exclude_groups) + ")")
        params.extend(exclude_groups)
    where = " AND ".join(filters)
    rows = db.execute(f"SELECT d.id,d.group_id,d.source_kind,d.provider,d.model,d.domain,d.register,t.total,t.extractor FROM documents d JOIN totals t ON d.id=t.document_id WHERE {where}", params).fetchall()
    versions = {r["extractor"] for r in rows}
    if len(versions) > 1:
        raise ValueError("Multiple extractor versions in profile; select --extractor explicitly")
    if not rows:
        raise ValueError(f"No {family} observations for {corpus}/{split} with these filters")
    ids = [r["id"] for r in rows]
    # Join the same selected totals instead of a potentially huge IN list.
    counts = db.execute(f"""SELECT f.feature,SUM(f.count) AS count,COUNT(*) AS document_count,
        COUNT(DISTINCT d.group_id) AS group_count
        FROM documents d JOIN totals t ON d.id=t.document_id
        JOIN features f ON f.document_id=t.document_id AND f.extractor=t.extractor AND f.family=t.family
        WHERE {where} GROUP BY f.feature""", params).fetchall()
    return {"corpus": corpus, "split": split, "family": family,
            "extractor": next(iter(versions)), "total": sum(r["total"] for r in rows),
            "documents": len(ids), "groups": len({r["group_id"] for r in rows}),
            "provenance": [list(t) for t in sorted({(r["source_kind"], r["provider"] or "", r["model"] or "") for r in rows})],
            "strata": [list(t) for t in sorted({(r["domain"], r["register"]) for r in rows})],
            "counts": {r["feature"]: r["count"] for r in counts},
            "document_counts": {r["feature"]: r["document_count"] for r in counts},
            "group_counts": {r["feature"]: r["group_count"] for r in counts}}


def status(db):
    corpora = [dict(r) for r in db.execute("""SELECT corpus, source_kind, provider, model, split,
        COUNT(*) AS documents, COUNT(DISTINCT group_id) AS source_groups
        FROM documents GROUP BY corpus,source_kind,provider,model,split ORDER BY corpus""")]
    return {"schema_version": SCHEMA_VERSION, "corpora": corpora,
            "detector_runs": db.execute("SELECT COUNT(*) FROM detector_runs").fetchone()[0],
            "extractors": [r[0] for r in db.execute("SELECT DISTINCT extractor FROM extractions")]}

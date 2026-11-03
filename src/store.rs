//! SQLite provenance, per-document features, and immutable detector records.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::Path;

pub const SCHEMA_VERSION: &str = "1";

pub fn digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

pub fn connect(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create {}", parent.display()))?;
    }
    let db = Connection::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
    db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
    let old: Option<String> = db
        .query_row(
            "SELECT value FROM settings WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(version) = old
        && version != SCHEMA_VERSION
    {
        bail!("Unsupported database schema {version}");
    }
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS documents(
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
            SELECT d.corpus,d.split,f.extractor,f.feature AS word,
                   SUM(f.count) AS occurrences,COUNT(*) AS document_count
            FROM features f JOIN documents d ON d.id=f.document_id
            WHERE f.family='word'
            GROUP BY d.corpus,d.split,f.extractor,f.feature;
        CREATE VIEW IF NOT EXISTS corpus_totals AS
            SELECT d.corpus,d.split,t.extractor,t.family,
                   SUM(t.total) AS total,COUNT(*) AS document_count
            FROM totals t JOIN documents d ON d.id=t.document_id
            GROUP BY d.corpus,d.split,t.extractor,t.family;",
    )?;
    db.execute(
        "INSERT OR IGNORE INTO settings VALUES ('schema_version',?)",
        [SCHEMA_VERSION],
    )?;
    Ok(db)
}

fn required<'a>(object: &'a Value, field: &str) -> Result<&'a str> {
    object[field]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("Document requires nonempty {field}"))
}

fn optional<'a>(object: &'a Value, field: &str) -> Result<Option<&'a str>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => bail!("Document {field} must be a string or null"),
    }
}

fn valid_extraction(extracted: &Value) -> Result<()> {
    required(extracted, "extractor")?;
    if !extracted["metrics"].is_object() {
        bail!("Extraction requires metrics object");
    }
    let families = extracted["families"]
        .as_object()
        .context("Extraction requires feature families")?;
    let totals = extracted["totals"]
        .as_object()
        .context("Extraction requires feature totals")?;
    if families.keys().collect::<BTreeSet<_>>() != totals.keys().collect::<BTreeSet<_>>() {
        bail!("Feature families and denominators must match");
    }
    for (family, values) in families {
        let total = totals[family]
            .as_i64()
            .filter(|v| *v >= 0)
            .context("Invalid feature denominator")?;
        let counts = values
            .as_object()
            .context("Feature counts must be an object")?;
        let mut sum = 0_i64;
        for value in counts.values() {
            let count = value
                .as_i64()
                .filter(|v| *v >= 0 && *v <= total)
                .context("Feature count must fit its opportunity denominator")?;
            if family != "construction" {
                sum = sum
                    .checked_add(count)
                    .context("Feature counts overflow their denominator")?;
            }
        }
        if family != "construction" && sum != total {
            bail!("Non-construction counts must sum to their family denominator");
        }
    }
    Ok(())
}

/// Caller owns the transaction. Re-importing a document cannot inflate counts.
pub fn add_document(db: &Connection, document: &Value, extracted: &Value) -> Result<(i64, bool)> {
    for field in [
        "text",
        "corpus",
        "source_kind",
        "domain",
        "register",
        "group_id",
        "split",
    ] {
        required(document, field)?;
    }
    let text = required(document, "text")?;
    let corpus = required(document, "corpus")?;
    let kind = required(document, "source_kind")?;
    let domain = required(document, "domain")?;
    let register = required(document, "register")?;
    let group = required(document, "group_id")?;
    let split = required(document, "split")?;
    let (provider, model) = (
        optional(document, "provider")?,
        optional(document, "model")?,
    );
    if ![
        "human",
        "model",
        "experimental",
        "unknown",
        "synthetic_fixture",
    ]
    .contains(&kind)
        || !["train", "dev", "test", "exploratory"].contains(&split)
    {
        bail!("Unknown source_kind or split");
    }
    if kind == "model"
        && (!provider.is_some_and(|s| !s.trim().is_empty())
            || !model.is_some_and(|s| !s.trim().is_empty()))
    {
        bail!("Model corpus requires exact provider and returned model");
    }
    if ["experimental", "unknown", "synthetic_fixture"].contains(&kind) && split != "exploratory" {
        bail!("Unverified or experimental documents must use exploratory split");
    }
    valid_extraction(extracted)?;
    let sha = digest(text);
    let leak = db
        .query_row(
            "SELECT id FROM documents WHERE (group_id=? OR sha256=?) AND split<>? LIMIT 1",
            params![group, sha, split],
            |r| r.get::<_, i64>(0),
        )
        .optional()?;
    if leak.is_some() {
        bail!("Source group or identical text already belongs to another split");
    }
    let existing = db.query_row(
        "SELECT id,source_kind,provider,model,domain,register,group_id,split FROM documents WHERE corpus=? AND sha256=?",
        params![corpus, sha], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?,
            r.get::<_, String>(6)?, r.get::<_, String>(7)?))
    ).optional()?;
    let doc_id = if let Some((
        id,
        old_kind,
        old_provider,
        old_model,
        old_domain,
        old_register,
        old_group,
        old_split,
    )) = existing
    {
        if old_kind != kind
            || old_provider.as_deref() != provider
            || old_model.as_deref() != model
            || old_domain != domain
            || old_register != register
            || old_group != group
            || old_split != split
        {
            bail!("Duplicate text has conflicting provenance");
        }
        id
    } else {
        let mut metadata = match document.get("metadata") {
            None => Map::new(),
            Some(Value::Object(value)) => value.clone(),
            _ => bail!("Document metadata must be an object"),
        };
        let fields = [
            "corpus",
            "source_kind",
            "provider",
            "model",
            "domain",
            "register",
            "group_id",
            "split",
            "text",
            "metadata",
        ];
        for (key, value) in document.as_object().context("Document must be an object")? {
            if !fields.contains(&key.as_str()) {
                metadata.insert(key.clone(), value.clone());
            }
        }
        db.execute("INSERT INTO documents(corpus,sha256,text,source_kind,provider,model,domain,register,group_id,split,metadata_json,created_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
            params![corpus, sha, text, kind, provider, model, domain, register, group, split, serde_json::to_string(&metadata)?, Utc::now().to_rfc3339()])?;
        db.last_insert_rowid()
    };
    let extractor = required(extracted, "extractor")?;
    if db
        .query_row(
            "SELECT 1 FROM extractions WHERE document_id=? AND extractor=?",
            params![doc_id, extractor],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        return Ok((doc_id, false));
    }
    db.execute(
        "INSERT INTO extractions VALUES (?,?,?)",
        params![
            doc_id,
            extractor,
            serde_json::to_string(&extracted["metrics"])?
        ],
    )?;
    let mut insert_total = db.prepare_cached("INSERT INTO totals VALUES (?,?,?,?)")?;
    let mut insert_feature = db.prepare_cached("INSERT INTO features VALUES (?,?,?,?,?)")?;
    for (family, counts) in extracted["families"].as_object().unwrap() {
        let total = extracted["totals"][family].as_i64().unwrap();
        insert_total.execute(params![doc_id, extractor, family, total])?;
        for (feature, count) in counts.as_object().unwrap() {
            let count = count.as_i64().unwrap();
            if count > 0 {
                insert_feature.execute(params![doc_id, extractor, family, feature, count])?;
            }
        }
    }
    Ok((doc_id, true))
}

/// Preserve every unique request, including repeated input and full API output.
pub fn add_run(db: &Connection, document_id: i64, record: &Value) -> Result<bool> {
    let submitted = record["submitted_text"]
        .as_str()
        .context("Detector record requires submitted_text")?;
    let sha = record["sha256"]
        .as_str()
        .context("Detector record requires sha256")?;
    if digest(submitted) != sha {
        bail!("Detector record submitted hash mismatch");
    }
    let doc: Option<String> = db
        .query_row(
            "SELECT sha256 FROM documents WHERE id=?",
            [document_id],
            |r| r.get(0),
        )
        .optional()?;
    if doc.as_deref() != Some(sha) {
        bail!("Detector record does not match document");
    }
    let detector = record
        .get("detector")
        .map(|v| v.as_str().context("Detector name must be a string"))
        .transpose()?
        .unwrap_or("pangram");
    let encoded = serde_json::to_string(record)?;
    let task_id = record
        .get("task_id")
        .filter(|v| !v.is_null())
        .map(|v| v.as_str().context("Detector task ID must be a string"))
        .transpose()?
        .filter(|s| !s.is_empty());
    let key = task_id
        .map(str::to_string)
        .unwrap_or_else(|| digest(&encoded));
    let old: Option<String> = db
        .query_row(
            "SELECT record_json FROM detector_runs WHERE detector=? AND run_key=?",
            params![detector, key],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(old) = old {
        if serde_json::from_str::<Value>(&old)? != *record {
            bail!("Existing task ID has conflicting result or provenance");
        }
        return Ok(false);
    }
    if task_id.is_none() {
        // Python v0.1 hashed JSON with spaces and ASCII escapes. Compare records
        // for taskless imports so opening that schema cannot duplicate old runs.
        let mut query =
            db.prepare("SELECT record_json FROM detector_runs WHERE detector=? AND document_id=?")?;
        for old in query.query_map(params![detector, document_id], |r| r.get::<_, String>(0))? {
            if serde_json::from_str::<Value>(&old?)? == *record {
                return Ok(false);
            }
        }
    }
    db.execute("INSERT INTO detector_runs(detector,run_key,document_id,record_json,imported_at) VALUES (?,?,?,?,?)",
        params![detector, key, document_id, encoded, Utc::now().to_rfc3339()])?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
pub fn profile(
    db: &Connection,
    corpus: &str,
    family: &str,
    split: &str,
    domain: Option<&str>,
    register: Option<&str>,
    extractor: Option<&str>,
    exclude_groups: &[String],
) -> Result<Value> {
    let mut filters = vec![
        "d.corpus=?".to_string(),
        "d.split=?".to_string(),
        "t.family=?".to_string(),
    ];
    let mut values = vec![corpus.to_string(), split.to_string(), family.to_string()];
    for (field, value) in [
        ("d.domain", domain),
        ("d.register", register),
        ("t.extractor", extractor.filter(|s| !s.is_empty())),
    ] {
        if let Some(value) = value {
            filters.push(format!("{field}=?"));
            values.push(value.to_string());
        }
    }
    if !exclude_groups.is_empty() {
        filters.push(format!(
            "d.group_id NOT IN ({})",
            vec!["?"; exclude_groups.len()].join(",")
        ));
        values.extend_from_slice(exclude_groups);
    }
    let where_clause = filters.join(" AND ");
    let mut query = db.prepare(&format!("SELECT d.group_id,d.source_kind,d.provider,d.model,d.domain,d.register,t.total,t.extractor FROM documents d JOIN totals t ON d.id=t.document_id WHERE {where_clause}"))?;
    let rows = query
        .query_map(params_from_iter(&values), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, u64>(6)?,
                r.get::<_, String>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let versions: BTreeSet<_> = rows.iter().map(|r| r.7.as_str()).collect();
    if versions.len() > 1 {
        bail!("Multiple extractor versions in profile; select --extractor explicitly");
    }
    if rows.is_empty() {
        bail!("No {family} observations for {corpus}/{split} with these filters");
    }
    let mut counts = Map::new();
    let mut document_counts = Map::new();
    let mut group_counts = Map::new();
    let mut feature_query = db.prepare(&format!(
        "SELECT f.feature,SUM(f.count),COUNT(*),COUNT(DISTINCT d.group_id)
        FROM documents d JOIN totals t ON d.id=t.document_id
        JOIN features f ON f.document_id=t.document_id AND f.extractor=t.extractor AND f.family=t.family
        WHERE {where_clause} GROUP BY f.feature"
    ))?;
    let features = feature_query.query_map(params_from_iter(&values), |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u64>(1)?,
            r.get::<_, u64>(2)?,
            r.get::<_, u64>(3)?,
        ))
    })?;
    for feature in features {
        let (feature, count, documents, groups) = feature?;
        counts.insert(feature.clone(), json!(count));
        document_counts.insert(feature.clone(), json!(documents));
        group_counts.insert(feature, json!(groups));
    }
    let groups: BTreeSet<_> = rows.iter().map(|r| &r.0).collect();
    let provenance: BTreeSet<_> = rows
        .iter()
        .map(|r| {
            [
                &r.1[..],
                r.2.as_deref().unwrap_or(""),
                r.3.as_deref().unwrap_or(""),
            ]
        })
        .collect();
    let strata: BTreeSet<_> = rows.iter().map(|r| [&r.4[..], &r.5[..]]).collect();
    let total = rows
        .iter()
        .try_fold(0_u64, |total, r| total.checked_add(r.6))
        .context("Corpus opportunity count overflow")?;
    Ok(json!({"corpus":corpus, "split":split, "family":family,
        "extractor":versions.first().unwrap(), "total":total,
        "documents":rows.len(), "groups":groups.len(), "provenance":provenance,
        "strata":strata, "counts":counts, "document_counts":document_counts,"group_counts":group_counts}))
}

pub fn status(db: &Connection) -> Result<Value> {
    let mut query = db.prepare("SELECT corpus,source_kind,provider,model,split,COUNT(*),COUNT(DISTINCT group_id) FROM documents GROUP BY corpus,source_kind,provider,model,split ORDER BY corpus,source_kind,provider,model,split")?;
    let corpora = query.query_map([], |r| Ok(json!({
        "corpus":r.get::<_, String>(0)?, "source_kind":r.get::<_, String>(1)?,
        "provider":r.get::<_, Option<String>>(2)?, "model":r.get::<_, Option<String>>(3)?,
        "split":r.get::<_, String>(4)?, "documents":r.get::<_, u64>(5)?, "source_groups":r.get::<_, u64>(6)?,
    })))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let detector_runs: u64 =
        db.query_row("SELECT COUNT(*) FROM detector_runs", [], |r| r.get(0))?;
    let mut query = db.prepare("SELECT DISTINCT extractor FROM extractions ORDER BY extractor")?;
    let extractors = query
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(
        json!({"schema_version":SCHEMA_VERSION,"corpora":corpora,"detector_runs":detector_runs,"extractors":extractors}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::extract;

    fn db() -> Connection {
        connect(Path::new(":memory:")).unwrap()
    }
    fn document() -> Value {
        json!({"text":"A mind changes. A person remembers.","corpus":"human-sample","source_kind":"human",
            "domain":"philosophy","register":"essay","group_id":"source-1","split":"train"})
    }
    fn add(db: &Connection, doc: &Value) -> Result<(i64, bool)> {
        add_document(
            db,
            doc,
            &extract(doc["text"].as_str().unwrap(), false, "python3")?,
        )
    }
    fn words_profile(db: &Connection) -> Result<Value> {
        profile(db, "human-sample", "word", "train", None, None, None, &[])
    }

    #[test]
    fn dedup_does_not_inflate_words_or_views() {
        let db = db();
        assert_eq!(add(&db, &document()).unwrap(), (1, true));
        assert_eq!(add(&db, &document()).unwrap(), (1, false));
        let result = words_profile(&db).unwrap();
        assert_eq!(result["counts"]["a"], 2);
        assert_eq!(result["total"], 6);
        assert_eq!(
            db.query_row(
                "SELECT total FROM corpus_totals WHERE family='word'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            6
        );
    }

    #[test]
    fn groups_and_identical_text_cannot_cross_splits() {
        let db = db();
        add(&db, &document()).unwrap();
        let mut other = document();
        other["text"] = json!("Different prose.");
        other["corpus"] = json!("other-model");
        other["split"] = json!("test");
        assert!(
            add(&db, &other)
                .unwrap_err()
                .to_string()
                .contains("another split")
        );
        other["text"] = document()["text"].clone();
        other["group_id"] = json!("pretend-new-source");
        assert!(
            add(&db, &other)
                .unwrap_err()
                .to_string()
                .contains("another split")
        );
    }

    #[test]
    fn unverified_generator_cannot_be_training_data() {
        let db = db();
        let mut doc = document();
        doc["source_kind"] = json!("model");
        assert!(
            add(&db, &doc)
                .unwrap_err()
                .to_string()
                .contains("exact provider")
        );
        doc["source_kind"] = json!("experimental");
        assert!(
            add(&db, &doc)
                .unwrap_err()
                .to_string()
                .contains("exploratory")
        );
    }

    #[test]
    fn domain_and_group_filters_change_denominators() {
        let db = db();
        add(&db, &document()).unwrap();
        let mut doc = document();
        doc["text"] = json!("The rat runs.");
        doc["group_id"] = json!("source-2");
        doc["domain"] = json!("biology");
        add(&db, &doc).unwrap();
        let filtered = profile(
            &db,
            "human-sample",
            "word",
            "train",
            Some("biology"),
            None,
            None,
            &[],
        )
        .unwrap();
        let excluded = profile(
            &db,
            "human-sample",
            "word",
            "train",
            None,
            None,
            None,
            &["source-1".into()],
        )
        .unwrap();
        assert_eq!(filtered["total"], 3);
        assert_eq!(filtered["groups"], 1);
        assert_eq!(filtered["counts"], excluded["counts"]);
    }

    #[test]
    fn extractor_versions_cannot_be_pooled_implicitly() {
        let db = db();
        let doc = document();
        add(&db, &doc).unwrap();
        let mut extracted = extract(doc["text"].as_str().unwrap(), false, "python3").unwrap();
        extracted["extractor"] = json!("new-version");
        add_document(&db, &doc, &extracted).unwrap();
        assert!(
            words_profile(&db)
                .unwrap_err()
                .to_string()
                .contains("Multiple extractor")
        );
        assert_eq!(
            profile(
                &db,
                "human-sample",
                "word",
                "train",
                None,
                None,
                Some("new-version"),
                &[]
            )
            .unwrap()["total"],
            6
        );
    }

    #[test]
    fn caller_transaction_rolls_back_bad_import() {
        let db = db();
        db.execute_batch("BEGIN").unwrap();
        add(&db, &document()).unwrap();
        let mut bad = document();
        bad["text"] = json!("Bad split.");
        bad["split"] = json!("other");
        assert!(add(&db, &bad).is_err());
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(status(&db).unwrap()["corpora"], json!([]));
    }

    #[test]
    fn repeated_requests_are_retained_and_conflicting_tasks_rejected() {
        let db = db();
        let (id, _) = add(&db, &document()).unwrap();
        let text = document()["text"].as_str().unwrap().to_string();
        let mut record =
            json!({"task_id":"1","sha256":digest(&text),"submitted_text":text,"result":{}});
        assert!(add_run(&db, id, &record).unwrap());
        assert!(!add_run(&db, id, &record).unwrap());
        record["task_id"] = json!("2");
        assert!(add_run(&db, id, &record).unwrap());
        record["result"] = json!({"new":1});
        assert!(
            add_run(&db, id, &record)
                .unwrap_err()
                .to_string()
                .contains("conflicting")
        );
        assert_eq!(status(&db).unwrap()["detector_runs"], 2);
    }

    #[test]
    fn hashes_and_wrong_document_references_are_rejected() {
        let db = db();
        let (id, _) = add(&db, &document()).unwrap();
        let mut record = json!({"sha256":"fake","submitted_text":"different"});
        assert!(
            add_run(&db, id, &record)
                .unwrap_err()
                .to_string()
                .contains("hash mismatch")
        );
        record["sha256"] = json!(digest("different"));
        assert!(
            add_run(&db, id, &record)
                .unwrap_err()
                .to_string()
                .contains("does not match")
        );
    }

    #[test]
    fn old_python_taskless_records_do_not_duplicate() {
        let db = db();
        let (id, _) = add(&db, &document()).unwrap();
        let text = document()["text"].as_str().unwrap().to_string();
        let record = json!({"sha256":digest(&text),"submitted_text":text,"result":{}});
        db.execute("INSERT INTO detector_runs(detector,run_key,document_id,record_json,imported_at) VALUES ('pangram','legacy-python-json-hash',?,?,'old')",params![id,serde_json::to_string_pretty(&record).unwrap()]).unwrap();
        assert!(!add_run(&db, id, &record).unwrap());
        assert_eq!(status(&db).unwrap()["detector_runs"], 1);
    }

    #[test]
    fn invalid_counts_are_rejected_before_any_mutation() {
        let db = db();
        let doc = document();
        let mut extracted = extract(doc["text"].as_str().unwrap(), false, "python3").unwrap();
        extracted["totals"]["word"] = json!(2);
        assert!(add_document(&db, &doc, &extracted).is_err());
        assert_eq!(status(&db).unwrap()["corpora"], json!([]));
        extracted["families"] = json!({"construction":{"passive":2,"relative":2}});
        extracted["totals"] = json!({"construction":2});
        add_document(&db, &doc, &extracted).unwrap();
        assert_eq!(
            profile(
                &db,
                "human-sample",
                "construction",
                "train",
                None,
                None,
                None,
                &[]
            )
            .unwrap()["total"],
            2
        );
    }

    #[test]
    fn duplicate_text_cannot_change_provenance() {
        let db = db();
        add(&db, &document()).unwrap();
        let mut doc = document();
        doc["domain"] = json!("biology");
        assert!(
            add(&db, &doc)
                .unwrap_err()
                .to_string()
                .contains("conflicting provenance")
        );
    }

    #[test]
    fn schema_version_is_checked_and_existing_database_reopens() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/corpus.sqlite3");
        let db = connect(&path).unwrap();
        add(&db, &document()).unwrap();
        drop(db);
        let db = connect(&path).unwrap();
        assert_eq!(words_profile(&db).unwrap()["total"], 6);
        db.execute(
            "UPDATE settings SET value='99' WHERE key='schema_version'",
            [],
        )
        .unwrap();
        drop(db);
        assert!(
            connect(&path)
                .unwrap_err()
                .to_string()
                .contains("Unsupported database schema 99")
        );
    }
}

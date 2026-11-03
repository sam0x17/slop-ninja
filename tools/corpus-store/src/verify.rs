//! Integrity checks and status counts.

use crate::archive;
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

fn rows(db: &Connection, sql: &str) -> Result<Vec<Vec<Value>>> {
    let mut st = db.prepare(sql)?;
    let n = st.column_count();
    let out = st
        .query_map([], |r| {
            let mut v = Vec::with_capacity(n);
            for k in 0..n {
                v.push(match r.get_ref(k)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(i) => json!(i),
                    rusqlite::types::ValueRef::Real(f) => json!(f),
                    rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    rusqlite::types::ValueRef::Blob(b) => json!(hex::encode(b)),
                });
            }
            Ok(v)
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(out)
}

fn one(db: &Connection, sql: &str) -> Result<i64> {
    Ok(db.query_row(sql, [], |r| r.get(0))?)
}

fn grouped(db: &Connection, sql: &str) -> Result<Value> {
    let mut m = Map::new();
    for r in rows(db, sql)? {
        let key = r[..r.len() - 1].iter().map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).collect::<Vec<_>>().join(" | ");
        m.insert(key, r[r.len() - 1].clone());
    }
    Ok(Value::Object(m))
}

pub fn status(db: &Connection) -> Result<Value> {
    Ok(json!({
        "texts": one(db, "SELECT COUNT(*) FROM text")?,
        "text_bytes": one(db, "SELECT COALESCE(SUM(n_bytes),0) FROM text")?,
        "blobs": one(db, "SELECT COUNT(*) FROM blob")?,
        "blob_bytes": one(db, "SELECT COALESCE(SUM(len),0) FROM blob")?,
        "blob_compressed_bytes": one(db, "SELECT COALESCE(SUM(zlen),0) FROM blob")?,
        "files": one(db, "SELECT COUNT(*) FROM file")?,
        "records": one(db, "SELECT COUNT(*) FROM record")?,
        "events": one(db, "SELECT COUNT(*) FROM event")?,
        "observations": one(db, "SELECT COUNT(*) FROM observation")?,
        "views": one(db, "SELECT COUNT(*) FROM view")?,
        "split_members": one(db, "SELECT COUNT(*) FROM split_member")?,
        "problems": one(db, "SELECT COUNT(*) FROM problem")?,
        "sources": grouped(db, "SELECT source_id, status FROM source ORDER BY priority, source_id")?,
    }))
}

const EVAL_ROLES: [&str; 6] = ["dev_iter", "dev_select", "cal", "test", "test_sealed", "test_consumed"];
const TRAIN_ROLES: [&str; 2] = ["train", "train_mining"];

/// Expected digests for a view file: SHA256SUMS lines in its directory or up to
/// three parents, and bundle BUILD.json `rows_sha256`.
fn expected_digests(path: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut dir = path.parent();
    for _ in 0..4 {
        let Some(d) = dir else { break };
        if let Ok(rel) = path.strip_prefix(d) {
            let rel = rel.to_string_lossy().to_string();
            for name in ["SHA256SUMS", "SHA256SUMS.txt"] {
                if let Ok(t) = std::fs::read_to_string(d.join(name)) {
                    for line in t.lines() {
                        let mut it = line.splitn(2, char::is_whitespace);
                        let (Some(h), Some(p)) = (it.next(), it.next()) else { continue };
                        let p = p.trim_start().trim_start_matches('*').trim_start_matches("./");
                        let decoded = p.strip_suffix(".zst").unwrap_or(p);
                        if p == rel || decoded == rel {
                            out.push((format!("{}/{}", d.display(), name), h.to_string()));
                        }
                    }
                }
            }
            if let Ok(b) = std::fs::read(d.join("MANIFEST.json")) {
                if let Ok(v) = serde_json::from_slice::<Value>(&b) {
                    if let Some(h) = v.get("files").and_then(|f| f.get(&rel)).and_then(|x| x.as_str()) {
                        out.push((format!("{}/MANIFEST.json", d.display()), h.to_string()));
                    }
                }
            }
            if let Ok(b) = std::fs::read(d.join("BUILD.json")) {
                if let Ok(v) = serde_json::from_slice::<Value>(&b) {
                    let parts: Vec<&str> = rel.split('/').collect();
                    if parts.len() == 2 && parts[1] == "rows.jsonl" {
                        let sp = parts[0].replace('-', "_");
                        for key in [parts[0].to_string(), sp] {
                            if let Some(h) = v.pointer(&format!("/splits/{key}/rows_sha256")).and_then(|x| x.as_str()) {
                                out.push((format!("{}/BUILD.json", d.display()), h.to_string()));
                            }
                        }
                    }
                }
            }
        }
        dir = d.parent();
    }
    out.sort();
    out.dedup();
    out
}

pub fn run(db: &Connection, store: &Path, expectations: Option<&Path>, rebuild: bool) -> Result<Value> {
    let mut report = Map::new();
    let mut failures: Vec<String> = Vec::new();
    let exp: Value = match expectations {
        Some(p) => serde_json::from_slice(&std::fs::read(p)?)?,
        None => json!({}),
    };

    // 1. Frozen views.
    let mut views = Vec::new();
    let mut view_ok = 0;
    for r in rows(db, "SELECT view_id, kind, path, file_sha256, n_rows FROM view ORDER BY view_id")? {
        let (vid, path, sha, n) = (r[0].as_str().unwrap().to_string(), r[2].as_str().unwrap().to_string(), r[3].as_str().unwrap().to_string(), r[4].as_i64().unwrap());
        let file_lines: Option<i64> = db.query_row("SELECT n_lines FROM file WHERE file_sha256 = ?1 AND complete = 1", [&sha], |r| r.get(0)).ok();
        let members: i64 = db.query_row("SELECT COUNT(*) FROM view_member WHERE view_id = ?1", [&vid], |r| r.get(0))?;
        let missing_texts: i64 = db.query_row(
            "SELECT COUNT(*) FROM view_member m WHERE m.view_id = ?1 AND m.text_sha256 IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM text t WHERE t.text_sha256 = m.text_sha256)
               AND NOT EXISTS (SELECT 1 FROM record r WHERE r.text_sha256 = m.text_sha256 AND r.role LIKE '%metadata%')",
            [&vid], |r| r.get(0))?;
        let mut declared = expected_digests(Path::new(&path));
        if let Some(e) = exp.get("view_digests").and_then(|m| m.get(&path)) {
            if let (Some(h), Some(b)) = (e.get("sha256").and_then(|x| x.as_str()), e.get("basis").and_then(|x| x.as_str())) {
                declared.push((b.to_string(), h.to_string()));
            }
        }
        let declared_ok = declared.iter().all(|(_, h)| h == &sha);
        let mut rebuilt = Value::Null;
        if rebuild {
            let mut sinkw = std::io::sink();
            rebuilt = match archive::rebuild_file(db, &store.join("packs"), &sha, &mut sinkw) {
                Ok(h) => json!(h == sha),
                Err(e) => json!(format!("error: {e}")),
            };
        }
        let ok = file_lines == Some(n) && members == n && missing_texts == 0 && declared_ok && (!rebuild || rebuilt == json!(true));
        if ok {
            view_ok += 1;
        } else {
            failures.push(format!("view {vid}"));
        }
        views.push(json!({"view": vid, "kind": r[1], "path": path, "file_sha256": sha, "rows": n, "file_lines": file_lines,
            "members": members, "member_texts_missing": missing_texts,
            "declared": declared.iter().map(|(b, h)| json!({"basis": b, "sha256": h, "match": h == &sha})).collect::<Vec<_>>(),
            "rebuilt_byte_identical": rebuilt, "ok": ok}));
    }
    report.insert("views".into(), json!({"total": views.len(), "ok": view_ok, "detail": views}));

    // 1b. Declared input files (for example every input of a bundle build): the
    // exact bytes are in the store under that path.
    let mut file_checks = Vec::new();
    let mut file_ok = 0;
    let mut superseded = 0;
    for e in exp.get("file_digests").and_then(|x| x.as_array()).into_iter().flatten() {
        let (Some(p), Some(h)) = (e.get("path").and_then(|x| x.as_str()), e.get("sha256").and_then(|x| x.as_str())) else { continue };
        let present: i64 = db.query_row(
            "SELECT COUNT(*) FROM file_path fp JOIN file f ON f.file_sha256 = fp.file_sha256 WHERE fp.path = ?1 AND fp.file_sha256 = ?2 AND f.complete = 1",
            params![p, h], |r| r.get(0))?;
        let any_path: i64 = db.query_row("SELECT COUNT(*) FROM file f WHERE f.file_sha256 = ?1 AND f.complete = 1", [h], |r| r.get(0))?;
        let other: i64 = db.query_row("SELECT COUNT(*) FROM file_path WHERE path = ?1 AND file_sha256 != ?2", params![p, h], |r| r.get(0))?;
        let ok = present > 0 || any_path > 0;
        let status = if ok { "stored" } else if other > 0 { "superseded_on_disk" } else { "missing" };
        match status {
            "stored" => file_ok += 1,
            "superseded_on_disk" => superseded += 1,
            _ => failures.push(format!("declared file missing: {p}")),
        }
        file_checks.push(json!({"path": p, "sha256": h, "basis": e.get("basis"), "stored_under_path": present > 0, "bytes_stored": any_path > 0, "status": status}));
    }
    report.insert("declared_files".into(), json!({"total": file_checks.len(), "stored": file_ok,
        "superseded_on_disk": superseded, "note": "superseded_on_disk: the path now holds newer bytes (for example an atomically republished export); the build that used the older bytes still reproduces from its own rows",
        "detail": file_checks}));

    // 2. Observation counts against expectations.
    let by_detector = grouped(db, "SELECT detector, status, COUNT(*) FROM observation GROUP BY 1, 2 ORDER BY 1, 2")?;
    let obs_texts = grouped(db, "SELECT detector, COUNT(DISTINCT text_sha256) FROM observation GROUP BY 1")?;
    let obs_unlinked = grouped(db, "SELECT detector, COUNT(*) FROM observation WHERE text_sha256 IS NULL OR text_sha256 NOT IN (SELECT text_sha256 FROM text) GROUP BY 1")?;
    let mut obs_checks = Vec::new();
    if let Some(Value::Object(m)) = exp.get("observations") {
        for (k, want) in m {
            // key: "<detector>" or "<detector>@<source_id>"
            let got: i64 = match k.split_once('@') {
                Some((d, s)) => db.query_row("SELECT COUNT(*) FROM observation WHERE detector = ?1 AND source_id = ?2", params![d, s], |r| r.get(0))?,
                None => db.query_row("SELECT COUNT(*) FROM observation WHERE detector = ?1", [k], |r| r.get(0))?,
            };
            let ok = want.as_i64() == Some(got);
            if !ok {
                failures.push(format!("observations {k}: {got} != {want}"));
            }
            obs_checks.push(json!({"key": k, "expected": want, "got": got, "ok": ok}));
        }
    }
    // Key-level comparison with the Pangram gold index (one line per text).
    let mut index_check = Value::Null;
    if let Some(p) = exp.get("pangram_index").and_then(|x| x.as_str()) {
        let mut want: BTreeMap<String, Option<String>> = BTreeMap::new();
        for line in std::fs::read_to_string(p)?.lines() {
            let v: Value = serde_json::from_str(line)?;
            let t = v.get("text_sha256").and_then(|x| x.as_str()).map(String::from);
            for o in v.get("observations").and_then(|x| x.as_array()).into_iter().flatten() {
                if let Some(k) = o.get("key").and_then(|x| x.as_str()) {
                    want.insert(k.to_string(), t.clone());
                }
            }
        }
        let mut got: BTreeMap<String, Option<String>> = BTreeMap::new();
        for r in rows(db, "SELECT obs_key, text_sha256 FROM observation WHERE detector = 'pangram'")? {
            got.insert(r[0].as_str().unwrap().to_string(), r[1].as_str().map(String::from));
        }
        let missing: Vec<&String> = want.keys().filter(|k| !got.contains_key(*k)).collect();
        let extra: Vec<&String> = got.keys().filter(|k| !want.contains_key(*k)).collect();
        let text_diff: Vec<&String> = want.iter().filter(|(k, t)| got.get(*k).is_some_and(|g| g != *t)).map(|x| x.0).collect();
        let ok = missing.is_empty() && text_diff.is_empty();
        if !ok {
            failures.push(format!("pangram index: {} missing, {} text mismatches", missing.len(), text_diff.len()));
        }
        index_check = json!({"index": p, "index_observations": want.len(), "store_observations": got.len(),
            "missing_from_store": missing.len(), "missing_examples": missing.iter().take(20).collect::<Vec<_>>(),
            "extra_in_store": extra.len(), "extra_examples": extra.iter().take(20).collect::<Vec<_>>(),
            "text_mismatches": text_diff.len(), "text_mismatch_examples": text_diff.iter().take(20).collect::<Vec<_>>(), "ok": ok});
    }
    report.insert("observations".into(), json!({"by_detector_status": by_detector, "distinct_texts": obs_texts,
        "without_stored_text": obs_unlinked, "checks": obs_checks, "pangram_index": index_check,
        "by_context": grouped(db, "SELECT detector, context, COUNT(*) FROM observation GROUP BY 1, 2 ORDER BY 1, 2")?}));

    // 3. Cross-split leaks by source group and by text, per experiment.
    let mut leaks = Map::new();
    let mut total_group_leaks = 0;
    let mut total_text_leaks = 0;
    let exps: Vec<String> = rows(db, "SELECT DISTINCT experiment FROM split_member ORDER BY 1")?.into_iter().map(|r| r[0].as_str().unwrap().to_string()).collect();
    for e in exps {
        let mut by_group: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut by_text: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut st = db.prepare("SELECT source_group, text_sha256, role FROM split_member WHERE experiment = ?1")?;
        let mut q = st.query([&e])?;
        while let Some(r) = q.next()? {
            let g: Option<String> = r.get(0)?;
            let t: Option<String> = r.get(1)?;
            let role: String = r.get(2)?;
            if let Some(g) = g {
                by_group.entry(g).or_default().insert(role.clone());
            }
            if let Some(t) = t {
                by_text.entry(t).or_default().insert(role);
            }
        }
        let crosses = |roles: &BTreeSet<String>| -> bool {
            let tr = roles.iter().any(|r| TRAIN_ROLES.contains(&r.as_str()));
            let ev: BTreeSet<&str> = roles.iter().filter(|r| EVAL_ROLES.contains(&r.as_str())).map(String::as_str).collect();
            (tr && !ev.is_empty()) || ev.len() > 1
        };
        let g_leaks: Vec<(&String, &BTreeSet<String>)> = by_group.iter().filter(|(_, r)| crosses(r)).collect();
        let t_leaks: Vec<(&String, &BTreeSet<String>)> = by_text.iter().filter(|(_, r)| crosses(r)).collect();
        total_group_leaks += g_leaks.len();
        total_text_leaks += t_leaks.len();
        leaks.insert(e.clone(), json!({
            "groups": by_group.len(), "texts": by_text.len(),
            "group_leaks": g_leaks.len(), "text_leaks": t_leaks.len(),
            "group_leak_examples": g_leaks.iter().take(10).map(|(g, r)| json!([g, r])).collect::<Vec<_>>(),
            "text_leak_examples": t_leaks.iter().take(10).map(|(t, r)| json!([t, r])).collect::<Vec<_>>(),
        }));
    }
    if total_group_leaks > 0 || total_text_leaks > 0 {
        failures.push(format!("split leaks: {total_group_leaks} groups, {total_text_leaks} texts"));
    }
    report.insert("split_leaks".into(), Value::Object(leaks));

    // 4. Non-commercial rows.
    // Licence strings only: free-text rights bases often state "no non-commercial
    // restriction" and are not matched.
    let nc_sql = "FROM record WHERE (
        lower(licence) LIKE '%by-nc%' OR lower(licence) LIKE '%-nc-%' OR lower(licence) LIKE '%-nc' OR lower(licence) LIKE '%cc-nc%'
        OR lower(licence) LIKE '%noncommercial%' OR lower(licence) LIKE '%non-commercial%' OR lower(licence) LIKE '%non commercial%'
        OR lower(licence) LIKE '%research only%' OR lower(licence) LIKE '%research-only%' OR lower(licence) LIKE '%research use only%'
        OR lower(licence) LIKE '%academic-only%' OR lower(licence) LIKE '%academic use%' OR lower(licence) LIKE '%educational use only%')";
    let commercial_false = grouped(db, "SELECT source_id, COUNT(*) FROM record WHERE json_extract(extra_json, '$.rights.commercial_training') = 0 GROUP BY 1")?;
    let licences = grouped(db, "SELECT COALESCE(licence, 'none'), COUNT(*) FROM record GROUP BY 1 ORDER BY 2 DESC")?;
    let nc_total = one(db, &format!("SELECT COUNT(*) {nc_sql}"))?;
    let nc_by = grouped(db, &format!("SELECT source_id, licence, COUNT(*) {nc_sql} GROUP BY 1, 2"))?;
    let excluded_collections = grouped(db,
        "SELECT source_id, collection, COUNT(*) FROM record WHERE lower(collection) LIKE '%blog_authorship%' OR lower(collection) LIKE '%blogauthorship%'
           OR lower(collection) LIKE '%pelic%' OR lower(collection) LIKE '%coraal%' OR lower(collection) LIKE '%mediasum%'
           OR lower(collection) LIKE '%goodreads%' OR lower(collection) LIKE '%yelp%' OR lower(collection) LIKE '%hacker_news%'
           OR lower(collection) LIKE '%hackernews%' GROUP BY 1, 2")?;
    let rights_nc = grouped(db, "SELECT cohort_id, commercial_use FROM rights WHERE commercial_use != 'permitted'")?;
    if nc_total > 0 || excluded_collections.as_object().is_some_and(|m| !m.is_empty()) {
        failures.push(format!("non-commercial rows: {nc_total}"));
    }
    // USPTO rule: publication (and so filing) before 2019-01-01; missing dates excluded.
    let uspto_bad = one(db, "SELECT COUNT(*) FROM record WHERE (lower(collection) LIKE '%uspto%' OR lower(cohort_id) LIKE '%uspto%')
        AND (doc_date IS NULL OR substr(doc_date, 1, 10) >= '2019-01-01')")?;
    let uspto_rows = one(db, "SELECT COUNT(*) FROM record WHERE lower(collection) LIKE '%uspto%' OR lower(cohort_id) LIKE '%uspto%'")?;
    if uspto_bad > 0 {
        failures.push(format!("{uspto_bad} USPTO rows violate the date rule"));
    }
    let human_after_cutoff = grouped(db, "SELECT source_id, COUNT(*) FROM record WHERE origin = 'human_only' AND doc_date IS NOT NULL
        AND substr(doc_date, 1, 10) >= '2022-11-30' AND substr(doc_date, 1, 1) BETWEEN '0' AND '9' GROUP BY 1")?;
    report.insert("date_rules".into(), json!({"uspto_rows": uspto_rows, "uspto_rows_violating_2019_rule": uspto_bad,
        "human_rows_dated_on_or_after_2022_11_30_by_source": human_after_cutoff}));
    report.insert("noncommercial".into(), json!({"records": nc_total, "by_source_licence": nc_by,
        "commercial_training_false_by_source": commercial_false,
        "commercial_training_false_note": "capture-time flags kept unchanged in archived exports; admission is decided by the hosted admission manifest (by record hash) and reported, not relabelled",
        "licence_strings": licences,
        "excluded_collections": excluded_collections, "rights_not_permitted": rights_nc}));

    // 5. Store integrity.
    let text_without_blob = one(db, "SELECT COUNT(*) FROM text t WHERE NOT EXISTS (SELECT 1 FROM blob b WHERE b.sha256 = unhex(t.text_sha256))")?;
    let records_without_text = grouped(db, "SELECT source_id, role, COUNT(*) FROM record WHERE text_sha256 IS NOT NULL AND text_sha256 NOT IN (SELECT text_sha256 FROM text) GROUP BY 1, 2")?;
    let incomplete_files = one(db, "SELECT COUNT(*) FROM file WHERE complete = 0")?;
    let incomplete_sources = grouped(db, "SELECT source_id, status FROM source WHERE status != 'complete'")?;
    if text_without_blob > 0 || incomplete_files > 0 {
        failures.push("store integrity".into());
    }
    let label_conflicts = one(db,
        "SELECT COUNT(*) FROM (SELECT text_sha256 FROM record WHERE origin IS NOT NULL AND text_sha256 IS NOT NULL GROUP BY text_sha256 HAVING COUNT(DISTINCT origin) > 1)")?;
    report.insert("integrity".into(), json!({
        "texts_without_blob": text_without_blob, "records_naming_unstored_text": records_without_text,
        "incomplete_files": incomplete_files, "incomplete_sources": incomplete_sources,
        "texts_with_conflicting_origin_labels": label_conflicts,
        "pangram_labels_used": "none: observations are a separate table and never written to record labels",
    }));

    // 6. Canonical split (most restrictive exposure) and the evaluation-only gate.
    let has_split = one(db, "SELECT COUNT(*) FROM sqlite_master WHERE name = 'text_split'")? > 0;
    if has_split {
        let gate = grouped(db,
            "SELECT m.experiment, COUNT(DISTINCT m.text_sha256) FROM split_member m JOIN text_split s ON s.text_sha256 = m.text_sha256
             WHERE m.role IN ('train', 'train_mining') AND (s.evaluation_only = 1 OR s.group_evaluation_only = 1)
             GROUP BY 1 ORDER BY 1")?;
        let gate_raw: i64 = one(db,
            "SELECT COUNT(DISTINCT m.text_sha256) FROM split_member m JOIN text_split s ON s.text_sha256 = m.text_sha256
             WHERE m.role IN ('train', 'train_mining') AND s.evaluation_only = 1
               AND (m.experiment LIKE 'a-comp-bundle-%' OR m.experiment LIKE 'a-comp-trainer-%')")?;
        // Roles released by a recorded decision before any opening (expectations
        // `approved_reassignments`: [{role, basis}]) do not make a text evaluation-only.
        let approved: Vec<String> = exp.get("approved_reassignments").and_then(|x| x.as_array()).into_iter().flatten()
            .filter_map(|a| a.get("role").and_then(|x| x.as_str()).map(|r| format!("'{}'", r.replace('\'', "''")))).collect();
        let approved_sql = if approved.is_empty() { "''".to_string() } else { approved.join(",") };
        // Set-based: one scan for the remaining evaluation texts, one for the Train texts.
        db.execute_batch(&format!(
            "DROP TABLE IF EXISTS temp.gate_eval; DROP TABLE IF EXISTS temp.gate_train;
             CREATE TEMP TABLE gate_eval (t TEXT PRIMARY KEY) WITHOUT ROWID;
             INSERT OR IGNORE INTO gate_eval SELECT text_sha256 FROM text_split_role WHERE rank >= 3 AND role NOT IN ({approved_sql});
             CREATE TEMP TABLE gate_train (t TEXT PRIMARY KEY) WITHOUT ROWID;
             INSERT OR IGNORE INTO gate_train SELECT text_sha256 FROM split_member
               WHERE text_sha256 IS NOT NULL AND role IN ('train', 'train_mining')
                 AND (experiment LIKE 'a-comp-bundle-%' OR experiment LIKE 'a-comp-trainer-%');"))?;
        let gate_comp: i64 = one(db, "SELECT COUNT(*) FROM gate_train g JOIN gate_eval e ON e.t = g.t")?;
        let gate_group: i64 = one(db,
            "SELECT COUNT(DISTINCT m.text_sha256) FROM split_member m JOIN text_split s ON s.text_sha256 = m.text_sha256
             WHERE m.role IN ('train', 'train_mining') AND s.group_evaluation_only = 1 AND s.evaluation_only = 0
               AND (m.experiment LIKE 'a-comp-bundle-%' OR m.experiment LIKE 'a-comp-trainer-%')")?;
        let pangram_train = one(db,
            "SELECT COUNT(DISTINCT m.text_sha256) FROM split_member m JOIN observation o ON o.text_sha256 = m.text_sha256
             WHERE o.detector = 'pangram' AND m.role IN ('train', 'train_mining')
               AND (m.experiment LIKE 'a-comp-bundle-%' OR m.experiment LIKE 'a-comp-trainer-%')")?;
        if gate_comp > 0 {
            failures.push(format!("{gate_comp} evaluation-only texts in A-competitive Train views"));
        }
        report.insert("canonical_split".into(), json!({
            "rule": "most restrictive exposure: test_consumed > test_sealed > test > cal > dev/diagnostic/reserved > held > train",
            "texts_by_canonical_split": grouped(db, "SELECT COALESCE(canonical_split, 'unassigned'), COUNT(*) FROM text_split GROUP BY 1 ORDER BY 1")?,
            "evaluation_only_texts": one(db, "SELECT COUNT(*) FROM text_split WHERE evaluation_only = 1")?,
            "group_evaluation_only_texts": one(db, "SELECT COUNT(*) FROM text_split WHERE group_evaluation_only = 1 AND evaluation_only = 0")?,
            "pangram_texts_by_canonical_split": grouped(db, "SELECT COALESCE(s.canonical_split, 'unassigned'), COUNT(DISTINCT o.text_sha256) FROM observation o JOIN text_split s ON s.text_sha256 = o.text_sha256 WHERE o.detector = 'pangram' GROUP BY 1 ORDER BY 1")?,
            "train_role_texts_that_are_evaluation_only_by_experiment": gate,
            "a_comp_train_views_evaluation_only_texts_raw": gate_raw,
            "approved_reassignments": exp.get("approved_reassignments"),
            "a_comp_train_views_evaluation_only_texts": gate_comp,
            "a_comp_train_views_texts_only_group_evaluation_only": gate_group,
            "a_comp_train_views_pangram_scored_texts": pangram_train,
        }));
    }
    report.insert("counts".into(), json!({
        "status": status(db)?,
        "records_by_source_role": grouped(db, "SELECT source_id, role, COUNT(*) FROM record GROUP BY 1, 2 ORDER BY 1, 2")?,
        "texts_by_first_source": grouped(db, "SELECT first_source, COUNT(*) FROM text GROUP BY 1 ORDER BY 1")?,
        "events_by_kind_status": grouped(db, "SELECT kind, status, COUNT(*) FROM event GROUP BY 1, 2 ORDER BY 1, 2")?,
        "problems_by_source_kind": grouped(db, "SELECT source_id, kind, COUNT(*) FROM problem GROUP BY 1, 2 ORDER BY 1, 2")?,
        "split_members_by_experiment_role": grouped(db, "SELECT experiment, role, COUNT(*) FROM split_member GROUP BY 1, 2 ORDER BY 1, 2")?,
    }));
    report.insert("summary".into(), json!({"ok": failures.is_empty(), "failures": failures}));
    Ok(Value::Object(report))
}

//! Source ingestion driver: input manifests, idempotence and resumption.

use crate::adapters;
use crate::archive::{self, Format};
use crate::packs::Packs;
use crate::plan::{self, Plan, SourceSpec};
use crate::sink::Sink;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub fn now_utc() -> String {
    let out = std::process::Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]).output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

pub struct Input {
    pub rel: String,
    pub abs: PathBuf,
    pub role: String,
    pub format: Format,
    pub sha: String,
    pub n_bytes: u64,
}

fn check_forbidden(plan: &Plan, p: &Path) -> Result<()> {
    let s = p.to_string_lossy();
    if plan.allowed_prefixes.iter().any(|a| {
        let a = a.trim_end_matches('/');
        s == a || s.starts_with(&format!("{a}/"))
    }) {
        return Ok(());
    }
    for f in &plan.forbidden_prefixes {
        let f = f.trim_end_matches('/');
        ensure!(!(s == f || s.starts_with(&format!("{f}/"))), "refusing forbidden path {s} (prefix {f})");
    }
    Ok(())
}

/// Resolve and hash every input of a source.
pub fn manifest(plan: &Plan, spec: &SourceSpec) -> Result<(Vec<Input>, String)> {
    manifest_opt(plan, spec, true)
}

pub fn manifest_opt(plan: &Plan, spec: &SourceSpec, hash: bool) -> Result<(Vec<Input>, String)> {
    let root = PathBuf::from(&spec.root);
    check_forbidden(plan, &root)?;
    let mut out: Vec<Input> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for inp in &spec.inputs {
        for (rel, abs) in plan::resolve(&root, inp)? {
            check_forbidden(plan, &abs)?;
            let canon = std::fs::canonicalize(&abs)?;
            check_forbidden(plan, &canon)?;
            if !seen.insert(rel.clone()) {
                continue;
            }
            let format = plan::format_of(inp, &rel)?;
            let (sha, n_bytes) = if hash { archive::hash_file(&abs)? } else { (String::new(), std::fs::metadata(&abs)?.len()) };
            out.push(Input { rel, abs, role: inp.role.clone(), format, sha, n_bytes });
        }
    }
    let m: Vec<Value> = out.iter().map(|i| json!([i.rel, i.role, i.sha, i.n_bytes])).collect();
    let msha = hex::encode(crate::skeleton::sha256(serde_json::to_string(&m)?.as_bytes()));
    Ok((out, msha))
}

pub fn ingest_source(db: &Connection, store_dir: &Path, plan: &Plan, spec: &SourceSpec, force: bool) -> Result<Value> {
    let adapter_version = adapters::version(&spec.adapter)?;
    let prev: Option<(String, Option<String>, i64)> = db
        .query_row("SELECT status, input_manifest_sha256, adapter_version FROM source WHERE source_id = ?1", [&spec.id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?;
    eprintln!("[{}] {} resolving inputs", now_utc(), spec.id);
    let (inputs, msha) = manifest(plan, spec)?;
    if let Some((st, m, v)) = &prev {
        if st == "complete" && m.as_deref() == Some(msha.as_str()) && *v == adapter_version && !force {
            // Inputs and adapter are unchanged; keep the current spec (notes and
            // link-time parameters such as split defaults) without re-ingesting.
            db.execute("UPDATE source SET spec_json = ?2, priority = ?3 WHERE source_id = ?1",
                params![spec.id, serde_json::to_string(spec)?, spec.priority])?;
            eprintln!("[{}] {} unchanged and complete; skipped", now_utc(), spec.id);
            return Ok(json!({"source": spec.id, "skipped": true}));
        }
    }
    db.execute(
        "INSERT INTO source(source_id, adapter, adapter_version, priority, root, spec_json, input_manifest_sha256, status, started_utc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running', ?8)
         ON CONFLICT(source_id) DO UPDATE SET adapter = ?2, adapter_version = ?3, priority = ?4, root = ?5, spec_json = ?6,
           input_manifest_sha256 = ?7, status = 'running', started_utc = ?8, finished_utc = NULL",
        params![spec.id, spec.adapter, adapter_version, spec.priority, spec.root, serde_json::to_string(spec)?, msha, now_utc()],
    )?;
    for i in &inputs {
        db.execute(
            "INSERT INTO source_input(source_id, rel_path, file_sha256, n_bytes, role, done) VALUES (?1, ?2, ?3, ?4, ?5, 0)
             ON CONFLICT(source_id, rel_path) DO UPDATE SET done = CASE WHEN file_sha256 = ?3 AND role = ?5 THEN done ELSE 0 END,
               file_sha256 = ?3, n_bytes = ?4, role = ?5",
            params![spec.id, i.rel, i.sha, i.n_bytes as i64, i.role],
        )?;
    }
    let mut packs = Packs::open(db, &store_dir.join("packs"))?;
    let mut sink = Sink::new(db, &mut packs, &spec.id, &spec.adapter);
    let mut ad = adapters::make(&spec.adapter, spec)?;
    let total = inputs.len();
    sink.begin()?;
    let r = (|| -> Result<()> {
        for (k, i) in inputs.iter().enumerate() {
            let done: i64 = db.query_row(
                "SELECT done FROM source_input WHERE source_id = ?1 AND rel_path = ?2",
                params![spec.id, i.rel],
                |r| r.get(0),
            )?;
            if done == 1 && !ad.needs_all_files() {
                continue;
            }
            if k % 2000 == 0 || i.n_bytes > (256 << 20) {
                eprintln!("[{}] {} file {}/{} {} ({} bytes)", now_utc(), spec.id, k + 1, total, i.rel, i.n_bytes);
            }
            let mut cb = |s: &mut Sink, l: &archive::Line| ad.line(s, i, l);
            archive::ingest_file(&mut sink, store_dir, &i.abs, &format!("{}/{}", spec.root, i.rel), &i.sha, i.n_bytes, i.format, &mut cb)
                .with_context(|| format!("source {} input {}", spec.id, i.rel))?;
            ad.end_file(&mut sink, i)?;
            db.execute("UPDATE source_input SET done = 1 WHERE source_id = ?1 AND rel_path = ?2", params![spec.id, i.rel])?;
            sink.checkpoint(false, false)?;
        }
        ad.finish(&mut sink)?;
        sink.checkpoint(true, true)
    })();
    if let Err(e) = r {
        sink.rollback();
        return Err(e);
    }
    let counts = source_counts(db, &spec.id)?;
    db.execute(
        "UPDATE source SET status = 'complete', finished_utc = ?2, counts_json = ?3 WHERE source_id = ?1",
        params![spec.id, now_utc(), counts.to_string()],
    )?;
    eprintln!("[{}] {} complete: {}", now_utc(), spec.id, counts);
    Ok(counts)
}

pub fn source_counts(db: &Connection, id: &str) -> Result<Value> {
    let q = |sql: &str| -> Result<i64> { Ok(db.query_row(sql, [id], |r| r.get(0))?) };
    Ok(json!({
        "source": id,
        "inputs": q("SELECT COUNT(*) FROM source_input WHERE source_id = ?1")?,
        "input_bytes": q("SELECT COALESCE(SUM(n_bytes),0) FROM source_input WHERE source_id = ?1")?,
        "records": q("SELECT COUNT(*) FROM record WHERE source_id = ?1")?,

        "texts_first_seen": q("SELECT COUNT(*) FROM text WHERE first_source = ?1")?,
        "events": q("SELECT COUNT(*) FROM event WHERE source_id = ?1")?,
        "observations": q("SELECT COUNT(*) FROM observation WHERE source_id = ?1")?,
        "views": q("SELECT COUNT(*) FROM view WHERE source_id = ?1")?,
        "problems": q("SELECT COUNT(*) FROM problem WHERE source_id = ?1")?,
    }))
}

pub fn run(db: &Connection, store_dir: &Path, plan: &Plan, only: &[String], force: bool) -> Result<Vec<Value>> {
    let mut specs: Vec<&SourceSpec> = plan.sources.iter().filter(|s| only.is_empty() || only.contains(&s.id)).collect();
    if !only.is_empty() && specs.len() != only.len() {
        bail!("unknown source id among {only:?}");
    }
    specs.sort_by_key(|s| s.priority);
    let mut out = Vec::new();
    for s in specs {
        out.push(ingest_source(db, store_dir, plan, s, force)?);
    }
    Ok(out)
}

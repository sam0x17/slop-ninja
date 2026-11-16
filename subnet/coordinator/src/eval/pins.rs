//! Reference pins and the reference presence check
//! (`slop_ninja_reference_pins_v1`).
//!
//! **Pins.** Every reference the chain names (`reference_state`: `R_k` and
//! `R_k-1`) and a promotion pending in a cohort's transition is pinned once
//! into the immutable reference storage the operator's staged packages use
//! ([`crate::eval::reference`]): each file at `reference/v2/files/<sha256>`
//! in the admission bucket, and a pinned files manifest at
//! `artifacts/packages/<package>.json` in the eval bucket. A promoted
//! entrant's files sit under its admission session's keys; the pin copies
//! each one there (R2 verifies the copy against the file's SHA-256 and
//! reads the source pinned to its recorded entity tag), heads the copies,
//! pins their entity tags and writes the manifest (R2 checking its
//! SHA-256). A package whose files are all at their reference locations
//! already (a staged package) is its own pin and copies nothing. The pin
//! keeps the reference's content (the inference content names each file by
//! role, digest and length, never by location) and its artifact digest;
//! only the package digest and manifest key are the pinned manifest's.
//!
//! The chain's resolution reads the pinned record first
//! (`calendar::resolve_record`), so the next cohort and its lanes load
//! `R_k` and `R_k-1` from the reference prefix, which nothing deletes: the
//! admission clean-up, the object-create observer and the standing
//! reconciliation act only under `admission/v2/`. Pinning is idempotent and
//! needs no operator step: a reference the chain names that has no row gets
//! one at the next step (the backfill of the references a deploy finds), and
//! a copy already in place is headed and kept.
//!
//! **Presence.** Before the calendar queues a cohort, and whenever the
//! packages the references resolve to change (a promotion, a reference
//! decision, a pin), every file of the next cohort's references and of the
//! references each open cohort names is headed, against the entity tag its
//! files manifest pins. The result is on `/eval/status` (`references`).
//!
//! **Gate.** The calendar queues the next cohort only when both of its
//! references are pinned and the presence check for that cohort found every
//! file of them ([`gate`]); otherwise it waits, with the reason on the
//! status, which the publisher alerts on (`reference_pin_failed`,
//! `reference_objects_missing`, `reference_presence_unchecked`). A failed
//! copy for a platform reason is
//! retried once a tick; one that cannot succeed by waiting (a source object
//! absent or changed, a file at its reference key that is not the named
//! one) stops until the operator's journaled recheck
//! (`POST /eval/references/recheck`), which drops every failed pin row so
//! it re-creates one only for a content the chain or a pending
//! promotion still names, from the record it names now, and re-runs the
//! presence check. The operator's `cohort/start` (D8) still queues a cohort
//! past the gate, so the rule never locks the operator out.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)
)]

use serde::Serialize;
use serde_json::{Value, json};
use slop_ninja_artifact_check::files_manifest::{
    FileEntry, FilesManifest, MAX_ETAG_BYTES, MAX_MANIFEST_BYTES, REFERENCE_BUCKET, reference_file_key, reference_label_ok,
};

use crate::eval::admission::{Head, manifest_key};
use crate::eval::engine::{self, EvalConfig, EvalError, R, i, opt_int, opt_text, s};
use crate::eval::plan::ArtifactJson;
use crate::operator::{self, Begin, Request};
use crate::sql::{Sql, SqlValue, bind_u64, int, text};

/// The rule's name, on the status.
pub const PIN_RULE: &str = "slop_ninja_reference_pins_v1";

/// v31: the reference pins and the latest presence check.
pub const SCHEMA_V31: &[&str] = &[
    // One row per reference content the chain or a pending promotion named:
    // the record the pin copies from, the pinned record once written, and
    // the state of the copy.
    "CREATE TABLE reference_pins (
        content TEXT PRIMARY KEY,
        source_record TEXT NOT NULL,
        record TEXT,
        state TEXT NOT NULL CHECK (state IN ('pending', 'pinned', 'failed')),
        attempts INTEGER NOT NULL DEFAULT 0,
        error TEXT,
        retry_ms INTEGER NOT NULL DEFAULT 0,
        copied INTEGER NOT NULL DEFAULT 0,
        at_ms INTEGER NOT NULL,
        pinned_ms INTEGER
    ) STRICT",
    // The latest presence check: the packages it headed (sorted, JSON), the
    // cohort number the calendar would queue next when it ran while a
    // cohort was due (else NULL), its outcome and when it runs again.
    "CREATE TABLE reference_presence (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        packages TEXT NOT NULL,
        for_cohort INTEGER,
        ok INTEGER NOT NULL,
        missing TEXT NOT NULL,
        error TEXT,
        checked_ms INTEGER NOT NULL,
        retry_ms INTEGER NOT NULL
    ) STRICT",
];

/// The calendar waits: a reference of the next cohort is not pinned yet.
pub const GATE_PIN_PENDING: &str = "reference_pin_pending";
/// The calendar waits: a reference's pin failed in a way waiting does not
/// fix (the operator's recheck retries it).
pub const GATE_PIN_FAILED: &str = "reference_pin_failed";
/// The calendar waits: no presence check covers the next cohort yet.
pub const GATE_UNCHECKED: &str = "reference_presence_unchecked";
/// The calendar waits: a file of the next cohort's references is absent or
/// no longer has its pinned entity tag.
pub const GATE_MISSING: &str = "reference_objects_missing";

// ------------------------------------------------------------------ pins

/// A pin to run: the content and the record it copies from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinJob {
    pub content: String,
    pub source: ArtifactJson,
}

fn has_row(sql: &impl Sql, content: &str) -> R<bool> {
    Ok(!sql.exec("SELECT 1 FROM reference_pins WHERE content = ?", &[s(content)])?.is_empty())
}

/// The contents a pin is kept for now: the chain's current and previous
/// references and a promotion pending in a cohort's transition. A read.
pub fn named(sql: &impl Sql) -> R<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    if let Some(r) = sql.exec("SELECT current, predecessor FROM reference_state WHERE id = 1", &[])?.first() {
        out.push(text(r, 0)?.to_owned());
        out.extend(opt_text(r, 1));
    }
    if let Some((c, _)) = crate::eval::calendar::pending_promoted(sql)?
        && !out.contains(&c)
    {
        out.push(c);
    }
    Ok(out)
}

/// The references to pin that have no row yet: the chain's current and
/// previous references (resolved as the stored records name them) and a
/// promotion pending in a cohort's transition. A read.
pub fn unrecorded(sql: &impl Sql) -> R<Vec<PinJob>> {
    let mut out: Vec<PinJob> = Vec::new();
    let chain = sql.exec("SELECT current, predecessor FROM reference_state WHERE id = 1", &[])?;
    let mut contents: Vec<String> = Vec::new();
    if let Some(r) = chain.first() {
        contents.push(text(r, 0)?.to_owned());
        contents.extend(opt_text(r, 1));
    }
    for c in contents {
        if !has_row(sql, &c)?
            && let Some(source) = crate::eval::calendar::resolve_source(sql, &c)?
            && !out.iter().any(|j| j.content == c)
        {
            out.push(PinJob { content: c, source });
        }
    }
    if let Some((c, source)) = crate::eval::calendar::pending_promoted(sql)?
        && !has_row(sql, &c)?
        && !out.iter().any(|j| j.content == c)
    {
        out.push(PinJob { content: c, source });
    }
    Ok(out)
}

/// Records a pending pin for every reference that has none
/// ([`unrecorded`]): idempotent, so it is also the backfill of the
/// references a deploy finds.
pub fn ensure(sql: &impl Sql, now_ms: u64) -> R<()> {
    for j in unrecorded(sql)? {
        let record = serde_json::to_string(&j.source).map_err(|_| EvalError::State("pin source record"))?;
        sql.exec(
            "INSERT OR IGNORE INTO reference_pins (content, source_record, state, at_ms) VALUES (?, ?, 'pending', ?)",
            &[s(j.content), s(record), bind_u64(now_ms)?],
        )?;
    }
    Ok(())
}

/// The next pin to run (pending, past its retry time), oldest first.
fn pin_ready(sql: &impl Sql, now_ms: u64) -> R<Option<PinJob>> {
    let rows = sql.exec(
        "SELECT content, source_record FROM reference_pins WHERE state = 'pending' AND retry_ms <= ? ORDER BY at_ms, content LIMIT 1",
        &[bind_u64(now_ms)?],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    let source = serde_json::from_str(text(r, 1)?).map_err(|_| EvalError::State("pin source record"))?;
    Ok(Some(PinJob { content: text(r, 0)?.to_owned(), source }))
}

/// Records missing pins ([`ensure`]), then the next pin to run.
pub fn due(sql: &impl Sql, now_ms: u64) -> R<Option<PinJob>> {
    ensure(sql, now_ms)?;
    pin_ready(sql, now_ms)
}

/// The pinned record of `content`, once its pin is written.
pub fn pinned(sql: &impl Sql, content: &str) -> R<Option<ArtifactJson>> {
    let rows = sql.exec("SELECT record FROM reference_pins WHERE content = ? AND state = 'pinned'", &[s(content)])?;
    let Some(r) = rows.first() else { return Ok(None) };
    let Some(record) = opt_text(r, 0) else { return Ok(None) };
    serde_json::from_str(&record).map(Some).map_err(|_| EvalError::State("pinned reference record"))
}

/// The state of `content`'s pin (`None`: no row).
fn pin_state(sql: &impl Sql, content: &str) -> R<Option<String>> {
    Ok(sql.exec("SELECT state FROM reference_pins WHERE content = ?", &[s(content)])?.first().and_then(|r| opt_text(r, 0)))
}

/// One file the pin copies (or finds in place).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyItem {
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
    /// The bucket (`admission` or `eval`), key and entity tag the source
    /// manifest names (the copy reads the source pinned to it; a source
    /// file with no recorded tag is read as it is, R2 still verifying the
    /// copy's SHA-256).
    pub src_bucket: String,
    pub src_key: String,
    pub src_etag: Option<String>,
    /// The reference key in the admission bucket.
    pub dst_key: String,
}

/// What one pin does: the files to copy, and the source manifest moved to
/// the reference locations, whose entity tags the heads of the copies pin.
#[derive(Clone, Debug)]
pub struct CopyPlan {
    pub items: Vec<CopyItem>,
    pub manifest: FilesManifest,
    /// Every file is at its reference location with a pinned tag already
    /// (a staged package): the source record is its own pin.
    pub as_is: bool,
}

/// Why a pin did not complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinFailure {
    /// A platform failure (R2, a read); retried after a tick.
    Transient(&'static str),
    /// Waiting cannot fix it (a source object absent or changed, a file at
    /// its reference key with other bytes, a manifest outside the rule):
    /// stops until the operator's recheck.
    Permanent(&'static str),
}

impl PinFailure {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Transient(c) | Self::Permanent(c) => c,
        }
    }
}

/// Plans one pin from the source's files manifest bytes (read at its
/// record's key): each file's copy to its reference key, or none when the
/// package is staged already. The source was admitted (or staged) under
/// its own spec; the pin moves its files and changes nothing the spec
/// checks, so the pinned manifest lists the same header ([`finish`]).
pub fn plan_copy(source_manifest: &[u8]) -> Result<CopyPlan, PinFailure> {
    let source: FilesManifest =
        serde_json::from_slice(source_manifest).map_err(|_| PinFailure::Permanent("source_manifest_invalid"))?;
    if source.header().is_err() || source.files.is_empty() {
        return Err(PinFailure::Permanent("source_manifest_invalid"));
    }
    let as_is = source
        .files
        .iter()
        .all(|f| f.bucket == REFERENCE_BUCKET && f.key == reference_file_key(&f.sha256) && f.etag.is_some());
    let items = source
        .files
        .iter()
        .map(|f| CopyItem {
            name: f.name.clone(),
            sha256: f.sha256.clone(),
            bytes: f.bytes,
            src_bucket: f.bucket.clone(),
            src_key: f.key.clone(),
            src_etag: f.etag.clone(),
            dst_key: reference_file_key(&f.sha256),
        })
        .collect();
    let manifest = FilesManifest {
        files: source
            .files
            .iter()
            .map(|f| FileEntry { bucket: REFERENCE_BUCKET.into(), key: reference_file_key(&f.sha256), etag: None, ..f.clone() })
            .collect(),
        ..source
    };
    Ok(CopyPlan { items, manifest, as_is })
}

/// A written pinned manifest: its exact bytes and package digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pinned {
    pub bytes: Vec<u8>,
    pub package: String,
}

/// Pins the copies' entity tags from their heads (in [`CopyPlan::items`]
/// order), under the staging rule's checks of each stored object (the
/// named length and stored SHA-256, clean attributes, a label entity tag):
/// the pinned manifest's bytes and package.
pub fn finish(plan: CopyPlan, heads: &[Head]) -> Result<Pinned, PinFailure> {
    let CopyPlan { mut manifest, .. } = plan;
    if heads.len() != manifest.files.len() {
        return Err(PinFailure::Transient("reference_head"));
    }
    let header = manifest.header().map_err(|_| PinFailure::Permanent("pinned_manifest_invalid"))?;
    for (f, h) in manifest.files.iter_mut().zip(heads) {
        let st = match h {
            Head::Absent => return Err(PinFailure::Transient("reference_copy_missing")),
            Head::Error => return Err(PinFailure::Transient("reference_head")),
            Head::Present(st) => st,
        };
        if st.size != f.bytes || st.sha256.map(hex::encode).as_deref() != Some(f.sha256.as_str()) {
            return Err(PinFailure::Permanent("reference_mismatch"));
        }
        if !st.clean {
            return Err(PinFailure::Permanent("reference_attributes"));
        }
        if !reference_label_ok(&st.etag, MAX_ETAG_BYTES) {
            return Err(PinFailure::Permanent("reference_entity_tag"));
        }
        f.etag = Some(st.etag.clone());
    }
    let bytes = manifest.to_bytes();
    let reparsed: FilesManifest = serde_json::from_slice(&bytes).map_err(|_| PinFailure::Permanent("pinned_manifest_invalid"))?;
    if bytes.len() > MAX_MANIFEST_BYTES || reparsed.header().ok() != Some(header) {
        return Err(PinFailure::Permanent("pinned_manifest_invalid"));
    }
    let package = FilesManifest::package_of(&bytes);
    Ok(Pinned { bytes, package })
}

/// The pinned record: the source record at the pinned package and its
/// manifest key.
pub fn pinned_record(source: &ArtifactJson, package: &str) -> ArtifactJson {
    let mut a = source.clone();
    a.package_sha256 = package.to_owned();
    a.manifest_key = manifest_key(package);
    a
}

/// A pin is written: later resolutions name `record`.
pub fn pin_done(sql: &impl Sql, content: &str, record: &ArtifactJson, copied: u64, now_ms: u64) -> R<()> {
    let r = serde_json::to_string(record).map_err(|_| EvalError::State("pinned reference record"))?;
    sql.exec(
        "UPDATE reference_pins SET state = 'pinned', record = ?, error = NULL, copied = ?, pinned_ms = ?,
            attempts = attempts + 1 WHERE content = ?",
        &[s(r), bind_u64(copied)?, bind_u64(now_ms)?, s(content)],
    )?;
    Ok(())
}

/// A pin did not complete: a transient failure waits a tick, a permanent one
/// stops until the operator's recheck.
pub fn pin_failed(sql: &impl Sql, content: &str, failure: &PinFailure, now_ms: u64) -> R<()> {
    let (state, retry) = match failure {
        PinFailure::Transient(_) => ("pending", now_ms.saturating_add(crate::eval::admission::TICK_MS)),
        PinFailure::Permanent(_) => ("failed", now_ms),
    };
    sql.exec(
        "UPDATE reference_pins SET state = ?, error = ?, retry_ms = ?, attempts = attempts + 1 WHERE content = ? AND state <> 'pinned'",
        &[s(state), s(failure.code()), bind_u64(retry)?, s(content)],
    )?;
    Ok(())
}

// -------------------------------------------------------------- presence

/// The packages a presence check heads: each with its files manifest key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Target {
    pub package: String,
    pub manifest_key: String,
}

/// One presence check to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenceJob {
    pub targets: Vec<Target>,
    /// The cohort number the calendar would queue next, when one is due.
    pub for_cohort: Option<u64>,
}

/// The next cohort's references as the calendar would name them, their
/// contents, and the cohort number it would queue (`None` while no cohort
/// is due).
struct Next {
    refs: Vec<Target>,
    contents: Vec<String>,
    for_cohort: Option<u64>,
}

fn next(sql: &impl Sql, first_cohort: u64) -> R<Option<Next>> {
    let Some(n) = crate::eval::calendar::next_references(sql)? else { return Ok(None) };
    let mut refs = vec![Target { package: n.reference.package_sha256.clone(), manifest_key: n.reference.manifest_key.clone() }];
    if let Some(p) = &n.previous {
        refs.push(Target { package: p.package_sha256.clone(), manifest_key: p.manifest_key.clone() });
    }
    let contents = match &n.contents {
        Some((c, p)) => std::iter::once(c.clone()).chain(p.clone()).collect(),
        None => Vec::new(),
    };
    let for_cohort = crate::eval::calendar::due_number(sql, first_cohort)?;
    Ok(Some(Next { refs, contents, for_cohort }))
}

/// Every package a presence check heads: the next cohort's references and
/// the references every open cohort names, sorted by package.
fn targets(sql: &impl Sql, next: Option<&Next>) -> R<Vec<Target>> {
    let mut out: Vec<Target> = next.map(|n| n.refs.clone()).unwrap_or_default();
    let rows = sql.exec(
        "SELECT json_extract(plan, '$.reference.package_sha256'), json_extract(plan, '$.reference.manifest_key'),
            json_extract(plan, '$.previous_reference.package_sha256'), json_extract(plan, '$.previous_reference.manifest_key')
         FROM cohorts WHERE phase NOT IN ('closed', 'abandoned')",
        &[],
    )?;
    for r in &rows {
        for (p, k) in [(0, 1), (2, 3)] {
            if let (Some(package), Some(manifest_key)) = (opt_text(r, p), opt_text(r, k)) {
                out.push(Target { package, manifest_key });
            }
        }
    }
    out.sort_by(|a, b| a.package.cmp(&b.package));
    out.dedup_by(|a, b| a.package == b.package);
    Ok(out)
}

fn packages_json(t: &[Target]) -> String {
    serde_json::to_string(&t.iter().map(|x| x.package.as_str()).collect::<Vec<_>>()).unwrap_or_default()
}

/// The latest presence check's row.
struct Presence {
    packages: String,
    for_cohort: Option<u64>,
    ok: bool,
    missing: Vec<Value>,
    error: Option<String>,
    checked_ms: u64,
    retry_ms: u64,
}

fn presence(sql: &impl Sql) -> R<Option<Presence>> {
    let rows = sql.exec(
        "SELECT packages, for_cohort, ok, missing, error, checked_ms, retry_ms FROM reference_presence WHERE id = 1",
        &[],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    let u = |v: Option<i64>| v.and_then(|x| u64::try_from(x).ok());
    Ok(Some(Presence {
        packages: text(r, 0)?.to_owned(),
        for_cohort: u(opt_int(r, 1)),
        ok: int(r, 2)? != 0,
        missing: serde_json::from_str(text(r, 3)?).unwrap_or_default(),
        error: opt_text(r, 4),
        checked_ms: u(opt_int(r, 5)).unwrap_or(0),
        retry_ms: u(opt_int(r, 6)).unwrap_or(0),
    }))
}

/// The presence check to run now, if one is due (only once a reference
/// chain exists): none has run; the packages
/// the references resolve to changed since (a promotion, a decision, a pin,
/// a cohort queued or closed); a cohort is due and the last check was not
/// for its number; or the last check found a file missing or could not head
/// one, and a tick has passed. A read.
pub fn presence_due(sql: &impl Sql, first_cohort: u64, now_ms: u64) -> R<Option<PresenceJob>> {
    // Only once a reference chain exists: before it, every reference is
    // the operator's staged package named by a template or plan.
    if sql.exec("SELECT 1 FROM reference_state WHERE id = 1", &[])?.is_empty() {
        return Ok(None);
    }
    let n = next(sql, first_cohort)?;
    let targets = targets(sql, n.as_ref())?;
    if targets.is_empty() {
        return Ok(None);
    }
    let for_cohort = n.as_ref().and_then(|n| n.for_cohort);
    let job = PresenceJob { targets, for_cohort };
    let Some(p) = presence(sql)? else { return Ok(Some(job)) };
    let due = p.packages != packages_json(&job.targets)
        || (for_cohort.is_some() && p.for_cohort != for_cohort)
        || (!p.ok && p.retry_ms <= now_ms);
    Ok(due.then_some(job))
}

/// What a head found for one file of a checked package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileCheck {
    Present,
    /// Absent, another entity tag or size than the manifest pins, or the
    /// files manifest itself absent or unreadable: the reason.
    Missing(&'static str),
    /// The head (or the manifest read) failed: never an absence.
    Error,
}

/// One file's outcome against its files manifest entry.
pub fn check_file(f: &FileEntry, head: &Head) -> FileCheck {
    match head {
        Head::Absent => FileCheck::Missing("absent"),
        Head::Error => FileCheck::Error,
        Head::Present(st) if f.etag.as_ref().is_some_and(|e| *e != st.etag) => FileCheck::Missing("etag_changed"),
        Head::Present(st) if st.size != f.bytes => FileCheck::Missing("size_changed"),
        Head::Present(_) => FileCheck::Present,
    }
}

/// One checked file (or a package's manifest, `file` `files_manifest`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub package: String,
    pub file: String,
    pub key: String,
    pub outcome: FileCheck,
}

/// Records a presence check: every file present, or the missing ones (by
/// package, file name, key and reason) and whether a head failed. A check
/// that is not clean runs again after a tick.
pub fn presence_done(sql: &impl Sql, job: &PresenceJob, checked: &[Checked], now_ms: u64) -> R<bool> {
    let missing: Vec<Value> = checked
        .iter()
        .filter_map(|c| match c.outcome {
            FileCheck::Missing(why) => Some(json!({"package": c.package, "file": c.file, "key": c.key, "reason": why})),
            _ => None,
        })
        .collect();
    let failed = checked.iter().any(|c| c.outcome == FileCheck::Error);
    let ok = missing.is_empty() && !failed;
    let retry = if ok { now_ms } else { now_ms.saturating_add(crate::eval::admission::TICK_MS) };
    sql.exec(
        "INSERT INTO reference_presence (id, packages, for_cohort, ok, missing, error, checked_ms, retry_ms)
         VALUES (1, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (id) DO UPDATE SET packages = excluded.packages, for_cohort = excluded.for_cohort, ok = excluded.ok,
            missing = excluded.missing, error = excluded.error, checked_ms = excluded.checked_ms, retry_ms = excluded.retry_ms",
        &[
            s(packages_json(&job.targets)),
            job.for_cohort.map_or(Ok(SqlValue::Null), bind_u64)?,
            i(i64::from(ok)),
            s(serde_json::to_string(&missing).unwrap_or_else(|_| "[]".into())),
            if failed { s("head_failed") } else { SqlValue::Null },
            bind_u64(now_ms)?,
            bind_u64(retry)?,
        ],
    )?;
    Ok(ok)
}

// ------------------------------------------------------------------ gate

/// Why the calendar may not queue its next cohort now (`None`: it may): a
/// reference of that cohort not pinned, or pinned but failed, or no clean
/// presence check for that cohort's number that headed its references. The
/// template's own references (no chain yet) are the operator's staged
/// packages and are not gated. A read.
pub fn gate(sql: &impl Sql, first_cohort: u64) -> R<Option<&'static str>> {
    let Some(n) = next(sql, first_cohort)? else { return Ok(None) };
    if n.contents.is_empty() {
        return Ok(None);
    }
    for c in &n.contents {
        match pin_state(sql, c)?.as_deref() {
            Some("pinned") => {}
            Some("failed") => return Ok(Some(GATE_PIN_FAILED)),
            _ => return Ok(Some(GATE_PIN_PENDING)),
        }
    }
    let Some(p) = presence(sql)? else { return Ok(Some(GATE_UNCHECKED)) };
    let headed: Vec<String> = serde_json::from_str(&p.packages).unwrap_or_default();
    let refs_headed = n.refs.iter().all(|t| headed.contains(&t.package));
    if !refs_headed || (n.for_cohort.is_some() && p.for_cohort != n.for_cohort) {
        return Ok(Some(GATE_UNCHECKED));
    }
    let missing_ref = p.missing.iter().any(|m| {
        let pkg = m.get("package").and_then(Value::as_str).unwrap_or_default();
        n.refs.iter().any(|t| t.package == pkg)
    });
    if missing_ref {
        return Ok(Some(GATE_MISSING));
    }
    if !p.ok && p.error.is_some() {
        return Ok(Some(GATE_UNCHECKED));
    }
    Ok(None)
}

/// Whether a pin or a presence check is due by `now_ms` (the alarm's
/// reason to run). A read.
pub fn has_work(sql: &impl Sql, first_cohort: u64, now_ms: u64) -> R<bool> {
    Ok(!unrecorded(sql)?.is_empty() || pin_ready(sql, now_ms)?.is_some() || presence_due(sql, first_cohort, now_ms)?.is_some())
}

/// The status block (`references` on `/eval/status`): every pin (with
/// `named`: whether the chain or a pending promotion names its content now,
/// `null` when that read fails), the latest presence check, and the
/// calendar's gate.
pub fn status(sql: &impl Sql, first_cohort: u64) -> R<Value> {
    let names = named(sql).ok();
    let pins: Vec<Value> = sql
        .exec(
            "SELECT content, state, attempts, error, retry_ms, copied, at_ms, pinned_ms, json_extract(source_record, '$.package_sha256'),
                json_extract(record, '$.package_sha256') FROM reference_pins ORDER BY at_ms, content",
            &[],
        )?
        .iter()
        .map(|r| {
            let content = text(r, 0)?;
            let is_named = names.as_ref().map(|n| n.iter().any(|c| c == content));
            Ok(json!({"content": content, "state": text(r, 1)?, "attempts": int(r, 2)?, "error": opt_text(r, 3),
                "retry_ms": opt_int(r, 4), "copied": opt_int(r, 5), "at_ms": opt_int(r, 6), "pinned_ms": opt_int(r, 7),
                "source_package": opt_text(r, 8), "package": opt_text(r, 9), "named": is_named}))
        })
        .collect::<R<_>>()?;
    let presence = presence(sql)?.map(|p| {
        let packages: Value = serde_json::from_str(&p.packages).unwrap_or(Value::Null);
        json!({"packages": packages, "for_cohort": p.for_cohort, "ok": p.ok, "missing": p.missing, "error": p.error,
            "checked_ms": p.checked_ms, "retry_ms": p.retry_ms})
    });
    let gate = gate(sql, first_cohort)?;
    Ok(json!({"rule": PIN_RULE, "pins": pins, "presence": presence, "gate": gate,
        "objects_missing": gate == Some(GATE_MISSING) || presence.as_ref().is_some_and(|p| p["missing"].as_array().is_some_and(|m| !m.is_empty())),
        "pin_failed": gate == Some(GATE_PIN_FAILED)}))
}

// -------------------------------------------------------------- operator

/// The operator's recheck (`POST /eval/references/recheck`, operator token,
/// journaled): every failed pin row is dropped and the rows are re-created
/// at once ([`ensure`]) for the contents the chain or a pending promotion
/// names now, each from the record it names now (so a pin that failed on
/// an older source, such as an abandoned promotion whose objects were
/// cleaned up, is retried from the current one), and the presence check
/// runs again at the next step. A failed row for a content nothing names
/// any more is not re-created. Changes no reference, plan or record; the
/// effect names the pins it retries and the ones it drops.
pub fn recheck(sql: &impl Sql, cfg: &EvalConfig, req: &Request, now_ms: u64) -> R<Result<Value, &'static str>> {
    let authority = cfg.authority.name().to_owned();
    match operator::begin(sql, req, "eval.references_recheck", false, None, &crate::eval::decide::address(cfg, &authority))? {
        Ok(Begin::Answer(v)) => return Ok(Ok(v)),
        Ok(Begin::Proceed(_)) => {}
        Err(code) => return Ok(Err(code)),
    }
    let failed: Vec<String> = sql
        .exec("SELECT content FROM reference_pins WHERE state = 'failed' ORDER BY content", &[])?
        .iter()
        .map(|r| text(r, 0).map(str::to_owned))
        .collect::<Result<_, _>>()?;
    let names = named(sql)?;
    let (retried, dropped): (Vec<String>, Vec<String>) = failed.into_iter().partition(|c| names.contains(c));
    let effect = json!({"pins_retried": retried, "pins_dropped": dropped, "presence_rechecked": true});
    if req.preview {
        return Ok(Ok(json!({"preview": true, "effect": effect})));
    }
    sql.exec("DELETE FROM reference_pins WHERE state = 'failed'", &[])?;
    ensure(sql, now_ms)?;
    sql.exec("DELETE FROM reference_presence", &[])?;
    let o = engine::observed(sql)?;
    let n = operator::record(sql, req, "eval.references_recheck", None, &effect, o.epoch.map(|e| e.index), now_ms)?;
    Ok(Ok(json!({"pins_retried": effect.get("pins_retried"), "pins_dropped": effect.get("pins_dropped"),
        "presence_rechecked": true, "operator_seq": n})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::admission::Stored;

    fn entry(name: &str, sha: &str, bucket: &str, key: &str, etag: Option<&str>) -> FileEntry {
        FileEntry {
            name: name.into(),
            role: "r".into(),
            bytes: 10,
            sha256: sha.into(),
            bucket: bucket.into(),
            key: key.into(),
            etag: etag.map(Into::into),
        }
    }

    /// The front router (`cf.rs`, wasm32 only, so this reads its source)
    /// forwards every main-only route the cohort object serves
    /// (`cf_eval.rs`), among them the journaled recheck this rule names.
    #[test]
    fn every_main_only_cohort_object_route_is_reachable_through_the_router() {
        let cf = include_str!("../cf.rs");
        let start = cf.find("async fn eval_route(").expect("eval_route");
        let body = cf.get(start..).and_then(|r| r.find("\n}\n").and_then(|e| r.get(..e))).expect("eval_route body");
        let eval = include_str!("../cf_eval.rs");
        let mut paths: Vec<&str> = Vec::new();
        for line in eval.lines().map(str::trim_start) {
            let Some(arm) = line.strip_prefix("(Method::") else { continue };
            let Some((pattern, _)) = arm.split_once(") if main =>") else { continue };
            paths.extend(pattern.split('"').skip(1).step_by(2));
        }
        assert!(paths.contains(&"/references/recheck") && paths.contains(&"/references/decision"), "{paths:?}");
        for p in &paths {
            assert!(body.contains(&format!("\"{p}\"")), "the router does not forward {p}");
        }
    }

    #[test]
    fn a_file_is_present_only_at_its_pinned_tag_and_size() {
        let f = entry("a", &"11".repeat(32), "admission", "k", Some("t1"));
        let st = |etag: &str, size: u64| Head::Present(Stored { size, sha256: None, etag: etag.into(), clean: true });
        assert_eq!(check_file(&f, &st("t1", 10)), FileCheck::Present);
        assert_eq!(check_file(&f, &st("t2", 10)), FileCheck::Missing("etag_changed"));
        assert_eq!(check_file(&f, &st("t1", 11)), FileCheck::Missing("size_changed"));
        assert_eq!(check_file(&f, &Head::Absent), FileCheck::Missing("absent"));
        assert_eq!(check_file(&f, &Head::Error), FileCheck::Error);
    }
}

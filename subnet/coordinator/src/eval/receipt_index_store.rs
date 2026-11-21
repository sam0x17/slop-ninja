//! Durable private index-chunk upload intents. The scheduler's versioned
//! producer is separate. All mutating calls run in the caller's SQL transaction.
//! Object I/O occurs between `prepare` and `confirm`, never inside SQL.
//!
//! A lost upload response leaves the same intent and exact object key. Recovery
//! reads that object, confirms its bytes, or retries the identical put. Receipt
//! object verification by the scoring/bundle lanes remains a separate required
//! check; this journal verifies index chunks, not the underlying receipts.

use super::{IndexScan, canonical_digest, packaging_manifest};
use crate::eval::engine::{EvalError, R};
use crate::sql::{Sql, SqlValue, bind_u64, int, text, uint};
use slop_ninja_protocol::receipt_chunks::{CHUNK_BYTES, ReceiptIndexChunk, ReceiptIndexCommitment};
use slop_ninja_protocol::{Digest, Record, object_sha256};

/// Additive migration; it neither changes nor enables a cohort's receipt format.
pub const SCHEMA_V33: &[&str] = &[
    "CREATE TABLE receipt_index_builds (
        seq INTEGER PRIMARY KEY, manifest TEXT NOT NULL, expected INTEGER NOT NULL,
        next_ordinal INTEGER NOT NULL, after_attempt TEXT, covered INTEGER NOT NULL
    ) STRICT",
    "CREATE TABLE receipt_index_chunks (
        seq INTEGER NOT NULL, ordinal INTEGER NOT NULL, digest TEXT NOT NULL,
        object_sha256 TEXT NOT NULL, bytes INTEGER NOT NULL, receipts INTEGER NOT NULL,
        last_attempt TEXT NOT NULL, confirmed INTEGER NOT NULL CHECK(confirmed IN (0,1)),
        PRIMARY KEY(seq, ordinal)
    ) STRICT",
];

struct State {
    scan: IndexScan,
    next: u64,
    after: Option<Digest>,
    covered: u64,
}

fn state(sql: &impl Sql, seq: i64) -> R<State> {
    let rows = sql.exec("SELECT manifest, expected, next_ordinal, after_attempt, covered FROM receipt_index_builds WHERE seq = ?", &[seq.into()])?;
    let row = rows
        .first()
        .ok_or(EvalError::State("receipt index build missing"))?;
    let manifest = canonical_digest(text(row, 0)?)?;
    if packaging_manifest(sql, seq)? != manifest {
        return Err(EvalError::State("receipt index build manifest changed"));
    }
    let after = match row.get(3) {
        Some(SqlValue::Null) => None,
        Some(SqlValue::Text(value)) => Some(canonical_digest(value)?),
        _ => return Err(EvalError::State("receipt index build cursor")),
    };
    let value = State {
        scan: IndexScan {
            seq,
            manifest,
            count: uint(row, 1)?,
        },
        next: uint(row, 2)?,
        after,
        covered: uint(row, 4)?,
    };
    if (value.next == 0) != value.after.is_none() || value.covered > value.scan.count {
        return Err(EvalError::State("receipt index build cursor"));
    }
    Ok(value)
}

/// Choose the scope before entering the package operation. Until the count is
/// captured, an indexed existence read distinguishes an empty cohort. After
/// capture, only confirmation of the last page selects final assembly.
pub fn page_pending(sql: &impl Sql, seq: i64) -> R<bool> {
    let rows = sql.exec("SELECT covered, expected FROM receipt_index_builds WHERE seq = ?", &[seq.into()])?;
    if let Some(row) = rows.first() {
        let covered = uint(row, 0)?;
        let expected = uint(row, 1)?;
        if covered > expected {
            return Err(EvalError::State("receipt index build cursor"));
        }
        return Ok(covered < expected);
    }
    Ok(!sql.exec("SELECT 1 FROM receipts WHERE seq = ? LIMIT 1", &[seq.into()])?.is_empty())
}

/// Idempotent initial binding. A second start never recaptures a changed count.
pub fn start(sql: &impl Sql, seq: i64) -> R<()> {
    let scan = IndexScan::open(sql, seq)?;
    let existing = sql.exec(
        "SELECT seq FROM receipt_index_builds WHERE seq = ?",
        &[seq.into()],
    )?;
    if !existing.is_empty() {
        let held = state(sql, seq)?;
        if held.scan != scan {
            return Err(EvalError::State("receipt index build changed"));
        }
        return Ok(());
    }
    sql.exec(
        "INSERT INTO receipt_index_builds VALUES (?, ?, ?, 0, NULL, 0)",
        &[
            seq.into(),
            scan.manifest.to_string().into(),
            bind_u64(scan.count)?,
        ],
    )?;
    Ok(())
}

/// The exact object upload. The key is cohort-private and lies beneath the
/// existing detailed-receipt retirement prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    pub key: String,
    pub ordinal: u64,
    pub sha256: Digest,
    pub bytes: Vec<u8>,
}

/// Resumes a root step without recounting the entire receipt table for every
/// chunk. The immutable count is captured once at start and checked again at
/// finish; each prepared/confirmed page still checks its packaging identity.
pub fn resume(sql: &impl Sql, seq: i64) -> R<Option<Upload>> {
    if sql
        .exec(
            "SELECT seq FROM receipt_index_builds WHERE seq = ?",
            &[seq.into()],
        )?
        .is_empty()
    {
        start(sql, seq)?;
    }
    prepare(sql, seq)
}

fn encoded(chunk: &ReceiptIndexChunk) -> R<Vec<u8>> {
    chunk
        .to_bytes()
        .map_err(|_| EvalError::State("receipt index chunk encoding"))
}

fn check_intent(sql: &impl Sql, seq: i64, chunk: &ReceiptIndexChunk, bytes: &[u8]) -> R<bool> {
    let rows = sql.exec("SELECT digest, object_sha256, bytes, receipts, last_attempt, confirmed FROM receipt_index_chunks WHERE seq = ? AND ordinal = ?",
        &[seq.into(), bind_u64(chunk.ordinal)?])?;
    let row = rows
        .first()
        .ok_or(EvalError::State("receipt index intent missing"))?;
    if text(row, 0)?
        != chunk
            .digest()
            .map_err(|_| EvalError::State("receipt index chunk digest"))?
            .to_string()
        || text(row, 1)? != object_sha256(bytes).to_string()
        || uint(row, 2)? != bytes.len() as u64
        || uint(row, 3)? != chunk.entries.len() as u64
        || text(row, 4)?
            != chunk
                .entries
                .last()
                .ok_or(EvalError::State("empty receipt index chunk"))?
                .attempt
                .to_string()
    {
        return Err(EvalError::State("receipt index intent conflict"));
    }
    match int(row, 5)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(EvalError::State("receipt index confirmation")),
    }
}

/// Creates or reconciles one intent. Returns the same key and bytes until its
/// stored object has been confirmed. `None` means all captured rows are covered.
pub fn prepare(sql: &impl Sql, seq: i64) -> R<Option<Upload>> {
    let s = state(sql, seq)?;
    if s.covered == s.scan.count {
        return Ok(None);
    }
    let chunk = s
        .scan
        .page(sql, s.next, s.after)?
        .ok_or(EvalError::State("receipt index rows missing"))?;
    let covered = s
        .covered
        .checked_add(chunk.entries.len() as u64)
        .ok_or(EvalError::State("receipt index count overflow"))?;
    if covered > s.scan.count {
        return Err(EvalError::State("receipt index rows grew"));
    }
    let bytes = encoded(&chunk)?;
    let sha256 = object_sha256(&bytes);
    let digest = chunk
        .digest()
        .map_err(|_| EvalError::State("receipt index chunk digest"))?;
    sql.exec("INSERT INTO receipt_index_chunks VALUES (?, ?, ?, ?, ?, ?, ?, 0) ON CONFLICT(seq, ordinal) DO NOTHING",
        &[seq.into(), bind_u64(s.next)?, digest.to_string().into(), sha256.to_string().into(),
          bind_u64(bytes.len() as u64)?, bind_u64(chunk.entries.len() as u64)?,
          chunk.entries.last().ok_or(EvalError::State("empty receipt index chunk"))?.attempt.to_string().into()])?;
    if check_intent(sql, seq, &chunk, &bytes)? {
        return Err(EvalError::State("receipt index cursor behind confirmation"));
    }
    let rows = sql.exec("SELECT label FROM cohorts WHERE seq = ?", &[seq.into()])?;
    let label = text(
        rows.first()
            .ok_or(EvalError::State("receipt index label"))?,
        0,
    )?;
    if label.is_empty()
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(EvalError::State("receipt index label"));
    }
    let key = format!(
        "{}/receipts/index-v2/{:016x}-{}.bin",
        crate::eval::engine::receipt_prefix(label),
        s.next,
        sha256
    );
    Ok(Some(Upload {
        key,
        ordinal: s.next,
        sha256,
        bytes,
    }))
}

/// Confirms bytes read back from the intended private object. The caller checks
/// that the read used `Upload::key`. This method bounds and decodes the bytes,
/// checks the persisted intent and current source page, then advances atomically.
/// Duplicate confirmations return successfully without advancing twice.
pub fn confirm(sql: &impl Sql, seq: i64, ordinal: u64, stored: &[u8]) -> R<()> {
    if stored.len() > CHUNK_BYTES {
        return Err(EvalError::State("receipt index object bound"));
    }
    let chunk = ReceiptIndexChunk::from_bytes(stored)
        .map_err(|_| EvalError::State("receipt index stored object"))?;
    let s = state(sql, seq)?;
    if chunk.ordinal != ordinal || chunk.cohort_manifest != s.scan.manifest {
        return Err(EvalError::State("receipt index stored identity"));
    }
    let confirmed = check_intent(sql, seq, &chunk, stored)?;
    if ordinal < s.next {
        return if confirmed {
            Ok(())
        } else {
            Err(EvalError::State("receipt index missing confirmation"))
        };
    }
    if ordinal != s.next || confirmed {
        return Err(EvalError::State("receipt index confirmation order"));
    }
    let current = s
        .scan
        .page(sql, ordinal, s.after)?
        .ok_or(EvalError::State("receipt index rows missing"))?;
    if encoded(&current)? != stored {
        return Err(EvalError::State("receipt index source changed"));
    }
    let covered = s
        .covered
        .checked_add(chunk.entries.len() as u64)
        .ok_or(EvalError::State("receipt index count overflow"))?;
    if covered > s.scan.count {
        return Err(EvalError::State("receipt index rows grew"));
    }
    let next = s
        .next
        .checked_add(1)
        .ok_or(EvalError::State("receipt index ordinal overflow"))?;
    sql.exec(
        "UPDATE receipt_index_chunks SET confirmed=1 WHERE seq=? AND ordinal=?",
        &[seq.into(), bind_u64(ordinal)?],
    )?;
    sql.exec(
        "UPDATE receipt_index_builds SET next_ordinal=?, after_attempt=?, covered=? WHERE seq=?",
        &[
            bind_u64(next)?,
            chunk
                .entries
                .last()
                .ok_or(EvalError::State("empty receipt index chunk"))?
                .attempt
                .to_string()
                .into(),
            bind_u64(covered)?,
            seq.into(),
        ],
    )?;
    Ok(())
}

/// Replays only the confirmed digest ledger, one bounded SQL page at a time.
/// It never serializes internal SHA256 state or reloads every receipt entry.
pub fn finish(sql: &impl Sql, seq: i64) -> R<ReceiptIndexCommitment> {
    let s = state(sql, seq)?;
    if s.covered != s.scan.count {
        return Err(EvalError::State("receipt index build incomplete"));
    }
    let mut after = 0_u64;
    let mut page = Vec::new().into_iter();
    let digests = std::iter::from_fn(|| {
        if page.len() == 0 {
            match sql.exec("SELECT ordinal, digest, confirmed FROM receipt_index_chunks WHERE seq=? AND ordinal>=? ORDER BY ordinal LIMIT 256",
                &[seq.into(), match bind_u64(after) { Ok(v) => v, Err(_) => return Some(Err(slop_ninja_protocol::Error::OutOfRange("receipt index ordinal"))) }]) {
                Ok(rows) => { if rows.is_empty() { return None; } page = rows.into_iter(); }
                Err(_) => return Some(Err(slop_ninja_protocol::Error::Invalid("receipt index ledger read"))),
            }
        }
        let row = page.next()?;
        if uint(&row, 0).ok() != Some(after) || int(&row, 2).ok() != Some(1) {
            return Some(Err(slop_ninja_protocol::Error::Invalid(
                "receipt index ledger coverage",
            )));
        }
        after += 1;
        Some(
            text(&row, 1)
                .ok()
                .and_then(|value| canonical_digest(value).ok())
                .ok_or(slop_ninja_protocol::Error::Invalid(
                    "receipt index ledger digest",
                )),
        )
    });
    let commitment =
        ReceiptIndexCommitment::from_verified_chunk_digests(s.scan.manifest, s.scan.count, digests)
            .map_err(|_| EvalError::State("receipt index ledger commitment"))?;
    if after != s.next {
        return Err(EvalError::State("receipt index ledger cursor"));
    }
    s.scan.check_complete(sql, &commitment)?;
    Ok(commitment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Sqlite, tx};
    use slop_ninja_protocol::receipt_chunks::{CHUNK_ENTRIES, ReceiptIndexBuilder};

    fn digest(n: u64) -> Digest {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&n.to_be_bytes());
        Digest(bytes)
    }

    fn db(count: usize) -> Sqlite {
        let db = Sqlite::open();
        let receipts = crate::eval::engine::SCHEMA_V2
            .iter()
            .find(|s| s.starts_with("CREATE TABLE receipts ("))
            .unwrap();
        db.conn.execute_batch(receipts).unwrap();
        db.conn.execute_batch("CREATE TABLE cohorts (seq INTEGER PRIMARY KEY, phase TEXT NOT NULL, manifest_digest TEXT, label TEXT NOT NULL, plan TEXT, plan_sha256 TEXT) STRICT").unwrap();
        db.conn
            .execute(
                "INSERT INTO cohorts VALUES (1, 'packaging', ?, 'synthetic-index-recovery', NULL, NULL)",
                [digest(91).to_string()],
            )
            .unwrap();
        for schema in SCHEMA_V33 {
            db.conn.execute_batch(schema).unwrap();
        }
        for n in (0..count as u64).rev() {
            db.conn
                .execute(
                    "INSERT INTO receipts VALUES (1, ?, 0, ?, 1, 1, ?, 153, 'object')",
                    rusqlite::params![digest(n).to_string(), n, digest(92).to_string()],
                )
                .unwrap();
        }
        db
    }

    #[test]
    fn package_scope_follows_confirmed_pages() {
        let empty = db(0);
        assert!(!page_pending(&empty, 1).unwrap());
        let db = db(CHUNK_ENTRIES + 1);
        for ordinal in 0..2 {
            assert!(page_pending(&db, 1).unwrap());
            let upload = tx(&db, |sql| resume(sql, 1)).unwrap().unwrap();
            assert_eq!(upload.ordinal, ordinal);
            assert!(page_pending(&db, 1).unwrap());
            tx(&db, |sql| confirm(sql, 1, ordinal, &upload.bytes)).unwrap();
        }
        assert!(!page_pending(&db, 1).unwrap());
        db.conn.execute("UPDATE receipt_index_builds SET covered = expected + 1", []).unwrap();
        assert!(page_pending(&db, 1).is_err());
    }

    #[test]
    fn root_upload_step_recovers_each_io_boundary_without_duplicate_objects() {
        use crate::eval::lane_logic::block_on;
        use crate::eval::receipt_index::io::{Io, step};
        struct Runtime<'a> {
            db: &'a Sqlite,
            objects: &'a mut std::collections::BTreeMap<String, Vec<u8>>,
            puts: &'a mut usize,
            fault: Option<&'static str>,
        }
        impl Runtime<'_> {
            fn fail(&mut self, stage: &'static str) -> bool {
                if self.fault == Some(stage) {
                    self.fault = None;
                    true
                } else {
                    false
                }
            }
        }
        impl Io for Runtime<'_> {
            fn prepare(&mut self) -> Result<Option<Upload>, &'static str> {
                tx(self.db, |s| resume(s, 1)).map_err(|_| "prepare")
            }
            fn finish(&mut self) -> Result<ReceiptIndexCommitment, &'static str> {
                tx(self.db, |s| finish(s, 1)).map_err(|_| "finish")
            }
            fn confirm(&mut self, ordinal: u64, bytes: Vec<u8>) -> Result<(), &'static str> {
                if self.fail("confirm before") { return Err("confirm before"); }
                tx(self.db, |s| confirm(s, 1, ordinal, &bytes)).map_err(|_| "confirm")?;
                if self.fail("confirm after") { return Err("confirm after"); }
                Ok(())
            }
            async fn read(&mut self, key: &str, stage: &'static str) -> Result<Option<Vec<u8>>, &'static str> {
                if self.fail(stage) { return Err(stage); }
                if stage == "receipt index readback" && self.fail("readback missing") { return Ok(None); }
                let mut value = self.objects.get(key).cloned();
                if stage == "receipt index readback" && self.fail("readback corrupt") {
                    value.as_mut().unwrap().push(0);
                }
                if value.as_ref().is_some_and(|bytes| bytes.len() > CHUNK_BYTES) { return Err("read bound"); }
                Ok(value)
            }
            async fn put(&mut self, key: &str, bytes: Vec<u8>) -> Result<(), &'static str> {
                *self.puts += 1;
                if self.fail("put before") { return Err("put before"); }
                assert!(self.objects.insert(key.into(), bytes).is_none(), "read-before-put avoids duplicate writes");
                if self.fail("put after") { return Err("put after"); }
                Ok(())
            }
        }
        for fault in ["receipt index read", "put before", "put after", "receipt index readback", "readback missing", "readback corrupt", "confirm before", "confirm after"] {
            let db = db(CHUNK_ENTRIES * 2 + 7);
            let mut objects = std::collections::BTreeMap::new();
            let mut puts = 0;
            let first = block_on(step(&mut Runtime { db: &db, objects: &mut objects, puts: &mut puts, fault: Some(fault) }));
            assert!(first.is_err(), "fault was exercised: {fault}");
            let mut completed = None;
            // Each retry constructs a fresh runtime: only SQLite and stored
            // objects survive the failed invocation.
            for _ in 0..5 {
                if let Some(commitment) = block_on(step(&mut Runtime { db: &db, objects: &mut objects, puts: &mut puts, fault: None })).unwrap() {
                    completed = Some(commitment);
                    break;
                }
            }
            let completed = completed.expect("bounded steps finish the captured index");
            let mut expected = ReceiptIndexBuilder::new(digest(91));
            for bytes in objects.values() {
                expected.push(&ReceiptIndexChunk::from_bytes(bytes).unwrap()).unwrap();
            }
            assert_eq!(completed, expected.finish(), "{fault}");
            assert_eq!(completed.receipt_count, (CHUNK_ENTRIES * 2 + 7) as u64);
            assert_eq!(objects.len(), 3);
            assert_eq!(puts, if fault == "put before" { 4 } else { 3 }, "{fault}");
            assert_eq!(block_on(step(&mut Runtime { db: &db, objects: &mut objects, puts: &mut puts, fault: None })).unwrap(), Some(completed));
            assert_eq!(objects.len(), 3);
        }
    }

    #[test]
    fn lost_put_response_and_repeated_confirmation_resume_same_objects() {
        let db = db(CHUNK_ENTRIES * 2 + 7);
        tx(&db, |sql| start(sql, 1)).unwrap();
        let mut expected = ReceiptIndexBuilder::new(digest(91));
        let mut objects = std::collections::BTreeMap::new();
        let mut ordinal = 0;
        while let Some(upload) = tx(&db, |sql| prepare(sql, 1)).unwrap() {
            assert_eq!(upload.ordinal, ordinal);
            assert!(
                upload
                    .key
                    .starts_with("cohorts/synthetic-index-recovery/receipts/index-v2/")
            );
            // Upload succeeded, but its response was lost. No in-memory upload
            // state is needed to recover the intent or the exact key and bytes.
            objects.insert(upload.key.clone(), upload.bytes.clone());
            tx(&db, |sql| start(sql, 1)).unwrap();
            let retry = tx(&db, |sql| prepare(sql, 1)).unwrap().unwrap();
            assert_eq!(retry, upload);
            let stored = &objects[&retry.key];
            tx(&db, |sql| confirm(sql, 1, retry.ordinal, stored)).unwrap();
            tx(&db, |sql| confirm(sql, 1, retry.ordinal, stored)).unwrap();
            expected
                .push(&ReceiptIndexChunk::from_bytes(stored).unwrap())
                .unwrap();
            ordinal += 1;
        }
        let commitment = tx(&db, |sql| finish(sql, 1)).unwrap();
        assert_eq!(commitment, expected.finish());
        assert_eq!(objects.len(), 3);
        assert_eq!(tx(&db, |sql| finish(sql, 1)).unwrap(), commitment);
        assert_eq!(db.count("receipt_index_chunks"), 3);
    }

    #[test]
    fn resumed_pages_do_not_recount_all_receipts_but_finish_checks_coverage() {
        let db = db(CHUNK_ENTRIES + 1);
        let first = tx(&db, |sql| resume(sql, 1)).unwrap().unwrap();
        tx(&db, |sql| confirm(sql, 1, first.ordinal, &first.bytes)).unwrap();
        db.fail_on.set(Some("SELECT COUNT(*) FROM receipts"));
        let last = tx(&db, |sql| resume(sql, 1)).unwrap().unwrap();
        tx(&db, |sql| confirm(sql, 1, last.ordinal, &last.bytes)).unwrap();
        assert!(tx(&db, |sql| resume(sql, 1)).unwrap().is_none());
        assert!(tx(&db, |sql| finish(sql, 1)).is_err());
        db.fail_on.set(None);
        assert_eq!(
            tx(&db, |sql| finish(sql, 1)).unwrap().receipt_count,
            (CHUNK_ENTRIES + 1) as u64
        );
    }

    #[test]
    fn probe_build_binds_exact_plan_and_never_falls_back_from_bad_manifest() {
        let db = db(1);
        let mut plan = crate::eval::plan::tests::plan(20);
        plan.schema = crate::eval::plan::PROBE_PLAN_SCHEMA.into();
        let bytes = serde_json::to_string(&plan).unwrap();
        let digest = object_sha256(bytes.as_bytes());
        db.conn
            .execute(
                "UPDATE cohorts SET manifest_digest=NULL, plan=?, plan_sha256=?",
                rusqlite::params![bytes, digest.to_string()],
            )
            .unwrap();
        tx(&db, |sql| start(sql, 1)).unwrap();
        let upload = tx(&db, |sql| prepare(sql, 1)).unwrap().unwrap();
        assert_eq!(
            ReceiptIndexChunk::from_bytes(&upload.bytes)
                .unwrap()
                .cohort_manifest,
            digest
        );
        assert!(
            upload
                .key
                .starts_with("cohorts/synthetic-index-recovery/receipts/index-v2/")
        );
        tx(&db, |sql| confirm(sql, 1, 0, &upload.bytes)).unwrap();
        assert_eq!(
            tx(&db, |sql| finish(sql, 1)).unwrap().cohort_manifest,
            digest
        );
        db.conn
            .execute("UPDATE cohorts SET plan=plan || ' '", [])
            .unwrap();
        assert!(tx(&db, |sql| start(sql, 1)).is_err());
        assert!(tx(&db, |sql| finish(sql, 1)).is_err());
        db.conn
            .execute("UPDATE cohorts SET plan=?, manifest_digest='bad'", [&bytes])
            .unwrap();
        assert!(tx(&db, |sql| start(sql, 1)).is_err());
        db.conn
            .execute("UPDATE cohorts SET manifest_digest=NULL", [])
            .unwrap();
        tx(&db, |sql| start(sql, 1)).unwrap();
        plan.schema = crate::eval::plan::PLAN_SCHEMA_V3.into();
        let normal = serde_json::to_string(&plan).unwrap();
        db.conn
            .execute(
                "UPDATE cohorts SET plan=?, plan_sha256=?",
                rusqlite::params![normal, object_sha256(normal.as_bytes()).to_string()],
            )
            .unwrap();
        assert!(tx(&db, |sql| start(sql, 1)).is_err());
        assert!(tx(&db, |sql| finish(sql, 1)).is_err());
    }

    #[test]
    fn finish_crosses_digest_page_boundary_and_rejects_second_page_hole() {
        let count = CHUNK_ENTRIES * 257 + 1;
        let db = db(count);
        tx(&db, |sql| start(sql, 1)).unwrap();
        let mut expected = ReceiptIndexBuilder::new(digest(91));
        let mut ordinal = 0;
        while let Some(upload) = tx(&db, |sql| prepare(sql, 1)).unwrap() {
            assert_eq!(upload.ordinal, ordinal);
            let chunk = ReceiptIndexChunk::from_bytes(&upload.bytes).unwrap();
            assert_eq!(
                chunk.entries.len(),
                (count - ordinal as usize * CHUNK_ENTRIES).min(CHUNK_ENTRIES)
            );
            for (i, entry) in chunk.entries.iter().enumerate() {
                assert_eq!(
                    entry.attempt,
                    digest(ordinal * CHUNK_ENTRIES as u64 + i as u64)
                );
            }
            expected.push(&chunk).unwrap();
            tx(&db, |sql| confirm(sql, 1, ordinal, &upload.bytes)).unwrap();
            ordinal += 1;
        }
        assert_eq!(ordinal, 258);
        assert_eq!(tx(&db, |sql| finish(sql, 1)).unwrap(), expected.finish());
        db.conn
            .execute("DELETE FROM receipt_index_chunks WHERE ordinal=256", [])
            .unwrap();
        assert!(tx(&db, |sql| finish(sql, 1)).is_err());
    }

    #[test]
    fn transaction_failure_does_not_leave_partial_confirmation() {
        let db = db(1);
        tx(&db, |sql| start(sql, 1)).unwrap();
        let upload = tx(&db, |sql| prepare(sql, 1)).unwrap().unwrap();
        db.fail_on.set(Some("UPDATE receipt_index_builds"));
        assert!(tx(&db, |sql| confirm(sql, 1, 0, &upload.bytes)).is_err());
        db.fail_on.set(None);
        assert_eq!(tx(&db, |sql| prepare(sql, 1)).unwrap().unwrap(), upload);
        assert!(tx(&db, |sql| finish(sql, 1)).is_err());
        tx(&db, |sql| confirm(sql, 1, 0, &upload.bytes)).unwrap();
        assert_eq!(tx(&db, |sql| finish(sql, 1)).unwrap().receipt_count, 1);
    }

    #[test]
    fn invalid_or_conflicting_stored_objects_do_not_advance() {
        let db = db(2);
        tx(&db, |sql| start(sql, 1)).unwrap();
        let upload = tx(&db, |sql| prepare(sql, 1)).unwrap().unwrap();
        let mut changed = upload.bytes.clone();
        let last = changed.len() - 1;
        changed[last] ^= 1;
        for bytes in [
            &changed[..],
            &upload.bytes[..upload.bytes.len() - 1],
            &vec![0; CHUNK_BYTES + 1][..],
        ] {
            assert!(tx(&db, |sql| confirm(sql, 1, 0, bytes)).is_err());
        }
        assert!(tx(&db, |sql| confirm(sql, 1, 1, &upload.bytes)).is_err());
        assert_eq!(tx(&db, |sql| prepare(sql, 1)).unwrap().unwrap(), upload);
        db.conn
            .execute("UPDATE receipts SET object_len=154 WHERE task=0", [])
            .unwrap();
        assert!(tx(&db, |sql| prepare(sql, 1)).is_err());
        assert!(tx(&db, |sql| confirm(sql, 1, 0, &upload.bytes)).is_err());
        assert!(tx(&db, |sql| finish(sql, 1)).is_err());
    }

    #[test]
    fn empty_build_and_changed_cohort_are_distinguished() {
        let db = db(0);
        tx(&db, |sql| start(sql, 1)).unwrap();
        assert!(tx(&db, |sql| prepare(sql, 1)).unwrap().is_none());
        assert_eq!(
            tx(&db, |sql| finish(sql, 1)).unwrap(),
            ReceiptIndexBuilder::new(digest(91)).finish()
        );
        db.conn
            .execute(
                "UPDATE cohorts SET manifest_digest=?",
                [digest(93).to_string()],
            )
            .unwrap();
        assert!(tx(&db, |sql| start(sql, 1)).is_err());
        assert!(tx(&db, |sql| prepare(sql, 1)).is_err());
        assert!(tx(&db, |sql| finish(sql, 1)).is_err());
    }

    #[test]
    fn missing_unconfirmed_and_extra_ledger_records_refuse_finish() {
        for mutation in [
            "DELETE FROM receipt_index_chunks WHERE ordinal=0",
            "UPDATE receipt_index_chunks SET confirmed=0 WHERE ordinal=0",
            "UPDATE receipt_index_chunks SET digest='bad' WHERE ordinal=0",
            "INSERT INTO receipt_index_chunks SELECT seq, ordinal+1, digest, object_sha256, bytes, receipts, last_attempt, confirmed FROM receipt_index_chunks",
        ] {
            let db = db(1);
            tx(&db, |sql| start(sql, 1)).unwrap();
            let upload = tx(&db, |sql| prepare(sql, 1)).unwrap().unwrap();
            tx(&db, |sql| confirm(sql, 1, 0, &upload.bytes)).unwrap();
            db.conn.execute(mutation, []).unwrap();
            assert!(tx(&db, |sql| finish(sql, 1)).is_err(), "{mutation}");
        }
    }

    #[test]
    fn changed_receipt_count_cannot_rebind_a_started_build() {
        let db = db(2);
        tx(&db, |sql| start(sql, 1)).unwrap();
        db.conn
            .execute("DELETE FROM receipts WHERE task=1", [])
            .unwrap();
        assert!(tx(&db, |sql| start(sql, 1)).is_err());
        let upload = tx(&db, |sql| prepare(sql, 1)).unwrap().unwrap();
        tx(&db, |sql| confirm(sql, 1, 0, &upload.bytes)).unwrap();
        assert!(tx(&db, |sql| prepare(sql, 1)).is_err());
        assert!(tx(&db, |sql| finish(sql, 1)).is_err());
    }
}

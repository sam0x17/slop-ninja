//! Bounded reads for the explicitly selected chunked receipt index.
//! These reads neither select a cohort's format nor install a target.
//!
//! A caller starts from a packaging cohort, reads its immutable receipt rows
//! through the existing `(seq, attempt_id)` primary-key index, and checks the
//! completed commitment against the captured count. Stored receipt objects must
//! also have been verified by the retained-evidence path before installation.
//! A count match alone is not that verification.

use slop_ninja_protocol::receipt_chunks::{
    CHUNK_ENTRIES, ReceiptIndexChunk, ReceiptIndexCommitment,
};
use slop_ninja_protocol::{Digest, OutcomeClass, ReceiptIndexEntry, Record};

use super::engine::{EvalError, R, parse_digest};
use crate::sql::{Sql, SqlValue, int, text, uint};

const FIRST_PAGE: &str = "SELECT attempt_id, outcome, object_sha256, object_len FROM receipts
    WHERE seq = ? ORDER BY attempt_id LIMIT ?";
const NEXT_PAGE: &str = "SELECT attempt_id, outcome, object_sha256, object_len FROM receipts
    WHERE seq = ? AND attempt_id > ? ORDER BY attempt_id LIMIT ?";

/// Identity and count captured in the caller's transaction. Production callers
/// must keep their normal packaging fences; this type is not a work lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexScan {
    seq: i64,
    manifest: Digest,
    count: u64,
}

pub(super) fn canonical_digest(value: &str) -> R<Digest> {
    let digest = parse_digest(value).ok_or(EvalError::State("receipt index digest"))?;
    if digest.to_string() != value {
        return Err(EvalError::State("noncanonical receipt index digest"));
    }
    Ok(digest)
}

#[path = "receipt_index_store.rs"]
pub mod store;

#[path = "receipt_index_io.rs"]
#[cfg(any(test, target_arch = "wasm32"))]
pub(crate) mod io;

/// The existing verified job-bundle map plus bounded index discovery metadata.
/// Discovery metadata is not authority: readers verify each chunk against the
/// signed commitment before relying on its entries.
pub fn chunk_map(sql: &impl Sql, seq: i64, index: &ReceiptIndexCommitment) -> R<serde_json::Value> {
    let mut map = super::engine::receipt_chunk_map(sql, seq, index.cohort_manifest,
        index.digest().map_err(|_| EvalError::State("receipt root"))?, index.receipt_count)?;
    let rows = sql.exec("SELECT label FROM cohorts WHERE seq = ?", &[seq.into()])?;
    let label = text(rows.first().ok_or(EvalError::State("receipt index label"))?, 0)?;
    map.as_object_mut().ok_or(EvalError::State("receipt chunk map shape"))?.insert("index_chunks".into(), serde_json::json!({
        "format": "receipt-index-chunk/v1",
        "prefix": format!("{}/receipts/index-v2/", super::engine::receipt_prefix(label)),
        "count": index.chunk_count(),
        "key_suffix": "{ordinal:016x}-{sha256}.bin"
    }));
    Ok(map)
}

fn packaging_manifest(sql: &impl Sql, seq: i64) -> R<Digest> {
    let rows = sql.exec(
        "SELECT phase, manifest_digest FROM cohorts WHERE seq = ?",
        &[seq.into()],
    )?;
    let row = rows
        .first()
        .ok_or(EvalError::State("receipt index cohort missing"))?;
    if text(row, 0)? != "packaging" {
        return Err(EvalError::State("receipt index outside packaging"));
    }
    match row.get(1) {
        Some(SqlValue::Text(value)) => canonical_digest(value),
        Some(SqlValue::Null) => {
            // Probe builds have no cohort manifest. Bind only a frozen probe
            // plan whose bytes still match its stored digest; a missing or
            // malformed normal manifest must never silently select this path.
            let rows = sql.exec(
                "SELECT plan, plan_sha256 FROM cohorts WHERE seq = ?",
                &[seq.into()],
            )?;
            let row = rows
                .first()
                .ok_or(EvalError::State("receipt index probe plan"))?;
            let bytes = text(row, 0)?.as_bytes();
            let digest = canonical_digest(text(row, 1)?)?;
            if slop_ninja_protocol::object_sha256(bytes) != digest {
                return Err(EvalError::State("receipt index probe plan digest"));
            }
            let plan: super::plan::CohortPlan = serde_json::from_slice(bytes)
                .map_err(|_| EvalError::State("receipt index probe plan"))?;
            if !plan.is_probe_build() {
                return Err(EvalError::State("receipt index missing manifest"));
            }
            Ok(digest)
        }
        _ => Err(EvalError::State("receipt index manifest")),
    }
}

fn receipt_count(sql: &impl Sql, seq: i64) -> R<u64> {
    let rows = sql.exec("SELECT COUNT(*) FROM receipts WHERE seq = ?", &[seq.into()])?;
    let row = rows
        .first()
        .ok_or(EvalError::State("receipt index count missing"))?;
    Ok(uint(row, 0)?)
}

impl IndexScan {
    pub fn open(sql: &impl Sql, seq: i64) -> R<Self> {
        Ok(Self {
            seq,
            manifest: packaging_manifest(sql, seq)?,
            count: receipt_count(sql, seq)?,
        })
    }

    pub fn manifest(&self) -> Digest {
        self.manifest
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    /// Reads at most one protocol-sized chunk. Repeating an identical request
    /// is read-only and deterministic while the packaging snapshot is held.
    /// The caller advances `after` only after durably accepting the prior chunk.
    pub fn page(
        &self,
        sql: &impl Sql,
        ordinal: u64,
        after: Option<Digest>,
    ) -> R<Option<ReceiptIndexChunk>> {
        if (ordinal == 0) != after.is_none() {
            return Err(EvalError::State("receipt index cursor"));
        }
        if packaging_manifest(sql, self.seq)? != self.manifest {
            return Err(EvalError::State("receipt index manifest changed"));
        }
        let limit = SqlValue::Integer(CHUNK_ENTRIES as i64);
        let rows = match after {
            Some(after) => sql.exec(
                NEXT_PAGE,
                &[self.seq.into(), after.to_string().into(), limit],
            )?,
            None => sql.exec(FIRST_PAGE, &[self.seq.into(), limit])?,
        };
        if rows.len() > CHUNK_ENTRIES {
            return Err(EvalError::State("receipt index page bound"));
        }
        if rows.is_empty() {
            return Ok(None);
        }
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let attempt = canonical_digest(text(&row, 0)?)?;
            if after.is_some_and(|after| attempt <= after) {
                return Err(EvalError::State("receipt index cursor order"));
            }
            let outcome = match int(&row, 1)? {
                0 => OutcomeClass::InfrastructureError,
                1 => OutcomeClass::Prediction,
                2 => OutcomeClass::ModelError,
                3 => OutcomeClass::InvalidOutput,
                4 => OutcomeClass::ParseFailure,
                _ => return Err(EvalError::State("receipt index outcome")),
            };
            let object_len = u32::try_from(uint(&row, 3)?)
                .map_err(|_| EvalError::State("receipt index object length"))?;
            if object_len == 0 {
                return Err(EvalError::State("receipt index object length"));
            }
            entries.push(ReceiptIndexEntry {
                attempt,
                outcome,
                object_sha256: canonical_digest(text(&row, 2)?)?,
                object_len,
            });
        }
        let chunk = ReceiptIndexChunk {
            cohort_manifest: self.manifest,
            ordinal,
            entries,
        };
        chunk
            .validate()
            .map_err(|_| EvalError::State("receipt index page order"))?;
        Ok(Some(chunk))
    }

    /// Checks complete index coverage and that the cohort still owns this
    /// packaging snapshot. This does not verify stored object availability.
    pub fn check_complete(&self, sql: &impl Sql, commitment: &ReceiptIndexCommitment) -> R<()> {
        commitment
            .validate()
            .map_err(|_| EvalError::State("receipt index commitment"))?;
        if packaging_manifest(sql, self.seq)? != self.manifest
            || commitment.cohort_manifest != self.manifest
            || commitment.receipt_count != self.count
            || receipt_count(sql, self.seq)? != self.count
        {
            return Err(EvalError::State("receipt index snapshot coverage"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Sqlite;
    use slop_ninja_protocol::receipt_chunks::ReceiptIndexBuilder;

    fn digest(n: u64) -> Digest {
        let mut d = [0; 32];
        d[..8].copy_from_slice(&n.to_be_bytes());
        Digest(d)
    }

    fn database() -> Sqlite {
        let db = Sqlite::open();
        let schema = super::super::engine::SCHEMA_V2
            .iter()
            .find(|s| s.starts_with("CREATE TABLE receipts ("))
            .unwrap();
        db.conn.execute_batch(schema).unwrap();
        db.conn.execute_batch("CREATE TABLE cohorts (seq INTEGER PRIMARY KEY, phase TEXT NOT NULL, manifest_digest TEXT NOT NULL) STRICT").unwrap();
        for seq in [1, 2] {
            db.conn
                .execute(
                    "INSERT INTO cohorts VALUES (?, 'packaging', ?)",
                    rusqlite::params![seq, digest(seq).to_string()],
                )
                .unwrap();
        }
        db
    }

    fn insert(db: &Sqlite, seq: u64, n: u64) {
        db.conn
            .execute(
                "INSERT INTO receipts VALUES (?, ?, 0, ?, 1, ?, ?, 153, 'receipt-object')",
                rusqlite::params![
                    seq,
                    digest(n).to_string(),
                    n,
                    n % 5,
                    digest(999).to_string()
                ],
            )
            .unwrap();
    }

    fn assemble(db: &Sqlite, scan: &IndexScan) -> ReceiptIndexCommitment {
        let mut b = ReceiptIndexBuilder::new(scan.manifest());
        let mut after = None;
        let mut ordinal = 0;
        while let Some(chunk) = scan.page(db, ordinal, after).unwrap() {
            assert!(chunk.entries.len() <= CHUNK_ENTRIES);
            // Reconciliation after a lost response returns identical bytes.
            assert_eq!(scan.page(db, ordinal, after).unwrap().unwrap(), chunk);
            after = chunk.entries.last().map(|e| e.attempt);
            b.push(&chunk).unwrap();
            ordinal += 1;
        }
        b.finish()
    }

    #[test]
    fn keyset_pages_cover_sorted_receipts_and_isolate_cohorts() {
        let db = database();
        let count = CHUNK_ENTRIES as u64 * 2 + 13;
        for n in (0..count).rev() {
            insert(&db, 1, n);
        }
        for n in 0..3 {
            insert(&db, 2, n);
        }
        let scan = IndexScan::open(&db, 1).unwrap();
        let result = assemble(&db, &scan);
        assert_eq!(result.receipt_count, count);
        scan.check_complete(&db, &result).unwrap();
        let other = IndexScan::open(&db, 2).unwrap();
        assert_eq!(other.count(), 3);
        assert!(other.check_complete(&db, &result).is_err());
        assert!(scan.page(&db, 1, None).is_err());
        assert!(scan.page(&db, 0, Some(digest(0))).is_err());
        assert!(
            db.conn
                .execute(
                    "INSERT INTO receipts SELECT * FROM receipts WHERE seq=1 LIMIT 1",
                    []
                )
                .is_err()
        );
    }

    #[test]
    fn partial_and_changed_snapshots_are_refused() {
        let db = database();
        insert(&db, 1, 0);
        let scan = IndexScan::open(&db, 1).unwrap();
        assert!(
            scan.check_complete(&db, &ReceiptIndexBuilder::new(scan.manifest()).finish())
                .is_err()
        );
        let result = assemble(&db, &scan);
        insert(&db, 1, 1);
        assert!(scan.check_complete(&db, &result).is_err());
        db.conn
            .execute("DELETE FROM receipts WHERE seq=1 AND task=1", [])
            .unwrap();
        scan.check_complete(&db, &result).unwrap();
        db.conn
            .execute("UPDATE cohorts SET phase='closed' WHERE seq=1", [])
            .unwrap();
        assert!(scan.page(&db, 0, None).is_err());
        assert!(scan.check_complete(&db, &result).is_err());
        assert!(IndexScan::open(&db, 1).is_err());
        db.conn
            .execute(
                "UPDATE cohorts SET phase='packaging', manifest_digest=? WHERE seq=1",
                [digest(9).to_string()],
            )
            .unwrap();
        assert!(scan.page(&db, 0, None).is_err());
        assert!(scan.check_complete(&db, &result).is_err());
    }

    #[test]
    fn malformed_rows_are_not_normalized_into_valid_receipts() {
        for update in [
            "UPDATE receipts SET outcome=5",
            "UPDATE receipts SET outcome=-1",
            "UPDATE receipts SET object_len=-1",
            "UPDATE receipts SET object_len=0",
            "UPDATE receipts SET object_len=4294967296",
            "UPDATE receipts SET object_sha256='bad'",
            "UPDATE receipts SET attempt_id=''",
            "UPDATE receipts SET attempt_id=upper(attempt_id)",
        ] {
            let db = database();
            insert(&db, 1, 15);
            let scan = IndexScan::open(&db, 1).unwrap();
            db.conn.execute(update, []).unwrap();
            assert!(scan.page(&db, 0, None).is_err(), "{update}");
        }
    }

    #[test]
    fn empty_snapshot_and_exact_chunk_boundary_complete() {
        let db = database();
        let empty = IndexScan::open(&db, 1).unwrap();
        empty.check_complete(&db, &assemble(&db, &empty)).unwrap();
        assert!(IndexScan::open(&db, 3).is_err());
        for n in 0..CHUNK_ENTRIES as u64 {
            insert(&db, 1, n);
        }
        let full = IndexScan::open(&db, 1).unwrap();
        full.check_complete(&db, &assemble(&db, &full)).unwrap();
    }

    #[test]
    fn query_uses_primary_key_range_without_sorting_all_receipts() {
        let db = database();
        for (query, params) in [
            (FIRST_PAGE, vec![1.into(), (CHUNK_ENTRIES as i64).into()]),
            (
                NEXT_PAGE,
                vec![
                    1.into(),
                    digest(0).to_string().into(),
                    (CHUNK_ENTRIES as i64).into(),
                ],
            ),
        ] {
            let plan = db
                .exec(&format!("EXPLAIN QUERY PLAN {query}"), &params)
                .unwrap();
            let detail: Vec<_> = plan.iter().map(|r| text(r, 3).unwrap()).collect();
            assert!(
                detail
                    .iter()
                    .any(|d| d.contains("SEARCH receipts USING INDEX sqlite_autoindex_receipts_1"))
            );
            assert!(
                detail
                    .iter()
                    .all(|d| !d.contains("TEMP B-TREE") && !d.starts_with("SCAN"))
            );
        }
    }
}

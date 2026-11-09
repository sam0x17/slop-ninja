//! P1-F: a store whose migration fails is frozen at its stored version and
//! read-only. Nothing here migrates or writes it beyond the one write the
//! deployed successor's N+2 sweep needs.
//!
//! A frozen store serves only statements valid at every stored version from
//! v2 on: `cohorts(seq, label, cohort, phase, target_digest)`, `receipts` and
//! `receipt_objects`. The optional columns (`closure_digest` from v8,
//! `receipts_deleted_ms` from v13) and the `authority_succession` table (from
//! v12) are read only where the store's own `PRAGMA table_info` / schema shows
//! them. A migration runs every step in one transaction, so a failed one
//! leaves the store exactly at its stored version.
//!
//! - [`status`] (`GET /frozen/status`, and the deployed store's `/status`):
//!   the stored version, the frozen reason, the succession row if present,
//!   and the cohort list.
//! - [`report`] (the publisher's report route): `{known, frozen: true}`,
//!   recording nothing; `known` only for a label in the store's own table in
//!   phase `closed` or `abandoned`.
//! - [`purge_receipts`] (the successor's sweep, journaled in the successor):
//!   deletes one cohort's `receipts` and `receipt_objects` rows by `seq`,
//!   once the successor has deleted that cohort's detailed receipt objects.
//!
//! Every other request gets `store_frozen`, and alarms do nothing.

use serde_json::{Value, json};

use crate::sql::{Sql, SqlValue, StoreError, int, text};

fn columns(sql: &impl Sql, table: &str) -> Result<Vec<String>, StoreError> {
    sql.exec(&format!("PRAGMA table_info({table})"), &[])?
        .iter()
        .map(|r| text(r, 1).map(str::to_owned))
        .collect()
}

fn has_table(sql: &impl Sql, table: &str) -> Result<bool, StoreError> {
    Ok(!sql.exec("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?", &[table.into()])?.is_empty())
}

fn opt(r: &[SqlValue], i: usize) -> Value {
    match r.get(i) {
        Some(SqlValue::Integer(n)) => json!(n),
        Some(SqlValue::Text(t)) => json!(t),
        _ => Value::Null,
    }
}

/// The frozen store's status: its stored version, why it is frozen, its
/// succession row (if its version has the table) and its cohorts.
pub fn status(sql: &impl Sql, reason: &str) -> Result<Value, StoreError> {
    let (_, version) = crate::sql::stored_version(sql)?;
    let cols = columns(sql, "cohorts")?;
    let optional: Vec<&str> = ["closure_digest", "receipts_deleted_ms"]
        .into_iter()
        .filter(|c| cols.iter().any(|x| x == c))
        .collect();
    let select = format!(
        "SELECT seq, label, cohort, phase, target_digest{} FROM cohorts ORDER BY seq",
        optional.iter().map(|c| format!(", {c}")).collect::<String>()
    );
    let cohorts: Vec<Value> = sql
        .exec(&select, &[])?
        .iter()
        .map(|r| {
            let mut v = json!({"seq": opt(r, 0), "label": opt(r, 1), "cohort": opt(r, 2), "phase": opt(r, 3),
                "target_digest": opt(r, 4)});
            for (i, c) in optional.iter().enumerate() {
                v[*c] = opt(r, 5 + i);
            }
            v
        })
        .collect();
    let succession = if has_table(sql, "authority_succession")? {
        sql.exec("SELECT * FROM authority_succession LIMIT 1", &[])?
            .first()
            .map(|r| Value::Array(r.iter().enumerate().map(|(i, _)| opt(r, i)).collect()))
    } else {
        None
    };
    Ok(json!({"frozen": true, "reason": reason, "schema_version": version, "succession": succession, "cohorts": cohorts}))
}

/// The publisher's report on a frozen store's cohort: recorded nowhere;
/// `known` only for its own ended label.
pub fn report(sql: &impl Sql, cohort: u64, label: &str) -> Result<Value, StoreError> {
    let known = !sql
        .exec(
            "SELECT 1 FROM cohorts WHERE label = ? AND cohort = ? AND phase IN ('closed', 'abandoned')",
            &[label.into(), SqlValue::Integer(cohort as i64)],
        )?
        .is_empty();
    let nothing = 0u64;
    Ok(json!({"recorded": nothing, "held": nothing, "known": known, "frozen": true}))
}

/// The detailed receipt rows of cohort `seq` (its own ended cohort only),
/// deleted once the successor deleted their objects. Returns the rows
/// deleted (`receipts`, `receipt_objects`); a cohort that is not its own or
/// not ended deletes nothing.
pub fn purge_receipts(sql: &impl Sql, seq: i64) -> Result<(u64, u64), StoreError> {
    let ended = sql.exec("SELECT phase FROM cohorts WHERE seq = ?", &[SqlValue::Integer(seq)])?;
    if !ended.first().is_some_and(|r| matches!(text(r, 0), Ok("closed" | "abandoned"))) {
        return Ok((0, 0));
    }
    let n = |table: &str| -> Result<u64, StoreError> {
        let c = sql.exec(&format!("SELECT COUNT(*) FROM {table} WHERE seq = ?"), &[SqlValue::Integer(seq)])?;
        sql.exec(&format!("DELETE FROM {table} WHERE seq = ?"), &[SqlValue::Integer(seq)])?;
        Ok(int(&c[0], 0)? as u64)
    };
    Ok((n("receipts")?, n("receipt_objects")?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::{EVAL_COHORT_SCHEMA, Migration, Schema, migrate};
    use crate::testing::{Sqlite, tx};

    /// A store at v2 only (the oldest any frozen store can be), with one
    /// closed and one open cohort and their receipt rows.
    fn v2_store() -> Sqlite {
        const V2: Schema = Schema {
            family: "eval_cohort",
            migrations: {
                const M: &[Migration] = EVAL_COHORT_SCHEMA.migrations;
                let (v2, _) = M.split_at(2);
                v2
            },
        };
        let db = Sqlite::open();
        tx(&db, |s| migrate(s, &V2)).unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO cohorts (seq, label, cohort, plan, plan_sha256, phase, phase_since_ms, queued_ms)
                    VALUES (1, 'old-1', 1, '{}', 'x', 'closed', 0, 0), (2, 'old-2', 2, '{}', 'y', 'scoring', 0, 0);
                 INSERT INTO receipts VALUES (1, 'a', 0, 0, 1, 0, 'h', 1, 'k1'), (2, 'b', 0, 0, 1, 0, 'h', 1, 'k2');
                 INSERT INTO receipt_objects VALUES (1, 'k1', 'h', 1, 1), (2, 'k2', 'h', 1, 1);",
            )
            .unwrap();
        db
    }

    /// P1-F: a frozen store answers status from its own v2-stable columns,
    /// reports `known` only for its own ended labels and records nothing, and
    /// purges receipt rows only of its own ended cohorts.
    #[test]
    fn a_frozen_store_reads_its_own_stable_columns_and_purges_only_ended_cohorts() {
        let db = v2_store();
        let v = tx(&db, |s| status(s, "migration_failed")).unwrap();
        assert_eq!((v["schema_version"].clone(), v["reason"].clone()), (json!(2), json!("migration_failed")));
        assert_eq!(v["cohorts"].as_array().unwrap().len(), 2);
        assert!(v["cohorts"][0].get("closure_digest").is_none(), "v2 has no closure_digest column");
        assert_eq!(v["succession"], Value::Null);
        assert_eq!(tx(&db, |s| report(s, 1, "old-1")).unwrap()["known"], json!(true));
        assert_eq!(tx(&db, |s| report(s, 2, "old-2")).unwrap()["known"], json!(false), "not ended");
        assert_eq!(tx(&db, |s| report(s, 9, "load-9")).unwrap()["known"], json!(false), "not its own");
        assert_eq!(tx(&db, |s| purge_receipts(s, 2)).unwrap(), (0, 0), "an open cohort keeps its rows");
        assert_eq!(tx(&db, |s| purge_receipts(s, 1)).unwrap(), (1, 1));
        assert_eq!(db.count("receipts"), 1);
        // The full schema's store reads the same way, with its optional columns.
        let full = Sqlite::open();
        tx(&full, |s| migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
        let v = tx(&full, |s| status(s, "x")).unwrap();
        assert_eq!(v["schema_version"], json!(EVAL_COHORT_SCHEMA.latest()));
    }
}

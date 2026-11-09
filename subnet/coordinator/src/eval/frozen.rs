//! P1-F: a store whose migration fails is frozen at its stored version and
//! read-only. Nothing here migrates or writes it beyond the one write the
//! deployed successor's N+2 sweep needs.
//!
//! Only a failed migration (`migration_failed`) of a main-instance store
//! that is not the deployed authority freezes ([`mode`]). The deployed store
//! never freezes: its guard refused the build, so it serves its status read
//! only and refuses everything else with the migration code (503), as does a
//! store a later build migrated (`stored_schema_newer`, a rollback) or one of
//! another family (`schema_family`). The publisher therefore keeps retrying
//! a report such a store cannot record.
//!
//! A frozen store serves only statements valid at every stored version from
//! v2 on: `cohorts(seq, label, cohort, phase, target_digest)`, `receipts` and
//! `receipt_objects`. The optional columns (`closure_digest` from v8,
//! `receipts_deleted_ms` from v13) and the `authority_succession` table (from
//! v12) are read only where the store's own `PRAGMA table_info` / schema shows
//! them. A migration runs every step in one transaction, so a failed one
//! leaves the store exactly at its stored version.
//!
//! - [`status`] (`GET /frozen/status` and `/status`, in either mode): the
//!   stored version, the refusal reason, the succession row if present, and
//!   the cohort list.
//! - [`report`] (the publisher's report route, frozen stores only): `{known,
//!   frozen: true}`, recording nothing; `known` only for a label in the
//!   store's own table in phase `closed` or `abandoned`. Any other label is
//!   `cohort_unknown` (404), as on the live route, so the publisher never
//!   takes it as recorded.
//! - [`purge_receipts`] (the successor's sweep, journaled in the successor,
//!   frozen stores only): deletes one cohort's `receipts` and
//!   `receipt_objects` rows by `seq`, once the successor has deleted that
//!   cohort's detailed receipt objects.
//!
//! Every other request to a frozen store gets `store_frozen`, and alarms do
//! nothing. [`serve`] is the route boundary both modes answer through.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::sql::{Sql, SqlValue, StoreError, int, text};

/// What a store whose migration was refused serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Frozen at its stored version: its status, the publisher's report
    /// (taken, recorded nowhere) and the successor sweep's purge.
    Frozen,
    /// Its status only; every other request is refused with the migration
    /// code (503), so callers retry.
    StatusOnly,
}

/// The mode of a store whose migration answered `reason`, from the identity
/// it stored (`instance`, `authority`; an object that stores an instance and
/// no authority is the original authority's) and the main authority the
/// deployment names (`deployed`, `None` when its vars are malformed). Only a
/// failed migration of a main-instance store whose authority is not the
/// deployed one freezes; every other case serves its status only.
pub fn mode(reason: &str, instance: Option<&str>, authority: Option<&str>, deployed: Option<&str>) -> Mode {
    let own = authority.or(instance.map(|_| crate::config::ORIGINAL_AUTHORITY));
    match (reason, instance, own, deployed) {
        ("migration_failed", Some(crate::eval::engine::INSTANCE_MAIN), Some(own), Some(deployed)) if own != deployed => {
            Mode::Frozen
        }
        _ => Mode::StatusOnly,
    }
}

/// One request's answer at the route boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    /// 200 with this JSON body.
    Json(Value),
    /// The refusal code and HTTP status.
    Refused(&'static str, u16),
}

/// The publisher's report: its cohort and label (the entrants are not read).
#[derive(Deserialize)]
struct Report {
    cohort: u64,
    label: String,
}

/// The successor sweep's purge of one cohort's receipt rows.
#[derive(Deserialize)]
struct Purge {
    seq: i64,
}

/// The routes a store in `mode` answers (`method` `GET` or `POST`, `body`
/// the request bytes read under the route's own bound): its status in
/// either mode; in a frozen store the publisher's report (404
/// `cohort_unknown` unless it is the store's own ended cohort) and the
/// purge, and `store_frozen` (409) for anything else; in a status-only
/// store the migration code `reason` (503) for anything else.
pub fn serve(sql: &impl Sql, mode: Mode, reason: &'static str, method: &str, path: &str, body: &[u8]) -> Result<Reply, StoreError> {
    match (mode, method, path) {
        (_, "GET", "/status" | "/frozen/status") => Ok(Reply::Json(status(sql, reason, mode)?)),
        (Mode::StatusOnly, _, _) => Ok(Reply::Refused(reason, 503)),
        (Mode::Frozen, "POST", "/admission/published") => {
            let Ok(r) = serde_json::from_slice::<Report>(body) else {
                return Ok(Reply::Refused("bad_request", 400));
            };
            let v = report(sql, r.cohort, &r.label)?;
            // Not this store's ended cohort: never a 200, so the publisher
            // keeps the report pending instead of taking it as recorded.
            if v["known"] != json!(true) {
                return Ok(Reply::Refused("cohort_unknown", 404));
            }
            Ok(Reply::Json(v))
        }
        (Mode::Frozen, "POST", "/frozen/purge") => {
            let Ok(p) = serde_json::from_slice::<Purge>(body) else {
                return Ok(Reply::Refused("bad_request", 400));
            };
            let (receipts, objects) = purge_receipts(sql, p.seq)?;
            Ok(Reply::Json(json!({"receipts": receipts, "receipt_objects": objects})))
        }
        (Mode::Frozen, _, _) => Ok(Reply::Refused("store_frozen", 409)),
    }
}

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

/// A refused store's status: its stored version, why its migration was
/// refused, whether it is frozen (`mode`), its succession row (if its
/// version has the table) and its cohorts.
pub fn status(sql: &impl Sql, reason: &str, mode: Mode) -> Result<Value, StoreError> {
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
    let serves = match mode {
        Mode::Frozen => "frozen",
        Mode::StatusOnly => "status_only",
    };
    Ok(json!({"frozen": mode == Mode::Frozen, "serves": serves, "reason": reason, "schema_version": version,
        "succession": succession, "cohorts": cohorts}))
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
        let v = tx(&db, |s| status(s, "migration_failed", Mode::Frozen)).unwrap();
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
        let v = tx(&full, |s| status(s, "x", Mode::StatusOnly)).unwrap();
        assert_eq!(v["schema_version"], json!(EVAL_COHORT_SCHEMA.latest()));
        assert_eq!((v["frozen"].clone(), v["serves"].clone()), (json!(false), json!("status_only")));
    }

    /// Only a failed migration of a main store that is not the
    /// deployed authority freezes. The deployed store, the load instance, a
    /// store with no identity or under a deployment that names no authority,
    /// and a store refused as newer (a rollback) or of another family serve
    /// their status only.
    #[test]
    fn only_a_non_deployed_main_store_whose_migration_failed_freezes() {
        use Mode::{Frozen, StatusOnly};
        assert_eq!(mode("migration_failed", Some("main"), Some("r1"), Some("r2")), Frozen);
        assert_eq!(mode("migration_failed", Some("main"), None, Some("r2")), Frozen, "the original authority's object");
        assert_eq!(mode("migration_failed", Some("main"), Some("r2"), Some("r2")), StatusOnly, "the deployed authority");
        assert_eq!(mode("migration_failed", Some("main"), None, Some("main")), StatusOnly, "the deployed original");
        assert_eq!(mode("migration_failed", Some("main"), Some("r1"), None), StatusOnly, "the deployment names none");
        assert_eq!(mode("migration_failed", Some("load"), None, Some("r2")), StatusOnly, "the load instance");
        assert_eq!(mode("migration_failed", None, None, Some("r2")), StatusOnly, "no identity");
        assert_eq!(mode("stored_schema_newer", Some("main"), Some("r1"), Some("r2")), StatusOnly, "a rollback");
        assert_eq!(mode("schema_family", Some("main"), Some("r1"), Some("r2")), StatusOnly);
    }

    /// At the route boundary: a status-only store (the deployed one)
    /// answers its status and refuses the publisher's report and the purge
    /// with the migration code (503: the publisher retries) and changes
    /// nothing; a frozen store takes a report only for its own ended cohort
    /// and answers 404 `cohort_unknown` otherwise, as the live route does.
    #[test]
    fn a_report_is_taken_only_by_a_frozen_store_for_its_own_ended_cohort() {
        let db = v2_store();
        let report = |cohort: u64, label: &str| serde_json::to_vec(&json!({"cohort": cohort, "label": label, "entrants": []})).unwrap();
        let ask = |mode: Mode, reason: &'static str, method: &str, path: &str, body: &[u8]| {
            tx(&db, |s| serve(s, mode, reason, method, path, body)).unwrap()
        };
        // The deployed store (and a rollback over a newer schema).
        for reason in ["migration_failed", "stored_schema_newer"] {
            match ask(Mode::StatusOnly, reason, "GET", "/status", b"") {
                Reply::Json(v) => assert_eq!((v["frozen"].clone(), v["reason"].clone()), (json!(false), json!(reason))),
                other => panic!("{other:?}"),
            }
            assert_eq!(ask(Mode::StatusOnly, reason, "POST", "/admission/published", &report(1, "old-1")), Reply::Refused(reason, 503));
            assert_eq!(ask(Mode::StatusOnly, reason, "POST", "/frozen/purge", br#"{"seq":1}"#), Reply::Refused(reason, 503));
            assert_eq!(ask(Mode::StatusOnly, reason, "POST", "/kick", b"{}"), Reply::Refused(reason, 503));
        }
        assert_eq!(db.count("receipts"), 2, "a status-only store purges nothing");
        // A frozen store: its own ended cohort is taken, recording nothing.
        match ask(Mode::Frozen, "migration_failed", "POST", "/admission/published", &report(1, "old-1")) {
            Reply::Json(v) => assert_eq!((v["known"].clone(), v["frozen"].clone()), (json!(true), json!(true))),
            other => panic!("{other:?}"),
        }
        // Not ended, or not its own: never a 200.
        assert_eq!(ask(Mode::Frozen, "migration_failed", "POST", "/admission/published", &report(2, "old-2")), Reply::Refused("cohort_unknown", 404));
        assert_eq!(ask(Mode::Frozen, "migration_failed", "POST", "/admission/published", &report(9, "load-9")), Reply::Refused("cohort_unknown", 404));
        assert_eq!(ask(Mode::Frozen, "migration_failed", "POST", "/admission/published", b"{}"), Reply::Refused("bad_request", 400));
        assert_eq!(ask(Mode::Frozen, "migration_failed", "POST", "/kick", b"{}"), Reply::Refused("store_frozen", 409));
        assert_eq!(
            ask(Mode::Frozen, "migration_failed", "POST", "/frozen/purge", br#"{"seq":1}"#),
            Reply::Json(json!({"receipts": 1, "receipt_objects": 1}))
        );
        match ask(Mode::Frozen, "migration_failed", "GET", "/frozen/status", b"") {
            Reply::Json(v) => assert_eq!((v["frozen"].clone(), v["serves"].clone()), (json!(true), json!("frozen"))),
            other => panic!("{other:?}"),
        }
    }
}

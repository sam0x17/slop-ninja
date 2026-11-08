//! The subnet's registered UIDs as the latest signing registration read saw
//! them (`chain_journal.registry` on `/status`, rule [`RULE`]).
//!
//! Both registration reads of the signer already fetch `Keys` and
//! `BlockAtRegistration` for every UID below `SubnetworkN` at one finalized
//! block: the coordinator's own commitment ([`super::apply_registration`])
//! and the row snapshot step other validators copy
//! ([`super::rows::apply_registration`], once a tempo in every signer
//! state). Each successful read is kept here (journal schema v14, one row,
//! overwritten by any read at the same or a later block) instead of being
//! dropped after the row is derived, so the publisher can list every
//! registered miner (`dashboard/v1/registry.json`) with no new RPC call.
//!
//! The values are public chain facts: UID, hotkey and registration block.
//! Freshness follows the reads (one or two a tempo); a signer that reads
//! nothing (no reward-v2 target installed) leaves the last read visible
//! with its own `epoch` and `block`. More UIDs than [`ROWS`] are reported
//! whole or not at all (`rows: null`, `refused: "over_bound"`), as the eval
//! Worker's UID map is.

use crate::sql::{Sql, SqlValue, StoreError, bind_u64, uint};
use serde_json::{Value, json};

/// The rule of the status's `registry`.
pub const RULE: &str = "slop_ninja.coordinator.registry/v1";

/// The most rows reported: the chain's initial `MaxAllowedUids` (256 in the
/// subtensor runtime the project pins; the testnet subnet's recorded value),
/// the eval Worker's UID map bound and the dashboard contract's
/// `registry::ROWS`. A read of more UIDs stores no rows.
pub const ROWS: usize = 256;

/// `(uid, hotkey, registration block)` of each UID the read covered, in
/// ascending UID order.
pub type Neurons = [(u16, [u8; 32], u64)];

/// Keeps one successful registration read of `population` UIDs at
/// confirmed block `block` (commit epoch `epoch`). An earlier block never
/// replaces a later read.
pub fn save(
    sql: &impl Sql,
    epoch: u64,
    block: u64,
    population: u16,
    neurons: &Neurons,
    now_ms: u64,
) -> Result<(), StoreError> {
    let rows = if usize::from(population) > ROWS || neurons.len() > ROWS {
        SqlValue::Null
    } else {
        let rows: Vec<Value> = neurons
            .iter()
            .map(|(uid, hotkey, reg)| json!([uid, hex::encode(hotkey), reg]))
            .collect();
        serde_json::to_string(&rows)
            .map_err(|_| StoreError("registry json"))?
            .into()
    };
    sql.exec(
        "INSERT INTO sign_registry (id, epoch, block_number, population, rows_json, updated_at_ms)
         VALUES (1, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET epoch = excluded.epoch, block_number = excluded.block_number,
            population = excluded.population, rows_json = excluded.rows_json,
            updated_at_ms = excluded.updated_at_ms
         WHERE excluded.block_number >= sign_registry.block_number",
        &[
            bind_u64(epoch)?,
            bind_u64(block)?,
            SqlValue::Integer(population.into()),
            rows,
            bind_u64(now_ms)?,
        ],
    )?;
    Ok(())
}

/// The status's `registry`: `null` before any read; else `{rule, epoch,
/// block, rows: [{uid, hotkey (64 lowercase hex), registration_block}]}`
/// in ascending UID order, or `rows: null` with `refused` (`over_bound`
/// with the UID count, or `invalid_store` for a row this build cannot
/// read back). Never fails the status that carries it.
pub fn view(sql: &impl Sql) -> Result<Value, StoreError> {
    let found = sql.exec(
        "SELECT epoch, block_number, population, rows_json FROM sign_registry WHERE id = 1",
        &[],
    )?;
    let Some(r) = found.first() else {
        return Ok(Value::Null);
    };
    let (epoch, block, population) = (uint(r, 0)?, uint(r, 1)?, uint(r, 2)?);
    let refused = |why: &str| json!({"rule": RULE, "epoch": epoch, "block": block, "rows": null, "refused": why, "uids": population});
    let text = match r.get(3) {
        Some(SqlValue::Text(t)) => t,
        _ if population > ROWS as u64 => return Ok(refused("over_bound")),
        _ => return Ok(refused("invalid_store")),
    };
    let Some(rows) = parse_rows(text, block) else {
        return Ok(refused("invalid_store"));
    };
    Ok(json!({"rule": RULE, "epoch": epoch, "block": block, "rows": rows}))
}

/// The stored rows, only when every one is well formed: strictly ascending
/// u16 UIDs, 64-hex hotkeys and registrations at or before `block`.
fn parse_rows(text: &str, block: u64) -> Option<Vec<Value>> {
    let stored: Vec<(u16, String, u64)> = serde_json::from_str(text).ok()?;
    if stored.len() > ROWS {
        return None;
    }
    let mut out = Vec::with_capacity(stored.len());
    let mut last: Option<u16> = None;
    for (uid, hotkey, reg) in stored {
        let hex_ok = hotkey.len() == 64 && hotkey.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !hex_ok || last.is_some_and(|l| l >= uid) || reg > block {
            return None;
        }
        last = Some(uid);
        out.push(json!({"uid": uid, "hotkey": hotkey, "registration_block": reg}));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::{CHAIN_JOURNAL_SCHEMA, migrate};
    use crate::testing::{Sqlite, tx};

    fn db() -> Sqlite {
        let db = Sqlite::open();
        tx(&db, |s| migrate(s, &CHAIN_JOURNAL_SCHEMA)).unwrap();
        db
    }

    fn key(uid: u16) -> [u8; 32] {
        let mut k = [0x5a; 32];
        k[..2].copy_from_slice(&uid.to_be_bytes());
        k
    }

    #[test]
    fn null_before_any_read_then_the_latest_read_in_uid_order() {
        let db = db();
        assert_eq!(tx(&db, view).unwrap(), Value::Null);
        let n = [(0, key(0), 10), (1, key(1), 900), (2, key(2), 12)];
        tx(&db, |s| save(s, 30, 1_000, 3, &n, 5)).unwrap();
        assert_eq!(
            tx(&db, view).unwrap(),
            json!({"rule": RULE, "epoch": 30, "block": 1_000, "rows": [
                {"uid": 0, "hotkey": hex::encode(key(0)), "registration_block": 10},
                {"uid": 1, "hotkey": hex::encode(key(1)), "registration_block": 900},
                {"uid": 2, "hotkey": hex::encode(key(2)), "registration_block": 12}]})
        );
        // A later read replaces it; an earlier one (the other read path of
        // the same tempo, or a delayed one) never does.
        let later = [(0, key(9), 1_050), (1, key(1), 900)];
        tx(&db, |s| save(s, 31, 1_360, 2, &later, 6)).unwrap();
        tx(&db, |s| save(s, 30, 1_002, 3, &n, 7)).unwrap();
        let v = tx(&db, view).unwrap();
        assert_eq!((v["epoch"].clone(), v["block"].clone()), (json!(31), json!(1_360)));
        assert_eq!(v["rows"][0]["hotkey"], hex::encode(key(9)));
        assert_eq!(v["rows"].as_array().map(Vec::len), Some(2));
        // The same block again (a retried read) is taken.
        tx(&db, |s| save(s, 31, 1_360, 1, &later[..1], 8)).unwrap();
        assert_eq!(tx(&db, view).unwrap()["rows"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn a_read_over_the_bound_is_reported_whole_or_not_at_all() {
        let db = db();
        let full: Vec<(u16, [u8; 32], u64)> = (0..ROWS as u16).map(|u| (u, key(u), u64::from(u))).collect();
        tx(&db, |s| save(s, 1, 400, ROWS as u16, &full, 1)).unwrap();
        let v = tx(&db, view).unwrap();
        assert_eq!(v["rows"].as_array().map(Vec::len), Some(ROWS));
        let text = serde_json::to_string(&v).unwrap();
        assert!(text.len() < 32 * 1024, "{} bytes: a full registry stays small beside the status", text.len());
        let over: Vec<(u16, [u8; 32], u64)> = (0..=ROWS as u16).map(|u| (u, key(u), 1)).collect();
        tx(&db, |s| save(s, 2, 760, ROWS as u16 + 1, &over, 2)).unwrap();
        assert_eq!(
            tx(&db, view).unwrap(),
            json!({"rule": RULE, "epoch": 2, "block": 760, "rows": null, "refused": "over_bound", "uids": ROWS + 1})
        );
    }

    #[test]
    fn a_stored_row_this_build_cannot_read_back_refuses_the_rows_not_the_status() {
        let db = db();
        tx(&db, |s| save(s, 3, 500, 2, &[(0, key(0), 1), (1, key(1), 2)], 1)).unwrap();
        for bad in [
            r#"[[1,"aa",1],[0,"bb",1]]"#.to_owned(),
            format!(r#"[[1,"{}",1],[1,"{}",1]]"#, hex::encode(key(0)), hex::encode(key(1))),
            format!(r#"[[0,"{}",501]]"#, hex::encode(key(0))),
            format!(r#"[[0,"{}",1]]"#, hex::encode(key(0)).to_uppercase()),
            "not json".to_owned(),
        ] {
            tx(&db, |s| s.exec("UPDATE sign_registry SET rows_json = ? WHERE id = 1", &[bad.clone().into()])).unwrap();
            let v = tx(&db, view).unwrap();
            assert_eq!((v["rows"].clone(), v["refused"].clone()), (Value::Null, json!("invalid_store")), "{bad}");
        }
    }

    /// Also equal to the dashboard contract's `registry::ROWS`
    /// (`eval::commitments` tests, which include the contract file).
    #[test]
    fn the_bound_equals_the_uid_maps() {
        assert_eq!(ROWS, crate::eval::commitments::UID_MAP_ROWS);
    }
}

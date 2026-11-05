//! The deployment's spend ledger (pure), held by the `SpendLedger` Durable
//! Object and shared by the main and load cohort instances.
//!
//! The ledger tracks; it never refuses or pauses anything for spend. Spend is
//! keyed by the account's billing cycle: the cycle containing `now` starts on
//! the configured day of the month (`BILLING_CYCLE_DAY`, a deployment value).
//! Each queued plan records a reservation of its budgets, every open
//! reservation counts toward the cycle's total, and at the end of a cohort its
//! reservation is replaced by what it measurably spent. Operator probes settle
//! their measured cost under their own keys.
//!
//! The alert threshold is a deployment value passed at deploy time, never a
//! tracked one. When the cycle's settled plus reserved total passes it,
//! `over_threshold` is set on the status (an operator alert keys on it);
//! without a threshold there is no alert. Per-plan generation and scoring
//! budgets stay: they are runaway guards for one cohort, not a monthly cap.

use crate::sql::{Row, Sql, SqlValue, StoreError, int, text};
use serde::Serialize;

pub const LEDGER_SCHEMA: crate::sql::Schema = crate::sql::Schema {
    family: "spend_ledger",
    migrations: &[crate::sql::Migration {
        version: 1,
        statements: &[
            "CREATE TABLE spend (cycle TEXT PRIMARY KEY, spent_microusd INTEGER NOT NULL) STRICT",
            "CREATE TABLE reservations (
                key TEXT PRIMARY KEY, cycle TEXT NOT NULL, microusd INTEGER NOT NULL, at_ms INTEGER NOT NULL
            ) STRICT",
            "CREATE TABLE charges (
                key TEXT NOT NULL, cycle TEXT NOT NULL, microusd INTEGER NOT NULL, at_ms INTEGER NOT NULL, note TEXT
            ) STRICT",
        ],
    }],
};

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The billing cycle containing `now_ms` (UTC): `YYYY-MM-DD` of its first day.
/// `cycle_day` is clamped to 1..=28 so every month has it.
pub fn cycle_key(now_ms: u64, cycle_day: u32) -> String {
    let day = cycle_day.clamp(1, 28);
    let (y, m, d) = civil((now_ms / 86_400_000) as i64);
    let (y, m) = if d >= day {
        (y, m)
    } else if m == 1 {
        (y - 1, 12)
    } else {
        (y, m - 1)
    };
    format!("{y:04}-{m:02}-{day:02}")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Standing {
    pub cycle: String,
    /// The deployment's alert threshold; `None`: no alert.
    pub alert_microusd: Option<u64>,
    pub spent_microusd: u64,
    pub reserved_microusd: u64,
    /// The cycle's settled plus reserved total is past the alert threshold.
    pub over_threshold: bool,
}

impl Standing {
    /// What the cycle has settled plus every open reservation.
    pub fn total(&self) -> u64 {
        self.spent_microusd.saturating_add(self.reserved_microusd)
    }

    /// What is left below the alert threshold (`u64::MAX` without one).
    /// Informational only: nothing is refused when it reaches zero.
    pub fn headroom(&self) -> u64 {
        self.alert_microusd.map_or(u64::MAX, |a| a.saturating_sub(self.total()))
    }
}

/// The alert threshold from the deployment's values: `SPEND_ALERT_MICROUSD`.
/// Missing means no alert, never a refusal.
pub fn alert_threshold(var: impl Fn(&str) -> Option<u64>) -> Option<u64> {
    var("SPEND_ALERT_MICROUSD")
}

/// The threshold the `SpendLedger` object passes in its cap position: a
/// threshold of `u64::MAX` is no threshold.
fn threshold(cap: u64) -> Option<u64> {
    (cap != u64::MAX).then_some(cap)
}

fn i(v: i64) -> SqlValue {
    SqlValue::Integer(v)
}

fn s(v: impl Into<String>) -> SqlValue {
    SqlValue::Text(v.into())
}

fn one(rows: &[Row]) -> i64 {
    rows.first().and_then(|r| int(r, 0).ok()).unwrap_or(0)
}

/// The cycle's spend and every open reservation, whatever cycle it was made
/// in: a cohort queued before a cycle boundary settles into the cycle current
/// at its end, so its reservation holds headroom there too.
///
/// `alert` is the deployment's alert threshold (`u64::MAX`: none).
pub fn standing(sql: &impl Sql, cycle: &str, alert: u64) -> Result<Standing, StoreError> {
    let spent = one(&sql.exec("SELECT spent_microusd FROM spend WHERE cycle = ?", &[s(cycle)])?) as u64;
    let reserved = one(&sql.exec("SELECT COALESCE(SUM(microusd), 0) FROM reservations", &[])?) as u64;
    let alert = threshold(alert);
    Ok(Standing {
        cycle: cycle.into(),
        alert_microusd: alert,
        spent_microusd: spent,
        reserved_microusd: reserved,
        over_threshold: alert.is_some_and(|a| spent.saturating_add(reserved) > a),
    })
}

/// An admitted reservation: the standing after it, and whether this call
/// created the key (a repeat finds it held already).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Reserved {
    pub standing: Standing,
    pub created: bool,
}

/// Records a reservation of `microusd` under `key`. It is never refused:
/// the ledger tracks spend (the inner `Err` is kept for the object's reply
/// shape and never produced). A repeated reservation of the same key is
/// idempotent (the larger amount holds) and reports `created: false`, so a
/// caller that fails afterwards releases only a reservation it made.
pub fn reserve(sql: &impl Sql, key: &str, microusd: u64, cycle: &str, alert: u64, now_ms: u64) -> Result<Result<Reserved, Standing>, StoreError> {
    let existing = sql.exec("SELECT microusd FROM reservations WHERE key = ?", &[s(key)])?;
    let held = existing.first().map(|r| int(r, 0).unwrap_or(0) as u64);
    sql.exec(
        "INSERT INTO reservations (key, cycle, microusd, at_ms) VALUES (?, ?, ?, ?)
         ON CONFLICT (key) DO UPDATE SET microusd = MAX(microusd, excluded.microusd)",
        &[s(key), s(cycle), i(microusd as i64), i(now_ms as i64)],
    )?;
    Ok(Ok(Reserved { standing: standing(sql, cycle, alert)?, created: held.is_none() }))
}

/// Drops `key`'s reservation without charging anything (a plan that was
/// reserved by this request and then not queued).
pub fn release(sql: &impl Sql, key: &str) -> Result<(), StoreError> {
    sql.exec("DELETE FROM reservations WHERE key = ?", &[s(key)])?;
    Ok(())
}

/// Charges measured spend to the current cycle and drops `key`'s reservation.
/// Idempotent: a key already charged is never charged again.
pub fn settle(sql: &impl Sql, key: &str, microusd: u64, cycle: &str, note: &str, now_ms: u64) -> Result<(), StoreError> {
    sql.exec("DELETE FROM reservations WHERE key = ?", &[s(key)])?;
    if !sql.exec("SELECT 1 FROM charges WHERE key = ?", &[s(key)])?.is_empty() {
        return Ok(());
    }
    if microusd == 0 {
        return Ok(());
    }
    sql.exec(
        "INSERT INTO spend (cycle, spent_microusd) VALUES (?, ?)
         ON CONFLICT (cycle) DO UPDATE SET spent_microusd = spent_microusd + excluded.spent_microusd",
        &[s(cycle), i(microusd as i64)],
    )?;
    sql.exec(
        "INSERT INTO charges (key, cycle, microusd, at_ms, note) VALUES (?, ?, ?, ?, ?)",
        &[s(key), s(cycle), i(microusd as i64), i(now_ms as i64), s(note)],
    )?;
    Ok(())
}

/// Content-free ledger status: every cycle's spend and the open reservations.
pub fn status(sql: &impl Sql, cycle: &str, alert: u64) -> Result<serde_json::Value, StoreError> {
    let st = standing(sql, cycle, alert)?;
    let res: Vec<serde_json::Value> = sql
        .exec("SELECT key, cycle, microusd, at_ms FROM reservations ORDER BY at_ms", &[])?
        .iter()
        .map(|r| serde_json::json!({"key": text(r, 0).unwrap_or(""), "cycle": text(r, 1).unwrap_or(""),
            "microusd": int(r, 2).unwrap_or(0), "at_ms": int(r, 3).unwrap_or(0)}))
        .collect();
    let charges: Vec<serde_json::Value> = sql
        .exec("SELECT key, cycle, microusd, at_ms, note FROM charges ORDER BY at_ms DESC LIMIT 50", &[])?
        .iter()
        .map(|r| serde_json::json!({"key": text(r, 0).unwrap_or(""), "cycle": text(r, 1).unwrap_or(""),
            "microusd": int(r, 2).unwrap_or(0), "at_ms": int(r, 3).unwrap_or(0), "note": text(r, 4).unwrap_or("")}))
        .collect();
    Ok(serde_json::json!({"standing": st, "cycle_total_microusd": st.total(), "over_threshold": st.over_threshold,
        "reservations": res, "recent_charges": charges, "vendor_cycle_microusd": vendor_cycle(sql, cycle)?}))
}

/// The ledger key prefixes of each frontier vendor's spend: cohort
/// generation (`gen:<vendor>:<instance>:<label>`) and the operator's
/// measurement calls (`probe:gen-measure-<vendor>:...`).
pub const VENDOR_KEY_PREFIXES: [(&str, [&str; 2]); 2] =
    [("openai", ["gen:openai:", "probe:gen-measure-openai:"]), ("anthropic", ["gen:anthropic:", "probe:gen-measure-anthropic:"])];

/// The cycle's settled spend with each frontier vendor, summed over every
/// charge of the cycle (not only the newest), so the private spend tracker
/// sees vendor spend apart from Cloudflare's without paging the charges.
pub fn vendor_cycle(sql: &impl Sql, cycle: &str) -> Result<serde_json::Value, StoreError> {
    let mut out = serde_json::Map::new();
    for (vendor, prefixes) in VENDOR_KEY_PREFIXES {
        let mut total = 0i64;
        for p in prefixes {
            // `substr` compares the prefix exactly (no LIKE wildcards).
            total += one(&sql.exec(
                "SELECT COALESCE(SUM(microusd), 0) FROM charges WHERE cycle = ? AND substr(key, 1, ?) = ?",
                &[s(cycle), i(p.len() as i64), s(p)],
            )?);
        }
        out.insert(vendor.into(), serde_json::json!(total));
    }
    Ok(serde_json::Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Sqlite, tx};

    #[test]
    fn billing_cycles_start_on_the_configured_day() {
        // 2026-10-30T12:00Z.
        let t = 1_793_361_600_000u64;
        assert_eq!(civil((t / 86_400_000) as i64), (2026, 10, 30));
        assert_eq!(cycle_key(t, 1), "2026-10-01");
        assert_eq!(cycle_key(t, 30), "2026-10-28", "clamped to 28");
        assert_eq!(cycle_key(t, 31), "2026-10-28");
        // Before the cycle day the cycle began last month, across a year end.
        let jan3 = 1_799_000_000_000u64; // 2027-01-03
        assert_eq!(civil((jan3 / 86_400_000) as i64), (2027, 1, 3));
        assert_eq!(cycle_key(jan3, 15), "2026-12-15");
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    #[test]
    fn reservations_are_tracked_never_refused_and_settle_to_measured_spend() {
        let db = Sqlite::open();
        tx(&db, |s| crate::sql::migrate(s, &LEDGER_SCHEMA)).unwrap();
        // An arbitrary test threshold, not a deployment value.
        let alert = 1_000_000_000;
        let c = "2026-10-01";
        assert!(tx(&db, |s| reserve(s, "main:a", 700_000_000, c, alert, 1)).unwrap().unwrap().created);
        // Past the threshold: recorded, and flagged for the alert.
        let past = tx(&db, |s| reserve(s, "load:b", 400_000_000, c, alert, 2)).unwrap().unwrap();
        assert!(past.created && past.standing.over_threshold);
        assert_eq!((past.standing.total(), past.standing.headroom()), (1_100_000_000, 0));
        // Idempotent: the same key again holds the larger amount, adding nothing new.
        assert!(!tx(&db, |s| reserve(s, "main:a", 300_000_000, c, alert, 3)).unwrap().unwrap().created);
        assert_eq!(tx(&db, |s| standing(s, c, alert)).unwrap().reserved_microusd, 1_100_000_000);
        tx(&db, |s| settle(s, "main:a", 40_000_000, c, "closed", 4)).unwrap();
        tx(&db, |s| settle(s, "main:a", 40_000_000, c, "closed again", 5)).unwrap();
        let st = tx(&db, |s| standing(s, c, alert)).unwrap();
        assert_eq!((st.spent_microusd, st.reserved_microusd, st.over_threshold), (40_000_000, 400_000_000, false));
        assert_eq!(st.headroom(), 560_000_000);
        // A new cycle starts with no spend; open reservations still count.
        assert_eq!(tx(&db, |s| standing(s, "2026-11-01", alert)).unwrap().total(), 400_000_000);
        tx(&db, |s| settle(s, "load:b", 10_000_000, c, "closed", 6)).unwrap();
        assert_eq!(tx(&db, |s| standing(s, "2026-11-01", alert)).unwrap().total(), 0);
        // No threshold: no alert and no headroom limit, whatever the total.
        let none = tx(&db, |s| reserve(s, "main:c", u64::MAX / 4, c, u64::MAX, 7)).unwrap().unwrap();
        assert_eq!(
            (none.standing.alert_microusd, none.standing.over_threshold, none.standing.headroom()),
            (None, false, u64::MAX)
        );
        assert_eq!(alert_threshold(|n| (n == "MONTHLY_CAP_MICROUSD").then_some(5)), None, "no earlier name is read");
        assert_eq!(alert_threshold(|n| (n == "SPEND_ALERT_MICROUSD").then_some(7)), Some(7));
        assert_eq!(alert_threshold(|_| None), None);
    }

    /// The ledger's `/status` body is the contract the publisher's
    /// `ledger_threshold` alert parses (embedded as `ledger` in
    /// `/eval/status`): the shared fixture is byte-for-byte what `status`
    /// serializes, and the publisher's test parses the same file.
    #[test]
    fn status_matches_the_publisher_golden_fixture() {
        let db = Sqlite::open();
        tx(&db, |s| crate::sql::migrate(s, &LEDGER_SCHEMA)).unwrap();
        let c = "2026-11-01";
        tx(&db, |s| reserve(s, "main:testnet-c7", 600, c, 1000, 1)).unwrap().unwrap();
        tx(&db, |s| reserve(s, "main:testnet-c8", 150, c, 1000, 2)).unwrap().unwrap();
        tx(&db, |s| settle(s, "main:testnet-c7", 900, c, "cohort spend", 3)).unwrap();
        let got = tx(&db, |s| status(s, c, 1000)).unwrap();
        let want: serde_json::Value =
            serde_json::from_str(include_str!("../../../publisher/tests/fixtures/eval-ledger-status.json")).unwrap();
        assert_eq!(got, want);
        // Without a threshold the flag is present and false.
        let none = tx(&db, |s| status(s, c, u64::MAX)).unwrap();
        assert_eq!((none["over_threshold"].as_bool(), none["standing"]["alert_microusd"].is_null()), (Some(false), true));
    }

    /// Generation v2: each frontier vendor's cycle spend is summed over every
    /// charge of the cycle (cohort generation and measurement calls), past
    /// the 50 newest the status lists, and apart from Cloudflare spend.
    #[test]
    fn vendor_spend_is_summed_per_cycle_apart_from_cloudflare() {
        let db = Sqlite::open();
        tx(&db, |s| crate::sql::migrate(s, &LEDGER_SCHEMA)).unwrap();
        let c = "2026-11-01";
        for k in 0..60u64 {
            tx(&db, |s| settle(s, &format!("gen:anthropic:main:testnet-c{k}"), 10, c, "vendor generation spend", k)).unwrap();
        }
        tx(&db, |s| settle(s, "gen:openai:main:testnet-c1", 7, c, "vendor generation spend", 100)).unwrap();
        tx(&db, |s| settle(s, "probe:gen-measure-openai:gpt-6-luna:low:0:0", 5, c, "measure", 101)).unwrap();
        tx(&db, |s| settle(s, "main:testnet-c1", 900, c, "cohort spend", 102)).unwrap();
        // A charge of another cycle, and a key that only looks alike.
        tx(&db, |s| settle(s, "gen:openai:main:testnet-c0", 1000, "2026-10-01", "vendor generation spend", 103)).unwrap();
        tx(&db, |s| settle(s, "gen_openai:x", 3, c, "other", 104)).unwrap();
        let got = tx(&db, |s| status(s, c, u64::MAX)).unwrap();
        assert_eq!(got["recent_charges"].as_array().unwrap().len(), 50);
        assert_eq!(got["vendor_cycle_microusd"], serde_json::json!({"openai": 12, "anthropic": 600}));
        assert_eq!(got["standing"]["spent_microusd"], 600 + 12 + 900 + 3);
    }

    /// Review 2026-10-31 (a): a repeated `/plan` for a queued label must not
    /// drop the live cohort's reservation. The route releases only a
    /// reservation its own call created.
    #[test]
    fn a_repeated_reservation_is_reported_so_a_failed_retry_keeps_it() {
        let db = Sqlite::open();
        tx(&db, |s| crate::sql::migrate(s, &LEDGER_SCHEMA)).unwrap();
        let (alert, c) = (1_000_000_000, "2026-10-01");
        let first = tx(&db, |s| reserve(s, "main:x", 600_000_000, c, alert, 1)).unwrap().unwrap();
        assert!(first.created);
        assert_eq!(first.standing.headroom(), 400_000_000);
        // The retry: held already, so the route's failure path must not release it.
        let retry = tx(&db, |s| reserve(s, "main:x", 600_000_000, c, alert, 2)).unwrap().unwrap();
        assert!(!retry.created);
        assert_eq!(retry.standing.reserved_microusd, 600_000_000);
        // A second plan of the same size is recorded beside the first.
        assert!(tx(&db, |s| reserve(s, "main:y", 600_000_000, c, alert, 3)).unwrap().unwrap().standing.over_threshold);
        tx(&db, |s| release(s, "main:y")).unwrap();
        // Releasing a reservation the call made removes it, and only it.
        tx(&db, |s| release(s, "main:none")).unwrap();
        assert_eq!(tx(&db, |s| standing(s, c, alert)).unwrap().reserved_microusd, 600_000_000);
    }

    /// Review 2026-10-31 (b): a reservation made before the billing-cycle
    /// boundary counts after it, since its cohort settles into the cycle
    /// current at its end.
    #[test]
    fn an_open_reservation_counts_across_the_cycle_boundary() {
        let db = Sqlite::open();
        tx(&db, |s| crate::sql::migrate(s, &LEDGER_SCHEMA)).unwrap();
        let alert = 1_000_000_000;
        let oct31 = 1_793_448_000_000u64; // 2026-10-31T12:00Z
        let nov1 = oct31 + 86_400_000;
        assert_eq!((cycle_key(oct31, 1), cycle_key(nov1, 1)), ("2026-10-01".to_owned(), "2026-11-01".to_owned()));
        assert!(tx(&db, |s| reserve(s, "main:a", 800_000_000, &cycle_key(oct31, 1), alert, oct31)).unwrap().is_ok());
        // November: the October reservation still counts.
        let b = tx(&db, |s| reserve(s, "main:b", 800_000_000, &cycle_key(nov1, 1), alert, nov1)).unwrap().unwrap();
        assert_eq!((b.standing.total(), b.standing.over_threshold, b.standing.headroom()), (1_600_000_000, true, 0));
        tx(&db, |s| settle(s, "main:a", 800_000_000, &cycle_key(nov1, 1), "closed", nov1)).unwrap();
        let st = tx(&db, |s| standing(s, &cycle_key(nov1, 1), alert)).unwrap();
        assert_eq!((st.spent_microusd, st.reserved_microusd), (800_000_000, 800_000_000));
    }
}

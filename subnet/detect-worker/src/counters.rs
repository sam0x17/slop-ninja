//! Durable hourly counts of scans, kept by the gate object and flushed to
//! the detect store.
//!
//! - **In the gate.** The ticket release that ends a scan carries its
//!   [`Count`]: the channel, the package the scorer loaded and the outcome.
//!   Before the release is answered, the gate adds one to the row of the
//!   release's UTC hour in its own SQLite ([`INCREMENT`], synchronous, so
//!   durable before the reply leaves the object). Nothing is kept only in
//!   memory, so an eviction or restart loses no acknowledged increment.
//! - **Flush by alarm.** Any increment arms an alarm when none is set, for
//!   the earlier of five minutes from now and one minute after the next hour
//!   boundary ([`alarm_at`]; a liveness interval, not a cap). The alarm reads
//!   the rows with `n > flushed_n` ([`PENDING`]) and writes them to D1
//!   `hour_counters` as absolute values in one batch ([`flush_sql`]), so a
//!   retried or repeated flush writes the same numbers. Once D1 confirms,
//!   each row's `flushed_n` becomes the value written ([`MARK_FLUSHED`]);
//!   increments that arrived meanwhile stay pending and re-arm the alarm.
//!   Rows of closed hours whose `flushed_n = n` are then deleted
//!   ([`DELETE_CLOSED`]). A failed flush leaves every row as it was. The gate
//!   is the only writer of `hour_counters`.
//! - **What counts.** The pot reads only `outcome = 'scored'` on the free
//!   channels in route-hours the edge coverage marks covered. A free scan
//!   that did not arrive through the site host's score route is served and
//!   recorded under outcome [`UNCOVERED_PATH`], never `scored`. Refusals
//!   before a ticket never reach the gate: they are AE `submit` points only.
//!   Each row holds counts only: no text, hash, address, key or band.

use slop_ninja_detect_page::{Channel, RefusalCode};

/// The gate's table (created when the object starts).
pub const GATE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS hour_counts (\
hour TEXT NOT NULL, channel TEXT NOT NULL, package_load TEXT NOT NULL, outcome TEXT NOT NULL, \
n INTEGER NOT NULL, flushed_n INTEGER NOT NULL, PRIMARY KEY (hour, channel, package_load, outcome))";

/// One increment: hour, channel, package, outcome.
pub const INCREMENT: &str = "INSERT INTO hour_counts (hour, channel, package_load, outcome, n, flushed_n) \
VALUES (?, ?, ?, ?, 1, 0) ON CONFLICT (hour, channel, package_load, outcome) DO UPDATE SET n = n + 1";

/// The rows to flush, at most `?` of them, in key order.
pub const PENDING: &str = "SELECT hour, channel, package_load, outcome, n FROM hour_counts \
WHERE n > flushed_n ORDER BY hour, channel, package_load, outcome LIMIT ?";

/// After D1 confirmed: the value written, then the key.
pub const MARK_FLUSHED: &str = "UPDATE hour_counts SET flushed_n = ? \
WHERE hour = ? AND channel = ? AND package_load = ? AND outcome = ? AND flushed_n < ?";

/// Rows of hours before `?` that D1 holds in full.
pub const DELETE_CLOSED: &str = "DELETE FROM hour_counts WHERE hour < ? AND flushed_n = n";

/// Whether any row is still to flush.
pub const ANY_PENDING: &str = "SELECT COUNT(*) AS c FROM hour_counts WHERE n > flushed_n";

/// D1's bound parameters per statement (its documented limit).
pub const D1_MAX_BOUND_PARAMETERS: usize = 100;
/// Each flushed row binds its key and value.
pub const PARAMS_PER_ROW: usize = 5;
/// Rows per D1 statement.
pub const ROWS_PER_STATEMENT: usize = D1_MAX_BOUND_PARAMETERS / PARAMS_PER_ROW;
/// Statements per flush: D1's queries per invocation on the Workers Free
/// plan, the smallest documented limit. A longer backlog takes further
/// alarms, set at once.
pub const STATEMENTS_PER_FLUSH: usize = 50;
/// Rows per flush.
pub const ROWS_PER_FLUSH: usize = ROWS_PER_STATEMENT * STATEMENTS_PER_FLUSH;

/// The outcome of a scan served on a path the pot does not count.
pub const UNCOVERED_PATH: &str = "uncovered_path";
/// The outcome of a scored scan.
pub const SCORED: &str = "scored";

const HOUR_MS: u64 = 3_600_000;
/// The flush's liveness interval.
pub const FLUSH_WITHIN_MS: u64 = 5 * 60_000;
/// How long after an hour closes its rows are flushed at the latest.
pub const AFTER_HOUR_MS: u64 = 60_000;

/// The UPSERT that writes `rows` rows to D1 `hour_counters` by absolute value.
pub fn flush_sql(rows: usize) -> String {
    let values = vec!["(?, ?, ?, ?, ?)"; rows].join(", ");
    format!(
        "INSERT INTO hour_counters (hour, channel, package_load, outcome, n) VALUES {values} \
ON CONFLICT (hour, channel, package_load, outcome) DO UPDATE SET n = excluded.n"
    )
}

/// What a scan's release counts.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Count {
    pub channel: String,
    pub package_load: String,
    pub outcome: String,
}

impl Count {
    /// A count the gate accepts: a known channel, a 64-hex-digit package and
    /// a known outcome (`scored`, `uncovered_path` or a refusal code).
    pub fn valid(&self) -> bool {
        Channel::parse(&self.channel).is_some()
            && self.package_load.len() == 64
            && self.package_load.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && (self.outcome == SCORED || self.outcome == UNCOVERED_PATH || RefusalCode::parse(&self.outcome).is_some())
    }
}

/// One pending row.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct Row {
    pub hour: String,
    pub channel: String,
    pub package_load: String,
    pub outcome: String,
    pub n: i64,
}

/// The UTC hour of `ms`: `YYYY-MM-DDTHH`.
pub fn hour_key(ms: u64) -> String {
    let hours = ms / HOUR_MS;
    let (y, m, d) = civil(i64::try_from(hours / 24).unwrap_or(i64::MAX));
    format!("{y:04}-{m:02}-{d:02}T{:02}", hours % 24)
}

/// The proleptic Gregorian date of a day number (days since 1970-01-01).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// When a flush alarm armed at `now_ms` fires: the earlier of
/// [`FLUSH_WITHIN_MS`] from now and [`AFTER_HOUR_MS`] past the next hour.
pub fn alarm_at(now_ms: u64) -> u64 {
    let next_hour = (now_ms / HOUR_MS + 1) * HOUR_MS;
    now_ms.saturating_add(FLUSH_WITHIN_MS).min(next_hour + AFTER_HOUR_MS)
}

/// The pending rows split into D1 statements of at most
/// [`ROWS_PER_STATEMENT`] rows each.
pub fn statements(rows: &[Row]) -> impl Iterator<Item = &[Row]> {
    rows.chunks(ROWS_PER_STATEMENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{Connection, params};

    const PKG: &str = "0b4e92b3aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const T0: u64 = 1_763_040_000_000; // 2025-11-13T13:20:00Z

    #[test]
    fn hours_and_alarms() {
        assert_eq!(hour_key(0), "1970-01-01T00");
        assert_eq!(hour_key(T0), "2025-11-13T13");
        assert_eq!(hour_key(951_782_400_000), "2000-02-29T00");
        assert_eq!(hour_key(4_107_542_399_999), "2100-02-28T23");
        assert_eq!(hour_key(4_107_542_400_000), "2100-03-01T00");
        // Keys sort as times do, so `hour < ?` selects the closed hours.
        let keys: Vec<String> = (0..20_000).map(|h| hour_key(T0 + h * 2_700_000)).collect();
        assert!(keys.windows(2).all(|w| w[0] <= w[1]));
        // Five minutes from now, or a minute after the hour closes.
        assert_eq!(alarm_at(T0), T0 + FLUSH_WITHIN_MS);
        let late = T0 - T0 % 3_600_000 + 3_600_000 - 1_000; // 13:59:59
        assert_eq!(alarm_at(late), late + 1_000 + AFTER_HOUR_MS);
    }

    #[test]
    fn statements_fit_d1() {
        assert_eq!(ROWS_PER_STATEMENT * PARAMS_PER_ROW, D1_MAX_BOUND_PARAMETERS);
        assert_eq!(flush_sql(ROWS_PER_STATEMENT).matches('?').count(), D1_MAX_BOUND_PARAMETERS);
        let rows: Vec<Row> = (0..45).map(|i| row(&format!("h{i:02}"), 1)).collect();
        let sizes: Vec<usize> = statements(&rows).map(<[Row]>::len).collect();
        assert_eq!(sizes, vec![20, 20, 5]);
    }

    #[test]
    fn counts_are_checked() {
        let ok = Count { channel: "free_text".into(), package_load: PKG.into(), outcome: "scored".into() };
        assert!(ok.valid());
        for c in [
            Count { channel: "free".into(), ..ok.clone() },
            Count { package_load: PKG[..63].into(), ..ok.clone() },
            Count { package_load: PKG.to_uppercase(), ..ok.clone() },
            Count { outcome: "maybe".into(), ..ok.clone() },
        ] {
            assert!(!c.valid(), "{c:?}");
        }
        assert!(Count { outcome: UNCOVERED_PATH.into(), ..ok.clone() }.valid());
        assert!(Count { outcome: "busy".into(), ..ok.clone() }.valid());
    }

    fn row(hour: &str, n: i64) -> Row {
        Row { hour: hour.into(), channel: "free_text".into(), package_load: PKG.into(), outcome: "scored".into(), n }
    }

    /// The gate's store and the detect store, as two SQLite databases: the
    /// gate's from [`GATE_SCHEMA`], the store from its migration.
    struct Sim {
        gate: Connection,
        d1: Connection,
        d1_down: bool,
    }

    impl Sim {
        fn new() -> Sim {
            let gate = Connection::open_in_memory().unwrap();
            gate.execute(GATE_SCHEMA, []).unwrap();
            let d1 = Connection::open_in_memory().unwrap();
            let schema =
                std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations/0001_detect_store.sql"))
                    .unwrap();
            d1.execute_batch(&schema).unwrap();
            Sim { gate, d1, d1_down: false }
        }

        fn release(&self, at: u64, outcome: &str) {
            self.gate.execute(INCREMENT, params![hour_key(at), "free_text", PKG, outcome]).unwrap();
        }

        /// The alarm, as the gate object runs it.
        fn flush(&self, now: u64) -> bool {
            let rows: Vec<Row> = {
                let mut st = self.gate.prepare(PENDING).unwrap();
                st.query_map(params![ROWS_PER_FLUSH as i64], |r| {
                    Ok(Row {
                        hour: r.get(0)?,
                        channel: r.get(1)?,
                        package_load: r.get(2)?,
                        outcome: r.get(3)?,
                        n: r.get(4)?,
                    })
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
            };
            if self.d1_down {
                return false;
            }
            // One D1 batch: a transaction.
            self.d1.execute_batch("BEGIN").unwrap();
            for chunk in statements(&rows) {
                let mut vals: Vec<rusqlite::types::Value> = Vec::new();
                for r in chunk {
                    vals.extend([
                        r.hour.clone().into(),
                        r.channel.clone().into(),
                        r.package_load.clone().into(),
                        r.outcome.clone().into(),
                        r.n.into(),
                    ]);
                }
                self.d1.execute(&flush_sql(chunk.len()), rusqlite::params_from_iter(vals)).unwrap();
            }
            self.d1.execute_batch("COMMIT").unwrap();
            for r in &rows {
                self.gate
                    .execute(MARK_FLUSHED, params![r.n, r.hour, r.channel, r.package_load, r.outcome, r.n])
                    .unwrap();
            }
            self.gate.execute(DELETE_CLOSED, params![hour_key(now)]).unwrap();
            true
        }

        fn d1_n(&self, hour: &str, outcome: &str) -> Option<i64> {
            self.d1
                .query_row("SELECT n FROM hour_counters WHERE hour = ? AND outcome = ?", params![hour, outcome], |r| {
                    r.get(0)
                })
                .ok()
        }

        fn gate_rows(&self) -> i64 {
            self.gate.query_row("SELECT COUNT(*) FROM hour_counts", [], |r| r.get(0)).unwrap()
        }

        fn pending(&self) -> i64 {
            self.gate.query_row(ANY_PENDING, [], |r| r.get(0)).unwrap()
        }
    }

    #[test]
    fn increments_flush_as_absolute_values() {
        let s = Sim::new();
        for _ in 0..3 {
            s.release(T0, SCORED);
        }
        s.release(T0, UNCOVERED_PATH);
        s.release(T0, "busy");
        assert!(s.flush(T0));
        let h = hour_key(T0);
        assert_eq!(s.d1_n(&h, SCORED), Some(3));
        assert_eq!(s.d1_n(&h, UNCOVERED_PATH), Some(1));
        assert_eq!(s.d1_n(&h, "busy"), Some(1));
        assert_eq!(s.pending(), 0);
        // The open hour's rows stay (more may come); nothing is pending.
        assert_eq!(s.gate_rows(), 3);
    }

    /// A flush run twice, or retried after an unconfirmed write, writes the
    /// same values.
    #[test]
    fn a_double_flush_writes_the_same_numbers() {
        let s = Sim::new();
        for _ in 0..5 {
            s.release(T0, SCORED);
        }
        assert!(s.flush(T0));
        assert!(s.flush(T0));
        assert_eq!(s.d1_n(&hour_key(T0), SCORED), Some(5));
        // A write D1 applied but the gate never saw confirmed: the row stays
        // pending and the next flush writes the same absolute value.
        s.release(T0, SCORED);
        s.d1.execute(&flush_sql(1), params![hour_key(T0), "free_text", PKG, SCORED, 6]).unwrap();
        assert_eq!(s.pending(), 1);
        assert!(s.flush(T0));
        assert_eq!(s.d1_n(&hour_key(T0), SCORED), Some(6));
    }

    /// An eviction loses nothing acknowledged: the counts live in the gate's
    /// SQLite, and a new object instance (a new connection to the same
    /// file) flushes them.
    #[test]
    fn an_evicted_gate_keeps_its_counts() {
        let dir = std::env::temp_dir().join(format!("sn-detect-counts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("gate.sqlite");
        let _ = std::fs::remove_file(&file);
        {
            let gate = Connection::open(&file).unwrap();
            gate.execute(GATE_SCHEMA, []).unwrap();
            for _ in 0..4 {
                gate.execute(INCREMENT, params![hour_key(T0), "free_text", PKG, SCORED]).unwrap();
            }
        } // evicted
        let mut s = Sim::new();
        s.gate = Connection::open(&file).unwrap();
        s.gate.execute(GATE_SCHEMA, []).unwrap();
        s.release(T0, SCORED);
        assert!(s.flush(T0));
        assert_eq!(s.d1_n(&hour_key(T0), SCORED), Some(5));
        drop(s);
        std::fs::remove_file(&file).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn a_failed_flush_leaves_the_rows_and_closed_hours_go_once_written() {
        let mut s = Sim::new();
        s.release(T0, SCORED);
        s.d1_down = true;
        assert!(!s.flush(T0 + 3_600_000));
        assert_eq!(s.pending(), 1);
        assert_eq!(s.d1_n(&hour_key(T0), SCORED), None);
        s.d1_down = false;
        // The next hour: the closed hour is written, then its row deleted.
        s.release(T0 + 3_600_000, SCORED);
        assert!(s.flush(T0 + 3_600_000));
        assert_eq!(s.d1_n(&hour_key(T0), SCORED), Some(1));
        assert_eq!(s.d1_n(&hour_key(T0 + 3_600_000), SCORED), Some(1));
        assert_eq!(s.gate_rows(), 1, "only the open hour's row is left");
    }

    /// Increments that arrive between the read and the confirmation stay
    /// pending: `flushed_n` is the value written, not the row's `n`.
    #[test]
    fn increments_during_a_flush_stay_pending() {
        let s = Sim::new();
        s.release(T0, SCORED);
        let rows: Vec<Row> = vec![row(&hour_key(T0), 1)];
        s.release(T0, SCORED); // arrives during the flush
        s.d1.execute(&flush_sql(1), params![rows[0].hour, rows[0].channel, rows[0].package_load, rows[0].outcome, 1])
            .unwrap();
        s.gate
            .execute(MARK_FLUSHED, params![1, rows[0].hour, rows[0].channel, rows[0].package_load, rows[0].outcome, 1])
            .unwrap();
        assert_eq!(s.pending(), 1);
        assert!(s.flush(T0));
        assert_eq!(s.d1_n(&hour_key(T0), SCORED), Some(2));
        // A late confirmation of an older value never moves flushed_n back.
        s.gate
            .execute(MARK_FLUSHED, params![1, rows[0].hour, rows[0].channel, rows[0].package_load, rows[0].outcome, 1])
            .unwrap();
        assert_eq!(s.pending(), 0);
    }

    #[test]
    fn a_backlog_flushes_in_bounded_alarms() {
        let s = Sim::new();
        let hours = ROWS_PER_FLUSH as u64 + 7;
        for h in 0..hours {
            s.release(T0 + h * 3_600_000, SCORED);
        }
        let now = T0 + hours * 3_600_000;
        assert!(s.flush(now));
        assert_eq!(s.pending(), 7);
        assert!(s.flush(now));
        assert_eq!(s.pending(), 0);
        assert_eq!(s.gate_rows(), 0);
        let total: i64 = s.d1.query_row("SELECT SUM(n) FROM hour_counters", [], |r| r.get(0)).unwrap();
        assert_eq!(total, hours as i64);
    }
}

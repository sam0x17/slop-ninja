//! The spend tracker's upload (`slop_ninja.spend_tracker.relay.v2`): the
//! alert fields and month totals of the tracker's `latest.json`, uploaded by
//! the operator's spend tracker after each scheduled run to the relay's private bucket.
//!
//! The upload carries no threshold (the threshold stays in the tracker's
//! private file), no report path, no failure text and no account id; only
//! the alert state, the months with an active alert, the months'
//! acknowledgment times, the month totals in micro-USD, the number of failed
//! queries and the tracker's own schedule period (v2), which the relay's
//! staleness bound uses.
//!
//! Every field is validated, so the upload's bound ([`upload_bound`])
//! derives from the fields' widths: the fixed fields at their widest, and
//! one entry of each list per month the tracker can have reported.

use serde::{Deserialize, Serialize};

pub const UPLOAD_SCHEMA: &str = "slop_ninja.spend_tracker.relay.v2";
/// The object key in the relay's bucket.
pub const UPLOAD_KEY: &str = "spend-tracker/latest.json";
/// The tracker's first report month: no upload names an earlier one.
pub const FIRST_MONTH: (i64, u32) = (2026, 10);
/// The tracker's authentication states (`auth`).
pub const AUTH_STATES: [&str; 4] = ["ok", "expired", "rejected", "unknown"];
/// The tracker's alert states (`alert_state`, its `report.jq`).
pub const ALERT_STATES: [&str; 5] = [
    "above",
    "projection_warning",
    "undetermined",
    "acknowledged",
    "below",
];

const fn longest(names: &[&str]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < names.len() {
        if names[i].len() > n {
            n = names[i].len();
        }
        i += 1;
    }
    n
}

/// An upload with empty strings and lists and no numbers, in serde's field
/// order (a test checks it).
const SKELETON: &str = r#"{"schema":"","generated_at":"","uploaded_at":"","month":"","final":,"exit_code":,"auth":"","complete":,"alert_any":,"alert_months":[],"alert_state":,"threshold_overridden":,"acked":[],"account":{"mtd_microusd":,"projected_microusd":},"testnet":{"mtd_microusd":,"projected_microusd":},"failures":,"period_secs":}"#;
/// `YYYY-MM-DDTHH:MM:SSZ`, the only time format the relay reads.
const TIME_BYTES: usize = "2026-11-02T13:06:08Z".len();
const MONTH: usize = "2026-11".len();
const FALSE: usize = "false".len();
const U64_DIGITS: usize = 20;

/// The fixed fields of an upload at their widest: the schema, two times,
/// the month, four booleans, the exit code (a `u8`), the longest auth state,
/// the longest alert state (quoted), four totals and the period (`u64`s)
/// and the failure count (a `u32`). A test checks a maximal upload against
/// it.
pub const FIXED_BYTES: u64 = (SKELETON.len()
    + UPLOAD_SCHEMA.len()
    + 2 * TIME_BYTES
    + MONTH
    + 4 * FALSE
    + 3
    + longest(&AUTH_STATES)
    + longest(&ALERT_STATES)
    + 2
    + 5 * U64_DIGITS
    + 10) as u64;
/// What one month can add: one `alert_months` entry (`"YYYY-MM",`) and one
/// acknowledgment (`{"month":"YYYY-MM","acked_at":"YYYY-MM-DDTHH:MM:SSZ"},`).
pub const MONTH_BYTES: u64 =
    ("\"\",".len() + MONTH + r#"{"month":"","acked_at":""},"#.len() + MONTH + TIME_BYTES) as u64;

/// The number of months from [`FIRST_MONTH`] through the month of `now_ms`,
/// plus one for an upload stamped by a clock ahead of the relay's.
pub fn months_through(now_ms: u64) -> u64 {
    let now = crate::mime::rfc3339(now_ms);
    let (y, m) = (
        now[0..4].parse::<i64>().unwrap_or(FIRST_MONTH.0),
        now[5..7].parse::<i64>().unwrap_or(1),
    );
    let n = (y - FIRST_MONTH.0) * 12 + (m - i64::from(FIRST_MONTH.1)) + 1;
    n.max(0) as u64 + 1
}

/// The largest upload the relay reads at `now_ms`: the fixed fields and one
/// entry of each list per month the tracker can have reported.
pub fn upload_bound(now_ms: u64) -> u64 {
    FIXED_BYTES + MONTH_BYTES * months_through(now_ms)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Totals {
    pub mtd_microusd: Option<u64>,
    pub projected_microusd: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acked {
    pub month: String,
    pub acked_at: String,
}

/// The uploaded object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upload {
    pub schema: String,
    /// The tracker run's own time (`latest.json` `generated_at`).
    pub generated_at: String,
    pub uploaded_at: String,
    /// The month the run priced.
    pub month: String,
    pub r#final: bool,
    pub exit_code: u8,
    /// `ok`, `expired`, `rejected` or `unknown`.
    pub auth: String,
    pub complete: bool,
    pub alert_any: bool,
    /// Every month with an active (unacknowledged) alert.
    pub alert_months: Vec<String>,
    /// This run's month only.
    pub alert_state: Option<String>,
    pub threshold_overridden: bool,
    /// Each acknowledged month's latest acknowledgment.
    pub acked: Vec<Acked>,
    pub account: Totals,
    pub testnet: Totals,
    /// How many queries failed (their text stays with the tracker).
    pub failures: u32,
    /// The tracker's schedule period in seconds (its `config.json`
    /// `schedule_period_secs`, kept beside its timer); `None` when the
    /// tracker does not state it, and the relay's `TRACKER_PERIOD_SECS`
    /// applies.
    pub period_secs: Option<u64>,
}

impl Upload {
    /// The schedule period in milliseconds, when stated.
    pub fn period_ms(&self) -> Option<u64> {
        self.period_secs.and_then(|s| s.checked_mul(1000))
    }
}

/// `YYYY-MM`.
pub fn valid_month(m: &str) -> bool {
    let b = m.as_bytes();
    b.len() == 7
        && b[4] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[5..].iter().all(u8::is_ascii_digit)
        && matches!(
            &m[5..],
            "01" | "02" | "03" | "04" | "05" | "06" | "07" | "08" | "09" | "10" | "11" | "12"
        )
}

/// What the relay read from the bucket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    /// The bucket did not answer: nothing is known this tick, and nothing
    /// the tracker raised changes (no new alert, no resolution).
    Unavailable,
    Missing,
    /// Not readable: too large, malformed, wrong schema or a test threshold.
    Unreadable(&'static str),
    Ok {
        upload: Box<Upload>,
        /// `generated_at` (the tracker's clock).
        generated_ms: u64,
        /// When the bucket stored the object (Cloudflare's clock): the age
        /// counts from the older of the two.
        uploaded_ms: u64,
    },
}

/// Parses an upload body (already within [`upload_bound`]) that the bucket
/// stored at `uploaded_ms`.
pub fn parse(body: &[u8], uploaded_ms: u64) -> Read {
    let Ok(u) = serde_json::from_slice::<Upload>(body) else {
        return Read::Unreadable("malformed");
    };
    if u.schema != UPLOAD_SCHEMA {
        return Read::Unreadable("schema");
    }
    if u.threshold_overridden {
        return Read::Unreadable("test threshold");
    }
    if !valid_month(&u.month)
        || !u.alert_months.iter().all(|m| valid_month(m))
        || !u
            .acked
            .iter()
            .all(|a| valid_month(&a.month) && crate::mime::parse_rfc3339(&a.acked_at).is_some())
        || u.alert_any != !u.alert_months.is_empty()
        || !AUTH_STATES.contains(&u.auth.as_str())
        || u.alert_state
            .as_deref()
            .is_some_and(|a| !ALERT_STATES.contains(&a))
        || crate::mime::parse_rfc3339(&u.uploaded_at).is_none()
        || u.period_secs.is_some_and(|p| p == 0)
        || u.period_secs.is_some() && u.period_ms().is_none()
    {
        return Read::Unreadable("fields");
    }
    let Some(generated_ms) = crate::mime::parse_rfc3339(&u.generated_at) else {
        return Read::Unreadable("generated_at");
    };
    Read::Ok {
        upload: Box::new(u),
        generated_ms,
        uploaded_ms,
    }
}

/// The latest acknowledgment of `month`, in milliseconds.
pub fn acked_ms(u: &Upload, month: &str) -> Option<u64> {
    u.acked
        .iter()
        .filter(|a| a.month == month)
        .filter_map(|a| crate::mime::parse_rfc3339(&a.acked_at))
        .max()
}

/// `$1,234.56` from micro-USD (cents rounded down).
pub fn usd(micro: u64) -> String {
    let cents = micro / 10_000;
    let dollars = (cents / 100).to_string();
    let mut grouped = String::new();
    for (i, c) in dollars.chars().enumerate() {
        if i > 0 && (dollars.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("${grouped}.{:02}", cents % 100)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn upload(alert_months: &[&str]) -> Upload {
        Upload {
            schema: UPLOAD_SCHEMA.into(),
            generated_at: "2026-11-02T13:06:08Z".into(),
            uploaded_at: "2026-11-02T13:06:10Z".into(),
            month: "2026-11".into(),
            r#final: false,
            exit_code: 0,
            auth: "ok".into(),
            complete: true,
            alert_any: !alert_months.is_empty(),
            alert_months: alert_months.iter().map(|m| (*m).into()).collect(),
            alert_state: Some("below".into()),
            threshold_overridden: false,
            acked: vec![],
            account: Totals {
                mtd_microusd: Some(12_345_678),
                projected_microusd: Some(98_765_432),
            },
            testnet: Totals::default(),
            failures: 0,
            period_secs: Some(86_400),
        }
    }

    #[test]
    fn uploads_parse_and_refuse() {
        let u = upload(&["2026-11"]);
        let body = serde_json::to_vec(&u).unwrap();
        assert!(
            matches!(parse(&body, 7), Read::Ok { ref upload, generated_ms, uploaded_ms: 7 } if **upload == u && generated_ms > 0)
        );
        let mut t = u.clone();
        t.threshold_overridden = true;
        assert_eq!(
            parse(&serde_json::to_vec(&t).unwrap(), 7),
            Read::Unreadable("test threshold")
        );
        let mut t = u.clone();
        t.alert_any = false;
        assert_eq!(
            parse(&serde_json::to_vec(&t).unwrap(), 7),
            Read::Unreadable("fields")
        );
        let mut t = u.clone();
        t.schema = "slop_ninja.spend_tracker.latest.v2".into();
        assert_eq!(
            parse(&serde_json::to_vec(&t).unwrap(), 7),
            Read::Unreadable("schema")
        );
        assert_eq!(parse(b"{\"schema\":1}", 7), Read::Unreadable("malformed"));
        let mut v = serde_json::to_value(&u).unwrap();
        v["report_md"] = "/private/path".into();
        assert_eq!(
            parse(&serde_json::to_vec(&v).unwrap(), 7),
            Read::Unreadable("malformed")
        );
        assert!(!valid_month("2026-13") && valid_month("2026-01"));
        // Free strings are validated against the tracker's own values.
        for bad in [
            Upload {
                alert_state: Some("maybe".into()),
                ..u.clone()
            },
            Upload {
                uploaded_at: "yesterday".into(),
                ..u.clone()
            },
            Upload {
                period_secs: Some(0),
                ..u.clone()
            },
            Upload {
                period_secs: Some(u64::MAX),
                ..u.clone()
            },
        ] {
            assert_eq!(
                parse(&serde_json::to_vec(&bad).unwrap(), 7),
                Read::Unreadable("fields")
            );
        }
        for state in ALERT_STATES {
            let ok = Upload {
                alert_state: Some(state.into()),
                ..u.clone()
            };
            assert!(matches!(
                parse(&serde_json::to_vec(&ok).unwrap(), 7),
                Read::Ok { .. }
            ));
        }
        let unstated = Upload {
            period_secs: None,
            alert_state: None,
            ..u.clone()
        };
        let Read::Ok { upload, .. } = parse(&serde_json::to_vec(&unstated).unwrap(), 7) else {
            panic!("unreadable")
        };
        assert_eq!(upload.period_ms(), None);
        assert_eq!(u.period_ms(), Some(86_400_000));
    }

    /// The tracker's upload (synthetic values, in the form the tracker
    /// writes) parses as the relay reads it.
    #[test]
    fn parses_the_upload_scripts_output() {
        let body = br#"{"schema":"slop_ninja.spend_tracker.relay.v2","generated_at":"2026-11-02T13:06:08Z","uploaded_at":"2026-11-02T14:48:01Z","month":"2026-11","final":false,"exit_code":0,"auth":"ok","complete":true,"alert_any":true,"alert_months":["2026-11"],"alert_state":"above","threshold_overridden":false,"acked":[{"month":"2026-10","acked_at":"2026-10-30T12:00:00Z"}],"account":{"mtd_microusd":1000000,"projected_microusd":2000000},"testnet":{"mtd_microusd":500000,"projected_microusd":900000},"failures":0,"period_secs":86400}"#;
        let Read::Ok { upload, .. } = parse(body, 7) else {
            panic!("unreadable")
        };
        assert_eq!(upload.alert_months, vec!["2026-11".to_string()]);
        assert_eq!(upload.period_ms(), Some(86_400_000));
        assert_eq!(
            acked_ms(&upload, "2026-10"),
            crate::mime::parse_rfc3339("2026-10-30T12:00:00Z")
        );
        assert!(body.len() as u64 <= upload_bound(1_793_627_107_000));
    }

    /// A maximal upload at a given month fits the bound the relay reads with,
    /// with only the lists' last separators to spare; the skeleton is serde's
    /// output.
    #[test]
    fn maximal_uploads_fit_their_bound() {
        let mut empty = serde_json::to_string(&upload(&[])).unwrap();
        for (from, to) in [
            (UPLOAD_SCHEMA, ""),
            ("2026-11-02T13:06:08Z", ""),
            ("2026-11-02T13:06:10Z", ""),
            ("\"2026-11\"", "\"\""),
            ("false", ""),
            ("true", ""),
            (":0,", ":,"),
            ("\"below\"", ""),
            ("\"ok\"", "\"\""),
            ("12345678", ""),
            ("98765432", ""),
            ("null", ""),
            ("86400", ""),
        ] {
            empty = empty.replace(from, to);
        }
        assert_eq!(empty, SKELETON);
        // 2031-05: months 2026-10 through 2031-05, and one ahead.
        let now = 1_938_000_000_000;
        assert_eq!(&crate::mime::rfc3339(now)[..7], "2031-05");
        let months = months_through(now) as usize;
        let list: Vec<String> = (0..months).map(|_| "2031-05".to_string()).collect();
        let u = Upload {
            schema: UPLOAD_SCHEMA.into(),
            generated_at: "2031-05-30T23:59:59Z".into(),
            uploaded_at: "2031-05-30T23:59:59Z".into(),
            month: "2031-05".into(),
            r#final: false,
            exit_code: u8::MAX,
            auth: "rejected".into(),
            complete: false,
            alert_any: true,
            alert_months: list.clone(),
            alert_state: Some("projection_warning".into()),
            threshold_overridden: false,
            acked: list
                .iter()
                .map(|m| Acked {
                    month: m.clone(),
                    acked_at: "2031-05-30T23:59:59Z".into(),
                })
                .collect(),
            account: Totals {
                mtd_microusd: Some(u64::MAX),
                projected_microusd: Some(u64::MAX),
            },
            testnet: Totals {
                mtd_microusd: Some(u64::MAX),
                projected_microusd: Some(u64::MAX),
            },
            failures: u32::MAX,
            period_secs: Some(u64::MAX),
        };
        let len = serde_json::to_vec(&u).unwrap().len() as u64;
        assert!(len <= upload_bound(now), "{len} > {}", upload_bound(now));
        // Exact but for the two lists' last separators and `alert_any`'s
        // `true` (the bound counts every boolean as `false`).
        assert_eq!(upload_bound(now) - len, 3);
        assert_eq!(u.exit_code.to_string().len(), 3);
        assert_eq!(u.failures.to_string().len(), 10);
        assert_eq!(
            months_through(1_793_627_107_000),
            3,
            "2026-10, 2026-11 and one ahead"
        );
    }

    #[test]
    fn figures() {
        assert_eq!(usd(0), "$0.00");
        assert_eq!(usd(12_345_678), "$12.34");
        assert_eq!(usd(1_234_567_890_123), "$1,234,567.89");
        let mut u = upload(&[]);
        u.acked = vec![
            Acked {
                month: "2026-10".into(),
                acked_at: "2026-11-01T00:00:00Z".into(),
            },
            Acked {
                month: "2026-10".into(),
                acked_at: "2026-11-03T00:00:00Z".into(),
            },
        ];
        assert_eq!(
            acked_ms(&u, "2026-10"),
            crate::mime::parse_rfc3339("2026-11-03T00:00:00Z")
        );
        assert_eq!(acked_ms(&u, "2026-11"), None);
    }
}

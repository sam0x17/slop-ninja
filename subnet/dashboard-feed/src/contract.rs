//! The `dashboard/v1` object grammar and byte bounds (rule
//! `slop_ninja.dashboard.read_bounds/v1`), in one file that both the
//! publisher (which writes the objects) and the dashboard feed Worker (which
//! serves them to the website through a service binding) compile verbatim:
//! the publisher includes it with `#[path]`, so the two can never disagree on
//! a key or a bound. Plain Rust, no dependencies.
//!
//! Keys are `dashboard/v1/` followed by exactly one of:
//!
//! - `index.json`: the mechanism registry;
//! - `m/<key>/now.json`: the current cohort, clock and best model;
//! - `m/<key>/cohorts/index.json`: the listed cohorts;
//! - `m/<key>/cohorts/<label>.json`: one cohort;
//! - `m/<key>/months/<YYYY-MM>.json`: one UTC month (phase P3).
//!
//! `<key>` is a mechanism key (`a`, `b`, `c`). `<label>` follows the
//! publisher's `layout::safe_label` (`[a-z0-9._-]`, 1 to 4096 bytes, never
//! `.` or `..`), and is never `index`, matching the website's `paths.rs`.
//! `<YYYY-MM>` is four digits, a hyphen and a month from `01` to `12`.
//!
//! Each bound follows from bounded fields and is versioned with the
//! contract, never tuned; the website's `data::bounds` carries the same
//! values:
//!
//! - `index` and `now`: fixed fields plus a few protocol strings of at most
//!   4,096 bytes (`MAX_STRING_LEN`): 64 KiB;
//! - a cohort document: a 64 KiB fixed part (header, phases, plan, band
//!   list) plus one row of at most 4 KiB per entrant for up to 4,096
//!   entrants;
//! - the cohort index: a 64 KiB fixed part plus about 1 KiB a cohort row for
//!   16,384 rows (decades at one cohort a day);
//! - a month: it lists at most the cohorts the index lists, each with its
//!   stage changes, so it takes the index bound.

/// Every dashboard key starts with this prefix.
pub const PREFIX: &str = "dashboard/v1/";

/// Mechanism keys: A (detection), B (rewriting), C (shared grammar).
pub const MECHANISMS: &[&str] = &["a", "b", "c"];

/// The longest label the protocol encodes (`MAX_STRING_LEN`).
pub const MAX_LABEL_LEN: usize = 4096;

/// The bounds rule these values belong to.
pub const BOUNDS_RULE: &str = "slop_ninja.dashboard.read_bounds/v1";

pub mod bounds {
    /// `index.json` and `m/<key>/now.json`.
    pub const SMALL: usize = 64 * 1024;
    /// `m/<key>/cohorts/<label>.json`: 4,096 entrant rows of 4 KiB.
    pub const ROSTER: usize = 64 * 1024 + 4096 * 4096;
    /// `m/<key>/cohorts/index.json`: 16,384 rows of 1 KiB.
    pub const INDEX: usize = 64 * 1024 + 16384 * 1024;
    /// `m/<key>/months/<YYYY-MM>.json`: at most the index's cohorts.
    pub const MONTH: usize = INDEX;
}

/// A cohort label, by the publisher's `layout::safe_label` rule.
pub fn safe_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= MAX_LABEL_LEN
        && label != "."
        && label != ".."
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

fn mechanism(segment: &str) -> Option<&'static str> {
    MECHANISMS.iter().copied().find(|m| *m == segment)
}

/// One `dashboard/v1` object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Doc {
    Index,
    Now(&'static str),
    Cohorts(&'static str),
    Cohort(&'static str, String),
    /// Year and month (1 to 12).
    Month(&'static str, u16, u8),
}

impl Doc {
    /// The object's key, built from the parsed parts only.
    pub fn key(&self) -> String {
        match self {
            Doc::Index => format!("{PREFIX}index.json"),
            Doc::Now(m) => format!("{PREFIX}m/{m}/now.json"),
            Doc::Cohorts(m) => format!("{PREFIX}m/{m}/cohorts/index.json"),
            Doc::Cohort(m, label) => format!("{PREFIX}m/{m}/cohorts/{label}.json"),
            Doc::Month(m, y, mo) => format!("{PREFIX}m/{m}/months/{y:04}-{mo:02}.json"),
        }
    }

    /// The largest object of this kind, in bytes.
    pub fn bound(&self) -> usize {
        match self {
            Doc::Index | Doc::Now(_) => bounds::SMALL,
            Doc::Cohort(..) => bounds::ROSTER,
            Doc::Cohorts(_) => bounds::INDEX,
            Doc::Month(..) => bounds::MONTH,
        }
    }

    /// Cache lifetime in seconds: the live documents are rewritten every
    /// minute, cohort and month documents only when their content changes.
    pub fn max_age(&self) -> u32 {
        match self {
            Doc::Cohort(..) | Doc::Month(..) => 300,
            _ => 60,
        }
    }

    /// The schema id a document of this kind carries.
    pub fn schema(&self) -> &'static str {
        match self {
            Doc::Index => "slop_ninja.dashboard.index/v1",
            Doc::Now(_) => "slop_ninja.dashboard.now/v1",
            Doc::Cohorts(_) => "slop_ninja.dashboard.cohorts/v1",
            Doc::Cohort(..) => "slop_ninja.dashboard.cohort/v1",
            Doc::Month(..) => "slop_ninja.dashboard.month/v1",
        }
    }

    /// Parses an object key (`dashboard/v1/...`, no leading slash). Anything
    /// outside the grammar is `None`.
    pub fn parse_key(key: &str) -> Option<Doc> {
        let rest = key.strip_prefix(PREFIX)?;
        let parts: Vec<&str> = rest.split('/').collect();
        Some(match parts.as_slice() {
            ["index.json"] => Doc::Index,
            ["m", m, "now.json"] => Doc::Now(mechanism(m)?),
            ["m", m, "cohorts", "index.json"] => Doc::Cohorts(mechanism(m)?),
            ["m", m, "cohorts", file] => {
                let label = file.strip_suffix(".json")?;
                if !safe_label(label) || label == "index" {
                    return None;
                }
                Doc::Cohort(mechanism(m)?, label.to_owned())
            }
            ["m", m, "months", file] => {
                let ym = file.strip_suffix(".json")?.as_bytes();
                let digits = |s: &[u8]| s.iter().all(u8::is_ascii_digit);
                if ym.len() != 7 || ym[4] != b'-' || !digits(&ym[..4]) || !digits(&ym[5..]) {
                    return None;
                }
                let year = std::str::from_utf8(&ym[..4]).ok()?.parse().ok()?;
                let month: u8 = std::str::from_utf8(&ym[5..]).ok()?.parse().ok()?;
                if !(1..=12).contains(&month) {
                    return None;
                }
                Doc::Month(mechanism(m)?, year, month)
            }
            _ => return None,
        })
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_bounds_follow_the_kind() {
        for (key, bound, age) in [
            ("dashboard/v1/index.json", bounds::SMALL, 60),
            ("dashboard/v1/m/a/now.json", bounds::SMALL, 60),
            ("dashboard/v1/m/b/cohorts/index.json", bounds::INDEX, 60),
            ("dashboard/v1/m/a/cohorts/testnet-r0-c12.json", bounds::ROSTER, 300),
            ("dashboard/v1/m/c/months/2026-11.json", bounds::MONTH, 300),
        ] {
            let d = Doc::parse_key(key).unwrap_or_else(|| panic!("{key}"));
            assert_eq!(d.key(), key);
            assert_eq!(d.bound(), bound);
            assert_eq!(d.max_age(), age);
        }
    }

    #[test]
    fn keys_outside_the_grammar_are_refused() {
        for bad in [
            "",
            "dashboard/v1/",
            "dashboard/v2/index.json",
            "/dashboard/v1/index.json",
            "dashboard/v1/m/d/now.json",
            "dashboard/v1/m/A/now.json",
            "dashboard/v1/m/a/cohorts/index",
            "dashboard/v1/m/a/cohorts/..json",
            "dashboard/v1/m/a/cohorts/.json",
            "dashboard/v1/m/a/cohorts/Bad.json",
            "dashboard/v1/m/a/cohorts/a%2Fb.json",
            "dashboard/v1/m/a/cohorts/x/y.json",
            "dashboard/v1/m/a/cohorts/../now.json",
            "dashboard/v1/m/a/months/2026-13.json",
            "dashboard/v1/m/a/months/2026-00.json",
            "dashboard/v1/m/a/months/2026-1.json",
            "dashboard/v1/m/a/months/26-11.json",
            "dashboard/v1/m/a/months/2026_11.json",
            "dashboard/v1/m/a/now.json/",
            "dashboard/v1//index.json",
            "v1/status.json",
            "dashboard/v1/m/a/now.json?x=1",
        ] {
            assert_eq!(Doc::parse_key(bad), None, "{bad:?}");
        }
        let long = format!("dashboard/v1/m/a/cohorts/{}.json", "a".repeat(MAX_LABEL_LEN + 1));
        assert_eq!(Doc::parse_key(&long), None);
    }
}

//! The `dashboard/v1` object grammar and byte bounds (rule
//! `slop_ninja.dashboard.read_bounds/v3`), in one file that both the
//! publisher (which writes the objects) and the dashboard feed Worker (which
//! serves them to the website through a service binding) compile verbatim:
//! the publisher includes it with `#[path]`, so the two can never disagree on
//! a key or a bound. Plain Rust, no dependencies.
//!
//! Keys are `dashboard/v1/` followed by exactly one of:
//!
//! - `index.json`: the mechanism registry;
//! - `registry.json`: the subnet's registered UIDs (rule v3);
//! - `m/<key>/now.json`: the current cohort, clock and best model;
//! - `m/<key>/cohorts/index.json`: the listed cohorts;
//! - `m/<key>/cohorts/<label>.json`: one cohort;
//! - `m/<key>/rosters/<label>.json`: one cohort's per-hotkey status (rule
//!   v3);
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
//! - `index`: fixed fields plus a few protocol strings of at most 4,096
//!   bytes (`MAX_STRING_LEN`): 64 KiB;
//! - `now`: the same 64 KiB plus the UID map, at most [`uid_map::ROWS`]
//!   rows of at most [`uid_map::ROW_BYTES`] (v2);
//! - a cohort document: a 64 KiB fixed part (header, phases, plan, band
//!   list) plus one row of at most 4 KiB per entrant for up to 4,096
//!   entrants;
//! - the cohort index: a 64 KiB fixed part plus about 1 KiB a cohort row for
//!   16,384 rows (decades at one cohort a day);
//! - a month: it lists at most the cohorts the index lists, each with its
//!   stage changes, so it takes the index bound;
//! - `registry`: the 64 KiB fixed part plus at most [`registry::ROWS`]
//!   rows of at most [`registry::ROW_BYTES`] (v3);
//! - a roster: the 64 KiB fixed part plus at most [`roster::ROWS`] rows
//!   (every registered UID and every former entrant) of at most
//!   [`roster::ROW_BYTES`] (v3).

/// Every dashboard key starts with this prefix.
pub const PREFIX: &str = "dashboard/v1/";

/// Mechanism keys: A (detection), B (rewriting), C (shared grammar).
pub const MECHANISMS: &[&str] = &["a", "b", "c"];

/// The longest label the protocol encodes (`MAX_STRING_LEN`).
pub const MAX_LABEL_LEN: usize = 4096;

/// The bounds rule these values belong to.
pub const BOUNDS_RULE: &str = "slop_ninja.dashboard.read_bounds/v3";

/// The `now` document's UID-to-hotkey map (`uids.rows[]`).
pub mod uid_map {
    /// The most rows: the chain's initial `MaxAllowedUids` (256 in the
    /// subtensor runtime the project pins; the testnet subnet's recorded
    /// value). The coordinator refuses a larger map whole rather than
    /// truncating it (`slop_ninja.eval.uid_map/v1`), and the publisher and
    /// the website drop one.
    pub const ROWS: usize = 256;
    /// The longest SS58 address of a 32-byte account: a two-byte network
    /// prefix, the account and a two-byte checksum (36 bytes) in base58.
    pub const SS58_MAX_LEN: usize = 50;
    /// The longest row with its separator: a five-digit UID and the longest
    /// address.
    pub const ROW_BYTES: usize = r#"{"uid":65535,"hotkey":""},"#.len() + SS58_MAX_LEN;
}

/// `registry.json`'s rows (`rows[]`): one per registered UID.
pub mod registry {
    /// At most one row per UID the chain allows: the UID map's bound. The
    /// coordinator refuses a larger read whole
    /// (`slop_ninja.coordinator.registry/v1`).
    pub const ROWS: usize = super::uid_map::ROWS;
    /// The longest row with its separator: a five-digit UID, the longest
    /// SS58 address, 64 hex characters and a twenty-digit block.
    pub const ROW_BYTES: usize = r#"{"uid":65535,"hotkey":"","hotkey_hex":"","registration_block":18446744073709551615},"#
        .len()
        + super::uid_map::SS58_MAX_LEN
        + 64;
}

/// A roster document's rows (`rows[]`): one per hotkey.
pub mod roster {
    /// Every registered UID (the UID map's bound) plus every former entrant
    /// of the cohort that is no longer registered: at most the protocol's
    /// roster bound (`records_v2::MAX_ROSTER`, 256; the publisher's tests
    /// check the two agree).
    pub const ROWS: usize = super::uid_map::ROWS + ENTRANTS;
    /// The most entrants a cohort admits: the protocol's roster bound
    /// (`records_v2::MAX_ROSTER`); also the most rows with challenge counts.
    pub const ENTRANTS: usize = 256;
    /// A row with every field at its longest is under 600 bytes (two SS58
    /// addresses, 64 hex characters, closed codes, three integers and the
    /// challenge counts' closed state and two ten-digit integers).
    pub const ROW_BYTES: usize = 1024;
}

pub mod bounds {
    /// `index.json`.
    pub const SMALL: usize = 64 * 1024;
    /// `m/<key>/now.json`: the small bound plus the UID map (v2).
    pub const NOW: usize = SMALL + super::uid_map::ROWS * super::uid_map::ROW_BYTES;
    /// `m/<key>/cohorts/<label>.json`: 4,096 entrant rows of 4 KiB.
    pub const ROSTER: usize = 64 * 1024 + 4096 * 4096;
    /// `m/<key>/cohorts/index.json`: 16,384 rows of 1 KiB.
    pub const INDEX: usize = 64 * 1024 + 16384 * 1024;
    /// `m/<key>/months/<YYYY-MM>.json`: at most the index's cohorts.
    pub const MONTH: usize = INDEX;
    /// `registry.json` (v3).
    pub const REGISTRY: usize = SMALL + super::registry::ROWS * super::registry::ROW_BYTES;
    /// `m/<key>/rosters/<label>.json` (v3).
    pub const ROSTER_STATUS: usize = SMALL + super::roster::ROWS * super::roster::ROW_BYTES;
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
    /// The subnet's registered UIDs (rule v3).
    Registry,
    Now(&'static str),
    Cohorts(&'static str),
    Cohort(&'static str, String),
    /// Year and month (1 to 12).
    Month(&'static str, u16, u8),
    /// One cohort's per-hotkey status (rule v3).
    Roster(&'static str, String),
}

impl Doc {
    /// The object's key, built from the parsed parts only.
    pub fn key(&self) -> String {
        match self {
            Doc::Index => format!("{PREFIX}index.json"),
            Doc::Registry => format!("{PREFIX}registry.json"),
            Doc::Now(m) => format!("{PREFIX}m/{m}/now.json"),
            Doc::Cohorts(m) => format!("{PREFIX}m/{m}/cohorts/index.json"),
            Doc::Cohort(m, label) => format!("{PREFIX}m/{m}/cohorts/{label}.json"),
            Doc::Month(m, y, mo) => format!("{PREFIX}m/{m}/months/{y:04}-{mo:02}.json"),
            Doc::Roster(m, label) => format!("{PREFIX}m/{m}/rosters/{label}.json"),
        }
    }

    /// The largest object of this kind, in bytes.
    pub fn bound(&self) -> usize {
        match self {
            Doc::Index => bounds::SMALL,
            Doc::Now(_) => bounds::NOW,
            Doc::Cohort(..) => bounds::ROSTER,
            Doc::Cohorts(_) => bounds::INDEX,
            Doc::Month(..) => bounds::MONTH,
            Doc::Registry => bounds::REGISTRY,
            Doc::Roster(..) => bounds::ROSTER_STATUS,
        }
    }

    /// Cache lifetime in seconds: the live documents are rewritten every
    /// minute, cohort and month documents only when their content changes.
    /// A roster is live while its cohort runs; the key cannot tell a closed
    /// cohort's roster apart, so every roster takes the live lifetime.
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
            Doc::Registry => "slop_ninja.dashboard.registry/v1",
            Doc::Roster(..) => "slop_ninja.dashboard.roster/v1",
        }
    }

    /// Parses an object key (`dashboard/v1/...`, no leading slash). Anything
    /// outside the grammar is `None`.
    pub fn parse_key(key: &str) -> Option<Doc> {
        let rest = key.strip_prefix(PREFIX)?;
        let parts: Vec<&str> = rest.split('/').collect();
        Some(match parts.as_slice() {
            ["index.json"] => Doc::Index,
            ["registry.json"] => Doc::Registry,
            ["m", m, "now.json"] => Doc::Now(mechanism(m)?),
            ["m", m, "cohorts", "index.json"] => Doc::Cohorts(mechanism(m)?),
            ["m", m, "cohorts", file] => {
                let label = file.strip_suffix(".json")?;
                if !safe_label(label) || label == "index" {
                    return None;
                }
                Doc::Cohort(mechanism(m)?, label.to_owned())
            }
            ["m", m, "rosters", file] => {
                let label = file.strip_suffix(".json")?;
                if !safe_label(label) || label == "index" {
                    return None;
                }
                Doc::Roster(mechanism(m)?, label.to_owned())
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
            ("dashboard/v1/m/a/now.json", bounds::NOW, 60),
            ("dashboard/v1/m/b/cohorts/index.json", bounds::INDEX, 60),
            ("dashboard/v1/m/a/cohorts/testnet-r0-c12.json", bounds::ROSTER, 300),
            ("dashboard/v1/m/c/months/2026-11.json", bounds::MONTH, 300),
            ("dashboard/v1/registry.json", bounds::REGISTRY, 60),
            ("dashboard/v1/m/a/rosters/testnet-r0-20.json", bounds::ROSTER_STATUS, 60),
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
            "dashboard/v1/registry",
            "dashboard/v1/m/a/registry.json",
            "dashboard/v1/m/a/rosters/index.json",
            "dashboard/v1/m/a/rosters/.json",
            "dashboard/v1/m/a/rosters/Bad.json",
            "dashboard/v1/m/a/rosters/x/y.json",
            "dashboard/v1/m/z/rosters/c1.json",
        ] {
            assert_eq!(Doc::parse_key(bad), None, "{bad:?}");
        }
        let long = format!("dashboard/v1/m/a/cohorts/{}.json", "a".repeat(MAX_LABEL_LEN + 1));
        assert_eq!(Doc::parse_key(&long), None);
    }

    #[test]
    fn the_v3_bounds_hold_their_full_row_sets() {
        assert_eq!(registry::ROW_BYTES, 198);
        assert_eq!(bounds::REGISTRY, 64 * 1024 + 256 * 198);
        assert_eq!(roster::ROWS, 512);
        assert_eq!(bounds::ROSTER_STATUS, 64 * 1024 + 512 * 1024);
    }

    #[test]
    fn the_now_bound_holds_a_full_uid_map() {
        assert_eq!(uid_map::ROW_BYTES, 76);
        assert_eq!(bounds::NOW, 64 * 1024 + 256 * 76);
    }
}

//! The kill switches as one request reads them: the deploy-time var (the
//! hard off) AND the D1 `switches` row (flipped at once by `set_switch`), by
//! the page contract's rule (`slop_ninja_detect_page::switches::effective`).
//!
//! A request reads every switch it needs in one D1 statement
//! ([`select_sql`]), so a flip takes effect on the next request and needs no
//! cache. When the row cannot be read (D1 unreachable, the binding missing,
//! the read past its wait), the var decides: its hard off still holds and
//! a D1 outage never stops free checks. The Worker then writes an AE
//! `error {d1}` point ([`Switches::d1_failed`]).

use slop_ninja_detect_page::switches::{self, Switch};

/// The switches the free check reads.
pub const SCORE: &[&str] = &[TOOL, SPANS, URLS, X];
/// The switches the warm answer reads.
pub const WARM: &[&str] = &[TOOL, SPANS];

pub const TOOL: &str = "DETECT_TOOL";
pub const SPANS: &str = "DETECT_SPANS";
pub const URLS: &str = "DETECT_URLS";
pub const X: &str = "DETECT_X";

/// The one statement that reads `n` switch rows (`n` bound names).
pub fn select_sql(n: usize) -> String {
    let marks = vec!["?"; n].join(", ");
    format!("SELECT name, state FROM switches WHERE name IN ({marks})")
}

/// One `switches` row.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct Row {
    pub name: String,
    pub state: String,
}

/// The effective states of the switches one request read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Switches {
    states: Vec<(&'static str, &'static str)>,
    d1_failed: bool,
}

impl Switches {
    /// The states of `names` from their vars and, when the read worked,
    /// the rows (`None`: the rows could not be read). A name the contract
    /// does not know reads as off.
    pub fn read(names: &[&str], var: impl Fn(&str) -> Option<String>, rows: Option<&[Row]>) -> Switches {
        let states = names
            .iter()
            .filter_map(|n| switches::switch(n))
            .map(|sw| (sw.name, state_of(sw.name, var(sw.name).as_deref(), rows)))
            .collect();
        Switches { states, d1_failed: rows.is_none() }
    }

    /// A switch's effective state (its off state when it was not read).
    pub fn state(&self, name: &str) -> &'static str {
        self.states
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, s)| *s)
            .or_else(|| switches::switch(name).map(|sw| sw.off))
            .unwrap_or("off")
    }

    /// Whether a switch is `on`.
    pub fn on(&self, name: &str) -> bool {
        self.state(name) == "on"
    }

    /// The rows could not be read, so the vars decided.
    pub fn d1_failed(&self) -> bool {
        self.d1_failed
    }
}

/// The effective state of switch `name` from its var and the rows read
/// (`None`: they could not be read, so the var decides). The one rule every
/// reader of the switches uses, the free check's and the store's
/// ([`crate::lookup::SwitchRows`]). A name the contract does not know reads
/// as off.
pub fn state_of(name: &str, var: Option<&str>, rows: Option<&[Row]>) -> &'static str {
    let Some(sw) = switches::switch(name) else { return "off" };
    let row = rows.and_then(|r| r.iter().find(|r| r.name == sw.name)).map(|r| r.state.as_str());
    switches::effective(sw, var.unwrap_or("").trim(), row)
}

/// A switch's state from its var alone: the hard off, for code that reads no
/// row (the gate object's admission answer).
pub fn var_state(sw: &Switch, var: Option<&str>) -> &'static str {
    switches::effective(sw, var.unwrap_or("").trim(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, state: &str) -> Row {
        Row { name: name.into(), state: state.into() }
    }

    fn vars(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |n| pairs.iter().find(|(k, _)| *k == n).map(|(_, v)| (*v).to_owned())
    }

    #[test]
    fn the_row_and_the_var_both_hold() {
        let on = vars(&[("DETECT_TOOL", "on"), ("DETECT_SPANS", "on"), ("DETECT_URLS", "on"), ("DETECT_X", "on")]);
        let s = Switches::read(SCORE, &on, Some(&[]));
        assert!(s.on(TOOL) && s.on(SPANS) && s.on(URLS) && s.on(X) && !s.d1_failed());
        // A row flips a switch off while its var allows it.
        let s = Switches::read(SCORE, &on, Some(&[row("DETECT_TOOL", "off"), row("DETECT_X", "purged")]));
        assert!(!s.on(TOOL) && s.on(SPANS));
        assert_eq!(s.state(X), "purged");
        // The var's hard off wins over a row that says on.
        let off = vars(&[("DETECT_TOOL", "off"), ("DETECT_X", "off")]);
        let s = Switches::read(SCORE, &off, Some(&[row("DETECT_TOOL", "on"), row("DETECT_X", "on")]));
        assert!(!s.on(TOOL) && !s.on(X));
        // ... but not over `purged`, which goes beyond the off.
        let s = Switches::read(SCORE, &off, Some(&[row("DETECT_X", "purged")]));
        assert_eq!(s.state(X), "purged");
    }

    /// Δ18: an unreadable row leaves the var to decide, so free checks go on.
    #[test]
    fn a_d1_outage_never_stops_free_checks() {
        let on = vars(&[("DETECT_TOOL", "on"), ("DETECT_SPANS", "on")]);
        let s = Switches::read(SCORE, &on, None);
        assert!(s.d1_failed());
        assert!(s.on(TOOL) && s.on(SPANS));
        // Unset vars take each switch's default: the tool on, X post links off.
        let s = Switches::read(SCORE, |_| None, None);
        assert!(s.on(TOOL) && s.on(SPANS) && s.on(URLS));
        assert_eq!(s.state(X), "off");
        // The hard off still holds.
        let s = Switches::read(SCORE, vars(&[("DETECT_TOOL", "off")]), None);
        assert!(!s.on(TOOL));
    }

    #[test]
    fn unreadable_values_read_as_off() {
        // A var typo forces the switch off; a row naming no state is ignored.
        let s = Switches::read(
            SCORE,
            vars(&[("DETECT_TOOL", "Off"), ("DETECT_SPANS", "on")]),
            Some(&[row("DETECT_SPANS", "maybe")]),
        );
        assert!(!s.on(TOOL) && s.on(SPANS));
        // A var cannot name `purged`: only the row holds it.
        let s = Switches::read(SCORE, vars(&[("DETECT_X", "purged")]), Some(&[]));
        assert_eq!(s.state(X), "off");
        // A switch that was not read, or that the contract does not know.
        let s = Switches::read(WARM, |_| None, Some(&[]));
        assert_eq!(s.state(URLS), "off");
        assert_eq!(s.state("NO_SUCH"), "off");
    }

    #[test]
    fn whitespace_around_a_var_is_ignored() {
        let s = Switches::read(SCORE, vars(&[("DETECT_TOOL", " off\n")]), Some(&[]));
        assert!(!s.on(TOOL));
        assert_eq!(var_state(switches::switch(TOOL).unwrap(), Some(" off ")), "off");
        assert_eq!(var_state(switches::switch(TOOL).unwrap(), None), "on");
    }

    #[test]
    fn one_statement_reads_them_all() {
        assert_eq!(select_sql(4), "SELECT name, state FROM switches WHERE name IN (?, ?, ?, ?)");
        for n in SCORE.iter().chain(WARM) {
            assert!(switches::switch(n).is_some(), "{n} is a contract switch");
        }
    }

    /// The statement runs against the store's own schema.
    #[test]
    fn the_statement_reads_the_store() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        let schema =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations/0001_detect_store.sql")).unwrap();
        c.execute_batch(&schema).unwrap();
        c.execute("INSERT INTO switches (name, state, at, journal_seq) VALUES ('DETECT_TOOL', 'off', 1, 1)", [])
            .unwrap();
        c.execute("INSERT INTO switches (name, state, at, journal_seq) VALUES ('ACCOUNTS', 'read_only', 1, 2)", [])
            .unwrap();
        let mut st = c.prepare(&select_sql(SCORE.len())).unwrap();
        let rows: Vec<Row> = st
            .query_map(rusqlite::params_from_iter(SCORE.iter()), |r| Ok(Row { name: r.get(0)?, state: r.get(1)? }))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows, vec![row("DETECT_TOOL", "off")]);
        let s = Switches::read(SCORE, |_| None, Some(&rows));
        assert!(!s.on(TOOL) && s.on(SPANS));
    }
}

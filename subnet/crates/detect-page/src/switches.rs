//! The kill switches.
//!
//! Each switch is the AND of a deploy-time var (the hard off) and a D1
//! `switches` row (flipped at once by the `set_switch` op). Both are
//! journaled and each has a revert. When the row cannot be read, the var
//! decides, so a D1 outage never stops free checks.

/// The Worker whose store holds a switch's row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Detect,
    Account,
}

/// One switch: its name, owner, states and the state that means "off".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Switch {
    pub name: &'static str,
    pub owner: Owner,
    /// The states it takes; the first is its default.
    pub states: &'static [&'static str],
    /// The hard-off state a var can force.
    pub off: &'static str,
}

const ON_OFF: &[&str] = &["on", "off"];
const OFF_ON: &[&str] = &["off", "on"];

/// Every switch.
pub const SWITCHES: &[Switch] = &[
    Switch { name: "DETECT_TOOL", owner: Owner::Detect, states: ON_OFF, off: "off" },
    Switch { name: "DETECT_SPANS", owner: Owner::Detect, states: ON_OFF, off: "off" },
    Switch { name: "DETECT_STORE", owner: Owner::Detect, states: ON_OFF, off: "off" },
    Switch { name: "DETECT_URLS", owner: Owner::Detect, states: ON_OFF, off: "off" },
    Switch { name: "DETECT_PAGES", owner: Owner::Detect, states: ON_OFF, off: "off" },
    Switch { name: "DETECT_CARDS", owner: Owner::Detect, states: &["normal", "generic"], off: "generic" },
    Switch { name: "DETECT_INDEX", owner: Owner::Detect, states: ON_OFF, off: "off" },
    Switch { name: "DETECT_EXCERPTS", owner: Owner::Detect, states: ON_OFF, off: "off" },
    Switch { name: "DETECT_EXPLAIN", owner: Owner::Detect, states: OFF_ON, off: "off" },
    Switch { name: "DETECT_PAID", owner: Owner::Detect, states: OFF_ON, off: "off" },
    Switch { name: "X_DEVELOPER", owner: Owner::Detect, states: &["closed", "open"], off: "closed" },
    Switch { name: "ADV16", owner: Owner::Detect, states: &["pending", "cleared"], off: "pending" },
    Switch { name: "EDGE_PRECLEARANCE", owner: Owner::Detect, states: OFF_ON, off: "off" },
    Switch { name: "ACCOUNTS", owner: Owner::Account, states: &["on", "read_only"], off: "read_only" },
    Switch { name: "auth.email", owner: Owner::Account, states: ON_OFF, off: "off" },
    Switch { name: "auth.google", owner: Owner::Account, states: ON_OFF, off: "off" },
    Switch { name: "auth.github", owner: Owner::Account, states: ON_OFF, off: "off" },
    Switch { name: "auth.x", owner: Owner::Account, states: ON_OFF, off: "off" },
    Switch { name: "auth.bittensor", owner: Owner::Account, states: ON_OFF, off: "off" },
    Switch { name: "BILLING", owner: Owner::Account, states: &["off", "grants", "live"], off: "off" },
];

/// The switch of a name.
pub fn switch(name: &str) -> Option<&'static Switch> {
    SWITCHES.iter().find(|s| s.name == name)
}

/// The effective state: the var's hard off wins; otherwise the D1 row, or
/// the var when the row is missing or unreadable. Unknown values fall back
/// to the var.
pub fn effective(sw: &Switch, var: &str, row: Option<&str>) -> &'static str {
    let var = sw.states.iter().copied().find(|s| *s == var).unwrap_or(sw.states[0]);
    if var == sw.off {
        return var;
    }
    match row.and_then(|r| sw.states.iter().copied().find(|s| *s == r)) {
        Some(r) => r,
        None => var,
    }
}

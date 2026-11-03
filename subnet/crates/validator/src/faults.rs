//! Fault-injection hooks at startup and application journal phase boundaries.
//!
//! Unit tests arm a hook with [`Faults::at`]; an injected fault then returns
//! [`InjectedFault`] and the caller performs no further journal writes.
//!
//! Testnet drills (shortcut T13) arm one hook per process with
//! `SLOP_NINJA_DRILL_FAULT=<phase>[:<occurrence>]`, only in a build with the
//! `testnet-drills` feature and only when the configured network is `test`.
//! An armed drill hook is a crash: the process exits with status
//! [`EXIT_DRILL_FAULT`] at once, without pausing the journal, closing the RPC
//! client or running destructors. Builds without the feature refuse to start
//! when the variable is set, so a drill never silently runs unarmed.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::NetworkName;

/// Environment variable that arms one drill fault.
pub const DRILL_FAULT_ENV: &str = "SLOP_NINJA_DRILL_FAULT";
/// Exit status of an injected drill crash.
pub const EXIT_DRILL_FAULT: i32 = 86;

/// Boundaries after a durable journal effect, or before the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// The journal is open and its pause is persisted.
    JournalOpened,
    /// One bounded recovery call has persisted its cursors.
    RecoveryRound,
    /// The subnet, hotkey uid and commit/reveal checks passed at finalized
    /// state; nothing new is persisted.
    CommitRevealChecked,
    /// Live-metadata readiness was evaluated (and persisted if it changed);
    /// the journal is still paused. This stage never halts startup.
    RuntimeSurfaceEvaluated,
    /// The explicit resume is persisted.
    Resumed,
    /// The signed package verified; nothing new is persisted.
    PackageVerified,
    /// The row was derived from the package and finalized registrations;
    /// nothing new is persisted.
    RowDerived,
    /// The row is persisted as journal work (created or already retained).
    Enqueued,
    /// One `dispatch_finalized` call returned. rtao persisted any attempt
    /// before its network I/O, so a crash here or inside the call leaves at
    /// most an unresolved attempt for finalized recovery.
    Dispatched,
    /// A `dispatch_finalized` call that added a signed attempt to the tracked
    /// operation returned: the commit was submitted and rtao recorded its
    /// outcome, and this process has not yet read it back.
    Submitted,
    /// A per-tempo re-commit was persisted as journal work.
    TempoEnqueued,
}

impl Phase {
    pub fn parse(name: &str) -> Result<Self> {
        serde_json::from_value(serde_json::Value::String(name.into()))
            .map_err(|_| anyhow::anyhow!("unknown fault phase {name:?}"))
    }
}

#[derive(Default)]
pub struct Faults {
    #[cfg(any(test, feature = "testnet-drills"))]
    armed: Option<(Phase, u32)>,
    #[cfg(any(test, feature = "testnet-drills"))]
    seen: std::cell::Cell<u32>,
    /// Exit the process instead of returning an error (drills).
    #[cfg(any(test, feature = "testnet-drills"))]
    crash: bool,
}

impl Faults {
    pub fn none() -> Self {
        Self::default()
    }

    /// Fail at the `occurrence`th (1-based) arrival at `phase`.
    #[cfg(test)]
    pub fn at(phase: Phase, occurrence: u32) -> Self {
        Self {
            armed: Some((phase, occurrence)),
            seen: std::cell::Cell::new(0),
            crash: false,
        }
    }

    /// Parse `<phase>[:<occurrence>]`.
    pub fn parse_spec(spec: &str) -> Result<(Phase, u32)> {
        let (name, occurrence) = spec.split_once(':').unwrap_or((spec, "1"));
        let occurrence: u32 = occurrence
            .parse()
            .map_err(|_| anyhow::anyhow!("fault occurrence {occurrence:?} is not a count"))?;
        anyhow::ensure!(occurrence >= 1, "fault occurrence counts from 1");
        Ok((Phase::parse(name)?, occurrence))
    }

    /// The drill hook from [`DRILL_FAULT_ENV`], honored only on the test network.
    #[cfg(feature = "testnet-drills")]
    pub fn from_env(network: NetworkName) -> Result<Self> {
        let Some(value) = std::env::var_os(DRILL_FAULT_ENV) else {
            return Ok(Self::none());
        };
        anyhow::ensure!(
            network == NetworkName::Test,
            "{DRILL_FAULT_ENV} is honored only on the test network"
        );
        let spec = value
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("{DRILL_FAULT_ENV} is not UTF-8"))?;
        let (phase, occurrence) = Self::parse_spec(spec)?;
        tracing::warn!(event = "drill_fault_armed", phase = ?phase, occurrence);
        Ok(Self {
            armed: Some((phase, occurrence)),
            seen: std::cell::Cell::new(0),
            crash: true,
        })
    }

    /// Without the `testnet-drills` feature no hook can be armed; a set
    /// variable is refused rather than ignored.
    #[cfg(not(feature = "testnet-drills"))]
    pub fn from_env(_: NetworkName) -> Result<Self> {
        anyhow::ensure!(
            std::env::var_os(DRILL_FAULT_ENV).is_none(),
            "{DRILL_FAULT_ENV} needs a build with the testnet-drills feature"
        );
        Ok(Self::none())
    }

    #[cfg(any(test, feature = "testnet-drills"))]
    pub fn hit(&self, phase: Phase) -> Result<()> {
        if let Some((armed, occurrence)) = self.armed
            && armed == phase
        {
            let seen = self.seen.get() + 1;
            self.seen.set(seen);
            if seen == occurrence {
                if self.crash {
                    tracing::warn!(event = "drill_fault_injected", phase = ?phase, occurrence);
                    std::process::exit(EXIT_DRILL_FAULT);
                }
                anyhow::bail!(InjectedFault(phase));
            }
        }
        Ok(())
    }

    #[cfg(not(any(test, feature = "testnet-drills")))]
    #[inline(always)]
    pub fn hit(&self, _: Phase) -> Result<()> {
        Ok(())
    }
}

/// Error marker for an injected crash.
#[derive(Debug, PartialEq, Eq)]
pub struct InjectedFault(pub Phase);

impl std::fmt::Display for InjectedFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "injected fault at {:?}", self.0)
    }
}

impl std::error::Error for InjectedFault {}

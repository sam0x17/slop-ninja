//! Which row each reveal must match.
//!
//! After a commit's reveal the chain's `Weights(netuid, uid)` must equal the
//! committed (max-upscaled) row. A dispatch run compares every reveal it can
//! attribute, including the reveal of a commit an earlier process made: the
//! last old-target commit before a supersession, or one made before a
//! restart. The row comes from the [`RevealLedger`], kept beside the journal
//! as `<journal>.reveal-rows.json` (mode 600): each enqueued operation's
//! committed row and the row hash the journal's correlation records for it.
//! An entry is used only while the journal still retains that operation with
//! the same row hash.
//!
//! A reveal is attributed by its commit epoch. rtao's completed reveal
//! outcomes carry the epoch that received the commit; an entry learns its
//! commit epoch from the journal's finalized inclusion block, through rtao's
//! outstanding reveal for that block (`reveal_epoch - RevealPeriodEpochs`)
//! or, when the reveal already completed, from `SubnetEpochIndex` at that
//! block. Outcomes are matched by key, not by counting the watcher's
//! 256-entry ring, so a full ring still compares every new reveal.
//!
//! A reveal no entry accounts for (a commit made by a binary that kept no
//! ledger, or under another configuration) is logged as observed, never as a
//! mismatch: comparing it with the current target's row would halt a
//! validator whose weights are right. A missed reveal is logged with
//! `alert = true`. Only a reveal whose own row is known can be a mismatch.
//!
//! The ledger holds the operations not yet revealed: an entry leaves when its
//! reveal is compared or missed, when the journal retires its operation or no
//! longer retains it, or when its reveal epoch has passed with no outcome in
//! the watcher (logged with `alert = true`). That is at most a few entries.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const LEDGER_SCHEMA: &str = "slop_ninja.validator_reveal_rows/v1";

/// One enqueued operation's committed row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowEntry {
    pub operation_id: String,
    /// The row hash the journal's correlation records for the operation.
    pub row_hash: String,
    /// The committed form of the row, which `Weights` must equal after its
    /// reveal.
    pub entries: Vec<(u16, u16)>,
    /// Finalized inclusion block of the operation's successful commit.
    #[serde(default)]
    pub inclusion_block: Option<u64>,
    /// The subnet epoch that received the commit.
    #[serde(default)]
    pub commit_epoch: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerFile {
    schema: String,
    entries: Vec<RowEntry>,
}

/// The journal's view of a ledger entry's operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpState {
    /// Not retained, or retained with another row hash.
    Gone,
    /// Retained, not yet landed.
    Pending,
    /// Handed back without a finalized success: retired, it never commits.
    Retired,
    /// Finalized successfully at this block.
    Landed(u64),
}

/// A completed reveal, as rtao's watcher reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completed {
    pub commit_epoch: u64,
    pub reveal_epoch: u64,
    pub round: u64,
    pub observed_block: u64,
    pub missed: bool,
}

impl Completed {
    fn key(&self) -> (u64, u64) {
        (self.commit_epoch, self.round)
    }
}

/// What a completed reveal is compared with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// Compare `Weights` at the reveal with this operation's row.
    Compare { entry: RowEntry, outcome: Completed },
    /// The reveal epoch ended without a success event.
    Missed { entry: RowEntry, outcome: Completed },
    /// No entry accounts for this reveal: observed only.
    Unattributed(Completed),
}

/// See the module documentation.
#[derive(Debug)]
pub struct RevealLedger {
    path: PathBuf,
    entries: Vec<RowEntry>,
    /// Outcomes already accounted for, by (commit epoch, round); trimmed to
    /// the watcher's retained outcomes.
    seen: BTreeSet<(u64, u64)>,
    /// Whether the outcomes present when the run began were taken in.
    primed: bool,
    dirty: bool,
}

/// `<journal>.reveal-rows.json`.
pub fn ledger_path(journal: &Path) -> PathBuf {
    let mut name = journal
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".reveal-rows.json");
    journal.with_file_name(name)
}

impl RevealLedger {
    /// Loads the ledger; a missing file is an empty ledger, and an unreadable
    /// one is set aside (renamed with `.unreadable`) and replaced, since its
    /// only use is attributing reveals.
    pub fn load(path: PathBuf) -> Self {
        let entries = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<LedgerFile>(&bytes) {
                Ok(file) if file.schema == LEDGER_SCHEMA => file.entries,
                _ => {
                    tracing::warn!(event = "reveal_ledger_unreadable", alert = true);
                    let mut aside = path.clone().into_os_string();
                    aside.push(".unreadable");
                    let _ = std::fs::rename(&path, aside);
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        Self {
            path,
            entries,
            seen: BTreeSet::new(),
            primed: false,
            dirty: false,
        }
    }

    pub fn entries(&self) -> &[RowEntry] {
        &self.entries
    }

    /// Records an enqueued operation's committed row (once per operation).
    pub fn record(&mut self, operation_id: &str, row_hash: &str, entries: Vec<(u16, u16)>) {
        match self
            .entries
            .iter_mut()
            .find(|e| e.operation_id == operation_id)
        {
            Some(existing) if existing.row_hash == row_hash => {}
            Some(existing) => {
                *existing = RowEntry {
                    operation_id: operation_id.to_owned(),
                    row_hash: row_hash.to_owned(),
                    entries,
                    inclusion_block: None,
                    commit_epoch: None,
                };
                self.dirty = true;
            }
            None => {
                self.entries.push(RowEntry {
                    operation_id: operation_id.to_owned(),
                    row_hash: row_hash.to_owned(),
                    entries,
                    inclusion_block: None,
                    commit_epoch: None,
                });
                self.dirty = true;
            }
        }
    }

    /// Settles each entry against the journal: drops entries whose operation
    /// is gone or retired, and records the inclusion block of landed ones.
    pub fn settle(&mut self, state: impl Fn(&RowEntry) -> Result<OpState>) -> Result<()> {
        let mut keep = Vec::with_capacity(self.entries.len());
        for mut entry in std::mem::take(&mut self.entries) {
            match state(&entry)? {
                OpState::Gone | OpState::Retired => {
                    tracing::info!(event = "reveal_row_dropped", operation_id = %entry.operation_id, reason = "not_committed");
                    self.dirty = true;
                    continue;
                }
                OpState::Pending => {}
                OpState::Landed(block) => {
                    if entry.inclusion_block != Some(block) {
                        entry.inclusion_block = Some(block);
                        entry.commit_epoch = None;
                        self.dirty = true;
                    }
                }
            }
            keep.push(entry);
        }
        self.entries = keep;
        Ok(())
    }

    /// Commit epochs from rtao's outstanding reveals, `(inclusion block,
    /// reveal epoch)`, for landed entries that have none yet. Returns the
    /// inclusion blocks of landed entries whose reveal is neither
    /// outstanding nor epoch-known: their reveal completed before this
    /// process saw it, so their commit epoch must be read at the block.
    pub fn assign_epochs(&mut self, outstanding: &[(u64, Option<u64>)], period: u64) -> Vec<u64> {
        let mut unknown = Vec::new();
        for entry in &mut self.entries {
            let (Some(block), None) = (entry.inclusion_block, entry.commit_epoch) else {
                continue;
            };
            match outstanding.iter().find(|(b, _)| *b == block) {
                Some((_, Some(reveal_epoch))) => {
                    entry.commit_epoch = reveal_epoch.checked_sub(period);
                    self.dirty = true;
                }
                // Located later by the watcher.
                Some((_, None)) => {}
                None => unknown.push(block),
            }
        }
        unknown
    }

    /// The commit epoch read at an entry's inclusion block.
    pub fn set_epoch(&mut self, inclusion_block: u64, commit_epoch: u64) {
        for entry in &mut self.entries {
            if entry.inclusion_block == Some(inclusion_block) && entry.commit_epoch.is_none() {
                entry.commit_epoch = Some(commit_epoch);
                self.dirty = true;
            }
        }
    }

    /// The checks the watcher's completed outcomes call for, oldest first.
    /// An entry is taken out when its outcome is found. Outcomes present
    /// when the run began are compared when an entry accounts for them and
    /// are otherwise taken in silently (they predate this process).
    pub fn checks(&mut self, outcomes: &[Completed]) -> Vec<Check> {
        let mut checks = Vec::new();
        for outcome in outcomes {
            if self.seen.contains(&outcome.key()) {
                continue;
            }
            let found = self
                .entries
                .iter()
                .position(|e| e.commit_epoch == Some(outcome.commit_epoch));
            match found {
                Some(index) => {
                    let entry = self.entries.remove(index);
                    self.dirty = true;
                    self.seen.insert(outcome.key());
                    checks.push(if outcome.missed {
                        Check::Missed {
                            entry,
                            outcome: *outcome,
                        }
                    } else {
                        Check::Compare {
                            entry,
                            outcome: *outcome,
                        }
                    });
                }
                // An entry whose commit epoch is not known yet may still
                // claim it: wait while any landed entry lacks one.
                None if self
                    .entries
                    .iter()
                    .any(|e| e.inclusion_block.is_some() && e.commit_epoch.is_none()) => {}
                None => {
                    self.seen.insert(outcome.key());
                    if self.primed {
                        checks.push(Check::Unattributed(*outcome));
                    }
                }
            }
        }
        self.primed = true;
        let retained: BTreeSet<_> = outcomes.iter().map(Completed::key).collect();
        self.seen.retain(|key| retained.contains(key));
        checks
    }

    /// Entries whose reveal epoch (`commit_epoch + period`) is before
    /// `newest_reveal_epoch`, the newest completed outcome's: the watcher
    /// completes reveal epochs in order and records an outcome, success or
    /// miss, for each, so it will never report these. Removed and returned.
    pub fn expire(&mut self, newest_reveal_epoch: u64, period: u64) -> Vec<RowEntry> {
        let (stale, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|e| {
                e.commit_epoch
                    .and_then(|c| c.checked_add(period))
                    .is_some_and(|reveal| reveal < newest_reveal_epoch)
            });
        if !stale.is_empty() {
            self.dirty = true;
        }
        self.entries = keep;
        stale
    }

    /// Writes the ledger when it changed (mode 600, atomically).
    pub fn persist(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let file = LedgerFile {
            schema: LEDGER_SCHEMA.into(),
            entries: self.entries.clone(),
        };
        crate::supersede::write_private_atomic(&self.path, &serde_json::to_vec_pretty(&file)?)
            .context("reveal ledger write")?;
        self.dirty = false;
        Ok(())
    }
}

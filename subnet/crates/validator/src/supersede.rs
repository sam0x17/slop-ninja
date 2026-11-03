//! When a fetched bundle replaces the installed one.
//!
//! A fetched bundle is installed (written over `[package] path`) only once it
//! verified completely against this validator's held target; a successor
//! whose records are still partial on the host never displaces the installed
//! bundle, so the old target keeps its per-tempo commitments meanwhile.
//!
//! [`decide`] compares the verified candidate with the installed bundle:
//!
//! - the same bytes: nothing to do;
//! - the same target with a longer lease, or one that covers the finalized
//!   epoch where the installed one no longer does (a new per-cohort
//!   authorization, a renewal, a newer carrier): a refresh. Operation IDs
//!   bind the target, not the authorization, so a refresh never changes the
//!   operation a commitment belongs to;
//! - another target (the verified successor of the held target, possibly
//!   after intermediate targets): a switch at the eligible boundary, the
//!   finalized epoch at or after the successor's activation. A switch waits
//!   while any pending operation of the mechanism has an attempt that is not
//!   settled (unresolved, or not yet classified by finalized recovery); work
//!   that never signed is retired as `superseded`. No new commitment of the
//!   old target is queued while a switch waits.
//!
//! A staged bundle whose target activates no later than the installed one's
//! is never installed (`regression`): a stale or rolled-back pointer cannot
//! move the validator back to a superseded target.
//!
//! A switch retires never-signed work, writes the held-target record past
//! any checked intermediate targets, installs the bundle and then records the
//! successor's own activation, so a stale pointer read right after the switch
//! is verified against the successor and refused. (The bundle is written
//! before the successor's record so that a crash between the two leaves an
//! installed successor that still verifies against the held record, which
//! the next run's verification then moves forward itself.) When the held
//! record must first move past intermediate targets, the bundle is staged
//! durably beside the installed one before that record is written: a crash
//! between the record and the install leaves a staged successor that the
//! next start completes ([`InstallPorts::stage_bundle`]), never an installed
//! bundle older than the held record.
//!
//! [`FetchState`] is the dispatch loop's use of these rules: it receives each
//! background session's outcome, re-decides the newest verified bundle at
//! every poll (an unchanged one too, since its eligibility moves with the
//! epoch), re-checks a waiting switch at every finalized block without a
//! chain read, and reports whether the run refreshed its authorization or
//! switched to a successor.

use anyhow::{Context, Result};
use serde::Serialize;

use crate::fetch::{BundleInfo, Outcome, Staged};

/// The facts [`decide`] compares, for one verified bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub sha256: String,
    pub target: String,
    pub activated_epoch: Option<u64>,
    /// Last epoch the authorization or its renewal covers.
    pub lease_end: u64,
    /// Whether the authorization yields an operation at the finalized epoch.
    pub covers_now: bool,
}

impl Candidate {
    pub fn of(sha256: &str, info: &BundleInfo, covers_now: bool) -> Self {
        Self {
            sha256: sha256.to_owned(),
            target: info.target.clone(),
            activated_epoch: info.activated_epoch,
            lease_end: info.lease_end,
            covers_now,
        }
    }
}

/// A pending operation of the mechanism, as the journal reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Pending {
    pub id: String,
    /// Never landed and never can (no unresolved attempt, no finalized
    /// success): retiring it is safe.
    pub retirable: bool,
}

impl From<&slop_ninja_subnet::PendingOperation> for Pending {
    fn from(op: &slop_ninja_subnet::PendingOperation) -> Self {
        Self {
            id: op.id.clone(),
            retirable: op.retirable(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// Keep the installed bundle.
    Keep { reason: &'static str },
    /// Same target, better authorization: install it in place.
    Refresh,
    /// A successor is eligible but a commitment is in flight.
    Wait { in_flight: Vec<String> },
    /// Retire these operations (they never signed), install, and restart on
    /// the successor.
    Switch { retire: Vec<String> },
}

/// See the module documentation. `installed` is `None` when no bundle is
/// installed or the installed one no longer verifies against the held target.
pub fn decide(
    installed: Option<&Candidate>,
    staged: &Candidate,
    finalized_epoch: u64,
    pending: &[Pending],
) -> Decision {
    if let Some(current) = installed {
        if current.sha256 == staged.sha256 {
            return Decision::Keep {
                reason: "unchanged",
            };
        }
        if current.target == staged.target {
            return if staged.lease_end > current.lease_end
                || (staged.covers_now && !current.covers_now)
            {
                Decision::Refresh
            } else {
                Decision::Keep {
                    reason: "lease_not_longer",
                }
            };
        }
        if !activates_later(current.activated_epoch, staged.activated_epoch) {
            return Decision::Keep {
                reason: "regression",
            };
        }
    }
    if staged
        .activated_epoch
        .is_some_and(|epoch| epoch > finalized_epoch)
    {
        return Decision::Keep {
            reason: "successor_not_yet_active",
        };
    }
    let in_flight: Vec<String> = pending
        .iter()
        .filter(|p| !p.retirable)
        .map(|p| p.id.clone())
        .collect();
    if !in_flight.is_empty() {
        return Decision::Wait { in_flight };
    }
    Decision::Switch {
        retire: pending.iter().map(|p| p.id.clone()).collect(),
    }
}

/// Whether a target activated at `staged` comes after one activated at
/// `installed` (`None` is the launch burn row, before every activation).
fn activates_later(installed: Option<u64>, staged: Option<u64>) -> bool {
    match (installed, staged) {
        (None, Some(_)) => true,
        (Some(a), Some(b)) => b > a,
        (_, None) => false,
    }
}

/// Writes `bytes` to `path` with mode 600 through a temporary sibling,
/// fsync and rename, then fsyncs the directory: a crash leaves the old or
/// the new file, never a torn one.
pub fn write_private_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = path.parent().context("path has no directory")?;
    let mut tmp_name = path
        .file_name()
        .context("path has no file name")?
        .to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .context("temporary file")?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path).context("rename")?;
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// Effects of an install, so tests can substitute fakes.
pub trait InstallPorts {
    /// Journaled retirement of an operation that never signed.
    fn retire(&mut self, operation_id: &str, reason: &str) -> Result<bool>;
    /// Durably moves the held-target record forward.
    fn record_target(&mut self, held: &crate::package_v2::HeldTargetFile) -> Result<()>;
    /// Durably stages a verified successor beside the installed bundle
    /// before the held record moves past intermediate targets. The next
    /// start installs a staged bundle that verifies against the held record
    /// and discards one that does not (the record never moved).
    fn stage_bundle(&mut self, bytes: &[u8]) -> Result<()>;
    /// Atomically replaces the installed bundle, then drops any staged copy.
    fn write_bundle(&mut self, bytes: &[u8]) -> Result<()>;
}

/// Applies a switch or refresh. A switch retires the never-signed work first
/// (refusing if any of it turns out not to be retirable), then (staging the
/// bundle first) moves the held record past checked intermediates, installs
/// the bundle and records the successor's activation.
pub fn install<P: InstallPorts>(
    ports: &mut P,
    staged: &Staged,
    decision: &Decision,
) -> Result<bool> {
    match decision {
        Decision::Keep { .. } | Decision::Wait { .. } => Ok(false),
        Decision::Refresh => {
            ports.write_bundle(&staged.bundle)?;
            Ok(true)
        }
        Decision::Switch { retire } => {
            for id in retire {
                if !ports.retire(id, "superseded")? {
                    anyhow::bail!(
                        "pending_operation_unsettled: a pending operation could not be retired"
                    );
                }
            }
            if let Some(advance) = &staged.advance {
                ports.stage_bundle(&staged.bundle)?;
                ports.record_target(advance)?;
            }
            ports.write_bundle(&staged.bundle)?;
            ports.record_target(&crate::package_v2::HeldTargetFile::of(
                &staged.authorized.verified.active,
            ))?;
            Ok(true)
        }
    }
}

/// What the dispatch loop does after [`FetchState::step`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopStep {
    /// Nothing to change.
    Continue,
    /// The same target's authorization was installed in place: later
    /// re-commits are derived under it.
    Refreshed,
    /// A successor was installed: the run continues on it.
    Switched,
}

/// The dispatch loop's effects for [`FetchState`], so tests can substitute
/// fakes for the chain and the journal.
#[allow(async_fn_in_trait)]
pub trait LoopPorts: InstallPorts {
    /// `SubnetEpochIndex` at finality.
    async fn finalized_epoch(&mut self) -> Result<u64>;
    /// Pending operations of the mechanism, any target.
    fn pending(&self) -> Result<Vec<Pending>>;
    /// SHA-256 of the installed bundle, when one is installed.
    fn installed_sha(&self) -> Option<String>;
    /// The installed bundle's facts at `epoch`, or `None` when none is
    /// installed or it no longer verifies against the held target.
    fn installed(&mut self, epoch: u64) -> Result<Option<Candidate>>;
    /// Whether `staged`'s authorization yields an operation at `epoch`.
    fn covers(&self, staged: &Staged, epoch: u64) -> bool;
}

/// The dispatch loop's view of fetched bundles. See the module documentation.
#[derive(Debug, Default)]
pub struct FetchState {
    /// The newest bundle a session verified.
    latest: Option<Box<Staged>>,
    /// The epoch a successor was found eligible at while work was in flight.
    waiting: Option<u64>,
    /// The last decision or outcome logged, so a repeat is quiet.
    last_logged: String,
    /// The last session's outcome code, for the run's report.
    last_code: String,
}

impl FetchState {
    /// A successor found eligible with work in flight before the loop began
    /// (the startup fetch): the loop queues no old-target commitment until it
    /// switches.
    pub fn seeded(staged: Box<Staged>, decision: &Decision, epoch: u64) -> Self {
        Self {
            waiting: matches!(decision, Decision::Wait { .. }).then_some(epoch),
            latest: Some(staged),
            last_logged: String::new(),
            last_code: "ready".into(),
        }
    }

    /// Whether a switch waits for in-flight work: no new commitment of the
    /// old target may be queued meanwhile.
    pub fn waiting(&self) -> bool {
        self.waiting.is_some()
    }

    /// The last session's outcome code (`ready`, `unchanged`, `unpublished`,
    /// a refusal code, ...), empty before the first.
    pub fn last_code(&self) -> &str {
        &self.last_code
    }

    /// One finalized block of the dispatch loop, with the outcome the
    /// background fetcher sent since the last block, if any. A new outcome
    /// re-decides the newest verified bundle (one finalized-epoch read); a
    /// block without one re-checks only a waiting switch, at the epoch it was
    /// found eligible (epochs only grow, so no chain read is needed).
    pub async fn step<P: LoopPorts>(
        &mut self,
        ports: &mut P,
        outcome: Option<Outcome>,
    ) -> Result<LoopStep> {
        let epoch = match outcome {
            None => match self.waiting {
                Some(epoch) => epoch,
                None => return Ok(LoopStep::Continue),
            },
            Some(outcome) => {
                self.last_code = outcome.code().to_owned();
                match outcome {
                    Outcome::Ready(staged) => self.latest = Some(staged),
                    Outcome::Unchanged => {}
                    other => {
                        self.note(&other);
                        if self.waiting.is_none() {
                            return Ok(LoopStep::Continue);
                        }
                    }
                }
                let Some(staged) = &self.latest else {
                    return Ok(LoopStep::Continue);
                };
                // An installed bundle needs no chain read to recognize.
                if ports.installed_sha().as_deref() == Some(staged.sha256.as_str()) {
                    self.waiting = None;
                    return Ok(LoopStep::Continue);
                }
                ports.finalized_epoch().await?
            }
        };
        let Some(staged) = &self.latest else {
            self.waiting = None;
            return Ok(LoopStep::Continue);
        };
        let installed = ports.installed(epoch)?;
        let candidate = Candidate::of(&staged.sha256, &staged.info, ports.covers(staged, epoch));
        let decision = decide(installed.as_ref(), &candidate, epoch, &ports.pending()?);
        let text = serde_json::to_string(&decision).unwrap_or_default();
        if text != self.last_logged {
            tracing::info!(
                event = "fetch_decision",
                decision = %text,
                epoch,
                target = %staged.info.target,
                lease_end = staged.info.lease_end
            );
            self.last_logged = text;
        }
        let installed_now = install(ports, staged, &decision)?;
        self.waiting = match &decision {
            Decision::Wait { .. } => Some(self.waiting.unwrap_or(epoch)),
            _ => None,
        };
        Ok(match decision {
            Decision::Refresh if installed_now => LoopStep::Refreshed,
            Decision::Switch { .. } if installed_now => {
                tracing::info!(event = "supersession_installed", target = %staged.info.target, epoch);
                LoopStep::Switched
            }
            _ => LoopStep::Continue,
        })
    }

    /// The installed bundle's verification, once a refresh installed it.
    pub fn latest(&self) -> Option<&Staged> {
        self.latest.as_deref()
    }

    /// Logs an outcome that produced no bundle once per change: a refusal or
    /// a digest failure at `warn` with `alert = true` (records the host
    /// served failed a check), anything else at `info`.
    fn note(&mut self, outcome: &Outcome) {
        let code = outcome.code();
        if code == self.last_logged {
            return;
        }
        if outcome.alert() {
            tracing::warn!(event = "fetch_refused", outcome = code, alert = true);
        } else {
            tracing::info!(event = "fetch_kept", outcome = code);
        }
        self.last_logged = code.to_owned();
    }
}

//! Intake diagnosis: one hotkey's answers to a miner's questions about the
//! admitting (or current) cohort, its state, the next step and the one
//! action the client may take by itself.
//!
//! The questions (status page and `status` share them): am I registered
//! (UID, hotkey); was my commitment seen; did my upload verify; am I
//! admitted; have my challenges started; how many are done.
//!
//! The inputs are what the client read, each optional: the public status API
//! ([`crate::status_api`], unsigned, about a minute behind), the
//! coordinator's unsigned cohort read, its signed view of the hotkey (open
//! only to allow-listed hotkeys until admission opens to every miner), the
//! chain at the finalized head (registration and the hotkey's commitment),
//! the state directory's openings and the artifact named on the command
//! line. [`diagnose`] is pure: it reads no clock, network or file.
//!
//! What the client may do by itself ([`Action`]) is bounded by these rules:
//!
//! - it never spends funds: registration burns TAO from the coldkey, which
//!   the client never loads, so a missing registration is a manual step;
//! - it never changes the submitted model: a commitment for an artifact
//!   other than the one this hotkey last committed for the cohort, or one
//!   that replaces a counted commitment this state directory cannot open,
//!   needs `allow_replace` (the `--allow-replace` flag);
//! - it commits only from the chain's own facts (a commitment the public
//!   status has not shown yet may only be feed lag), only while the
//!   coordinator's cohort read says the cohort admits, and only when the
//!   cutoff's earliest block is more than the commit's own landing bound
//!   away, so a commitment never lands after the cutoff;
//! - uploads, verification and repairs are idempotent (the session id is the
//!   envelope's digest), so they are taken whenever the state calls for them.

use std::path::PathBuf;

use serde::Serialize;
use slop_ninja_admission_wire::ErrorCode;
use slop_ninja_admission_wire::json::{CohortInfo, HotkeyView, SessionSummary};
use slop_ninja_protocol::NeuronIdentity;

use crate::object::ObjectHeader;
use crate::opening::OpeningFile;
use crate::status_api::{ApiStatus, MinerRow, ReadError};

/// The chain at the finalized head, for one hotkey.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainFacts {
    /// The genesis of the chain that answered, when the access reports it.
    pub genesis: Option<slop_ninja_protocol::Digest>,
    pub finalized_block: u64,
    pub registration: Option<NeuronIdentity>,
    /// The hotkey's commitment in finalized state: its block and payload.
    pub commitment: Option<(u64, Vec<u8>)>,
}

/// What the client read.
#[derive(Clone, Copy, Debug)]
pub struct Inputs<'a> {
    /// The hotkey, SS58.
    pub hotkey: &'a str,
    pub api: Option<&'a ApiStatus>,
    pub api_error: Option<&'a ReadError>,
    pub cohort_read: Option<&'a CohortInfo>,
    pub view: Option<&'a HotkeyView>,
    pub chain: Option<&'a ChainFacts>,
    /// This hotkey's openings (any cohort), oldest first.
    pub openings: &'a [(PathBuf, OpeningFile)],
    pub artifact: Option<&'a ObjectHeader>,
    /// The coordinator's signed routes answered this hotkey `not_found` or
    /// `forbidden`: they are not open to it yet, so an upload cannot follow
    /// a commitment.
    pub signed_closed: bool,
    /// Unix seconds, for estimated times.
    pub now: u64,
}

/// What the client is allowed to do.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// Commit for another artifact than this hotkey's last opening for the
    /// cohort, or over a counted commitment no local opening opens.
    pub allow_replace: bool,
    /// Blocks a commitment may take from signing to inclusion: its
    /// submission deadline at the nominal block time.
    pub commit_margin_blocks: u64,
    /// The run reads the chain (`resolve`). `status` does not: it shows the
    /// action `resolve` would take once it has read the chain.
    pub reads_chain: bool,
}

/// One answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    Yes,
    No,
    /// Not yet: it can still happen without the miner doing anything.
    Pending,
    /// The inputs do not say.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Answer {
    pub answer: Mark,
    pub detail: String,
}

fn ans(answer: Mark, detail: impl Into<String>) -> Answer {
    Answer {
        answer,
        detail: detail.into(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Answers {
    pub registered: Answer,
    pub commitment_seen: Answer,
    pub upload_verified: Answer,
    pub admitted: Answer,
    pub challenges_started: Answer,
    pub challenges_done: Answer,
}

/// The hotkey's intake state for the cohort: each failure state names its
/// next step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Neither the status API nor the coordinator could be read.
    StatusUnavailable,
    /// No cohort admits or runs; the next one has not opened.
    NoCohort,
    /// The coordinator reports a pause: the cutoff moves later, never earlier.
    Paused,
    NotRegistered,
    /// Admission is open and no commitment of this hotkey counts for it.
    NotEntered,
    /// This client committed; the coordinator or the feed has not shown it.
    CommitPending,
    /// The hotkey's commitment is not one this state directory can open.
    CommitmentNotLocal,
    /// The commitment that counts is for another artifact than the one named.
    OtherArtifact,
    /// The commitment that counts was made outside the cohort's window.
    CommitmentOutsideWindow,
    /// The opening names a registration the hotkey no longer has.
    StaleRegistration,
    Committed,
    Uploading,
    Verifying,
    Verified,
    RefusedCommitment,
    RefusedArtifact,
    RefusedCutoff,
    RefusedRegistration,
    Overflow,
    /// Admission closed before this hotkey's entry completed.
    MissedCutoff,
    RepairNeeded,
    Admitted,
    Evaluating,
    Scored,
    Withheld,
    NotExecuted,
    /// The inputs do not decide.
    Unknown,
}

impl State {
    /// The serde name.
    pub fn code(self) -> &'static str {
        use State::*;
        match self {
            StatusUnavailable => "status_unavailable",
            NoCohort => "no_cohort",
            Paused => "paused",
            NotRegistered => "not_registered",
            NotEntered => "not_entered",
            CommitPending => "commit_pending",
            CommitmentNotLocal => "commitment_not_local",
            OtherArtifact => "other_artifact",
            CommitmentOutsideWindow => "commitment_outside_window",
            StaleRegistration => "stale_registration",
            Committed => "committed",
            Uploading => "uploading",
            Verifying => "verifying",
            Verified => "verified",
            RefusedCommitment => "refused_commitment",
            RefusedArtifact => "refused_artifact",
            RefusedCutoff => "refused_cutoff",
            RefusedRegistration => "refused_registration",
            Overflow => "overflow",
            MissedCutoff => "missed_cutoff",
            RepairNeeded => "repair_needed",
            Admitted => "admitted",
            Evaluating => "evaluating",
            Scored => "scored",
            Withheld => "withheld",
            NotExecuted => "not_executed",
            Unknown => "unknown",
        }
    }

    /// Whether intake for this cohort needs nothing more from the client
    /// (a watch loop stops here).
    pub fn settled(self) -> bool {
        use State::*;
        matches!(
            self,
            Verified
                | Admitted
                | Evaluating
                | Scored
                | Withheld
                | NotExecuted
                | Overflow
                | RefusedCutoff
                | RefusedRegistration
                | MissedCutoff
        )
    }
}

/// The client's own next action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Nothing for the client to do now.
    None,
    /// Read again later: transient trouble, a pause, feed lag, or a
    /// coordinator decision still to come.
    Wait,
    /// Make a fresh opening for the admitting cohort and commit it.
    /// `replace`: an opening for the cohort exists (it is superseded).
    Commit { cohort: u64, replace: bool },
    /// Open or resume the upload of this opening: PUTs with fresh URLs,
    /// completion, and the wait for verification.
    Upload { opening: PathBuf },
    /// Send again the recorded objects that went missing on the
    /// coordinator's side.
    Repair { session: String },
}

/// The diagnosis of one hotkey.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diagnosis {
    pub hotkey: String,
    pub cohort: Option<u64>,
    pub label: Option<String>,
    pub state: State,
    pub answers: Answers,
    /// What to do next, in words.
    pub next: String,
    pub action: Action,
    /// Why the client does not take the action the state calls for by
    /// itself, when it does not.
    pub blocked: Option<String>,
    pub warnings: Vec<String>,
    /// The cutoff's earliest block, when known: a commitment re-checks it
    /// against the finalized head just before signing.
    pub cutoff_block: Option<u64>,
}

/// Where the cohort's admission stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Window {
    Open,
    /// Admission closed; the roster is not frozen yet.
    Closing,
    /// The roster is frozen (scoring, packaging, publication, close).
    Frozen,
    Closed,
    NoCohort,
    Unknown,
}

/// The cohort the diagnosis is about.
#[derive(Clone, Debug)]
struct Target {
    cohort: Option<u64>,
    label: Option<String>,
    window: Window,
    open_block: Option<u64>,
    /// The cutoff's earliest block (a lower bound).
    cutoff_block: Option<u64>,
    /// The public row is about this cohort.
    row_applies: bool,
}

fn tempo_blocks(api: Option<&ApiStatus>) -> u64 {
    api.and_then(|a| a.clock.as_ref()?.tempo_blocks)
        .filter(|t| *t > 0)
        .unwrap_or(360)
}

fn stage_window(stage: &str, state: Option<&str>) -> Window {
    match stage {
        "admission_open" => Window::Open,
        "admission_closing" => Window::Closing,
        "generating" | "scoring" | "packaging" | "published" | "publication" | "transition" => {
            Window::Frozen
        }
        "closed" | "ended" => Window::Closed,
        "idle" | "queued" => Window::NoCohort,
        _ if matches!(state, Some("closed" | "ended" | "retired_unpublished")) => Window::Closed,
        _ if matches!(state, Some("idle" | "queued")) => Window::NoCohort,
        _ if matches!(state, Some("running")) => Window::Frozen,
        _ => Window::Unknown,
    }
}

fn target(inputs: &Inputs<'_>) -> Target {
    let api_cohort = inputs.api.and_then(|a| a.cohort.as_ref());
    let api_number = api_cohort.and_then(|c| c.cohort);
    let api_window = api_cohort
        .map(|c| stage_window(c.stage.as_deref().unwrap_or(""), c.state.as_deref()))
        .unwrap_or(Window::Unknown);
    // The feed's earliest end of admission_open is the cutoff's earliest
    // block while the stage is open; an observed cutoff once it closed.
    let api_cutoff = api_cohort.and_then(|c| {
        if api_window == Window::Open {
            c.expected.as_ref()?.stage_end.as_ref()?.block
        } else {
            c.bounds.as_ref()?.cutoff.as_ref()?.block
        }
    });
    let api_open = api_cohort.and_then(|c| c.bounds.as_ref()?.open.as_ref()?.block);
    if let Some(read) = inputs.cohort_read {
        if let Some(a) = &read.admitting {
            let epochs = a.planned_cutoff_epoch.saturating_sub(a.opened_epoch);
            let from_read = a
                .open_block
                .saturating_add(epochs.saturating_mul(tempo_blocks(inputs.api)));
            let same = api_number == Some(a.cohort);
            let cutoff = match (same.then_some(api_cutoff).flatten(), a.closing) {
                (Some(api), false) => Some(api.min(from_read)),
                _ => Some(from_read),
            };
            return Target {
                cohort: Some(a.cohort),
                label: Some(a.label.clone()),
                window: if a.closing {
                    Window::Closing
                } else {
                    Window::Open
                },
                open_block: Some(a.open_block),
                cutoff_block: cutoff,
                row_applies: same,
            };
        }
        // The coordinator admits nothing: the feed's cohort, if it runs past
        // admission, is the one to describe; never an open window.
        let window = match api_window {
            Window::Open | Window::Unknown if api_number.is_some() => Window::Closing,
            Window::Open | Window::Unknown => Window::NoCohort,
            other => other,
        };
        return Target {
            cohort: api_number,
            label: api_cohort.and_then(|c| c.label.clone()),
            window,
            open_block: api_open,
            cutoff_block: api_cutoff,
            row_applies: api_number.is_some(),
        };
    }
    Target {
        cohort: api_number,
        label: api_cohort.and_then(|c| c.label.clone()),
        window: if inputs.api.is_some() {
            api_window
        } else {
            Window::Unknown
        },
        open_block: api_open,
        cutoff_block: api_cutoff,
        row_applies: api_number.is_some(),
    }
}

/// The best known current block.
fn current_block(inputs: &Inputs<'_>) -> Option<u64> {
    [
        inputs.chain.map(|c| c.finalized_block),
        inputs.view.map(|v| v.finalized_block),
        inputs.cohort_read.map(|r| r.finalized_block),
        inputs.api.and_then(|a| a.clock.as_ref()?.confirmed_block),
    ]
    .into_iter()
    .flatten()
    .max()
}

/// The cohort a 44-byte chain payload names (`"sna1" || u64_le(cohort) ||
/// digest`).
pub fn payload_cohort(payload: &[u8]) -> Option<u64> {
    if payload.len() != slop_ninja_protocol::submission::SUBMISSION_PAYLOAD_LEN
        || &payload[..4] != b"sna1"
    {
        return None;
    }
    Some(u64::from_le_bytes(payload[4..12].try_into().ok()?))
}

/// The commitment that counts, as the best source sees it.
#[derive(Clone, Debug, Default)]
struct Commit {
    /// Known to exist for this cohort (by chain, the signed view or the feed).
    seen: Option<bool>,
    block: Option<u64>,
    in_window: Option<bool>,
    /// The local opening whose payload it is.
    ours: Option<usize>,
    /// The payload is known (chain or signed view), so `ours: None` means
    /// no local opening opens it.
    identified: bool,
    source: &'static str,
}

fn commitment(inputs: &Inputs<'_>, t: &Target, row: Option<&MinerRow>) -> Commit {
    let cohort = t.cohort;
    let local = |payload_hex: &str| {
        inputs
            .openings
            .iter()
            .position(|(_, f)| f.chain_payload == payload_hex && Some(f.cohort) == cohort)
    };
    if let Some(chain) = inputs.chain {
        return match &chain.commitment {
            Some((block, payload)) if payload_cohort(payload) == cohort && cohort.is_some() => {
                let in_window = match (t.open_block, t.cutoff_block, t.window) {
                    (Some(open), _, Window::Open) => Some(*block >= open),
                    (Some(open), Some(cut), _) => Some(*block >= open && *block < cut),
                    (Some(open), None, _) => Some(*block >= open),
                    _ => None,
                };
                Commit {
                    seen: Some(true),
                    block: Some(*block),
                    in_window,
                    ours: local(&hex::encode(payload)),
                    identified: true,
                    source: "chain",
                }
            }
            // None, or one for another cohort: nothing counts for this one.
            _ => Commit {
                seen: Some(false),
                identified: true,
                source: "chain",
                ..Commit::default()
            },
        };
    }
    if let Some(view) = inputs.view.filter(|v| Some(v.cohort) == cohort) {
        return match &view.commitment {
            Some(c) if c.payload_cohort == cohort || c.payload_cohort.is_none() => Commit {
                seen: Some(true),
                block: Some(c.block),
                in_window: Some(c.in_window),
                ours: local(&c.payload),
                identified: true,
                source: "coordinator",
            },
            _ => Commit {
                seen: Some(false),
                identified: true,
                source: "coordinator",
                ..Commit::default()
            },
        };
    }
    let status = row.and_then(|r| r.cohort_status.as_deref());
    match status {
        Some("none") => Commit {
            seen: Some(false),
            source: "status API",
            ..Commit::default()
        },
        Some(_) => Commit {
            seen: Some(true),
            in_window: Some(true),
            source: "status API",
            ..Commit::default()
        },
        None => Commit::default(),
    }
}

/// The session that still decides something, by state priority.
///
/// A session decides only while it belongs to the commitment that counts
/// now. Refused sessions are never superseded (the coordinator supersedes
/// only live ones, when a newer commitment's session opens), so after a
/// fresh commitment an old refusal stays in the view. When the counted
/// commitment is one of this state directory's openings and its session id
/// is known (the envelope's digest), only that session decides; when the
/// counted commitment is known and none of ours, the sessions of our other
/// openings no longer decide. Roster facts (`admitted`, `overflow`) always
/// decide. Otherwise (the commitment is not identified, or the opening is a
/// v1 file naming no header) every session decides, by state priority.
fn session<'a>(
    view: Option<&'a HotkeyView>,
    cohort: Option<u64>,
    commit: &Commit,
    openings: &[(PathBuf, OpeningFile)],
    artifact: Option<&ObjectHeader>,
) -> Option<&'a SessionSummary> {
    let view = view.filter(|v| Some(v.cohort) == cohort)?;
    // A commitment outside the window does not count, so it says nothing
    // about which session is current.
    let counts = commit.identified && commit.in_window != Some(false);
    // A v1 file names no header; the named artifact fills it in when the
    // opening commits to its content (as `upload` migrates it).
    let ids: Vec<Option<String>> = openings
        .iter()
        .map(|(_, f)| {
            (Some(f.cohort) == cohort)
                .then(|| {
                    f.session_id().or_else(|| {
                        artifact
                            .and_then(|a| f.migrated(a))
                            .and_then(|m| m.session_id())
                    })
                })
                .flatten()
        })
        .collect();
    let counted = commit
        .ours
        .filter(|_| counts)
        .and_then(|i| ids.get(i).cloned().flatten());
    let decides = |s: &&SessionSummary| {
        if matches!(s.state.as_str(), "admitted" | "overflow") {
            return true;
        }
        match &counted {
            Some(id) => s.session == *id,
            None if counts => !ids.iter().flatten().any(|id| *id == s.session),
            None => true,
        }
    };
    let rank = |s: &SessionSummary| match s.state.as_str() {
        "admitted" => 0,
        "verified" => 1,
        "verifying" => 2,
        "uploading" => 3,
        "overflow" => 4,
        "refused" => 5,
        _ => 6,
    };
    // The view lists sessions oldest first: among equals the newest decides.
    view.sessions
        .iter()
        .rev()
        .filter(decides)
        .min_by_key(|s| rank(s))
}

/// The refusal class of a session refusal code (the eval Worker's
/// `refusal_class`, so the signed view and the public status agree).
pub fn refusal_class(code: ErrorCode) -> Option<&'static str> {
    use ErrorCode::*;
    Some(match code {
        CommitmentOverwritten
        | CommitmentMismatch
        | CommitmentNotFound
        | CommitmentUnresolvable
        | CommitmentBeforeOpen
        | WrongChain
        | WrongCohort => "commitment",
        ObjectMismatch | ArtifactInvalid | ContractMismatch | ContentMismatch | CheckFailed
        | ObjectReplaced | HeaderTooLarge | SecondArtifact | HeaderMissing | ObjectsMissing => {
            "artifact"
        }
        IncompleteAtCutoff | AfterCutoff | NotAdmitting => "cutoff",
        NotRegistered | RegistrationMismatch | HotkeyMismatch => "registration",
        _ => return None,
    })
}

fn short(hotkey: &str) -> String {
    if hotkey.len() > 12 {
        format!("{}…{}", &hotkey[..6], &hotkey[hotkey.len() - 4..])
    } else {
        hotkey.to_owned()
    }
}

/// `n` seconds in words.
pub fn duration_words(secs: u64) -> String {
    match secs {
        0..=89 => format!("{secs} s"),
        90..=5_399 => format!("{} min", secs.div_ceil(60)),
        _ => {
            let h = secs / 3_600;
            let m = (secs % 3_600) / 60;
            if m == 0 {
                format!("{h} h")
            } else {
                format!("{h} h {m} min")
            }
        }
    }
}

/// The time an upload of `len` bytes takes at the coordinator's planning
/// rate, plus its allowance: the URL policy's own life of one URL
/// (`ceil(L / b_min) + skew_secs`, `slop_ninja_upload_url_policy_v1`).
pub fn upload_need_secs(read: Option<&CohortInfo>, len: u64) -> u64 {
    let (b_min, skew) = read
        .and_then(|r| r.admitting.as_ref())
        .map(|a| (a.upload_policy.b_min, a.upload_policy.skew_secs))
        .filter(|(b, _)| *b > 0)
        .unwrap_or((
            slop_ninja_r2_presign::policy::B_MIN,
            slop_ninja_r2_presign::policy::SKEW_SECS,
        ));
    len.div_ceil(b_min).saturating_add(skew)
}

struct Ctx<'a> {
    inputs: &'a Inputs<'a>,
    policy: &'a Policy,
    t: Target,
    row: Option<&'a MinerRow>,
    registered: Option<bool>,
    uid: Option<u16>,
    commit: Commit,
    session: Option<&'a SessionSummary>,
    /// The coordinator's signed view of this hotkey for the cohort was read.
    view_applies: bool,
    block: Option<u64>,
    block_secs: u64,
    warnings: Vec<String>,
}

/// What a decision returns before the answers are filled in.
struct Decision {
    state: State,
    next: String,
    action: Action,
    blocked: Option<String>,
}

fn decided(state: State, next: impl Into<String>, action: Action) -> Decision {
    Decision {
        state,
        next: next.into(),
        action,
        blocked: None,
    }
}

impl Ctx<'_> {
    /// The newest local opening for the target cohort.
    fn latest_opening(&self) -> Option<(usize, &(PathBuf, OpeningFile))> {
        let cohort = self.t.cohort?;
        self.inputs
            .openings
            .iter()
            .enumerate()
            .filter(|(_, (_, f))| f.cohort == cohort)
            .max_by_key(|(i, (_, f))| {
                (
                    f.submitted
                        .as_ref()
                        .and_then(|s| s.finalized_commitment_block)
                        .unwrap_or(0),
                    *i,
                )
            })
    }

    fn opening_path(&self, index: usize) -> PathBuf {
        self.inputs.openings[index].0.clone()
    }

    /// Whether the named artifact is the one opening `index` was made for.
    fn fits(&self, index: usize) -> Option<bool> {
        self.inputs
            .artifact
            .map(|a| self.inputs.openings[index].1.fits(a))
    }

    /// Blocks from now to the cutoff's earliest block.
    fn blocks_left(&self) -> Option<u64> {
        Some(self.t.cutoff_block?.saturating_sub(self.block?))
    }

    /// Why a commitment may not be made now, if it may not.
    fn commit_guard(&self) -> Option<String> {
        let Some(read) = self.inputs.cohort_read else {
            return Some(
                "a commitment needs the coordinator's cohort read, which did not answer".into(),
            );
        };
        let Some(admitting) = &read.admitting else {
            return Some("the coordinator admits no cohort now".into());
        };
        if Some(admitting.cohort) != self.t.cohort
            || admitting.closing
            || self.t.window != Window::Open
        {
            return Some(format!(
                "cohort {} is closing admission: the client never commits after the cutoff",
                admitting.cohort
            ));
        }
        if let Some(epoch) = read.confirmed_epoch
            && epoch >= admitting.planned_cutoff_epoch
        {
            return Some(format!(
                "the confirmed epoch {epoch} has reached cohort {}'s planned cutoff epoch {}: the \
                 client never commits after the cutoff",
                admitting.cohort, admitting.planned_cutoff_epoch
            ));
        }
        if self.inputs.signed_closed {
            return Some(
                "the coordinator's signed admission routes are not open to this hotkey yet \
                 (allow-listed hotkeys only until admission opens to every miner): a commitment \
                 now could not be uploaded"
                    .into(),
            );
        }
        if self.inputs.chain.is_none() && self.policy.reads_chain {
            return Some(
                "a commitment needs the chain's finalized registration and commitment, which \
                 were not read (RPC unavailable or lagging)"
                    .into(),
            );
        }
        match self.blocks_left() {
            Some(left) if left <= self.policy.commit_margin_blocks => Some(format!(
                "the cutoff is at most {left} blocks away, within the {} blocks a commitment may \
                 take to land: the client never risks a commitment after the cutoff",
                self.policy.commit_margin_blocks
            )),
            Some(_) => None,
            None => Some("the cutoff block is not known".into()),
        }
    }

    /// The registration of the hotkey now, from the chain.
    fn chain_registration(&self) -> Option<&NeuronIdentity> {
        self.inputs.chain?.registration.as_ref()
    }

    /// A commit decision: taken only when the guards allow it.
    /// `supersedes`: the new commitment replaces one the chain holds.
    fn commit(&self, state: State, why: &str, supersedes: bool) -> Decision {
        let cohort = self.t.cohort.unwrap_or_default();
        let latest = self.latest_opening();
        let mut blocked = None;
        if self.inputs.artifact.is_none() {
            blocked = Some(
                "no --artifact named: the client commits only for an artifact you name".into(),
            );
        } else if let Some((i, _)) = latest
            && self.fits(i) == Some(false)
            && !self.policy.allow_replace
        {
            blocked = Some(format!(
                "this hotkey's last opening for cohort {cohort} is for another artifact; \
                 committing for this one changes the submitted model: pass --allow-replace to do that"
            ));
        } else if let Some(why) = self.commit_guard() {
            blocked = Some(why);
        }
        let next = format!(
            "{why}: commit for cohort {cohort} (`slop_ninja_miner resolve --artifact <object>` does \
             it, or `slop_ninja_miner commit --artifact <object>{}`), then upload{}",
            if latest.is_some() { " --replace" } else { "" },
            if supersedes {
                "; the new commitment replaces the one on chain and takes a later position"
            } else {
                ""
            }
        );
        match blocked {
            None => decided(
                state,
                next,
                Action::Commit {
                    cohort,
                    replace: latest.is_some(),
                },
            ),
            Some(why) => Decision {
                state,
                next,
                action: Action::None,
                blocked: Some(why),
            },
        }
    }

    /// An upload decision for local opening `index`.
    fn upload(&self, state: State, index: usize, next: &str) -> Decision {
        let path = self.opening_path(index);
        match self.fits(index) {
            None => Decision {
                state,
                next: format!("{next} (`slop_ninja_miner resolve --artifact <object>`)"),
                action: Action::None,
                blocked: Some("no --artifact named: the upload needs the object".into()),
            },
            Some(false) if self.policy.allow_replace && self.t.window == Window::Open => self
                .commit(
                    State::OtherArtifact,
                    "the commitment that counts is for another artifact than --artifact",
                    true,
                ),
            Some(false) => Decision {
                state: State::OtherArtifact,
                next: format!(
                    "the commitment that counts opens with {} for another artifact than --artifact: \
                     upload that artifact, or pass --allow-replace to commit again for this one (it \
                     replaces the commitment and moves your position later)",
                    path.display()
                ),
                action: Action::None,
                blocked: Some("committing for another artifact changes the submitted model".into()),
            },
            Some(true) => decided(state, next, Action::Upload { opening: path }),
        }
    }

    fn decide(&mut self) -> Decision {
        let t = self.t.clone();
        if self.inputs.api.is_none() && self.inputs.cohort_read.is_none() {
            return decided(
                State::StatusUnavailable,
                "neither the status API nor the coordinator answered: read again later (the \
                 client backs off)",
                Action::Wait,
            );
        }
        if t.window == Window::NoCohort {
            let next = self.next_cohort_words();
            return decided(State::NoCohort, next, Action::Wait);
        }
        let paused = self.inputs.api.is_some_and(ApiStatus::paused)
            || self.inputs.cohort_read.is_some_and(|r| r.paused);
        // The public row's (or the signed session's) state for the cohort.
        let status = self.status();
        // A hotkey with a session (or a result) for the cohort keeps that
        // state when it deregisters later; registration decides entry only.
        let entered = status.is_some();
        if self.registered == Some(false) && entered {
            self.warnings.push(
                "this hotkey no longer holds a UID: register again before entering another cohort"
                    .into(),
            );
        }
        if self.registered == Some(false) && !entered {
            return Decision {
                state: State::NotRegistered,
                next: format!(
                    "register {} on subnet {} (`rtao` burned registration from your coldkey, \
                     miner guide section 4), then run resolve again",
                    short(self.inputs.hotkey),
                    self.netuid()
                ),
                action: Action::None,
                blocked: Some(
                    "registration burns TAO from the coldkey; the client never loads a coldkey or \
                     spends funds"
                        .into(),
                ),
            };
        }
        if paused && t.window != Window::Closed {
            return decided(
                State::Paused,
                "the coordinator is paused or catching up: the cutoff moves later, never earlier, \
                 and the cohort is never skipped; nothing to do (the client waits)",
                Action::Wait,
            );
        }
        if let Some(d) = self.repair() {
            return d;
        }
        match status.as_deref() {
            Some("scored") => decided(State::Scored, "the cohort closed and published your result", Action::None),
            Some("withheld") => decided(State::Withheld, "the cohort closed; your result is withheld", Action::None),
            Some("not_executed") => decided(
                State::NotExecuted,
                "the cohort closed; your entry's scoring was not executed",
                Action::None,
            ),
            Some("evaluating") => decided(
                State::Evaluating,
                "scoring runs; nothing to do (keep your object until the cohort closes)",
                Action::None,
            ),
            Some("admitted") => decided(
                State::Admitted,
                "on the roster: challenges start with scoring; nothing to do (keep your object until \
                 the cohort closes)",
                Action::None,
            ),
            Some("overflow") => decided(
                State::Overflow,
                format!(
                    "verified, but the roster was full at the freeze. An opening never carries \
                     over: commit again for the next cohort once it admits ({})",
                    self.next_cohort_words()
                ),
                Action::None,
            ),
            Some("verified") => match t.window {
                Window::Open | Window::Closing => decided(
                    State::Verified,
                    "verified: the roster freezes after the cutoff (earlier commitments win under \
                     the roster cap); nothing to do",
                    Action::None,
                ),
                _ => decided(
                    State::Verified,
                    "verified; the frozen roster decides admission",
                    Action::None,
                ),
            },
            Some("refused") => self.refused(),
            Some("verifying") => match self.local_for_commit() {
                Some(i) if t.window == Window::Open => {
                    self.upload(State::Verifying, i, "verifying: the client follows the verification")
                }
                _ => decided(
                    State::Verifying,
                    "verifying: the coordinator's verdict comes without you; nothing to do",
                    Action::Wait,
                ),
            },
            Some("uploading") => match (t.window, self.local_for_commit()) {
                (Window::Open, Some(i)) => self.upload(
                    State::Uploading,
                    i,
                    "the upload is incomplete: resume it (fresh URLs; files R2 holds are not sent again)",
                ),
                (Window::Open, None) => decided(
                    State::Uploading,
                    "an upload is in progress from an opening this state directory does not hold: \
                     resume it from the machine that committed",
                    Action::None,
                ),
                _ => decided(
                    State::MissedCutoff,
                    format!(
                        "admission closed before the upload completed (it ends incomplete_at_cutoff); \
                         enter the next cohort ({})",
                        self.next_cohort_words()
                    ),
                    Action::None,
                ),
            },
            _ => self.before_session(),
        }
    }

    /// The state from the signed session (precise), else the public row.
    fn status(&self) -> Option<String> {
        if let Some(s) = self.session {
            return Some(match s.state.as_str() {
                // A superseded session decides nothing: the newer commitment does.
                "superseded" => return None,
                "admitted" if matches!(self.t.window, Window::Frozen) => {
                    // The public row tells evaluating apart once scoring runs.
                    match self.row.and_then(|r| r.cohort_status.as_deref()) {
                        Some(p @ ("evaluating" | "scored" | "withheld" | "not_executed")) => {
                            p.to_owned()
                        }
                        _ => "admitted".to_owned(),
                    }
                }
                other => other.to_owned(),
            });
        }
        let row = self.row?;
        match row.cohort_status.as_deref() {
            Some("none" | "committed") | None => None,
            // The signed view is the coordinator's live state and shows no
            // session deciding for the counted commitment: the feed's
            // session state (a minute or more older) is about an earlier one.
            Some("uploading" | "verifying" | "verified" | "refused") if self.view_applies => None,
            Some(other) => Some(other.to_owned()),
        }
    }

    fn netuid(&self) -> u16 {
        self.inputs
            .cohort_read
            .map(|r| r.chain.netuid)
            .or_else(|| self.inputs.api.and_then(|a| a.chain.as_ref()?.netuid))
            .unwrap_or(601)
    }

    fn next_cohort_words(&self) -> String {
        let from_read = self
            .inputs
            .cohort_read
            .and_then(|r| r.next.as_ref())
            .map(|n| (Some(n.cohort), n.earliest_open_epoch, None::<u64>));
        let from_api = self.inputs.api.and_then(|a| a.next.as_ref()).map(|n| {
            (
                n.cohort,
                n.earliest_open.as_ref().and_then(|e| e.epoch),
                n.earliest_open.as_ref().and_then(|e| e.at_estimate),
            )
        });
        let (cohort, epoch, at) = match (from_read, from_api) {
            (Some((c, e, _)), Some((_, _, at))) => (c, e, at),
            (Some(x), None) | (None, Some(x)) => x,
            (None, None) => return "the next cohort's opening is not published yet".into(),
        };
        let mut out = match cohort {
            Some(c) => format!("cohort {c} opens"),
            None => "the next cohort opens".into(),
        };
        if let Some(e) = epoch {
            out.push_str(&format!(" at epoch {e} at the earliest"));
        }
        if let Some(at) = at {
            out.push_str(&format!(
                ", in about {} or later",
                duration_words(at.saturating_sub(self.inputs.now))
            ));
        }
        out
    }

    /// A local opening that opens the counted commitment, else (when the
    /// commitment is not identified) the newest submitted one for the cohort.
    fn local_for_commit(&self) -> Option<usize> {
        if let Some(i) = self.commit.ours {
            return Some(i);
        }
        if self.commit.identified {
            return None;
        }
        let cohort = self.t.cohort?;
        self.inputs
            .openings
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (_, f))| f.cohort == cohort && f.submitted.is_some())
            .map(|(i, _)| i)
    }

    fn repair(&self) -> Option<Decision> {
        let s = self.session?;
        if s.repair_needed.is_empty() {
            return None;
        }
        let mut d = decided(
            State::RepairNeeded,
            format!(
                "{} recorded object(s) went missing on the coordinator's side (never your fault): \
                 send them again from your object (`slop_ninja_miner repair --artifact <object>`); \
                 valid until the cohort closes",
                s.repair_needed.len()
            ),
            Action::Repair {
                session: s.session.clone(),
            },
        );
        if self.inputs.artifact.is_none() {
            d.action = Action::None;
            d.blocked =
                Some("no --artifact named: a repair sends the bytes from your object".into());
        }
        Some(d)
    }

    /// Whether `--artifact` is the object of the deciding session's opening:
    /// the opening whose envelope is that session, else the one that opens
    /// the counted commitment. `None` when neither is known or no artifact
    /// was named.
    fn refused_fits(&self) -> Option<bool> {
        let cohort = self.t.cohort?;
        let by_session = self.session.and_then(|s| {
            self.inputs.openings.iter().position(|(_, f)| {
                f.cohort == cohort && f.session_id().as_deref() == Some(&s.session)
            })
        });
        self.fits(by_session.or(self.commit.ours)?)
    }

    fn refused(&self) -> Decision {
        let code = self
            .session
            .and_then(|s| s.refusal.as_ref())
            .and_then(|r| r.code());
        let class = code
            .and_then(refusal_class)
            .map(str::to_owned)
            .or_else(|| self.row.and_then(|r| r.reason_class.clone()));
        let code_words = code.map(|c| format!(" ({c})")).unwrap_or_default();
        let open = self.t.window == Window::Open;
        match class.as_deref() {
            Some("commitment") if open => self.commit(
                State::RefusedCommitment,
                &format!(
                    "the session was refused for its commitment{code_words}; a fresh commitment for \
                     the same artifact fixes it"
                ),
                true,
            ),
            Some("commitment") => decided(
                State::RefusedCommitment,
                format!(
                    "refused for its commitment{code_words} after admission closed; enter the next \
                     cohort ({})",
                    self.next_cohort_words()
                ),
                Action::None,
            ),
            Some("artifact")
                if open
                    && matches!(code, Some(ErrorCode::ObjectMismatch | ErrorCode::ObjectReplaced)) =>
            {
                self.commit(
                    State::RefusedArtifact,
                    &format!(
                        "a stored object did not match its commitment{code_words}, which the client's \
                         own PUTs never cause; a fresh commitment for the same artifact uploads it \
                         cleanly"
                    ),
                    true,
                )
            }
            // A changed model named with --artifact and --allow-replace: commit
            // for it (the guards of `commit` still apply); its upload follows.
            Some("artifact")
                if open
                    && self.policy.allow_replace
                    && self.refused_fits() == Some(false) =>
            {
                self.commit(
                    State::RefusedArtifact,
                    &format!(
                        "the artifact was refused{code_words} and --artifact names another one \
                         (--allow-replace)"
                    ),
                    true,
                )
            }
            Some("artifact") => {
                let next = format!(
                    "the artifact was refused{code_words}: run `slop_ninja_miner check <object>`, fix \
                     the model and pack it again, then `slop_ninja_miner resolve --artifact <new \
                     object> --allow-replace` while admission is open (miner guide section 14)"
                );
                let blocked = match (self.inputs.artifact, self.refused_fits()) {
                    (None, _) => "no --artifact named: a refused artifact needs a changed model",
                    (Some(_), Some(false)) if open => {
                        "--artifact names another model than the refused one: pass --allow-replace \
                         to commit for it (the submitted model changes)"
                    }
                    (Some(_), Some(false)) => "admission has closed: the changed model waits for the next cohort",
                    (Some(_), Some(true)) => {
                        "--artifact is the refused model: it needs a changed model, which is never \
                         automatic"
                    }
                    (Some(_), None) => {
                        "this state directory holds no opening for the refused session, so the \
                         client cannot tell whether --artifact is a changed model: commit it with \
                         `slop_ninja_miner commit --artifact <object> --replace`"
                    }
                };
                Decision {
                    state: State::RefusedArtifact,
                    next,
                    action: Action::None,
                    blocked: Some(blocked.into()),
                }
            }
            Some("cutoff") => decided(
                State::RefusedCutoff,
                format!(
                    "the upload was not complete at the cutoff; enter the next cohort ({})",
                    self.next_cohort_words()
                ),
                Action::None,
            ),
            Some("registration") if open && self.registered == Some(true) => self.commit(
                State::RefusedRegistration,
                &format!(
                    "the opening names another registration than the hotkey's current one{code_words}"
                ),
                true,
            ),
            Some("registration") => decided(
                State::RefusedRegistration,
                format!(
                    "the registration changed before the cutoff{code_words}; register again if you \
                     need to and enter the next cohort ({})",
                    self.next_cohort_words()
                ),
                Action::None,
            ),
            _ => decided(
                State::Unknown,
                "the session was refused for a reason this client does not know; run `slop_ninja_miner \
                 status` for the coordinator's message",
                Action::None,
            ),
        }
    }

    /// No session decides yet: the commitment does.
    fn before_session(&self) -> Decision {
        let t = &self.t;
        let open = t.window == Window::Open;
        if !open {
            // Only a source of this hotkey's own state says the entry is
            // missing: the feed's row, or the coordinator's signed view.
            let known = self.row.and_then(|r| r.cohort_status.as_ref()).is_some()
                || self.inputs.view.is_some_and(|v| Some(v.cohort) == t.cohort);
            if !known && t.window != Window::Unknown {
                return decided(
                    State::Unknown,
                    "this hotkey's intake state for the cohort is not published yet (the status \
                     API fills it as the per-miner documents go live); `slop_ninja_miner status` \
                     with a hotkey the coordinator's signed view serves shows it",
                    Action::None,
                );
            }
            return match (t.window, self.commit.seen) {
                (Window::Closing | Window::Frozen | Window::Closed, Some(true)) => decided(
                    State::MissedCutoff,
                    format!(
                        "admission closed with a commitment but no completed upload; enter the next \
                         cohort ({})",
                        self.next_cohort_words()
                    ),
                    Action::None,
                ),
                (Window::Closing | Window::Frozen | Window::Closed, _) => decided(
                    State::MissedCutoff,
                    format!(
                        "admission for cohort {} has closed; enter the next one ({})",
                        t.cohort.unwrap_or_default(),
                        self.next_cohort_words()
                    ),
                    Action::None,
                ),
                _ => decided(
                    State::Unknown,
                    "the cohort's stage is not known yet: read again later",
                    Action::Wait,
                ),
            };
        }
        // A newer registration than the opening's: the opening can never be
        // admitted (registration_mismatch).
        if let (Some(reg), Some(i)) = (self.chain_registration(), self.commit.ours)
            && self.inputs.openings[i].1.registration_block != reg.registration_block
        {
            return self.commit(
                State::StaleRegistration,
                "the hotkey registered again after this commitment; its opening names the old \
                 registration",
                true,
            );
        }
        match (self.commit.seen, self.commit.in_window) {
            (Some(true), Some(false)) => self.commit(
                State::CommitmentOutsideWindow,
                "the hotkey's commitment was made before this cohort opened, so it does not count",
                true,
            ),
            (Some(true), _) => match self.local_for_commit() {
                Some(i) => self.upload(
                    State::Committed,
                    i,
                    "committed and not uploaded: upload now (the coordinator observes a commitment \
                     about a minute after finality)",
                ),
                None if self.commit.identified => {
                    if self.policy.allow_replace {
                        self.commit(
                            State::CommitmentNotLocal,
                            "the commitment that counts is not one this state directory can open",
                            true,
                        )
                    } else {
                        Decision {
                            state: State::CommitmentNotLocal,
                            next:
                                "the commitment that counts was made elsewhere (no local opening \
                                   opens it): upload from the machine and state directory that \
                                   committed, or pass --allow-replace to commit again here (it \
                                   replaces that commitment)"
                                    .into(),
                            action: Action::None,
                            blocked: Some(
                                "replacing a commitment this client cannot open may change the \
                                 submitted model"
                                    .into(),
                            ),
                        }
                    }
                }
                None => decided(
                    State::Committed,
                    "a commitment counts, and this state directory holds no opening confirmed for \
                     this cohort; run resolve where you committed",
                    Action::None,
                ),
            },
            (Some(false), _) | (None, _) => {
                // Not seen by the source that answered. Committed by this
                // client and not yet seen anywhere authoritative: feed lag
                // unless the chain says otherwise.
                let pending = self
                    .latest_opening()
                    .filter(|(_, (_, f))| f.submitted.is_some());
                match (self.commit.source, pending) {
                    ("chain", _) => self.commit(
                        State::NotEntered,
                        "no commitment of this hotkey counts for this cohort on chain",
                        false,
                    ),
                    (_, Some((i, _))) => {
                        let mut d = self.upload(
                            State::CommitPending,
                            i,
                            "this client committed and the coordinator or the feed has not shown it \
                             yet (finality plus about a minute, and the feed refreshes once a \
                             minute): upload (it waits for the coordinator) or read again shortly",
                        );
                        // Without the chain the commitment is not confirmed; the
                        // upload answers commitment_not_found until it is seen.
                        d.state = State::CommitPending;
                        d
                    }
                    (_, None) => self.commit(
                        State::NotEntered,
                        "no commitment of this hotkey counts for this cohort yet",
                        false,
                    ),
                }
            }
        }
    }
}

/// Whether the hotkey is registered, and its UID, by the best source.
fn registration(
    inputs: &Inputs<'_>,
    row: Option<&MinerRow>,
) -> (Option<bool>, Option<u16>, &'static str) {
    if let Some(chain) = inputs.chain {
        return (
            Some(chain.registration.is_some()),
            chain.registration.map(|r| r.uid),
            "chain",
        );
    }
    if let Some(row) = row {
        match row.registered {
            Some(r) => return (Some(r), row.uid.filter(|_| r), "status API"),
            None if row.uid.is_some() => return (Some(true), row.uid, "status API"),
            None => {}
        }
    }
    if inputs.api.is_some_and(ApiStatus::registry_complete) && row.is_none() {
        return (Some(false), None, "status API registry");
    }
    if let Some(r) = inputs.view.and_then(|v| v.registration.as_ref()) {
        return (Some(true), Some(r.uid), "coordinator");
    }
    (None, None, "")
}

/// Diagnoses one hotkey from what the client read.
pub fn diagnose(inputs: &Inputs<'_>, policy: &Policy) -> Diagnosis {
    let t = target(inputs);
    let row = inputs
        .api
        .and_then(|a| a.row(inputs.hotkey))
        .filter(|_| t.row_applies);
    let (registered, uid, reg_source) = registration(inputs, row);
    let commit = commitment(inputs, &t, row);
    let session = session(
        inputs.view,
        t.cohort,
        &commit,
        inputs.openings,
        inputs.artifact,
    );
    let view_applies = inputs.view.is_some_and(|v| Some(v.cohort) == t.cohort);
    let block_secs = inputs.api.map_or(12, ApiStatus::block_secs);
    let mut cx = Ctx {
        inputs,
        policy,
        row,
        registered,
        uid,
        commit,
        session,
        view_applies,
        block: current_block(inputs),
        block_secs,
        t,
        warnings: Vec::new(),
    };
    let decision = cx.decide();
    cx.cutoff_warnings(decision.state);
    if let Some(error) = inputs.api_error {
        cx.warnings.push(format!("{error}"));
    }
    if inputs.api.is_some() && row.is_none() && cx.t.row_applies && registered != Some(false) {
        let published = inputs
            .api
            .and_then(|a| a.miners.as_ref()?.rows.as_ref())
            .is_some();
        if published {
            cx.warnings.push(
                "the status API's miner list does not name this hotkey yet (it lists the latest \
                 committing holder of each UID until the registry is published)"
                    .into(),
            );
        }
    }
    let answers = cx.answers(decision.state, reg_source);
    Diagnosis {
        hotkey: inputs.hotkey.to_owned(),
        cohort: cx.t.cohort,
        label: cx.t.label.clone(),
        state: decision.state,
        answers,
        next: decision.next,
        action: decision.action,
        blocked: decision.blocked,
        warnings: cx.warnings,
        cutoff_block: cx.t.cutoff_block,
    }
}

impl Ctx<'_> {
    /// Early and late warnings about the cutoff, for an entry not yet
    /// verified. The window is sized so the commitment-to-verified time of
    /// the largest object fits in under half of it, so less than twice the
    /// upload's planned time is an early warning and less than that time a
    /// late one.
    fn cutoff_warnings(&mut self, state: State) {
        use State::*;
        if self.t.window != Window::Open
            || !matches!(
                state,
                NotEntered
                    | CommitPending
                    | Committed
                    | Uploading
                    | CommitmentOutsideWindow
                    | StaleRegistration
                    | RefusedCommitment
                    | RefusedArtifact
                    | OtherArtifact
                    | CommitmentNotLocal
            )
        {
            return;
        }
        let Some(left) = self.blocks_left() else {
            return;
        };
        let secs_left = left.saturating_mul(self.block_secs);
        let len = self.inputs.artifact.map(|a| a.object_len).or_else(|| {
            self.inputs
                .cohort_read
                .and_then(|r| r.admitting.as_ref())
                .map(|a| a.max_object_len)
        });
        let Some(len) = len else { return };
        let need = upload_need_secs(self.inputs.cohort_read, len);
        let at = |n: u64| duration_words(n);
        if secs_left < need {
            self.warnings.push(format!(
                "late: the cutoff is about {} away at the earliest ({left} blocks); an upload of {} MB \
                 at the coordinator's planning rate needs about {}. It may not complete in time: \
                 start now on your fastest link",
                at(secs_left),
                len / 1_000_000,
                at(need)
            ));
        } else if secs_left < need.saturating_mul(2) {
            self.warnings.push(format!(
                "the cutoff is near: about {} away at the earliest ({left} blocks), less than twice \
                 the {} an upload of {} MB needs at the coordinator's planning rate. Commit and \
                 upload now",
                at(secs_left),
                at(need),
                len / 1_000_000
            ));
        }
    }

    fn answers(&self, state: State, reg_source: &str) -> Answers {
        use State::*;
        let hotkey = short(self.inputs.hotkey);
        let registered = match (self.registered, self.uid) {
            (Some(true), Some(uid)) => ans(
                Mark::Yes,
                format!("uid {uid}, hotkey {hotkey} ({reg_source})"),
            ),
            (Some(true), None) => ans(Mark::Yes, format!("hotkey {hotkey} ({reg_source})")),
            (Some(false), _) => ans(
                Mark::No,
                format!("hotkey {hotkey} holds no UID ({reg_source})"),
            ),
            (None, _) => ans(
                Mark::Unknown,
                "the inputs read do not list the registry yet",
            ),
        };
        let entered_or_later = matches!(
            state,
            Uploading
                | Verifying
                | Verified
                | Admitted
                | Evaluating
                | Scored
                | Withheld
                | NotExecuted
                | Overflow
                | RepairNeeded
                | RefusedArtifact
                | RefusedCutoff
        );
        let commitment_seen = match (state, self.commit.seen, self.commit.block) {
            (CommitPending, _, _) => ans(
                Mark::Pending,
                "committed by this client; not shown by the coordinator or the feed yet",
            ),
            (CommitmentOutsideWindow, _, Some(b)) => ans(
                Mark::No,
                format!("a commitment at block {b}, before the cohort opened"),
            ),
            (_, Some(true), Some(b)) => {
                ans(Mark::Yes, format!("at block {b} ({})", self.commit.source))
            }
            (_, Some(true), None) => ans(
                Mark::Yes,
                format!("inside the admission window ({})", self.commit.source),
            ),
            _ if entered_or_later => ans(Mark::Yes, "a session exists for it"),
            (_, Some(false), _) => ans(
                Mark::No,
                format!(
                    "no commitment counts for this cohort ({})",
                    self.commit.source
                ),
            ),
            _ => ans(Mark::Unknown, "not published yet"),
        };
        let upload_verified = match state {
            Verified | Admitted | Evaluating | Scored | Withheld | NotExecuted | Overflow => {
                ans(Mark::Yes, "verified")
            }
            RepairNeeded => ans(Mark::Yes, "verified; objects to repair"),
            Uploading => ans(Mark::Pending, "uploading"),
            Verifying => ans(Mark::Pending, "verifying"),
            RefusedArtifact | RefusedCommitment | RefusedRegistration | RefusedCutoff => ans(
                Mark::No,
                format!("refused ({})", state.code().trim_start_matches("refused_")),
            ),
            NotEntered
            | CommitPending
            | Committed
            | CommitmentNotLocal
            | OtherArtifact
            | CommitmentOutsideWindow
            | StaleRegistration => ans(Mark::No, "no upload yet"),
            MissedCutoff => ans(Mark::No, "not completed before the cutoff"),
            _ => ans(Mark::Unknown, "not published yet"),
        };
        let admitted = match state {
            Admitted | Evaluating | Scored | Withheld | NotExecuted => {
                ans(Mark::Yes, "on the frozen roster")
            }
            RepairNeeded if matches!(self.t.window, Window::Frozen | Window::Closed) => {
                ans(Mark::Yes, "on the frozen roster")
            }
            Overflow => ans(Mark::No, "the roster was full at the freeze"),
            RefusedArtifact | RefusedCommitment | RefusedRegistration | RefusedCutoff
            | MissedCutoff => ans(Mark::No, "not for this cohort"),
            _ if matches!(self.t.window, Window::Open | Window::Closing) => ans(
                Mark::Pending,
                "decided when the roster freezes after the cutoff",
            ),
            _ => ans(Mark::Unknown, "not published yet"),
        };
        let challenges = self.row.and_then(|r| r.challenges.as_ref());
        let pending = challenges.is_some_and(crate::status_api::Challenges::pending);
        let counts = challenges.and_then(crate::status_api::Challenges::counts);
        let (challenges_started, challenges_done) = match (state, counts) {
            (Evaluating | Admitted | RepairNeeded, _) if pending => (
                ans(
                    Mark::Pending,
                    "challenges pending: this entry's scoring has not started",
                ),
                ans(
                    Mark::Pending,
                    counts.map_or_else(
                        || "challenges pending".to_owned(),
                        |(done, total)| format!("{done} of {total}"),
                    ),
                ),
            ),
            (Evaluating | Admitted | RepairNeeded, Some((done, total))) => (
                ans(Mark::Yes, format!("scoring runs ({done} of {total} done)")),
                ans(
                    if done == total {
                        Mark::Yes
                    } else {
                        Mark::Pending
                    },
                    format!("{done} of {total}"),
                ),
            ),
            (Scored | Withheld | NotExecuted, Some((done, total))) => (
                ans(Mark::Yes, "the cohort closed"),
                ans(Mark::Yes, format!("{done} of {total}")),
            ),
            (Scored | Withheld | NotExecuted, None) => (
                ans(Mark::Yes, "the cohort closed"),
                ans(Mark::Unknown, "counts are not published after the close"),
            ),
            (Admitted | Evaluating | RepairNeeded, None) => (
                ans(
                    Mark::Pending,
                    "challenges pending: this entry's scoring has not started",
                ),
                ans(Mark::Pending, "challenges pending"),
            ),
            (StatusUnavailable | NoCohort | Paused | Unknown, _) => (
                ans(Mark::Unknown, "not published yet"),
                ans(Mark::Unknown, "not published yet"),
            ),
            _ => (
                ans(Mark::No, "only admitted entries receive challenges"),
                ans(Mark::No, "none"),
            ),
        };
        Answers {
            registered,
            commitment_seen,
            upload_verified,
            admitted,
            challenges_started,
            challenges_done,
        }
    }
}

/// The diagnosis as a report for people.
pub fn report(d: &Diagnosis) -> String {
    let mark = |m: Mark| match m {
        Mark::Yes => "yes",
        Mark::No => "NO",
        Mark::Pending => "not yet",
        Mark::Unknown => "unknown",
    };
    let mut lines = vec![format!(
        "hotkey {} in cohort {}{}: {}",
        d.hotkey,
        d.cohort.map_or_else(|| "-".to_owned(), |c| c.to_string()),
        d.label
            .as_ref()
            .map(|l| format!(" ({l})"))
            .unwrap_or_default(),
        d.state.code()
    )];
    let a = &d.answers;
    for (q, answer) in [
        ("registered?", &a.registered),
        ("commitment seen?", &a.commitment_seen),
        ("upload verified?", &a.upload_verified),
        ("admitted?", &a.admitted),
        ("challenges started?", &a.challenges_started),
        ("challenges done?", &a.challenges_done),
    ] {
        lines.push(format!(
            "  {q:<20} {:<8} {}",
            mark(answer.answer),
            answer.detail
        ));
    }
    lines.push(format!("  next: {}", d.next));
    match (&d.action, &d.blocked) {
        (_, Some(why)) => lines.push(format!("  not automatic: {why}")),
        (Action::None, None) => {}
        (Action::Wait, None) => lines.push("  resolve: waits and reads again".into()),
        (Action::Commit { cohort, .. }, None) => lines.push(format!(
            "  resolve: commits for cohort {cohort}, then uploads"
        )),
        (Action::Upload { opening }, None) => {
            lines.push(format!("  resolve: uploads opening {}", opening.display()))
        }
        (Action::Repair { session }, None) => {
            lines.push(format!("  resolve: repairs session {session}"))
        }
    }
    for w in &d.warnings {
        lines.push(format!("  warning: {w}"));
    }
    lines.join("\n")
}

#[cfg(test)]
#[path = "intake_tests.rs"]
mod tests;

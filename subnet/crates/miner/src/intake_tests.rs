//! One test per intake state: simulated status API answers, cohort reads,
//! signed views, chain facts and local openings.

use super::*;
use crate::opening::Submitted;
use crate::status_api::{self, ApiStatus};
use slop_ninja_protocol::{AccountId, ArtifactHeader, Digest};

const HOTKEY: &str = "5HotkeyUnderTest00000000000000000000000000000000";
const COHORT: u64 = 7;
const OPEN_BLOCK: u64 = 10_000;
/// The coordinator's planned cutoff: 16 epochs of 360 blocks after the open.
const CUTOFF: u64 = OPEN_BLOCK + 16 * 360;

fn payload(cohort: u64, tag: u8) -> Vec<u8> {
    let mut p = b"sna1".to_vec();
    p.extend_from_slice(&cohort.to_le_bytes());
    p.extend_from_slice(&[tag; 32]);
    p
}

fn object(tag: u8) -> ObjectHeader {
    ObjectHeader {
        header: ArtifactHeader {
            contract: "c".into(),
            files: vec![],
        },
        bytes: vec![],
        sha256: Digest([tag; 32]),
        len: 10,
        content: Digest([0; 32]),
        object_len: 100_000_000,
    }
}

fn opening(cohort: u64, tag: u8, artifact: u8, submitted: bool) -> (PathBuf, OpeningFile) {
    (
        PathBuf::from(format!("o-{cohort}-{tag}.json")),
        OpeningFile {
            schema: crate::opening::OPENING_SCHEMA.into(),
            opening: String::new(),
            commitment: String::new(),
            chain_payload: hex::encode(payload(cohort, tag)),
            cohort,
            netuid: 601,
            hotkey: HOTKEY.into(),
            registration_block: 5,
            content: String::new(),
            header_sha256: Some(Digest([artifact; 32]).hex()),
            header_len: Some(10),
            object_len: Some(100_000_000),
            submitted: submitted.then(|| Submitted {
                finalized_commitment_block: Some(OPEN_BLOCK + 100),
                ..Default::default()
            }),
        },
    )
}

/// The coordinator's cohort read: cohort 7 admitting (or closing), or none.
fn read(admitting: Option<bool>, finalized: u64) -> CohortInfo {
    let admitting = admitting.map(|closing| {
        serde_json::json!({
            "cohort": COHORT, "label": "testnet-r0-7", "opened_epoch": 100, "open_block": OPEN_BLOCK,
            "planned_cutoff_epoch": 116, "closing": closing, "output_contract": "c", "roster_cap": 40,
            "selection": "s", "max_object_len": 1_000_000_000u64,
            "upload_policy": {"id": "slop_ninja_upload_url_policy_v1", "b_min": 125_000, "skew_secs": 900},
            "limits": "l"
        })
    });
    serde_json::from_value(serde_json::json!({
        "schema": slop_ninja_admission_wire::json::SCHEMA_COHORT,
        "contract": slop_ninja_admission_wire::CONTRACT,
        "chain": {"genesis": "00".repeat(32), "netuid": 601, "mechanism": 0},
        "finalized_block": finalized,
        "request_validity_blocks": 100,
        "admitting": admitting,
        "next": {"cohort": COHORT + 1, "earliest_open_epoch": 120},
        "confirmed_epoch": 101
    }))
    .unwrap()
}

/// A status API answer for cohort 7 at `stage`, with this hotkey's row.
fn api(stage: &str, row: Option<serde_json::Value>, block: u64) -> ApiStatus {
    let rows = row.map(|r| serde_json::json!([r]));
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schema": status_api::STATUS_SCHEMA,
        "view": "current",
        "network": "testnet",
        "chain": {"netuid": 601, "mechanism": 0},
        "clock": {"confirmed_block": block, "confirmed_epoch": 101, "tempo_blocks": 360, "block_secs_nominal": 12},
        "pause": {"paused": false},
        "cohort": {"cohort": COHORT, "label": "testnet-r0-7", "state": "running", "stage": stage,
                   "expected": {"stage_end": {"epoch": 116, "block": CUTOFF}},
                   "bounds": {"open": {"block": OPEN_BLOCK}}},
        "next": {"cohort": COHORT + 1, "earliest_open": {"epoch": 120, "at_estimate": 1_000_000}},
        "miners": {"source": "registry", "complete": rows.is_some(), "rows": rows}
    }))
    .unwrap();
    status_api::decode(200, &bytes).unwrap()
}

fn row(status: &str) -> serde_json::Value {
    serde_json::json!({"uid": 12, "hotkey": HOTKEY, "registered": true, "cohort_status": status})
}

fn chain(block: u64, registered: bool, commitment: Option<(u64, Vec<u8>)>) -> ChainFacts {
    ChainFacts {
        genesis: None,
        finalized_block: block,
        registration: registered.then_some(NeuronIdentity {
            uid: 12,
            hotkey: AccountId([1; 32]),
            registration_block: 5,
        }),
        commitment,
    }
}

fn view(sessions: serde_json::Value, commitment: Option<(u64, Vec<u8>)>) -> HotkeyView {
    serde_json::from_value(serde_json::json!({
        "schema": slop_ninja_admission_wire::json::SCHEMA_HOTKEY,
        "cohort": COHORT, "hotkey": HOTKEY, "finalized_block": OPEN_BLOCK + 200,
        "registration": {"uid": 12, "registration_block": 5},
        "commitment": commitment.map(|(b, p)| serde_json::json!({
            "block": b, "extrinsic": 1, "payload": hex::encode(&p), "payload_cohort": payload_cohort(&p),
            "in_window": b >= OPEN_BLOCK})),
        "sessions": sessions
    }))
    .unwrap()
}

const POLICY: Policy = Policy {
    allow_replace: false,
    commit_margin_blocks: 50,
    reads_chain: true,
};

struct Case {
    signed_closed: bool,
    api: Option<ApiStatus>,
    read: Option<CohortInfo>,
    view: Option<HotkeyView>,
    chain: Option<ChainFacts>,
    openings: Vec<(PathBuf, OpeningFile)>,
    artifact: Option<ObjectHeader>,
}

impl Case {
    fn new() -> Self {
        Self {
            signed_closed: false,
            api: None,
            read: None,
            view: None,
            chain: None,
            openings: Vec::new(),
            artifact: None,
        }
    }
    fn run(&self, policy: &Policy) -> Diagnosis {
        diagnose(
            &Inputs {
                hotkey: HOTKEY,
                api: self.api.as_ref(),
                api_error: None,
                cohort_read: self.read.as_ref(),
                view: self.view.as_ref(),
                chain: self.chain.as_ref(),
                openings: &self.openings,
                artifact: self.artifact.as_ref(),
                signed_closed: self.signed_closed,
                now: 0,
            },
            policy,
        )
    }
}

/// An open window, early in it: cohort read admitting, chain at `OPEN_BLOCK + 200`.
fn open_case() -> Case {
    let block = OPEN_BLOCK + 200;
    Case {
        api: Some(api("admission_open", Some(row("none")), block)),
        read: Some(read(Some(false), block)),
        chain: Some(chain(block, true, None)),
        artifact: Some(object(1)),
        ..Case::new()
    }
}

#[test]
fn nothing_readable_is_status_unavailable_and_waits() {
    let d = Case::new().run(&POLICY);
    assert_eq!(d.state, State::StatusUnavailable);
    assert_eq!(d.action, Action::Wait);
    assert!(!d.state.settled());
}

#[test]
fn no_cohort_names_the_next_opening_and_waits() {
    let mut idle = api("idle", None, 9_000);
    idle.cohort.as_mut().unwrap().cohort = None;
    let case = Case {
        api: Some(idle),
        read: Some(read(None, 9_000)),
        ..Case::new()
    };
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::NoCohort);
    assert_eq!(d.action, Action::Wait);
    assert!(
        d.next
            .contains("cohort 8 opens at epoch 120 at the earliest"),
        "{}",
        d.next
    );
}

#[test]
fn a_pause_waits_and_says_the_cutoff_only_moves_later() {
    let mut case = open_case();
    case.api.as_mut().unwrap().pause = Some(status_api::Pause {
        paused: true,
        class: Some("calendar_paused".into()),
    });
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::Paused);
    assert_eq!(d.action, Action::Wait);
    assert!(d.next.contains("later, never earlier"), "{}", d.next);
}

#[test]
fn not_registered_is_never_resolved_automatically() {
    let mut case = open_case();
    case.chain = Some(chain(OPEN_BLOCK + 200, false, None));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::NotRegistered);
    assert_eq!(d.action, Action::None);
    assert!(
        d.blocked
            .as_deref()
            .unwrap()
            .contains("never loads a coldkey")
    );
    assert_eq!(d.answers.registered.answer, Mark::No);
    // Also from the public registry alone (complete, and no row names the hotkey).
    let mut other = api("admission_open", Some(row("none")), OPEN_BLOCK + 200);
    other.miners.as_mut().unwrap().rows.as_mut().unwrap()[0].hotkey = "5Other".into();
    let case = Case {
        api: Some(other),
        ..Case::new()
    };
    assert_eq!(case.run(&POLICY).state, State::NotRegistered);
}

#[test]
fn not_entered_commits_only_with_an_artifact_and_before_the_cutoff() {
    let d = open_case().run(&POLICY);
    assert_eq!(d.state, State::NotEntered);
    assert_eq!(
        d.action,
        Action::Commit {
            cohort: COHORT,
            replace: false
        }
    );
    assert_eq!(d.answers.registered.answer, Mark::Yes);
    assert!(d.answers.registered.detail.contains("uid 12"));
    assert_eq!(d.answers.commitment_seen.answer, Mark::No);
    // No artifact named: the step is shown, not taken.
    let mut case = open_case();
    case.artifact = None;
    let d = case.run(&POLICY);
    assert_eq!(d.action, Action::None);
    assert!(d.blocked.as_deref().unwrap().contains("--artifact"));
    // Within the commit's landing bound of the cutoff: never commits.
    let near = CUTOFF - 50;
    let mut case = open_case();
    case.read = Some(read(Some(false), near));
    case.chain = Some(chain(near, true, None));
    let d = case.run(&POLICY);
    assert_eq!(d.action, Action::None);
    assert!(
        d.blocked
            .as_deref()
            .unwrap()
            .contains("never risks a commitment after the cutoff")
    );
    // One block more is fine.
    let mut case = open_case();
    case.chain = Some(chain(near - 1, true, None));
    case.read = Some(read(Some(false), near - 1));
    case.api = Some(api("admission_open", Some(row("none")), near - 1));
    assert!(matches!(case.run(&POLICY).action, Action::Commit { .. }));
    // The coordinator's read says closing: never commits; the entry missed the cutoff.
    let mut case = open_case();
    case.read = Some(read(Some(true), CUTOFF + 1));
    case.api = Some(api("admission_closing", Some(row("none")), CUTOFF + 1));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::MissedCutoff);
    assert_eq!(d.action, Action::None);
    // The signed routes are not open to this hotkey yet: a commitment could
    // not be uploaded, so none is made.
    let mut case = open_case();
    case.signed_closed = true;
    let d = case.run(&POLICY);
    assert_eq!(d.action, Action::None);
    assert!(
        d.blocked
            .as_deref()
            .unwrap()
            .contains("not open to this hotkey")
    );
    // No chain read (RPC lag or outage): the commit waits for the chain.
    let mut case = open_case();
    case.chain = None;
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::NotEntered);
    assert!(
        d.blocked.as_deref().unwrap().contains("RPC"),
        "{:?}",
        d.blocked
    );
    // `status` reads no chain: it shows the commit resolve would make.
    let d = case.run(&Policy {
        reads_chain: false,
        ..POLICY
    });
    assert_eq!(
        d.action,
        Action::Commit {
            cohort: COHORT,
            replace: false
        }
    );
}

#[test]
fn a_commitment_the_feed_has_not_shown_is_uploaded_not_committed_again() {
    // No chain read: the public row says none, but this client committed.
    let mut case = open_case();
    case.chain = None;
    case.openings = vec![opening(COHORT, 1, 1, true)];
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::CommitPending);
    assert_eq!(
        d.action,
        Action::Upload {
            opening: PathBuf::from("o-7-1.json")
        }
    );
    assert_eq!(d.answers.commitment_seen.answer, Mark::Pending);
    // The chain holds it: the feed only lags. Upload it.
    let mut case = open_case();
    case.chain = Some(chain(
        OPEN_BLOCK + 200,
        true,
        Some((OPEN_BLOCK + 100, payload(COHORT, 1))),
    ));
    case.openings = vec![opening(COHORT, 1, 1, true)];
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::Committed);
    assert_eq!(
        d.action,
        Action::Upload {
            opening: PathBuf::from("o-7-1.json")
        }
    );
}

#[test]
fn a_commitment_that_never_landed_is_committed_again_for_the_same_artifact() {
    let mut case = open_case();
    // The chain holds this hotkey's commitment for the previous cohort only.
    case.chain = Some(chain(
        OPEN_BLOCK + 200,
        true,
        Some((OPEN_BLOCK - 900, payload(COHORT - 1, 9))),
    ));
    case.openings = vec![opening(COHORT, 1, 1, true)];
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::NotEntered);
    assert_eq!(
        d.action,
        Action::Commit {
            cohort: COHORT,
            replace: true
        }
    );
    // For another artifact than the last opening's: that changes the model.
    case.artifact = Some(object(2));
    let d = case.run(&POLICY);
    assert_eq!(d.action, Action::None);
    assert!(d.blocked.as_deref().unwrap().contains("--allow-replace"));
    let d = case.run(&Policy {
        allow_replace: true,
        ..POLICY
    });
    assert_eq!(
        d.action,
        Action::Commit {
            cohort: COHORT,
            replace: true
        }
    );
}

#[test]
fn a_commitment_made_elsewhere_is_replaced_only_on_request() {
    let mut case = open_case();
    case.chain = Some(chain(
        OPEN_BLOCK + 200,
        true,
        Some((OPEN_BLOCK + 50, payload(COHORT, 5))),
    ));
    case.api = Some(api(
        "admission_open",
        Some(row("committed")),
        OPEN_BLOCK + 200,
    ));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::CommitmentNotLocal);
    assert_eq!(d.action, Action::None);
    assert!(d.blocked.is_some());
    let d = case.run(&Policy {
        allow_replace: true,
        ..POLICY
    });
    assert_eq!(
        d.action,
        Action::Commit {
            cohort: COHORT,
            replace: false
        }
    );
}

#[test]
fn another_artifact_than_the_committed_one_needs_allow_replace() {
    let mut case = open_case();
    case.chain = Some(chain(
        OPEN_BLOCK + 200,
        true,
        Some((OPEN_BLOCK + 100, payload(COHORT, 1))),
    ));
    case.openings = vec![opening(COHORT, 1, 1, true)];
    case.artifact = Some(object(2));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::OtherArtifact);
    assert_eq!(d.action, Action::None);
    let d = case.run(&Policy {
        allow_replace: true,
        ..POLICY
    });
    assert_eq!(d.state, State::OtherArtifact);
    assert_eq!(
        d.action,
        Action::Commit {
            cohort: COHORT,
            replace: true
        }
    );
}

#[test]
fn a_commitment_before_the_window_is_made_again_inside_it() {
    let mut case = open_case();
    // The payload names cohort 7 but was made before it opened.
    case.chain = Some(chain(
        OPEN_BLOCK + 200,
        true,
        Some((OPEN_BLOCK - 5, payload(COHORT, 1))),
    ));
    case.openings = vec![opening(COHORT, 1, 1, true)];
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::CommitmentOutsideWindow);
    assert_eq!(
        d.action,
        Action::Commit {
            cohort: COHORT,
            replace: true
        }
    );
    assert_eq!(d.answers.commitment_seen.answer, Mark::No);
}

#[test]
fn a_registration_newer_than_the_opening_is_committed_again() {
    let mut case = open_case();
    let mut facts = chain(
        OPEN_BLOCK + 200,
        true,
        Some((OPEN_BLOCK + 100, payload(COHORT, 1))),
    );
    facts.registration.as_mut().unwrap().registration_block = 9_999;
    case.chain = Some(facts);
    case.openings = vec![opening(COHORT, 1, 1, true)];
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::StaleRegistration);
    assert_eq!(
        d.action,
        Action::Commit {
            cohort: COHORT,
            replace: true
        }
    );
}

#[test]
fn uploading_resumes_while_open_and_misses_the_cutoff_after() {
    let mut case = open_case();
    case.api = Some(api(
        "admission_open",
        Some(row("uploading")),
        OPEN_BLOCK + 200,
    ));
    case.chain = Some(chain(
        OPEN_BLOCK + 200,
        true,
        Some((OPEN_BLOCK + 100, payload(COHORT, 1))),
    ));
    case.openings = vec![opening(COHORT, 1, 1, true)];
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::Uploading);
    assert_eq!(
        d.action,
        Action::Upload {
            opening: PathBuf::from("o-7-1.json")
        }
    );
    assert_eq!(d.answers.upload_verified.answer, Mark::Pending);
    case.read = Some(read(Some(true), CUTOFF + 2));
    case.api = Some(api("admission_closing", Some(row("uploading")), CUTOFF + 2));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::MissedCutoff);
    assert_eq!(d.action, Action::None);
    assert!(d.state.settled());
}

#[test]
fn verifying_follows_the_verification_and_verified_is_settled() {
    let mut case = open_case();
    case.api = Some(api(
        "admission_open",
        Some(row("verifying")),
        OPEN_BLOCK + 200,
    ));
    case.chain = Some(chain(
        OPEN_BLOCK + 200,
        true,
        Some((OPEN_BLOCK + 100, payload(COHORT, 1))),
    ));
    case.openings = vec![opening(COHORT, 1, 1, true)];
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::Verifying);
    assert!(matches!(d.action, Action::Upload { .. }));
    case.api = Some(api(
        "admission_open",
        Some(row("verified")),
        OPEN_BLOCK + 200,
    ));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::Verified);
    assert_eq!(d.action, Action::None);
    assert!(d.state.settled());
    assert_eq!(d.answers.upload_verified.answer, Mark::Yes);
    assert_eq!(d.answers.admitted.answer, Mark::Pending);
    assert_eq!(d.answers.challenges_started.answer, Mark::No);
}

#[test]
fn refusals_by_class() {
    let refused = |class: &str| {
        let mut r = row("refused");
        r["reason_class"] = serde_json::json!(class);
        r
    };
    let mut case = open_case();
    case.chain = Some(chain(
        OPEN_BLOCK + 200,
        true,
        Some((OPEN_BLOCK + 100, payload(COHORT, 1))),
    ));
    case.openings = vec![opening(COHORT, 1, 1, true)];
    // Commitment class: a fresh commitment for the same artifact.
    case.api = Some(api(
        "admission_open",
        Some(refused("commitment")),
        OPEN_BLOCK + 200,
    ));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::RefusedCommitment);
    assert_eq!(
        d.action,
        Action::Commit {
            cohort: COHORT,
            replace: true
        }
    );
    // Artifact class without the code: a model change, never automatic.
    case.api = Some(api(
        "admission_open",
        Some(refused("artifact")),
        OPEN_BLOCK + 200,
    ));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::RefusedArtifact);
    assert_eq!(d.action, Action::None);
    assert!(d.next.contains("check"), "{}", d.next);
    // object_mismatch in the signed view: the same artifact again, freshly.
    case.view = Some(view(
        serde_json::json!([{"session": "ab", "state": "refused",
            "refusal": {"code": "object_mismatch", "message": "m"}}]),
        Some((OPEN_BLOCK + 100, payload(COHORT, 1))),
    ));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::RefusedArtifact);
    assert_eq!(
        d.action,
        Action::Commit {
            cohort: COHORT,
            replace: true
        }
    );
    case.view = None;
    // Cutoff class: over for this cohort.
    case.read = Some(read(Some(true), CUTOFF + 2));
    case.api = Some(api(
        "admission_closing",
        Some(refused("cutoff")),
        CUTOFF + 2,
    ));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::RefusedCutoff);
    assert_eq!(d.action, Action::None);
    assert!(d.next.contains("next cohort"), "{}", d.next);
    // Registration class while open, registered again: commit again.
    case.read = Some(read(Some(false), OPEN_BLOCK + 200));
    case.api = Some(api(
        "admission_open",
        Some(refused("registration")),
        OPEN_BLOCK + 200,
    ));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::RefusedRegistration);
    assert_eq!(
        d.action,
        Action::Commit {
            cohort: COHORT,
            replace: true
        }
    );
}

#[test]
fn overflow_points_at_the_next_cohort() {
    let mut case = open_case();
    case.read = Some(read(None, CUTOFF + 400));
    case.api = Some(api("generating", Some(row("overflow")), CUTOFF + 400));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::Overflow);
    assert_eq!(d.action, Action::None);
    assert_eq!(d.answers.admitted.answer, Mark::No);
    assert!(d.next.contains("cohort 8 opens"), "{}", d.next);
}

#[test]
fn a_missing_recorded_object_is_repaired() {
    let mut case = open_case();
    case.read = Some(read(None, CUTOFF + 400));
    case.api = Some(api("scoring", Some(row("evaluating")), CUTOFF + 400));
    case.view = Some(view(
        serde_json::json!([{"session": "cd", "state": "admitted", "repair_needed": ["header"]}]),
        Some((OPEN_BLOCK + 100, payload(COHORT, 1))),
    ));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::RepairNeeded);
    assert_eq!(
        d.action,
        Action::Repair {
            session: "cd".into()
        }
    );
    case.artifact = None;
    let d = case.run(&POLICY);
    assert_eq!(d.action, Action::None);
    assert!(d.blocked.is_some());
}

#[test]
fn challenge_progress_is_counts_only_and_pending_before_scoring() {
    let mut case = open_case();
    case.read = Some(read(None, CUTOFF + 400));
    case.chain = None;
    case.api = Some(api("admission_closing", Some(row("admitted")), CUTOFF + 10));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::Admitted);
    assert_eq!(d.answers.admitted.answer, Mark::Yes);
    assert_eq!(d.answers.challenges_started.answer, Mark::Pending);
    assert!(
        d.answers
            .challenges_started
            .detail
            .contains("challenges pending")
    );
    let mut r = row("evaluating");
    r["challenges"] = serde_json::json!({"state": "running", "done": 3, "total": 40});
    case.api = Some(api("scoring", Some(r), CUTOFF + 400));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::Evaluating);
    assert_eq!(d.answers.challenges_started.answer, Mark::Yes);
    assert_eq!(d.answers.challenges_done.answer, Mark::Pending);
    assert_eq!(d.answers.challenges_done.detail, "3 of 40");
    assert!(d.state.settled());
    // Sealed but this entry's scoring has not started: pending, with its total.
    let mut r = row("evaluating");
    r["challenges"] = serde_json::json!({"state": "pending", "done": 0, "total": 40});
    case.api = Some(api("scoring", Some(r), CUTOFF + 400));
    let d = case.run(&POLICY);
    assert_eq!(d.answers.challenges_started.answer, Mark::Pending);
    assert_eq!(d.answers.challenges_done.detail, "0 of 40");
    // Evaluating with challenges still null: pending.
    case.api = Some(api("scoring", Some(row("evaluating")), CUTOFF + 400));
    assert_eq!(
        case.run(&POLICY).answers.challenges_done.answer,
        Mark::Pending
    );
    // Closed and scored.
    let mut r = row("scored");
    r["challenges"] = serde_json::json!({"state": "complete", "done": 40, "total": 40});
    case.api = Some(api("closed", Some(r), CUTOFF + 2_000));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::Scored);
    assert_eq!(d.answers.challenges_done.answer, Mark::Yes);
}

#[test]
fn cutoff_warnings_are_early_then_late() {
    // A 100 MB object at 125 kB/s plus 900 s needs 1,700 s, about 142 blocks.
    let at = |left: u64| {
        let block = CUTOFF - left;
        let mut case = open_case();
        case.read = Some(read(Some(false), block));
        case.chain = Some(chain(block, true, None));
        case.api = Some(api("admission_open", Some(row("none")), block));
        case.run(&POLICY).warnings
    };
    assert!(at(1_000).is_empty());
    let early = at(200);
    assert!(
        early.iter().any(|w| w.starts_with("the cutoff is near")),
        "{early:?}"
    );
    let late = at(100);
    assert!(late.iter().any(|w| w.starts_with("late:")), "{late:?}");
    assert_eq!(upload_need_secs(None, 100_000_000), 800 + 900);
}

#[test]
fn a_frozen_cohort_without_a_per_hotkey_source_is_unknown_not_missed() {
    let case = Case {
        api: Some(api("scoring", None, CUTOFF + 400)),
        read: Some(read(None, CUTOFF + 400)),
        ..Case::new()
    };
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::Unknown);
    assert_eq!(d.action, Action::None);
    assert_eq!(d.answers.registered.answer, Mark::Unknown);
}

#[test]
fn a_row_about_another_cohort_is_not_applied() {
    // The feed still describes cohort 6 while the coordinator admits 7.
    let mut stale = api("admission_open", Some(row("verified")), OPEN_BLOCK + 200);
    stale.cohort.as_mut().unwrap().cohort = Some(COHORT - 1);
    let mut case = open_case();
    case.api = Some(stale);
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::NotEntered);
    assert!(matches!(d.action, Action::Commit { .. }));
}

#[test]
fn the_report_answers_each_question() {
    let text = report(&open_case().run(&POLICY));
    for q in [
        "registered?",
        "commitment seen?",
        "upload verified?",
        "admitted?",
        "challenges started?",
        "challenges done?",
        "next:",
        "resolve: commits for cohort 7",
    ] {
        assert!(text.contains(q), "{q}: {text}");
    }
    assert_eq!(duration_words(30), "30 s");
    assert_eq!(duration_words(600), "10 min");
    assert_eq!(duration_words(7_260), "2 h 1 min");
    assert_eq!(payload_cohort(&payload(9, 1)), Some(9));
    assert_eq!(payload_cohort(b"short"), None);
}

/// A decodable opening (its session id is derivable), committed at `block`.
fn real(cohort: u64, nonce: u8, artifact: u8, block: Option<u64>) -> (PathBuf, OpeningFile) {
    let opening = slop_ninja_protocol::submission::SubmissionOpening {
        chain: slop_ninja_protocol::ChainBinding {
            genesis: Digest([0; 32]),
            netuid: 601,
            mechanism: 0,
        },
        cohort,
        hotkey: AccountId([1; 32]),
        registration_block: 5,
        content: Digest([0; 32]),
        nonce: slop_ninja_protocol::Nonce([nonce; 32]),
    };
    let mut file = OpeningFile::new(&opening, &object(artifact), HOTKEY);
    file.submitted = block.map(|b| Submitted {
        finalized_commitment_block: Some(b),
        ..Default::default()
    });
    (PathBuf::from(format!("r-{cohort}-{nonce}.json")), file)
}

fn real_payload(o: &(PathBuf, OpeningFile)) -> Vec<u8> {
    hex::decode(&o.1.chain_payload).unwrap()
}

fn sid(o: &(PathBuf, OpeningFile)) -> String {
    o.1.session_id().unwrap()
}

fn refused_session(id: &str, code: &str) -> serde_json::Value {
    serde_json::json!([{"session": id, "state": "refused",
        "refusal": {"code": code, "message": "m"}}])
}

#[test]
fn an_opening_names_its_session_id() {
    let o = real(COHORT, 1, 1, None);
    let id = sid(&o);
    assert!(crate::wire::is_session_id(&id), "{id}");
    assert_ne!(
        id,
        sid(&real(COHORT, 2, 1, None)),
        "one session per opening"
    );
    assert_ne!(id, sid(&real(COHORT, 1, 2, None)), "one session per object");
    // A file that does not decode (or a v1 file naming no header) has none.
    assert_eq!(opening(COHORT, 1, 1, true).1.session_id(), None);
}

/// After a commitment-class refusal the client commits again; the refused
/// session (never superseded) then belongs to the old commitment, so the
/// new one is uploaded, not committed again and again.
#[test]
fn after_a_commitment_refusal_the_fresh_commitment_is_uploaded() {
    let old = real(COHORT, 1, 1, Some(OPEN_BLOCK + 100));
    let new = real(COHORT, 2, 1, Some(OPEN_BLOCK + 250));
    let mut case = open_case();
    case.api = Some(api(
        "admission_open",
        Some(row("refused")),
        OPEN_BLOCK + 300,
    ));
    case.openings = vec![old.clone()];
    // First pass: the refused session is the counted commitment's own.
    case.chain = Some(chain(
        OPEN_BLOCK + 300,
        true,
        Some((OPEN_BLOCK + 100, real_payload(&old))),
    ));
    case.view = Some(view(
        refused_session(&sid(&old), "commitment_mismatch"),
        Some((OPEN_BLOCK + 100, real_payload(&old))),
    ));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::RefusedCommitment);
    assert_eq!(
        d.action,
        Action::Commit {
            cohort: COHORT,
            replace: true
        }
    );
    // The fresh commitment landed: upload it.
    case.openings = vec![old.clone(), new.clone()];
    case.chain = Some(chain(
        OPEN_BLOCK + 300,
        true,
        Some((OPEN_BLOCK + 250, real_payload(&new))),
    ));
    case.view = Some(view(
        refused_session(&sid(&old), "commitment_mismatch"),
        Some((OPEN_BLOCK + 250, real_payload(&new))),
    ));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::Committed, "{d:?}");
    assert_eq!(
        d.action,
        Action::Upload {
            opening: new.0.clone()
        }
    );
    // The same with a refused session this directory cannot name (made by
    // another client), and before the coordinator logs the new commitment.
    case.view = Some(view(
        refused_session(&"ab".repeat(32), "commitment_mismatch"),
        Some((OPEN_BLOCK + 100, real_payload(&old))),
    ));
    let d = case.run(&POLICY);
    assert_eq!(
        d.action,
        Action::Upload {
            opening: new.0.clone()
        },
        "{d:?}"
    );
    // Its own session, once opened, decides.
    case.view = Some(view(
        serde_json::json!([
            {"session": sid(&old), "state": "refused",
             "refusal": {"code": "commitment_mismatch", "message": "m"}},
            {"session": sid(&new), "state": "verified"}
        ]),
        Some((OPEN_BLOCK + 250, real_payload(&new))),
    ));
    let d = case.run(&POLICY);
    assert_eq!(d.state, State::Verified, "{d:?}");
    // Without the signed view the feed's refusal (a minute old, naming no
    // session) cannot be placed: never a commitment on top of the new one.
    case.view = None;
    let d = case.run(&POLICY);
    assert!(!matches!(d.action, Action::Commit { .. }), "{d:?}");
}

/// An artifact-class refusal: with --allow-replace and a changed model the
/// client commits for it, and its upload follows; never without the flag,
/// and never for the refused model itself.
#[test]
fn a_changed_model_after_an_artifact_refusal_needs_allow_replace() {
    let old = real(COHORT, 1, 1, Some(OPEN_BLOCK + 100));
    let mut case = open_case();
    case.openings = vec![old.clone()];
    case.chain = Some(chain(
        OPEN_BLOCK + 300,
        true,
        Some((OPEN_BLOCK + 100, real_payload(&old))),
    ));
    case.view = Some(view(
        refused_session(&sid(&old), "artifact_invalid"),
        Some((OPEN_BLOCK + 100, real_payload(&old))),
    ));
    let allow = Policy {
        allow_replace: true,
        ..POLICY
    };
    // The refused model again: a model change is never automatic.
    let d = case.run(&allow);
    assert_eq!(d.state, State::RefusedArtifact);
    assert_eq!(d.action, Action::None);
    assert!(
        d.blocked.as_deref().unwrap().contains("refused model"),
        "{d:?}"
    );
    // A changed model without the flag: blocked, and says which flag.
    case.artifact = Some(object(9));
    let d = case.run(&POLICY);
    assert_eq!(d.action, Action::None);
    assert!(
        d.blocked.as_deref().unwrap().contains("--allow-replace"),
        "{d:?}"
    );
    // With it: commit for the changed model.
    let d = case.run(&allow);
    assert_eq!(d.state, State::RefusedArtifact);
    assert_eq!(
        d.action,
        Action::Commit {
            cohort: COHORT,
            replace: true
        }
    );
    // Its commitment landed: upload it (the old refusal no longer decides).
    let new = real(COHORT, 2, 9, Some(OPEN_BLOCK + 250));
    case.openings = vec![old.clone(), new.clone()];
    case.chain = Some(chain(
        OPEN_BLOCK + 300,
        true,
        Some((OPEN_BLOCK + 250, real_payload(&new))),
    ));
    let d = case.run(&allow);
    assert_eq!(d.state, State::Committed, "{d:?}");
    assert_eq!(
        d.action,
        Action::Upload {
            opening: new.0.clone()
        }
    );
    // After the cutoff: nothing, even with the flag.
    case.openings = vec![old.clone()];
    case.chain = Some(chain(
        CUTOFF + 2,
        true,
        Some((OPEN_BLOCK + 100, real_payload(&old))),
    ));
    case.read = Some(read(Some(true), CUTOFF + 2));
    case.api = Some(api("admission_closing", Some(row("refused")), CUTOFF + 2));
    let d = case.run(&allow);
    assert_eq!(d.state, State::RefusedArtifact);
    assert_eq!(d.action, Action::None);
}

#[test]
fn the_commit_window_ends_the_era_before_the_margin() {
    use crate::chain::commit_window;
    let w = commit_window(600);
    assert_eq!((w.margin_blocks, w.era_blocks), (50, 16));
    for secs in [0, 1, 60, 96, 120, 600, 3_600, 86_400] {
        let w = commit_window(secs);
        assert!(w.margin_blocks >= 8);
        assert!(w.era_blocks.is_power_of_two() && w.era_blocks >= 4);
        assert!(
            u64::from(w.era_blocks) * 2 <= w.margin_blocks,
            "{secs}: {w:?}"
        );
    }
}

/// A v1 opening names no header: the named artifact supplies it (as
/// `upload` migrates the file), so its session is still recognised.
#[test]
fn a_v1_opening_is_matched_through_the_named_artifact() {
    let old = real(COHORT, 1, 1, Some(OPEN_BLOCK + 100));
    let mut new = real(COHORT, 2, 1, Some(OPEN_BLOCK + 250));
    new.1.schema = crate::opening::OPENING_SCHEMA_V1.into();
    new.1.header_sha256 = None;
    new.1.header_len = None;
    new.1.object_len = None;
    assert_eq!(new.1.session_id(), None);
    let mut case = open_case();
    case.openings = vec![old.clone(), new.clone()];
    case.chain = Some(chain(
        OPEN_BLOCK + 300,
        true,
        Some((OPEN_BLOCK + 250, real_payload(&new))),
    ));
    case.view = Some(view(
        refused_session(&sid(&old), "commitment_mismatch"),
        Some((OPEN_BLOCK + 250, real_payload(&new))),
    ));
    let d = case.run(&POLICY);
    assert_eq!(
        d.action,
        Action::Upload {
            opening: new.0.clone()
        },
        "{d:?}"
    );
}

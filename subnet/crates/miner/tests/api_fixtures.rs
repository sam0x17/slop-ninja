//! The status API's own golden answers (copied from the website repository's
//! `tests/golden/api/`, web branch `intake-progress`; `tests/fixtures/api/`)
//! through the client's decoder and diagnosis: the per-miner rows the
//! website composes from the publisher's roster document, with challenge
//! counts during scoring. Synthetic hotkeys only.

use slop_ninja_miner::intake::{Action, Inputs, Mark, Policy, State, diagnose};
use slop_ninja_miner::status_api::{ApiStatus, decode};

fn fixture(name: &str) -> ApiStatus {
    let path = format!("{}/tests/fixtures/api/{name}", env!("CARGO_MANIFEST_DIR"));
    decode(200, &std::fs::read(path).unwrap()).unwrap()
}

fn for_hotkey(api: &ApiStatus, hotkey: &str) -> slop_ninja_miner::intake::Diagnosis {
    diagnose(
        &Inputs {
            hotkey,
            api: Some(api),
            api_error: None,
            cohort_read: None,
            view: None,
            chain: None,
            openings: &[],
            artifact: None,
            signed_closed: false,
            now: 0,
        },
        &Policy {
            allow_replace: false,
            commit_margin_blocks: 50,
            reads_chain: false,
        },
    )
}

const UID1: &str = "5C7LYpP2ZH3tpKbvVvwiVe54AapxErdPBbvkYhe6y9ZBkqWt";
const UID3: &str = "5C62Ck4UrFPiBtoCmeSrgF7x9yv9mn38446dhCpsi2mLHiFT";
const UID4: &str = "5C9yEy27yLNG5BDMxVwS8RyGBneZB1ouShazFhGZVP8thK5z";
const UID5: &str = "5CBHb3LfgN2Shc25gnSHwpvNCPZMe6QAaFR77C5nkVvkAK1o";
const GONE: &str = "5CCbw7fDPPgdL2poR4w9mDsUCzUA7AzRhoFDxgu21cibdUmW";

#[test]
fn a_scoring_roster_answers_each_entrant() {
    let api = fixture("status-scoring-roster.json");
    assert!(api.registry_complete());
    let d = for_hotkey(&api, UID4);
    assert_eq!(d.state, State::Evaluating);
    assert_eq!(d.answers.registered.answer, Mark::Yes);
    assert!(d.answers.registered.detail.contains("uid 4"));
    assert_eq!(d.answers.admitted.answer, Mark::Yes);
    assert_eq!(d.answers.challenges_started.answer, Mark::Yes);
    assert_eq!(d.answers.challenges_done.detail, "142 of 312");
    assert_eq!(d.action, Action::None);
    let d = for_hotkey(&api, UID5);
    assert_eq!(d.state, State::RefusedArtifact);
    assert_eq!(d.answers.upload_verified.answer, Mark::No);
    // Registered without an entry, after the cutoff: the next cohort.
    for hotkey in [UID1, UID3] {
        let d = for_hotkey(&api, hotkey);
        assert_eq!(d.state, State::MissedCutoff, "{hotkey}");
        assert!(d.next.contains("next"), "{}", d.next);
    }
    // Overflow, and no longer registered: the state stays, with a warning.
    let d = for_hotkey(&api, GONE);
    assert_eq!(d.state, State::Overflow);
    assert!(
        d.warnings
            .iter()
            .any(|w| w.contains("no longer holds a UID"))
    );
    // A hotkey the complete registry does not name.
    let d = for_hotkey(&api, "5DAAnrj7VHTznn2AWBemMuyBwZWs6FNFjdyVXUeYum3PTXFy");
    assert_eq!(d.state, State::NotRegistered);
}

#[test]
fn a_frozen_roster_shows_challenges_pending() {
    let api = fixture("status-closing-roster.json");
    let d = for_hotkey(&api, UID4);
    assert_eq!(d.state, State::Admitted);
    assert_eq!(d.answers.challenges_started.answer, Mark::Pending);
    assert!(
        d.answers
            .challenges_started
            .detail
            .contains("challenges pending")
    );
    let d = for_hotkey(&api, UID3);
    assert_eq!(d.state, State::MissedCutoff);
}

#[test]
fn an_idle_feed_names_no_cohort() {
    let api = fixture("status-idle.json");
    let d = for_hotkey(&api, UID4);
    assert_eq!(d.state, State::NoCohort);
    assert_eq!(d.action, Action::Wait);
    assert_eq!(d.answers.challenges_done.answer, Mark::Unknown);
}

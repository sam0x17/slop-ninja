//! The miner client against a local fake of the deployed routes: the mock
//! fronted by the key-free upload Worker's own gate
//! (`slop_ninja_admission_gate`, `upload_gate`), so every request
//! the client signs is verified by the code the upload Worker runs, in its
//! order (signature, registration, body), before the mock decides as the
//! cohort authority. Nothing is signed for or sent to a chain.

mod support;

use std::time::Duration;

use slop_ninja_admission_wire::{ErrorCode, SessionState, path};
use slop_ninja_miner::client::{ApiError, http_client};
use slop_ninja_miner::opening::Store;
use slop_ninja_miner::upload::{UploadOutcome, upload};
use support::common::*;
use support::mock::Mock;

fn api_code(error: &anyhow::Error) -> Option<ErrorCode> {
    error.downcast_ref::<ApiError>().and_then(ApiError::code)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_registered_miner_uploads_through_the_upload_workers_gate() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let (artifact_path, artifact) = packed(dir.path(), 0x21, "{\"publisher\":\"alice\"}");
    let mock = Mock::start(config(), 1_950).await;
    mock.with(|s| s.upload_gate = true);
    let alice = entrant(&mock, &store, "//Alice", 12, &artifact, 2_000);
    let mut c = client(&mock, alice.signer.clone()).await;
    let outcome = upload(&mut c, &store, &alice.prepared.file, &artifact_path, &artifact, &quick())
        .await
        .unwrap();
    let UploadOutcome::Finished(session) = outcome else {
        panic!("not finished: {outcome:?}");
    };
    assert_eq!(session.state(), Some(SessionState::Verified));
    // Every signed request, parts included, passed the gate.
    let (refused, parts) = mock.with(|s| (s.counters.gate_refused, s.counters.routes.get("part").copied()));
    assert_eq!(refused, 0);
    assert!(parts.unwrap_or(0) > 1, "the object went up in several parts");
    // The hotkey view is signed and passes the gate too.
    let view = c.hotkey_view(7).await.unwrap();
    assert!(view.last_refusal.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_and_unsigned_requests_are_refused_by_the_gate_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let (artifact_path, artifact) = packed(dir.path(), 0x22, "{\"publisher\":\"bob\"}");
    let mock = Mock::start(config(), 1_950).await;
    mock.with(|s| s.upload_gate = true);

    // A hotkey that commits but holds no UID: the gate refuses its first
    // signed request with not_registered, and no session exists.
    let bob = entrant(&mock, &store, "//Bob", 13, &artifact, 2_000);
    mock.with(|s| s.registrations.clear());
    let mut c = client(&mock, bob.signer.clone()).await;
    let err = upload(&mut c, &store, &bob.prepared.file, &artifact_path, &artifact, &quick())
        .await
        .unwrap_err();
    assert_eq!(api_code(&err), Some(ErrorCode::NotRegistered), "{err:#}");
    let e = c.hotkey_view(7).await.unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::NotRegistered));
    let created = mock.with(|s| s.counters.routes.get("create").copied().unwrap_or(0));
    assert!(created >= 1);
    assert!(mock.with(|s| s.counters.part_puts.is_empty()), "no part reached the authority");

    // An unsigned request: bad_signature, before anything else.
    let http = http_client(Duration::from_secs(10)).unwrap();
    let status = http
        .post(format!("{}{}", mock.url().trim_end_matches('/'), path::SESSIONS))
        .body(vec![0u8; 16])
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, ErrorCode::BadSignature.http_status().unwrap());
    // The unsigned cohort read is the one route the gate lets through.
    let status = http
        .get(format!("{}{}", mock.url().trim_end_matches('/'), path::COHORT))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 200);
    assert!(mock.with(|s| s.counters.gate_refused) >= 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edge_error_page_is_retried_as_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let (artifact_path, artifact) = packed(dir.path(), 0x23, "{\"publisher\":\"carol\"}");
    let mock = Mock::start(config(), 1_950).await;
    mock.with(|s| s.upload_gate = true);
    let carol = entrant(&mock, &store, "//Charlie", 14, &artifact, 2_000);
    let mut c = client(&mock, carol.signer.clone()).await;
    // Two gateway failures without a contract body: the client retries
    // them as `unavailable` and the upload finishes.
    mock.with(|s| s.edge_errors = 2);
    let outcome = upload(&mut c, &store, &carol.prepared.file, &artifact_path, &artifact, &quick())
        .await
        .unwrap();
    assert!(matches!(outcome, UploadOutcome::Finished(_)), "{outcome:?}");
    assert_eq!(mock.with(|s| s.edge_errors), 0);
    assert!(slop_ninja_miner::client::edge_unavailable(502) && slop_ninja_miner::client::edge_unavailable(524));
    assert!(!slop_ninja_miner::client::edge_unavailable(500) && !slop_ninja_miner::client::edge_unavailable(404));
}

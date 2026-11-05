//! More cases against the local mock of the admission contract v2: every
//! request and session refusal not covered in `e2e_mock.rs`, the last refused
//! request in the hotkey view, a replayed signed request that can only mint
//! URLs for the same keys, the commitment log at the cutoff, and the
//! transport rules (`Retry-After`, redirects and response limits). Nothing
//! is signed for or sent to a chain.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use slop_ninja_admission_wire::json::LastRefusal;
use slop_ninja_admission_wire::json::Session;
use slop_ninja_admission_wire::{
    ErrorCode, RESPONSE_BASE_BYTES, RequestScope, SessionState, empty_sha256, header, path,
    request_message,
};
use slop_ninja_miner::check::{ArtifactCheck, CheckContext, SpecCheck};
use slop_ninja_miner::client::{
    AdmissionClient, ApiError, RequestSigner, base_url, http_client, ss58,
};
use slop_ninja_miner::direct::{PutTarget, Silent, put_object};
use slop_ninja_miner::opening::{OsNonce, Store, make_opening, upload_envelope};
use slop_ninja_miner::upload::{UploadOptions, UploadOutcome, describe, upload};
use slop_ninja_miner::{commit, object};
use slop_ninja_protocol::submission::SubmissionUpload;
use slop_ninja_protocol::{ChainBinding, Record, object_sha256};
use slop_ninja_r2_presign::layout::{ObjectId, ObjectKind};
use support::common::*;
use support::mock::{CheckHook, Mock};

fn api_code(error: &anyhow::Error) -> Option<ErrorCode> {
    error.downcast_ref::<ApiError>().and_then(ApiError::code)
}

/// The hotkey view's last refused request, which must have `code`.
async fn refused_as(client: &mut AdmissionClient, code: ErrorCode) -> LastRefusal {
    let view = client.hotkey_view(7).await.unwrap();
    let last = view.last_refusal.expect("a recorded refusal");
    assert_eq!(last.code(), Some(code), "{last:?}");
    assert_eq!(last.opening_cohort, 7);
    last
}

fn finished(outcome: UploadOutcome) -> Session {
    match outcome {
        UploadOutcome::Finished(session) => *session,
        other => panic!("not finished: {other:?}"),
    }
}

/// Opens a session for `envelope` and PUTs `header` (the bytes at offset
/// `offset` of the file at `path`) as its header object with the low-level
/// calls, for envelopes the high-level upload refuses to build; then asks
/// for the file URLs, where the coordinator settles the header.
async fn raw_header(
    client: &mut AdmissionClient,
    envelope: &SubmissionUpload,
    path: &std::path::Path,
    offset: u64,
) -> Session {
    let (session, _) = client.create_session(envelope).await.unwrap();
    let target = PutTarget {
        id: ObjectId {
            cohort: envelope.opening.cohort,
            commitment: envelope.commitment.0,
            kind: ObjectKind::Header,
        },
        sha256: envelope.header_sha256.0,
        len: u64::from(envelope.header_len),
        path: path.to_path_buf(),
        offset,
        name: "header".into(),
    };
    put_object(
        client.http(),
        client.put_policy(),
        &session.uploads[0],
        &target,
        Duration::from_secs(10),
        Arc::new(Silent),
    )
    .await
    .unwrap();
    client.urls(&session.session).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_refusals_are_recorded_for_status() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (_, artifact) = packed(dir.path(), 0x41, "{}");
    let alice = dev_signer("//Alice");
    let alice_id = identity(&alice, 12);
    let mut alice_client = client(&mock, alice.clone()).await;

    // Not registered.
    let opening = make_opening(
        &chain601(),
        7,
        alice_id.hotkey,
        alice_id.registration_block,
        &artifact,
        &mut OsNonce,
    )
    .unwrap();
    let envelope = upload_envelope(opening.clone(), &artifact).unwrap();
    let error = alice_client.create_session(&envelope).await.unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::NotRegistered));
    let last = refused_as(&mut alice_client, ErrorCode::NotRegistered).await;
    assert_eq!(last.session, envelope.digest().unwrap().hex());

    // Another chain or subnet.
    mock.register(alice_id);
    let other_chain = ChainBinding {
        netuid: 602,
        ..chain601()
    };
    let opening_602 = make_opening(
        &other_chain,
        7,
        alice_id.hotkey,
        alice_id.registration_block,
        &artifact,
        &mut OsNonce,
    )
    .unwrap();
    let error = alice_client
        .create_session(&upload_envelope(opening_602, &artifact).unwrap())
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::WrongChain));
    refused_as(&mut alice_client, ErrorCode::WrongChain).await;

    // An opening for another hotkey than the signer.
    let bob = dev_signer("//Bob");
    let for_bob =
        make_opening(&chain601(), 7, bob.hotkey(), 1_013, &artifact, &mut OsNonce).unwrap();
    let error = alice_client
        .create_session(&upload_envelope(for_bob, &artifact).unwrap())
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::HotkeyMismatch));
    refused_as(&mut alice_client, ErrorCode::HotkeyMismatch).await;

    // Another registration generation than the hotkey's: the opening names
    // an earlier registration block, and the chain holds its commitment.
    let stale = make_opening(
        &chain601(),
        7,
        alice_id.hotkey,
        alice_id.registration_block - 500,
        &artifact,
        &mut OsNonce,
    )
    .unwrap();
    mock.commit(alice_id.hotkey, 2_000, 0, &stale.chain_payload());
    let error = alice_client
        .create_session(&upload_envelope(stale, &artifact).unwrap())
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::RegistrationMismatch));
    refused_as(&mut alice_client, ErrorCode::RegistrationMismatch).await;

    // A later accepted request does not erase the record; a later refusal
    // replaces it.
    let good = commit::prepare(
        &store,
        &chain601(),
        7,
        &alice_id,
        &artifact,
        &mut OsNonce,
        true,
    )
    .unwrap();
    mock.commit(alice_id.hotkey, 2_010, 0, &good.payload);
    let envelope = upload_envelope(good.opening.clone(), &artifact).unwrap();
    alice_client.create_session(&envelope).await.unwrap();
    refused_as(&mut alice_client, ErrorCode::RegistrationMismatch).await;
    let error = alice_client
        .create_session(&upload_envelope(opening, &artifact).unwrap())
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::CommitmentMismatch));
    refused_as(&mut alice_client, ErrorCode::CommitmentMismatch).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_refusals_reach_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;

    // contract_mismatch: the header names another contract.
    let src = dir.path().join("other");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("model.onnx"), vec![3u8; 2_000]).unwrap();
    let other_path = dir.path().join("other.sna");
    let other = object::pack(
        "another_contract_v1",
        &[object::PackInput::parse("graph:model.onnx", &src).unwrap()],
        &other_path,
    )
    .unwrap();
    let a = entrant(&mock, &store, "//Alice", 12, &other, 2_000);
    let mut a_client = client(&mock, a.signer.clone()).await;
    let session = finished(
        upload(
            &mut a_client,
            &store,
            &a.prepared.file,
            &other_path,
            &other,
            &quick(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        session.refusal.as_ref().unwrap().code(),
        Some(ErrorCode::ContractMismatch)
    );
    assert!(
        describe(&session).contains("commit again"),
        "{}",
        describe(&session)
    );
    // An artifact doomed by its header is refused before any file is sent.
    assert_eq!(mock.puts(), 1);

    // content_mismatch: the object is not the content the opening commits to.
    let (path_a, art_a) = packed(dir.path(), 0x51, "{}");
    let (path_b, art_b) = packed(dir.path(), 0x52, "{}");
    assert_ne!(art_a.content, art_b.content);
    let b = entrant(&mock, &store, "//Bob", 13, &art_a, 2_010);
    let envelope = SubmissionUpload {
        commitment: b.prepared.opening.commitment(),
        opening: b.prepared.opening.clone(),
        header_sha256: art_b.sha256,
        header_len: art_b.len,
    };
    let mut b_client = client(&mock, b.signer.clone()).await;
    let session = raw_header(&mut b_client, &envelope, &path_b, 4).await;
    assert_eq!(
        session.refusal.unwrap().code(),
        Some(ErrorCode::ContentMismatch)
    );
    // The high-level upload refuses to build that envelope at all.
    let error = upload(
        &mut b_client,
        &store,
        &b.prepared.file,
        &path_b,
        &art_b,
        &quick(),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("not the artifact this opening was made for"),
        "{error:#}"
    );

    // artifact_invalid: bytes that are not an artifact object.
    let c = entrant(&mock, &store, "//Charlie", 14, &art_a, 2_020);
    let junk: Vec<u8> = (0..2_500u32).map(|i| (i * 7) as u8).collect();
    let junk_path = dir.path().join("junk");
    std::fs::write(&junk_path, &junk).unwrap();
    let envelope = SubmissionUpload {
        commitment: c.prepared.opening.commitment(),
        opening: c.prepared.opening.clone(),
        header_sha256: object_sha256(&junk),
        header_len: junk.len() as u32,
    };
    let mut c_client = client(&mock, c.signer.clone()).await;
    let session = raw_header(&mut c_client, &envelope, &junk_path, 0).await;
    let refusal = session.refusal.unwrap();
    assert_eq!(refusal.code(), Some(ErrorCode::ArtifactInvalid));
    assert!(refusal.detail.reason.is_some());

    // check_failed with the artifact spec's own code (here from its
    // header-only checks, before any file is sent), which is the code the
    // client's check reports for the same object.
    mock.with(|s| s.check_hook = Some(CheckHook::Spec));
    let d = entrant(&mock, &store, "//Dave", 15, &art_a, 2_030);
    let mut d_client = client(&mock, d.signer.clone()).await;
    let session = finished(
        upload(
            &mut d_client,
            &store,
            &d.prepared.file,
            &path_a,
            &art_a,
            &quick(),
        )
        .await
        .unwrap(),
    );
    let refusal = session.refusal.clone().unwrap();
    assert_eq!(refusal.code(), Some(ErrorCode::CheckFailed));
    let local = SpecCheck
        .check(&CheckContext {
            object: &path_a,
            header: &art_a,
            expected_contract: None,
            max_object_len: None,
            artifact_spec: None,
            cohort_unknown: None,
            next_cohort: None,
        })
        .unwrap();
    let local_code = local
        .findings
        .iter()
        .find(|f| f.refuses)
        .unwrap()
        .code
        .clone();
    assert_eq!(refusal.detail.check.as_deref(), Some(local_code.as_str()));
    assert!(describe(&session).contains(&format!("[check {local_code}]")));

    // check_failed for a graph check, as the coordinator reports it.
    mock.with(|s| s.check_hook = Some(CheckHook::Refuse("graph_op_not_allowed".into())));
    let e = entrant(&mock, &store, "//Eve", 16, &art_a, 2_040);
    let mut e_client = client(&mock, e.signer.clone()).await;
    let session = finished(
        upload(
            &mut e_client,
            &store,
            &e.prepared.file,
            &path_a,
            &art_a,
            &quick(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        session.refusal.unwrap().detail.check.as_deref(),
        Some("graph_op_not_allowed")
    );

    // A refused envelope stays refused; a new commitment (a new nonce) for
    // the same object opens a new session that the checks now pass.
    mock.with(|s| s.check_hook = None);
    let again = finished(
        upload(
            &mut e_client,
            &store,
            &e.prepared.file,
            &path_a,
            &art_a,
            &quick(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(again.state(), Some(SessionState::Refused));
    let renewed = commit::prepare(
        &store,
        &chain601(),
        7,
        &e.identity,
        &art_a,
        &mut OsNonce,
        true,
    )
    .unwrap();
    mock.commit(e.identity.hotkey, 2_045, 0, &renewed.payload);
    let again = finished(
        upload(
            &mut e_client,
            &store,
            &renewed.file,
            &path_a,
            &art_a,
            &quick(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(again.state(), Some(SessionState::Verified));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paused_and_rate_limited_requests_wait_as_told() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (path_a, art_a) = packed(dir.path(), 0x61, "{}");
    let a = entrant(&mock, &store, "//Alice", 12, &art_a, 2_000);
    let mut a_client = client(&mock, a.signer.clone()).await;

    // Paused: retryable; without retries the miner sees the code.
    mock.with(|s| s.paused_requests = 1);
    let error = upload(
        &mut a_client,
        &store,
        &a.prepared.file,
        &path_a,
        &art_a,
        &once(),
    )
    .await
    .unwrap_err();
    assert_eq!(api_code(&error), Some(ErrorCode::Paused));
    mock.with(|s| s.paused_requests = 2);
    let session = finished(
        upload(
            &mut a_client,
            &store,
            &a.prepared.file,
            &path_a,
            &art_a,
            &quick(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(session.state(), Some(SessionState::Verified));

    // Rate limited with Retry-After: 1 waits a second, not the 10 ms backoff.
    let b = entrant(&mock, &store, "//Bob", 13, &art_a, 2_010);
    let mut b_client = client(&mock, b.signer.clone()).await;
    mock.with(|s| {
        s.rate_limited_requests = 1;
        s.retry_after_secs = 1;
    });
    let started = Instant::now();
    let session = finished(
        upload(
            &mut b_client,
            &store,
            &b.prepared.file,
            &path_a,
            &art_a,
            &quick(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(session.state(), Some(SessionState::Verified));
    assert!(
        started.elapsed() >= Duration::from_millis(950),
        "{:?}",
        started.elapsed()
    );
    mock.with(|s| s.rate_limited_requests = 1);
    let error = b_client.hotkey_view(7).await.unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::RateLimited));
    assert_eq!(error.retry_after(), Some(Duration::from_secs(1)));
}

/// Sends a request with exactly these signed headers, as a replaying
/// observer would.
async fn send_signed(
    http: &reqwest::Client,
    base: &str,
    method: reqwest::Method,
    route: &str,
    signer: &Arc<slop_ninja_miner::client::RtaoSigner>,
    valid_until: u64,
) -> impl Fn() -> futures::future::BoxFuture<'static, u16> + use<> {
    let message = request_message(&RequestScope {
        chain: &chain601(),
        hotkey: &signer.hotkey(),
        valid_until,
        method: method.as_str(),
        path: route,
        content_sha256: &empty_sha256(),
    });
    let signature = hex::encode(signer.sign(&message).await.unwrap());
    let url = format!("{}{}", base.trim_end_matches('/'), route);
    let hotkey = ss58(&signer.hotkey());
    let http = http.clone();
    move || {
        let request = http
            .request(method.clone(), &url)
            .header(header::HOTKEY, &hotkey)
            .header(header::VALID_UNTIL, valid_until.to_string())
            .header(header::CONTENT_SHA256, empty_sha256().hex())
            .header(header::SIGNATURE, &signature);
        Box::pin(async move { request.send().await.unwrap().status().as_u16() })
    }
}

/// A signed `urls` request captured and replayed can only mint more URLs
/// for the same derived keys: the content each key admits is fixed by R2's
/// checksum, so a replay moves no byte anywhere else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replayed_urls_request_only_mints_urls_for_the_same_keys() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (path_a, art_a) = packed(dir.path(), 0x81, "{}");
    let a = entrant(&mock, &store, "//Alice", 12, &art_a, 2_000);
    let mut a_client = client(&mock, a.signer.clone()).await;
    let interrupted = upload(
        &mut a_client,
        &store,
        &a.prepared.file,
        &path_a,
        &art_a,
        &UploadOptions {
            stop_after_objects: Some(1),
            ..quick()
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        interrupted,
        UploadOutcome::Interrupted { uploaded: 1, .. }
    ));
    let envelope = upload_envelope(a.prepared.opening.clone(), &art_a).unwrap();
    let id = envelope.digest().unwrap().hex();
    let http = http_client(Duration::from_secs(30)).unwrap();
    let valid_until = mock.with(|s| s.finalized_block) + 300;
    let urls = send_signed(
        &http,
        &mock.url(),
        reqwest::Method::POST,
        &path::urls(&id),
        &a.signer,
        valid_until,
    )
    .await;
    let minted = mock.with(|s| s.counters.minted);
    assert_eq!(urls().await, 200);
    assert_eq!(urls().await, 200);
    assert_eq!(
        mock.with(|s| s.counters.minted),
        minted + 6,
        "three file URLs each time"
    );
    // Every key it can mint is under this commitment's prefix.
    let session = a_client.urls(&id).await.unwrap();
    let prefix = format!("admission/v2/c7/{}/f/", envelope.commitment.hex());
    assert_eq!(session.uploads.len(), 3);
    assert!(
        session
            .uploads
            .iter()
            .all(|u| u.url.contains(&prefix) && u.url.contains(&u.sha256))
    );
    // The upload finishes as if nothing happened.
    let session = finished(
        upload(
            &mut a_client,
            &store,
            &a.prepared.file,
            &path_a,
            &art_a,
            &quick(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(session.state(), Some(SessionState::Verified));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cutoff_reads_the_commitment_log_not_the_latest_commitment() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (path_a, art_a) = packed(dir.path(), 0x91, "{}");
    let mut verified = Vec::new();
    for (uri, uid, block) in [
        ("//Alice", 12, 2_000),
        ("//Bob", 13, 2_010),
        ("//Charlie", 14, 2_020),
    ] {
        let e = entrant(&mock, &store, uri, uid, &art_a, block);
        let mut c = client(&mock, e.signer.clone()).await;
        let session = finished(
            upload(&mut c, &store, &e.prepared.file, &path_a, &art_a, &quick())
                .await
                .unwrap(),
        );
        assert_eq!(session.state(), Some(SessionState::Verified));
        verified.push((e, c, session));
    }
    let cutoff = 2_100;
    // Alice commits for cohort 8 after the cutoff block, before the
    // coordinator processes the close (an outage, or catch-up).
    let alice = &verified[0].0;
    let next = make_opening(
        &chain601(),
        8,
        alice.identity.hotkey,
        alice.identity.registration_block,
        &art_a,
        &mut OsNonce,
    )
    .unwrap();
    mock.commit(alice.identity.hotkey, 2_150, 1, &next.chain_payload());
    // Bob commits again for cohort 7 after the cutoff block: too late to count.
    let bob = &verified[1].0;
    let late = make_opening(
        &chain601(),
        7,
        bob.identity.hotkey,
        bob.identity.registration_block,
        &art_a,
        &mut OsNonce,
    )
    .unwrap();
    mock.commit(bob.identity.hotkey, 2_120, 0, &late.chain_payload());
    // Charlie overwrites his commitment before the cutoff block.
    let charlie = &verified[2].0;
    let early = make_opening(
        &chain601(),
        7,
        charlie.identity.hotkey,
        charlie.identity.registration_block,
        &art_a,
        &mut OsNonce,
    )
    .unwrap();
    mock.commit(charlie.identity.hotkey, 2_090, 0, &early.chain_payload());

    mock.close_at(cutoff);
    let (_, alice_client, alice_session) = &mut verified[0];
    let admitted = alice_client.session(&alice_session.session).await.unwrap();
    assert_eq!(admitted.state(), Some(SessionState::Admitted));
    assert_eq!(
        admitted.position.map(|p| (p.block, p.extrinsic)),
        Some((2_000, 0))
    );
    // The view for cohort 7 shows the commitment that counted, not the
    // later one for cohort 8.
    let view = alice_client.hotkey_view(7).await.unwrap();
    assert_eq!(view.cutoff_block, Some(cutoff));
    let counted = view.commitment.unwrap();
    assert_eq!(
        (counted.block, counted.payload_cohort, counted.in_window),
        (2_000, Some(7), true)
    );
    let (_, bob_client, bob_session) = &mut verified[1];
    let bob_final = bob_client.session(&bob_session.session).await.unwrap();
    assert_eq!(bob_final.state(), Some(SessionState::Admitted));
    assert_eq!(bob_final.position.map(|p| p.block), Some(2_010));
    let (_, charlie_client, charlie_session) = &mut verified[2];
    let charlie_final = charlie_client
        .session(&charlie_session.session)
        .await
        .unwrap();
    assert_eq!(
        charlie_final.refusal.unwrap().code(),
        Some(ErrorCode::CommitmentOverwritten)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redirects_are_never_followed_and_bodies_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (path_a, art_a) = packed(dir.path(), 0xa1, "{}");
    let a = entrant(&mock, &store, "//Alice", 12, &art_a, 2_000);

    // A second host that counts every connection it receives.
    let far = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let far_addr = far.local_addr().unwrap();
    let hits = Arc::new(AtomicU32::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = far.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    mock.with(|s| s.redirect_urls_to = Some(format!("http://{far_addr}/stolen")));
    let mut a_client = client(&mock, a.signer.clone()).await;
    let error = upload(
        &mut a_client,
        &store,
        &a.prepared.file,
        &path_a,
        &art_a,
        &quick(),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("redirect"), "{error:#}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "the redirect target was contacted"
    );

    // A response over the contract's limit is refused, not buffered.
    mock.with(|s| {
        s.redirect_urls_to = None;
        s.pad_cohort_bytes = RESPONSE_BASE_BYTES;
    });
    let http = http_client(Duration::from_secs(30)).unwrap();
    let error =
        AdmissionClient::connect(http, base_url(&mock.url()).unwrap(), a.signer.clone(), None)
            .await
            .err()
            .unwrap();
    assert!(format!("{error:#}").contains("byte limit"), "{error:#}");
}

/// The opening's hotkey must sign; the envelope digest is the session id,
/// whatever account signs the request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_is_only_its_hotkeys() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (_, art_a) = packed(dir.path(), 0xb1, "{}");
    let a = entrant(&mock, &store, "//Alice", 12, &art_a, 2_000);
    let mut a_client = client(&mock, a.signer.clone()).await;
    let (session, _) = a_client
        .create_session(&upload_envelope(a.prepared.opening.clone(), &art_a).unwrap())
        .await
        .unwrap();
    let mut bob_client = client(&mock, dev_signer("//Bob")).await;
    for error in [
        bob_client.session(&session.session).await.unwrap_err(),
        bob_client.urls(&session.session).await.unwrap_err(),
        bob_client.complete(&session.session).await.unwrap_err(),
        bob_client.repair(&session.session).await.unwrap_err(),
    ] {
        assert_eq!(error.code(), Some(ErrorCode::Forbidden));
    }
}

/// Third W3 review finding 1, fix round 1 findings 1, 3 and 9: a returning
/// miner commits again and uploads at once, while the coordinator still
/// holds the hotkey's previous commitment and its reader has not polled
/// since. The coordinator answers `commitment_not_found` (retried as told)
/// until a poll at or after its first answer; the cursor running ahead of
/// the last poll (the heartbeat advances it every block, the reader polls
/// once a minute) never keeps a wrong opening waiting longer than that.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_returning_miner_waits_for_its_new_commitment_to_be_logged() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (artifact_path, artifact) = packed(dir.path(), 0x51, "{}");
    let (artifact2_path, artifact2) = packed(dir.path(), 0x52, "{\"v\":2}");
    let signer = dev_signer("//Dave");
    let id = identity(&signer, 15);
    mock.register(id);
    // The hotkey's previous commitment, made before cohort 7 opened, and a
    // reader that polled since.
    let old = make_opening(
        &chain601(),
        6,
        id.hotkey,
        id.registration_block,
        &artifact,
        &mut OsNonce,
    )
    .unwrap();
    mock.commit(id.hotkey, 1_899, 0, &old.chain_payload());
    mock.poll();
    let prepared =
        commit::prepare(&store, &chain601(), 7, &id, &artifact, &mut OsNonce, true).unwrap();
    let mut c = client(&mock, signer.clone()).await;

    // The heartbeat moves the cursor past the last poll: the first answer is
    // not final yet.
    mock.advance(3);
    let error = upload(
        &mut c,
        &store,
        &prepared.file,
        &artifact_path,
        &artifact,
        &once(),
    )
    .await
    .unwrap_err();
    assert_eq!(api_code(&error), Some(ErrorCode::CommitmentNotFound));
    // The next poll completes; the cursor runs ahead of it again with no new
    // poll, and the definitive answer still comes back.
    mock.poll();
    mock.advance(5);
    let error = upload(
        &mut c,
        &store,
        &prepared.file,
        &artifact_path,
        &artifact,
        &once(),
    )
    .await
    .unwrap_err();
    assert_eq!(api_code(&error), Some(ErrorCode::CommitmentBeforeOpen));

    // A new commitment, logged a moment after the upload starts: the upload
    // retries the not-yet answer and goes through.
    let options = UploadOptions {
        attempts: 10,
        backoff: Duration::from_millis(50),
        ..once()
    };
    let fresh =
        commit::prepare(&store, &chain601(), 7, &id, &artifact, &mut OsNonce, true).unwrap();
    let logged = |payload: [u8; slop_ninja_protocol::submission::SUBMISSION_PAYLOAD_LEN],
                  block: u64| {
        let state = mock.state.clone();
        let hotkey = id.hotkey;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let mock = Mock {
                addr: "127.0.0.1:1".parse().unwrap(),
                r2_addr: "127.0.0.1:1".parse().unwrap(),
                state,
            };
            mock.commit(hotkey, block, 0, &payload);
        })
    };
    let late = logged(fresh.payload, 2_005);
    let started = Instant::now();
    let session = finished(
        upload(
            &mut c,
            &store,
            &fresh.file,
            &artifact_path,
            &artifact,
            &options,
        )
        .await
        .unwrap(),
    );
    late.await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(250));
    assert_eq!(session.state(), Some(SessionState::Verified));

    // Fix round 1, finding 3: a commitment replaced within the window (a new
    // opening for another artifact), uploaded before the reader logs it.
    mock.advance(2);
    let replaced =
        commit::prepare(&store, &chain601(), 7, &id, &artifact2, &mut OsNonce, true).unwrap();
    let late = logged(replaced.payload, 2_020);
    let session = finished(
        upload(
            &mut c,
            &store,
            &replaced.file,
            &artifact2_path,
            &artifact2,
            &options,
        )
        .await
        .unwrap(),
    );
    late.await.unwrap();
    assert_eq!(session.state(), Some(SessionState::Verified));

    // Fix round 2, finding 1: the coordinator's cursor lags the block of the
    // wallet's newest commitment (a heartbeat or webhook stall) and its
    // reader last polled at that cursor. The upload sends the confirmed
    // block, so no poll below it makes the older commitment's answer final;
    // once the cursor reaches the block the commitment is logged and the
    // upload goes through.
    mock.poll();
    let lagging =
        commit::prepare(&store, &chain601(), 7, &id, &artifact, &mut OsNonce, true).unwrap();
    let cursor = mock.with(|s| s.finalized_block);
    // Past the cursor and past every commitment this test logged so far.
    let confirmed_at = cursor.max(2_020) + 10;
    let mut file = lagging.file.clone();
    file.submitted = Some(slop_ninja_miner::opening::Submitted {
        finalized_commitment_block: Some(confirmed_at),
        ..Default::default()
    });
    let error = upload(&mut c, &store, &file, &artifact_path, &artifact, &once())
        .await
        .unwrap_err();
    assert_eq!(api_code(&error), Some(ErrorCode::CommitmentNotFound));
    assert_eq!(
        mock.with(|s| s.commitment_block_hints.last().copied()),
        Some(Some(confirmed_at))
    );
    mock.advance((confirmed_at - cursor) / 2);
    mock.poll();
    let error = upload(&mut c, &store, &file, &artifact_path, &artifact, &once())
        .await
        .unwrap_err();
    assert_eq!(
        api_code(&error),
        Some(ErrorCode::CommitmentNotFound),
        "a poll below the block is not final"
    );
    mock.advance(confirmed_at - mock.with(|s| s.finalized_block));
    mock.commit(id.hotkey, confirmed_at, 0, &lagging.payload);
    mock.poll();
    let session = finished(
        upload(&mut c, &store, &file, &artifact_path, &artifact, &once())
            .await
            .unwrap(),
    );
    assert_eq!(session.state(), Some(SessionState::Verified));
}

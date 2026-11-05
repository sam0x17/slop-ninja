//! End to end against a local mock of the admission contract v2 and of R2's
//! presigned PUT: pack (each file hashed once), check (no hash), commit (the
//! opening and the composed call; nothing is signed for or sent to a chain),
//! an interrupted and resumed direct upload, verification, the cutoff, and
//! every admission refusal the client must show.

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use rtao_runtime::RuntimeHandle;
use slop_ninja_admission_wire::{ErrorCode, SessionState};
use slop_ninja_miner::check::{CheckContext, Outcome, SpecCheck, StructuralCheck, run_all};
use slop_ninja_miner::client::{
    AdmissionClient, ApiError, RequestSigner, RtaoSigner, http_client, ss58,
};
use slop_ninja_miner::opening::{
    NonceSource, OPENING_SCHEMA, OPENING_SCHEMA_V1, OsNonce, Store, load_opening, upload_envelope,
};
use slop_ninja_miner::upload::{UploadOptions, UploadOutcome, describe, upload};
use slop_ninja_miner::{chain, commit, object};
use slop_ninja_protocol::{AccountId, FileRole};
use support::common::*;
use support::mock::{Fault, Match, Mock};

const METADATA: &[u8] = include_bytes!("../../../fixtures/runtime-486/testnet-486.v15.scale");

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn api_code(error: &anyhow::Error) -> Option<ErrorCode> {
    error.downcast_ref::<ApiError>().and_then(ApiError::code)
}

/// The headers every PUT carried: exactly the three signed ones (and the
/// HTTP stack's own), never another `x-amz-*`, `content-encoding` or chunked
/// transfer encoding.
fn assert_only_signed_headers(mock: &Mock) {
    let all = mock.with(|s| s.r2.put_headers.clone());
    assert!(!all.is_empty());
    for names in all {
        for needed in [
            "host",
            "content-length",
            "if-none-match",
            "x-amz-checksum-sha256",
        ] {
            assert!(
                names.iter().any(|n| n == needed),
                "{needed} missing in {names:?}"
            );
        }
        for name in &names {
            assert!(
                !name.starts_with("x-amz-") || name == "x-amz-checksum-sha256",
                "unsigned {name} in {names:?}"
            );
            assert!(
                name != "transfer-encoding" && name != "content-encoding",
                "{name} in {names:?}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_check_commit_upload_resume_and_admit() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    assert_eq!(mode(store.root()), 0o700);
    assert_eq!(mode(&store.root().join("openings")), 0o700);

    // pack: the object, written while each file is hashed once, is
    // byte-identical to the protocol's builder.
    let owner_text = "{\"publisher\":\"alice\"}";
    let (artifact_path, artifact) = packed(dir.path(), 0x11, owner_text);
    let src = dir.path().join(format!("src-17-{}", owner_text.len()));
    let graph = std::fs::read(src.join("model.onnx")).unwrap();
    let tokenizer = std::fs::read(src.join("tok/tokenizer.json")).unwrap();
    let owner = std::fs::read(src.join("owner.json")).unwrap();
    let reference = slop_ninja_protocol::artifact_object(
        CONTRACT,
        &[
            ("tok/tokenizer.json", FileRole::Tokenizer, &tokenizer),
            ("owner.json", FileRole::OwnerMetadata, &owner),
            ("model.onnx", FileRole::Graph, &graph),
        ],
    )
    .unwrap();
    assert_eq!(std::fs::read(&artifact_path).unwrap(), reference);
    assert_eq!(object::read_header(&artifact_path).unwrap(), artifact);
    assert_eq!(artifact.object_len, reference.len() as u64);

    // check: the cohort's contract and bound pass; the artifact spec's
    // admission checks refuse this synthetic object (it is not the spec's
    // six-file table), with the code the coordinator would report.
    let cx = CheckContext {
        object: &artifact_path,
        header: &artifact,
        expected_contract: Some(CONTRACT),
        max_object_len: Some(1 << 20),
        artifact_spec: None,
        cohort_unknown: None,
        next_cohort: None,
    };
    let (outcome, reports) = run_all(&[&StructuralCheck], &cx).unwrap();
    assert_eq!(outcome, Outcome::Pass);
    let (outcome, reports_spec) = run_all(&[&StructuralCheck, &SpecCheck], &cx).unwrap();
    assert_eq!(outcome, Outcome::Refused);
    assert!(reports.len() == 1 && reports_spec.len() == 2);
    assert!(
        reports_spec[1]
            .findings
            .iter()
            .any(|f| f.refuses && f.code == "undeclared_file"),
        "{reports_spec:?}"
    );

    // commit: a fresh opening stored 0600 (schema v2: it names the header),
    // and the call composed through rtao.
    let alice = dev_signer("//Alice");
    let alice_id = identity(&alice, 12);
    let prepared = commit::prepare(
        &store,
        &chain601(),
        7,
        &alice_id,
        &artifact,
        &mut OsNonce,
        false,
    )
    .unwrap();
    assert_eq!(mode(&prepared.path), 0o600);
    assert_eq!(load_opening(&prepared.path).unwrap(), prepared.file);
    assert_eq!(prepared.file.schema, OPENING_SCHEMA);
    assert_eq!(
        prepared.file.header_sha256.as_deref(),
        Some(artifact.sha256.hex().as_str())
    );
    let runtime = RuntimeHandle::parse(METADATA, 486, 1, 42).unwrap();
    let call = chain::compose(&chain::commitment_call(601, &prepared.payload), &runtime).unwrap();
    assert_eq!(&call.as_bytes()[..6], &[0x12, 0x00, 0x59, 0x02, 0x04, 0x2d]);
    assert_eq!(&call.as_bytes()[6..], prepared.payload.as_slice());

    // The commitment finalizes on the mock chain.
    let mock = Mock::start(config(), 1_950).await;
    mock.register(alice_id);
    mock.commit(alice_id.hotkey, 2_000, 3, &prepared.payload);

    // An upload interrupted after two objects (the header, then the largest
    // file)...
    let mut first = client(&mock, alice.clone()).await;
    let interrupted = upload(
        &mut first,
        &store,
        &prepared.file,
        &artifact_path,
        &artifact,
        &UploadOptions {
            stop_after_objects: Some(2),
            ..quick()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        interrupted,
        UploadOutcome::Interrupted {
            uploaded: 2,
            remaining: 2
        }
    );
    assert_eq!(mock.puts(), 2);
    // ...resumes in a fresh client, through one transient R2 failure (503),
    // which gets a fresh URL and resends that file from byte 0.
    mock.with(|s| s.r2.faults = vec![(Match::File, Fault::Status(503, "SlowDown"))]);
    let recorder = Arc::new(Recorder::default());
    let mut second = client(&mock, alice.clone()).await;
    let outcome = upload(
        &mut second,
        &store,
        &prepared.file,
        &artifact_path,
        &artifact,
        &UploadOptions {
            progress: recorder.clone(),
            ..quick()
        },
    )
    .await
    .unwrap();
    let UploadOutcome::Finished(session) = outcome else {
        panic!("finished")
    };
    assert_eq!(
        session.state(),
        Some(SessionState::Verified),
        "{}",
        describe(&session)
    );
    let puts = mock.with(|s| s.r2.puts.clone());
    assert_eq!(puts.len(), 4, "one key per object: {puts:?}");
    assert_eq!(
        puts.values().sum::<u32>(),
        5,
        "the two sent objects are not sent again; one retry: {puts:?}"
    );
    assert_eq!(puts.values().filter(|n| **n == 2).count(), 1);
    // Progress counted the body bytes it streamed (the retried file twice).
    let events = recorder.events.lock().unwrap().clone();
    assert!(
        events.iter().any(|e| e.ends_with(" 2")),
        "a second attempt: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| e.contains("not stored") || e.contains("false")),
        "{events:?}"
    );
    let sent = recorder.bytes.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        sent > 0 && sent <= mock.with(|s| s.r2.bytes_received),
        "{sent}"
    );
    assert_only_signed_headers(&mock);
    let state_file = store.upload_state_path(&session.session);
    assert_eq!(mode(&state_file), 0o600);

    // A rerun after success sends nothing.
    let again = upload(
        &mut second,
        &store,
        &prepared.file,
        &artifact_path,
        &artifact,
        &quick(),
    )
    .await
    .unwrap();
    assert!(
        matches!(again, UploadOutcome::Finished(ref s) if s.state() == Some(SessionState::Verified))
    );
    assert_eq!(mock.puts(), 5);

    // status: the coordinator's view of the hotkey. The coordinator's cursor
    // has moved past the client's validity window, so the first signature
    // expires; the refusal carries the cursor and the client re-signs from it.
    mock.with(|s| s.finalized_block += 1_000);
    let before = mock.with(|s| s.counters.requests);
    let view = second.hotkey_view(7).await.unwrap();
    assert_eq!(
        mock.with(|s| s.counters.requests) - before,
        2,
        "expired, re-signed from the carried cursor"
    );
    // Without a carried cursor, the client reads the cohort first.
    mock.with(|s| {
        s.finalized_block += 1_000;
        s.expired_with_cursor = false;
    });
    let before = mock.with(|s| s.counters.requests);
    second.hotkey_view(7).await.unwrap();
    assert_eq!(
        mock.with(|s| s.counters.requests) - before,
        3,
        "expired, cohort read, re-signed"
    );
    mock.with(|s| s.expired_with_cursor = true);
    assert_eq!(view.registration.unwrap().uid, 12);
    let seen = view.commitment.unwrap();
    assert_eq!(
        (
            seen.block,
            seen.extrinsic,
            seen.payload_cohort,
            seen.in_window
        ),
        (2_000, 3, Some(7), true)
    );
    assert_eq!(seen.block_hash, Some(support::mock::block_hash(2_000)));
    assert_eq!(seen.payload, prepared.file.chain_payload);
    assert_eq!(view.sessions[0].state, "verified");

    // The cutoff admits it at its commitment position.
    mock.close();
    let admitted = second.session(&session.session).await.unwrap();
    assert_eq!(admitted.state(), Some(SessionState::Admitted));
    let position = admitted.position.unwrap();
    assert_eq!((position.block, position.extrinsic), (2_000, 3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusals_reach_the_miner() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (artifact_path, artifact) = packed(dir.path(), 0x21, "{\"v\":1}");
    let one = UploadOptions {
        attempts: 1,
        ..quick()
    };

    // No commitment, or one not yet final: refused, and retryable.
    let bob = dev_signer("//Bob");
    let bob_id = identity(&bob, 13);
    mock.register(bob_id);
    let bob_opening = commit::prepare(
        &store,
        &chain601(),
        7,
        &bob_id,
        &artifact,
        &mut OsNonce,
        false,
    )
    .unwrap();
    let mut bob_client = client(&mock, bob.clone()).await;
    let error = upload(
        &mut bob_client,
        &store,
        &bob_opening.file,
        &artifact_path,
        &artifact,
        &one,
    )
    .await
    .unwrap_err();
    assert_eq!(
        api_code(&error),
        Some(ErrorCode::CommitmentNotFound),
        "{error:#}"
    );
    let view = bob_client.hotkey_view(7).await.unwrap();
    assert!(view.commitment.is_none() && view.sessions.is_empty());

    // A commitment made before the cohort opened.
    mock.commit(bob_id.hotkey, 1_899, 0, &bob_opening.payload);
    let error = upload(
        &mut bob_client,
        &store,
        &bob_opening.file,
        &artifact_path,
        &artifact,
        &one,
    )
    .await
    .unwrap_err();
    assert_eq!(api_code(&error), Some(ErrorCode::CommitmentBeforeOpen));

    // An overwritten commitment: the chain holds a newer one.
    let newer = commit::prepare(
        &store,
        &chain601(),
        7,
        &bob_id,
        &artifact,
        &mut OsNonce,
        true,
    )
    .unwrap();
    assert_ne!(
        newer.payload, bob_opening.payload,
        "a fresh nonce each time"
    );
    mock.commit(bob_id.hotkey, 2_010, 1, &newer.payload);
    let error = upload(
        &mut bob_client,
        &store,
        &bob_opening.file,
        &artifact_path,
        &artifact,
        &one,
    )
    .await
    .unwrap_err();
    assert_eq!(api_code(&error), Some(ErrorCode::CommitmentMismatch));
    // Nothing went to R2 for a refused session request.
    assert_eq!(mock.puts(), 0);

    // The newer opening uploads and verifies.
    let done = upload(
        &mut bob_client,
        &store,
        &newer.file,
        &artifact_path,
        &artifact,
        &quick(),
    )
    .await
    .unwrap();
    let UploadOutcome::Finished(bob_session) = done else {
        panic!()
    };
    assert_eq!(bob_session.state(), Some(SessionState::Verified));

    // A second artifact for the same commitment: same content, another object.
    let (other_path, other) = packed(dir.path(), 0x21, "{\"v\":22}");
    assert_eq!(other.content, artifact.content);
    assert_ne!(other.sha256, artifact.sha256);
    let mut second_file = newer.file.clone();
    second_file.header_sha256 = Some(other.sha256.hex());
    second_file.header_len = Some(other.len);
    let error = upload(
        &mut bob_client,
        &store,
        &second_file,
        &other_path,
        &other,
        &one,
    )
    .await
    .unwrap_err();
    assert_eq!(api_code(&error), Some(ErrorCode::SecondArtifact));

    // The refusal names the live session, and status sees it.
    let live = error
        .downcast_ref::<ApiError>()
        .and_then(|e| e.body())
        .and_then(|b| b.error.detail.session.clone());
    assert_eq!(live.as_deref(), Some(bob_session.session.as_str()));
    let refused = bob_client
        .hotkey_view(7)
        .await
        .unwrap()
        .last_refusal
        .unwrap();
    assert_eq!(refused.code(), Some(ErrorCode::SecondArtifact));
    assert_eq!(
        refused.detail.session.as_deref(),
        Some(bob_session.session.as_str())
    );

    // Another object needs another commitment: committing again supersedes
    // the earlier session, and the new one is ranked by its own position.
    let bob_other =
        commit::prepare(&store, &chain601(), 7, &bob_id, &other, &mut OsNonce, true).unwrap();
    mock.commit(bob_id.hotkey, 2_015, 0, &bob_other.payload);
    let UploadOutcome::Finished(bob_session2) = upload(
        &mut bob_client,
        &store,
        &bob_other.file,
        &other_path,
        &other,
        &quick(),
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert_eq!(bob_session2.state(), Some(SessionState::Verified));
    assert_eq!(
        mock.session_state(&bob_session.session).as_deref(),
        Some("superseded")
    );
    let bob_session = bob_session2;

    // Wrong cohort: an opening for a later cohort cannot be uploaded now.
    let carol = dev_signer("//Charlie");
    let carol_id = identity(&carol, 14);
    mock.register(carol_id);
    let later = commit::prepare(
        &store,
        &chain601(),
        8,
        &carol_id,
        &artifact,
        &mut OsNonce,
        false,
    )
    .unwrap();
    mock.commit(carol_id.hotkey, 2_020, 0, &later.payload);
    let mut carol_client = client(&mock, carol.clone()).await;
    let error = upload(
        &mut carol_client,
        &store,
        &later.file,
        &artifact_path,
        &artifact,
        &one,
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("cannot carry over"),
        "{error:#}"
    );
    let upload_8 = upload_envelope(later.opening.clone(), &artifact).unwrap();
    let error = carol_client.create_session(&upload_8).await.unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::WrongCohort));

    // A signature by another key than the named hotkey.
    struct Impostor(AccountId, Arc<RtaoSigner>);
    impl RequestSigner for Impostor {
        fn hotkey(&self) -> AccountId {
            self.0
        }
        fn sign<'a>(&'a self, message: &'a [u8; 32]) -> BoxFuture<'a, anyhow::Result<[u8; 64]>> {
            self.1.sign(message)
        }
    }
    let impostor = Arc::new(Impostor(bob_id.hotkey, carol.clone()));
    let http = http_client(Duration::from_secs(30)).unwrap();
    let base = slop_ninja_miner::client::base_url(&mock.url()).unwrap();
    let (mut forged, _) = AdmissionClient::connect(http, base, impostor, None)
        .await
        .unwrap();
    let error = forged.hotkey_view(7).await.unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::BadSignature));

    // An object over the cohort's bound: refused by the client before it
    // signs anything.
    mock.with(|s| s.config.max_object_len = artifact.object_len - 1);
    let carol_7 = commit::prepare(
        &store,
        &chain601(),
        7,
        &carol_id,
        &artifact,
        &mut OsNonce,
        false,
    )
    .unwrap();
    mock.commit(carol_id.hotkey, 2_030, 2, &carol_7.payload);
    let creates = mock.with(|s| s.counters.routes.get("create").copied());
    let error = upload(
        &mut carol_client,
        &store,
        &carol_7.file,
        &artifact_path,
        &artifact,
        &one,
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("allows"), "{error:#}");
    assert_eq!(
        mock.with(|s| s.counters.routes.get("create").copied()),
        creates
    );
    mock.with(|s| s.config.max_object_len = 1 << 20);
    let done = upload(
        &mut carol_client,
        &store,
        &carol_7.file,
        &artifact_path,
        &artifact,
        &quick(),
    )
    .await
    .unwrap();
    let UploadOutcome::Finished(carol_session) = done else {
        panic!()
    };
    assert_eq!(carol_session.state(), Some(SessionState::Verified));

    // Dave verifies, then overwrites his commitment before the cutoff.
    let dave = dev_signer("//Dave");
    let dave_id = identity(&dave, 15);
    mock.register(dave_id);
    let dave_7 = commit::prepare(
        &store,
        &chain601(),
        7,
        &dave_id,
        &artifact,
        &mut OsNonce,
        false,
    )
    .unwrap();
    mock.commit(dave_id.hotkey, 2_040, 0, &dave_7.payload);
    let mut dave_client = client(&mock, dave.clone()).await;
    let UploadOutcome::Finished(dave_session) = upload(
        &mut dave_client,
        &store,
        &dave_7.file,
        &artifact_path,
        &artifact,
        &quick(),
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert_eq!(dave_session.state(), Some(SessionState::Verified));
    let dave_again = commit::prepare(
        &store,
        &chain601(),
        7,
        &dave_id,
        &artifact,
        &mut OsNonce,
        true,
    )
    .unwrap();
    mock.commit(dave_id.hotkey, 2_050, 0, &dave_again.payload);

    // Eve starts an upload but does not finish it before the cutoff.
    let eve = dev_signer("//Eve");
    let eve_id = identity(&eve, 16);
    mock.register(eve_id);
    let eve_7 = commit::prepare(
        &store,
        &chain601(),
        7,
        &eve_id,
        &artifact,
        &mut OsNonce,
        false,
    )
    .unwrap();
    mock.commit(eve_id.hotkey, 2_060, 0, &eve_7.payload);
    let mut eve_client = client(&mock, eve.clone()).await;
    let partial = upload(
        &mut eve_client,
        &store,
        &eve_7.file,
        &artifact_path,
        &artifact,
        &UploadOptions {
            stop_after_objects: Some(1),
            ..quick()
        },
    )
    .await
    .unwrap();
    assert!(matches!(partial, UploadOutcome::Interrupted { .. }));

    // The cutoff, with a roster cap of one: Bob (block 2015) is admitted
    // before Carol (block 2030), who overflows.
    mock.with(|s| s.config.roster_cap = 1);
    mock.close();
    let bob_final = bob_client.session(&bob_session.session).await.unwrap();
    assert_eq!(bob_final.state(), Some(SessionState::Admitted));
    assert_eq!(bob_final.position.map(|p| p.block), Some(2_015));
    let carol_final = carol_client.session(&carol_session.session).await.unwrap();
    assert_eq!(carol_final.state(), Some(SessionState::Overflow));
    assert_eq!(carol_final.next_cohort, Some(8));
    let text = describe(&carol_final);
    assert!(
        text.contains("cannot carry over") && text.contains("cohort 8"),
        "{text}"
    );
    let dave_final = dave_client.session(&dave_session.session).await.unwrap();
    assert_eq!(
        dave_final.refusal.unwrap().code(),
        Some(ErrorCode::CommitmentOverwritten)
    );
    let eve_view = eve_client.hotkey_view(7).await.unwrap();
    let eve_refusal = eve_view.sessions[0].refusal.clone().unwrap();
    assert_eq!(eve_refusal.code(), Some(ErrorCode::IncompleteAtCutoff));

    // After the cutoff nothing more is accepted for cohort 7.
    let error = upload(
        &mut eve_client,
        &store,
        &eve_7.file,
        &artifact_path,
        &artifact,
        &one,
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("no cohort is admitting"),
        "{error:#}"
    );
    let envelope = upload_envelope(eve_7.opening.clone(), &artifact).unwrap();
    let error = eve_client.create_session(&envelope).await.unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::AfterCutoff));
    let eve_session = eve_view.sessions[0].session.clone();
    let error = eve_client.urls(&eve_session).await.unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::AfterCutoff));

    // The overflow entrant must commit again for cohort 8; its cohort-7
    // opening is refused there.
    mock.open_next(2_400);
    let error = upload(
        &mut carol_client,
        &store,
        &carol_7.file,
        &artifact_path,
        &artifact,
        &one,
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("cannot carry over"),
        "{error:#}"
    );
    let carol_8 = commit::prepare(
        &store,
        &chain601(),
        8,
        &carol_id,
        &artifact,
        &mut OsNonce,
        true,
    )
    .unwrap();
    assert_ne!(carol_8.opening.nonce, carol_7.opening.nonce);
    mock.commit(carol_id.hotkey, 2_410, 0, &carol_8.payload);
    let UploadOutcome::Finished(carol_next) = upload(
        &mut carol_client,
        &store,
        &carol_8.file,
        &artifact_path,
        &artifact,
        &quick(),
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert_eq!(carol_next.cohort, 8);
    assert_eq!(carol_next.state(), Some(SessionState::Verified));
    assert_eq!(carol_next.position.map(|p| p.block), Some(2_410));
}

#[test]
fn openings_stay_private_and_are_never_reused() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let (_, artifact) = packed(dir.path(), 0x31, "{}");
    let alice = dev_signer("//Alice");
    let id = identity(&alice, 12);
    let first =
        commit::prepare(&store, &chain601(), 7, &id, &artifact, &mut OsNonce, false).unwrap();
    let refused =
        commit::prepare(&store, &chain601(), 7, &id, &artifact, &mut OsNonce, false).unwrap_err();
    assert!(refused.to_string().contains("--replace"), "{refused}");
    assert!(
        refused.to_string().contains("never confirmed on chain"),
        "{refused}"
    );
    // Once the opening is confirmed on chain, the refusal says so.
    let mut confirmed = first.file.clone();
    confirmed.submitted = Some(Default::default());
    store.update_opening(&first.path, &confirmed).unwrap();
    let refused =
        commit::prepare(&store, &chain601(), 7, &id, &artifact, &mut OsNonce, false).unwrap_err();
    assert!(
        refused.to_string().contains("already committed"),
        "{refused}"
    );
    // A dry run's opening is never stored, so it can never be chosen later.
    let draft = commit::make(&store, &chain601(), 7, &id, &artifact, &mut OsNonce, true).unwrap();
    assert_ne!(draft.opening.nonce, first.opening.nonce);
    assert_eq!(
        store.openings_for(601, 7, &ss58(&id.hotkey)).unwrap().len(),
        1
    );
    let second =
        commit::prepare(&store, &chain601(), 7, &id, &artifact, &mut OsNonce, true).unwrap();
    assert_ne!(first.opening.nonce, second.opening.nonce);
    assert_ne!(first.path, second.path);
    assert_eq!(
        store.openings_for(601, 7, &ss58(&id.hotkey)).unwrap().len(),
        2
    );

    // A broken random source never yields an opening.
    struct Zeros;
    impl NonceSource for Zeros {
        fn fill(&mut self, out: &mut [u8; 32]) -> anyhow::Result<()> {
            *out = [0; 32];
            Ok(())
        }
    }
    assert!(commit::prepare(&store, &chain601(), 9, &id, &artifact, &mut Zeros, false).is_err());

    // A readable opening file is refused until it is private again.
    std::fs::set_permissions(&first.path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let error = load_opening(&first.path).unwrap_err();
    assert!(error.to_string().contains("chmod 600"), "{error}");
    std::fs::set_permissions(&first.path, std::fs::Permissions::from_mode(0o600)).unwrap();
    load_opening(&first.path).unwrap();

    // A tampered opening file is refused.
    let mut text: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&first.path).unwrap()).unwrap();
    text["cohort"] = serde_json::json!(8);
    std::fs::write(&first.path, serde_json::to_vec(&text).unwrap()).unwrap();
    assert!(load_opening(&first.path).is_err());
}

/// An opening file of schema v1 (which named its object by a whole-object
/// SHA-256) still loads, matches its object by the committed content, and
/// is migrated in place to v2 once the object's header is known.
#[test]
fn a_v1_opening_matches_by_content_and_migrates_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let (_, artifact) = packed(dir.path(), 0x32, "{}");
    let (_, other) = packed(dir.path(), 0x33, "{}");
    let id = identity(&dev_signer("//Alice"), 12);
    let prepared =
        commit::prepare(&store, &chain601(), 7, &id, &artifact, &mut OsNonce, false).unwrap();
    // Rewrite the stored file as v1 had it.
    let mut v1: serde_json::Value = serde_json::to_value(&prepared.file).unwrap();
    let map = v1.as_object_mut().unwrap();
    map.insert("schema".into(), OPENING_SCHEMA_V1.into());
    for field in ["header_sha256", "header_len", "object_len"] {
        map.remove(field);
    }
    map.insert("artifact_sha256".into(), "ab".repeat(32).into());
    map.insert("artifact_len".into(), 12_345.into());
    std::fs::write(&prepared.path, serde_json::to_vec_pretty(&v1).unwrap()).unwrap();
    let loaded = load_opening(&prepared.path).unwrap();
    assert_eq!(loaded.schema, OPENING_SCHEMA_V1);
    assert!(loaded.header_sha256.is_none());
    assert!(loaded.fits(&artifact), "matched by the committed content");
    assert!(!loaded.fits(&other));
    assert!(
        loaded.migrated(&other).is_none(),
        "never migrated to another object"
    );
    let migrated = loaded.migrated(&artifact).unwrap();
    store.update_opening(&prepared.path, &migrated).unwrap();
    let reloaded = load_opening(&prepared.path).unwrap();
    assert_eq!(reloaded.schema, OPENING_SCHEMA);
    assert_eq!(reloaded, prepared.file);
    assert!(reloaded.migrated(&artifact).is_none());
}

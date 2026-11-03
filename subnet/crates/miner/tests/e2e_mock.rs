//! End to end against a local mock of the admission contract: pack, check,
//! commit (the opening and the composed call; nothing is signed for or sent
//! to a chain), an interrupted and resumed upload, verification, the cutoff,
//! and every admission refusal the client must show.

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use rtao_primitives::CryptoType;
use rtao_runtime::RuntimeHandle;
use rtao_signer::KeypairSigner;
use slop_ninja_admission_wire::{ErrorCode, SessionState};
use slop_ninja_miner::check::{CheckContext, Outcome, SpecCheck, StructuralCheck, run_all};
use slop_ninja_miner::client::{
    AdmissionClient, ApiError, RequestSigner, RtaoSigner, http_client, ss58,
};
use slop_ninja_miner::opening::{NonceSource, OsNonce, Store, load_opening, upload_envelope};
use slop_ninja_miner::upload::{UploadOptions, UploadOutcome, describe, upload};
use slop_ninja_miner::{chain, commit, object};
use slop_ninja_protocol::{
    AccountId, ChainBinding, FileRole, NeuronIdentity, TESTNET_GENESIS, VerifiedArtifact,
};
use support::mock::{Mock, MockConfig};

const CONTRACT: &str = slop_ninja_artifact_check::spec::CONTRACT_ID;
const METADATA: &[u8] = include_bytes!("../../../fixtures/runtime-486/testnet-486.v15.scale");

fn chain601() -> ChainBinding {
    ChainBinding {
        genesis: TESTNET_GENESIS,
        netuid: 601,
        mechanism: 0,
    }
}

fn config() -> MockConfig {
    MockConfig {
        chain: chain601(),
        cohort: 7,
        opened_epoch: 23_169,
        open_block: 1_900,
        admission_min: 1,
        output_contract: CONTRACT.into(),
        roster_cap: 40,
        max_object_len: 1 << 20,
        part_size: 1_024,
        max_parts: 10_000,
        request_validity_blocks: 360,
    }
}

fn dev_signer(uri: &str) -> Arc<RtaoSigner> {
    let signer = KeypairSigner::from_uri(uri, CryptoType::Sr25519).expect("dev key");
    Arc::new(RtaoSigner(Arc::new(signer)))
}

fn identity(signer: &RtaoSigner, uid: u16) -> NeuronIdentity {
    NeuronIdentity {
        uid,
        hotkey: signer.hotkey(),
        registration_block: 1_000 + u64::from(uid),
    }
}

/// Synthetic model files (no real model), with owner metadata that changes
/// the object but not its inference content.
fn model_files(dir: &Path, family: u8, owner: &str) -> Vec<object::PackInput> {
    std::fs::create_dir_all(dir.join("tok")).unwrap();
    let graph: Vec<u8> = (0..3_000u32).map(|i| (i as u8) ^ family).collect();
    std::fs::write(dir.join("model.onnx"), &graph).unwrap();
    std::fs::write(
        dir.join("tok/tokenizer.json"),
        format!("{{\"family\":{family}}}"),
    )
    .unwrap();
    std::fs::write(dir.join("owner.json"), owner).unwrap();
    [
        "graph:model.onnx",
        "tokenizer:tok/tokenizer.json",
        "owner-metadata:owner.json",
    ]
    .iter()
    .map(|spec| object::PackInput::parse(spec, dir).unwrap())
    .collect()
}

fn packed(dir: &Path, family: u8, owner: &str) -> (PathBuf, VerifiedArtifact) {
    let inputs = model_files(
        &dir.join(format!("src-{family}-{}", owner.len())),
        family,
        owner,
    );
    let out = dir.join(format!("artifact-{family}-{}.sna", owner.len()));
    let verified = object::pack(CONTRACT, &inputs, &out).unwrap();
    (out, verified)
}

async fn client(mock: &Mock, signer: Arc<RtaoSigner>) -> AdmissionClient {
    let http = http_client(Duration::from_secs(30)).unwrap();
    let base = slop_ninja_miner::client::base_url(&mock.url()).unwrap();
    AdmissionClient::connect(http, base, signer, Some(&chain601()))
        .await
        .unwrap()
        .0
}

fn quick() -> UploadOptions {
    UploadOptions {
        attempts: 3,
        backoff: Duration::from_millis(10),
        wait: Duration::from_secs(5),
        poll: Duration::from_millis(10),
        stop_after_parts: None,
        restart: false,
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn api_code(error: &anyhow::Error) -> Option<ErrorCode> {
    error.downcast_ref::<ApiError>().and_then(ApiError::code)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_check_commit_upload_resume_and_admit() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    assert_eq!(mode(store.root()), 0o700);
    assert_eq!(mode(&store.root().join("openings")), 0o700);

    // pack: the streamed object is byte-identical to the protocol's builder.
    let (artifact_path, artifact) = packed(dir.path(), 0x11, "{\"publisher\":\"alice\"}");
    let src = dir
        .path()
        .join(format!("src-17-{}", "{\"publisher\":\"alice\"}".len()));
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
    assert_eq!(object::verify_file(&artifact_path).unwrap(), artifact);

    // check: the cohort's contract and bound pass; the artifact spec's
    // admission checks refuse this synthetic object (it is not the spec's
    // six-file table), with the code the coordinator would report.
    let cx = CheckContext {
        object: &artifact_path,
        verified: &artifact,
        expected_contract: Some(CONTRACT),
        max_object_len: Some(1 << 20),
        artifact_spec: None,
        cohort_unknown: None,
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

    // commit: a fresh opening stored 0600, and the call composed through rtao.
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
    let runtime = RuntimeHandle::parse(METADATA, 486, 1, 42).unwrap();
    let call = chain::compose(&chain::commitment_call(601, &prepared.payload), &runtime).unwrap();
    assert_eq!(&call.as_bytes()[..6], &[0x12, 0x00, 0x59, 0x02, 0x04, 0x2d]);
    assert_eq!(&call.as_bytes()[6..], prepared.payload.as_slice());

    // The commitment finalizes on the mock chain.
    let mock = Mock::start(config(), 1_950).await;
    mock.register(alice_id);
    mock.commit(alice_id.hotkey, 2_000, 3, &prepared.payload);

    // An upload interrupted after two parts...
    let mut first = client(&mock, alice.clone()).await;
    let interrupted = upload(
        &mut first,
        &store,
        &prepared.file,
        &artifact_path,
        &artifact,
        &UploadOptions {
            stop_after_parts: Some(2),
            ..quick()
        },
    )
    .await
    .unwrap();
    let parts = artifact.len.div_ceil(1_024) as u32;
    assert_eq!(
        interrupted,
        UploadOutcome::Interrupted {
            uploaded: 2,
            remaining: parts - 2
        }
    );
    // ...resumes in a fresh client, through one transient failure.
    mock.with(|s| s.fail_part_once = vec![3]);
    let mut second = client(&mock, alice.clone()).await;
    let outcome = upload(
        &mut second,
        &store,
        &prepared.file,
        &artifact_path,
        &artifact,
        &quick(),
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
    let puts = mock.with(|s| s.counters.part_puts.clone());
    assert_eq!(puts[&1], 1, "a recorded part is not sent again");
    assert_eq!(puts[&2], 1);
    assert_eq!(puts[&3], 2, "the failed part is retried");
    assert!((4..=parts).all(|n| puts[&n] == 1));
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
    assert_eq!(
        mock.with(|s| s.counters.part_puts.values().sum::<u32>()),
        parts + 1
    );

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
    second_file.artifact_sha256 = other.sha256.hex();
    second_file.artifact_len = other.len;
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

    // Aborting frees the hotkey; reopening an aborted envelope restarts it
    // empty as the next attempt.
    let aborted = bob_client
        .abort(&bob_session.session, bob_session.attempt)
        .await
        .unwrap();
    assert_eq!(aborted.state(), Some(SessionState::Aborted));
    let UploadOutcome::Finished(bob_session) = upload(
        &mut bob_client,
        &store,
        &newer.file,
        &artifact_path,
        &artifact,
        &quick(),
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert_eq!(bob_session.state(), Some(SessionState::Verified));
    assert_eq!(bob_session.attempt, 2);

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

    // An object over the cohort's bound, refused by the coordinator.
    mock.with(|s| s.config.max_object_len = artifact.len - 1);
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
    let envelope = upload_envelope(carol_7.opening.clone(), &artifact).unwrap();
    let error = carol_client.create_session(&envelope).await.unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::ObjectTooLarge));
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
            stop_after_parts: Some(1),
            ..quick()
        },
    )
    .await
    .unwrap();
    assert!(matches!(partial, UploadOutcome::Interrupted { .. }));

    // The cutoff, with a roster cap of one: Bob (block 2010) is admitted
    // before Carol (block 2030), who overflows.
    mock.with(|s| s.config.roster_cap = 1);
    mock.close();
    let bob_final = bob_client.session(&bob_session.session).await.unwrap();
    assert_eq!(bob_final.state(), Some(SessionState::Admitted));
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

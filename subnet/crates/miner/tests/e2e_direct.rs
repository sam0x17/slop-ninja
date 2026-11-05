//! The direct upload against the mock coordinator and its presigned-PUT R2
//! stub (`tests/support/mock.rs`): the client's URL checks before any byte,
//! the per-object retry rules (fresh URL after a transport error, a 5xx, 429
//! or expiry; 412 means present; `BadDigest`, `SignatureDoesNotMatch` and
//! 501 `NotImplemented` stop), resume per object, the URL policy's life, and repair of recorded
//! objects that went missing. Nothing is sent to Cloudflare or a chain.

mod support;

use std::sync::Arc;

use slop_ninja_admission_wire::{ErrorCode, SessionState};
use slop_ninja_miner::direct::{PutError, PutPolicy, PutTarget, put_object};
use slop_ninja_miner::opening::{Store, upload_envelope};
use slop_ninja_miner::upload::{UploadOptions, UploadOutcome, describe, repair, upload};
use slop_ninja_r2_presign::layout::{ObjectId, ObjectKind};
use slop_ninja_r2_presign::policy;
use support::common::*;
use support::mock::{BadUrl, Fault, Match, Mock, S3_HOST};

fn finished(outcome: UploadOutcome) -> slop_ninja_admission_wire::json::Session {
    match outcome {
        UploadOutcome::Finished(session) => *session,
        other => panic!("not finished: {other:?}"),
    }
}

fn refused_codes(mock: &Mock) -> Vec<String> {
    mock.with(|s| s.r2.refused.iter().map(|r| r.2.clone()).collect())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presigned_urls_are_checked_before_any_byte() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (path, art) = packed(dir.path(), 0x13, "{}");
    let a = entrant(&mock, &store, "//Alice", 12, &art, 2_000);
    let mut c = client(&mock, a.signer.clone()).await;
    for (bad, says) in [
        (
            BadUrl::Host("evil.example.com".into()),
            "refusing the presigned URL",
        ),
        (
            BadUrl::Host("otheracct.r2.cloudflarestorage.com.evil.example".into()),
            "refusing the presigned URL",
        ),
        (BadUrl::Key, "refusing the presigned URL"),
        (BadUrl::ExtraQuery, "refusing the presigned URL"),
        (BadUrl::Checksum, "names other headers"),
    ] {
        mock.with(|s| s.bad_urls = Some(bad.clone()));
        let error = upload(&mut c, &store, &a.prepared.file, &path, &art, &quick())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains(says), "{bad:?}: {error:#}");
        assert_eq!(mock.puts(), 0, "{bad:?}: no byte was sent");
    }
    mock.with(|s| s.bad_urls = None);

    // A trust file's pinned upload host: a URL for any other R2 host is
    // refused; the pinned one passes.
    let pinned_other = PutPolicy {
        pinned_host: Some("otheracct.r2.cloudflarestorage.com".into()),
        ..stub_policy(&mock)
    };
    let mut c = client(&mock, a.signer.clone())
        .await
        .with_put_policy(pinned_other);
    let error = upload(&mut c, &store, &a.prepared.file, &path, &art, &quick())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("refusing the presigned URL"),
        "{error:#}"
    );
    assert_eq!(mock.puts(), 0);
    let pinned = PutPolicy {
        pinned_host: Some(S3_HOST.into()),
        ..stub_policy(&mock)
    };
    let mut c = client(&mock, a.signer.clone())
        .await
        .with_put_policy(pinned);
    let session = finished(
        upload(&mut c, &store, &a.prepared.file, &path, &art, &quick())
            .await
            .unwrap(),
    );
    assert_eq!(session.state(), Some(SessionState::Verified));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn urls_live_by_the_policy_and_expiry_gets_a_fresh_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (path, art) = packed(dir.path(), 0x14, "{}");
    let a = entrant(&mock, &store, "//Alice", 12, &art, 2_000);
    let mut c = client(&mock, a.signer.clone()).await;

    // The header URL lives ceil(L / B_min) + S for the committed size known
    // so far (the header alone).
    let envelope = upload_envelope(a.prepared.opening.clone(), &art).unwrap();
    let (session, created) = c.create_session(&envelope).await.unwrap();
    assert!(created);
    let header_url = session.uploads[0].clone();
    assert_eq!(
        header_url.valid_secs,
        policy::valid_secs(u64::from(art.len)).unwrap()
    );
    assert_eq!(header_url.kind, "header");

    // Past its life, R2 answers ExpiredRequest and stores nothing.
    let target = PutTarget {
        id: ObjectId {
            cohort: 7,
            commitment: envelope.commitment.0,
            kind: ObjectKind::Header,
        },
        sha256: art.sha256.0,
        len: u64::from(art.len),
        path: path.clone(),
        offset: 4,
        name: "header".into(),
    };
    mock.with(|s| s.now += u64::from(header_url.valid_secs) + 1);
    let http = slop_ninja_miner::client::http_client(std::time::Duration::from_secs(10)).unwrap();
    let outcome = put_object(
        &http,
        &stub_policy(&mock),
        &header_url,
        &target,
        std::time::Duration::from_secs(10),
        Arc::new(slop_ninja_miner::direct::Silent),
    )
    .await;
    assert!(
        matches!(outcome, Err(PutError::Expired { .. })),
        "{outcome:?}"
    );
    assert!(mock.with(|s| s.r2.objects.is_empty()));

    // A transport failure never prints the presigned URL (review round 2:
    // its query is a bearer capability, and error logs get pasted).
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let unreachable = PutPolicy::default()
        .with_endpoint_override(&format!("http://127.0.0.1:{port}/"))
        .unwrap();
    let outcome = put_object(
        &http,
        &unreachable,
        &header_url,
        &target,
        std::time::Duration::from_secs(10),
        Arc::new(slop_ninja_miner::direct::Silent),
    )
    .await;
    let Err(PutError::Retry(why)) = outcome else {
        panic!("{outcome:?}")
    };
    assert!(why.contains("header") && why.contains(S3_HOST), "{why}");
    for leak in ["X-Amz", "Signature", "Credential", "?", "/admission/"] {
        assert!(!why.contains(leak), "{leak} in {why}");
    }

    // The upload asks again (the session answers a fresh header URL) and
    // goes on; an expired file URL also gets a fresh one at once (R2 checks
    // expiry when a PUT starts, so nothing about the rate is declared).
    mock.with(|s| s.r2.faults = vec![(Match::File, Fault::Expire)]);
    let minted = mock.with(|s| s.counters.minted);
    let session = finished(
        upload(&mut c, &store, &a.prepared.file, &path, &art, &quick())
            .await
            .unwrap(),
    );
    assert_eq!(session.state(), Some(SessionState::Verified));
    assert!(mock.with(|s| s.counters.minted) > minted + 3);
    // File URLs were minted for the whole artifact's committed size.
    let codes = refused_codes(&mock);
    assert_eq!(codes, ["ExpiredRequest", "ExpiredRequest"], "{codes:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_is_per_object_and_412_means_present() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (path, art) = packed(dir.path(), 0x15, "{}");
    let a = entrant(&mock, &store, "//Alice", 12, &art, 2_000);
    let mut c = client(&mock, a.signer.clone()).await;
    let interrupted = upload(
        &mut c,
        &store,
        &a.prepared.file,
        &path,
        &art,
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

    // From another machine (no local record): the coordinator has not
    // recorded the file R2 holds, so it is sent again, R2 answers 412, and
    // completion confirms it by its head.
    let elsewhere = Store::open(&dir.path().join("elsewhere")).unwrap();
    let session = finished(
        upload(&mut c, &elsewhere, &a.prepared.file, &path, &art, &quick())
            .await
            .unwrap(),
    );
    assert_eq!(
        session.state(),
        Some(SessionState::Verified),
        "{}",
        describe(&session)
    );
    assert_eq!(refused_codes(&mock), ["PreconditionFailed"]);
    assert_eq!(mock.puts(), 5);
    assert!(session.objects.iter().all(|o| o.present));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_object_stops_with_bad_digest_and_nothing_is_stored() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (path, art) = packed(dir.path(), 0x16, "{}");
    let a = entrant(&mock, &store, "//Alice", 12, &art, 2_000);
    let mut c = client(&mock, a.signer.clone()).await;

    // One byte of a file changes on disk after pack (same length): R2's
    // checksum refuses it, and the client says to re-pack.
    let graph = art
        .header
        .files
        .iter()
        .find(|f| f.name == "model.onnx")
        .unwrap();
    let (offset, _) = art.file_range(&graph.sha256).unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut changed = original.clone();
    changed[offset as usize + 10] ^= 0x55;
    std::fs::write(&path, &changed).unwrap();
    let error = upload(&mut c, &store, &a.prepared.file, &path, &art, &quick())
        .await
        .unwrap_err();
    let text = format!("{error:#}");
    assert!(
        text.contains("BadDigest") && text.contains("re-pack"),
        "{text}"
    );
    assert_eq!(
        refused_codes(&mock),
        ["BadDigest"],
        "no retry of a changed object"
    );
    let graph_key = ObjectId {
        cohort: 7,
        commitment: a.prepared.opening.commitment().0,
        kind: ObjectKind::File(graph.sha256.0),
    }
    .key();
    assert!(
        !mock.with(|s| s.r2.objects.contains_key(graph_key.as_str())),
        "nothing stored"
    );

    // The same bytes as packed: the rerun goes through.
    std::fs::write(&path, &original).unwrap();
    let session = finished(
        upload(&mut c, &store, &a.prepared.file, &path, &art, &quick())
            .await
            .unwrap(),
    );
    assert_eq!(session.state(), Some(SessionState::Verified));

    // A length change is caught before any request.
    let b = entrant(&mock, &store, "//Bob", 13, &art, 2_010);
    let mut cb = client(&mock, b.signer.clone()).await;
    let mut longer = original.clone();
    longer.push(0);
    std::fs::write(&path, &longer).unwrap();
    let before = mock.with(|s| s.counters.requests);
    let error = upload(&mut cb, &store, &b.prepared.file, &path, &art, &quick())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("changed since pack"),
        "{error:#}"
    );
    assert_eq!(mock.with(|s| s.counters.requests), before);
    std::fs::write(&path, &original).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_failures_retry_and_defects_stop() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (path, art) = packed(dir.path(), 0x17, "{}");

    // A dropped connection on the header and a 429 on a file: each gets a
    // fresh URL and goes again.
    let a = entrant(&mock, &store, "//Alice", 12, &art, 2_000);
    let mut ca = client(&mock, a.signer.clone()).await;
    mock.with(|s| {
        s.r2.faults = vec![
            (Match::Header, Fault::Drop),
            (Match::File, Fault::Status(429, "SlowDown")),
        ];
    });
    let session = finished(
        upload(&mut ca, &store, &a.prepared.file, &path, &art, &quick())
            .await
            .unwrap(),
    );
    assert_eq!(session.state(), Some(SessionState::Verified));
    assert_eq!(mock.puts(), 6);

    // Retries run out: the run stops and says to run again; the rerun
    // resumes and finishes.
    let b = entrant(&mock, &store, "//Bob", 13, &art, 2_010);
    let mut cb = client(&mock, b.signer.clone()).await;
    mock.with(|s| {
        s.r2.faults = vec![
            (Match::File, Fault::Status(500, "InternalError")),
            (Match::File, Fault::Status(503, "ServiceUnavailable")),
        ];
    });
    let two = UploadOptions {
        attempts: 2,
        ..quick()
    };
    let error = upload(&mut cb, &store, &b.prepared.file, &path, &art, &two)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("gave up after 2 attempts"),
        "{error:#}"
    );
    let session = finished(
        upload(&mut cb, &store, &b.prepared.file, &path, &art, &quick())
            .await
            .unwrap(),
    );
    assert_eq!(session.state(), Some(SessionState::Verified));

    // A refused signature is a client defect: no retry.
    let c = entrant(&mock, &store, "//Charlie", 14, &art, 2_020);
    let mut cc = client(&mock, c.signer.clone()).await;
    mock.with(|s| s.r2.faults = vec![(Match::Header, Fault::Status(403, "SignatureDoesNotMatch"))]);
    let before = mock.puts();
    let error = upload(&mut cc, &store, &c.prepared.file, &path, &art, &quick())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("client defect"), "{error:#}");
    assert_eq!(mock.puts(), before + 1);

    // 501 `NotImplemented` (R2's answer to a multipart or version parameter,
    // qualification G11) is a refusal like 403: no retry, nothing stored.
    let d = entrant(&mock, &store, "//Dave", 15, &art, 2_030);
    let mut cd = client(&mock, d.signer.clone()).await;
    mock.with(|s| s.r2.faults = vec![(Match::Header, Fault::Status(501, "NotImplemented"))]);
    let before = mock.puts();
    let error = upload(&mut cd, &store, &d.prepared.file, &path, &art, &quick())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("not implemented"), "{error:#}");
    assert_eq!(mock.puts(), before + 1);

    // A redirect from the coordinator is never followed.
    mock.with(|s| s.redirect_urls_to = Some("http://127.0.0.1:9/stolen".into()));
    let error = upload(&mut cc, &store, &c.prepared.file, &path, &art, &quick())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("redirect"), "{error:#}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn objects_missing_at_completion_are_sent_again() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (path, art) = packed(dir.path(), 0x18, "{}");
    let a = entrant(&mock, &store, "//Alice", 12, &art, 2_000);
    let mut c = client(&mock, a.signer.clone()).await;
    mock.with(|s| s.drop_before_complete = vec![Match::File]);
    let session = finished(
        upload(&mut c, &store, &a.prepared.file, &path, &art, &quick())
            .await
            .unwrap(),
    );
    assert_eq!(session.state(), Some(SessionState::Verified));
    let puts = mock.with(|s| s.r2.puts.clone());
    assert_eq!(puts.values().sum::<u32>(), 5, "{puts:?}");
    assert_eq!(
        puts.values().filter(|n| **n == 2).count(),
        1,
        "the lost file went again: {puts:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repair_restores_recorded_objects_that_went_missing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let (path, art) = packed(dir.path(), 0x19, "{}");
    let (other_path, other) = packed(dir.path(), 0x1a, "{}");
    let a = entrant(&mock, &store, "//Alice", 12, &art, 2_000);
    let mut c = client(&mock, a.signer.clone()).await;
    let session = finished(
        upload(&mut c, &store, &a.prepared.file, &path, &art, &quick())
            .await
            .unwrap(),
    );
    assert_eq!(session.state(), Some(SessionState::Verified));
    mock.close();

    // A platform loss after admission: the session lists what to repair,
    // also after the close.
    let lost = mock.lose_objects(&session.session, |key| key.contains("/f/"));
    assert_eq!(lost, 3);
    let view = c.hotkey_view(7).await.unwrap();
    assert_eq!(view.sessions[0].repair_needed.len(), 3);
    let read = c.session(&session.session).await.unwrap();
    assert_eq!(read.state(), Some(SessionState::Admitted));
    assert!(
        describe(&read).contains("slop_ninja_miner repair"),
        "{}",
        describe(&read)
    );

    // Only the session's own object may repair it.
    let error = repair(&mut c, &session.session, &other_path, &other, &quick())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("is not the artifact of session"),
        "{error:#}"
    );

    let before = mock.puts();
    let repaired = repair(&mut c, &session.session, &path, &art, &quick())
        .await
        .unwrap();
    assert!(repaired.repair_needed.is_empty(), "{repaired:?}");
    assert_eq!(repaired.state(), Some(SessionState::Admitted));
    assert_eq!(mock.puts(), before + 3);
    // Nothing to repair: no request to R2.
    let again = repair(&mut c, &session.session, &path, &art, &quick())
        .await
        .unwrap();
    assert!(again.repair_needed.is_empty());
    assert_eq!(mock.puts(), before + 3);
    let error = c.repair(&session.session).await.unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::NotNeeded));

    // Once publication is recorded the admission copies are no longer
    // needed: a loss lists nothing and repair answers not_needed.
    mock.publish(&session.session);
    mock.lose_objects(&session.session, |_| true);
    let done = repair(&mut c, &session.session, &path, &art, &quick())
        .await
        .unwrap();
    assert!(done.repair_needed.is_empty());
    assert_eq!(mock.puts(), before + 3);
    assert_eq!(
        c.repair(&session.session).await.unwrap_err().code(),
        Some(ErrorCode::NotNeeded)
    );
}

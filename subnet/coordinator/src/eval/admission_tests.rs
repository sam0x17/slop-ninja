//! Admission tests: spec bounds, request authentication, the session state
//! machine and every request refusal, framing, verdict rules (a)-(d), the
//! cutoff and the freeze (ordering, cap, overflow, overwritten commitments,
//! registration changes, the fresh-stock gate), and the template calendar.

use super::*;
use crate::eval::chain::EpochObservation;
use crate::eval::engine::{self, EvalConfig, Kick};
use crate::eval::plan::{self, AdmissionPlan, Ceilings, Incentive, PoolRef, ProbeRef};
use crate::sql::EVAL_COHORT_SCHEMA;
use crate::testing::{Sqlite, tx};
use crate::wire::{BlockRef, Id};
use slop_ninja_admission_wire::{RequestScope, request_message};
use slop_ninja_protocol::submission::{SubmissionOpening, SubmissionUpload};
use slop_ninja_protocol::{FileRole, Nonce, TESTNET_GENESIS, artifact_object};

const NOW: u64 = 1_793_000_000_000;
const OPEN_BLOCK: u64 = 9_000;

fn chain() -> ChainBinding {
    ChainBinding {
        genesis: TESTNET_GENESIS,
        netuid: 601,
        mechanism: 0,
    }
}

fn cfg() -> EvalConfig {
    EvalConfig {
        chain: chain(),
        private_seed: [7; 32],
        instance: engine::INSTANCE_MAIN.into(),
        max_score_lanes: 64,
        ceilings: Ceilings {
            roster_cap: Some(40),
            accept_reward_v0: false,
        },
        exposed_pools: vec!["p".into()],
        scorer_binary: "aa".repeat(32),
        authority: crate::config::Authority::default(),
    }
}

fn env() -> AdmissionEnv {
    AdmissionEnv {
        scorer_binary: "aa".repeat(32),
        max_instances: 64,
    }
}

fn spec_v2() -> SpecBounds {
    SpecBounds::of(Version::V2).unwrap()
}

fn admission_block(cap: u16) -> AdmissionPlan {
    let b = spec_v2();
    AdmissionPlan {
        artifact_spec: b.spec_id.into(),
        artifact_spec_sha256: b.spec_sha256,
        roster_cap: cap,
        selection: slop_ninja_admission_wire::SELECTION_EARLIEST_COMMITMENT_V1.into(),
        part_size: 64 << 20,
        max_parts: 64,
        request_validity_blocks: 360,
        operator_hotkeys: vec![format!("0x{}", hex::encode(key(9).public()))],
        fresh_pool: Some(PoolRef {
            pool_id: "fresh".into(),
            manifest_key: "pools/fresh/manifest.json".into(),
            manifest_sha256: hex::encode([5; 32]),
        }),
    }
}

/// A reward-v1 plan with coordinator admission.
fn admission_plan(cohort: u64, cap: u16) -> plan::CohortPlan {
    let mut p = plan::tests::plan(100);
    p.schema = plan::PLAN_SCHEMA_V3.into();
    p.cohort = cohort;
    p.label = format!("testnet-c{cohort}");
    p.reward_rule = "reward-v1".into();
    p.mix = plan::Mix {
        human_only: 20,
        mixed: 55,
        model_only: 25,
    };
    p.k_tempos = 4;
    p.admission_tempos = 1;
    p.transition_tempos = 2;
    p.scorer.chunk_docs = 4;
    p.pool.pool_id = "p".into();
    let f = p.fanout.as_mut().unwrap();
    f.score.lane_docs = 100;
    f.score.artifact_max_mib = None;
    f.score.artifact_spec = Some(spec_v2().spec_id.into());
    f.score.load_ms_per_mib = Some(100);
    p.entrants = Vec::new();
    p.reward_v1 = Some(plan::RewardV1Plan {
        sigma: 40_000_000_000_000_000,
        sigma_basis: "synthetic test value".into(),
        lease_tempos: 8,
        reuse: plan::Reuse {
            max_uses: 4,
            min_gap: 7,
        },
        probes: vec![ProbeRef {
            version: 1,
            size: 10,
            key: "probes/v1/record.json".into(),
            sha256: hex::encode([3; 32]),
        }],
        gates: plan::GatePolicy {
            operating_ppb: 500_000_000,
            loose_fpr_max_permille: 100,
            strict_fpr_max_permille: 50,
            share_mae_max_ppb: 300_000_000,
            attribution_min_permille: 0,
        },
        canary: plan::CanaryPolicy {
            draw_humans: 20,
            drill: Vec::new(),
        },
        spares_permille: 200,
        output_contract: spec::CONTRACT_ID.into(),
        probe_version: None,
        incentive: Some(Incentive {
            rule: plan::INCENTIVE_REFERENCE_MARGIN.into(),
            provisional_gates: true,
        }),
    });
    p.admission = Some(admission_block(cap));
    p
}

fn key(n: u8) -> crate::chain::Hotkey {
    crate::chain::Hotkey::from_seed_hex(&hex::encode([n; 32])).unwrap()
}

fn account(n: u8) -> AccountId {
    AccountId(key(n).public())
}

fn db_with_open_cohort(cap: u16) -> Sqlite {
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let bytes = serde_json::to_vec(&admission_plan(1, cap)).unwrap();
    tx(&db, |s| engine::queue(s, &cfg(), &bytes, NOW)).unwrap();
    epoch(&db, 10, OPEN_BLOCK - 100, NOW);
    epoch(&db, 11, OPEN_BLOCK, NOW + 1);
    db
}

fn epoch(db: &Sqlite, index: u64, last_epoch_block: u64, now: u64) {
    let block = BlockRef {
        number: last_epoch_block + 2,
        hash: Id([index as u8; 32]),
    };
    let kick = Kick {
        confirmed: Some(block),
        epoch: Some(EpochObservation {
            block,
            index,
            last_epoch_block,
        }),
    };
    tx(db, |s| engine::observe(s, &kick, now)).unwrap();
}

/// A small artifact object (not spec-valid: framing's header checks are
/// tested separately); its content is what the miner commits.
fn object(tag: u8) -> Vec<u8> {
    let files: Vec<(&str, FileRole, Vec<u8>)> = vec![
        ("a-fp32band.onnx", FileRole::Graph, vec![tag; 300]),
        ("calibration.json", FileRole::Calibration, vec![b'{', tag, b'}']),
        ("contract.json", FileRole::OutputSchema, vec![b'c', tag]),
        ("heads.json", FileRole::Weights, vec![b'h', tag]),
        ("manifest.json", FileRole::OwnerMetadata, vec![b'm', tag]),
        ("tokenizer.json", FileRole::Tokenizer, vec![b't', tag]),
    ];
    let refs: Vec<(&str, FileRole, &[u8])> = files.iter().map(|(n, r, b)| (*n, *r, b.as_slice())).collect();
    artifact_object(spec::CONTRACT_ID, &refs).unwrap()
}

struct Miner {
    n: u8,
    uid: u16,
    registration_block: u64,
    obj: Vec<u8>,
    upload: SubmissionUpload,
}

fn miner(n: u8, uid: u16, cohort: u64, tag: u8) -> Miner {
    let obj = object(tag);
    let content = VerifiedArtifact::from_bytes(&obj).unwrap().content;
    let opening = SubmissionOpening {
        chain: chain(),
        cohort,
        hotkey: account(n),
        registration_block: 100 + u64::from(uid),
        content,
        nonce: Nonce([n.wrapping_mul(31).wrapping_add(tag); 32]),
    };
    let upload = SubmissionUpload::new(opening, &obj);
    Miner {
        n,
        uid,
        registration_block: 100 + u64::from(uid),
        obj,
        upload,
    }
}

impl Miner {
    fn commit(&self, db: &Sqlite, block: u64, extrinsic: u32) {
        let e = LogEntry {
            hotkey: account(self.n),
            block,
            block_hash: format!("0x{}", hex::encode([block as u8; 32])),
            extrinsic,
            payload: self.upload.opening.chain_payload().to_vec(),
            uid: Some(self.uid),
            registration_block: Some(self.registration_block),
        };
        tx(db, |s| commitments::record(s, &e, NOW)).unwrap();
    }

    fn session(&self) -> String {
        self.upload.digest().unwrap().hex()
    }

    fn signed(&self, db: &Sqlite, method: &str, path: &str, body: &[u8]) -> Signed {
        let cursor = tx(db, cursor).unwrap().unwrap();
        signed_by(self.n, cursor + 10, method, path, body)
    }

    fn create(&self, db: &Sqlite) -> (Reply, Effects) {
        let body = self.upload.to_bytes().unwrap();
        let mut r = self.signed(db, "POST", "/admission/v1/sessions", &body);
        r.body = Some(hex::encode(&body));
        tx(db, |s| Ok::<_, EvalError>(create(s, &chain(), &r, NOW))).unwrap()
    }
}

fn signed_by(n: u8, valid_until: u64, method: &str, path: &str, body: &[u8]) -> Signed {
    let digest = Digest(Sha256::digest(body).into());
    let message = request_message(&RequestScope {
        chain: &chain(),
        hotkey: &account(n),
        valid_until,
        method,
        path,
        content_sha256: &digest,
    });
    Signed {
        method: method.into(),
        path: path.into(),
        hotkey: Some(crate::chain::ss58(key(n).public())),
        valid_until: Some(valid_until.to_string()),
        content_sha256: Some(digest.hex()),
        signature: Some(hex::encode(key(n).sign(&message).unwrap())),
        body_sha256: Some(digest.hex()),
        body: None,
    }
}

fn code(r: &Reply) -> String {
    r.body["error"]["code"].as_str().unwrap_or("").to_owned()
}

fn state_of(db: &Sqlite, id: &str) -> (String, Option<String>) {
    let row = tx(db, |s| session_row(s, id)).unwrap().unwrap();
    (row.state.as_str().to_owned(), row.refusal.map(|r| r.code))
}

/// Uploads every part (recorded with the R2 tag the test invents) and
/// completes the session; returns the completion reply.
fn upload_all(db: &Sqlite, m: &Miner) -> Reply {
    let id = m.session();
    tx(db, |s| set_upload_id(s, &id, 1, "upload-1")).unwrap();
    let w = tx(db, window).unwrap().unwrap();
    let plan = w.part_plan(m.upload.artifact_len).unwrap();
    for n in 1..=plan.count() {
        let (a, b) = plan.range(n).unwrap();
        let part = m.obj.get(a as usize..(a + b) as usize).unwrap();
        let path = format!("/admission/v1/sessions/{id}/parts/{n}");
        let req = m.signed(db, "PUT", &path, part);
        let mut req = req;
        req.body_sha256 = None;
        let t = tx(db, |s| {
            Ok::<_, EvalError>(part_begin(s, &chain(), &req, &id, n, Some(b)))
        })
        .unwrap()
        .unwrap();
        let r = tx(db, |s| {
            Ok::<_, EvalError>(part_record(s, &id, &t, &format!("etag-{n}")))
        })
        .unwrap();
        assert_eq!(r.status, 200, "{:?}", r.body);
    }
    let path = format!("/admission/v1/sessions/{id}/complete");
    let req = m.signed(db, "POST", &path, &[]);
    let (r, fx) = tx(db, |s| Ok::<_, EvalError>(complete(s, &chain(), &req, &id, NOW))).unwrap();
    if r.status == 202 {
        assert!(fx.complete.is_some());
        tx(db, |s| completed(s, &id, NOW)).unwrap();
    }
    r
}

/// Framing as the Worker does it, with the object's own reader result (the
/// spec's header checks are skipped: the test object is not spec-valid).
fn frame_ok(db: &Sqlite, m: &Miner) {
    let id = m.session();
    let artifact = VerifiedArtifact::from_bytes(&m.obj).unwrap();
    let manifest = format!("manifest-of-{id}").into_bytes();
    let framed = Framed {
        package_sha256: hex::encode(Sha256::digest(&manifest)),
        model_sha256: hex::encode([1; 32]),
        manifest,
        slices: vec![Slice {
            sha256: hex::encode([2; 32]),
            offset: 0,
            len: 1,
        }],
        artifact,
    };
    tx(db, |s| {
        apply_frame(
            s,
            &chain(),
            &id,
            (m.upload.artifact_sha256, m.upload.artifact_len, Ok(framed)),
            NOW,
        )
    })
    .unwrap();
    tx(db, |s| blobs_written(s, &env(), &id, NOW)).unwrap();
}

fn admit_all(db: &Sqlite) {
    loop {
        let runs = tx(db, |s| claim_runs(s, &env(), 8, NOW)).unwrap();
        if runs.is_empty() {
            break;
        }
        for r in runs {
            tx(db, |s| record_run(s, &env(), &r, &LaneOutcome::Admitted, 3, NOW)).unwrap();
        }
    }
}

/// The reader's snapshot at the cutoff, as its ingest would leave it.
fn snapshot(
    db: &Sqlite,
    cutoff: u64,
    m: &Miner,
    block: Option<u64>,
    payload: &[u8],
    uid: Option<u16>,
    reg: Option<u64>,
) {
    tx(db, |s| {
        s.exec(
            "INSERT OR REPLACE INTO adm_snapshot (cutoff_block, hotkey, block, payload, uid, registration_block, done) VALUES (?, ?, ?, ?, ?, ?, 1)",
            &[
                bind_u64(cutoff)?,
                SqlValue::Text(hex::encode(account(m.n).0)),
                block.map_or(SqlValue::Null, |b| SqlValue::Integer(b as i64)),
                SqlValue::Text(hex::encode(payload)),
                uid.map_or(SqlValue::Null, |u| SqlValue::Integer(i64::from(u))),
                reg.map_or(SqlValue::Null, |b| SqlValue::Integer(b as i64)),
            ],
        )?;
        Ok::<_, EvalError>(())
    })
    .unwrap();
}

fn close_at(db: &Sqlite, cutoff: u64) {
    epoch(db, 12, cutoff, NOW + 2);
}

// ----------------------------------------------------------------- tests

#[test]
fn spec_bounds_derive_from_the_spec_and_size_the_lane_limits() {
    assert!(
        SpecBounds::of(Version::V1).is_none(),
        "spec v1 bounds no JSON file: the scorer never admits by it"
    );
    let v2 = spec_v2();
    assert_eq!(
        v2.graph_bytes,
        Version::V2.classes().iter().map(|c| c.bytes).max().unwrap()
    );
    assert_eq!(
        v2.files_bytes,
        v2.graph_bytes + spec::TOKENIZER_BYTES + Version::V2.max_json_bytes().unwrap()
    );
    assert_eq!(v2.object_bytes, v2.files_bytes + 4 + u64::from(MAX_ARTIFACT_HEADER_LEN));
    let v3 = SpecBounds::of(Version::V3).unwrap();
    assert!(v3.graph_bytes > v2.graph_bytes, "the tower class is longer");
    // The plan's lane limits come from the spec, not a tuned MiB value.
    let p = admission_plan(1, 2);
    let l = p.fanout.as_ref().unwrap().score.limits();
    assert_eq!(l.artifact_max_bytes, v2.files_bytes);
    assert_eq!(l.load_wait_ms, v2.files_bytes.div_ceil(1 << 20) * 100);
    plan::CohortPlan::parse(&serde_json::to_vec(&p).unwrap(), cfg().ceilings).unwrap();
    // Exactly one of the two bounds.
    let mut both = p.clone();
    both.fanout.as_mut().unwrap().score.artifact_max_mib = Some(640);
    assert!(plan::CohortPlan::parse(&serde_json::to_vec(&both).unwrap(), cfg().ceilings).is_err());
    // The admission block's own checks.
    let mut bad = p.clone();
    bad.admission.as_mut().unwrap().artifact_spec_sha256 = hex::encode([0; 32]);
    assert!(
        plan::CohortPlan::parse(&serde_json::to_vec(&bad).unwrap(), cfg().ceilings).is_err(),
        "the spec digest must be the spec's"
    );
    let mut bad = p.clone();
    bad.admission.as_mut().unwrap().max_parts = 2;
    assert!(
        plan::CohortPlan::parse(&serde_json::to_vec(&bad).unwrap(), cfg().ceilings).is_err(),
        "the largest object must fit"
    );
    let mut bad = p.clone();
    bad.admission.as_mut().unwrap().part_size = 1 << 20;
    assert!(
        plan::CohortPlan::parse(&serde_json::to_vec(&bad).unwrap(), cfg().ceilings).is_err(),
        "below R2's part minimum"
    );
    let mut bad = p.clone();
    bad.admission.as_mut().unwrap().roster_cap = 41;
    assert!(
        plan::CohortPlan::parse(&serde_json::to_vec(&bad).unwrap(), cfg().ceilings).is_err(),
        "past the deployment cap"
    );
    let mut bad = p;
    bad.fanout.as_mut().unwrap().score.artifact_spec = Some(spec::SPEC_ID_V3.into());
    assert!(
        plan::CohortPlan::parse(&serde_json::to_vec(&bad).unwrap(), cfg().ceilings).is_err(),
        "lanes size by the admission spec"
    );
}

#[test]
fn signed_requests_bind_chain_route_body_and_validity() {
    let r = signed_by(1, 1_010, "GET", "/admission/v1/sessions/ab", &[]);
    let (who, _) = authenticate(&chain(), 1_000, 360, &r).unwrap();
    assert_eq!(who, account(1));
    // Another path, method or chain: the signature does not verify.
    let mut other = r.clone();
    other.path = "/admission/v1/sessions/cd".into();
    assert_eq!(
        authenticate(&chain(), 1_000, 360, &other).unwrap_err().code,
        ErrorCode::BadSignature
    );
    let mut other_chain = chain();
    other_chain.netuid = 602;
    assert_eq!(
        authenticate(&other_chain, 1_000, 360, &r).unwrap_err().code,
        ErrorCode::BadSignature
    );
    // Expired, with the cursor to re-sign from; too long a window.
    let e = authenticate(&chain(), 1_011, 360, &r).unwrap_err();
    assert_eq!(
        (e.code, e.detail.finalized_block),
        (ErrorCode::RequestExpired, Some(1_011))
    );
    assert_eq!(
        authenticate(&chain(), 600, 360, &r).unwrap_err().code,
        ErrorCode::RequestValidityTooLong
    );
    // The body digest must be the signed one.
    let mut body = r.clone();
    body.body_sha256 = Some(hex::encode([0; 32]));
    assert_eq!(
        authenticate(&chain(), 1_000, 360, &body).unwrap_err().code,
        ErrorCode::ContentDigestMismatch
    );
    let mut nohk = r;
    nohk.hotkey = Some("not-an-address".into());
    assert_eq!(
        authenticate(&chain(), 1_000, 360, &nohk).unwrap_err().code,
        ErrorCode::BadSignature
    );
}

#[test]
fn opening_a_session_runs_every_check_in_contract_order() {
    let db = db_with_open_cohort(2);
    let w = tx(&db, window).unwrap().unwrap();
    assert_eq!((w.open_block, w.admitting(), w.cohort), (OPEN_BLOCK, true, 1));
    let m = miner(1, 3, 1, 0x11);
    // No commitment logged yet: retryable.
    let (r, _) = m.create(&db);
    assert_eq!(
        (r.status, code(&r).as_str(), r.retry_after),
        (409, "commitment_not_found", Some(60))
    );
    // A commitment before the cohort opened.
    m.commit(&db, OPEN_BLOCK - 1, 0);
    assert_eq!(code(&m.create(&db).0), "commitment_before_open");
    // Inside the window: created (201), then the same envelope is returned (200).
    m.commit(&db, OPEN_BLOCK + 5, 2);
    let (r, fx) = m.create(&db);
    assert_eq!(r.status, 201, "{:?}", r.body);
    assert_eq!(r.body["state"], "uploading");
    assert_eq!(fx.create.as_ref().map(|c| c.2), Some(1));
    assert_eq!(m.create(&db).0.status, 200);
    // Wrong cohort, wrong chain, another hotkey than the signer.
    let other = miner(2, 4, 2, 0x12);
    other.commit(&db, OPEN_BLOCK + 6, 1);
    let (r, _) = other.create(&db);
    assert_eq!(
        (code(&r).as_str(), r.body["error"]["detail"]["admitting"].as_u64()),
        ("wrong_cohort", Some(1))
    );
    let mut wrong_chain = miner(2, 4, 1, 0x12);
    wrong_chain.upload.opening.chain.netuid = 602;
    wrong_chain.upload = SubmissionUpload::new(wrong_chain.upload.opening.clone(), &wrong_chain.obj);
    assert_eq!(code(&wrong_chain.create(&db).0), "wrong_chain");
    let theirs = miner(3, 5, 1, 0x13);
    let body = theirs.upload.to_bytes().unwrap();
    let mut req = signed_by(
        2,
        tx(&db, cursor).unwrap().unwrap() + 1,
        "POST",
        "/admission/v1/sessions",
        &body,
    );
    req.body = Some(hex::encode(&body));
    let (r, _) = tx(&db, |s| Ok::<_, EvalError>(create(s, &chain(), &req, NOW))).unwrap();
    assert_eq!(code(&r), "hotkey_mismatch");
    // The opening does not open the hotkey's current commitment.
    let stale = miner(1, 3, 1, 0x21);
    let (r, _) = stale.create(&db);
    assert_eq!(
        (code(&r).as_str(), r.body["error"]["detail"]["payload_cohort"].as_u64()),
        ("commitment_mismatch", Some(1))
    );
    // Another registration generation.
    let mut regen = miner(1, 3, 1, 0x11);
    regen.upload.opening.registration_block = 1;
    regen.upload = SubmissionUpload::new(regen.upload.opening.clone(), &regen.obj);
    regen.commit(&db, OPEN_BLOCK + 7, 0);
    assert_eq!(code(&regen.create(&db).0), "registration_mismatch");
    // Not registered at its commitment.
    let unreg = miner(4, 6, 1, 0x14);
    let e = LogEntry {
        uid: None,
        registration_block: None,
        ..log(&unreg, OPEN_BLOCK + 8, 0)
    };
    tx(&db, |s| commitments::record(s, &e, NOW)).unwrap();
    assert_eq!(code(&unreg.create(&db).0), "not_registered");
    // An envelope larger than the spec's object bound.
    let mut big = miner(5, 7, 1, 0x15);
    big.upload.artifact_len = spec_v2().object_bytes + 1;
    big.commit(&db, OPEN_BLOCK + 9, 0);
    assert_eq!(code(&big.create(&db).0), "object_too_large");
    // Each refusal after the signature verifies is the hotkey's last refusal.
    let path = format!("/admission/v1/cohorts/1/hotkeys/{}", crate::chain::ss58(account(1).0));
    let req = regen.signed(&db, "GET", &path, &[]);
    let v = tx(&db, |s| {
        Ok::<_, EvalError>(hotkey_view(s, &chain(), &req, 1, &crate::chain::ss58(account(1).0)))
    })
    .unwrap();
    assert_eq!(v.body["last_refusal"]["code"], "registration_mismatch");
    assert_eq!(v.body["commitment"]["block"], OPEN_BLOCK + 7);
    assert_eq!(v.body["sessions"].as_array().unwrap().len(), 1);
    // Another hotkey's view is forbidden.
    let req = m.signed(&db, "GET", &path, &[]);
    let v = tx(&db, |s| {
        Ok::<_, EvalError>(hotkey_view(s, &chain(), &req, 1, &crate::chain::ss58(account(9).0)))
    })
    .unwrap();
    assert_eq!(code(&v), "forbidden");
}

fn log(m: &Miner, block: u64, extrinsic: u32) -> LogEntry {
    LogEntry {
        hotkey: account(m.n),
        block,
        block_hash: format!("0x{}", hex::encode([block as u8; 32])),
        extrinsic,
        payload: m.upload.opening.chain_payload().to_vec(),
        uid: Some(m.uid),
        registration_block: Some(m.registration_block),
    }
}

#[test]
fn one_artifact_per_hotkey_attempts_and_supersession() {
    let db = db_with_open_cohort(2);
    let a = miner(1, 3, 1, 0x11);
    a.commit(&db, OPEN_BLOCK + 1, 0);
    assert_eq!(a.create(&db).0.status, 201);
    // Another envelope for the same commitment (a different object with the
    // same committed content is impossible; a different length is enough).
    let mut b = miner(1, 3, 1, 0x11);
    b.upload.artifact_len += 1;
    let (r, _) = b.create(&db);
    assert_eq!(
        (code(&r).as_str(), r.body["error"]["detail"]["session"].as_str()),
        ("second_artifact", Some(a.session().as_str()))
    );
    // Abort the live attempt; a stale attempt number is refused; then the
    // second envelope opens.
    let id = a.session();
    let req = a.signed(&db, "DELETE", &format!("/admission/v1/sessions/{id}/attempts/2"), &[]);
    let (r, _) = tx(&db, |s| Ok::<_, EvalError>(abort(s, &chain(), &req, &id, 2, NOW))).unwrap();
    assert_eq!(
        (code(&r).as_str(), r.body["error"]["detail"]["attempt"].as_u64()),
        ("stale_attempt", Some(1))
    );
    let req = a.signed(&db, "DELETE", &format!("/admission/v1/sessions/{id}/attempts/1"), &[]);
    let (r, fx) = tx(&db, |s| Ok::<_, EvalError>(abort(s, &chain(), &req, &id, 1, NOW))).unwrap();
    assert_eq!((r.body["state"].as_str(), fx.abort.len()), (Some("aborted"), 1));
    // Sending the abort again returns the session unchanged.
    let (r, fx) = tx(&db, |s| Ok::<_, EvalError>(abort(s, &chain(), &req, &id, 1, NOW))).unwrap();
    assert_eq!((r.body["state"].as_str(), fx.abort.len()), (Some("aborted"), 0));
    // The same envelope returns the aborted session; reopening is its own
    // request, as attempt 2 (attempt 3 is stale).
    assert_eq!(a.create(&db).0.body["state"], "aborted");
    let req = a.signed(&db, "POST", &format!("/admission/v1/sessions/{id}/attempts/3"), &[]);
    let (r, _) = tx(&db, |s| Ok::<_, EvalError>(reopen(s, &chain(), &req, &id, 3, NOW))).unwrap();
    assert_eq!(code(&r), "stale_attempt");
    let req = a.signed(&db, "POST", &format!("/admission/v1/sessions/{id}/attempts/2"), &[]);
    let (r, fx) = tx(&db, |s| Ok::<_, EvalError>(reopen(s, &chain(), &req, &id, 2, NOW))).unwrap();
    assert_eq!(
        (r.body["attempt"].as_u64(), r.body["state"].as_str()),
        (Some(2), Some("uploading"))
    );
    assert!(
        fx.create.unwrap().0.ends_with("/a2.object"),
        "a new attempt has its own object"
    );
    // A newer commitment supersedes the live session.
    let c = miner(1, 3, 1, 0x31);
    c.commit(&db, OPEN_BLOCK + 2, 0);
    let (r, fx) = c.create(&db);
    assert_eq!(r.status, 201);
    assert_eq!(fx.abort.len(), 1);
    assert_eq!(state_of(&db, &id).0, "superseded");
}

#[test]
fn parts_completion_and_the_hotkey_view() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    let id = m.session();
    // No upload id yet: retry shortly.
    let req = m.signed(&db, "PUT", &format!("/admission/v1/sessions/{id}/parts/1"), &m.obj);
    let e = tx(&db, |s| {
        Ok::<_, EvalError>(part_begin(s, &chain(), &req, &id, 1, Some(m.obj.len() as u64)))
    })
    .unwrap()
    .unwrap_err();
    assert_eq!(code(&e), "unavailable");
    assert_eq!(tx(&db, |s| upload_missing(s, &id)).unwrap().map(|x| x.1), Some(1));
    tx(&db, |s| set_upload_id(s, &id, 1, "u")).unwrap();
    let e = tx(&db, |s| {
        Ok::<_, EvalError>(part_begin(s, &chain(), &req, &id, 2, Some(1)))
    })
    .unwrap()
    .unwrap_err();
    assert_eq!(code(&e), "part_out_of_range");
    let e = tx(&db, |s| {
        Ok::<_, EvalError>(part_begin(s, &chain(), &req, &id, 1, Some(1)))
    })
    .unwrap()
    .unwrap_err();
    assert_eq!(code(&e), "part_length_mismatch");
    // Completion before every part.
    let creq = m.signed(&db, "POST", &format!("/admission/v1/sessions/{id}/complete"), &[]);
    let (r, _) = tx(&db, |s| Ok::<_, EvalError>(complete(s, &chain(), &creq, &id, NOW))).unwrap();
    assert_eq!(
        (
            code(&r).as_str(),
            r.body["error"]["detail"]["missing"].as_array().map(Vec::len)
        ),
        ("parts_missing", Some(1))
    );
    assert_eq!(upload_all(&db, &m).status, 202);
    assert_eq!(state_of(&db, &id).0, "verifying");
    // A completion sent again returns the session; parts are no longer accepted.
    let (r, _) = tx(&db, |s| Ok::<_, EvalError>(complete(s, &chain(), &creq, &id, NOW))).unwrap();
    assert_eq!(r.status, 202);
    let e = tx(&db, |s| {
        Ok::<_, EvalError>(part_begin(s, &chain(), &req, &id, 1, Some(m.obj.len() as u64)))
    })
    .unwrap()
    .unwrap_err();
    assert_eq!(code(&e), "session_not_uploading");
    // The session read shows the recorded parts.
    let rreq = m.signed(&db, "GET", &format!("/admission/v1/sessions/{id}"), &[]);
    let r = tx(&db, |s| Ok::<_, EvalError>(read(s, &chain(), &rreq, &id))).unwrap();
    assert_eq!(
        (r.body["parts"].as_array().map(Vec::len), r.body["part_count"].as_u64()),
        (Some(1), Some(1))
    );
    // Another hotkey cannot read it.
    let other = miner(2, 4, 1, 0x12);
    let oreq = other.signed(&db, "GET", &format!("/admission/v1/sessions/{id}"), &[]);
    assert_eq!(
        code(&tx(&db, |s| Ok::<_, EvalError>(read(s, &chain(), &oreq, &id))).unwrap()),
        "forbidden"
    );
    // A multipart completion R2 refuses ends the session as object_mismatch.
    let m2 = miner(2, 4, 1, 0x12);
    m2.commit(&db, OPEN_BLOCK + 2, 0);
    m2.create(&db);
    let id2 = m2.session();
    tx(&db, |s| set_upload_id(s, &id2, 1, "u2")).unwrap();
    let preq = m2.signed(&db, "PUT", &format!("/admission/v1/sessions/{id2}/parts/1"), &m2.obj);
    let t = tx(&db, |s| {
        Ok::<_, EvalError>(part_begin(s, &chain(), &preq, &id2, 1, Some(m2.obj.len() as u64)))
    })
    .unwrap()
    .unwrap();
    tx(&db, |s| Ok::<_, EvalError>(part_record(s, &id2, &t, "e"))).unwrap();
    let creq2 = m2.signed(&db, "POST", &format!("/admission/v1/sessions/{id2}/complete"), &[]);
    tx(&db, |s| Ok::<_, EvalError>(complete(s, &chain(), &creq2, &id2, NOW))).unwrap();
    tx(&db, |s| completion_failed(s, &id2, NOW)).unwrap();
    assert_eq!(state_of(&db, &id2), ("refused".into(), Some("object_mismatch".into())));
}

#[test]
fn framing_checks_the_envelope_the_contract_and_the_commitment() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 4);
    m.create(&db);
    upload_all(&db, &m);
    let id = m.session();
    // The test object fails the spec's header checks: a check refusal.
    let mut f = Framer::new();
    f.update(&m.obj);
    let framed = f.finish(Version::V2);
    assert_eq!((framed.0, framed.1), (m.upload.artifact_sha256, m.upload.artifact_len));
    assert!(
        matches!(&framed.2, Err(FrameError::Check(c)) if c == "tokenizer_mismatch" || c == "file_size"),
        "{:?}",
        framed.2.as_ref().err()
    );
    tx(&db, |s| apply_frame(s, &chain(), &id, framed, NOW)).unwrap();
    assert_eq!(state_of(&db, &id), ("refused".into(), Some("check_failed".into())));
    // A changed byte: object_mismatch, decided before anything else.
    let m2 = miner(2, 4, 1, 0x12);
    m2.commit(&db, OPEN_BLOCK + 2, 0);
    m2.create(&db);
    upload_all(&db, &m2);
    let mut bytes = m2.obj.clone();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let mut f = Framer::new();
    f.update(&bytes);
    tx(&db, |s| {
        apply_frame(s, &chain(), &m2.session(), f.finish(Version::V2), NOW)
    })
    .unwrap();
    assert_eq!(
        state_of(&db, &m2.session()),
        ("refused".into(), Some("object_mismatch".into()))
    );
    // Garbage that is not an artifact object: artifact_invalid.
    let mut f = Framer::new();
    f.update(&[0xff; 64]);
    assert!(matches!(f.finish(Version::V2).2, Err(FrameError::Reader(_))));
    // A framed object whose content is not the committed one.
    let m3 = miner(3, 5, 1, 0x13);
    m3.commit(&db, OPEN_BLOCK + 3, 0);
    m3.create(&db);
    upload_all(&db, &m3);
    let other = VerifiedArtifact::from_bytes(&object(0x77)).unwrap();
    let framed = Framed {
        package_sha256: hex::encode([1; 32]),
        model_sha256: hex::encode([1; 32]),
        manifest: vec![],
        slices: vec![],
        artifact: other,
    };
    tx(&db, |s| {
        apply_frame(
            s,
            &chain(),
            &m3.session(),
            (m3.upload.artifact_sha256, m3.upload.artifact_len, Ok(framed)),
            NOW,
        )
    })
    .unwrap();
    assert_eq!(
        state_of(&db, &m3.session()),
        ("refused".into(), Some("content_mismatch".into()))
    );
}

#[test]
fn framer_slices_each_file_at_the_scorer_part_size() {
    // Slicing is per file, at PART_BYTES, the last slice of a file shorter:
    // checked on a small object through the private slice table.
    let obj = object(0x42);
    let mut f = Framer::new();
    for chunk in obj.chunks(7) {
        f.update(chunk);
    }
    assert!(f.failed.is_none());
    let header = f.reader.header().map(|(h, _)| h.clone()).unwrap();
    assert_eq!(f.slices.len(), header.files.len());
    for (file, parts) in header.files.iter().zip(&f.slices) {
        assert_eq!(parts.len(), 1, "files under 64 MiB are one slice");
        assert_eq!(parts[0].bytes, file.len);
        assert_eq!(
            parts[0].sha256,
            file.sha256.hex(),
            "a whole-file slice hashes as the file"
        );
    }
    // Offsets follow the header order after the length prefix and header,
    // as the reader reports them (the framer parses nothing itself).
    let header_len = u32::from_le_bytes(obj[..4].try_into().unwrap()) as u64;
    assert_eq!(f.files_at, Some(4 + header_len));
    let (_, _, framed) = f.finish(Version::V2);
    assert!(
        matches!(framed, Err(FrameError::Check(_))),
        "the test object is not spec-valid"
    );
}

#[test]
fn verdict_rules_one_run_bound_definitive_and_counted() {
    let db = db_with_open_cohort(4);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    upload_all(&db, &m);
    frame_ok(&db, &m);
    // (d) the deployment's instance ceiling bounds claims.
    let none = tx(&db, |s| {
        claim_runs(
            s,
            &AdmissionEnv {
                max_instances: 0,
                ..env()
            },
            8,
            NOW,
        )
    })
    .unwrap();
    assert!(none.is_empty());
    let runs = tx(&db, |s| claim_runs(s, &env(), 8, NOW)).unwrap();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].lane_name("testnet").contains("/admit/testnet-c1/"));
    // (c) platform failures retry without counting.
    tx(&db, |s| {
        record_run(
            s,
            &env(),
            &runs[0],
            &LaneOutcome::Platform("insufficient_memory".into()),
            2,
            NOW,
        )
    })
    .unwrap();
    assert!(
        tx(&db, |s| claim_runs(s, &env(), 8, NOW)).unwrap().is_empty(),
        "the retry waits"
    );
    let later = NOW + LANE_RETRY_MS;
    let run = tx(&db, |s| claim_runs(s, &env(), 8, later)).unwrap().remove(0);
    assert_eq!(run.try_no, 2, "each try is a fresh instance");
    // Attributable failures count toward the plan's bound, then refuse.
    tx(&db, |s| {
        record_run(
            s,
            &env(),
            &run,
            &LaneOutcome::ArtifactFailure("artifact load failed".into()),
            2,
            later,
        )
    })
    .unwrap();
    assert_eq!(state_of(&db, &m.session()).0, "verifying");
    let run = tx(&db, |s| claim_runs(s, &env(), 8, later + LANE_RETRY_MS))
        .unwrap()
        .remove(0);
    tx(&db, |s| {
        record_run(
            s,
            &env(),
            &run,
            &LaneOutcome::ArtifactFailure("artifact load failed".into()),
            2,
            later,
        )
    })
    .unwrap();
    let (st, c) = state_of(&db, &m.session());
    assert_eq!((st.as_str(), c.as_deref()), ("refused", Some("check_failed")));
    // A stale claim's outcome is ignored.
    tx(&db, |s| {
        record_run(s, &env(), &runs[0], &LaneOutcome::Admitted, 2, later)
    })
    .unwrap();
    assert_eq!(state_of(&db, &m.session()).0, "refused");
    // A definitive refusal ends the session with the scorer's code.
    let m2 = miner(2, 4, 1, 0x12);
    m2.commit(&db, OPEN_BLOCK + 2, 0);
    m2.create(&db);
    upload_all(&db, &m2);
    frame_ok(&db, &m2);
    let run = tx(&db, |s| claim_runs(s, &env(), 8, later * 2)).unwrap().remove(0);
    tx(&db, |s| {
        record_run(
            s,
            &env(),
            &run,
            &classify(Some(502), &serde_json::json!({"error": "refused graph_op_not_allowed"})),
            2,
            later,
        )
    })
    .unwrap();
    let row = tx(&db, |s| session_row(s, &m2.session())).unwrap().unwrap();
    assert_eq!(
        row.refusal.unwrap().detail.check.as_deref(),
        Some("graph_op_not_allowed")
    );
    // (b) a verdict reached on another scorer binary is admitted again.
    let m3 = miner(3, 5, 1, 0x13);
    m3.commit(&db, OPEN_BLOCK + 3, 0);
    m3.create(&db);
    upload_all(&db, &m3);
    frame_ok(&db, &m3);
    let run = tx(&db, |s| claim_runs(s, &env(), 8, later * 2)).unwrap().remove(0);
    let new_build = AdmissionEnv {
        scorer_binary: "bb".repeat(32),
        ..env()
    };
    let rerun = tx(&db, |s| claim_runs(s, &new_build, 8, later * 2 + LANE_LEASE_MS)).unwrap();
    // Rule (b): the pending run is re-bound, and both refusals reached on
    // the old build run again too (their sessions wait on them again).
    assert_eq!(rerun.len(), 3, "every verdict not reached on the new build runs on it");
    assert!(rerun.iter().all(|r| r.scorer_binary == new_build.scorer_binary));
    assert_eq!(state_of(&db, &m.session()).0, "verifying");
    assert_eq!(state_of(&db, &m2.session()).0, "verifying");
    let m3_hotkey = hex::encode(account(3).0);
    let rerun3 = rerun.iter().find(|r| r.hotkey == m3_hotkey).unwrap().clone();
    tx(&db, |s| {
        record_run(s, &new_build, &run, &LaneOutcome::Admitted, 2, later)
    })
    .unwrap();
    assert_eq!(
        state_of(&db, &m3.session()).0,
        "verifying",
        "the old build's claim no longer counts"
    );
    tx(&db, |s| {
        record_run(s, &new_build, &rerun3, &LaneOutcome::Admitted, 2, later)
    })
    .unwrap();
    assert_eq!(state_of(&db, &m3.session()).0, "verified");
    let reached = tx(&db, |s| {
        let r = s.exec(
            "SELECT reached_binary FROM adm_verdicts WHERE hotkey = ?",
            &[SqlValue::Text(m3_hotkey.clone())],
        )?;
        Ok::<_, EvalError>(text(&r[0], 0)?.to_owned())
    })
    .unwrap();
    assert_eq!(
        reached, new_build.scorer_binary,
        "the verdict records the build it was reached on"
    );
    // (a) one run per hotkey and commitment: another object for the same
    // commitment, after an abort, is refused without a run.
    let id = m3.session();
    let req = m3.signed(&db, "DELETE", &format!("/admission/v1/sessions/{id}/attempts/1"), &[]);
    tx(&db, |s| Ok::<_, EvalError>(abort(s, &chain(), &req, &id, 1, NOW))).unwrap();
    let mut again = miner(3, 5, 1, 0x13);
    again.upload.artifact_len += 0;
    let req = m3.signed(&db, "POST", &format!("/admission/v1/sessions/{id}/attempts/2"), &[]);
    tx(&db, |s| Ok::<_, EvalError>(reopen(s, &chain(), &req, &id, 2, NOW))).unwrap();
    upload_all_attempt(&db, &again, 2);
    let artifact = VerifiedArtifact::from_bytes(&again.obj).unwrap();
    let framed = Framed {
        package_sha256: hex::encode([0xee; 32]),
        model_sha256: hex::encode([1; 32]),
        manifest: vec![1],
        slices: vec![],
        artifact,
    };
    tx(&db, |s| {
        apply_frame(
            s,
            &chain(),
            &id,
            (again.upload.artifact_sha256, again.upload.artifact_len, Ok(framed)),
            NOW,
        )
    })
    .unwrap();
    tx(&db, |s| blobs_written(s, &env(), &id, NOW)).unwrap();
    let row = tx(&db, |s| session_row(s, &id)).unwrap().unwrap();
    assert_eq!(row.refusal.unwrap().detail.check.as_deref(), Some("admission_run_used"));
}

fn control_result(status: &str) -> serde_json::Value {
    let sha = hex::encode(Sha256::digest(crate::eval::lane_logic::CONTROL_TEXT.as_bytes()));
    serde_json::json!({"results": [{"text_sha256": sha, "status": status,
        "prediction": {"ai_confidence_percent": 40.0, "ai_share_percent": 10.0}}]})
}

#[test]
fn classify_reads_the_reply_and_counts_only_the_artifacts_own_failures() {
    use serde_json::json;
    let err = |c: &str| json!({"error": c});
    // Admitted only when the control document scored.
    assert_eq!(classify(Some(200), &control_result("ok")), LaneOutcome::Admitted);
    assert!(matches!(
        classify(Some(200), &control_result("error")),
        LaneOutcome::ArtifactFailure(_)
    ));
    assert!(matches!(
        classify(Some(200), &control_result("unsupported_input")),
        LaneOutcome::ArtifactFailure(_)
    ));
    assert!(
        matches!(classify(Some(200), &err("Failed")), LaneOutcome::ArtifactFailure(c) if c == "control_document_failed"),
        "a load that succeeded but could not score the control document is no admission"
    );
    assert!(matches!(classify(Some(200), &json!({})), LaneOutcome::Platform(_)));
    // Definitive: the scorer's recorded refusal codes, and an unknown one.
    assert!(
        matches!(classify(Some(502), &err("refused graph_op_not_allowed")), LaneOutcome::Refused(c) if c == "graph_op_not_allowed")
    );
    assert!(matches!(classify(Some(502), &err("refused")), LaneOutcome::Refused(c) if c == "refused"));
    // A host fault filed under `object` counts on fresh instances, never refuses.
    assert!(crate::eval::scorer_client::HOST_FAULT_CODES.contains(&"refused object"));
    assert!(matches!(
        classify(Some(502), &err("refused object")),
        LaneOutcome::ArtifactFailure(_)
    ));
    assert!(matches!(
        classify(Some(502), &err("artifact load outlasted its allowance")),
        LaneOutcome::ArtifactFailure(_)
    ));
    // The coordinator's own manifest and blobs, and the platform's codes.
    for c in [
        "artifact manifest read",
        "artifact manifest missing",
        "artifact manifest digest",
        "artifact stream timeout",
        "part missing",
        "insufficient_memory",
        "load refused",
        crate::eval::scorer_client::REUSED,
        "something new",
    ] {
        assert!(matches!(classify(Some(502), &err(c)), LaneOutcome::Platform(_)), "{c}");
    }
    assert!(matches!(classify(Some(404), &json!(null)), LaneOutcome::Platform(_)));
    assert!(matches!(classify(None, &json!(null)), LaneOutcome::Platform(_)));
}

fn upload_all_attempt(db: &Sqlite, m: &Miner, attempt: u32) {
    let id = m.session();
    tx(db, |s| set_upload_id(s, &id, attempt, "upload-n")).unwrap();
    let req = m.signed(db, "PUT", &format!("/admission/v1/sessions/{id}/parts/1"), &m.obj);
    let t = tx(db, |s| {
        Ok::<_, EvalError>(part_begin(s, &chain(), &req, &id, 1, Some(m.obj.len() as u64)))
    })
    .unwrap()
    .unwrap();
    tx(db, |s| Ok::<_, EvalError>(part_record(s, &id, &t, "e"))).unwrap();
    let creq = m.signed(db, "POST", &format!("/admission/v1/sessions/{id}/complete"), &[]);
    tx(db, |s| Ok::<_, EvalError>(complete(s, &chain(), &creq, &id, NOW))).unwrap();
    tx(db, |s| completed(s, &id, NOW)).unwrap();
}

#[test]
fn the_cutoff_ranks_caps_queues_overflow_and_refuses_changed_commitments() {
    let db = db_with_open_cohort(2);
    // Five miners: positions decide the order, not upload time.
    let ms: Vec<Miner> = (1..=5u8).map(|n| miner(n, u16::from(n) + 2, 1, 0x10 + n)).collect();
    let positions = [
        (OPEN_BLOCK + 9, 1),
        (OPEN_BLOCK + 3, 7),
        (OPEN_BLOCK + 3, 2),
        (OPEN_BLOCK + 20, 0),
        (OPEN_BLOCK + 25, 0),
    ];
    for (m, (b, x)) in ms.iter().zip(positions) {
        m.commit(&db, b, x);
        assert_eq!(m.create(&db).0.status, 201);
        upload_all(&db, m);
        frame_ok(&db, m);
    }
    admit_all(&db);
    // A sixth miner never completes.
    let late = miner(6, 8, 1, 0x16);
    late.commit(&db, OPEN_BLOCK + 30, 0);
    late.create(&db);
    let cutoff = OPEN_BLOCK + 360;
    close_at(&db, cutoff);
    let w = tx(&db, window).unwrap().unwrap();
    assert_eq!((w.state.as_str(), w.cutoff_block), ("closing", Some(cutoff)));
    assert_eq!(
        state_of(&db, &late.session()),
        ("refused".into(), Some("incomplete_at_cutoff".into()))
    );
    // After the close: parts and completions are refused, new sessions too.
    let req = late.signed(
        &db,
        "PUT",
        &format!("/admission/v1/sessions/{}/parts/1", late.session()),
        &late.obj,
    );
    let e = tx(&db, |s| {
        Ok::<_, EvalError>(part_begin(s, &chain(), &req, &late.session(), 1, Some(1)))
    })
    .unwrap()
    .unwrap_err();
    assert_eq!(code(&e), "after_cutoff");
    let after = miner(7, 9, 1, 0x17);
    after.commit(&db, cutoff - 1, 0);
    assert_eq!(code(&after.create(&db).0), "after_cutoff");
    // The snapshot is needed for the five verified hotkeys.
    let (c, wanted) = tx(&db, snapshot_needs).unwrap().unwrap();
    assert_eq!((c, wanted.len()), (cutoff, 5));
    assert!(matches!(
        tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap(),
        Some(Boundary::Waiting(_))
    ));
    // Miner 4 committed again before the cutoff: overwritten. Miner 5's UID
    // changed hands before the cutoff: a registration change.
    for (k, m) in ms.iter().enumerate() {
        let (b, _) = positions[k];
        let payload = m.upload.opening.chain_payload().to_vec();
        match k {
            3 => snapshot(
                &db,
                cutoff,
                m,
                Some(cutoff - 5),
                &[1, 2, 3],
                Some(m.uid),
                Some(m.registration_block),
            ),
            4 => snapshot(&db, cutoff, m, Some(b), &payload, Some(m.uid), Some(cutoff - 50)),
            _ => snapshot(
                &db,
                cutoff,
                m,
                Some(b),
                &payload,
                Some(m.uid),
                Some(m.registration_block),
            ),
        }
    }
    let b = tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap().unwrap();
    assert_eq!(
        b,
        Boundary::Frozen {
            admitted: 2,
            overflow: 1,
            outside: true
        }
    );
    assert_eq!(state_of(&db, &ms[2].session()).0, "admitted", "(OPEN+3, 2) is first");
    assert_eq!(state_of(&db, &ms[1].session()).0, "admitted", "(OPEN+3, 7) is second");
    let over = tx(&db, |s| session_row(s, &ms[0].session())).unwrap().unwrap();
    assert_eq!((over.state.as_str(), over.next_cohort), ("overflow", Some(2)));
    assert_eq!(
        state_of(&db, &ms[3].session()).1.as_deref(),
        Some("commitment_overwritten")
    );
    assert_eq!(
        state_of(&db, &ms[4].session()).1.as_deref(),
        Some("registration_mismatch")
    );
    // The frozen plan names the admitted roster with openings and positions,
    // and the outside entrant unlocks the fresh pool.
    let c = tx(&db, |s| engine::cohort(s, 1)).unwrap();
    assert_eq!(c.phase, "generating");
    assert_eq!(c.plan.entrants.len(), 2);
    let first = &c.plan.entrants[0];
    assert_eq!(first.owner.hotkey, format!("0x{}", hex::encode(account(3).0)));
    assert_eq!(
        (first.commitment_block, first.extrinsic_index),
        (OPEN_BLOCK + 3, Some(2))
    );
    assert_eq!(
        first.opening_nonce.as_deref(),
        Some(
            ms[2]
                .upload
                .opening
                .nonce
                .0
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
                .as_str()
        )
    );
    assert!(
        first
            .object_key
            .as_deref()
            .unwrap()
            .starts_with("admission/testnet-c1/")
    );
    assert_eq!(c.plan.pool.pool_id, "fresh");
    assert!(tx(&db, |s| has_outside_entrant(s, 1)).unwrap());
    assert!(tx(&db, |s| crate::eval::v3::fresh_allowed(s, &c)).unwrap());
    // The stored plan is the frozen one, and validates.
    plan::CohortPlan::parse(&serde_json::to_vec(&c.plan).unwrap(), cfg().ceilings).unwrap();
    assert_eq!(c.plan_sha256, engine::sha_hex(&serde_json::to_vec(&c.plan).unwrap()));
    // Objects of the sessions that ended without admission are cleaned up.
    let due = tx(&db, |s| cleanup_due(s, 10)).unwrap();
    assert_eq!(due.len(), 4, "overflow, overwritten, registration and incomplete");
    // The closure names the admitted objects for publication.
    let v = closure_artifacts(&c.plan);
    assert_eq!(v["artifacts"].as_array().unwrap().len(), 2);
    // The admission status is content-free.
    let st = tx(&db, status).unwrap();
    assert_eq!(st["cohorts"][0]["state"], "frozen");
    assert!(!st.to_string().contains(&hex::encode(ms[2].upload.opening.nonce.0)));
}

#[test]
fn operator_entrants_never_unlock_fresh_stock_and_pending_verdicts_hold_the_freeze() {
    let db = db_with_open_cohort(2);
    let ours = miner(9, 2, 1, 0x19);
    ours.commit(&db, OPEN_BLOCK + 1, 0);
    ours.create(&db);
    upload_all(&db, &ours);
    frame_ok(&db, &ours);
    let cutoff = OPEN_BLOCK + 360;
    close_at(&db, cutoff);
    snapshot(
        &db,
        cutoff,
        &ours,
        Some(OPEN_BLOCK + 1),
        &ours.upload.opening.chain_payload(),
        Some(ours.uid),
        Some(ours.registration_block),
    );
    // The verdict is still pending: the freeze waits, the cohort is not skipped.
    assert!(matches!(
        tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap(),
        Some(Boundary::Waiting(_))
    ));
    assert_eq!(tx(&db, |s| engine::cohort(s, 1)).unwrap().phase, "admission");
    admit_all(&db);
    let b = tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap().unwrap();
    assert_eq!(
        b,
        Boundary::Frozen {
            admitted: 1,
            overflow: 0,
            outside: false
        }
    );
    let c = tx(&db, |s| engine::cohort(s, 1)).unwrap();
    assert_eq!(c.plan.pool.pool_id, "p", "no outside entrant: exposed stock only");
    assert!(!tx(&db, |s| crate::eval::v3::fresh_allowed(s, &c)).unwrap());
}

#[test]
fn the_cohort_response_names_the_window_and_its_limits() {
    let db = db_with_open_cohort(2);
    let info = tx(&db, |s| cohort_info(s, &chain(), false)).unwrap();
    let a = info.admitting.unwrap();
    assert_eq!(
        (a.cohort, a.open_block, a.planned_cutoff_epoch, a.roster_cap),
        (1, OPEN_BLOCK, 12, 2)
    );
    assert_eq!(a.max_object_len, spec_v2().object_bytes);
    assert_eq!(a.output_contract, spec::CONTRACT_ID);
    assert_eq!(a.artifact_spec.unwrap().sha256, Version::V2.spec_sha256());
    assert_eq!(info.next.unwrap().earliest_open_epoch, Some(15));
    assert_eq!(info.request_validity_blocks, 360);
    assert!(serde_json::to_vec(&admission_plan(1, 2)).unwrap().len() < crate::arena_routes::MAX_PLAN_BODY_BYTES);
}

#[test]
fn the_calendar_opens_cohorts_from_the_standing_template() {
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let t = crate::eval::calendar::Template {
        schema: crate::eval::calendar::TEMPLATE_SCHEMA.into(),
        label_prefix: "testnet-cohort".into(),
        plan: admission_plan(0, 40),
    };
    let bytes = serde_json::to_vec(&t).unwrap();
    tx(&db, |s| crate::eval::calendar::install(s, &cfg(), &bytes, NOW)).unwrap();
    let (label, plan) = tx(&db, |s| crate::eval::calendar::due_any(s, 0)).unwrap().unwrap();
    assert_eq!(label, "testnet-cohort-1");
    // The step's reason to run counts the calendar only for an authority that
    // can open a cohort: a retired one (superseded, or not the one the
    // deployment names) has no work here, so its alarm never re-arms for it.
    assert!(tx(&db, |s| engine::has_work(s, &cfg())).unwrap());
    let retired = engine::EvalConfig { authority: crate::config::Authority::retired("main"), ..cfg() };
    assert!(!tx(&db, |s| engine::has_work(s, &retired)).unwrap());
    tx(&db, |s| engine::queue(s, &cfg(), &plan, NOW)).unwrap();
    assert!(
        tx(&db, |s| crate::eval::calendar::due_any(s, 0)).unwrap().is_none(),
        "one queued at a time"
    );
    assert_eq!(tx(&db, crate::eval::calendar::next).unwrap().map(|x| x.0), Some(1));
    // A cohort in its transition lets the calendar queue the next one.
    tx(&db, |s| {
        s.exec("UPDATE cohorts SET phase = 'transition'", &[]).map(|_| ())
    })
    .unwrap();
    let (label, _) = tx(&db, |s| crate::eval::calendar::due_any(s, 0)).unwrap().unwrap();
    assert_eq!(label, "testnet-cohort-2");
    // Paused: nothing is due.
    tx(&db, |s| crate::eval::calendar::set_paused(s, true)).unwrap();
    assert!(tx(&db, |s| crate::eval::calendar::due_any(s, 0)).unwrap().is_none());
    // Templates are checked at their roster cap.
    let mut wide = t.clone();
    wide.plan.admission.as_mut().unwrap().roster_cap = 41;
    assert!(
        crate::eval::calendar::check(&cfg(), &serde_json::to_vec(&wide).unwrap()).is_err(),
        "past the deployment cap"
    );
    let mut entrants = t.clone();
    entrants.plan.entrants = vec![plan::tests::artifact(plan::tests::identity(3, 3), 3)];
    assert!(crate::eval::calendar::check(&cfg(), &serde_json::to_vec(&entrants).unwrap()).is_err());
    let mut poor = t.clone();
    poor.plan.fanout.as_mut().unwrap().score.scoring_budget_microusd = 1;
    assert!(
        crate::eval::calendar::check(&cfg(), &serde_json::to_vec(&poor).unwrap()).is_err(),
        "the budget covers a full roster"
    );
    let mut fresh = t;
    fresh.plan.pool.pool_id = "fresh".into();
    assert!(
        crate::eval::calendar::check(&cfg(), &serde_json::to_vec(&fresh).unwrap()).is_err(),
        "the template's own pool is exposed stock"
    );
}

#[test]
fn the_memory_model_bounds_the_template_roster() {
    // At n = 1,000 with the 1,024-document probe and 200 audit humans, the
    // package step fits 64 entrants and refuses 65 (arena review, 2026-11-01).
    let mut p = admission_plan(1, 40);
    p.n = 1000;
    p.reward_v1.as_mut().unwrap().probes[0].size = 1024;
    p.reward_v1.as_mut().unwrap().canary.draw_humans = 200;
    p.entrants = (0..64)
        .map(|n| {
            let mut a = p.reference.clone();
            a.package_sha256 = hex::encode([n as u8; 32]);
            a
        })
        .collect();
    assert!(crate::eval::calendar::fits(&p));
    p.entrants.push(p.reference.clone());
    assert!(!crate::eval::calendar::fits(&p));
}

#[test]
fn the_gate_policy_binds_the_incentive_rule_only_when_declared() {
    let p = admission_plan(1, 2);
    let v = p.reward_v1.unwrap();
    let j = crate::eval::v3::gate_policy_json(&v);
    assert_eq!(j["incentive"]["rule"], plan::INCENTIVE_REFERENCE_MARGIN);
    assert_eq!(j["incentive"]["provisional_gates"], true);
    let mut old = v;
    old.incentive = None;
    assert!(
        crate::eval::v3::gate_policy_json(&old).get("incentive").is_none(),
        "earlier plans keep their policy digest"
    );
}

#[test]
fn the_ledger_records_probe_charges_at_plan_rates() {
    let p = admission_plan(1, 2);
    let m = &p.fanout.as_ref().unwrap().cost;
    let per_s = m.instance_nanousd_per_s + m.vcpu_nanousd_per_s * u64::from(m.vcpus) + m.lane_do_nanousd_per_s;
    assert_eq!(crate::eval::cost::probe_charge_microusd(m, 1000), per_s.div_ceil(1000));
    assert_eq!(crate::eval::cost::probe_charge_microusd(m, 0), 0);
}

#[test]
fn a_new_scorer_build_admits_verified_sessions_again_before_the_freeze() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    upload_all(&db, &m);
    frame_ok(&db, &m);
    admit_all(&db);
    let cutoff = OPEN_BLOCK + 360;
    close_at(&db, cutoff);
    snapshot(
        &db,
        cutoff,
        &m,
        Some(OPEN_BLOCK + 1),
        &m.upload.opening.chain_payload(),
        Some(m.uid),
        Some(m.registration_block),
    );
    let new_build = EvalConfig {
        scorer_binary: "bb".repeat(32),
        ..cfg()
    };
    assert!(matches!(
        tx(&db, |s| try_freeze(s, &new_build, NOW)).unwrap(),
        Some(Boundary::Waiting(_))
    ));
    assert_eq!(state_of(&db, &m.session()).0, "verifying");
    let env2 = AdmissionEnv {
        scorer_binary: "bb".repeat(32),
        ..env()
    };
    let runs = tx(&db, |s| claim_runs(s, &env2, 8, NOW)).unwrap();
    assert_eq!(runs.len(), 1);
    tx(&db, |s| record_run(s, &env2, &runs[0], &LaneOutcome::Admitted, 3, NOW)).unwrap();
    assert_eq!(
        tx(&db, |s| try_freeze(s, &new_build, NOW)).unwrap(),
        Some(Boundary::Frozen {
            admitted: 1,
            overflow: 0,
            outside: true
        })
    );
}

// ------------------------------------------- review round 1 regressions

fn verdict_of(db: &Sqlite, n: u8) -> (String, String) {
    tx(db, |s| {
        let r = s.exec(
            "SELECT state, scorer_binary FROM adm_verdicts WHERE hotkey = ?",
            &[SqlValue::Text(hex::encode(account(n).0))],
        )?;
        Ok::<_, EvalError>((text(&r[0], 0)?.to_owned(), text(&r[0], 1)?.to_owned()))
    })
    .unwrap()
}

#[test]
fn a_verdict_reached_on_another_build_is_run_again_even_when_another_claim_rebinds() {
    let db = db_with_open_cohort(4);
    let a = miner(1, 3, 1, 0x11);
    a.commit(&db, OPEN_BLOCK + 1, 0);
    a.create(&db);
    upload_all(&db, &a);
    frame_ok(&db, &a);
    admit_all(&db);
    assert_eq!(state_of(&db, &a.session()).0, "verified");
    // The build changes; miner B's claim re-binds A's verdict too, and A's
    // session waits on it again (it no longer reads as verified).
    let env2 = AdmissionEnv {
        scorer_binary: "bb".repeat(32),
        ..env()
    };
    let b = miner(2, 4, 1, 0x12);
    b.commit(&db, OPEN_BLOCK + 2, 0);
    b.create(&db);
    upload_all(&db, &b);
    frame_ok(&db, &b);
    let runs = tx(&db, |s| claim_runs(s, &env2, 8, NOW)).unwrap();
    assert_eq!(runs.len(), 2, "A runs again on the new build beside B");
    assert_eq!(verdict_of(&db, 1), ("pending".into(), "bb".repeat(32)));
    assert_eq!(state_of(&db, &a.session()).0, "verifying");
    let cutoff = OPEN_BLOCK + 360;
    close_at(&db, cutoff);
    for (m, blk) in [(&a, OPEN_BLOCK + 1), (&b, OPEN_BLOCK + 2)] {
        snapshot(
            &db,
            cutoff,
            m,
            Some(blk),
            &m.upload.opening.chain_payload(),
            Some(m.uid),
            Some(m.registration_block),
        );
    }
    let new_build = EvalConfig {
        scorer_binary: "bb".repeat(32),
        ..cfg()
    };
    // Only B's run done: the freeze waits for A's.
    let rb = runs.iter().find(|r| r.hotkey == hex::encode(account(2).0)).unwrap();
    tx(&db, |s| record_run(s, &env2, rb, &LaneOutcome::Admitted, 3, NOW)).unwrap();
    assert!(matches!(
        tx(&db, |s| try_freeze(s, &new_build, NOW)).unwrap(),
        Some(Boundary::Waiting(_))
    ));
    let ra = runs.iter().find(|r| r.hotkey == hex::encode(account(1).0)).unwrap();
    tx(&db, |s| record_run(s, &env2, ra, &LaneOutcome::Admitted, 3, NOW)).unwrap();
    assert_eq!(
        tx(&db, |s| try_freeze(s, &new_build, NOW)).unwrap(),
        Some(Boundary::Frozen {
            admitted: 2,
            overflow: 0,
            outside: true
        })
    );
}

#[test]
fn a_lost_multipart_completion_is_resumed_and_an_operator_can_refuse_a_stalled_session() {
    let db = db_with_open_cohort(4);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    let id = m.session();
    tx(&db, |s| set_upload_id(s, &id, 1, "upload-1")).unwrap();
    let w = tx(&db, window).unwrap().unwrap();
    let plan = w.part_plan(m.upload.artifact_len).unwrap();
    for n in 1..=plan.count() {
        let (a0, len) = plan.range(n).unwrap();
        let part = m.obj.get(a0 as usize..(a0 + len) as usize).unwrap();
        let mut req = m.signed(&db, "PUT", &format!("/admission/v1/sessions/{id}/parts/{n}"), part);
        req.body_sha256 = None;
        let t = tx(&db, |s| {
            Ok::<_, EvalError>(part_begin(s, &chain(), &req, &id, n, Some(len)))
        })
        .unwrap()
        .unwrap();
        tx(&db, |s| {
            Ok::<_, EvalError>(part_record(s, &id, &t, &format!("etag-{n}")))
        })
        .unwrap();
    }
    let req = m.signed(&db, "POST", &format!("/admission/v1/sessions/{id}/complete"), &[]);
    let (r, fx) = tx(&db, |s| Ok::<_, EvalError>(complete(s, &chain(), &req, &id, NOW))).unwrap();
    assert_eq!(r.status, 202);
    assert!(fx.complete.is_some());
    // The object is evicted before R2's outcome is recorded: the completion
    // is durable work, and a retried request issues it again.
    assert_eq!(
        tx(&db, |s| verify_due(s, NOW)).unwrap(),
        Some((id.clone(), "complete".into()))
    );
    assert!(matches!(
        tx(&db, |s| next_work(s, &cfg(), NOW)).unwrap(),
        Some(engine::Work::AdmissionVerify { stage, .. }) if stage == "complete"
    ));
    let (r, fx) = tx(&db, |s| Ok::<_, EvalError>(complete(s, &chain(), &req, &id, NOW))).unwrap();
    assert_eq!(
        (r.status, fx.complete.as_ref().map(|c| c.2.len())),
        (202, Some(plan.count() as usize))
    );
    // A transport failure is retried after a backoff, never a refusal.
    let t = tx(&db, |s| completion_begin(s, &id, NOW)).unwrap();
    assert!(t.is_some());
    tx(&db, |s| step_retry(s, &id, false, NOW)).unwrap();
    assert!(tx(&db, |s| verify_due(s, NOW)).unwrap().is_none(), "backing off");
    assert!(tx(&db, |s| verify_due(s, NOW + backoff_ms(1))).unwrap().is_some());
    assert_eq!(state_of(&db, &id).0, "verifying");
    // R2's definitive refusal ends it; a found object completes it.
    assert!(completion_refused(
        "completeMultipartUpload: One or more parts not found (10025)"
    ));
    assert!(completion_refused("NoSuchUpload: the upload does not exist"));
    assert!(!completion_refused("Network connection lost."));
    assert!(!completion_refused("internal error (10001)"));
    tx(&db, |s| completed(s, &id, NOW)).unwrap();
    assert_eq!(
        tx(&db, |s| verify_due(s, NOW)).unwrap(),
        Some((id.clone(), "frame".into()))
    );
    // A session stalled in any verification stage: the operator refuses it
    // with a recorded reason, and the freeze no longer waits on it.
    assert!(operator_refuse_reason_is_checked(&db, &id));
    assert!(tx(&db, |s| operator_refuse(s, &id, "stalled-completion", NOW)).unwrap());
    let row = tx(&db, |s| session_row(s, &id)).unwrap().unwrap();
    assert_eq!(
        row.refusal.unwrap().detail.check.as_deref(),
        Some("operator:stalled-completion")
    );
    assert!(
        !tx(&db, |s| operator_refuse(s, &id, "again", NOW)).unwrap(),
        "already ended"
    );
    let cutoff = OPEN_BLOCK + 360;
    close_at(&db, cutoff);
    assert_eq!(
        tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap(),
        Some(Boundary::Frozen {
            admitted: 0,
            overflow: 0,
            outside: false
        })
    );
}

fn operator_refuse_reason_is_checked(db: &Sqlite, id: &str) -> bool {
    tx(db, |s| operator_refuse(s, id, "Not A Code", NOW)).is_err()
}

#[test]
fn framing_attempts_that_never_finish_refuse_the_upload() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    upload_all(&db, &m);
    let id = m.session();
    // Each begun attempt counts until an outcome is recorded; the plan's
    // attributable-failure bound (3 here) then refuses the upload.
    for _ in 0..3 {
        assert!(tx(&db, |s| frame_begin(s, &id, NOW)).unwrap().is_some());
    }
    // A failed read is the platform's: not counted.
    tx(&db, |s| step_retry(s, &id, true, NOW)).unwrap();
    assert!(tx(&db, |s| frame_begin(s, &id, NOW + LANE_LEASE_MS)).unwrap().is_some());
    assert!(tx(&db, |s| frame_begin(s, &id, NOW + LANE_LEASE_MS)).unwrap().is_none());
    let row = tx(&db, |s| session_row(s, &id)).unwrap().unwrap();
    assert_eq!(row.state, SessionState::Refused);
    assert_eq!(row.refusal.unwrap().code, "artifact_invalid");
}

#[test]
fn a_zero_header_length_is_refused_by_the_one_reader_framing_uses() {
    use std::sync::mpsc;
    use std::time::Duration;
    let (tx1, rx1) = mpsc::channel();
    std::thread::spawn(move || {
        let mut f = Framer::new();
        f.update(&[0u8, 0, 0, 0, 1, 2, 3]);
        f.update(&[4, 5]);
        let _ = tx1.send(f.finish(Version::V2).2.err());
    });
    let got = rx1.recv_timeout(Duration::from_secs(10)).expect("framing returns");
    assert!(
        matches!(&got, Some(FrameError::Reader(r)) if r.contains("artifact header length")),
        "{got:?}"
    );
}

#[test]
fn a_header_listing_more_files_than_the_spec_is_refused_before_slicing() {
    // The test object's six files and one more than the spec's file table.
    let files: Vec<(&str, FileRole, Vec<u8>)> = vec![
        ("a-fp32band.onnx", FileRole::Graph, vec![1; 300]),
        ("calibration.json", FileRole::Calibration, b"{1}".to_vec()),
        ("contract.json", FileRole::OutputSchema, b"c1".to_vec()),
        ("extra.json", FileRole::OwnerMetadata, b"e1".to_vec()),
        ("heads.json", FileRole::Weights, b"h1".to_vec()),
        ("manifest.json", FileRole::OwnerMetadata, b"m1".to_vec()),
        ("tokenizer.json", FileRole::Tokenizer, b"t1".to_vec()),
    ];
    assert_eq!(files.len(), spec::ROLES.len() + 1);
    let refs: Vec<(&str, FileRole, &[u8])> = files.iter().map(|(n, r, b)| (*n, *r, b.as_slice())).collect();
    let obj = artifact_object(spec::CONTRACT_ID, &refs).unwrap();
    let mut f = Framer::new();
    f.update(&obj);
    assert!(f.slices.is_empty(), "nothing sliced");
    assert!(matches!(f.finish(Version::V2).2, Err(FrameError::Check(c)) if c == "undeclared_file"));
}

#[test]
fn commitments_at_one_position_are_refused_and_a_refused_plan_faults_the_cohort() {
    let db = db_with_open_cohort(4);
    // Two hotkeys whose commitments one extrinsic made (a batch): no order.
    let a = miner(1, 3, 1, 0x11);
    let b = miner(2, 4, 1, 0x12);
    let c = miner(3, 5, 1, 0x13);
    for (m, x) in [(&a, 5), (&b, 5), (&c, 6)] {
        m.commit(&db, OPEN_BLOCK + 1, x);
        m.create(&db);
        upload_all(&db, m);
        frame_ok(&db, m);
    }
    admit_all(&db);
    let cutoff = OPEN_BLOCK + 360;
    close_at(&db, cutoff);
    for m in [&a, &b, &c] {
        snapshot(
            &db,
            cutoff,
            m,
            Some(OPEN_BLOCK + 1),
            &m.upload.opening.chain_payload(),
            Some(m.uid),
            Some(m.registration_block),
        );
    }
    let r = tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap();
    assert_eq!(
        r,
        Some(Boundary::Frozen {
            admitted: 1,
            overflow: 0,
            outside: true
        })
    );
    for m in [&a, &b] {
        let row = tx(&db, |s| session_row(s, &m.session())).unwrap().unwrap();
        let refusal = row.refusal.unwrap();
        assert_eq!(
            (refusal.code.as_str(), refusal.detail.reason.as_deref()),
            ("commitment_mismatch", Some(commitments::SHARED_EXTRINSIC))
        );
    }
    // A frozen plan the engine refuses faults the cohort, never every step.
    let db = db_with_open_cohort(4);
    let d = miner(4, 6, 1, 0x14);
    d.commit(&db, OPEN_BLOCK + 1, 0);
    d.create(&db);
    upload_all(&db, &d);
    frame_ok(&db, &d);
    admit_all(&db);
    tx(&db, |s| {
        s.exec(
            "UPDATE adm_sessions SET object_key = '../outside' WHERE session = ?",
            &[SqlValue::Text(d.session())],
        )
        .map(|_| ())
    })
    .unwrap();
    close_at(&db, cutoff);
    snapshot(
        &db,
        cutoff,
        &d,
        Some(OPEN_BLOCK + 1),
        &d.upload.opening.chain_payload(),
        Some(d.uid),
        Some(d.registration_block),
    );
    assert!(matches!(
        tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap(),
        Some(Boundary::Waiting(_))
    ));
    let co = tx(&db, |s| engine::cohort(s, 1)).unwrap();
    assert_eq!(
        (co.phase.as_str(), co.fault.as_deref()),
        ("admission", Some(FREEZE_FAULT))
    );
    assert!(tx(&db, |s| next_work(s, &cfg(), NOW)).is_ok(), "the step goes on");
    // The operator abandons it: its live sessions end and are cleaned up.
    assert!(tx(&db, |s| engine::abandon(s, &co.label, "freeze-drill", NOW)).unwrap());
    assert_eq!(tx(&db, |s| engine::cohort(s, 1)).unwrap().phase, "abandoned");
    assert_eq!(state_of(&db, &d.session()).0, "refused");
    assert_eq!(tx(&db, |s| cleanup_due(s, 10)).unwrap().len(), 1);
}

#[test]
fn the_freeze_waits_for_a_tempo_boundary_once_its_work_is_in_late() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    upload_all(&db, &m);
    frame_ok(&db, &m);
    let cutoff = OPEN_BLOCK + 360;
    close_at(&db, cutoff);
    snapshot(
        &db,
        cutoff,
        &m,
        Some(OPEN_BLOCK + 1),
        &m.upload.opening.chain_payload(),
        Some(m.uid),
        Some(m.registration_block),
    );
    // The verdict comes in after the close's epoch: ready at epoch 13, the
    // roster freezes at the next boundary (14), not mid-tempo.
    epoch(&db, 13, cutoff + 360, NOW + 3);
    assert!(matches!(
        tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap(),
        Some(Boundary::Waiting(_))
    ));
    admit_all(&db);
    assert!(matches!(
        tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap(),
        Some(Boundary::Waiting(w)) if w.contains("next tempo boundary")
    ));
    epoch(&db, 14, cutoff + 720, NOW + 4);
    assert!(matches!(
        tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap(),
        Some(Boundary::Frozen { .. })
    ));
    let c = tx(&db, |s| engine::cohort(s, 1)).unwrap();
    let closed = tx(&db, |s| {
        let r = s.exec("SELECT admission_closed_epoch FROM cohorts WHERE seq = 1", &[])?;
        Ok::<_, EvalError>(int(&r[0], 0)?)
    })
    .unwrap();
    assert_eq!((c.phase.as_str(), closed), ("generating", 14));
}

#[test]
fn copies_of_published_artifacts_and_plans_without_operator_hotkeys_never_unlock_fresh_stock() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    // Its content is already published.
    tx(&db, |s| {
        s.exec(
            "INSERT INTO published_contents (content, seq) VALUES (?, 0)",
            &[SqlValue::Text(m.upload.opening.content.hex())],
        )
        .map(|_| ())
    })
    .unwrap();
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    upload_all(&db, &m);
    frame_ok(&db, &m);
    admit_all(&db);
    let cutoff = OPEN_BLOCK + 360;
    close_at(&db, cutoff);
    snapshot(
        &db,
        cutoff,
        &m,
        Some(OPEN_BLOCK + 1),
        &m.upload.opening.chain_payload(),
        Some(m.uid),
        Some(m.registration_block),
    );
    assert_eq!(
        tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap(),
        Some(Boundary::Frozen {
            admitted: 1,
            overflow: 0,
            outside: false
        })
    );
    assert_eq!(tx(&db, |s| engine::cohort(s, 1)).unwrap().plan.pool.pool_id, "p");
    // A plan with a fresh pool must name the operator's hotkeys.
    let mut p = admission_plan(1, 2);
    p.admission.as_mut().unwrap().operator_hotkeys.clear();
    assert!(plan::CohortPlan::parse(&serde_json::to_vec(&p).unwrap(), cfg().ceilings).is_err());
    p.admission.as_mut().unwrap().fresh_pool = None;
    plan::CohortPlan::parse(&serde_json::to_vec(&p).unwrap(), cfg().ceilings).unwrap();
    // Admission specs other than v2 are refused until runs name the spec.
    let mut v3p = admission_plan(1, 2);
    let b3 = SpecBounds::of(Version::V3).unwrap();
    let a = v3p.admission.as_mut().unwrap();
    a.artifact_spec = b3.spec_id.into();
    a.artifact_spec_sha256 = b3.spec_sha256.clone();
    v3p.fanout.as_mut().unwrap().score.artifact_spec = Some(b3.spec_id.into());
    let e = plan::CohortPlan::parse(&serde_json::to_vec(&v3p).unwrap(), cfg().ceilings).unwrap_err();
    assert!(e.0.starts_with("admission_spec_unsupported"), "{}", e.0);
}

#[test]
fn one_session_per_commitment_and_a_retried_creation_answers_before_the_checks() {
    let db = db_with_open_cohort(2);
    let a = miner(1, 3, 1, 0x11);
    a.commit(&db, OPEN_BLOCK + 1, 0);
    assert_eq!(a.create(&db).0.status, 201);
    let id = a.session();
    let req = a.signed(&db, "DELETE", &format!("/admission/v1/sessions/{id}/attempts/1"), &[]);
    tx(&db, |s| Ok::<_, EvalError>(abort(s, &chain(), &req, &id, 1, NOW))).unwrap();
    // Another envelope for the aborted session's commitment: refused, the
    // miner reopens or commits again (sessions stay bounded by commitments).
    let mut b = miner(1, 3, 1, 0x11);
    b.upload.artifact_len += 1;
    let (r, _) = b.create(&db);
    assert_eq!(code(&r), "second_artifact");
    // After a newer commitment, the earlier envelope still answers 200.
    let c = miner(1, 3, 1, 0x31);
    c.commit(&db, OPEN_BLOCK + 2, 0);
    let (r, _) = a.create(&db);
    assert_eq!((r.status, r.body["session"].as_str()), (200, Some(id.as_str())));
}

#[test]
fn verify_due_waits_while_the_hotkeys_commitment_is_being_logged() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    upload_all(&db, &m);
    tx(&db, |s| {
        s.exec(
            "INSERT INTO commit_seen (hotkey, block, payload, seen_block, stage) VALUES (?, ?, '00', 1, 'events')",
            &[
                SqlValue::Text(hex::encode(account(1).0)),
                SqlValue::Integer((OPEN_BLOCK + 5) as i64),
            ],
        )
        .map(|_| ())
    })
    .unwrap();
    assert!(
        tx(&db, |s| verify_due(s, NOW)).unwrap().is_none(),
        "framing waits, never refuses"
    );
    // Framed meanwhile: a retryable not-found leaves the stage, no refusal.
    frame_ok(&db, &m);
    assert_eq!(state_of(&db, &m.session()).0, "verifying");
}

#[test]
fn cleanup_deletes_only_the_blobs_a_refused_session_created() {
    let db = db_with_open_cohort(2);
    let (a, b) = (miner(1, 3, 1, 0x11), miner(2, 4, 1, 0x12));
    for (m, x) in [(&a, 0), (&b, 1)] {
        m.commit(&db, OPEN_BLOCK + 1, x);
        m.create(&db);
        upload_all(&db, m);
    }
    // A created blobs x and y and its manifest; B reuses y.
    tx(&db, |s| blob_created(s, &a.session(), "x")).unwrap();
    tx(&db, |s| blob_created(s, &a.session(), "y")).unwrap();
    tx(&db, |s| manifest_created(s, &a.session())).unwrap();
    tx(&db, |s| {
        s.exec(
            "INSERT INTO adm_blobs (session, sha256, created) VALUES (?, 'y', 0)",
            &[SqlValue::Text(b.session())],
        )
        .map(|_| ())
    })
    .unwrap();
    tx(&db, |s| {
        s.exec(
            "UPDATE adm_sessions SET manifest_key = 'artifacts/packages/a.json', package_sha256 = 'a' WHERE session = ?",
            &[SqlValue::Text(a.session())],
        )
        .map(|_| ())
    })
    .unwrap();
    let keys = tx(&db, |s| cleanup_keys(s, &a.session())).unwrap();
    assert_eq!(
        keys,
        vec!["artifacts/blobs/x".to_owned(), "artifacts/packages/a.json".to_owned()]
    );
    // Once B is cleaned up too, y goes with whichever comes last.
    tx(&db, |s| mark_cleaned(s, &b.session())).unwrap();
    let keys = tx(&db, |s| cleanup_keys(s, &a.session())).unwrap();
    assert!(keys.contains(&"artifacts/blobs/y".to_owned()));
}

#[test]
fn the_hotkey_view_stays_within_the_response_limit() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    // Many ended sessions (written directly): the view keeps the newest that fit.
    let row = tx(&db, |s| {
        let r = s.exec(
            "SELECT upload FROM adm_sessions WHERE session = ?",
            &[SqlValue::Text(m.session())],
        )?;
        Ok::<_, EvalError>(text(&r[0], 0)?.to_owned())
    })
    .unwrap();
    for n in 0..800u32 {
        tx(&db, |s| {
            s.exec(
                "INSERT INTO adm_sessions (session, seq, cohort, hotkey, upload, commitment, content, artifact_sha256, artifact_len,
                    part_size, part_count, attempt, state, refusal, object_key, created_ms, updated_ms)
                 VALUES (?, 1, 1, ?, ?, 'c', 'c', 'a', 1, 1, 1, 1, 'aborted', NULL, 'k', ?, ?)",
                &[
                    SqlValue::Text(format!("{n:064x}")),
                    SqlValue::Text(hex::encode(account(1).0)),
                    SqlValue::Text(row.clone()),
                    SqlValue::Integer(i64::from(n)),
                    SqlValue::Integer(i64::from(n)),
                ],
            )
            .map(|_| ())
        })
        .unwrap();
    }
    let path = format!(
        "/admission/v1/cohorts/1/hotkeys/{}",
        crate::chain::ss58(key(1).public())
    );
    let req = m.signed(&db, "GET", &path, &[]);
    let hk = crate::chain::ss58(key(1).public());
    let r = tx(&db, |s| Ok::<_, EvalError>(hotkey_view(s, &chain(), &req, 1, &hk))).unwrap();
    let limit = slop_ninja_admission_wire::response_limit(Some(64));
    assert!(serde_json::to_vec(&r.body).unwrap().len() <= limit);
    let sessions = r.body["sessions"].as_array().unwrap();
    assert!(
        sessions.iter().any(|x| x["state"] == "uploading"),
        "the live session stays"
    );
    assert!(sessions.len() < 801);
}

#[test]
fn the_calendar_follows_a_pending_promotion_and_the_chain_not_the_template() {
    use crate::eval::v3::{ArtifactDigests, H, V3State};
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let t = crate::eval::calendar::Template {
        schema: crate::eval::calendar::TEMPLATE_SCHEMA.into(),
        label_prefix: "testnet-cohort".into(),
        plan: admission_plan(0, 40),
    };
    tx(&db, |s| {
        crate::eval::calendar::install(s, &cfg(), &serde_json::to_vec(&t).unwrap(), NOW)
    })
    .unwrap();
    let (_, plan1) = tx(&db, |s| crate::eval::calendar::due_any(s, 0)).unwrap().unwrap();
    tx(&db, |s| engine::queue(s, &cfg(), &plan1, NOW)).unwrap();
    // Cohort 1 promoted its one entrant at packaging and is in transition.
    let entrant = plan::tests::artifact(plan::tests::identity(3, 3), 3);
    let mut p1: plan::CohortPlan = serde_json::from_slice(&plan1).unwrap();
    p1.entrants = vec![entrant.clone()];
    let dg = |b: u8| ArtifactDigests {
        content: H(Digest([b; 32])),
        artifact: H(Digest([b ^ 0xff; 32])),
    };
    let st = V3State {
        reference: Some(dg(0x01)),
        entrants: vec![dg(0x02)],
        promoted: Some(0),
        ..Default::default()
    };
    tx(&db, |s| {
        s.exec(
            "UPDATE cohorts SET plan = ?, phase = 'transition', v3 = ? WHERE seq = 1",
            &[
                SqlValue::Text(serde_json::to_string(&p1).unwrap()),
                SqlValue::Text(serde_json::to_string(&st).unwrap()),
            ],
        )
        .map(|_| ())
    })
    .unwrap();
    let (_, next) = tx(&db, |s| crate::eval::calendar::due_any(s, 0)).unwrap().unwrap();
    let next: plan::CohortPlan = serde_json::from_slice(&next).unwrap();
    assert_eq!(
        next.reference.package_sha256, entrant.package_sha256,
        "queued during the transition: R_k+1"
    );
    assert_eq!(
        next.previous_reference.as_ref().map(|p| p.package_sha256.clone()),
        Some(t.plan.reference.package_sha256.clone())
    );
    // Closed: the chain (as `close` leaves it) is followed, and a reinstall
    // of the template never resets it.
    tx(&db, |s| {
        s.exec("UPDATE cohorts SET phase = 'closed' WHERE seq = 1", &[])?;
        s.exec(
            "INSERT INTO reference_state (id, k, current, predecessor) VALUES (1, 1, ?, ?)",
            &[
                SqlValue::Text(Digest([2; 32]).to_string()),
                SqlValue::Text(Digest([1; 32]).to_string()),
            ],
        )
        .map(|_| ())
    })
    .unwrap();
    let v = tx(&db, |s| {
        crate::eval::calendar::install(s, &cfg(), &serde_json::to_vec(&t).unwrap(), NOW)
    })
    .unwrap();
    assert_eq!(v["next_references"]["source"], "chain");
    let (_, after) = tx(&db, |s| crate::eval::calendar::due_any(s, 0)).unwrap().unwrap();
    let after: plan::CohortPlan = serde_json::from_slice(&after).unwrap();
    assert_eq!(after.reference.package_sha256, entrant.package_sha256);
    assert!(after.reference.opening_nonce.is_none() && after.reference.object_key.is_none());
    // A chain naming nothing stored pauses the calendar with the reason.
    tx(&db, |s| {
        s.exec(
            "UPDATE reference_state SET current = ? WHERE id = 1",
            &[SqlValue::Text(Digest([9; 32]).to_string())],
        )
        .map(|_| ())
    })
    .unwrap();
    assert!(tx(&db, |s| crate::eval::calendar::due_any(s, 0)).unwrap().is_none());
    let view = tx(&db, crate::eval::calendar::view).unwrap();
    assert_eq!(view["template"]["paused"], true);
    assert!(
        view["template"]["last_error"]
            .as_str()
            .unwrap()
            .starts_with("template_reference_unresolved")
    );
}

#[test]
fn the_template_runs_the_testnet_plan_defaults() {
    let t = crate::eval::calendar::Template {
        schema: crate::eval::calendar::TEMPLATE_SCHEMA.into(),
        label_prefix: "testnet-cohort".into(),
        plan: admission_plan(0, 4),
    };
    crate::eval::calendar::check(&cfg(), &serde_json::to_vec(&t).unwrap()).unwrap();
    let mut wide = t.clone();
    wide.plan.fanout.as_mut().unwrap().score.lane_docs = wide.plan.n - 1;
    assert!(
        crate::eval::calendar::check(&cfg(), &serde_json::to_vec(&wide).unwrap()).is_err(),
        "W = 1"
    );
    let mut pre = t.clone();
    pre.plan.fanout.as_mut().unwrap().score.prestart_lead_ms = 1000;
    assert!(
        crate::eval::calendar::check(&cfg(), &serde_json::to_vec(&pre).unwrap()).is_err(),
        "no pre-start"
    );
    let mut bare = t.clone();
    bare.plan.reward_v1.as_mut().unwrap().incentive = None;
    assert!(
        crate::eval::calendar::check(&cfg(), &serde_json::to_vec(&bare).unwrap()).is_err(),
        "the D11 rule"
    );
    let mut final_gates = t;
    final_gates
        .plan
        .reward_v1
        .as_mut()
        .unwrap()
        .incentive
        .as_mut()
        .unwrap()
        .provisional_gates = false;
    assert!(crate::eval::calendar::check(&cfg(), &serde_json::to_vec(&final_gates).unwrap()).is_err());
}

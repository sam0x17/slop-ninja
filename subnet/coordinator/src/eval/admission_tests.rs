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
use slop_ninja_protocol::{FileRole, MAX_ARTIFACT_HEADER_LEN, Nonce, TESTNET_GENESIS, VerifiedArtifact, artifact_object};

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
        operator_coldkeys: vec![format!("0x{}", hex::encode([0xc0u8; 32]))],
        fresh_pool: Some(PoolRef {
            pool_id: "fresh".into(),
            manifest_key: "pools/fresh/manifest.json".into(),
            manifest_sha256: hex::encode([5; 32]),
        }),
    }
}

/// A standing template's plan: admission without a fresh pool (standing
/// templates never unlock fresh stock).
fn template_plan(cohort: u64, cap: u16) -> plan::CohortPlan {
    let mut p = admission_plan(cohort, cap);
    p.admission.as_mut().unwrap().fresh_pool = None;
    p
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
        task_order: Some(plan::TASK_ORDER_SEEDED_RANK.into()),
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
        head_epoch: Some(index),
    };
    tx(db, |s| engine::observe(s, &kick, now)).unwrap();
    // The commitment reader keeps up with the cursor (one poll per block).
    polled(db, block.number);
}

/// The per-block heartbeat: advances the finalized cursor by `blocks`, with
/// no commitment poll (production polls on the minute cron tick).
fn heartbeat(db: &Sqlite, blocks: u64) {
    let number = tx(db, cursor).unwrap().unwrap_or(0) + blocks;
    let kick = Kick {
        confirmed: Some(BlockRef { number, hash: Id([0xbe; 32]) }),
        ..Kick::default()
    };
    tx(db, |s| engine::observe(s, &kick, NOW)).unwrap();
}

/// Records the commitment reader's last completed poll at `block`.
fn polled(db: &Sqlite, block: u64) {
    tx(db, |s| s.exec("UPDATE commit_reader SET polled_block = ? WHERE id = 1", &[bind_u64(block)?])).unwrap();
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
        self.create_hinted(db, None)
    }

    /// Session creation carrying `x-sn-commitment-block` (`hint`).
    fn create_hinted(&self, db: &Sqlite, hint: Option<u64>) -> (Reply, Effects) {
        let body = self.upload.to_bytes().unwrap();
        let mut r = self.signed(db, "POST", "/admission/v1/sessions", &body);
        r.body = Some(hex::encode(&body));
        r.commitment_block = hint;
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
        commitment_block: None,
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
            Ok::<_, EvalError>(part_record(s, &chain(), &req, &id, &t, &format!("etag-{n}")))
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
    frame_only(db, m);
    tx(db, |s| blobs_written(s, &env(), &m.session(), NOW)).unwrap();
}

/// The framing stage's outcome for a well-formed object (no blob step).
fn frame_only(db: &Sqlite, m: &Miner) {
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
            (m.upload.artifact_sha256, m.upload.artifact_len, Ok(framed)).into(),
            NOW,
        )
    })
    .unwrap();
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
            "INSERT OR REPLACE INTO adm_snapshot (cutoff_block, hotkey, block, payload, uid, registration_block, done, coldkey)
             VALUES (?, ?, ?, ?, ?, ?, 1, ?)",
            &[
                bind_u64(cutoff)?,
                SqlValue::Text(hex::encode(account(m.n).0)),
                block.map_or(SqlValue::Null, |b| SqlValue::Integer(b as i64)),
                SqlValue::Text(hex::encode(payload)),
                uid.map_or(SqlValue::Null, |u| SqlValue::Integer(i64::from(u))),
                reg.map_or(SqlValue::Null, |b| SqlValue::Integer(b as i64)),
                // Each miner's own coldkey (never the operator's 0xc0..).
                SqlValue::Text(hex::encode([m.n; 32])),
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
    // A commitment before the cohort opened (the CLI names its confirmed
    // block, which the reader's last poll already holds: final at once).
    m.commit(&db, OPEN_BLOCK - 1, 0);
    assert_eq!(code(&m.create_hinted(&db, Some(OPEN_BLOCK - 1)).0), "commitment_before_open");
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
    // From here the reader has polled past every commitment this test logs,
    // and each request names the block its CLI confirmed (a block that poll
    // holds), so each code is final at once.
    polled(&db, OPEN_BLOCK + 100);
    // The opening does not open the hotkey's current commitment.
    let stale = miner(1, 3, 1, 0x21);
    let (r, _) = stale.create_hinted(&db, Some(OPEN_BLOCK + 5));
    assert_eq!(
        (code(&r).as_str(), r.body["error"]["detail"]["payload_cohort"].as_u64()),
        ("commitment_mismatch", Some(1))
    );
    // Another registration generation.
    let mut regen = miner(1, 3, 1, 0x11);
    regen.upload.opening.registration_block = 1;
    regen.upload = SubmissionUpload::new(regen.upload.opening.clone(), &regen.obj);
    regen.commit(&db, OPEN_BLOCK + 7, 0);
    assert_eq!(code(&regen.create_hinted(&db, Some(OPEN_BLOCK + 7)).0), "registration_mismatch");
    // Not registered at its commitment.
    let unreg = miner(4, 6, 1, 0x14);
    let e = LogEntry {
        uid: None,
        registration_block: None,
        ..log(&unreg, OPEN_BLOCK + 8, 0)
    };
    tx(&db, |s| commitments::record(s, &e, NOW)).unwrap();
    assert_eq!(code(&unreg.create_hinted(&db, Some(OPEN_BLOCK + 8)).0), "not_registered");
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
    // Third W3 review, finding 6: the aborted attempt's object stays
    // recorded until a clean-up step confirms its delete (the session's own
    // clean-up names only a2 from now on).
    let due = tx(&db, |s| object_deletes_due(s, 8, NOW)).unwrap();
    assert_eq!(due.len(), 1);
    assert!(due[0].0.ends_with(&format!("/{id}/a1.object")));
    assert!(matches!(
        tx(&db, |s| next_work(s, &cfg(), NOW)).unwrap(),
        Some(engine::Work::AdmissionCleanup)
    ));
    // Fix round 1, finding 5: a delete the bucket did not confirm backs off,
    // so the step reaches its other work (the calendar, the active cohort)
    // instead of choosing the clean-up again; it is due after the backoff.
    tx(&db, |s| object_delete_failed(s, &due[0].0, NOW)).unwrap();
    assert!(tx(&db, |s| object_deletes_due(s, 8, NOW)).unwrap().is_empty());
    assert!(!matches!(
        tx(&db, |s| next_work(s, &cfg(), NOW)).unwrap(),
        Some(engine::Work::AdmissionCleanup)
    ));
    let later = NOW + backoff_ms(1);
    assert_eq!(tx(&db, |s| object_deletes_due(s, 8, later)).unwrap().len(), 1);
    tx(&db, |s| object_delete_failed(s, &due[0].0, later)).unwrap();
    assert!(tx(&db, |s| object_deletes_due(s, 8, later + backoff_ms(1))).unwrap().is_empty(), "the backoff grows");
    tx(&db, |s| object_deleted(s, &due[0].0)).unwrap();
    assert!(tx(&db, |s| object_deletes_due(s, 8, crate::wire::MAX_SAFE_INTEGER)).unwrap().is_empty());
    // A newer commitment supersedes the live session.
    let c = miner(1, 3, 1, 0x31);
    c.commit(&db, OPEN_BLOCK + 2, 0);
    let (r, fx) = c.create(&db);
    assert_eq!(r.status, 201);
    assert_eq!(fx.abort.len(), 1);
    assert_eq!(state_of(&db, &id).0, "superseded");
}

/// Review round 1 of the upload Worker split, findings 2 and 3: the
/// key-holding side creates a multipart upload only for the authenticated
/// owner, keeps one upload per attempt when two creations race, and
/// re-derives every part record from the session instead of taking the
/// upload Worker's target as given.
#[test]
fn part_uploads_are_created_for_the_owner_only_and_records_are_rederived() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    let id = m.session();
    let path = format!("/admission/v1/sessions/{id}/parts/1");
    let req = m.signed(&db, "PUT", &path, &m.obj);
    // Another hotkey's signed request, an unsigned one, or a tampered
    // signature creates nothing; the owner's does.
    let other = miner(2, 4, 1, 0x12);
    let oreq = other.signed(&db, "PUT", &path, &m.obj);
    let missing = |r: &Signed| tx(&db, |s| Ok::<_, EvalError>(upload_missing_for(s, &chain(), r, &id))).unwrap();
    assert_eq!(missing(&oreq), None);
    assert_eq!(missing(&Signed::default()), None);
    let mut bad = req.clone();
    bad.path = format!("/admission/v1/sessions/{id}/parts/2");
    assert_eq!(missing(&bad), None);
    assert_eq!(missing(&req).map(|x| x.1), Some(1));
    // Two creations race: the first stored keeps the attempt's upload.
    assert!(tx(&db, |s| set_upload_id(s, &id, 1, "u-first")).unwrap());
    assert!(!tx(&db, |s| set_upload_id(s, &id, 1, "u-second")).unwrap());
    assert!(tx(&db, |s| set_upload_id(s, &id, 1, "u-first")).unwrap());
    assert_eq!(missing(&req), None, "created");
    let t = tx(&db, |s| Ok::<_, EvalError>(part_begin(s, &chain(), &req, &id, 1, Some(m.obj.len() as u64))))
        .unwrap()
        .unwrap();
    assert_eq!(t.upload_id, "u-first");
    // Every tampered target is refused; nothing is recorded.
    let record = |r: &Signed, t: &PartTarget| tx(&db, |s| Ok::<_, EvalError>(part_record(s, &chain(), r, &id, t, "e"))).unwrap();
    let tampered: [(&str, PartTarget); 6] = [
        ("key", PartTarget { key: "admission/x/y/a1.object".into(), ..t.clone() }),
        ("upload id", PartTarget { upload_id: "u-second".into(), ..t.clone() }),
        ("part", PartTarget { part: 2, ..t.clone() }),
        ("len", PartTarget { len: t.len - 1, ..t.clone() }),
        ("digest", PartTarget { sha256: "00".repeat(32), ..t.clone() }),
        ("attempt", PartTarget { attempt: 2, ..t.clone() }),
    ];
    for (what, bad_t) in &tampered {
        let r = record(&req, bad_t);
        assert!(r.status >= 400, "{what}: {:?}", r.body);
    }
    // Another hotkey's signature, or none, cannot record the owner's part.
    assert_eq!(code(&record(&oreq, &t)), "forbidden");
    assert_eq!(code(&record(&Signed::default(), &t)), "bad_signature");
    let rows = tx(&db, |s| s.exec("SELECT COUNT(*) FROM adm_parts", &[])).unwrap();
    assert_eq!(crate::sql::int(&rows[0], 0).unwrap(), 0);
    // The target part_begin issued records.
    assert_eq!(record(&req, &t).status, 200);
    // The pre-deploy check of a bucket change sees the session in progress.
    let st = tx(&db, status).unwrap();
    assert_eq!(st["objects"]["in_progress"], 1);
    assert_eq!(st["objects"]["uncleaned"], 1);
    assert_eq!(st["objects"]["pending_object_deletes"], 0);
}

/// Findings 2, 3 and 5: the forwarded route fields must be the ones the
/// signed method and path name.
#[test]
fn forwarded_fields_must_be_the_signed_requests_own() {
    use slop_ninja_admission_wire::internal::op;
    let sid = "ab".repeat(32);
    let f = |method: &str, path: String| Forward {
        signed: Signed { method: method.into(), path, ..Signed::default() },
        session: sid.clone(),
        part: 3,
        attempt: Some(2),
        cohort: 7,
        hotkey: "5Fhot".into(),
        ..Forward::default()
    };
    let ok = [
        (op::CREATE, f("POST", "/admission/v1/sessions".into())),
        (op::READ, f("GET", format!("/admission/v1/sessions/{sid}"))),
        (op::PART_BEGIN, f("PUT", format!("/admission/v1/sessions/{sid}/parts/3"))),
        (op::PART_RECORD, f("PUT", format!("/admission/v1/sessions/{sid}/parts/3"))),
        (op::COMPLETE, f("POST", format!("/admission/v1/sessions/{sid}/complete"))),
        (op::ABORT, f("DELETE", format!("/admission/v1/sessions/{sid}/attempts/2"))),
        (op::REOPEN, f("POST", format!("/admission/v1/sessions/{sid}/attempts/2"))),
        (op::HOTKEY, f("GET", "/admission/v1/cohorts/7/hotkeys/5Fhot".into())),
    ];
    for (o, fw) in &ok {
        assert!(forward_matches(o, fw), "{o}");
    }
    let other = "cd".repeat(32);
    let bad = [
        (op::READ, f("GET", format!("/admission/v1/sessions/{other}"))),
        (op::PART_BEGIN, f("PUT", format!("/admission/v1/sessions/{sid}/parts/4"))),
        (op::ABORT, f("DELETE", format!("/admission/v1/sessions/{sid}/attempts/1"))),
        (op::REOPEN, f("DELETE", format!("/admission/v1/sessions/{sid}/attempts/2"))),
        (op::HOTKEY, f("GET", "/admission/v1/cohorts/8/hotkeys/5Fhot".into())),
        (op::COMPLETE, f("POST", format!("/admission/v1/sessions/{sid}"))),
        (op::CREATE, f("POST", "/admission/v1/sessions?x".into())),
    ];
    for (o, fw) in &bad {
        assert!(!forward_matches(o, fw), "{o} {}", fw.signed.path);
    }
    assert_eq!(code(&forward_mismatch()), "bad_request");
}

/// Finding 23: the eval Worker's JS route class gives the interface route's
/// footprint only to requests for its internal host (`cf_arena.rs`, read in
/// JS before any Rust runs).
#[test]
fn the_eval_classifier_names_the_internal_host() {
    let src = include_str!("../cf_arena.rs");
    let rule = format!(
        "if (p.startsWith('/internal/admission/') && u.host === '{}') return 9;",
        slop_ninja_admission_wire::internal::EVAL_INTERFACE_HOST
    );
    assert!(src.contains(&rule), "{rule}");
    assert!(slop_ninja_admission_wire::internal::EVAL_PATH_PREFIX.starts_with("/internal/admission/"));
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
    tx(&db, |s| Ok::<_, EvalError>(part_record(s, &chain(), &preq, &id2, &t, "e"))).unwrap();
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
    tx(&db, |s| apply_frame(s, &chain(), &id, framed.into(), NOW)).unwrap();
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
        apply_frame(s, &chain(), &m2.session(), f.finish(Version::V2).into(), NOW)
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
        package_sha256: hex::encode(Sha256::digest([])),
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
            (m3.upload.artifact_sha256, m3.upload.artifact_len, Ok(framed)).into(),
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
        package_sha256: hex::encode(Sha256::digest([1])),
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
            (again.upload.artifact_sha256, again.upload.artifact_len, Ok(framed)).into(),
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
    tx(db, |s| Ok::<_, EvalError>(part_record(s, &chain(), &req, &id, &t, "e"))).unwrap();
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
    let due = tx(&db, |s| cleanup_due(s, 10, crate::wire::MAX_SAFE_INTEGER)).unwrap();
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

/// Third W3 review finding 1, fix round 1 findings 1 and 9: a returning
/// miner commits again and opens a session before the reader has logged the
/// new commitment. The older logged commitment (from before the window, or
/// replaced within it) answers the retryable `commitment_not_found` at
/// first, but only until a poll completes at or after the cursor of that
/// first answer: production polls once a minute while the heartbeat moves
/// the cursor every block, so a wait tied to the moving cursor would almost
/// never end. Every definitive code comes back after one poll, even with the
/// cursor already ahead of it again.
#[test]
fn a_returning_miner_waits_one_poll_for_a_definitive_answer() {
    let db = db_with_open_cohort(2);
    let poll_now = |db: &Sqlite| polled(db, tx(db, cursor).unwrap().unwrap());
    // The hotkey's earlier cohort commitment is logged; the new one is not.
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK - 50, 0);
    poll_now(&db);
    heartbeat(&db, 2);
    let (r, _) = m.create(&db);
    assert_eq!(
        (r.status, code(&r).as_str(), r.retry_after),
        (409, "commitment_not_found", Some(60))
    );
    // The cursor keeps moving; no poll yet: the same answer.
    heartbeat(&db, 3);
    assert_eq!(code(&m.create(&db).0), "commitment_not_found");
    // One poll completes (at the cursor as of its plan, after the first
    // answer), then the heartbeat runs ahead of it again: final.
    poll_now(&db);
    heartbeat(&db, 4);
    assert_eq!(code(&m.create(&db).0), "commitment_before_open");
    assert_eq!(code(&m.create(&db).0), "commitment_before_open", "and stays final");
    // A poll that completed before the first answer (planned at an older
    // cursor) does not end the wait: no poll completed at all, likewise.
    m.commit(&db, OPEN_BLOCK + 3, 1);
    let replaced = miner(1, 3, 1, 0x22);
    heartbeat(&db, 1);
    let first = tx(&db, cursor).unwrap().unwrap();
    assert_eq!(code(&replaced.create(&db).0), "commitment_not_found");
    polled(&db, first - 1);
    heartbeat(&db, 6);
    assert_eq!(code(&replaced.create(&db).0), "commitment_not_found");
    polled(&db, first);
    heartbeat(&db, 6);
    assert_eq!(code(&replaced.create(&db).0), "commitment_mismatch");
    // The new commitment logged: the session opens.
    replaced.commit(&db, OPEN_BLOCK + 4, 0);
    assert_eq!(replaced.create(&db).0.status, 201);
    // Another hotkey with no registration at its commitment: retryable at
    // first, then `not_registered`.
    let unreg = miner(3, 5, 1, 0x33);
    let e = LogEntry {
        hotkey: account(3),
        block: OPEN_BLOCK + 5,
        block_hash: format!("0x{}", hex::encode([7u8; 32])),
        extrinsic: 0,
        payload: unreg.upload.opening.chain_payload().to_vec(),
        uid: None,
        registration_block: None,
    };
    tx(&db, |s| commitments::record(s, &e, NOW)).unwrap();
    heartbeat(&db, 1);
    assert_eq!(code(&unreg.create(&db).0), "commitment_not_found");
    tx(&db, |s| s.exec("UPDATE commit_reader SET polled_block = NULL WHERE id = 1", &[])).unwrap();
    assert_eq!(code(&unreg.create(&db).0), "commitment_not_found", "no poll at all: not final");
    poll_now(&db);
    heartbeat(&db, 2);
    assert_eq!(code(&unreg.create(&db).0), "not_registered");
    // An opening for another cohort is never held as retryable.
    let elsewhere = miner(2, 4, 2, 0x12);
    elsewhere.commit(&db, OPEN_BLOCK - 50, 0);
    heartbeat(&db, 1);
    assert_eq!(code(&elsewhere.create(&db).0), "wrong_cohort");
}

/// Fix round 2, finding 1: a first answer right after a poll at the current
/// cursor (no heartbeat since: the normal state during a heartbeat or webhook
/// stall) is not final, since a commitment made after that poll's block
/// cannot be in it; and a coordinator whose cursor lags the block holding
/// the miner's new commitment waits for that block when the request names it
/// (`x-sn-commitment-block`), never answering from the older commitment.
#[test]
fn a_first_answer_waits_past_the_polled_cursor_and_the_miners_commitment_block() {
    let db = db_with_open_cohort(2);
    let poll_now = |db: &Sqlite| polled(db, tx(db, cursor).unwrap().unwrap());
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK - 50, 0);
    poll_now(&db);
    let (r, _) = m.create(&db);
    assert_eq!(
        (r.status, code(&r).as_str(), r.retry_after),
        (409, "commitment_not_found", Some(60)),
        "a poll at the cursor does not make the first answer final"
    );
    // A poll planned after the first answer does.
    heartbeat(&db, 1);
    poll_now(&db);
    assert_eq!(code(&m.create(&db).0), "commitment_before_open");

    // The cursor lags the miner's new commitment by 20 blocks.
    let old = miner(2, 4, 1, 0x21);
    old.commit(&db, OPEN_BLOCK - 40, 0);
    poll_now(&db);
    let fresh = miner(2, 4, 1, 0x22);
    let at = tx(&db, cursor).unwrap().unwrap() + 20;
    assert_eq!(code(&fresh.create_hinted(&db, Some(at)).0), "commitment_not_found");
    // Polls below that block are not final, with or without the hint again.
    heartbeat(&db, 5);
    poll_now(&db);
    assert_eq!(code(&fresh.create_hinted(&db, Some(at)).0), "commitment_not_found");
    heartbeat(&db, 5);
    poll_now(&db);
    assert_eq!(code(&fresh.create(&db).0), "commitment_not_found", "the recorded block stays");
    // A lower hint never moves the block down.
    assert_eq!(code(&fresh.create_hinted(&db, Some(at - 15)).0), "commitment_not_found");
    // The cursor reaches the block and the reader logs the commitment.
    heartbeat(&db, 10);
    fresh.commit(&db, at, 0);
    poll_now(&db);
    assert_eq!(fresh.create(&db).0.status, 201);

    // A hint at or below the last poll: that poll holds the commitment, so
    // the answer is final at once.
    let held = miner(4, 6, 1, 0x41);
    miner(4, 6, 1, 0x42).commit(&db, OPEN_BLOCK + 20, 0);
    poll_now(&db);
    assert_eq!(code(&held.create_hinted(&db, Some(OPEN_BLOCK + 20)).0), "commitment_mismatch");

    // A hint can be false (a block nothing was committed at): only this
    // hotkey's own answer waits, until a poll reaches it.
    let liar = miner(3, 5, 1, 0x31);
    let other = miner(3, 5, 1, 0x32);
    other.commit(&db, OPEN_BLOCK + 30, 0);
    poll_now(&db);
    let far = tx(&db, cursor).unwrap().unwrap() + 3;
    assert_eq!(code(&liar.create_hinted(&db, Some(far)).0), "commitment_not_found");
    heartbeat(&db, 3);
    poll_now(&db);
    assert_eq!(code(&liar.create(&db).0), "commitment_mismatch");
}

/// Fix round 1, finding 4: at the verify stage the logged commitment's
/// answer is final. A commitment replaced after the session opened refuses
/// the session with `commitment_mismatch`, never the retryable
/// `commitment_not_found` the open path can answer.
#[test]
fn a_commitment_replaced_after_open_refuses_at_verify_with_its_real_code() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x41);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    let (r, _) = m.create(&db);
    assert_eq!(r.status, 201);
    upload_all(&db, &m);
    // Replaced (and logged) after the open, with the reader behind the cursor.
    let other = miner(1, 3, 1, 0x42);
    other.commit(&db, OPEN_BLOCK + 2, 0);
    polled(&db, OPEN_BLOCK);
    heartbeat(&db, 5);
    frame_only(&db, &m);
    assert_eq!(
        state_of(&db, &m.session()),
        ("refused".to_owned(), Some("commitment_mismatch".to_owned()))
    );
}

/// Third W3 review, finding 7: a status read (`has_work`) never changes
/// state. A reference chain naming a record no stored cohort holds leaves
/// the calendar untouched until its own step (`calendar::due`) records the
/// error and pauses the template.
#[test]
fn reading_status_never_pauses_the_calendar() {
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let t = crate::eval::calendar::Template {
        schema: crate::eval::calendar::TEMPLATE_SCHEMA.into(),
        label_prefix: "testnet-cohort".into(),
        plan: template_plan(0, 40),
    };
    let bytes = serde_json::to_vec(&t).unwrap();
    tx(&db, |s| crate::eval::calendar::install(s, &cfg(), &bytes, NOW)).unwrap();
    tx(&db, |s| {
        s.exec(
            "INSERT INTO reference_state (id, k, current, predecessor) VALUES (1, 1, ?, NULL)",
            &[crate::eval::engine::s("ab".repeat(32))],
        )
    })
    .unwrap();
    let template = |db: &Sqlite| {
        tx(db, |s| {
            let r = s.exec("SELECT paused, last_error FROM standing_template WHERE id = 1", &[])?;
            Ok::<_, EvalError>((int(&r[0], 0)?, crate::eval::engine::opt_text(&r[0], 1)))
        })
        .unwrap()
    };
    for _ in 0..3 {
        assert!(tx(&db, |s| engine::has_work(s, &cfg())).unwrap());
        let _ = tx(&db, |s| engine::status(s, NOW)).unwrap();
        let _ = tx(&db, crate::eval::calendar::view).unwrap();
    }
    assert_eq!(template(&db), (0, None), "status reads wrote nothing");
    // The calendar's own step records the reason and pauses.
    assert!(tx(&db, |s| crate::eval::calendar::due(s, &cfg())).unwrap().is_none());
    let (paused, err) = template(&db);
    assert_eq!(paused, 1);
    assert!(err.unwrap().starts_with("template_reference_unresolved"));
    assert!(!tx(&db, |s| engine::has_work(s, &cfg())).unwrap());
}

/// Third W3 review, finding 5: an admission run's charge is recorded with
/// the run under a key naming the hotkey, commitment and try, and stays due
/// (a reason to step) until the ledger has confirmed it.
#[test]
fn admission_charges_are_durable_and_keyed_per_run() {
    let db = db_with_open_cohort(2);
    let run = |hotkey: &str, try_no: u32| LaneRun {
        hotkey: hotkey.into(),
        commitment: "cc".repeat(32),
        package_sha256: "dd".repeat(32),
        manifest_key: "artifacts/packages/x.json".into(),
        label: "testnet-c1".into(),
        spec_sha256: spec_v2().spec_sha256,
        scorer_binary: "aa".repeat(32),
        try_no,
        claim_until_ms: 0,
        limits: AdmitLimits { artifact_max_bytes: 1 << 30, load_wait_ms: 600_000, inactivity_ms: 60_000, doc_timeout_ms: 120_000 },
    };
    let a1 = run(&"01".repeat(32), 1);
    let (key, micro) = tx(&db, |s| record_charge(s, &a1, 600_000, NOW)).unwrap().expect("a charge");
    assert!(micro > 0);
    assert_eq!(key, format!("main:testnet-c1:admission:{}:{}:1", "01".repeat(32), "cc".repeat(32)));
    // The same run recorded again (a retried transaction) is one charge;
    // another try and another hotkey are their own.
    tx(&db, |s| record_charge(s, &a1, 600_000, NOW + 1)).unwrap();
    tx(&db, |s| record_charge(s, &run(&"01".repeat(32), 2), 1_000, NOW)).unwrap();
    tx(&db, |s| record_charge(s, &run(&"02".repeat(32), 1), 1_000, NOW)).unwrap();
    let count = |db: &Sqlite| {
        tx(db, |s| {
            Ok::<_, EvalError>(int(&s.exec("SELECT COUNT(*) FROM adm_charges", &[])?[0], 0)?)
        })
        .unwrap()
    };
    assert_eq!(count(&db), 3);
    assert!(tx(&db, |s| has_work(s, &cfg(), None)).unwrap());
    // Settled one by one, oldest first, only when the ledger answered.
    let mut settled = Vec::new();
    while let Some(engine::Work::AdmissionSettle { key, microusd }) = tx(&db, charge_due).unwrap() {
        assert!(microusd > 0);
        tx(&db, |s| charge_settled(s, &key)).unwrap();
        settled.push(key);
    }
    assert_eq!(settled.len(), 3);
    assert_eq!(settled[0], key);
    assert_eq!(count(&db), 0);
}

/// Finding 6 of the second W3 review: free signatures cannot grow the cohort
/// object's refusal table. A refusal is stored only for the admitting cohort
/// and a hotkey the commitment reader knows; any other is answered only.
#[test]
fn refusals_of_unknown_hotkeys_are_answered_but_never_stored() {
    let db = db_with_open_cohort(2);
    let rows = |db: &Sqlite| {
        tx(db, |s| Ok::<_, EvalError>(int(&s.exec("SELECT COUNT(*) FROM adm_refusals", &[])?[0], 0)?)).unwrap()
    };
    for n in 20..30u8 {
        let stranger = miner(n, 40, 1, n);
        assert_eq!(code(&stranger.create(&db).0), "commitment_not_found");
        // An opening naming a cohort that does not exist.
        let elsewhere = miner(n, 40, 9_000 + u64::from(n), n);
        assert_ne!(code(&elsewhere.create(&db).0), "");
    }
    assert_eq!(rows(&db), 0, "nothing stored for hotkeys with no commitment");
    let known = miner(31, 41, 1, 0x31);
    known.commit(&db, OPEN_BLOCK - 1, 0);
    assert_eq!(code(&known.create_hinted(&db, Some(OPEN_BLOCK - 1)).0), "commitment_before_open");
    assert_eq!(rows(&db), 1, "a known hotkey's refusal is its last refusal");
}

/// Finding 7 of the second W3 review: an entrant under a new hotkey that an
/// operator coldkey owned at the cutoff is no outside entrant, nor is one
/// whose owner the snapshot could not read; another owner is.
#[test]
fn entrants_owned_by_an_operator_coldkey_never_unlock_fresh_stock() {
    for (owner, outside) in [(Some([0xc0u8; 32]), false), (None, false), (Some([0x77u8; 32]), true)] {
        let db = db_with_open_cohort(2);
        let m = miner(4, 2, 1, 0x14);
        m.commit(&db, OPEN_BLOCK + 1, 0);
        m.create(&db);
        upload_all(&db, &m);
        frame_ok(&db, &m);
        let cutoff = OPEN_BLOCK + 360;
        close_at(&db, cutoff);
        snapshot(&db, cutoff, &m, Some(OPEN_BLOCK + 1), &m.upload.opening.chain_payload(), Some(m.uid), Some(m.registration_block));
        tx(&db, |s| {
            s.exec(
                "UPDATE adm_snapshot SET coldkey = ?",
                &[owner.map_or(SqlValue::Null, |c| SqlValue::Text(hex::encode(c)))],
            )?;
            Ok::<_, EvalError>(())
        })
        .unwrap();
        admit_all(&db);
        let b = tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap().unwrap();
        assert_eq!(b, Boundary::Frozen { admitted: 1, overflow: 0, outside }, "{owner:?}");
        let c = tx(&db, |s| engine::cohort(s, 1)).unwrap();
        assert_eq!(c.plan.pool.pool_id == "fresh", outside);
    }
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
    // The epoch the window's epochs are measured from: the last observed one.
    assert_eq!(info.confirmed_epoch, Some(11));
    assert!(serde_json::to_vec(&admission_plan(1, 2)).unwrap().len() < crate::arena_routes::MAX_PLAN_BODY_BYTES);
    // Before any epoch observation the field is absent, not null.
    let fresh = Sqlite::open();
    tx(&fresh, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let info = tx(&fresh, |s| cohort_info(s, &chain(), false)).unwrap();
    assert_eq!(info.confirmed_epoch, None);
    assert!(serde_json::to_value(&info).unwrap().get("confirmed_epoch").is_none());
}

#[test]
fn the_calendar_opens_cohorts_from_the_standing_template() {
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let t = crate::eval::calendar::Template {
        schema: crate::eval::calendar::TEMPLATE_SCHEMA.into(),
        label_prefix: "testnet-cohort".into(),
        plan: template_plan(0, 40),
    };
    let bytes = serde_json::to_vec(&t).unwrap();
    tx(&db, |s| crate::eval::calendar::install(s, &cfg(), &bytes, NOW)).unwrap();
    // Between windows the cohort read names the next cohort's contract,
    // object bound and spec from the plan it opens with (here the template's;
    // below, once queued, its own), so `check` can compare an object.
    let bounds = SpecBounds::of_id(&t.plan.admission.as_ref().unwrap().artifact_spec).unwrap();
    let limits = |db: &Sqlite| {
        let n = tx(db, |s| cohort_info(s, &chain(), false)).unwrap().next.unwrap();
        (n.cohort, n.output_contract, n.max_object_len, n.artifact_spec.map(|a| a.sha256))
    };
    let expect = |c: u64| {
        (c, t.plan.reward_v1.as_ref().map(|v| v.output_contract.clone()), Some(bounds.object_bytes), Some(bounds.spec_sha256.clone()))
    };
    assert_eq!(limits(&db), expect(1));
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
    assert_eq!(tx(&db, crate::eval::calendar::next).unwrap().map(|x| x.cohort), Some(1));
    assert_eq!(limits(&db), expect(1), "the queued plan's");
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
    let mut unlocks = t.clone();
    unlocks.plan.admission = admission_plan(0, 40).admission;
    assert!(
        crate::eval::calendar::check(&cfg(), &serde_json::to_vec(&unlocks).unwrap()).is_err(),
        "a standing template never names a fresh pool"
    );
    let mut fresh = t;
    fresh.plan.pool.pool_id = "fresh".into();
    assert!(
        crate::eval::calendar::check(&cfg(), &serde_json::to_vec(&fresh).unwrap()).is_err(),
        "the template's own pool is exposed stock"
    );
}

/// Miner UX: the calendar's next cohort opens at the boundary that can close
/// the cohort before it (the engine opens it only then), never at the next
/// boundary while that cohort is still admitting, working or in its
/// transition (live, cohort 19 showed epoch 99 while cohort 18's close was
/// due at 113); and the next cohort carries its admission window.
#[test]
fn the_calendar_names_the_next_opening_at_the_earliest_close_before_it() {
    use crate::eval::calendar;
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let t = calendar::Template {
        schema: calendar::TEMPLATE_SCHEMA.into(),
        label_prefix: "testnet-cohort".into(),
        plan: template_plan(0, 40),
    };
    let (k, a, tr) = (t.plan.k_tempos, t.plan.admission_tempos, t.plan.transition_tempos);
    assert_eq!((k, a, tr), (4, 1, 2));
    tx(&db, |s| calendar::install(s, &cfg(), &serde_json::to_vec(&t).unwrap(), NOW)).unwrap();
    let next = |db: &Sqlite| tx(db, calendar::next).unwrap().unwrap();
    // A boundary the coordinator credited (one active tempo per epoch), and
    // a jump it did not credit (an outage).
    let set = |db: &Sqlite, e: u64, credit: bool| {
        tx(db, |s| {
            s.exec(
                "UPDATE eval_obs SET active_tempos = active_tempos + CASE WHEN ? THEN ? - epoch_index ELSE 0 END,
                    epoch_index = ? WHERE id = 1",
                &[SqlValue::Integer(i64::from(credit)), bind_u64(e)?, bind_u64(e)?],
            )
            .map(|_| ())
        })
        .unwrap()
    };
    let at = |db: &Sqlite, e: u64| set(db, e, true);
    let jump = |db: &Sqlite, e: u64| set(db, e, false);
    let active = |db: &Sqlite| tx(db, engine::observed).unwrap().active_tempos;
    epoch(&db, 10, OPEN_BLOCK - 100, NOW);
    // Nothing yet: the template's first cohort, at the next boundary.
    assert_eq!(next(&db), NextCohort { cohort: 1, earliest_open_epoch: Some(11), admission_tempos: Some(a), ..Default::default() });
    let (_, plan) = tx(&db, |s| calendar::due_any(s, 0)).unwrap().unwrap();
    tx(&db, |s| engine::queue(s, &cfg(), &plan, NOW)).unwrap();
    assert_eq!(next(&db).earliest_open_epoch, Some(11), "queued, nothing before it");
    epoch(&db, 11, OPEN_BLOCK, NOW + 1);
    assert_eq!(tx(&db, |s| engine::cohort(s, 1)).unwrap().phase, "admission");
    // Admitting: cutoff at 12, package at 12 at the earliest, published at
    // 13, closed 2 tempos later and at least K after 11.
    let n = next(&db);
    assert_eq!((n.cohort, n.earliest_open_epoch, n.admission_tempos), (2, Some(15), Some(a)));
    // The dashboard's bounds for the current cohort (E1): the credited
    // cutoff, the credited close (the next cohort's earliest opening) and
    // the opening block.
    let cur = tx(&db, calendar::view).unwrap()["current"].clone();
    assert_eq!(cur["earliest_cutoff_epoch"], 12);
    assert_eq!(cur["earliest_close_epoch_credited"], 15);
    assert_eq!(cur["opened_block"], OPEN_BLOCK);
    assert!(cur["window_closed_epoch"].is_null(), "still admitting");
    // In its transition: not the next boundary (the earlier display bug).
    tx(&db, |s| {
        s.exec("UPDATE cohorts SET phase = 'transition', packaged_epoch = 12 WHERE cohort = 1", &[]).map(|_| ())
    })
    .unwrap();
    at(&db, 12);
    assert_eq!(next(&db).earliest_open_epoch, Some(15));
    // Published late (at 14): the transition counts from there.
    at(&db, 14);
    let published = active(&db);
    tx(&db, |s| {
        s.exec("UPDATE cohorts SET published_epoch = 14, published_active = ? WHERE cohort = 1", &[bind_u64(published)?])
            .map(|_| ())
    })
    .unwrap();
    assert_eq!(next(&db).earliest_open_epoch, Some(16));
    let cur = tx(&db, calendar::view).unwrap()["current"].clone();
    assert_eq!(cur["earliest_close_epoch_credited"], 16, "the same bound as the next opening");
    assert!(cur["earliest_cutoff_epoch"].is_null(), "past admission");
    // The next cohort queued: the same opening, from the cohort before it.
    let (_, plan) = tx(&db, |s| calendar::due_any(s, 0)).unwrap().unwrap();
    tx(&db, |s| engine::queue(s, &cfg(), &plan, NOW)).unwrap();
    assert_eq!(next(&db), NextCohort { cohort: 2, earliest_open_epoch: Some(16), admission_tempos: Some(a), ..Default::default() });
    // An outage the coordinator did not credit (14 to 19): the transition
    // still owes both its tempos, so the close is two boundaries after the
    // observed one, never the observed one itself.
    jump(&db, 19);
    assert_eq!(next(&db).earliest_open_epoch, Some(21));
    // Credited at last, and still open past its due close (work or a
    // missed wakeup): the next boundary, never the observed one.
    at(&db, 22);
    assert_eq!(next(&db).earliest_open_epoch, Some(23));
    // Closed: the next boundary.
    tx(&db, |s| s.exec("UPDATE cohorts SET phase = 'closed' WHERE cohort = 1", &[]).map(|_| ())).unwrap();
    assert_eq!(next(&db).earliest_open_epoch, Some(23));
    // The status view and the cohort read carry the same next cohort.
    let view = tx(&db, calendar::view).unwrap();
    assert_eq!(view["next"], serde_json::json!({"cohort": 2, "earliest_open_epoch": 23, "admission_tempos": a}));
    // Paused with nothing queued: no next cohort.
    tx(&db, |s| s.exec("DELETE FROM cohorts WHERE cohort = 2", &[]).map(|_| ())).unwrap();
    tx(&db, |s| calendar::set_paused(s, true)).unwrap();
    assert!(tx(&db, calendar::next).unwrap().is_none());
    // The rule itself at the daily shape (K 20, 17 admitting, transition 2):
    // the live case, and work done in the window's closing tempo still
    // closes K after the opening.
    let daily = calendar::CloseShape { opened: 93, k: 20, admission: 1, transition: 2, v3: true };
    assert_eq!(calendar::earliest_close(&daily, Some(94), Some(95), 98, None), 113);
    let long = calendar::CloseShape { opened: 0, k: 20, admission: 17, transition: 2, v3: true };
    assert_eq!(calendar::earliest_close(&long, None, None, 5, None), 20);
    assert_eq!(calendar::earliest_close(&long, Some(17), None, 17, None), 20);
    assert_eq!(calendar::earliest_close(&long, Some(18), None, 18, None), 21, "one tempo late moves the close one tempo");
    let v0 = calendar::CloseShape { v3: false, ..long };
    assert_eq!(calendar::earliest_close(&v0, Some(19), None, 19, None), 21);
    // In credited tempos: every boundary credited, the same bound.
    let credited = |now, packaged, published| calendar::Credited { now, opened: 0, packaged, published };
    assert_eq!(calendar::earliest_close(&long, None, None, 5, Some(&credited(5, None, None))), 20);
    assert_eq!(calendar::earliest_close(&long, Some(17), None, 17, Some(&credited(17, Some(17), None))), 20);
    // Three uncredited tempos while admitting (epoch 8, 5 credited): the
    // window, the work and the close all move three boundaries later.
    assert_eq!(calendar::earliest_close(&long, None, None, 8, Some(&credited(5, None, None))), 23);
    // Published at 21 with 18 credited: two transition tempos owed.
    assert_eq!(calendar::earliest_close(&long, Some(20), Some(21), 21, Some(&credited(18, None, Some(18)))), 23);
    // v0, packaged with 17 credited at epoch 19: K owes 3, the transition 2.
    assert_eq!(calendar::earliest_close(&v0, Some(19), None, 19, Some(&credited(17, Some(17), None))), 22);
    assert_eq!(calendar::earliest_close(&v0, None, None, 19, Some(&credited(19, None, None))), 21);
}

/// Miner UX: the standing template's admission window is derived from
/// measurements (work after the cutoff, one submission of a measured size,
/// one scoring lane and one cold path of a measured size) and the plan's own
/// shape, with a full roster's verdict queue at the cutoff, so a full cohort
/// still closes K tempos after it opened; nothing in it is a tuned figure.
/// The inputs are the recorded r0 ones (TESTNET.md).
#[test]
fn the_admission_window_is_derived_from_measured_work_upload_and_scoring() {
    use crate::eval::calendar::{self, WindowInputs, derive_admission_tempos};
    let mut t = daily_template("testnet-cohort", 40);
    // The installed r0 template's load allowance and the step budget that
    // holds one isolation proof of the spec's largest object at it.
    let score = &mut t.plan.fanout.as_mut().unwrap().score;
    score.load_ms_per_mib = Some(120);
    score.step_budget_ms = Some(780_000);
    let object = SpecBounds::of(Version::V3).unwrap().object_bytes;
    assert_eq!(object, 954_576_228);
    assert_eq!(t.plan.entrant_docs(), 1000 + 1024 + 200);
    // Cohort 18's work (cutoff to durable package, 732 s), test miner 1's
    // 613,185,948-byte submission in the cohort 13 drill (582 s, commit to
    // verified), cohort 18's R_0 lane (306 s for 1,000 documents with a
    // 613,189,229-byte object) and the longest cold path of that object in
    // the load runs (54.6 s, so 55), at 360 blocks per tempo.
    let live = WindowInputs {
        tempo_blocks: 360,
        block_secs: calendar::NOMINAL_BLOCK_SECS,
        work_secs: 732,
        upload_secs: 582,
        upload_bytes: 613_185_948,
        score_secs: 306,
        score_docs: 1000,
        score_bytes: 613_189_229,
        cold_path_secs: 55,
    };
    let d = derive_admission_tempos(&t.plan, object, 40, &live).unwrap();
    assert_eq!(d.tempo_secs, 4320);
    assert_eq!((d.largest_upload_secs, d.load_secs), (907, 110));
    assert_eq!(d.entrant_lane_secs, 1060);
    // A full roster of 40 verifying at the cutoff: 7 rounds of 6, each one
    // cold path scaled to the largest object (86 s) and one load of it.
    assert_eq!((d.verdict_secs, d.verdict_rounds, d.verdict_queue_secs), (86 + 110, 7, 7 * 196));
    assert_eq!(d.work_bound_secs, 732 + 907 + 1372 + 110 + 1060);
    assert!(d.work_bound_secs * 2 <= 2 * 4320, "the queue still fits two tempos at r0");
    assert_eq!((d.work_tempos, d.admission_tempos), (2, 20 - 2 - 2));
    // The verdict queue grows by whole rounds: 43 entrants take an eighth.
    assert_eq!(derive_admission_tempos(&t.plan, object, 43, &live).unwrap().verdict_rounds, 8);
    // Without the scoring lane and the verdict queue (roster 0: one
    // post-cutoff verdict only) the same inputs give the first install's 17,
    // and with the queue but no scoring lane, 16.
    let unscored = WindowInputs { score_secs: 0, ..live };
    let first = derive_admission_tempos(&t.plan, object, 0, &unscored).unwrap();
    assert_eq!((first.verdict_queue_secs, first.work_bound_secs, first.work_tempos, first.admission_tempos), (0, 1749, 1, 17));
    let queued = derive_admission_tempos(&t.plan, object, 40, &unscored).unwrap();
    assert_eq!((queued.work_bound_secs, queued.admission_tempos), (1749 + 1372, 16));
    // The derived window passes the template check, and a full cohort's
    // work in the tempos after the window closes still closes at K.
    let mut long = t.clone();
    long.plan.admission_tempos = d.admission_tempos;
    calendar::check(&cfg(), &serde_json::to_vec(&long).unwrap()).unwrap();
    let shape = calendar::CloseShape { opened: 0, k: 20, admission: d.admission_tempos, transition: 2, v3: true };
    for packaged in [16, 17] {
        assert_eq!(calendar::earliest_close(&shape, Some(packaged), None, packaged, None), 20);
    }
    // Work bound past one tempo takes a third tempo from the window.
    let slow = WindowInputs { work_secs: 4321 - 907 - 1372 - 110 - 1060, ..live };
    assert_eq!(derive_admission_tempos(&t.plan, object, 40, &slow).unwrap().admission_tempos, 15);
    let at_bound = WindowInputs { work_secs: 4320 - 907 - 1372 - 110 - 1060, ..live };
    assert_eq!(derive_admission_tempos(&t.plan, object, 40, &at_bound).unwrap().admission_tempos, 16);
    // A slower block interval lengthens the tempo: 15 s blocks give 5,400 s,
    // the bound still needs two tempos.
    let slower = WindowInputs { block_secs: 15, ..live };
    assert_eq!(derive_admission_tempos(&t.plan, object, 40, &slower).unwrap().tempo_secs, 5400);
    // A submission that would not fit half the window is refused, as is a
    // period with no room for a window, or a zero input.
    let crawl = WindowInputs { upload_secs: 40_000_000, ..live };
    assert!(derive_admission_tempos(&t.plan, object, 40, &crawl).is_err());
    let mut short = t.plan.clone();
    short.k_tempos = 3;
    assert!(derive_admission_tempos(&short, object, 40, &live).is_err());
    for zero in [
        WindowInputs { tempo_blocks: 0, ..live },
        WindowInputs { block_secs: 0, ..live },
        WindowInputs { score_docs: 0, ..live },
        WindowInputs { score_bytes: 0, ..live },
    ] {
        assert!(derive_admission_tempos(&t.plan, object, 40, &zero).is_err());
    }
    // The display: tempos, blocks and nominal minutes.
    assert_eq!(
        calendar::window_json(16, 360),
        serde_json::json!({"tempos": 16, "blocks": 5760, "nominal_minutes": 1152})
    );
}

/// The daily template shape: n = 1,000, K = 20, the 1,024-document probe,
/// 200 audit humans, spec v3, under a named authority's labels.
fn daily_template(prefix: &str, cap: u16) -> crate::eval::calendar::Template {
    let mut p = admission_plan(0, cap);
    p.n = 1000;
    p.k_tempos = 20;
    p.max_attempts = 3;
    let v = p.reward_v1.as_mut().unwrap();
    v.probes[0].size = 1024;
    v.canary.draw_humans = 200;
    v.lease_tempos = 40;
    let f = p.fanout.as_mut().unwrap();
    f.score.lane_docs = 1000;
    f.score.scoring_budget_microusd = 1_000_000_000;
    let b3 = SpecBounds::of(Version::V3).unwrap();
    f.score.artifact_spec = Some(b3.spec_id.into());
    let a = p.admission.as_mut().unwrap();
    a.artifact_spec = b3.spec_id.into();
    a.artifact_spec_sha256 = b3.spec_sha256.clone();
    a.max_parts = u32::try_from(b3.object_bytes.div_ceil(a.part_size)).unwrap();
    // Standing templates never name a fresh pool (`calendar::check`).
    a.fresh_pool = None;
    crate::eval::calendar::Template {
        schema: crate::eval::calendar::TEMPLATE_SCHEMA.into(),
        label_prefix: prefix.into(),
        plan: p,
    }
}

#[test]
fn the_receipt_index_bounds_the_template_roster_and_validators_hold_it_by_reference() {
    use slop_ninja_protocol::target_bundle::{
        VALIDATOR_BUNDLE_BYTES, receipt_bundle_bytes, receipt_index_bytes, receipt_index_capacity,
        validator_receipt_index_bytes,
    };
    let cfg = cfg();
    let check = |t: &crate::eval::calendar::Template| crate::eval::calendar::check(&cfg, &serde_json::to_vec(t).unwrap());
    // Receipts per cohort from the shape alone: R_k, R_{k-1} and `cap`
    // entrants, every document at every attempt.
    let per_cohort = |cap: u64, attempts: u64| attempts * (2 * 1000 + cap * (1000 + 1024 + 200));
    // The full 40-entrant daily template is admitted: its receipts fit one
    // signed receipt index, although a validator bundle that embeds them
    // (schema v2) for a target and its carrier would exceed the 64 MiB file
    // bound.
    let t = daily_template("testnet-cohort", 40);
    check(&t).unwrap();
    let c = crate::eval::calendar::capacity(&t);
    let capacity = receipt_index_capacity().unwrap();
    assert_eq!(c["roster_cap"], 40);
    assert_eq!(c["receipts_per_cohort"], per_cohort(40, 3));
    // The signed-record limit is tighter than the codec's sequence bound,
    // and a validator session holds two indexes at it.
    assert!(capacity < u64::from(slop_ninja_protocol::MAX_SEQUENCE_LEN));
    assert_eq!(c["receipt_index_bound"], capacity);
    assert_eq!(c["receipt_bounds"]["sequence"], slop_ninja_protocol::MAX_SEQUENCE_LEN);
    assert_eq!(c["receipt_bounds"]["signed_record"], capacity);
    assert_eq!(c["receipt_bounds"]["validator_session"], capacity);
    assert_eq!(c["receipt_index_bytes"], receipt_index_bytes(per_cohort(40, 3)).unwrap());
    assert_eq!(c["validator_receipt_index_bytes"], validator_receipt_index_bytes().unwrap());
    let embedded = 2 * receipt_bundle_bytes(per_cohort(40, 3)).unwrap();
    assert!(embedded > VALIDATOR_BUNDLE_BYTES);
    assert_eq!(c["embedded_bundle_receipt_bytes"], embedded);
    assert_eq!((c["embedded_bundle_fits"].as_bool(), c["validator_bundle_schema"].as_str()), (Some(false), Some("v3")));
    // One signed receipt index is the bound: the most attempts whose
    // receipts it holds is admitted, one more is refused. The attempts that
    // fit the sequence bound alone (11 at 40 entrants: 1,000,560 receipts,
    // a 69 MB signed index the protocol refuses to sign) are refused.
    let most = (1..=u8::MAX).filter(|&a| per_cohort(40, u64::from(a)) <= capacity).max().unwrap();
    let by_sequence = (1..=u8::MAX)
        .filter(|&a| per_cohort(40, u64::from(a)) <= u64::from(slop_ninja_protocol::MAX_SEQUENCE_LEN))
        .max()
        .unwrap();
    assert_eq!((most, by_sequence), (10, 11));
    let mut at = daily_template("testnet-cohort", 40);
    at.plan.max_attempts = most;
    check(&at).unwrap();
    for refused in [most + 1, by_sequence] {
        at.plan.max_attempts = refused;
        let e = check(&at).unwrap_err();
        assert!(matches!(&e, EvalError::Plan(c) if c.contains("signed receipt index")), "{e:?}");
    }
    // A small template still fits the embedded form.
    let one = daily_template("testnet-cohort", 1);
    let c = crate::eval::calendar::capacity(&one);
    assert_eq!((c["embedded_bundle_fits"].as_bool(), c["validator_bundle_schema"].as_str()), (Some(true), Some("v2 or v3")));
}

#[test]
fn a_named_authoritys_spec_v3_template_opens_its_own_numbers_and_labels() {
    let r0 = EvalConfig { authority: crate::config::Authority::named("r0", 14).unwrap(), ..cfg() };
    let t = daily_template("testnet-r0", 4);
    let bytes = serde_json::to_vec(&t).unwrap();
    // Another prefix is not the authority's: refused at install.
    let other = serde_json::to_vec(&daily_template("testnet-adm", 4)).unwrap();
    assert!(crate::eval::calendar::check(&r0, &other).is_err(), "labels carry the authority's prefix");
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    let reply = tx(&db, |s| crate::eval::calendar::install(s, &r0, &bytes, NOW)).unwrap();
    assert_eq!(reply["capacity"]["roster_cap"], 4);
    let (label, plan_bytes) = tx(&db, |s| crate::eval::calendar::due(s, &r0)).unwrap().unwrap();
    assert_eq!(label, "testnet-r0-14", "numbered from the authority's first cohort");
    let plan: plan::CohortPlan = serde_json::from_slice(&plan_bytes).unwrap();
    assert_eq!((plan.cohort, plan.k_tempos), (14, 20));
    assert_eq!(plan.load_spec_sha256(), Some(Version::V3.spec_sha256().as_str()));
    assert_eq!(plan.admission.as_ref().unwrap().artifact_spec, Version::V3.id());
    tx(&db, |s| engine::queue(s, &r0, &plan_bytes, NOW)).unwrap();
    // After the authority's operator-posted cohorts 15-17, the next is 18.
    tx(&db, |s| {
        s.exec("UPDATE cohorts SET phase = 'closed', cohort = 17, label = 'testnet-r0-c'", &[]).map(|_| ())
    })
    .unwrap();
    let (label, _) = tx(&db, |s| crate::eval::calendar::due(s, &r0)).unwrap().unwrap();
    assert_eq!(label, "testnet-r0-18");
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
            Ok::<_, EvalError>(part_record(s, &chain(), &req, &id, &t, &format!("etag-{n}")))
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

/// Finding 10 of the second W3 review: a framing attempt cut off by the
/// platform (CPU, eviction) is never charged to the upload. Each attempt
/// arms a growing backoff before it runs, so a cut-off attempt runs again
/// after it, and the session is never refused for it.
#[test]
fn framing_attempts_that_never_finish_back_off_and_never_refuse_the_upload() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    upload_all(&db, &m);
    let id = m.session();
    let mut t = NOW;
    for n in 1..=6u32 {
        assert_eq!(tx(&db, |s| verify_due(s, t)).unwrap().map(|x| x.1), Some("frame".into()), "attempt {n}");
        assert!(tx(&db, |s| frame_begin(s, &id, t)).unwrap().is_some());
        // Cut off: no outcome. Not due again before the backoff.
        assert!(tx(&db, |s| verify_due(s, t + backoff_ms(n) - 1)).unwrap().is_none());
        t += backoff_ms(n);
    }
    let row = tx(&db, |s| session_row(s, &id)).unwrap().unwrap();
    assert_eq!(row.state, SessionState::Verifying, "never refused for the platform's cut-offs");
    let fault = |db: &Sqlite| tx(db, |s| engine::cohort(s, 1)).unwrap().fault;
    assert_eq!(fault(&db), None, "below the plan's pause bound");
    let retrying = |db: &Sqlite| tx(db, status).unwrap()["platform_retries"]["steps_retrying"].as_i64();
    assert_eq!(retrying(&db), Some(1));
    // Fix round 2, finding 5: at the plan's pause bound
    // (`failures_before_pause`, 6 here) the next attempt faults the cohort
    // (resumable `framing_unavailable`): the freeze waits on the same cohort,
    // visibly, and the attempt still runs.
    let bound = tx(&db, window).unwrap().unwrap().failures_before_pause;
    assert_eq!(bound, 6);
    assert!(tx(&db, |s| frame_begin(s, &id, t)).unwrap().is_some());
    assert_eq!(fault(&db).as_deref(), Some(FRAMING_UNAVAILABLE));
    assert!(engine::RESUMABLE_FAULTS.contains(&FRAMING_UNAVAILABLE));
    close_at(&db, OPEN_BLOCK + 360);
    assert_eq!(tx(&db, |s| try_freeze(s, &cfg(), t)).unwrap(), None, "a faulted cohort never freezes");
    // The operator clears it; the next bound of cut-offs faults again, not
    // the first attempt after the clear.
    let label = tx(&db, |s| engine::cohort(s, 1)).unwrap().label;
    assert!(tx(&db, |s| engine::clear_fault(s, &label, t)).unwrap().cleared);
    t += backoff_ms(7);
    for n in 8..=12u32 {
        assert!(tx(&db, |s| frame_begin(s, &id, t)).unwrap().is_some());
        assert_eq!(fault(&db), None, "attempt {n}");
        t += backoff_ms(n);
    }
    assert!(tx(&db, |s| frame_begin(s, &id, t)).unwrap().is_some());
    assert_eq!(fault(&db).as_deref(), Some(FRAMING_UNAVAILABLE));
    // A failed read is the platform's too: not counted, and backed off.
    tx(&db, |s| step_retry(s, &id, true, t)).unwrap();
    assert!(tx(&db, |s| verify_due(s, t)).unwrap().is_none());
    assert!(backoff_ms(40) <= LANE_LEASE_MS, "the backoff is bounded by the lane lease");
}

/// Review round 1 of the upload Worker split, finding 8: a framing call
/// that fails through the link between the two Workers is counted (so the
/// plan's pause bound faults the cohort), a link that is not configured
/// faults it at once, and only the upload Worker's object-read answer is
/// uncounted.
#[test]
fn framing_link_failures_are_counted_and_a_misconfigured_link_faults_at_once() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    upload_all(&db, &m);
    let id = m.session();
    let fault = |db: &Sqlite| tx(db, |s| engine::cohort(s, 1)).unwrap().fault;
    let tries = |db: &Sqlite| {
        let rows = tx(db, |q| q.exec("SELECT step_tries FROM adm_sessions WHERE session = ?", &[s(&id)])).unwrap();
        u32::try_from(crate::sql::int(&rows[0], 0).unwrap()).unwrap()
    };
    let mut t = NOW;
    // An object read: uncounted.
    assert!(tx(&db, |s| frame_begin(s, &id, t)).unwrap().is_some());
    tx(&db, |s| frame_failed(s, &id, FrameFailure::ObjectRead, t)).unwrap();
    assert_eq!(tries(&db), 0);
    // Counted failures reach the pause bound and fault the cohort.
    let bound = tx(&db, window).unwrap().unwrap().failures_before_pause;
    for n in 1..=bound {
        t += backoff_ms(n);
        assert!(tx(&db, |s| frame_begin(s, &id, t)).unwrap().is_some());
        assert_eq!(fault(&db), None, "attempt {n}");
        tx(&db, |s| frame_failed(s, &id, FrameFailure::Counted, t)).unwrap();
        assert_eq!(tries(&db), n);
    }
    t += backoff_ms(bound + 1);
    assert!(tx(&db, |s| frame_begin(s, &id, t)).unwrap().is_some());
    assert_eq!(fault(&db).as_deref(), Some(FRAMING_UNAVAILABLE));
    // After a clear, a misconfigured link faults on its first failure.
    let label = tx(&db, |s| engine::cohort(s, 1)).unwrap().label;
    assert!(tx(&db, |s| engine::clear_fault(s, &label, t)).unwrap().cleared);
    tx(&db, |s| frame_failed(s, &id, FrameFailure::Misconfigured, t)).unwrap();
    assert_eq!(fault(&db).as_deref(), Some(FRAMING_UNAVAILABLE));
    assert_eq!(tx(&db, |s| session_row(s, &id)).unwrap().unwrap().state, SessionState::Verifying);
}

/// Fix round 2, findings 5 and 7: a commitment resolution whose reads keep
/// serving no state pauses the admitting cohort visibly (resumable
/// `commitment_state_unavailable`) at the plan's pause bound, but only when
/// the hotkey has a session verifying there (framing, and so the freeze,
/// waits for the resolution). A verdict run that keeps meeting platform
/// failures pauses it likewise (`scorer_unavailable`).
#[test]
fn stalled_resolutions_and_verdict_platform_failures_pause_the_cohort_visibly() {
    let db = db_with_open_cohort(2);
    let fault = |db: &Sqlite| tx(db, |s| engine::cohort(s, 1)).unwrap().fault;
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    // No session yet: nothing in this cohort waits on it.
    tx(&db, |s| waiting_on_state(s, &account(1), OPEN_BLOCK + 1, 6, NOW)).unwrap();
    assert_eq!(fault(&db), None);
    m.create(&db);
    upload_all(&db, &m);
    tx(&db, |s| waiting_on_state(s, &account(1), OPEN_BLOCK + 1, 5, NOW)).unwrap();
    assert_eq!(fault(&db), None, "below the bound");
    tx(&db, |s| waiting_on_state(s, &account(1), OPEN_BLOCK + 1, 6, NOW)).unwrap();
    assert_eq!(fault(&db).as_deref(), Some(commitments::STATE_UNAVAILABLE));
    assert!(engine::RESUMABLE_FAULTS.contains(&commitments::STATE_UNAVAILABLE));
    let label = tx(&db, |s| engine::cohort(s, 1)).unwrap().label;
    assert!(tx(&db, |s| engine::clear_fault(s, &label, NOW)).unwrap().cleared);
    // Verdict platform failures: the sixth pauses it.
    let hotkey = hex::encode(account(1).0);
    let commitment = m.upload.commitment.hex();
    for n in 0..5u32 {
        tx(&db, |s| platform_wait(s, &hotkey, &commitment, "lane_unavailable", n, NOW)).unwrap();
        assert_eq!(fault(&db), None, "platform failure {}", n + 1);
    }
    tx(&db, |s| platform_wait(s, &hotkey, &commitment, "lane_unavailable", 5, NOW)).unwrap();
    assert_eq!(fault(&db).as_deref(), Some("scorer_unavailable"));
}

/// Fix round 2, finding 8: the step alarm re-arms only for admission work
/// that is due. A clean-up or object delete waiting out its backoff is
/// still unfinished work (the deploy check and retirement wait for it), but
/// no reason to wake the cohort object before the backoff ends.
#[test]
fn backed_off_housekeeping_is_work_but_not_due_work() {
    let db = db_with_open_cohort(2);
    let a = miner(1, 3, 1, 0x11);
    a.commit(&db, OPEN_BLOCK + 1, 0);
    a.create(&db);
    let id = a.session();
    let req = a.signed(&db, "DELETE", &format!("/admission/v1/sessions/{id}/attempts/1"), &[]);
    let (r, _) = tx(&db, |s| Ok::<_, EvalError>(abort(s, &chain(), &req, &id, 1, NOW))).unwrap();
    assert_eq!(r.body["state"].as_str(), Some("aborted"), "{:?}", r.body);
    let req = a.signed(&db, "POST", &format!("/admission/v1/sessions/{id}/attempts/2"), &[]);
    tx(&db, |s| Ok::<_, EvalError>(reopen(s, &chain(), &req, &id, 2, NOW))).unwrap();
    let due = tx(&db, |s| object_deletes_due(s, 8, NOW)).unwrap();
    assert_eq!(due.len(), 1);
    // Only the uploading session and the recorded delete are pending.
    let only = |db: &Sqlite, now: Option<u64>| tx(db, |s| has_work(s, &cfg(), now)).unwrap();
    assert!(only(&db, None) && only(&db, Some(NOW)));
    tx(&db, |s| object_delete_failed(s, &due[0].0, NOW)).unwrap();
    assert!(only(&db, None), "still unfinished");
    assert!(!only(&db, Some(NOW)), "not due during the backoff");
    assert!(!tx(&db, |s| engine::has_due_work(s, &cfg(), NOW)).unwrap());
    assert!(tx(&db, |s| engine::has_work(s, &cfg())).unwrap());
    assert!(only(&db, Some(NOW + backoff_ms(1))), "due once the backoff passed");
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
    assert!(tx(&db, |s| engine::abandon(s, &co.label, "drill", NOW)).unwrap());
    assert_eq!(tx(&db, |s| engine::cohort(s, 1)).unwrap().phase, "abandoned");
    assert_eq!(state_of(&db, &d.session()).0, "refused");
    assert_eq!(tx(&db, |s| cleanup_due(s, 10, crate::wire::MAX_SAFE_INTEGER)).unwrap().len(), 1);
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
    // roster freezes at the next boundary (14), not mid-tempo. The status
    // reads what the freeze waits on without changing anything.
    let wait = |db: &Sqlite| {
        let v = tx(db, crate::eval::calendar::view).unwrap();
        (v["current"]["window_closing"].clone(), v["current"]["freeze_wait"].clone())
    };
    assert_eq!(wait(&db), (json!(true), json!("verification or verdicts pending")));
    epoch(&db, 13, cutoff + 360, NOW + 3);
    assert!(matches!(
        tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap(),
        Some(Boundary::Waiting(_))
    ));
    admit_all(&db);
    assert_eq!(wait(&db).1, json!("the freeze step has not run since the window closed"));
    assert!(matches!(
        tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap(),
        Some(Boundary::Waiting(w)) if w.contains("next tempo boundary")
    ));
    assert_eq!(wait(&db).1, json!("ready: the roster freezes at the next tempo boundary"));
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
    assert_eq!(wait(&db), (json!(false), json!(null)), "frozen: nothing to wait on");
    // The status names the boundary the window closed at for the cohort
    // itself (E1), after the calendar has moved on: the dashboard dates the
    // closing step by it.
    let st = tx(&db, |s| engine::status(s, NOW)).unwrap();
    assert_eq!(st["cohorts"][0]["window_closed_epoch"], 12, "{}", st["cohorts"][0]["window_closed_epoch"]);
    // The measured window: blocks per tempo over its epoch span (11 to 12).
    let measured = |db: &Sqlite| tx(db, crate::eval::calendar::admission_window).unwrap()["last_measured"].clone();
    let m1 = measured(&db);
    assert_eq!((m1["blocks"].clone(), m1["epochs"].clone(), m1["blocks_per_tempo"].clone()), (json!(360), json!(1), json!(360)));
    assert_eq!(m1["extended"], json!(false));
    // A window the coordinator extended past two uncredited tempos spans
    // three boundaries: still 360 blocks per tempo (not 1,080), and marked.
    tx(&db, |s| {
        s.exec(
            "UPDATE adm_cohorts SET cutoff_block = ?, closed_at_epoch = 14 WHERE seq = 1",
            &[bind_u64(OPEN_BLOCK + 1080)?],
        )
        .map(|_| ())
    })
    .unwrap();
    let m2 = measured(&db);
    assert_eq!((m2["blocks"].clone(), m2["epochs"].clone(), m2["blocks_per_tempo"].clone()), (json!(1080), json!(3), json!(360)));
    assert_eq!(m2["extended"], json!(true));
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
    // Admission runs name the cohort's spec on their loads, so a spec v3
    // (span tower class) admission plan is accepted.
    let mut v3p = admission_plan(1, 2);
    let b3 = SpecBounds::of(Version::V3).unwrap();
    let a = v3p.admission.as_mut().unwrap();
    a.artifact_spec = b3.spec_id.into();
    a.artifact_spec_sha256 = b3.spec_sha256.clone();
    v3p.fanout.as_mut().unwrap().score.artifact_spec = Some(b3.spec_id.into());
    let parsed = plan::CohortPlan::parse(&serde_json::to_vec(&v3p).unwrap(), cfg().ceilings).unwrap();
    assert_eq!(parsed.load_spec_sha256(), Some(b3.spec_sha256.as_str()));
    assert_eq!(ADMISSION_SPECS, &[spec::SPEC_ID_V2, spec::SPEC_ID_V3]);
    let mut few = v3p.clone();
    few.admission.as_mut().unwrap().max_parts = u32::try_from(b3.object_bytes.div_ceil(64 << 20)).unwrap() - 1;
    assert!(
        plan::CohortPlan::parse(&serde_json::to_vec(&few).unwrap(), cfg().ceilings).is_err(),
        "the v3 object bound needs its parts"
    );
    // A spec no admission run loads by is refused.
    let mut v4 = v3p;
    v4.admission.as_mut().unwrap().artifact_spec = "slop_ninja_a_artifact_spec_v4".into();
    assert!(plan::CohortPlan::parse(&serde_json::to_vec(&v4).unwrap(), cfg().ceilings).is_err());
    // A plan with a fresh pool must name the operator's coldkeys too.
    let mut p = admission_plan(1, 2);
    p.admission.as_mut().unwrap().operator_coldkeys.clear();
    assert!(plan::CohortPlan::parse(&serde_json::to_vec(&p).unwrap(), cfg().ceilings).is_err());
}

#[test]
fn an_admission_run_names_its_cohorts_spec_to_the_scorer() {
    let limits = AdmitLimits { artifact_max_bytes: 1 << 30, load_wait_ms: 600_000, inactivity_ms: 60_000, doc_timeout_ms: 120_000 };
    for v in [Version::V2, Version::V3] {
        let run = LaneRun {
            hotkey: "0x01".into(),
            commitment: "0x02".into(),
            package_sha256: "aa".repeat(32),
            manifest_key: format!("artifacts/packages/{}.json", "aa".repeat(32)),
            label: "testnet-r0-18".into(),
            spec_sha256: v.spec_sha256(),
            scorer_binary: "bb".repeat(32),
            try_no: 1,
            claim_until_ms: 0,
            limits,
        };
        let b = admit_body(&run);
        assert_eq!(b["spec_sha256"], v.spec_sha256(), "the load names the cohort's spec");
        assert_eq!(b["package_sha256"], run.package_sha256);
        assert_eq!(b["label"], run.label);
        assert_eq!(b["limits"]["artifact_max_bytes"], limits.artifact_max_bytes);
        assert_eq!(b["docs"][0]["text"], crate::eval::lane_logic::CONTROL_TEXT);
        assert_eq!(b.as_object().unwrap().len(), 6);
    }
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
        plan: template_plan(0, 40),
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
        plan: template_plan(0, 4),
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
    let mut appended = t.clone();
    appended.plan.reward_v1.as_mut().unwrap().task_order = None;
    assert!(
        crate::eval::calendar::check(&cfg(), &serde_json::to_vec(&appended).unwrap()).is_err(),
        "a standing template seals in the seeded task order"
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

/// Review round 2 of the upload Worker split, finding 12: a multipart
/// upload that R2 created while an abort landed is not stored on the
/// aborted attempt (the caller aborts it), so none is orphaned.
#[test]
fn an_upload_created_across_an_abort_is_not_stored() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    let id = m.session();
    let req = m.signed(&db, "DELETE", &format!("/admission/v1/sessions/{id}/attempts/1"), &[]);
    let (r, _) = tx(&db, |s| Ok::<_, EvalError>(abort(s, &chain(), &req, &id, 1, NOW))).unwrap();
    assert_eq!(r.body["state"].as_str(), Some("aborted"));
    assert!(!tx(&db, |s| set_upload_id(s, &id, 1, "late")).unwrap(), "the caller aborts it");
    let row = tx(&db, |s| session_row(s, &id)).unwrap().unwrap();
    assert_eq!(row.upload_id, None);
}

/// Review round 2, finding 11: the key-holding side hashes a forwarded
/// creation body itself; a body without the upload Worker's digest, or a
/// body that is not the signed one, is refused.
#[test]
fn a_creation_body_is_hashed_by_the_authority() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    let body = m.upload.to_bytes().unwrap();
    let mut r = m.signed(&db, "POST", "/admission/v1/sessions", &body);
    r.body = Some(hex::encode(&body));
    let mut no_digest = r.clone();
    no_digest.body_sha256 = None;
    let (reply, fx) = tx(&db, |s| Ok::<_, EvalError>(create(s, &chain(), &no_digest, NOW))).unwrap();
    assert_eq!((code(&reply).as_str(), fx.create), ("content_digest_mismatch", None));
    let mut other = r.clone();
    let mut changed = body.clone();
    if let Some(b) = changed.last_mut() {
        *b ^= 1;
    }
    other.body = Some(hex::encode(&changed));
    let (reply, _) = tx(&db, |s| Ok::<_, EvalError>(create(s, &chain(), &other, NOW))).unwrap();
    assert_eq!(code(&reply), "content_digest_mismatch");
    let (reply, fx) = tx(&db, |s| Ok::<_, EvalError>(create(s, &chain(), &r, NOW))).unwrap();
    assert_eq!(reply.status, 201, "{}", reply.body);
    assert!(fx.create.is_some());
}

/// The cohort snapshot answers `paused` once it is as old as the
/// heartbeat freshness the authority answers `paused` after.
#[test]
fn the_snapshot_freshness_is_the_heartbeat_freshness() {
    assert_eq!(slop_ninja_admission_wire::internal::COHORT_SNAPSHOT_FRESH_MS, crate::eval::engine::HEARTBEAT_FRESH_MS);
}

/// L2 (B8 item 1, C1 item 2): a refused roster freeze records its code in
/// the phase log and is resumable. A clear while the cause remains pauses
/// the same cohort again (never skipped); once the cause is corrected a
/// clear freezes the same roster.
#[test]
fn a_refused_freeze_logs_its_code_and_a_clear_freezes_the_same_roster() {
    let db = db_with_open_cohort(4);
    let d = miner(4, 6, 1, 0x14);
    d.commit(&db, OPEN_BLOCK + 1, 0);
    d.create(&db);
    upload_all(&db, &d);
    frame_ok(&db, &d);
    admit_all(&db);
    let key_of = |db: &Sqlite| {
        let rows = tx(db, |q| q.exec("SELECT object_key FROM adm_sessions WHERE session = ?", &[s(d.session())])).unwrap();
        crate::sql::text(&rows[0], 0).unwrap().to_owned()
    };
    let good = key_of(&db);
    let set_key = |db: &Sqlite, k: &str| {
        tx(db, |q| q.exec("UPDATE adm_sessions SET object_key = ? WHERE session = ?", &[s(k), s(d.session())]).map(|_| ()))
            .unwrap()
    };
    set_key(&db, "../outside");
    let cutoff = OPEN_BLOCK + 360;
    close_at(&db, cutoff);
    snapshot(&db, cutoff, &d, Some(OPEN_BLOCK + 1), &d.upload.opening.chain_payload(), Some(d.uid), Some(d.registration_block));
    assert!(matches!(tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap(), Some(Boundary::Waiting(_))));
    let co = tx(&db, |s| engine::cohort(s, 1)).unwrap();
    assert_eq!((co.phase.as_str(), co.fault.as_deref()), ("admission", Some(FREEZE_FAULT)));
    assert!(engine::RESUMABLE_FAULTS.contains(&FREEZE_FAULT));
    let notes = |db: &Sqlite| -> Vec<String> {
        tx(db, |q| q.exec("SELECT note FROM phase_log WHERE seq = 1 ORDER BY rowid", &[]))
            .unwrap()
            .iter()
            .filter_map(|r| crate::sql::text(r, 0).ok().map(str::to_owned))
            .collect()
    };
    let refused: Vec<String> = notes(&db).into_iter().filter(|n| n.starts_with("roster_freeze_refused:")).collect();
    assert_eq!(refused.len(), 1, "the refusal's code is logged once: {refused:?}");
    assert!(refused[0].len() > "roster_freeze_refused:".len());
    // Cleared with the cause still there: it pauses again, the same cohort.
    assert!(tx(&db, |s| engine::clear_fault(s, &co.label, NOW)).unwrap().cleared);
    tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap();
    assert_eq!(tx(&db, |s| engine::cohort(s, 1)).unwrap().fault.as_deref(), Some(FREEZE_FAULT));
    // The cause corrected: the clear freezes the same roster.
    set_key(&db, &good);
    assert!(tx(&db, |s| engine::clear_fault(s, &co.label, NOW)).unwrap().cleared);
    assert_eq!(
        tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap(),
        Some(Boundary::Frozen { admitted: 1, overflow: 0, outside: true })
    );
    let after = tx(&db, |s| engine::cohort(s, 1)).unwrap();
    assert_eq!((after.phase.as_str(), after.fault), ("generating", None));
    assert_eq!(after.plan.entrants.len(), 1, "the admitted entrant is in the frozen roster");
}

/// L2 (A2/A3, admission half): admission's outage faults clear by
/// themselves once the step that raised them succeeds again: a framing
/// call the upload Worker answers, a commitment read that serves state,
/// and a verdict run that reaches the scorer. A different fault is never
/// cleared this way.
#[test]
fn admission_outage_faults_clear_on_the_next_success_of_their_step() {
    let fault = |db: &Sqlite| tx(db, |s| engine::cohort(s, 1)).unwrap().fault;
    let log_has = |db: &Sqlite, note: &str| {
        tx(db, |q| q.exec("SELECT 1 FROM phase_log WHERE seq = 1 AND note LIKE ?", &[s(format!("{note}%"))]))
            .unwrap()
            .len()
    };
    // Framing: the bound of cut-offs pauses the cohort; the next answer resumes it.
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    upload_all(&db, &m);
    let id = m.session();
    let mut t = NOW;
    let bound = tx(&db, window).unwrap().unwrap().failures_before_pause;
    for n in 0..=bound {
        assert!(tx(&db, |s| frame_begin(s, &id, t)).unwrap().is_some());
        t += backoff_ms(n + 1);
    }
    assert_eq!(fault(&db).as_deref(), Some(FRAMING_UNAVAILABLE));
    frame_only(&db, &m);
    assert_eq!(fault(&db), None, "the upload Worker answered");
    assert_eq!(log_has(&db, "fault_auto_cleared:framing_unavailable"), 1);
    // The commitment state: a served read resumes it.
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    upload_all(&db, &m);
    tx(&db, |s| waiting_on_state(s, &account(1), OPEN_BLOCK + 1, 6, NOW)).unwrap();
    assert_eq!(fault(&db).as_deref(), Some(commitments::STATE_UNAVAILABLE));
    tx(&db, |s| state_served(s, &account(1), OPEN_BLOCK + 1, NOW)).unwrap();
    assert_eq!(fault(&db), None);
    assert_eq!(log_has(&db, "fault_auto_cleared:commitment_state_unavailable"), 1);
    // Verdict platform failures: a run that reaches the scorer resumes it,
    // even when it refuses the artifact.
    let hotkey = hex::encode(account(1).0);
    let commitment = m.upload.commitment.hex();
    tx(&db, |s| platform_wait(s, &hotkey, &commitment, "lane_unavailable", 5, NOW)).unwrap();
    assert_eq!(fault(&db).as_deref(), Some("scorer_unavailable"));
    frame_ok(&db, &m);
    let runs = tx(&db, |s| claim_runs(s, &env(), 8, NOW + LANE_LEASE_MS)).unwrap();
    assert_eq!(runs.len(), 1);
    tx(&db, |s| record_run(s, &env(), &runs[0], &LaneOutcome::Platform("lane_unavailable".into()), 3, NOW)).unwrap();
    assert_eq!(fault(&db).as_deref(), Some("scorer_unavailable"), "a platform failure proves nothing");
    let runs = tx(&db, |s| claim_runs(s, &env(), 8, NOW + 2 * LANE_LEASE_MS)).unwrap();
    tx(&db, |s| record_run(s, &env(), &runs[0], &LaneOutcome::Refused("artifact_spec".into()), 3, NOW)).unwrap();
    assert_eq!(fault(&db), None, "the scorer answered");
    // Another fault is never cleared by these successes.
    tx(&db, |s| engine::fault(s, 1, FREEZE_FAULT, NOW)).unwrap();
    tx(&db, |s| state_served(s, &account(1), OPEN_BLOCK + 1, NOW)).unwrap();
    assert!(!tx(&db, |s| engine::auto_clear(s, 1, FREEZE_FAULT, "probe", NOW)).unwrap());
    assert_eq!(fault(&db).as_deref(), Some(FREEZE_FAULT));
}

/// L2 review r1 (item 2): an admission outage fault clears only once the
/// subject that raised it succeeds and no other subject still meets its
/// pause condition. Another subject's success leaves the fault in place.
#[test]
fn an_admission_outage_fault_is_cleared_by_its_own_subject_not_another() {
    let fault = |db: &Sqlite| tx(db, |s| engine::cohort(s, 1)).unwrap().fault;
    // Framing: session 1's attempts keep being cut off; session 2 frames.
    let db = db_with_open_cohort(4);
    let (m1, m2) = (miner(1, 3, 1, 0x11), miner(2, 4, 1, 0x12));
    for (m, b) in [(&m1, 1), (&m2, 2)] {
        m.commit(&db, OPEN_BLOCK + b, 0);
        m.create(&db);
        upload_all(&db, m);
    }
    let id = m1.session();
    let mut t = NOW;
    let bound = tx(&db, window).unwrap().unwrap().failures_before_pause;
    for n in 0..=bound {
        assert!(tx(&db, |s| frame_begin(s, &id, t)).unwrap().is_some());
        t += backoff_ms(n + 1);
    }
    assert_eq!(fault(&db).as_deref(), Some(FRAMING_UNAVAILABLE));
    frame_only(&db, &m2);
    assert_eq!(fault(&db).as_deref(), Some(FRAMING_UNAVAILABLE), "another session framed: session 1 is still cut off");
    frame_only(&db, &m1);
    assert_eq!(fault(&db), None, "session 1 framed");
    // Commitment state: hotkey 1's block keeps answering without state;
    // hotkey 2's read is served.
    let db = db_with_open_cohort(4);
    for (m, b) in [(&m1, 1), (&m2, 2)] {
        m.commit(&db, OPEN_BLOCK + b, 0);
        m.create(&db);
        upload_all(&db, m);
    }
    let bound = i64::from(tx(&db, window).unwrap().unwrap().failures_before_pause);
    tx(&db, |s| {
        s.exec("DELETE FROM commit_seen WHERE hotkey = ?", &[s_(hex::encode(account(1).0))])?;
        s.exec(
            "INSERT INTO commit_seen (hotkey, block, payload, seen_block, stage, absent_tries) VALUES (?, ?, '', ?, 'events', ?)",
            &[s_(hex::encode(account(1).0)), crate::sql::SqlValue::Integer((OPEN_BLOCK + 1) as i64),
              crate::sql::SqlValue::Integer((OPEN_BLOCK + 1) as i64), crate::sql::SqlValue::Integer(bound)],
        )
        .map(|_| ())
    })
    .unwrap();
    tx(&db, |s| waiting_on_state(s, &account(1), OPEN_BLOCK + 1, u32::try_from(bound).unwrap(), NOW)).unwrap();
    assert_eq!(fault(&db).as_deref(), Some(commitments::STATE_UNAVAILABLE));
    tx(&db, |s| state_served(s, &account(2), OPEN_BLOCK + 2, NOW)).unwrap();
    assert_eq!(fault(&db).as_deref(), Some(commitments::STATE_UNAVAILABLE), "another hotkey's state was served");
    tx(&db, |s| state_served(s, &account(1), OPEN_BLOCK + 1, NOW)).unwrap();
    assert_eq!(fault(&db), None, "hotkey 1's block was served");
    // Verdicts: hotkey 1's verdict keeps meeting platform failures; hotkey
    // 2's run reaches the scorer.
    let db = db_with_open_cohort(4);
    for (m, b) in [(&m1, 1), (&m2, 2)] {
        m.commit(&db, OPEN_BLOCK + b, 0);
        m.create(&db);
        upload_all(&db, m);
        frame_ok(&db, m);
    }
    let (h1, c1) = (hex::encode(account(1).0), m1.upload.commitment.hex());
    let n = tx(&db, window).unwrap().unwrap().failures_before_pause;
    tx(&db, |s| {
        s.exec("UPDATE adm_verdicts SET platform_tries = ? WHERE hotkey = ?", &[crate::sql::SqlValue::Integer(i64::from(n - 1)), s_(&h1)])
            .map(|_| ())
    })
    .unwrap();
    tx(&db, |s| platform_wait(s, &h1, &c1, "lane_unavailable", n - 1, NOW)).unwrap();
    assert_eq!(fault(&db).as_deref(), Some("scorer_unavailable"));
    let later = NOW + 30 * 24 * 3_600_000;
    let runs = tx(&db, |s| claim_runs(s, &env(), 8, later)).unwrap();
    let run_of = |h: &str| runs.iter().find(|r| r.hotkey == h).cloned().unwrap();
    tx(&db, |s| record_run(s, &env(), &run_of(&hex::encode(account(2).0)), &LaneOutcome::Admitted, 3, later)).unwrap();
    assert_eq!(fault(&db).as_deref(), Some("scorer_unavailable"), "another verdict reached the scorer");
    tx(&db, |s| record_run(s, &env(), &run_of(&h1), &LaneOutcome::Admitted, 3, later)).unwrap();
    assert_eq!(fault(&db), None, "hotkey 1's verdict reached the scorer");
}

/// L2 review r2 (item 5): an admission outage fault whose subject leaves
/// without succeeding (its session refused) clears on the next admission
/// step once no other subject meets the pause condition, with no operator;
/// while the subject still waits on its step, the step leaves it paused.
#[test]
fn an_admission_outage_fault_clears_once_its_subject_is_gone() {
    let fault = |db: &Sqlite| tx(db, |s| engine::cohort(s, 1)).unwrap().fault;
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    upload_all(&db, &m);
    let id = m.session();
    let mut t = NOW;
    let bound = tx(&db, window).unwrap().unwrap().failures_before_pause;
    for n in 0..=bound {
        assert!(tx(&db, |s| frame_begin(s, &id, t)).unwrap().is_some());
        t += backoff_ms(n + 1);
    }
    assert_eq!(fault(&db).as_deref(), Some(FRAMING_UNAVAILABLE));
    tx(&db, |s| crate::eval::admission::next_work(s, &cfg(), t)).unwrap();
    assert_eq!(fault(&db).as_deref(), Some(FRAMING_UNAVAILABLE), "the session still waits on framing");
    tx(&db, |s| s.exec("UPDATE adm_sessions SET state = 'refused' WHERE session = ?", &[s_(&id)]).map(|_| ())).unwrap();
    tx(&db, |s| crate::eval::admission::next_work(s, &cfg(), t)).unwrap();
    assert_eq!(fault(&db), None, "no subject meets the pause condition any more");
    let cleared = tx(&db, |q| {
        q.exec("SELECT 1 FROM phase_log WHERE seq = 1 AND note LIKE 'fault_auto_cleared:framing_unavailable (subject_settled)%'", &[])
    })
    .unwrap();
    assert_eq!(cleared.len(), 1);
}

fn s_(v: impl Into<String>) -> crate::sql::SqlValue {
    crate::sql::SqlValue::Text(v.into())
}

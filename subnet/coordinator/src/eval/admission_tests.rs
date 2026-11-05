//! Admission tests (contract v2): spec bounds, request authentication, the
//! session state machine and every request refusal, the header settle and
//! the R2 heads that record objects, the mint aggregate, the upload-path
//! faults and the canary, the close sweep, repair and audit, events and the
//! standing reconciliation, verdict rules (a)-(d), the cutoff and the freeze
//! (ordering, cap, overflow, overwritten commitments, registration changes,
//! the fresh-stock gate), and the template calendar.

use super::*;
use crate::eval::chain::EpochObservation;
use crate::eval::engine::{self, EvalConfig, Kick};
use crate::eval::plan::{self, AdmissionPlan, Ceilings, Incentive, PoolRef, ProbeRef};
use crate::sql::EVAL_COHORT_SCHEMA;
use crate::testing::{Sqlite, tx};
use crate::wire::{BlockRef, Id};
use slop_ninja_admission_wire::{RequestScope, request_message};
use slop_ninja_artifact_check::admission::HEADER_OUTCOME_SCHEMA;
use slop_ninja_protocol::submission::{SubmissionOpening, SubmissionUpload};
use slop_ninja_protocol::{ArtifactFile, ArtifactHeader, FileRole, MAX_ARTIFACT_HEADER_LEN, Nonce, TESTNET_GENESIS};

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
        upload_policy: Some(plan::UploadPolicyPlan::v1()),
        part_size: None,
        max_parts: None,
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
    p.reward_rule = "reward-v2".into();
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
        probe_cut: false,
        quant: Some(plan::tests::quant_block()),
    });
    p.admission = Some(admission_block(cap));
    p
}

/// Records every probe version `t` names, as a closed probe build would:
/// `calendar::install` names only probe versions the store recorded.
fn record_probes(db: &Sqlite, t: &crate::eval::calendar::Template) {
    for p in &t.plan.reward_v1.as_ref().unwrap().probes {
        db.conn
            .execute(
                "INSERT OR IGNORE INTO probe_versions (version, record_key, record_sha256, size, digest, seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                (p.version, &p.key, &p.sha256, p.size, hex::encode([0u8; 32])),
            )
            .unwrap();
    }
}

/// A fingerprint of `content` on probe version `version`, as a probe build
/// or a close records it.
fn record_fingerprint(db: &Sqlite, content: Digest, version: u32) {
    db.conn
        .execute(
            "INSERT INTO fingerprints (content, version, fingerprint, vector_key, seq) VALUES (?1, ?2, ?3, 'fp', 0)",
            (content.to_string(), version, hex::encode([0u8; 32])),
        )
        .unwrap();
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

/// A spec-v2-valid artifact header: admission reads no file byte, so the
/// files need only their digests and lengths (the graph at the served
/// class's exact length, the pinned tokenizer, small JSON role files);
/// `tag` varies the digests and so the committed content.
fn header(tag: u8) -> ArtifactHeader {
    let class = Version::V2.classes().iter().find(|c| Version::V2.serves(c)).unwrap();
    let d = |n: u8| Digest([tag ^ n; 32]);
    let tok = Digest(crate::eval::plan::parse_hex32(spec::TOKENIZER_SHA256).unwrap());
    let f = |name: &str, role, sha256, len| ArtifactFile { name: name.into(), role, sha256, len };
    ArtifactHeader {
        contract: spec::CONTRACT_ID.into(),
        files: vec![
            f("a-fp32band.onnx", FileRole::Graph, d(1), class.bytes),
            f("calibration.json", FileRole::Calibration, d(2), 100),
            f("contract.json", FileRole::OutputSchema, d(3), 100),
            f("heads.json", FileRole::Weights, d(4), 100),
            f("manifest.json", FileRole::OwnerMetadata, d(5), 100),
            f("tokenizer.json", FileRole::Tokenizer, tok, spec::TOKENIZER_BYTES),
        ],
    }
}

struct Miner {
    n: u8,
    uid: u16,
    registration_block: u64,
    header: ArtifactHeader,
    hbytes: Vec<u8>,
    upload: SubmissionUpload,
}

fn miner(n: u8, uid: u16, cohort: u64, tag: u8) -> Miner {
    miner_with(n, uid, cohort, tag, header(tag))
}

fn miner_with(n: u8, uid: u16, cohort: u64, tag: u8, header: ArtifactHeader) -> Miner {
    let hbytes = header.to_bytes().unwrap();
    let content = header.content().unwrap().digest().unwrap();
    let opening = SubmissionOpening {
        chain: chain(),
        cohort,
        hotkey: account(n),
        registration_block: 100 + u64::from(uid),
        content,
        nonce: Nonce([n.wrapping_mul(31).wrapping_add(tag); 32]),
    };
    let upload = SubmissionUpload::new(opening, &hbytes).unwrap();
    Miner {
        n,
        uid,
        registration_block: 100 + u64::from(uid),
        header,
        hbytes,
        upload,
    }
}

/// The signer's answer to a mint, as a test stands in for it.
fn minted(job: &MintJob) -> Vec<Minted> {
    job.entries
        .iter()
        .map(|e| Minted {
            url: format!("https://acct.r2.cloudflarestorage.com/slop-ninja-admission-testnet/x?len={}", e.len),
            checksum: "Y2hlY2tzdW0=".into(),
            signed_at: NOW / 1000,
            valid_secs: e.valid_secs,
        })
        .collect()
}

/// Runs a step's mint as the cohort object would (the signer answering).
fn drive(db: &Sqlite, step: Step) -> (Reply, Option<MintJob>) {
    match step {
        Step::Reply(r) => (r, None),
        Step::Mint(job) => {
            let r = tx(db, |s| finish_mint(s, &job, Ok(minted(&job)), NOW)).unwrap();
            (r, Some(job))
        }
        other => panic!("not a reply or a mint: {other:?}"),
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

    /// `POST /admission/v2/sessions/{session}/<op>`, signed by the owner.
    fn post(&self, db: &Sqlite, op: &str) -> Signed {
        self.signed(db, "POST", &format!("/admission/v2/sessions/{}/{op}", self.session()), &[])
    }

    fn create(&self, db: &Sqlite) -> (Reply, Option<MintJob>) {
        self.create_hinted(db, None)
    }

    /// Session creation carrying `x-sn-commitment-block` (`hint`).
    fn create_hinted(&self, db: &Sqlite, hint: Option<u64>) -> (Reply, Option<MintJob>) {
        let body = self.upload.to_bytes().unwrap();
        let mut r = self.signed(db, "POST", "/admission/v2/sessions", &body);
        r.body = Some(hex::encode(&body));
        r.commitment_block = hint;
        let step = tx(db, |s| Ok::<_, EvalError>(create(s, &chain(), &r, NOW))).unwrap();
        drive(db, step)
    }

    fn job(&self) -> HeaderJob {
        HeaderJob {
            session: self.session(),
            key: object_key(self.upload.opening.cohort, &self.upload.commitment, ObjectKind::Header),
            sha256: self.upload.header_sha256.0,
            len: self.upload.header_len,
            spec_id: spec_v2().spec_id,
        }
    }

    /// The upload Worker's outcome for this miner's header.
    fn checked(&self) -> HeaderSeen {
        HeaderSeen::Checked { etag: "etag-h".into(), outcome: outcome_of(&self.header) }
    }

    /// Applies a header settle that found `seen`.
    fn settle(&self, db: &Sqlite, seen: HeaderSeen) -> Settled {
        tx(db, |s| apply_header(s, &chain(), &self.job(), seen, NOW)).unwrap()
    }

    /// One probe per distinct file of the header.
    fn file_probes(&self) -> Vec<Probe> {
        let mut out: Vec<Probe> = Vec::new();
        for f in &self.header.files {
            if out.iter().any(|p| p.sha256 == f.sha256.0) {
                continue;
            }
            out.push(Probe {
                session: self.session(),
                kind: ObjectKind::File(f.sha256.0),
                sha256: f.sha256.0,
                len: f.len,
                key: object_key(self.upload.opening.cohort, &self.upload.commitment, ObjectKind::File(f.sha256.0)),
                etag: None,
            });
        }
        out
    }

    /// Every file present in R2 as committed, applied as `who` records it.
    fn put_files(&self, db: &Sqlite, who: Recorder) -> Applied {
        let heads: Vec<(Probe, Head)> = self.file_probes().into_iter().map(|p| {
            let h = stored(p.sha256, p.len, &etag_of(&p));
            (p, h)
        }).collect();
        tx(db, |s| apply_heads(s, &heads, who, NOW)).unwrap()
    }
}

fn etag_of(p: &Probe) -> String {
    format!("etag-{}", hex::encode(&p.sha256[..4]))
}

/// The upload Worker's checked outcome for `h`.
fn outcome_of(h: &ArtifactHeader) -> HeaderOutcome {
    HeaderOutcome {
        schema: HEADER_OUTCOME_SCHEMA.into(),
        result: HeaderResult::Checked {
            contract: h.contract.clone(),
            content: h.content().unwrap().digest().unwrap().hex(),
            files: h
                .files
                .iter()
                .map(|f| HeaderFile { name: f.name.clone(), role: f.role.tag(), sha256: hex::encode(f.sha256.0), len: f.len })
                .collect(),
        },
    }
}

/// An object R2 holds with its committed size and SHA-256, allowed
/// attributes, and entity tag `etag`.
fn stored(sha256: [u8; 32], len: u64, etag: &str) -> Head {
    Head::Present(Stored { size: len, sha256: Some(sha256), etag: etag.into(), clean: true })
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

fn stage_of(db: &Sqlite, id: &str) -> Option<String> {
    tx(db, |s| session_row(s, id)).unwrap().unwrap().stage
}

/// Uploads the header and every file (each settled from R2's heads as the
/// cohort object records them) and completes the session; returns the
/// completion's answer.
fn upload_all(db: &Sqlite, m: &Miner) -> Reply {
    assert_eq!(m.settle(db, m.checked()), Settled::Recorded);
    m.put_files(db, Recorder::Upload);
    let req = m.post(db, "complete");
    let step = tx(db, |s| Ok::<_, EvalError>(complete(s, &chain(), &req, &m.session()))).unwrap();
    drive(db, step).0
}

/// The verifying session's files manifest written: its verdict is pending.
fn frame_ok(db: &Sqlite, m: &Miner) {
    let id = m.session();
    assert!(tx(db, |s| manifest_due(s, &id)).unwrap().is_some(), "the manifest is due");
    tx(db, |s| manifest_written(s, &env(), &id, NOW)).unwrap();
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

/// The boundary at `cutoff` arrives, its canary passes, and the close sweep
/// runs to the recorded close (every unrecorded object headed absent).
fn close_at(db: &Sqlite, cutoff: u64) {
    epoch(db, 12, cutoff, NOW + 2);
    assert!(tx(db, |s| canary_begin(s, NOW + 2)).unwrap().is_some(), "the close's canary is due");
    assert!(tx(db, |s| canary_done(s, Ok(()), NOW + 3)).unwrap(), "the window closes at this boundary");
    sweep_close(db, |_| Head::Absent);
}

/// Runs the close sweep to the recorded close, each head answered by `head`.
fn sweep_close(db: &Sqlite, head: impl Fn(&Probe) -> Head) {
    sweep_close_at(db, NOW + 4, head)
}

/// [`sweep_close`] at `at`.
fn sweep_close_at(db: &Sqlite, at: u64, head: impl Fn(&Probe) -> Head) {
    while let Some((seq, probes)) = tx(db, |s| close_begin(s, at)).unwrap() {
        let heads: Vec<(Probe, Head)> = probes.into_iter().map(|p| {
            let h = head(&p);
            (p, h)
        }).collect();
        let applied = tx(db, |s| apply_heads(s, &heads, Recorder::Close, at)).unwrap();
        assert!(!applied.platform, "a close head failed");
        tx(db, |s| close_step_done(s, seq, &applied, at)).unwrap();
    }
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
    bad.admission.as_mut().unwrap().part_size = Some(64 << 20);
    assert!(
        plan::CohortPlan::parse(&serde_json::to_vec(&bad).unwrap(), cfg().ceilings).is_err(),
        "admission v2 has no part size"
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
    let r = signed_by(1, 1_010, "GET", "/admission/v2/sessions/ab", &[]);
    let (who, _) = authenticate(&chain(), 1_000, 360, &r).unwrap();
    assert_eq!(who, account(1));
    // Another path, method or chain: the signature does not verify.
    let mut other = r.clone();
    other.path = "/admission/v2/sessions/cd".into();
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
    assert_eq!(fx.map(|j| j.entries.len()), Some(1), "the header URL is minted");
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
    wrong_chain.upload = SubmissionUpload::new(wrong_chain.upload.opening.clone(), &wrong_chain.hbytes).unwrap();
    assert_eq!(code(&wrong_chain.create(&db).0), "wrong_chain");
    let theirs = miner(3, 5, 1, 0x13);
    let body = theirs.upload.to_bytes().unwrap();
    let mut req = signed_by(
        2,
        tx(&db, cursor).unwrap().unwrap() + 1,
        "POST",
        "/admission/v2/sessions",
        &body,
    );
    req.body = Some(hex::encode(&body));
    let (r, _) = drive(&db, tx(&db, |s| Ok::<_, EvalError>(create(s, &chain(), &req, NOW))).unwrap());
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
    regen.upload = SubmissionUpload::new(regen.upload.opening.clone(), &regen.hbytes).unwrap();
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
    // A header beyond the protocol's bound is no envelope at all (the record
    // refuses it), so the gate's bound and the authority's agree.
    let mut big = miner(5, 7, 1, 0x15);
    big.upload.header_len = MAX_ARTIFACT_HEADER_LEN + 1;
    assert!(big.upload.to_bytes().is_err());
    // Each refusal after the signature verifies is the hotkey's last refusal.
    let path = format!("/admission/v2/cohorts/1/hotkeys/{}", crate::chain::ss58(account(1).0));
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
fn one_artifact_per_hotkey_and_supersession() {
    let db = db_with_open_cohort(2);
    let a = miner(1, 3, 1, 0x11);
    a.commit(&db, OPEN_BLOCK + 1, 0);
    assert_eq!(a.create(&db).0.status, 201);
    // Another envelope for the same commitment (another header with the
    // same committed content: an owner-metadata file of another length).
    let mut h = header(0x11);
    h.files[4].len += 1;
    let b = miner_with(1, 3, 1, 0x11, h);
    assert_eq!(b.upload.commitment, a.upload.commitment);
    let (r, _) = b.create(&db);
    assert_eq!(
        (code(&r).as_str(), r.body["error"]["detail"]["session"].as_str()),
        ("second_artifact", Some(a.session().as_str()))
    );
    // A newer commitment supersedes the live session; its objects are
    // queued for deletion at once.
    let c = miner(1, 3, 1, 0x31);
    c.commit(&db, OPEN_BLOCK + 2, 0);
    assert_eq!(c.create(&db).0.status, 201);
    assert_eq!(state_of(&db, &a.session()).0, "superseded");
    let due = tx(&db, |s| object_deletes_due(s, 8, NOW)).unwrap();
    assert_eq!(due, vec![(a.job().key, None)]);
    // The alarm sweep of the new session's header first, then the deletes.
    assert_eq!(tx(&db, |s| next_work(s, &cfg(), NOW)).unwrap(), Some(engine::Work::AdmissionSweep));
    tx(&db, |s| sweep_begin(s, NOW)).unwrap();
    assert_eq!(tx(&db, |s| next_work(s, &cfg(), NOW)).unwrap(), Some(engine::Work::AdmissionCleanup));
    // A delete the bucket did not confirm backs off, so the step reaches its
    // other work instead of choosing the clean-up again; due after the backoff.
    tx(&db, |s| object_delete_failed(s, &due[0].0, NOW)).unwrap();
    assert!(tx(&db, |s| object_deletes_due(s, 8, NOW)).unwrap().is_empty());
    let later = NOW + backoff_ms(1);
    assert_eq!(tx(&db, |s| object_deletes_due(s, 8, later)).unwrap().len(), 1);
    tx(&db, |s| object_delete_failed(s, &due[0].0, later)).unwrap();
    assert!(tx(&db, |s| object_deletes_due(s, 8, later + backoff_ms(1))).unwrap().is_empty(), "the backoff grows");
    tx(&db, |s| object_deleted(s, &due[0].0)).unwrap();
    assert!(tx(&db, |s| object_deletes_due(s, 8, crate::wire::MAX_SAFE_INTEGER)).unwrap().is_empty());
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
        cohort: 7,
        hotkey: "5Fhot".into(),
    };
    let ok = [
        (op::CREATE, f("POST", "/admission/v2/sessions".into())),
        (op::READ, f("GET", format!("/admission/v2/sessions/{sid}"))),
        (op::URLS, f("POST", format!("/admission/v2/sessions/{sid}/urls"))),
        (op::COMPLETE, f("POST", format!("/admission/v2/sessions/{sid}/complete"))),
        (op::REPAIR, f("POST", format!("/admission/v2/sessions/{sid}/repair"))),
        (op::HOTKEY, f("GET", "/admission/v2/cohorts/7/hotkeys/5Fhot".into())),
    ];
    for (o, fw) in &ok {
        assert!(forward_matches(o, fw), "{o}");
    }
    let other = "cd".repeat(32);
    let bad = [
        (op::READ, f("GET", format!("/admission/v2/sessions/{other}"))),
        (op::URLS, f("POST", format!("/admission/v2/sessions/{other}/urls"))),
        (op::URLS, f("POST", format!("/admission/v2/sessions/{sid}/complete"))),
        (op::REPAIR, f("POST", format!("/admission/v2/sessions/{sid}/urls"))),
        (op::HOTKEY, f("GET", "/admission/v2/cohorts/8/hotkeys/5Fhot".into())),
        (op::COMPLETE, f("POST", format!("/admission/v2/sessions/{sid}"))),
        (op::CREATE, f("POST", "/admission/v2/sessions?x".into())),
        (op::CREATE, f("POST", "/admission/v1/sessions".into())),
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
    // (a) one run per hotkey and commitment: a manifest that is not the
    // package the commitment's run already judged is refused without a run.
    let m4 = miner(4, 6, 1, 0x14);
    m4.commit(&db, OPEN_BLOCK + 4, 0);
    m4.create(&db);
    upload_all(&db, &m4);
    tx(&db, |s| {
        s.exec(
            "INSERT INTO adm_verdicts (hotkey, commitment, package_sha256, manifest_key, label, scorer_binary, spec_sha256, state, updated_ms)
             VALUES (?, ?, 'another', 'k', 'testnet-c1', 'b', 's', 'refused', 1)",
            &[SqlValue::Text(hex::encode(account(4).0)), SqlValue::Text(m4.upload.commitment.hex())],
        )
        .map(|_| ())
    })
    .unwrap();
    tx(&db, |s| manifest_written(s, &env(), &m4.session(), NOW)).unwrap();
    let row = tx(&db, |s| session_row(s, &m4.session())).unwrap().unwrap();
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
        "chunk rejected",
        "missing chunks",
        crate::eval::scorer_client::OBJECT_MISSING,
        crate::eval::scorer_client::OBJECT_CHANGED,
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
    // After the close: URLs and completions are refused, new sessions too.
    let req = late.post(&db, "urls");
    let step = tx(&db, |s| Ok::<_, EvalError>(urls(s, &chain(), &req, &late.session()))).unwrap();
    assert_eq!(code(&drive(&db, step).0), "after_cutoff");
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
    let header_key = first.object_key.as_deref().unwrap();
    assert!(header_key.starts_with("admission/v2/c1/") && header_key.ends_with("/h"), "{header_key}");
    assert_eq!(c.plan.pool.pool_id, "fresh");
    assert!(tx(&db, |s| has_outside_entrant(s, 1)).unwrap());
    assert!(tx(&db, |s| crate::eval::v3::fresh_allowed(s, &c)).unwrap());
    // The stored plan is the frozen one, and validates.
    plan::CohortPlan::parse(&serde_json::to_vec(&c.plan).unwrap(), cfg().ceilings).unwrap();
    assert_eq!(c.plan_sha256, engine::sha_hex(&serde_json::to_vec(&c.plan).unwrap()));
    // Objects of the sessions that ended without admission are queued for
    // deletion: overflow, overwritten and registration (seven objects each)
    // and the incomplete one's header.
    tx(&db, |s| next_work(s, &cfg(), NOW)).unwrap();
    let due = tx(&db, |s| object_deletes_due(s, 100, crate::wire::MAX_SAFE_INTEGER)).unwrap();
    assert_eq!(due.len(), 3 * 7 + 1);
    // The closure names the admitted objects for publication.
    let v = closure_artifacts(&c.plan);
    assert_eq!(v["artifacts"].as_array().unwrap().len(), 2);
    // The admission status is content-free.
    let st = tx(&db, status).unwrap();
    assert_eq!(st["cohorts"][0]["state"], "frozen");
    assert!(!st.to_string().contains(&hex::encode(ms[2].upload.opening.nonce.0)));
}

/// The v2 path through to the seal's inputs: the frozen plan names each
/// admitted entrant's files manifest v2 (written by the `manifest` step),
/// from which the seal takes the entrant's content (the opening's) and its
/// artifact digest (the SHA-256 of the header the miner uploaded and R2
/// verified), the digest the package binds and a published header must
/// match (`check_published_artifact`, run on every sim package in
/// `eval::tests`).
#[test]
fn a_frozen_v2_entrant_seals_under_its_uploaded_header_digest() {
    let db = db_with_open_cohort(2);
    let ms: Vec<Miner> = (1..=2u8).map(|n| miner(n, u16::from(n) + 2, 1, 0x20 + n)).collect();
    let mut manifests = std::collections::BTreeMap::new();
    for (k, m) in ms.iter().enumerate() {
        m.commit(&db, OPEN_BLOCK + 3 + k as u64, 0);
        assert_eq!(m.create(&db).0.status, 201);
        upload_all(&db, m);
        let id = m.session();
        let (key, bytes) = tx(&db, |s| manifest_due(s, &id)).unwrap().unwrap();
        manifests.insert(key, bytes);
        frame_ok(&db, m);
    }
    admit_all(&db);
    let cutoff = OPEN_BLOCK + 360;
    close_at(&db, cutoff);
    for (k, m) in ms.iter().enumerate() {
        let payload = m.upload.opening.chain_payload().to_vec();
        snapshot(&db, cutoff, m, Some(OPEN_BLOCK + 3 + k as u64), &payload, Some(m.uid), Some(m.registration_block));
    }
    let b = tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap().unwrap();
    assert!(matches!(b, Boundary::Frozen { admitted: 2, .. }), "{b:?}");
    let c = tx(&db, |s| engine::cohort(s, 1)).unwrap();
    assert_eq!(c.plan.entrants.len(), 2);
    for a in &c.plan.entrants {
        let m = ms.iter().find(|m| a.owner.hotkey == format!("0x{}", hex::encode(account(m.n).0))).unwrap();
        let bytes = &manifests[&a.manifest_key];
        assert_eq!(engine::sha_hex(bytes), a.package_sha256, "the plan binds the written manifest");
        // The seal takes the header digest admission recorded (R2 verified
        // it): no hash on our side; it is the canonical record's digest.
        let recorded = tx(&db, |s| recorded_header_sha256(s, &a.package_sha256)).unwrap();
        assert_eq!(recorded, Some(m.upload.header_sha256));
        let d = crate::eval::v3::artifact_digests(bytes, recorded).unwrap();
        assert_eq!(d.content.0, m.upload.opening.content);
        assert_eq!(d.artifact.0, m.upload.header_sha256, "the artifact digest is the uploaded header's");
        assert_eq!(crate::eval::v3::artifact_digests(bytes, None).unwrap(), d, "the same as the record's own digest");
        let vh = slop_ninja_protocol::VerifiedHeader::from_stored(&m.hbytes, m.upload.header_sha256).unwrap();
        assert_eq!((vh.sha256, vh.content), (d.artifact.0, d.content.0));
    }
    // A files manifest v1 (whole-object parts) is refused by the seal.
    let v1 = br#"{"schema":"slop_ninja_a_artifact_files_v1","profile_id":"x","contract":"c","graph":"g","files":[]}"#;
    assert_eq!(crate::eval::v3::artifact_digests(v1, None), Err("artifact manifest schema"));
    assert_eq!(tx(&db, |s| recorded_header_sha256(s, &"00".repeat(32))).unwrap(), None, "a reference: no recorded digest");
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
    // Replaced (and logged) after the open, with the reader behind the cursor.
    let other = miner(1, 3, 1, 0x42);
    other.commit(&db, OPEN_BLOCK + 2, 0);
    polled(&db, OPEN_BLOCK);
    heartbeat(&db, 5);
    assert!(matches!(m.settle(&db, m.checked()), Settled::Refused(None)));
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
    record_probes(&db, &t);
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
    assert_eq!(
        (a.upload_policy.id.as_str(), a.upload_policy.b_min, a.upload_policy.skew_secs),
        (policy::POLICY_ID, policy::B_MIN, policy::SKEW_SECS)
    );
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
    record_probes(&db, &t);
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
    record_probes(&db, &t);
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
    record_probes(&db, &t);
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
    let c = miner(1, 3, 1, 0x31);
    c.commit(&db, OPEN_BLOCK + 2, 0);
    c.create(&db);
    let due = tx(&db, |s| object_deletes_due(s, 8, NOW)).unwrap();
    assert_eq!(due.len(), 1);
    // The paced work (the alarm sweep, the reconciliation) is done for now.
    tx(&db, |s| sweep_begin(s, NOW)).unwrap();
    tx(&db, |s| reconcile_page(s, &[], None, NOW)).unwrap();
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
            "UPDATE adm_sessions SET model_sha256 = 'not-a-digest' WHERE session = ?",
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
    tx(&db, |s| next_work(s, &cfg(), NOW)).unwrap();
    assert_eq!(tx(&db, |s| object_deletes_due(s, 100, crate::wire::MAX_SAFE_INTEGER)).unwrap().len(), 7);
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
    assert_eq!(ADMISSION_SPECS, &[spec::SPEC_ID_V2, spec::SPEC_ID_V3, spec::SPEC_ID_V4]);
    // The v3 object bound fits one URL life and one PUT (no part count);
    // spec v4's is v3's (it admits what v3 admits).
    assert!(b3.object_bytes <= policy::MAX_L_AT_B_MIN.min(policy::R2_SINGLE_PUT_MAX));
    let b4 = SpecBounds::of(Version::V4).unwrap();
    assert_eq!((b4.graph_bytes, b4.files_bytes, b4.object_bytes), (b3.graph_bytes, b3.files_bytes, b3.object_bytes));
    let mut v4p = v3p.clone();
    let a = v4p.admission.as_mut().unwrap();
    a.artifact_spec = b4.spec_id.into();
    a.artifact_spec_sha256 = b4.spec_sha256.clone();
    v4p.fanout.as_mut().unwrap().score.artifact_spec = Some(b4.spec_id.into());
    let parsed = plan::CohortPlan::parse(&serde_json::to_vec(&v4p).unwrap(), cfg().ceilings).unwrap();
    assert_eq!(parsed.load_spec_sha256(), Some(b4.spec_sha256.as_str()));
    // A spec no admission run loads by is refused.
    let mut v5 = v3p;
    v5.admission.as_mut().unwrap().artifact_spec = "slop_ninja_a_artifact_spec_v5".into();
    assert!(plan::CohortPlan::parse(&serde_json::to_vec(&v5).unwrap(), cfg().ceilings).is_err());
    // A plan with a fresh pool must name the operator's coldkeys too.
    let mut p = admission_plan(1, 2);
    p.admission.as_mut().unwrap().operator_coldkeys.clear();
    assert!(plan::CohortPlan::parse(&serde_json::to_vec(&p).unwrap(), cfg().ceilings).is_err());
}

#[test]
fn an_admission_run_names_its_cohorts_spec_to_the_scorer() {
    let limits = AdmitLimits { artifact_max_bytes: 1 << 30, load_wait_ms: 600_000, inactivity_ms: 60_000, doc_timeout_ms: 120_000 };
    for v in [Version::V2, Version::V3, Version::V4] {
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
    tx(&db, |s| operator_refuse(s, &id, "drill", NOW)).unwrap();
    // Another envelope for the ended session's commitment: refused, the
    // miner commits again (sessions stay bounded by commitments).
    let mut h = header(0x11);
    h.files[4].len += 1;
    let b = miner_with(1, 3, 1, 0x11, h);
    assert_eq!(code(&b.create(&db).0), "second_artifact");
    // After a newer commitment, the earlier envelope still answers 200.
    let c = miner(1, 3, 1, 0x31);
    c.commit(&db, OPEN_BLOCK + 2, 0);
    let (r, job) = a.create(&db);
    assert_eq!((r.status, r.body["session"].as_str(), job.is_none()), (200, Some(id.as_str()), true));
}

#[test]
fn a_header_settle_waits_while_the_hotkeys_commitment_is_being_logged() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
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
    let Settled::Wait(e) = m.settle(&db, m.checked()) else { panic!("waits, never refuses") };
    assert_eq!(e.code, ErrorCode::CommitmentNotFound);
    assert_eq!(state_of(&db, &m.session()), ("uploading".into(), None));
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
                "INSERT INTO adm_sessions (session, seq, cohort, hotkey, upload, commitment, content, header_sha256, header_len,
                    state, refusal, created_ms, updated_ms)
                 VALUES (?, 1, 1, ?, ?, 'c', 'c', 'a', 1, 'superseded', NULL, ?, ?)",
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
        "/admission/v2/cohorts/1/hotkeys/{}",
        crate::chain::ss58(key(1).public())
    );
    let req = m.signed(&db, "GET", &path, &[]);
    let hk = crate::chain::ss58(key(1).public());
    let r = tx(&db, |s| Ok::<_, EvalError>(hotkey_view(s, &chain(), &req, 1, &hk))).unwrap();
    let limit = slop_ninja_admission_wire::response_limit();
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
    record_probes(&db, &t);
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
    // The promoted entrant's fingerprint on the cohort's probe version is
    // pending at its close; the reference's is recorded.
    let st = V3State {
        reference: Some(dg(0x01)),
        entrants: vec![dg(0x02)],
        promoted: Some(0),
        pending_fingerprints: vec![crate::eval::v3::PendingFingerprint {
            content: H(Digest([0x02; 32])),
            version: 1,
            fingerprint: H(Digest([0; 32])),
            vector_key: "fp".into(),
        }],
        ..Default::default()
    };
    record_fingerprint(&db, Digest([0x01; 32]), 1);
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
    // Without the promoted entrant's fingerprint on a version the template
    // names, the calendar pauses instead of queuing a cohort that could not
    // package.
    let bare = V3State { pending_fingerprints: Vec::new(), ..st.clone() };
    tx(&db, |s| {
        s.exec("UPDATE cohorts SET v3 = ? WHERE seq = 1", &[SqlValue::Text(serde_json::to_string(&bare).unwrap())]).map(|_| ())
    })
    .unwrap();
    assert!(tx(&db, |s| crate::eval::calendar::due_any(s, 0)).unwrap().is_none());
    let view = tx(&db, crate::eval::calendar::view).unwrap();
    assert!(view["template"]["last_error"].as_str().unwrap().starts_with("template_probe_unfingerprinted"));
    tx(&db, |s| crate::eval::calendar::set_paused(s, false)).unwrap();
    // Closed: the chain (as `close` leaves it) is followed, and a reinstall
    // of the template never resets it (the close recorded the promoted
    // entrant's fingerprint).
    record_fingerprint(&db, Digest([0x02; 32]), 1);
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

/// Review round 2, finding 11: the key-holding side hashes a forwarded
/// creation body itself; a body without the upload Worker's digest, or a
/// body that is not the signed one, is refused.
#[test]
fn a_creation_body_is_hashed_by_the_authority() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    let body = m.upload.to_bytes().unwrap();
    let mut r = m.signed(&db, "POST", "/admission/v2/sessions", &body);
    r.body = Some(hex::encode(&body));
    let mut no_digest = r.clone();
    no_digest.body_sha256 = None;
    let (reply, fx) = drive(&db, tx(&db, |s| Ok::<_, EvalError>(create(s, &chain(), &no_digest, NOW))).unwrap());
    assert_eq!((code(&reply).as_str(), fx), ("content_digest_mismatch", None));
    let mut other = r.clone();
    let mut changed = body.clone();
    if let Some(b) = changed.last_mut() {
        *b ^= 1;
    }
    other.body = Some(hex::encode(&changed));
    let (reply, _) = drive(&db, tx(&db, |s| Ok::<_, EvalError>(create(s, &chain(), &other, NOW))).unwrap());
    assert_eq!(code(&reply), "content_digest_mismatch");
    let (reply, fx) = drive(&db, tx(&db, |s| Ok::<_, EvalError>(create(s, &chain(), &r, NOW))).unwrap());
    assert_eq!(reply.status, 201, "{}", reply.body);
    assert!(fx.is_some());
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
        let rows = tx(db, |q| q.exec("SELECT model_sha256 FROM adm_sessions WHERE session = ?", &[s(d.session())])).unwrap();
        crate::sql::text(&rows[0], 0).unwrap().to_owned()
    };
    let good = key_of(&db);
    let set_key = |db: &Sqlite, k: &str| {
        tx(db, |q| q.exec("UPDATE adm_sessions SET model_sha256 = ? WHERE session = ?", &[s(k), s(d.session())]).map(|_| ()))
            .unwrap()
    };
    set_key(&db, "not-a-digest");
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
/// themselves once the step that raised them succeeds again: a commitment
/// read that serves state, and a verdict run that reaches the scorer. A different fault is never
/// cleared this way.
#[test]
fn admission_outage_faults_clear_on_the_next_success_of_their_step() {
    let fault = |db: &Sqlite| tx(db, |s| engine::cohort(s, 1)).unwrap().fault;
    let log_has = |db: &Sqlite, note: &str| {
        tx(db, |q| q.exec("SELECT 1 FROM phase_log WHERE seq = 1 AND note LIKE ?", &[s(format!("{note}%"))]))
            .unwrap()
            .len()
    };
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
    let (m1, m2) = (miner(1, 3, 1, 0x11), miner(2, 4, 1, 0x12));
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

/// Cleanup review (11-05): a verifying hotkey's resolution held at the block
/// hash stage by a lagging node (unknown-block refusals and `null` hashes)
/// pauses the admitting cohort at the plan's bound, like the events and
/// registration stages. The admission step's recheck keeps that pause while
/// the hash stage still waits, and the read that serves the hash clears it.
#[test]
fn a_hash_stage_a_lagging_node_holds_pauses_until_the_hash_is_served() {
    let fault = |db: &Sqlite| tx(db, |s| engine::cohort(s, 1)).unwrap().fault;
    let rt = crate::chain::ChainRuntime::parse(
        include_bytes!("../../../fixtures/runtime-486/testnet-486.v15.scale"),
        486,
        1,
    )
    .unwrap();
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    let block = OPEN_BLOCK + 1;
    m.commit(&db, block, 0);
    m.create(&db);
    upload_all(&db, &m);
    let who = hex::encode(account(1).0);
    // The reader saw the commitment and still resolves its block hash.
    tx(&db, |s| {
        s.exec("DELETE FROM commit_seen WHERE hotkey = ?", &[s_(&who)])?;
        s.exec(
            "INSERT INTO commit_seen (hotkey, block, payload, seen_block, stage) VALUES (?, ?, '', ?, 'hash')",
            &[s_(&who), crate::sql::SqlValue::Integer(block as i64), crate::sql::SqlValue::Integer(block as i64)],
        )
        .map(|_| ())
    })
    .unwrap();
    let hash_read = |reply: serde_json::Value| {
        vec![commitments::ReadResult {
            tag: format!("hash@{who}@{block}"),
            id: 1,
            reply: Some(reply.to_string()),
        }]
    };
    let not_imported = hash_read(serde_json::json!({"jsonrpc": "2.0", "id": 1,
        "error": {"code": 4003, "message": "Client error: UnknownBlock: Header was not found in the database"}}));
    let null_hash = hash_read(serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": null}));
    let bound = tx(&db, window).unwrap().unwrap().failures_before_pause;
    for n in 1..=bound {
        let replies = if n % 2 == 1 { &not_imported } else { &null_hash };
        tx(&db, |s| commitments::ingest(s, Some(&rt), 601, replies, NOW)).unwrap();
        if n < bound {
            assert_eq!(fault(&db), None, "answer {n} is below the bound");
        }
    }
    assert_eq!(fault(&db).as_deref(), Some(commitments::STATE_UNAVAILABLE));
    let stage = |db: &Sqlite| {
        tx(db, |s| s.exec("SELECT stage, absent_tries FROM commit_seen WHERE hotkey = ?", &[s_(&who)]))
            .map(|r| (text(&r[0], 0).unwrap().to_owned(), int(&r[0], 1).unwrap()))
            .unwrap()
    };
    assert_eq!(stage(&db), ("hash".into(), i64::from(bound)));
    // The admission step's recheck: the hash stage still waits.
    tx(&db, |s| outage_recheck(s, NOW + 1)).unwrap();
    assert_eq!(fault(&db).as_deref(), Some(commitments::STATE_UNAVAILABLE), "the hash stage still waits");
    // A node serves the hash: the resolution moves on and the pause clears.
    let served = hash_read(serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": Id([0xc1; 32]).to_hex()}));
    tx(&db, |s| commitments::ingest(s, Some(&rt), 601, &served, NOW + 2)).unwrap();
    assert_eq!(stage(&db), ("events".into(), 0));
    assert_eq!(fault(&db), None, "the hash was served");
    let cleared = tx(&db, |q| {
        q.exec(
            "SELECT 1 FROM phase_log WHERE seq = 1 AND note LIKE ?",
            &[s_("fault_auto_cleared:commitment_state_unavailable%")],
        )
    })
    .unwrap();
    assert_eq!(cleared.len(), 1);
}

/// Snapshot-retry review (11-05): at the cutoff snapshot a lagging node's
/// answers (unknown-block refusals, answers without state) pause the
/// closing cohort only at the plan's pause bound, as a resolution stage's
/// do, with a growing backoff and the archive endpoint between them; the
/// first read that answers resumes the same cohort. An explicit
/// discarded-state refusal still pauses it at once.
#[test]
fn a_lagging_node_at_the_snapshot_pauses_the_closing_cohort_only_at_the_bound() {
    use crate::chain::Value as V;
    let fault = |db: &Sqlite| tx(db, |s| engine::cohort(s, 1)).unwrap().fault;
    let metadata: &[u8] = include_bytes!("../../../fixtures/runtime-486/testnet-486.v15.scale");
    let rt = crate::chain::ChainRuntime::parse(metadata, 486, 1).unwrap();
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    upload_all(&db, &m);
    frame_ok(&db, &m);
    admit_all(&db);
    let cutoff = OPEN_BLOCK + 360;
    close_at(&db, cutoff);
    let (c, wanted) = tx(&db, snapshot_needs).unwrap().unwrap();
    assert_eq!((c, wanted.clone()), (cutoff, vec![account(1)]));
    // The snapshot block's hash is read and the stored runtime wrote it.
    let snap = Id([0x5b; 32]);
    let entry = rt.core().storage_entry("System", "LastRuntimeUpgrade").unwrap();
    let mut marker = Vec::new();
    rt.core()
        .encode_id(
            entry.value_type,
            &V::record(vec![("spec_version".into(), V::Int(486)), ("spec_name".into(), V::str("node-subtensor"))]),
            &mut marker,
        )
        .unwrap();
    tx(&db, |s| {
        commitments::store_runtime(s, 486, 1, metadata, cutoff - 1, &hex::encode(&marker))?;
        s.exec(
            "INSERT INTO adm_snapshot_blocks (cutoff_block, block_hash) VALUES (?, ?)",
            &[bind_u64(cutoff)?, s_(snap.to_hex())],
        )?;
        Ok::<_, EvalError>(())
    })
    .unwrap();
    let needs = commitments::Needs { poll: false, snapshot: Some((cutoff, wanted)) };
    let at = BlockRef { number: cutoff + 2, hash: Id([0x0c; 32]) };
    let plan_at = |db: &Sqlite, now: u64| tx(db, |s| commitments::plan(s, Some(&rt), 601, Some(at), &needs, 1_000, now)).unwrap();
    let reply_to = |round: &commitments::Round, body: serde_json::Value| -> Vec<commitments::ReadResult> {
        round
            .reads
            .iter()
            .map(|r| {
                let mut v = body.clone();
                v["jsonrpc"] = "2.0".into();
                v["id"] = r.id.into();
                commitments::ReadResult { tag: r.tag.clone(), id: r.id, reply: Some(v.to_string()) }
            })
            .collect()
    };
    let not_imported = serde_json::json!({"error": {"code": 4003, "message": "Client error: UnknownBlock: Header was not found in the database"}});
    let no_state = serde_json::json!({"result": [{"block": snap.to_hex(), "changes": []}]});
    let tries = |db: &Sqlite| tx(db, commitments::status).unwrap()["snapshot_absent_tries"].as_i64().unwrap();
    let bound = tx(&db, window).unwrap().unwrap().failures_before_pause;
    assert!(bound > 1, "the plan's bound leaves room for a lagging node");
    let mut now = NOW + 10;
    for n in 1..=bound {
        let round = plan_at(&db, now);
        assert_eq!(round.reads.len(), 1, "answer {n}: the snapshot is due");
        assert!(round.reads[0].tag.starts_with("snapvals@"));
        assert_eq!(round.reads[0].archive, n > 1, "answer {n}: archive after the first answer without state");
        let body = if n % 2 == 1 { &not_imported } else { &no_state };
        tx(&db, |s| commitments::ingest(s, Some(&rt), 601, &reply_to(&round, body.clone()), now)).unwrap();
        assert_eq!(tries(&db), i64::from(n));
        if n < bound {
            assert_eq!(fault(&db), None, "answer {n} is below the bound");
            assert!(plan_at(&db, now + backoff_ms(n) - 1).reads.is_empty(), "answer {n}: backing off");
        }
        now += backoff_ms(n);
    }
    assert_eq!(fault(&db).as_deref(), Some(commitments::SNAPSHOT_DISCARDED), "paused at the bound");
    assert_eq!(tx(&db, commitments::status).unwrap()["state_discarded"], 0, "nothing was discarded");
    assert_eq!(tx(&db, |s| freeze_wait(s, 1)).unwrap(), Some("commitment snapshot pending"));
    // The snapshot keeps being read while paused; the first answer resumes
    // the same cohort and ends the run.
    let round = plan_at(&db, now);
    assert!(round.reads[0].tag.starts_with("snapvals@") && round.reads[0].archive);
    let served = serde_json::json!({"result": [{"block": snap.to_hex(), "changes": [[commitments::marker_key(), format!("0x{}", hex::encode(&marker))]]}]});
    tx(&db, |s| commitments::ingest(s, Some(&rt), 601, &reply_to(&round, served), now)).unwrap();
    assert_eq!(fault(&db), None, "the snapshot was served");
    assert_eq!(tries(&db), 0);
    let round = plan_at(&db, now);
    assert!(!round.reads[0].archive, "read from the RPC endpoint again");
    // An explicit discarded-state refusal pauses at once and is counted.
    let gone = serde_json::json!({"error": {"code": 4003, "message": "UnknownBlock: State already discarded for 0x5b"}});
    tx(&db, |s| commitments::ingest(s, Some(&rt), 601, &reply_to(&round, gone), now)).unwrap();
    assert_eq!(fault(&db).as_deref(), Some(commitments::SNAPSHOT_DISCARDED), "discarded pauses at once");
    assert_eq!(tx(&db, commitments::status).unwrap()["state_discarded"], 1);
    // The registration-block stage of the snapshot (`snapregs`) treats a
    // lagging node the same way. The values stage answers with the miner's
    // uid (and no commitment), which resumes the cohort and leaves only
    // its registration block to read.
    let encode = |pallet: &str, item: &str, v: &V| {
        let entry = rt.core().storage_entry(pallet, item).unwrap();
        let mut out = Vec::new();
        rt.core().encode_id(entry.value_type, v, &mut out).unwrap();
        format!("0x{}", hex::encode(out))
    };
    let (ck, uk) = (
        commitments::commitment_key(&rt, 601, &account(1)).unwrap(),
        commitments::uids_key(&rt, 601, &account(1)).unwrap(),
    );
    let uid = encode(crate::chain::SUBTENSOR, "Uids", &V::Int(i128::from(m.uid)));
    let round = plan_at(&db, now);
    assert!(round.reads[0].tag.starts_with("snapvals@"));
    let with_uid = serde_json::json!({"result": [{"block": snap.to_hex(), "changes": [
        [ck, serde_json::Value::Null], [uk, uid], [commitments::marker_key(), format!("0x{}", hex::encode(&marker))]
    ]}]});
    tx(&db, |s| commitments::ingest(s, Some(&rt), 601, &reply_to(&round, with_uid), now)).unwrap();
    assert_eq!(fault(&db), None, "the values stage was served");
    let round = plan_at(&db, now);
    assert_eq!(round.reads.len(), 1);
    assert!(round.reads[0].tag.starts_with("snapregs@"), "the registration block is read next");
    assert!(!round.reads[0].archive);
    for n in 1..=bound {
        let round = plan_at(&db, now);
        assert_eq!(round.reads.len(), 1, "registration answer {n}: the snapshot is due");
        assert!(round.reads[0].tag.starts_with("snapregs@"));
        assert_eq!(round.reads[0].archive, n > 1, "registration answer {n}: archive after the first answer without state");
        let body = if n % 2 == 1 { &not_imported } else { &no_state };
        tx(&db, |s| commitments::ingest(s, Some(&rt), 601, &reply_to(&round, body.clone()), now)).unwrap();
        assert_eq!(tries(&db), i64::from(n));
        assert_eq!(tx(&db, commitments::status).unwrap()["state_discarded"], 1, "registration answer {n} is not discarded");
        if n < bound {
            assert_eq!(fault(&db), None, "registration answer {n} is below the bound");
            assert!(plan_at(&db, now + backoff_ms(n) - 1).reads.is_empty(), "registration answer {n}: backing off");
        }
        now += backoff_ms(n);
    }
    assert_eq!(fault(&db).as_deref(), Some(commitments::SNAPSHOT_DISCARDED), "registrations paused at the bound");
    // The registration block served: the same cohort resumes and the
    // snapshot is complete.
    let round = plan_at(&db, now);
    assert!(round.reads[0].tag.starts_with("snapregs@") && round.reads[0].archive);
    let rk = commitments::registration_key(&rt, 601, m.uid).unwrap();
    let reg = encode(crate::chain::SUBTENSOR, "BlockAtRegistration", &V::Int(i128::from(m.registration_block)));
    let served = serde_json::json!({"result": [{"block": snap.to_hex(), "changes": [
        [rk, reg], [commitments::marker_key(), format!("0x{}", hex::encode(&marker))]
    ]}]});
    tx(&db, |s| commitments::ingest(s, Some(&rt), 601, &reply_to(&round, served), now)).unwrap();
    assert_eq!(fault(&db), None, "the registration block was served");
    assert_eq!(tries(&db), 0);
    assert!(plan_at(&db, now).reads.is_empty(), "the snapshot is complete");
}

fn s_(v: impl Into<String>) -> crate::sql::SqlValue {
    crate::sql::SqlValue::Text(v.into())
}

// ------------------------------------------------- framing off the object

/// Store v21 restarts the reader's stored call maxima once (the drill's
/// 3000 ms were the in-object framing stalls) and keeps its failure count,
/// so the next admitting window measures the reader without them.
#[test]
fn store_v21_restarts_the_reader_call_maxima_and_keeps_the_failure_count() {
    static UP_TO_V20: crate::sql::Schema = crate::sql::Schema {
        family: EVAL_COHORT_SCHEMA.family,
        migrations: EVAL_COHORT_SCHEMA.migrations.split_at(20).0,
    };
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &UP_TO_V20)).unwrap();
    let prior = |plan_ms, failed: Option<&str>| commitments::Prior { plan_ms: Some(plan_ms), ingest_ms: Some(plan_ms), failed: failed.map(Into::into) };
    tx(&db, |s| commitments::note_prior(s, &prior(3000, Some("plan")))).unwrap();
    tx(&db, |s| commitments::note_prior(s, &prior(80, None))).unwrap();
    let calls = |db: &Sqlite| tx(db, commitments::status).unwrap()["calls"].clone();
    let max = tx(&db, |q| q.exec("SELECT plan_ms_max FROM commit_reader WHERE id = 1", &[])).unwrap();
    assert_eq!(crate::sql::int(&max[0], 0).unwrap(), 3000);
    let report = tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    assert_eq!((report.from, report.to), (20, 27));
    let c = calls(&db);
    assert_eq!((c["plan_ms_max"].as_i64(), c["ingest_ms_max"].as_i64(), c["failed"].as_i64()), (Some(0), Some(0), Some(1)));
    assert_eq!(c["plan_ms_last"], 80);
}

/// The reader's call maxima restart at each admission window's opening and
/// name that window (review finding corr-5: per-window maxima).
#[test]
fn the_reader_call_maxima_restart_at_each_admission_window() {
    let db = db_with_open_cohort(2);
    let calls = |db: &Sqlite| tx(db, commitments::status).unwrap()["calls"].clone();
    assert_eq!(calls(&db)["maxima_cohort"], 1);
    let prior = commitments::Prior { plan_ms: Some(900), ingest_ms: Some(700), failed: None };
    tx(&db, |s| commitments::note_prior(s, &prior)).unwrap();
    assert_eq!(calls(&db)["plan_ms_max"], 900);
    // Re-recording the same window's opening restarts nothing.
    let c = tx(&db, |s| engine::cohort(s, 1)).unwrap();
    tx(&db, |s| open(s, &c, OPEN_BLOCK)).unwrap();
    assert_eq!(calls(&db)["plan_ms_max"], 900);
}

/// The install journal (store v22): a template installed before it is its
/// version 1 (governing an unrecorded range), a retried install of those
/// bytes is a replay that changes nothing, and another template is version
/// 2, naming the digest it replaced and the first cohort it can govern.
#[test]
fn a_template_installed_before_the_journal_is_its_first_version() {
    use crate::eval::calendar;
    let db = Sqlite::open();
    let v21 = crate::sql::Schema { family: EVAL_COHORT_SCHEMA.family, migrations: &EVAL_COHORT_SCHEMA.migrations[..21] };
    tx(&db, |s| crate::sql::migrate(s, &v21)).unwrap();
    let t = calendar::Template {
        schema: calendar::TEMPLATE_SCHEMA.into(),
        label_prefix: "testnet-cohort".into(),
        plan: template_plan(0, 40),
    };
    let bytes = serde_json::to_vec(&t).unwrap();
    let sha = crate::eval::engine::sha_hex(&bytes);
    db.conn
        .execute(
            "INSERT INTO standing_template (id, template, sha256, installed_ms, paused, first_cohort) VALUES (1, ?1, ?2, 5, 1, 0)",
            (String::from_utf8(bytes.clone()).unwrap(), &sha),
        )
        .unwrap();
    tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    record_probes(&db, &t);
    let r = tx(&db, |s| calendar::install(s, &cfg(), &bytes, NOW)).unwrap();
    assert_eq!(r["replayed"], true);
    assert_eq!(r["template_version"], 1);
    assert!(r["governs_from"].is_null());
    assert_eq!(r["paused"], true, "a replay never resumes a paused calendar");
    let mut narrower = t.clone();
    narrower.plan.admission.as_mut().unwrap().roster_cap = 39;
    let r = tx(&db, |s| calendar::install(s, &cfg(), &serde_json::to_vec(&narrower).unwrap(), NOW + 1)).unwrap();
    assert_eq!((r["template_version"].clone(), r["governs_from"].clone(), r["probe_versions"].clone()), (json!(2), json!(1), json!([1])));
    let replaced: String =
        db.conn.query_row("SELECT replaced_sha256 FROM template_installs WHERE version = 2", [], |r| r.get(0)).unwrap();
    assert_eq!(replaced, sha);
    assert_eq!(tx(&db, calendar::view).unwrap()["template"]["paused"], false, "a new template resumes the calendar");
}

// --------------------------------------------- admission v2: direct upload

/// A session mints its header URL at creation (a lost answer gets a fresh
/// one), `urls` settles the header before any file URL (`header_missing`
/// with a fresh header URL while R2 holds none), then mints one URL per
/// distinct file, every URL's life from the upload URL policy.
#[test]
fn a_session_mints_its_header_url_and_settles_the_header_before_any_file_url() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    let (r, job) = m.create(&db);
    assert_eq!(r.status, 201, "{}", r.body);
    let job = job.unwrap();
    let hl = u64::from(m.upload.header_len);
    assert_eq!(job.entries.len(), 1);
    let e = job.entries[0];
    assert_eq!((e.op, e.len, e.sha256, e.cohort), (mint::Op::Header, hl, m.upload.header_sha256.0, 1));
    assert_eq!(e.valid_secs, policy::valid_secs(hl).unwrap(), "the policy's life for the header alone");
    let up = &r.body["uploads"][0];
    assert_eq!((up["kind"].as_str(), up["content_length"].as_str()), (Some("header"), Some(hl.to_string().as_str())));
    assert_eq!(up["if_none_match"], "*");
    assert_eq!(r.body["objects"].as_array().unwrap().len(), 1, "only the header before it is recorded");
    assert_eq!(r.body["objects"][0]["present"], false);
    // A retried creation answers the same session with a fresh header URL.
    let (r, job) = m.create(&db);
    assert_eq!((r.status, job.map(|j| j.entries.len())), (200, Some(1)));
    // `urls` before the header is in R2: settled first, then header_missing.
    let req = m.post(&db, "urls");
    let step = tx(&db, |s| Ok::<_, EvalError>(urls(s, &chain(), &req, &m.session()))).unwrap();
    let Step::Header(hj) = step else { panic!("{step:?}") };
    assert_eq!(hj, m.job());
    assert!(hj.key.ends_with("/h") && hj.key.starts_with("admission/v2/c1/"));
    assert_eq!(m.settle(&db, HeaderSeen::Absent), Settled::Absent);
    let step = tx(&db, |s| Ok::<_, EvalError>(urls_after(s, &chain(), &req, &m.session(), &Settled::Absent))).unwrap();
    let (r, _) = drive(&db, step);
    assert_eq!((r.status, code(&r).as_str()), (409, "header_missing"));
    assert_eq!(r.body["session"]["uploads"].as_array().map(Vec::len), Some(1), "a fresh header URL beside the error");
    // The header recorded: one URL per file, for the whole artifact's life.
    assert_eq!(m.settle(&db, m.checked()), Settled::Recorded);
    let step = tx(&db, |s| Ok::<_, EvalError>(urls(s, &chain(), &req, &m.session()))).unwrap();
    let (r, job) = drive(&db, step);
    let job = job.unwrap();
    assert_eq!(job.entries.len(), 6);
    assert!(job.entries.iter().all(|e| e.op == mint::Op::File));
    let total = hl + m.header.files.iter().map(|f| f.len).sum::<u64>();
    assert!(job.entries.iter().all(|e| e.valid_secs == policy::valid_secs(total).unwrap()));
    assert_eq!(r.body["objects"].as_array().unwrap().len(), 7);
    assert_eq!(r.body["objects"][0]["present"], true);
    // The mint aggregate: one row, its count and latest expiry.
    let agg = tx(&db, |s| {
        let r = s.exec("SELECT mints, url_until_ms FROM adm_sessions WHERE session = ?", &[SqlValue::Text(m.session())])?;
        Ok::<_, EvalError>((int(&r[0], 0)?, int(&r[0], 1)?))
    })
    .unwrap();
    assert_eq!(agg.0, 1 + 1 + 1 + 6);
    assert_eq!(agg.1 as u64, (NOW / 1000 + u64::from(job.entries[0].valid_secs)) * 1000);
}

/// The largest answer (a session with one URL per object at the URL format
/// bound) fits the contract's response limit.
#[test]
fn the_largest_session_answer_fits_the_response_limit() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    m.settle(&db, m.checked());
    let r = tx(&db, |s| session_row(s, &m.session())).unwrap().unwrap();
    let upload = Upload {
        kind: "file".into(),
        sha256: "f".repeat(64),
        len: u64::MAX,
        url: "u".repeat(slop_ninja_r2_presign::presign::MAX_URL_LEN),
        content_length: u64::MAX.to_string(),
        if_none_match: "*".into(),
        checksum_sha256: "c".repeat(44),
        signed_at: u64::MAX,
        valid_secs: u32::MAX,
    };
    let s = tx(&db, |s| session_json(s, &r, u64::MAX, vec![upload; 1 + spec::ROLES.len()])).unwrap();
    let mut body = serde_json::to_value(&s).unwrap();
    let e = reply_error(&ApiError::new(ErrorCode::HeaderMissing, "x"));
    body = serde_json::json!({"error": e.body, "session": body});
    assert!(serde_json::to_vec(&body).unwrap().len() <= slop_ninja_admission_wire::response_limit());
}

/// Design A1-A3 and skeptic K6: what a header settle can find. R2's stored
/// size, checksum and attributes decide before the bytes are read; the
/// upload Worker's table is rebuilt and its content recomputed here.
#[test]
fn a_header_is_recorded_only_as_committed_and_the_platforms_faults_refuse_nothing() {
    let db = db_with_open_cohort(4);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    let job = m.job();
    let len = u64::from(job.len);
    // The head: no stored checksum is never a pass; other bytes are a
    // mismatch, the committed bytes with a forbidden attribute are dirty;
    // the committed object reads on.
    assert_eq!(header_head(&job, &Head::Error), Err(HeaderSeen::Platform("header head")));
    let no_sum = Head::Present(Stored { size: len, sha256: None, etag: "e".into(), clean: true });
    assert!(matches!(header_head(&job, &no_sum), Err(HeaderSeen::Platform(_))));
    let short = Head::Present(Stored { size: len - 1, sha256: Some(job.sha256), etag: "e1".into(), clean: true });
    assert_eq!(header_head(&job, &short), Err(HeaderSeen::Mismatch("e1".into())));
    let dirty = Head::Present(Stored { size: len, sha256: Some(job.sha256), etag: "e2".into(), clean: false });
    assert_eq!(header_head(&job, &dirty), Err(HeaderSeen::Dirty("e2".into())));
    assert_eq!(header_head(&job, &stored(job.sha256, len, "e3")), Ok("e3".into()));
    // The platform's failures record nothing and refuse nothing.
    assert_eq!(m.settle(&db, HeaderSeen::Platform("header read")), Settled::Platform("header read"));
    let mut lying = outcome_of(&m.header);
    if let HeaderResult::Checked { content, .. } = &mut lying.result {
        *content = "00".repeat(32);
    }
    assert_eq!(
        m.settle(&db, HeaderSeen::Checked { etag: "e".into(), outcome: lying }),
        Settled::Platform("header outcome content"),
        "an outcome whose table does not recompute to its content"
    );
    let mut table = outcome_of(&m.header);
    if let HeaderResult::Checked { files, .. } = &mut table.result {
        files[0].len += 1;
        // The content recomputed from the forged table matches its claim,
        // but the table fails the spec's own header checks here.
        let (h, c) = rebuild_header(spec::CONTRACT_ID, files).unwrap();
        let _ = h;
        if let HeaderResult::Checked { content, .. } = &mut table.result {
            *content = c.hex();
        }
    }
    assert_eq!(m.settle(&db, HeaderSeen::Checked { etag: "e".into(), outcome: table }), Settled::Platform("header outcome checks"));
    assert_eq!(state_of(&db, &m.session()), ("uploading".into(), None));
    // Another artifact's header (its own consistent table): the content is
    // not the committed one.
    let other = header(0x77);
    assert!(matches!(
        m.settle(&db, HeaderSeen::Checked { etag: "e".into(), outcome: outcome_of(&other) }),
        Settled::Refused(None)
    ));
    assert_eq!(state_of(&db, &m.session()), ("refused".into(), Some("content_mismatch".into())));
    // A mismatched object refuses its session and is deleted while it keeps
    // the tag seen.
    let m2 = miner(2, 4, 1, 0x12);
    m2.commit(&db, OPEN_BLOCK + 2, 0);
    m2.create(&db);
    let j2 = m2.job();
    let del = |etag: &str| Delete { key: j2.key.clone(), etag: etag.into(), sha256: j2.sha256, len: u64::from(j2.len) };
    // A header with the committed bytes but a stray stored attribute (one
    // unsigned Content-Type) is deleted, and the session keeps uploading
    // under a fresh header URL: no new commitment is needed.
    assert_eq!(m2.settle(&db, HeaderSeen::Dirty("dirty".into())), Settled::Unclean(del("dirty")));
    assert_eq!(state_of(&db, &m2.session()), ("uploading".into(), None));
    let req = m2.post(&db, "urls");
    let step = tx(&db, |s| Ok::<_, EvalError>(urls_after(s, &chain(), &req, &m2.session(), &Settled::Unclean(del("dirty"))))).unwrap();
    let (r, job) = drive(&db, step);
    assert_eq!((r.status, code(&r).as_str()), (409, "header_missing"));
    assert_eq!(job.unwrap().entries.len(), 1, "a fresh header URL");
    // The conditional delete runs only while the object is still unclean:
    // the clean re-upload of the same bytes (the same entity tag) stays.
    let d = del("dirty");
    assert!(d.allows(&Head::Present(Stored { size: d.len, sha256: Some(d.sha256), etag: "dirty".into(), clean: false })));
    assert!(!d.allows(&stored(d.sha256, d.len, "dirty")), "a clean copy with the same tag survives");
    assert!(!d.allows(&Head::Present(Stored { size: d.len, sha256: Some(d.sha256), etag: "other".into(), clean: false })));
    assert!(!d.allows(&Head::Absent) && !d.allows(&Head::Error));
    // Other bytes refuse the session (R2 verified the checksum, so only a
    // write of ours could leave them) and are deleted while they keep the
    // tag seen.
    assert_eq!(m2.settle(&db, HeaderSeen::Mismatch("bad".into())), Settled::Refused(Some(del("bad"))));
    assert_eq!(state_of(&db, &m2.session()).1.as_deref(), Some("object_mismatch"));
    // The upload Worker's refusals are the session's.
    let m3 = miner(3, 5, 1, 0x13);
    m3.commit(&db, OPEN_BLOCK + 3, 0);
    m3.create(&db);
    let invalid = HeaderOutcome { schema: HEADER_OUTCOME_SCHEMA.into(), result: HeaderResult::Invalid { reason: "prefix".into() } };
    m3.settle(&db, HeaderSeen::Checked { etag: "e".into(), outcome: invalid });
    let row = tx(&db, |s| session_row(s, &m3.session())).unwrap().unwrap();
    let refusal = row.refusal.unwrap();
    assert_eq!((refusal.code.as_str(), refusal.detail.reason.as_deref()), ("artifact_invalid", Some("prefix")));
    let m4 = miner(4, 6, 1, 0x14);
    m4.commit(&db, OPEN_BLOCK + 4, 0);
    m4.create(&db);
    let check = HeaderOutcome { schema: HEADER_OUTCOME_SCHEMA.into(), result: HeaderResult::Check { code: "graph_size".into() } };
    m4.settle(&db, HeaderSeen::Checked { etag: "e".into(), outcome: check });
    assert_eq!(state_of(&db, &m4.session()).1.as_deref(), Some("check_failed"));
    // The committed header is recorded once, with its file table and its
    // position from `admit_header`.
    let m5 = miner(5, 7, 1, 0x15);
    m5.commit(&db, OPEN_BLOCK + 5, 3);
    m5.create(&db);
    assert_eq!(m5.settle(&db, m5.checked()), Settled::Recorded);
    assert_eq!(m5.settle(&db, m5.checked()), Settled::Recorded, "again: nothing more");
    let row = tx(&db, |s| session_row(s, &m5.session())).unwrap().unwrap();
    assert_eq!(row.files.as_ref().map(Vec::len), Some(6));
    assert_eq!(row.position, Some(Position { block: OPEN_BLOCK + 5, extrinsic: 3 }));
    let objs = tx(&db, |s| objects_of(s, &m5.session())).unwrap();
    assert_eq!(objs.len(), 7);
    assert_eq!(objs[0].etag.as_deref(), Some("etag-h"));
}

/// Duplicate digests in a header share one object (and one URL).
#[test]
fn files_with_one_digest_share_one_object() {
    let db = db_with_open_cohort(2);
    let mut h = header(0x11);
    h.files[2].sha256 = h.files[1].sha256;
    let m = miner_with(1, 3, 1, 0x11, h);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    assert_eq!(m.settle(&db, m.checked()), Settled::Recorded);
    let req = m.post(&db, "urls");
    let Step::Mint(job) = tx(&db, |s| Ok::<_, EvalError>(urls(s, &chain(), &req, &m.session()))).unwrap() else { panic!() };
    assert_eq!(job.entries.len(), 5);
    assert_eq!(upload_all(&db, &m).status, 202);
}

/// Design A4 and skeptic K1: files are recorded from R2's heads; an object
/// present with other bytes or attributes is deleted (it may be PUT again);
/// a head error or a missing stored checksum records nothing. Once every
/// object is recorded the session verifies, its files manifest v2 naming
/// each object's key and entity tag.
#[test]
fn files_are_recorded_from_r2_heads_and_the_session_verifies() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    let req = m.post(&db, "complete");
    // The header first (complete settles it too).
    let step = tx(&db, |s| Ok::<_, EvalError>(complete(s, &chain(), &req, &m.session()))).unwrap();
    assert!(matches!(step, Step::Header(_)));
    let step = tx(&db, |s| Ok::<_, EvalError>(complete_after(s, &chain(), &req, &m.session(), &Settled::Absent))).unwrap();
    let (r, _) = drive(&db, step);
    assert_eq!(
        (code(&r).as_str(), r.body["error"]["detail"]["missing"][0].as_str()),
        ("objects_missing", Some(m.upload.header_sha256.hex().as_str()))
    );
    m.settle(&db, m.checked());
    let step = tx(&db, |s| Ok::<_, EvalError>(complete(s, &chain(), &req, &m.session()))).unwrap();
    let Step::Heads(probes) = step else { panic!("{step:?}") };
    assert_eq!(probes.len(), 6);
    let heads: Vec<(Probe, Head)> = probes
        .iter()
        .cloned()
        .enumerate()
        .map(|(n, p)| {
            let h = match n {
                0 => Head::Absent,
                1 => Head::Present(Stored { size: p.len, sha256: Some([0; 32]), etag: "bad".into(), clean: true }),
                2 => Head::Present(Stored { size: p.len, sha256: None, etag: "x".into(), clean: true }),
                3 => Head::Error,
                4 => Head::Present(Stored { size: p.len, sha256: Some(p.sha256), etag: "attr".into(), clean: false }),
                _ => stored(p.sha256, p.len, &etag_of(&p)),
            };
            (p, h)
        })
        .collect();
    let applied = tx(&db, |s| apply_heads(s, &heads, Recorder::Upload, NOW)).unwrap();
    assert!(applied.platform);
    let del = |p: &Probe, etag: &str| Delete { key: p.key.clone(), etag: etag.into(), sha256: p.sha256, len: p.len };
    assert_eq!(applied.deletes, vec![del(&probes[1], "bad"), del(&probes[4], "attr")]);
    let e = tx(&db, |s| Ok::<_, EvalError>(complete_reply(s, &m.session()))).unwrap().unwrap_err();
    assert_eq!((e.code, e.detail.missing.as_ref().map(Vec::len)), (ErrorCode::ObjectsMissing, Some(5)));
    // Every object as committed: the session verifies.
    assert_eq!(upload_all(&db, &m).status, 202);
    assert_eq!((state_of(&db, &m.session()).0.as_str(), stage_of(&db, &m.session()).as_deref()), ("verifying", Some("manifest")));
    let (key, bytes) = tx(&db, |s| manifest_due(s, &m.session())).unwrap().unwrap();
    let package = package_of(&bytes);
    assert_eq!(key, manifest_key(&package));
    let fm: FilesManifest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(fm.files.len(), 6);
    for f in &fm.files {
        assert_eq!(f.bucket, "admission");
        assert!(f.key.starts_with(&format!("admission/v2/c1/{}/f/", m.upload.commitment.hex())));
        assert_eq!(f.etag.as_deref().map(|e| e.starts_with("etag-")), Some(true));
    }
    // The scorer's own parse accepts it under the cohort's spec.
    FilesManifest::parse(&bytes, &package, Version::V2).unwrap();
    // The written manifest is the run's package.
    tx(&db, |s| manifest_written(s, &env(), &m.session(), NOW)).unwrap();
    let runs = tx(&db, |s| claim_runs(s, &env(), 8, NOW)).unwrap();
    assert_eq!((runs.len(), runs[0].package_sha256.as_str(), runs[0].manifest_key.as_str()), (1, package.as_str(), key.as_str()));
    // A complete sent again answers 202; urls after it, the session.
    let step = tx(&db, |s| Ok::<_, EvalError>(complete(s, &chain(), &req, &m.session()))).unwrap();
    assert_eq!(drive(&db, step).0.status, 202);
}

/// Skeptic K1, qualifications G9 and G12: only an object with no stored
/// attribute a presigned PUT leaves unsigned is admitted.
#[test]
fn stored_attributes_a_presigned_put_does_not_sign_are_refused() {
    assert_eq!(OBJECT_ATTRIBUTES_RULE, "slop_ninja_object_attributes_v1");
    assert_eq!(HTTP_METADATA_FIELDS.len(), 6, "the six standard headers G9 found stored");
    assert!(attrs_clean(false, None, false, false));
    assert!(attrs_clean(false, Some("Standard"), false, false), "a plain PUT (G12)");
    assert!(!attrs_clean(true, Some("Standard"), false, false), "a customer key");
    assert!(!attrs_clean(false, Some("InfrequentAccess"), false, false), "another storage class");
    assert!(!attrs_clean(false, Some("Standard"), true, false), "custom metadata");
    // Any HTTP metadata, the generic content type included.
    assert!(!attrs_clean(false, Some("Standard"), false, true), "httpMetadata not empty");
}

/// Skeptic K14: `urls`, `complete`, `repair` and reads need the session's
/// own hotkey.
#[test]
fn only_the_sessions_hotkey_acts_on_it() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    let other = miner(2, 4, 1, 0x12);
    let id = m.session();
    for op in ["urls", "complete", "repair"] {
        let req = other.signed(&db, "POST", &format!("/admission/v2/sessions/{id}/{op}"), &[]);
        let step = tx(&db, |s| {
            Ok::<_, EvalError>(match op {
                "urls" => urls(s, &chain(), &req, &id),
                "complete" => complete(s, &chain(), &req, &id),
                _ => repair(s, &chain(), &req, &id),
            })
        })
        .unwrap();
        assert_eq!(code(&drive(&db, step).0), "forbidden", "{op}");
    }
    let req = other.signed(&db, "GET", &format!("/admission/v2/sessions/{id}"), &[]);
    assert_eq!(code(&tx(&db, |s| Ok::<_, EvalError>(read(s, &chain(), &req, &id))).unwrap()), "forbidden");
}

/// A mint the signer could not make is the platform's: answered
/// `unavailable`, the session unchanged, `signer_unavailable` persisted
/// until a later mint succeeds.
#[test]
fn a_mint_failure_is_the_platforms_and_a_later_mint_clears_it() {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    let body = m.upload.to_bytes().unwrap();
    let mut req = m.signed(&db, "POST", "/admission/v2/sessions", &body);
    req.body = Some(hex::encode(&body));
    let Step::Mint(job) = tx(&db, |s| Ok::<_, EvalError>(create(s, &chain(), &req, NOW))).unwrap() else { panic!() };
    let r = tx(&db, |s| finish_mint(s, &job, Err("signer unreachable"), NOW)).unwrap();
    assert_eq!((r.status, code(&r).as_str()), (503, "unavailable"));
    assert_eq!(state_of(&db, &m.session()).0, "uploading");
    let st = tx(&db, status).unwrap();
    assert_eq!(st["upload_path"]["faults"][0], SIGNER_UNAVAILABLE);
    assert_eq!(st["upload_path"]["signer_error"], "signer unreachable");
    // A reply that does not answer every entry is a failure too.
    let r = tx(&db, |s| finish_mint(s, &job, Ok(Vec::new()), NOW)).unwrap();
    assert_eq!(code(&r), "unavailable");
    let (r, _) = m.create(&db);
    assert_eq!(r.status, 200);
    assert!(tx(&db, status).unwrap()["upload_path"]["faults"].as_array().unwrap().is_empty());
}

/// Skeptic K4 and review R3: the close starts with a fresh canary; a mint or
/// canary failure recorded since the last success defers the close to the
/// first boundary after a later success, so the platform's outage never
/// ends a miner's upload.
#[test]
fn an_upload_path_fault_defers_the_close_until_a_later_success() {
    let db = db_with_open_cohort(2);
    let admitting = |db: &Sqlite| tx(db, window).unwrap().unwrap().admitting();
    // A canary passed earlier, then the signer failed a mint.
    assert!(tx(&db, |s| canary_begin(s, NOW)).unwrap().is_none(), "nothing to check yet");
    tx(&db, |s| canary_done(s, Ok(()), NOW)).unwrap();
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    let body = m.upload.to_bytes().unwrap();
    let mut req = m.signed(&db, "POST", "/admission/v2/sessions", &body);
    req.body = Some(hex::encode(&body));
    let Step::Mint(job) = tx(&db, |s| Ok::<_, EvalError>(create(s, &chain(), &req, NOW))).unwrap() else { panic!() };
    tx(&db, |s| finish_mint(s, &job, Err("signer status"), NOW + 10)).unwrap();
    // The boundary arrives: deferred, still admitting, the canary retries.
    epoch(&db, 12, OPEN_BLOCK + 360, NOW + 20);
    assert!(admitting(&db), "the fault defers the close");
    assert_eq!(tx(&db, |s| canary_begin(s, NOW + 20)).unwrap(), Some(1));
    assert!(tx(&db, |s| canary_begin(s, NOW + 21)).unwrap().is_none(), "once a tick");
    tx(&db, |s| canary_done(s, Err(CanaryFailure::Path("canary put refused")), NOW + 30)).unwrap();
    let st = tx(&db, status).unwrap();
    assert_eq!(st["upload_path"]["faults"], serde_json::json!([UPLOAD_PATH_UNAVAILABLE]), "the canary's mint cleared the signer's");
    // The next boundary: still faulted, still deferred.
    epoch(&db, 13, OPEN_BLOCK + 720, NOW + 40);
    assert!(admitting(&db));
    // A success clears it, but the close waits for the next boundary.
    assert!(tx(&db, |s| canary_begin(s, NOW + 30 + TICK_MS)).unwrap().is_some());
    assert!(!tx(&db, |s| canary_done(s, Ok(()), NOW + 30 + TICK_MS)).unwrap(), "no close at a boundary from before the clear");
    assert!(admitting(&db));
    // The first boundary after the clear: its own canary, then the close.
    epoch(&db, 14, OPEN_BLOCK + 1080, NOW + 2 * TICK_MS);
    assert!(admitting(&db), "the canary is the close's first step");
    assert!(tx(&db, |s| canary_begin(s, NOW + 2 * TICK_MS)).unwrap().is_some());
    assert!(tx(&db, |s| canary_done(s, Ok(()), NOW + 2 * TICK_MS + 1)).unwrap());
    let w = tx(&db, window).unwrap().unwrap();
    assert_eq!((w.state.as_str(), w.cutoff_block), ("closing", Some(OPEN_BLOCK + 1080)));
    // Health passed, then the signer failed, then the boundary: deferred
    // (a stale success never counts).
    let db = db_with_open_cohort(2);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    tx(&db, |s| canary_done(s, Ok(()), NOW)).unwrap();
    let Step::Mint(job) = tx(&db, |s| Ok::<_, EvalError>(create(s, &chain(), &req, NOW))).unwrap() else { panic!() };
    tx(&db, |s| finish_mint(s, &job, Err("signer status"), NOW + 1)).unwrap();
    epoch(&db, 12, OPEN_BLOCK + 360, NOW + 2);
    assert!(admitting(&db));
}

/// Skeptic K7 and K8: once `closing` is persisted only the close sweep
/// records presence (in batches, its progress per object); a head error
/// defers the sweep; sessions missing an object at its close head are
/// refused, complete ones verify, and the freeze waits for the recorded
/// close.
#[test]
fn once_closing_only_the_close_sweep_records_presence() {
    let db = db_with_open_cohort(4);
    let (a, b, c) = (miner(1, 3, 1, 0x11), miner(2, 4, 1, 0x12), miner(3, 5, 1, 0x13));
    for (m, x) in [(&a, 1), (&b, 2), (&c, 3)] {
        m.commit(&db, OPEN_BLOCK + x, 0);
        m.create(&db);
    }
    // A and B have their headers recorded; C never uploads its header.
    a.settle(&db, a.checked());
    b.settle(&db, b.checked());
    let mut probes = a.file_probes();
    let last = probes.pop().unwrap();
    let heads: Vec<(Probe, Head)> = probes.into_iter().map(|p| {
        let h = stored(p.sha256, p.len, &etag_of(&p));
        (p, h)
    }).collect();
    tx(&db, |s| apply_heads(s, &heads, Recorder::Upload, NOW)).unwrap();
    // The boundary and its canary: closing.
    epoch(&db, 12, OPEN_BLOCK + 360, NOW + 2);
    tx(&db, |s| canary_begin(s, NOW + 2)).unwrap();
    assert!(tx(&db, |s| canary_done(s, Ok(()), NOW + 3)).unwrap());
    assert_eq!(tx(&db, |s| freeze_wait(s, 1)).unwrap(), Some("the close sweep is heading the open sessions' objects"));
    // Every other path records nothing now.
    for op in ["urls", "complete"] {
        let req = a.post(&db, op);
        let step = tx(&db, |s| Ok::<_, EvalError>(if op == "urls" { urls(s, &chain(), &req, &a.session()) } else { complete(s, &chain(), &req, &a.session()) })).unwrap();
        assert_eq!(code(&drive(&db, step).0), "after_cutoff", "{op}");
    }
    let late = vec![(last.clone(), stored(last.sha256, last.len, &etag_of(&last)))];
    tx(&db, |s| apply_heads(s, &late, Recorder::Upload, NOW + 5)).unwrap();
    assert!(tx(&db, |s| objects_of(s, &a.session())).unwrap().iter().any(|o| o.etag.is_none()), "not recorded by an upload path");
    assert_eq!(tx(&db, |s| observe(s, &last.key)).unwrap(), Observed::Nothing);
    // The close sweep: a head error defers it, its progress is kept.
    let (seq, probes) = tx(&db, |s| close_begin(s, NOW + 5)).unwrap().unwrap();
    assert_eq!(probes.len(), 1 + 6, "A's last file and every file of B");
    let mut heads: Vec<(Probe, Head)> = probes.iter().cloned().map(|p| (p, Head::Absent)).collect();
    heads[0].1 = Head::Error;
    let applied = tx(&db, |s| apply_heads(s, &heads, Recorder::Close, NOW + 5)).unwrap();
    tx(&db, |s| close_step_done(s, seq, &applied, NOW + 5)).unwrap();
    assert!(tx(&db, |s| close_begin(s, NOW + 6)).unwrap().is_none(), "backing off");
    // After the backoff the same objects are headed again: A's last file is
    // present now and counts; B's files are absent.
    sweep_close_at(&db, NOW + 5 + LANE_RETRY_MS, |p| if p.session == a.session() { stored(p.sha256, p.len, &etag_of(p)) } else { Head::Absent });
    assert_eq!(state_of(&db, &a.session()).0, "verifying");
    assert_eq!(state_of(&db, &b.session()), ("refused".into(), Some("incomplete_at_cutoff".into())));
    assert_eq!(state_of(&db, &c.session()), ("refused".into(), Some("incomplete_at_cutoff".into())));
    let (cutoff, wanted) = tx(&db, snapshot_needs).unwrap().unwrap();
    assert_eq!((cutoff, wanted), (OPEN_BLOCK + 360, vec![account(1)]));
    assert_eq!(tx(&db, |s| freeze_wait(s, 1)).unwrap(), Some("verification or verdicts pending"));
}

/// Skeptic K2 and K13: a run that met a missing or changed object audits the
/// session. Absent is the platform's: the session waits for repair (no run
/// is claimed), `repair` mints a URL for exactly that recorded object, and
/// its return resumes the run. Present with another entity tag is the
/// miner's (`object_replaced`). Repair answers `not_needed` once nothing is
/// missing, or after publication is recorded.
#[test]
fn a_missing_object_is_repaired_and_a_replaced_one_refuses_its_session() {
    let db = db_with_open_cohort(4);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    upload_all(&db, &m);
    frame_ok(&db, &m);
    let id = m.session();
    let runs = tx(&db, |s| claim_runs(s, &env(), 8, NOW)).unwrap();
    tx(&db, |s| record_run(s, &env(), &runs[0], &LaneOutcome::Platform(crate::eval::scorer_client::OBJECT_MISSING.into()), 3, NOW))
        .unwrap();
    assert_eq!(stage_of(&db, &id).as_deref(), Some("audit"));
    assert_eq!(tx(&db, |s| verify_due(s, NOW)).unwrap(), Some((id.clone(), "audit".into())));
    let probes = tx(&db, |s| audit_probes(s, &id)).unwrap();
    assert_eq!(probes.len(), 7);
    let gone = probes[3].clone();
    let heads: Vec<(Probe, Head)> = probes
        .iter()
        .cloned()
        .map(|p| {
            let h = if p == gone { Head::Absent } else { stored(p.sha256, p.len, p.etag.as_deref().unwrap()) };
            (p, h)
        })
        .collect();
    let applied = tx(&db, |s| apply_heads(s, &heads, Recorder::Recorded, NOW)).unwrap();
    tx(&db, |s| audit_done(s, &id, &applied, NOW)).unwrap();
    assert_eq!(stage_of(&db, &id).as_deref(), Some("repair"));
    assert!(tx(&db, |s| claim_runs(s, &env(), 8, NOW + LANE_LEASE_MS)).unwrap().is_empty(), "the run waits for the object");
    let req = m.post(&db, "repair");
    let Step::Heads(rp) = tx(&db, |s| Ok::<_, EvalError>(repair(s, &chain(), &req, &id))).unwrap() else { panic!() };
    let heads: Vec<(Probe, Head)> = rp.iter().cloned().map(|p| {
        let h = if p == gone { Head::Absent } else { stored(p.sha256, p.len, p.etag.as_deref().unwrap()) };
        (p, h)
    }).collect();
    tx(&db, |s| apply_heads(s, &heads, Recorder::Recorded, NOW)).unwrap();
    let (r, job) = drive(&db, tx(&db, |s| Ok::<_, EvalError>(repair_after(s, &id))).unwrap());
    let job = job.unwrap();
    assert_eq!((job.entries.len(), job.entries[0].sha256, job.entries[0].len), (1, gone.sha256, gone.len));
    assert_eq!(r.body["repair_needed"][0].as_str(), Some(hex::encode(gone.sha256).as_str()));
    // The object is back (its event): the run resumes.
    let Observed::Heads(p) = tx(&db, |s| observe(s, &gone.key)).unwrap() else { panic!() };
    let back = vec![(p[0].clone(), stored(gone.sha256, gone.len, gone.etag.as_deref().unwrap()))];
    tx(&db, |s| apply_heads(s, &back, Recorder::Recorded, NOW)).unwrap();
    assert_eq!(stage_of(&db, &id).as_deref(), Some("verdict"));
    assert_eq!(tx(&db, |s| claim_runs(s, &env(), 8, NOW + 2 * LANE_LEASE_MS)).unwrap().len(), 1);
    let Step::Heads(rp) = tx(&db, |s| Ok::<_, EvalError>(repair(s, &chain(), &req, &id))).unwrap() else { panic!() };
    let fine: Vec<(Probe, Head)> = rp.iter().cloned().map(|p| {
        let h = stored(p.sha256, p.len, p.etag.as_deref().unwrap());
        (p, h)
    }).collect();
    tx(&db, |s| apply_heads(s, &fine, Recorder::Recorded, NOW)).unwrap();
    assert_eq!(code(&drive(&db, tx(&db, |s| Ok::<_, EvalError>(repair_after(s, &id))).unwrap()).0), "not_needed");
    // Replaced: present with its digest but another entity tag.
    let m2 = miner(2, 4, 1, 0x12);
    m2.commit(&db, OPEN_BLOCK + 2, 0);
    m2.create(&db);
    upload_all(&db, &m2);
    frame_ok(&db, &m2);
    let runs = tx(&db, |s| claim_runs(s, &env(), 8, NOW + 3 * LANE_LEASE_MS)).unwrap();
    let r2 = runs.iter().find(|r| r.hotkey == hex::encode(account(2).0)).unwrap();
    tx(&db, |s| record_run(s, &env(), r2, &LaneOutcome::Platform(crate::eval::scorer_client::OBJECT_CHANGED.into()), 3, NOW)).unwrap();
    let probes = tx(&db, |s| audit_probes(s, &m2.session())).unwrap();
    let heads: Vec<(Probe, Head)> = probes.iter().cloned().map(|p| {
        let h = stored(p.sha256, p.len, "another-tag");
        (p, h)
    }).collect();
    tx(&db, |s| apply_heads(s, &heads, Recorder::Recorded, NOW)).unwrap();
    assert_eq!(state_of(&db, &m2.session()), ("refused".into(), Some("object_replaced".into())));
    // The repair window and the admitted objects' retention end once the
    // cohort is closed AND its publication is recorded (the publisher's
    // report), never at one of them alone: scoring lanes read the objects
    // until the cohort closes.
    tx(&db, |s| s.exec("UPDATE adm_sessions SET state = 'admitted' WHERE session = ?", &[SqlValue::Text(id.clone())]).map(|_| ())).unwrap();
    assert!(matches!(tx(&db, |s| Ok::<_, EvalError>(repair(s, &chain(), &req, &id))).unwrap(), Step::Heads(_)));
    let entry = |state: &str| PublicationEntry { commitment: m.upload.commitment.hex(), state: state.into() };
    // Another authority's cohort (its label, or a number this store does
    // not hold) is not this one's: nothing is recorded, and the Worker
    // answers 404 so the publisher keeps the report pending.
    let v = tx(&db, |s| publication_report(s, 1, "testnet-r0b-1", &[entry("published")], NOW)).unwrap();
    assert_eq!((v["recorded"].as_u64(), v["known"].as_bool()), (Some(0), Some(false)));
    assert_eq!(tx(&db, |s| publication_report(s, 99, "testnet-c1", &[entry("published")], NOW)).unwrap()["known"], false);
    let held = |db: &Sqlite| tx(db, objects_retained).unwrap();
    assert!(held(&db), "unpublished admitted objects hold a succession");
    let v = tx(&db, |s| publication_report(s, 1, "testnet-c1", &[entry("published")], NOW)).unwrap();
    assert_eq!((v["recorded"].as_u64(), v["known"].as_bool()), (Some(1), Some(true)));
    let v = tx(&db, |s| publication_report(s, 1, "testnet-c1", &[entry("published")], NOW)).unwrap();
    assert_eq!(v["recorded"].as_u64(), Some(0), "once");
    assert!(
        matches!(tx(&db, |s| Ok::<_, EvalError>(repair(s, &chain(), &req, &id))).unwrap(), Step::Heads(_)),
        "published but the cohort is still scoring: still needed"
    );
    let key = ObjectId::parse(&gone.key).unwrap();
    assert!(tx(&db, |s| live_holder(s, &key)).unwrap().is_some(), "the reconciliation keeps it");
    tx(&db, |s| s.exec("UPDATE cohorts SET phase = 'closed' WHERE seq = 1", &[]).map(|_| ())).unwrap();
    assert_eq!(code(&drive(&db, tx(&db, |s| Ok::<_, EvalError>(repair(s, &chain(), &req, &id))).unwrap()).0), "not_needed");
    assert!(tx(&db, |s| live_holder(s, &key)).unwrap().is_none(), "closed and published: the reconciliation may delete it");
    assert!(held(&db), "published and closed, but its deletes are not queued yet");
    // The clean-up queues every admitted object, and the reconciliation's
    // listing returns them for deletion too.
    tx(&db, |s| next_work(s, &cfg(), NOW)).unwrap();
    assert!(!held(&db), "released: what is left is the queued deletes' own work");
    let due = tx(&db, |s| object_deletes_due(s, 100, crate::wire::MAX_SAFE_INTEGER)).unwrap();
    let keys: Vec<String> = tx(&db, |s| objects_of(s, &id)).unwrap().iter()
        .map(|o| object_key(1, &m.upload.commitment, o.kind)).collect();
    for k in &keys {
        assert!(due.iter().any(|(d, _)| d == k), "{k} queued");
        assert!(tx(&db, |s| queued_delete_allowed(s, k)).unwrap());
    }
    let gone = tx(&db, |s| reconcile_page(s, &keys, None, NOW)).unwrap();
    assert_eq!(gone.len(), keys.len());
}

/// Design section 4 and section 9: an object-create event is a wakeup
/// only. It settles the object it names, or deletes one no live session
/// holds; anything else is dropped.
#[test]
fn object_create_events_settle_their_object_or_delete_an_orphan() {
    let db = db_with_open_cohort(4);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    let hk = m.job().key;
    let msg = |key: &str| serde_json::json!({"account": "a", "action": "PutObject", "bucket": "b",
        "object": {"key": key, "size": 1, "eTag": "e"}, "eventTime": "t"}).to_string();
    assert_eq!(event_key(&msg(&hk)).as_deref(), Some(hk.as_str()));
    assert_eq!(event_key(&msg("health/c1/aa")), None, "not an admission object");
    assert_eq!(event_key("{"), None);
    assert_eq!(event_key(&format!("{}{}", msg(&hk), " ".repeat(EVENT_MESSAGE_BYTES))), None, "refused past its bound");
    assert_eq!(tx(&db, |s| observe(s, "admission/v2/c1/zz/h")).unwrap(), Observed::Nothing);
    assert_eq!(tx(&db, |s| observe(s, &hk)).unwrap(), Observed::Header(m.job()));
    m.settle(&db, m.checked());
    assert_eq!(tx(&db, |s| observe(s, &hk)).unwrap(), Observed::Nothing, "recorded already");
    let fp = m.file_probes();
    assert_eq!(tx(&db, |s| observe(s, &fp[0].key)).unwrap(), Observed::Heads(vec![fp[0].clone()]));
    // A file key the header does not list, and any key of an ended
    // session, is an orphan.
    let stray = object_key(1, &m.upload.commitment, ObjectKind::File([0xee; 32]));
    assert_eq!(tx(&db, |s| observe(s, &stray)).unwrap(), Observed::Delete(stray.clone()));
    let nobody = object_key(1, &Digest([0xab; 32]), ObjectKind::Header);
    assert_eq!(tx(&db, |s| observe(s, &nobody)).unwrap(), Observed::Delete(nobody.clone()));
    tx(&db, |s| operator_refuse(s, &m.session(), "drill", NOW)).unwrap();
    assert_eq!(tx(&db, |s| observe(s, &fp[0].key)).unwrap(), Observed::Delete(fp[0].key.clone()));
}

/// Skeptic K10: one paginated listing of `admission/v2/` at a time, keys no
/// live session holds deleted, the pass never ended by time or an empty
/// list, a new pass one tick after the last ended; it gates nothing.
#[test]
fn the_standing_reconciliation_deletes_what_no_live_session_holds() {
    let db = db_with_open_cohort(4);
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    let r = miner(2, 4, 1, 0x12);
    r.commit(&db, OPEN_BLOCK + 2, 0);
    r.create(&db);
    tx(&db, |s| operator_refuse(s, &r.session(), "drill", NOW)).unwrap();
    assert_eq!(tx(&db, |s| reconcile_begin(s, NOW)).unwrap(), Some(None), "a first pass is due");
    let keys = vec![m.job().key, r.job().key, object_key(1, &m.upload.commitment, ObjectKind::File([1; 32]))];
    let gone = tx(&db, |s| reconcile_page(s, &keys, Some("next"), NOW)).unwrap();
    assert_eq!(gone, keys[1..].to_vec());
    assert_eq!(tx(&db, |s| reconcile_begin(s, NOW)).unwrap(), Some(Some("next".into())), "the pass continues");
    assert!(tx(&db, |s| reconcile_page(s, &[], None, NOW + 1)).unwrap().is_empty());
    assert_eq!(tx(&db, |s| reconcile_begin(s, NOW + 2)).unwrap(), None);
    assert_eq!(tx(&db, |s| reconcile_begin(s, NOW + 1 + TICK_MS)).unwrap(), Some(None));
    assert!(!tx(&db, |s| has_work(s, &cfg(), None)).unwrap(), "the reconciliation is never unfinished work");
    assert_eq!(tx(&db, status).unwrap()["upload_path"]["reconcile_deleted"], 2);
    // Review round 2: a cohort this store never opened is another
    // authority's (a successor's, for a retired predecessor that still
    // reconciles): its keys are never this store's to delete.
    let other = vec![object_key(20, &Digest([0xcd; 32]), ObjectKind::Header)];
    assert!(tx(&db, |s| reconcile_page(s, &other, None, NOW + 2)).unwrap().is_empty());
}

/// The alarm sweep heads every unrecorded object of sessions that had a URL
/// minted, once per tick.
#[test]
fn the_alarm_sweep_runs_once_a_tick_for_sessions_with_urls() {
    let db = db_with_open_cohort(4);
    assert!(tx(&db, |s| sweep_begin(s, NOW)).unwrap().is_none(), "nothing minted");
    let m = miner(1, 3, 1, 0x11);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    let sw = tx(&db, |s| sweep_begin(s, NOW)).unwrap().unwrap();
    assert_eq!((sw.headers, sw.heads.len()), (vec![m.job()], 0));
    assert!(tx(&db, |s| sweep_begin(s, NOW + 1)).unwrap().is_none(), "once a tick");
    m.settle(&db, m.checked());
    let sw = tx(&db, |s| sweep_begin(s, NOW + TICK_MS)).unwrap().unwrap();
    assert_eq!((sw.headers.len(), sw.heads.len()), (0, 6));
    m.put_files(&db, Recorder::Upload);
    assert!(tx(&db, |s| sweep_begin(s, NOW + 2 * TICK_MS)).unwrap().is_none(), "all recorded");
    assert_eq!(state_of(&db, &m.session()).0, "verifying", "the sweep's records complete a session too");
}

/// The signer's interface and the deployment's bindings are the ones this
/// side names.
#[test]
fn the_signer_interface_and_bindings_are_the_deployments() {
    let lib = include_str!("../../../r2-signer/src/lib.rs");
    for (name, value) in [
        ("SIGNER_ORIGIN", signer::ORIGIN),
        ("MINT_PATH", signer::MINT_PATH),
        ("TOKEN_HEADER", signer::TOKEN_HEADER),
        ("MEDIA_TYPE", signer::MEDIA_TYPE),
    ] {
        assert!(lib.contains(&format!("pub const {name}: &str = \"{value}\";")), "{name}");
    }
    let signer_toml = include_str!("../../../r2-signer/wrangler.toml");
    assert!(signer_toml.contains("name = \"slop-ninja-r2-signer-testnet\""));
    assert!(signer_toml.contains(signer::TOKEN_SECRET));
    let toml = include_str!("../../wrangler.toml");
    for line in [
        format!("binding = \"{}\"", signer::BINDING),
        "service = \"slop-ninja-r2-signer-testnet\"".to_owned(),
        format!("queue = \"{EVENTS_QUEUE}\""),
        signer::TOKEN_SECRET.to_owned(),
    ] {
        assert!(toml.contains(&line), "{line}");
    }
    // The events consumer's concurrency is the bound the memory fit counts.
    let consumer = &toml[toml.find(&format!("queue = \"{EVENTS_QUEUE}\"")).unwrap()..];
    let consumer = &consumer[..consumer.find("\n\n").unwrap_or(consumer.len())];
    assert!(consumer.lines().any(|l| l.trim() == format!("max_concurrency = {EVENTS_MAX_CONCURRENCY}")), "{consumer}");
}

/// The plan names the upload URL policy and no v1 part fields; a plan
/// frozen under v1 still parses as stored (its digest is bound) but is no
/// valid new plan.
#[test]
fn the_plan_names_the_url_policy_and_no_part_fields() {
    let p = admission_plan(1, 2);
    plan::CohortPlan::parse(&serde_json::to_vec(&p).unwrap(), cfg().ceilings).unwrap();
    let refused = |f: &dyn Fn(&mut AdmissionPlan)| {
        let mut q = p.clone();
        f(q.admission.as_mut().unwrap());
        plan::CohortPlan::parse(&serde_json::to_vec(&q).unwrap(), cfg().ceilings).is_err()
    };
    assert!(refused(&|a| a.upload_policy = None));
    assert!(refused(&|a| a.upload_policy.as_mut().unwrap().b_min = 1));
    assert!(refused(&|a| a.upload_policy.as_mut().unwrap().id = "slop_ninja_upload_url_policy_v2".into()));
    assert!(refused(&|a| a.part_size = Some(64 << 20)));
    assert!(refused(&|a| a.max_parts = Some(64)));
    let mut v1 = serde_json::to_value(&p).unwrap();
    let a = v1["admission"].as_object_mut().unwrap();
    a.remove("upload_policy");
    a.insert("part_size".into(), serde_json::json!(64 << 20));
    a.insert("max_parts".into(), serde_json::json!(64));
    let stored: plan::CohortPlan = serde_json::from_value(v1.clone()).unwrap();
    assert_eq!(serde_json::to_value(&stored).unwrap(), v1, "a stored v1 plan round-trips unchanged");
    assert_eq!(upload_policy(stored.admission.as_ref().unwrap()).id, policy::POLICY_ID);
}

/// Store v25 replaces the v1 session tables in place once no v1 admission
/// is live: v1 objects not cleaned up and not admitted are queued for
/// deletion, pending verdicts dropped, reached ones kept. While a v1
/// session is uploading, verifying or verified, a window is admitting or
/// closing, or an admitted session's cohort is not closed, the migration
/// refuses and the store stays at its pre-deploy version: v24, or v23 when
/// v24 and v25 arrive in one deploy (one transaction applies both).
#[test]
fn store_v25_refuses_a_live_v1_admission_then_replaces_the_session_tables() {
    static UP_TO_V24: crate::sql::Schema = crate::sql::Schema {
        family: EVAL_COHORT_SCHEMA.family,
        migrations: EVAL_COHORT_SCHEMA.migrations.split_at(24).0,
    };
    let v1_session = |db: &Sqlite, id: &str, state: &str, cleaned: i64| {
        db.conn
            .execute(
                "INSERT INTO adm_sessions (session, seq, cohort, hotkey, upload, commitment, content, artifact_sha256, artifact_len,
                    part_size, part_count, attempt, state, object_key, upload_id, created_ms, updated_ms, cleaned)
                 VALUES (?1, 1, 1, 'h', 'u', 'c', 'c', 'a', 1, 1, 1, 1, ?2, ?3, ?4, 1, 2, ?5)",
                (id, state, format!("admission/testnet-c1/{id}/a1.object"), format!("up-{id}"), cleaned),
            )
            .unwrap();
    };
    let version = |db: &Sqlite| tx(db, |s| Ok::<_, EvalError>(crate::sql::stored_version(s)?.1)).unwrap();
    // Each live v1 state refuses on its own; the store stays at v24.
    for live in ["uploading", "verifying", "verified", "admitted"] {
        let db = Sqlite::open();
        tx(&db, |s| crate::sql::migrate(s, &UP_TO_V24)).unwrap();
        v1_session(&db, "x", live, 0);
        assert!(tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).is_err(), "{live} refuses");
        assert_eq!(version(&db), 24, "{live}: the store keeps the previous build's version");
        assert!(tx(&db, |s| s.exec("SELECT * FROM adm_parts", &[])).is_ok(), "{live}: nothing was dropped");
    }
    // From v23 (quant-v1's v24 not yet deployed), the refusal rolls back
    // v24 too: the store stays at v23 for the build deployed before both.
    {
        static UP_TO_V23: crate::sql::Schema = crate::sql::Schema {
            family: EVAL_COHORT_SCHEMA.family,
            migrations: EVAL_COHORT_SCHEMA.migrations.split_at(23).0,
        };
        let db = Sqlite::open();
        tx(&db, |s| crate::sql::migrate(s, &UP_TO_V23)).unwrap();
        v1_session(&db, "x", "verifying", 0);
        assert!(tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).is_err());
        assert_eq!(version(&db), 23, "v24 rolls back with the refused v25");
        assert!(tx(&db, |s| s.exec("SELECT * FROM adm_parts", &[])).is_ok(), "nothing was dropped");
    }
    // Review round 2: a queued cohort (its frozen plan names a v1 reference
    // package), a pending v1 blob delete, or a created blob of a v1 session
    // not cleaned up refuses too.
    let refuses = |setup: &dyn Fn(&Sqlite), what: &str| {
        let db = Sqlite::open();
        tx(&db, |s| crate::sql::migrate(s, &UP_TO_V24)).unwrap();
        setup(&db);
        assert!(tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).is_err(), "{what} refuses");
        assert_eq!(version(&db), 24, "{what}");
    };
    refuses(
        &|db| {
            db.conn
                .execute(
                    "INSERT INTO cohorts (seq, label, cohort, plan, plan_sha256, phase, phase_since_ms, queued_ms)
                     VALUES (2, 'queued-v1', 2, '{}', 'p', 'queued', 1, 1)",
                    (),
                )
                .unwrap();
        },
        "a queued v1 plan",
    );
    refuses(&|db| { db.conn.execute("INSERT INTO adm_blob_deletes (key, session) VALUES ('k', 's')", ()).unwrap(); }, "a blob delete");
    refuses(
        &|db| {
            v1_session(db, "f", "refused", 0);
            db.conn.execute("INSERT INTO adm_blobs (session, sha256, created) VALUES ('f', 'aa', 1)", ()).unwrap();
        },
        "an uncleaned session's blob",
    );
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &UP_TO_V24)).unwrap();
    for (id, state, cleaned) in [("c", "refused", 1), ("d", "overflow", 0), ("e", "refused", 0)] {
        v1_session(&db, id, state, cleaned);
    }
    // An admitted v1 session of a closed cohort does not hold the upgrade.
    v1_session(&db, "b", "admitted", 0);
    db.conn
        .execute(
            "INSERT INTO cohorts (seq, label, cohort, plan, plan_sha256, phase, phase_since_ms, queued_ms)
             VALUES (1, 'closed-c1', 1, '{}', 'p', 'closed', 1, 1)",
            (),
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO adm_verdicts (hotkey, commitment, package_sha256, manifest_key, label, scorer_binary, spec_sha256, state, updated_ms)
             VALUES ('h', 'c', 'p', 'k', 'l', 'b', 's', 'pending', 1), ('h2', 'c', 'p', 'k', 'l', 'b', 's', 'admitted', 1)",
            (),
        )
        .unwrap();
    let report = tx(&db, |s| crate::sql::migrate(s, &EVAL_COHORT_SCHEMA)).unwrap();
    assert_eq!((report.from, report.to), (24, 27));
    let queued: std::collections::BTreeSet<(String, Option<String>)> =
        tx(&db, |s| object_deletes_due(s, 10, crate::wire::MAX_SAFE_INTEGER)).unwrap().into_iter().collect();
    let want: std::collections::BTreeSet<(String, Option<String>)> = ["d", "e"]
        .iter()
        .map(|id| (format!("admission/testnet-c1/{id}/a1.object"), Some(format!("up-{id}"))))
        .collect();
    assert_eq!(queued, want, "the overflow and uncleaned refused objects; admitted and cleaned ones stay");
    let n = |q: &str| tx(&db, |s| Ok::<_, EvalError>(int(&s.exec(q, &[])?[0], 0)?)).unwrap();
    assert_eq!(n("SELECT COUNT(*) FROM adm_sessions"), 0);
    assert_eq!(n("SELECT COUNT(*) FROM adm_verdicts"), 1, "reached verdicts stay as history");
    assert_eq!(n("SELECT COUNT(*) FROM adm_platform"), 1);
    assert!(tx(&db, |s| s.exec("SELECT * FROM adm_parts", &[])).is_err());
    assert!(tx(&db, |s| s.exec("SELECT * FROM adm_v25_guard", &[])).is_err(), "the guard leaves nothing");
}

/// Every compiled admission spec's largest object fits one URL life at the
/// policy's B_MIN within R2's 7 days, and one PUT.
#[test]
fn every_admission_specs_largest_object_fits_the_url_policy() {
    for id in ADMISSION_SPECS {
        let b = SpecBounds::of_id(id).unwrap();
        let v = policy::valid_secs(b.object_bytes).unwrap();
        assert!(u64::from(v) <= policy::R2_MAX_VALID_SECS);
        assert!(b.object_bytes <= policy::R2_SINGLE_PUT_MAX);
    }
    assert_eq!(policy::valid_secs(954_576_228), Ok(8_537), "design section 3.3");
}

/// A legacy v1 fault still clears: nothing keeps `framing_unavailable`.
#[test]
fn a_legacy_framing_fault_clears_on_the_next_admission_step() {
    let db = db_with_open_cohort(2);
    let fault = |db: &Sqlite| tx(db, |s| engine::cohort(s, 1)).unwrap().fault;
    tx(&db, |s| engine::fault_with_cause(s, 1, FRAMING_UNAVAILABLE, Some("session:x"), NOW)).unwrap();
    assert_eq!(fault(&db).as_deref(), Some(FRAMING_UNAVAILABLE));
    tx(&db, |s| next_work(s, &cfg(), NOW)).unwrap();
    assert_eq!(fault(&db), None);
}

/// A frozen cohort with one admitted entrant `m` (cohort 1, seq 1).
fn admitted_one(tag: u8) -> (Sqlite, Miner) {
    let db = db_with_open_cohort(2);
    let m = miner(1, 3, 1, tag);
    m.commit(&db, OPEN_BLOCK + 3, 0);
    assert_eq!(m.create(&db).0.status, 201);
    upload_all(&db, &m);
    frame_ok(&db, &m);
    admit_all(&db);
    let cutoff = OPEN_BLOCK + 360;
    close_at(&db, cutoff);
    let payload = m.upload.opening.chain_payload().to_vec();
    snapshot(&db, cutoff, &m, Some(OPEN_BLOCK + 3), &payload, Some(m.uid), Some(m.registration_block));
    let b = tx(&db, |s| try_freeze(s, &cfg(), NOW)).unwrap().unwrap();
    assert!(matches!(b, Boundary::Frozen { admitted: 1, .. }), "{b:?}");
    assert_eq!(state_of(&db, &m.session()).0, "admitted");
    (db, m)
}

fn package_of_session(db: &Sqlite, id: &str) -> String {
    let rows = tx(db, |s| s.exec("SELECT package_sha256 FROM adm_sessions WHERE session = ?", &[SqlValue::Text(id.into())])).unwrap();
    text(&rows[0], 0).unwrap().to_owned()
}

/// Review finding (correctness 1, design R4 and skeptic K2): an admitted
/// entrant's object lost after the freeze is the coordinator's fault. A
/// scoring lane's load (or publication) that meets it audits the session
/// (`object_fault`); the audit lists the object `repair_needed`, the lane
/// holds uncounted while the session waits in `repair`, and the miner's
/// re-PUT (recorded by the sweep or the repair route) ends the hold.
#[test]
fn an_admitted_entrants_lost_object_is_audited_listed_and_repaired() {
    let (db, m) = admitted_one(0x31);
    let id = m.session();
    let package = package_of_session(&db, &id);
    // Another package, or another cohort: nothing to hold.
    assert!(!tx(&db, |s| object_fault(s, 1, FaultBy::Package(&"00".repeat(32)), NOW)).unwrap());
    assert!(!tx(&db, |s| object_fault(s, 2, FaultBy::Package(&package), NOW)).unwrap());
    // The lane's load met an object absent: the session is audited.
    assert!(tx(&db, |s| object_fault(s, 1, FaultBy::Package(&package), NOW)).unwrap(), "the lane holds");
    assert_eq!(stage_of(&db, &id).as_deref(), Some("audit"));
    assert_eq!(tx(&db, |s| verify_due(s, NOW)).unwrap(), Some((id.clone(), "audit".into())));
    assert!(tx(&db, |s| has_work(s, &cfg(), Some(NOW))).unwrap(), "the audit is due work");
    let probes = tx(&db, |s| audit_probes(s, &id)).unwrap();
    assert_eq!(probes.len(), 7);
    let gone = probes[2].clone();
    let heads: Vec<(Probe, Head)> = probes
        .iter()
        .cloned()
        .map(|p| {
            let h = if p == gone { Head::Absent } else { stored(p.sha256, p.len, p.etag.as_deref().unwrap()) };
            (p, h)
        })
        .collect();
    let applied = tx(&db, |s| apply_heads(s, &heads, Recorder::Recorded, NOW)).unwrap();
    tx(&db, |s| audit_done(s, &id, &applied, NOW)).unwrap();
    assert_eq!(stage_of(&db, &id).as_deref(), Some("repair"));
    assert_eq!(state_of(&db, &id).0, "admitted", "never refused: the loss is ours");
    // Still held: a second lane's report starts no second audit.
    assert!(tx(&db, |s| object_fault(s, 1, FaultBy::Package(&package), NOW)).unwrap());
    assert_eq!(stage_of(&db, &id).as_deref(), Some("repair"));
    // The miner sees it: the session read lists the object.
    let req = m.signed(&db, "GET", &format!("/admission/v2/sessions/{id}"), &[]);
    let r = tx(&db, |s| Ok::<_, EvalError>(read(s, &chain(), &req, &id))).unwrap();
    assert_eq!(r.body["repair_needed"][0].as_str(), Some(hex::encode(gone.sha256).as_str()), "{}", r.body);
    // Repair mints exactly that object's URL.
    let rq = m.post(&db, "repair");
    let Step::Heads(rp) = tx(&db, |s| Ok::<_, EvalError>(repair(s, &chain(), &rq, &id))).unwrap() else { panic!() };
    let still: Vec<(Probe, Head)> = rp.iter().cloned().map(|p| {
        let h = if p == gone { Head::Absent } else { stored(p.sha256, p.len, p.etag.as_deref().unwrap()) };
        (p, h)
    }).collect();
    tx(&db, |s| apply_heads(s, &still, Recorder::Recorded, NOW)).unwrap();
    let (_, job) = drive(&db, tx(&db, |s| Ok::<_, EvalError>(repair_after(s, &id))).unwrap());
    assert_eq!(job.unwrap().entries.len(), 1);
    // The alarm sweep heads it (missing = 1), and records the re-PUT.
    let sweep = tx(&db, |s| sweep_begin(s, NOW + TICK_MS)).unwrap().unwrap();
    assert!(sweep.heads.iter().any(|p| p.key == gone.key), "{sweep:?}");
    let back = vec![(gone.clone(), stored(gone.sha256, gone.len, gone.etag.as_deref().unwrap()))];
    tx(&db, |s| apply_heads(s, &back, Recorder::Recorded, NOW)).unwrap();
    assert_eq!(stage_of(&db, &id).as_deref(), None, "the hold ends: the lanes and publication resume");
    let r = tx(&db, |s| Ok::<_, EvalError>(read(s, &chain(), &req, &id))).unwrap();
    assert_eq!(r.body["repair_needed"].as_array().map(Vec::len).unwrap_or(0), 0);
    // Publication finding a missing object audits the same way (by the
    // roster entry's commitment).
    let entry = PublicationEntry { commitment: m.upload.commitment.hex(), state: "missing".into() };
    let v = tx(&db, |s| publication_report(s, 1, "testnet-c1", &[entry], NOW)).unwrap();
    assert_eq!(v["held"].as_u64(), Some(1));
    assert_eq!(stage_of(&db, &id).as_deref(), Some("audit"));
}

/// Review finding (correctness 3, skeptic K1): a recorded object found
/// present with its committed bytes but a forbidden stored attribute (a
/// repair PUT with an unsigned Content-Type) is deleted (only while still
/// unclean) and listed missing, in every state, so the miner can PUT it
/// again cleanly; `object_replaced` stays for other bytes or another tag
/// before admission.
#[test]
fn a_dirty_recorded_object_is_deleted_for_a_clean_put_in_every_state() {
    let dirty = |p: &Probe| Head::Present(Stored { size: p.len, sha256: Some(p.sha256), etag: p.etag.clone().unwrap(), clean: false });
    // Admitted.
    let (db, m) = admitted_one(0x41);
    let id = m.session();
    let package = package_of_session(&db, &id);
    let probes = tx(&db, |s| {
        object_fault(s, 1, FaultBy::Package(&package), NOW)?;
        audit_probes(s, &id)
    })
    .unwrap();
    let bad = probes[1].clone();
    let heads: Vec<(Probe, Head)> = probes.iter().cloned().map(|p| {
        let h = if p == bad { dirty(&p) } else { stored(p.sha256, p.len, p.etag.as_deref().unwrap()) };
        (p, h)
    }).collect();
    let applied = tx(&db, |s| apply_heads(s, &heads, Recorder::Recorded, NOW)).unwrap();
    assert_eq!(applied.deletes.len(), 1);
    assert_eq!((applied.deletes[0].key.as_str(), applied.deletes[0].etag.as_str()), (bad.key.as_str(), bad.etag.as_deref().unwrap()));
    tx(&db, |s| audit_done(s, &id, &applied, NOW)).unwrap();
    assert_eq!((state_of(&db, &id).0.as_str(), stage_of(&db, &id).as_deref()), ("admitted", Some("repair")));
    let req = m.signed(&db, "GET", &format!("/admission/v2/sessions/{id}"), &[]);
    let r = tx(&db, |s| Ok::<_, EvalError>(read(s, &chain(), &req, &id))).unwrap();
    assert_eq!(r.body["repair_needed"].as_array().unwrap().len(), 1);
    // Verifying (the run met it) and verified: the same, never object_replaced.
    for verified in [false, true] {
        let db = db_with_open_cohort(4);
        let m = miner(2, 4, 1, 0x42);
        m.commit(&db, OPEN_BLOCK + 2, 0);
        m.create(&db);
        upload_all(&db, &m);
        frame_ok(&db, &m);
        let id = m.session();
        if verified {
            admit_all(&db);
            assert_eq!(state_of(&db, &id).0, "verified");
        }
        let req = m.post(&db, "repair");
        let Step::Heads(rp) = tx(&db, |s| Ok::<_, EvalError>(repair(s, &chain(), &req, &id))).unwrap() else { panic!() };
        let bad = rp[0].clone();
        let heads: Vec<(Probe, Head)> = rp.iter().cloned().map(|p| {
            let h = if p == bad { dirty(&p) } else { stored(p.sha256, p.len, p.etag.as_deref().unwrap()) };
            (p, h)
        }).collect();
        let applied = tx(&db, |s| apply_heads(s, &heads, Recorder::Recorded, NOW)).unwrap();
        assert_eq!(applied.deletes.len(), 1, "verified {verified}");
        let st = if verified { "verified" } else { "verifying" };
        assert_eq!(state_of(&db, &id), (st.into(), None), "never object_replaced for the committed bytes");
        let (_, job) = drive(&db, tx(&db, |s| Ok::<_, EvalError>(repair_after(s, &id))).unwrap());
        assert_eq!(job.unwrap().entries.len(), 1, "a fresh URL for the deleted object");
    }
}

/// Review finding (security 3): a queued delete never removes a key a live
/// session holds (its own heads delete an unclean copy conditionally), and
/// a v1 or orphan key is deleted as before.
#[test]
fn a_queued_delete_never_removes_a_live_sessions_object() {
    let (db, m) = admitted_one(0x51);
    let probes = tx(&db, |s| objects_of(s, &m.session())).unwrap();
    let key = object_key(1, &m.upload.commitment, probes[0].kind);
    assert!(!tx(&db, |s| queued_delete_allowed(s, &key)).unwrap());
    let orphan = object_key(1, &Digest([9; 32]), ObjectKind::Header);
    assert!(tx(&db, |s| queued_delete_allowed(s, &orphan)).unwrap());
    assert!(tx(&db, |s| queued_delete_allowed(s, "admission/testnet-c1/x/a1.object")).unwrap());
}

/// Review round 2 (correctness 2): a load that reports an object missing or
/// changed while the audit finds every recorded object present with its
/// recorded entity tag is the read path's failure (an etag-conditional
/// read the platform refused, say). It is counted, never a silent loop: a
/// verifying session's verdict takes a platform try with the growing
/// backoff and pauses its cohort (`scorer_unavailable`) at the plan's bound;
/// an admitted entrant's next lane report answers "not held" (the lane
/// counts it as a start failure) and the status counts it.
#[test]
fn an_object_read_failure_with_every_object_intact_is_counted() {
    let db = db_with_open_cohort(4);
    let m = miner(1, 3, 1, 0x91);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    upload_all(&db, &m);
    frame_ok(&db, &m);
    let id = m.session();
    let intact = |db: &Sqlite| -> Applied {
        let probes = tx(db, |s| audit_probes(s, &id)).unwrap();
        assert_eq!(probes.len(), 7);
        let heads: Vec<(Probe, Head)> =
            probes.iter().cloned().map(|p| { let h = stored(p.sha256, p.len, p.etag.as_deref().unwrap()); (p, h) }).collect();
        tx(db, |s| apply_heads(s, &heads, Recorder::Recorded, NOW)).unwrap()
    };
    let bound = tx(&db, window).unwrap().unwrap().failures_before_pause;
    let mut at = NOW;
    for round in 1..=bound {
        let runs = tx(&db, |s| claim_runs(s, &env(), 8, at)).unwrap();
        assert_eq!(runs.len(), 1, "round {round}: claimable after its backoff");
        let changed = LaneOutcome::Platform(crate::eval::scorer_client::OBJECT_CHANGED.into());
        tx(&db, |s| record_run(s, &env(), &runs[0], &changed, 3, at)).unwrap();
        assert_eq!(stage_of(&db, &id).as_deref(), Some("audit"));
        let applied = intact(&db);
        tx(&db, |s| audit_done(s, &id, &applied, at)).unwrap();
        assert_eq!(stage_of(&db, &id).as_deref(), Some("verdict"));
        assert!(tx(&db, |s| claim_runs(s, &env(), 8, at)).unwrap().is_empty(), "round {round}: backed off");
        let st = tx(&db, status).unwrap();
        assert_eq!(st["platform_retries"]["max_platform_tries"].as_i64(), Some(i64::from(round)), "{}", st["platform_retries"]);
        assert_eq!(st["platform_retries"]["latest_code"].as_str(), Some(OBJECT_READ_FAULT));
        at += 2 * 3_600_000;
    }
    let fault = tx(&db, |s| crate::eval::engine::cohort(s, 1)).unwrap().fault;
    assert_eq!(fault.as_deref(), Some(crate::eval::engine::SCORER_UNAVAILABLE), "paused at the plan's bound");
    assert_eq!(state_of(&db, &id).0, "verifying", "never the miner's");

    // Admitted: the first report holds (and audits); once an audit found
    // everything intact, the next one is the lane's to count.
    let (db, m) = admitted_one(0x92);
    let id = m.session();
    let package = package_of_session(&db, &id);
    assert!(tx(&db, |s| object_fault(s, 1, FaultBy::Package(&package), NOW)).unwrap(), "held");
    assert!(tx(&db, |s| object_held(s, 1, &package)).unwrap());
    let probes = tx(&db, |s| audit_probes(s, &id)).unwrap();
    let heads: Vec<(Probe, Head)> =
        probes.iter().cloned().map(|p| { let h = stored(p.sha256, p.len, p.etag.as_deref().unwrap()); (p, h) }).collect();
    let applied = tx(&db, |s| apply_heads(s, &heads, Recorder::Recorded, NOW)).unwrap();
    tx(&db, |s| audit_done(s, &id, &applied, NOW)).unwrap();
    assert_eq!(stage_of(&db, &id), None);
    assert!(!tx(&db, |s| object_held(s, 1, &package)).unwrap(), "released: the lane loads again");
    assert_eq!(tx(&db, status).unwrap()["objects"]["object_read_faults"].as_i64(), Some(1));
    assert!(!tx(&db, |s| object_fault(s, 1, FaultBy::Package(&package), NOW)).unwrap(), "counted by the lane");
    assert_eq!(stage_of(&db, &id).as_deref(), Some("audit"), "still audited: a real loss is still found");
    // A real loss found later holds again and clears the count.
    let probes = tx(&db, |s| audit_probes(s, &id)).unwrap();
    let gone = probes[0].clone();
    let heads: Vec<(Probe, Head)> = probes.iter().cloned().map(|p| {
        let h = if p == gone { Head::Absent } else { stored(p.sha256, p.len, p.etag.as_deref().unwrap()) };
        (p, h)
    }).collect();
    let applied = tx(&db, |s| apply_heads(s, &heads, Recorder::Recorded, NOW)).unwrap();
    tx(&db, |s| audit_done(s, &id, &applied, NOW)).unwrap();
    assert_eq!(stage_of(&db, &id).as_deref(), Some("repair"));
    assert!(tx(&db, |s| object_fault(s, 1, FaultBy::Package(&package), NOW)).unwrap(), "held for the repair");
    assert_eq!(tx(&db, status).unwrap()["objects"]["object_read_faults"].as_i64(), Some(0));
}

/// Review round 2 (correctness 3, security 1): a lane held for an
/// entrant's repair asks the cohort before it starts a Container
/// (`object_held`), and the cohort snapshot names exactly the hotkeys whose
/// sessions wait for a repair, so the gate lets only those read and repair
/// unregistered.
#[test]
fn held_lanes_and_the_snapshots_repair_list_follow_the_repair_stage() {
    let (db, m) = admitted_one(0x93);
    let id = m.session();
    let package = package_of_session(&db, &id);
    assert_eq!(tx(&db, repair_list).unwrap(), (Vec::new(), false));
    assert!(!tx(&db, |s| object_held(s, 1, &package)).unwrap());
    assert!(tx(&db, |s| object_fault(s, 1, FaultBy::Package(&package), NOW)).unwrap());
    let probes = tx(&db, |s| audit_probes(s, &id)).unwrap();
    let gone = probes[0].clone();
    let heads: Vec<(Probe, Head)> = probes.iter().cloned().map(|p| {
        let h = if p == gone { Head::Absent } else { stored(p.sha256, p.len, p.etag.as_deref().unwrap()) };
        (p, h)
    }).collect();
    let applied = tx(&db, |s| apply_heads(s, &heads, Recorder::Recorded, NOW)).unwrap();
    tx(&db, |s| audit_done(s, &id, &applied, NOW)).unwrap();
    assert!(tx(&db, |s| object_held(s, 1, &package)).unwrap(), "no Container while the miner has not repaired");
    assert!(!tx(&db, |s| object_held(s, 2, &package)).unwrap() && !tx(&db, |s| object_held(s, 1, &"00".repeat(32))).unwrap());
    assert_eq!(tx(&db, repair_list).unwrap(), (vec![hex::encode(account(1).0)], false));
    let back = vec![(gone.clone(), stored(gone.sha256, gone.len, gone.etag.as_deref().unwrap()))];
    tx(&db, |s| apply_heads(s, &back, Recorder::Recorded, NOW)).unwrap();
    assert!(!tx(&db, |s| object_held(s, 1, &package)).unwrap(), "repaired: the lane loads");
    assert_eq!(tx(&db, repair_list).unwrap(), (Vec::new(), false));
    // Past one roster of hotkeys, the list names everyone instead.
    let max = slop_ninja_admission_wire::internal::REPAIR_LIST_MAX;
    tx(&db, |s| {
        for n in 0..=max {
            s.exec(
                "INSERT INTO adm_sessions (session, seq, cohort, hotkey, upload, commitment, content, header_sha256, header_len,
                    state, stage, created_ms, updated_ms)
                 VALUES (?, 1, 1, ?, '', ?, '', '', 0, 'admitted', 'repair', 0, 0)",
                &[SqlValue::Text(format!("s{n}")), SqlValue::Text(format!("{n:064x}")), SqlValue::Text(format!("c{n}"))],
            )?;
        }
        Ok::<_, EvalError>(())
    })
    .unwrap();
    assert_eq!(tx(&db, repair_list).unwrap(), (Vec::new(), true));
}

/// Review round 2 (security 2): one conditional delete per key is in
/// flight at a time, so two paths that headed the same unclean object never
/// both delete (the second could remove the clean copy PUT after the
/// first); a marker left by a dead invocation is taken over after the
/// longest request scope.
#[test]
fn one_conditional_delete_per_key_is_in_flight() {
    let db = db_with_open_cohort(4);
    let k = "admission/v2/c1/aa/h";
    assert!(tx(&db, |s| delete_begin(s, k, NOW)).unwrap(), "the first path deletes");
    assert!(!tx(&db, |s| delete_begin(s, k, NOW + 1)).unwrap(), "the second leaves it to the first");
    assert!(tx(&db, |s| delete_begin(s, "admission/v2/c1/aa/f/bb", NOW + 1)).unwrap(), "per key");
    tx(&db, |s| delete_end(s, k)).unwrap();
    assert!(tx(&db, |s| delete_begin(s, k, NOW + 2)).unwrap(), "released");
    let stale = NOW + 2 + crate::arena_routes::UNBOUNDED_FETCH_MS;
    assert!(!tx(&db, |s| delete_begin(s, k, stale - 1)).unwrap());
    assert!(tx(&db, |s| delete_begin(s, k, stale)).unwrap(), "a dead invocation's marker is taken over");
}

/// Review finding (correctness 11): URLs the signer minted after `closing`
/// was persisted (by another invocation) are never handed out: the mint
/// counts as the signer's success, the answer is `after_cutoff` (or the
/// session the close ended, without URLs).
#[test]
fn urls_minted_after_the_close_is_persisted_are_never_handed_out() {
    let db = db_with_open_cohort(4);
    let m = miner(1, 3, 1, 0x61);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    let body = m.upload.to_bytes().unwrap();
    let mut r = m.signed(&db, "POST", "/admission/v2/sessions", &body);
    r.body = Some(hex::encode(&body));
    let Step::Mint(job) = tx(&db, |s| Ok::<_, EvalError>(create(s, &chain(), &r, NOW))).unwrap() else { panic!() };
    // The close lands while the signer mints.
    epoch(&db, 12, OPEN_BLOCK + 360, NOW + 2);
    assert!(tx(&db, |s| canary_begin(s, NOW + 2)).unwrap().is_some());
    assert!(tx(&db, |s| canary_done(s, Ok(()), NOW + 3)).unwrap());
    assert_eq!(tx(&db, window).unwrap().unwrap().state, "closing");
    let reply = tx(&db, |s| finish_mint(s, &job, Ok(minted(&job)), NOW + 4)).unwrap();
    assert_eq!(code(&reply), "after_cutoff");
    assert!(!reply.body.to_string().contains("r2.cloudflarestorage.com"), "no URL: {}", reply.body);
    // The close sweep ends it: a later mint's answer is the ended session.
    sweep_close(&db, |_| Head::Absent);
    let reply = tx(&db, |s| finish_mint(s, &job, Ok(minted(&job)), NOW + 5)).unwrap();
    assert!(!reply.body.to_string().contains("r2.cloudflarestorage.com"), "no URL: {}", reply.body);
    assert_eq!(state_of(&db, &m.session()).1.as_deref(), Some("incomplete_at_cutoff"));
}

/// Review finding (correctness 2): the front every session passes before
/// any file URL exists is part of the upload path. A header settle the
/// platform could not run (R2, the upload Worker's binding or header route)
/// persists `upload_front_unavailable`, as does a failed link probe in the
/// close's canary (the header route and the upload Worker's registration
/// read); either defers the close until a settle the upload Worker answers,
/// or a passing canary, clears it.
#[test]
fn a_front_outage_is_persisted_and_defers_the_close() {
    let db = db_with_open_cohort(2);
    let admitting = |db: &Sqlite| tx(db, window).unwrap().unwrap().admitting();
    let faults = |db: &Sqlite| tx(db, status).unwrap()["upload_path"]["faults"].clone();
    let m = miner(1, 3, 1, 0x71);
    m.commit(&db, OPEN_BLOCK + 1, 0);
    m.create(&db);
    assert_eq!(m.settle(&db, HeaderSeen::Platform("header call")), Settled::Platform("header call"));
    assert_eq!(faults(&db), serde_json::json!([UPLOAD_FRONT_UNAVAILABLE]));
    assert_eq!(state_of(&db, &m.session()), ("uploading".into(), None), "never the miner's");
    // Review round 2: an answered outcome the eval side finds inconsistent
    // (version skew between the Workers, or a faulty upload Worker) is the
    // same fault, persisted, and never clears it.
    let mut lying = m.checked();
    if let HeaderSeen::Checked { outcome, .. } = &mut lying
        && let HeaderResult::Checked { content, .. } = &mut outcome.result
    {
        *content = "00".repeat(32);
    }
    assert_eq!(m.settle(&db, lying), Settled::Platform("header outcome content"));
    assert_eq!(faults(&db), serde_json::json!([UPLOAD_FRONT_UNAVAILABLE]));
    assert_eq!(
        tx(&db, status).unwrap()["upload_path"]["front_error"],
        serde_json::json!("header outcome content"),
        "{}",
        tx(&db, status).unwrap()["upload_path"]
    );
    // The boundary arrives: deferred; the canary retries each tick.
    epoch(&db, 12, OPEN_BLOCK + 360, NOW + 20);
    assert!(admitting(&db), "the front fault defers the close");
    assert_eq!(state_of(&db, &m.session()), ("uploading".into(), None), "never the miner's");
    assert_eq!(tx(&db, |s| canary_begin(s, NOW + 20)).unwrap(), Some(1));
    // The canary's PUT passed but its link probe failed: still faulted.
    assert!(!tx(&db, |s| canary_done(s, Err(CanaryFailure::Front("registration read")), NOW + 30)).unwrap());
    assert_eq!(faults(&db), serde_json::json!([UPLOAD_FRONT_UNAVAILABLE]));
    epoch(&db, 13, OPEN_BLOCK + 720, NOW + 40);
    assert!(admitting(&db));
    // A header settle the upload Worker answered clears it; the close waits
    // for the first boundary after the clear, with its own canary.
    assert_eq!(m.settle(&db, m.checked()), Settled::Recorded);
    assert_eq!(faults(&db), serde_json::json!([]));
    epoch(&db, 14, OPEN_BLOCK + 1080, NOW + 2 * TICK_MS);
    assert!(tx(&db, |s| canary_begin(s, NOW + 2 * TICK_MS)).unwrap().is_some());
    assert!(tx(&db, |s| canary_done(s, Ok(()), NOW + 2 * TICK_MS + 1)).unwrap());
    assert_eq!(tx(&db, window).unwrap().unwrap().state, "closing");
}

/// Arena witnesses of admission v2 (`--features arena-witness`, one test
/// thread): each route's heaviest native path at its bounds, under
/// `Mode::Strict` in exactly its declared footprint (`arena_routes`). Each
/// path runs once outside the witness first, so lazily built statics (the
/// spec tables) are not counted as the route's.
#[cfg(feature = "arena-witness")]
mod witness {
    use super::*;
    use crate::arena_routes::{
        ADMISSION_EVENT_BYTES, ADMISSION_STEP_BYTES, R_COHORT_ADMISSION, R_COHORT_SESSION, R_QUEUE_ADMISSION,
        cohort_session_bytes,
    };
    use crate::arena_witness::witness;
    use slop_ninja_arena::{Mode, admit_current_block, set_mode};

    fn read_in(input: &[u8]) -> Vec<u8> {
        assert_eq!(admit_current_block(input.len().max(1), 1), Ok(true));
        let mut out = Vec::with_capacity(input.len().max(1));
        out.extend_from_slice(input);
        out
    }

    /// One session's whole life through the session operations: creation
    /// and its mint, `urls` minting the spec's whole file table, `complete`
    /// heading every file and building the spec-maximum files manifest, an
    /// event's `observe`, and the manifest step.
    fn session_ops(db: &Sqlite, m: &Miner, observed: bool) {
        let body = m.upload.to_bytes().unwrap();
        let mut r = m.signed(db, "POST", "/admission/v2/sessions", &body);
        r.body = Some(hex::encode(&body));
        let id = m.session();
        let run = |f: &dyn Fn() -> usize| if observed { witness(R_COHORT_SESSION.route(), cohort_session_bytes(), f) } else { f() };
        run(&|| {
            let Step::Mint(job) = tx(db, |s| Ok::<_, EvalError>(create(s, &chain(), &r, NOW))).unwrap() else { panic!("mint") };
            let reply = tx(db, |s| finish_mint(s, &job, Ok(minted(&job)), NOW)).unwrap();
            serde_json::to_vec(&reply.body).unwrap().len()
        });
        assert_eq!(m.settle(db, m.checked()), Settled::Recorded);
        let req = m.post(db, "urls");
        run(&|| {
            let Step::Mint(job) = tx(db, |s| Ok::<_, EvalError>(urls(s, &chain(), &req, &id))).unwrap() else { panic!("mint") };
            assert_eq!(job.entries.len(), spec::ROLES.len(), "the whole file table");
            let reply = tx(db, |s| finish_mint(s, &job, Ok(minted(&job)), NOW)).unwrap();
            serde_json::to_vec(&reply.body).unwrap().len()
        });
        let req = m.post(db, "complete");
        run(&|| {
            let Step::Heads(probes) = tx(db, |s| Ok::<_, EvalError>(complete(s, &chain(), &req, &id))).unwrap() else { panic!("heads") };
            let heads: Vec<(Probe, Head)> = probes.into_iter().map(|p| {
                let h = stored(p.sha256, p.len, &etag_of(&p));
                (p, h)
            }).collect();
            tx(db, |s| apply_heads(s, &heads, Recorder::Upload, NOW)).unwrap();
            let reply = tx(db, |s| Ok::<_, EvalError>(complete_reply(s, &id))).unwrap().unwrap();
            serde_json::to_vec(&reply.body).unwrap().len()
        });
        let key = m.job().key;
        run(&|| match tx(db, |s| observe(s, &key)).unwrap() {
            Observed::Nothing | Observed::Heads(_) => 0,
            other => panic!("{other:?}"),
        });
        let step = || {
            let (_, bytes) = tx(db, |s| manifest_due(s, &id)).unwrap().unwrap();
            bytes.len()
        };
        if observed { witness(R_COHORT_ADMISSION, ADMISSION_STEP_BYTES, step) } else { step() };
    }

    #[test]
    fn arena_witness_admission_session_operations_fit() {
        set_mode(Mode::Enforce);
        let db = db_with_open_cohort(4);
        let warm = miner(1, 3, 1, 0x11);
        warm.commit(&db, OPEN_BLOCK + 1, 0);
        session_ops(&db, &warm, false);
        let m = miner(2, 4, 1, 0x12);
        m.commit(&db, OPEN_BLOCK + 2, 0);
        session_ops(&db, &m, true);
    }

    /// The alarm's admission steps at their bounds: a sweep with
    /// `R2_CALLS_PER_STEP` probes, and a reconciliation listing page of the
    /// platform's 1,000 keys at R2's key length.
    #[test]
    fn arena_witness_admission_steps_fit_at_their_bounds() {
        set_mode(Mode::Enforce);
        let db = db_with_open_cohort(4);
        let m = miner(1, 3, 1, 0x11);
        m.commit(&db, OPEN_BLOCK + 1, 0);
        m.create(&db);
        assert_eq!(m.settle(&db, m.checked()), Settled::Recorded);
        let req = m.post(&db, "urls");
        drive(&db, tx(&db, |s| Ok::<_, EvalError>(urls(s, &chain(), &req, &m.session()))).unwrap());
        // Copies of the session until the sweep has more unrecorded objects
        // than one step heads.
        let per = spec::ROLES.len();
        let copies = R2_CALLS_PER_STEP.div_ceil(per) + 1;
        tx(&db, |s| {
            let id = SqlValue::Text(m.session());
            s.exec(
                "WITH RECURSIVE k(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM k WHERE n < ?)
                 INSERT INTO adm_sessions (session, seq, cohort, hotkey, upload, commitment, content, header_sha256,
                    header_len, state, stage, refusal, pos_block, pos_extrinsic, next_cohort, files, package_sha256,
                    manifest_key, model_sha256, mints, url_until_ms, created_ms, updated_ms, cleaned, step_tries,
                    step_after_ms, published)
                 SELECT printf('%064d', k.n), seq, cohort, hotkey, upload, printf('%064d', k.n), content, header_sha256,
                    header_len, state, stage, refusal, pos_block, pos_extrinsic, next_cohort, files, package_sha256,
                    manifest_key, model_sha256, mints, url_until_ms, created_ms, updated_ms, cleaned, step_tries,
                    step_after_ms, published FROM adm_sessions, k WHERE session = ?",
                &[SqlValue::Integer(copies as i64), id.clone()],
            )?;
            s.exec(
                "WITH RECURSIVE k(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM k WHERE n < ?)
                 INSERT INTO adm_objects (session, kind, sha256, len, etag, missing, close_headed)
                 SELECT printf('%064d', k.n), kind, sha256, len, etag, missing, close_headed FROM adm_objects, k
                 WHERE session = ?",
                &[SqlValue::Integer(copies as i64), id],
            )?;
            Ok::<_, EvalError>(())
        })
        .unwrap();
        // Warm: one sweep outside the witness.
        let warm = tx(&db, |s| sweep_begin(s, NOW + TICK_MS)).unwrap().unwrap();
        assert_eq!(warm.heads.len(), R2_CALLS_PER_STEP, "the sweep stops at the step's call bound");
        let probes = witness(R_COHORT_ADMISSION, ADMISSION_STEP_BYTES, || {
            let sweep = tx(&db, |s| sweep_begin(s, NOW + 2 * TICK_MS)).unwrap().unwrap();
            sweep.heads.len()
        });
        assert_eq!(probes, R2_CALLS_PER_STEP);
        // A listing page: 1,000 keys at R2's key length (none parses, so
        // each is returned for deletion: the largest answer).
        let keys: Vec<String> = (0..1_000).map(|n| format!("{ADMISSION_PREFIX}{n:0>1011}")).collect();
        assert!(keys.iter().all(|k| k.len() == 1_024));
        let _ = tx(&db, |s| reconcile_page(s, &keys[..1], None, NOW)).unwrap();
        let gone = witness(R_COHORT_ADMISSION, ADMISSION_STEP_BYTES, || {
            tx(&db, |s| reconcile_page(s, &keys, Some(&"c".repeat(1_024)), NOW)).unwrap().len()
        });
        assert_eq!(gone, 1_000);
    }

    /// One admission event delivery at its bound: the message as read (at
    /// `EVENT_MESSAGE_BYTES`) and its parse.
    #[test]
    fn arena_witness_an_admission_event_fits() {
        set_mode(Mode::Enforce);
        let key = format!("{ADMISSION_PREFIX}c1/{}/h", "ab".repeat(32));
        let base = serde_json::json!({"account": "a", "action": "PutObject", "bucket": "b",
            "object": {"key": key, "size": 1, "eTag": "e"}, "eventTime": "t", "copySource": {"bucket": "", "object": ""}})
            .to_string();
        let pad = EVENT_MESSAGE_BYTES - base.len();
        let msg = base.replacen(r#""bucket":"""#, &format!(r#""bucket":"{}""#, "x".repeat(pad)), 1);
        assert_eq!(msg.len(), EVENT_MESSAGE_BYTES);
        let _ = event_key(&msg);
        let got = witness(R_QUEUE_ADMISSION, ADMISSION_EVENT_BYTES, || {
            let read = read_in(msg.as_bytes());
            let text = String::from_utf8(read).unwrap();
            event_key(&text).map(|k| k.len())
        });
        assert_eq!(got, Some(key.len()));
    }
}

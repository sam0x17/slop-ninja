//! `reward-v1` cohorts (plan v3) and fingerprint probe builds.
//!
//! A v3 cohort runs through the same phases, lanes and receipts as the v2
//! engine, with these differences:
//!
//! - **Draw.** Pure-human negatives are fresh and scored once. Each mixed
//!   child's parent is, in order, a same-cohort negative (one child per parent
//!   per cohort), an earlier passage with fewer than `reuse.max_uses` parent
//!   uses whose last use is at least `reuse.min_gap` cohorts earlier (oldest
//!   first), or fresh stock used only as a parent (`docset = 'source'`, never a
//!   task). Same-cohort reuse counts toward the limit. A reused parent avoids
//!   the generators of its earlier uses where the plan offers another. Two
//!   fresh canary audit draws (`audit_a`, `audit_b`) and every probe version's
//!   documents (`probe`) join the cohort's documents. Mixed and model-only
//!   items carry the plan's spares; the seal keeps exactly the planned count
//!   of each kind by private rank, so the manifest's mix is exact. Under the
//!   seeded order the draw first applies the input screen to the pool
//!   ([`INPUT_SCREEN_RULE`]).
//! - **Tasks.** Under the seeded order (`reward_v1.task_order =
//!   seeded_rank_v1`, every standing template) every sealed document, batch,
//!   probe and audit alike, takes its task index from a private rank keyed by
//!   the document (`pool::rank(seed, "task", key)`), independent of the
//!   selection rank, its kind and its set, so position predicts neither.
//!   Which tasks the references score still names the batch set to anyone
//!   who reads their receipts or chunk maps, so the set stays hidden only
//!   where those are private. Plans that name no order keep the
//!   appended layout: the batch is tasks `[0, n)`, probe versions follow in
//!   version order, then audit draw A and audit draw B. References score the
//!   batch tasks only; every executed entrant scores every task; a chunk is a
//!   window of its job's task list, starting at a task index. The package
//!   reads every vector back into the appended layout ([`canonical_layout`]),
//!   so the measurements, the probe fingerprints (in probe-record order) and
//!   the package are the same under either order. An entrant whose inference
//!   content is already published is never executed.
//! - **Package.** [`slop_ninja_protocol::reward_v1::finalize`] with the
//!   measurements of [`Measure`]: near statistics on the batch and probe,
//!   cluster pairs, gates, and promotion audits.
//! - **Closure.** At the closing boundary the cohort's contents and canonical
//!   fingerprints join the published index, a promotion advances the
//!   reference chain, and a signed [`CohortClosure`] publishes the openings.
//!
//! A probe build draws, generates and seals a probe version like a batch,
//! scores the references on it, records their fingerprints and the private
//! probe record, and closes with no package or target.
//!
//! New code here keeps no global state, takes explicit versioned bounds from
//! the plan, and refuses rather than panics.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use crate::eval::engine::{
    self, Cohort, EvalConfig, EvalError, MixedSpec, R, ScratchSpec, cohort, cohort_seed, fault, i, item_digest,
    observed, opt_int, opt_text, parse_digest, s, set_phase, sha_hex,
};
use crate::eval::plan::{ArtifactJson, CohortPlan, RewardV1Plan};
use crate::eval::pool::{self, Draw, PoolRow};
use crate::eval::textgen;
use crate::sql::{Sql, SqlValue, StoreError, bind_u64, int, text, uint};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use slop_ninja_protocol::reward_v1::{self, ArchiveProbe, Graded, Measurements};
use slop_ninja_protocol::submission::{CohortClosure, PublishedOpening, SubmissionOpening};
use slop_ninja_protocol::{
    AccountId, BatchDesign, CalculationTargetV2, CanaryAudit, ChainBinding, CohortManifestV2, CohortSchedule,
    Component, ComponentRole, CutoffMode, Digest, EntrantFingerprint, EntrantScore, Fingerprint, Gate, GateFailure,
    HUMAN_FPR_LOOSE_REASON, InferenceContent, MAX_ROSTER, NearStats, NeuronIdentity, Nonce, Origin, OriginMix,
    Probability, ProbeBinding, ProbeMatch, PromotionAudit, PublishedIndex, Record, ReferenceModel, ReferenceScore,
    ReferenceSet, RewardRule, RewardV1Params, RosterEntry, ScorePackageV2, Stratum, domain_hash,
};
use std::collections::{BTreeMap, BTreeSet};

/// v8: `reward-v1` cohorts and probe builds.
/// - `docs.docset`: `batch`, `source` (a parent only), `probe`, `audit_a`,
///   `audit_b`, or `spare` (generated beyond the planned count, never a task).
/// - `docs.pool_row`: the pool row behind a human, source or audit document.
/// - `reuse_sources`: every negative and parent passage with its parent uses,
///   last parent cohort and the generators of its earlier children.
/// - `published_contents`, `fingerprints`: the published-artifact index.
/// - `probe_versions`: each probe version's private record binding.
/// - `reference_state`: `R_k` and `R_k-1` as the closed cohorts left them.
/// - `cohorts.v3`: the cohort's reward-v1 state (JSON); the epochs the
///   closure records; `jobs.tasks`: a job's task count.
pub const SCHEMA_V8: &[&str] = &[
    "ALTER TABLE docs ADD COLUMN docset TEXT NOT NULL DEFAULT 'batch'",
    "ALTER TABLE docs ADD COLUMN pool_row TEXT",
    "CREATE TABLE reuse_sources (
        row_id TEXT PRIMARY KEY, text_sha256 TEXT NOT NULL, source_group TEXT NOT NULL, domain TEXT NOT NULL,
        shard INTEGER NOT NULL, first_cohort INTEGER NOT NULL, uses INTEGER NOT NULL, last_cohort INTEGER,
        generators TEXT NOT NULL DEFAULT ''
    ) STRICT",
    "CREATE TABLE published_contents (content TEXT PRIMARY KEY, seq INTEGER NOT NULL) STRICT",
    "CREATE TABLE fingerprints (
        content TEXT NOT NULL, version INTEGER NOT NULL, fingerprint TEXT NOT NULL, vector_key TEXT NOT NULL,
        seq INTEGER NOT NULL, PRIMARY KEY (content, version)
    ) STRICT",
    "CREATE TABLE probe_versions (
        version INTEGER PRIMARY KEY, record_key TEXT NOT NULL, record_sha256 TEXT NOT NULL, size INTEGER NOT NULL,
        digest TEXT NOT NULL, seq INTEGER NOT NULL
    ) STRICT",
    "CREATE TABLE reference_state (
        id INTEGER PRIMARY KEY CHECK (id = 1), k INTEGER NOT NULL, current TEXT NOT NULL, predecessor TEXT
    ) STRICT",
    "ALTER TABLE cohorts ADD COLUMN v3 TEXT",
    "ALTER TABLE cohorts ADD COLUMN admission_closed_epoch INTEGER",
    "ALTER TABLE cohorts ADD COLUMN published_epoch INTEGER",
    "ALTER TABLE cohorts ADD COLUMN published_active INTEGER",
    "ALTER TABLE cohorts ADD COLUMN closure_digest TEXT",
    "ALTER TABLE jobs ADD COLUMN tasks INTEGER",
    "ALTER TABLE eval_targets ADD COLUMN rule TEXT NOT NULL DEFAULT 'testnet-reward-v0'",
    "CREATE INDEX docs_set ON docs (seq, docset, idx)",
];

/// v9: the rewrite-source ledger records each use's voice and operation and
/// the pool it came from, and counts the reuse gap in drawn reward-v1 cohorts
/// (`cohorts.reuse_ordinal`; probe builds take none) rather than in operator
/// cohort numbers. Existing rows are migrated in place.
pub const SCHEMA_V9: &[&str] = &[
    "ALTER TABLE reuse_sources ADD COLUMN voices TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE reuse_sources ADD COLUMN operations TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE reuse_sources ADD COLUMN pool_id TEXT",
    "ALTER TABLE reuse_sources ADD COLUMN first_ordinal INTEGER",
    "ALTER TABLE reuse_sources ADD COLUMN last_ordinal INTEGER",
    "ALTER TABLE cohorts ADD COLUMN reuse_ordinal INTEGER",
    "UPDATE cohorts SET reuse_ordinal = (
        SELECT COUNT(*) FROM cohorts c2
        WHERE c2.draw IS NOT NULL AND json_extract(c2.plan, '$.schema') = 'slop_ninja_eval_cohort_plan_v3'
          AND c2.seq <= cohorts.seq)
     WHERE draw IS NOT NULL AND json_extract(plan, '$.schema') = 'slop_ninja_eval_cohort_plan_v3'",
    "UPDATE reuse_sources SET
        first_ordinal = (SELECT c.reuse_ordinal FROM cohorts c WHERE c.cohort = reuse_sources.first_cohort),
        last_ordinal = (SELECT c.reuse_ordinal FROM cohorts c WHERE c.cohort = reuse_sources.last_cohort),
        pool_id = (SELECT json_extract(c.plan, '$.pool.pool_id') FROM cohorts c WHERE c.cohort = reuse_sources.first_cohort)",
];

pub const PROBE_RECORD_SCHEMA: &str = "slop_ninja_eval_probe_record_v1";
pub const PROBE_RECORD_DOMAIN: &str = "slop_ninja.eval/probe-record/v1";
pub const FINGERPRINT_DOMAIN: &str = "slop_ninja.eval/fingerprint-vector/v1";
pub const CANARY_DRAW_DOMAIN: &str = "slop_ninja.eval/canary-draw/v1";
pub const OPENING_NONCE_DOMAIN: &str = "slop_ninja.eval/testnet-opening-nonce/v1";
/// v2: the gap counts drawn reward-v1 cohorts (ordinals), candidates come
/// from the plan's pool, every use records its generator, voice and
/// operation, and canary audit humans join the ledger after their cohort.
pub const REUSE_RULE: &str = "rewrite_source_ledger_v2";
/// Draw modes. `exposed`: a pool the deployment lists as already exposed
/// (`EXPOSED_POOLS`), from which used humans may be drawn again (never-drawn
/// rows first) under the reuse limits; `fresh`: never-reuse stock, drawn only
/// for a cohort with an admitted external entrant.
pub const DRAW_EXPOSED: &str = "exposed";
pub const DRAW_FRESH: &str = "fresh";
/// The testnet canary without anchors: flags only from the plan's drill list.
pub const CANARY_RULE: &str = "testnet_drill_only_v1";
pub const DRILL_REASON: &str = "canary.drill";
/// A failed prediction on a human-only document counts as a false positive.
pub const FPR_RULE: &str = "fpr_at_operating_point_failures_flagged_v1";
/// The package build refused its own inputs (`reward_v1::finalize`, the
/// package's validation or its manifest binding). The same inputs fail the
/// same way on every retry, so the cohort faults and pauses for
/// reconciliation instead of retrying every tick.
pub const PACKAGE_REFUSED: &str = "package_refused";

// ---------------------------------------------------------------- artifacts

/// The inference content of a scorer artifact (`slop_ninja_a_artifact_files_v1`):
/// the contract and each inference component by role, SHA-256 and length. The
/// export manifest is metadata and excluded; any other role is refused.
pub fn content_of(manifest_bytes: &[u8]) -> Result<InferenceContent, &'static str> {
    let v: Value = serde_json::from_slice(manifest_bytes).map_err(|_| "artifact manifest json")?;
    let field = |o: &Value, k: &str| o.get(k).cloned().unwrap_or(Value::Null);
    if field(&v, "schema") != "slop_ninja_a_artifact_files_v1" {
        return Err("artifact manifest schema");
    }
    let contract = field(&v, "contract").as_str().ok_or("artifact contract")?.to_owned();
    let mut components = Vec::new();
    for f in field(&v, "files").as_array().ok_or("artifact files")? {
        let role = match field(f, "role").as_str().ok_or("artifact role")? {
            "encoder_graph" => ComponentRole::Graph,
            "heads" => ComponentRole::Weights,
            "tokenizer" => ComponentRole::Tokenizer,
            "calibration" => ComponentRole::Calibration,
            "contract" => ComponentRole::OutputSchema,
            "export_manifest" => continue,
            _ => return Err("undeclared artifact role"),
        };
        let sha = field(f, "sha256").as_str().and_then(parse_digest).ok_or("artifact file digest")?;
        let len = field(f, "bytes").as_u64().ok_or("artifact file length")?;
        components.push(Component { role, sha256: sha, len });
    }
    components.sort();
    let c = InferenceContent { contract, components };
    c.validate().map_err(|_| "inference content")?;
    Ok(c)
}

/// Content digest and artifact digest (the files manifest's SHA-256) of one artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactDigests {
    pub content: H,
    pub artifact: H,
}

pub fn artifact_digests(manifest_bytes: &[u8]) -> Result<ArtifactDigests, &'static str> {
    let c = content_of(manifest_bytes)?;
    Ok(ArtifactDigests {
        content: H(c.digest().map_err(|_| "content digest")?),
        artifact: H(Digest(sha2_digest(manifest_bytes))),
    })
}

fn sha2_digest(b: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(b).into()
}

// ----------------------------------------------------------------- v3 state

/// A digest stored as lowercase hex in the cohort's JSON state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct H(pub Digest);

impl Serialize for H {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for H {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let t = String::deserialize(d)?;
        parse_digest(&t).map(H).ok_or_else(|| serde::de::Error::custom("digest hex"))
    }
}

/// A canonical fingerprint pending at closure, with its private vector key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingFingerprint {
    pub content: H,
    pub version: u32,
    pub fingerprint: H,
    pub vector_key: String,
}

/// One closure fingerprint (an executed entrant on one probe version).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosureFingerprint {
    pub hotkey: H,
    pub version: u32,
    pub fingerprint: H,
}

impl ClosureFingerprint {
    pub fn record(&self) -> EntrantFingerprint {
        EntrantFingerprint { hotkey: AccountId(self.hotkey.0.0), version: self.version, fingerprint: self.fingerprint.0 }
    }
}

/// The cohort's reward-v1 state, stored as JSON on its row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct V3State {
    /// Content and artifact digests by job role and roster position, set at seal.
    #[serde(default)]
    pub reference: Option<ArtifactDigests>,
    #[serde(default)]
    pub predecessor: Option<ArtifactDigests>,
    #[serde(default)]
    pub entrants: Vec<ArtifactDigests>,
    /// Entrants (roster positions) never executed: their content was published.
    #[serde(default)]
    pub unexecuted: Vec<usize>,
    /// The reference index `k`.
    #[serde(default)]
    pub reference_index: u64,
    /// Batch, probe and audit task counts.
    #[serde(default)]
    pub batch_tasks: u32,
    #[serde(default)]
    pub probe_tasks: Vec<(u32, u32)>,
    #[serde(default)]
    pub audit_tasks: u32,
    /// Tasks in audit draw B, when it differs from draw A (absent: equal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_b_tasks: Option<u32>,
    /// Private digests of the two canary audit draws.
    #[serde(default)]
    pub audit_draws: Option<(H, H)>,
    /// The published index at seal, encoded (contents, fingerprints), hex.
    #[serde(default)]
    pub published_at_seal: Option<(String, String)>,
    /// The signed manifest's record bytes, hex.
    #[serde(default)]
    pub manifest_hex: Option<String>,
    /// Package outcome.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub promoted: Option<usize>,
    /// The index additions at closure: entrant contents and canonical
    /// fingerprints with their private vector keys.
    #[serde(default)]
    pub pending_contents: Vec<H>,
    #[serde(default)]
    pub pending_fingerprints: Vec<PendingFingerprint>,
    /// The closure's per-entrant fingerprints.
    #[serde(default)]
    pub closure_fingerprints: Vec<ClosureFingerprint>,
}

pub fn state(sql: &impl Sql, seq: i64) -> R<V3State> {
    let rows = sql.exec("SELECT v3 FROM cohorts WHERE seq = ?", &[i(seq)])?;
    match rows.first().and_then(|r| opt_text(r, 0)) {
        Some(t) => serde_json::from_str(&t).map_err(|_| EvalError::State("v3 state")),
        None => Ok(V3State::default()),
    }
}

fn save_state(sql: &impl Sql, seq: i64, st: &V3State) -> R<()> {
    let t = serde_json::to_string(st).map_err(|_| EvalError::State("v3 state"))?;
    sql.exec("UPDATE cohorts SET v3 = ? WHERE seq = ?", &[s(t), i(seq)])?;
    Ok(())
}

fn v1(plan: &CohortPlan) -> R<&RewardV1Plan> {
    plan.reward_v1.as_ref().ok_or(EvalError::Plan("reward_v1 block"))
}

// --------------------------------------------------------------- draw (cf)

/// An earlier passage eligible as a parent again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReuseCandidate {
    pub row: PoolRow,
    pub uses: u32,
    pub generators: Vec<String>,
    pub voices: Vec<String>,
    pub operations: Vec<String>,
}

/// The reuse ordinal of cohort `cohort`: one more than the latest drawn
/// reward-v1 cohort of this authority (probe builds take none). The reuse gap
/// counts these, so probe builds, abandoned queue entries and gaps in the
/// operator's cohort numbers never shorten it.
pub fn reuse_ordinal(sql: &impl Sql, cohort: u64) -> R<u64> {
    let rows = sql.exec(
        "SELECT COALESCE(MAX(reuse_ordinal), 0) FROM cohorts WHERE reuse_ordinal IS NOT NULL AND cohort <> ?",
        &[bind_u64(cohort)?],
    )?;
    let last = uint(rows.first().ok_or(StoreError("reuse ordinal"))?, 0)?;
    last.checked_add(1).ok_or(EvalError::State("reuse ordinal"))
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',').filter(|g| !g.is_empty()).map(str::to_owned).collect()
}

/// Earlier passages of the plan's pool that cohort `plan` may parent from:
/// fewer than `max_uses` parent uses, first seen in an earlier drawn cohort,
/// and the last use at least `min_gap` drawn cohorts back; oldest last use
/// first (a never-parented passage first by the cohort that drew it).
pub fn reuse_candidates(sql: &impl Sql, plan: &CohortPlan) -> R<Vec<ReuseCandidate>> {
    let ordinal = reuse_ordinal(sql, plan.cohort)?;
    reuse_candidates_at(sql, plan, ordinal)
}

/// [`reuse_candidates`] for a cohort with reuse ordinal `ordinal`.
pub fn reuse_candidates_at(sql: &impl Sql, plan: &CohortPlan, ordinal: u64) -> R<Vec<ReuseCandidate>> {
    let v = v1(plan)?;
    let rows = sql.exec(
        "SELECT row_id, text_sha256, source_group, domain, shard, uses, generators, voices, operations
         FROM reuse_sources
         WHERE uses < ? AND first_ordinal < ? AND (last_ordinal IS NULL OR last_ordinal + ? <= ?)
           AND pool_id = ?
         ORDER BY COALESCE(last_ordinal, first_ordinal), row_id",
        &[
            bind_u64(u64::from(v.reuse.max_uses))?,
            bind_u64(ordinal)?,
            bind_u64(v.reuse.min_gap)?,
            bind_u64(ordinal)?,
            s(&plan.pool.pool_id),
        ],
    )?;
    rows.iter()
        .map(|r| {
            Ok(ReuseCandidate {
                row: PoolRow {
                    row_id: text(r, 0)?.to_owned(),
                    text_sha256: text(r, 1)?.to_owned(),
                    source_group: text(r, 2)?.to_owned(),
                    domain: text(r, 3)?.to_owned(),
                    shard: u32::try_from(int(r, 4)?).map_err(|_| StoreError("shard"))?,
                },
                uses: u32::try_from(int(r, 5)?).map_err(|_| StoreError("uses"))?,
                generators: split_list(text(r, 6)?),
                voices: split_list(text(r, 7)?),
                operations: split_list(text(r, 8)?),
            })
        })
        .collect()
}

/// Rows of the plan's pool already in the rewrite-source ledger, and those
/// among them that may not parent a child in cohort `plan` (the reuse limit
/// or the gap). An exposed-stock draw may take any of them as a negative or
/// an audit human, but a fresh-parent draw skips them all (an eligible one is
/// already an earlier candidate) and a same-cohort negative among the
/// ineligible ones parents nothing.
pub fn ledger_rows(sql: &impl Sql, plan: &CohortPlan) -> R<(BTreeSet<String>, BTreeSet<String>)> {
    let v = v1(plan)?;
    let ordinal = reuse_ordinal(sql, plan.cohort)?;
    let rows = sql.exec(
        "SELECT row_id, uses >= ? OR (last_ordinal IS NOT NULL AND last_ordinal + ? > ?) FROM reuse_sources
         WHERE pool_id = ?",
        &[
            bind_u64(u64::from(v.reuse.max_uses))?,
            bind_u64(v.reuse.min_gap)?,
            bind_u64(ordinal)?,
            s(&plan.pool.pool_id),
        ],
    )?;
    let mut all = BTreeSet::new();
    let mut ineligible = BTreeSet::new();
    for r in &rows {
        let id = text(r, 0)?.to_owned();
        if int(r, 1)? != 0 {
            ineligible.insert(id.clone());
        }
        all.insert(id);
    }
    Ok((all, ineligible))
}

/// Records an accepted mixed child's generator against its parent passage.
pub fn record_child_generator(sql: &impl Sql, seq: i64, parent_idx: i64, generator: &str) -> R<()> {
    let rows = sql.exec("SELECT pool_row FROM docs WHERE seq = ? AND idx = ?", &[i(seq), i(parent_idx)])?;
    let Some(row_id) = rows.first().and_then(|r| opt_text(r, 0)) else {
        return Ok(());
    };
    sql.exec(
        "UPDATE reuse_sources SET generators = CASE WHEN generators = '' THEN ? ELSE generators || ',' || ? END
         WHERE row_id = ?",
        &[s(generator), s(generator), s(row_id)],
    )?;
    Ok(())
}

/// One probe document as the probe record lists it (no text).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeDoc {
    pub task: u32,
    pub kind: String,
    pub domain: String,
    pub share_ppb: u32,
    pub text_sha256: String,
}

/// The private record of one probe version: its documents and labels in
/// task order, the shards that hold their texts, and a fresh nonce. Its
/// digest is the manifest's probe binding digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeRecord {
    pub schema: String,
    pub version: u32,
    pub label: String,
    pub size: u32,
    pub nonce: String,
    pub docs: Vec<ProbeDoc>,
    /// Shard keys and SHA-256 in task order (the probe build's batch shards).
    pub shards: Vec<(String, String)>,
}

impl ProbeRecord {
    pub fn digest(bytes: &[u8]) -> Digest {
        domain_hash(PROBE_RECORD_DOMAIN, bytes)
    }
}

/// Everything the cohort's draw found, assembled by the Worker from the pool,
/// the ledger and the probe records.
#[derive(Clone, Debug)]
pub struct DrawV3 {
    pub negatives: Draw,
    pub audit_a: Option<Draw>,
    pub audit_b: Option<Draw>,
    /// Fresh rows drawn as potential parents (whole groups), in rank order.
    pub fresh: Option<Draw>,
    pub earlier: Vec<ReuseCandidate>,
    /// Probe documents with their texts, per version in use.
    pub probe: Vec<(u32, Vec<(ProbeDoc, String)>)>,
}

/// The parents chosen for the mixed children and the rows the draw consumes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parents {
    /// `(row id, origin)` per child, origin `same`, `earlier` or `fresh`.
    pub children: Vec<(String, &'static str)>,
    /// Fresh rows used as parents (marked used).
    pub fresh_used: Vec<PoolRow>,
}

/// Chooses the parents of `want` mixed children: same-cohort negatives with a
/// selectable span (except `ineligible` ones, which reached the reuse limit
/// or are inside the gap), then earlier eligible passages, then fresh stock.
/// Fresh groups are taken whole, and a fresh group's rows go together.
pub fn choose_parents(
    seed: &[u8; 32],
    negatives: &[PoolRow],
    earlier: &[ReuseCandidate],
    fresh: &[PoolRow],
    texts: &BTreeMap<String, String>,
    want: u32,
    ineligible: &BTreeSet<String>,
) -> Parents {
    let usable = |row: &PoolRow, key: &str| -> bool {
        texts
            .get(&row.row_id)
            .is_some_and(|t| textgen::select_span(textgen::sentences(t).len(), &item_digest(seed, key)).is_some())
    };
    let mut out = Parents::default();
    let mut left = want as usize;
    for r in negatives {
        if left == 0 {
            break;
        }
        if !ineligible.contains(&r.row_id) && usable(r, &format!("m:{}", r.row_id)) {
            out.children.push((r.row_id.clone(), "same"));
            left -= 1;
        }
    }
    for c in earlier {
        if left == 0 {
            break;
        }
        if usable(&c.row, &format!("m:{}", c.row.row_id)) {
            out.children.push((c.row.row_id.clone(), "earlier"));
            left -= 1;
        }
    }
    let mut groups_taken: BTreeSet<&str> = BTreeSet::new();
    for r in fresh {
        if left == 0 && !groups_taken.contains(r.source_group.as_str()) {
            continue;
        }
        if left > 0 && usable(r, &format!("m:{}", r.row_id)) {
            out.children.push((r.row_id.clone(), "fresh"));
            left -= 1;
            groups_taken.insert(r.source_group.as_str());
        }
    }
    // Every row of a group that parented a child is consumed, so the group
    // never splits between the stock and the cohort.
    out.fresh_used = fresh
        .iter()
        .filter(|r| groups_taken.contains(r.source_group.as_str()))
        .cloned()
        .collect();
    out
}

/// The private digest of an audit draw: its documents' digests in order and a
/// per-cohort private salt.
pub fn audit_draw_digest(seed: &[u8; 32], which: &str, shas: &[&str]) -> Digest {
    let mut payload = seed.to_vec();
    payload.extend_from_slice(which.as_bytes());
    for s in shas {
        payload.push(0);
        payload.extend_from_slice(s.as_bytes());
    }
    domain_hash(CANARY_DRAW_DOMAIN, &payload)
}

struct NewDoc {
    key: String,
    kind: &'static str,
    docset: &'static str,
    domain: String,
    group: String,
    parent_key: Option<String>,
    text: Option<String>,
    spec: Option<String>,
    share_ppb: Option<u32>,
    pool_row: Option<String>,
}

#[allow(clippy::too_many_arguments)]
/// Creates a v3 cohort's (or probe build's) documents, marks the consumed
/// rows used and updates the reuse ledger, in one transaction. Returns false,
/// with the cohort paused (`draw_shortfall`) and nothing marked used, when the
/// pool cannot supply the humans, audit draws or parents.
pub fn apply_draw(
    sql: &impl Sql,
    cfg: &EvalConfig,
    seq: i64,
    d: &DrawV3,
    parents: &Parents,
    texts: &BTreeMap<String, String>,
    ledger: &pool::Used,
    now_ms: u64,
) -> R<bool> {
    let c = cohort(sql, seq)?;
    if c.phase != "generating" {
        return Err(EvalError::State("draw outside generating"));
    }
    if int(sql.exec("SELECT COUNT(*) FROM docs WHERE seq = ?", &[i(seq)])?.first().ok_or(StoreError("count"))?, 0)?
        > 0
    {
        return Err(EvalError::State("already drawn"));
    }
    let plan = &c.plan;
    let v = v1(plan)?;
    let exposed = cfg.exposed_pools.contains(&plan.pool.pool_id);
    // Fresh stock only for a cohort with an admitted outside entrant.
    if !exposed && !fresh_allowed(sql, &c)? {
        return Err(EvalError::State(engine::FRESH_STOCK_REFUSED_CODE));
    }
    let seed = cohort_seed(&cfg.private_seed, &c.label);
    let [n_h, n_m, n_s] = plan.generated_counts();
    let c_a = plan.audit_humans();
    let c_b = plan.audit_b_humans();
    let audit_a: &[PoolRow] = d.audit_a.as_ref().map_or(&[], |x| x.rows.as_slice());
    let audit_b: &[PoolRow] = d.audit_b.as_ref().map_or(&[], |x| x.rows.as_slice());
    let consumed: Vec<&PoolRow> = d
        .negatives
        .rows
        .iter()
        .chain(audit_a)
        .chain(audit_b)
        .chain(parents.fresh_used.iter())
        .collect();
    // Fresh stock: never a row or group any cohort used, here or in the
    // ledger. Exposed stock may be drawn again.
    for r in consumed.iter().filter(|_| !exposed) {
        let used = sql.exec(
            "SELECT 1 FROM pool_used WHERE row_id = ? OR text_sha256 = ?
             UNION ALL SELECT 1 FROM pool_groups_used WHERE source_group = ?",
            &[s(&r.row_id), s(&r.text_sha256), s(&r.source_group)],
        )?;
        if !used.is_empty() || ledger.contains(r) {
            return Err(EvalError::State("pool row or group already used"));
        }
    }
    for r in consumed.iter().copied().chain(d.earlier.iter().map(|e| &e.row)) {
        if let Some(t) = texts.get(&r.row_id)
            && sha_hex(t.as_bytes()) != r.text_sha256
        {
            return Err(EvalError::State("drawn row text digest"));
        }
    }
    let short = d.negatives.rows.len() < n_h as usize
        || audit_a.len() < c_a as usize
        || audit_b.len() < c_b as usize
        || parents.children.len() < n_m as usize;
    if short {
        let have = json!({"humans": d.negatives.rows.len(), "audit_a": audit_a.len(), "audit_b": audit_b.len(),
            "parents": parents.children.len()});
        let want = json!({"humans": n_h, "audit": c_a, "audit_b": c_b, "parents": n_m});
        sql.exec(
            "UPDATE cohorts SET counts = ? WHERE seq = ?",
            &[s(json!({"rule": "reward_v1_exact_v1", "have": have, "want": want}).to_string()), i(seq)],
        )?;
        fault(sql, seq, "draw_shortfall", now_ms)?;
        return Ok(false);
    }
    let get_text = |row: &str| texts.get(row).cloned().ok_or(EvalError::State("drawn row text missing"));
    let mut docs: Vec<NewDoc> = Vec::new();
    for r in &d.negatives.rows {
        docs.push(NewDoc {
            key: format!("h:{}", r.row_id),
            kind: "human_only",
            docset: "batch",
            domain: r.domain.clone(),
            group: r.source_group.clone(),
            parent_key: None,
            text: Some(get_text(&r.row_id)?),
            spec: None,
            share_ppb: Some(0),
            pool_row: Some(r.row_id.clone()),
        });
    }
    let rows_by_id: BTreeMap<&str, &PoolRow> = d
        .negatives
        .rows
        .iter()
        .chain(parents.fresh_used.iter())
        .chain(d.earlier.iter().map(|e| &e.row))
        .map(|r| (r.row_id.as_str(), r))
        .collect();
    // Each parent's earlier generators, voices and operations, from the
    // ledger (an earlier candidate, or an exposed row drawn again).
    let history = |row_id: &str| -> R<(Vec<String>, Vec<String>, Vec<String>)> {
        let rows = sql.exec("SELECT generators, voices, operations FROM reuse_sources WHERE row_id = ?", &[s(row_id)])?;
        Ok(match rows.first() {
            Some(r) => (split_list(text(r, 0)?), split_list(text(r, 1)?), split_list(text(r, 2)?)),
            None => (Vec::new(), Vec::new(), Vec::new()),
        })
    };
    let mut variants: BTreeMap<String, (&'static str, &'static str)> = BTreeMap::new();
    for (row_id, origin) in &parents.children {
        let row = *rows_by_id.get(row_id.as_str()).ok_or(EvalError::State("parent row"))?;
        let parent_text = get_text(row_id)?;
        let parent_key = if *origin == "same" {
            format!("h:{row_id}")
        } else {
            let key = format!("p:{row_id}");
            docs.push(NewDoc {
                key: key.clone(),
                kind: "source",
                docset: "source",
                domain: row.domain.clone(),
                group: row.source_group.clone(),
                parent_key: None,
                text: Some(parent_text.clone()),
                spec: None,
                share_ppb: None,
                pool_row: Some(row_id.clone()),
            });
            key
        };
        let key = format!("m:{row_id}");
        let digest = item_digest(&seed, &key);
        let sents = textgen::sentences(&parent_text);
        let (first, last, percent) =
            textgen::select_span(sents.len(), &digest).ok_or(EvalError::State("parent span"))?;
        let (start, end) = (
            sents.get(first).ok_or(EvalError::State("span"))?.0,
            sents.get(last).ok_or(EvalError::State("span"))?.1,
        );
        let (avoid, avoid_voices, avoid_operations) = history(row_id)?;
        let (voice, operation) = textgen::rewrite_variant(&digest, &avoid_voices, &avoid_operations);
        variants.insert(row_id.clone(), (voice, operation));
        let spec = MixedSpec {
            first,
            last,
            start,
            end,
            percent,
            avoid,
            voice: Some(voice.to_owned()),
            operation: Some(operation.to_owned()),
        };
        docs.push(NewDoc {
            key,
            kind: "mixed",
            docset: "batch",
            domain: row.domain.clone(),
            group: row.source_group.clone(),
            parent_key: Some(parent_key),
            text: None,
            spec: Some(serde_json::to_string(&spec).map_err(|_| EvalError::State("spec"))?),
            share_ppb: None,
            pool_row: None,
        });
    }
    // Model-only documents follow the negatives' domain mix.
    let mut weights: BTreeMap<String, u64> = BTreeMap::new();
    for r in &d.negatives.rows {
        *weights.entry(r.domain.clone()).or_default() += 1;
    }
    for (domain, count) in pool::proportional(&weights, u64::from(n_s)) {
        for j in 0..count {
            let key = format!("s:{domain}:{j}");
            let dg = item_digest(&seed, &key);
            let spec = ScratchSpec { topic: textgen::topic(&domain, &dg).into(), words: textgen::scratch_words(&dg) };
            docs.push(NewDoc {
                group: format!("scratch:{key}"),
                key,
                kind: "model_only",
                docset: "batch",
                domain: domain.clone(),
                parent_key: None,
                text: None,
                spec: Some(serde_json::to_string(&spec).map_err(|_| EvalError::State("spec"))?),
                share_ppb: None,
                pool_row: None,
            });
        }
    }
    // Batch documents in a private rank; then sources, probes and audits.
    docs.sort_by_key(|x| {
        let set_order = match x.docset {
            "batch" => 0,
            "source" => 1,
            _ => 2,
        };
        (set_order, pool::rank(&seed, "order", &x.key))
    });
    for (version, pdocs) in &d.probe {
        for (pd, t) in pdocs {
            if sha_hex(t.as_bytes()) != pd.text_sha256 {
                return Err(EvalError::State("probe text digest"));
            }
            let kind = match pd.kind.as_str() {
                "human_only" => "human_only",
                "mixed" => "mixed",
                "model_only" => "model_only",
                _ => return Err(EvalError::State("probe kind")),
            };
            docs.push(NewDoc {
                key: format!("q:{version}:{}", pd.task),
                kind,
                docset: "probe",
                domain: pd.domain.clone(),
                group: format!("probe:{version}"),
                parent_key: None,
                text: Some(t.clone()),
                spec: None,
                share_ppb: Some(pd.share_ppb),
                pool_row: None,
            });
        }
    }
    for (set, rows) in [("audit_a", audit_a), ("audit_b", audit_b)] {
        for r in rows {
            docs.push(NewDoc {
                key: format!("{}:{}", if set == "audit_a" { "a" } else { "b" }, r.row_id),
                kind: "human_only",
                docset: set,
                domain: r.domain.clone(),
                group: r.source_group.clone(),
                parent_key: None,
                text: Some(get_text(&r.row_id)?),
                spec: None,
                share_ppb: Some(0),
                pool_row: Some(r.row_id.clone()),
            });
        }
    }
    let index: BTreeMap<String, usize> = docs.iter().enumerate().map(|(k, x)| (x.key.clone(), k)).collect();
    for (k, x) in docs.iter().enumerate() {
        let (status, sha) = match &x.text {
            Some(t) => ("ready", s(sha_hex(t.as_bytes()))),
            None => ("pending", SqlValue::Null),
        };
        let parent = match &x.parent_key {
            Some(p) => i(*index.get(p).ok_or(EvalError::State("parent index"))? as i64),
            None => SqlValue::Null,
        };
        sql.exec(
            "INSERT INTO docs (seq, idx, key, kind, domain, source_group, parent, status, text, text_sha256,
                share_ppb, spec, docset, pool_row) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                i(seq),
                i(k as i64),
                s(&x.key),
                s(x.kind),
                s(&x.domain),
                s(&x.group),
                parent,
                s(status),
                x.text.clone().map_or(SqlValue::Null, s),
                sha,
                x.share_ppb.map_or(SqlValue::Null, |v| i(i64::from(v))),
                x.spec.clone().map_or(SqlValue::Null, s),
                s(x.docset),
                x.pool_row.clone().map_or(SqlValue::Null, s),
            ],
        )?;
    }
    // Used rows and groups: negatives, audits and fresh parents. An exposed
    // row drawn again keeps its first record.
    let mut groups: BTreeSet<&str> = BTreeSet::new();
    let insert_used = if exposed {
        "INSERT OR IGNORE INTO pool_used (row_id, text_sha256, source_group, seq) VALUES (?, ?, ?, ?)"
    } else {
        "INSERT INTO pool_used (row_id, text_sha256, source_group, seq) VALUES (?, ?, ?, ?)"
    };
    for r in &consumed {
        sql.exec(insert_used, &[s(&r.row_id), s(&r.text_sha256), s(&r.source_group), i(seq)])?;
        groups.insert(r.source_group.as_str());
    }
    for g in groups {
        sql.exec("INSERT OR IGNORE INTO pool_groups_used (source_group, seq) VALUES (?, ?)", &[s(g), i(seq)])?;
    }
    // The reuse ledger: probe builds withdraw their passages for good. Every
    // negative, fresh parent and canary audit human of this cohort joins it
    // (an exposed row drawn again adds to its record), each parent use with
    // its voice and operation; the generator follows when the child is
    // accepted ([`record_child_generator`]). The gap counts drawn cohorts.
    let ordinal = if plan.is_probe_build() { None } else { Some(reuse_ordinal(sql, plan.cohort)?) };
    if let Some(ordinal) = ordinal {
        sql.exec("UPDATE cohorts SET reuse_ordinal = ? WHERE seq = ?", &[bind_u64(ordinal)?, i(seq)])?;
        let upsert = "INSERT INTO reuse_sources (row_id, text_sha256, source_group, domain, shard, first_cohort, uses,
                last_cohort, pool_id, first_ordinal, last_ordinal, voices, operations)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(row_id) DO UPDATE SET uses = uses + excluded.uses,
                last_cohort = COALESCE(excluded.last_cohort, last_cohort),
                last_ordinal = COALESCE(excluded.last_ordinal, last_ordinal),
                voices = CASE WHEN excluded.voices = '' THEN voices WHEN voices = '' THEN excluded.voices
                    ELSE voices || ',' || excluded.voices END,
                operations = CASE WHEN excluded.operations = '' THEN operations WHEN operations = ''
                    THEN excluded.operations ELSE operations || ',' || excluded.operations END";
        let rows = d.negatives.rows.iter().chain(parents.fresh_used.iter()).chain(audit_a).chain(audit_b);
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for r in rows {
            if !seen.insert(r.row_id.as_str()) {
                continue;
            }
            let variant = variants.get(&r.row_id);
            sql.exec(
                upsert,
                &[
                    s(&r.row_id),
                    s(&r.text_sha256),
                    s(&r.source_group),
                    s(&r.domain),
                    i(i64::from(r.shard)),
                    bind_u64(plan.cohort)?,
                    i(i64::from(variant.is_some())),
                    if variant.is_some() { bind_u64(plan.cohort)? } else { SqlValue::Null },
                    s(&plan.pool.pool_id),
                    bind_u64(ordinal)?,
                    if variant.is_some() { bind_u64(ordinal)? } else { SqlValue::Null },
                    s(variant.map_or("", |v| v.0)),
                    s(variant.map_or("", |v| v.1)),
                ],
            )?;
        }
        for e in &d.earlier {
            if let Some((voice, operation)) = variants.get(&e.row.row_id) {
                sql.exec(
                    "UPDATE reuse_sources SET uses = uses + 1, last_cohort = ?, last_ordinal = ?,
                        voices = CASE WHEN voices = '' THEN ? ELSE voices || ',' || ? END,
                        operations = CASE WHEN operations = '' THEN ? ELSE operations || ',' || ? END
                     WHERE row_id = ?",
                    &[
                        bind_u64(plan.cohort)?,
                        bind_u64(ordinal)?,
                        s(*voice),
                        s(*voice),
                        s(*operation),
                        s(*operation),
                        s(&e.row.row_id),
                    ],
                )?;
            }
        }
    }
    let by_origin = |o: &str| parents.children.iter().filter(|(_, x)| *x == o).count();
    let summary = json!({"rule": pool::DRAW_RULE, "reuse_rule": REUSE_RULE,
        "draw_mode": if exposed { DRAW_EXPOSED } else { DRAW_FRESH }, "reuse_ordinal": ordinal,
        "rewrite_prompts": textgen::REWRITE_PROMPT_SET,
        "reuse": {"max_uses": v.reuse.max_uses, "min_gap": v.reuse.min_gap},
        "humans": d.negatives.rows.len(), "audit_a": audit_a.len(), "audit_b": audit_b.len(),
        "mixed_parents": {"same_cohort": by_origin("same"), "earlier": by_origin("earlier"), "fresh": by_origin("fresh")},
        "fresh_parent_rows": parents.fresh_used.len(), "model_only": n_s,
        "probe_docs": d.probe.iter().map(|(_, x)| x.len()).sum::<usize>(),
        "available_before": d.negatives.available_rows,
        "ledger": {"rows": ledger.rows.len(), "groups": ledger.groups.len()}});
    sql.exec("UPDATE cohorts SET draw = ? WHERE seq = ?", &[s(summary.to_string()), i(seq)])?;
    set_phase(sql, seq, "generating", now_ms, "humans drawn (reward-v1)")?;
    crate::eval::lanes::plan_generation(sql, cfg, seq, now_ms)?;
    Ok(true)
}

/// Whether every kind has at least its planned count ready (spares beyond it
/// are dropped at the seal by private rank).
pub fn batch_complete(sql: &impl Sql, c: &Cohort) -> R<bool> {
    let have = batch_ready(sql, c.seq)?;
    Ok(have.iter().zip(c.plan.counts()).all(|(h, w)| *h >= w))
}

fn batch_ready(sql: &impl Sql, seq: i64) -> R<[u32; 3]> {
    let mut have = [0u32; 3];
    for r in sql.exec(
        "SELECT kind, COUNT(*) FROM docs WHERE seq = ? AND docset = 'batch' AND status = 'ready' GROUP BY kind",
        &[i(seq)],
    )? {
        let k = match text(&r, 0)? {
            "human_only" => 0,
            "mixed" => 1,
            _ => 2,
        };
        if let Some(h) = have.get_mut(k) {
            *h = u32::try_from(int(&r, 1)?).map_err(|_| StoreError("count"))?;
        }
    }
    Ok(have)
}

/// Whether generation can no longer fill a kind: its ready and pending items
/// together are fewer than its planned count.
pub fn batch_infeasible(sql: &impl Sql, c: &Cohort) -> R<bool> {
    let mut live = [0u32; 3];
    for r in sql.exec(
        "SELECT kind, COUNT(*) FROM docs WHERE seq = ? AND docset = 'batch' AND status IN ('ready','pending')
         GROUP BY kind",
        &[i(c.seq)],
    )? {
        let k = match text(&r, 0)? {
            "human_only" => 0,
            "mixed" => 1,
            _ => 2,
        };
        if let Some(h) = live.get_mut(k) {
            *h = u32::try_from(int(&r, 1)?).map_err(|_| StoreError("count"))?;
        }
    }
    Ok(live.iter().zip(c.plan.counts()).any(|(h, w)| *h < w))
}

// ---------------------------------------------------------------------- seal

/// What the Worker read before the seal: each scored artifact's digests.
#[derive(Clone, Debug)]
pub struct SealInputs {
    pub reference: ArtifactDigests,
    pub predecessor: Option<ArtifactDigests>,
    pub entrants: Vec<ArtifactDigests>,
}

#[derive(Clone, Debug)]
pub struct SealBuild {
    /// `None` for a probe build.
    pub manifest: Option<CohortManifestV2>,
    pub manifest_digest: Option<Digest>,
    pub shards: Vec<(String, Vec<u8>)>,
    pub index_key: String,
    pub index: Vec<u8>,
    /// `(doc idx, task)`; docs not listed are spares (never tasks).
    pub task_map: Vec<(i64, u32)>,
    pub spares: Vec<i64>,
    pub state: V3State,
    /// The published index at seal, encoded, for the receipts bucket.
    pub published: Option<(Vec<u8>, Vec<u8>)>,
    /// A probe build's private record and its key.
    pub probe_record: Option<(String, Vec<u8>)>,
}

/// Where a probe version's objects live: under the authority's probe prefix
/// (`config::Authority::probe_prefix`), so a restarted authority's probe
/// version 1 never overwrites an earlier authority's.
pub fn probe_prefix(cfg: &EvalConfig, version: u32) -> String {
    cfg.authority.probe_prefix(version)
}

pub fn shard_prefix(cfg: &EvalConfig, c: &Cohort) -> R<String> {
    if c.plan.is_probe_build() {
        let v = v1(&c.plan)?.probe_version.ok_or(EvalError::Plan("probe_version"))?;
        Ok(probe_prefix(cfg, v))
    } else {
        Ok(engine::batch_prefix(&c.label))
    }
}

/// The published index as this authority holds it.
/// Each fingerprint's private vector key, by (content, probe version).
pub type VectorKeys = BTreeMap<(Digest, u32), String>;

pub fn published_index(sql: &impl Sql) -> R<(PublishedIndex, VectorKeys)> {
    let contents = sql
        .exec("SELECT content FROM published_contents", &[])?
        .iter()
        .map(|r| text(r, 0).map(|t| parse_digest(t).ok_or(StoreError("content"))).and_then(|x| x))
        .collect::<Result<Vec<_>, _>>()?;
    let mut keys = BTreeMap::new();
    let mut fps = Vec::new();
    for r in sql.exec("SELECT content, version, fingerprint, vector_key FROM fingerprints", &[])? {
        let content = parse_digest(text(&r, 0)?).ok_or(StoreError("content"))?;
        let version = u32::try_from(int(&r, 1)?).map_err(|_| StoreError("version"))?;
        let fingerprint = parse_digest(text(&r, 2)?).ok_or(StoreError("fingerprint"))?;
        keys.insert((content, version), text(&r, 3)?.to_owned());
        fps.push(Fingerprint { content, version, fingerprint });
    }
    let index = PublishedIndex::new(contents, fps).map_err(|_| EvalError::State("published index"))?;
    Ok((index, keys))
}

fn identity_of(a: &ArtifactJson) -> R<NeuronIdentity> {
    a.owner.identity().map_err(|e| EvalError::Plan(e.0))
}

/// The private nonce of an entrant's testnet opening (operator roster).
pub fn opening_nonce(private: &[u8; 32], label: &str, hotkey: &AccountId) -> Nonce {
    let mut payload = private.to_vec();
    payload.extend_from_slice(label.as_bytes());
    payload.push(0);
    payload.extend_from_slice(&hotkey.0);
    Nonce(domain_hash(OPENING_NONCE_DOMAIN, &payload).0)
}

/// An entrant's opening nonce: the miner's own for an admitted entrant (set
/// at the roster freeze), else the private derived one (operator roster).
pub fn entrant_nonce(cfg: &EvalConfig, c: &Cohort, a: &ArtifactJson) -> R<Nonce> {
    match &a.opening_nonce {
        Some(n) => crate::eval::plan::parse_hex32(n).map(Nonce).ok_or(EvalError::State("opening nonce")),
        None => Ok(opening_nonce(&cfg.private_seed, &c.label, &identity_of(a)?.hotkey)),
    }
}

pub fn opening(cfg: &EvalConfig, c: &Cohort, a: &ArtifactJson, content: Digest) -> R<SubmissionOpening> {
    let id = identity_of(a)?;
    Ok(SubmissionOpening {
        chain: cfg.chain.clone(),
        cohort: c.plan.cohort,
        hotkey: id.hotkey,
        registration_block: id.registration_block,
        content,
        nonce: entrant_nonce(cfg, c, a)?,
    })
}

/// The seal's refusal of a plan whose references are not the chain's.
pub const REFERENCE_CHAIN_MISMATCH: &str = "plan references differ from the reference chain";

/// The references the plan names, checked against the chain the closed
/// cohorts left (`reference_state`); the first v3 cohort fixes `k` from the
/// plan (`R_0` when it names no predecessor).
fn reference_set(sql: &impl Sql, inputs: &SealInputs) -> R<(ReferenceSet, u64)> {
    let current = ReferenceModel { content: inputs.reference.content.0, artifact: inputs.reference.artifact.0 };
    let predecessor =
        inputs.predecessor.map(|p| ReferenceModel { content: p.content.0, artifact: p.artifact.0 });
    let rows = sql.exec("SELECT k, current, predecessor FROM reference_state WHERE id = 1", &[])?;
    let k = match rows.first() {
        Some(r) => {
            let k = uint(r, 0)?;
            let want_current = text(r, 1)?;
            let want_pred = opt_text(r, 2);
            if want_current != current.content.to_string()
                || want_pred != predecessor.map(|p| p.content.to_string())
            {
                return Err(EvalError::State(REFERENCE_CHAIN_MISMATCH));
            }
            k
        }
        None => u64::from(predecessor.is_some()),
    };
    let set = ReferenceSet { index: k, current, predecessor };
    set.validate().map_err(|_| EvalError::State("reference set"))?;
    Ok((set, k))
}

/// The policy the manifest binds by digest: the gates, the canary, and the
/// draw policy (the reuse limit and gap with their unit, the rewrite prompt
/// set and the reuse rule), so the reuse values are frozen in the signed
/// manifest, not only in the private plan.
fn gate_policy_digest(v: &RewardV1Plan) -> R<Digest> {
    let j = gate_policy_json(v);
    Ok(domain_hash("slop_ninja.eval/gate-policy/v2", j.to_string().as_bytes()))
}

pub fn gate_policy_json(v: &RewardV1Plan) -> Value {
    let mut j = json!({"schema": "slop_ninja_eval_gate_policy_v2", "gates": v.gates, "canary": v.canary,
        "canary_rule": CANARY_RULE, "fpr_rule": FPR_RULE,
        "reuse": {"max_uses": v.reuse.max_uses, "min_gap": v.reuse.min_gap, "gap_unit": "drawn_reward_v1_cohorts",
            "rule": REUSE_RULE, "rewrite_prompts": textgen::REWRITE_PROMPT_SET}});
    // The incentive rule and the provisional flag join the policy the
    // manifest binds, for plans that declare them (earlier plans' policy
    // documents, and so their digests, are unchanged).
    if let (Some(i), Some(o)) = (&v.incentive, j.as_object_mut()) {
        o.insert("incentive".into(), json!({"rule": i.rule, "provisional_gates": i.provisional_gates}));
    }
    j
}

/// Whether a cohort may draw fresh stock: its roster froze with an admitted
/// outside entrant, and its pool is the admission block's fresh pool.
pub fn fresh_allowed(sql: &impl Sql, c: &Cohort) -> R<bool> {
    let Some(a) = &c.plan.admission else { return Ok(false) };
    Ok(a.fresh_pool.as_ref() == Some(&c.plan.pool) && crate::eval::admission::has_outside_entrant(sql, c.seq)?)
}

/// The domain the v1 limits record (a plan that names no artifact spec) is
/// hashed under; the manifest binds the digest.
pub const LIMITS_DOMAIN_V1: &str = "slop_ninja.eval/limits/v1";
/// The v2 limits record's domain: v1's fields plus the artifact spec every
/// scoring load names, by id and by the digest the plan froze.
pub const LIMITS_DOMAIN_V2: &str = "slop_ninja.eval/limits/v2";

/// The limits record the manifest binds by digest, with its domain. The v2
/// record belongs to plans that carry `fanout.score.artifact_spec_sha256`
/// (the field this record version came with): the same limits (the step's
/// wall budget as `step_wall_ms`, so the public copy carries no spend-like
/// key) and the spec every scoring load names, by id and the digest the plan
/// froze. Every other plan keeps the v1 record, so a stored plan's record
/// and digest never change with the build: a plan that names its spec only
/// in the admission block (written before that field) bound v1 at seal and
/// still binds v1 at package time. The v2 record is published in
/// `components.json` (`limits`), so the bound digest can be checked against
/// public material ([`published_limits`]).
pub fn limits_record(plan: &CohortPlan) -> R<(Value, &'static str)> {
    let f = plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    let l = f.score.limits();
    match (&f.score.artifact_spec, f.score.artifact_spec_sha256.as_deref()) {
        (Some(id), Some(sha)) => Ok((
            json!({"schema": "slop_ninja_eval_limits_v2", "max_attempts": plan.max_attempts,
                "chunk_docs": plan.scorer.chunk_docs, "step_wall_ms": l.step_budget_ms, "doc_timeout_ms": l.doc_timeout_ms,
                "artifact_max_bytes": l.artifact_max_bytes, "load_wait_ms": l.load_wait_ms,
                "artifact_failures_before_model_error": l.artifact_failures_before_model_error,
                "crash_rule": engine::CRASH_RULE, "artifact_rule": engine::ARTIFACT_RULE,
                "artifact_spec": {"id": id, "sha256": sha}}),
            LIMITS_DOMAIN_V2,
        )),
        _ => Ok((
            json!({"schema": "slop_ninja_eval_limits_v1", "max_attempts": plan.max_attempts,
                "chunk_docs": plan.scorer.chunk_docs, "step_budget_ms": l.step_budget_ms, "doc_timeout_ms": l.doc_timeout_ms,
                "artifact_max_bytes": l.artifact_max_bytes, "load_wait_ms": l.load_wait_ms,
                "artifact_failures_before_model_error": l.artifact_failures_before_model_error,
                "crash_rule": engine::CRASH_RULE, "artifact_rule": engine::ARTIFACT_RULE}),
            LIMITS_DOMAIN_V1,
        )),
    }
}

/// The limits record `components.json` publishes: the v2 record (a plan that
/// names an artifact spec); `None` for a v1 record, whose components are as
/// they were.
pub fn published_limits(plan: &CohortPlan) -> R<Option<Value>> {
    let (j, domain) = limits_record(plan)?;
    Ok((domain == LIMITS_DOMAIN_V2).then_some(j))
}

fn limits_digest(plan: &CohortPlan) -> R<Digest> {
    let (j, domain) = limits_record(plan)?;
    Ok(domain_hash(domain, j.to_string().as_bytes()))
}

fn origin_of(kind: &str) -> R<Origin> {
    match kind {
        "human_only" => Ok(Origin::HumanOnly),
        "mixed" => Ok(Origin::Mixed),
        "model_only" => Ok(Origin::ModelOnly),
        _ => Err(EvalError::State("document kind")),
    }
}

/// A seal refusal: a sealed probe, audit or human document the scorer would
/// refuse for every artifact. A backstop: under the seeded order the draw
/// already holds every such pool row out ([`screen_pool`]), so this fires
/// only for a defect in a probe record or a document changed after the draw.
/// Deterministic ([`engine::step_error_is_deterministic`]): the cohort
/// pauses as `step_refused` at once, and a clear re-runs the same refusal,
/// because the document is fixed by the cohort's draw or its plan's probe
/// record.
pub const UNSCORABLE_DOCUMENT: &str = "unscorable sealed document";

/// A draw refusal under the seeded order: the plan's pool records no token
/// bound that keeps every row inside the scorer's limit
/// ([`pool_token_bound_ok`]; pool v1 records none). Deterministic: the
/// cohort pauses before anything is drawn, written or exposed.
pub const POOL_TOKEN_BOUND_REFUSED: &str = "pool token bound";

/// The input screen the seeded order applies (finding L9), named in the
/// batch index:
/// - **Pool rows** (batch humans, audit draws, mixed parents): the draw
///   refuses a pool whose export records no per-row token bound within the
///   scorer's limit ([`pool_token_bound_ok`]), and holds out of every role
///   each row whose text the scorer refuses as empty ([`screen_pool`]),
///   with its source group; the held rows are listed in the cohort's ledger
///   entry.
/// - **Generated documents**: one the scorer refuses as empty gives way to
///   a spare at seal.
/// - **Seal backstop**: any other sealed document the scorer refuses as
///   empty stops the seal ([`UNSCORABLE_DOCUMENT`]).
///
/// Not screened: the token count of a generated document (a mixed child or
/// a model-only text). The coordinator holds no copy of the pinned
/// tokenizer, and the generation check bounds length in words. A generated
/// batch document over the limit is found only at packaging, as a terminal
/// `reference_failure`. An audit human the scorer refuses as over-length
/// cannot be drawn from a bounded pool; should one be sealed anyway, the
/// strict false-positive count leaves it out ([`over_length_tasks`]).
pub const INPUT_SCREEN_RULE: &str = "pool_token_bound_and_empty_text_v1";

/// Whether the scorer refuses `text` as empty for every artifact (the
/// scorer's own rule, `artifact-check` `text::scorer_is_empty`).
pub fn scorer_refuses(text: &str) -> bool {
    slop_ninja_artifact_check::text::scorer_is_empty(text)
}

/// Whether a pool's recorded token bound, which counts no special tokens,
/// keeps every row inside the scorer's limit once `[CLS]` and `[SEP]` are
/// added.
pub fn pool_token_bound_ok(m: &pool::PoolManifest) -> bool {
    use slop_ninja_artifact_check::text::MAX_TOKENS_INCLUDING_SPECIALS;
    m.token_bound().is_some_and(|b| b.saturating_add(2) <= MAX_TOKENS_INCLUDING_SPECIALS)
}

/// The draw's screen over pool rows `(row id, text)`: the ids of the rows
/// the scorer refuses for every artifact. The draw treats them as drawn
/// before any role draws, so no role takes them or their source groups.
pub fn screen_pool<'a>(rows: impl IntoIterator<Item = (&'a str, &'a str)>) -> BTreeSet<String> {
    rows.into_iter().filter(|(_, t)| scorer_refuses(t)).map(|(id, _)| id.to_owned()).collect()
}

/// The tasks of cohort `seq` whose final outcome, for any job, is the
/// scorer's over-length refusal (a parse failure with a token count above
/// the limit). The count comes from the pinned tokenizer every admitted
/// artifact ships, before any model runs, so the refusal holds for every
/// artifact alike and says nothing about any of them.
pub fn over_length_tasks(sql: &impl Sql, seq: i64) -> R<BTreeSet<u32>> {
    use slop_ninja_artifact_check::text::MAX_TOKENS_INCLUDING_SPECIALS;
    let limit = i64::try_from(MAX_TOKENS_INCLUDING_SPECIALS).map_err(|_| EvalError::State("token limit"))?;
    sql.exec(
        "SELECT DISTINCT task FROM preds WHERE seq = ? AND outcome = 4 AND token_count > ? ORDER BY task",
        &[i(seq), i(limit)],
    )?
    .iter()
    .map(|r| u32::try_from(int(r, 0)?).map_err(|_| EvalError::State("task")))
    .collect()
}

/// The seeded task order: `docs` (each with its document key, unique within
/// the cohort) sorted by `pool::rank(seed, "task", key)`. The rank depends on
/// the key alone, never on the selection rank, the kind or the set.
pub fn seeded_task_order<T>(seed: &[u8; 32], docs: Vec<(&str, T)>) -> Vec<T> {
    let mut keyed: Vec<([u8; 32], T)> = docs.into_iter().map(|(k, d)| (pool::rank(seed, "task", k), d)).collect();
    keyed.sort_by_key(|a| a.0);
    keyed.into_iter().map(|(_, d)| d).collect()
}

/// Builds the sealed task space, the private batch index, the shards and, for
/// a cohort, the reward-v1 manifest.
pub fn prepare_seal(sql: &impl Sql, cfg: &EvalConfig, seq: i64, inputs: &SealInputs) -> R<SealBuild> {
    let c = cohort(sql, seq)?;
    if c.phase != "sealing" {
        return Err(EvalError::State("seal outside sealing"));
    }
    let plan = &c.plan;
    let v = v1(plan)?;
    let want = plan.counts();
    let rows = sql.exec(
        "SELECT idx, kind, domain, source_group, text, text_sha256, share_ppb, span_start, span_end, generator,
                words_total, words_ai, parent, docset, key
         FROM docs WHERE seq = ? AND status = 'ready' AND docset IN ('batch','probe','audit_a','audit_b')
         ORDER BY CASE docset WHEN 'batch' THEN 0 WHEN 'probe' THEN 1 WHEN 'audit_a' THEN 2 ELSE 3 END, idx",
        &[i(seq)],
    )?;
    let seeded = v.seeded_task_order();
    let seed = cohort_seed(&cfg.private_seed, &c.label);
    // Exactly the planned count of each kind, by private rank (idx order).
    let mut kept = [0u32; 3];
    let mut spares = Vec::new();
    let mut ordered = Vec::new();
    for r in &rows {
        let set = text(r, 13)?;
        // The seeded order seals no document the scorer refuses for every
        // artifact as empty (finding L9). A generated one gives way to a
        // spare. The draw already held out every such pool row, so any other
        // is a defect in a probe record (or a document changed after the
        // draw) and stops the seal before anything is exposed.
        if seeded && scorer_refuses(text(r, 4)?) {
            if set == "batch" && text(r, 1)? != "human_only" {
                spares.push(int(r, 0)?);
                continue;
            }
            return Err(EvalError::State(UNSCORABLE_DOCUMENT));
        }
        if set == "batch" {
            let k = match text(r, 1)? {
                "human_only" => 0,
                "mixed" => 1,
                _ => 2,
            };
            let (have, cap) = (kept.get(k).copied().unwrap_or(0), want.get(k).copied().unwrap_or(0));
            if have >= cap {
                spares.push(int(r, 0)?);
                continue;
            }
            if let Some(x) = kept.get_mut(k) {
                *x += 1;
            }
        }
        ordered.push(r);
    }
    if kept != want {
        return Err(EvalError::State("batch shortfall"));
    }
    // Finding L4: the selection rank fills the generated kinds' quotas first
    // (only they have spares), so its tail is mostly human, and the probe and
    // audit draws were appended after the batch. Under the seeded order every
    // sealed document takes its task index from a second private rank, keyed
    // by the document alone, so neither kind nor set follows from position.
    if seeded {
        let keyed = ordered.iter().map(|r| Ok((text(r, 14)?, *r))).collect::<R<Vec<_>>>()?;
        ordered = seeded_task_order(&seed, keyed);
    }
    let n = want.iter().sum::<u32>();
    let mut task_map = Vec::new();
    let mut entries = Vec::new();
    let mut lines = Vec::new();
    let mut strata: BTreeMap<(String, Origin), u32> = BTreeMap::new();
    let mut groups: BTreeSet<String> = BTreeSet::new();
    let mut probe_counts: BTreeMap<u32, u32> = BTreeMap::new();
    let (mut audit_a, mut audit_b) = (Vec::new(), Vec::new());
    for (task, r) in ordered.iter().enumerate() {
        let idx = int(r, 0)?;
        let task = u32::try_from(task).map_err(|_| EvalError::State("task"))?;
        task_map.push((idx, task));
        let set = text(r, 13)?.to_owned();
        let kind = text(r, 1)?.to_owned();
        if set == "batch" {
            *strata.entry((text(r, 2)?.to_owned(), origin_of(&kind)?)).or_default() += 1;
            groups.insert(text(r, 3)?.to_owned());
        } else if set == "probe" {
            let version = text(r, 14)?
                .split(':')
                .nth(1)
                .and_then(|x| x.parse::<u32>().ok())
                .ok_or(EvalError::State("probe key"))?;
            *probe_counts.entry(version).or_default() += 1;
        } else if set == "audit_a" {
            audit_a.push((idx, text(r, 5)?.to_owned()));
        } else {
            audit_b.push((idx, text(r, 5)?.to_owned()));
        }
        let meta = json!({"task": task, "set": set, "kind": kind, "domain": text(r, 2)?,
            "source_group_sha256": sha_hex(text(r, 3)?.as_bytes()), "text_sha256": text(r, 5)?,
            "share_ppb": opt_int(r, 6), "ai_byte_span": [opt_int(r, 7), opt_int(r, 8)],
            "generator": opt_text(r, 9), "words": opt_int(r, 10), "ai_words": opt_int(r, 11),
            "policy": match kind.as_str() {
                "mixed" => textgen::SPLICE_POLICY, "model_only" => textgen::SCRATCH_POLICY, _ => "human" }});
        let mut line = meta.clone();
        if let Some(o) = line.as_object_mut() {
            o.insert("text".into(), json!(text(r, 4)?));
        }
        lines.push(line.to_string());
        entries.push(meta);
    }
    let prefix = shard_prefix(cfg, &c)?;
    let mut shards = Vec::new();
    let mut shard_index = Vec::new();
    for (k, chunk) in lines.chunks(engine::BATCH_SHARD_DOCS).enumerate() {
        let body: String = chunk.iter().map(|l| format!("{l}\n")).collect();
        let key = format!("{prefix}/shard-{k:04}.jsonl");
        shard_index.push(json!({"key": key, "docs": chunk.len(), "sha256": sha_hex(body.as_bytes())}));
        shards.push((key, body.into_bytes()));
    }
    // A draw's digest lists its documents in draw order (idx order), which is
    // their task order in an appended seal, so it names the draw, not where
    // its documents sit.
    audit_a.sort_unstable();
    audit_b.sort_unstable();
    let a_refs: Vec<&str> = audit_a.iter().map(|(_, sha)| sha.as_str()).collect();
    let b_refs: Vec<&str> = audit_b.iter().map(|(_, sha)| sha.as_str()).collect();
    let draws = (audit_draw_digest(&seed, "a", &a_refs), audit_draw_digest(&seed, "b", &b_refs));
    let mut index_json = json!({
        "schema": "slop_ninja_eval_batch_index_v2",
        "cohort": plan.cohort, "label": c.label, "plan_sha256": c.plan_sha256,
        "word_policy": textgen::WORD_POLICY, "prompt_version": textgen::PROMPT_VERSION,
        "temperature": textgen::TEMPERATURE, "top_p": textgen::TOP_P,
        "rewrite_prompts": textgen::REWRITE_PROMPT_SET,
        "batch_tasks": n, "probe_tasks": probe_counts, "audit_tasks": [audit_a.len(), audit_b.len()],
        "tasks": entries.len(), "spares": spares.len(), "shards": shard_index, "documents": entries,
    });
    // The seeded order's index (v3) names its rule; the manifest binds the
    // index by digest, so the rule is committed with the seal. Every earlier
    // seal's index is unchanged.
    if let (true, Some(o)) = (seeded, index_json.as_object_mut()) {
        o.insert("schema".into(), json!("slop_ninja_eval_batch_index_v3"));
        o.insert("task_order".into(), json!(super::plan::TASK_ORDER_SEEDED_RANK));
        o.insert("input_screen".into(), json!(INPUT_SCREEN_RULE));
    }
    let index = serde_json::to_vec_pretty(&index_json).map_err(|_| EvalError::State("batch index"))?;
    let mut st = state(sql, seq)?;
    st.reference = Some(inputs.reference);
    st.predecessor = inputs.predecessor;
    st.entrants = inputs.entrants.clone();
    st.batch_tasks = n;
    st.probe_tasks = probe_counts.iter().map(|(v, c)| (*v, *c)).collect();
    st.audit_tasks = u32::try_from(audit_a.len()).map_err(|_| EvalError::State("audit"))?;
    st.audit_b_tasks = Some(u32::try_from(audit_b.len()).map_err(|_| EvalError::State("audit"))?);
    st.audit_draws = Some((H(draws.0), H(draws.1)));
    if plan.is_probe_build() {
        let version = v.probe_version.ok_or(EvalError::Plan("probe_version"))?;
        let nonce = hex::encode(domain_hash("slop_ninja.eval/probe-nonce/v1", &seed).0);
        let record = ProbeRecord {
            schema: PROBE_RECORD_SCHEMA.into(),
            version,
            label: c.label.clone(),
            size: n,
            nonce,
            docs: ordered
                .iter()
                .enumerate()
                .map(|(task, r)| {
                    Ok(ProbeDoc {
                        task: u32::try_from(task).map_err(|_| EvalError::State("task"))?,
                        kind: text(r, 1)?.to_owned(),
                        domain: text(r, 2)?.to_owned(),
                        share_ppb: opt_int(r, 6).and_then(|x| u32::try_from(x).ok()).unwrap_or(0),
                        text_sha256: text(r, 5)?.to_owned(),
                    })
                })
                .collect::<R<_>>()?,
            shards: shards.iter().map(|(k, b)| (k.clone(), sha_hex(b))).collect(),
        };
        let bytes = serde_json::to_vec_pretty(&record).map_err(|_| EvalError::State("probe record"))?;
        st.reference_index = u64::from(inputs.predecessor.is_some());
        // A probe build has no manifest: its attempts and receipts bind the
        // probe record's digest instead.
        let record_digest = ProbeRecord::digest(&bytes);
        return Ok(SealBuild {
            manifest: None,
            manifest_digest: Some(record_digest),
            shards,
            index_key: format!("{prefix}/index.json"),
            index,
            task_map,
            spares,
            state: st,
            published: None,
            probe_record: Some((format!("{prefix}/record.json"), bytes)),
        });
    }
    // The cohort manifest.
    let (references, k) = reference_set(sql, inputs)?;
    st.reference_index = k;
    let (published, _) = published_index(sql)?;
    for r in std::iter::once(&references.current).chain(references.predecessor.iter()) {
        if !published.contains(&r.content) {
            return Err(EvalError::State("reference not published"));
        }
    }
    let mut probes = Vec::new();
    for p in &v.probes {
        let row = sql.exec(
            "SELECT record_sha256, size, digest FROM probe_versions WHERE version = ?",
            &[i(i64::from(p.version))],
        )?;
        let r = row.first().ok_or(EvalError::State("probe version unknown"))?;
        if text(r, 0)? != p.sha256 {
            return Err(EvalError::State("probe record digest"));
        }
        let size = u32::try_from(int(r, 1)?).map_err(|_| EvalError::State("probe size"))?;
        if probe_counts.get(&p.version).copied() != Some(size) {
            return Err(EvalError::State("probe documents"));
        }
        probes.push(ProbeBinding {
            version: p.version,
            size,
            digest: parse_digest(text(r, 2)?).ok_or(EvalError::State("probe digest"))?,
        });
    }
    let mut roster = Vec::new();
    let mut unexecuted = Vec::new();
    for (pos, (a, dg)) in plan.entrants.iter().zip(&inputs.entrants).enumerate() {
        let op = opening(cfg, &c, a, dg.content.0)?;
        roster.push(RosterEntry {
            identity: identity_of(a)?,
            commitment: op.commitment(),
            commitment_block: a.commitment_block,
            // An admitted entrant's position is its commitment's; an
            // operator-roster entrant's is its roster position.
            extrinsic_index: match a.extrinsic_index {
                Some(x) => x,
                None => u32::try_from(pos).map_err(|_| EvalError::State("position"))?,
            },
        });
        if published.contains(&dg.content.0) {
            unexecuted.push(pos);
        }
    }
    st.unexecuted = unexecuted;
    let mix = OriginMix {
        human_only: u8::try_from(plan.mix.human_only).map_err(|_| EvalError::Plan("mix"))?,
        mixed: u8::try_from(plan.mix.mixed).map_err(|_| EvalError::Plan("mix"))?,
        model_only: u8::try_from(plan.mix.model_only).map_err(|_| EvalError::Plan("mix"))?,
    };
    let manifest = CohortManifestV2 {
        chain: cfg.chain.clone(),
        cohort: plan.cohort,
        cohort_label: c.label.clone(),
        opened_epoch: c.opened_epoch.ok_or(EvalError::State("opened epoch"))?,
        reward_rule: RewardRule::RewardV1,
        schedule: CohortSchedule {
            tempos: u16::try_from(plan.k_tempos).map_err(|_| EvalError::Plan("k_tempos"))?,
            admission_min: u16::try_from(plan.admission_tempos).map_err(|_| EvalError::Plan("admission"))?,
            transition_min: u16::try_from(plan.transition_tempos).map_err(|_| EvalError::Plan("transition"))?,
            lease_tempos: v.lease_tempos,
        },
        batch: BatchDesign {
            cases: n,
            source_groups: u32::try_from(groups.len()).map_err(|_| EvalError::State("groups"))?,
            mix,
            strata: strata
                .into_iter()
                .map(|((domain, origin), cases)| Stratum { domain, origin, cases })
                .collect(),
            batch: Digest(sha2_digest(&index)),
        },
        params: RewardV1Params::new(v.sigma),
        references,
        published: published.binding().map_err(|_| EvalError::State("published binding"))?,
        probes,
        input_profile: domain_hash("slop_ninja.eval/input-profile/v1", plan.reference.contract.as_bytes()),
        output_contract: v.output_contract.clone(),
        reference_gallery: domain_hash("slop_ninja.eval/reference-gallery/v1", b"testnet: no gallery"),
        gate_policy: gate_policy_digest(v)?,
        span_gate: false,
        limits: limits_digest(plan)?,
        max_attempts: plan.max_attempts,
        // The cohort's own cap when it admits its roster (`ROSTER_CAP` bounds
        // it at validation), else the deployment's.
        roster_cap: match &plan.admission {
            Some(a) => a.roster_cap,
            None => cfg.ceilings.roster_cap.map_or(Ok(MAX_ROSTER), u16::try_from).map_err(|_| EvalError::State("roster cap"))?,
        },
        roster,
    };
    manifest.validate().map_err(|e| {
        let _ = e;
        EvalError::State("cohort manifest")
    })?;
    let manifest_digest = manifest.digest().map_err(|_| EvalError::State("manifest digest"))?;
    let enc_contents = published.contents.to_bytes().map_err(|_| EvalError::State("published contents"))?;
    let enc_fps = published.fingerprints.to_bytes().map_err(|_| EvalError::State("fingerprint index"))?;
    st.published_at_seal = Some((hex::encode(&enc_contents), hex::encode(&enc_fps)));
    st.manifest_hex = Some(hex::encode(manifest.to_bytes().map_err(|_| EvalError::State("manifest bytes"))?));
    Ok(SealBuild {
        manifest: Some(manifest),
        manifest_digest: Some(manifest_digest),
        shards,
        index_key: format!("{prefix}/index.json"),
        index,
        task_map,
        spares,
        state: st,
        published: Some((enc_contents, enc_fps)),
        probe_record: None,
    })
}

/// Commits the seal: tasks, spares, digests, jobs and chunks.
pub fn apply_seal(sql: &impl Sql, seq: i64, b: &SealBuild, puts: u64, now_ms: u64) -> R<()> {
    let c = cohort(sql, seq)?;
    if c.phase != "sealing" {
        return Err(EvalError::State("seal outside sealing"));
    }
    for (idx, task) in &b.task_map {
        sql.exec("UPDATE docs SET task = ? WHERE seq = ? AND idx = ?", &[i(i64::from(*task)), i(seq), i(*idx)])?;
    }
    for idx in &b.spares {
        sql.exec("UPDATE docs SET docset = 'spare' WHERE seq = ? AND idx = ?", &[i(seq), i(*idx)])?;
    }
    let total = b.task_map.len() as i64;
    let n = i64::from(b.state.batch_tasks);
    sql.exec(
        "UPDATE cohorts SET manifest_digest = ?, evaluation_set = ?, batch_index_sha256 = ?, task_count = ?,
            r2_puts = r2_puts + ? WHERE seq = ?",
        &[
            b.manifest_digest.map_or(SqlValue::Null, |d| s(d.to_string())),
            s(sha_hex(&b.index)),
            s(sha_hex(&b.index)),
            i(total),
            bind_u64(puts)?,
            i(seq),
        ],
    )?;
    save_state(sql, seq, &b.state)?;
    let per = i64::from(c.plan.scorer.chunk_docs.max(1));
    let batch_tasks = sql
        .exec("SELECT task FROM docs WHERE seq = ? AND docset = 'batch' AND task IS NOT NULL ORDER BY task", &[i(seq)])?
        .iter()
        .map(|r| Ok(int(r, 0)?))
        .collect::<R<Vec<i64>>>()?;
    if batch_tasks.len() as i64 != n {
        return Err(EvalError::State("batch task"));
    }
    let mut roster: Vec<(i64, &str, &ArtifactJson, i64)> = vec![(0, "reference", &c.plan.reference, n)];
    let mut job = 1;
    if let Some(p) = &c.plan.previous_reference {
        roster.push((job, "previous_reference", p, n));
        job += 1;
    }
    for (pos, e) in c.plan.entrants.iter().enumerate() {
        if !b.state.unexecuted.contains(&pos) {
            roster.push((job, "entrant", e, total));
        }
        job += 1;
    }
    for (job, role, a, tasks) in &roster {
        let id = a.id().map_err(|e| EvalError::Plan(e.0))?;
        let chunks = (tasks + per - 1) / per;
        sql.exec(
            "INSERT INTO jobs (seq, job, role, artifact, package_sha256, manifest_key, owner, status, chunks, tasks)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)",
            &[
                i(seq),
                i(*job),
                s(*role),
                s(id.to_string()),
                s(&a.package_sha256),
                s(&a.manifest_key),
                s(serde_json::to_string(&a.owner).map_err(|_| EvalError::State("owner"))?),
                i(chunks),
                i(*tasks),
            ],
        )?;
        // A window holds `per` of the job's tasks from `first_task`, a task
        // index (`lanes::unresolved`): an entrant's tasks are every task, a
        // reference's the batch tasks, so its window starts at its first
        // batch task. Where the batch is tasks `[0, n)` (every appended seal)
        // both are `k * per`.
        for k in 0..chunks {
            let first = if *role == "entrant" {
                k * per
            } else {
                *usize::try_from(k * per).ok().and_then(|x| batch_tasks.get(x)).ok_or(EvalError::State("batch task"))?
            };
            sql.exec(
                "INSERT INTO chunks (seq, job, chunk, first_task, count, status) VALUES (?, ?, ?, ?, ?, 'pending')",
                &[i(seq), i(*job), i(k), i(first), i(per.min(tasks - k * per))],
            )?;
        }
    }
    for (k, (key, body)) in b.shards.iter().enumerate() {
        let docs = body.iter().filter(|x| **x == b'\n').count() as i64;
        sql.exec(
            "INSERT INTO batch_shards (seq, shard, key, sha256, first_task, docs) VALUES (?, ?, ?, ?, ?, ?)",
            &[
                i(seq),
                i(k as i64),
                s(key),
                s(sha_hex(body)),
                i((k * engine::BATCH_SHARD_DOCS) as i64),
                i(docs),
            ],
        )?;
    }
    crate::eval::lanes::fit_score_ranges_per_job(sql, seq)?;
    sql.exec("UPDATE cohorts SET scoring_started_ms = ? WHERE seq = ?", &[bind_u64(now_ms)?, i(seq)])?;
    set_phase(sql, seq, "scoring", now_ms, "batch, probe, audit draws and roster sealed (reward-v1)")?;
    Ok(())
}

// ------------------------------------------------------------------- package

/// One job's predictions over its tasks.
#[derive(Clone, Debug, Default)]
pub struct JobVectors {
    pub involvement: Vec<Option<Probability>>,
    pub share: Vec<Option<u32>>,
    pub roles: Vec<bool>,
}

impl JobVectors {
    /// Reads the vectors of a job that scored every task into `layout`
    /// (slot -> task, [`canonical_layout`]) in place: slot `s` takes task
    /// `layout[s]`. Follows each cycle of the permutation with swaps, so the
    /// only memory beside the vectors is `visited` (one flag a task, reused
    /// across jobs).
    fn permute_into_layout(&mut self, layout: &[usize], visited: &mut Vec<bool>) -> R<()> {
        let n = layout.len();
        if self.involvement.len() != n || self.share.len() != n || self.roles.len() != n {
            return Err(EvalError::State("job vector length"));
        }
        visited.clear();
        visited.resize(n, false);
        for start in 0..n {
            let mut at = start;
            loop {
                match visited.get_mut(at) {
                    Some(seen) if !*seen => *seen = true,
                    Some(_) => break,
                    None => return Err(EvalError::State("task space")),
                }
                let next = *layout.get(at).ok_or(EvalError::State("task space"))?;
                if next == start {
                    break;
                }
                if next >= n {
                    return Err(EvalError::State("task space"));
                }
                self.involvement.swap(at, next);
                self.share.swap(at, next);
                self.roles.swap(at, next);
                at = next;
            }
        }
        Ok(())
    }
}

/// The appended layout of a sealed task space, as slot -> task: the batch
/// tasks in task order, then each probe version's documents in version and
/// probe-record order (the order of its fingerprints), then audit draws A and
/// B in task order. `tasks` lists every task as `(task, docset, key)`, a
/// probe document's key being `q:<version>:<record task>`. For a cohort
/// sealed in the appended order this is the identity; under the seeded order
/// it undoes the interleaving, so the package's measurements and every probe
/// vector are computed exactly as before.
pub fn canonical_layout(tasks: &[(u32, &str, &str)]) -> R<Vec<usize>> {
    let mut slots = tasks
        .iter()
        .map(|(task, set, key)| {
            let place = match *set {
                "batch" => (0, 0, *task),
                "probe" => {
                    let mut parts = key.strip_prefix("q:").ok_or(EvalError::State("probe key"))?.split(':');
                    let version = parts.next().and_then(|x| x.parse::<u32>().ok());
                    let record = parts.next().and_then(|x| x.parse::<u32>().ok());
                    match (version, record, parts.next()) {
                        (Some(v), Some(r), None) => (1, v, r),
                        _ => return Err(EvalError::State("probe key")),
                    }
                }
                "audit_a" => (2, 0, *task),
                "audit_b" => (3, 0, *task),
                _ => return Err(EvalError::State("task set")),
            };
            Ok((place, *task))
        })
        .collect::<R<Vec<_>>>()?;
    slots.sort_unstable();
    if slots.windows(2).any(|w| w.first().map(|a| a.0) == w.get(1).map(|b| b.0)) {
        return Err(EvalError::State("task space"));
    }
    let layout: Vec<usize> = slots.iter().map(|(_, t)| *t as usize).collect();
    let mut seen = vec![false; layout.len()];
    for t in &layout {
        match seen.get_mut(*t) {
            Some(x) if !*x => *x = true,
            _ => return Err(EvalError::State("task space")),
        }
    }
    Ok(layout)
}

/// A task's private label.
#[derive(Clone, Debug)]
pub struct Label {
    pub set: String,
    pub kind: String,
    pub share_ppb: u32,
}

impl Label {
    fn involved(&self) -> bool {
        self.kind != "human_only"
    }
}

/// The measurements finalize asks for, over the hidden predictions.
pub struct Measure<'a> {
    pub labels: &'a [Label],
    pub batch: usize,
    /// Probe version -> its task range.
    pub probe_ranges: BTreeMap<u32, (usize, usize)>,
    pub audit_a: (usize, usize),
    pub audit_b: (usize, usize),
    pub reference: &'a JobVectors,
    pub predecessor: Option<&'a JobVectors>,
    /// Package index -> the entrant's vectors (`None`: never executed).
    pub entrants: Vec<Option<&'a JobVectors>>,
    pub hotkeys: Vec<[u8; 32]>,
    /// Every fingerprint on every probe version in use: (content, version) -> vector.
    pub fingerprints: &'a BTreeMap<(Digest, u32), Vec<Option<Probability>>>,
    pub reference_contents: Vec<Digest>,
    pub plan: &'a RewardV1Plan,
    pub draws: (Digest, Digest),
    /// Slots (in the appended layout) of the documents the scorer refused
    /// as over-length for every artifact ([`over_length_tasks`]), left out
    /// of the strict false-positive count. Empty unless the cohort was
    /// sealed in the seeded order.
    pub over_length: BTreeSet<usize>,
}

fn slice<T: Clone>(v: &[T], (a, b): (usize, usize)) -> R<Vec<T>> {
    v.get(a..b).map(<[T]>::to_vec).ok_or(EvalError::State("task range"))
}

fn graded(v: &[Option<Probability>], labels: &[Label]) -> Vec<Graded> {
    v.iter().zip(labels).map(|(p, l)| Graded { involvement: *p, label: l.involved() }).collect()
}

fn protocol(e: slop_ninja_protocol::Error) -> EvalError {
    let _ = e;
    EvalError::State("reward-v1 measurement")
}

impl Measure<'_> {
    fn entrant(&self, i: usize) -> slop_ninja_protocol::Result<&JobVectors> {
        self.entrants
            .get(i)
            .copied()
            .flatten()
            .ok_or(slop_ninja_protocol::Error::Invalid("unexecuted entrant measured"))
    }

    fn batch_of(&self, v: &JobVectors) -> slop_ninja_protocol::Result<Vec<Option<Probability>>> {
        v.involvement
            .get(..self.batch)
            .map(<[_]>::to_vec)
            .ok_or(slop_ninja_protocol::Error::Invalid("batch range"))
    }

    fn compare(&self, entrant: usize, content: &Digest, version: u32) -> slop_ninja_protocol::Result<ProbeMatch> {
        let range = *self.probe_ranges.get(&version).ok_or(slop_ninja_protocol::Error::Invalid("probe version"))?;
        let own = self
            .entrant(entrant)?
            .involvement
            .get(range.0..range.1)
            .ok_or(slop_ninja_protocol::Error::Invalid("probe range"))?;
        let fp = self
            .fingerprints
            .get(&(*content, version))
            .ok_or(slop_ninja_protocol::Error::Invalid("fingerprint missing"))?;
        let labels = self.labels.get(range.0..range.1).ok_or(slop_ninja_protocol::Error::Invalid("labels"))?;
        Ok(ProbeMatch {
            content: *content,
            version,
            stats: reward_v1::near_stats(own, fp)?,
            entrant_probe_score: reward_v1::document_score(&graded(own, labels))?,
            public_probe_score: reward_v1::document_score(&graded(fp, labels))?,
        })
    }

    /// False positives at the operating point over a range: a failure counts
    /// as flagged, except on a document no artifact can answer
    /// ([`Measure::over_length`]).
    fn false_positive_permille_exceeds(&self, v: &JobVectors, range: (usize, usize), max: u32) -> bool {
        let op = self.plan.gates.operating_ppb;
        let mut humans = 0u64;
        let mut flagged = 0u64;
        for t in range.0..range.1 {
            // No artifact can answer an over-length document, so it measures
            // none of them; a failure on any other document still counts.
            if self.over_length.contains(&t) {
                continue;
            }
            if self.labels.get(t).is_some_and(|l| l.kind == "human_only") {
                humans += 1;
                let p = v.involvement.get(t).copied().flatten();
                if p.is_none_or(|p| p.ppb() >= op) {
                    flagged += 1;
                }
            }
        }
        flagged * 1000 > u64::from(max) * humans
    }
}

impl Measurements for Measure<'_> {
    fn current(&mut self, entrant: usize) -> slop_ninja_protocol::Result<NearStats> {
        let e = self.batch_of(self.entrant(entrant)?)?;
        reward_v1::near_stats(&e, &self.batch_of(self.reference)?)
    }
    fn predecessor(&mut self, entrant: usize) -> slop_ninja_protocol::Result<NearStats> {
        let p = self.predecessor.ok_or(slop_ninja_protocol::Error::Invalid("no predecessor"))?;
        let e = self.batch_of(self.entrant(entrant)?)?;
        reward_v1::near_stats(&e, &self.batch_of(p)?)
    }
    fn reference_probe(&mut self, entrant: usize, content: &Digest, version: u32) -> slop_ninja_protocol::Result<ProbeMatch> {
        self.compare(entrant, content, version)
    }
    fn archive_probe(&mut self, entrant: usize) -> slop_ninja_protocol::Result<ArchiveProbe> {
        let mut out = ArchiveProbe::default();
        for (content, version) in self.fingerprints.keys() {
            if self.reference_contents.contains(content) {
                continue;
            }
            let m = self.compare(entrant, content, *version)?;
            if let Some(d) = m.stats.distance_ppb() {
                out.min_distance_ppb = Some(out.min_distance_ppb.map_or(d, |x| x.min(d)));
            }
            out.candidates.push(m);
        }
        Ok(out)
    }
    fn pair(&mut self, head: usize, other: usize) -> slop_ninja_protocol::Result<NearStats> {
        let o = self.batch_of(self.entrant(other)?)?;
        reward_v1::near_stats(&o, &self.batch_of(self.entrant(head)?)?)
    }
    fn promotion_audit(&mut self, head: usize) -> slop_ninja_protocol::Result<PromotionAudit> {
        let v = self.entrant(head)?;
        let hotkey = *self.hotkeys.get(head).ok_or(slop_ninja_protocol::Error::Invalid("head"))?;
        let strict = !self.false_positive_permille_exceeds(v, self.audit_a, self.plan.gates.strict_fpr_max_permille);
        let flags = self.plan.drill_flags(&hotkey);
        let flag = |on: bool| on.then(|| DRILL_REASON.to_owned());
        let mut canary = vec![CanaryAudit { draw: self.draws.0, flag: flag(flags >= 1) }];
        if flags >= 1 {
            canary.push(CanaryAudit { draw: self.draws.1, flag: flag(flags >= 2) });
        }
        Ok(PromotionAudit { head: AccountId(hotkey), strict_fpr_passed: strict, canary })
    }
}

/// The canonical bytes of a probe prediction vector: `u32_le(ppb)` per
/// document, `0xffffffff` for a failure.
pub fn vector_bytes(v: &[Option<Probability>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for p in v {
        out.extend_from_slice(&p.map_or(u32::MAX, |p| p.ppb()).to_le_bytes());
    }
    out
}

pub fn vector_from_bytes(b: &[u8]) -> Result<Vec<Option<Probability>>, &'static str> {
    let (words, rest) = b.as_chunks::<4>();
    if !rest.is_empty() {
        return Err("fingerprint vector length");
    }
    words
        .iter()
        .map(|a| {
            match u32::from_le_bytes(*a) {
                u32::MAX => Ok(None),
                v => Probability::new(v).map(Some).map_err(|_| "fingerprint value"),
            }
        })
        .collect()
}

pub fn vector_digest(v: &[Option<Probability>]) -> Digest {
    domain_hash(FINGERPRINT_DOMAIN, &vector_bytes(v))
}

/// What the package step produced for a v3 cohort.
#[derive(Clone, Debug)]
pub struct PackageV3 {
    pub manifest: CohortManifestV2,
    pub package: ScorePackageV2,
    pub target: Option<CalculationTargetV2>,
    pub components: Value,
    /// Private probe vectors to store: (key, bytes).
    pub vectors: Vec<(String, Vec<u8>)>,
    pub state: V3State,
}

/// The active reward-v1 target digest: the last activated cohort target, or
/// the launch burn row.
pub fn active_target_digest(sql: &impl Sql, chain: &ChainBinding) -> R<Digest> {
    let rows =
        sql.exec("SELECT target_digest FROM eval_targets WHERE rule = 'reward-v1' ORDER BY seq DESC LIMIT 1", &[])?;
    match rows.first() {
        Some(r) => parse_digest(text(r, 0)?).ok_or(EvalError::State("target digest")),
        None => CalculationTargetV2::launch_burn(chain.clone())
            .digest()
            .map_err(|_| EvalError::State("launch target")),
    }
}

/// The signed manifest as stored at seal, checked against the stored digest.
pub fn manifest_again(sql: &impl Sql, c: &Cohort) -> R<CohortManifestV2> {
    let st = state(sql, c.seq)?;
    let bytes = hex::decode(st.manifest_hex.ok_or(EvalError::State("stored manifest"))?)
        .map_err(|_| EvalError::State("stored manifest hex"))?;
    let m = CohortManifestV2::from_bytes(&bytes).map_err(|_| EvalError::State("stored manifest"))?;
    let rows = sql.exec("SELECT manifest_digest FROM cohorts WHERE seq = ?", &[i(c.seq)])?;
    let want = rows.first().and_then(|r| opt_text(r, 0)).ok_or(EvalError::State("manifest digest"))?;
    if m.digest().map_err(|_| EvalError::State("manifest digest"))?.to_string() != want {
        return Err(EvalError::State("stored manifest differs"));
    }
    Ok(m)
}

/// The published index this cohort's manifest binds.
pub fn index_at_seal(st: &V3State) -> R<PublishedIndex> {
    let (sc, sf) = st.published_at_seal.clone().ok_or(EvalError::State("published index at seal"))?;
    Ok(PublishedIndex {
        contents: slop_ninja_protocol::PublishedContents::from_bytes(
            &hex::decode(sc).map_err(|_| EvalError::State("hex"))?,
        )
        .map_err(|_| EvalError::State("published contents"))?,
        fingerprints: slop_ninja_protocol::FingerprintIndex::from_bytes(
            &hex::decode(sf).map_err(|_| EvalError::State("hex"))?,
        )
        .map_err(|_| EvalError::State("fingerprint index"))?,
    })
}

/// Builds the reward-v1 package from the jobs' verified summaries, the
/// private labels and the fingerprint vectors of every fingerprinted artifact
/// on the probe versions in use.
pub fn prepare_package(
    sql: &impl Sql,
    cfg: &EvalConfig,
    c: &Cohort,
    summaries: &[engine::JobSummary],
    fingerprints: &BTreeMap<(Digest, u32), Vec<Option<Probability>>>,
    receipt_root: Digest,
    receipt_count: u64,
) -> R<PackageV3> {
    let plan = &c.plan;
    let v = v1(plan)?;
    let mut st = state(sql, c.seq)?;
    let manifest = manifest_again(sql, c)?;
    let index = index_at_seal(&st)?;
    // Labels in task order. Under the seeded order they, and every entrant's
    // vectors, are read back into the appended layout every measurement
    // below assumes; an appended seal is in that layout already, and its
    // package reads exactly what it read before the seeded order existed.
    let seeded = v.seeded_task_order();
    let label_rows = sql.exec(
        if seeded {
            "SELECT task, docset, kind, share_ppb, CASE WHEN docset = 'probe' THEN key END FROM docs
             WHERE seq = ? AND task IS NOT NULL ORDER BY task"
        } else {
            "SELECT task, docset, kind, share_ppb FROM docs WHERE seq = ? AND task IS NOT NULL ORDER BY task"
        },
        &[i(c.seq)],
    )?;
    let label_of = |r: &crate::sql::Row| -> R<Label> {
        Ok(Label {
            set: text(r, 1)?.to_owned(),
            kind: text(r, 2)?.to_owned(),
            share_ppb: opt_int(r, 3).and_then(|x| u32::try_from(x).ok()).unwrap_or(0),
        })
    };
    let (labels, layout): (Vec<Label>, Option<Vec<usize>>) = if seeded {
        let layout = canonical_layout(
            &label_rows
                .iter()
                .map(|r| {
                    let task = u32::try_from(int(r, 0)?).map_err(|_| EvalError::State("task"))?;
                    // A probe document's key; NULL for every other set.
                    let key = match r.get(4) {
                        Some(SqlValue::Text(k)) => k.as_str(),
                        _ => "",
                    };
                    Ok((task, text(r, 1)?, key))
                })
                .collect::<R<Vec<_>>>()?,
        )?;
        // `canonical_layout` checked that the tasks are exactly `0..len`, so
        // row `t` (in task order) is task `t`.
        let labels = layout.iter().map(|t| label_rows.get(*t).ok_or(EvalError::State("task space")).and_then(label_of)).collect::<R<_>>()?;
        (labels, Some(layout))
    } else {
        (label_rows.iter().map(label_of).collect::<R<_>>()?, None)
    };
    drop(label_rows);
    let total = labels.len();
    let n = st.batch_tasks as usize;
    let mut probe_ranges = BTreeMap::new();
    let mut at = n;
    for (version, count) in &st.probe_tasks {
        probe_ranges.insert(*version, (at, at + *count as usize));
        at += *count as usize;
    }
    let audit = st.audit_tasks as usize;
    let audit_b_len = st.audit_b_tasks.map_or(audit, |b| b as usize);
    let audit_a = (at, at + audit);
    let audit_b = (at + audit, at + audit + audit_b_len);
    if audit_b.1 != total {
        return Err(EvalError::State("task space"));
    }
    // Job vectors from the summaries, checked against their verified reports.
    let jobs = sql.exec(
        "SELECT job, role, artifact, summary_sha256, summary_count, tasks FROM jobs WHERE seq = ? ORDER BY job",
        &[i(c.seq)],
    )?;
    let mut vectors: BTreeMap<i64, JobVectors> = BTreeMap::new();
    let mut visited = Vec::new();
    for j in &jobs {
        let job = int(j, 0)?;
        let tasks = opt_int(j, 5).unwrap_or(0);
        let sm = summaries.iter().find(|x| x.job == job).ok_or(EvalError::State("job summary missing"))?;
        if sm.artifact != text(j, 2)? || i64::from(sm.count) != opt_int(j, 4).unwrap_or(-1) || sm.inv_ppb.len() as i64 != tasks
        {
            return Err(EvalError::State("job summary does not match its verified report"));
        }
        let involvement = sm
            .inv_ppb
            .iter()
            .map(|x| x.map(Probability::new).transpose().map_err(|_| EvalError::State("probability")))
            .collect::<R<Vec<_>>>()?;
        let mut jv = JobVectors { involvement, share: sm.share_ppb.clone(), roles: sm.roles.clone() };
        // A reference's vectors are its batch tasks in task order, which is
        // the layout's batch order already; an entrant's cover every task and
        // are permuted in place, so no job's vectors are held twice.
        if let (Some(layout), false) = (&layout, matches!(text(j, 1)?, "reference" | "previous_reference")) {
            jv.permute_into_layout(layout, &mut visited)?;
        }
        vectors.insert(job, jv);
    }
    let job_of_role = |role: &str| -> R<i64> {
        jobs.iter()
            .find(|j| text(j, 1).is_ok_and(|r| r == role))
            .map(|j| int(j, 0))
            .transpose()?
            .ok_or(EvalError::State("reference job"))
    };
    let ref_job = job_of_role("reference")?;
    let reference = vectors.get(&ref_job).ok_or(EvalError::State("reference vectors"))?;
    let pred_job = plan.previous_reference.as_ref().map(|_| job_of_role("previous_reference")).transpose()?;
    let predecessor = pred_job.and_then(|j| vectors.get(&j));
    let batch_labels = labels.get(..n).ok_or(EvalError::State("labels"))?;
    let ref_batch = slice(&reference.involvement, (0, n))?;
    // A reference with any failure holds the package.
    let ref_score = match reward_v1::reference_score(&graded(&ref_batch, batch_labels)) {
        Ok(s) => s,
        Err(_) => return Err(EvalError::State("reference_failure")),
    };
    let pred_score = match predecessor {
        Some(p) => Some(
            reward_v1::reference_score(&graded(&slice(&p.involvement, (0, n))?, batch_labels))
                .map_err(|_| EvalError::State("reference_failure"))?,
        ),
        None => None,
    };
    let reference_digests = st.reference.ok_or(EvalError::State("reference digests"))?;
    // Entrants in roster order.
    let mut entrants = Vec::new();
    let mut entrant_vectors = Vec::new();
    let mut hotkeys = Vec::new();
    let first_entrant_job = 1 + i64::from(plan.previous_reference.is_some());
    for (pos, (a, dg)) in plan.entrants.iter().zip(&st.entrants).enumerate() {
        let id = identity_of(a)?;
        let roster = manifest.roster.get(pos).ok_or(EvalError::State("roster"))?;
        hotkeys.push(id.hotkey.0);
        let job = first_entrant_job + pos as i64;
        let vec = if st.unexecuted.contains(&pos) { None } else { vectors.get(&job) };
        let mut e = EntrantScore {
            identity: id,
            commitment: roster.commitment,
            commitment_block: roster.commitment_block,
            extrinsic_index: roster.extrinsic_index,
            content: dg.content.0,
            artifact: dg.artifact.0,
            gate_failures: Vec::new(),
            score: None,
            assigned: 0,
            succeeded: 0,
            failed: 0,
            public: None,
            cluster_head: None,
            cluster_stats: None,
            units: 0,
        };
        if let Some(jv) = vec {
            let batch = slice(&jv.involvement, (0, n))?;
            e.score = Some(reward_v1::document_score(&graded(&batch, batch_labels)).map_err(protocol)?);
            e.assigned = u32::try_from(n).map_err(|_| EvalError::State("n"))?;
            e.succeeded = u32::try_from(batch.iter().filter(|p| p.is_some()).count()).map_err(|_| EvalError::State("n"))?;
            e.failed = e.assigned - e.succeeded;
            e.gate_failures = gates(v, jv, batch_labels, n);
        }
        entrants.push(e);
        entrant_vectors.push(vec);
    }
    let reference_contents: Vec<Digest> =
        std::iter::once(reference_digests.content.0).chain(st.predecessor.map(|p| p.content.0)).collect();
    let draws = st.audit_draws.map(|(a, b)| (a.0, b.0)).ok_or(EvalError::State("audit draws"))?;
    // Under the seeded order, the over-length refusals by slot.
    let over_length: BTreeSet<usize> = match &layout {
        Some(layout) => {
            let tasks = over_length_tasks(sql, c.seq)?;
            layout.iter().enumerate().filter(|(_, t)| u32::try_from(**t).is_ok_and(|t| tasks.contains(&t))).map(|(slot, _)| slot).collect()
        }
        None => BTreeSet::new(),
    };
    let mut package = ScorePackageV2 {
        chain: manifest.chain.clone(),
        cohort: manifest.cohort,
        cohort_label: manifest.cohort_label.clone(),
        cohort_manifest: manifest.digest().map_err(|_| EvalError::State("manifest digest"))?,
        revision: 0,
        reward_rule: RewardRule::RewardV1,
        params: manifest.params,
        span_gate: manifest.span_gate,
        cases: manifest.batch.cases,
        reference_index: manifest.references.index,
        reference: ReferenceScore {
            model: manifest.references.current,
            score: ref_score,
            assigned: manifest.batch.cases,
            succeeded: manifest.batch.cases,
            failed: 0,
        },
        predecessor: match (manifest.references.predecessor, pred_score) {
            (Some(model), Some(score)) => Some(ReferenceScore {
                model,
                score,
                assigned: manifest.batch.cases,
                succeeded: manifest.batch.cases,
                failed: 0,
            }),
            (None, None) => None,
            _ => return Err(EvalError::State("predecessor score")),
        },
        published: manifest.published,
        probes: manifest.probes.clone(),
        entrants,
        mode: CutoffMode::NoTarget,
        promoted: None,
        promotion_audits: Vec::new(),
        credits: Vec::new(),
        distance_monitor: Default::default(),
        receipt_root,
        receipt_count,
    };
    let evaluation = {
        let mut m = Measure {
            labels: &labels,
            batch: n,
            probe_ranges: probe_ranges.clone(),
            audit_a,
            audit_b,
            reference,
            predecessor,
            entrants: entrant_vectors.clone(),
            hotkeys: hotkeys.clone(),
            fingerprints,
            reference_contents,
            plan: v,
            draws,
            over_length,
        };
        reward_v1::finalize(&mut package, &index, &mut m).map_err(|_| EvalError::State(PACKAGE_REFUSED))?
    };
    package.validate().map_err(|_| EvalError::State(PACKAGE_REFUSED))?;
    package.check_manifest(&manifest).map_err(|_| EvalError::State(PACKAGE_REFUSED))?;
    let target = match package.mode {
        CutoffMode::NoTarget => None,
        _ => Some(
            CalculationTargetV2::for_package(&package, Some(active_target_digest(sql, &cfg.chain)?))
                .map_err(|_| EvalError::State("calculation target"))?,
        ),
    };
    // Every scored entrant's probe fingerprint, and the canonical ones.
    let mut closure_fps = Vec::new();
    let mut vectors_out = Vec::new();
    let mut vector_key_of: BTreeMap<(AccountId, u32), String> = BTreeMap::new();
    for (e, jv) in package.entrants.iter().zip(&entrant_vectors) {
        let Some(jv) = jv else { continue };
        if e.score.is_none() {
            continue;
        }
        for (version, range) in &probe_ranges {
            let pv = slice(&jv.involvement, *range)?;
            let key =
                format!("{}/fp/{}/{}.bin", probe_prefix(cfg, *version), c.label, hex::encode(e.identity.hotkey.0));
            closure_fps.push(EntrantFingerprint {
                hotkey: e.identity.hotkey,
                version: *version,
                fingerprint: vector_digest(&pv),
            });
            vector_key_of.insert((e.identity.hotkey, *version), key.clone());
            vectors_out.push((key, vector_bytes(&pv)));
        }
    }
    let canonical = slop_ninja_protocol::canonical_fingerprints(&package, &closure_fps)
        .map_err(|_| EvalError::State("closure fingerprints"))?;
    let mut pending_fps = Vec::new();
    for f in &canonical {
        let key = closure_fps
            .iter()
            .filter(|x| x.version == f.version && x.fingerprint == f.fingerprint)
            .find_map(|x| vector_key_of.get(&(x.hotkey, x.version)))
            .ok_or(EvalError::State("canonical fingerprint vector"))?;
        pending_fps.push(PendingFingerprint {
            content: H(f.content),
            version: f.version,
            fingerprint: H(f.fingerprint),
            vector_key: key.clone(),
        });
    }
    st.mode = Some(format!("{:?}", package.mode));
    st.promoted = evaluation.promoted;
    st.pending_contents = package.entrants.iter().map(|e| H(e.content)).collect();
    st.pending_fingerprints = pending_fps;
    st.closure_fingerprints = closure_fps
        .iter()
        .map(|x| ClosureFingerprint { hotkey: H(Digest(x.hotkey.0)), version: x.version, fingerprint: H(x.fingerprint) })
        .collect();
    let components = json!({
        "schema": "slop_ninja_eval_score_components_v2",
        "cohort": plan.cohort, "label": c.label, "reward_rule": "reward-v1",
        "cohort_manifest": package.cohort_manifest.to_string(),
        "mode": format!("{:?}", package.mode),
        "promoted": package.promoted.map(|h| hex::encode(h.0)),
        "reference_score": package.reference.score.get().to_string(),
        "predecessor_score": package.predecessor.as_ref().map(|p| p.score.get().to_string()),
        "gate_policy": v.gates, "canary_rule": CANARY_RULE, "fpr_rule": FPR_RULE, "reuse_rule": REUSE_RULE,
        "policy": gate_policy_json(v),
        "unexecuted": st.unexecuted,
        "entrants": package.entrants.iter().map(|e| json!({
            "hotkey": hex::encode(e.identity.hotkey.0), "uid": e.identity.uid,
            "score": e.score.map(|s| s.get().to_string()),
            "gates": e.gate_failures.iter().map(|g| g.reason.clone()).collect::<Vec<_>>(),
            "cluster_head": e.cluster_head.map(|h| hex::encode(h.0)), "units": e.units,
        })).collect::<Vec<_>>(),
        "crash_rule": engine::CRASH_RULE, "artifact_rule": engine::ARTIFACT_RULE,
    });
    let mut components = components;
    // The v2 limits record the manifest binds (`published_limits`).
    if let (Some(l), Some(o)) = (published_limits(plan)?, components.as_object_mut()) {
        o.insert("limits".into(), l);
    }
    Ok(PackageV3 { manifest, package, target, components, vectors: vectors_out, state: st })
}

#[cfg(test)]
pub fn gates_for_test(v: &RewardV1Plan, jv: &JobVectors, labels: &[Label], n: usize) -> Vec<GateFailure> {
    gates(v, jv, labels, n)
}

/// The share, attribution and loose false-positive gates of one executed
/// entrant on the batch, in gate order.
fn gates(v: &RewardV1Plan, jv: &JobVectors, labels: &[Label], n: usize) -> Vec<GateFailure> {
    let mut out = Vec::new();
    let g = &v.gates;
    // Share: mean absolute error on mixed documents; a failure counts as the
    // largest error.
    let (mut err, mut mixed) = (0u128, 0u128);
    for t in 0..n {
        if labels.get(t).is_some_and(|l| l.kind == "mixed") {
            mixed += 1;
            let label = labels.get(t).map_or(0, |l| l.share_ppb);
            err += match jv.share.get(t).copied().flatten() {
                Some(p) => u128::from(p.abs_diff(label)),
                None => 1_000_000_000,
            };
        }
    }
    if mixed > 0 && err > u128::from(g.share_mae_max_ppb) * mixed {
        out.push(GateFailure { gate: Gate::Share, reason: "share.mae_above_threshold".into() });
    }
    // Attribution: predictions carrying both role results.
    let succeeded = (0..n).filter(|t| jv.involvement.get(*t).copied().flatten().is_some()).count() as u64;
    let with_roles = (0..n)
        .filter(|t| jv.involvement.get(*t).copied().flatten().is_some() && jv.roles.get(*t).copied().unwrap_or(false))
        .count() as u64;
    if with_roles * 1000 < u64::from(g.attribution_min_permille) * succeeded.max(1) {
        out.push(GateFailure { gate: Gate::Attribution, reason: "attribution.coverage_below_threshold".into() });
    }
    // Loose pure-human false-positive tier.
    let (mut humans, mut flagged) = (0u64, 0u64);
    for t in 0..n {
        if labels.get(t).is_some_and(|l| l.kind == "human_only") {
            humans += 1;
            if jv.involvement.get(t).copied().flatten().is_none_or(|p| p.ppb() >= g.operating_ppb) {
                flagged += 1;
            }
        }
    }
    if flagged * 1000 > u64::from(g.loose_fpr_max_permille) * humans {
        out.push(GateFailure { gate: Gate::HumanFalsePositive, reason: HUMAN_FPR_LOOSE_REASON.into() });
    }
    out
}

/// Commits the package: digests, outcome, the pending index additions, and
/// the start of the transition (whose first boundary is the publication epoch).
pub fn commit_package(sql: &impl Sql, seq: i64, b: &PackageV3, puts: u64, now_ms: u64) -> R<()> {
    let c = cohort(sql, seq)?;
    if c.phase != "packaging" {
        return Err(EvalError::State("package outside packaging"));
    }
    let o = observed(sql)?;
    sql.exec(
        "UPDATE cohorts SET receipt_root = ?, receipt_count = ?, package_digest = ?, target_digest = ?,
            packaged_epoch = ?, packaged_active = ?, r2_puts = r2_puts + ? WHERE seq = ?",
        &[
            s(b.package.receipt_root.to_string()),
            bind_u64(b.package.receipt_count)?,
            s(b.package.digest().map_err(|_| EvalError::State("package digest"))?.to_string()),
            match &b.target {
                Some(t) => s(t.digest().map_err(|_| EvalError::State("target digest"))?.to_string()),
                None => SqlValue::Null,
            },
            o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)),
            bind_u64(o.active_tempos)?,
            bind_u64(puts)?,
            i(seq),
        ],
    )?;
    save_state(sql, seq, &b.state)?;
    set_phase(sql, seq, "transition", now_ms, "package, target and receipts durable; publication at the next boundary")?;
    engine::advance(sql, now_ms)?;
    Ok(())
}

// --------------------------------------------------------------- probe build

/// A probe build's finish: each reference's fingerprint, the probe version's
/// binding and the published index seed.
#[derive(Clone, Debug)]
pub struct ProbeFinish {
    pub version: u32,
    pub vectors: Vec<(String, Vec<u8>)>,
    pub fingerprints: Vec<(Fingerprint, String)>,
    pub contents: Vec<Digest>,
    pub record_key: String,
    pub record_sha256: String,
    pub record_digest: Digest,
    pub size: u32,
}

pub fn prepare_probe_finish(
    sql: &impl Sql,
    cfg: &EvalConfig,
    c: &Cohort,
    summaries: &[engine::JobSummary],
    record: &[u8],
) -> R<ProbeFinish> {
    let v = v1(&c.plan)?;
    let version = v.probe_version.ok_or(EvalError::Plan("probe_version"))?;
    let st = state(sql, c.seq)?;
    let rec: ProbeRecord = serde_json::from_slice(record).map_err(|_| EvalError::State("probe record"))?;
    if rec.version != version || rec.size != st.batch_tasks {
        return Err(EvalError::State("probe record"));
    }
    let jobs = sql.exec("SELECT job, role, artifact, summary_count FROM jobs WHERE seq = ? ORDER BY job", &[i(c.seq)])?;
    let mut vectors = Vec::new();
    let mut fps = Vec::new();
    let mut contents = Vec::new();
    for j in &jobs {
        let job = int(j, 0)?;
        let role = text(j, 1)?;
        let dg = match role {
            "reference" => st.reference,
            _ => st.predecessor,
        }
        .ok_or(EvalError::State("reference digests"))?;
        let sm = summaries.iter().find(|x| x.job == job).ok_or(EvalError::State("job summary missing"))?;
        if sm.artifact != text(j, 2)? || i64::from(sm.count) != opt_int(j, 3).unwrap_or(-1) {
            return Err(EvalError::State("job summary does not match its verified report"));
        }
        let pv = sm
            .inv_ppb
            .iter()
            .map(|x| x.map(Probability::new).transpose().map_err(|_| EvalError::State("probability")))
            .collect::<R<Vec<_>>>()?;
        if pv.iter().any(Option::is_none) {
            // A reference must answer every probe document.
            return Err(EvalError::State("reference_failure"));
        }
        let key = format!("{}/fp/{}.bin", probe_prefix(cfg, version), dg.content.0);
        fps.push((Fingerprint { content: dg.content.0, version, fingerprint: vector_digest(&pv) }, key.clone()));
        vectors.push((key, vector_bytes(&pv)));
        contents.push(dg.content.0);
    }
    Ok(ProbeFinish {
        version,
        vectors,
        fingerprints: fps,
        contents,
        record_key: format!("{}/record.json", probe_prefix(cfg, version)),
        record_sha256: sha_hex(record),
        record_digest: ProbeRecord::digest(record),
        size: rec.size,
    })
}

pub fn commit_probe_finish(sql: &impl Sql, seq: i64, f: &ProbeFinish, now_ms: u64) -> R<()> {
    let c = cohort(sql, seq)?;
    if c.phase != "packaging" || !c.plan.is_probe_build() {
        return Err(EvalError::State("probe finish outside packaging"));
    }
    if !sql.exec("SELECT 1 FROM probe_versions WHERE version = ?", &[i(i64::from(f.version))])?.is_empty() {
        return Err(EvalError::State("probe version exists"));
    }
    sql.exec(
        "INSERT INTO probe_versions (version, record_key, record_sha256, size, digest, seq) VALUES (?, ?, ?, ?, ?, ?)",
        &[
            i(i64::from(f.version)),
            s(&f.record_key),
            s(&f.record_sha256),
            i(i64::from(f.size)),
            s(f.record_digest.to_string()),
            i(seq),
        ],
    )?;
    for content in &f.contents {
        sql.exec(
            "INSERT OR IGNORE INTO published_contents (content, seq) VALUES (?, ?)",
            &[s(content.to_string()), i(seq)],
        )?;
    }
    for (fp, key) in &f.fingerprints {
        sql.exec(
            "INSERT INTO fingerprints (content, version, fingerprint, vector_key, seq) VALUES (?, ?, ?, ?, ?)",
            &[s(fp.content.to_string()), i(i64::from(fp.version)), s(fp.fingerprint.to_string()), s(key), i(seq)],
        )?;
    }
    let st = state(sql, seq)?;
    if sql.exec("SELECT 1 FROM reference_state WHERE id = 1", &[])?.is_empty()
        && let Some(r) = st.reference
    {
        sql.exec(
            "INSERT INTO reference_state (id, k, current, predecessor) VALUES (1, ?, ?, ?)",
            &[
                bind_u64(st.reference_index)?,
                s(r.content.0.to_string()),
                st.predecessor.map_or(SqlValue::Null, |p| s(p.content.0.to_string())),
            ],
        )?;
    }
    let o = observed(sql)?;
    sql.exec(
        "UPDATE cohorts SET packaged_epoch = ?, closed_epoch = ?, closure_digest = 'probe-build' WHERE seq = ?",
        &[
            o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)),
            o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)),
            i(seq),
        ],
    )?;
    sql.exec("UPDATE docs SET text = NULL WHERE seq = ?", &[i(seq)])?;
    set_phase(sql, seq, "closed", now_ms, "probe version recorded; references fingerprinted; no package or target")?;
    Ok(())
}

// ------------------------------------------------------------------- closure

/// At the closing boundary (inside `advance`): the cohort's contents and
/// canonical fingerprints join the index, its target activates, and a
/// promotion advances the reference chain.
pub fn close(sql: &impl Sql, c: &Cohort, epoch: u64) -> R<()> {
    let st = state(sql, c.seq)?;
    for content in &st.pending_contents {
        sql.exec(
            "INSERT OR IGNORE INTO published_contents (content, seq) VALUES (?, ?)",
            &[s(content.0.to_string()), i(c.seq)],
        )?;
    }
    for f in &st.pending_fingerprints {
        sql.exec(
            "INSERT OR IGNORE INTO fingerprints (content, version, fingerprint, vector_key, seq) VALUES (?, ?, ?, ?, ?)",
            &[
                s(f.content.0.to_string()),
                i(i64::from(f.version)),
                s(f.fingerprint.0.to_string()),
                s(&f.vector_key),
                i(c.seq),
            ],
        )?;
    }
    let target = sql.exec("SELECT target_digest FROM cohorts WHERE seq = ?", &[i(c.seq)])?;
    if let Some(t) = target.first().and_then(|r| opt_text(r, 0)) {
        sql.exec(
            "INSERT INTO eval_targets (seq, target_digest, activated_epoch, rule) VALUES (?, ?, ?, 'reward-v1')",
            &[i(c.seq), s(t), bind_u64(epoch)?],
        )?;
    }
    if let (Some(p), Some(current)) = (st.promoted, st.reference) {
        let promoted = st.entrants.get(p).ok_or(EvalError::State("promoted entrant"))?;
        sql.exec(
            "INSERT INTO reference_state (id, k, current, predecessor) VALUES (1, ?, ?, ?)
             ON CONFLICT (id) DO UPDATE SET k = excluded.k, current = excluded.current,
                predecessor = excluded.predecessor",
            &[bind_u64(st.reference_index + 1)?, s(promoted.content.0.to_string()), s(current.content.0.to_string())],
        )?;
    } else if sql.exec("SELECT 1 FROM reference_state WHERE id = 1", &[])?.is_empty()
        && let Some(r) = st.reference
    {
        sql.exec(
            "INSERT INTO reference_state (id, k, current, predecessor) VALUES (1, ?, ?, ?)",
            &[
                bind_u64(st.reference_index)?,
                s(r.content.0.to_string()),
                st.predecessor.map_or(SqlValue::Null, |p| s(p.content.0.to_string())),
            ],
        )?;
    }
    Ok(())
}

/// The signed closure of a closed cohort, and the published index after it.
#[derive(Clone, Debug)]
pub struct ClosureBuild {
    pub closure: CohortClosure,
    pub published_after: (Vec<u8>, Vec<u8>),
    pub closure_fingerprints: Vec<EntrantFingerprint>,
}

pub fn prepare_closure(sql: &impl Sql, cfg: &EvalConfig, seq: i64) -> R<ClosureBuild> {
    let c = cohort(sql, seq)?;
    if c.phase != "closed" {
        return Err(EvalError::State("closure before close"));
    }
    let st = state(sql, seq)?;
    let rows = sql.exec(
        "SELECT package_digest, target_digest, admission_closed_epoch, published_epoch, closed_epoch, manifest_digest
         FROM cohorts WHERE seq = ?",
        &[i(seq)],
    )?;
    let r = rows.first().ok_or(EvalError::State("cohort"))?;
    let package = parse_digest(text(r, 0)?).ok_or(EvalError::State("package digest"))?;
    let target = opt_text(r, 1).map(|t| parse_digest(&t).ok_or(EvalError::State("target digest"))).transpose()?;
    let manifest = parse_digest(text(r, 5)?).ok_or(EvalError::State("manifest digest"))?;
    let mut openings = Vec::new();
    for a in &c.plan.entrants {
        let id = identity_of(a)?;
        openings.push(PublishedOpening {
            hotkey: id.hotkey,
            registration_block: id.registration_block,
            nonce: entrant_nonce(cfg, &c, a)?,
        });
    }
    let closure = CohortClosure {
        chain: cfg.chain.clone(),
        cohort: c.plan.cohort,
        cohort_label: c.label.clone(),
        cohort_manifest: manifest,
        score_package: package,
        target,
        admission_closed_epoch: uint(r, 2)?,
        published_epoch: uint(r, 3)?,
        closed_epoch: uint(r, 4)?,
        openings,
    };
    closure.validate().map_err(|_| EvalError::State("closure"))?;
    let (index, _) = published_index(sql)?;
    Ok(ClosureBuild {
        closure,
        published_after: (
            index.contents.to_bytes().map_err(|_| EvalError::State("published contents"))?,
            index.fingerprints.to_bytes().map_err(|_| EvalError::State("fingerprint index"))?,
        ),
        closure_fingerprints: st.closure_fingerprints.iter().map(ClosureFingerprint::record).collect(),
    })
}

pub fn commit_closure(sql: &impl Sql, seq: i64, digest: &Digest, puts: u64) -> R<()> {
    sql.exec(
        "UPDATE cohorts SET closure_digest = ?, r2_puts = r2_puts + ? WHERE seq = ? AND closure_digest IS NULL",
        &[s(digest.to_string()), bind_u64(puts)?, i(seq)],
    )?;
    Ok(())
}

/// A closed v3 cohort whose closure is not yet published.
/// A closed reward-v1 cohort whose closure is not yet written. With
/// `due_by`, only one that may run then: not paused by a fault (a closure
/// that cannot be written pauses its own cohort as `step_refused` and blocks
/// nothing else) and past the retry time of its last failed attempt
/// (`engine::note_step_error`), so a failing closure yields to admission,
/// later cohorts, settlement and purges instead of holding the head of every
/// step. Without it, any pending closure (the work a succession waits for).
pub fn closure_due(sql: &impl Sql, due_by: Option<u64>) -> R<Option<i64>> {
    let rows = match due_by {
        Some(now) => sql.exec(
            "SELECT seq FROM cohorts WHERE phase = 'closed' AND v3 IS NOT NULL AND closure_digest IS NULL
               AND package_digest IS NOT NULL AND fault IS NULL AND step_retry_ms <= ? ORDER BY seq LIMIT 1",
            &[bind_u64(now)?],
        )?,
        None => sql.exec(
            "SELECT seq FROM cohorts WHERE phase = 'closed' AND v3 IS NOT NULL AND closure_digest IS NULL
               AND package_digest IS NOT NULL ORDER BY seq LIMIT 1",
            &[],
        )?,
    };
    rows.first().map(|r| int(r, 0)).transpose().map_err(EvalError::from)
}

/// The incentive regime for `/eval/status`.
pub fn regime(sql: &impl Sql, chain: &ChainBinding) -> R<Value> {
    let active = active_target_digest(sql, chain)?;
    let launch = CalculationTargetV2::launch_burn(chain.clone()).digest().map_err(|_| EvalError::State("launch"))?;
    let refs = sql.exec("SELECT k, current, predecessor FROM reference_state WHERE id = 1", &[])?;
    let reference = refs.first().map(|r| {
        json!({"k": opt_int(r, 0), "current_content": opt_text(r, 1), "predecessor_content": opt_text(r, 2)})
    });
    let note = if active == launch {
        "launch burn: every unit goes to the owner hotkey's UID until an entrant beats R_k by delta and its cohort target activates"
    } else {
        "a cohort target is active: validators pay its clusters every tempo; unregistered or re-registered hotkeys burn to the owner hotkey's UID"
    };
    Ok(json!({"rule": "reward-v1", "active_target": active.to_string(), "launch_burn": active == launch,
        "references": reference, "note": note}))
}

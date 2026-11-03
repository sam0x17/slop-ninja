//! Replay of the v1 draws: the cohorts the testnet coordinator drew from
//! `testnet-human-pool-v1`, re-run through the engine's own draw code.
//!
//! [`replay_counts`] needs only the content-free record (the pool's rows per domain
//! and each cohort's plan): domain quotas are the engine's largest-remainder split
//! of the rows still available, so while every domain fills its quota exactly the
//! quotas, human counts, availability and the final shortfall follow without the
//! private cohort seed. [`replay_pool`] runs the engine's `pool::draw` on the real
//! manifest and its text shards under a stand-in seed (never the coordinator's):
//! the same seed-free quantities must come out, and the mixed counts follow the
//! engine's rule that a parent needs at least two sentences. Which groups are drawn,
//! and so the group counts, depend on the private seed and are not replayed.
//!
//! Both replays run the frozen copies in [`crate::engine_v1`] and first check that
//! they are byte for byte the files the record names, so a later engine change
//! cannot silently change what "matches the v1 draws" tests.
use crate::engine_v1::{kind_counts, pool, textgen};
use crate::stock::read_checked;
use crate::{data_dir, rel_under, resolve};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const RECORD_SCHEMA: &str = "slop_ninja_testnet_pool_v1_recorded_draws_v1";
pub const TRACKED_RECORD: &str = include_str!("../fixtures/v1-draws.json");
const REPLAY_SEED_DOMAIN: &[u8] = b"slop_ninja/testnet_pool_v1_replay/stand_in_seed/v1\0";

#[derive(Clone, Debug, Deserialize)]
pub struct RecordedDraw {
    pub available_before: u64,
    pub humans: u64,
    pub groups: u64,
    pub quotas: BTreeMap<String, u64>,
    pub mixed: u64,
    pub scratch: u64,
    pub shortfall: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RecordedCounts {
    pub plan: [u32; 3],
    pub have: [u32; 3],
    pub shortfall: [u32; 3],
}

#[derive(Clone, Debug, Deserialize)]
pub struct RecordedCohort {
    pub label: String,
    pub cohort: u64,
    pub n: u32,
    pub mix: [u32; 3],
    pub max_kind_shortfall_permille: u16,
    #[serde(default)]
    pub draw: Option<RecordedDraw>,
    #[serde(default)]
    pub counts: Option<RecordedCounts>,
    pub phase: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RecordedPool {
    pub pool_id: String,
    pub manifest_sha256: String,
    pub rows: u64,
    pub source_groups: u64,
    pub by_domain: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UsedAfter {
    pub rows: u64,
    pub remaining: u64,
}

/// The engine source that drew the recorded cohorts.
#[derive(Clone, Debug, Deserialize)]
pub struct RecordedEngine {
    pub source: String,
    pub commit: String,
    pub pool_rs_sha256: String,
    pub textgen_rs_sha256: String,
    pub cohorts: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Record {
    pub schema: String,
    pub engine: RecordedEngine,
    pub pool: RecordedPool,
    pub cohorts: Vec<RecordedCohort>,
    pub used_after: UsedAfter,
}

impl Record {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let r: Self = serde_json::from_slice(bytes).context("recorded draws JSON")?;
        ensure!(r.schema == RECORD_SCHEMA, "recorded draws schema {}", r.schema);
        ensure!(r.pool.by_domain.values().sum::<u64>() == r.pool.rows, "pool rows by domain");
        ensure!(r.engine.commit.len() == 40 && crate::is_digest(&r.engine.pool_rs_sha256) && crate::is_digest(&r.engine.textgen_rs_sha256),
            "the record names the engine commit and file digests");
        Ok(r)
    }

    /// Refuses when the frozen engine copies are not the files that drew the record.
    pub fn check_engine(&self) -> Result<()> {
        let pool = crate::sha256_hex(crate::engine_v1::POOL_SOURCE);
        let textgen = crate::sha256_hex(crate::engine_v1::TEXTGEN_SOURCE);
        ensure!(pool == self.engine.pool_rs_sha256, "frozen pool.rs {pool} is not the engine that drew the record ({})", self.engine.pool_rs_sha256);
        ensure!(textgen == self.engine.textgen_rs_sha256, "frozen textgen.rs {textgen} is not the engine that drew the record ({})", self.engine.textgen_rs_sha256);
        Ok(())
    }
}

/// Per-kind shortfall beyond the plan's tolerance (the engine's `excess_shortfall`).
fn excess(plan: [u32; 3], have: [u32; 3], permille: u16) -> Option<[u32; 3]> {
    let short = [0, 1, 2].map(|k| plan[k].saturating_sub(have[k]));
    let over = (0..3).any(|k| u64::from(short[k]) > u64::from(plan[k]) * u64::from(permille) / 1000);
    over.then_some(short)
}

/// Replays the quota sequence from counts alone and checks it against the record.
pub fn replay_counts(r: &Record) -> Result<Value> {
    r.check_engine()?;
    let mut left = r.pool.by_domain.clone();
    let mut out = Vec::new();
    for c in &r.cohorts {
        let plan = kind_counts(c.n, c.mix);
        let available: u64 = left.values().sum();
        let want = u64::from(plan[0]);
        let quotas = pool::proportional(&left, want.min(available));
        let humans: u64 = quotas.values().sum();
        let have = [humans as u32, (humans as u32).min(plan[1]), plan[2]];
        let short = excess(plan, have, c.max_kind_shortfall_permille);
        if let Some(d) = &c.draw {
            ensure!(d.available_before == available, "{}: available {available}, recorded {}", c.label, d.available_before);
            ensure!(d.humans == humans, "{}: humans {humans}, recorded {}", c.label, d.humans);
            ensure!(d.quotas == quotas, "{}: quotas {quotas:?}, recorded {:?}", c.label, d.quotas);
            ensure!(d.scratch == u64::from(plan[2]), "{}: scratch", c.label);
            ensure!(d.shortfall == want - humans, "{}: shortfall", c.label);
            ensure!(d.mixed <= humans.min(u64::from(plan[1])), "{}: mixed above the one-child bound", c.label);
        }
        if let Some(k) = &c.counts {
            ensure!(k.plan == plan, "{}: plan counts {plan:?}, recorded {:?}", c.label, k.plan);
            ensure!(k.have == have, "{}: have {have:?}, recorded {:?}", c.label, k.have);
            ensure!(k.shortfall == short.unwrap_or([0; 3]), "{}: shortfall", c.label);
        }
        let drawn = short.is_none();
        ensure!(drawn == (c.draw.is_some() && c.phase != "abandoned"), "{}: drawn {drawn}, phase {}", c.label, c.phase);
        if drawn {
            for (d, q) in &quotas {
                *left.get_mut(d).expect("domain") -= q;
            }
        }
        out.push(json!({"label": c.label, "available_before": available, "plan": plan, "humans": humans,
            "quotas": quotas, "drawn": drawn, "shortfall": short}));
    }
    let used = r.pool.rows - left.values().sum::<u64>();
    ensure!(used == r.used_after.rows && left.values().sum::<u64>() == r.used_after.remaining, "used {used} after the replay");
    Ok(json!({"rule": pool::DRAW_RULE, "engine_commit": r.engine.commit, "cohorts": out, "used_rows": used,
        "remaining_rows": r.used_after.remaining}))
}

/// Replays the record through the engine's `pool::draw` on the real pool manifest
/// and shards under `dir` (under `data/`), with a stand-in seed per cohort.
pub fn replay_pool(repo: &Path, dir: &str, r: &Record) -> Result<Value> {
    r.check_engine()?;
    let mpath = format!("{dir}/manifest.json");
    let mbytes = read_checked(&resolve(repo, &mpath)?, &mpath, &r.pool.manifest_sha256)?;
    let manifest = pool::PoolManifest::parse(&mbytes, &r.pool.pool_id).map_err(|e| anyhow::anyhow!(e.0))?;
    ensure!(manifest.rows.len() as u64 == r.pool.rows, "pool rows");
    let mut texts: BTreeMap<String, String> = BTreeMap::new();
    for s in &manifest.shards {
        let p = format!("{dir}/{}", s.file);
        let body = read_checked(&resolve(repo, &p)?, &p, &s.sha256)?;
        for line in std::str::from_utf8(&body).context("shard UTF-8")?.lines() {
            let row: pool::ShardRow = serde_json::from_str(line).context("shard row")?;
            ensure!(row.schema == pool::SHARD_SCHEMA && crate::sha256_hex(row.text.as_bytes()) == row.text_sha256, "shard row");
            texts.insert(row.row_id, row.text);
        }
    }
    ensure!(texts.len() == manifest.rows.len(), "every pool row has its text");
    let parents_ok: BTreeSet<&str> =
        manifest.rows.iter().filter(|x| textgen::sentences(&texts[&x.row_id]).len() >= 2).map(|x| x.row_id.as_str()).collect();
    let (mut used_rows, mut used_groups) = (BTreeSet::new(), BTreeSet::new());
    let mut out = Vec::new();
    for c in &r.cohorts {
        let plan = kind_counts(c.n, c.mix);
        let seed: [u8; 32] = Sha256::new().chain_update(REPLAY_SEED_DOMAIN).chain_update(c.label.as_bytes()).finalize().into();
        let d = pool::draw(&manifest, &used_rows, &used_groups, u64::from(plan[0]), &seed);
        let mut mixed = 0u32;
        for row in &d.rows {
            if mixed < plan[1] && parents_ok.contains(row.row_id.as_str()) {
                mixed += 1;
            }
        }
        let have = [d.rows.len() as u32, mixed, plan[2]];
        let short = excess(plan, have, c.max_kind_shortfall_permille);
        if let Some(rd) = &c.draw {
            ensure!(rd.available_before == d.available_rows, "{}: available {}", c.label, d.available_rows);
            ensure!(rd.humans == d.rows.len() as u64, "{}: humans {}", c.label, d.rows.len());
            ensure!(rd.quotas == d.quotas, "{}: quotas {:?}", c.label, d.quotas);
            ensure!(rd.mixed == u64::from(mixed), "{}: mixed {mixed}, recorded {}", c.label, rd.mixed);
            ensure!(rd.shortfall == d.shortfall, "{}: shortfall {}", c.label, d.shortfall);
        }
        if let Some(k) = &c.counts {
            ensure!(k.have == have && k.plan == plan, "{}: counts {have:?} of {plan:?}", c.label);
        }
        let drawn = short.is_none();
        if drawn {
            used_rows.extend(d.rows.iter().map(|x| x.row_id.clone()));
            used_groups.extend(d.groups.iter().cloned());
        }
        out.push(json!({"label": c.label, "available_before": d.available_rows, "humans": d.rows.len(),
            "quotas": d.quotas, "mixed": mixed, "drawn": drawn, "groups_under_stand_in_seed": d.groups.len(),
            "recorded_groups": c.draw.as_ref().map(|x| x.groups)}));
    }
    ensure!(used_rows.len() as u64 == r.used_after.rows, "used rows {} after the replay", used_rows.len());
    ensure!(r.pool.rows - used_rows.len() as u64 == r.used_after.remaining, "remaining rows");
    let data = data_dir(repo)?;
    let rel = rel_under(&data, &resolve(repo, &mpath)?)?;
    Ok(json!({"rule": pool::DRAW_RULE, "pool_manifest": rel, "rows_with_two_sentences": parents_ok.len(),
        "cohorts": out, "used_rows": used_rows.len(), "used_groups_under_stand_in_seed": used_groups.len(),
        "seed": "stand-in per cohort label; the coordinator's private seed is not used"}))
}

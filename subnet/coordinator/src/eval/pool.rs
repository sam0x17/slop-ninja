//! The hidden human pool and the never-reuse draw.
//!
//! The pool is a content-free manifest (row ids, text digests, domains, source
//! groups, shard digests) plus private text shards in the eval bucket. A cohort
//! draws whole source groups, never a row or group that any earlier cohort used.
//! The used set lives in the cohort Durable Object, written in the same
//! transaction that creates the cohort's documents, and in a durable ledger
//! outside it: one content-free entry per cohort beside the pool in the eval
//! bucket (`<pool dir>/used/<label>.json`), written before that transaction. A
//! draw avoids the union of both, and refuses to draw when the ledger cannot be
//! read, so a new or emptied cohort authority (another Durable Object namespace,
//! a deleted Worker) still never reuses a human that any cohort drew.
//!
//! Strata are domains: each domain's quota is proportional to its available rows
//! (largest remainder), so the sample self-weights by domain. Within a domain,
//! groups are ranked by a private digest and taken while they fit the quota;
//! a remaining shortfall is filled from any domain in rank order.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const POOL_SCHEMA: &str = "slop_ninja_testnet_human_pool_v1";
pub const SHARD_SCHEMA: &str = "slop_ninja_testnet_human_pool_row_v1";
pub const DRAW_RULE: &str = "whole_group_domain_proportional_v1";
/// Ledger entries written before authorities were named in them.
pub const LEDGER_SCHEMA_V1: &str = "slop_ninja_eval_pool_used_v1";
pub const LEDGER_SCHEMA: &str = "slop_ninja_eval_pool_used_v2";

#[derive(Clone, Debug, Deserialize)]
pub struct PoolManifest {
    pub schema: String,
    pub pool_id: String,
    pub shards: Vec<ShardInfo>,
    pub rows: Vec<PoolRow>,
    /// The export's selection record. Only its token bound is read here
    /// ([`PoolManifest::token_bound`]); pool v1 records none.
    #[serde(default)]
    pub selection: Option<PoolSelection>,
}

/// The part of a pool export's `selection` record the draw reads.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PoolSelection {
    /// Every row's token count, special tokens excluded, is at most this
    /// (`tools/a-testnet-eval-manifest` public-candidate export: rows over
    /// it, or without a count, are excluded before the pool is written).
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ShardInfo {
    pub index: u32,
    pub file: String,
    pub rows: u32,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PoolRow {
    pub row_id: String,
    pub text_sha256: String,
    pub domain: String,
    pub source_group: String,
    pub shard: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShardRow {
    pub schema: String,
    pub row_id: String,
    pub text_sha256: String,
    pub domain: String,
    pub source_group: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolError(pub &'static str);

impl PoolManifest {
    pub fn parse(bytes: &[u8], pool_id: &str) -> Result<Self, PoolError> {
        let m: Self = serde_json::from_slice(bytes).map_err(|_| PoolError("pool manifest json"))?;
        if m.schema != POOL_SCHEMA || m.pool_id != pool_id {
            return Err(PoolError("pool manifest schema or id"));
        }
        let mut ids = BTreeSet::new();
        let mut texts = BTreeSet::new();
        for r in &m.rows {
            if !ids.insert(r.row_id.as_str()) || !texts.insert(r.text_sha256.as_str()) {
                return Err(PoolError("duplicate pool row"));
            }
            if !m.shards.iter().any(|s| s.index == r.shard) {
                return Err(PoolError("row names an unknown shard"));
            }
        }
        Ok(m)
    }

    /// The largest token count, special tokens excluded, the export admitted
    /// for any row of the pool, if it records one.
    pub fn token_bound(&self) -> Option<u64> {
        self.selection.as_ref().and_then(|s| s.max_tokens)
    }
}

/// One used row in the ledger (private ids and digests; never text).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerRow {
    pub row_id: String,
    pub text_sha256: String,
}

/// One cohort's ledger entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerEntry {
    pub schema: String,
    pub pool_id: String,
    pub cohort: u64,
    pub label: String,
    pub rule: String,
    /// The cohort authority that wrote the entry (absent in v1 entries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_sha256: Option<String>,
    pub rows: Vec<LedgerRow>,
    pub groups: Vec<String>,
    /// reward-v1: every parent use of this cohort's mixed children (a
    /// same-cohort negative, an earlier passage or a fresh parent). A record
    /// outside the cohort authority: a new authority does not rebuild its
    /// rewrite-source ledger from it, so the ledger starts empty there (the
    /// announce restart resets it on purpose).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceUse>,
    /// reward-v1: `exposed` when the cohort drew exposed stock, whose rows
    /// later cohorts may draw again (fresh stock is never reused).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draw_mode: Option<String>,
    /// Rows the draw's input screen held out of every role of this cohort
    /// (`v3::screen_pool`): the scorer would refuse their text for every
    /// artifact. Written only by cohorts sealed in the seeded order; not
    /// used rows (a later cohort's screen finds them again).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screened: Vec<LedgerRow>,
}

/// One parent use in the rewrite-source ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceUse {
    pub row_id: String,
    pub text_sha256: String,
    /// `same`, `earlier` or `fresh`.
    pub origin: String,
}

impl LedgerEntry {
    pub fn parse(bytes: &[u8], pool_id: &str) -> Result<Self, PoolError> {
        let e: Self = serde_json::from_slice(bytes).map_err(|_| PoolError("used ledger entry json"))?;
        if !matches!(e.schema.as_str(), LEDGER_SCHEMA | LEDGER_SCHEMA_V1) || e.pool_id != pool_id {
            return Err(PoolError("used ledger entry schema or pool"));
        }
        if e.schema == LEDGER_SCHEMA && e.authority.is_none() {
            return Err(PoolError("used ledger entry authority"));
        }
        Ok(e)
    }
}

/// Rows, text digests and source groups a draw must avoid.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Used {
    pub rows: BTreeSet<String>,
    pub digests: BTreeSet<String>,
    pub groups: BTreeSet<String>,
}

impl Used {
    /// Adds every ledger entry except this authority's own earlier write for
    /// `label` (a draw whose commit did not land is redrawn and rewritten). An
    /// entry from any other authority, or with no authority, always counts.
    pub fn extend_from_ledger(&mut self, entries: &[LedgerEntry], authority: &str, label: &str) {
        for e in entries {
            if e.authority.as_deref() == Some(authority) && e.label == label {
                continue;
            }
            for r in &e.rows {
                self.rows.insert(r.row_id.clone());
                self.digests.insert(r.text_sha256.clone());
            }
            self.groups.extend(e.groups.iter().cloned());
        }
    }

    /// The row ids of `manifest` that are used by id or by text digest.
    pub fn row_ids(&self, manifest: &PoolManifest) -> BTreeSet<String> {
        let mut ids = self.rows.clone();
        ids.extend(manifest.rows.iter().filter(|r| self.digests.contains(&r.text_sha256)).map(|r| r.row_id.clone()));
        ids
    }

    pub fn contains(&self, r: &PoolRow) -> bool {
        self.rows.contains(&r.row_id) || self.digests.contains(&r.text_sha256) || self.groups.contains(&r.source_group)
    }
}

/// The ledger entry a draw writes before it commits.
pub fn ledger_entry(pool_id: &str, cohort: u64, label: &str, authority: &str, plan_sha256: &str, d: &Draw) -> LedgerEntry {
    LedgerEntry {
        schema: LEDGER_SCHEMA.into(),
        pool_id: pool_id.into(),
        cohort,
        label: label.into(),
        rule: DRAW_RULE.into(),
        authority: Some(authority.into()),
        plan_sha256: Some(plan_sha256.into()),
        rows: d.rows.iter().map(|r| LedgerRow { row_id: r.row_id.clone(), text_sha256: r.text_sha256.clone() }).collect(),
        groups: d.groups.clone(),
        sources: Vec::new(),
        draw_mode: None,
        screened: Vec::new(),
    }
}

pub fn rank(seed: &[u8; 32], what: &str, key: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"slop_ninja.eval/pool-rank/v1\0");
    h.update(seed);
    h.update(what.as_bytes());
    h.update([0]);
    h.update(key.as_bytes());
    h.finalize().into()
}

/// Largest-remainder split of `total` proportional to `weights` (ties by key order).
pub fn proportional(weights: &BTreeMap<String, u64>, total: u64) -> BTreeMap<String, u64> {
    let sum: u64 = weights.values().sum();
    let mut out: BTreeMap<String, u64> = weights.keys().map(|k| (k.clone(), 0)).collect();
    if sum == 0 {
        return out;
    }
    let mut rem: Vec<(u64, String)> = Vec::new();
    let mut given = 0;
    for (k, w) in weights {
        let x = u128::from(total) * u128::from(*w);
        let q = (x / u128::from(sum)) as u64;
        out.insert(k.clone(), q);
        given += q;
        rem.push(((x % u128::from(sum)) as u64, k.clone()));
    }
    rem.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (_, k) in rem.into_iter().take((total - given) as usize) {
        *out.get_mut(&k).unwrap() += 1;
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draw {
    pub rows: Vec<PoolRow>,
    pub groups: Vec<String>,
    pub quotas: BTreeMap<String, u64>,
    pub shortfall: u64,
    pub available_rows: u64,
}

/// Draws `want` human rows as whole source groups, skipping every used row and group.
pub fn draw(
    manifest: &PoolManifest,
    used_rows: &BTreeSet<String>,
    used_groups: &BTreeSet<String>,
    want: u64,
    seed: &[u8; 32],
) -> Draw {
    let mut groups: BTreeMap<String, Vec<&PoolRow>> = BTreeMap::new();
    for r in &manifest.rows {
        groups.entry(r.source_group.clone()).or_default().push(r);
    }
    // A group is available only if no row of it was ever used.
    groups.retain(|g, rows| !used_groups.contains(g) && rows.iter().all(|r| !used_rows.contains(&r.row_id)));
    let mut by_domain: BTreeMap<String, Vec<(&String, &Vec<&PoolRow>)>> = BTreeMap::new();
    for (g, rows) in &groups {
        // A group spanning domains is assigned to its first row's domain.
        by_domain.entry(rows[0].domain.clone()).or_default().push((g, rows));
    }
    let weights: BTreeMap<String, u64> =
        by_domain.iter().map(|(d, gs)| (d.clone(), gs.iter().map(|(_, r)| r.len() as u64).sum())).collect();
    let available: u64 = weights.values().sum();
    let quotas = proportional(&weights, want.min(available));
    for gs in by_domain.values_mut() {
        gs.sort_by_key(|(g, _)| rank(seed, "group", g));
    }
    let mut taken: BTreeSet<&String> = BTreeSet::new();
    let mut rows: Vec<PoolRow> = Vec::new();
    for (d, gs) in &by_domain {
        let mut left = quotas[d];
        for (g, members) in gs {
            if left == 0 {
                break;
            }
            if members.len() as u64 <= left {
                left -= members.len() as u64;
                taken.insert(g);
                rows.extend(members.iter().map(|r| (*r).clone()));
            }
        }
    }
    // Fill any shortfall from the remaining groups of every domain, in rank order.
    let mut rest: Vec<(&String, &Vec<&PoolRow>)> =
        by_domain.values().flatten().filter(|(g, _)| !taken.contains(g)).copied().collect();
    rest.sort_by_key(|(g, _)| rank(seed, "group", g));
    let mut short = want.min(available).saturating_sub(rows.len() as u64);
    for (g, members) in rest {
        if short == 0 {
            break;
        }
        if members.len() as u64 <= short {
            short -= members.len() as u64;
            taken.insert(g);
            rows.extend(members.iter().map(|r| (*r).clone()));
        }
    }
    rows.sort_by_key(|r| rank(seed, "row", &r.row_id));
    Draw {
        groups: taken.into_iter().cloned().collect(),
        shortfall: want - rows.len() as u64,
        rows,
        quotas,
        available_rows: available,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn manifest(groups: &[(&str, &str, usize)]) -> PoolManifest {
        let mut rows = Vec::new();
        for (d, g, n) in groups {
            for i in 0..*n {
                rows.push(PoolRow {
                    row_id: format!("{g}-{i}"),
                    text_sha256: hex::encode(Sha256::digest(format!("{g}-{i}"))),
                    domain: (*d).into(),
                    source_group: (*g).into(),
                    shard: 0,
                });
            }
        }
        PoolManifest {
            schema: POOL_SCHEMA.into(),
            pool_id: "p".into(),
            shards: vec![ShardInfo { index: 0, file: "shard-0000.jsonl".into(), rows: rows.len() as u32, sha256: String::new() }],
            rows,
            selection: None,
        }
    }

    #[test]
    fn draws_whole_groups_proportionally_and_never_reuses() {
        let mut spec = Vec::new();
        let names: Vec<String> = (0..60).map(|i| format!("g{i}")).collect();
        for (i, g) in names.iter().enumerate() {
            spec.push((if i < 40 { "wiki" } else { "news" }, g.as_str(), if i % 5 == 0 { 2 } else { 1 }));
        }
        let m = manifest(&spec);
        let seed = [9u8; 32];
        let first = draw(&m, &BTreeSet::new(), &BTreeSet::new(), 30, &seed);
        assert_eq!(first.rows.len(), 30);
        assert_eq!(first.shortfall, 0);
        assert_eq!(first.quotas["wiki"] + first.quotas["news"], 30);
        assert_eq!(first.quotas["wiki"], 20, "48 of 72 rows are wiki");
        // Whole groups only.
        for g in &first.groups {
            let all = m.rows.iter().filter(|r| &r.source_group == g).count();
            assert_eq!(first.rows.iter().filter(|r| &r.source_group == g).count(), all);
        }
        let used_rows: BTreeSet<String> = first.rows.iter().map(|r| r.row_id.clone()).collect();
        let used_groups: BTreeSet<String> = first.groups.iter().cloned().collect();
        let second = draw(&m, &used_rows, &used_groups, 100, &seed);
        assert!(second.rows.iter().all(|r| !used_rows.contains(&r.row_id)));
        assert_eq!(second.rows.len() as u64 + second.shortfall, 100);
        assert_eq!(second.rows.len(), 72 - 30);
        let third = draw(&m, &used_rows.union(&second.rows.iter().map(|r| r.row_id.clone()).collect()).cloned().collect(),
            &used_groups.union(&second.groups.iter().cloned().collect()).cloned().collect(), 5, &seed);
        assert!(third.rows.is_empty() && third.shortfall == 5, "an exhausted pool draws nothing");
        assert_eq!(draw(&m, &BTreeSet::new(), &BTreeSet::new(), 30, &seed), first, "deterministic");
    }

    #[test]
    fn the_ledger_keeps_humans_used_when_the_authority_starts_empty() {
        let names: Vec<String> = (0..20).map(|i| format!("g{i}")).collect();
        let spec: Vec<(&str, &str, usize)> = names.iter().map(|g| ("wiki", g.as_str(), 1)).collect();
        let m = manifest(&spec);
        let seed = [3u8; 32];
        let first = draw(&m, &BTreeSet::new(), &BTreeSet::new(), 8, &seed);
        let entry = ledger_entry("p", 1, "c1", "authority-a", "aa", &first);
        let bytes = serde_json::to_vec(&entry).unwrap();
        let parsed = LedgerEntry::parse(&bytes, "p").unwrap();
        assert_eq!(parsed, entry);
        assert!(LedgerEntry::parse(&bytes, "other-pool").is_err());
        // A v1 entry (no authority) always counts; a v2 entry must name one.
        let mut v1 = serde_json::to_value(&entry).unwrap();
        v1["schema"] = LEDGER_SCHEMA_V1.into();
        v1.as_object_mut().unwrap().remove("authority");
        let v1 = LedgerEntry::parse(&serde_json::to_vec(&v1).unwrap(), "p").unwrap();
        let mut v2_bad = serde_json::to_value(&entry).unwrap();
        v2_bad.as_object_mut().unwrap().remove("authority");
        assert!(LedgerEntry::parse(&serde_json::to_vec(&v2_bad).unwrap(), "p").is_err());

        // A new authority (empty Durable Object set) under the same label still avoids them.
        for e in [&parsed, &v1] {
            let mut used = Used::default();
            used.extend_from_ledger(std::slice::from_ref(e), "authority-b", "c1");
            let again = draw(&m, &used.row_ids(&m), &used.groups, 8, &seed);
            assert!(again.rows.iter().all(|r| !used.contains(r)));
            assert!(again.rows.iter().all(|r| !first.rows.contains(r)), "never reused");
        }
        // Only this authority's own write for this label is redrawn.
        let mut own = Used::default();
        own.extend_from_ledger(&[parsed], "authority-a", "c1");
        assert_eq!(own, Used::default());
        assert_eq!(draw(&m, &own.row_ids(&m), &own.groups, 8, &seed), first);
        // A text digest match excludes a row under a new id.
        let mut by_digest = Used::default();
        by_digest.digests.insert(m.rows[0].text_sha256.clone());
        assert!(by_digest.row_ids(&m).contains(&m.rows[0].row_id));
    }

    #[test]
    fn proportional_split_sums_exactly() {
        let w: BTreeMap<String, u64> = [("a", 160), ("b", 85), ("c", 85), ("d", 75), ("e", 75), ("f", 65), ("g", 25)]
            .iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let q = proportional(&w, 400);
        assert_eq!(q.values().sum::<u64>(), 400);
        assert_eq!(q["a"], 112);
    }
}

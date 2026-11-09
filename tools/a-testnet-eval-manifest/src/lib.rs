//! Testnet plumbing evaluation manifests from allow-listed Dev-iter rows.
//!
//! Testnet-only shortcut (`testnet_plumbing_only`): testnet cohorts are scored on a
//! rotating sample of Dev-iter, an iteration split exposed by design. The scores test
//! the evaluation plumbing and are not evidence of model quality. Removed by mainnet
//! hidden evaluation on fresh, unexposed cohorts.
//!
//! Rows are read only from files named in a tracked allow-list by exact repository
//! path and SHA-256. Dev-select, CAL, Test and everything under Tier C are refused by
//! path before any file is opened, again over the whole canonical path after symlinks
//! are resolved (`data/` itself must be a real directory), and by the bundle manifest
//! and every row's split label. The manifest holds row ids, text digests and labels,
//! never text, and is written only under `data/`.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

pub const ALLOWLIST_SCHEMA: &str = "slop_ninja_testnet_plumbing_eval_allowlist_v1";
pub const MANIFEST_SCHEMA: &str = "slop_ninja_testnet_plumbing_eval_manifest_v1";
pub const PURPOSE: &str = "testnet_plumbing_only";
pub const ALLOWED_SPLIT: &str = "dev_iter";
pub const ROW_SCHEMA: &str = "slop_ninja_a_bundle_row_v1";
pub const BUNDLE_MANIFEST_SCHEMA: &str = "slop_ninja_a_bundle_manifest_v1";
pub const SELECTION_RULE: &str = "round_robin_strata_v1";
const SELECTION_DOMAIN: &[u8] = b"slop_ninja/testnet_plumbing_eval/selection/v1\0";
const ORIGINS: [&str; 3] = ["human_only", "model_only", "mixed"];
/// Largest rows file read into memory.
const MAX_ROWS_BYTES: u64 = 256 << 20;
/// The tracked allow-list.
pub const TRACKED_ALLOWLIST: &str = include_str!("../allowlists/testnet-plumbing-dev-iter-v1.json");

/// Path tokens that name a sealed, single-use or calibration split. Each path
/// component is split into lowercase alphanumeric tokens; any of these refuses the path,
/// as does the token pair `tier`, `c` within a component or across adjacent ones.
const REFUSED_TOKENS: [&str; 8] = ["cal", "calibration", "test", "select", "sealed", "unsealed", "tierc", "devselect"];
/// Substrings of a component with its separators removed that refuse the path.
const REFUSED_COMPACT: [&str; 4] = ["tierc", "devselect", "evalhumanstest", "calsubsample"];

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Allowlist {
    pub schema: String,
    pub allowlist_id: String,
    pub purpose: String,
    pub removed_by: String,
    pub sources: Vec<Source>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub path: String,
    pub sha256: String,
    pub rows: usize,
    pub split: String,
    pub populations: Vec<String>,
    pub bundle_manifest_path: String,
    pub bundle_manifest_sha256: String,
}

#[derive(Clone, Debug)]
pub struct Selection {
    pub seed: [u8; 32],
    pub cohort: u64,
    pub per_stratum: usize,
    pub max_per_source_group: usize,
}

#[derive(Deserialize)]
struct Row {
    schema: String,
    id: String,
    split: String,
    text: String,
    text_sha256: String,
    origin: String,
    share: f64,
    domain: String,
    source_group: String,
    generator_label: Option<String>,
    population: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Selected {
    pub source: usize,
    pub line: usize,
    pub id: String,
    pub text_sha256: String,
    pub origin: String,
    pub share: f64,
    pub domain: String,
    pub generator_label: Option<String>,
    pub source_group: String,
    #[serde(skip)]
    rank: [u8; 32],
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(crate) fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Why a path with these components is refused by name, if it is.
pub(crate) fn component_refusal(comps: &[&str]) -> Option<String> {
    let mut after_tier = false;
    for c in comps {
        let lower = c.to_ascii_lowercase();
        let tokens: Vec<&str> = lower.split(|ch: char| !ch.is_ascii_alphanumeric()).filter(|t| !t.is_empty()).collect();
        if let Some(t) = tokens.iter().find(|t| REFUSED_TOKENS.contains(t)) {
            return Some(format!("names a refused split or area ({t:?} in {c:?})"));
        }
        if (after_tier && tokens.first() == Some(&"c")) || tokens.windows(2).any(|w| w == ["tier", "c"]) {
            return Some(format!("is under Tier C ({c:?})"));
        }
        let compact: String = tokens.concat();
        if let Some(t) = REFUSED_COMPACT.iter().find(|t| compact.contains(**t)) {
            return Some(format!("names a refused split or area ({t:?} in {c:?})"));
        }
        after_tier = tokens.last() == Some(&"tier");
    }
    None
}

/// Why a repository-relative path is refused, if it is.
pub fn refusal(rel: &str) -> Option<String> {
    if rel.is_empty() || rel.starts_with('/') || rel.contains('\\') {
        return Some(format!("{rel:?} is not a relative repository path"));
    }
    let comps: Vec<&str> = rel.split('/').collect();
    if comps.iter().any(|c| c.is_empty() || *c == "." || *c == "..") {
        return Some(format!("{rel:?} is not a normalized path"));
    }
    if comps[0] != "data" || comps.len() < 2 {
        return Some(format!("{rel:?} is not under data/"));
    }
    component_refusal(&comps).map(|why| format!("{rel:?} {why}"))
}

pub(crate) fn check_rel(rel: &str) -> Result<()> {
    match refusal(rel) {
        Some(why) => bail!("refused: {why}"),
        None => Ok(()),
    }
}

/// The canonical `data/` directory of the repository. The canonical repository root
/// must pass the name refusals, and `data` must be a real directory, not a symlink,
/// so every path under it is checked from the repository root down.
pub(crate) fn data_dir(repo: &Path) -> Result<PathBuf> {
    let root = repo.canonicalize().context("repository root")?;
    let comps: Vec<&str> = root
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_str().context("non-UTF-8 repository path")),
            _ => None,
        })
        .collect::<Result<_>>()?;
    if let Some(why) = component_refusal(&comps) {
        bail!("refused: repository root {} {why}", root.display());
    }
    let data = root.join("data");
    let meta = fs::symlink_metadata(&data).context("repository data/ directory")?;
    ensure!(!meta.file_type().is_symlink(), "refused: repository data/ is a symlink");
    ensure!(meta.is_dir(), "repository data/ is not a directory");
    ensure!(data.canonicalize()? == data, "refused: repository data/ does not resolve to itself");
    Ok(data)
}

/// The repository-relative form of a canonical path under the canonical `data/`.
pub(crate) fn rel_under(data: &Path, real: &Path) -> Result<String> {
    let inside = real.strip_prefix(data).map_err(|_| anyhow::anyhow!("refused: {} is outside data/", real.display()))?;
    let mut parts = vec!["data".to_string()];
    for c in inside.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str().context("non-UTF-8 path")?.to_string()),
            _ => bail!("refused: {} is an unusual path", real.display()),
        }
    }
    Ok(parts.join("/"))
}

/// Resolve an allow-listed path, refusing it lexically first and again after
/// symlinks are resolved. Nothing is opened before both checks pass.
pub(crate) fn resolve(repo: &Path, rel: &str) -> Result<PathBuf> {
    check_rel(rel)?;
    let data = data_dir(repo)?;
    let real = data.parent().unwrap().join(rel).canonicalize().with_context(|| format!("resolve {rel}"))?;
    let resolved = rel_under(&data, &real).with_context(|| format!("{rel} resolves outside data/"))?;
    check_rel(&resolved).with_context(|| format!("{rel} resolves to {resolved}"))?;
    ensure!(real.is_file(), "{rel} is not a regular file");
    Ok(real)
}

impl Allowlist {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let a: Self = serde_json::from_slice(bytes).context("allow-list JSON")?;
        ensure!(a.schema == ALLOWLIST_SCHEMA, "allow-list schema {}", a.schema);
        ensure!(a.purpose == PURPOSE, "allow-list purpose must be {PURPOSE}");
        ensure!(!a.allowlist_id.is_empty() && !a.removed_by.is_empty(), "allow-list id and removal milestone");
        ensure!(!a.sources.is_empty(), "empty allow-list");
        let mut paths = BTreeSet::new();
        for s in &a.sources {
            ensure!(s.split == ALLOWED_SPLIT, "refused: allow-listed split {:?}; only {ALLOWED_SPLIT}", s.split);
            check_rel(&s.path)?;
            check_rel(&s.bundle_manifest_path)?;
            ensure!(paths.insert(s.path.as_str()), "duplicate allow-listed path {}", s.path);
            ensure!(is_digest(&s.sha256) && is_digest(&s.bundle_manifest_sha256), "{}: digest format", s.path);
            let parent = |p: &str| p.rsplit_once('/').map(|x| x.0.to_string());
            ensure!(
                parent(&s.path) == parent(&s.bundle_manifest_path),
                "{}: the bundle manifest must sit beside the rows",
                s.path
            );
            ensure!(s.rows > 0 && !s.populations.is_empty(), "{}: row count and populations", s.path);
        }
        Ok(a)
    }
}

/// Validate one allow-listed source and return its usable rows with their file lines.
fn load_source(repo: &Path, s: &Source) -> Result<(Vec<(usize, Row)>, usize)> {
    let rows_path = resolve(repo, &s.path)?;
    let manifest_path = resolve(repo, &s.bundle_manifest_path)?;
    let dir = rows_path.parent().context("rows directory")?;
    ensure!(manifest_path.parent() == Some(dir), "refused: {} resolves away from its bundle manifest", s.path);
    for e in fs::read_dir(dir)? {
        let name = e?.file_name().to_string_lossy().to_ascii_uppercase();
        ensure!(
            !(name.starts_with("SEALED") || name.starts_with("UNSEALED")),
            "refused: {} sits beside a seal record ({name})",
            s.path
        );
    }

    let mb = fs::read(&manifest_path)?;
    ensure!(sha256_hex(&mb) == s.bundle_manifest_sha256, "refused: {} digest differs", s.bundle_manifest_path);
    let m: Value = serde_json::from_slice(&mb).context("bundle manifest JSON")?;
    ensure!(m["schema"] == BUNDLE_MANIFEST_SCHEMA, "bundle manifest schema");
    ensure!(
        m["split"] == ALLOWED_SPLIT && m["sealed"] == false && m["test_opened"] == false,
        "refused: bundle manifest is not an unsealed {ALLOWED_SPLIT} split with Test unopened"
    );
    ensure!(m["rows"] == s.rows && m["files"]["rows.jsonl"] == s.sha256.as_str(), "refused: bundle manifest rows differ from the allow-list");

    let len = fs::metadata(&rows_path)?.len();
    ensure!(len <= MAX_ROWS_BYTES, "{} is larger than {MAX_ROWS_BYTES} bytes", s.path);
    let bytes = fs::read(&rows_path)?;
    ensure!(sha256_hex(&bytes) == s.sha256, "refused: {} digest differs from the allow-list", s.path);
    let text = std::str::from_utf8(&bytes).context("rows are not UTF-8")?;
    let mut out = Vec::new();
    let mut ids = BTreeSet::new();
    let mut lines = 0;
    for (i, line) in text.lines().enumerate() {
        lines += 1;
        let r: Row = serde_json::from_str(line).with_context(|| format!("{} line {}", s.path, i + 1))?;
        ensure!(r.schema == ROW_SCHEMA, "{} line {}: row schema", s.path, i + 1);
        ensure!(r.split == ALLOWED_SPLIT, "refused: {} line {} has split {:?}", s.path, i + 1, r.split);
        ensure!(ids.insert(r.id.clone()), "{} line {}: duplicate id", s.path, i + 1);
        ensure!(sha256_hex(r.text.as_bytes()) == r.text_sha256, "{} line {}: text digest differs", s.path, i + 1);
        let labels_ok = match r.origin.as_str() {
            "human_only" => r.share == 0. && r.generator_label.is_none(),
            "model_only" => r.share == 1. && r.generator_label.is_some(),
            "mixed" => r.share > 0. && r.share <= 1. && r.generator_label.is_some(),
            _ => false,
        };
        ensure!(labels_ok, "{} line {}: inconsistent origin, share or generator label", s.path, i + 1);
        ensure!(!r.domain.is_empty() && !r.source_group.is_empty(), "{} line {}: domain and source group", s.path, i + 1);
        if s.populations.contains(&r.population) {
            out.push((i + 1, r));
        }
    }
    ensure!(lines == s.rows, "{}: {lines} rows, allow-list records {}", s.path, s.rows);
    Ok((out, lines))
}

fn rank(sel: &Selection, source: usize, id: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(SELECTION_DOMAIN);
    h.update(sel.seed);
    h.update(sel.cohort.to_le_bytes());
    h.update((source as u64).to_le_bytes());
    h.update(id.as_bytes());
    h.finalize().into()
}

/// Build a plumbing evaluation manifest from an allow-list. The same allow-list, rows,
/// seed and cohort always give the same manifest; another cohort rotates the sample.
pub fn build(repo: &Path, allowlist_bytes: &[u8], sel: &Selection) -> Result<Value> {
    ensure!(sel.per_stratum > 0 && sel.max_per_source_group > 0, "per-stratum and per-group limits must be positive");
    let allow = Allowlist::parse(allowlist_bytes)?;
    let mut strata: BTreeMap<(String, String), Vec<Selected>> = BTreeMap::new();
    let mut sources = Vec::new();
    for (si, s) in allow.sources.iter().enumerate() {
        let (rows, total) = load_source(repo, s)?;
        sources.push(json!({"path": s.path, "sha256": s.sha256, "rows": total, "split": s.split,
            "populations": s.populations, "eligible_rows": rows.len(),
            "bundle_manifest_sha256": s.bundle_manifest_sha256}));
        for (line, r) in rows {
            strata.entry((r.domain.clone(), r.origin.clone())).or_default().push(Selected {
                source: si,
                line,
                rank: rank(sel, si, &r.id),
                id: r.id,
                text_sha256: r.text_sha256,
                origin: r.origin,
                share: r.share,
                domain: r.domain,
                generator_label: r.generator_label,
                source_group: r.source_group,
            });
        }
    }
    for rows in strata.values_mut() {
        rows.sort_by_key(|r| r.rank);
    }

    // Round robin over strata in key order: each round, every stratum takes its next
    // candidate whose source group is still under the per-cohort limit.
    let mut cursor: BTreeMap<&(String, String), usize> = strata.keys().map(|k| (k, 0)).collect();
    let mut taken: BTreeMap<&(String, String), Vec<&Selected>> = BTreeMap::new();
    let mut groups: BTreeMap<(usize, &str), usize> = BTreeMap::new();
    for _ in 0..sel.per_stratum {
        for (key, rows) in &strata {
            let c = cursor.get_mut(key).unwrap();
            while *c < rows.len() {
                let r = &rows[*c];
                *c += 1;
                let g = groups.entry((r.source, r.source_group.as_str())).or_default();
                if *g < sel.max_per_source_group {
                    *g += 1;
                    taken.entry(key).or_default().push(r);
                    break;
                }
            }
        }
    }
    let mut selected: Vec<&Selected> = taken.values().flatten().copied().collect();
    selected.sort_by_key(|r| (r.source, r.line));
    let strata_out: Vec<Value> = strata
        .iter()
        .map(|(k, rows)| {
            let n = taken.get(k).map_or(0, Vec::len);
            json!({"domain": k.0, "origin": k.1, "available": rows.len(), "selected": n,
                "shortfall": sel.per_stratum - n})
        })
        .collect();
    let mut by_origin: BTreeMap<&str, usize> = ORIGINS.iter().map(|o| (*o, 0)).collect();
    for r in &selected {
        *by_origin.get_mut(r.origin.as_str()).unwrap() += 1;
    }
    let rows_json = serde_json::to_value(&selected)?;
    let rows_sha = sha256_hex(serde_json::to_string(&rows_json)?.as_bytes());
    Ok(json!({
        "schema": MANIFEST_SCHEMA,
        "purpose": PURPOSE,
        "evidence": "none: Dev-iter is an exposed iteration split; these rows test evaluation plumbing only",
        "allowlist": {"id": allow.allowlist_id, "sha256": sha256_hex(allowlist_bytes), "removed_by": allow.removed_by},
        "sources": sources,
        "selection": {"rule": SELECTION_RULE, "seed_sha256": sha256_hex(&sel.seed), "cohort": sel.cohort,
            "per_stratum": sel.per_stratum, "max_per_source_group": sel.max_per_source_group},
        "strata": strata_out,
        "counts": {"selected": selected.len(), "by_origin": by_origin,
            "source_groups": groups.len()},
        "rows_sha256": rows_sha,
        "rows": rows_json,
    }))
}

pub const POOL_SCHEMA: &str = "slop_ninja_testnet_human_pool_v1";
pub const POOL_SHARD_SCHEMA: &str = "slop_ninja_testnet_human_pool_row_v1";

/// Export the allow-listed `human_only` rows as a private, never-reused testnet human
/// pool: JSONL shards of at most `shard_rows` rows (with text) and a content-free
/// manifest (row ids, text digests, domains, source groups and shard digests). Every
/// refusal of [`build`] applies. Files go to a new directory under `data/`; nothing is
/// uploaded here. Returns the manifest.
pub fn export_human_pool(repo: &Path, allowlist_bytes: &[u8], pool_id: &str, shard_rows: usize, out: &Path) -> Result<Value> {
    ensure!((1..=4096).contains(&shard_rows), "shard rows must be 1..=4096");
    ensure!(
        !pool_id.is_empty() && pool_id.len() <= 64 && pool_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "pool id must be 1-64 ASCII letters, digits or hyphens"
    );
    let allow = Allowlist::parse(allowlist_bytes)?;
    let mut rows = Vec::new();
    let mut sources = Vec::new();
    for s in &allow.sources {
        let (eligible, total) = load_source(repo, s)?;
        let humans: Vec<Row> = eligible.into_iter().map(|(_, r)| r).filter(|r| r.origin == "human_only").collect();
        sources.push(json!({"path": s.path, "sha256": s.sha256, "rows": total, "split": s.split,
            "populations": s.populations, "human_rows": humans.len(),
            "bundle_manifest_sha256": s.bundle_manifest_sha256}));
        rows.extend(humans);
    }
    ensure!(!rows.is_empty(), "no human_only rows in the allow-listed populations");
    let mut seen = BTreeSet::new();
    for r in &rows {
        ensure!(seen.insert(r.text_sha256.clone()), "two pool rows share a text digest");
    }
    rows.sort_by(|a, b| (&a.domain, &a.source_group, &a.id).cmp(&(&b.domain, &b.source_group, &b.id)));

    // The output directory must be new and under data/.
    let name = out.file_name().and_then(|n| n.to_str()).context("output directory name")?;
    let parent = out.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let parent = parent.canonicalize().context("output parent must exist")?;
    let data = data_dir(repo)?;
    let rel = rel_under(&data, &parent.join(name)).context("refused: pools are written only under data/")?;
    check_rel(&rel)?;
    let dir = parent.join(name);
    fs::create_dir(&dir).with_context(|| format!("create {rel} (existing pools are never replaced)"))?;

    let mut shards = Vec::new();
    let mut listed = Vec::new();
    let mut by_domain: BTreeMap<&str, usize> = BTreeMap::new();
    for (k, chunk) in rows.chunks(shard_rows).enumerate() {
        let file = format!("shard-{k:04}.jsonl");
        let mut body = String::new();
        for r in chunk {
            body.push_str(&serde_json::to_string(&json!({"schema": POOL_SHARD_SCHEMA, "row_id": r.id,
                "text_sha256": r.text_sha256, "domain": r.domain, "source_group": r.source_group,
                "text": r.text}))?);
            body.push('\n');
            listed.push(json!({"row_id": r.id, "text_sha256": r.text_sha256, "domain": r.domain,
                "source_group": r.source_group, "shard": k}));
            *by_domain.entry(r.domain.as_str()).or_default() += 1;
        }
        let mut f = fs::OpenOptions::new().write(true).create_new(true).open(dir.join(&file))?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        shards.push(json!({"index": k, "file": file, "rows": chunk.len(), "bytes": body.len(),
            "sha256": sha256_hex(body.as_bytes())}));
    }
    let groups: BTreeSet<&str> = rows.iter().map(|r| r.source_group.as_str()).collect();
    let manifest = json!({
        "schema": POOL_SCHEMA,
        "purpose": PURPOSE,
        "pool_id": pool_id,
        "evidence": "none: Dev-iter is an exposed iteration split; this pool tests evaluation plumbing only",
        "reuse": "never: every row and source group is drawn at most once across all cohorts",
        "allowlist": {"id": allow.allowlist_id, "sha256": sha256_hex(allowlist_bytes), "removed_by": allow.removed_by},
        "sources": sources,
        "shard_rows": shard_rows,
        "shards": shards,
        "counts": {"rows": rows.len(), "source_groups": groups.len(), "by_domain": by_domain},
        "rows": listed,
    });
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(dir.join("manifest.json"))?;
    f.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(manifest)
}

/// Write a manifest to a new file under the repository's `data/` directory.
pub fn write_manifest(repo: &Path, out: &Path, manifest: &Value) -> Result<PathBuf> {
    let name = out.file_name().and_then(|n| n.to_str()).context("output file name")?;
    let parent = out.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let parent = parent.canonicalize().context("output directory must exist")?;
    let data = data_dir(repo)?;
    let rel = rel_under(&data, &parent.join(name)).context("refused: manifests are written only under data/")?;
    check_rel(&rel)?;
    let path = parent.join(name);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("create {rel} (existing manifests are never replaced)"))?;
    f.write_all(serde_json::to_string_pretty(manifest)?.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(path)
}

pub mod capacity;
pub mod carve_v3;
pub mod engine_v1;
pub mod gate;
pub mod pool_v2;
pub mod public_candidate;
pub mod replay;
pub mod stock;
pub mod token_bound;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_v2;
#[cfg(test)]
mod tests_carve;

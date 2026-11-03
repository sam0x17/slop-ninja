//! Bundle build: configuration, input reading, output layout.
//!
//! `build` is deterministic and re-runnable. The build id is a digest of the
//! configuration and every input's SHA-256, so an unchanged input set maps to
//! the same directory; new positives give a new build, and `CURRENT.json` in
//! the output root names the latest complete one.
use crate::*;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

pub const CONFIG_SCHEMA: &str = "slop_ninja_a_assemble_config_v1";
pub const MANIFEST_SCHEMA: &str = "slop_ninja_a_bundle_manifest_v1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitInputs {
    /// Human documents of the split (split/pool document schema).
    pub humans: Vec<PathBuf>,
    /// Optional id -> evaluation domain manifests (rows with `id` and `domain`).
    #[serde(default)]
    pub domain_manifests: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema: String,
    /// Splits to build, in order; `dev_select` and `cal` are written sealed.
    pub splits: BTreeMap<String, SplitInputs>,
    #[serde(default)]
    pub sealed: Vec<String>,
    /// Train-side manifests whose ids may parent Train positives (Train-mining).
    #[serde(default)]
    pub train_side_manifests: Vec<PathBuf>,
    /// Local positive record files (any split; rows of unbuilt splits are ignored).
    #[serde(default)]
    pub local_positives: Vec<PathBuf>,
    /// Frontier export directories (`<label>/records.jsonl`, v2 records).
    #[serde(default)]
    pub frontier_exports: Vec<PathBuf>,
    /// Admitted external Train pools, filtered and sampled at read time.
    #[serde(default)]
    pub external_train_pools: Vec<ExternalPool>,
    /// Stage-3 legacy Train rows (existing-data full-A `train.jsonl`).
    #[serde(default)]
    pub legacy_train: Option<PathBuf>,
    /// Legacy generator repository -> family id.
    #[serde(default)]
    pub legacy_families: BTreeMap<String, String>,
    /// Frozen style partitions (`freeze-styles`); the only source of style partitions.
    pub frozen_partitions: PathBuf,
    /// The earlier hosted captures admitted by `docs/a-hosted-output-admission.json`.
    #[serde(default)]
    pub hosted_captures: Option<HostedCaptures>,
    /// Reservation manifests of every evaluation split, by split (metadata only).
    #[serde(default)]
    pub evaluation_manifests: BTreeMap<String, Vec<PathBuf>>,
    #[serde(default)]
    pub collection_domains: BTreeMap<String, String>,
    #[serde(default)]
    pub cohort_weights: CohortWeights,
    /// Train origin shares and label-balance fit controls.
    #[serde(default)]
    pub weighting: Weighting,
    /// Train-side source-group manifests (rows with `source_group`) that close
    /// the evaluation splits when Train itself is not built in this config.
    #[serde(default)]
    pub train_group_manifests: Vec<PathBuf>,
    /// Diagnostic human slices reported with the Test split (protocol `diagnostic_slices`).
    #[serde(default)]
    pub diagnostic_humans: Vec<DiagnosticHumans>,
    /// Overlap-audit receipts (`slop_ninja_a_test_overlap_exclusion_v1` rows with
    /// `source_group`): whole source groups excluded from the evaluation splits.
    #[serde(default)]
    pub excluded_source_groups: Vec<PathBuf>,
    /// Round-2 options (protocol A2 §6.3–§6.4, amendment 5). Absent, or with
    /// every option off, the build's bytes are exactly those of a config without it.
    #[serde(default)]
    pub round2: Option<Round2>,
}

/// Round-2 assembler options. With any option on, the build is a round-2
/// build: its Train bundle is refused when its Kish ESS under the trainer's
/// weighting falls below R0's 32,827 (V24; amendment 5 item 5.1, with no
/// switch), its rows carry `ROW_SCHEMA_R2` and its Train weights the scheme
/// `TRAIN_SCHEME_R2` (amendment 5 item 5.4).
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Round2 {
    /// `a_covariate_balance_v1`: rake the Train AI-involved cells to the
    /// domain's human word-count bins (S0-14).
    #[serde(default)]
    pub covariate_balance: Option<String>,
    /// The round-2 cohorts reported in the Train `weights.json`.
    #[serde(default)]
    pub cohort_roles: Vec<CohortRole>,
    /// Tag every row with its block (`T` or `C`).
    #[serde(default)]
    pub block_tags: Option<BlockTags>,
    /// A `renorm-rows` output directory: the RAID, stage-3 legacy and
    /// hosted-capture Train rows it lists take their laid-out text, and the
    /// planned rows it leaves out are dropped with their receipts
    /// (`rows_renorm`; amendment 20 item 20.7, ruling R3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows_renorm: Option<PathBuf>,
}

impl Round2 {
    /// True when any option is on (only then does the build change).
    pub fn on(&self) -> bool {
        self.covariate_balance.is_some() || !self.cohort_roles.is_empty() || self.block_tags.is_some() || self.rows_renorm.is_some()
    }
}

/// A round-2 cohort role: `enron_train_half`, `base_model` or `cc_n2`. A Train
/// row belongs to it when its cohort id starts with one of the prefixes or
/// its collection is listed; a row may belong to one role only.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CohortRole {
    pub role: String,
    #[serde(default)]
    pub cohort_prefixes: Vec<String>,
    #[serde(default)]
    pub collections: Vec<String>,
    /// No declared cohort factor and no declared label factor other than 1,
    /// hosted cohorts included (always so for `cc_n2`).
    #[serde(default)]
    pub self_weighting: bool,
    /// The writer keys the role's rows may carry (required for `enron_train_half`).
    #[serde(default)]
    pub writer_keys: Option<WriterKeys>,
}

/// A JSON file holding an array of writer keys, at `pointer` (RFC 6901) if given.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriterKeys {
    pub file: PathBuf,
    #[serde(default)]
    pub pointer: Option<String>,
}

/// Block tags (protocol A2: T = Tier-domain populations, C = Common Crawl
/// populations). A row is `C` when its cohort id starts with a listed prefix,
/// its collection is listed, or its parent is a `C` human of the same split;
/// every other row is `T`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BlockTags {
    #[serde(default)]
    pub c_cohort_prefixes: Vec<String>,
    #[serde(default)]
    pub c_collections: Vec<String>,
}

/// Set `block` on every row (see `BlockTags`).
pub fn tag_blocks(rows: &mut [Row], bt: &BlockTags) {
    let is_c = |r: &Row| {
        bt.c_cohort_prefixes.iter().any(|p| r.cohort_id.starts_with(p.as_str()))
            || r.collection.as_ref().is_some_and(|c| bt.c_collections.contains(c))
    };
    let c_humans: HashSet<String> = rows.iter().filter(|r| r.source_kind == SourceKind::Human && is_c(r)).map(|r| r.id.clone()).collect();
    for r in rows.iter_mut() {
        let c = is_c(r) || r.parent_id.as_ref().is_some_and(|p| c_humans.contains(p));
        r.block = Some(if c { "C" } else { "T" }.into());
    }
}

/// Validate the round-2 block and read its writer-key allow-lists (recorded as inputs).
fn round2_inputs(r2: &Round2, inputs: &mut Inputs) -> Result<Vec<Option<HashSet<String>>>> {
    weights::check_roles(r2)?;
    if let Some(rule) = &r2.covariate_balance {
        ensure!(rule == covariate::COVARIATE_RULE, "unknown covariate balance rule {rule}");
    }
    if let Some(bt) = &r2.block_tags {
        ensure!(!bt.c_cohort_prefixes.is_empty() || !bt.c_collections.is_empty(), "block tags name no C cohort or collection");
        ensure!(bt.c_cohort_prefixes.iter().all(|p| !p.is_empty()), "block tags: empty C cohort prefix");
    }
    let mut out = Vec::new();
    for role in &r2.cohort_roles {
        out.push(match &role.writer_keys {
            None => None,
            Some(wk) => {
                let v = inputs.json(&wk.file, "round2_writer_keys")?;
                let list = match &wk.pointer {
                    Some(p) => v.pointer(p).with_context(|| format!("{}: no value at {p}", wk.file.display()))?,
                    None => &v,
                };
                let arr = list.as_array().with_context(|| format!("{}: writer keys must be an array", wk.file.display()))?;
                let set: HashSet<String> = arr.iter()
                    .map(|x| x.as_str().filter(|s| !s.is_empty()).map(String::from).context("writer keys must be non-empty strings"))
                    .collect::<Result<_>>()?;
                ensure!(!set.is_empty() && set.len() == arr.len(), "{}: writer keys must be distinct and non-empty", wk.file.display());
                Some(set)
            }
        });
    }
    Ok(out)
}

/// One diagnostic slice: its rendered human documents (`diagnostic_<name>` frozen split).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticHumans {
    pub name: String,
    pub documents: PathBuf,
    /// The evaluation split it is reported with (Test).
    pub split: String,
}

/// The written seal that opens Test: its path, the sha256 the caller states,
/// and the assembler binary digest the seal must bind.
#[derive(Clone, Debug)]
pub struct SealCheck {
    pub path: PathBuf,
    pub sha256: String,
}

impl SealCheck {
    /// Refuse unless the seal exists, hashes to the stated digest, says
    /// `"sealed": true` and binds `binary_sha256` (the running assembler).
    pub fn verify(&self, binary_sha256: &str) -> Result<Value> {
        ensure!(self.path.is_file(), "the seal {} does not exist: Test is sealed", self.path.display());
        let b = fs::read(&self.path)?;
        let got = sha_hex(&b);
        ensure!(got == self.sha256, "seal {} hashes to {got}, not the stated {}", self.path.display(), self.sha256);
        let v: Value = serde_json::from_slice(&b).context("seal JSON")?;
        ensure!(v["sealed"] == true, "the seal does not say \"sealed\": true");
        ensure!(!binary_sha256.is_empty() && String::from_utf8_lossy(&b).contains(binary_sha256),
            "the seal does not bind this assembler binary ({binary_sha256})");
        Ok(json!({"path": self.path, "sha256": got, "binary_sha256_bound": binary_sha256}))
    }
}

/// An admitted external Train pool (`slop_ninja_a_pool_document_v1` rows).
/// A row is read only if its cohort is in `include_cohorts`, its
/// `format.repeated_word_8gram_share` is below `max_repeated_word_8gram_share`
/// (when declared), and it is sampled: rows of `keep_all_cohorts` always;
/// otherwise its source group must pass `group_per_mille` and, for model
/// rows, the row id must also pass `positive_per_mille`. Sampling uses
/// `per_mille(seed, key) < rate`. Every skipped row is counted by reason.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalPool {
    pub name: String,
    pub documents: PathBuf,
    pub include_cohorts: Vec<String>,
    #[serde(default)]
    pub keep_all_cohorts: Vec<String>,
    #[serde(default)]
    pub max_repeated_word_8gram_share: Option<f64>,
    pub group_per_mille: u64,
    pub positive_per_mille: u64,
    pub seed: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedCaptures {
    /// `docs/a-hosted-output-admission.json` (admits records by text hash).
    pub admission: PathBuf,
    /// Decoded CLI capture review files (`slop_ninja_cli_capture_review_v1`).
    #[serde(default)]
    pub reviews: Vec<PathBuf>,
    /// Hosted style export record files (`slop_ninja_a_supervised_record_v1`).
    #[serde(default)]
    pub style_records: Vec<PathBuf>,
}

pub struct Input {
    pub path: PathBuf,
    pub kind: &'static str,
    pub sha256: String,
    pub bytes: u64,
    pub rows: usize,
}

/// Inputs read so far; rows record their input index.
#[derive(Default)]
pub struct Inputs {
    pub list: Vec<Input>,
}

impl Inputs {
    /// Read a JSONL file, calling `f` with each line; records digest and counts.
    pub fn jsonl(
        &mut self,
        path: &Path,
        kind: &'static str,
        mut f: impl FnMut(usize, usize, &[u8], Value) -> Result<()>,
    ) -> Result<()> {
        let idx = self.list.len();
        let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mut rd = BufReader::with_capacity(1 << 20, file);
        let mut h = sha2::Sha256::new();
        let (mut n, mut bytes, mut line) = (0usize, 0u64, Vec::new());
        loop {
            line.clear();
            let k = rd.read_until(b'\n', &mut line)?;
            if k == 0 {
                break;
            }
            sha2::Digest::update(&mut h, &line);
            bytes += k as u64;
            let body = line.strip_suffix(b"\n").unwrap_or(&line);
            if body.is_empty() {
                continue;
            }
            n += 1;
            let v: Value = serde_json::from_slice(body)
                .with_context(|| format!("{} line {n}", path.display()))?;
            f(idx, n, body, v).with_context(|| format!("{} line {n}", path.display()))?;
        }
        self.list.push(Input {
            path: path.into(),
            kind,
            sha256: hex::encode(sha2::Digest::finalize(h)),
            bytes,
            rows: n,
        });
        Ok(())
    }
    pub fn json(&mut self, path: &Path, kind: &'static str) -> Result<Value> {
        let b = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        self.list.push(Input { path: path.into(), kind, sha256: sha_hex(&b), bytes: b.len() as u64, rows: 1 });
        Ok(serde_json::from_slice(&b)?)
    }
    pub fn report(&self) -> Value {
        self.list
            .iter()
            .enumerate()
            .map(|(i, x)| json!({"index": i, "path": x.path, "kind": x.kind, "sha256": x.sha256, "bytes": x.bytes, "rows": x.rows}))
            .collect()
    }
}

use sha2::Digest as _;

/// Everything read for one build, before selection.
pub struct Gathered {
    pub inputs: Inputs,
    pub candidates: BTreeMap<String, Vec<Row>>,
    pub parents: HashMap<String, Parent>,
    pub eval_keys: EvalKeys,
    pub eval_texts: HashMap<String, String>,
    pub train_families: HashSet<String>,
    pub train_side_groups: HashMap<String, String>,
    pub frozen_sha256: String,
    pub read_errors: Vec<Exclusion>,
    pub ignored_rows: BTreeMap<String, usize>,
    /// Source group -> overlap-audit receipt row (evaluation splits drop the whole group).
    pub excluded_groups: HashMap<String, Value>,
    /// Writer-key allow-lists of the round-2 cohort roles, in declaration order.
    pub round2_writer_keys: Vec<Option<HashSet<String>>>,
    /// The rows re-verdict applied to the Train candidates (`round2.rows_renorm`).
    pub rows_renorm: Option<rows_renorm::Applied>,
}

fn reject(read_errors: &mut Vec<Exclusion>, id: &str, split: &str, kind: SourceKind, e: anyhow::Error) {
    read_errors.push(Exclusion {
        id: id.into(),
        split: split.into(),
        source_kind: kind,
        reason: "invalid_record".into(),
        detail: json!(format!("{e:#}")),
    });
}

pub fn gather(cfg: &Config) -> Result<Gathered> {
    gather_sealed(cfg, false)
}

/// `test_opened`: the caller verified the written seal (`SealCheck::verify`);
/// only then may the config build the Test split.
pub fn gather_sealed(cfg: &Config, test_opened: bool) -> Result<Gathered> {
    ensure!(cfg.schema == CONFIG_SCHEMA, "config schema must be {CONFIG_SCHEMA}");
    for s in cfg.splits.keys() {
        ensure!(SPLITS.contains(&s.as_str()) || (s == TEST_SPLIT && test_opened),
            "unknown split {s} (Test is assembled only after the written seal, with --seal and --seal-sha256)");
    }
    for s in &cfg.sealed {
        ensure!(SPLITS.contains(&s.as_str()), "unknown sealed split {s}");
    }
    for d in &cfg.diagnostic_humans {
        ensure!(cfg.splits.contains_key(&d.split) && d.split != "train", "diagnostic slice {} needs a built evaluation split", d.name);
    }
    let mut inputs = Inputs::default();
    let styles = {
        let ix = FrozenIndex::read(&cfg.frozen_partitions)?;
        inputs.list.push(Input { path: cfg.frozen_partitions.clone(), kind: "frozen_style_partitions",
            sha256: ix.sha256.clone(), bytes: fs::metadata(&cfg.frozen_partitions)?.len(), rows: ix.items.len() });
        ix
    };
    let mut eval_keys = EvalKeys::default();
    let mut manifest_texts: HashMap<String, String> = HashMap::new();
    for (split, files) in &cfg.evaluation_manifests {
        let split = split_name(split);
        ensure!(split != "train", "evaluation manifests cannot be Train");
        for f in files {
            inputs.jsonl(f, "evaluation_manifest", |_, _, _, v| {
                eval_keys.add_manifest_row(&split, &v);
                // Document manifests (diagnostic slices) carry the text digest: Train never repeats it.
                if let Some(d) = v["text_sha256"].as_str() {
                    manifest_texts.entry(d.to_string()).or_insert_with(|| split.clone());
                }
                Ok(())
            })?;
        }
    }
    let mut candidates: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    let mut parents: HashMap<String, Parent> = HashMap::new();
    let mut eval_texts: HashMap<String, String> = manifest_texts;
    let mut train_families = HashSet::new();
    let mut read_errors = Vec::new();
    let mut ignored_rows: BTreeMap<String, usize> = BTreeMap::new();
    for f in &cfg.train_group_manifests {
        inputs.jsonl(f, "train_group_manifest", |_, _, _, v| {
            if let Some(g) = v["source_group"].as_str() {
                train_families.insert(g.to_string());
            }
            Ok(())
        })?;
    }
    for (split, si) in &cfg.splits {
        let mut domain_by_id = HashMap::new();
        for f in &si.domain_manifests {
            inputs.jsonl(f, "domain_manifest", |_, _, _, v| {
                if let (Some(i), Some(d)) = (v["id"].as_str(), v["domain"].as_str()) {
                    domain_by_id.insert(i.to_string(), d.to_string());
                }
                Ok(())
            })?;
        }
        let rows = candidates.entry(split.clone()).or_default();
        for f in &si.humans {
            inputs.jsonl(f, "humans", |input, line, body, v| {
                let r = human_row(&v, split, &domain_by_id, &cfg.collection_domains, &Origin { input, line, bytes: body })?;
                if split == "train" {
                    train_families.insert(r.source_group.clone());
                } else {
                    eval_keys.add_human(&r);
                    eval_texts.entry(r.text_sha256.clone()).or_insert_with(|| split.clone());
                }
                ensure!(parents.insert(r.id.clone(), Parent::of(&r)).is_none(), "human id {} twice", r.id);
                rows.push(r);
                Ok(())
            })?;
        }
    }
    for d in &cfg.diagnostic_humans {
        let rows = candidates.get_mut(&d.split).unwrap();
        inputs.jsonl(&d.documents, "diagnostic_humans", |input, line, body, v| {
            let r = diagnostic_row(&v, &d.name, &d.split, &Origin { input, line, bytes: body })?;
            eval_keys.add_human(&r);
            eval_texts.entry(r.text_sha256.clone()).or_insert_with(|| d.split.clone());
            rows.push(r);
            Ok(())
        })?;
    }
    let mut excluded_groups: HashMap<String, Value> = HashMap::new();
    for f in &cfg.excluded_source_groups {
        inputs.jsonl(f, "overlap_exclusion_receipt", |_, line, _, v| {
            ensure!(v["schema"] == "slop_ninja_a_test_overlap_exclusion_v1", "receipt line {line}: schema");
            let g = str_req(&v, "source_group")?;
            excluded_groups.entry(g.to_string()).or_insert(v.clone());
            Ok(())
        })?;
    }
    let built: HashSet<String> = cfg.splits.keys().cloned().collect();
    let push = |r: Row, candidates: &mut BTreeMap<String, Vec<Row>>, ignored: &mut BTreeMap<String, usize>| {
        if built.contains(&r.split) {
            candidates.get_mut(&r.split).unwrap().push(r);
        } else {
            *ignored.entry(format!("{:?}:{}", r.source_kind, r.split)).or_default() += 1;
        }
    };
    for f in &cfg.local_positives {
        inputs.jsonl(f, "local_positives", |input, line, body, v| {
            match local_row(&v, &styles, &Origin { input, line, bytes: body }) {
                Ok(r) => push(r, &mut candidates, &mut ignored_rows),
                Err(e) => reject(&mut read_errors, v["id"].as_str().unwrap_or("?"),
                    &split_name(v["frozen_split"].as_str().unwrap_or("?")), SourceKind::LocalPositive, e),
            }
            Ok(())
        })?;
    }
    for d in &cfg.frontier_exports {
        let mut labels: Vec<String> = fs::read_dir(d)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("records.jsonl").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect();
        labels.sort();
        for l in labels {
            inputs.jsonl(&d.join(&l).join("records.jsonl"), "frontier_records", |input, line, body, v| {
                match frontier_row(&v, &l, &styles, &Origin { input, line, bytes: body }) {
                    Ok(r) => push(r, &mut candidates, &mut ignored_rows),
                    Err(e) => reject(&mut read_errors, v["id"].as_str().unwrap_or("?"),
                        &split_name(v["frozen_split"].as_str().unwrap_or("?")), SourceKind::Frontier, e),
                }
                Ok(())
            })?;
        }
    }
    if let Some(hc) = &cfg.hosted_captures {
        let adm = captures::admitted(&inputs.json(&hc.admission, "hosted_admission")?)?;
        for (files, kind) in [(&hc.reviews, "hosted_capture_reviews"), (&hc.style_records, "hosted_style_records")] {
            for f in files {
                inputs.jsonl(f, kind, |input, line, body, v| {
                    let o = Origin { input, line, bytes: body };
                    let r = if kind == "hosted_capture_reviews" { captures::review_row(&v, &adm, &o) } else { captures::style_row(&v, &adm, &o) };
                    match r {
                        Ok(Some(r)) => push(r, &mut candidates, &mut ignored_rows),
                        Ok(None) => *ignored_rows.entry(format!("{kind}:not_admitted")).or_default() += 1,
                        Err(e) => reject(&mut read_errors, v["id"].as_str().or(v["request_sha256"].as_str()).unwrap_or("?"), "train", SourceKind::HostedCapture, e),
                    }
                    Ok(())
                })?;
            }
        }
    }
    if let Some(f) = &cfg.legacy_train {
        inputs.jsonl(f, "stage3_legacy_train", |input, line, body, v| {
            match legacy_row(&v, &cfg.legacy_families, &Origin { input, line, bytes: body }) {
                Ok(r) => push(r, &mut candidates, &mut ignored_rows),
                Err(e) => reject(&mut read_errors, v["id"].as_str().unwrap_or("?"), "train", SourceKind::Stage3Legacy, e),
            }
            Ok(())
        })?;
    }
    for pool in &cfg.external_train_pools {
        ensure!(pool.group_per_mille <= 1000 && pool.positive_per_mille <= 1000, "external pool {}: rates are per mille", pool.name);
        ensure!(pool.keep_all_cohorts.iter().all(|c| pool.include_cohorts.contains(c)), "external pool {}: keep_all cohorts must be included", pool.name);
        if let Some(x) = pool.max_repeated_word_8gram_share {
            ensure!(x.is_finite() && x > 0.0 && x <= 1.0, "external pool {}: repetition bound in (0,1]", pool.name);
        }
        inputs.jsonl(&pool.documents, "external_pool_documents", |input, line, body, v| {
            let skip = |why: &str, ignored: &mut BTreeMap<String, usize>| *ignored.entry(format!("external:{}:{why}", pool.name)).or_default() += 1;
            let cohort = v["cohort_id"].as_str().unwrap_or("");
            if !pool.include_cohorts.iter().any(|c| c == cohort) {
                skip("cohort_not_included", &mut ignored_rows);
                return Ok(());
            }
            if let Some(x) = pool.max_repeated_word_8gram_share {
                let rep = v["format"]["repeated_word_8gram_share"].as_f64().context("repeated_word_8gram_share")?;
                if rep >= x {
                    skip("repeated_word_8gram_share_at_or_above_bound", &mut ignored_rows);
                    return Ok(());
                }
            }
            if !pool.keep_all_cohorts.iter().any(|c| c == cohort) {
                let g = v["source_group"].as_str().unwrap_or("");
                if per_mille(&pool.seed, &format!("group:{g}")) >= pool.group_per_mille {
                    skip("group_not_sampled", &mut ignored_rows);
                    return Ok(());
                }
                let id = v["id"].as_str().unwrap_or("");
                if v["origin"] == "model_only" && per_mille(&pool.seed, &format!("row:{id}")) >= pool.positive_per_mille {
                    skip("model_row_not_sampled", &mut ignored_rows);
                    return Ok(());
                }
            }
            match external_row(&v, &pool.name, &Origin { input, line, bytes: body }) {
                Ok(r) => push(r, &mut candidates, &mut ignored_rows),
                Err(e) => reject(&mut read_errors, v["id"].as_str().unwrap_or("?"), "train", SourceKind::ExternalPool, e),
            }
            Ok(())
        })?;
    }
    // Train positives may descend from Train-mining humans; resolve only the missing parents.
    if built.contains("train") && !cfg.train_side_manifests.is_empty() {
        let missing: HashSet<String> = candidates["train"]
            .iter()
            .filter_map(|r| r.parent_id.clone())
            .filter(|p| !parents.contains_key(p))
            .collect();
        if !missing.is_empty() {
            for f in &cfg.train_side_manifests {
                inputs.jsonl(f, "train_side_manifest", |_, _, _, v| {
                    let Some(id) = v["id"].as_str() else { return Ok(()) };
                    if missing.contains(id) && v["split"].as_str().map(split_name).as_deref() == Some("train_mining") {
                        parents.insert(id.into(), Parent {
                            split: "train_mining".into(),
                            source_group: str_req(&v, "source_group")?.into(),
                            writer_key: str_opt(&v, "writer_key"),
                            genre: None,
                            collection: None,
                            domain: str_req(&v, "domain")?.into(),
                        });
                    }
                    Ok(())
                })?;
            }
        }
    }
    // Source groups that hosted captures name: Train humans, legacy Train rows and Train-mining groups.
    let mut train_side_groups: HashMap<String, String> = HashMap::new();
    if let Some(train) = candidates.get("train") {
        let wanted: HashSet<String> = train.iter().filter(|r| r.source_kind == SourceKind::HostedCapture).map(|r| r.source_group.clone()).collect();
        if !wanted.is_empty() {
            for r in train.iter().filter(|r| matches!(r.source_kind, SourceKind::Human | SourceKind::Stage3Legacy)) {
                if wanted.contains(&r.source_group) {
                    train_side_groups.entry(r.source_group.clone()).or_insert_with(|| r.domain.clone());
                }
            }
            for f in &cfg.train_side_manifests {
                inputs.jsonl(f, "train_side_manifest_capture_groups", |_, _, _, v| {
                    if let (Some(g), Some(d)) = (v["source_group"].as_str(), v["domain"].as_str()) {
                        if wanted.contains(g) && v["split"].as_str().map(split_name).as_deref() == Some("train_mining") {
                            train_side_groups.entry(g.into()).or_insert_with(|| d.into());
                        }
                    }
                    Ok(())
                })?;
            }
        }
    }
    // Round 2: the rows re-verdict's laid-out RAID, stage-3 and capture rows,
    // before selection, dedup and weights (`rows_renorm`). It runs after the
    // Train-mining parents and the capture source groups are resolved from the
    // candidates as gathered, so a planned row it drops still names its source
    // group and parent (`ORDER_RULE`); replacement changes neither.
    let rows_renorm = match cfg.round2.as_ref().and_then(|r| r.rows_renorm.as_deref()) {
        Some(dir) => {
            ensure!(built.contains("train"), "round2.rows_renorm replaces Train rows: the config must build the Train split");
            let rr = rows_renorm::RowsRenorm::read(dir, &mut inputs).with_context(|| format!("round2.rows_renorm {}", dir.display()))?;
            Some(rr.apply(candidates.get_mut("train").unwrap(), &eval_texts)?)
        }
        None => None,
    };
    let frozen_sha256 = styles.sha256.clone();
    let round2_writer_keys = match &cfg.round2 {
        Some(r2) => round2_inputs(r2, &mut inputs)?,
        None => Vec::new(),
    };
    Ok(Gathered { inputs, candidates, parents, eval_keys, eval_texts, train_families, train_side_groups, frozen_sha256, read_errors, ignored_rows, excluded_groups, round2_writer_keys, rows_renorm })
}

fn write_atomic(p: &Path, b: &[u8]) -> Result<String> {
    let tmp = p.with_extension("tmp-write");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(b)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, p)?;
    Ok(sha_hex(b))
}

fn pretty(v: &Value) -> Vec<u8> {
    let mut b = serde_json::to_vec_pretty(v).unwrap();
    b.push(b'\n');
    b
}

/// Build every configured split under `root/<build_id>/` and point
/// `root/CURRENT.json` at it. Returns the build summary.
pub fn build(config_path: &Path, root: &Path, code: &Value) -> Result<Value> {
    build_sealed(config_path, root, code, None)
}

/// As `build`; with `seal`, the written seal is verified first (it must bind
/// `code.assembler_sha256`) and the config may build the Test split.
pub fn build_sealed(config_path: &Path, root: &Path, code: &Value, seal: Option<&SealCheck>) -> Result<Value> {
    let cfg_bytes = fs::read(config_path)?;
    let cfg: Config = serde_json::from_slice(&cfg_bytes).context("config")?;
    let seal_record = match seal {
        Some(s) => Some(s.verify(code["assembler_sha256"].as_str().unwrap_or(""))?),
        None => None,
    };
    let g = gather_sealed(&cfg, seal_record.is_some())?;
    let mut id_src = sha_hex(&cfg_bytes);
    for i in &g.inputs.list {
        id_src.push_str(&i.sha256);
    }
    id_src.push_str(code["assembler_sha256"].as_str().unwrap_or(""));
    let build_id = sha_hex(id_src.as_bytes())[..16].to_string();
    let dir = root.join(format!("build-{build_id}"));
    fs::create_dir_all(root)?;
    if dir.join("BUILD.json").is_file() {
        let b: Value = serde_json::from_slice(&fs::read(dir.join("BUILD.json"))?)?;
        write_atomic(&root.join("CURRENT.json"), &pretty(&json!({"current": dir, "build_id": build_id})))?;
        return Ok(json!({"build_id": build_id, "dir": dir, "unchanged": true, "splits": b["splits"]}));
    }
    let tmp = root.join(format!(".tmp-build-{build_id}"));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(&tmp)?;
    let inputs = g.inputs.report();
    let mut splits = serde_json::Map::new();
    let empty_groups: HashMap<String, String> = HashMap::new();
    for (split, mut cands) in g.candidates {
        let train = split == "train";
        let mut audit_ex = Vec::new();
        if !train && !g.excluded_groups.is_empty() {
            cands.retain(|r| match g.excluded_groups.get(&r.source_group) {
                Some(rec) => {
                    audit_ex.push(Exclusion { id: r.id.clone(), split: split.clone(), source_kind: r.source_kind,
                        reason: "overlap_audit_excluded_source_group".into(), detail: json!({"receipt": rec})});
                    false
                }
                None => true,
            });
        }
        let (mut rows, mut ex) = select_with(&split, cands, &g.parents, &g.eval_keys, &g.eval_texts, &g.train_families,
            if train { &g.train_side_groups } else { &empty_groups }, if train { g.rows_renorm.as_ref() } else { None });
        ex.extend(audit_ex);
        ex.extend(g.read_errors.iter().filter(|e| e.split == split).cloned());
        let renorm_record = match (&g.rows_renorm, train) {
            (Some(a), true) => {
                let rec = a.check_selected(&rows, &ex)?;
                ex.extend(a.drops.iter().cloned());
                Some(rec)
            }
            _ => None,
        };
        let block_tags = cfg.round2.as_ref().and_then(|r| r.block_tags.as_ref());
        if let Some(bt) = block_tags {
            tag_blocks(&mut rows, bt);
        }
        let r2 = cfg.round2.as_ref().filter(|r| r.on()).map(|opts| weights::Round2Train { opts, writer_keys: &g.round2_writer_keys });
        let weights = weights::assign_weights_with(&mut rows, train, &cfg.cohort_weights, &cfg.weighting, r2.as_ref())?;
        let row_schema = if r2.is_some() { ROW_SCHEMA_R2 } else { ROW_SCHEMA };
        for r in rows.iter_mut() {
            r.schema = row_schema;
        }
        let d = tmp.join(split.replace('_', "-"));
        fs::create_dir_all(&d)?;
        let mut rb = Vec::new();
        for r in &rows {
            serde_json::to_writer(&mut rb, r)?;
            rb.push(b'\n');
        }
        let rows_sha = write_atomic(&d.join("rows.jsonl"), &rb)?;
        let mut eb = Vec::new();
        for e in &ex {
            serde_json::to_writer(&mut eb, e)?;
            eb.push(b'\n');
        }
        let ex_sha = write_atomic(&d.join("exclusions.jsonl"), &eb)?;
        let weights_sha = write_atomic(&d.join("weights.json"), &pretty(&weights))?;
        let mut by_reason: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
        for e in &ex {
            *by_reason.entry(e.reason.clone()).or_default().entry(format!("{:?}", e.source_kind)).or_default() += 1;
        }
        let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_cohort: BTreeMap<String, usize> = BTreeMap::new();
        let mut relabelled: BTreeMap<String, usize> = BTreeMap::new();
        for r in &rows {
            if let Some(d) = &r.record_domain {
                *relabelled.entry(format!("{d} -> {}", r.domain)).or_default() += 1;
            }
        }
        let mut held_out = 0usize;
        let mut style_unknown_rows = 0usize;
        let mut text_matched = 0usize;
        for r in &rows {
            *by_kind.entry(format!("{:?}", r.source_kind)).or_default() += 1;
            *by_cohort.entry(r.cohort_id.clone()).or_default() += 1;
            held_out += usize::from(r.held_out_style);
            style_unknown_rows += usize::from(style_unknown(r));
            text_matched += usize::from(r.style_text_match.is_some());
        }
        let sealed = cfg.sealed.contains(&split);
        let groups: HashSet<&str> = rows.iter().map(|r| r.source_group.as_str()).collect();
        let manifest = json!({
            "schema": MANIFEST_SCHEMA, "build_id": build_id, "split": split, "sealed": sealed,
            "row_schema": row_schema, "rows": rows.len(), "source_groups": groups.len(),
            "files": {"rows.jsonl": rows_sha, "weights.json": weights_sha, "exclusions.jsonl": ex_sha},
            "counts_domain_origin_generator": counts(&rows),
            "rows_by_source_kind": by_kind, "rows_by_cohort": by_cohort,
            "positive_domains_taken_from_parent": relabelled,
            "held_out_style_rows": held_out, "style_partition_unknown_rows": style_unknown_rows,
            "style_text_matched_rows": text_matched, "frozen_partitions_sha256": g.frozen_sha256,
            "exclusions": ex.len(), "exclusions_by_reason": by_reason,
            "checks": {
                "kept_rows_colliding_with_other_side": rows.iter().filter(|r| if train {
                    g.eval_keys.collision(r).is_some() || g.eval_texts.contains_key(&r.text_sha256)
                } else { g.train_families.contains(&r.source_group) }).count(),
                "held_out_style_rows_in_train": if train { json!(held_out) } else { Value::Null },
                "unknown_style_rows_in_train": if train { json!(style_unknown_rows) } else { Value::Null },
                "evaluation_rows_in_train_groups_excluded": by_reason.get("evaluation_row_in_train_group").map(|m| m.values().sum::<usize>()).unwrap_or(0),
                "text_digests_verified": rows.len(),
                "spans_validated": rows.len(),
            },
            "inputs": inputs, "config_sha256": sha_hex(&cfg_bytes), "code": code,
            "detector_outputs_used": false, "test_opened": split == TEST_SPLIT,
        });
        let mut manifest = manifest;
        if block_tags.is_some() {
            let mut by_block: BTreeMap<&str, usize> = BTreeMap::new();
            for r in &rows {
                *by_block.entry(r.block.as_deref().unwrap_or("")).or_default() += 1;
            }
            manifest["rows_by_block"] = json!(by_block);
        }
        if let Some(r) = renorm_record {
            manifest["rows_renorm"] = r;
        }
        if split == TEST_SPLIT {
            manifest["seal"] = seal_record.clone().unwrap_or(Value::Null);
            let mut pops: BTreeMap<String, usize> = BTreeMap::new();
            for r in &rows {
                *pops.entry(r.population.clone()).or_default() += 1;
            }
            manifest["rows_by_population"] = json!(pops);
        }
        let manifest_sha = write_atomic(&d.join("MANIFEST.json"), &pretty(&manifest))?;
        if sealed {
            write_atomic(&d.join("SEALED.json"), &pretty(&json!({
                "schema": "slop_ninja_a_bundle_seal_v1", "split": split, "sealed": true,
                "rule": "built by sealed pipeline code only; nobody inspects or scores this bundle until its protocol use (Dev-select: the single finalist selection; CAL: calibration after selection); trainer-bundle refuses it without --unseal-for, which records the one use in UNSEALED-<purpose>.json",
                "rows_sha256": rows_sha})))?;
            for f in ["rows.jsonl", "SEALED.json", "MANIFEST.json", "weights.json", "exclusions.jsonl"] {
                set_read_only(&d.join(f))?;
            }
        }
        splits.insert(split.clone(), json!({"rows": rows.len(), "exclusions": ex.len(), "sealed": sealed,
            "rows_sha256": rows_sha, "manifest_sha256": manifest_sha}));
    }
    let mut b = json!({"schema": "slop_ninja_a_bundle_build_v1", "build_id": build_id, "config": config_path,
        "config_sha256": sha_hex(&cfg_bytes), "splits": splits, "ignored_rows_of_unbuilt_splits": g.ignored_rows,
        "code": code});
    if let Some(s) = &seal_record {
        b["seal"] = s.clone();
    }
    write_atomic(&tmp.join("BUILD.json"), &pretty(&b))?;
    fs::rename(&tmp, &dir)?;
    write_atomic(&root.join("CURRENT.json"), &pretty(&json!({"current": dir, "build_id": build_id})))?;
    Ok(json!({"build_id": build_id, "dir": dir, "unchanged": false, "splits": b["splits"]}))
}

/// Remove every write permission bit of a file.
pub fn set_read_only(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(p)?.permissions();
    perm.set_mode(perm.mode() & 0o555);
    fs::set_permissions(p, perm)?;
    Ok(())
}

/// Digest of a file, streamed.
pub fn file_sha(p: &Path) -> Result<String> {
    let mut f = fs::File::open(p).with_context(|| format!("open {}", p.display()))?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

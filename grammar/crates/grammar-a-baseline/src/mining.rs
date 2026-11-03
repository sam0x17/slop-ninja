//! Hard-negative mining rounds for full A over the Train-mining human pool.
//!
//! The PyTorch scorer (`mine_full_a.py`) writes A's own uncalibrated P(any AI)
//! per Train-mining document. This module ranks those humans, excludes any
//! document, source family or writer that belongs to Train already or to an
//! evaluation split (CAL, Dev-iter, Dev-select, Test), writes the mined rows
//! as shared-contract Train rows, queues matched-positive requests for the
//! local generator (file based; nothing is generated here), extends the
//! bundle for the next round and applies the protocol stop rule: stop after
//! the first round whose mean Dev-iter balanced Brier falls by less than
//! 0.001, and after three rounds at most.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

pub const ROUND_SCHEMA: &str = "slop_ninja_full_a_mining_round_v1";
pub const REQUEST_SCHEMA: &str = "slop_ninja_a_mining_request_v1";
pub const QUEUE_SCHEMA: &str = "slop_ninja_a_mining_queue_v1";
pub const STOP_SCHEMA: &str = "slop_ninja_full_a_mining_stop_v1";
pub const WEIGHTS_SCHEMA: &str = "slop_ninja_a_bundle_weights_v1";
pub const MANIFEST_SCHEMA: &str = "slop_ninja_a_bundle_manifest_v1";
pub const STOP_DELTA: f64 = 0.001;
pub const MAX_ROUNDS: usize = 3;
pub const POOL_SPLIT: &str = "train_mining";
pub const HUMAN_LABEL_KEY: &str = "human";
/// Splits whose documents, families and writers are never mined.
pub const EVALUATION_SPLITS: [&str; 4] = ["cal", "dev_iter", "dev_select", "test"];
/// Default minimum P(any AI) of a mined row: mined humans are false positives.
pub const DEFAULT_MIN_P: f64 = 0.5;
/// Top-level splits directories that are not evaluation material.
const TRAIN_SIDE_DIRS: [&str; 3] = ["train", "train-mining", "held"];
pub const SCORE_SCHEMA: &str = "slop_ninja_full_a_mining_scores_v2";

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut h = Sha256::new();
    let mut f = BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    loop {
        let buf = f.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        h.update(buf);
        let n = buf.len();
        f.consume(n);
    }
    Ok(hex::encode(h.finalize()))
}

/// Deterministic tie key: SHA-256 of "seed\0id".
pub fn tie_key(seed: &str, id: &str) -> String {
    sha256_hex(format!("{seed}\0{id}").as_bytes())
}

/// Visit each nonempty JSONL line as a JSON value.
pub fn for_each_jsonl(path: &Path, mut f: impl FnMut(usize, Value) -> Result<()>) -> Result<()> {
    let reader = BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    for (n, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).with_context(|| format!("{}:{}", path.display(), n + 1))?;
        f(n + 1, v)?;
    }
    Ok(())
}

fn text(v: &Value, key: &str) -> Result<String> {
    Ok(v.get(key).and_then(Value::as_str).with_context(|| format!("{key} missing"))?.to_owned())
}

fn opt_text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Canonical split name, with the aliases the Python reader accepts.
pub fn canonical_split(s: &str) -> Result<&'static str> {
    Ok(match s {
        "train" => "train",
        "train_mining" | "train-mining" => "train_mining",
        "dev_iter" | "dev-iter" | "development_iter" => "dev_iter",
        "dev_select" | "dev-select" | "development_select" => "dev_select",
        "cal" | "calibration" => "cal",
        "test" => "test",
        other => bail!("unknown split {other:?}"),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Score {
    pub id: String,
    pub text_sha256: String,
    pub source_group: String,
    pub p_any_ai: f64,
}

impl Score {
    pub fn from_json(v: &Value) -> Result<Self> {
        let p = v["p_any_ai"].as_f64().context("p_any_ai")?;
        ensure!(p.is_finite() && (0.0..=1.0).contains(&p), "p_any_ai out of range");
        Ok(Self { id: text(v, "id")?, text_sha256: text(v, "text_sha256")?,
                  source_group: text(v, "source_group")?, p_any_ai: p })
    }
}

/// Keys that must never be mined.
#[derive(Default, Debug)]
pub struct Exclusions {
    pub ids: HashSet<String>,
    pub texts: HashSet<String>,
    pub families: HashSet<String>,
    pub writers: HashSet<String>,
}

impl Exclusions {
    /// Every row already in the bundle (any split): its id, text, and, for
    /// evaluation splits, its source family.
    pub fn add_bundle(&mut self, rows: &Path) -> Result<()> {
        for_each_jsonl(rows, |_, v| {
            self.ids.insert(text(&v, "id")?);
            self.texts.insert(text(&v, "text_sha256")?);
            let split = canonical_split(&text(&v, "split")?)?;
            if EVALUATION_SPLITS.contains(&split) {
                self.families.insert(text(&v, "source_group")?);
            }
            Ok(())
        })
    }

    /// A sealed evaluation split manifest (family_key, group_key, optional
    /// text_sha256); metadata only. `expected` is the split its directory names.
    pub fn add_evaluation_manifest(&mut self, path: &Path) -> Result<usize> {
        self.add_split_manifest(path, None)
    }

    fn add_split_manifest(&mut self, path: &Path, expected: Option<&str>) -> Result<usize> {
        let mut n = 0;
        for_each_jsonl(path, |_, v| {
            let split = canonical_split(&text(&v, "split")?)?;
            ensure!(EVALUATION_SPLITS.contains(&split), "{} is not an evaluation manifest", path.display());
            ensure!(expected.is_none_or(|e| e == split), "{}: row split {split} differs from its directory",
                    path.display());
            self.families.insert(text(&v, "family_key")?);
            if let Some(w) = opt_text(&v, "group_key") {
                self.writers.insert(w);
            }
            if let Some(t) = opt_text(&v, "text_sha256") {
                self.texts.insert(t);
            }
            n += 1;
            Ok(())
        })?;
        Ok(n)
    }
}

impl Exclusions {
    /// An ungated diagnostic slice manifest (`split` `diagnostic_<slice>`,
    /// `source_group`, `writer_key`, `text_sha256`): never Train or mining.
    pub fn add_diagnostic_manifest(&mut self, path: &Path) -> Result<usize> {
        let mut n = 0;
        for_each_jsonl(path, |_, v| {
            ensure!(text(&v, "split")?.starts_with("diagnostic_"), "{} is not a diagnostic manifest", path.display());
            self.families.insert(text(&v, "source_group")?);
            if let Some(w) = opt_text(&v, "writer_key") {
                self.writers.insert(w);
            }
            self.texts.insert(text(&v, "text_sha256")?);
            n += 1;
            Ok(())
        })?;
        Ok(n)
    }

    /// Groups reserved for evaluation but not yet allocated (`family_keys`,
    /// `group_key`): conservative exclusions.
    pub fn add_reserved_groups(&mut self, path: &Path) -> Result<usize> {
        let mut n = 0;
        for_each_jsonl(path, |line, v| {
            let mut any = false;
            if let Some(keys) = v.get("family_keys").and_then(Value::as_array) {
                for k in keys {
                    self.families.insert(k.as_str().context("family key")?.to_owned());
                    any = true;
                }
            }
            if let Some(g) = opt_text(&v, "group_key") {
                self.writers.insert(g);
                any = true;
            }
            ensure!(any, "{}:{line}: reserved group without family_keys or group_key", path.display());
            n += 1;
            Ok(())
        })?;
        Ok(n)
    }

    /// One reserved family key per line.
    pub fn add_reserved_families(&mut self, path: &Path) -> Result<usize> {
        let mut n = 0;
        let reader = BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
        for line in reader.lines() {
            let line = line?;
            let key = line.trim();
            if !key.is_empty() {
                self.families.insert(key.to_owned());
                n += 1;
            }
        }
        Ok(n)
    }

    /// Every exclusion source of a splits directory, derived from its
    /// SHA256SUMS: all evaluation manifests (CAL, Dev-iter, Dev-select and
    /// Test of every part directory, each part complete), every diagnostic
    /// manifest and every reserved group or family list. Each file must match
    /// its listed digest; an unrecognised manifest is refused, so a new kind of
    /// evaluation file cannot be skipped silently.
    pub fn add_splits_dir(&mut self, dir: &Path) -> Result<Vec<Value>> {
        let sums = fs::read_to_string(dir.join("SHA256SUMS")).context("splits SHA256SUMS")?;
        let mut sources = Vec::new();
        let mut parts: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
        let mut diagnostics = 0;
        for line in sums.lines().filter(|l| !l.trim().is_empty()) {
            let (digest, path) = line.split_once("  ").context("SHA256SUMS line")?;
            let rel = path.trim().trim_start_matches("./");
            let comps: Vec<&str> = rel.split('/').collect();
            let file = *comps.last().context("path")?;
            let reserved = comps.iter().any(|c| *c == "reserved-unassigned" || *c == "evaluation-reserved");
            let kind = if file == "manifest.jsonl" {
                if TRAIN_SIDE_DIRS.contains(&comps[0]) && comps.len() == 2 {
                    continue;
                }
                if comps[0] == "diagnostics" {
                    "diagnostic_manifest"
                } else {
                    ensure!(comps.len() >= 2, "unrecognised manifest {rel}");
                    let split = canonical_split(comps[comps.len() - 2])
                        .ok()
                        .filter(|s| EVALUATION_SPLITS.contains(s))
                        .with_context(|| format!("unrecognised manifest {rel}: not an evaluation split directory"))?;
                    let part = if comps.len() == 2 { ".".to_owned() } else { comps[..comps.len() - 2].join("/") };
                    parts.entry(part).or_default().insert(split);
                    "evaluation_manifest"
                }
            } else if reserved && file == "families.txt" {
                "reserved_families"
            } else if reserved && file.ends_with("groups.jsonl") {
                "reserved_groups"
            } else {
                continue;
            };
            let full = dir.join(rel);
            ensure!(sha256_file(&full)? == digest.trim(), "{rel} differs from SHA256SUMS");
            let rows = match kind {
                "evaluation_manifest" => {
                    let split = canonical_split(comps[comps.len() - 2])?;
                    self.add_split_manifest(&full, Some(split))?
                }
                "diagnostic_manifest" => {
                    diagnostics += 1;
                    self.add_diagnostic_manifest(&full)?
                }
                "reserved_families" => self.add_reserved_families(&full)?,
                _ => self.add_reserved_groups(&full)?,
            };
            sources.push(json!({"path": full.display().to_string(), "kind": kind, "sha256": digest.trim(),
                                "rows": rows}));
        }
        ensure!(parts.contains_key("."), "splits directory has no top-level evaluation manifests");
        for (part, splits) in &parts {
            ensure!(splits.len() == EVALUATION_SPLITS.len(),
                    "evaluation part {part} lacks some of CAL, Dev-iter, Dev-select and Test: {splits:?}");
        }
        sources.push(json!({"kind": "summary", "evaluation_parts": parts.keys().collect::<Vec<_>>(),
                            "diagnostic_manifests": diagnostics,
                            "splits_sha256sums": sha256_file(&dir.join("SHA256SUMS"))?}));
        Ok(sources)
    }
}

#[derive(Clone, Debug)]
pub struct SelectConfig {
    pub round: usize,
    pub top: usize,
    pub per_group_cap: usize,
    pub min_p: f64,
    pub seed: String,
}

/// Pool facts needed for exclusion (text-free).
#[derive(Clone, Debug)]
pub struct PoolFacts {
    pub text_sha256: String,
    pub source_group: String,
    pub writer_key: Option<String>,
}

/// Rank by P(any AI) descending, then the seeded tie key; drop excluded rows;
/// cap rows per source family; keep the top `top` at or above `min_p`.
pub fn select(scores: &[Score], pool: &HashMap<String, PoolFacts>, excl: &Exclusions, cfg: &SelectConfig)
    -> Result<(Vec<Score>, BTreeMap<String, usize>)> {
    ensure!(cfg.round >= 1 && cfg.round <= MAX_ROUNDS, "mining round must be 1..={MAX_ROUNDS}");
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut ranked: Vec<(&Score, String)> = Vec::new();
    for s in scores {
        ensure!(seen.insert(s.id.as_str()), "duplicate score id {}", s.id);
        let facts = pool.get(&s.id).with_context(|| format!("scored id {} is not a Train-mining pool row", s.id))?;
        ensure!(facts.text_sha256 == s.text_sha256 && facts.source_group == s.source_group,
                "score {} disagrees with the pool row", s.id);
        let reason = if excl.ids.contains(&s.id) {
            Some("id_in_bundle")
        } else if excl.texts.contains(&s.text_sha256) {
            Some("text_in_bundle_or_evaluation")
        } else if excl.families.contains(&s.source_group) {
            Some("family_in_evaluation")
        } else if facts.writer_key.as_ref().is_some_and(|w| excl.writers.contains(w)) {
            Some("writer_in_evaluation")
        } else if s.p_any_ai < cfg.min_p {
            Some("below_min_p")
        } else {
            None
        };
        match reason {
            Some(r) => *reasons.entry(r.to_owned()).or_default() += 1,
            None => ranked.push((s, tie_key(&cfg.seed, &s.id))),
        }
    }
    ranked.sort_by(|a, b| b.0.p_any_ai.total_cmp(&a.0.p_any_ai).then_with(|| a.1.cmp(&b.1)));
    let mut per_group: HashMap<&str, usize> = HashMap::new();
    let mut chosen = Vec::new();
    for (s, _) in ranked {
        if chosen.len() >= cfg.top {
            *reasons.entry("beyond_top".into()).or_default() += 1;
            continue;
        }
        let n = per_group.entry(s.source_group.as_str()).or_default();
        if *n >= cfg.per_group_cap {
            *reasons.entry("per_group_cap".into()).or_default() += 1;
            continue;
        }
        *n += 1;
        chosen.push(s.clone());
    }
    Ok((chosen, reasons))
}

/// Shared-contract Train row for a mined human (exact pool text).
pub fn mined_row(doc: &Value, round: usize, cohort_weight: f64, provenance: &Value) -> Result<Value> {
    let t = text(doc, "text")?;
    ensure!(sha256_hex(t.as_bytes()) == text(doc, "text_sha256")?, "pool text binding differs");
    Ok(json!({
        "id": text(doc, "id")?, "split": "train", "text": t, "text_sha256": text(doc, "text_sha256")?,
        "origin": "human_only", "share": 0.0, "ai_byte_spans": null, "generator_label": null,
        "cohort_id": mined_cohort(round), "cohort_weight": cohort_weight,
        "source_group": text(doc, "source_group")?, "domain": text(doc, "domain")?,
        "genre": doc.get("genre").cloned().unwrap_or(Value::Null),
        "voice_id": null, "voice_cluster_id": null, "voice_origin": null, "style_axes": null,
        "operation": "hard_negative_mining", "held_out_style": false,
        "provenance_sha256": sha256_hex(&serde_json::to_vec(provenance)?),
    }))
}

pub fn mined_cohort(round: usize) -> String {
    format!("mined_humans_round_{round}")
}

pub fn matched_cohort(round: usize) -> String {
    format!("mined_matched_positives_round_{round}")
}

/// Matched-positive requests for one mined human: one per kind, generator by
/// the seeded rotation. Topic and length are matched to the parent; the
/// generator's own planner renders prompts and spans.
pub fn requests_for(doc: &Value, rank: usize, round: usize, kinds: &[String], generators: &[String])
    -> Result<Vec<Value>> {
    ensure!(!generators.is_empty() && !kinds.is_empty(), "generators and kinds required");
    let id = text(doc, "id")?;
    let mut out = Vec::new();
    for (k, kind) in kinds.iter().enumerate() {
        let generator = &generators[(rank * kinds.len() + k) % generators.len()];
        out.push(json!({
            "schema": REQUEST_SCHEMA, "status": "pending", "round": round,
            "request_id": format!("mreq-{}", &sha256_hex(format!("{round}\0{id}\0{kind}").as_bytes())[..24]),
            "parent_id": id, "parent_text_sha256": text(doc, "text_sha256")?,
            "source_group": text(doc, "source_group")?, "writer_key": doc.get("writer_key").cloned().unwrap_or(Value::Null),
            "domain": text(doc, "domain")?, "split": "train", "kind": kind, "generator": generator,
            "match": {"topic": "parent_passage", "word_count": doc.get("word_count").cloned().unwrap_or(Value::Null)},
            "cohort_id": matched_cohort(round), "selection_rank": rank,
        }));
    }
    Ok(out)
}

#[derive(Debug, PartialEq)]
pub struct StopDecision {
    pub stop: bool,
    pub next_round: Option<usize>,
    pub reason: &'static str,
    pub improvements: Vec<f64>,
}

/// means[0] is the pre-mining arm's mean Dev-iter balanced Brier, means[k]
/// after round k. Continue while the latest round improved by >= 0.001 and
/// fewer than three rounds ran.
pub fn stop_rule(means: &[f64]) -> Result<StopDecision> {
    ensure!(!means.is_empty() && means.len() <= MAX_ROUNDS + 1, "1..=4 round means required");
    ensure!(means.iter().all(|m| m.is_finite() && *m >= 0.0), "finite Brier means required");
    let improvements: Vec<f64> = means.windows(2).map(|w| w[0] - w[1]).collect();
    let rounds = means.len() - 1;
    let (stop, reason) = if rounds == 0 {
        (false, "no round yet")
    } else if improvements[rounds - 1] < STOP_DELTA {
        (true, "latest round improved mean Dev-iter balanced Brier by less than 0.001")
    } else if rounds >= MAX_ROUNDS {
        (true, "three rounds completed")
    } else {
        (false, "latest round improved by at least 0.001")
    };
    Ok(StopDecision { stop, next_round: (!stop).then_some(rounds + 1), reason, improvements })
}

/// One seed fit of a mining round (fit.json, schema v2), text-free.
#[derive(Clone, Debug)]
pub struct RoundFit {
    pub path: String,
    pub seed: Value,
    pub arm: Value,
    pub arm_definition: Value,
    pub protocol_sha256: Value,
    pub dev_iter_rows_sha256: String,
    pub bundle_sha256: Value,
    pub brier: f64,
    pub fit_sha256: String,
}

impl RoundFit {
    pub fn read(path: &Path) -> Result<Self> {
        let v: Value = serde_json::from_slice(&fs::read(path.join("fit.json"))?)?;
        ensure!(v["schema"] == "slop_ninja_full_a_fit_v2" && v["complete"] == true &&
                v["selection_split"] == "dev_iter" && v["final_test_opened"] == false,
                "{} is not a complete Dev-iter-selected v2 fit", path.display());
        ensure!(v["cohort_weight_log"]["complete"] == true, "{}: fit without its cohort weight log is invalid",
                path.display());
        let b = &v["binding"];
        let dev = v["dev_iter_rows_sha256"].as_str().context("fit.json dev_iter_rows_sha256")?;
        ensure!(b["dev_iter_rows_sha256"] == dev, "{}: Dev-iter digest differs from its binding", path.display());
        let brier = v["selection_split_involvement_brier"].as_f64().context("Brier")?;
        ensure!(brier.is_finite() && brier >= 0.0, "{}: Brier", path.display());
        Ok(Self { path: path.display().to_string(), seed: b["seed"].clone(), arm: b["arm"].clone(),
                  arm_definition: b["arm_definition"].clone(), protocol_sha256: b["protocol_sha256"].clone(),
                  dev_iter_rows_sha256: dev.to_owned(), bundle_sha256: b["bundle_sha256"].clone(), brier,
                  fit_sha256: sha256_file(&path.join("fit.json"))? })
    }

    fn json(&self) -> Value {
        json!({"fit": self.path, "seed": self.seed, "dev_iter_balanced_brier": self.brier,
               "fit_sha256": self.fit_sha256, "bundle_sha256": self.bundle_sha256})
    }
}

/// SHA-256 of a bundle's three files, as the trainer binds them.
pub fn bundle_digests(dir: &Path) -> Result<Value> {
    let mut m = Map::new();
    for name in ["rows.jsonl", "weights.json", "MANIFEST.json"] {
        m.insert(name.to_owned(), json!(sha256_file(&dir.join(name))?));
    }
    Ok(Value::Object(m))
}

/// Round k's bundle must descend from round k-1's through `extend` steps that
/// append only round-k cohorts, including the mined humans of round k.
/// Returns the appended cohorts, oldest first.
pub fn check_lineage(child: &Path, parent: &Value, round: usize) -> Result<Vec<String>> {
    let allowed = [mined_cohort(round), matched_cohort(round)];
    let mut cohorts = Vec::new();
    let mut current = child.to_path_buf();
    for _ in 0..8 {
        let m: Value = serde_json::from_slice(&fs::read(current.join("MANIFEST.json"))?)?;
        ensure!(m["schema"] == MANIFEST_SCHEMA, "{}: not an extended bundle (round {round} must extend round {})",
                current.display(), round - 1);
        let mut step = Vec::new();
        for c in m["appended_cohorts"].as_array().context("appended_cohorts")? {
            let c = c.as_str().context("cohort")?.to_owned();
            ensure!(allowed.contains(&c), "{}: appended cohort {c} is not a round-{round} mining cohort",
                    current.display());
            step.push(c);
        }
        step.extend(cohorts);
        cohorts = step;
        let inputs = m["inputs"].as_array().context("inputs")?;
        ensure!(inputs.len() >= 3, "{}: parent inputs", current.display());
        let parent_digests: Vec<&Value> = inputs[..3].iter().map(|i| &i["sha256"]).collect();
        let expected = ["rows.jsonl", "weights.json", "MANIFEST.json"].map(|k| &parent[k]);
        if parent_digests == expected {
            ensure!(cohorts.contains(&mined_cohort(round)), "round {round} bundle lacks cohort {}",
                    mined_cohort(round));
            return Ok(cohorts);
        }
        current = m["parent_bundle"].as_str().context("parent_bundle")?.into();
        ensure!(bundle_digests(&current)? == json!({"rows.jsonl": parent_digests[0], "weights.json": parent_digests[1],
                "MANIFEST.json": parent_digests[2]}), "{}: parent bundle changed since extension", current.display());
    }
    bail!("round {round} bundle does not descend from the round {} bundle", round - 1)
}

/// Check that the rounds are comparable and return their means and records:
/// one bundle per round (every seed fit on it), round k extending round k-1
/// with round-k cohorts only, and across rounds the same seed set, arm, arm
/// definition, protocol and Dev-iter rows.
pub fn comparable_rounds(rounds: &[Vec<RoundFit>], bundles: &[&Path]) -> Result<(Vec<f64>, Vec<Value>)> {
    ensure!(!rounds.is_empty() && rounds.len() == bundles.len(), "one bundle per round required");
    let first = rounds[0].first().context("at least one fit per round")?;
    let seeds = |r: &[RoundFit]| -> Result<BTreeSet<i64>> {
        let mut s = BTreeSet::new();
        for f in r {
            ensure!(s.insert(f.seed.as_i64().context("seed")?), "{}: seed repeated within a round", f.path);
        }
        Ok(s)
    };
    let seed_set = seeds(&rounds[0])?;
    let mut means = Vec::new();
    let mut records = Vec::new();
    for (k, (fits, dir)) in rounds.iter().zip(bundles).enumerate() {
        ensure!(seeds(fits)? == seed_set, "round {k}: seed set differs from round 0");
        let digests = bundle_digests(dir)?;
        for f in fits {
            ensure!(f.arm == first.arm && f.arm_definition == first.arm_definition,
                    "round {k}: {} uses another arm or arm definition", f.path);
            ensure!(f.protocol_sha256 == first.protocol_sha256, "round {k}: {} uses another protocol", f.path);
            ensure!(f.dev_iter_rows_sha256 == first.dev_iter_rows_sha256,
                    "round {k}: {} was selected on other Dev-iter rows", f.path);
            ensure!(f.bundle_sha256 == digests, "round {k}: {} was not fitted on {}", f.path, dir.display());
        }
        let lineage = if k == 0 { Vec::new() } else { check_lineage(dir, &bundle_digests(bundles[k - 1])?, k)? };
        let mean = fits.iter().map(|f| f.brier).sum::<f64>() / fits.len() as f64;
        means.push(mean);
        records.push(json!({"round": k, "bundle": dir.display().to_string(), "bundle_sha256": digests,
                            "appended_cohorts": lineage, "mean_dev_iter_balanced_brier": mean,
                            "fits": fits.iter().map(RoundFit::json).collect::<Vec<_>>()}));
    }
    Ok((means, records))
}

/// Row facts used for counts and checks.
fn row_key(v: &Value) -> Result<(String, String, String, Option<String>, String, String)> {
    Ok((canonical_split(&text(v, "split")?)?.to_owned(), text(v, "domain")?, text(v, "origin")?,
        opt_text(v, "generator_label"), text(v, "id")?, text(v, "text_sha256")?))
}

/// Append Train rows (mined humans and optional matched positives) to a bundle,
/// declaring their cohorts in weights.json and recounting MANIFEST.json.
pub fn extend_bundle(bundle: &Path, additions: &[&Path], out: &Path) -> Result<Value> {
    ensure!(!out.exists(), "new output bundle directory required");
    let old_weights: Value = serde_json::from_slice(&fs::read(bundle.join("weights.json"))?)?;
    let mut weights = old_weights.clone();
    let cohorts = weights["cohorts"].as_object_mut().context("weights.json cohorts")?;
    let existing: BTreeSet<String> = cohorts.keys().cloned().collect();
    fs::create_dir(out)?;
    let tmp = out.join("rows.jsonl.tmp");
    let mut writer = BufWriter::new(File::create(&tmp)?);
    let mut ids = HashSet::new();
    let mut texts = HashSet::new();
    let mut counts: BTreeMap<(String, String, String, Option<String>), usize> = BTreeMap::new();
    let mut inputs = vec![];
    for name in ["rows.jsonl", "weights.json", "MANIFEST.json"] {
        inputs.push(json!({"path": bundle.join(name).display().to_string(), "sha256": sha256_file(&bundle.join(name))?}));
    }
    let reader = BufReader::new(File::open(bundle.join("rows.jsonl"))?);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line)?;
        let (split, domain, origin, label, id, sha) = row_key(&v)?;
        ensure!(split != "test", "a training bundle must not contain Test rows");
        ensure!(ids.insert(id), "duplicate id in base bundle");
        texts.insert(sha);
        *counts.entry((split, domain, origin, label)).or_default() += 1;
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
    }
    // New cohorts: (cohort, label key) -> (records, weight).
    let mut declared: BTreeMap<String, BTreeMap<String, (usize, f64)>> = BTreeMap::new();
    for path in additions {
        inputs.push(json!({"path": path.display().to_string(), "sha256": sha256_file(path)?}));
        for_each_jsonl(path, |n, v| {
            let (split, domain, origin, label, id, sha) = row_key(&v)?;
            ensure!(split == "train", "{}:{n}: appended rows must be Train rows", path.display());
            ensure!(ids.insert(id.clone()), "{}:{n}: id {id} already in the bundle", path.display());
            ensure!(texts.insert(sha), "{}:{n}: text already in the bundle", path.display());
            ensure!(v["held_out_style"] == false, "{}:{n}: held-out styles never enter Train", path.display());
            let cohort = text(&v, "cohort_id")?;
            ensure!(!existing.contains(&cohort), "{}:{n}: cohort {cohort} already declared", path.display());
            let w = v["cohort_weight"].as_f64().context("cohort_weight")?;
            ensure!(w.is_finite() && w >= 0.0, "cohort weight");
            let key = label.clone().unwrap_or_else(|| HUMAN_LABEL_KEY.to_owned());
            let e = declared.entry(cohort).or_default().entry(key).or_insert((0, w));
            ensure!(e.1 == w, "{}:{n}: cohort weight must be uniform per cohort and label", path.display());
            e.0 += 1;
            *counts.entry((split, domain, origin, label)).or_default() += 1;
            writer.write_all(serde_json::to_string(&v)?.as_bytes())?;
            writer.write_all(b"\n")?;
            Ok(())
        })?;
    }
    writer.flush()?;
    drop(writer);
    fs::rename(&tmp, out.join("rows.jsonl"))?;
    for (cohort, labels) in &declared {
        let policy = if cohort.starts_with("mined_humans_round_") {
            "mined_humans_v1"
        } else if cohort.starts_with("mined_matched_positives_round_") {
            "mined_matched_positives_v1"
        } else {
            "appended_train_rows_v1"
        };
        let mut l = Map::new();
        for (key, (records, weight)) in labels {
            l.insert(key.clone(), json!({"records": records, "weight": weight}));
        }
        cohorts.insert(cohort.clone(), json!({"policy": policy, "default": true, "labels": l}));
    }
    weights["schema"] = json!(WEIGHTS_SCHEMA);
    fs::write(out.join("weights.json"), serde_json::to_vec_pretty(&weights)?)?;
    let count_rows: Vec<Value> = counts.iter().map(|((s, d, o, g), n)| {
        json!({"split": s, "domain": d, "origin": o, "generator_label": g, "rows": n})
    }).collect();
    let manifest = json!({
        "schema": MANIFEST_SCHEMA, "inputs": inputs, "parent_bundle": bundle.display().to_string(),
        "rows_sha256": sha256_file(&out.join("rows.jsonl"))?,
        "weights_sha256": sha256_file(&out.join("weights.json"))?,
        "appended_cohorts": declared.keys().collect::<Vec<_>>(), "counts": count_rows,
        "final_test_opened": false,
    });
    fs::write(out.join("MANIFEST.json"), serde_json::to_vec_pretty(&manifest)?)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(id: &str, g: &str, p: f64) -> Score {
        Score { id: id.into(), text_sha256: format!("t-{id}"), source_group: g.into(), p_any_ai: p }
    }

    fn pool(scores: &[Score], writer: &str) -> HashMap<String, PoolFacts> {
        scores.iter().map(|s| (s.id.clone(), PoolFacts { text_sha256: s.text_sha256.clone(),
            source_group: s.source_group.clone(), writer_key: Some(format!("{writer}-{}", s.source_group)) })).collect()
    }

    #[test]
    fn selection_ranks_caps_and_excludes() {
        let scores = vec![score("a", "g1", 0.9), score("b", "g1", 0.95), score("c", "g1", 0.8),
                          score("d", "g2", 0.7), score("e", "g3", 0.99), score("f", "g4", 0.2),
                          score("g", "g5", 0.85), score("h", "g6", 0.6)];
        let pool = pool(&scores, "w");
        let mut excl = Exclusions::default();
        excl.families.insert("g3".into());
        excl.writers.insert("w-g5".into());
        excl.ids.insert("h".into());
        let cfg = SelectConfig { round: 1, top: 3, per_group_cap: 2, min_p: 0.5, seed: "s".into() };
        let (chosen, reasons) = select(&scores, &pool, &excl, &cfg).unwrap();
        assert_eq!(chosen.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["b", "a", "d"]);
        assert_eq!(reasons["family_in_evaluation"], 1);
        assert_eq!(reasons["writer_in_evaluation"], 1);
        assert_eq!(reasons["id_in_bundle"], 1);
        assert_eq!(reasons["below_min_p"], 1);
        assert_eq!(reasons["per_group_cap"], 1);
        let mut bad = scores.clone();
        bad.push(score("zz", "g9", 0.5));
        assert!(select(&bad, &pool, &excl, &cfg).is_err(), "unknown pool id must fail");
        let mut changed = scores.clone();
        changed[0].text_sha256 = "other".into();
        assert!(select(&changed, &pool, &excl, &cfg).is_err());
        assert!(select(&scores, &pool, &excl, &SelectConfig { round: 4, ..cfg.clone() }).is_err());
    }

    #[test]
    fn ties_break_by_seeded_key_deterministically() {
        let scores = vec![score("x", "g1", 0.5), score("y", "g2", 0.5), score("z", "g3", 0.5)];
        let pool = pool(&scores, "w");
        let cfg = SelectConfig { round: 1, top: 3, per_group_cap: 9, min_p: 0.0, seed: "s1".into() };
        let a = select(&scores, &pool, &Exclusions::default(), &cfg).unwrap().0;
        let b = select(&scores, &pool, &Exclusions::default(), &cfg).unwrap().0;
        assert_eq!(a, b);
        let mut keys: Vec<_> = ["x", "y", "z"].iter().map(|i| (tie_key("s1", i), *i)).collect();
        keys.sort();
        assert_eq!(a.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
                   keys.iter().map(|k| k.1).collect::<Vec<_>>());
    }

    #[test]
    fn stop_rule_follows_protocol() {
        assert_eq!(stop_rule(&[0.08]).unwrap().next_round, Some(1));
        let d = stop_rule(&[0.08, 0.075]).unwrap();
        assert!(!d.stop && d.next_round == Some(2));
        let d = stop_rule(&[0.08, 0.075, 0.0745]).unwrap();
        assert!(d.stop && d.next_round.is_none());
        assert!(stop_rule(&[0.08, 0.081]).unwrap().stop, "a worse round stops");
        let d = stop_rule(&[0.08, 0.07, 0.06, 0.05]).unwrap();
        assert!(d.stop && d.reason.contains("three"));
        assert!(stop_rule(&[0.1, 0.09, 0.08, 0.07, 0.06]).is_err());
        assert!(stop_rule(&[]).is_err());
        assert!(stop_rule(&[f64::NAN]).is_err());
    }

    fn doc(id: &str, text_value: &str) -> Value {
        json!({"id": id, "text": text_value, "text_sha256": sha256_hex(text_value.as_bytes()),
               "source_group": format!("fam-{id}"), "domain": "fineweb", "writer_key": "w", "word_count": 3,
               "frozen_split": "train_mining"})
    }

    #[test]
    fn rows_requests_and_extension() {
        let d = doc("p1", "one two three");
        let row = mined_row(&d, 2, 1.0, &json!({"pool_id": "p1"})).unwrap();
        assert_eq!(row["cohort_id"], "mined_humans_round_2");
        assert_eq!(row["origin"], "human_only");
        assert!(mined_row(&json!({"id": "x", "text": "a", "text_sha256": "0", "source_group": "g", "domain": "d"}),
                          1, 1.0, &json!({})).is_err());
        let gens = vec!["qwen36".to_owned(), "phi4mini".to_owned(), "mistral7b".to_owned()];
        let kinds = vec!["mirror".to_owned(), "mixed".to_owned()];
        let r = requests_for(&d, 1, 2, &kinds, &gens).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!((r[0]["generator"].as_str(), r[1]["generator"].as_str()), (Some("mistral7b"), Some("qwen36")));
        assert_ne!(r[0]["request_id"], r[1]["request_id"]);

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        fs::create_dir(&base).unwrap();
        let human = json!({"id": "h1", "split": "train", "text": "a b", "text_sha256": sha256_hex(b"a b"),
                           "origin": "human_only", "share": 0.0, "generator_label": null, "cohort_id": "humans_v1",
                           "cohort_weight": 1.0, "source_group": "g", "domain": "wikipedia", "held_out_style": false,
                           "provenance_sha256": "p"});
        fs::write(base.join("rows.jsonl"), format!("{human}\n")).unwrap();
        fs::write(base.join("weights.json"), json!({"schema": "x", "base_weighting": "family_origin_balanced",
            "cohorts": {"humans_v1": {"policy": "uniform_v1", "default": true,
                                      "labels": {"human": {"records": 1, "weight": 1.0}}}}}).to_string()).unwrap();
        fs::write(base.join("MANIFEST.json"), "{}").unwrap();
        let mined = dir.path().join("mined.jsonl");
        fs::write(&mined, format!("{row}\n")).unwrap();
        let out = dir.path().join("next");
        let m = extend_bundle(&base, &[&mined], &out).unwrap();
        let w: Value = serde_json::from_slice(&fs::read(out.join("weights.json")).unwrap()).unwrap();
        assert_eq!(w["cohorts"]["mined_humans_round_2"]["labels"]["human"]["records"], 1);
        assert_eq!(w["cohorts"]["mined_humans_round_2"]["policy"], "mined_humans_v1");
        assert_eq!(m["counts"].as_array().unwrap().len(), 2);
        assert_eq!(fs::read_to_string(out.join("rows.jsonl")).unwrap().lines().count(), 2);
        // Re-appending the same rows fails (duplicate id), as does a non-Train row.
        assert!(extend_bundle(&out, &[&mined], &dir.path().join("again")).is_err());
        let eval_row = dir.path().join("eval.jsonl");
        let mut e = row.clone();
        e["split"] = json!("cal");
        e["id"] = json!("other");
        e["text_sha256"] = json!("t2");
        fs::write(&eval_row, format!("{e}\n")).unwrap();
        assert!(extend_bundle(&base, &[&eval_row], &dir.path().join("bad")).is_err());
    }

    fn write_sums(dir: &Path, files: &[&str]) {
        let mut sums = String::new();
        for f in files {
            sums.push_str(&format!("{}  ./{f}\n", sha256_file(&dir.join(f)).unwrap()));
        }
        fs::write(dir.join("SHA256SUMS"), sums).unwrap();
    }

    fn put(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn splits_dir_requires_every_evaluation_manifest_and_accepts_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mut files = vec![];
        for part in ["", "new-domains/"] {
            for split in ["cal", "dev-iter", "dev-select", "test"] {
                let rel = format!("{part}{split}/manifest.jsonl");
                let s = canonical_split(split).unwrap();
                put(d, &rel, &format!("{}\n", json!({"split": s, "family_key": format!("fam-{part}{split}"),
                    "group_key": format!("w-{part}{split}"), "text_sha256": format!("t-{part}{split}")})));
                files.push(rel);
            }
        }
        put(d, "train/manifest.jsonl", "{\"split\":\"train\"}\n");
        files.push("train/manifest.jsonl".into());
        put(d, "diagnostics/ell/manifest.jsonl", &format!("{}\n", json!({"split": "diagnostic_ell",
            "source_group": "fam-diag", "writer_key": "w-diag", "text_sha256": "t-diag"})));
        files.push("diagnostics/ell/manifest.jsonl".into());
        put(d, "reserved-unassigned/groups.jsonl", &format!("{}\n", json!({"family_keys": ["fam-res"],
            "group_key": "w-res"})));
        files.push("reserved-unassigned/groups.jsonl".into());
        put(d, "blogs/evaluation-reserved/x/families.txt", "fam-blog\n");
        files.push("blogs/evaluation-reserved/x/families.txt".into());
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        write_sums(d, &refs);
        let mut excl = Exclusions::default();
        let sources = excl.add_splits_dir(d).unwrap();
        for f in ["fam-cal", "fam-new-domains/test", "fam-diag", "fam-res", "fam-blog"] {
            assert!(excl.families.contains(f), "{f}");
        }
        assert!(excl.writers.contains("w-diag") && excl.writers.contains("w-res") && excl.texts.contains("t-diag"));
        assert!(!excl.families.iter().any(|f| f.contains("train")));
        assert_eq!(sources.iter().filter(|s| s["kind"] == "evaluation_manifest").count(), 8);
        // A part missing one split is refused.
        let missing: Vec<&str> = refs.iter().copied().filter(|f| *f != "new-domains/cal/manifest.jsonl").collect();
        write_sums(d, &missing);
        assert!(Exclusions::default().add_splits_dir(d).unwrap_err().to_string().contains("new-domains"));
        // A changed file and an unrecognised manifest are refused.
        write_sums(d, &refs);
        put(d, "dev-iter/manifest.jsonl", "{\"split\":\"dev_iter\",\"family_key\":\"changed\"}\n");
        assert!(Exclusions::default().add_splits_dir(d).is_err());
        write_sums(d, &refs);
        put(d, "odd/manifest.jsonl", "{}\n");
        let mut odd = refs.clone();
        odd.push("odd/manifest.jsonl");
        write_sums(d, &odd);
        assert!(Exclusions::default().add_splits_dir(d).unwrap_err().to_string().contains("unrecognised"));
        // A manifest whose rows name another split than its directory is refused.
        put(d, "cal/manifest.jsonl", &format!("{}\n", json!({"split": "test", "family_key": "f"})));
        write_sums(d, &refs);
        assert!(Exclusions::default().add_splits_dir(d).is_err());
    }

    fn fake_fit(dir: &Path, seed: i64, bundle: &Value, dev: &str, brier: f64, arm: &str) {
        fs::create_dir_all(dir).unwrap();
        let b = json!({"seed": seed, "arm": arm, "arm_definition": {"x": 1}, "protocol_sha256": "p",
                       "bundle_sha256": bundle, "dev_iter_rows_sha256": dev});
        fs::write(dir.join("fit.json"), json!({"schema": "slop_ninja_full_a_fit_v2", "complete": true,
            "selection_split": "dev_iter", "final_test_opened": false, "cohort_weight_log": {"complete": true},
            "binding": b, "dev_iter_rows_sha256": dev, "selection_split_involvement_brier": brier}).to_string())
            .unwrap();
    }

    #[test]
    fn stop_requires_comparable_rounds_and_bundle_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let base = d.join("b0");
        fs::create_dir(&base).unwrap();
        let human = json!({"id": "h1", "split": "train", "text": "a b", "text_sha256": sha256_hex(b"a b"),
                           "origin": "human_only", "share": 0.0, "generator_label": null, "cohort_id": "humans_v1",
                           "cohort_weight": 1.0, "source_group": "g", "domain": "wikipedia", "held_out_style": false,
                           "provenance_sha256": "p"});
        fs::write(base.join("rows.jsonl"), format!("{human}\n")).unwrap();
        fs::write(base.join("weights.json"), json!({"base_weighting": "family_origin_balanced",
            "cohorts": {"humans_v1": {"policy": "uniform_v1", "default": true,
                                      "labels": {"human": {"records": 1, "weight": 1.0}}}}}).to_string()).unwrap();
        fs::write(base.join("MANIFEST.json"), "{}").unwrap();
        let mined = d.join("mined.jsonl");
        let row = mined_row(&doc("p1", "one two three"), 1, 1.0, &json!({})).unwrap();
        fs::write(&mined, format!("{row}\n")).unwrap();
        let b1 = d.join("b1");
        extend_bundle(&base, &[&mined], &b1).unwrap();
        let (g0, g1) = (bundle_digests(&base).unwrap(), bundle_digests(&b1).unwrap());
        for (name, g, seed, brier) in [("r0s17", &g0, 17, 0.08), ("r0s29", &g0, 29, 0.09),
                                        ("r1s17", &g1, 17, 0.07), ("r1s29", &g1, 29, 0.075)] {
            fake_fit(&d.join(name), seed, g, "dev", brier, "E1");
        }
        let read = |names: &[&str]| names.iter().map(|n| RoundFit::read(&d.join(n)).unwrap()).collect::<Vec<_>>();
        let rounds = vec![read(&["r0s17", "r0s29"]), read(&["r1s17", "r1s29"])];
        let (means, records) = comparable_rounds(&rounds, &[&base, &b1]).unwrap();
        assert!((means[0] - 0.085).abs() < 1e-12 && (means[1] - 0.0725).abs() < 1e-12);
        assert_eq!(records[1]["appended_cohorts"], json!(["mined_humans_round_1"]));
        // Different seed sets, Dev-iter rows, arms or bundles are refused.
        fake_fit(&d.join("r1s41"), 41, &g1, "dev", 0.07, "E1");
        assert!(comparable_rounds(&[read(&["r0s17", "r0s29"]), read(&["r1s17", "r1s41"])], &[&base, &b1]).is_err());
        fake_fit(&d.join("r1dev"), 29, &g1, "other-dev", 0.07, "E1");
        assert!(comparable_rounds(&[read(&["r0s17", "r0s29"]), read(&["r1s17", "r1dev"])], &[&base, &b1])
            .unwrap_err().to_string().contains("Dev-iter"));
        fake_fit(&d.join("r1arm"), 29, &g1, "dev", 0.07, "E2");
        assert!(comparable_rounds(&[read(&["r0s17", "r0s29"]), read(&["r1s17", "r1arm"])], &[&base, &b1]).is_err());
        assert!(comparable_rounds(&rounds, &[&base, &base]).is_err(), "round-1 fits were not on the base bundle");
        // A round-1 bundle that does not extend round 0 is refused.
        let other = d.join("other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("rows.jsonl"), format!("{human}\n")).unwrap();
        fs::write(other.join("weights.json"), "{\"cohorts\":{}}").unwrap();
        fs::write(other.join("MANIFEST.json"), "{\"x\":1}").unwrap();
        let b1x = d.join("b1x");
        extend_bundle(&other, &[&mined], &b1x).unwrap();
        let gx = bundle_digests(&b1x).unwrap();
        fake_fit(&d.join("x17"), 17, &gx, "dev", 0.07, "E1");
        fake_fit(&d.join("x29"), 29, &gx, "dev", 0.07, "E1");
        assert!(comparable_rounds(&[read(&["r0s17", "r0s29"]), read(&["x17", "x29"])], &[&base, &b1x]).is_err());
    }
}

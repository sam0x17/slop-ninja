//! Splits successor: a new splits directory that keeps every evaluation file
//! of its base byte for byte (hard links) and rewrites only the Train side.
//!
//! The successor applies three Train-side rules, each recorded per removed or
//! moved row in `successor-changes.jsonl` (ids, reasons and measures only,
//! never text):
//!
//! - **OCR screen and cap** (`ocr_screen_v1`). Rows of the OCR newspaper
//!   domains in Train and Train-mining pass only when every measure is within
//!   its fixed threshold: mid-word case flips, broken line-end hyphenation,
//!   the non-word (out-of-lexicon) share of alphabetic tokens and the odd-token
//!   share (`grammar_a_pools::ocr::Quality`, the same lexicon as the
//!   adapters). Train-mining then keeps at most `cap_train_mining` passing rows
//!   per domain, OCR tier `a` before `b`, then by SHA-256 of salt and id.
//! - **Private-use glyphs** (`private_use_screen_v1`). Rows of the listed
//!   domains that contain a private-use code point (U+E000..U+F8FF and the
//!   supplementary private-use planes) or U+FFFD, which is symbol-font
//!   residue, are dropped whole. The text of a kept row is never edited.
//! - **ELLIPSE half split** (`ellipse_prompt_half_v1`). The essay prompts of
//!   the ELLIPSE diagnostic are ranked by SHA-256 of salt and prompt key; the
//!   prompts of even rank move to Train (every essay of the prompt, with
//!   `frozen_split` and `split` set to `train`), the others stay in the
//!   ungated held-out non-native diagnostic. A prompt that is an evaluation
//!   group key of any split always stays in the diagnostic.
//!
//! Every other file is a hard link to the base file, so evaluation
//! assignments cannot change. `SHA256SUMS` is rewritten for the changed files
//! and `SUCCESSOR.json` records the base, the rules, the thresholds, the
//! lexicon digest and the counts.
use crate::consolidate::for_lines;
use crate::sha_hex;
use anyhow::{Context, Result, bail, ensure};
use grammar_a_pools::ocr::{Lexicon, Quality};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

pub const CONFIG_SCHEMA: &str = "slop_ninja_a_splits_successor_config_v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OcrRule {
    pub domains: Vec<String>,
    pub max_case_flip_rate: f64,
    pub max_broken_hyphen_rate: f64,
    pub max_nonword_rate: f64,
    pub max_odd_token_rate: f64,
    pub cap_train_mining: usize,
    pub salt: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PuaRule {
    pub domains: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EllipseRule {
    /// Diagnostic slice directory name under `diagnostics/`.
    pub slice: String,
    pub salt: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema: String,
    pub base: PathBuf,
    /// Version tag written into moved rows (`splits_version`).
    pub version: String,
    pub lexicon: PathBuf,
    pub ocr: OcrRule,
    pub pua: PuaRule,
    pub ellipse: EllipseRule,
}

/// OCR measures of one text.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OcrMeasure {
    pub alpha_tokens: u64,
    pub case_flips: u64,
    pub broken_hyphens: u64,
    pub case_flip_rate: f64,
    pub broken_hyphen_rate: f64,
    pub nonword_rate: f64,
    pub odd_token_rate: f64,
}

/// Mixed-case surname prefixes that are not case flips (McDonald, MacArthur, O'Neil, ...).
const NAME_PREFIXES: [&str; 8] = ["Mc", "Mac", "O'", "D'", "De", "La", "Le", "Van"];

/// A word with an uppercase letter right after a lowercase one (`tHe`, `thE`),
/// other than a surname prefix followed by a capitalised remainder.
pub fn case_flip(word: &str) -> bool {
    let cs: Vec<char> = word.chars().collect();
    if cs.len() < 2 || !cs.iter().all(|c| c.is_alphabetic() || *c == '\'') {
        return false;
    }
    let flip = cs.windows(2).any(|w| w[0].is_lowercase() && w[1].is_uppercase());
    if !flip {
        return false;
    }
    for p in NAME_PREFIXES {
        if let Some(rest) = word.strip_prefix(p) {
            let rc: Vec<char> = rest.chars().collect();
            if rc.len() >= 2 && rc[0].is_uppercase() && rc[1..].iter().all(|c| c.is_lowercase()) {
                return false;
            }
        }
    }
    true
}

pub fn measure(text: &str, lex: &Lexicon) -> OcrMeasure {
    let q = Quality::measure(text, lex);
    let toks: Vec<&str> = text.split_whitespace().collect();
    let mut flips = 0u64;
    let mut broken = 0u64;
    for (i, t) in toks.iter().enumerate() {
        let core = t.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'');
        if case_flip(core) {
            flips += 1;
        }
        // `infor- mation`: a word ending in letter + hyphen followed by a lowercase-initial word.
        let tc: Vec<char> = t.chars().collect();
        if tc.len() >= 3 && tc[tc.len() - 1] == '-' && tc[tc.len() - 2].is_alphabetic() {
            if let Some(n) = toks.get(i + 1) {
                if n.chars().next().is_some_and(|c| c.is_lowercase()) {
                    broken += 1;
                }
            }
        }
    }
    let a = q.alpha_tokens.max(1) as f64;
    OcrMeasure {
        alpha_tokens: q.alpha_tokens,
        case_flips: flips,
        broken_hyphens: broken,
        case_flip_rate: flips as f64 / a,
        broken_hyphen_rate: broken as f64 / a,
        nonword_rate: 1.0 - q.lexicon_rate(),
        odd_token_rate: q.odd_token_rate(),
    }
}

impl OcrRule {
    /// The first measure above its threshold, if any.
    pub fn failure(&self, m: &OcrMeasure) -> Option<&'static str> {
        if m.case_flip_rate > self.max_case_flip_rate {
            Some("ocr_case_flip_rate_above_threshold")
        } else if m.broken_hyphen_rate > self.max_broken_hyphen_rate {
            Some("ocr_broken_hyphen_rate_above_threshold")
        } else if m.nonword_rate > self.max_nonword_rate {
            Some("ocr_nonword_rate_above_threshold")
        } else if m.odd_token_rate > self.max_odd_token_rate {
            Some("ocr_odd_token_rate_above_threshold")
        } else {
            None
        }
    }
}

/// Private-use code points or the replacement character (symbol-font residue).
pub fn has_private_use(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD | 0xFFFD)
    })
}

fn rank_key(salt: &str, id: &str) -> String {
    sha_hex(format!("{salt}\x1f{id}").as_bytes())
}

/// Prompts that move to Train: even rank by SHA-256 of salt and prompt key,
/// never an evaluation group key.
pub fn ellipse_train_prompts(prompts: &BTreeSet<String>, salt: &str, eval_keys: &HashSet<String>) -> BTreeSet<String> {
    let mut ranked: Vec<(String, &String)> = prompts.iter().map(|p| (rank_key(salt, p), p)).collect();
    ranked.sort();
    ranked
        .into_iter()
        .enumerate()
        .filter(|(i, (_, p))| i % 2 == 0 && !eval_keys.contains(*p))
        .map(|(_, (_, p))| p.clone())
        .collect()
}

fn read_jsonl(p: &Path) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for_lines(p, |l| {
        out.push(serde_json::from_slice(l)?);
        Ok(())
    })?;
    Ok(out)
}

/// Group keys of every evaluation file of the base (metadata only).
fn evaluation_group_keys(base: &Path) -> Result<HashSet<String>> {
    let mut keys = HashSet::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d)? {
            let p = e?.path();
            if p.is_dir() {
                let n = p.file_name().unwrap().to_string_lossy().into_owned();
                if !["train", "train-mining", "diagnostics", "held", "base-v2", "base-v3"].contains(&n.as_str()) {
                    stack.push(p);
                }
            } else if matches!(p.file_name().and_then(|n| n.to_str()), Some("manifest.jsonl" | "groups.jsonl")) {
                for_lines(&p, |l| {
                    let v: Value = serde_json::from_slice(l)?;
                    for k in ["source_group", "group_key", "family_key"] {
                        if let Some(s) = v[k].as_str() {
                            keys.insert(s.to_string());
                        }
                    }
                    for s in v["family_keys"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        keys.insert(s.to_string());
                    }
                    Ok(())
                })?;
            }
        }
    }
    Ok(keys)
}

/// Text-free quantiles of the OCR measures per domain over Train and
/// Train-mining rows of `domains` (to choose and record thresholds).
pub fn ocr_profile(base: &Path, lexicon: &Path, domains: &[String]) -> Result<Value> {
    let lex = Lexicon::parse(&fs::read_to_string(lexicon)?);
    let want: HashSet<&str> = domains.iter().map(String::as_str).collect();
    let mut out = serde_json::Map::new();
    for split in ["train", "train-mining"] {
        let dom = manifest_domains(&base.join(split).join("manifest.jsonl"), |d| want.contains(d))?;
        let mut per: BTreeMap<String, Vec<OcrMeasure>> = BTreeMap::new();
        let mut tiers: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
        for_lines(&base.join(split).join("documents.jsonl"), |l| {
            let id = line_id(l)?;
            if let Some(d) = dom.get(id.as_str()) {
                let v: Value = serde_json::from_slice(l)?;
                per.entry(d.clone()).or_default().push(measure(v["text"].as_str().context("text")?, &lex));
                *tiers.entry(d.clone()).or_default().entry(v["ocr_tier"].as_str().unwrap_or("none").into()).or_default() += 1;
            }
            Ok(())
        })?;
        let mut s = serde_json::Map::new();
        for (d, ms) in per {
            let q = |f: fn(&OcrMeasure) -> f64| -> Value {
                let mut v: Vec<f64> = ms.iter().map(f).collect();
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let at = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
                json!({"p50": at(0.5), "p75": at(0.75), "p90": at(0.9), "p95": at(0.95), "p99": at(0.99), "max": at(1.0)})
            };
            s.insert(d.clone(), json!({"rows": ms.len(), "ocr_tier": tiers[&d],
                "case_flip_rate": q(|m| m.case_flip_rate), "broken_hyphen_rate": q(|m| m.broken_hyphen_rate),
                "nonword_rate": q(|m| m.nonword_rate), "odd_token_rate": q(|m| m.odd_token_rate)}));
        }
        out.insert(split.into(), Value::Object(s));
    }
    Ok(json!({"lexicon_sha256": lex.sha256, "domains": domains, "splits": out}))
}

fn line_id(l: &[u8]) -> Result<String> {
    // Every split row starts its object with keys in sorted order; parse only what is needed.
    #[derive(Deserialize)]
    struct Id {
        id: String,
    }
    Ok(serde_json::from_slice::<Id>(l).context("row id")?.id)
}

/// id -> domain for manifest rows whose domain passes `keep`.
fn manifest_domains(p: &Path, keep: impl Fn(&str) -> bool) -> Result<HashMap<String, String>> {
    #[derive(Deserialize)]
    struct Row {
        id: String,
        domain: String,
    }
    let mut out = HashMap::new();
    for_lines(p, |l| {
        let r: Row = serde_json::from_slice(l).context("manifest row")?;
        if keep(&r.domain) {
            out.insert(r.id, r.domain);
        }
        Ok(())
    })?;
    Ok(out)
}

/// One Train-side change, recorded without text.
#[derive(Clone, Debug, Serialize)]
pub struct Change {
    pub id: String,
    pub split: String,
    pub domain: String,
    pub action: &'static str,
    pub reason: String,
    pub detail: Value,
}

struct Writer {
    w: BufWriter<fs::File>,
    h: sha2::Sha256,
    n: usize,
}

impl Writer {
    fn new(p: &Path) -> Result<Writer> {
        Ok(Writer { w: BufWriter::with_capacity(1 << 20, fs::File::create_new(p).with_context(|| format!("create {}", p.display()))?), h: sha2::Digest::new(), n: 0 })
    }
    fn line(&mut self, b: &[u8]) -> Result<()> {
        self.w.write_all(b)?;
        self.w.write_all(b"\n")?;
        sha2::Digest::update(&mut self.h, b);
        sha2::Digest::update(&mut self.h, b"\n");
        self.n += 1;
        Ok(())
    }
    fn finish(self) -> Result<(String, usize)> {
        let f = self.w.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        Ok((hex::encode(sha2::Digest::finalize(self.h)), self.n))
    }
}

/// Hard-link every file of `base` into `out` except `skip` (relative paths).
fn link_tree(base: &Path, out: &Path, rel: &Path, skip: &HashSet<PathBuf>, linked: &mut usize) -> Result<()> {
    for e in fs::read_dir(base.join(rel))? {
        let e = e?;
        let r = rel.join(e.file_name());
        let src = base.join(&r);
        if src.is_dir() {
            fs::create_dir_all(out.join(&r))?;
            link_tree(base, out, &r, skip, linked)?;
        } else if !skip.contains(&r) {
            fs::hard_link(&src, out.join(&r)).with_context(|| format!("link {}", src.display()))?;
            *linked += 1;
        }
    }
    Ok(())
}

/// Write the successor of `cfg.base` into the new directory `out`.
pub fn successor(cfg_path: &Path, out: &Path, code: &Value) -> Result<Value> {
    let cfg_bytes = fs::read(cfg_path)?;
    let cfg: Config = serde_json::from_slice(&cfg_bytes).context("successor config")?;
    ensure!(cfg.schema == CONFIG_SCHEMA, "config schema must be {CONFIG_SCHEMA}");
    ensure!(!out.exists(), "{} exists; a successor is written once", out.display());
    let base = cfg.base.clone();
    let lex_text = fs::read_to_string(&cfg.lexicon)?;
    let lex = Lexicon::parse(&lex_text);
    ensure!(!lex.is_empty(), "empty lexicon");
    let eval_keys = evaluation_group_keys(&base)?;
    let ocr_domains: HashSet<&str> = cfg.ocr.domains.iter().map(String::as_str).collect();
    let pua_domains: HashSet<&str> = cfg.pua.domains.iter().map(String::as_str).collect();
    let tmp = out.with_extension("tmp-successor");
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(&tmp)?;
    let diag = PathBuf::from("diagnostics").join(&cfg.ellipse.slice);
    let rewritten: Vec<PathBuf> = vec![
        "train/documents.jsonl".into(), "train/manifest.jsonl".into(),
        "train-mining/documents.jsonl".into(), "train-mining/manifest.jsonl".into(),
        diag.join("documents.jsonl"), diag.join("manifest.jsonl"), diag.join("SLICE.json"),
        "SHA256SUMS".into(),
    ];
    let skip: HashSet<PathBuf> = rewritten.iter().cloned().collect();
    let mut linked = 0usize;
    link_tree(&base, &tmp, Path::new(""), &skip, &mut linked)?;

    let mut changes: Vec<Change> = Vec::new();
    let mut drop: HashMap<String, (String, String)> = HashMap::new(); // id -> (split, reason)
    let mut new_sha: BTreeMap<String, (String, usize)> = BTreeMap::new();
    let mut counts = serde_json::Map::new();

    // Pass 1 (Train and Train-mining): screen the OCR and private-use domains.
    for split in ["train", "train-mining"] {
        let dom = manifest_domains(&base.join(split).join("manifest.jsonl"), |d| ocr_domains.contains(d) || pua_domains.contains(d))?;
        // passing OCR rows of Train-mining: domain -> (tier rank, salt key, id)
        let mut passing: BTreeMap<String, Vec<(u8, String, String)>> = BTreeMap::new();
        let mut screened: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
        let mut pua_any: BTreeMap<String, usize> = BTreeMap::new();
        for_lines(&base.join(split).join("documents.jsonl"), |l| {
            let Some(d) = dom.get(&line_id(l)?) else { return Ok(()) };
            let v: Value = serde_json::from_slice(l)?;
            let id = v["id"].as_str().context("id")?.to_string();
            let text = v["text"].as_str().context("text")?;
            let e = screened.entry(d.clone()).or_default();
            *e.entry("rows".into()).or_default() += 1;
            if pua_domains.contains(d.as_str()) && has_private_use(text) {
                *pua_any.entry(d.clone()).or_default() += 1;
                *e.entry("private_use_glyphs".into()).or_default() += 1;
                drop.insert(id.clone(), (split.into(), "private_use_glyphs".into()));
                changes.push(Change { id, split: split.into(), domain: d.clone(), action: "dropped",
                    reason: "private_use_glyphs".into(), detail: Value::Null });
                return Ok(());
            }
            if ocr_domains.contains(d.as_str()) {
                let m = measure(text, &lex);
                if let Some(why) = cfg.ocr.failure(&m) {
                    *e.entry(why.into()).or_default() += 1;
                    drop.insert(id.clone(), (split.into(), why.into()));
                    changes.push(Change { id, split: split.into(), domain: d.clone(), action: "dropped",
                        reason: why.into(), detail: serde_json::to_value(&m)? });
                    return Ok(());
                }
                *e.entry("ocr_passed".into()).or_default() += 1;
                if split == "train-mining" {
                    let tier = match v["ocr_tier"].as_str() { Some("a") => 0, Some("b") => 1, _ => 2 };
                    passing.entry(d.clone()).or_default().push((tier, rank_key(&cfg.ocr.salt, &id), id));
                }
            }
            Ok(())
        })?;
        for (d, mut v) in passing {
            v.sort();
            let e = screened.entry(d.clone()).or_default();
            for (tier, _, id) in v.into_iter().skip(cfg.ocr.cap_train_mining) {
                *e.entry("over_cap".into()).or_default() += 1;
                drop.insert(id.clone(), (split.into(), "ocr_train_mining_cap".into()));
                changes.push(Change { id, split: split.into(), domain: d.clone(), action: "dropped",
                    reason: "ocr_train_mining_cap".into(), detail: json!({"ocr_tier_rank": tier}) });
            }
        }
        counts.insert(split.into(), json!({"screened": screened, "private_use_rows": pua_any}));
    }

    // ELLIPSE: prompts by rank; Train half moves.
    let diag_docs = read_jsonl(&base.join(&diag).join("documents.jsonl"))?;
    let mut prompt_of: HashMap<String, String> = HashMap::new();
    let mut prompts = BTreeSet::new();
    for d in &diag_docs {
        let p = d["provenance"]["prompt_key"].as_str().context("ELLIPSE prompt_key")?.to_string();
        prompts.insert(p.clone());
        prompt_of.insert(d["id"].as_str().context("id")?.into(), p);
    }
    let to_train = ellipse_train_prompts(&prompts, &cfg.ellipse.salt, &eval_keys);
    let blocked: Vec<&String> = prompts.iter().filter(|p| eval_keys.contains(*p)).collect();

    // Pass 2: write Train and Train-mining without dropped rows; Train gains the ELLIPSE half.
    let moved_doc = |v: &Value| -> Result<Vec<u8>> {
        let mut v = v.clone();
        v["frozen_split"] = json!("train");
        v["split_assignment"] = json!({"schema": "slop_ninja_a_competitive_split_document_v1", "split": "train",
            "splits_version": cfg.version, "rule": format!("{}: prompt moved from the ELLIPSE diagnostic to Train by SHA-256 rank of salt and prompt key", cfg.ellipse.salt)});
        Ok(serde_json::to_vec(&v)?)
    };
    let moved_manifest = |v: &Value| -> Result<Vec<u8>> {
        let mut v = v.clone();
        v["split"] = json!("train");
        v["held_reasons"] = Value::Null;
        v["splits_version"] = json!(cfg.version);
        Ok(serde_json::to_vec(&v)?)
    };
    let diag_manifest = read_jsonl(&base.join(&diag).join("manifest.jsonl"))?;
    for split in ["train", "train-mining"] {
        for file in ["documents.jsonl", "manifest.jsonl"] {
            let rel = format!("{split}/{file}");
            let mut w = Writer::new(&tmp.join(&rel))?;
            let mut dropped = 0usize;
            for_lines(&base.join(&rel), |l| {
                if drop.get(&line_id(l)?).is_some_and(|(s, _)| s == split) {
                    dropped += 1;
                    return Ok(());
                }
                w.line(l)
            })?;
            if split == "train" {
                let rows = if file == "documents.jsonl" { &diag_docs } else { &diag_manifest };
                for v in rows {
                    let id = v["id"].as_str().context("id")?;
                    if to_train.contains(&prompt_of[id]) {
                        w.line(&if file == "documents.jsonl" { moved_doc(v)? } else { moved_manifest(v)? })?;
                    }
                }
            }
            let expect = drop.values().filter(|(s, _)| s == split).count();
            ensure!(dropped == expect, "{rel}: dropped {dropped} rows, expected {expect}");
            new_sha.insert(rel, w.finish()?);
        }
    }
    let mut moved = 0usize;
    for v in &diag_docs {
        let id = v["id"].as_str().unwrap();
        if to_train.contains(&prompt_of[id]) {
            moved += 1;
            changes.push(Change { id: id.into(), split: "train".into(), domain: "ellipse".into(), action: "moved_to_train",
                reason: cfg.ellipse.salt.clone(), detail: json!({"prompt_key": prompt_of[id]}) });
        }
    }
    // The held-out half stays in the diagnostic.
    for (file, rows) in [("documents.jsonl", &diag_docs), ("manifest.jsonl", &diag_manifest)] {
        let rel = diag.join(file);
        let mut w = Writer::new(&tmp.join(&rel))?;
        for (v, line) in rows.iter().zip(raw_lines(&base.join(&rel))?) {
            ensure!(v["id"] == serde_json::from_slice::<Value>(&line)?["id"], "diagnostic row order");
            if !to_train.contains(&prompt_of[v["id"].as_str().unwrap()]) {
                w.line(&line)?;
            }
        }
        new_sha.insert(rel.to_string_lossy().into_owned(), w.finish()?);
    }
    let held_docs = new_sha[&diag.join("documents.jsonl").to_string_lossy().into_owned()].clone();
    let held_man = new_sha[&diag.join("manifest.jsonl").to_string_lossy().into_owned()].clone();
    let mut slice: Value = serde_json::from_slice(&fs::read(base.join(&diag).join("SLICE.json"))?)?;
    let held_prompts: Vec<&String> = prompts.iter().filter(|p| !to_train.contains(*p)).collect();
    slice["documents"] = json!(held_docs.1);
    slice["documents_sha256"] = json!(held_docs.0);
    slice["manifest_sha256"] = json!(held_man.0);
    slice["spec"]["basis"] = json!(format!(
        "held-out half of the ELLIPSE essay prompts ({}: SHA-256 rank of salt and prompt key, odd ranks and evaluation prompts held); the other half is Train. Ungated non-native-writer diagnostic: humans only, positives generated after the seal", cfg.ellipse.salt));
    slice["spec"]["held_prompts"] = json!(held_prompts);
    slice["spec"]["train_prompts"] = json!(to_train);
    slice["splits_version"] = json!(cfg.version);
    let slice_b = serde_json::to_vec_pretty(&slice)?;
    fs::write(tmp.join(&diag).join("SLICE.json"), &slice_b)?;
    new_sha.insert(diag.join("SLICE.json").to_string_lossy().into_owned(), (sha_hex(&slice_b), 1));

    // Changes, SUCCESSOR.json and SHA256SUMS.
    changes.sort_by(|a, b| (&a.split, &a.id).cmp(&(&b.split, &b.id)));
    let mut cw = Writer::new(&tmp.join("successor-changes.jsonl"))?;
    let mut by_reason: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for c in &changes {
        cw.line(&serde_json::to_vec(c)?)?;
        *by_reason.entry(format!("{}:{}", c.split, c.reason)).or_default().entry(c.domain.clone()).or_default() += 1;
    }
    let changes_sha = cw.finish()?;
    let base_sums = fs::read_to_string(base.join("SHA256SUMS"))?;
    let mut sums = String::new();
    let mut listed = HashSet::new();
    for line in base_sums.lines().filter(|l| !l.trim().is_empty()) {
        let (sha, path) = line.split_once("  ").context("SHA256SUMS line")?;
        let rel = path.trim_start_matches("./").to_string();
        listed.insert(rel.clone());
        match new_sha.get(&rel) {
            Some((s, _)) => sums.push_str(&format!("{s}  ./{rel}\n")),
            None => {
                // Unchanged files keep their digest; verify the link target still matches.
                ensure!(crate::bundle::file_sha(&tmp.join(&rel))? == sha, "{rel} differs from the base SHA256SUMS");
                sums.push_str(&format!("{sha}  ./{rel}\n"));
            }
        }
    }
    for rel in new_sha.keys() {
        ensure!(listed.contains(rel), "{rel} is not listed in the base SHA256SUMS");
    }
    sums.push_str(&format!("{}  ./successor-changes.jsonl\n", changes_sha.0));
    let report = json!({
        "schema": "slop_ninja_a_splits_successor_v1", "version": cfg.version, "base": base,
        "base_sha256sums_sha256": sha_hex(base_sums.as_bytes()), "config": cfg, "config_sha256": sha_hex(&cfg_bytes),
        "lexicon_sha256": lex.sha256, "lexicon_words": lex.len(),
        "rules": {
            "ocr_screen_v1": "OCR newspaper rows of Train and Train-mining pass when case-flip, broken-hyphen, non-word and odd-token rates are all within the thresholds; Train-mining keeps at most cap_train_mining passing rows per domain (tier a first, then SHA-256 of salt and id)",
            "private_use_screen_v1": "rows of the listed domains with a private-use code point or U+FFFD are dropped whole; no kept text is edited",
            "ellipse_prompt_half_v1": "ELLIPSE prompts ranked by SHA-256 of salt and prompt key; even ranks move to Train, odd ranks and any evaluation prompt stay in the ungated held-out diagnostic",
            "evaluation_files": "every other file is a hard link to the base file (assignments unchanged)",
        },
        "files_linked": linked, "files_rewritten": new_sha.iter().map(|(k, (s, n))| (k.clone(), json!({"sha256": s, "rows": n}))).collect::<serde_json::Map<_, _>>(),
        "changes": {"sha256": changes_sha.0, "rows": changes_sha.1, "by_split_reason_domain": by_reason},
        "screen_counts": counts,
        "ellipse": {"prompts": prompts.len(), "train_prompts": to_train.len(), "held_prompts": held_prompts.len(),
            "evaluation_prompts_held": blocked, "essays_moved_to_train": moved, "essays_held": held_docs.1},
        "code": code, "test_opened": false, "detector_outputs_used": false,
    });
    let rb = serde_json::to_vec_pretty(&report)?;
    fs::write(tmp.join("SUCCESSOR.json"), &rb)?;
    sums.push_str(&format!("{}  ./SUCCESSOR.json\n", sha_hex(&rb)));
    fs::write(tmp.join("SHA256SUMS"), sums.as_bytes())?;
    fs::rename(&tmp, out)?;
    Ok(json!({"out": out, "files_linked": linked, "changes": report["changes"], "ellipse": report["ellipse"],
        "screen_counts": report["screen_counts"]}))
}

fn raw_lines(p: &Path) -> Result<Vec<Vec<u8>>> {
    let mut v = Vec::new();
    for_lines(p, |l| {
        v.push(l.to_vec());
        Ok(())
    })?;
    if v.is_empty() {
        bail!("{} is empty", p.display());
    }
    Ok(v)
}

//! spans-v1.1 span evaluator (PROTOCOL-v1.1 Sections 4.2-4.3, 5.3-5.4, 6.1,
//! 6.3 and 7). Text-free outputs only.
//!
//! a_spans_evaluate CONFIG.json OUT.json
//!
//! The config names evaluation populations (trainer-row JSONL files with the
//! exact text, the tokenizer-only span inputs of `export_full_a.py offsets` or
//! `score-rows`, and for spliced rows the v1.1 records for their metadata),
//! the meta-screen exclusion file and one or more systems (per-row token logits
//! and document outputs, raw or CAL-calibrated). The emission rule is the
//! runtime's own (`full_a_spans`): words, word probabilities, the share tilt,
//! the sealed sentence splitter and the decoder, so evaluator and runtime
//! cannot drift.
//!
//! Modes:
//! - `metrics`: per system the primary span metric (tilted word Brier on the
//!   mixed documents of the populations marked `primary`, balanced
//!   source-group/origin weights over all included documents, words equal
//!   within a document) and the Section 6.1 metrics overall and by slice
//!   (domain, generator, policy, operation, change-fraction band, share bin,
//!   cohort kind, typography stratum, copied words); arm three-seed means and
//!   paired source-group bootstrap intervals (Section 6.3) for declared pairs.
//!   Decoder-dependent and confidence metrics need `emission`.
//! - `grid`: the 108-point decoder grid on one system (Section 5.4 step 4):
//!   balanced character F1 on the primary mixed documents, human documents of
//!   the constraint population carrying any span, and the rule's choice. An
//!   optional `grid` config field gives per-parameter value lists (the
//!   v1.3-ship G576 extension); each list defaults to the v1.1 one, so a
//!   config without it evaluates the same 108 points in the same order.
//! - `fit_cal`: the CAL sentence and span confidence maps (Section 7,
//!   clipped isotonic) with the chosen decoder on one CAL system.
use anyhow::{Context, Result, bail, ensure};
use grammar_a_baseline::comp_eval::bootstrap_draws;
use grammar_a_baseline::full_a_spans::{self as sp, Decoder, ScoreMap};
use grammar_a_baseline::involvement_calibration::{Map, fit_isotonic_scores};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

const SCHEMA: &str = "slop_ninja_a_spans_evaluation_v1";
const CONFIG_SCHEMA: &str = "slop_ninja_a_spans_eval_config_v1";
const OFFSETS_SCHEMA: &str = "slop_ninja_full_a_span_offsets_v1";
const TOKEN_SCHEMA: &str = "slop_ninja_full_a_token_logits_v1";
const SPLICED: &str = "spliced_rewrite_v1";
const BINS: usize = 10;

/// The runtime's sigmoid (`shared::sigmoid`, crate-private there), the same expression.
fn sigmoid(x: f64) -> f64 {
    if x >= 0. {
        1. / (1. + (-x).exp())
    } else {
        let e = x.exp();
        e / (1. + e)
    }
}

fn sha256_file(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p).with_context(|| p.display().to_string())?)))
}

fn jsonl(p: &Path, mut f: impl FnMut(&str) -> Result<()>) -> Result<()> {
    for line in BufReader::new(fs::File::open(p).with_context(|| p.display().to_string())?).lines() {
        let line = line?;
        if !line.trim().is_empty() {
            f(&line)?;
        }
    }
    Ok(())
}

fn s(v: &Value, k: &str) -> Result<String> {
    Ok(v[k].as_str().with_context(|| format!("field {k}"))?.to_owned())
}

// ------------------------------------------------------------------ inputs

#[derive(Clone, Debug, Default)]
struct Edit {
    operation: String,
    change: Option<f64>,
    longest_copied: usize,
}

#[derive(Clone, Debug, Default)]
struct Spliced {
    policy: String,
    stratum: String,
    markers: bool,
    copied: Vec<(usize, usize)>,
    edits: Vec<Edit>,
}

struct Doc {
    pop: String,
    primary: bool,
    constraint: bool,
    id: String,
    text_sha256: String,
    origin: u8,
    share: f64,
    generator: Option<String>,
    cohort: String,
    group: String,
    domain: String,
    operation: Option<String>,
    spliced: Option<Spliced>,
    words: Vec<sp::Segment>,
    groups: Vec<Vec<usize>>,
    ranges: Vec<(usize, usize, usize)>,
    word_labels: Vec<i8>,
    /// AI characters inside each word (words are non-whitespace runs).
    word_ai_chars: Vec<usize>,
    word_copied: Vec<bool>,
    token_labels: Vec<i8>,
    word_mask: Vec<bool>,
    weight: f64,
}

impl Doc {
    fn len(&self, w: usize) -> usize {
        self.words[w].end_char - self.words[w].start_char
    }
    fn kind(&self) -> &'static str {
        if self.cohort == SPLICED {
            "spliced"
        } else if self.origin == 2 && self.share >= 1.0 {
            "full_rewrite"
        } else if self.cohort.starts_with("hosted") || self.cohort.starts_with("frontier") {
            "frontier"
        } else if self.cohort == "local:primary_mixed" {
            "planner_span"
        } else if self.origin == 2 {
            "plan_v2_operation"
        } else {
            "unmixed"
        }
    }
}

fn char_spans(text: &str, bytes: &[(usize, usize)]) -> Result<Vec<(usize, usize)>> {
    let starts: Vec<usize> = text.char_indices().map(|(b, _)| b).chain([text.len()]).collect();
    let ci = |b: usize| -> Result<usize> {
        let i = starts.partition_point(|x| *x < b);
        ensure!(i < starts.len() && starts[i] == b, "span offset is not a character boundary");
        Ok(i)
    };
    bytes.iter().map(|&(a, b)| Ok((ci(a)?, ci(b)?))).collect()
}

fn pairs(v: &Value) -> Result<Vec<(usize, usize)>> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Array(a) => a
            .iter()
            .map(|p| Ok((p[0].as_u64().context("span")? as usize, p[1].as_u64().context("span")? as usize)))
            .collect(),
        _ => bail!("span list"),
    }
}

fn spliced_meta(path: &Path, half: Option<&str>, want: &HashSet<String>) -> Result<HashMap<String, (String, Spliced)>> {
    let mut out = HashMap::new();
    let marker = half.map(|h| format!("\"span_half\":\"{h}\""));
    jsonl(path, |line| {
        if let Some(m) = &marker {
            if !line.contains(m.as_str()) {
                return Ok(());
            }
        }
        let r: Value = serde_json::from_str(line)?;
        let id = s(&r, "id")?;
        if !want.contains(&id) {
            return Ok(());
        }
        let markers = r["typography"]["markers"].as_object().is_some_and(|m| m.values().any(|v| v.as_f64().unwrap_or(0.) > 0.));
        let edits = r["edits"]
            .as_array()
            .context("edits")?
            .iter()
            .map(|e| Edit {
                operation: e["operation"].as_str().unwrap_or("?").to_owned(),
                change: e["change_fraction"].as_f64(),
                longest_copied: e["copied"]["longest_run"].as_u64().unwrap_or(0) as usize,
            })
            .collect();
        out.insert(id, (s(&r, "text_sha256")?, Spliced {
            policy: s(&r, "policy")?,
            stratum: r["typography"]["stratum"].as_str().unwrap_or("?").to_owned(),
            markers,
            copied: pairs(&r["ai_copied_char_spans"])?,
            edits,
        }));
        Ok(())
    })?;
    Ok(out)
}

fn load_population(p: &Value, flagged: &HashSet<String>, docs: &mut Vec<Doc>, report: &mut BTreeMap<String, Value>) -> Result<()> {
    let name = s(p, "name")?;
    let split = s(p, "split")?;
    let rows = Path::new(p["rows"].as_str().context("rows")?);
    let primary = p["primary"].as_bool().unwrap_or(false);
    let constraint = p["constraint"].as_bool().unwrap_or(false);
    let mut texts: Vec<(Value, String)> = Vec::new();
    let mut skipped = 0usize;
    jsonl(rows, |line| {
        // Sealed or other-split lines are skipped before parsing.
        for other in ["dev_select", "test", "dev_iter", "cal", "train"] {
            if other != split && line.contains(&format!("\"split\":\"{other}\"")) {
                skipped += 1;
                return Ok(());
            }
        }
        let r: Value = serde_json::from_str(line)?;
        ensure!(r["split"] == split.as_str(), "row split differs from the population split");
        let text = s(&r, "text")?;
        texts.push((r, text));
        Ok(())
    })?;
    let ids: HashSet<String> = texts.iter().map(|(r, _)| r["id"].as_str().unwrap().to_owned()).collect();
    let meta = match p["records"].as_str() {
        Some(path) => spliced_meta(Path::new(path), p["records_half"].as_str(), &ids)?,
        None => HashMap::new(),
    };
    let mut offsets: HashMap<String, Value> = HashMap::new();
    jsonl(Path::new(p["offsets"].as_str().context("offsets")?), |line| {
        let v: Value = serde_json::from_str(line)?;
        ensure!(v["schema"] == OFFSETS_SCHEMA, "offsets schema");
        let id = s(&v, "id")?;
        if ids.contains(&id) {
            offsets.insert(id, v);
        }
        Ok(())
    })?;
    let (mut n, mut excluded, mut unsupported) = (0usize, 0usize, 0usize);
    for (r, text) in texts {
        let id = s(&r, "id")?;
        if flagged.contains(&id) {
            excluded += 1;
            continue;
        }
        let o = offsets.remove(&id).with_context(|| format!("{id}: no span offsets"))?;
        ensure!(o["text_sha256"] == r["text_sha256"], "{id}: offsets bound to other text");
        let origin = match r["origin"].as_str() {
            Some("human_only") => 0,
            Some("model_only") => 1,
            Some("mixed") => 2,
            _ => bail!("{id}: origin"),
        };
        let share = r["share"].as_f64().context("share")?;
        let offs: Vec<(usize, usize)> = pairs(&o["token_offsets"])?;
        let word_mask: Vec<bool> = o["word_mask"].as_array().context("mask")?.iter().map(|x| x.as_u64() == Some(1)).collect();
        let token_labels: Vec<i8> = o["token_labels"].as_array().context("labels")?.iter().map(|x| x.as_i64().unwrap_or(-1) as i8).collect();
        let word_labels: Vec<i8> = o["word_labels"].as_array().context("word labels")?.iter().map(|x| x.as_i64().unwrap_or(-1) as i8).collect();
        let words = sp::words(&text);
        ensure!(words.len() == word_labels.len(), "{id}: word count differs from the offsets dump");
        let n_chars = text.chars().count();
        let groups = match sp::word_groups(n_chars, &words, &offs, &word_mask) {
            Ok(g) => g,
            Err(_) => {
                unsupported += 1;
                continue;
            }
        };
        let ranges = sp::sentence_word_ranges(&sp::sentences(&text), &words)?;
        let ai = if origin == 2 {
            char_spans(&text, &pairs(&r["ai_byte_spans"])?)?
        } else if origin == 1 {
            vec![(0, n_chars)]
        } else {
            Vec::new()
        };
        let mut inside = vec![false; n_chars];
        for &(a, b) in &ai {
            inside[a..b].fill(true);
        }
        let word_ai_chars = words.iter().map(|w| inside[w.start_char..w.end_char].iter().filter(|x| **x).count()).collect();
        let spliced = match meta.get(&id) {
            Some((sha, m)) => {
                ensure!(sha == r["text_sha256"].as_str().unwrap(), "{id}: record bound to other text");
                Some(m.clone())
            }
            None => {
                ensure!(r["cohort_id"] != SPLICED, "{id}: spliced row without its v1.1 record");
                None
            }
        };
        let mut copied_chars = vec![false; n_chars];
        if let Some(m) = &spliced {
            for &(a, b) in &m.copied {
                copied_chars[a..b.min(n_chars)].fill(true);
            }
        }
        let word_copied = words.iter().map(|w| copied_chars[w.start_char..w.end_char].iter().any(|x| *x)).collect();
        docs.push(Doc {
            pop: name.clone(),
            primary,
            constraint,
            id,
            text_sha256: s(&r, "text_sha256")?,
            origin,
            share,
            generator: r["generator_label"].as_str().map(str::to_owned),
            cohort: s(&r, "cohort_id")?,
            group: s(&r, "source_group")?,
            domain: s(&r, "domain")?,
            operation: r["operation"].as_str().map(str::to_owned),
            spliced,
            words,
            groups,
            ranges,
            word_labels,
            word_ai_chars,
            word_copied,
            token_labels,
            word_mask,
            weight: 0.,
        });
        n += 1;
    }
    report.insert(name, json!({"rows_file": rows, "rows_file_sha256": sha256_file(rows)?, "documents": n,
        "excluded_meta_flagged": excluded, "unsupported_word_without_token": unsupported,
        "skipped_other_split_lines": skipped, "primary": primary, "constraint": constraint}));
    Ok(())
}

/// Balanced weights over all included documents: equal source-group mass,
/// equal origin-cell mass within a group, equal document mass within a cell.
fn balance(docs: &mut [Doc]) {
    let mut cells: HashMap<(String, u8), usize> = HashMap::new();
    for d in docs.iter() {
        *cells.entry((d.group.clone(), d.origin)).or_default() += 1;
    }
    let mut strata: HashMap<String, usize> = HashMap::new();
    for (g, _) in cells.keys() {
        *strata.entry(g.clone()).or_default() += 1;
    }
    let groups = strata.len() as f64;
    for d in docs.iter_mut() {
        d.weight = 1. / (groups * strata[&d.group] as f64 * cells[&(d.group.clone(), d.origin)] as f64);
    }
}

// ------------------------------------------------------------------ systems

struct Outputs {
    logits: HashMap<String, Vec<f64>>,
    share: HashMap<String, f64>,
    confidence: HashMap<String, f64>,
}

fn load_system(sys: &Value, want: &HashMap<String, String>) -> Result<Outputs> {
    let mut o = Outputs { logits: HashMap::new(), share: HashMap::new(), confidence: HashMap::new() };
    for p in sys["logits"].as_array().context("logits")? {
        jsonl(Path::new(p.as_str().context("logits path")?), |line| {
            let v: Value = serde_json::from_str(line)?;
            ensure!(v["schema"] == TOKEN_SCHEMA, "token logits schema");
            let id = s(&v, "id")?;
            if let Some(sha) = want.get(&id) {
                ensure!(v["text_sha256"] == sha.as_str(), "{id}: logits bound to other text");
                let t: Vec<f64> = v["token_logits"].as_array().context("logits")?.iter().map(|x| x.as_f64().unwrap()).collect();
                o.logits.insert(id, t);
            }
            Ok(())
        })?;
    }
    for d in sys["documents"].as_array().context("documents")? {
        let kind = s(d, "kind")?;
        // raw_calibrated: the frozen CAL map applied to raw heads exactly as calibrate_full_a and the
        // runtime do (head share mode): p, r from apply_origin; share = p r + p (1 - r) q, clamped to [p r, p].
        let map = match d["calibration"].as_str() {
            Some(c) => {
                let v: Value = serde_json::from_slice(&fs::read(c)?)?;
                Some(Map::from_description(&v["involvement"]["selected"])?)
            }
            None => None,
        };
        jsonl(Path::new(d["path"].as_str().context("documents path")?), |line| {
            let v: Value = serde_json::from_str(line)?;
            let id = s(&v, "id")?;
            if !want.contains_key(&id) {
                return Ok(());
            }
            let (share, conf) = match kind.as_str() {
                "raw" => {
                    ensure!(v["text_sha256"] == want[&id].as_str(), "{id}: raw output bound to other text");
                    let u = &v["uncalibrated_origin"];
                    (v["uncalibrated_share"].as_f64().context("share")?,
                     u["model_only"].as_f64().context("p")? + u["mixed"].as_f64().context("p")?)
                }
                "raw_calibrated" => {
                    ensure!(v["text_sha256"] == want[&id].as_str(), "{id}: raw output bound to other text");
                    let m = map.as_ref().context("raw_calibrated needs a calibration file")?;
                    let logits: Vec<f64> = v["origin_logits"].as_array().context("origin logits")?.iter().map(|x| x.as_f64().unwrap()).collect();
                    let (p, r) = m.apply_origin(&logits, v["involvement_logit"].as_f64().context("logit")?)?;
                    let q = v["conditional_mixed_share"].as_f64().context("mixed share")?;
                    ((p * r + p * (1. - r) * q).clamp(p * r, p), p)
                }
                "calibrated" => {
                    ensure!(v["calibrated"] == true, "{id}: calibrated predictions required");
                    (v["share"].as_f64().context("share")?, 1. - v["p_human"].as_f64().context("p_human")?)
                }
                _ => bail!("document output kind"),
            };
            o.share.insert(id.clone(), share);
            o.confidence.insert(id, conf.clamp(0., 1.));
            Ok(())
        })?;
    }
    for id in want.keys() {
        ensure!(o.logits.contains_key(id) && o.share.contains_key(id), "{id}: system outputs missing");
    }
    Ok(o)
}

struct Probs {
    p_tok: Vec<f64>,
    tilt: f64,
    p: Vec<f64>,
    q: Vec<f64>,
    share: f64,
    target: f64,
    confidence: f64,
}

fn probs(d: &Doc, o: &Outputs) -> Result<Probs> {
    let t = &o.logits[&d.id];
    ensure!(t.len() == d.word_mask.len(), "{}: token count differs", d.id);
    let p_tok: Vec<f64> = t.iter().map(|x| sigmoid(*x)).collect();
    let p: Vec<f64> = d.groups.iter().map(|g| g.iter().map(|j| p_tok[*j]).sum::<f64>() / g.len() as f64).collect();
    let share = o.share[&d.id];
    let (b, target, q) = sp::tilt(&p, share)?;
    Ok(Probs { p_tok, tilt: b, p, q, share, target, confidence: o.confidence[&d.id] })
}

// ------------------------------------------------------------------ per-document metrics

#[derive(Clone, Debug, Default)]
struct DocM {
    labelled: bool,
    word_brier: f64,
    word_brier_raw: f64,
    token_brier: Option<f64>,
    token_brier_raw: Option<f64>,
    raw_minus_share: f64,
    tilt_residual: f64,
    // decoder-dependent
    decoded: bool,
    any_span: bool,
    decoded_word_rate: f64,
    char_tp: f64,
    char_pred: f64,
    char_true: f64,
    span_f1: f64,
    span_precision: Option<f64>,
    span_recall: Option<f64>,
    mean_best_iou: Option<f64>,
    boundaries: Vec<[f64; 4]>, // start words, end words, start chars, end chars
    sentences: Vec<(f64, f64, f64, f64)>, // confidence, contains AI, predicted share, true share
    span_conf: Vec<(f64, f64)>,
    // copied-word variants (spliced rows)
    nocopy_word_brier: Option<f64>,
    nocopy_char: Option<(f64, f64, f64)>,
    nocopy_sent_share: Vec<(f64, f64)>,
}

fn runs_of(labels: &[i8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < labels.len() {
        if labels[i] == 1 {
            let mut j = i;
            while j < labels.len() && labels[j] == 1 {
                j += 1;
            }
            out.push((i, j));
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Character IoU of two word runs (non-whitespace characters; overlapping runs have a contiguous union).
fn iou(d: &Doc, a: (usize, usize), b: (usize, usize)) -> f64 {
    let inter: usize = (a.0.max(b.0)..a.1.min(b.1)).map(|w| d.len(w)).sum();
    if inter == 0 {
        return 0.;
    }
    let union: usize = (a.0.min(b.0)..a.1.max(b.1)).map(|w| d.len(w)).sum();
    inter as f64 / union as f64
}

struct Emission<'a> {
    decoder: &'a Decoder,
    sentence: &'a ScoreMap,
    span: &'a ScoreMap,
}

fn doc_metrics(d: &Doc, pr: &Probs, em: Option<&Emission>) -> DocM {
    let mut m = DocM::default();
    let lab: Vec<usize> = (0..d.words.len()).filter(|w| d.word_labels[*w] >= 0).collect();
    if !lab.is_empty() {
        m.labelled = true;
        let y = |w: usize| d.word_labels[w] as f64;
        m.word_brier = lab.iter().map(|&w| (pr.q[w] - y(w)).powi(2)).sum::<f64>() / lab.len() as f64;
        m.word_brier_raw = lab.iter().map(|&w| (pr.p[w] - y(w)).powi(2)).sum::<f64>() / lab.len() as f64;
    }
    let toks: Vec<usize> = (0..d.word_mask.len()).filter(|j| d.word_mask[*j] && d.token_labels[*j] >= 0).collect();
    if !toks.is_empty() {
        let n = toks.len() as f64;
        m.token_brier_raw = Some(toks.iter().map(|&j| (pr.p_tok[j] - d.token_labels[j] as f64).powi(2)).sum::<f64>() / n);
        let lg = |p: f64| { let p = p.clamp(1e-9, 1. - 1e-9); p.ln() - (-p).ln_1p() };
        m.token_brier = Some(toks.iter().map(|&j| (sigmoid(lg(pr.p_tok[j]) + pr.tilt) - d.token_labels[j] as f64).powi(2)).sum::<f64>() / n);
    }
    let nw = d.words.len() as f64;
    m.raw_minus_share = pr.p.iter().sum::<f64>() / nw - pr.share;
    m.tilt_residual = (pr.q.iter().sum::<f64>() / nw - pr.target).abs();
    let Some(em) = em else { return m };
    m.decoded = true;
    let runs = em.decoder.decode(&pr.q, &d.ranges);
    m.any_span = !runs.is_empty();
    let mut dec = vec![false; d.words.len()];
    for &(a, b) in &runs {
        dec[a..b].fill(true);
    }
    m.decoded_word_rate = dec.iter().filter(|x| **x).count() as f64 / nw;
    for w in 0..d.words.len() {
        let ai = d.word_ai_chars[w] as f64;
        m.char_true += ai;
        if dec[w] {
            m.char_pred += d.len(w) as f64;
            m.char_tp += ai;
        }
    }
    // Span level: true spans are maximal runs of AI words.
    let truth = runs_of(&d.word_labels);
    if !truth.is_empty() || !runs.is_empty() {
        let mut cand: Vec<(f64, usize, usize)> = Vec::new();
        for (i, &t) in truth.iter().enumerate() {
            for (j, &r) in runs.iter().enumerate() {
                let v = iou(d, r, t);
                if v > 0. {
                    cand.push((v, i, j));
                }
            }
        }
        cand.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let (mut ut, mut ur) = (vec![false; truth.len()], vec![false; runs.len()]);
        let mut tp = 0usize;
        for (v, i, j) in cand.iter().copied() {
            if v >= 0.5 && !ut[i] && !ur[j] {
                ut[i] = true;
                ur[j] = true;
                tp += 1;
                let (t, r) = (truth[i], runs[j]);
                m.boundaries.push([
                    (r.0 as f64 - t.0 as f64).abs(),
                    (r.1 as f64 - t.1 as f64).abs(),
                    (d.words[r.0].start_char as f64 - d.words[t.0].start_char as f64).abs(),
                    (d.words[r.1 - 1].end_char as f64 - d.words[t.1 - 1].end_char as f64).abs(),
                ]);
            }
        }
        m.span_f1 = 2. * tp as f64 / (truth.len() + runs.len()) as f64;
        m.span_precision = (!runs.is_empty()).then(|| tp as f64 / runs.len() as f64);
        m.span_recall = (!truth.is_empty()).then(|| tp as f64 / truth.len() as f64);
        if !truth.is_empty() {
            m.mean_best_iou = Some(truth.iter().map(|&t| runs.iter().map(|&r| iou(d, r, t)).fold(0., f64::max)).sum::<f64>() / truth.len() as f64);
        }
    } else {
        m.span_f1 = 1.;
    }
    for &(_, a, e) in &d.ranges {
        let lab: Vec<usize> = (a..e).filter(|w| d.word_labels[*w] >= 0).collect();
        if lab.is_empty() {
            continue;
        }
        let max = pr.q[a..e].iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let conf = em.sentence.apply(max).min(pr.confidence);
        let contains = lab.iter().any(|w| d.word_labels[*w] == 1) as u8 as f64;
        let share = pr.q[a..e].iter().sum::<f64>() / (e - a) as f64;
        let truth = lab.iter().filter(|w| d.word_labels[**w] == 1).count() as f64 / lab.len() as f64;
        m.sentences.push((conf, contains, share, truth));
        if d.spliced.is_some() {
            let kept: Vec<usize> = lab.iter().copied().filter(|w| !d.word_copied[*w]).collect();
            if !kept.is_empty() {
                let ps = kept.iter().map(|w| pr.q[*w]).sum::<f64>() / kept.len() as f64;
                let ts = kept.iter().filter(|w| d.word_labels[**w] == 1).count() as f64 / kept.len() as f64;
                m.nocopy_sent_share.push((ps, ts));
            }
        }
    }
    for &(a, e) in &runs {
        let mean = pr.q[a..e].iter().sum::<f64>() / (e - a) as f64;
        let conf = em.span.apply(mean).min(pr.confidence);
        let ai = (a..e).filter(|w| d.word_labels[*w] == 1).count() as f64 / (e - a) as f64;
        m.span_conf.push((conf, (ai >= 0.5) as u8 as f64));
    }
    if d.spliced.is_some() {
        let kept: Vec<usize> = lab.iter().copied().filter(|w| !d.word_copied[*w]).collect();
        if !kept.is_empty() {
            m.nocopy_word_brier = Some(kept.iter().map(|&w| (pr.q[w] - d.word_labels[w] as f64).powi(2)).sum::<f64>() / kept.len() as f64);
        }
        let (mut tp, mut pp, mut tt) = (0., 0., 0.);
        for w in (0..d.words.len()).filter(|w| !d.word_copied[*w]) {
            let ai = d.word_ai_chars[w] as f64;
            tt += ai;
            if dec[w] {
                pp += d.len(w) as f64;
                tp += ai;
            }
        }
        m.nocopy_char = Some((tp, pp, tt));
    }
    m
}

// ------------------------------------------------------------------ aggregation

fn quantile(v: &mut [f64], q: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[((q * (v.len() - 1) as f64).round() as usize).min(v.len() - 1)])
}

/// (ECE over 10 equal-width bins, Brier) of weighted (prediction, target) pairs.
fn ece(pairs: &[(f64, f64, f64)]) -> Option<(f64, f64)> {
    let total: f64 = pairs.iter().map(|x| x.2).sum();
    if pairs.is_empty() || total <= 0. {
        return None;
    }
    let mut bins = vec![(0., 0., 0.); BINS];
    for &(p, y, w) in pairs {
        let b = ((p * BINS as f64) as usize).min(BINS - 1);
        bins[b].0 += w * p;
        bins[b].1 += w * y;
        bins[b].2 += w;
    }
    let e = bins.iter().filter(|b| b.2 > 0.).map(|b| b.2 / total * (b.0 / b.2 - b.1 / b.2).abs()).sum();
    let brier = pairs.iter().map(|&(p, y, w)| w * (p - y).powi(2)).sum::<f64>() / total;
    Some((e, brier))
}

fn wmean(xs: impl Iterator<Item = (f64, f64)>) -> Option<f64> {
    let (mut a, mut b) = (0., 0.);
    for (w, x) in xs {
        a += w * x;
        b += w;
    }
    (b > 0.).then(|| a / b)
}

fn aggregate(docs: &[&Doc], ms: &[&DocM], decoded: bool) -> Value {
    let n = docs.len();
    let wsum: f64 = docs.iter().map(|d| d.weight).sum();
    let lab = || docs.iter().zip(ms).filter(|(_, m)| m.labelled);
    let by_origin: Vec<usize> = [0u8, 1, 2].iter().map(|o| docs.iter().filter(|d| d.origin == *o).count()).collect();
    let mut out = json!({
        "documents": n,
        "documents_by_origin": by_origin,
        "word_brier": wmean(lab().map(|(d, m)| (d.weight, m.word_brier))),
        "word_brier_raw": wmean(lab().map(|(d, m)| (d.weight, m.word_brier_raw))),
        "token_brier": wmean(docs.iter().zip(ms).filter_map(|(d, m)| m.token_brier.map(|x| (d.weight, x)))),
        "token_brier_raw": wmean(docs.iter().zip(ms).filter_map(|(d, m)| m.token_brier_raw.map(|x| (d.weight, x)))),
        "raw_mean_word_probability_minus_share_mae": if n > 0 { Some(ms.iter().map(|m| m.raw_minus_share.abs()).sum::<f64>() / n as f64) } else { None },
        "raw_mean_word_probability_minus_share_p90": quantile(&mut ms.iter().map(|m| m.raw_minus_share.abs()).collect::<Vec<_>>(), 0.9),
        "post_tilt_residual_max": ms.iter().map(|m| m.tilt_residual).fold(0., f64::max),
        "weight_sum": wsum,
    });
    if !decoded {
        return out;
    }
    let f1 = |tp: f64, pp: f64, tt: f64| if pp + tt > 0. { 2. * tp / (pp + tt) } else { 1. };
    let ai_docs: Vec<(f64, &DocM)> = docs.iter().zip(ms).filter(|(d, _)| d.origin != 0).map(|(d, m)| (d.weight, *m)).collect();
    out["char_f1"] = json!(wmean(ai_docs.iter().map(|(w, m)| (*w, f1(m.char_tp, m.char_pred, m.char_true)))));
    out["char_precision"] = json!(wmean(ai_docs.iter().filter(|(_, m)| m.char_pred > 0.).map(|(w, m)| (*w, m.char_tp / m.char_pred))));
    out["char_recall"] = json!(wmean(ai_docs.iter().filter(|(_, m)| m.char_true > 0.).map(|(w, m)| (*w, m.char_tp / m.char_true))));
    out["ai_documents_without_decoded_span"] = json!(ai_docs.iter().filter(|(_, m)| !m.any_span).count());
    out["span_f1"] = json!(wmean(ai_docs.iter().map(|(w, m)| (*w, m.span_f1))));
    out["span_precision"] = json!(wmean(ai_docs.iter().filter_map(|(w, m)| m.span_precision.map(|x| (*w, x)))));
    out["span_recall"] = json!(wmean(ai_docs.iter().filter_map(|(w, m)| m.span_recall.map(|x| (*w, x)))));
    out["mean_best_match_iou"] = json!(wmean(ai_docs.iter().filter_map(|(w, m)| m.mean_best_iou.map(|x| (*w, x)))));
    let mut b: Vec<[f64; 4]> = ms.iter().flat_map(|m| m.boundaries.iter().copied()).collect();
    let stat = |k: usize, b: &mut Vec<[f64; 4]>| {
        let mut v: Vec<f64> = b.iter().map(|x| x[k]).collect();
        let mean = if v.is_empty() { None } else { Some(v.iter().sum::<f64>() / v.len() as f64) };
        json!({"median": quantile(&mut v, 0.5), "mean": mean, "p90": quantile(&mut v, 0.9)})
    };
    out["boundary_error"] = json!({"matched_spans": b.len(), "start_words": stat(0, &mut b), "end_words": stat(1, &mut b),
        "start_chars": stat(2, &mut b), "end_chars": stat(3, &mut b)});
    let sent: Vec<(f64, f64, f64)> = docs.iter().zip(ms).flat_map(|(d, m)| {
        let k = m.sentences.len().max(1) as f64;
        m.sentences.iter().map(move |x| (x.0, x.1, d.weight / k))
    }).collect();
    let share: Vec<(f64, f64, f64)> = docs.iter().zip(ms).flat_map(|(d, m)| {
        let k = m.sentences.len().max(1) as f64;
        m.sentences.iter().map(move |x| (x.2, x.3, d.weight / k))
    }).collect();
    let tot: f64 = share.iter().map(|x| x.2).sum();
    out["sentences"] = json!(sent.len());
    out["sentence_confidence_ece"] = json!(ece(&sent).map(|x| x.0));
    out["sentence_confidence_brier"] = json!(ece(&sent).map(|x| x.1));
    out["sentence_share_mae"] = json!((tot > 0.).then(|| share.iter().map(|x| x.2 * (x.0 - x.1).abs()).sum::<f64>() / tot));
    out["sentence_share_ece"] = json!(ece(&share).map(|x| x.0));
    let spans: Vec<(f64, f64, f64)> = docs.iter().zip(ms).flat_map(|(d, m)| {
        let k = m.span_conf.len().max(1) as f64;
        m.span_conf.iter().map(move |x| (x.0, x.1, d.weight / k))
    }).collect();
    out["decoded_spans"] = json!(spans.len());
    out["span_confidence_ece"] = json!(ece(&spans).map(|x| x.0));
    let humans: Vec<(&&Doc, &&DocM)> = docs.iter().zip(ms).filter(|(d, _)| d.origin == 0).collect();
    let hn = humans.len();
    let hs = humans.iter().filter(|(_, m)| m.any_span).count();
    out["human_documents"] = json!(hn);
    out["human_documents_with_span"] = json!(hs);
    out["human_documents_with_span_rate"] = json!((hn > 0).then(|| hs as f64 / hn as f64));
    out["human_decoded_word_rate_mean"] = json!((hn > 0).then(|| humans.iter().map(|(_, m)| m.decoded_word_rate).sum::<f64>() / hn as f64));
    let mo: Vec<&&DocM> = docs.iter().zip(ms).filter(|(d, _)| d.origin == 1).map(|(_, m)| m).collect();
    out["model_only_missed_word_rate_mean"] = json!((!mo.is_empty()).then(|| mo.iter().map(|m| 1. - m.decoded_word_rate).sum::<f64>() / mo.len() as f64));
    let sp_docs: Vec<(f64, &DocM)> = docs.iter().zip(ms).filter(|(d, _)| d.spliced.is_some()).map(|(d, m)| (d.weight, *m)).collect();
    if !sp_docs.is_empty() {
        let ss: Vec<(f64, f64, f64)> = docs.iter().zip(ms).filter(|(d, _)| d.spliced.is_some()).flat_map(|(d, m)| {
            let k = m.nocopy_sent_share.len().max(1) as f64;
            m.nocopy_sent_share.iter().map(move |x| (x.0, x.1, d.weight / k))
        }).collect();
        let t: f64 = ss.iter().map(|x| x.2).sum();
        out["copied_words_removed"] = json!({
            "word_brier": wmean(sp_docs.iter().filter_map(|(w, m)| m.nocopy_word_brier.map(|x| (*w, x)))),
            "char_f1": wmean(sp_docs.iter().filter_map(|(w, m)| m.nocopy_char.map(|c| (*w, f1(c.0, c.1, c.2))))),
            "sentence_share_mae": (t > 0.).then(|| ss.iter().map(|x| x.2 * (x.0 - x.1).abs()).sum::<f64>() / t),
            "documents": sp_docs.len()});
    }
    out
}

fn band(c: f64) -> &'static str {
    if c < 0.1 { "<0.1" } else if c < 0.3 { "0.1-0.3" } else { ">=0.3" }
}

fn share_bin(s: f64) -> &'static str {
    if s < 0.1 { "<10%" } else if s < 0.25 { "10-25%" } else if s < 0.5 { "25-50%" } else if s < 0.75 { "50-75%" } else { ">=75%" }
}

fn slices(d: &Doc) -> Vec<String> {
    let o = ["human_only", "model_only", "mixed"][d.origin as usize];
    let mut k = vec!["all".to_owned(), format!("population={}", d.pop), format!("origin={o}"), format!("domain={}", d.domain)];
    if d.primary {
        k.push("selection".into());
        if d.origin == 2 {
            k.push("selection_mixed".into());
        }
    }
    if let Some(g) = &d.generator {
        k.push(format!("generator={g}"));
    }
    if d.origin == 2 {
        k.push(format!("kind={}", d.kind()));
        k.push(format!("cohort={}", d.cohort));
        k.push(format!("share_bin={}", share_bin(d.share)));
        k.push(format!("domain_mixed={}", d.domain));
        if let Some(g) = &d.generator {
            k.push(format!("generator_mixed={g}"));
        }
        if d.spliced.is_none() {
            if let Some(op) = &d.operation {
                k.push(format!("operation={op}"));
            }
        }
    }
    if let Some(m) = &d.spliced {
        k.push(format!("policy={}", m.policy));
        k.push(format!("typography={}", m.stratum));
        if m.stratum == "raw" {
            k.push(format!("typography=raw_{}", if m.markers { "with_markers" } else { "without_markers" }));
        }
        let ops: BTreeSet<&str> = m.edits.iter().map(|e| e.operation.as_str()).collect();
        for op in ops {
            k.push(format!("operation={op}"));
        }
        let cmin = m.edits.iter().filter_map(|e| e.change).fold(f64::INFINITY, f64::min);
        if cmin.is_finite() {
            k.push(format!("change_band={}", band(cmin)));
        } else {
            k.push("change_band=insert_only".into());
        }
        if m.edits.iter().any(|e| e.longest_copied >= 8) {
            k.push("copied_run_ge_8".into());
        }
    }
    k
}

// ------------------------------------------------------------------ modes

/// The v1.1 decoder grid (108 points), the default when a grid config has no `grid` field.
#[cfg(test)]
fn decoder_grid() -> Vec<Value> {
    decoder_grid_of(&Value::Null).expect("the v1.1 grid")
}

/// The decoder grid from the config's optional `grid` field: per-parameter value lists `tau_hi`, `delta`,
/// `min_words`, `gap` and `snap` (null = off), each defaulting to the v1.1 list. Points run in the v1.1 order
/// (tau_hi, then delta, min_words, gap, snap), so among points that differ only in delta the larger comes later.
/// Lists must be non-empty with distinct values; every point must be a valid decoder.
fn decoder_grid_of(spec: &Value) -> Result<Vec<Value>> {
    const KEYS: [&str; 5] = ["tau_hi", "delta", "min_words", "gap", "snap"];
    let defaults = [json!([0.5, 0.6, 0.7]), json!([0.0, 0.1, 0.2]), json!([1, 3, 5]), json!([0, 2]), json!([null, 0.8])];
    ensure!(spec.is_null() || spec.is_object(), "grid must be an object of value lists");
    if let Some(o) = spec.as_object() {
        ensure!(o.keys().all(|k| KEYS.contains(&k.as_str())), "grid: unknown parameter");
    }
    let mut lists: Vec<Vec<Value>> = Vec::new();
    for (k, d) in KEYS.iter().zip(defaults) {
        let l = spec.get(*k).unwrap_or(&d).as_array().with_context(|| format!("grid.{k}: a list"))?.clone();
        ensure!(!l.is_empty(), "grid.{k}: empty");
        for (i, x) in l.iter().enumerate() {
            ensure!(!l[..i].contains(x), "grid.{k}: duplicate value");
        }
        lists.push(l);
    }
    let mut out = Vec::new();
    for tau in &lists[0] {
        for delta in &lists[1] {
            for m in &lists[2] {
                for g in &lists[3] {
                    for snap in &lists[4] {
                        let p = json!({"rule": sp::DECODER_RULE, "tau_hi": tau, "delta": delta, "min_words": m, "gap": g, "snap": snap});
                        Decoder::from_json(&p).with_context(|| format!("grid point {p}"))?;
                        out.push(p);
                    }
                }
            }
        }
    }
    Ok(out)
}

fn emission_of(v: &Value) -> Result<(Decoder, ScoreMap, ScoreMap)> {
    Ok((Decoder::from_json(&v["decoder"])?, ScoreMap::from_json(&v["sentence_confidence"])?, ScoreMap::from_json(&v["span_confidence"])?))
}

fn group_values(docs: &[&Doc], x: &[f64]) -> BTreeMap<String, f64> {
    let total: f64 = docs.iter().map(|d| d.weight).sum();
    let mut m: BTreeMap<String, f64> = BTreeMap::new();
    for d in docs {
        m.entry(d.group.clone()).or_default();
    }
    let g = m.len() as f64;
    for (d, v) in docs.iter().zip(x) {
        *m.get_mut(&d.group).unwrap() += g * d.weight / total * v;
    }
    m
}

fn run(cfg: &Value, cfg_sha: &str) -> Result<Value> {
    ensure!(cfg["schema"] == CONFIG_SCHEMA, "config schema");
    let mode = s(cfg, "mode")?;
    let mut flagged: HashSet<String> = HashSet::new();
    let mut flag_record = Value::Null;
    if let Some(ex) = cfg.get("exclusions").filter(|v| !v.is_null()) {
        let path = Path::new(ex["file"].as_str().context("exclusion file")?);
        ensure!(sha256_file(path)? == s(ex, "sha256")?, "exclusion file changed");
        let v: Value = serde_json::from_slice(&fs::read(path)?)?;
        let split = s(ex, "split")?;
        for r in v["flagged"].as_array().context("flagged")? {
            if r["split"] == split.as_str() {
                flagged.insert(s(r, "id")?);
            }
        }
        flag_record = json!({"file": path, "sha256": s(ex, "sha256")?, "split": split, "flagged_ids": flagged.len()});
    }
    let mut docs: Vec<Doc> = Vec::new();
    let mut pops = BTreeMap::new();
    for p in cfg["populations"].as_array().context("populations")? {
        load_population(p, &flagged, &mut docs, &mut pops)?;
    }
    let mut seen = HashSet::new();
    for d in &docs {
        ensure!(seen.insert(d.id.clone()), "duplicate id {} across populations", d.id);
    }
    balance(&mut docs);
    let want: HashMap<String, String> = docs.iter().map(|d| (d.id.clone(), d.text_sha256.clone())).collect();
    let mut out = json!({"schema": SCHEMA, "mode": mode, "label": cfg["label"], "config_sha256": cfg_sha,
        "populations": pops, "exclusions": flag_record,
        "weights": "balanced over all included documents: equal source-group mass, equal origin-cell mass within a group, equal document mass within a cell; a slice renormalizes",
        "rules": {"emission": sp::EMISSION_RULE, "words": sp::WORD_RULE, "word_probability": sp::WORD_PROBABILITY_RULE,
                  "tilt": sp::TILT_RULE, "splitter": sp::SPLITTER_ID, "decoder": sp::DECODER_RULE}});
    let systems = cfg["systems"].as_array().context("systems")?;
    match mode.as_str() {
        "metrics" => {
            let em = match cfg.get("emission").filter(|v| !v.is_null()) {
                Some(v) => Some(emission_of(v)?),
                None => None,
            };
            let emission = em.as_ref().map(|(d, a, b)| Emission { decoder: d, sentence: a, span: b });
            let mut per_system = serde_json::Map::new();
            let mut primary_docs: BTreeMap<String, Vec<f64>> = BTreeMap::new();
            let mut doc_vals: BTreeMap<String, Vec<f64>> = BTreeMap::new();
            let mut slice_vals: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
            let mut arms: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for sys in systems {
                let name = format!("{}:{}", s(sys, "arm")?, sys["seed"]);
                arms.entry(s(sys, "arm")?).or_default().push(name.clone());
                let o = load_system(sys, &want)?;
                let mut ms = Vec::with_capacity(docs.len());
                for d in &docs {
                    ms.push(doc_metrics(d, &probs(d, &o)?, emission.as_ref()));
                }
                let mut by: BTreeMap<String, (Vec<&Doc>, Vec<&DocM>)> = BTreeMap::new();
                for (d, m) in docs.iter().zip(&ms) {
                    for k in slices(d) {
                        let e = by.entry(k).or_default();
                        e.0.push(d);
                        e.1.push(m);
                    }
                }
                let mut sl = serde_json::Map::new();
                for (k, (ds, mm)) in &by {
                    sl.insert(k.clone(), aggregate(ds, mm, emission.is_some()));
                }
                let prim: Vec<f64> = docs.iter().zip(&ms).filter(|(d, _)| d.primary && d.origin == 2).map(|(_, m)| m.word_brier).collect();
                let pd: Vec<&Doc> = docs.iter().filter(|d| d.primary && d.origin == 2).collect();
                let primary = wmean(pd.iter().zip(&prim).map(|(d, x)| (d.weight, *x)));
                primary_docs.insert(name.clone(), prim);
                slice_vals.insert(name.clone(), sl.clone().into_iter().collect());
                // Document block (all included documents, the same balanced weights): involvement Brier of
                // the document confidence, share MAE in points, flags at declared operating points.
                let doc_b: Vec<f64> = docs.iter().map(|d| (o.confidence[&d.id] - (d.origin != 0) as u8 as f64).powi(2)).collect();
                let wt: f64 = docs.iter().map(|d| d.weight).sum();
                let brier = docs.iter().zip(&doc_b).map(|(d, b)| d.weight * b).sum::<f64>() / wt;
                let share_mae = docs.iter().map(|d| d.weight * (o.share[&d.id] - d.share).abs()).sum::<f64>() / wt * 100.;
                let mut ops = serde_json::Map::new();
                if let Some(points) = cfg["operating_points"].as_object() {
                    for (k, t) in points {
                        let t = t.as_f64().context("threshold")?;
                        let h: Vec<&Doc> = docs.iter().filter(|d| d.origin == 0).collect();
                        let pz: Vec<&Doc> = docs.iter().filter(|d| d.origin != 0).collect();
                        let hf = h.iter().filter(|d| o.confidence[&d.id] >= t).count();
                        let tp = pz.iter().filter(|d| o.confidence[&d.id] >= t).count();
                        ops.insert(k.clone(), json!({"threshold": t, "humans": h.len(), "humans_flagged": hf,
                            "positives": pz.len(), "positives_flagged": tp, "tpr": tp as f64 / pz.len().max(1) as f64}));
                    }
                }
                doc_vals.insert(name.clone(), doc_b);
                per_system.insert(name, json!({"arm": sys["arm"], "seed": sys["seed"], "inputs": sys,
                    "primary_tilted_word_brier_selection_mixed": primary, "slices": sl,
                    "document": {"balanced_involvement_brier": brier, "share_mae_points": share_mae, "operating_points": ops,
                                 "basis": "document confidence and share as given by the system's document outputs; balanced weights over all included documents"}}));
            }
            out["systems"] = Value::Object(per_system);
            let pd: Vec<&Doc> = docs.iter().filter(|d| d.primary && d.origin == 2).collect();
            let mut arm_out = serde_json::Map::new();
            for (arm, names) in &arms {
                let vals: Vec<f64> = names.iter().map(|n| out["systems"][n]["primary_tilted_word_brier_selection_mixed"].as_f64().unwrap()).collect();
                let mut means = serde_json::Map::new();
                if let Some(first) = slice_vals.get(&names[0]) {
                    for (k, v) in first {
                        let mut row = serde_json::Map::new();
                        for metric in v.as_object().unwrap().keys() {
                            let xs: Vec<f64> = names.iter().filter_map(|n| slice_vals[n].get(k).and_then(|x| x[metric].as_f64())).collect();
                            if xs.len() == names.len() && !["documents", "weight_sum"].contains(&metric.as_str()) {
                                row.insert(metric.clone(), json!(xs.iter().sum::<f64>() / xs.len() as f64));
                            }
                        }
                        means.insert(k.clone(), Value::Object(row));
                    }
                }
                arm_out.insert(arm.clone(), json!({"systems": names, "primary_per_seed": vals,
                    "primary_mean": vals.iter().sum::<f64>() / vals.len() as f64, "slice_means": means}));
            }
            out["arms"] = Value::Object(arm_out);
            let mut cmps = Vec::new();
            for pair in cfg["compare"].as_array().cloned().unwrap_or_default() {
                let (a, b) = (s(&pair, "a")?, s(&pair, "b")?);
                let (na, nb) = (&arms[&a], &arms[&b]);
                ensure!(na.len() == nb.len(), "compare {a},{b}: seed counts differ");
                // Per-group differences averaged over seeds (seed order as listed), then resampled.
                let mut diff: BTreeMap<String, f64> = BTreeMap::new();
                for (x, y) in na.iter().zip(nb) {
                    let ga = group_values(&pd, &primary_docs[x]);
                    let gb = group_values(&pd, &primary_docs[y]);
                    for (k, v) in ga {
                        *diff.entry(k.clone()).or_default() += (v - gb[&k]) / na.len() as f64;
                    }
                }
                let v: Vec<f64> = diff.into_values().collect();
                let point = v.iter().sum::<f64>() / v.len() as f64;
                let draws = bootstrap_draws(v.len(), |idx| idx.iter().map(|i| v[*i]).sum::<f64>() / idx.len() as f64);
                cmps.push(json!({"a": a, "b": b, "difference_a_minus_b": point, "groups": v.len(),
                    "interval_95": draws.map(|d| json!({"lower": d[24], "upper": d[974],
                        "method": "protocol_source_group_bootstrap_1000_seed20261026_draws24_974; per-group differences averaged over seeds"}))}));
            }
            out["comparisons"] = json!(cmps);
            let all: Vec<&Doc> = docs.iter().collect();
            let mut dcmps = Vec::new();
            for pair in cfg["document_compare"].as_array().cloned().unwrap_or_default() {
                let (a, b) = (s(&pair, "a")?, s(&pair, "b")?);
                let (na, nb) = (&arms[&a], &arms[&b]);
                ensure!(na.len() == nb.len(), "document compare {a},{b}: seed counts differ");
                let mut diff: BTreeMap<String, f64> = BTreeMap::new();
                for (x, y) in na.iter().zip(nb) {
                    let ga = group_values(&all, &doc_vals[x]);
                    let gb = group_values(&all, &doc_vals[y]);
                    for (k, v) in ga {
                        *diff.entry(k.clone()).or_default() += (v - gb[&k]) / na.len() as f64;
                    }
                }
                let v: Vec<f64> = diff.into_values().collect();
                let point = v.iter().sum::<f64>() / v.len() as f64;
                let draws = bootstrap_draws(v.len(), |idx| idx.iter().map(|i| v[*i]).sum::<f64>() / idx.len() as f64);
                dcmps.push(json!({"a": a, "b": b, "balanced_involvement_brier_difference_a_minus_b": point, "groups": v.len(),
                    "interval_95": draws.map(|d| json!({"lower": d[24], "upper": d[974]}))}));
            }
            out["document_comparisons"] = json!(dcmps);
        }
        "grid" => {
            ensure!(systems.len() == 1, "grid takes one system");
            let o = load_system(&systems[0], &want)?;
            let limit = cfg["human_constraint"]["max_documents"].as_u64().context("max_documents")? as usize;
            let pr: Vec<Probs> = docs.iter().map(|d| probs(d, &o)).collect::<Result<_>>()?;
            let ident = ScoreMap::Identity;
            let mut rows = Vec::new();
            let points = decoder_grid_of(&cfg["grid"])?;
            if !cfg["grid"].is_null() {
                out["grid_spec"] = cfg["grid"].clone();
                out["grid_points"] = json!(points.len());
            }
            for g in points {
                let dec = Decoder::from_json(&g)?;
                let em = Emission { decoder: &dec, sentence: &ident, span: &ident };
                let (mut f, mut w, mut humans, mut hn) = (0., 0., 0usize, 0usize);
                for (d, p) in docs.iter().zip(&pr) {
                    if d.primary && d.origin == 2 {
                        let m = doc_metrics(d, p, Some(&em));
                        let pp = m.char_pred + m.char_true;
                        f += d.weight * if pp > 0. { 2. * m.char_tp / pp } else { 1. };
                        w += d.weight;
                    }
                    if d.constraint && d.origin == 0 {
                        hn += 1;
                        humans += !dec.decode(&p.q, &d.ranges).is_empty() as usize;
                    }
                }
                rows.push(json!({"decoder": g, "char_f1": f / w, "constraint_human_documents": hn, "human_documents_with_span": humans,
                                 "satisfies": humans <= limit}));
            }
            let key = |r: &Value| -> (f64, f64, f64, f64, f64) {
                let d = &r["decoder"];
                (r["char_f1"].as_f64().unwrap(), d["min_words"].as_f64().unwrap(), d["tau_hi"].as_f64().unwrap(),
                 -d["gap"].as_f64().unwrap(), if d["snap"].is_null() { 1. } else { 0. })
            };
            let ok: Vec<&Value> = rows.iter().filter(|r| r["satisfies"] == true).collect();
            let chosen = if !ok.is_empty() {
                (*ok.iter().max_by(|a, b| { let (x, y) = (key(a), key(b)); x.partial_cmp(&y).unwrap() }).unwrap()).clone()
            } else {
                rows.iter().max_by(|a, b| {
                    let (ha, hb) = (a["human_documents_with_span"].as_u64().unwrap(), b["human_documents_with_span"].as_u64().unwrap());
                    hb.cmp(&ha).then(key(a).partial_cmp(&key(b)).unwrap())
                }).unwrap().clone()
            };
            out["system"] = systems[0].clone();
            out["human_constraint"] = cfg["human_constraint"].clone();
            out["grid"] = json!(rows);
            out["chosen"] = chosen;
            out["rule"] = json!("highest balanced character F1 on the primary mixed documents among points with at most max_documents constraint human documents carrying a span; ties: larger min_words, larger tau_hi, smaller gap, snap off; if none qualifies, fewest such human documents then highest F1");
        }
        "fit_cal" => {
            ensure!(systems.len() == 1, "fit_cal takes one system");
            let o = load_system(&systems[0], &want)?;
            let dec = Decoder::from_json(&cfg["decoder"])?;
            let (mut sent, mut spans) = (Vec::new(), Vec::new());
            for d in &docs {
                let p = probs(d, &o)?;
                let k = d.ranges.iter().filter(|r| (r.1..r.2).any(|w| d.word_labels[w] >= 0)).count().max(1) as f64;
                for &(_, a, e) in &d.ranges {
                    if !(a..e).any(|w| d.word_labels[w] >= 0) {
                        continue;
                    }
                    let max = p.q[a..e].iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    let y = (a..e).any(|w| d.word_labels[w] == 1) as u8 as f64;
                    sent.push((max, y, d.weight / k));
                }
                let runs = dec.decode(&p.q, &d.ranges);
                let k = runs.len().max(1) as f64;
                for (a, e) in runs {
                    let mean = p.q[a..e].iter().sum::<f64>() / (e - a) as f64;
                    let y = ((a..e).filter(|w| d.word_labels[*w] == 1).count() as f64 / (e - a) as f64 >= 0.5) as u8 as f64;
                    spans.push((mean, y, d.weight / k));
                }
            }
            let fit = |samples: Vec<(f64, f64, f64)>, input: &str, target: &str| -> Result<Value> {
                let n1 = samples.iter().filter(|x| x.1 == 1.).count();
                let n0 = samples.len() - n1;
                ensure!(n0 > 0 && n1 > 0, "{target}: both classes required");
                let (lo, hi) = (1. / (2. * n0 as f64), 1. - 1. / (2. * n1 as f64));
                let scale = samples.len() as f64 / samples.iter().map(|x| x.2).sum::<f64>();
                let knots = fit_isotonic_scores(samples.iter().map(|x| (x.0, x.1, x.2 * scale)).collect())?;
                // Keep the end points of every constant block: the same piecewise-linear function.
                let mut kept: Vec<(f64, f64)> = Vec::new();
                for (i, k) in knots.iter().enumerate() {
                    let v = k.involvement.clamp(lo, hi);
                    let prev = i.checked_sub(1).map(|j| knots[j].involvement.clamp(lo, hi));
                    let next = knots.get(i + 1).map(|k| k.involvement.clamp(lo, hi));
                    if prev != Some(v) || next != Some(v) {
                        kept.push((k.score, v));
                    }
                }
                Ok(json!({"method": "isotonic_score_v1_clipped", "input": input, "target": target, "bound": sp::SCORE_MAP_BOUND,
                          "lo": lo, "hi": hi, "knots": kept.iter().map(|(x, y)| json!({"score": x, "value": y})).collect::<Vec<_>>(),
                          "fit": {"samples": samples.len(), "negatives": n0, "positives": n1, "pav_levels": knots.len(),
                                  "weights": "document balanced weight / emitted sentences (spans) of the document"}}))
            };
            let mut sm = fit(sent, "max_tilted_word_probability_in_sentence", "sentence_contains_ai_word")?;
            let mut pm = fit(spans, "mean_tilted_word_probability_in_span", "at_least_half_of_span_words_ai")?;
            let fits = json!({"sentence": sm["fit"].take(), "span": pm["fit"].take()});
            sm.as_object_mut().unwrap().remove("fit");
            pm.as_object_mut().unwrap().remove("fit");
            ScoreMap::from_json(&sm)?;
            ScoreMap::from_json(&pm)?;
            out["system"] = systems[0].clone();
            out["decoder"] = cfg["decoder"].clone();
            out["sentence_confidence"] = sm;
            out["span_confidence"] = pm;
            out["fit"] = fits;
        }
        _ => bail!("mode must be metrics, grid or fit_cal"),
    }
    Ok(out)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    ensure!(args.len() == 3, "usage: a_spans_evaluate CONFIG.json OUT.json");
    let out = Path::new(&args[2]);
    ensure!(!out.exists(), "new output required");
    let cfg_path = Path::new(&args[1]);
    let cfg: Value = serde_json::from_slice(&fs::read(cfg_path)?)?;
    let v = run(&cfg, &sha256_file(cfg_path)?)?;
    let tmp = out.with_extension("tmp");
    let mut b = serde_json::to_vec_pretty(&v)?;
    b.push(b'\n');
    fs::write(&tmp, b)?;
    fs::rename(tmp, out)?;
    println!("{}", serde_json::to_string(&json!({"mode": v["mode"], "out": out, "populations": v["populations"]}))?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str, labels: Vec<i8>) -> Doc {
        let words = sp::words(text);
        let ranges = sp::sentence_word_ranges(&sp::sentences(text), &words).unwrap();
        let n = words.len();
        let word_ai_chars = words.iter().zip(&labels).map(|(w, l)| if *l == 1 { w.end_char - w.start_char } else { 0 }).collect();
        Doc { pop: "t".into(), primary: true, constraint: false, id: "d".into(), text_sha256: String::new(), origin: 2,
              share: 0.5, generator: None, cohort: "x".into(), group: "g".into(), domain: "x".into(), operation: None,
              spliced: None, groups: (0..n).map(|i| vec![i]).collect(), ranges, word_labels: labels, word_ai_chars,
              word_copied: vec![false; n], token_labels: vec![], word_mask: vec![], words, weight: 1. }
    }

    #[test]
    fn char_and_span_metrics() {
        // "aa bb cc. dd ee." words: aa bb cc. dd ee. ; truth: words 1-2 and 4.
        let d = doc("aa bb cc. dd ee.", vec![0, 1, 1, 0, 1]);
        let q = vec![0.1, 0.9, 0.9, 0.9, 0.1];
        let pr = Probs { p_tok: vec![], tilt: 0., p: q.clone(), q, share: 0.5, target: 0.5, confidence: 1. };
        let dec = Decoder { tau_hi: 0.5, delta: 0., min_words: 1, gap: 0, snap: None };
        let id = ScoreMap::Identity;
        let m = doc_metrics(&d, &pr, Some(&Emission { decoder: &dec, sentence: &id, span: &id }));
        // Decoded words 1..4 (bb cc. dd) = 2+3+2 chars; truth bb cc. ee. = 2+3+3; overlap 5.
        assert_eq!((m.char_tp, m.char_pred, m.char_true), (5., 7., 8.));
        // One decoded run [1,4) vs truth runs [1,3), [4,5): IoU with [1,3) = 5/7 -> one match.
        assert!((m.span_f1 - 2. / 3.).abs() < 1e-12);
        assert_eq!(m.boundaries, vec![[0., 1., 0., 3.]]);
        assert!((m.mean_best_iou.unwrap() - (5. / 7. + 0.) / 2.).abs() < 1e-12);
        // Sentences: [aa bb cc.] contains AI; [dd ee.] contains AI.
        assert_eq!(m.sentences.len(), 2);
        assert_eq!(m.sentences[0].1, 1.);
        assert!((m.sentences[0].3 - 2. / 3.).abs() < 1e-12);
    }

    #[test]
    fn ece_bins() {
        let (e, b) = ece(&[(0.05, 0., 1.), (0.95, 1., 1.), (0.95, 0., 2.)]).unwrap();
        // Bin 0: |0.05-0| weight 1/4; bin 9: |0.95 - 1/3| weight 3/4.
        assert!((e - (0.25 * 0.05 + 0.75 * (0.95 - 1. / 3.))).abs() < 1e-12);
        assert!((b - (0.0025 + 0.0025 + 2. * 0.9025) / 4.).abs() < 1e-12);
    }

    #[test]
    fn grid_has_108_points() {
        assert_eq!(decoder_grid().len(), 108);
        for g in decoder_grid() {
            Decoder::from_json(&g).unwrap();
        }
    }

    /// The v1.1 grid as written before the optional `grid` field, point for point.
    fn v1_1_grid() -> Vec<Value> {
        let mut out = Vec::new();
        for tau in [0.5, 0.6, 0.7] {
            for delta in [0.0, 0.1, 0.2] {
                for m in [1u64, 3, 5] {
                    for g in [0u64, 2] {
                        for snap in [Value::Null, json!(0.8)] {
                            out.push(json!({"rule": sp::DECODER_RULE, "tau_hi": tau, "delta": delta, "min_words": m, "gap": g, "snap": snap}));
                        }
                    }
                }
            }
        }
        out
    }

    #[test]
    fn default_grid_is_the_v1_1_grid_in_order() {
        assert_eq!(decoder_grid(), v1_1_grid());
        assert_eq!(decoder_grid_of(&json!({})).unwrap(), v1_1_grid());
        let ser = |v: &Vec<Value>| serde_json::to_string(v).unwrap();
        assert_eq!(ser(&decoder_grid()), ser(&v1_1_grid()));
    }

    #[test]
    fn g576_spec_extends_g108() {
        let spec = json!({"tau_hi": [0.5, 0.6, 0.7, 0.75, 0.8, 0.85, 0.9, 0.95], "min_words": [1, 3, 5, 6, 8, 10]});
        let g = decoder_grid_of(&spec).unwrap();
        assert_eq!(g.len(), 576);
        let distinct: std::collections::BTreeSet<String> = g.iter().map(|p| p.to_string()).collect();
        assert_eq!(distinct.len(), 576);
        for p in v1_1_grid() {
            assert!(g.contains(&p), "{p}");
        }
        // Every added point is more conservative: tau_hi above 0.7 or min_words above 5.
        for p in g.iter().filter(|p| !v1_1_grid().contains(p)) {
            assert!(p["tau_hi"].as_f64().unwrap() > 0.7 || p["min_words"].as_u64().unwrap() > 5, "{p}");
        }
        assert_eq!(g.len() - 108, 468);
        // Points differing only in delta keep delta ascending in grid order.
        let pos = |d: f64| g.iter().position(|p| p["tau_hi"] == json!(0.95) && p["min_words"] == json!(10) &&
                                             p["gap"] == json!(0) && p["snap"].is_null() && p["delta"] == json!(d)).unwrap();
        assert!(pos(0.0) < pos(0.1) && pos(0.1) < pos(0.2));
    }

    #[test]
    fn grid_spec_is_checked() {
        assert!(decoder_grid_of(&json!({"tau": [0.5]})).is_err());
        assert!(decoder_grid_of(&json!({"tau_hi": []})).is_err());
        assert!(decoder_grid_of(&json!({"tau_hi": [0.5, 0.5]})).is_err());
        assert!(decoder_grid_of(&json!({"min_words": 5})).is_err());
        assert!(decoder_grid_of(&json!([0.5])).is_err());
    }
}

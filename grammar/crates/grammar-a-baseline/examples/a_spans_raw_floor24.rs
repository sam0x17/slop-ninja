//! Evaluate raw token outputs using the runtime decoder and a 24-word gold floor.
//! Usage: a_spans_raw_floor24 CONFIG.json OUT.json
//! All input files are SHA-256 bound. Outputs contain identities and statistics,
//! never text. This tool opens labels: use only after the experiment's read gate.
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::full_a_spans as sp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

const FLOOR: usize = 24;
const RULE: &str = "slop_ninja_raw_span_floor24_v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: PathBuf,
    sha256: String,
}

impl Input {
    fn read(&self) -> Result<Vec<u8>> {
        let bytes = fs::read(&self.path)?;
        ensure!(
            hex::encode(Sha256::digest(&bytes)) == self.sha256,
            "input digest differs"
        );
        Ok(bytes)
    }

    fn rows<T: for<'a> Deserialize<'a>>(&self) -> Result<Vec<T>> {
        let bytes = self.read()?;
        std::str::from_utf8(&bytes)?
            .lines()
            .map(|line| Ok(serde_json::from_str(line)?))
            .collect()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema: String,
    split: String,
    rows: Input,
    offsets: Input,
    logits: Input,
    /// Served document outputs on the 0..1 scale, after the system's frozen map.
    documents: Input,
    /// One or more frozen runtime emission specs, each with min_words >= 24.
    emissions: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
struct Row {
    id: String,
    text_sha256: String,
    split: String,
    text: String,
    origin: String,
    ai_byte_spans: Option<Vec<(usize, usize)>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Offsets {
    schema: String,
    id: String,
    text_sha256: String,
    tokens: usize,
    token_offsets: Vec<(usize, usize)>,
    word_mask: Vec<bool>,
    offset_unit: String,
    labels_included: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Logits {
    schema: String,
    id: String,
    text_sha256: String,
    tokens: usize,
    token_logits: Vec<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: String,
    id: String,
    text_sha256: String,
    share: f64,
    confidence: f64,
}

fn keyed<T>(rows: Vec<T>, id: impl Fn(&T) -> &str) -> Result<BTreeMap<String, T>> {
    let mut out = BTreeMap::new();
    for row in rows {
        let key = id(&row).to_owned();
        ensure!(
            !key.is_empty() && out.insert(key, row).is_none(),
            "empty or duplicate id"
        );
    }
    Ok(out)
}

struct Gold {
    words: Vec<sp::Segment>,
    labels: Vec<i8>,
    ignored: Vec<bool>,
    long: Vec<(usize, usize)>,
    short_runs: usize,
}

impl Gold {
    fn new(row: &Row) -> Result<Self> {
        let words = sp::words(&row.text);
        ensure!(!words.is_empty(), "empty document");
        let spans = match row.origin.as_str() {
            "human_only" => {
                ensure!(
                    row.ai_byte_spans.as_ref().is_none_or(Vec::is_empty),
                    "human has AI spans"
                );
                vec![]
            }
            "model_only" => vec![(0, row.text.len())],
            "mixed" => row
                .ai_byte_spans
                .clone()
                .context("mixed row needs exact AI spans")?,
            _ => anyhow::bail!("unknown origin"),
        };
        let mut end = 0;
        for &(a, b) in &spans {
            ensure!(
                a >= end && a < b && row.text.is_char_boundary(a) && row.text.is_char_boundary(b),
                "gold spans must be sorted, disjoint, nonempty UTF-8 byte ranges"
            );
            end = b;
        }
        let chars: Vec<_> = row.text.char_indices().collect();
        let labels: Vec<i8> = words
            .iter()
            .map(|w| {
                let ai = chars[w.start_char..w.end_char]
                    .iter()
                    .filter(|(b, _)| spans.iter().any(|&(a, e)| a <= *b && *b < e))
                    .count();
                if ai == w.end_char - w.start_char {
                    1
                } else if ai == 0 {
                    0
                } else {
                    -1
                }
            })
            .collect();
        let mut long = vec![];
        let mut short_runs = 0;
        let mut at = 0;
        while at < labels.len() {
            if labels[at] != 1 {
                at += 1;
                continue;
            }
            let a = at;
            while at < labels.len() && labels[at] == 1 {
                at += 1;
            }
            if at - a >= FLOOR {
                long.push((a, at));
            } else {
                short_runs += 1;
            }
        }
        let mut ignored: Vec<_> = labels.iter().map(|&y| y != 0).collect();
        for &(a, b) in &long {
            ignored[a..b].fill(false);
        }
        Ok(Self {
            words,
            labels,
            ignored,
            long,
            short_runs,
        })
    }

    fn chars(&self, a: usize, b: usize) -> usize {
        self.words[a..b]
            .iter()
            .map(|w| w.end_char - w.start_char)
            .sum()
    }

    fn iou(&self, a: (usize, usize), b: (usize, usize)) -> f64 {
        let start = a.0.max(b.0);
        let end = a.1.min(b.1);
        if start >= end {
            return 0.;
        }
        self.chars(start, end) as f64 / self.chars(a.0.min(b.0), a.1.max(b.1)) as f64
    }
}

#[derive(Default, Serialize)]
struct Metric {
    id: String,
    text_sha256: String,
    mixed_in_scope: bool,
    human_document: bool,
    short_gold_runs: usize,
    ignored_ai_words: usize,
    char_tp: usize,
    char_pred: usize,
    char_gold: usize,
    human_chars: usize,
    fp_human_chars: usize,
    char_f1: Option<f64>,
    human_has_span: bool,
    gold_spans: usize,
    counted_spans: usize,
    ignored_spans: usize,
    matched_spans: usize,
    /// Each entry is [gold count, matched count], for 24-49, 50-99, 100+ words.
    recall_bands: [[usize; 2]; 3],
    word_brier_raw: Option<f64>,
    word_brier_tilted: Option<f64>,
    /// [confidence, target]; only sentences with fully scored words.
    sentence_confidence: Vec<[f64; 2]>,
    /// [confidence, target]; only decoded spans with fully scored words.
    span_confidence: Vec<[f64; 2]>,
    confidence_sentences_excluded: usize,
    confidence_spans_excluded: usize,
}

fn ratio(a: usize, b: usize) -> Option<f64> {
    (b > 0).then(|| a as f64 / b as f64)
}

fn metrics(row: &Row, g: &Gold, emission: &sp::Emission) -> Result<Metric> {
    let mut m = Metric {
        id: row.id.clone(),
        text_sha256: row.text_sha256.clone(),
        mixed_in_scope: row.origin == "mixed" && !g.long.is_empty(),
        human_document: row.origin == "human_only",
        short_gold_runs: g.short_runs,
        ignored_ai_words: g.ignored.iter().filter(|&&x| x).count(),
        human_has_span: row.origin == "human_only" && !emission.spans.is_empty(),
        gold_spans: g.long.len(),
        ..Metric::default()
    };
    let mut decoded = vec![false; g.words.len()];
    let mut runs = vec![];
    for s in &emission.spans {
        let a = g
            .words
            .iter()
            .position(|w| w.start_char == s.start_char)
            .context("span start")?;
        let b = g
            .words
            .iter()
            .position(|w| w.end_char == s.end_char)
            .context("span end")?
            + 1;
        ensure!(
            b - a >= FLOOR && s.words == b - a,
            "decoder emitted under floor"
        );
        decoded[a..b].fill(true);
        runs.push((a, b));
    }
    let mut candidates = vec![];
    for (i, &truth) in g.long.iter().enumerate() {
        for (j, &run) in runs.iter().enumerate() {
            let iou = g.iou(truth, run);
            if iou >= 0.5 {
                candidates.push((iou, i, j));
            }
        }
    }
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut matched_gold = vec![false; g.long.len()];
    let mut matched_runs = vec![false; runs.len()];
    for (_, i, j) in candidates {
        if !matched_gold[i] && !matched_runs[j] {
            matched_gold[i] = true;
            matched_runs[j] = true;
            m.matched_spans += 1;
        }
    }
    for (i, &(a, b)) in g.long.iter().enumerate() {
        let band = if b - a < 50 {
            0
        } else if b - a < 100 {
            1
        } else {
            2
        };
        m.recall_bands[band][0] += 1;
        m.recall_bands[band][1] += usize::from(matched_gold[i]);
    }
    for (j, &(a, b)) in runs.iter().enumerate() {
        let touches = g.long.iter().any(|&(x, y)| a < y && x < b);
        let ignored: usize = (a..b)
            .filter(|&w| g.ignored[w])
            .map(|w| g.chars(w, w + 1))
            .sum();
        if !matched_runs[j] && !touches && 2 * ignored >= g.chars(a, b) {
            m.ignored_spans += 1;
        } else {
            m.counted_spans += 1;
        }
        if g.ignored[a..b].iter().any(|&x| x) {
            m.confidence_spans_excluded += 1;
        } else {
            let ai = g.labels[a..b].iter().filter(|&&y| y == 1).count();
            m.span_confidence.push([
                emission.spans[j].ai_confidence_percent / 100.,
                f64::from(2 * ai >= b - a),
            ]);
        }
    }
    let mut raw = 0.;
    let mut tilted = 0.;
    let mut scored = 0;
    for (w, &covered) in decoded.iter().enumerate() {
        if g.ignored[w] {
            continue;
        }
        let chars = g.chars(w, w + 1);
        let ai = g.labels[w] == 1;
        m.char_gold += usize::from(ai) * chars;
        m.char_pred += usize::from(covered) * chars;
        m.char_tp += usize::from(ai && covered) * chars;
        m.human_chars += usize::from(!ai) * chars;
        m.fp_human_chars += usize::from(!ai && covered) * chars;
        let y = f64::from(ai);
        raw += (emission.diagnostics.word_probabilities_raw[w] - y).powi(2);
        tilted += (emission.diagnostics.word_probabilities[w] - y).powi(2);
        scored += 1;
    }
    m.word_brier_raw = (scored > 0).then(|| raw / scored as f64);
    m.word_brier_tilted = (scored > 0).then(|| tilted / scored as f64);
    m.char_f1 = m
        .mixed_in_scope
        .then(|| 2. * m.char_tp as f64 / (m.char_pred + m.char_gold) as f64);
    for s in &emission.sentences {
        let ws: Vec<_> = g
            .words
            .iter()
            .enumerate()
            .filter(|(_, w)| w.start_byte >= s.start_byte && w.end_byte <= s.end_byte)
            .map(|(i, _)| i)
            .collect();
        ensure!(!ws.is_empty(), "sentence without words");
        if ws.iter().any(|&w| g.ignored[w]) {
            m.confidence_sentences_excluded += 1;
            continue;
        }
        m.sentence_confidence.push([
            s.ai_confidence_percent / 100.,
            f64::from(ws.iter().any(|&w| g.labels[w] == 1)),
        ]);
    }
    Ok(m)
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, n) = values.fold((0., 0), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / n as f64)
}

fn ece(points: impl Iterator<Item = [f64; 2]>) -> Value {
    let mut bins = [(0usize, 0., 0.); 10];
    for [p, y] in points {
        let b = &mut bins[((p * 10.) as usize).min(9)];
        b.0 += 1;
        b.1 += p;
        b.2 += y;
    }
    let n: usize = bins.iter().map(|b| b.0).sum();
    json!({"events":n, "equal_width_bins":10,
        "ece":(n > 0).then(|| bins.iter().map(|b| (b.1-b.2).abs()).sum::<f64>() / n as f64)})
}

fn aggregate(rows: &[Metric]) -> Value {
    let mixed: Vec<_> = rows.iter().filter(|r| r.mixed_in_scope).collect();
    let sum = |f: fn(&Metric) -> usize| mixed.iter().map(|r| f(r)).sum::<usize>();
    let mut bands = [[0; 2]; 3];
    for m in &mixed {
        for (a, b) in bands.iter_mut().zip(m.recall_bands) {
            a[0] += b[0];
            a[1] += b[1];
        }
    }
    json!({"documents":rows.len(), "mixed_in_scope":mixed.len(),
        "mean_character_f1":mean(mixed.iter().filter_map(|m| m.char_f1)),
        "in_context_human_fpr":ratio(sum(|m|m.fp_human_chars),sum(|m|m.human_chars)),
        "human_documents":rows.iter().filter(|m|m.human_document).count(),
        "humans_with_span_rate":ratio(rows.iter().filter(|m|m.human_has_span).count(),rows.iter().filter(|m|m.human_document).count()),
        "span_precision":ratio(sum(|m|m.matched_spans),sum(|m|m.counted_spans)),
        "span_recall":ratio(sum(|m|m.matched_spans),sum(|m|m.gold_spans)),
        "recall_bands":bands, "recall_band_names":["24-49","50-99","100+"],
        "word_brier_raw":mean(rows.iter().filter_map(|m|m.word_brier_raw)),
        "word_brier_tilted":mean(rows.iter().filter_map(|m|m.word_brier_tilted)),
        "span_confidence":ece(rows.iter().flat_map(|m|m.span_confidence.iter().copied())),
        "sentence_confidence":ece(rows.iter().flat_map(|m|m.sentence_confidence.iter().copied())),
        "confidence_spans_excluded":rows.iter().map(|m|m.confidence_spans_excluded).sum::<usize>(),
        "confidence_sentences_excluded":rows.iter().map(|m|m.confidence_sentences_excluded).sum::<usize>()})
}

fn evaluate(cfg: &Config) -> Result<Value> {
    ensure!(
        cfg.schema == "slop_ninja_raw_span_floor24_config_v1",
        "config schema"
    );
    ensure!(
        ["train", "train_mining", "dev_iter", "cal"].contains(&cfg.split.as_str()),
        "split is sealed or unsupported"
    );
    ensure!(!cfg.emissions.is_empty(), "no decoders");
    let specs = cfg
        .emissions
        .iter()
        .map(|(name, v)| {
            let spec = sp::Spec::from_json(v)?;
            ensure!(
                spec.decoder.min_words >= FLOOR,
                "decoder below 24-word floor"
            );
            Ok((name.clone(), spec))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let rows = keyed(cfg.rows.rows::<Row>()?, |r| &r.id)?;
    ensure!(!rows.is_empty(), "no rows");
    let offsets = keyed(cfg.offsets.rows::<Offsets>()?, |r| &r.id)?;
    let logits = keyed(cfg.logits.rows::<Logits>()?, |r| &r.id)?;
    let documents = keyed(cfg.documents.rows::<Document>()?, |r| &r.id)?;
    ensure!(
        rows.keys().eq(offsets.keys())
            && rows.keys().eq(logits.keys())
            && rows.keys().eq(documents.keys()),
        "input identity sets differ"
    );
    let mut results: BTreeMap<String, Vec<Metric>> =
        specs.keys().map(|name| (name.clone(), vec![])).collect();
    for (id, row) in &rows {
        let (o, l, d) = (&offsets[id], &logits[id], &documents[id]);
        ensure!(row.split == cfg.split, "row split differs");
        ensure!(
            hex::encode(Sha256::digest(row.text.as_bytes())) == row.text_sha256
                && [&o.text_sha256, &l.text_sha256, &d.text_sha256]
                    .iter()
                    .all(|h| **h == row.text_sha256),
            "text digest differs"
        );
        ensure!(
            o.schema == "slop_ninja_full_a_token_offsets_v1"
                && o.offset_unit == "unicode_scalar"
                && !o.labels_included,
            "offset schema or semantics"
        );
        ensure!(
            l.schema == "slop_ninja_full_a_token_logits_v1"
                && d.schema == "slop_ninja_span_document_v1",
            "output schema"
        );
        ensure!(
            o.tokens == o.token_offsets.len()
                && o.tokens == o.word_mask.len()
                && l.tokens == o.tokens
                && l.tokens == l.token_logits.len()
                && l.token_logits.iter().all(|v| v.is_finite()),
            "token length or nonfinite logit"
        );
        ensure!(
            [d.share, d.confidence]
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            "document probability"
        );
        let gold = Gold::new(row)?;
        let token_logits: Vec<_> = l
            .token_logits
            .iter()
            .zip(&o.word_mask)
            .map(|(&v, &keep)| keep.then_some(v))
            .collect();
        for (name, spec) in &specs {
            let emission = sp::emit(
                &row.text,
                &o.token_offsets,
                &o.word_mask,
                &token_logits,
                d.share,
                d.confidence,
                spec,
            )?;
            results
                .get_mut(name)
                .unwrap()
                .push(metrics(row, &gold, &emission)?);
        }
    }
    let systems: BTreeMap<_, _> = results
        .into_iter()
        .map(|(name, rows)| (name, json!({"aggregate":aggregate(&rows),"rows":rows})))
        .collect();
    Ok(
        json!({"schema":RULE,"complete":true,"split":cfg.split,"floor_words":FLOOR,
        "inputs":{"rows":cfg.rows.sha256,"offsets":cfg.offsets.sha256,"logits":cfg.logits.sha256,"documents":cfg.documents.sha256},
        "emissions":cfg.emissions, "systems":systems,
        "aggregation":"F1 and word Brier: equal document means; human FPR: pooled characters; span P/R: pooled mixed-in-scope runs; ECE: pooled fully scored events",
        "confidence_scope":"Exclude every sentence or decoded span touching an ignored short-AI or partial-AI word; span target is at least half AI words; sentence target is any AI word"}),
    )
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 2,
        "usage: a_spans_raw_floor24 CONFIG.json OUT.json"
    );
    ensure!(!PathBuf::from(&args[1]).exists(), "output already exists");
    let bytes = fs::read(&args[0])?;
    let cfg: Config = serde_json::from_slice(&bytes)?;
    let mut result = evaluate(&cfg)?;
    result["config_sha256"] = json!(hex::encode(Sha256::digest(&bytes)));
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[1])?;
    serde_json::to_writer_pretty(&mut out, &result)?;
    out.write_all(b"\n")?;
    out.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_span_eval as gate;

    fn spec() -> Value {
        json!({"rule":sp::EMISSION_RULE,"word_rule":sp::WORD_RULE,
            "word_probability_rule":sp::WORD_PROBABILITY_RULE,"tilt_rule":sp::TILT_RULE,
            "splitter":sp::SPLITTER_ID,
            "decoder":{"rule":sp::DECODER_RULE,"tau_hi":0.5,"delta":0.1,"min_words":24,"gap":0,"snap":null},
            "sentence_confidence":{"method":"identity_v1"},"span_confidence":{"method":"identity_v1"}})
    }

    fn row(gold: &[(usize, usize)], origin: &str) -> Row {
        let text = vec!["écho"; 90].join(" ");
        let words = sp::words(&text);
        Row {
            id: "synthetic".into(),
            text_sha256: hex::encode(Sha256::digest(text.as_bytes())),
            split: "dev_iter".into(),
            ai_byte_spans: Some(
                gold.iter()
                    .map(|&(a, b)| (words[a].start_byte, words[b - 1].end_byte))
                    .collect(),
            ),
            text,
            origin: origin.into(),
        }
    }

    fn emission(row: &Row, high: &[(usize, usize)]) -> sp::Emission {
        let words = sp::words(&row.text);
        let logits: Vec<Option<f64>> = (0..words.len())
            .map(|w| {
                Some(if high.iter().any(|&(a, b)| a <= w && w < b) {
                    10.
                } else {
                    -10.
                })
            })
            .collect();
        let share = logits
            .iter()
            .map(|x| 1. / (1. + (-x.unwrap()).exp()))
            .sum::<f64>()
            / words.len() as f64;
        sp::emit(
            &row.text,
            &words
                .iter()
                .map(|w| (w.start_char, w.end_char))
                .collect::<Vec<_>>(),
            &vec![true; words.len()],
            &logits,
            share,
            1.,
            &sp::Spec::from_json(&spec()).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn single_run_matches_subnet_gate_with_unicode_and_failed_localization() {
        for truth in [(10, 34), (10, 60), (0, 90)] {
            let row = row(&[truth], "mixed");
            let gold = Gold::new(&row).unwrap();
            for high in [
                vec![],
                vec![truth],
                vec![(0, 90)],
                vec![(30, 65)],
                vec![(0, 23)],
            ] {
                let em = emission(&row, &high);
                let ours = metrics(&row, &gold, &em).unwrap();
                let spans: Vec<_> = em
                    .spans
                    .iter()
                    .map(|s| gate::Span {
                        start_byte: s.start_byte as u32,
                        end_byte: s.end_byte as u32,
                        words: s.words as u32,
                    })
                    .collect();
                let (a, b) = row.ai_byte_spans.as_ref().unwrap()[0];
                let theirs = gate::compute(
                    &row.text,
                    gate::Kind::Mixed,
                    Some((a as u32, b as u32)),
                    gate::Emission::Spans(&spans),
                    Some(24),
                )
                .unwrap();
                assert_eq!(theirs.format_fault, None);
                assert_eq!(theirs.fp_human as usize, ours.fp_human_chars);
                assert_eq!(theirs.human as usize, ours.human_chars);
                assert_eq!(
                    theirs.f1_ppb,
                    gate::f1_ppb(
                        ours.char_tp as u64,
                        ours.char_pred as u64,
                        ours.char_gold as u64
                    )
                );
                assert!((ours.char_f1.unwrap() - f64::from(theirs.f1_ppb) / 1e9).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn mixed_short_runs_are_masked_not_human_and_gate_counts_agree_on_projection() {
        let row = row(&[(10, 34), (50, 55)], "mixed");
        let g = Gold::new(&row).unwrap();
        let em = emission(&row, &[(0, 90)]);
        let m = metrics(&row, &g, &em).unwrap();
        assert_eq!(
            (
                m.short_gold_runs,
                m.ignored_ai_words,
                m.char_gold,
                m.human_chars,
                m.fp_human_chars
            ),
            (1, 5, 96, 244, 244)
        );
        assert_eq!(m.confidence_spans_excluded, 1);
        assert_eq!(m.confidence_sentences_excluded, 1);
        // Independent gate oracle after replacing only ignored AI characters
        // with whitespace. Byte ranges are recomputed from character indices.
        let projected: String = row
            .text
            .chars()
            .enumerate()
            .map(|(i, c)| {
                if g.words
                    .iter()
                    .enumerate()
                    .any(|(w, s)| g.ignored[w] && s.start_char <= i && i < s.end_char)
                {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        let byte_at = |c| {
            projected
                .char_indices()
                .map(|(b, _)| b)
                .chain([projected.len()])
                .nth(c)
                .unwrap()
        };
        let a = byte_at(g.words[10].start_char);
        let b = byte_at(g.words[33].end_char);
        let spans = [gate::Span {
            start_byte: 0,
            end_byte: projected.len() as u32,
            words: 85,
        }];
        let t = gate::compute(
            &projected,
            gate::Kind::Mixed,
            Some((a as u32, b as u32)),
            gate::Emission::Spans(&spans),
            Some(24),
        )
        .unwrap();
        assert_eq!(t.format_fault, None);
        assert_eq!(
            (t.fp_human as usize, t.human as usize),
            (m.fp_human_chars, m.human_chars)
        );
        assert_eq!(
            t.f1_ppb,
            gate::f1_ppb(m.char_tp as u64, m.char_pred as u64, m.char_gold as u64)
        );
        let expected = (0..90)
            .filter(|&w| !g.ignored[w])
            .map(|w| {
                (em.diagnostics.word_probabilities_raw[w] - f64::from(g.labels[w] == 1)).powi(2)
            })
            .sum::<f64>()
            / 85.;
        assert_eq!(m.word_brier_raw, Some(expected));
    }

    #[test]
    fn short_only_partial_labels_humans_and_recall_bands() {
        let r = row(&[(0, 23)], "mixed");
        let m = metrics(&r, &Gold::new(&r).unwrap(), &emission(&r, &[(0, 24)])).unwrap();
        assert!(!m.mixed_in_scope);
        assert_eq!(m.char_f1, None);
        assert_eq!((m.gold_spans, m.ignored_spans, m.counted_spans), (0, 1, 0));
        let mut r = row(&[(0, 24)], "mixed");
        r.ai_byte_spans.as_mut().unwrap()[0].0 = 2; // Inside a word, on a Unicode boundary.
        let g = Gold::new(&r).unwrap();
        assert!(g.long.is_empty());
        assert_eq!(g.labels[0], -1);
        assert_eq!(g.ignored.iter().filter(|&&v| v).count(), 24);
        for high in [vec![], vec![(0, 23)], vec![(0, 24)]] {
            let r = row(&[], "human_only");
            let e = emission(&r, &high);
            let m = metrics(&r, &Gold::new(&r).unwrap(), &e).unwrap();
            assert_eq!(m.human_has_span, high == vec![(0, 24)]);
        }
        let r = row(&[(0, 24), (30, 80)], "mixed");
        let m = metrics(
            &r,
            &Gold::new(&r).unwrap(),
            &emission(&r, &[(0, 24), (30, 80)]),
        )
        .unwrap();
        assert_eq!(m.recall_bands, [[1, 1], [1, 1], [0, 0]]);
        assert_eq!((m.matched_spans, m.counted_spans), (2, 2));
    }

    fn input(dir: &std::path::Path, name: &str, rows: &[Value]) -> Input {
        let bytes = rows
            .iter()
            .map(|r| serde_json::to_string(r).unwrap() + "\n")
            .collect::<String>()
            .into_bytes();
        let path = dir.join(name);
        fs::write(&path, &bytes).unwrap();
        Input {
            path,
            sha256: hex::encode(Sha256::digest(bytes)),
        }
    }

    #[test]
    fn bound_raw_files_end_to_end_reject_identity_hash_shape_and_floor_changes() {
        let dir = tempfile::tempdir().unwrap();
        let r = row(&[(0, 24)], "mixed");
        let ws = sp::words(&r.text);
        let off = json!({"schema":"slop_ninja_full_a_token_offsets_v1","id":r.id,"text_sha256":r.text_sha256,
            "tokens":90,"token_offsets":ws.iter().map(|w|[w.start_char,w.end_char]).collect::<Vec<_>>(),
            "word_mask":vec![true;90],"offset_unit":"unicode_scalar","labels_included":false});
        let mut logits = json!({"schema":"slop_ninja_full_a_token_logits_v1","id":r.id,"text_sha256":r.text_sha256,
            "tokens":90,"token_logits":vec![10.;90]});
        let mut cfg = Config {
            schema: "slop_ninja_raw_span_floor24_config_v1".into(),
            split: "dev_iter".into(),
            rows: input(
                dir.path(),
                "rows",
                &[
                    json!({"id":r.id,"text_sha256":r.text_sha256,"text":r.text,"split":r.split,
                "origin":r.origin,"ai_byte_spans":r.ai_byte_spans}),
                ],
            ),
            offsets: input(dir.path(), "offsets", &[off]),
            logits: input(dir.path(), "logits", &[logits.clone()]),
            documents: input(
                dir.path(),
                "documents",
                &[json!({"schema":"slop_ninja_span_document_v1","id":r.id,
                "text_sha256":r.text_sha256,"share":0.9,"confidence":0.95})],
            ),
            emissions: BTreeMap::from([("d24".into(), spec())]),
        };
        let result = evaluate(&cfg).unwrap();
        assert_eq!(result["systems"]["d24"]["aggregate"]["mixed_in_scope"], 1);
        assert!(!serde_json::to_string(&result).unwrap().contains("écho"));
        cfg.logits.sha256 = "0".repeat(64);
        assert!(evaluate(&cfg).unwrap_err().to_string().contains("digest"));
        cfg.logits = input(dir.path(), "logits", &[logits.clone(), logits.clone()]);
        assert!(
            evaluate(&cfg)
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
        logits["id"] = json!("different");
        cfg.logits = input(dir.path(), "logits", &[logits.clone()]);
        assert!(evaluate(&cfg).unwrap_err().to_string().contains("identity"));
        logits["id"] = json!("synthetic");
        logits["tokens"] = json!(89);
        cfg.logits = input(dir.path(), "logits", &[logits.clone()]);
        assert!(
            evaluate(&cfg)
                .unwrap_err()
                .to_string()
                .contains("token length")
        );
        logits["tokens"] = json!(90);
        cfg.logits = input(dir.path(), "logits", &[logits]);
        cfg.emissions.get_mut("d24").unwrap()["decoder"]["min_words"] = json!(23);
        assert!(evaluate(&cfg).unwrap_err().to_string().contains("floor"));
        cfg.split = "test".into();
        assert!(evaluate(&cfg).unwrap_err().to_string().contains("sealed"));
    }

    #[test]
    fn empty_aggregates_and_confidence_bins_are_explicit() {
        let a = aggregate(&[]);
        assert!(a["mean_character_f1"].is_null());
        assert!(a["span_precision"].is_null());
        assert!(a["sentence_confidence"]["ece"].is_null());
        assert_eq!(
            ece([[1., 1.], [0., 0.], [0.5, 1.]].into_iter())["ece"],
            json!(1. / 6.)
        );
    }
}

//! American Stories (dell-research-harvard/AmericanStories) adapter over the
//! per-year scan archives `faro_<year>.tar.gz` (one JSON file per newspaper
//! page scan, with layout boxes, legibility classes and associated
//! full-article texts).
//!
//! * **Rights.** The dataset is CC BY 4.0; the underlying scans are the
//!   public-domain Chronicling America collection. Text stays in ignored
//!   storage and is not redistributed. The OCR is the dataset's own
//!   layout-and-OCR pipeline (no language-model correction is described).
//! * **Sample.** An article is visited iff
//!   `rank(seed, "american_stories_article", family key) < 2^64 * fraction`,
//!   by identifier only.
//! * **Groups.** Unit and source family: one associated article
//!   (`american_stories_article:<scan id>:<article id>`). Bucket, split
//!   group and `writer_key`: the newspaper title by LCCN
//!   (`american_stories_lccn:<lccn>`), so one newspaper never spans splits.
//!   Bylines are rare and syndicated (wire) copy recurs across titles; the
//!   pool's near-duplicate screen keeps one copy.
//! * **Gates.** Every content box of the article must be classed `Legible`
//!   by the dataset's legibility model. Paragraph blocks are the article's
//!   blank-line-separated segments; lines inside a block are unwrapped,
//!   a lowercase-initial block continues an unfinished previous block, and
//!   line-end hyphens resolved (`ocr`). The shared prose screen and the
//!   first whole-paragraph run of 150..450 words follow, then the English
//!   screen and the OCR quality gate. The residual OCR measures are recorded
//!   per document.
use crate::{
    adapter::Run,
    ocr::{self, Gate, Lexicon, Normalisation, Quality, QualityStats},
    record::{self, Doc},
    splits::rank,
    text::{self, Para, Screen},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Read, path::PathBuf};

pub const COLLECTION: &str = "american_stories_historical";
pub const LICENSE: &str = "CC-BY-4.0";
pub const DATASET: &str = "dell-research-harvard/AmericanStories";

pub struct Options<'a> {
    pub inputs: &'a [PathBuf],
    pub seed: &'a str,
    pub fraction: f64,
    pub revision: &'a str,
    pub lexicon: &'a Lexicon,
    pub gate: Gate,
}

/// Paragraphs of one article text, with normalisation counts.
pub fn paragraphs(article: &str, lex: &Lexicon, n: &mut Normalisation) -> Vec<Para> {
    let t = ocr::map_chars(article, n);
    let lines: Vec<String> = ocr::blocks(&t).iter().map(|b| ocr::unwrap_lines(b, lex, n)).collect();
    ocr::join_continuations(lines, lex, n).into_iter().map(|b| ocr::classify_block(b, Screen::XML)).collect()
}

fn s(v: &Value) -> String {
    match v {
        Value::String(x) => x.clone(),
        Value::Number(x) => x.to_string(),
        _ => String::new(),
    }
}

fn ids(v: &Value) -> Vec<i64> {
    match v {
        Value::Array(a) => a.iter().filter_map(Value::as_i64).collect(),
        Value::String(x) => x.trim_matches(['[', ']']).split(',').filter_map(|p| p.trim().parse().ok()).collect(),
        _ => vec![],
    }
}

pub fn adapt(opt: &Options, run: &mut Run) -> Result<Value> {
    let threshold = if opt.fraction >= 1.0 { u64::MAX } else { (opt.fraction.max(0.0) * u64::MAX as f64) as u64 };
    let method = format!("american_stories_full_article_blank_line_blocks;{};{}", ocr::NORMALISATION_METHOD, text::PARAGRAPH_RUN_METHOD);
    let mut per_file = Vec::new();
    let mut stats = QualityStats::default();
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut by_year: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut norm_total = Normalisation::default();
    for path in opt.inputs {
        let (sha, bytes) = crate::io::file_sha(path)?;
        let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(std::io::BufReader::with_capacity(1 << 20, f)));
        let (mut scans, mut bad_json, mut articles, mut sampled) = (0u64, 0u64, 0u64, 0u64);
        for entry in ar.entries()? {
            if run.full() {
                break;
            }
            let mut e = entry?;
            if !e.header().entry_type().is_file() {
                continue;
            }
            let name = e.path()?.to_string_lossy().into_owned();
            if !name.ends_with(".json") {
                continue;
            }
            let mut buf = Vec::with_capacity(e.size() as usize);
            e.read_to_end(&mut buf)?;
            scans += 1;
            let Ok(d) = serde_json::from_slice::<Value>(&buf) else {
                bad_json += 1;
                continue;
            };
            let file = name.rsplit('/').next().unwrap_or(&name).to_owned();
            let scan_id = file.trim_end_matches(".json").to_owned();
            let date = scan_id.split('_').next().unwrap_or("").to_owned();
            let page = scan_id.split('_').nth(1).unwrap_or("").to_owned();
            let lccn = s(&d["lccn"]["lccn"]);
            let title = crate::wikitext::decode_entities(&s(&d["lccn"]["title"]));
            let state = s(&d["lccn"]["state"]);
            let mut legibility: BTreeMap<i64, (String, String)> = BTreeMap::new();
            for b in d["bboxes"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
                if let Some(id) = b["id"].as_i64() {
                    legibility.insert(id, (s(&b["class"]), s(&b["legibility"])));
                }
            }
            for a in d["full articles"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
                articles += 1;
                let aid = s(&a["full_article_id"]);
                let unit = format!("american_stories_article:{scan_id}:{aid}");
                if rank(opt.seed, "american_stories_article", &unit) >= threshold {
                    continue;
                }
                sampled += 1;
                run.units_seen += 1;
                if lccn.is_empty() {
                    run.reject(&unit, "no_lccn")?;
                    continue;
                }
                let group = format!("american_stories_lccn:{lccn}");
                let article = s(&a["article"]);
                let boxes = ids(&a["object_ids"]);
                let content: Vec<&(String, String)> = boxes.iter().filter_map(|i| legibility.get(i)).filter(|(c, _)| c == "article").collect();
                let all_legible = !content.is_empty() && content.iter().all(|(_, l)| l == "Legible");
                let meta = || {
                    json!({"article_id": unit, "scan_id": scan_id, "lccn": lccn, "newspaper": title, "state": state, "date": date, "page": page,
                           "article_text_sha256": text::sha_hex(article.as_bytes()), "content_boxes": content.len(), "all_boxes_legible": all_legible,
                           "input_file": path.file_name().and_then(|n| n.to_str()), "input_sha256": sha})
                };
                if !run.sampled_meta_grouped(&group, &unit, meta)? {
                    continue;
                }
                if !record::valid_date(&date) {
                    run.reject(&unit, "no_scan_date")?;
                    continue;
                }
                if content.is_empty() {
                    run.reject(&unit, "no_article_content_box")?;
                    continue;
                }
                if let Some((_, l)) = content.iter().find(|(_, l)| l != "Legible") {
                    run.reject(&unit, &format!("content_box_not_legible:{}", l.to_lowercase()))?;
                    continue;
                }
                let year = date[..4].to_owned();
                by_year.entry(year.clone()).or_default().0 += 1;
                let mut norm = Normalisation::default();
                let paras = paragraphs(&article, opt.lexicon, &mut norm);
                for x in &paras {
                    if let Some(r) = x.reason {
                        *breaks.entry(r).or_default() += 1;
                    }
                }
                let Some(ex) = text::paragraph_run(&paras) else {
                    run.reject(&unit, "no_paragraph_run_within_bounds")?;
                    continue;
                };
                if let Some(r) = text::english_screen(&ex.text) {
                    run.reject(&unit, r)?;
                    continue;
                }
                let q = Quality::measure(&ex.text, opt.lexicon);
                if let Some(r) = opt.gate.check(&q) {
                    stats.add(&q);
                    run.reject(&unit, r)?;
                    continue;
                }
                stats.add(&q);
                norm_total.add(&norm);
                by_year.entry(year).or_default().1 += 1;
                let headline = s(&a["headline"]);
                let doc = Doc {
                    id: format!("pool-amstories-{scan_id}-{aid}"),
                    collection: COLLECTION,
                    domain: "historical_newspapers_us",
                    source_group: unit.clone(),
                    writer_key: Some(group.clone()),
                    evidence: "historical_public_domain_proxy",
                    excerpt: &ex,
                    extraction_method: &method,
                    unit: json!({"kind": "american_stories_full_article", "span_basis": "blank_line_block_index_half_open",
                                 "article_text_sha256": text::sha_hex(article.as_bytes()), "scan_json": name, "input_file": path.file_name().and_then(|n| n.to_str()),
                                 "input_sha256": sha, "normalisation": norm.to_value(), "ocr_quality": q.to_value(),
                                 "ocr_text": true, "ocr_artefacts_remaining": q.odd_tokens > 0 || q.alpha_tokens > q.alpha_in_lexicon}),
                    provenance: json!({"dataset": DATASET, "dataset_revision": opt.revision, "scan_id": scan_id, "full_article_id": aid,
                                       "lccn": lccn, "newspaper": title, "state": state, "date": date, "page": page,
                                       "headline_sha256": text::sha_hex(headline.as_bytes()), "byline_present": !s(&a["byline"]).trim().is_empty(),
                                       "source_url": format!("https://chroniclingamerica.loc.gov/lccn/{lccn}/"),
                                       "content_boxes": content.len(), "legibility": "all content boxes Legible"}),
                    date: Some(date.clone()),
                    date_basis: "newspaper_issue_date_from_scan_id",
                    cutoff_policy: "dated_before_cutoff",
                    rights: {
                        let mut r = record::rights(
                            LICENSE,
                            format!("American Stories ({DATASET}, revision {}), CC BY 4.0; {title} (LCCN {lccn}), {date}, page {page}; from Chronicling America, Library of Congress; OCR text, line breaks unwrapped", opt.revision),
                            "https://huggingface.co/datasets/dell-research-harvard/AmericanStories",
                            "Dataset CC BY 4.0; underlying newspaper scans are the public-domain Chronicling America collection.",
                        );
                        r["admission_decision"] = json!("historical_2026-10-27: American Stories (CC BY 4.0) admitted Train-side only (Train and Train-mining), capped and OCR-quality gated; text in ignored storage, never redistributed");
                        r["redistribute_text"] = json!(false);
                        r["recommended_use"] = json!("train_only_capped");
                        r
                    },
                    aliases: vec![],
                };
                run.accept(&unit, &doc)?;
            }
        }
        let d = json!({"role": "american_stories_year_archive", "path": path, "sha256": sha, "bytes": bytes, "scans": scans,
                       "unparsable_scan_json": bad_json, "articles": articles, "articles_sampled": sampled, "sample_fraction": opt.fraction,
                       "sample_threshold_u64": threshold, "dataset": DATASET, "dataset_revision": opt.revision});
        run.input(d.clone());
        per_file.push(d);
    }
    let by_year: serde_json::Map<String, Value> = by_year.into_iter().map(|(y, (n, k))| (y, json!({"legible_articles_screened": n, "admitted_candidates": k}))).collect();
    Ok(json!({"files": per_file, "paragraph_breaks": breaks, "ocr_gate": opt.gate.to_value(), "ocr_quality_screened": stats.to_value(),
              "normalisation_totals_admitted": norm_total.to_value(), "lexicon_sha256": opt.lexicon.sha256, "lexicon_words": opt.lexicon.len(),
              "yield_by_year": by_year,
              "grouping": "unit and source family: associated article; bucket, split group and writer_key: newspaper LCCN",
              "limits": ["OCR misreadings remain in admitted text; their rate is recorded per document and must be matched or capped in training",
                         "syndicated wire copy recurs across newspapers; only exact and near-duplicate screens separate it",
                         "bylines are rare, so no individual writer separation exists beyond the newspaper title"]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn article_paragraphs() {
        let lex = Lexicon::from_words(["the", "death", "was", "reported", "yesterday", "by", "a", "man", "of", "town"]);
        let mut n = Normalisation::default();
        let p = paragraphs("By the Associated Press\n\n\nThe death was re-\nported yesterday by a man of the town in the\n\nnorth of the county.\n\n\nSecond", &lex, &mut n);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].reason, Some("short_paragraph"));
        assert!(p[1].prose);
        assert_eq!(p[1].text, "The death was reported yesterday by a man of the town in the north of the county.");
        assert_eq!((n.hyphen_joins_dropped, n.block_continuations_joined), (1, 1));
        assert_eq!(ids(&json!([5, 11, 0])), vec![5, 11, 0]);
        assert_eq!(ids(&json!("[5, 11]")), vec![5, 11]);
    }
}

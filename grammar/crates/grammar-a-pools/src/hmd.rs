//! British Library Heritage Made Digital newspapers (`biglam/hmd_newspapers`)
//! parquet adapter: article-level OCR text of 19th-century UK newspapers.
//!
//! * **Rights.** CC0 1.0 dataset of public-domain newspapers.
//! * **Sample.** A row is visited iff
//!   `rank(seed, "bl_hmd_article", family key) < 2^64 * fraction`.
//! * **Identity.** Rows carry no article id. The source family is
//!   `bl_hmd_article:<first 20 hex of SHA-256(source, title, date, text)>`
//!   at the pinned dataset revision; `source` (the dataset's own item
//!   reference) is kept in provenance. A row identical to an earlier row
//!   in all four fields is counted and skipped.
//! * **Groups.** Bucket, split group and `writer_key`: the newspaper title
//!   (`bl_hmd_title:<folded title>`), so one newspaper never spans splits.
//! * **Gates.** `item_type` must be an article type (`--item-types`), the
//!   dataset's mean OCR word confidence at least `--min-ocr-confidence`,
//!   then the shared paragraph rules as for American Stories (blank-line
//!   blocks, unwrapped lines, resolved line-end hyphens), the English screen
//!   and the OCR quality gate.
use crate::{
    adapter::Run,
    ocr::{self, Gate, Lexicon, Normalisation, Quality, QualityStats},
    record::{self, Doc},
    splits::rank,
    text::{self, Para, Screen},
};
use anyhow::{Context, Result};
use parquet::{
    file::reader::{FileReader, SerializedFileReader},
    record::Field,
};
use serde_json::{Value, json};
use std::{collections::{BTreeMap, HashSet}, path::PathBuf};

pub const COLLECTION: &str = "bl_hmd_newspapers_historical";
pub const LICENSE: &str = "CC0-1.0";
pub const DATASET: &str = "biglam/hmd_newspapers";

pub struct Options<'a> {
    pub inputs: &'a [PathBuf],
    pub seed: &'a str,
    pub fraction: f64,
    pub revision: &'a str,
    pub lexicon: &'a Lexicon,
    pub gate: Gate,
    pub item_types: &'a HashSet<String>,
    pub min_ocr_confidence: f64,
}

/// Days since 1970-01-01 to `YYYY-MM-DD` (proleptic Gregorian).
pub fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

fn field_date(f: &Field) -> Option<String> {
    match f {
        Field::TimestampMillis(ms) => Some(civil_date(ms.div_euclid(86_400_000))),
        Field::TimestampMicros(us) => Some(civil_date(us.div_euclid(86_400_000_000))),
        Field::Long(s) => Some(civil_date(s.div_euclid(86_400))),
        Field::Date(d) => Some(civil_date(*d as i64)),
        Field::Str(s) => Some(s.get(..10).unwrap_or(s).to_owned()),
        _ => None,
    }
}

fn field_str(f: &Field) -> Option<String> {
    match f {
        Field::Str(s) => Some(s.clone()),
        _ => None,
    }
}

fn field_f64(f: &Field) -> Option<f64> {
    match f {
        Field::Double(x) => Some(*x),
        Field::Float(x) => Some(*x as f64),
        _ => None,
    }
}

pub fn paragraphs(t: &str, lex: &Lexicon, n: &mut Normalisation) -> Vec<Para> {
    let t = ocr::map_chars(t, n);
    let lines: Vec<String> = ocr::blocks(&t).iter().map(|b| ocr::unwrap_lines(b, lex, n)).collect();
    ocr::join_continuations(lines, lex, n).into_iter().map(|b| ocr::classify_block(b, Screen::XML)).collect()
}

pub fn family_key(source: &str, title: &str, date: &str, text: &str) -> String {
    let h = crate::text::sha_hex(format!("{source}\0{title}\0{date}\0{text}").as_bytes());
    format!("bl_hmd_article:{}", &h[..20])
}

pub fn adapt(opt: &Options, run: &mut Run) -> Result<Value> {
    let threshold = if opt.fraction >= 1.0 { u64::MAX } else { (opt.fraction.max(0.0) * u64::MAX as f64) as u64 };
    let method = format!("bl_hmd_article_text_blank_line_blocks;{};{}", ocr::NORMALISATION_METHOD, text::PARAGRAPH_RUN_METHOD);
    let mut per_file = Vec::new();
    let mut stats = QualityStats::default();
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut item_types: BTreeMap<String, u64> = BTreeMap::new();
    let mut confidence = ocr::Hist::default();
    let mut structure: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut by_decade: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut norm_total = Normalisation::default();
    let mut titles: BTreeMap<String, u64> = BTreeMap::new();
    let mut seen_units: HashSet<String> = HashSet::new();
    let mut duplicate_rows = 0u64;
    for path in opt.inputs {
        let (sha, bytes) = crate::io::file_sha(path)?;
        let reader = SerializedFileReader::new(std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?)?;
        let rows = reader.metadata().file_metadata().num_rows();
        let (mut seen, mut sampled) = (0u64, 0u64);
        for row in reader.get_row_iter(None)? {
            if run.full() {
                break;
            }
            let row = row?;
            seen += 1;
            let mut v: BTreeMap<&str, &Field> = BTreeMap::new();
            for (k, f) in row.get_column_iter() {
                v.insert(k.as_str(), f);
            }
            let get = |k: &str| v.get(k).and_then(|f| field_str(f)).unwrap_or_default();
            let text_s = get("text");
            let source = get("source");
            let title = get("title");
            let date = v.get("date").and_then(|f| field_date(f)).unwrap_or_default();
            let unit = family_key(&source, &title, &date, &text_s);
            if rank(opt.seed, "bl_hmd_article", &unit) >= threshold {
                continue;
            }
            if !seen_units.insert(unit.clone()) {
                duplicate_rows += 1;
                run.count("identical_row_repeated_in_dataset");
                continue;
            }
            sampled += 1;
            run.units_seen += 1;
            let item_type = get("item_type");
            *item_types.entry(item_type.clone()).or_default() += 1;
            let conf = v.get("ocr_quality_mean").and_then(|f| field_f64(f)).unwrap_or(-1.0);
            confidence.add(conf);
            *structure.entry(if text_s.contains("\n\n") { "text_with_blank_lines" } else if text_s.contains('\n') { "text_with_line_breaks_only" } else { "text_single_line" }).or_default() += 1;
            let tkey = text::underscore_key(&title);
            let group = format!("bl_hmd_title:{tkey}");
            let location = get("location");
            let meta = || {
                json!({"article_key": unit, "source": source, "newspaper": title, "location": location, "date": date, "item_type": item_type,
                       "ocr_quality_mean": conf, "text_sha256": text::sha_hex(text_s.as_bytes()),
                       "input_file": path.file_name().and_then(|n| n.to_str()), "input_sha256": sha})
            };
            if tkey.is_empty() {
                run.reject(&unit, "no_newspaper_title")?;
                continue;
            }
            if !run.sampled_meta_grouped(&group, &unit, meta)? {
                continue;
            }
            if !record::valid_date(&date) {
                run.reject(&unit, "no_issue_date")?;
                continue;
            }
            if !opt.item_types.contains(&item_type) {
                run.reject(&unit, "item_type_not_admitted")?;
                continue;
            }
            if conf < opt.min_ocr_confidence {
                run.reject(&unit, "dataset_ocr_confidence_below_gate")?;
                continue;
            }
            *titles.entry(tkey).or_default() += 1;
            let decade = format!("{}0s", &date[..3]);
            by_decade.entry(decade.clone()).or_default().0 += 1;
            let mut norm = Normalisation::default();
            let paras = paragraphs(&text_s, opt.lexicon, &mut norm);
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
            stats.add(&q);
            if let Some(r) = opt.gate.check(&q) {
                run.reject(&unit, r)?;
                continue;
            }
            norm_total.add(&norm);
            by_decade.entry(decade).or_default().1 += 1;
            let tail = &unit["bl_hmd_article:".len()..];
            let doc = Doc {
                id: format!("pool-blhmd-{tail}"),
                collection: COLLECTION,
                domain: "historical_newspapers_uk",
                source_group: unit.clone(),
                writer_key: Some(group.clone()),
                evidence: "historical_public_domain_proxy",
                excerpt: &ex,
                extraction_method: &method,
                unit: json!({"kind": "bl_hmd_article_text", "span_basis": "blank_line_block_index_half_open", "text_sha256": text::sha_hex(text_s.as_bytes()),
                             "input_file": path.file_name().and_then(|n| n.to_str()), "input_sha256": sha, "normalisation": norm.to_value(),
                             "ocr_quality": q.to_value(), "dataset_ocr_quality_mean": conf, "ocr_text": true,
                             "ocr_artefacts_remaining": q.odd_tokens > 0 || q.alpha_tokens > q.alpha_in_lexicon}),
                provenance: json!({"dataset": DATASET, "dataset_revision": opt.revision, "source": source, "newspaper": title, "location": location,
                                   "date": date, "item_type": item_type, "ocr_quality_mean": conf,
                                   "ocr_quality_sd": v.get("ocr_quality_sd").and_then(|f| field_f64(f)),
                                   "source_url": "https://bl.iro.bl.uk/collections/9a6a4cdd-2bfe-47bb-8c14-c0a5d100501f"}),
                date: Some(date.clone()),
                date_basis: "newspaper_issue_date",
                cutoff_policy: "dated_before_cutoff",
                rights: {
                    let mut r = record::rights(
                        LICENSE,
                        format!("Heritage Made Digital newspapers, British Library ({DATASET}, revision {}), CC0 1.0; {title}, {location}, {date}; OCR text, line breaks unwrapped", opt.revision),
                        "https://huggingface.co/datasets/biglam/hmd_newspapers",
                        "CC0 1.0 dataset of public-domain 19th-century newspapers digitised by the British Library.",
                    );
                    r["admission_decision"] = json!("historical_2026-10-27: British Library HMD newspapers (CC0) admitted Train-side only (Train and Train-mining), capped and OCR-quality gated; text in ignored storage, never redistributed");
                    r["redistribute_text"] = json!(false);
                    r["recommended_use"] = json!("train_only_capped");
                    r
                },
                aliases: vec![],
            };
            run.accept(&unit, &doc)?;
        }
        let d = json!({"role": "bl_hmd_parquet", "path": path, "sha256": sha, "bytes": bytes, "rows_in_file": rows, "rows_read": seen,
                       "rows_sampled": sampled, "sample_fraction": opt.fraction, "sample_threshold_u64": threshold, "dataset": DATASET, "dataset_revision": opt.revision});
        run.input(d.clone());
        per_file.push(d);
    }
    let by_decade: serde_json::Map<String, Value> = by_decade.into_iter().map(|(y, (n, k))| (y, json!({"screened": n, "admitted_candidates": k}))).collect();
    Ok(json!({"files": per_file, "paragraph_breaks": breaks, "item_types_sampled": item_types, "dataset_ocr_confidence_hist_sampled": confidence.to_value(),
              "text_structure_sampled": structure, "ocr_gate": opt.gate.to_value(), "min_dataset_ocr_confidence": opt.min_ocr_confidence,
              "admitted_item_types": opt.item_types.iter().collect::<std::collections::BTreeSet<_>>(),
              "ocr_quality_screened": stats.to_value(), "normalisation_totals_admitted": norm_total.to_value(),
              "lexicon_sha256": opt.lexicon.sha256, "lexicon_words": opt.lexicon.len(), "yield_by_decade": by_decade,
              "screened_titles": titles.len(), "identical_rows_repeated_in_dataset": duplicate_rows,
              "grouping": "unit and source family: article row; bucket, split group and writer_key: newspaper title",
              "limits": ["OCR misreadings remain in admitted text; their rate is recorded per document and must be matched or capped in training",
                         "the dataset selected poorly preserved newspapers for digitisation, so OCR is worse than average",
                         "no byline or writer separation beyond the newspaper title"]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dates_and_keys() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(-25_567), "1900-01-01");
        assert_eq!(civil_date(-62_091), "1800-01-01");
        assert_eq!(field_date(&Field::TimestampMillis(-62_091 * 86_400_000)).as_deref(), Some("1800-01-01"));
        let k = family_key("s", "The Times", "1850-01-02", "text");
        assert!(k.starts_with("bl_hmd_article:") && k.len() == "bl_hmd_article:".len() + 20);
        assert_ne!(k, family_key("s", "The Times", "1850-01-03", "text"));
    }
}

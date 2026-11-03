//! FineWeb (HuggingFaceFW/fineweb) parquet adapter: a seeded row sample of
//! pre-cutoff Common Crawl snapshots, rendered into whole prose paragraphs.
//!
//! * **Sample.** In each input file a row is kept iff
//!   `rank(seed, "fineweb_row", id) < 2^64 * min(1, target / rows_in_file)`;
//!   selection uses the row identifier only.
//! * **Gates (metadata).** Crawl capture time before 2022-11-30, FineWeb's
//!   own `language == "en"` with `language_score >= min_language_score`, an
//!   http(s) URL with a registered domain (Public Suffix List, see `psl`)
//!   that is not on the excluded list (sites of the other pool sources).
//! * **Groups.** Unit and source family: one page (`fineweb_page:<uuid>`).
//!   Bucket and split group: the registered domain (`fineweb_site:<domain>`),
//!   so one site never spans splits; it is also the `writer_key` used for
//!   Train caps. Same-writer pages on different sites are not linked.
//! * **Paragraphs.** FineWeb text lines (trafilatura blocks) are paragraphs.
//!   Breaks: the shared prose screen (`Screen::MARKUP`: sentence end, URL,
//!   markup, non-ASCII letters, quoted spans of 8+ words), list markers,
//!   boilerplate terms, and a site template screen: a paragraph whose
//!   whitespace-normalised hash occurs on two or more rendered pages of the
//!   same registered domain is a break. Then the shared paragraph run
//!   (first run reaching 220 words, 150..450) and the English screen.
//!   Cross-site templates are screened at dedup (`--near-collection`).
use crate::{
    adapter::Run,
    psl::{self, Psl},
    record::{self, Doc},
    splits::rank,
    text::{self, Para},
};
use anyhow::{Context, Result};
use parquet::{
    file::reader::{FileReader, SerializedFileReader},
    record::Field,
    schema::types::Type,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

pub const COLLECTION: &str = "fineweb_cc_historical";
pub const METHOD: &str = "fineweb_text_lines_as_paragraphs;boilerplate_list_and_site_template_breaks_v1";

// The line classifier and its screens live in `grammar_a_form::screen`, the one
// source the form path shares; they are re-exported here under their existing paths.
pub use grammar_a_form::screen::{BOILERPLATE, boilerplate, classify, list_like};

pub(crate) const COLUMNS: &[&str] = &["text", "id", "dump", "url", "date", "file_path", "language", "language_score", "token_count"];

pub struct Options<'a> {
    pub inputs: &'a [PathBuf],
    pub psl: &'a Psl,
    pub seed: &'a str,
    pub target_rows_per_file: u64,
    pub min_language_score: f64,
    pub excluded_domains: &'a HashSet<String>,
    pub dataset_revision: &'a str,
}

struct Page {
    id: String,
    uuid: String,
    url: String,
    dump: String,
    date: String,
    file_path: String,
    language_score: f64,
    token_count: i64,
    site: String,
    text: String,
    input: usize,
}

pub(crate) fn field_str(f: &Field) -> Option<String> {
    match f {
        Field::Str(s) => Some(s.clone()),
        _ => None,
    }
}

pub(crate) fn field_f64(f: &Field) -> Option<f64> {
    match f {
        Field::Double(x) => Some(*x),
        Field::Float(x) => Some(*x as f64),
        _ => None,
    }
}

pub(crate) fn field_i64(f: &Field) -> Option<i64> {
    match f {
        Field::Long(x) => Some(*x),
        Field::Int(x) => Some(*x as i64),
        _ => None,
    }
}

pub(crate) fn lines(t: &str) -> impl Iterator<Item = &str> {
    t.split('\n').map(str::trim).filter(|l| !l.is_empty())
}

pub fn adapt(opt: &Options, run: &mut Run) -> Result<Value> {
    let mut pages: Vec<Page> = Vec::new();
    let mut per_file = Vec::new();
    for (fi, path) in opt.inputs.iter().enumerate() {
        let (sha, bytes) = crate::io::file_sha(path)?;
        let reader = SerializedFileReader::new(std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?)?;
        let fm = reader.metadata().file_metadata();
        let rows = fm.num_rows().max(0) as u64;
        let schema = fm.schema();
        let fields: Vec<Arc<Type>> = schema.get_fields().iter().filter(|f| COLUMNS.contains(&f.name())).cloned().collect();
        anyhow::ensure!(fields.len() == COLUMNS.len(), "{}: missing FineWeb columns", path.display());
        let proj = Type::group_type_builder(schema.name()).with_fields(fields).build()?;
        let frac = if rows == 0 { 0.0 } else { (opt.target_rows_per_file as f64 / rows as f64).min(1.0) };
        let threshold = if frac >= 1.0 { u64::MAX } else { (frac * u64::MAX as f64) as u64 };
        let (mut seen, mut sampled) = (0u64, 0u64);
        let mut gate: BTreeMap<&'static str, u64> = BTreeMap::new();
        for row in reader.get_row_iter(Some(proj))? {
            let row = row?;
            seen += 1;
            let mut v: HashMap<&str, &Field> = HashMap::new();
            for (k, f) in row.get_column_iter() {
                v.insert(k.as_str(), f);
            }
            let Some(id) = v.get("id").and_then(|f| field_str(f)) else {
                *gate.entry("row_without_id").or_default() += 1;
                continue;
            };
            if rank(opt.seed, "fineweb_row", &id) >= threshold {
                continue;
            }
            sampled += 1;
            run.units_seen += 1;
            let uuid = id.trim_start_matches("<urn:uuid:").trim_end_matches('>').to_owned();
            let unit = format!("fineweb_page:{uuid}");
            let url = v.get("url").and_then(|f| field_str(f)).unwrap_or_default();
            let date = v.get("date").and_then(|f| field_str(f)).unwrap_or_default();
            let lang = v.get("language").and_then(|f| field_str(f)).unwrap_or_default();
            let score = v.get("language_score").and_then(|f| field_f64(f)).unwrap_or(0.0);
            let Some(host) = psl::url_host(&url).filter(|_| url.starts_with("http://") || url.starts_with("https://")) else {
                run.reject(&unit, "no_http_url_host")?;
                continue;
            };
            let Some(domain) = opt.psl.registered_domain(&host) else {
                run.reject(&unit, "no_registered_domain")?;
                continue;
            };
            let site = format!("fineweb_site:{domain}");
            let excluded = opt.excluded_domains.contains(&domain);
            let dump = v.get("dump").and_then(|f| field_str(f)).unwrap_or_default();
            let token_count = v.get("token_count").and_then(|f| field_i64(f)).unwrap_or(-1);
            let file_path = v.get("file_path").and_then(|f| field_str(f)).unwrap_or_default();
            let page_text = v.get("text").and_then(|f| field_str(f));
            let meta = || {
                json!({"fineweb_id": id, "url": url, "host": host, "registered_domain": domain, "dump": dump, "date": date,
                       "language": lang, "language_score": score, "token_count": token_count, "warc_file_path": file_path,
                       "input_file": path.file_name().and_then(|n| n.to_str()), "input_sha256": sha,
                       "page_text_sha256": page_text.as_deref().map(|t| text::sha_hex(t.as_bytes())),
                       "excluded_registered_domain": excluded,
                       "eligible_metadata": record::before_cutoff(&date) && lang == "en" && score >= opt.min_language_score && !excluded && page_text.is_some()})
            };
            if !run.sampled_meta_grouped(&site, &unit, meta)? {
                continue;
            }
            let reason = if !record::before_cutoff(&date) {
                Some("crawl_date_not_before_2022_11_30")
            } else if lang != "en" {
                Some("fineweb_language_not_en")
            } else if score < opt.min_language_score {
                Some("fineweb_language_score_below_minimum")
            } else if excluded {
                Some("registered_domain_of_another_pool_source")
            } else if page_text.is_none() {
                Some("no_text")
            } else {
                None
            };
            if let Some(r) = reason {
                run.reject(&unit, r)?;
                continue;
            }
            pages.push(Page { id, uuid, url, dump, date, file_path, language_score: score, token_count, site, text: page_text.unwrap_or_default(), input: fi });
        }
        let row_groups = reader.metadata().num_row_groups();
        let d = json!({"role": "fineweb_parquet", "path": path, "sha256": sha, "bytes": bytes, "rows_in_file": rows, "rows_read": seen,
                       "row_groups": row_groups, "sample_fraction": frac, "sample_threshold_u64": threshold, "rows_sampled": sampled,
                       "row_level_gates": gate, "dataset": "HuggingFaceFW/fineweb", "dataset_revision": opt.dataset_revision});
        run.input(d.clone());
        per_file.push(d);
    }

    // Site template screen over the pages rendered in this run.
    let mut site_para: HashMap<(u64, u64), u32> = HashMap::new();
    for p in &pages {
        let sk = text::h64(p.site.as_bytes());
        let mut mine: HashSet<u64> = HashSet::new();
        for l in lines(&p.text) {
            let n = text::normalized(l);
            if text::words(&n) >= 8 {
                mine.insert(text::h64(n.as_bytes()));
            }
        }
        for h in mine {
            *site_para.entry((sk, h)).or_default() += 1;
        }
    }
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let method = format!("{METHOD};{}", text::PARAGRAPH_RUN_METHOD);
    for p in &pages {
        if run.full() {
            break;
        }
        let unit = format!("fineweb_page:{}", p.uuid);
        let sk = text::h64(p.site.as_bytes());
        let paras: Vec<Para> = lines(&p.text)
            .map(|l| {
                let n = text::normalized(l);
                let rep = text::words(&n) >= 8 && site_para.get(&(sk, text::h64(n.as_bytes()))).copied().unwrap_or(0) >= 2;
                classify(l, rep)
            })
            .collect();
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
        let domain = p.site.trim_start_matches("fineweb_site:");
        let rights = json!({
            "license": record::FINEWEB_LICENSE,
            "license_url": "https://opendatacommons.org/licenses/by/1-0/",
            "additional_terms_url": "https://commoncrawl.org/terms-of-use",
            "attribution": format!("FineWeb (HuggingFaceFW/fineweb, revision {}), ODC-By 1.0; Common Crawl {}; page {}", opt.dataset_revision, p.dump, p.url),
            "evidence_url": "https://huggingface.co/datasets/HuggingFaceFW/fineweb",
            "commercial_training": true,
            "model_release": true,
            "external_evaluation": true,
            "redistribute_text": false,
            "admission_decision": "user_2026-10-27: FineWeb admitted for Train, Train-mining and as an evaluation domain; crawl snapshots before 2022-11-30; grouped by registered domain; text kept in ignored storage and never redistributed",
            "notes": "The database licence covers the FineWeb compilation; rights in each page's text remain with its owner and are not granted by the licence.",
        });
        let doc = Doc {
            id: format!("pool-fineweb-{}", p.uuid),
            collection: COLLECTION,
            domain: "web",
            source_group: unit.clone(),
            writer_key: Some(p.site.clone()),
            evidence: "historical_proxy",
            excerpt: &ex,
            extraction_method: &method,
            unit: json!({"kind": "fineweb_page_text_lines", "span_basis": "nonempty_text_line_index_half_open", "fineweb_id": p.id,
                         "page_text_sha256": text::sha_hex(p.text.as_bytes()), "input_file": opt.inputs[p.input].file_name().and_then(|n| n.to_str())}),
            provenance: json!({"source_url": p.url, "registered_domain": domain, "site_key": p.site, "fineweb_id": p.id, "dump": p.dump,
                               "crawl_date": p.date, "warc_file_path": p.file_path, "language_score": p.language_score, "token_count": p.token_count,
                               "dataset": "HuggingFaceFW/fineweb", "dataset_revision": opt.dataset_revision}),
            date: Some(p.date.clone()),
            date_basis: "common_crawl_capture_time_upper_bound_on_composition",
            cutoff_policy: "dated_before_cutoff",
            rights,
            aliases: vec![],
        };
        run.accept(&unit, &doc)?;
    }
    Ok(json!({"files": per_file, "pages_rendered": pages.len(), "paragraph_breaks": breaks,
              "site_template_rule": "a paragraph (8+ words) whose whitespace-normalised hash occurs on 2+ rendered pages of one registered domain is a break",
              "boilerplate_terms": BOILERPLATE, "min_language_score": opt.min_language_score,
              "excluded_registered_domains": opt.excluded_domains.iter().collect::<std::collections::BTreeSet<_>>(),
              "psl_rules": opt.psl.rule_count,
              "grouping": "unit and source family: page; bucket, split group and writer_key: registered domain"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn line_screens() {
        assert!(list_like("- an item that ends with a period."));
        assert!(list_like("12. Twelfth item in the list goes here."));
        assert!(!list_like("In 2012. the year it all changed, we moved."));
        assert!(boilerplate("Please read our Privacy Policy before you continue with this site."));
        assert!(boilerplate("Write to me at someone@example.com if you have any questions."));
        let p = classify("This is an ordinary sentence of running prose written by the site owner.", false);
        assert!(p.prose);
        let r = classify("This is an ordinary sentence of running prose written by the site owner.", true);
        assert_eq!(r.reason, Some("repeated_on_site_template"));
    }
}

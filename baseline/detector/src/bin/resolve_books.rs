//! Match a frozen CMU summary sample to historical Wikipedia revisions.
#[path = "support/cmu_history.rs"]
mod cmu_history;
use anyhow::{Context, Result, ensure};
use clap::Parser;
use reqwest::Url;
use serde_json::{Value, json};
use slop_ninja_detector::{
    acquire::{Capture, Collector},
    dataset::{self, sha256},
    wikipedia::{lead_text, literal_text, normalize},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::PathBuf,
};

const INPUT_HASH: &str = "94516c64d1b4ac6b8b397ef106ee598b43d0e5364ffd8305edc945dce04f4305";
const CUTOFFS: [&str; 2] = ["2012-11-02T00:00:00Z", "2013-06-01T00:00:00Z"];

#[derive(Parser)]
struct Args {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
    #[arg(long, default_value_t = 100)]
    limit: usize,
    #[arg(long, default_value_t = 0)]
    offset: usize,
    #[arg(long)]
    exclude_corpus: Vec<PathBuf>,
    /// Separate fixed review for complete 650-1000-word summaries.
    #[arg(long, requires = "historical_ledger")]
    long_form: bool,
    /// Use the separate v2 review of complete 1200-1600-word summaries.
    #[arg(long, requires = "long_form")]
    extended: bool,
    #[arg(long, requires = "long_form")]
    historical_ledger: Option<PathBuf>,
}

struct Book<'a> {
    page_id: u64,
    title: &'a str,
    text: &'a str,
    row_hash: String,
}

fn books(input: &str) -> Result<Vec<Book<'_>>> {
    let mut seen = BTreeSet::new();
    input
        .lines()
        .map(|line| {
            let f: Vec<_> = line.splitn(7, '\t').collect();
            ensure!(f.len() == 7, "Expected seven CMU columns");
            let page_id = f[0].parse::<u64>()?;
            ensure!(
                page_id > 0 && seen.insert(page_id),
                "Invalid or duplicate page ID"
            );
            Ok(Book {
                page_id,
                title: f[2],
                text: f[6],
                row_hash: sha256(line),
            })
        })
        .collect()
}

fn request(c: &mut Collector, pairs: &[(&str, &str)]) -> Result<Capture> {
    let mut url = Url::parse("https://en.wikipedia.org/w/api.php")?;
    url.query_pairs_mut()
        .extend_pairs([("format", "json"), ("formatversion", "2"), ("maxlag", "5")])
        .extend_pairs(pairs.iter().copied());
    c.get(&url)
}

fn revision(c: &mut Collector, book: &Book<'_>, cutoff: &str) -> Result<Value> {
    let raw = request(
        c,
        &[
            ("action", "query"),
            ("pageids", &book.page_id.to_string()),
            ("prop", "revisions"),
            ("rvprop", "ids|timestamp|content"),
            ("rvslots", "main"),
            ("rvstart", cutoff),
            ("rvlimit", "1"),
        ],
    )?;
    let query: Value = serde_json::from_slice(&raw.bytes)?;
    let page = &query["query"]["pages"][0];
    ensure!(
        page["pageid"] == book.page_id,
        "Requested page identity differs"
    );
    let rev = &page["revisions"][0];
    let id = rev["revid"].as_u64().context("No historical revision")?;
    let stamp = rev["timestamp"].as_str().context("Missing revision date")?;
    ensure!(stamp <= cutoff, "Revision exceeds historical cutoff");
    let wiki = rev["slots"]["main"]["content"]
        .as_str()
        .context("Missing wikitext")?;
    let rendered = request(
        c,
        &[
            ("action", "parse"),
            ("oldid", &id.to_string()),
            ("prop", "text"),
            ("disableeditsection", "1"),
        ],
    )?;
    let parsed: Value = serde_json::from_slice(&rendered.bytes)?;
    ensure!(parsed["parse"]["revid"] == id, "Rendered revision differs");
    let html = parsed["parse"]["text"]
        .as_str()
        .context("Missing rendered article")?;
    let needle = normalize(book.text);
    ensure!(!needle.is_empty(), "Empty summary");
    let literal_match = literal_text(wiki).contains(&needle);
    // The same paragraph extractor can inspect a full article, including plot
    // paragraphs, when the parse request omits the lead-only section parameter.
    let rendered_match = lead_text(html).contains(&needle);
    let lowered = wiki.to_lowercase();
    let notices: Vec<_> = [
        "copyvio",
        "copyright",
        "fair use",
        "fairuse",
        "cc-notice",
        "cc-by",
        "attribution",
        "copied",
        "public domain",
        "pd-notice",
        "eb1911",
        "gfdl",
        "plagiar",
        "permission=",
    ]
    .into_iter()
    .filter(|term| lowered.contains(term))
    .collect();
    Ok(json!({"status":"resolved_for_review", "cutoff":cutoff,
        "page_id":book.page_id, "title":page["title"], "revision_id":id,
        "revision_timestamp":stamp, "revision_url":format!("https://en.wikipedia.org/w/index.php?oldid={id}"),
        "history_url":format!("https://en.wikipedia.org/w/index.php?curid={}&action=history",book.page_id),
        "literal_match":literal_match,"rendered_match":rendered_match,
        "historical_match":literal_match && rendered_match,
        "rights_notice_terms":notices,"revision_capture":raw.metadata,
        "rendered_capture":rendered.metadata}))
}

fn main() -> Result<()> {
    let a = Args::parse();
    let long = cmu_history::long_review(a.extended);
    ensure!(
        !a.long_form || !a.exclude_corpus.is_empty(),
        "Long review requires post-ledger corpus exclusions"
    );
    ensure!(
        !a.long_form || (a.offset == 0 && a.limit == 100),
        "Long review requires the fixed first 100 sources"
    );
    ensure!(
        (1..=100).contains(&a.limit),
        "Review at most100 sources per batch"
    );
    ensure!(!a.output_dir.exists(), "Use a fresh review directory");
    let bytes = fs::read(&a.input)?;
    ensure!(
        sha256(&bytes) == INPUT_HASH,
        "Expected pinned CMU archive member"
    );
    let mut rows = books(std::str::from_utf8(&bytes)?)?;
    let mut excluded = BTreeSet::new();
    let mut excluded_texts = BTreeSet::new();
    let mut exclusions = Vec::new();
    let mut excluded_exact = BTreeSet::new();
    if let Some(path) = &a.historical_ledger {
        let history = cmu_history::load(path)?;
        excluded.extend(history.groups);
        excluded_texts.extend(history.lexical);
        excluded_exact.extend(history.exact);
    }
    for path in &a.exclude_corpus {
        for record in dataset::read_records(path)? {
            excluded.insert(record.source_group);
            excluded_texts.insert(sha256(
                grammar_core::features::words(&record.text).join(" "),
            ));
        }
        exclusions.push(json!({"corpus_sha256":sha256(fs::read(path)?)}));
    }
    let total = rows.len();
    if a.long_form {
        ensure!(
            exclusions == vec![json!({"corpus_sha256":cmu_history::FRESH_SHA256})],
            "Long review requires the frozen V6 source exclusions"
        );
    }
    let word_range = if a.long_form {
        long.words[0]..=long.words[1]
    } else {
        80..=500
    };
    rows.retain(|b| {
        word_range.contains(&grammar_core::features::words(b.text).len())
            && !excluded.contains(&format!("wikipedia:page:{}", b.page_id))
            && !excluded_exact.contains(&sha256(b.text))
            && !excluded_texts.contains(&sha256(grammar_core::features::words(b.text).join(" ")))
    });
    let rank_domain = if a.long_form {
        long.domain
    } else {
        "slop-ninja-cmu-review-v1"
    };
    rows.sort_by_cached_key(|b| sha256(format!("{rank_domain}|{}", b.page_id)));
    ensure!(a.offset < rows.len(), "Offset exceeds eligible source pool");
    let selected = &rows[a.offset..rows.len().min(a.offset + a.limit)];
    let mut c = Collector::new(&a.output_dir)?;
    let mut plan = json!({"schema":"slop_ninja_cmu_lineage_review_v1","input_sha256":INPUT_HASH,
        "total_rows":total,"eligible_pool":rows.len(),"offset":a.offset,"limit":a.limit,
        "actual_rows":selected.len(),"cutoffs":CUTOFFS,"exclude_corpora":exclusions,
        "sample_order":"sha256(slop-ninja-cmu-review-v1|page_id) ascending after length/overlap filters",
        "matching":"Full normalized summary contained in both literal historical wikitext and rendered article paragraphs. Stop at the first matching cutoff; retain all attempted observations.",
        "admission":"Review only; no training records or detector predictions",
        "selected":selected.iter().map(|b|json!({"page_id":b.page_id,"row_sha256":b.row_hash,"text_sha256":sha256(b.text)})).collect::<Vec<_>>()});
    if a.long_form {
        plan["schema"] = json!(format!(
            "slop_ninja_cmu_long_lineage_review_v{}",
            long.version
        ));
        plan["word_range"] = json!(long.words);
        plan["policy_sha256"] = json!(sha256(long.policy));
        plan["historical_ledger_sha256"] = json!(cmu_history::HISTORY_SHA256);
        plan["sample_order"] = json!(format!(
            "sha256({}|page_id) ascending after fixed length/overlap filters",
            long.domain
        ));
    }
    fs::write(
        a.output_dir.join("plan.json"),
        serde_json::to_vec_pretty(&plan)?,
    )?;
    let mut output = fs::File::create_new(a.output_dir.join("results.jsonl"))?;
    let mut counts = BTreeMap::<String, usize>::new();
    for (index, book) in selected.iter().enumerate() {
        let mut attempts = Vec::new();
        for cutoff in CUTOFFS {
            let result = revision(&mut c, book, cutoff).unwrap_or_else(
                |err| json!({"cutoff":cutoff,"status":"unresolved","error":err.to_string()}),
            );
            let matched = result["historical_match"] == true;
            attempts.push(result);
            if matched {
                break;
            }
        }
        let matched = attempts.iter().find(|v| v["historical_match"] == true);
        let status = if matched.is_some() {
            "historical_match"
        } else {
            "no_historical_match"
        };
        *counts.entry(status.into()).or_default() += 1;
        if matched.is_some_and(|v| {
            v["rights_notice_terms"]
                .as_array()
                .is_some_and(|t| t.is_empty())
        }) {
            *counts
                .entry("matched_without_notice_terms".into())
                .or_default() += 1;
        }
        let result = json!({"page_id":book.page_id,"book_title":book.title,
            "row_sha256":book.row_hash,"text_sha256":sha256(book.text),
            "sample_index":a.offset+index,"status":status,"attempts":attempts});
        serde_json::to_writer(&mut output, &result)?;
        output.write_all(b"\n")?;
        output.sync_all()?;
        if (index + 1).is_multiple_of(10) {
            eprintln!("Reviewed {}/{}: {:?}", index + 1, selected.len(), counts);
        }
    }
    let report = json!({"schema":"slop_ninja_cmu_lineage_review_summary_v1",
        "status":"complete","counts":counts,"reviewed":selected.len(),
        "plan_sha256":sha256(fs::read(a.output_dir.join("plan.json"))?),
        "results_sha256":sha256(fs::read(a.output_dir.join("results.jsonl"))?),
        "executable_sha256":sha256(fs::read(std::env::current_exe()?)?),
        "training_records_exported":0,"pangram_calls":0});
    fs::write(
        a.output_dir.join("summary.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_identity_comes_from_page_id_and_summary_bytes_remain_exact() {
        let line =
            "17\t/m/book\tA synthetic title\tBook author\t1840\t{}\t A plot with a tab\tinside.";
        let rows = books(line).unwrap();
        assert_eq!(rows[0].page_id, 17);
        assert_eq!(rows[0].text, " A plot with a tab\tinside.");
        assert_eq!(rows[0].row_hash, sha256(line));
        assert!(books(&format!("{line}\n{line}")).is_err());
        assert!(books("17\ttoo few columns").is_err());
    }
}

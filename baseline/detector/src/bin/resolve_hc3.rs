//! Recover historical Wikipedia lineage for a frozen HC3 source-review sample.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use reqwest::Url;
use serde_json::{Value, json};
use slop_ninja_detector::{
    acquire::{Capture, Collector},
    dataset::sha256,
};
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};

const INPUT_HASH: &str = "4f78105c72e7edd3b619b3b44abac1e6f8bdb42211f22e43191d4dae889e1968";
const CUTOFF: &str = "2022-11-01T00:00:00Z";

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
}

use slop_ninja_detector::wikipedia::{lead_text, literal_text, normalize};

fn request(collector: &mut Collector, pairs: &[(&str, &str)]) -> Result<Capture> {
    let mut url = Url::parse("https://en.wikipedia.org/w/api.php")?;
    url.query_pairs_mut()
        .extend_pairs([("format", "json"), ("formatversion", "2"), ("maxlag", "5")])
        .extend_pairs(pairs.iter().copied());
    collector.get(&url)
}

fn resolve(collector: &mut Collector, row: &Value) -> Result<Value> {
    let question = row["question"].as_str().context("Missing question")?;
    let title = question
        .strip_prefix("Please explain what is \"")
        .and_then(|s| s.strip_suffix('"'))
        .context("Unrecognized question/title format")?;
    ensure!(!title.is_empty(), "Empty article title");
    let raw = request(
        collector,
        &[
            ("action", "query"),
            ("titles", title),
            ("redirects", "1"),
            ("prop", "revisions"),
            ("rvprop", "ids|timestamp|content"),
            ("rvslots", "main"),
            ("rvstart", CUTOFF),
            ("rvlimit", "1"),
        ],
    )?;
    let query: Value = serde_json::from_slice(&raw.bytes)?;
    let page = &query["query"]["pages"][0];
    let revision = &page["revisions"][0];
    let revid = revision["revid"]
        .as_u64()
        .context("No historical revision")?;
    let pageid = page["pageid"].as_u64().context("No page ID")?;
    let timestamp = revision["timestamp"]
        .as_str()
        .context("No revision timestamp")?;
    ensure!(timestamp <= CUTOFF, "Revision exceeds cutoff");
    let wikitext = revision["slots"]["main"]["content"]
        .as_str()
        .context("Missing revision text")?;
    let rendered = request(
        collector,
        &[
            ("action", "parse"),
            ("oldid", &revid.to_string()),
            ("prop", "text"),
            ("section", "0"),
            ("disableeditsection", "1"),
        ],
    )?;
    let parsed: Value = serde_json::from_slice(&rendered.bytes)?;
    ensure!(
        parsed["parse"]["revid"] == revid,
        "Rendered revision mismatch"
    );
    let html = parsed["parse"]["text"]
        .as_str()
        .context("Missing rendered lead")?;
    let lead = lead_text(html);
    let literal = literal_text(wikitext);
    let lowered = wikitext.to_lowercase();
    let notices: Vec<_> = [
        "copyvio",
        "copyright",
        "fair use",
        "fairuse",
        "cc-notice",
        "cc-by",
        "attribution",
        "source-attribution",
        "copied",
        "public domain",
        "pd-notice",
        "eb1911",
    ]
    .into_iter()
    .filter(|term| lowered.contains(term))
    .collect();
    let answers: Vec<_> = row["human_answers"]
        .as_array()
        .context("Missing human answers")?
        .iter()
        .map(|answer| -> Result<_> {
            let text = answer.as_str().context("Invalid human answer")?;
            let normalized = normalize(text);
            ensure!(!normalized.is_empty(), "Empty answer");
            let rendered_match = lead.contains(&normalized);
            let literal_match = literal.contains(&normalized);
            Ok(
                json!({"text_sha256":sha256(text),"words":text.split_whitespace().count(),
            "rendered_match":rendered_match,"literal_match":literal_match,
            "historical_match":rendered_match && literal_match}),
            )
        })
        .collect::<Result<_>>()?;
    Ok(
        json!({"title":page["title"],"requested_title":title,"page_id":pageid,"revision_id":revid,
        "revision_timestamp":timestamp,"revision_url":format!("https://en.wikipedia.org/w/index.php?oldid={revid}"),
        "history_url":format!("https://en.wikipedia.org/w/index.php?curid={pageid}&action=history"),
        "revision_capture":raw.metadata,"rendered_capture":rendered.metadata,
        "answers":answers,"rights_notice_terms":notices,"status":"resolved_for_review"}),
    )
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        (1..=100).contains(&args.limit),
        "Review at most 100 rows per batch"
    );
    ensure!(
        !args.output_dir.join("results.jsonl").exists(),
        "Completed review exists"
    );
    let input = fs::read(&args.input)?;
    ensure!(
        sha256(&input) == INPUT_HASH,
        "Expected the pinned HC3 Wikipedia capture"
    );
    let mut rows: Vec<Value> = input
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(serde_json::from_slice)
        .collect::<std::result::Result<_, _>>()?;
    rows.sort_by_cached_key(|row| sha256(row["question"].as_str().unwrap_or("")));
    ensure!(args.offset < rows.len(), "Offset exceeds corpus");
    let rows = &rows[args.offset..rows.len().min(args.offset + args.limit)];
    let config = json!({"schema":"slop_ninja_hc3_lineage_review_v1","input_sha256":INPUT_HASH,
        "cutoff":CUTOFF,"sample_order":"sha256(question) ascending", "offset":args.offset,"limit":args.limit,
        "actual_rows":rows.len(),"historical_match":"normalized full answer contained in both rendered lead and literal historical wikitext after conservative markup removal",
        "admission":"review only; no origin records exported","executed_at":null});
    let mut collector = Collector::new(&args.output_dir)?;
    let config_path = args.output_dir.join("plan.json");
    if config_path.exists() {
        ensure!(
            serde_json::from_slice::<Value>(&fs::read(&config_path)?)? == config,
            "Plan changed"
        );
    } else {
        fs::write(config_path, serde_json::to_vec_pretty(&config)?)?;
    }
    let mut results = Vec::new();
    let mut counts = BTreeMap::<String, usize>::new();
    for (index, row) in rows.iter().enumerate() {
        let mut result = match resolve(&mut collector, row) {
            Ok(value) => value,
            Err(error) => json!({"status":"unresolved","error":error.to_string()}),
        };
        result["row_sha256"] = json!(sha256(serde_json::to_vec(row)?));
        result["question_sha256"] = json!(sha256(
            row["question"].as_str().context("Missing question")?
        ));
        result["sample_index"] = json!(args.offset + index);
        *counts
            .entry(result["status"].as_str().unwrap().into())
            .or_default() += 1;
        if let Some(answers) = result["answers"].as_array() {
            for answer in answers {
                let key = if answer["historical_match"] == true {
                    "matched_answers"
                } else {
                    "unmatched_answers"
                };
                *counts.entry(key.into()).or_default() += 1;
            }
        }
        results.push(result);
        if (index + 1).is_multiple_of(10) {
            eprintln!("Reviewed {} / {}: {:?}", index + 1, rows.len(), counts);
        }
    }
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(args.output_dir.join("results.jsonl"))?;
    for result in &results {
        serde_json::to_writer(&mut output, result)?;
        output.write_all(b"\n")?;
    }
    output.sync_all()?;
    let summary = json!({"schema":"slop_ninja_hc3_lineage_review_summary_v1","counts":counts,
        "input_sha256":INPUT_HASH,"plan_sha256":sha256(fs::read(args.output_dir.join("plan.json"))?),
        "results_sha256":sha256(fs::read(args.output_dir.join("results.jsonl"))?),"training_records_exported":0});
    fs::write(
        args.output_dir.join("summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn historical_match_cannot_be_supplied_by_a_current_template() {
        let wikitext = "'''Copper kites''' have [[glass wing|glass wings]].<ref>Some source</ref>\n{{current facts}}";
        let html = "<div class='mw-parser-output'><p><b>Copper kites</b> have glass wings.<sup>1</sup></p><p>New facts appeared today.</p></div>";
        assert!(literal_text(wikitext).contains("Copper kites have glass wings."));
        assert!(lead_text(html).contains("Copper kites have glass wings."));
        assert!(lead_text(html).contains("New facts appeared today."));
        assert!(!literal_text(wikitext).contains("New facts appeared today."));
        assert!(!literal_text(wikitext).contains("have metal wings"));
    }
}

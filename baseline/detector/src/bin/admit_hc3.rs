//! Admit source-verified HC3 human excerpts with explicit attribution obligations.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    acquire::Collector,
    dataset::{self, Evidence, Origin, OriginRecord, RECORD_SCHEMA, Rights, Source, sha256},
    rights::{self, ShareAlike},
    wikipedia::{captured_json, screen_revision},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const INPUT_HASH: &str = "4f78105c72e7edd3b619b3b44abac1e6f8bdb42211f22e43191d4dae889e1968";
const REVISION: &str = "4d0ff18143b5a7e1b1e79beb540c04549d1e59d3";
const SEED: &str = "slop-ninja-standard-hc3-v1";
const POLICY: &str = include_str!("../../SHARE_ALIKE_POLICY.md");

#[derive(Parser)]
struct Args {
    #[arg(long)]
    input: PathBuf,
    #[arg(long, required = true)]
    review_dir: Vec<PathBuf>,
    #[arg(long)]
    output_dir: PathBuf,
    /// Optional old corpus, used only to reject previously seen families/text.
    #[arg(long)]
    exclude_corpus: Vec<PathBuf>,
}

fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn str_at<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}
fn capture(root: &Path, meta: &Value) -> Result<Value> {
    captured_json(root, meta)
}

fn screen(review: &Value, raw: &Value, rendered: &Value, text: &str) -> Result<()> {
    screen_revision(review, raw, rendered, text, "2022-11-01T00:00:00Z")
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new admission directory");
    let input = fs::read(&args.input)?;
    ensure!(sha256(&input) == INPUT_HASH, "Expected pinned HC3 subset");
    let rows: BTreeMap<String, Value> = input
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|line| -> Result<_> {
            let row: Value = serde_json::from_slice(line)?;
            Ok((sha256(str_at(&row, "question")?), row))
        })
        .collect::<Result<_>>()?;
    ensure!(rows.len() == 842, "Expected unique 842-question subset");
    let mut excluded_groups = BTreeSet::new();
    let mut excluded_texts = BTreeSet::new();
    let mut exclusions = Vec::new();
    for path in &args.exclude_corpus {
        for record in dataset::read_records(path)? {
            excluded_groups.insert(record.source_group);
            excluded_texts.insert(sha256(
                grammar_core::features::words(&record.text).join(" "),
            ));
        }
        exclusions.push(json!({"path":path,"sha256":sha256(fs::read(path)?)}));
    }
    let mut collector = Collector::new(&args.output_dir)?;
    let mut rights_captures = Vec::new();
    for url in [
        "https://foundation.wikimedia.org/wiki/Policy:Terms_of_Use",
        "https://creativecommons.org/licenses/by-sa/3.0/legalcode.en",
        "https://creativecommons.org/licenses/by-sa/4.0/legalcode.en",
    ] {
        rights_captures.push(collector.get(&url.parse()?)?.metadata);
    }
    fs::write(args.output_dir.join("SHARE_ALIKE_POLICY.md"), POLICY)?;
    let mut review_inputs = Vec::new();
    let mut decisions = Vec::new();
    let mut records = Vec::new();
    let mut seen = BTreeSet::new();
    for root in &args.review_dir {
        let plan = read_json(&root.join("plan.json"))?;
        let summary = read_json(&root.join("summary.json"))?;
        let bytes = fs::read(root.join("results.jsonl"))?;
        ensure!(
            plan["input_sha256"] == INPUT_HASH
                && plan["cutoff"] == "2022-11-01T00:00:00Z"
                && summary["results_sha256"] == sha256(&bytes)
                && summary["plan_sha256"] == sha256(fs::read(root.join("plan.json"))?),
            "Review input binding mismatch"
        );
        review_inputs.push(json!({"path":root,"plan_sha256":summary["plan_sha256"],"results_sha256":summary["results_sha256"]}));
        for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            let review: Value = serde_json::from_slice(line)?;
            let question_hash = str_at(&review, "question_sha256")?;
            ensure!(
                seen.insert(question_hash.to_owned()),
                "Repeated source-review question"
            );
            let row = rows
                .get(question_hash)
                .context("Review question absent from pinned input")?;
            ensure!(
                review["row_sha256"] == sha256(serde_json::to_vec(row)?),
                "Review row mismatch"
            );
            if review["status"] != "resolved_for_review" {
                decisions.push(json!({"question_sha256":question_hash,"status":"excluded","reason":"unresolved historical source"}));
                continue;
            }
            let raw = capture(root, &review["revision_capture"])?;
            let rendered = capture(root, &review["rendered_capture"])?;
            for (index, answer) in row["human_answers"]
                .as_array()
                .context("Missing human answers")?
                .iter()
                .enumerate()
            {
                let text = answer.as_str().context("Non-text human answer")?;
                let text_hash = sha256(text);
                let group = format!(
                    "wikipedia:page:{}",
                    review["page_id"].as_u64().context("Page ID")?
                );
                let result = screen(&review, &raw, &rendered, text).and_then(|()| {
                    ensure!(
                        !excluded_groups.contains(&group)
                            && !excluded_texts
                                .contains(&sha256(grammar_core::features::words(text).join(" "))),
                        "Overlaps previously used corpus"
                    );
                    Ok(())
                });
                if let Err(error) = result {
                    decisions.push(json!({"question_sha256":question_hash,"text_sha256":text_hash,"status":"excluded","reason":error.to_string()}));
                    continue;
                }
                let decision = json!({"question_sha256":question_hash,"text_sha256":text_hash,"status":"admitted",
                    "historical_review":review,"policy_sha256":sha256(POLICY),"rights_captures":rights_captures,
                    "upstream_dataset_revision":REVISION,"source_input_sha256":INPUT_HASH});
                let decision_hash = sha256(serde_json::to_vec(&decision)?);
                let title = str_at(&review, "title")?;
                let revision_url = str_at(&review, "revision_url")?;
                let history_url = str_at(&review, "history_url")?;
                records.push(OriginRecord {
                    schema: RECORD_SCHEMA.into(), id: format!("hc3-wiki:{question_hash}:{index}"), source_group: group,
                    split: None, origin: Origin::HumanOnly, evidence: Evidence::HistoricalProxy,
                    evidence_notes: "HC3 human-answer bytes independently matched in full, modulo whitespace, to both a pre-2022-11-01 Wikipedia revision and its rendered lead. Historical publication is weak supervision; it does not document a human-only workflow or an individual writer.".into(),
                    text: text.into(), text_sha256: text_hash,
                    source: Source {
                        collection: "hc3-wiki-historical".into(), url: revision_url.into(),
                        version: format!("HC3@{REVISION};Wikipedia-oldid={}", review["revision_id"]),
                        published_at: Some(str_at(&review, "revision_timestamp")?.into()), author_ids: Vec::new(),
                        raw_path: root.canonicalize()?.join(str_at(&review["revision_capture"], "raw_path")?).to_string_lossy().into_owned(),
                        raw_sha256: str_at(&review["revision_capture"], "body_sha256")?.into(),
                        extraction: "HC3 human_answers exact UTF-8 bytes; slop_ninja_hc3_admission_v1 checks full historical containment".into(),
                    },
                    rights: Rights {
                        license: "CC-BY-SA-3.0".into(), evidence_url: rights_captures[0]["request_url"].as_str().unwrap().into(),
                        evidence_sha256: rights_captures[0]["body_sha256"].as_str().unwrap().into(),
                        attribution: format!("Wikipedia contributors, {title}, {revision_url}; contributor history: {history_url}. Extracted in HC3 by Biyang Guo et al., https://github.com/Hello-SimpleAI/chatgpt-comparison-detection. CC BY-SA 3.0; https://creativecommons.org/licenses/by-sa/3.0/ ."),
                        commercial_training: true, model_release: true, external_evaluation: true, redistribute_text: true,
                        share_alike: Some(ShareAlike {
                            policy: rights::POLICY.into(), source_title: title.into(), article_url: revision_url.into(), history_url: history_url.into(),
                            source_license: "CC-BY-SA-3.0".into(), source_license_url: "https://creativecommons.org/licenses/by-sa/3.0/".into(),
                            review_sha256: decision_hash,
                            notices: vec!["Wikipedia contributors; Creative Commons Attribution-ShareAlike 3.0 Unported. The work is supplied without warranties; see the linked legal code. No additional imported-text notice was detected in the archived source screen; detections are excluded for separate review.".into()],
                            changes: "Article excerpt selected/formatted upstream by HC3; its human-answer text is preserved byte for byte. Markup and reference presentation differ from Wikipedia. No prose rewrite by Slop Ninja.".into(),
                        }),
                    }, parent_id: None, generation: None,
                });
                decisions.push(decision);
            }
        }
    }
    ensure!(!records.is_empty(), "No admitted sources");
    let split_report = dataset::assign_splits(&mut records, SEED)?;
    let corpus = args.output_dir.join("human.jsonl");
    dataset::write_records(&corpus, &records)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(args.output_dir.join("decisions.jsonl"))?;
    let mut reasons = BTreeMap::<String, usize>::new();
    for decision in &decisions {
        serde_json::to_writer(&mut file, decision)?;
        file.write_all(b"\n")?;
        *reasons
            .entry(decision["reason"].as_str().unwrap_or("admitted").into())
            .or_default() += 1;
    }
    file.sync_all()?;
    let report = json!({"schema":"slop_ninja_hc3_admission_summary_v1","input_sha256":INPUT_HASH,
        "policy_sha256":sha256(POLICY),"review_inputs":review_inputs,"exclude_corpora":exclusions,
        "questions_reviewed":seen.len(),"decisions":reasons,"corpus_sha256":sha256(fs::read(corpus)?),
        "importer_executable_sha256":sha256(fs::read(std::env::current_exe()?)?),
        "decisions_sha256":sha256(fs::read(args.output_dir.join("decisions.jsonl"))?),
        "dataset":dataset::summarize(&records),"split_report":split_report,
        "generator_calls":0,"pangram_calls":0,"upstream_chatgpt_answers_admitted":0,
        "qualification":"Historical weak labels; conservative rights/quotation screen, not a certification of every possible source issue. No individual-author labels."});
    fs::write(
        args.output_dir.join("summary.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_match_does_not_override_source_notices_or_revision_identity() {
        let text = "The violet kite stands beside the amber cabinet. ".repeat(10);
        let review = json!({"page_id":1,"revision_id":2});
        let mut raw = json!({"query":{"pages":[{"pageid":1,"revisions":[{"revid":2,"timestamp":"2020-01-01T00:00:00Z","slots":{"main":{"content":text}}}]}]}});
        let rendered = json!({"parse":{"revid":2,"text":format!("<div class='mw-parser-output'><p>{text}</p></div>")}});
        screen(&review, &raw, &rendered, &text).unwrap();
        raw["query"]["pages"][0]["revisions"][0]["slots"]["main"]["content"] = json!(format!(
            "{text} {{{{source-attribution|some imported source}}}}"
        ));
        assert!(
            screen(&review, &raw, &rendered, &text)
                .unwrap_err()
                .to_string()
                .contains("notice")
        );
        assert!(
            screen(
                &json!({"page_id":1,"revision_id":3}),
                &raw,
                &rendered,
                &text
            )
            .is_err()
        );
    }
}

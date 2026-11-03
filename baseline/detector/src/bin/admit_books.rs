//! Admit complete historical CMU summaries with both collection and source notices.
#[path = "support/cmu_history.rs"]
mod cmu_history;
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{self, Evidence, Origin, OriginRecord, RECORD_SCHEMA, Rights, Source, sha256},
    rights::{self, ShareAlike},
    wikipedia::{captured_bytes, captured_json, screen_revision_in_word_range},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const INPUT_HASH: &str = "94516c64d1b4ac6b8b397ef106ee598b43d0e5364ffd8305edc945dce04f4305";
const CUTOFFS: [&str; 2] = ["2012-11-02T00:00:00Z", "2013-06-01T00:00:00Z"];
const POLICY: &str = include_str!("../../CMU_SOURCE_REVIEW.md");

#[derive(Parser)]
struct Args {
    #[arg(long)]
    staging_dir: PathBuf,
    #[arg(long, required = true)]
    review_dir: Vec<PathBuf>,
    #[arg(long)]
    output_dir: PathBuf,
    #[arg(long)]
    exclude_corpus: Vec<PathBuf>,
    /// Admit the separate long-summary review without assigning experiment splits.
    #[arg(long, requires = "historical_ledger")]
    long_form: bool,
    /// Admit the separate v2 review of complete 1200-1600-word summaries.
    #[arg(long, requires = "long_form")]
    extended: bool,
    #[arg(long, requires = "long_form")]
    historical_ledger: Option<PathBuf>,
}
fn object(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}

fn main() -> Result<()> {
    let a = Args::parse();
    let long = cmu_history::long_review(a.extended);
    ensure!(
        !a.long_form || !a.exclude_corpus.is_empty(),
        "Long admission requires post-ledger corpus exclusions"
    );
    let policy = if a.long_form { long.policy } else { POLICY };
    ensure!(!a.output_dir.exists(), "Use a fresh admission directory");
    let input = fs::read(a.staging_dir.join("booksummaries.txt"))?;
    ensure!(
        sha256(&input) == INPUT_HASH,
        "Expected pinned CMU summary file"
    );
    let mut books = BTreeMap::new();
    for line in std::str::from_utf8(&input)?.lines() {
        let f: Vec<_> = line.splitn(7, '\t').collect();
        ensure!(f.len() == 7, "Unexpected CMU columns");
        let id = f[0].parse::<u64>()?;
        ensure!(
            id > 0 && books.insert(id, (line, f[6])).is_none(),
            "Invalid or duplicate CMU identity"
        );
    }
    let staging = object(&a.staging_dir.join("staging.json"))?;
    ensure!(
        staging["text_sha256"] == INPUT_HASH && staging["collection_license"] == "CC-BY-SA-3.0-US",
        "Staging contract differs"
    );
    let captures = staging["captures"]
        .as_array()
        .context("Missing source captures")?;
    let card = captures
        .iter()
        .find(|c| c["request_url"] == "https://www.cs.cmu.edu/~dbamman/booksummaries.html")
        .context("Missing CMU grant capture")?;
    let license = captures
        .iter()
        .find(|c| c["request_url"] == "https://creativecommons.org/licenses/by-sa/3.0/us/legalcode")
        .context("Missing US license capture")?;
    ensure!(
        sha256(captured_bytes(&a.staging_dir, card)?)
            == "d3d98a18df61b1189298d34ca849df4529249155f6435589d2f4b869d76734bd",
        "CMU grant changed; review separately"
    );
    ensure!(
        sha256(captured_bytes(&a.staging_dir, license)?)
            == "2693108e2c3e0183b2a5e2f33317fb828f1858474cbeef66552392dba2358108",
        "US license capture changed; review separately"
    );
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
        for r in dataset::read_records(path)? {
            excluded.insert(r.source_group);
            excluded_texts.insert(sha256(grammar_core::features::words(&r.text).join(" ")));
        }
        exclusions.push(json!({"corpus_sha256":sha256(fs::read(path)?)}));
    }
    fs::create_dir_all(&a.output_dir)?;
    let mut decisions = fs::File::create_new(a.output_dir.join("decisions.jsonl"))?;
    let mut records = Vec::new();
    let mut seen = BTreeSet::new();
    let mut reasons = BTreeMap::<String, usize>::new();
    let mut review_bindings = Vec::new();
    for root in &a.review_dir {
        let plan = object(&root.join("plan.json"))?;
        let summary = object(&root.join("summary.json"))?;
        let results = fs::read(root.join("results.jsonl"))?;
        let schema = if a.long_form {
            format!("slop_ninja_cmu_long_lineage_review_v{}", long.version)
        } else {
            "slop_ninja_cmu_lineage_review_v1".into()
        };
        if a.long_form {
            ensure!(
                exclusions == vec![json!({"corpus_sha256":cmu_history::FRESH_SHA256})]
                    && plan["exclude_corpora"] == json!(exclusions),
                "Long review/admission requires the frozen V6 source exclusions"
            );
            ensure!(
                plan["offset"] == 0 && plan["limit"] == 100,
                "Changed long-summary selection range"
            );
            ensure!(
                plan["word_range"] == json!(long.words)
                    && plan["policy_sha256"] == sha256(policy)
                    && plan["sample_order"]
                        == format!(
                            "sha256({}|page_id) ascending after fixed length/overlap filters",
                            long.domain
                        )
                    && plan["historical_ledger_sha256"] == cmu_history::HISTORY_SHA256,
                "Changed long-summary review policy"
            );
        }
        ensure!(
            plan["schema"] == schema
                && plan["input_sha256"] == INPUT_HASH
                && plan["cutoffs"] == json!(CUTOFFS)
                && summary["status"] == "complete"
                && summary["plan_sha256"] == sha256(fs::read(root.join("plan.json"))?)
                && summary["results_sha256"] == sha256(&results),
            "Review bindings differ or review is incomplete"
        );
        let selected: BTreeMap<u64, &Value> = plan["selected"]
            .as_array()
            .context("Missing frozen sample")?
            .iter()
            .map(|v| Ok((v["page_id"].as_u64().context("Invalid sample ID")?, v)))
            .collect::<Result<_>>()?;
        ensure!(
            selected.len() == plan["selected"].as_array().unwrap().len(),
            "Duplicate selected page ID"
        );
        let mut batch_seen = BTreeSet::new();
        review_bindings.push(json!({"plan_sha256":summary["plan_sha256"],"results_sha256":summary["results_sha256"]}));
        for line in results.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            let review: Value = serde_json::from_slice(line)?;
            let id = review["page_id"]
                .as_u64()
                .context("Missing reviewed page ID")?;
            ensure!(
                seen.insert(id) && batch_seen.insert(id),
                "Repeated reviewed source"
            );
            let frozen = selected.get(&id).context("Review not in frozen sample")?;
            let (original, text) = books.get(&id).context("Reviewed source not in CMU input")?;
            let text_hash = sha256(text);
            ensure!(
                review["row_sha256"] == sha256(original)
                    && review["text_sha256"] == text_hash
                    && frozen["row_sha256"] == review["row_sha256"]
                    && frozen["text_sha256"] == text_hash,
                "Reviewed row bytes differ"
            );
            let group = format!("wikipedia:page:{id}");
            let result: Result<&Value> = (|| {
                ensure!(
                    !excluded.contains(&group)
                        && !excluded_exact.contains(&text_hash)
                        && !excluded_texts
                            .contains(&sha256(grammar_core::features::words(text).join(" "))),
                    "Overlaps previously used corpus"
                );
                let matched = review["attempts"]
                    .as_array()
                    .context("Missing attempts")?
                    .iter()
                    .find(|v| v["historical_match"] == true)
                    .context("No full historical match")?;
                let cutoff = string(matched, "cutoff")?;
                ensure!(
                    CUTOFFS.contains(&cutoff) && matched["page_id"] == id,
                    "Historical identity/cutoff differs"
                );
                let raw = captured_json(root, &matched["revision_capture"])?;
                let rendered = captured_json(root, &matched["rendered_capture"])?;
                ensure!(
                    matched["title"] == raw["query"]["pages"][0]["title"]
                        && matched["revision_timestamp"]
                            == raw["query"]["pages"][0]["revisions"][0]["timestamp"],
                    "Review title or date differs from the captured source"
                );
                let words = if a.long_form {
                    long.words[0]..=long.words[1]
                } else {
                    80..=500
                };
                screen_revision_in_word_range(matched, &raw, &rendered, text, cutoff, words)?;
                Ok(matched)
            })();
            let mut decision = json!({"page_id":id,"text_sha256":text_hash,
                "review_sha256":sha256(serde_json::to_vec(&review)?),"policy_sha256":sha256(policy)});
            match result {
                Err(error) => {
                    let reason = error.to_string();
                    *reasons.entry(reason.clone()).or_default() += 1;
                    decision["status"] = json!("excluded");
                    decision["reason"] = json!(reason);
                }
                Ok(matched) => {
                    *reasons.entry("admitted".into()).or_default() += 1;
                    decision["status"] = json!("admitted");
                    decision["collection_grant_capture"] = card.clone();
                    decision["collection_license_capture"] = license.clone();
                    let title = string(matched, "title")?;
                    let url = string(matched, "revision_url")?;
                    let history = string(matched, "history_url")?;
                    let revision_id = matched["revision_id"]
                        .as_u64()
                        .context("Invalid revision ID")?;
                    ensure!(
                        url == format!("https://en.wikipedia.org/w/index.php?oldid={revision_id}")
                            && history
                                == format!(
                                    "https://en.wikipedia.org/w/index.php?curid={id}&action=history"
                                ),
                        "Attribution URLs differ from matched identities"
                    );
                    records.push(OriginRecord {
                        schema:RECORD_SCHEMA.into(),id:format!("cmu-books:{id}:{}",&text_hash[..16]),source_group:group,split:None,
                        origin:Origin::HumanOnly,evidence:Evidence::HistoricalProxy,
                        evidence_notes:"Complete CMU summary matched modulo whitespace to historical Wikipedia wikitext and rendered paragraphs. This is weak historical evidence, not a documented human-only workflow. Book author and book date do not identify or date the summary.".into(),
                        text:(*text).into(),text_sha256:text_hash,
                        source:Source {collection:"cmu-books-historical".into(),url:url.into(),version:format!("CMU-books@{INPUT_HASH};Wikipedia-oldid={revision_id}"),
                            published_at:Some(string(matched,"revision_timestamp")?.into()),author_ids:Vec::new(),
                            raw_path:root.canonicalize()?.join(string(&matched["revision_capture"],"raw_path")?).to_string_lossy().into_owned(),
                            raw_sha256:string(&matched["revision_capture"],"body_sha256")?.into(),
                            extraction:"CMU TSV summary field preserved byte for byte; full historical containment checked by slop_ninja_cmu_admission_v1".into()},
                        rights:Rights {license:"CC-BY-SA-3.0".into(),evidence_url:string(card,"request_url")?.into(),evidence_sha256:string(card,"body_sha256")?.into(),
                            attribution:format!("Wikipedia contributors, {title}, {url}; history {history}; original text CC BY-SA 3.0 Unported, https://creativecommons.org/licenses/by-sa/3.0/ . CMU Book Summaries collection by David Bamman and Noah Smith (2013), https://www.cs.cmu.edu/~dbamman/booksummaries.html, CC BY-SA 3.0 United States, https://creativecommons.org/licenses/by-sa/3.0/us/ ."),
                            commercial_training:true,model_release:true,external_evaluation:true,redistribute_text:true,
                            share_alike:Some(ShareAlike {policy:rights::POLICY.into(),source_title:title.into(),article_url:url.into(),history_url:history.into(),
                                source_license:"CC-BY-SA-3.0".into(),source_license_url:"https://creativecommons.org/licenses/by-sa/3.0/".into(),
                                review_sha256:sha256(serde_json::to_vec(&decision)?),
                                notices:vec!["Wikipedia contributors; original text licensed CC BY-SA 3.0 Unported. No additional imported-text notice detected by the conservative screen; detections excluded for separate review. No warranties.".into(),
                                    "CMU collection grant: CC BY-SA 3.0 United States, https://creativecommons.org/licenses/by-sa/3.0/us/legalcode . Preserve David Bamman and Noah Smith (2013) extraction/collection credit. This notice is additional to the original Wikipedia text license; it does not erase or replace it.".into()],
                                changes:"CMU selected and formatted the Wikipedia plot summary; this importer preserves its TSV summary bytes without rewriting. Book metadata is not summary authorship evidence.".into()})},
                        parent_id:None,generation:None});
                }
            }
            serde_json::to_writer(&mut decisions, &decision)?;
            decisions.write_all(b"\n")?;
            decisions.sync_all()?;
        }
        ensure!(
            batch_seen.len() == selected.len() && summary["reviewed"] == selected.len(),
            "Review coverage is incomplete"
        );
    }
    ensure!(
        !records.is_empty(),
        "No admitted summaries; decisions retained"
    );
    let splits = if a.long_form {
        None
    } else {
        Some(dataset::assign_splits(
            &mut records,
            "slop-ninja-cmu-narrative-v1",
        )?)
    };
    let corpus = a.output_dir.join("human.jsonl");
    dataset::write_records(&corpus, &records)?;
    let mut report = json!({"schema":"slop_ninja_cmu_admission_summary_v1","status":"complete",
        "input_sha256":INPUT_HASH,"policy_sha256":sha256(policy),"review_inputs":review_bindings,
        "exclude_corpora":exclusions,"decisions":reasons,"reviewed":seen.len(),
        "corpus_sha256":sha256(fs::read(corpus)?),"decisions_sha256":sha256(fs::read(a.output_dir.join("decisions.jsonl"))?),
        "executable_sha256":sha256(fs::read(std::env::current_exe()?)?),"dataset":dataset::summarize(&records),"split_report":splits,
        "generator_calls":0,"pangram_calls":0,"book_authors_used_as_summary_authors":false,
        "qualification":"Historical proxies, conservative notice/quotation screen; no strong provenance or detector qualification claim."});
    if a.long_form {
        report["schema"] = json!(format!(
            "slop_ninja_cmu_long_admission_summary_v{}",
            long.version
        ));
        report["word_range"] = json!(long.words);
        report["historical_ledger_sha256"] = json!(cmu_history::HISTORY_SHA256);
        report["experiment_splits_assigned"] = json!(false);
    }
    fs::write(
        a.output_dir.join("summary.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

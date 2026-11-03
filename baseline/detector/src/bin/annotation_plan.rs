//! Prepare exact-input Pangram bulk requests without making network calls.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{Evidence, OriginRecord, read_records, sha256};
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};

#[derive(Parser)]
struct Args {
    /// A previously frozen, rights-admitted corpus. No sampling happens here.
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
    /// Repeated observations are distinct requests, never substitutes.
    #[arg(long, default_value_t = 1)]
    repeats: usize,
    /// Start after existing observations when scheduling deliberate repeats.
    #[arg(long, default_value_t = 0)]
    repeat_start: usize,
}

fn plan(
    records: &[OriginRecord],
    repeats: usize,
    repeat_start: usize,
) -> Result<(Value, Vec<Value>)> {
    ensure!(
        (1..=10).contains(&repeats),
        "Use between one and ten repeats"
    );
    let mut unique = BTreeMap::<&str, (&str, Vec<Value>)>::new();
    let index = records.iter().map(|r| (r.id.as_str(), r)).collect();
    for record in records {
        record.validate()?;
        ensure!(
            record.rights.external_evaluation,
            "External evaluation not admitted: {}",
            record.id
        );
        ensure!(
            record.evidence != Evidence::SyntheticFixture,
            "Do not submit synthetic test fixtures"
        );
        ensure!(
            record.split.is_some(),
            "Freeze source-family splits before annotation"
        );
        let words = record.text.split_whitespace().count();
        ensure!(words >= 50, "{}: fewer than 50 words", record.id);
        ensure!(
            words.div_ceil(100) <= 1_000,
            "{}: exceeds a bulk job's unit limit",
            record.id
        );
        let entry = unique
            .entry(&record.text_sha256)
            .or_insert((&record.text, Vec::new()));
        ensure!(entry.0 == record.text, "Text hash collision");
        entry.1.push(json!({
            "record_id":record.id,"source_group":record.source_group,"split":record.split,
            "origin":record.origin,"evidence":record.evidence,"parent_id":record.parent_id,
            "generator_attribution":slop_ninja_detector::attribution::describe(record, &index)?
        }));
    }
    ensure!(!unique.is_empty(), "Empty corpus");
    let mut batches = Vec::new();
    let mut items = Vec::new();
    let mut units = 0;
    let mut total_units = 0;
    let mut total_words = 0;
    let mut members = Vec::new();
    // All repeats use the same stable text order. Each member retains every
    // source label, even when multiple records refer to identical input bytes.
    let repeat_end = repeat_start
        .checked_add(repeats)
        .context("Repeat index overflow")?;
    for repeat in repeat_start..repeat_end {
        for (hash, (text, provenance)) in &unique {
            let words = text.split_whitespace().count();
            let billable = words.div_ceil(100);
            if units + billable > 1_000 {
                batches.push(json!({"model":"pangram-4","items":items}));
                items = Vec::new();
                units = 0;
            }
            let id = format!("{hash}:{repeat}");
            members.push(json!({
                "request_id":id,"text_sha256":hash,"repeat_index":repeat,
                "batch_index":batches.len(),"item_index":items.len(),
                "words":words,"estimated_billable_units":billable,"records":provenance
            }));
            items.push(json!({"id":id,"text":text}));
            units += billable;
            total_units += billable;
            total_words += words;
        }
    }
    if !items.is_empty() {
        batches.push(json!({"model":"pangram-4","items":items}));
    }
    let manifest = json!({
        "schema":"slop_ninja_pangram_annotation_plan_v1",
        "model_selector":"pangram-4", "status":"prepared_not_submitted",
        "record_count":records.len(), "unique_texts":unique.len(),
        "repeats":repeats,"repeat_start":repeat_start,"request_items":members.len(),"request_batches":batches.len(),
        "estimated_submitted_words":total_words,"estimated_billable_units":total_units,
        "estimated_bulk_usd_cents":total_units * 4,
        "estimated_realtime_usd_cents":total_units * 5,
        "pricing_checked":"2026-09-11",
        "pricing_url":"https://www.pangram.com/pricing",
        "api_url":"https://docs.pangram.com/api-reference/bulk-api",
        "word_count_policy":"Rust split_whitespace; provider counting can differ; estimate only",
        "label_policy":"Origin/evidence are retained independently; provider fractions are annotations, not document-origin probabilities or authorship truth",
        "annotation_publication":"retain in ignored storage until a release includes them",
        "members":members
    });
    Ok((manifest, batches))
}

fn write_new(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new output directory");
    let input = fs::read(&args.input)?;
    let records = read_records(&args.input)?;
    let (mut manifest, batches) = plan(&records, args.repeats, args.repeat_start)?;
    ensure!(
        fs::read(&args.input)? == input,
        "Corpus changed while preparing"
    );
    let serialized: Vec<_> = batches
        .iter()
        .map(serde_json::to_vec)
        .collect::<std::result::Result<_, _>>()?;
    manifest["input_sha256"] = json!(sha256(&input));
    manifest["planner_executable_sha256"] = json!(sha256(fs::read(std::env::current_exe()?)?));
    manifest["request_files"] = json!(
        serialized
            .iter()
            .enumerate()
            .map(|(i, bytes)| json!({
                "path":format!("requests/batch-{i:04}.json"),"sha256":sha256(bytes)
            }))
            .collect::<Vec<_>>()
    );
    fs::create_dir_all(args.output_dir.join("requests"))?;
    write_new(&args.output_dir.join("source-records.jsonl"), &input)?;
    for (i, bytes) in serialized.iter().enumerate() {
        write_new(
            &args.output_dir.join(format!("requests/batch-{i:04}.json")),
            bytes,
        )?;
    }
    write_new(
        &args.output_dir.join("plan.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    manifest.as_object_mut().unwrap().remove("members");
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_detector::dataset::{Origin, RECORD_SCHEMA, Rights, Source, Split};

    fn record(id: &str, words: usize) -> OriginRecord {
        let text = std::iter::repeat_n(id, words).collect::<Vec<_>>().join(" ");
        OriginRecord {
            schema: RECORD_SCHEMA.into(),
            id: id.into(),
            source_group: id.into(),
            split: Some(Split::Development),
            origin: Origin::HumanOnly,
            evidence: Evidence::DocumentedHuman,
            evidence_notes: "Synthetic unit-test metadata, never submitted".into(),
            text_sha256: sha256(&text),
            text,
            source: Source {
                collection: "unit-test".into(),
                url: "https://example.org/text".into(),
                version: "1".into(),
                published_at: None,
                author_ids: vec!["test-author".into()],
                raw_path: "fixture".into(),
                raw_sha256: sha256("fixture"),
                extraction: "test".into(),
            },
            rights: Rights {
                license: "Explicit-Contributor-Grant".into(),
                evidence_url: "https://example.org/grant".into(),
                evidence_sha256: sha256("grant"),
                attribution: "Unit test".into(),
                commercial_training: true,
                model_release: true,
                external_evaluation: true,
                redistribute_text: false,
                share_alike: None,
            },
            parent_id: None,
            generation: None,
        }
    }

    #[test]
    fn batches_round_each_text_retain_duplicates_and_enforce_external_rights() {
        let small = record("small", 101);
        let large = record("large", 99_801);
        let mut duplicate = small.clone();
        duplicate.id = "same-bytes".into();
        let (manifest, batches) = plan(&[small.clone(), large, duplicate], 2, 1).unwrap();
        assert_eq!(manifest["members"][0]["repeat_index"], 1);
        assert_eq!(manifest["members"][3]["repeat_index"], 2);
        assert_eq!(manifest["record_count"], 3);
        assert_eq!(manifest["unique_texts"], 2);
        assert_eq!(manifest["request_items"], 4);
        assert_eq!(manifest["estimated_billable_units"], 2_002);
        assert_eq!(manifest["estimated_bulk_usd_cents"], 8_008);
        assert_eq!(batches.len(), 4);
        assert!(batches.iter().all(|b| {
            b["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| {
                    i["text"]
                        .as_str()
                        .unwrap()
                        .split_whitespace()
                        .count()
                        .div_ceil(100)
                })
                .sum::<usize>()
                <= 1_000
        }));
        assert_eq!(
            manifest["members"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["records"].as_array().unwrap().len() == 2)
                .count(),
            2
        );
        let mut denied = small;
        denied.rights.external_evaluation = false;
        assert!(plan(&[denied], 1, 0).is_err());
    }
}

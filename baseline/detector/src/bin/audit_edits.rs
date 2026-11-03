//! Measure source overlap in recorded Train outputs without scoring or filtering.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{self, OriginRecord, Split, sha256};
use std::{collections::BTreeMap, fs, path::PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    sources: PathBuf,
    #[arg(long, required = true)]
    generation_dir: Vec<PathBuf>,
    #[arg(long)]
    output: PathBuf,
}

// Two O(output_words) rows suffice for both ordered and contiguous overlap.
fn overlap(source: &[String], output: &[String]) -> (usize, usize) {
    let mut subsequence = vec![0; output.len() + 1];
    let mut runs = vec![0; output.len() + 1];
    let mut longest_run = 0;
    for word in source {
        let (mut diagonal, mut diagonal_run) = (0, 0);
        for (j, candidate) in output.iter().enumerate() {
            let (previous, previous_run) = (subsequence[j + 1], runs[j + 1]);
            if word == candidate {
                subsequence[j + 1] = diagonal + 1;
                runs[j + 1] = diagonal_run + 1;
                longest_run = longest_run.max(runs[j + 1]);
            } else {
                subsequence[j + 1] = subsequence[j].max(previous);
                runs[j + 1] = 0;
            }
            diagonal = previous;
            diagonal_run = previous_run;
        }
    }
    (subsequence[output.len()], longest_run)
}

fn whitespace_view(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn distribution(mut values: Vec<f64>) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    values.sort_by(f64::total_cmp);
    let percentile = |p: f64| values[((p * values.len() as f64).ceil() as usize).saturating_sub(1)];
    json!({"count":values.len(),"min":values[0],"max":values[values.len()-1],
        "mean":values.iter().sum::<f64>() / values.len() as f64,
        "nearest_rank_p50":percentile(0.5),"nearest_rank_p90":percentile(0.9)})
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output.exists(), "Use a new audit output");
    let source_bytes = fs::read(&args.sources)?;
    let source_hash = sha256(&source_bytes);
    let sources = dataset::read_records(&args.sources)?;
    let roots: BTreeMap<_, _> = sources.iter().map(|r| (r.id.as_str(), r)).collect();
    ensure!(roots.len() == sources.len(), "Duplicate source ID");
    let mut records = Vec::new();
    let mut cohorts = Vec::new();
    for dir in &args.generation_dir {
        let run_bytes = fs::read(dir.join("run.json"))?;
        let run: Value = serde_json::from_slice(&run_bytes)?;
        ensure!(
            run["input_sha256"] == source_hash,
            "Generation input differs from source corpus"
        );
        let mut paths = fs::read_dir(dir.join("calls"))?
            .map(|entry| entry.map(|e| e.path().join("record.json")))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.retain(|path| path.is_file());
        paths.sort();
        let mut inspected = 0;
        let mut excluded_heldout = 0;
        for path in &paths {
            let bytes = fs::read(path)?;
            let record: OriginRecord = serde_json::from_slice(&bytes)?;
            // Held-out outputs are only checked for their partition field.
            if record.split != Some(Split::Train) {
                excluded_heldout += 1;
                continue;
            }
            record.validate()?;
            let root = roots
                .get(
                    record
                        .parent_id
                        .as_deref()
                        .context("Missing source parent")?,
                )
                .context("Generation references an unknown source")?;
            ensure!(
                root.split == Some(Split::Train) && record.source_group == root.source_group,
                "Generation family or partition changed"
            );
            let generation = record
                .generation
                .as_ref()
                .context("Missing recorded generation")?;
            ensure!(
                run["model"]["revision"] == generation.model_revision,
                "Generator revision mismatch"
            );
            let source_words = grammar_core::features::words(&root.text);
            let output_words = grammar_core::features::words(&record.text);
            ensure!(
                !source_words.is_empty() && !output_words.is_empty(),
                "Empty word sequence"
            );
            let (lcs, longest_run) = overlap(&source_words, &output_words);
            records.push(json!({"id":record.id,"source_group":record.source_group,
                "record_sha256":sha256(bytes),"source_text_sha256":root.text_sha256,
                "output_text_sha256":record.text_sha256,"model_revision":generation.model_revision,
                "operation":generation.operation,"origin":record.origin,"split":record.split,
                "profile":generation.prompt_profile.as_ref().map(|p| p.profile.id.as_str()),
                "source_words":source_words.len(),"output_words":output_words.len(),
                "identical_raw":root.text == record.text,
                "identical_after_trim":root.text.trim() == record.text.trim(),
                "identical_after_whitespace_collapse":whitespace_view(&root.text) == whitespace_view(&record.text),
                "identical_normalized_words":source_words == output_words,
                "ordered_shared_words":lcs,"source_retention":lcs as f64 / source_words.len() as f64,
                "output_from_source":lcs as f64 / output_words.len() as f64,
                "longest_contiguous_shared_words":longest_run}));
            inspected += 1;
        }
        cohorts.push(json!({"directory":dir,"run_sha256":sha256(run_bytes),
            "snapshot_record_files":paths.len(),"train_records_inspected":inspected,
            "heldout_records_excluded":excluded_heldout,
            "terminal_summary_present":dir.join("summary.json").exists()}));
    }
    records.sort_by_cached_key(|r| r["id"].as_str().unwrap().to_owned());
    ensure!(
        records.windows(2).all(|p| p[0]["id"] != p[1]["id"]),
        "Repeated generation record"
    );
    let mut groups = BTreeMap::<String, Vec<&Value>>::new();
    for record in &records {
        let key = format!(
            "{}|{}",
            record["model_revision"].as_str().unwrap(),
            record["operation"].as_str().unwrap()
        );
        groups.entry(key).or_default().push(record);
    }
    let aggregates: BTreeMap<_, _> = groups.into_iter().map(|(key, rows)| {
        let mut result = json!({"records":rows.len(),
            "source_retention":distribution(rows.iter().map(|r| r["source_retention"].as_f64().unwrap()).collect()),
            "output_from_source":distribution(rows.iter().map(|r| r["output_from_source"].as_f64().unwrap()).collect()),
            "source_retention_below_half":rows.iter().filter(|r| r["source_retention"].as_f64().unwrap() < 0.5).count(),
            "source_retention_at_least_95_percent":rows.iter().filter(|r| r["source_retention"].as_f64().unwrap() >= 0.95).count()});
        for field in ["identical_raw", "identical_after_trim", "identical_after_whitespace_collapse", "identical_normalized_words"] {
            result[field] = json!(rows.iter().filter(|r| r[field] == true).count());
        }
        (key, result)
    }).collect();
    ensure!(
        sha256(fs::read(args.sources)?) == source_hash,
        "Source file changed during audit"
    );
    let report = json!({"schema":"slop_ninja_recorded_edit_audit_v1",
        "source_corpus_sha256":source_hash,"cohorts":cohorts,"aggregates":aggregates,"records":records,
        "scope":"Snapshot of completed Train record files, including successful siblings of subsequently excluded families. Held-out outputs and unfinished calls are excluded; do not treat this as a final cohort or an admission decision.",
        "method":"Longest common subsequence and longest contiguous run of grammar_core lowercased Unicode word tokens. Whitespace equality uses Rust str::split_whitespace and ASCII-space joining. Fractions measure surface overlap, not meaning or editing quality.",
        "detector_calls":0,"labels_changed":false,"records_filtered":false,"factual_fidelity_certified":false});
    fs::write(&args.output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report["aggregates"])?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }
    #[test]
    fn overlap_preserves_order_and_distinguishes_runs_from_subsequences() {
        assert_eq!(overlap(&words("a b a"), &words("a a")), (2, 1));
        assert_eq!(overlap(&words("a b c d"), &words("b c x d")), (3, 2));
        assert_eq!(overlap(&words("a b c"), &words("c b a")), (1, 1));
        assert_eq!(overlap(&words("a b"), &words("x y")), (0, 0));
        assert_eq!(overlap(&words("a b"), &words("a b")), (2, 2));
    }
}

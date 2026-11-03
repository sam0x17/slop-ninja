//! Measure source reuse in a frozen set of admitted and rejected draft captures.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::features::words;
use serde_json::{Value, json};
use slop_ninja_detector::{dataset::sha256, generation::reject_source_copy};
use std::{collections::BTreeMap, fs, path::PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    input_sha256: String,
    #[arg(long)]
    output: PathBuf,
}

fn measure(source: &[String], draft: &[String]) -> Value {
    let mut runs = vec![0; draft.len() + 1];
    let (mut longest, mut source_start, mut draft_start) = (0, 0, 0);
    for (i, left) in source.iter().enumerate() {
        for j in (0..draft.len()).rev() {
            runs[j + 1] = if *left == draft[j] { runs[j] + 1 } else { 0 };
            if runs[j + 1] > longest {
                longest = runs[j + 1];
                source_start = i + 1 - longest;
                draft_start = j + 1 - longest;
            }
        }
    }
    let mut source_windows = BTreeMap::<String, Vec<usize>>::new();
    for (i, span) in source.windows(20).enumerate() {
        source_windows.entry(span.join(" ")).or_default().push(i);
    }
    let mut source_covered = vec![false; source.len()];
    let mut draft_covered = vec![false; draft.len()];
    for (j, span) in draft.windows(20).enumerate() {
        if let Some(starts) = source_windows.get(&span.join(" ")) {
            draft_covered[j..j + 20].fill(true);
            for &i in starts {
                source_covered[i..i + 20].fill(true);
            }
        }
    }
    let source_count = source_covered.iter().filter(|&&v| v).count();
    let draft_count = draft_covered.iter().filter(|&&v| v).count();
    json!({"source_words":source.len(),"draft_words":draft.len(),
        "longest_contiguous_shared_words":longest,
        "longest_source_word_range":[source_start,source_start+longest],
        "longest_draft_word_range":[draft_start,draft_start+longest],
        "longest_shared_normalized_text":source[source_start..source_start+longest].join(" "),
        "source_words_in_shared_runs_at_least_20":source_count,
        "draft_words_in_shared_runs_at_least_20":draft_count,
        "source_coverage_by_runs_at_least_20":source_count as f64/source.len() as f64,
        "draft_coverage_by_runs_at_least_20":draft_count as f64/draft.len() as f64})
}

fn string<'a>(row: &'a Value, key: &str) -> Result<&'a str> {
    row[key].as_str().with_context(|| format!("Missing {key}"))
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output.exists(), "Use a new audit output");
    let bytes = fs::read(&args.input)?;
    ensure!(sha256(&bytes) == args.input_sha256, "Input hash mismatch");
    let mut rows = Vec::new();
    for line in std::str::from_utf8(&bytes)?
        .lines()
        .filter(|v| !v.is_empty())
    {
        let mut row: Value = serde_json::from_str(line)?;
        ensure!(
            row["split"] == "train",
            "Only Train captures may be inspected"
        );
        for key in [
            "task",
            "source_group",
            "generator",
            "profile",
            "request_sha256",
            "raw_response_sha256",
        ] {
            ensure!(!string(&row, key)?.is_empty(), "Empty {key}");
        }
        let source = string(&row, "source")?;
        let draft = string(&row, "draft")?;
        ensure!(
            sha256(source) == string(&row, "source_sha256")?,
            "Source hash mismatch"
        );
        ensure!(
            sha256(draft) == string(&row, "draft_sha256")?,
            "Draft hash mismatch"
        );
        let (source_words, draft_words) = (words(source), words(draft));
        ensure!(
            !source_words.is_empty() && !draft_words.is_empty(),
            "Empty word sequence"
        );
        ensure!(
            source_words
                .len()
                .checked_mul(draft_words.len())
                .is_some_and(|v| v <= 20_000_000),
            "Pair exceeds this diagnostic's 20-million-cell bound; no truncation permitted"
        );
        let copy_error = reject_source_copy(source, draft)
            .err()
            .map(|v| v.to_string());
        let admitted = row["automatic_origin_admission"]
            .as_bool()
            .context("Missing admission status")?;
        if admitted {
            ensure!(
                copy_error.is_none() && row["admission_error"].is_null(),
                "Admitted capture fails source-copy replay"
            );
        } else if string(&row, "admission_error")?.starts_with("draft_source_copy:") {
            ensure!(
                copy_error.as_deref() == row["admission_error"].as_str(),
                "Source-copy rejection changed"
            );
        }
        let measures = measure(&source_words, &draft_words);
        let object = row.as_object_mut().context("Expected object")?;
        object.remove("source");
        object.remove("draft");
        row["measures"] = measures;
        row["replayed_source_copy_error"] = json!(copy_error);
        rows.push(row);
    }
    ensure!(!rows.is_empty(), "No captures supplied");
    rows.sort_by_cached_key(|r| r["task"].as_str().unwrap().to_owned());
    ensure!(
        rows.windows(2).all(|v| v[0]["task"] != v[1]["task"]),
        "Repeated capture"
    );
    ensure!(
        sha256(fs::read(&args.input)?) == args.input_sha256,
        "Input changed during audit"
    );
    let report = json!({"schema":"slop_ninja_draft_source_copy_audit_v1",
        "input_sha256":args.input_sha256,"rows":rows,
        "method":"Exact longest contiguous run and union coverage of all matching 20-word windows, using grammar_core lowercased Unicode words. Source-copy decisions replay the existing generation guard.",
        "limitations":"Pinned Train pairs include admitted and rejected captures. This tool verifies pair hashes and guard consistency; the capture preparation must separately bind raw requests, responses and model provenance. Surface reuse is neither semantic fidelity nor a verified fraction of human authorship. No admission, label or detector result changes.",
        "detector_calls":0,"admission_changed":false});
    fs::write(args.output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "Audited {} frozen captures",
        report["rows"].as_array().unwrap().len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_windows_count_each_word_once_and_keep_run_boundaries() {
        let source: Vec<String> = (0..30).map(|i| format!("word{i}")).collect();
        let mut draft = source[..21].to_vec();
        draft.push("separator".into());
        draft.extend_from_slice(&source[..21]);
        let result = measure(&source, &draft);
        assert_eq!(result["longest_contiguous_shared_words"], 21);
        assert_eq!(result["source_words_in_shared_runs_at_least_20"], 21);
        assert_eq!(result["draft_words_in_shared_runs_at_least_20"], 42);
        assert_eq!(
            measure(&source, &source[..19])["draft_words_in_shared_runs_at_least_20"],
            0
        );
        assert_eq!(
            measure(&words("a b a"), &words("a a"))["longest_contiguous_shared_words"],
            1
        );
    }
}

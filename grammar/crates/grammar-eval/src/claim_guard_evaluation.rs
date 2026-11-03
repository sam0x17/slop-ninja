//! Apply a separate preservation constraint before frozen author proximity.
use crate::{claim_guard, edit_utility_scoring::Scoring};
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_core::{
    edits::{self, Edit},
    syntax::Document,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(about = "Check source-aligned claims before ranking edits by author proximity")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compare two validated annotations and an exact list of source patches.
    Compare {
        #[arg(long)]
        source_annotation: PathBuf,
        #[arg(long)]
        expected_source_annotation_sha256: String,
        #[arg(long)]
        candidate_annotation: PathBuf,
        #[arg(long)]
        expected_candidate_annotation_sha256: String,
        #[arg(long)]
        edits: PathBuf,
        #[arg(long)]
        expected_edits_sha256: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Evaluate predeclared synthetic fixtures against unchanged author models.
    Evaluate {
        #[arg(long)]
        protocol: PathBuf,
        #[arg(long)]
        expected_protocol_sha256: String,
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn read_bound(path: &Path, digest: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("Read {}", path.display()))?;
    ensure!(hash(&bytes) == digest, "SHA256 differs: {}", path.display());
    Ok(bytes)
}
fn bound_json(path: &Path, digest: &str) -> Result<Value> {
    Ok(serde_json::from_slice(&read_bound(path, digest)?)?)
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("Missing string {key}"))
}
fn rows<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    v[key]
        .as_array()
        .with_context(|| format!("Missing array {key}"))
}
fn fresh_json(path: &Path, value: &Value) -> Result<String> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?
        .write_all(&bytes)?;
    Ok(hash(&bytes))
}

fn validate_anchor(text: &str, anchor: &Value) -> Result<()> {
    let start = anchor["start_byte"]
        .as_u64()
        .context("Missing anchor start")? as usize;
    let end = anchor["end_byte"].as_u64().context("Missing anchor end")? as usize;
    ensure!(
        start < end
            && end <= text.len()
            && text.is_char_boundary(start)
            && text.is_char_boundary(end),
        "Invalid UTF-8 anchor span"
    );
    ensure!(
        &text[start..end] == string(anchor, "expected")?,
        "Anchor text differs"
    );
    Ok(())
}

/// A closer target score cannot override any changed or unresolved guard.
/// Exact score ties retain the source and no missing family is filled with zero.
fn selection(outcome: &str, source: &Value, candidate: &Value) -> Result<Value> {
    ensure!(
        ["observed_preserved", "changed", "unresolved"].contains(&outcome),
        "Unknown guard outcome"
    );
    if source["status"] != "scored" || candidate["status"] != "scored" {
        return Ok(
            json!({"status":"unscorable", "selected":"source", "reason":"Required source or candidate features unavailable", "guard_outcome":outcome}),
        );
    }
    let mut candidate_scores = BTreeMap::new();
    for score in rows(candidate, "scores")? {
        let key = (
            score["seed"].as_u64().context("Missing score seed")?,
            string(score, "author")?,
        );
        ensure!(
            candidate_scores.insert(key, score).is_none(),
            "Duplicate candidate score"
        );
    }
    let mut seen = BTreeSet::new();
    let mut decisions = Vec::new();
    for before in rows(source, "scores")? {
        let key = (
            before["seed"].as_u64().context("Missing score seed")?,
            string(before, "author")?,
        );
        ensure!(seen.insert(key), "Duplicate source score");
        let after = candidate_scores
            .get(&key)
            .context("Missing candidate score pair")?;
        let source_logit = before["logit"].as_f64().context("Missing source logit")?;
        let candidate_logit = after["logit"].as_f64().context("Missing candidate logit")?;
        ensure!(
            source_logit.is_finite() && candidate_logit.is_finite(),
            "Nonfinite logit"
        );
        let delta = candidate_logit - source_logit;
        ensure!(delta.is_finite(), "Nonfinite score difference");
        let choose_candidate = outcome == "observed_preserved" && delta > 0.0;
        decisions.push(json!({"seed":key.0,"author":key.1,"source_logit":source_logit,"candidate_logit":candidate_logit,"candidate_minus_source":delta,
            "candidate_closer_without_guard":delta>0.0,"candidate_eligible":outcome=="observed_preserved",
            "selected":if choose_candidate {"candidate"} else {"source"},
            "reason":if outcome!="observed_preserved" {"changed_or_unresolved_guard"} else if delta>0.0 {"eligible_and_closer"} else {"no_strict_proximity_improvement"}}));
    }
    ensure!(
        seen.len() == candidate_scores.len() && !seen.is_empty(),
        "Source/candidate score coverage differs"
    );
    Ok(
        json!({"status":"scored","guard_outcome":outcome,"decisions":decisions,
        "semantic_equivalence_certified":false,"readability":"unjudged"}),
    )
}

fn evaluate(protocol_path: &Path, expected: &str, repo: &Path, out: &Path) -> Result<()> {
    let protocol = bound_json(protocol_path, expected)?;
    ensure!(
        protocol["schema"] == "slopninja-claim-guard-evaluation-protocol-v1",
        "Wrong evaluation protocol"
    );
    ensure!(!out.exists(), "Use a fresh output directory");
    let source_files = protocol["source_files"]
        .as_object()
        .context("Missing source bindings")?;
    ensure!(!source_files.is_empty(), "Empty source bindings");
    for (path, digest) in source_files {
        read_bound(&repo.join(path), digest.as_str().context("Source SHA256")?)?;
    }
    let fixture_binding = &protocol["fixture_manifest"];
    let fixture = bound_json(
        &repo.join(string(fixture_binding, "path")?),
        string(fixture_binding, "sha256")?,
    )?;
    ensure!(
        fixture["schema"] == "slopninja-claim-guard-fixtures-v1",
        "Wrong fixture schema"
    );
    let cases = rows(&fixture, "cases")?;
    ensure!(
        cases.len()
            == protocol["expected_cases"]
                .as_u64()
                .context("Missing expected case count")? as usize,
        "Case count differs"
    );
    let mut ids = BTreeSet::new();
    let mut parse_texts = BTreeSet::new();
    for case in cases {
        ensure!(ids.insert(string(case, "id")?), "Duplicate case ID");
        let source = string(case, "source_text")?;
        let candidate = string(case, "candidate_text")?;
        let patches: Vec<Edit> = serde_json::from_value(case["edits"].clone())?;
        ensure!(
            edits::apply(source, &patches)? == candidate,
            "Fixture candidate differs from exact patches"
        );
        ensure!(
            ["observed_preserved", "changed", "unresolved"]
                .contains(&string(case, "expected_outcome")?),
            "Unknown engineering expectation"
        );
        if let Some(local) = case["expected_local_operator_outcome"].as_str() {
            ensure!(
                ["observed_preserved", "changed", "unresolved"].contains(&local),
                "Unknown local operator expectation"
            );
        } else {
            ensure!(
                case["expected_local_operator_outcome"].is_null(),
                "Invalid local operator expectation"
            );
        }
        for protected in rows(case, "protected_expectations")? {
            for anchor in rows(protected, "source_anchors")? {
                validate_anchor(source, anchor)?;
            }
            for anchor in rows(protected, "candidate_anchors")? {
                validate_anchor(candidate, anchor)?;
            }
        }
        for text in [source, candidate] {
            parse_texts.insert(text.to_string());
            for context in rows(&protocol, "scoring_contexts")? {
                parse_texts.insert(format!(
                    "{}{}{}",
                    string(context, "prefix")?,
                    text,
                    string(context, "suffix")?
                ));
            }
        }
    }
    let scoring_binding = &protocol["scoring_protocol"];
    let scoring_protocol = bound_json(
        &repo.join(string(scoring_binding, "path")?),
        string(scoring_binding, "sha256")?,
    )?;
    let parser = string(&protocol, "parser_identity")?;
    ensure!(
        scoring_protocol["parser_identity"] == parser,
        "Scoring parser differs"
    );
    let scoring = Scoring::load(&scoring_protocol)?;
    fs::create_dir_all(out)?;
    fresh_json(&out.join("protocol.json"), &protocol)?;
    fresh_json(&out.join("target-audit.json"), &scoring.target_audit())?;
    let input: Vec<_> = parse_texts.iter().map(String::as_str).collect();
    let parsed = grammar_spacy::parse_batch(&input, string(&protocol, "parser_python")?)?;
    ensure!(
        parsed.len() == input.len(),
        "Parser result coverage differs"
    );
    let mut annotations = BTreeMap::new();
    let mut failures = BTreeMap::new();
    fs::create_dir(out.join("annotations"))?;
    let mut annotation_bindings = Vec::new();
    for (text, annotation) in input.into_iter().zip(parsed) {
        let text_sha = hash(text.as_bytes());
        let value = match annotation {
            Ok(doc) => {
                ensure!(
                    doc.text == text && doc.parser_identity == parser,
                    "Parser source/identity differs"
                );
                grammar_core::syntax::validate(&doc)?;
                let value = json!({"text_sha256":text_sha,"document":doc});
                annotations.insert(text.to_string(), doc);
                value
            }
            Err(error) => {
                failures.insert(text.to_string(), error.clone());
                json!({"text_sha256":text_sha,"error":error})
            }
        };
        let filename = format!("annotations/{text_sha}.json");
        let digest = fresh_json(&out.join(&filename), &value)?;
        annotation_bindings.push(json!({"path":filename,"sha256":digest}));
    }
    let mut reports = Vec::new();
    let mut matrix = BTreeMap::<String, BTreeMap<String, usize>>::new();
    let mut matched = 0usize;
    let mut local_expectations = 0usize;
    let mut local_matched = 0usize;
    for case in cases {
        let source_text = string(case, "source_text")?;
        let candidate_text = string(case, "candidate_text")?;
        let expected_outcome = string(case, "expected_outcome")?;
        let (Some(source), Some(candidate)) = (
            annotations.get(source_text),
            annotations.get(candidate_text),
        ) else {
            *matrix
                .entry(expected_outcome.into())
                .or_default()
                .entry("parse_error".into())
                .or_default() += 1;
            reports.push(json!({"id":case["id"],"status":"parse_error","expected_outcome":expected_outcome,"source_error":failures.get(source_text),"candidate_error":failures.get(candidate_text)}));
            continue;
        };
        let patches: Vec<Edit> = serde_json::from_value(case["edits"].clone())?;
        let guard = serde_json::to_value(claim_guard::compare(source, candidate, &patches)?)?;
        let outcome = string(&guard, "outcome")?;
        let expectation_met = outcome == expected_outcome;
        matched += usize::from(expectation_met);
        let local_expectation_met = case["expected_local_operator_outcome"]
            .as_str()
            .map(|value| {
                local_expectations += 1;
                let met = guard["local_operator_outcome"] == value;
                local_matched += usize::from(met);
                met
            });
        *matrix
            .entry(expected_outcome.into())
            .or_default()
            .entry(outcome.into())
            .or_default() += 1;
        let mut contexts = BTreeMap::new();
        for context in rows(&protocol, "scoring_contexts")? {
            let name = string(context, "id")?;
            let contextual = |text: &str| -> Result<String> {
                Ok(format!(
                    "{}{}{}",
                    string(context, "prefix")?,
                    text,
                    string(context, "suffix")?
                ))
            };
            let source_key = contextual(source_text)?;
            let candidate_key = contextual(candidate_text)?;
            let value = if let (Some(a), Some(b)) = (
                annotations.get(&source_key),
                annotations.get(&candidate_key),
            ) {
                let before = scoring.score(a)?;
                let after = scoring.score(b)?;
                let decisions = selection(outcome, &before, &after)?;
                json!({"source_text_sha256":hash(source_key.as_bytes()),"candidate_text_sha256":hash(candidate_key.as_bytes()),"source":before,"candidate":after,"selection":decisions})
            } else {
                json!({"status":"parse_error","source_error":failures.get(&source_key),"candidate_error":failures.get(&candidate_key)})
            };
            ensure!(
                contexts.insert(name.to_string(), value).is_none(),
                "Repeated context ID"
            );
        }
        reports.push(json!({"id":case["id"],"partition":case["partition"],"status":"evaluated","expected_outcome":expected_outcome,"expectation_met":expectation_met,
            "declared_local_operator_expectation":case["expected_local_operator_outcome"],"local_operator_expectation_met":local_expectation_met,"rationale":case["rationale"],"protected_expectations":case["protected_expectations"],
            "source_text_sha256":hash(source_text.as_bytes()),"candidate_text_sha256":hash(candidate_text.as_bytes()),"edits":patches,"guard":guard,"scoring_contexts":contexts}));
    }
    let mut selection_counts = BTreeMap::<String, BTreeMap<String, usize>>::new();
    for report in &reports {
        if let Some(contexts) = report["scoring_contexts"].as_object() {
            for (name, context) in contexts {
                let totals = selection_counts.entry(name.clone()).or_default();
                if let Some(decisions) = context["selection"]["decisions"].as_array() {
                    for row in decisions {
                        *totals
                            .entry(format!("selected_{}", string(row, "selected")?))
                            .or_default() += 1;
                        if row["candidate_closer_without_guard"] == true
                            && row["candidate_eligible"] == false
                        {
                            *totals
                                .entry("closer_but_guard_ineligible".into())
                                .or_default() += 1;
                        }
                    }
                } else {
                    *totals.entry("unscorable_case".into()).or_default() += 1;
                }
            }
        }
    }
    let report = json!({"schema":"slopninja-claim-guard-evaluation-v1","protocol_sha256":expected,"fixture_sha256":fixture_binding["sha256"],"parser_identity":parser,
        "cases":reports,"aggregate":{"cases":cases.len(),"expected_outcomes_matched":matched,"local_operator_expectations":local_expectations,"local_operator_expectations_matched":local_matched,"confusion_matrix":matrix,"unique_parsed_texts":annotations.len(),"parse_failures":failures.len(),"selection_counts":selection_counts},
        "source_files":source_files,"executable_sha256":hash(&fs::read(std::env::current_exe()?)?),"target_audit_sha256":hash(&fs::read(out.join("target-audit.json"))?),
        "annotation_bindings":annotation_bindings,"guard_scope":"Exact-patch source alignment and observed lexical/operator/attachment evidence; no semantic-equivalence certification",
        "scoring_scope":"Same frozen original14 targets; guard applied to raw pair. Appended context is a separately declared score measurement.","semantic_equivalence_certified":false,"readability":"unjudged","tone":"unjudged","model_parameter_updates":0,"llm_calls":0,"detector_calls":0});
    let report_sha = fresh_json(&out.join("report.json"), &report)?;
    println!(
        "{}",
        serde_json::to_string(
            &json!({"report_sha256":report_sha,"aggregate":report["aggregate"]})
        )?
    );
    Ok(())
}

pub fn main() -> Result<()> {
    match Args::parse().command {
        Command::Compare {
            source_annotation,
            expected_source_annotation_sha256,
            candidate_annotation,
            expected_candidate_annotation_sha256,
            edits: edits_path,
            expected_edits_sha256,
            out,
        } => {
            let source: Document = serde_json::from_slice(&read_bound(
                &source_annotation,
                &expected_source_annotation_sha256,
            )?)?;
            let candidate: Document = serde_json::from_slice(&read_bound(
                &candidate_annotation,
                &expected_candidate_annotation_sha256,
            )?)?;
            let patches: Vec<Edit> =
                serde_json::from_slice(&read_bound(&edits_path, &expected_edits_sha256)?)?;
            let guard = claim_guard::compare(&source, &candidate, &patches)?;
            fresh_json(
                &out,
                &json!({"schema":"slopninja-claim-guard-comparison-v1","source_annotation_sha256":expected_source_annotation_sha256,"candidate_annotation_sha256":expected_candidate_annotation_sha256,"edits_sha256":expected_edits_sha256,"guard":guard}),
            )?;
            Ok(())
        }
        Command::Evaluate {
            protocol,
            expected_protocol_sha256,
            repo,
            out,
        } => evaluate(&protocol, &expected_protocol_sha256, &repo, &out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn score(logit: f64) -> Value {
        json!({"status":"scored","scores":[{"seed":0,"author":"synthetic-author","logit":logit}]})
    }
    #[test]
    fn proximity_cannot_override_changed_or_unresolved_claims() {
        for outcome in ["changed", "unresolved"] {
            let result = selection(outcome, &score(-2.0), &score(100.0)).unwrap();
            assert_eq!(result["decisions"][0]["selected"], "source");
            assert_eq!(
                result["decisions"][0]["candidate_closer_without_guard"],
                true
            );
        }
        assert_eq!(
            selection("observed_preserved", &score(-2.0), &score(1.0)).unwrap()["decisions"][0]["selected"],
            "candidate"
        );
    }
    #[test]
    fn equality_and_missing_features_keep_the_source() {
        assert_eq!(
            selection("observed_preserved", &score(1.0), &score(1.0)).unwrap()["decisions"][0]["selected"],
            "source"
        );
        assert_eq!(
            selection(
                "observed_preserved",
                &score(1.0),
                &json!({"status":"unscorable"})
            )
            .unwrap()["selected"],
            "source"
        );
        assert!(selection("unknown", &score(1.0), &score(1.0)).is_err());
        assert!(
            selection(
                "changed",
                &score(1.0),
                &json!({"status":"scored","scores":[]})
            )
            .is_err()
        );
    }
    #[test]
    fn utf8_anchors_reject_stale_or_split_codepoints() {
        assert!(
            validate_anchor("Zoë", &json!({"start_byte":2,"end_byte":4,"expected":"ë"})).is_ok()
        );
        assert!(
            validate_anchor("Zoë", &json!({"start_byte":2,"end_byte":3,"expected":"ë"})).is_err()
        );
        assert!(
            validate_anchor("Zoë", &json!({"start_byte":0,"end_byte":2,"expected":"No"})).is_err()
        );
    }
}

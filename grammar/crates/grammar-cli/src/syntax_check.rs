//! Compare explicit reviewer expectations with actual parses; never infer accuracy.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::syntax::{Document, Token};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, io::Write, path::PathBuf};

#[derive(Parser)]
struct Args {
    manifest: PathBuf,
    /// New private output directory. Contains source text and full annotations.
    #[arg(long)]
    out: PathBuf,
    #[arg(long)]
    python: String,
    #[arg(long, default_value = grammar_spacy::DEFAULT_MODEL)]
    parser_model: String,
    #[arg(long)]
    paragraph_boundaries: bool,
    /// Experimental v2: omit paragraph-edge whitespace from NLP, retaining source.
    #[arg(long, conflicts_with = "paragraph_boundaries")]
    paragraph_content: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String,
    reviewer: String,
    scope: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    text: String,
    expectations: Vec<Expectation>,
}

/// An omitted occurrence requires a unique token. No silent first-match selection.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Anchor {
    text: String,
    occurrence: Option<usize>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Expectation {
    token: Anchor,
    head: Option<Anchor>,
    #[serde(default)]
    dep: Vec<String>,
    #[serde(default)]
    pos: Vec<String>,
    #[serde(default)]
    forbidden_dep: Vec<String>,
    reason: String,
}

fn anchor<'a>(doc: &'a Document, a: &Anchor) -> Result<&'a Token> {
    let tokens: Vec<_> = doc.tokens.iter().filter(|t| t.text == a.text).collect();
    let i = match a.occurrence {
        Some(i) => i,
        None => {
            ensure!(
                tokens.len() == 1,
                "expected a unique token; found {}",
                tokens.len()
            );
            0
        }
    };
    tokens.get(i).copied().context("token occurrence absent")
}

fn check(doc: &Document, e: &Expectation) -> Value {
    let t = match anchor(doc, &e.token) {
        Ok(t) => t,
        Err(error) => {
            return json!({"expected":e,"status":"anchor_unresolved","reason":error.to_string()});
        }
    };
    let h = &doc.tokens[t.head];
    let mut differences = Vec::new();
    if let Some(expected) = &e.head {
        match anchor(doc, expected) {
            Ok(expected) if t.head != expected.i => differences.push("head"),
            Err(error) => {
                return json!({"expected":e,"status":"anchor_unresolved","reason":format!("head: {error}")});
            }
            _ => {}
        }
    }
    if !e.dep.is_empty() && !e.dep.contains(&t.dep) {
        differences.push("dependency");
    }
    if !e.pos.is_empty() && !e.pos.contains(&t.pos) {
        differences.push("part_of_speech");
    }
    if e.forbidden_dep.contains(&t.dep) {
        differences.push("forbidden_dependency");
    }
    json!({"expected":e,"status":if differences.is_empty(){"match"}else{"mismatch"},"differences":differences,"observed":{"token":t,"head":h}})
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn validate_manifest(m: &Manifest) -> Result<()> {
    ensure!(
        m.schema == "slop_ninja_syntax_expectations_v1",
        "Unknown schema"
    );
    ensure!(
        !m.reviewer.trim().is_empty() && !m.scope.trim().is_empty(),
        "Missing reviewer or scope"
    );
    ensure!(!m.cases.is_empty(), "Empty review");
    let mut ids = BTreeSet::new();
    for c in &m.cases {
        ensure!(
            !c.id.trim().is_empty() && ids.insert(&c.id),
            "Empty or duplicate case ID"
        );
        ensure!(
            !c.text.trim().is_empty() && !c.expectations.is_empty(),
            "Empty case"
        );
        for e in &c.expectations {
            ensure!(
                !e.reason.trim().is_empty() && !e.token.text.is_empty(),
                "Missing expectation rationale or token"
            );
            ensure!(
                e.head.is_some()
                    || !e.dep.is_empty()
                    || !e.pos.is_empty()
                    || !e.forbidden_dep.is_empty(),
                "Vacuous expectation"
            );
            ensure!(
                e.head.as_ref().is_none_or(|a| !a.text.is_empty()),
                "Empty head anchor"
            );
            ensure!(
                e.dep
                    .iter()
                    .chain(&e.pos)
                    .chain(&e.forbidden_dep)
                    .all(|s| !s.trim().is_empty()),
                "Empty label"
            );
            ensure!(
                e.dep.iter().all(|s| !e.forbidden_dep.contains(s)),
                "Contradictory dependency expectation"
            );
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    let bytes = fs::read(&args.manifest)?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    validate_manifest(&manifest)?;
    // Creating just the final directory fails on existing outputs, including partial runs.
    fs::create_dir(&args.out).context("Use a new output directory with an existing parent")?;
    fs::write(args.out.join("expectations.json"), &bytes)?;
    let parser = grammar_spacy::Parser {
        python: &args.python,
        model: &args.parser_model,
    };
    let mut stream = if args.paragraph_content {
        parser.stream_with_paragraph_content()?
    } else if args.paragraph_boundaries {
        parser.stream_with_paragraph_boundaries()?
    } else {
        parser.stream()?
    };
    let mut view = fs::File::create_new(args.out.join("review.txt"))?;
    let mut results = Vec::new();
    let mut identity = None;
    for (index, case) in manifest.cases.iter().enumerate() {
        writeln!(view, "\nCASE {}\n{}\n", case.id, case.text)?;
        match stream.parse(&case.text)? {
            Err(error) => {
                writeln!(view, "ANNOTATION UNAVAILABLE: {error}")?;
                results.push(json!({"id":case.id,"status":"annotation_unavailable","error":error,"expected_checks":case.expectations.len(),"source_sha256":hash(case.text.as_bytes())}));
            }
            Ok(doc) => {
                ensure!(
                    identity
                        .as_ref()
                        .is_none_or(|id| id == &doc.parser_identity),
                    "Parser identity changed"
                );
                identity = Some(doc.parser_identity.clone());
                let annotation = serde_json::to_vec_pretty(&doc)?;
                let path = format!("{index:03}.annotation.json");
                fs::write(args.out.join(&path), &annotation)?;
                for t in &doc.tokens {
                    let h = &doc.tokens[t.head];
                    writeln!(
                        view,
                        "{}:{} {:?} {}/{} {} -> {}:{:?}",
                        t.sentence, t.i, t.text, t.pos, t.tag, t.dep, h.i, h.text
                    )?;
                }
                let checks: Vec<_> = case.expectations.iter().map(|e| check(&doc, e)).collect();
                for c in &checks {
                    writeln!(
                        view,
                        "{} {:?}: {}",
                        c["status"].as_str().unwrap(),
                        c["expected"]["token"]["text"].as_str().unwrap(),
                        c["expected"]["reason"].as_str().unwrap()
                    )?;
                }
                results.push(json!({"id":case.id,"status":"annotated","source_sha256":hash(case.text.as_bytes()),"annotation":path,"annotation_sha256":hash(&annotation),"checks":checks}));
            }
        }
    }
    stream.finish()?;
    let mut statuses = std::collections::BTreeMap::<String, usize>::new();
    for c in &results {
        if let Some(checks) = c["checks"].as_array() {
            for check in checks {
                *statuses
                    .entry(check["status"].as_str().unwrap().into())
                    .or_default() += 1;
            }
        } else {
            *statuses.entry("annotation_unavailable".into()).or_default() +=
                c["expected_checks"].as_u64().unwrap() as usize;
        }
    }
    let report = json!({"schema":"slop_ninja_syntax_check_v1","manifest_sha256":hash(&bytes),"checker_source_sha256":hash(include_bytes!("syntax_check.rs")),"reviewer":manifest.reviewer,"scope":manifest.scope,"parser_identity":identity,"expectation_counts":statuses,"results":results,"parse_accuracy":null,"correctness_certified":false,"interpretation":"Matches apply only to the stated partial expectations. Inspect full annotations and revise disputed judgments separately. Exit success means the comparison completed, including mismatches and unavailable annotations."});
    fs::write(
        args.out.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "{}",
        json!({"expectation_counts":statuses,"parse_accuracy":null,"correctness_certified":false})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::syntax::{Sentence, validate};

    fn document() -> Document {
        let mut tokens = Vec::new();
        for (i, (text, start, head, dep, pos)) in [
            ("Birds", 0, 1, "nsubj", "NOUN"),
            ("eat", 6, 1, "ROOT", "VERB"),
            ("seeds", 10, 1, "dobj", "NOUN"),
        ]
        .into_iter()
        .enumerate()
        {
            tokens.push(Token {
                i,
                start_byte: start,
                end_byte: start + text.len(),
                text: text.into(),
                lemma: text.into(),
                pos: pos.into(),
                tag: pos.into(),
                dep: dep.into(),
                head,
                sentence: 0,
                morph: Default::default(),
                is_punct: false,
                is_space: false,
            });
        }
        Document {
            text: "Birds eat seeds".into(),
            parser_identity: "synthetic".into(),
            tokens,
            sentences: vec![Sentence {
                start_byte: 0,
                end_byte: 15,
                root: 1,
                token_start: 0,
                token_end: 3,
            }],
        }
    }

    #[test]
    fn valid_tree_can_still_have_wrong_subject_attachment() {
        let mut doc = document();
        let e: Expectation = serde_json::from_value(json!({"token":{"text":"Birds"},"head":{"text":"eat"},"dep":["nsubj"],"reason":"Birds performs the eating."})).unwrap();
        validate(&doc).unwrap();
        assert_eq!(check(&doc, &e)["status"], "match");
        doc.tokens[0].head = 2;
        validate(&doc).unwrap();
        assert_eq!(check(&doc, &e)["status"], "mismatch");
        assert_eq!(check(&doc, &e)["differences"], json!(["head"]));
    }

    #[test]
    fn repeated_and_absent_anchors_do_not_silently_match() {
        let mut doc = document();
        doc.tokens[2].text = "Birds".into();
        assert!(
            anchor(
                &doc,
                &Anchor {
                    text: "Birds".into(),
                    occurrence: None
                }
            )
            .is_err()
        );
        assert_eq!(
            anchor(
                &doc,
                &Anchor {
                    text: "Birds".into(),
                    occurrence: Some(1)
                }
            )
            .unwrap()
            .i,
            2
        );
        assert!(
            anchor(
                &doc,
                &Anchor {
                    text: "missing".into(),
                    occurrence: None
                }
            )
            .is_err()
        );
    }
}

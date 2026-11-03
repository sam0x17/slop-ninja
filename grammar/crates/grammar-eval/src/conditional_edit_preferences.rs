//! Bind the conditional-choice study to prior TRAIN annotations and exact sources.
use crate::{claim_guard, edit_choice_observation, edit_preference_model as model};
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::{edits, syntax::Document};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    expected_protocol_sha256: String,
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}
fn rows<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    v[key].as_array().with_context(|| format!("Missing {key}"))
}
fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("Read {}", path.display()))?;
    ensure!(hash(&bytes) == expected, "Hash differs: {}", path.display());
    Ok(bytes)
}
fn bound(repo: &Path, value: &Value) -> Result<Value> {
    Ok(serde_json::from_slice(&checked(
        &repo.join(text(value, "path")?),
        text(value, "sha256")?,
    )?)?)
}
fn fresh_json(path: &Path, value: &Value) -> Result<String> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(&bytes)?;
    Ok(hash(&bytes))
}

pub fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !args.out.exists(),
        "Use a new output directory for each attempt"
    );
    let protocol: Value =
        serde_json::from_slice(&checked(&args.protocol, &args.expected_protocol_sha256)?)?;
    validate_protocol(&protocol)?;
    for (path, digest) in protocol["source_files"]
        .as_object()
        .context("Source bindings")?
    {
        checked(
            &args.repo.join(path),
            digest.as_str().context("Source hash")?,
        )?;
    }
    fs::create_dir_all(&args.out)?;
    for path in protocol["source_files"].as_object().unwrap().keys() {
        let target = args.out.join("executed-source").join(path);
        fs::create_dir_all(target.parent().unwrap())?;
        fs::copy(args.repo.join(path), target)?;
    }
    fresh_json(&args.out.join("protocol.json"), &protocol)?;
    let result = run(&args, &protocol);
    if let Err(error) = &result {
        fresh_json(
            &args.out.join("failure.json"),
            &json!({
                "status":"failed; preserved attempt", "error":format!("{error:#}"),
                "protocol_sha256":args.expected_protocol_sha256
            }),
        )?;
    }
    result
}

fn validate_protocol(p: &Value) -> Result<()> {
    ensure!(
        p["schema"] == "slopninja-edit-preferences-protocol-v1"
            && p["expected_posts"] == 3889
            && p["expected_authors"] == 300
            && p["expected_panels"]
                == json!({"original":1224,"replication":1363,"confirmation":1302})
            && p["choice_rules"]
                == json!(["contract_negative_auxiliary", "expand_negative_auxiliary"])
            && p["context"] == "[canonical inflected auxiliary, declarative or do_imperative]"
            && p["personalization_prior_effective_dates"] == 4
            && p["population_beta_prior"] == json!([0.5, 0.5])
            && p["bootstrap"]["replicates"] == model::BOOTSTRAP_REPLICATES
            && p["bootstrap"]["seed_hex"] == "0x6a09e667f3bcc909"
            && p["bootstrap"]["strata"] == json!(["confirmation", "original", "replication"])
            && p["bootstrap"]["percentile_indices_zero_based"] == json!([49, 1949])
            && p["calibration"]["bins"] == 10
            && p["query_exclusion"] == "entire target-author query date"
            && p["population_exclusion"] == "entire target author"
            && p["primary_metric"]
                == "macro-author paired Brier loss: population minus personalized"
            && p["scorable_coverage"]
                == "retain every post/date/author; null metric for zero opportunities; report scored denominator"
            && p["synthetic_selection"]
                == "actual licensed counterpart plus observed_preserved guard and strictly higher personalized probability; otherwise source",
        "Frozen preference design differs"
    );
    Ok(())
}

fn validate_document(doc: &Document, post: &Value, parser: &str) -> Result<()> {
    ensure!(post["split"] == "train", "Non-TRAIN input");
    let date = text(post, "date")?;
    grammar_eval::validate_date(date)?;
    ensure!(
        post["source_group"] == format!("blog:{}:date:{date}", text(post, "author")?),
        "Source group differs"
    );
    ensure!(
        doc.parser_identity == parser && hash(doc.text.as_bytes()) == post["text_sha256"],
        "Annotation identity differs"
    );
    grammar_core::syntax::validate(doc)
}

fn run(args: &Args, protocol: &Value) -> Result<()> {
    let started = Instant::now();
    let fixture = bound(&args.repo, &protocol["fixture_manifest"])?;
    validate_fixture(&fixture)?;
    let predecessor = bound(&args.repo, &protocol["inputs"]["predecessor_stage"])?;
    ensure!(
        predecessor["status"] == "pass" && predecessor["stage_complete"] == true,
        "Predecessor stage is incomplete"
    );
    let prior = bound(&args.repo, &protocol["inputs"]["support_protocol"])?;
    let receipt = bound(&args.repo, &protocol["inputs"]["support_receipt"])?;
    ensure!(
        receipt["protocol_sha256"] == protocol["inputs"]["support_protocol"]["sha256"]
            && receipt["sources"] == prior["source_files"]
            && prior["parser_identity"] == protocol["parser_identity"],
        "Prior audit binding differs"
    );
    for (path, digest) in prior["source_files"]
        .as_object()
        .context("Prior source bindings")?
    {
        checked(
            &args.repo.join(path),
            digest.as_str().context("Prior source hash")?,
        )?;
    }
    let corpus = args.repo.join("data/author-corpora/blog-authorship-2004");
    let cache = corpus.join("vector-feature-cache-v1");
    let parser = text(protocol, "parser_identity")?;
    let mut posts = Vec::new();
    let mut authors = BTreeSet::new();
    let mut seen_ids = BTreeSet::new();
    let mut seen_texts = BTreeSet::new();
    let mut panel_counts = BTreeMap::new();
    let mut coverage = BTreeMap::<String, u64>::new();
    let mut output = BufWriter::new(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(args.out.join("observations.jsonl"))?,
    );
    for panel in rows(&prior, "panels")? {
        let panel_name = text(panel, "name")?;
        ensure!(!panel_counts.contains_key(panel_name), "Repeated panel");
        let export = corpus.join(text(panel, "export")?);
        let manifest: Value = serde_json::from_slice(&checked(
            &export.join("manifest.json"),
            text(panel, "manifest_sha256")?,
        )?)?;
        let audit: Value = serde_json::from_slice(&checked(
            &export.join("audit.json"),
            text(&manifest, "audit_sha256")?,
        )?)?;
        let queries: Value = serde_json::from_slice(&checked(
            &export.join("queries.json"),
            text(&manifest, "queries_sha256")?,
        )?)?;
        ensure!(
            manifest["split"] == "train"
                && audit["loaded_splits"] == json!(["train"])
                && manifest["parser_identity"] == parser,
            "Panel scope differs"
        );
        let ids = queries
            .as_array()
            .context("Query array")?
            .iter()
            .map(|q| text(q, "id").map(str::to_owned))
            .collect::<Result<BTreeSet<_>>>()?;
        ensure!(
            ids.len() == panel["posts"].as_u64().context("Panel post count")? as usize,
            "Query count differs"
        );
        let mut panel_authors = BTreeSet::new();
        let mut loaded = 0;
        for binding in rows(&audit, "posts")? {
            let post = &binding["post"];
            let id = text(post, "id")?;
            let author = text(post, "author")?;
            let text_sha = text(post, "text_sha256")?;
            ensure!(
                ids.contains(id)
                    && seen_ids.insert(id.to_owned())
                    && seen_texts.insert(text_sha.to_owned()),
                "Repeated/unbound post"
            );
            let document: Document = serde_json::from_slice(&checked(
                &cache.join(format!("{text_sha}.annotation.json")),
                text(binding, "annotation_sha256")?,
            )?)?;
            validate_document(&document, post, parser)?;
            let observed = serde_json::to_value(edit_choice_observation::observe(&document)?)?;
            let choices = rows(&observed, "opportunities")?;
            let mut observations = Vec::new();
            for choice in choices {
                let form = text(choice, "observed_form")?;
                ensure!(
                    ["contracted", "expanded"].contains(&form),
                    "Unknown observed form"
                );
                observations.push(model::Observation {
                    conditioning_key: text(choice, "conditioning_key")?.into(),
                    contracted: form == "contracted",
                });
            }
            for (name, count) in observed["coverage"].as_object().context("Coverage")? {
                if let Some(count) = count.as_u64() {
                    *coverage.entry(name.clone()).or_default() += count;
                }
            }
            serde_json::to_writer(
                &mut output,
                &json!({"panel":panel_name,"post":post,"annotation_sha256":binding["annotation_sha256"],"observation":observed}),
            )?;
            output.write_all(b"\n")?;
            posts.push(model::Post {
                id: id.into(),
                author: author.into(),
                date: text(post, "date")?.into(),
                panel: panel_name.into(),
                observations,
            });
            panel_authors.insert(author.to_owned());
            loaded += 1;
        }
        ensure!(
            loaded == ids.len()
                && panel_authors.len() == 100
                && panel_authors.is_disjoint(&authors),
            "Panel coverage differs"
        );
        authors.extend(panel_authors);
        panel_counts.insert(panel_name.to_owned(), loaded);
    }
    output.flush()?;
    ensure!(
        posts.len() == 3889
            && authors.len() == 300
            && serde_json::to_value(&panel_counts)? == protocol["expected_panels"],
        "Complete TRAIN scope differs"
    );
    fresh_json(
        &args.out.join("model-input.json"),
        &serde_json::to_value(&posts)?,
    )?;
    let model = model::PreferenceModel::new(posts)?;
    let contexts = model.context_keys();
    let mut profiles = Vec::new();
    for author in &authors {
        let values = contexts
            .iter()
            .map(|key| model.predict(author, None, key))
            .collect::<Result<Vec<_>>>()?;
        profiles.push(json!({"author":author,"context_predictions":values}));
    }
    fresh_json(
        &args.out.join("profiles.json"),
        &json!({
            "schema":"slopninja-conditional-choice-profiles-v1","model_schema":model::SCHEMA,
            "protocol_sha256":args.expected_protocol_sha256,"conditioning_keys":contexts,"profiles":profiles,
            "interpretation":"Full TRAIN context probabilities with explicit support and fallback; these do not define a semantic distance or certify edits."
        }),
    )?;
    let evaluation = model.evaluate()?;
    fresh_json(&args.out.join("evaluation.json"), &evaluation)?;
    let application = synthetic_application(args, protocol, &fixture, &model, &authors)?;
    fresh_json(&args.out.join("synthetic-application.json"), &application)?;
    let report = json!({
        "schema":"slopninja-edit-preferences-study-v1","status":"complete", "protocol_sha256":args.expected_protocol_sha256,
        "posts":3889,"authors":300,"panel_posts":panel_counts,"opportunity_coverage":coverage,
        "evaluation_sha256":hash(&fs::read(args.out.join("evaluation.json"))?),
        "synthetic_application_sha256":hash(&fs::read(args.out.join("synthetic-application.json"))?),
        "synthetic_summary":application["summary"], "elapsed_seconds":started.elapsed().as_secs_f64(),
        "fresh_author_allocation":0,"detector_calls":0,"llm_calls":0,"nn_fits":0,"hyperparameter_search":false,
        "interpretation":"Reused TRAIN dates; fixed Beta shrinkage estimates grammatical choices, not semantic fidelity, tone or detector outcomes.",
        "model_adoption":"none; conditional model evaluated separately from existing author-recognition models"
    });
    fresh_json(&args.out.join("report.json"), &report)?;
    let mut artifacts = BTreeMap::new();
    for name in [
        "observations.jsonl",
        "model-input.json",
        "profiles.json",
        "evaluation.json",
        "synthetic-application.json",
        "report.json",
    ] {
        artifacts.insert(name, hash(&fs::read(args.out.join(name))?));
    }
    fresh_json(
        &args.out.join("receipt.json"),
        &json!({
            "schema":"slopninja-edit-preferences-receipt-v1","protocol_sha256":args.expected_protocol_sha256,
            "source_files":protocol["source_files"],"inputs":protocol["inputs"],"fixture":protocol["fixture_manifest"],
            "artifacts_sha256":artifacts,"executable_sha256":hash(&fs::read(std::env::current_exe()?)?)
        }),
    )?;
    println!(
        "Completed conditional preferences on {} posts; report saved to {}",
        seen_ids.len(),
        args.out.display()
    );
    Ok(())
}

fn choose(guard: &str, contracted: bool, probability: f64) -> Result<bool> {
    ensure!(
        ["observed_preserved", "changed", "unresolved"].contains(&guard),
        "Unknown guard outcome"
    );
    ensure!(
        probability.is_finite() && (0.0..=1.0).contains(&probability),
        "Invalid choice probability"
    );
    let source_probability = if contracted {
        probability
    } else {
        1.0 - probability
    };
    let counterpart_probability = 1.0 - source_probability;
    Ok(guard == "observed_preserved" && counterpart_probability > source_probability)
}

fn validate_fixture(fixture: &Value) -> Result<()> {
    ensure!(
        fixture["schema"] == "slopninja-edit-preference-fixtures-v1"
            && rows(fixture, "cases")?.len() == 6,
        "Fixture scope differs"
    );
    let mut ids = BTreeSet::new();
    for case in rows(fixture, "cases")? {
        ensure!(
            ids.insert(text(case, "id")?),
            "Duplicate fixture identifier"
        );
        let source = text(case, "source_text")?;
        let candidate = text(case, "candidate_text")?;
        let patches: Vec<edits::Edit> = serde_json::from_value(case["edits"].clone())?;
        ensure!(
            edits::apply(source, &patches)? == candidate,
            "Fixture reconstruction differs"
        );
        for expected in rows(case, "protected_expectations")? {
            for (field, value) in [("source_anchors", source), ("candidate_anchors", candidate)] {
                for anchor in rows(expected, field)? {
                    let start = anchor["start_byte"].as_u64().context("Anchor start")? as usize;
                    let end = anchor["end_byte"].as_u64().context("Anchor end")? as usize;
                    ensure!(
                        value.get(start..end) == Some(text(anchor, "expected")?),
                        "Protected anchor differs"
                    );
                }
            }
        }
    }
    Ok(())
}

fn synthetic_application(
    args: &Args,
    protocol: &Value,
    fixture: &Value,
    model: &model::PreferenceModel,
    authors: &BTreeSet<String>,
) -> Result<Value> {
    let cases = rows(fixture, "cases")?;
    let mut texts = BTreeSet::new();
    for case in cases {
        texts.insert(text(case, "source_text")?.to_owned());
        texts.insert(text(case, "candidate_text")?.to_owned());
    }
    let input = texts.iter().map(String::as_str).collect::<Vec<_>>();
    let parsed = grammar_spacy::parse_batch(&input, text(protocol, "parser_python")?)?;
    ensure!(
        parsed.len() == input.len(),
        "Synthetic parser result coverage differs"
    );
    let mut documents = BTreeMap::new();
    let mut bindings = Vec::new();
    fs::create_dir(args.out.join("synthetic-annotations"))?;
    for (source, result) in input.into_iter().zip(parsed) {
        let annotation = match result {
            Ok(doc) => {
                ensure!(
                    doc.text == source && doc.parser_identity == protocol["parser_identity"],
                    "Synthetic parser identity differs"
                );
                documents.insert(source.to_owned(), doc.clone());
                json!({"document":doc})
            }
            Err(error) => json!({"error":error}),
        };
        let path = format!("synthetic-annotations/{}.json", hash(source.as_bytes()));
        let digest = fresh_json(&args.out.join(&path), &annotation)?;
        bindings.push(json!({"path":path,"sha256":digest}));
    }
    let mut reports = Vec::new();
    let mut selected = 0usize;
    let mut unavailable = 0usize;
    let mut extraction_matches = 0usize;
    let mut guard_matches = 0usize;
    for case in cases {
        let source_text = text(case, "source_text")?;
        let candidate_text = text(case, "candidate_text")?;
        let patches: Vec<edits::Edit> = serde_json::from_value(case["edits"].clone())?;
        ensure!(
            edits::apply(source_text, &patches)? == candidate_text,
            "Synthetic fixture patches differ"
        );
        let (Some(source), Some(candidate)) =
            (documents.get(source_text), documents.get(candidate_text))
        else {
            unavailable += 1;
            reports.push(json!({"id":case["id"],"status":"parse_unavailable","selected":"source","target_authors":authors.len()}));
            continue;
        };
        let observed = serde_json::to_value(edit_choice_observation::observe(source)?)?;
        let actual = rows(&observed, "opportunities")?.iter().find(|row| {
            row["counterpart"]["text"] == candidate_text
                && row["counterpart"]["edits"] == case["edits"]
        });
        let guard = serde_json::to_value(claim_guard::compare(source, candidate, &patches)?)?;
        let guard_expectation_met = guard["outcome"] == case["expected_guard_outcome"];
        guard_matches += usize::from(guard_expectation_met);
        let Some(choice) = actual else {
            unavailable += 1;
            reports.push(json!({"id":case["id"],"status":"rule_unavailable","guard":guard,"guard_expectation_met":guard_expectation_met,"extraction_expectation_met":false,"selected":"source","target_authors":authors.len()}));
            continue;
        };
        let extraction = json!({"opportunity_count":rows(&observed,"opportunities")?.len(),"observed_form":choice["observed_form"],"canonical_auxiliary_form":choice["canonical_auxiliary_form"],"canonical_expanded_form":choice["canonical_expanded_form"],"clause_type":choice["clause_type"],"conditioning_key":choice["conditioning_key"],"rule":choice["counterpart"]["rule"],"counterpart_text":choice["counterpart"]["text"]});
        let extraction_expectation_met = extraction == case["expected_extraction"];
        extraction_matches += usize::from(extraction_expectation_met);
        let key = text(choice, "conditioning_key")?;
        let contracted = text(choice, "observed_form")? == "contracted";
        let mut decisions = Vec::new();
        for author in authors {
            let prediction = model.predict(author, None, key)?;
            let change = choose(
                text(&guard, "outcome")?,
                contracted,
                prediction.author_probability,
            )?;
            selected += usize::from(change);
            decisions.push(json!({"author":author,"prediction":prediction,"selected":if change {"candidate"} else {"source"}}));
        }
        reports.push(json!({"id":case["id"],"status":"evaluated","extraction":extraction,"extraction_expectation_met":extraction_expectation_met,"guard_expectation_met":guard_expectation_met,"conditioning_key":key,"observed_contracted":contracted,"guard":guard,"decisions":decisions}));
    }
    Ok(
        json!({"schema":"slopninja-conditional-preference-synthetic-application-v1","annotations":bindings,"cases":reports,
        "summary":{"cases":cases.len(),"unavailable_cases":unavailable,"extraction_expectations_matched":extraction_matches,"guard_expectations_matched":guard_matches,"authors_per_case":authors.len(),"candidate_selections":selected,"source_selections":cases.len()*authors.len()-selected},
        "interpretation":"Correlated synthetic choices using full TRAIN profiles; no held-date performance or semantic/readability/tone judgment; no new corpus rewrite."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prediction_cannot_override_changed_unresolved_or_tied_source() {
        for guard in ["changed", "unresolved"] {
            assert!(!choose(guard, false, 0.99).unwrap());
            assert!(!choose(guard, true, 0.01).unwrap());
        }
        assert!(!choose("observed_preserved", false, 0.5).unwrap());
        assert!(!choose("observed_preserved", true, 0.5).unwrap());
        assert!(choose("observed_preserved", false, 0.7).unwrap());
        assert!(choose("observed_preserved", true, 0.3).unwrap());
        assert!(choose("observed_preserved", true, f64::NAN).is_err());
    }

    #[test]
    fn fixed_fixture_patches_and_protected_anchors_reconstruct() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../fixtures/edit-preferences-v1/manifest.json"
        ))
        .unwrap();
        validate_fixture(&fixture).unwrap();
        let mut broken = fixture;
        broken["cases"][0]["protected_expectations"][0]["source_anchors"][0]["start_byte"] =
            json!(0);
        assert!(validate_fixture(&broken).is_err());
    }
}

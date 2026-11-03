//! Fixed synthetic comparison; intrinsic rankings remain separate from guard decisions.
use crate::{
    claim_guard, edit_choice_observation, edit_preference_model as model,
    edit_utility_scoring::Scoring,
};
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::{edits, syntax::Document};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
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
fn checked(path: &Path, sha: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(path)?;
    ensure!(hash(&bytes) == sha, "Hash mismatch {}", path.display());
    Ok(bytes)
}
fn bound(repo: &Path, v: &Value) -> Result<Value> {
    Ok(serde_json::from_slice(&checked(
        &repo.join(text(v, "path")?),
        text(v, "sha256")?,
    )?)?)
}
fn fresh(path: &Path, v: &Value) -> Result<String> {
    let mut bytes = serde_json::to_vec_pretty(v)?;
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
        "Retain old attempts; use a new output directory"
    );
    let protocol: Value =
        serde_json::from_slice(&checked(&args.protocol, &args.expected_protocol_sha256)?)?;
    ensure!(
        protocol["schema"] == "slopninja-preference-score-comparison-protocol-v1"
            && protocol["directional_cases"] == 6
            && protocol["canonical_pairs"] == 3
            && protocol["target_authors"] == 10
            && protocol["seeds"] == json!([0, 17, 29])
            && protocol["comparison"]
                == "intrinsic contracted/expanded/exact_tie preference; guard decisions separate"
            && protocol["primary_rows"]
                == "expanded-source direction only; 3 pairs x10 authors x3 seeds per context"
            && protocol["confidence_intervals"] == false,
        "Protocol design differs"
    );
    for (name, sha) in protocol["source_files"].as_object().context("Sources")? {
        checked(&args.repo.join(name), sha.as_str().context("Source hash")?)?;
    }
    fs::create_dir_all(&args.out)?;
    for name in protocol["source_files"].as_object().unwrap().keys() {
        let to = args.out.join("executed-source").join(name);
        fs::create_dir_all(to.parent().unwrap())?;
        fs::copy(args.repo.join(name), to)?;
    }
    fresh(&args.out.join("protocol.json"), &protocol)?;
    let result = run(&args, &protocol);
    if let Err(error) = &result {
        fresh(
            &args.out.join("failure.json"),
            &json!({"error":format!("{error:#}"),"status":"failed; retained attempt"}),
        )?;
    }
    result
}

fn conditional_form(p: f64) -> Result<&'static str> {
    ensure!(
        p.is_finite() && (0.0..=1.0).contains(&p),
        "Invalid conditional probability"
    );
    Ok(if p > 0.5 {
        "contracted"
    } else if p < 0.5 {
        "expanded"
    } else {
        "exact_tie"
    })
}
fn proximity_form(
    source: Option<f64>,
    candidate: Option<f64>,
    source_contracted: bool,
) -> Result<&'static str> {
    let (Some(source), Some(candidate)) = (source, candidate) else {
        return Ok("unavailable");
    };
    ensure!(
        source.is_finite() && candidate.is_finite(),
        "Nonfinite proximity score"
    );
    if source == candidate {
        return Ok("exact_tie");
    }
    let choose_contracted = if candidate > source {
        !source_contracted
    } else {
        source_contracted
    };
    Ok(if choose_contracted {
        "contracted"
    } else {
        "expanded"
    })
}
fn guarded_choice(guard: &str, preferred: &str, source_contracted: bool) -> Result<&'static str> {
    ensure!(
        ["observed_preserved", "changed", "unresolved"].contains(&guard),
        "Unknown guard outcome"
    );
    ensure!(
        ["contracted", "expanded", "exact_tie", "unavailable"].contains(&preferred),
        "Unknown preference"
    );
    let source = if source_contracted {
        "contracted"
    } else {
        "expanded"
    };
    Ok(
        if guard == "observed_preserved"
            && ["contracted", "expanded"].contains(&preferred)
            && preferred != source
        {
            "candidate"
        } else {
            "source"
        },
    )
}
fn score_for(value: &Value, author: &str, seed: u64) -> Result<Option<f64>> {
    if value["status"] == "unscorable" {
        return Ok(None);
    }
    ensure!(value["status"] == "scored", "Unknown scoring status");
    let matches = rows(value, "scores")?
        .iter()
        .filter(|v| v["author"] == author && v["seed"] == seed)
        .collect::<Vec<_>>();
    ensure!(matches.len() == 1, "Missing or repeated seed/target score");
    Ok(Some(matches[0]["logit"].as_f64().context("Missing logit")?))
}
fn summary(rows: &[Value]) -> Value {
    let mut counts = [
        "total",
        "both_scorable",
        "source_only_unavailable",
        "candidate_only_unavailable",
        "both_unavailable",
        "population_only_fallback",
        "personal_context_supported",
        "conditional_exact_tie",
        "proximity_exact_tie",
        "both_exact_tie",
        "both_strict",
        "same_preferred_form",
        "opposite_preferred_form",
    ]
    .into_iter()
    .map(|key| (key.to_owned(), 0usize))
    .collect::<BTreeMap<_, _>>();
    let mut table = ["contracted", "expanded", "exact_tie"]
        .into_iter()
        .map(|key| {
            (
                key.to_owned(),
                ["contracted", "expanded", "exact_tie"]
                    .into_iter()
                    .map(|inner| (inner.to_owned(), 0usize))
                    .collect::<BTreeMap<_, _>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for row in rows {
        *counts.entry("total".into()).or_default() += 1;
        let source = row["source_logit"].is_number();
        let candidate = row["candidate_logit"].is_number();
        let availability = match (source, candidate) {
            (true, true) => "both_scorable",
            (false, true) => "source_only_unavailable",
            (true, false) => "candidate_only_unavailable",
            (false, false) => "both_unavailable",
        };
        *counts.entry(availability.into()).or_default() += 1;
        if row["conditional_prediction"]["author_uses_population_only"] == true {
            *counts.entry("population_only_fallback".into()).or_default() += 1;
        } else {
            *counts
                .entry("personal_context_supported".into())
                .or_default() += 1;
        }
        let conditional = row["conditional_preferred_form"].as_str().unwrap();
        let proximity = row["proximity_preferred_form"].as_str().unwrap();
        if conditional == "exact_tie" {
            *counts.entry("conditional_exact_tie".into()).or_default() += 1;
        }
        if proximity == "exact_tie" {
            *counts.entry("proximity_exact_tie".into()).or_default() += 1;
        }
        if source && candidate {
            *table
                .entry(proximity.into())
                .or_default()
                .entry(conditional.into())
                .or_default() += 1;
            if conditional == "exact_tie" && proximity == "exact_tie" {
                *counts.entry("both_exact_tie".into()).or_default() += 1;
            }
            if conditional != "exact_tie" && proximity != "exact_tie" {
                *counts.entry("both_strict".into()).or_default() += 1;
                *counts
                    .entry(
                        if conditional == proximity {
                            "same_preferred_form"
                        } else {
                            "opposite_preferred_form"
                        }
                        .into(),
                    )
                    .or_default() += 1;
            }
        }
    }
    json!({"counts":counts,"proximity_by_conditional_preference":table,"agreement_denominator":"both_strict only; ties and unavailable rows retained separately"})
}

fn run(args: &Args, p: &Value) -> Result<()> {
    let fixture = bound(&args.repo, &p["fixture_manifest"])?;
    let old_protocol = bound(&args.repo, &p["scoring_protocol"])?;
    let preference_receipt = bound(&args.repo, &p["preference_receipt"])?;
    ensure!(
        preference_receipt["artifacts_sha256"]["model-input.json"] == p["model_input"]["sha256"],
        "Preference input binding differs"
    );
    let training: Vec<model::Post> = serde_json::from_value(bound(&args.repo, &p["model_input"])?)?;
    ensure!(
        training.len() == 3889
            && training
                .iter()
                .map(|post| &post.author)
                .collect::<BTreeSet<_>>()
                .len()
                == 300,
        "Preference TRAIN scope differs"
    );
    let scoring = Scoring::load(&old_protocol)?;
    let audit = scoring.target_audit();
    let expected_audit = bound(&args.repo, &p["expected_target_audit"])?;
    ensure!(audit == expected_audit, "Proximity target bindings changed");
    let authors = rows(&audit, "selected_author_ids")?
        .iter()
        .map(|v| v.as_str().context("Author ID").map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    let training_index = training
        .iter()
        .map(|post| (post.id.as_str(), post))
        .collect::<BTreeMap<_, _>>();
    let mut target_ids = BTreeSet::new();
    for post in rows(&audit, "selected_post_bindings")? {
        let input = training_index
            .get(text(post, "id")?)
            .context("Target post absent from conditional inputs")?;
        ensure!(
            input.author == post["author"]
                && input.date == post["date"]
                && input.panel == "original",
            "Conditional/proximity target training differs"
        );
        ensure!(target_ids.insert(input.id.clone()), "Repeated target input");
    }
    ensure!(
        training
            .iter()
            .filter(|post| authors.contains(&post.author))
            .map(|post| post.id.clone())
            .collect::<BTreeSet<_>>()
            == target_ids,
        "Conditional target contains different training posts"
    );
    let preference = model::PreferenceModel::new(training)?;
    fresh(&args.out.join("target-audit.json"), &audit)?;
    let cases = rows(&fixture, "cases")?;
    ensure!(cases.len() == 6, "Fixture scope differs");
    let contexts = rows(p, "scoring_contexts")?;
    let old_context_protocol = bound(&args.repo, &p["context_protocol"])?;
    ensure!(
        p["scoring_contexts"] == old_context_protocol["scoring_contexts"],
        "Synthetic surroundings changed"
    );
    let mut texts = BTreeSet::new();
    for case in cases {
        let patches: Vec<edits::Edit> = serde_json::from_value(case["edits"].clone())?;
        ensure!(
            edits::apply(text(case, "source_text")?, &patches)? == case["candidate_text"],
            "Fixture patch differs"
        );
        for key in ["source_text", "candidate_text"] {
            texts.insert(text(case, key)?.to_owned());
            for context in contexts {
                texts.insert(format!(
                    "{}{}{}",
                    text(context, "prefix")?,
                    text(case, key)?,
                    text(context, "suffix")?
                ));
            }
        }
    }
    let input = texts.iter().map(String::as_str).collect::<Vec<_>>();
    let parsed = grammar_spacy::parse_batch(&input, text(p, "parser_python")?)?;
    ensure!(parsed.len() == input.len(), "Parser result count differs");
    let mut documents = BTreeMap::<String, Document>::new();
    let mut annotations = Vec::new();
    fs::create_dir(args.out.join("annotations"))?;
    for (source, parsed) in input.into_iter().zip(parsed) {
        let value = match parsed {
            Ok(doc) => {
                ensure!(
                    doc.text == source && doc.parser_identity == p["parser_identity"],
                    "Parser identity differs"
                );
                documents.insert(source.into(), doc.clone());
                json!({"document":doc})
            }
            Err(error) => json!({"error":error}),
        };
        let path = format!("annotations/{}.json", hash(source.as_bytes()));
        let sha = fresh(&args.out.join(&path), &value)?;
        annotations.push(json!({"path":path,"sha256":sha}));
    }
    // A parse failure is retained and stops this complete fixed-panel comparison.
    ensure!(
        documents.len() == texts.len(),
        "Synthetic parse failure; annotation failures retained"
    );
    let mut scores = BTreeMap::new();
    for (source, doc) in &documents {
        scores.insert(source.clone(), scoring.score(doc)?);
    }
    let mut directional = Vec::new();
    let mut case_reports = Vec::new();
    for case in cases {
        let source_text = text(case, "source_text")?;
        let candidate_text = text(case, "candidate_text")?;
        let patches: Vec<edits::Edit> = serde_json::from_value(case["edits"].clone())?;
        let observations = edit_choice_observation::observe(&documents[source_text])?;
        let choices = observations
            .opportunities
            .iter()
            .filter(|choice| {
                choice.counterpart.text == candidate_text && choice.counterpart.edits == patches
            })
            .collect::<Vec<_>>();
        ensure!(
            choices.len() == 1,
            "Expected one exact original-rule counterpart"
        );
        let choice = choices[0];
        let source_contracted =
            choice.observed_form == edit_choice_observation::ObservedForm::Contracted;
        let guard = serde_json::to_value(claim_guard::compare(
            &documents[source_text],
            &documents[candidate_text],
            &patches,
        )?)?;
        case_reports.push(json!({"id":case["id"],"choice":choice,"guard":guard,"expected_guard_outcome":case["expected_guard_outcome"],"guard_expectation_met":guard["outcome"]==case["expected_guard_outcome"]}));
        for context in contexts {
            let before = format!(
                "{}{}{}",
                text(context, "prefix")?,
                source_text,
                text(context, "suffix")?
            );
            let after = format!(
                "{}{}{}",
                text(context, "prefix")?,
                candidate_text,
                text(context, "suffix")?
            );
            let source_score = &scores[&before];
            let candidate_score = &scores[&after];
            for author in &authors {
                let prediction = preference.predict(author, None, &choice.conditioning_key)?;
                let conditional = conditional_form(prediction.author_probability)?;
                for seed in [0, 17, 29] {
                    let source_logit = score_for(source_score, author, seed)?;
                    let candidate_logit = score_for(candidate_score, author, seed)?;
                    let proximity =
                        proximity_form(source_logit, candidate_logit, source_contracted)?;
                    directional.push(json!({"case":case["id"],"context":context["id"],"author":author,"seed":seed,"conditioning_key":choice.conditioning_key,"source_contracted":source_contracted,
                        "conditional_prediction":prediction,"conditional_preferred_form":conditional,"proximity_preferred_form":proximity,
                        "source_logit":source_logit,"candidate_logit":candidate_logit,"candidate_minus_source_logit":source_logit.zip(candidate_logit).map(|(before,after)|after-before),
                        "source_unavailable_families":source_score["unavailable_families"],"candidate_unavailable_families":candidate_score["unavailable_families"],
                        "guard_outcome":guard["outcome"],"conditional_guarded_choice":guarded_choice(text(&guard,"outcome")?,conditional,source_contracted)?,"proximity_guarded_choice":guarded_choice(text(&guard,"outcome")?,proximity,source_contracted)?}));
                }
            }
        }
    }
    ensure!(
        directional.len() == 360,
        "Directional comparison coverage differs"
    );
    let mut summaries = BTreeMap::new();
    let mut canonical = Vec::new();
    for context in contexts {
        let primary = directional
            .iter()
            .filter(|row| row["context"] == context["id"] && row["source_contracted"] == false)
            .cloned()
            .collect::<Vec<_>>();
        ensure!(primary.len() == 90, "Canonical comparison coverage differs");
        let supported = primary
            .iter()
            .filter(|row| row["conditional_prediction"]["author_uses_population_only"] == false)
            .cloned()
            .collect::<Vec<_>>();
        let fallback = primary
            .iter()
            .filter(|row| row["conditional_prediction"]["author_uses_population_only"] == true)
            .cloned()
            .collect::<Vec<_>>();
        summaries.insert(text(context,"id")?.to_owned(),json!({"all":summary(&primary),"personal_supported":summary(&supported),"population_fallback":summary(&fallback)}));
        canonical.extend(primary);
    }
    for row in &canonical {
        let inverse = directional
            .iter()
            .filter(|v| {
                v["source_contracted"] == true
                    && v["conditioning_key"] == row["conditioning_key"]
                    && v["author"] == row["author"]
                    && v["seed"] == row["seed"]
                    && v["context"] == row["context"]
            })
            .collect::<Vec<_>>();
        ensure!(
            inverse.len() == 1
                && inverse[0]["conditional_preferred_form"] == row["conditional_preferred_form"]
                && inverse[0]["proximity_preferred_form"] == row["proximity_preferred_form"],
            "Inverse form ranking differs"
        );
    }
    let report = json!({"schema":"slopninja-preference-score-comparison-v1","status":"complete","protocol_sha256":args.expected_protocol_sha256,"annotations":annotations,"cases":case_reports,"directional_rows":directional,"canonical_rows":canonical,"summaries":summaries,
        "coverage":{"unique_pairs":3,"directional_cases":6,"contexts":contexts.len(),"authors":authors.len(),"seeds":3,"canonical_rows":180,"directional_rows":360,"inverse_checks":180,"parsed_texts":documents.len()},
        "interpretation":"Intrinsic preference agreement on fixed reused synthetic pairs; no prediction-accuracy, semantic-fidelity, readability or detector claim. Guarded source retention is separate.","llm_calls":0,"detector_calls":0,"parameter_updates":0});
    let report_sha = fresh(&args.out.join("report.json"), &report)?;
    fresh(
        &args.out.join("receipt.json"),
        &json!({"schema":"slopninja-preference-score-comparison-receipt-v1","protocol_sha256":args.expected_protocol_sha256,"report_sha256":report_sha,"sources":p["source_files"],"target_audit_sha256":hash(&fs::read(args.out.join("target-audit.json"))?),"executable_sha256":hash(&fs::read(std::env::current_exe()?)?)}),
    )?;
    println!(
        "Completed exact-edit preference comparison; {}",
        args.out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_preference_does_not_flip_with_source_direction() {
        assert_eq!(
            proximity_form(Some(1.0), Some(2.0), false).unwrap(),
            "contracted"
        );
        assert_eq!(
            proximity_form(Some(2.0), Some(1.0), true).unwrap(),
            "contracted"
        );
        assert_eq!(
            proximity_form(None, Some(2.0), false).unwrap(),
            "unavailable"
        );
        assert_eq!(
            proximity_form(Some(2.0), Some(2.0), false).unwrap(),
            "exact_tie"
        );
        assert_eq!(conditional_form(0.5).unwrap(), "exact_tie");
        assert!(conditional_form(f64::NAN).is_err());
    }
    #[test]
    fn guard_retention_is_not_intrinsic_preference() {
        assert_eq!(
            guarded_choice("changed", "contracted", false).unwrap(),
            "source"
        );
        assert_eq!(
            guarded_choice("changed", "expanded", true).unwrap(),
            "source"
        );
        assert_eq!(
            guarded_choice("observed_preserved", "contracted", false).unwrap(),
            "candidate"
        );
        assert_eq!(
            guarded_choice("observed_preserved", "exact_tie", false).unwrap(),
            "source"
        );
    }

    #[test]
    fn agreement_denominator_excludes_ties_and_every_unavailable_side() {
        let examples = [
            ("contracted", "contracted", true, true),
            ("expanded", "contracted", true, true),
            ("expanded", "expanded", true, true),
            ("exact_tie", "contracted", true, true),
            ("contracted", "exact_tie", true, true),
            ("exact_tie", "exact_tie", true, true),
            ("unavailable", "expanded", false, true),
            ("unavailable", "expanded", true, false),
            ("unavailable", "contracted", false, false),
        ];
        let rows = examples.into_iter().enumerate().map(|(i,(proximity,conditional,source,candidate))| json!({
            "source_logit":source.then_some(1.0),"candidate_logit":candidate.then_some(2.0),
            "conditional_preferred_form":conditional,"proximity_preferred_form":proximity,
            "conditional_prediction":{"author_uses_population_only":i%2==0},"guard_outcome":"changed"
        })).collect::<Vec<_>>();
        let actual = summary(&rows);
        for (key, expected) in [
            ("total", 9),
            ("both_scorable", 6),
            ("both_strict", 3),
            ("same_preferred_form", 2),
            ("opposite_preferred_form", 1),
            ("source_only_unavailable", 1),
            ("candidate_only_unavailable", 1),
            ("both_unavailable", 1),
            ("conditional_exact_tie", 2),
            ("proximity_exact_tie", 2),
            ("both_exact_tie", 1),
            ("population_only_fallback", 5),
            ("personal_context_supported", 4),
        ] {
            assert_eq!(actual["counts"][key], json!(expected), "{key}");
        }
        assert_eq!(
            actual["proximity_by_conditional_preference"]["contracted"]["expanded"],
            0
        );
        assert_eq!(
            actual["proximity_by_conditional_preference"]["expanded"]["contracted"],
            1
        );
    }
}

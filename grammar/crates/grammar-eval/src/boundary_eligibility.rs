//! Retain every TRAIN boundary site and the parsed inverse/guard results.
use crate::boundary_choice_observation as observation;
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
fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(path)?;
    ensure!(hash(&bytes) == expected, "Hash mismatch {}", path.display());
    Ok(bytes)
}
fn bound(repo: &Path, value: &Value) -> Result<Value> {
    Ok(serde_json::from_slice(&checked(
        &repo.join(text(value, "path")?),
        text(value, "sha256")?,
    )?)?)
}
fn fresh(path: &Path, value: &Value) -> Result<String> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(&bytes)?;
    Ok(hash(&bytes))
}
fn bind_output(out: &Path, name: &str, value: &Value) -> Result<Value> {
    Ok(json!({"path":name,"sha256":fresh(&out.join(name),value)?}))
}

pub fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a fresh output directory");
    let p: Value =
        serde_json::from_slice(&checked(&args.protocol, &args.expected_protocol_sha256)?)?;
    ensure!(
        p["schema"] == "slopninja-boundary-eligibility-protocol-v1"
            && p["expected_posts"] == 3889
            && p["expected_authors"] == 300
            && p["parser_batch_size"] == 64
            && p["preference_estimation"] == false
            && p["query_posts"] == 0,
        "Protocol design differs"
    );
    for (name, sha) in p["source_files"].as_object().context("Source bindings")? {
        checked(&args.repo.join(name), sha.as_str().context("Source hash")?)?;
    }
    fs::create_dir_all(&args.out)?;
    for name in p["source_files"].as_object().unwrap().keys() {
        let to = args.out.join("executed-source").join(name);
        fs::create_dir_all(to.parent().unwrap())?;
        fs::copy(args.repo.join(name), to)?;
    }
    fresh(&args.out.join("protocol.json"), &p)?;
    let result = run(&args, &p);
    if let Err(error) = &result {
        fresh(
            &args.out.join("failure.json"),
            &json!({"status":"failed; retained attempt","error":format!("{error:#}")}),
        )?;
    }
    result
}

#[derive(Default)]
struct Annotations {
    documents: BTreeMap<String, std::result::Result<Document, String>>,
    bindings: Vec<Value>,
    batches: usize,
}
impl Annotations {
    fn parse(&mut self, requested: BTreeMap<String, String>, args: &Args, p: &Value) -> Result<()> {
        let pending: Vec<_> = requested
            .into_iter()
            .filter(|(sha, _)| !self.documents.contains_key(sha))
            .collect();
        for batch in pending.chunks(64) {
            let inputs: Vec<_> = batch.iter().map(|(_, text)| text.as_str()).collect();
            let parsed = grammar_spacy::parse_batch(&inputs, text(p, "parser_python")?)?;
            self.batches += 1;
            for ((sha, source), result) in batch.iter().zip(parsed) {
                if let Ok(doc) = &result {
                    ensure!(
                        doc.text == *source
                            && hash(doc.text.as_bytes()) == *sha
                            && doc.parser_identity == p["parser_identity"],
                        "Parsed text or parser identity changed"
                    );
                }
                let record = match &result {
                    Ok(doc) => json!({"status":"ok","document":doc}),
                    Err(error) => json!({"status":"error","text":source,"error":error}),
                };
                self.bindings.push(bind_output(
                    &args.out,
                    &format!("annotations/{sha}.json"),
                    &record,
                )?);
                ensure!(
                    self.documents.insert(sha.clone(), result).is_none(),
                    "Repeated parse"
                );
            }
            println!(
                "Parsed {} unique requested texts in {} batches",
                self.documents.len(),
                self.batches
            );
        }
        Ok(())
    }
    fn get(&self, sha: &str) -> Result<&std::result::Result<Document, String>> {
        self.documents
            .get(sha)
            .context("Missing requested annotation")
    }
}

const COUNTERS: &[&str] = &[
    "sites",
    "sites_with_proposal",
    "sites_without_proposal",
    "sites_with_eligible_proposal",
    "proposals",
    "parse_errors",
    "parsed_proposals",
    "no_exact_inverse",
    "guard_rejected",
    "eligible",
    "proposals_with_exact_inverse",
    "exact_inverse_matches",
    "passed_inverse_matches",
    "forward_observed_preserved",
    "forward_changed",
    "forward_unresolved",
    "forward_boundary_rule_unrecognized",
    "reverse_observed_preserved",
    "reverse_changed",
    "reverse_unresolved",
    "reverse_boundary_rule_unrecognized",
    "inverse_proposals",
    "nonrestoring_inverse_proposals",
    "posts",
    "posts_with_sites",
    "posts_with_proposals",
    "posts_with_eligible",
    "dates",
    "dates_with_sites",
    "dates_with_proposals",
    "dates_with_eligible",
];
type Counts = BTreeMap<String, usize>;
type Forms = BTreeMap<String, Counts>;
fn empty_forms() -> Forms {
    ["period", "semicolon"]
        .into_iter()
        .map(|form| {
            (
                form.into(),
                COUNTERS.iter().map(|key| ((*key).into(), 0)).collect(),
            )
        })
        .collect()
}
fn increment(counts: &mut Counts, key: &str, value: usize) -> Result<()> {
    *counts
        .get_mut(key)
        .with_context(|| format!("Unknown counter {key}"))? += value;
    Ok(())
}
fn form_key(form: observation::ObservedForm) -> Result<String> {
    serde_json::to_value(form)?
        .as_str()
        .context("Observed form")
        .map(str::to_owned)
}
fn post_counts(
    observed: &observation::ObservationReport,
    results: &BTreeMap<String, Value>,
) -> Result<Forms> {
    let mut forms = empty_forms();
    let mut eligible_sites = BTreeSet::new();
    for opportunity in &observed.opportunities {
        let form = form_key(opportunity.observed_form)?;
        let counts = forms.get_mut(&form).context("Form counter")?;
        increment(counts, "proposals", 1)?;
        let result = results
            .get(&opportunity.id)
            .context("Missing proposal result")?;
        if result["status"] == "parse_error" {
            increment(counts, "parse_errors", 1)?;
            continue;
        }
        increment(counts, "parsed_proposals", 1)?;
        let pair = &result["pair"];
        increment(counts, text(pair, "status")?, 1)?;
        let matches = pair["inverse_matches"]
            .as_array()
            .context("Inverse matches")?;
        increment(
            counts,
            "proposals_with_exact_inverse",
            usize::from(!matches.is_empty()),
        )?;
        increment(counts, "exact_inverse_matches", matches.len())?;
        increment(
            counts,
            "inverse_proposals",
            pair["inverse_proposal_count"]
                .as_u64()
                .context("Inverse proposal count")? as usize,
        )?;
        increment(
            counts,
            "nonrestoring_inverse_proposals",
            pair["inverse_nonrestoring_count"]
                .as_u64()
                .context("Nonrestoring count")? as usize,
        )?;
        increment(
            counts,
            "passed_inverse_matches",
            pair["passed_inverse_matches"]
                .as_u64()
                .context("Passed inverse matches")? as usize,
        )?;
        increment(
            counts,
            &format!("forward_{}", text(&pair["forward_guard"], "outcome")?),
            1,
        )?;
        increment(
            counts,
            "forward_boundary_rule_unrecognized",
            usize::from(pair["forward_guard"]["licensed_boundary_rule"].is_null()),
        )?;
        for inverse in matches {
            increment(
                counts,
                &format!("reverse_{}", text(&inverse["reverse_guard"], "outcome")?),
                1,
            )?;
            increment(
                counts,
                "reverse_boundary_rule_unrecognized",
                usize::from(inverse["reverse_guard"]["licensed_boundary_rule"].is_null()),
            )?;
        }
        if pair["eligible"] == true {
            eligible_sites.insert(opportunity.site_id.clone());
        }
    }
    for site in &observed.sites {
        let form = form_key(site.observed_form)?;
        let counts = forms.get_mut(&form).context("Site form counter")?;
        increment(counts, "sites", 1)?;
        increment(
            counts,
            "sites_with_proposal",
            usize::from(!site.proposal_ids.is_empty()),
        )?;
        increment(
            counts,
            "sites_with_eligible_proposal",
            usize::from(eligible_sites.contains(&site.id)),
        )?;
    }
    for counts in forms.values_mut() {
        counts.insert(
            "sites_without_proposal".into(),
            counts["sites"] - counts["sites_with_proposal"],
        );
        counts.insert("posts".into(), 1);
        for (counter, source) in [
            ("posts_with_sites", "sites"),
            ("posts_with_proposals", "proposals"),
            ("posts_with_eligible", "eligible"),
        ] {
            counts.insert(counter.into(), usize::from(counts[source] > 0));
        }
        ensure!(
            counts["proposals"] == counts["parse_errors"] + counts["parsed_proposals"],
            "Parse coverage loss"
        );
        ensure!(
            counts["parsed_proposals"]
                == counts["eligible"] + counts["no_exact_inverse"] + counts["guard_rejected"],
            "Pair coverage loss"
        );
    }
    Ok(forms)
}
fn merge(to: &mut Forms, from: &Forms) -> Result<()> {
    for (form, values) in from {
        for (key, count) in values {
            increment(to.get_mut(form).context("Merge form")?, key, *count)?;
        }
    }
    Ok(())
}

fn rates(forms: &Forms) -> Value {
    let mut result = BTreeMap::new();
    for (form, counts) in forms {
        let mut values = BTreeMap::new();
        for (key, numerator, denominator) in [
            ("proposed_site_fraction", "sites_with_proposal", "sites"),
            (
                "eligible_site_fraction",
                "sites_with_eligible_proposal",
                "sites",
            ),
            ("parsed_proposal_fraction", "parsed_proposals", "proposals"),
            (
                "restorable_parsed_fraction",
                "proposals_with_exact_inverse",
                "parsed_proposals",
            ),
            ("eligible_proposal_fraction", "eligible", "proposals"),
        ] {
            let n = counts[numerator];
            let d = counts[denominator];
            values.insert(
                key,
                json!({"numerator":n,"denominator":d,"fraction":(d>0).then(||n as f64/d as f64)}),
            );
        }
        result.insert(form, values);
    }
    json!(result)
}

fn request_text(requested: &mut BTreeMap<String, String>, value: &str) -> Result<()> {
    if let Some(previous) = requested.insert(hash(value.as_bytes()), value.into()) {
        ensure!(previous == value, "Text digest collision");
    }
    Ok(())
}

struct Source {
    binding: Value,
    document: Document,
    observed: observation::ObservationReport,
}

fn run(args: &Args, p: &Value) -> Result<()> {
    let started = Instant::now();
    for folder in ["annotations", "source-observations", "pair-results"] {
        fs::create_dir(args.out.join(folder))?;
    }
    bound(&args.repo, &p["predecessor_stage"])?;
    bound(&args.repo, &p["training_metadata_receipt"])?;
    bound(&args.repo, &p["study_plan"])?;
    let metadata = bound(&args.repo, &p["training_metadata"])?;
    let training = metadata.as_array().context("TRAIN metadata array")?;
    ensure!(training.len() == 3889, "TRAIN count differs");
    let fixture = bound(&args.repo, &p["fixture_manifest"])?;
    let cases = fixture["cases"].as_array().context("Synthetic cases")?;
    ensure!(cases.len() == 24, "Fixture scope differs");
    let mut artifacts = Vec::new();
    let mut sources = Vec::new();
    let mut ids = BTreeSet::new();
    let mut source_hashes = BTreeSet::new();
    let mut authors = BTreeMap::<String, (String, Forms)>::new();
    let mut panels = BTreeMap::<String, Forms>::new();
    let mut requested = BTreeMap::new();
    let mut proposed = 0;
    for binding in training {
        let post = &binding["post"];
        let author = text(post, "author")?;
        let panel = text(binding, "panel")?;
        let date = text(post, "date")?;
        grammar_eval::validate_date(date)?;
        ensure!(
            post["split"] == "train"
                && post["source_group"] == format!("blog:{author}:date:{date}"),
            "TRAIN metadata differs"
        );
        ensure!(
            ids.insert(text(post, "id")?.to_owned())
                && source_hashes.insert(text(post, "text_sha256")?.to_owned()),
            "Repeated TRAIN source"
        );
        let bytes = checked(
            &args.repo.join(text(binding, "annotation_path")?),
            text(binding, "annotation_sha256")?,
        )?;
        let doc: Document = serde_json::from_slice(&bytes)?;
        ensure!(
            doc.parser_identity == p["parser_identity"]
                && hash(doc.text.as_bytes()) == post["text_sha256"],
            "Cached source identity differs"
        );
        let observed = observation::observe(&doc)?;
        for opportunity in &observed.opportunities {
            request_text(&mut requested, &opportunity.counterpart.text)?;
        }
        proposed += observed.opportunities.len();
        artifacts.push(bind_output(
            &args.out,
            &format!("source-observations/{}.json", text(post, "text_sha256")?),
            &json!({"binding":binding,"observation":observed}),
        )?);
        let target = authors
            .entry(author.into())
            .or_insert_with(|| (panel.into(), empty_forms()));
        ensure!(target.0 == panel, "Author panel differs");
        panels.entry(panel.into()).or_insert_with(empty_forms);
        sources.push(Source {
            binding: binding.clone(),
            document: doc,
            observed,
        });
        if sources.len() % 500 == 0 {
            println!(
                "Enumerated {} TRAIN posts and {} proposals",
                sources.len(),
                proposed
            );
        }
    }
    ensure!(authors.len() == 300, "Author count differs");
    let panel_posts: BTreeMap<String, usize> = panels
        .keys()
        .map(|panel| {
            (
                panel.clone(),
                sources
                    .iter()
                    .filter(|s| s.binding["panel"] == *panel)
                    .count(),
            )
        })
        .collect();
    ensure!(
        serde_json::to_value(&panel_posts)? == p["expected_panel_posts"],
        "Panel sizes differ"
    );
    artifacts.push(bind_output(&args.out, "source-enumeration.json", &json!({"posts":sources.len(),"authors":authors.len(),"panel_posts":panel_posts,"proposals":proposed,"unique_counterpart_texts":requested.len()}))?);

    // Parse both fixture forms even when the source rule abstains, preserving
    // desired-pair guard observations separately from rule eligibility.
    for case in cases {
        for key in ["source_text", "candidate_text"] {
            let input = text(case, key)?;
            request_text(&mut requested, input)?;
        }
    }
    let mut annotations = Annotations::default();
    annotations.parse(requested, args, p)?;
    let mut pooled = empty_forms();
    let mut post_rows = Vec::new();
    let mut date_rows = BTreeMap::<(String, String, String), Forms>::new();
    for source in &sources {
        let mut results = BTreeMap::new();
        for opportunity in &source.observed.opportunities {
            let candidate_sha = hash(opportunity.counterpart.text.as_bytes());
            let result = match annotations.get(&candidate_sha)? {
                Ok(candidate) => {
                    json!({"status":"evaluated","pair":observation::evaluate_counterpart(&source.document, opportunity, candidate)?})
                }
                Err(error) => json!({"status":"parse_error","error":error}),
            };
            let name = format!("pair-results/{}.json", opportunity.id);
            artifacts.push(bind_output(&args.out, &name, &json!({"source_binding":source.binding,"opportunity_id":opportunity.id,"candidate_sha256":candidate_sha,"result":result}))?);
            ensure!(
                results.insert(opportunity.id.clone(), result).is_none(),
                "Repeated opportunity ID"
            );
        }
        let counts = post_counts(&source.observed, &results)?;
        merge(&mut pooled, &counts)?;
        let panel = text(&source.binding, "panel")?;
        let author = text(&source.binding["post"], "author")?;
        merge(panels.get_mut(panel).unwrap(), &counts)?;
        merge(&mut authors.get_mut(author).unwrap().1, &counts)?;
        let date = text(&source.binding["post"], "date")?;
        merge(
            date_rows
                .entry((author.into(), panel.into(), date.into()))
                .or_insert_with(empty_forms),
            &counts,
        )?;
        post_rows.push(json!({"binding":source.binding,"forms":counts}));
    }
    for ((author, panel, _), forms) in &mut date_rows {
        for (form, counts) in forms {
            for (key, value) in [
                ("dates", 1),
                ("dates_with_sites", usize::from(counts["sites"] > 0)),
                ("dates_with_proposals", usize::from(counts["proposals"] > 0)),
                ("dates_with_eligible", usize::from(counts["eligible"] > 0)),
            ] {
                increment(counts, key, value)?;
                increment(pooled.get_mut(form).unwrap(), key, value)?;
                increment(
                    panels.get_mut(panel).unwrap().get_mut(form).unwrap(),
                    key,
                    value,
                )?;
                increment(
                    authors.get_mut(author).unwrap().1.get_mut(form).unwrap(),
                    key,
                    value,
                )?;
            }
        }
    }
    artifacts.push(bind_output(&args.out, "date-coverage.json", &json!(date_rows.iter().map(|((author,panel,date),forms)|json!({"author":author,"panel":panel,"date":date,"forms":forms,"rates":rates(forms)})).collect::<Vec<_>>()))?);
    artifacts.push(bind_output(
        &args.out,
        "post-coverage.json",
        &json!(post_rows),
    )?);
    artifacts.push(bind_output(&args.out, "author-coverage.json", &json!(authors.iter().map(|(author,(panel,forms))|json!({"author":author,"panel":panel,"forms":forms,"rates":rates(forms)})).collect::<Vec<_>>()))?);
    let author_coverage: BTreeMap<String, Value> = ["period", "semicolon"].into_iter().map(|form| {
        let coverage = json!({"authors":authors.len(),"with_sites":authors.values().filter(|(_,f)| f[form]["sites"]>0).count(),"with_proposals":authors.values().filter(|(_,f)| f[form]["proposals"]>0).count(),"with_eligible":authors.values().filter(|(_,f)| f[form]["eligible"]>0).count()});
        (form.into(), coverage)
    }).collect();

    let mut synthetic = Vec::new();
    for case in cases {
        let source_text = text(case, "source_text")?;
        let candidate_text = text(case, "candidate_text")?;
        let patches: Vec<edits::Edit> = serde_json::from_value(case["edits"].clone())?;
        ensure!(
            edits::apply(source_text, &patches)? == candidate_text,
            "Fixture patch differs"
        );
        let source = annotations.get(&hash(source_text.as_bytes()))?;
        let candidate = annotations.get(&hash(candidate_text.as_bytes()))?;
        match (source, candidate) {
            (Ok(source), Ok(candidate)) => {
                let observed = observation::observe(source)?;
                let mut exact = Vec::new();
                for opportunity in &observed.opportunities {
                    if opportunity.counterpart.text == candidate_text {
                        exact.push(observation::evaluate_counterpart(source, opportunity, candidate)?);
                    }
                }
                let expected = &case["expected_rule_candidate"];
                synthetic.push(json!({"case":case,"status":"evaluated","observation":observed,"exact_candidate_emitted":!exact.is_empty(),"rule_expectation_met":expected.as_bool().map(|value| value != exact.is_empty()),"exact_pairs":exact,"desired_pair_guard":crate::claim_guard::compare(source,candidate,&patches)?}));
            }
            _ => synthetic.push(json!({"case":case,"status":"parse_error","source_error":source.as_ref().err(),"candidate_error":candidate.as_ref().err()})),
        }
    }
    artifacts.push(bind_output(&args.out, "synthetic.json", &json!(synthetic))?);
    let report = json!({
        "schema":"slopninja-boundary-eligibility-v1","posts":sources.len(),"authors":authors.len(),
        "panel_posts":panel_posts,"pooled":pooled,"panels":panels,"author_coverage":author_coverage,
        "rates":{"pooled":rates(&pooled),"panels":panels.iter().map(|(panel,forms)|(panel,rates(forms))).collect::<BTreeMap<_,_>>()},
        "synthetic_coverage":{"directional_cases":synthetic.len(),"unique_pairs":cases.iter().map(|case| text(case,"pair_id")).collect::<Result<BTreeSet<_>>>()?.len(),"parsed_cases":synthetic.iter().filter(|row|row["status"]=="evaluated").count(),"exact_candidate_cases":synthetic.iter().filter(|row|row["exact_candidate_emitted"]==true).count(),"eligible_cases":synthetic.iter().filter(|row|row["exact_pairs"].as_array().is_some_and(|pairs|pairs.iter().any(|p|p["eligible"]==true))).count()},
        "parser_unique_texts":annotations.documents.len(),"parser_batches":annotations.batches,
        "parser_error_texts":annotations.documents.values().filter(|result|result.is_err()).count(),
        "annotations":annotations.bindings,"artifacts":artifacts,
        "guard_internal_candidate_recognition_limit":256,
        "elapsed_seconds":started.elapsed().as_secs_f64(),"llm_calls":0,"detector_calls":0,
        "preference_estimation":false,"neural_fits":0,"query_posts":0,"semantic_equivalence_certified":false,
        "interpretation":"TRAIN-only eligibility and parser/guard compatibility; asymmetric rejection is not author preference, semantic/discourse equivalence, readability or detector evidence"
    });
    let report_sha = fresh(&args.out.join("report.json"), &report)?;
    fresh(
        &args.out.join("receipt.json"),
        &json!({"schema":"slopninja-boundary-eligibility-receipt-v1","protocol_sha256":args.expected_protocol_sha256,"report_sha256":report_sha,"sources":p["source_files"],"executable_sha256":hash(&fs::read(std::env::current_exe()?)?)}),
    )?;
    println!(
        "Completed boundary eligibility on {} TRAIN posts",
        sources.len()
    );
    Ok(())
}

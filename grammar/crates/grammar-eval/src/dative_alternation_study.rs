//! Frozen local evaluation of dative proposals, reparsed alignment and style vectors.
use crate::{
    dative_alternation as alternation,
    dative_vector_scoring::Scoring,
    lexical_frame_observation::{self as observation, Mapping},
    lexical_frame_study as study,
    verbnet_resource::Resource,
};
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_core::{
    edits, features,
    syntax::{self, Document},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use study::{bound, bound_bytes, checked, hash, output, rows, safe_relative, text};

const BASE: &str = "data/author-corpora/blog-authorship-2004";
const VARIANTS: [&str; 4] = ["incumbent_sm", "isolated_sm", "isolated_md", "isolated_trf"];
const CONTEXT: &str = "\n\nAfter the reviewer of the draft had checked the notes, the editor put the folder on the desk beside the lamp.";
const SCHEMA: &str = "slopninja-dative-alternation-study-protocol-v1";
type Parsed = std::result::Result<Document, String>;
type Documents = BTreeMap<String, Parsed>;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Bind sources and all declared inputs before any fresh annotations or scores.
    Freeze {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    Run {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        protocol: PathBuf,
        #[arg(long)]
        expected_protocol_sha256: String,
        #[arg(long)]
        out: PathBuf,
    },
}
fn binding(repo: &Path, name: &str) -> Result<Value> {
    Ok(json!({"path":name,"sha256":hash(&fs::read(repo.join(name))?)}))
}
fn freeze(repo: &Path, out: &Path) -> Result<()> {
    let previous = binding(
        repo,
        &format!("{BASE}/lexical-frame-dative-v1/protocol-v2.json"),
    )?;
    let old = bound(repo, &previous)?;
    let mut names = rows(&old, "source_bindings")?
        .iter()
        .map(|s| text(s, "path").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    for name in [
        "dative_alternation.rs",
        "dative_vector_scoring.rs",
        "dative_alternation_study.rs",
        "bin/slop_ninja-dative-alternation.rs",
    ] {
        names.insert(format!("grammar/crates/grammar-eval/src/{name}"));
    }
    let sources = names
        .iter()
        .map(|n| binding(repo, n))
        .collect::<Result<Vec<_>>>()?;
    let protocol = json!({"schema":SCHEMA,"source_bindings":sources,
        "executable_sha256":hash(&fs::read(std::env::current_exe()?)?),
        "previous_protocol":previous,
        "previous_receipt":binding(repo,&format!("{BASE}/lexical-frame-dative-v1/run-v2/receipt.json"))?,
        "resource_manifest":old["resource_manifest"],"parsers":old["parsers"],
        "fixture":binding(repo,"grammar/fixtures/dative-alternation-v1/manifest.json")?,
        "space":binding(repo,&format!("{BASE}/vector-evaluation-v1/space.json"))?,
        "author_targets":binding(repo,&format!("{BASE}/vector-evaluation-v1/author-targets.json"))?,
        "mappings":[Mapping::BaselineV1.identity(),Mapping::PrepositionalDativeV1.identity(),Mapping::DativeAlternationV1.identity()],
        "historical_replay":{"cases":736,"old_mappings_whole_case_equal_required":2,"before_fresh_nlp":true},
        "scoring":{"parser":"incumbent_sm","authors":"first ten lexicographic original TRAIN profiles","contexts":["raw","appended"],"appended_context":CONTEXT,"missing_families":"unavailable; never fill with zero","geometry":"fixed scales and weights; categorical union support","purpose":"exploratory distances, not edit preference evidence"},
        "candidate_policy":"all generated proposals and rejected sites; unchanged source retained; no parser selection or cap",
        "training_corpus_reads":0,"model_fit_calls":0,"llm_calls":0,"detector_calls":0,
        "automatic_edit_license":false,"semantic_equivalence_certified":false});
    fs::create_dir(out)?;
    println!("{}", output(out, "protocol.json", &protocol)?);
    Ok(())
}

fn artifacts(directory: &Path, report: &Value) -> Result<BTreeMap<String, Value>> {
    let mut result = BTreeMap::new();
    for b in rows(report, "artifacts")? {
        let name = text(b, "path")?;
        safe_relative(name)?;
        bound_bytes(directory, b)?;
        ensure!(
            result.insert(name.into(), b.clone()).is_none(),
            "Duplicate historical artifact"
        );
    }
    Ok(result)
}
fn historical_report(
    repo: &Path,
    protocol: &Value,
    receipt: &Value,
) -> Result<(PathBuf, Value, BTreeMap<String, Value>)> {
    let p = bound(repo, protocol)?;
    let r = bound(repo, receipt)?;
    ensure!(
        r["status"] == "completed" && r["protocol_sha256"] == protocol["sha256"],
        "Historical receipt differs"
    );
    let file = repo.join(text(receipt, "path")?);
    let directory = file.parent().context("Receipt parent")?.to_path_buf();
    let report = bound(&directory, &r["report"])?;
    ensure!(
        report["protocol_sha256"] == r["protocol_sha256"],
        "Historical report protocol differs"
    );
    let a = artifacts(&directory, &report)?;
    for s in rows(&p, "source_bindings")? {
        let snapshot = a
            .get(&format!("executed-source/{}", text(s, "path")?))
            .context("Historical source snapshot absent")?;
        ensure!(
            snapshot["sha256"] == s["sha256"],
            "Executed historical source differs"
        );
    }
    Ok((directory, p, a))
}
fn annotation(value: &Value, source: &str, parser: &str) -> Result<Parsed> {
    match text(value, "status")? {
        "ok" => {
            let d: Document = serde_json::from_value(value["document"].clone())?;
            syntax::validate(&d)?;
            ensure!(
                d.text == source && d.parser_identity == parser,
                "Annotation identity differs"
            );
            Ok(Ok(d))
        }
        "error" => {
            ensure!(value["text"] == source, "Failed annotation text differs");
            Ok(Err(text(value, "error")?.into()))
        }
        _ => anyhow::bail!("Unknown annotation status"),
    }
}
fn inherited_v2(base: &Value) -> Value {
    let mut value = base.clone();
    if let Some(o) = value["observation"].as_object_mut() {
        o.insert(
            "schema".into(),
            json!("slopninja-lexical-frame-observation-v2"),
        );
        o.insert(
            "mapping_identity".into(),
            json!(Mapping::PrepositionalDativeV1.identity()),
        );
    }
    value
}
fn raw_ledger(row: &Value) -> Value {
    let mut value = row["observation"].clone();
    if let Some(o) = value.as_object_mut() {
        o.remove("schema");
        o.remove("mapping_identity");
    }
    if let Some(ps) = value["predicates"].as_array_mut() {
        for p in ps {
            p.as_object_mut().unwrap().remove("frame_possibilities");
        }
    }
    value
}
struct ReplayCase {
    name: String,
    comparison: Value,
    annotation: Value,
    parser: String,
}
fn replay(
    repo: &Path,
    protocol: &Value,
    out: &Path,
    resource: &Resource,
    outputs: &mut Vec<Value>,
) -> Result<BTreeSet<String>> {
    let (directory, old, old_artifacts) = historical_report(
        repo,
        &protocol["previous_protocol"],
        &protocol["previous_receipt"],
    )?;
    ensure!(
        old["resource_manifest"] == protocol["resource_manifest"],
        "Historical resource differs"
    );
    let mut origins = BTreeMap::new();
    for run in rows(&old, "saved_runs")? {
        let (dir, p, a) = historical_report(repo, &run["protocol"], &run["receipt"])?;
        ensure!(
            p["resource_manifest"] == protocol["resource_manifest"],
            "Ancestor resource differs"
        );
        let key = format!("{}/{}", text(run, "panel")?, text(run, "variant")?);
        ensure!(
            origins.insert(key, (dir, p, a)).is_none(),
            "Duplicate historical origin"
        );
    }
    let mut cases = Vec::new();
    let mut sources = BTreeSet::new();
    let mut budgets = BTreeMap::<String, usize>::new();
    for (name, b) in &old_artifacts {
        if !name.ends_with("/comparison.json") {
            continue;
        }
        let parts = name.split('/').collect::<Vec<_>>();
        ensure!(parts.len() == 4, "Unexpected historical case path");
        let key = format!("{}/{}", parts[0], parts[1]);
        let comparison = bound(&directory, b)?;
        let source = text(&comparison["case"], "text")?;
        let sha = hash(source.as_bytes());
        sources.insert(sha.clone());
        let (annotation, parser) = if parts[0] == "fresh24" {
            let a = old_artifacts
                .get(&format!("{key}/annotations/{sha}.json"))
                .context("Missing previous fresh annotation")?;
            let p = rows(&old, "parsers")?
                .iter()
                .find(|p| p["id"] == parts[1])
                .context("Missing parser")?;
            (bound(&directory, a)?, text(p, "parser_identity")?.into())
        } else {
            let (dir, p, a) = origins.get(&key).context("Missing ancestor panel")?;
            (
                bound(
                    dir,
                    a.get(&format!("annotations/{sha}.json"))
                        .context("Missing ancestor annotation")?,
                )?,
                text(&p["parser"], "parser_identity")?.into(),
            )
        };
        *budgets.entry(key).or_default() += 1;
        cases.push(ReplayCase {
            name: name.trim_end_matches("/comparison.json").into(),
            comparison,
            annotation,
            parser,
        });
    }
    ensure!(
        cases.len() == 736 && sources.len() == 184,
        "Historical coverage differs"
    );
    for variant in VARIANTS {
        for (panel, n) in [("reused96", 96), ("reused64", 64), ("fresh24", 24)] {
            ensure!(
                budgets.get(&format!("{panel}/{variant}")) == Some(&n),
                "Historical panel coverage differs"
            );
        }
    }
    // This barrier checks both old versions on every saved case before v3 or fresh NLP.
    for saved in &cases {
        let base = bound(&directory, &saved.comparison["baseline_case"])?;
        let ext = if saved.comparison["extended_case"].is_null() {
            inherited_v2(&base)
        } else {
            bound(&directory, &saved.comparison["extended_case"])?
        };
        let case = &saved.comparison["case"];
        let parsed = annotation(&saved.annotation, text(case, "text")?, &saved.parser)?;
        ensure!(
            study::evaluate_case(case, &parsed, resource, Mapping::BaselineV1)? == base,
            "Baseline v1 replay differs: {}",
            saved.name
        );
        ensure!(
            study::evaluate_case(case, &parsed, resource, Mapping::PrepositionalDativeV1)? == ext,
            "Prepositional v2 replay differs: {}",
            saved.name
        );
    }
    outputs.push(output(out,"historical-replay.json",&json!({"cases":cases.len(),"mapping_case_comparisons":cases.len()*2,"old_mapping_whole_case_equal":true,"unique_texts":sources.len(),"parser_calls":0,"budgets":budgets}))?);
    println!("Both historical mappings reproduce all 736 saved cases exactly.");
    let mut summaries = BTreeMap::<String, Vec<Value>>::new();
    for saved in &cases {
        let base = bound(&directory, &saved.comparison["baseline_case"])?;
        let ext = if saved.comparison["extended_case"].is_null() {
            inherited_v2(&base)
        } else {
            bound(&directory, &saved.comparison["extended_case"])?
        };
        let case = &saved.comparison["case"];
        let parsed = annotation(&saved.annotation, text(case, "text")?, &saved.parser)?;
        let current = study::evaluate_case(case, &parsed, resource, Mapping::DativeAlternationV1)?;
        ensure!(
            raw_ledger(&current) == raw_ledger(&ext),
            "V3 altered raw ledger"
        );
        let mut version_only = ext.clone();
        if let Some(o) = version_only["observation"].as_object_mut() {
            o.insert(
                "schema".into(),
                json!("slopninja-lexical-frame-observation-v3"),
            );
            o.insert(
                "mapping_identity".into(),
                json!(Mapping::DativeAlternationV1.identity()),
            );
        }
        let changed = current != version_only;
        let current_binding = if changed {
            let b = output(out, &format!("historical/{}/v3.json", saved.name), &current)?;
            outputs.push(b.clone());
            b
        } else {
            Value::Null
        };
        let verdicts=rows(&ext,"expectations")?.iter().zip(rows(&current,"expectations")?).map(|(a,b)|json!({"expectation":a["expectation"],"previous":a["status"],"current":b["status"]})).collect::<Vec<_>>();
        ensure!(
            rows(&ext, "expectations")?.len() == rows(&current, "expectations")?.len(),
            "Expectation count differs"
        );
        let result = json!({"case":saved.name,"changed_beyond_version":changed,"previous_comparison":old_artifacts[&format!("{}/comparison.json",saved.name)],"current_case":current_binding,"unchanged_storage":"When current_case is null, inherit previous v2 row body and set v3 schema/mapping identity","raw_ledger_equal":true,"expectations":verdicts,"previous_targets":ext["target_predicates"],"current_targets":current["target_predicates"]});
        let group = saved.name.rsplit_once('/').unwrap().0.to_owned();
        summaries.entry(group).or_default().push(result);
    }
    for (group, values) in summaries {
        outputs.push(output(out,&format!("historical/{group}/comparison.json"),&json!({"cases":values.len(),"changed_cases":values.iter().filter(|v|v["changed_beyond_version"]==true).count(),"results":values}))?);
    }
    Ok(sources)
}

fn word_bag(source: &str) -> Vec<String> {
    let mut w = features::words(source);
    w.sort();
    w
}
fn span_valid(source: &str, value: &Value) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    let [a, b]: [usize; 2] = serde_json::from_value(value.clone())?;
    ensure!(
        a < b && source.get(a..b).is_some(),
        "Invalid fixture byte anchor"
    );
    Ok(())
}
fn validate_fixture(fixture: &Value, old: &BTreeSet<String>) -> Result<BTreeMap<String, String>> {
    ensure!(
        fixture["schema"] == "slopninja-dative-alternation-fixtures-v1",
        "Unknown fresh fixture schema"
    );
    let cases = rows(fixture, "cases")?;
    ensure!(cases.len() == 24, "Fresh case budget differs");
    let mut ids = BTreeSet::new();
    let mut texts = BTreeMap::new();
    let mut allocation = [0; 3];
    for c in cases {
        let id = text(c, "id")?;
        ensure!(
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                && ids.insert(id),
            "Unsafe or duplicate case ID"
        );
        let source = text(c, "source_text")?;
        let sha = hash(source.as_bytes());
        ensure!(
            c["source_sha256"] == sha && !old.contains(&sha),
            "Fresh source hash/reuse differs"
        );
        ensure!(
            texts.insert(sha, source.into()).is_none(),
            "Duplicate fresh text"
        );
        for v in c["source_anchors"]
            .as_object()
            .context("Source anchors")?
            .values()
        {
            span_valid(source, v)?;
        }
        match c["expected_proposal"].as_bool() {
            Some(true) => {
                allocation[0] += 1;
                let candidate = text(c, "candidate_text")?;
                ensure!(
                    c["candidate_sha256"] == hash(candidate.as_bytes()),
                    "Candidate digest differs"
                );
                let patch: Vec<edits::Edit> = serde_json::from_value(c["expected_edits"].clone())?;
                let inverse: Vec<edits::Edit> =
                    serde_json::from_value(c["expected_inverse_edits"].clone())?;
                ensure!(
                    patch.len() == 1
                        && inverse.len() == 1
                        && edits::apply(source, &patch)? == candidate
                        && edits::apply(candidate, &inverse)? == source,
                    "Fixture forward/inverse differs"
                );
                for v in c["candidate_anchors"]
                    .as_object()
                    .context("Candidate anchors")?
                    .values()
                {
                    span_valid(candidate, v)?;
                }
                let a = word_bag(source);
                let b = word_bag(candidate);
                ensure!(
                    c["production_word_delta"]["source_words"] == json!(a)
                        && c["production_word_delta"]["candidate_words"] == json!(b),
                    "Production word bag differs"
                );
                let (mut longer, shorter) = if a.len() > b.len() { (a, b) } else { (b, a) };
                let index = longer
                    .iter()
                    .position(|w| w == "to")
                    .context("No to in alternation")?;
                longer.remove(index);
                ensure!(longer == shorter, "Alternation changes words beyond to");
                let reciprocal = cases
                    .iter()
                    .find(|r| r["id"] == c["reciprocal_case_id"])
                    .context("No reciprocal case")?;
                ensure!(
                    reciprocal["source_text"] == candidate
                        && reciprocal["candidate_text"] == source
                        && reciprocal["reciprocal_case_id"] == id,
                    "Reciprocal fixture differs"
                );
            }
            Some(false) => allocation[1] += 1,
            None => {
                ensure!(
                    c["expected_proposal"].is_null(),
                    "Malformed expected proposal"
                );
                allocation[2] += 1;
            }
        }
    }
    ensure!(
        allocation == [12, 11, 1],
        "Fresh fixture allocation differs"
    );
    Ok(texts)
}

fn parse_missing(
    repo: &Path,
    out: &Path,
    parser: &Value,
    texts: &BTreeMap<String, String>,
    docs: &mut Documents,
    outputs: &mut Vec<Value>,
) -> Result<usize> {
    let missing = texts
        .iter()
        .filter(|(sha, _)| !docs.contains_key(*sha))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(0);
    }
    study::verify_parser(repo, parser)?;
    let values = missing.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>();
    let parsed = grammar_spacy::parse_batch(
        &values,
        &repo
            .join(text(&parser["parser_python"], "path")?)
            .to_string_lossy(),
    )?;
    ensure!(parsed.len() == missing.len(), "Parser output count differs");
    let count = parsed.len();
    for ((sha, source), doc) in missing.into_iter().zip(parsed) {
        let value = match doc {
            Ok(d) => json!({"status":"ok","document":d}),
            Err(e) => json!({"status":"error","text":source,"error":e}),
        };
        let parsed = annotation(&value, source, text(parser, "parser_identity")?)?;
        outputs.push(output(
            out,
            &format!("annotations/{}/{sha}.json", text(parser, "id")?),
            &value,
        )?);
        docs.insert(sha.clone(), parsed);
    }
    study::verify_parser(repo, parser)?;
    Ok(count)
}

fn proposal_expectation(
    case: &Value,
    doc: &Document,
    report: &alternation::ProposalReport,
) -> Result<Value> {
    let target: [usize; 2] =
        serde_json::from_value(case["source_anchors"]["predicate_span"].clone())?;
    let target_proposals = report
        .proposals
        .iter()
        .filter(|p| {
            let t = &doc.tokens[p.predicate_index];
            [t.start_byte, t.end_byte] == target
        })
        .collect::<Vec<_>>();
    let all_expected=target_proposals.iter().map(|p|{
        let role_match=[("agent",&p.roles.agent),("theme",&p.roles.theme),("recipient",&p.roles.recipient)].into_iter().all(|(name,role)|{
            let t=&doc.tokens[role.head_index];case["source_anchors"][format!("{name}_head_span")]==json!([t.start_byte,t.end_byte]) && case["source_anchors"][format!("{name}_np_span")]==json!(role.span)
        });
        json!({"proposal_id":p.id,"matches_declared_counterpart":p.candidate.text==case["candidate_text"].as_str().unwrap_or(""),"matches_declared_edits":json!(p.candidate.edits)==case["expected_edits"],"matches_declared_inverse":json!(p.inverse_edits)==case["expected_inverse_edits"],"matches_declared_roles":role_match,"matches_declared_resource":p.member_class_id==case["resource_expectation"]["member_class"].as_str().unwrap_or("") && p.source_frame_id==case["resource_expectation"]["source_frame_id"].as_str().unwrap_or("") && p.target_frame_id==case["resource_expectation"]["target_frame_id"].as_str().unwrap_or("")})
    }).collect::<Vec<_>>();
    let exact = all_expected
        .iter()
        .filter(|v| {
            [
                "matches_declared_counterpart",
                "matches_declared_edits",
                "matches_declared_inverse",
                "matches_declared_roles",
                "matches_declared_resource",
            ]
            .into_iter()
            .all(|k| v[k] == true)
        })
        .count();
    let met = match case["expected_proposal"].as_bool() {
        Some(true) => Some(exact > 0 && exact == report.proposals.len()),
        Some(false) => Some(report.proposals.is_empty()),
        None => None,
    };
    Ok(
        json!({"expected_proposal":case["expected_proposal"],"met":met,"target_proposals":target_proposals.len(),"all_proposals":report.proposals.len(),"exact_declared_proposals":exact,"details":all_expected}),
    )
}

struct Work {
    variant: String,
    case: Value,
    report: alternation::ProposalReport,
}

fn candidate_role_expectation(case: &Value, doc: &Document, alignment: &Value) -> Result<Value> {
    if case["expected_proposal"] != true || case["candidate_text"] != doc.text {
        return Ok(json!({"declared":false,"met":null}));
    }
    let Some(roles) = alignment["candidate_roles"].as_object() else {
        return Ok(json!({"declared":true,"met":null,"reason":"No complete reparsed role set"}));
    };
    let mut results = BTreeMap::new();
    for name in ["agent", "theme", "recipient"] {
        let role = roles.get(name).context("Missing aligned candidate role")?;
        let i = role["head_index"]
            .as_u64()
            .context("Missing candidate role head")? as usize;
        let t = doc
            .tokens
            .get(i)
            .context("Candidate role head out of range")?;
        let head = json!([t.start_byte, t.end_byte]);
        let met = head == case["candidate_anchors"][format!("{name}_head_span")]
            && role["span"] == case["candidate_anchors"][format!("{name}_np_span")];
        results.insert(
            name,
            json!({"met":met,"observed_head_span":head,"observed_np_span":role["span"]}),
        );
    }
    Ok(json!({"declared":true,"met":results.values().all(|r|r["met"]==true),"roles":results}))
}
fn execute(repo: &Path, protocol: &Value, out: &Path, outputs: &mut Vec<Value>) -> Result<Value> {
    let resource = study::resource(repo, &bound(repo, &protocol["resource_manifest"])?)?;
    bound_bytes(repo, &protocol["space"])?;
    bound_bytes(repo, &protocol["author_targets"])?;
    let scoring = Scoring::load(
        &repo.join(text(&protocol["space"], "path")?),
        &repo.join(text(&protocol["author_targets"], "path")?),
    )?;
    outputs.push(output(out, "vectors/target-audit.json", &scoring.audit())?);
    let old = replay(repo, protocol, out, &resource, outputs)?;
    let fixture = bound(repo, &protocol["fixture"])?;
    let sources = validate_fixture(&fixture, &old)?;
    let parsers = rows(protocol, "parsers")?;
    ensure!(
        parsers
            .iter()
            .map(|p| text(p, "id"))
            .collect::<Result<Vec<_>>>()?
            == VARIANTS,
        "Parser catalog differs"
    );
    let mut documents = BTreeMap::<String, Documents>::new();
    let mut work = Vec::new();
    let mut errors = Vec::new();
    let mut annotations = 0;
    let mut all_texts = sources.clone();
    for parser in parsers {
        let id = text(parser, "id")?;
        let mut docs = Documents::new();
        annotations += parse_missing(repo, out, parser, &sources, &mut docs, outputs)?;
        for case in rows(&fixture, "cases")? {
            let sha = text(case, "source_sha256")?;
            match &docs[sha] {
                Ok(doc)=>match alternation::propose(doc,&resource) {
                    Ok(report)=>{
                        let observed=observation::observe_with_mapping(doc,&resource,&BTreeSet::new(),Mapping::DativeAlternationV1)?;
                        outputs.push(output(out,&format!("fresh/{id}/{}/source.json",text(case,"id")?),&json!({"case":case,"annotation_sha256":sha,"observation":observed,"proposal_report":report,"expectation":proposal_expectation(case,doc,&report)?}))?);
                        for p in &report.proposals {all_texts.insert(hash(p.candidate.text.as_bytes()),p.candidate.text.clone());}
                        work.push(Work{variant:id.into(),case:case.clone(),report});
                    },Err(e)=>errors.push(json!({"variant":id,"case":case["id"],"status":"proposal_error","error":format!("{e:#}")})),
                },Err(e)=>errors.push(json!({"variant":id,"case":case["id"],"status":"parse_error","error":e})),
            }
        }
        documents.insert(id.into(), docs);
        println!("Generated all source proposals for {id}.");
    }
    let mut alignments = Vec::new();
    let mut scoring_pairs = BTreeMap::<(String, String), Vec<Value>>::new();
    for parser in parsers {
        let id = text(parser, "id")?;
        let docs = documents.get_mut(id).unwrap();
        let needed = work
            .iter()
            .filter(|w| w.variant == id)
            .flat_map(|w| w.report.proposals.iter())
            .map(|p| (hash(p.candidate.text.as_bytes()), p.candidate.text.clone()))
            .collect();
        annotations += parse_missing(repo, out, parser, &needed, docs, outputs)?;
        for w in work.iter().filter(|w| w.variant == id) {
            let source_sha = text(&w.case, "source_sha256")?;
            let source = docs[source_sha]
                .as_ref()
                .map_err(|e| anyhow::anyhow!(e.clone()))?;
            for proposal in &w.report.proposals {
                let candidate_sha = hash(proposal.candidate.text.as_bytes());
                let mut alignment = match &docs[&candidate_sha] {
                    Ok(candidate) => {
                        match alternation::compare(source, candidate, proposal, &resource) {
                            Ok(r) => serde_json::to_value(r)?,
                            Err(e) => json!({"outcome":"unresolved","error":format!("{e:#}")}),
                        }
                    }
                    Err(e) => json!({"outcome":"unresolved","parse_error":e}),
                };
                alignment["candidate_role_expectation"] = match &docs[&candidate_sha] {
                    Ok(candidate) => candidate_role_expectation(&w.case, candidate, &alignment)?,
                    Err(_) => {
                        json!({"declared":w.case["expected_proposal"]==true,"met":null,"reason":"Candidate parse unavailable"})
                    }
                };
                let b = output(
                    out,
                    &format!(
                        "fresh/{id}/{}/alignment-{}.json",
                        text(&w.case, "id")?,
                        proposal.id
                    ),
                    &alignment,
                )?;
                outputs.push(b.clone());
                let row = json!({"variant":id,"case":w.case["id"],"proposal_id":proposal.id,"direction":proposal.direction,"source_sha256":source_sha,"candidate_sha256":candidate_sha,"alignment":alignment["outcome"],"candidate_role_expectation":alignment["candidate_role_expectation"],"report":b});
                scoring_pairs
                    .entry((source_sha.into(), candidate_sha))
                    .or_default()
                    .push(row.clone());
                alignments.push(row);
            }
        }
    }
    // Candidate availability in another parser never changes the parser used for geometry.
    let incumbent = &parsers[0];
    let docs = documents.get_mut("incumbent_sm").unwrap();
    annotations += parse_missing(repo, out, incumbent, &all_texts, docs, outputs)?;
    let contextual = all_texts
        .values()
        .map(|s| {
            let s = format!("{s}{CONTEXT}");
            (hash(s.as_bytes()), s)
        })
        .collect();
    annotations += parse_missing(repo, out, incumbent, &contextual, docs, outputs)?;
    let mut scores = Vec::new();
    for ((source_sha, candidate_sha), origins) in scoring_pairs {
        for context in ["raw", "appended"] {
            let (a, b) = if context == "raw" {
                (source_sha.clone(), candidate_sha.clone())
            } else {
                (
                    hash(format!("{}{CONTEXT}", all_texts[&source_sha]).as_bytes()),
                    hash(format!("{}{CONTEXT}", all_texts[&candidate_sha]).as_bytes()),
                )
            };
            let score = match (&docs[&a], &docs[&b]) {
                (Ok(source), Ok(candidate)) => match scoring.score_pair(source, candidate) {
                    Ok(s) => s,
                    Err(e) => json!({"status":"scoring_error","error":format!("{e:#}")}),
                },
                (source, candidate) => {
                    json!({"status":"parse_error","source_error":source.as_ref().err(),"candidate_error":candidate.as_ref().err()})
                }
            };
            let b = output(
                out,
                &format!("vectors/{context}/{source_sha}--{candidate_sha}.json"),
                &json!({"context":context,"source_sha256":a,"candidate_sha256":b,"proposal_origins":origins,"score":score}),
            )?;
            outputs.push(b.clone());
            scores.push(b);
        }
    }
    let mut summaries = Vec::new();
    for variant in VARIANTS {
        let these = work
            .iter()
            .filter(|w| w.variant == variant)
            .collect::<Vec<_>>();
        let mut expectations = Vec::new();
        for w in &these {
            let d = documents[variant][text(&w.case, "source_sha256")?]
                .as_ref()
                .unwrap();
            expectations.push(json!({"case":w.case["id"],"group":w.case["group"],"result":proposal_expectation(&w.case,d,&w.report)?}));
        }
        let this_align = alignments
            .iter()
            .filter(|a| a["variant"] == variant)
            .collect::<Vec<_>>();
        let mut outcomes = BTreeMap::<String, usize>::new();
        for a in &this_align {
            *outcomes.entry(text(a, "alignment")?.into()).or_default() += 1;
        }
        let summary = json!({"variant":variant,"evaluated_sources":these.len(),"sources_with_proposals":these.iter().filter(|w|!w.report.proposals.is_empty()).count(),"proposals":this_align.len(),"alignment_outcomes":outcomes,"expectations":expectations});
        outputs.push(output(
            out,
            &format!("fresh/{variant}/summary.json"),
            &summary,
        )?);
        summaries.push(summary);
    }
    outputs.push(output(out, "errors.json", &json!(errors))?);
    Ok(
        json!({"schema":"slopninja-dative-alternation-study-report-v1","historical_replay_cases":736,"old_mapping_case_comparisons":1472,"fresh_sources":24,"new_annotations":annotations,"summaries":summaries,"alignments":alignments,"vector_reports":scores,"errors":errors,"automatic_edit_license":false,"semantic_equivalence_certified":false,"human_semantic_ratings":0,"detector_calls":0,"llm_calls":0,"model_fit_calls":0,"training_corpus_reads":0}),
    )
}

fn run(repo: &Path, file: &Path, expected: &str, out: &Path) -> Result<()> {
    let p: Value = serde_json::from_slice(&checked(file, expected)?)?;
    ensure!(
        p["schema"] == SCHEMA && p["scoring"]["appended_context"] == CONTEXT,
        "Unknown protocol/context"
    );
    ensure!(
        p["mappings"]
            == json!([
                Mapping::BaselineV1.identity(),
                Mapping::PrepositionalDativeV1.identity(),
                Mapping::DativeAlternationV1.identity()
            ]),
        "Mapping protocol differs"
    );
    ensure!(
        p["executable_sha256"] == hash(&fs::read(std::env::current_exe()?)?),
        "Study executable differs"
    );
    for b in rows(&p, "source_bindings")? {
        safe_relative(text(b, "path")?)?;
        bound_bytes(repo, b)?;
    }
    fs::create_dir(out)?;
    let result = (|| -> Result<()> {
        let mut outputs = vec![output(out, "protocol.json", &p)?];
        for b in rows(&p, "source_bindings")? {
            let name = format!("executed-source/{}", text(b, "path")?);
            let file = out.join(&name);
            fs::create_dir_all(file.parent().unwrap())?;
            let bytes = bound_bytes(repo, b)?;
            fs::write(&file, &bytes)?;
            outputs.push(json!({"path":name,"sha256":hash(&bytes)}));
        }
        let mut report = execute(repo, &p, out, &mut outputs)?;
        report["protocol_sha256"] = json!(expected);
        report["artifacts"] = json!(outputs);
        let b = output(out, "report.json", &report)?;
        output(
            out,
            "receipt.json",
            &json!({"status":"completed","schema":"slopninja-dative-alternation-study-receipt-v1","protocol_sha256":expected,"report":b}),
        )?;
        Ok(())
    })();
    if let Err(e) = &result {
        output(
            out,
            "failure.json",
            &json!({"status":"failed","error":format!("{e:#}"),"protocol_sha256":expected}),
        )?;
    }
    result
}
pub fn main() -> Result<()> {
    match Args::parse().command {
        Command::Freeze { repo, out } => freeze(&repo, &out),
        Command::Run {
            repo,
            protocol,
            expected_protocol_sha256,
            out,
        } => run(&repo, &protocol, &expected_protocol_sha256, &out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inherited_version_changes_only_present_observation_metadata() {
        let row = json!({"observation":{"schema":"v1","predicates":[],"raw":"kept"},"expectations":[{"status":"met"}]});
        let v2 = inherited_v2(&row);
        assert_eq!(v2["observation"]["raw"], "kept");
        assert_eq!(v2["expectations"], row["expectations"]);
        let absent = json!({"observation":null,"status":"parse_error"});
        assert_eq!(inherited_v2(&absent), absent);
    }
    #[test]
    fn source_anchors_require_utf8_boundaries() {
        assert!(span_valid("café", &json!([0, 5])).is_ok());
        assert!(span_valid("café", &json!([0, 4])).is_err());
        assert!(span_valid("x", &json!([1, 1])).is_err());
    }
    #[test]
    fn apostrophes_remain_in_production_word_bags() {
        assert_ne!(
            word_bag("Mira lends Mira's compass to Theo."),
            word_bag("Mira lends Theo's compass to Mira.")
        );
    }

    #[test]
    fn frozen_fixture_rejects_a_false_word_delta_or_nonreciprocal_pair() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../fixtures/dative-alternation-v1/manifest.json"
        ))
        .unwrap();
        assert_eq!(
            validate_fixture(&fixture, &BTreeSet::new()).unwrap().len(),
            24
        );
        let mut bad_words = fixture.clone();
        bad_words["cases"][0]["production_word_delta"]["source_words"] = json!([]);
        assert!(validate_fixture(&bad_words, &BTreeSet::new()).is_err());
        let mut bad_pair = fixture;
        bad_pair["cases"][0]["reciprocal_case_id"] = json!("control-missing_object");
        assert!(validate_fixture(&bad_pair, &BTreeSet::new()).is_err());
    }
}

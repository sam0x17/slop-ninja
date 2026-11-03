//! Matched sentence-context diagnostic; existing whole-post eligibility is unchanged.
use crate::{
    dative_alternation::{self as alternation, ArgumentPolicy},
    parser_repeat_observation as repeat, sentence_context_comparison as comparison,
    sentence_context_projection as projection,
    verbnet_resource::{Resource, XmlSource},
};
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_core::{
    edits,
    syntax::{self, Document},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    time::Instant,
};

const PREVIOUS: &str = "data/author-corpora/blog-authorship-2004/parser-repeat-v1";
const SCHEMA: &str = "slopninja-sentence-context-protocol-v1";
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
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
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("Missing string {k}"))
}
fn rows<'a>(v: &'a Value, k: &str) -> Result<&'a Vec<Value>> {
    v[k].as_array()
        .with_context(|| format!("Missing array {k}"))
}
fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}
fn safe(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && Path::new(name)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "Expected normal relative path"
    );
    Ok(())
}
fn checked(file: &Path, expected: &str) -> Result<Vec<u8>> {
    ensure!(
        expected.len() == 64
            && expected
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "Invalid SHA256"
    );
    let bytes = fs::read(file).with_context(|| format!("Read {}", file.display()))?;
    ensure!(hash(&bytes) == expected, "Hash differs: {}", file.display());
    Ok(bytes)
}
fn bytes(repo: &Path, b: &Value) -> Result<Vec<u8>> {
    checked(&repo.join(text(b, "path")?), text(b, "sha256")?)
}
fn bound(repo: &Path, b: &Value) -> Result<Value> {
    Ok(serde_json::from_slice(&bytes(repo, b)?)?)
}
fn binding(repo: &Path, name: &str) -> Result<Value> {
    safe(name)?;
    Ok(json!({"path":name,"sha256":hash(&fs::read(repo.join(name))?)}))
}
fn rebase(prefix: &str, b: &Value) -> Result<Value> {
    safe(text(b, "path")?)?;
    let mut b = b.clone();
    b["path"] = json!(format!("{prefix}/{}", text(&b, "path")?));
    Ok(b)
}
fn output(out: &Path, name: &str, value: &Value) -> Result<Value> {
    safe(name)?;
    let target = out.join(name);
    fs::create_dir_all(target.parent().unwrap())?;
    let mut data = serde_json::to_vec_pretty(value)?;
    data.push(b'\n');
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(target)?
        .write_all(&data)?;
    Ok(json!({"path":name,"sha256":hash(&data)}))
}
fn verify_parser(repo: &Path, parser: &Value) -> Result<()> {
    bytes(repo, &parser["parser_python"])?;
    bytes(repo, &parser["environment_manifest"])?;
    ensure!(
        !rows(parser, "asset_bindings")?.is_empty(),
        "Missing parser assets"
    );
    for b in rows(parser, "asset_bindings")? {
        bytes(repo, b)?;
    }
    Ok(())
}
fn resource(repo: &Path, b: &Value) -> Result<Resource> {
    let m = bound(repo, b)?;
    ensure!(
        m["schema"] == "slopninja-verbnet-resource-manifest-v1" && m["version"] == "3.4",
        "Resource identity differs"
    );
    for key in ["archive", "license", "readme"] {
        bytes(repo, &m[key])?;
    }
    for b in rows(&m, "supporting_files")? {
        bytes(repo, b)?;
    }
    let sources = rows(&m, "files")?
        .iter()
        .map(|b| {
            Ok(XmlSource {
                path: text(b, "path")?.into(),
                sha256: text(b, "sha256")?.into(),
                xml: String::from_utf8(bytes(repo, b)?)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Resource::from_sources(sources)
}
fn index(v: &Value) -> Result<BTreeMap<String, Value>> {
    let mut m = BTreeMap::new();
    for b in rows(v, "artifacts")? {
        let name = text(b, "path")?;
        safe(name)?;
        ensure!(
            m.insert(name.into(), b.clone()).is_none(),
            "Duplicate artifact binding"
        );
    }
    Ok(m)
}
fn document(repo: &Path, b: &Value, sha: &str, identity: &str) -> Result<Document> {
    let v = bound(repo, b)?;
    ensure!(
        v["status"] == "ok",
        "Expected completed whole-post annotation"
    );
    let d: Document = serde_json::from_value(v["document"].clone())?;
    syntax::validate(&d)?;
    ensure!(
        edits::digest(&d.text) == sha && d.parser_identity == identity,
        "Full annotation identity differs"
    );
    Ok(d)
}
fn projected(value: &Value) -> Result<Option<Document>> {
    match text(value, "status")? {
        "available" => {
            let d: Document = serde_json::from_value(value["document"].clone())?;
            syntax::validate(&d)?;
            Ok(Some(d))
        }
        "unavailable" => Ok(None),
        _ => anyhow::bail!("Unknown projection status"),
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Case {
    id: String,
    alignment: Value,
    source_sha256: String,
    candidate_sha256: String,
    full_annotations: BTreeMap<String, Value>,
    preparation: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Input {
    sha256: String,
    text: String,
    origins: Vec<Value>,
}
fn inputs(cases: &[Case]) -> Result<Vec<Input>> {
    let mut m = BTreeMap::<String, Input>::new();
    for c in cases {
        if c.preparation["status"] != "prepared" {
            continue;
        }
        for side in ["source", "candidate"] {
            let s = text(&c.preparation, &format!("{side}_text"))?;
            let sha = edits::digest(s);
            let input = m.entry(sha.clone()).or_insert_with(|| Input {
                sha256: sha,
                text: s.into(),
                origins: Vec::new(),
            });
            ensure!(input.text == s, "Conflicting parse input digest");
            input.origins.push(json!({"case_id":c.id,"side":side}));
        }
    }
    Ok(m.into_values().collect())
}
fn full_pair(repo: &Path, c: &Case, p: &Value) -> Result<(Document, Document)> {
    let variant = text(p, "id")?;
    let b = c
        .full_annotations
        .get(variant)
        .context("Missing matched full annotations")?;
    let identity = text(p, "parser_identity")?;
    Ok((
        document(repo, &b["source"], &c.source_sha256, identity)?,
        document(repo, &b["candidate"], &c.candidate_sha256, identity)?,
    ))
}
fn freeze(repo: &Path, out: &Path) -> Result<()> {
    let predecessor = binding(repo, &format!("{PREVIOUS}/final-checks-v1.json"))?;
    let completed = bound(repo, &predecessor)?;
    ensure!(
        completed["stage_completed"] == true,
        "Predecessor incomplete"
    );
    let previous_protocol = rebase(PREVIOUS, &completed["protocol"])?;
    let old = bound(repo, &previous_protocol)?;
    let previous_report = rebase(PREVIOUS, &completed["report"])?;
    let report = bound(repo, &previous_report)?;
    let artifact_index = index(&report)?;
    let old_dative = bound(repo, &old["previous_protocol"])?;
    let r = resource(repo, &old_dative["resource_manifest"])?;
    let parsers = rows(&old, "parsers")?;
    ensure!(
        parsers.len() == 2
            && parsers[0]["id"] == "incumbent_sm"
            && parsers[1]["id"] == "isolated_trf",
        "Parser catalog differs"
    );
    let mut cases = Vec::new();
    let mut ids = BTreeSet::new();
    let mut generated = BTreeMap::new();
    for b in rows(&old, "selection_inputs")?.iter().filter(|b| {
        b["path"]
            .as_str()
            .is_some_and(|s| s.contains("/alignments/isolated_trf/"))
    }) {
        let a = bound(repo, b)?;
        let proposal = &a["proposal"];
        let source_sha = text(&a["input"]["post"], "text_sha256")?;
        ensure!(a["input"]["post"]["split"] == "train", "Non-TRAIN case");
        let candidate_sha = text(&proposal["candidate"], "candidate_sha256")?;
        let id = edits::digest(&format!("{source_sha}:{}", text(proposal, "id")?));
        ensure!(ids.insert(id.clone()), "Repeated proposal occurrence");
        let mut full = BTreeMap::new();
        for p in parsers {
            let variant = text(p, "id")?;
            let mut pair = json!({});
            for (side, sha) in [("source", source_sha), ("candidate", candidate_sha)] {
                let name = format!("annotations/{variant}/canonical_a/{sha}-r0.json");
                pair[side] = rebase(
                    &format!("{PREVIOUS}/run-v1"),
                    artifact_index
                        .get(&name)
                        .context("Missing matched annotation")?,
                )?;
            }
            full.insert(variant.into(), pair);
        }
        let mut c = Case {
            id,
            alignment: b.clone(),
            source_sha256: source_sha.into(),
            candidate_sha256: candidate_sha.into(),
            full_annotations: full,
            preparation: Value::Null,
        };
        let (s, t) = full_pair(repo, &c, &parsers[1])?;
        if !generated.contains_key(source_sha) {
            generated.insert(
                source_sha.to_owned(),
                alternation::propose_with_policy(&s, &r, ArgumentPolicy::RoleAwareV1)?,
            );
        }
        ensure!(
            generated[source_sha]
                .proposals
                .iter()
                .any(|p| serde_json::to_value(p).ok().as_ref() == Some(proposal)),
            "Original primary proposal no longer reproduces exactly"
        );
        c.preparation = projection::prepare(&s, &t, proposal)?;
        for p in parsers {
            full_pair(repo, &c, p)?;
        }
        cases.push(c);
    }
    cases.sort_by(|a, b| a.id.cmp(&b.id));
    ensure!(
        cases.len() == 60,
        "Expected every 60 primary proposal occurrences"
    );
    let parse_inputs = inputs(&cases)?;
    ensure!(parse_inputs.len() <= 120, "Sentence input budget differs");
    let mut names = BTreeSet::new();
    for b in rows(&old, "source_bindings")? {
        bytes(repo, b)?;
        names.insert(text(b, "path")?.to_owned());
    }
    for name in [
        "sentence_context_study.rs",
        "sentence_context_projection.rs",
        "sentence_context_comparison.rs",
        "bin/slop_ninja-sentence-context.rs",
    ] {
        names.insert(format!("grammar/crates/grammar-eval/src/{name}"));
    }
    let p = json!({"schema":SCHEMA,"executable_sha256":hash(&fs::read(std::env::current_exe()?)?),
        "source_bindings":names.iter().map(|n|binding(repo,n)).collect::<Result<Vec<_>>>()?,
        "predecessor":predecessor,"previous_protocol":previous_protocol,"previous_report":previous_report,
        "dative_protocol":old["previous_protocol"],"resource_manifest":old_dative["resource_manifest"],"parsers":parsers,
        "requested_cases":60,"prepared_cases":cases.iter().filter(|c|c.preparation["status"]=="prepared").count(),
        "parse_requests_per_parser":parse_inputs.len(),"total_parse_requests":parse_inputs.len()*2,"cases":cases,"parse_inputs":parse_inputs,
        "selection":"All 60 primary dative proposals regardless of prior outcome; same primary-defined exact source sentence and patched counterpart for both parsers",
        "candidate_segmentation_policy":"A changed candidate sentence boundary does not prevent parsing the source-defined projected candidate slice; exact full-sentence projection is separately unavailable",
        "annotation_schedule":"One parse_batch per parser, SHA256-sorted unique exact slice texts; retain all case origins; no additional generated candidate parses",
        "comparisons":["Original primary whole-post check, retained","Exact full-post sentence projections under unchanged annotations, when available","Fresh isolated sentence annotations","Each full-post projection versus its same-text isolated annotation"],
        "scope_only_baseline":"Run identical local structural evaluator on the saved closed sentence projections and the isolated annotations; separate scope effects from changed parse observations",
        "runtime_changes":false,"fit_models":false,"later_posts":0,"llm_calls":0,"detector_calls":0,
        "automatic_edit_license":false,"semantic_equivalence_certified":false,"window_causation_established":false});
    fs::create_dir(out)?;
    println!("{}", output(out, "protocol.json", &p)?);
    println!(
        "Prepared {}/60 cases; {} unique slice texts per parser",
        p["prepared_cases"],
        parse_inputs.len()
    );
    Ok(())
}
fn span(v: &Value, k: &str) -> Result<[usize; 2]> {
    serde_json::from_value(v[k].clone()).with_context(|| format!("Invalid span {k}"))
}
fn evaluate(
    s: Option<&Document>,
    t: Option<&Document>,
    prepared: &Value,
    original: &Value,
    r: &Resource,
) -> Value {
    match (s, t) {
        (Some(s), Some(t)) => match comparison::evaluate(s, t, prepared, original, r) {
            Ok(v) => v,
            Err(e) => {
                json!({"status":"execution_error","error":format!("{e:#}"),"assessment_complete":false,"reciprocally_observed_preserved":null})
            }
        },
        _ => {
            json!({"status":"unavailable","reason":"annotation_or_projection_unavailable","assessment_complete":false,"reciprocally_observed_preserved":null})
        }
    }
}
fn change(projected: Option<&Document>, isolated: Option<&Document>) -> Result<Value> {
    match (projected, isolated) {
        (Some(a), Some(b)) => Ok(json!({"status":"compared","observation":repeat::compare(a,b)?})),
        _ => Ok(
            json!({"status":"unavailable","projection_available":projected.is_some(),"isolated_annotation_available":isolated.is_some()}),
        ),
    }
}
fn result_label(v: &Value) -> String {
    if v["reciprocally_observed_preserved"] == true {
        "preserved".into()
    } else if v["assessment_complete"] == true {
        "not_preserved".into()
    } else {
        format!(
            "{}:{}",
            v["status"].as_str().unwrap_or("unknown"),
            v["reason"].as_str().unwrap_or("incomplete")
        )
    }
}
fn summary(cases: &[Value]) -> Value {
    let mut transitions = BTreeMap::<String, usize>::new();
    let mut stages = BTreeMap::new();
    for c in cases {
        let key = format!(
            "{} -> {}",
            result_label(&c["projected_evaluation"]),
            result_label(&c["isolated_evaluation"])
        );
        *transitions.entry(key).or_default() += 1;
    }
    for key in ["projected_evaluation", "isolated_evaluation"] {
        let mut statuses = BTreeMap::<String, usize>::new();
        let mut reasons = BTreeMap::<String, usize>::new();
        for c in cases {
            *statuses
                .entry(c[key]["status"].as_str().unwrap_or("unavailable").into())
                .or_default() += 1;
            if let Some(reason) = c[key]["reason"].as_str() {
                *reasons.entry(reason.into()).or_default() += 1;
            }
        }
        stages.insert(key,json!({"requested":cases.len(),"status_counts":statuses,"reason_counts":reasons,
            "reciprocally_observed_preserved":cases.iter().filter(|c|c[key]["reciprocally_observed_preserved"]==true).count(),
            "assessment_complete":cases.iter().filter(|c|c[key]["assessment_complete"]==true).count()}));
    }
    let mut context = BTreeMap::new();
    for side in ["source", "candidate"] {
        let v = cases
            .iter()
            .map(|c| &c["context_change"][side])
            .collect::<Vec<_>>();
        context.insert(side,json!({"requested":v.len(),"compared":v.iter().filter(|v|v["status"]=="compared").count(),
            "identical":v.iter().filter(|v|v["observation"]["exact_document_equal"]==true).count(),
            "different":v.iter().filter(|v|v["observation"]["exact_document_equal"]==false).count(),
            "unavailable":v.iter().filter(|v|v["status"]!="compared").count()}));
    }
    json!({"requested_cases":cases.len(),"prepared_cases":cases.iter().filter(|c|c["preparation_status"]=="prepared").count(),
        "stages":stages,"projected_to_isolated_transitions":transitions,"context_change":context,
        "original_primary_whole_post_passes":cases.iter().filter(|c|c["original_primary_whole_post"]["reciprocally_observed_preserved"]==true).count()})
}
fn execute(repo: &Path, out: &Path, p: &Value, artifacts: &mut Vec<Value>) -> Result<Value> {
    let cases: Vec<Case> = serde_json::from_value(p["cases"].clone())?;
    let parse_inputs: Vec<Input> = serde_json::from_value(p["parse_inputs"].clone())?;
    ensure!(
        cases.len() == 60 && inputs(&cases)? == parse_inputs,
        "Frozen cases/input schedule differs"
    );
    let parsers = rows(p, "parsers")?;
    ensure!(
        parsers.len() == 2
            && parsers[0]["id"] == "incumbent_sm"
            && parsers[1]["id"] == "isolated_trf",
        "Parser order differs"
    );
    let r = resource(repo, &p["resource_manifest"])?;
    let mut originals = BTreeMap::new();
    for c in &cases {
        let a = bound(repo, &c.alignment)?;
        let (s, t) = full_pair(repo, c, &parsers[1])?;
        ensure!(
            projection::prepare(&s, &t, &a["proposal"])? == c.preparation,
            "Frozen projection differs"
        );
        originals.insert(c.id.clone(), a);
    }
    let mut reports = Vec::new();
    for parser in parsers {
        let variant = text(parser, "id")?;
        verify_parser(repo, parser)?;
        let input = parse_inputs
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>();
        println!("{variant}: parsing {} exact isolated texts", input.len());
        let started = Instant::now();
        let parsed = grammar_spacy::parse_batch(
            &input,
            &repo
                .join(text(&parser["parser_python"], "path")?)
                .to_string_lossy(),
        );
        let parse_seconds = started.elapsed().as_secs_f64();
        let (parsed, process_error) = match parsed {
            Ok(v) => (v, None),
            Err(e) => {
                let message = format!("{e:#}");
                (
                    (0..input.len()).map(|_| Err(message.clone())).collect(),
                    Some(message),
                )
            }
        };
        ensure!(
            parsed.len() == parse_inputs.len(),
            "Parser result count differs"
        );
        verify_parser(repo, parser)?;
        let mut docs = BTreeMap::new();
        let mut refs = BTreeMap::new();
        let mut errors = 0;
        for (i, d) in parse_inputs.iter().zip(parsed) {
            let value = match &d {
                Ok(d) => {
                    syntax::validate(d)?;
                    ensure!(
                        d.text == i.text && d.parser_identity == parser["parser_identity"],
                        "Isolated annotation identity differs"
                    );
                    json!({"status":"ok","document":d,"origins":i.origins})
                }
                Err(error) => {
                    errors += 1;
                    json!({"status":if process_error.is_some(){"process_error"}else{"error"},"error":error,"text":i.text,"origins":i.origins})
                }
            };
            let b = output(
                out,
                &format!("annotations/{variant}/{}.json", i.sha256),
                &value,
            )?;
            artifacts.push(b.clone());
            refs.insert(i.sha256.clone(), b);
            docs.insert(i.sha256.clone(), d);
        }
        let mut results = Vec::new();
        for c in &cases {
            let a = &originals[&c.id];
            let mut value = json!({"case_id":c.id,"primary":variant=="isolated_trf","variant":variant,
                "preparation_status":c.preparation["status"],"preparation":c.preparation,
                "original_alignment":c.alignment,"original_primary_whole_post":a["result"],
                "automatic_edit_license":false,"semantic_equivalence_certified":false});
            if c.preparation["status"] == "prepared" {
                let (full_s, full_t) = full_pair(repo, c, parser)?;
                let ps = projection::project(&full_s, span(&c.preparation, "source_span")?)?;
                let pt = projection::project(&full_t, span(&c.preparation, "candidate_span")?)?;
                let s = projected(&ps)?;
                let t = projected(&pt)?;
                let ssha = edits::digest(text(&c.preparation, "source_text")?);
                let tsha = edits::digest(text(&c.preparation, "candidate_text")?);
                let isolated_s = docs[&ssha].as_ref().ok();
                let isolated_t = docs[&tsha].as_ref().ok();
                value["full_annotations"] = c.full_annotations[variant].clone();
                value["full_sentence_projections"] = json!({"source":ps,"candidate":pt});
                value["isolated_annotations"] =
                    json!({"source":refs[&ssha],"candidate":refs[&tsha]});
                value["projected_evaluation"] =
                    evaluate(s.as_ref(), t.as_ref(), &c.preparation, &a["proposal"], &r);
                value["isolated_evaluation"] =
                    evaluate(isolated_s, isolated_t, &c.preparation, &a["proposal"], &r);
                value["context_change"] = json!({"source":change(s.as_ref(),isolated_s)?,"candidate":change(t.as_ref(),isolated_t)?});
                value["annotation_errors"] = json!({"source":docs[&ssha].as_ref().err(),"candidate":docs[&tsha].as_ref().err()});
            } else {
                let unavailable = json!({"status":"unavailable","reason":"source_sentence_not_prepared","assessment_complete":false,"reciprocally_observed_preserved":null});
                value["projected_evaluation"] = unavailable.clone();
                value["isolated_evaluation"] = unavailable;
                value["context_change"] =
                    json!({"source":{"status":"unavailable"},"candidate":{"status":"unavailable"}});
            }
            let pointer = output(out, &format!("cases/{variant}/{}.json", c.id), &value)?;
            artifacts.push(pointer.clone());
            // Compact tables retain states and counts; raw evidence stays in each case artifact.
            let mut compact = value.clone();
            for key in [
                "preparation",
                "full_sentence_projections",
                "original_primary_whole_post",
            ] {
                compact.as_object_mut().unwrap().remove(key);
            }
            compact["original_primary_whole_post"] = json!({"reciprocally_observed_preserved":a["result"]["reciprocally_observed_preserved"]});
            for key in ["projected_evaluation", "isolated_evaluation"] {
                compact[key] = json!({"status":value[key]["status"],"reason":value[key]["reason"],"assessment_complete":value[key]["assessment_complete"],
                    "source_match_count":value[key]["source_match_count"],"reverse_match_count":value[key]["reverse_match_count"],
                    "reciprocally_observed_preserved":value[key]["reciprocally_observed_preserved"]});
            }
            for side in ["source", "candidate"] {
                let v = &value["context_change"][side];
                compact["context_change"][side] = json!({"status":v["status"],"observation":{"exact_document_equal":v["observation"]["exact_document_equal"],"counts":v["observation"]["counts"]}});
            }
            compact["report"] = pointer;
            results.push(compact);
        }
        let report = json!({"variant":variant,"primary":variant=="isolated_trf","annotation_requests":input.len(),"annotation_errors":errors,
            "process_error":process_error,"parse_seconds":parse_seconds,"summary":summary(&results),"cases":results,
            "automatic_edit_license":false,"semantic_equivalence_certified":false});
        let b = output(out, &format!("{variant}/report.json"), &report)?;
        artifacts.push(b.clone());
        println!("{variant}: {}", report["summary"]);
        reports.push(json!({"variant":variant,"report":b,"summary":report["summary"],"annotation_requests":input.len(),"annotation_errors":errors,"process_error":process_error}));
    }
    Ok(
        json!({"schema":"slopninja-sentence-context-report-v1","requested_cases_per_parser":60,"reports":reports,"total_annotation_requests":parse_inputs.len()*2,
        "fit_models":false,"llm_calls":0,"detector_calls":0,"later_posts":0,
        "automatic_edit_license":false,"semantic_equivalence_certified":false,"window_causation_established":false}),
    )
}
fn run(repo: &Path, file: &Path, expected: &str, out: &Path) -> Result<()> {
    let p: Value = serde_json::from_slice(&checked(file, expected)?)?;
    ensure!(
        p["schema"] == SCHEMA
            && p["requested_cases"] == 60
            && p["runtime_changes"] == false
            && p["fit_models"] == false
            && p["executable_sha256"] == hash(&fs::read(std::env::current_exe()?)?),
        "Protocol/executable differs"
    );
    ensure!(
        bound(repo, &p["predecessor"])?["stage_completed"] == true,
        "Predecessor incomplete"
    );
    for b in rows(&p, "source_bindings")? {
        safe(text(b, "path")?)?;
        bytes(repo, b)?;
    }
    fs::create_dir(out)?;
    let result = (|| -> Result<()> {
        let mut artifacts = vec![output(out, "protocol.json", &p)?];
        for b in rows(&p, "source_bindings")? {
            let name = format!("executed-source/{}", text(b, "path")?);
            let target = out.join(&name);
            fs::create_dir_all(target.parent().unwrap())?;
            fs::write(target, bytes(repo, b)?)?;
            artifacts.push(json!({"path":name,"sha256":b["sha256"]}));
        }
        let mut report = execute(repo, out, &p, &mut artifacts)?;
        report["protocol_sha256"] = json!(expected);
        report["artifacts"] = json!(artifacts);
        let b = output(out, "report.json", &report)?;
        output(
            out,
            "receipt.json",
            &json!({"schema":"slopninja-sentence-context-receipt-v1","status":"completed","protocol_sha256":expected,"report":b}),
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
    fn same_text_inputs_share_one_parse_but_keep_every_case_origin() {
        let case = |id: &str, s: &str, t: &str| Case {
            id: id.into(),
            alignment: Value::Null,
            source_sha256: String::new(),
            candidate_sha256: String::new(),
            full_annotations: BTreeMap::new(),
            preparation: json!({"status":"prepared","source_text":s,"candidate_text":t}),
        };
        let cases = vec![
            case("one", "Source.", "Candidate."),
            case("two", "Source.", "Different."),
        ];
        let inputs = inputs(&cases).unwrap();
        assert_eq!(inputs.len(), 3);
        assert_eq!(
            inputs
                .iter()
                .find(|i| i.text == "Source.")
                .unwrap()
                .origins
                .len(),
            2
        );
        assert!(
            inputs
                .windows(2)
                .all(|pair| pair[0].sha256 < pair[1].sha256)
        );
    }
    #[test]
    fn summaries_keep_unavailable_context_and_incomplete_evaluations_separate() {
        assert_ne!(
            result_label(&json!({"status":"compared","assessment_complete":false,
                "reciprocally_observed_preserved":false})),
            "not_preserved"
        );
        let cases = vec![
            json!({"preparation_status":"prepared","projected_evaluation":{"status":"unavailable","reason":"no_projection","assessment_complete":false},
            "isolated_evaluation":{"status":"compared","assessment_complete":true,"reciprocally_observed_preserved":false},
            "context_change":{"source":{"status":"compared","observation":{"exact_document_equal":true}},"candidate":{"status":"unavailable"}}}),
            json!({"preparation_status":"unavailable","projected_evaluation":{"status":"unavailable","reason":"source_sentence_not_prepared","assessment_complete":false},
            "isolated_evaluation":{"status":"execution_error","assessment_complete":false},"context_change":{}}),
        ];
        let s = summary(&cases);
        assert_eq!(s["requested_cases"], 2);
        assert_eq!(s["context_change"]["source"]["identical"], 1);
        assert_eq!(s["context_change"]["candidate"]["unavailable"], 2);
        assert_eq!(
            s["stages"]["isolated_evaluation"]["reciprocally_observed_preserved"],
            0
        );
        assert_eq!(s["stages"]["isolated_evaluation"]["assessment_complete"], 1);
    }
}

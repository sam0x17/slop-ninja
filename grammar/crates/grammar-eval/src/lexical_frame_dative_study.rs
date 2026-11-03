//! Matched mapping comparisons, with exact historical baseline replay first.
use crate::{lexical_frame_observation::Mapping, lexical_frame_study as study};
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::syntax::{self, Document};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Instant,
};
use study::{bound, bound_bytes, checked, evaluate_case, hash, output, rows, safe_relative, text};

const VARIANTS: [&str; 4] = ["incumbent_sm", "isolated_sm", "isolated_md", "isolated_trf"];

#[derive(Parser)]
struct Args {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    expected_protocol_sha256: String,
    #[arg(long)]
    out: PathBuf,
}

struct Saved {
    panel: String,
    variant: String,
    directory: PathBuf,
    fixture: Value,
    artifacts: BTreeMap<String, Value>,
    parser_identity: String,
}
impl Saved {
    fn artifact(&self, name: &str) -> Result<Value> {
        bound(
            &self.directory,
            self.artifacts.get(name).context("Missing saved artifact")?,
        )
    }
    fn annotation(&self, case: &Value) -> Result<std::result::Result<Document, String>> {
        let source = text(case, "text")?;
        annotation(
            &self.artifact(&format!("annotations/{}.json", hash(source.as_bytes())))?,
            source,
            &self.parser_identity,
        )
    }
}

fn annotation(
    value: &Value,
    source: &str,
    parser: &str,
) -> Result<std::result::Result<Document, String>> {
    match text(value, "status")? {
        "ok" => {
            let doc: Document = serde_json::from_value(value["document"].clone())?;
            syntax::validate(&doc)?;
            ensure!(
                doc.text == source && doc.parser_identity == parser,
                "Annotation identity differs"
            );
            Ok(Ok(doc))
        }
        "error" => {
            ensure!(value["text"] == source, "Failed annotation text differs");
            Ok(Err(text(value, "error")?.into()))
        }
        _ => anyhow::bail!("Unknown annotation status"),
    }
}

fn load_saved(repo: &Path, descriptor: &Value, resource_binding: &Value) -> Result<Saved> {
    let directory = repo.join(text(descriptor, "run_directory")?);
    let receipt = bound(repo, &descriptor["receipt"])?;
    ensure!(
        receipt["status"] == "completed"
            && receipt["protocol_sha256"] == descriptor["protocol"]["sha256"],
        "Historical receipt differs"
    );
    ensure!(
        repo.join(text(&descriptor["receipt"], "path")?)
            .canonicalize()?
            == directory.join("receipt.json").canonicalize()?,
        "Historical receipt directory differs"
    );
    let protocol = bound(repo, &descriptor["protocol"])?;
    let report = bound(&directory, &receipt["report"])?;
    ensure!(
        report["protocol_sha256"] == receipt["protocol_sha256"]
            && protocol["resource_manifest"] == *resource_binding,
        "Historical protocol/resource differs"
    );
    let fixture = bound(repo, &descriptor["fixture"])?;
    ensure!(
        bound(repo, &protocol["fixture_manifest"])? == fixture,
        "Historical fixture differs"
    );
    let variant = text(descriptor, "variant")?;
    ensure!(
        protocol["parser"]["id"] == variant,
        "Historical parser variant differs"
    );
    let mut artifacts = BTreeMap::<String, Value>::new();
    for binding in rows(&report, "artifacts")? {
        let name = text(binding, "path")?;
        safe_relative(name)?;
        bound_bytes(&directory, binding)?;
        ensure!(
            artifacts.insert(name.into(), binding.clone()).is_none(),
            "Duplicate historical artifact"
        );
    }
    // Historical source paths may evolve. Verify the executed snapshots instead.
    for binding in rows(&protocol, "source_bindings")? {
        let name = format!("executed-source/{}", text(binding, "path")?);
        let snapshot = artifacts
            .get(&name)
            .context("Missing historical executed source")?;
        ensure!(
            snapshot["sha256"] == binding["sha256"],
            "Historical source snapshot differs"
        );
    }
    let expected_cases = rows(&fixture, "cases")?.len();
    ensure!(
        artifacts.keys().filter(|p| p.starts_with("cases/")).count() == expected_cases,
        "Saved case coverage differs"
    );
    Ok(Saved {
        panel: text(descriptor, "panel")?.into(),
        variant: variant.into(),
        directory,
        fixture,
        artifacts,
        parser_identity: text(&protocol["parser"], "parser_identity")?.into(),
    })
}

fn stripped_observation(row: &Value) -> Value {
    let mut value = row["observation"].clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("schema");
        object.remove("mapping_identity");
    }
    value
}

fn ledger_without_frames(row: &Value) -> Value {
    let mut value = stripped_observation(row);
    if let Some(predicates) = value["predicates"].as_array_mut() {
        for predicate in predicates {
            predicate
                .as_object_mut()
                .unwrap()
                .remove("frame_possibilities");
        }
    }
    value
}

fn frame_keys(row: &Value) -> Vec<Value> {
    row["observation"]["predicates"].as_array().map(|ps| ps.iter().map(|p| json!({"head":p["head"],"frames":p["frame_possibilities"].as_array().unwrap().iter().map(|f|json!([f["member_class_id"],f["member_ordinal"],f["frame"]["id"]])).collect::<Vec<_>>()})).collect()).unwrap_or_default()
}

fn label(value: &Value) -> &'static str {
    match value.as_bool() {
        Some(true) => "complete",
        Some(false) => "incomplete",
        None => "unavailable",
    }
}
fn transition(a: &str, b: &str) -> String {
    format!("{a}_to_{b}")
}

fn applicability(case: &Value, parsed: &std::result::Result<Document, String>) -> Result<Value> {
    let Some(spec) = case.get("raw_mapping_applicability") else {
        return Ok(Value::Null);
    };
    ensure!(
        spec["policy_ref"] == "direct_adp_recipient_v1"
            && spec["parser_label_is_asserted"] == false,
        "Unknown raw mapping policy"
    );
    let Ok(doc) = parsed else {
        return Ok(
            json!({"specification":spec,"status":"unavailable","parse_error":parsed.as_ref().err()}),
        );
    };
    let at = |value: &Value| -> Result<Option<&grammar_core::syntax::Token>> {
        if value.is_null() {
            return Ok(None);
        }
        let [start, end]: [usize; 2] = serde_json::from_value(value.clone())?;
        Ok(doc
            .tokens
            .iter()
            .find(|t| t.start_byte == start && t.end_byte == end))
    };
    let predicate = at(&spec["predicate_span"])?;
    let carrier = at(&spec["carrier_span"])?;
    let recipient = at(&spec["recipient_span"])?;
    if (!spec["predicate_span"].is_null() && predicate.is_none())
        || (!spec["carrier_span"].is_null() && carrier.is_none())
        || (!spec["recipient_span"].is_null() && recipient.is_none())
    {
        return Ok(
            json!({"specification":spec,"status":"unavailable_anchor","predicate":predicate,"carrier":carrier,"recipient":recipient,"baseline_to_slot":null,"extended_to_slot":null}),
        );
    }
    if spec["carrier_span"].is_null() {
        return Ok(
            json!({"specification":spec,"status":"no_explicit_carrier_declared","predicate":predicate,"bare_recipient":recipient,"baseline_to_slot":null,"extended_to_slot":null,"observed_rejection_certified":false}),
        );
    }
    let (base, ext, shape, literal) =
        if let (Some(p), Some(c), Some(r)) = (predicate, carrier, recipient) {
            let shape = c.head == p.i
                && c.i > p.i
                && c.pos == "ADP"
                && crate::lexical_context::lexical(c)
                && r.head == c.i
                && r.i > c.i
                && r.dep == "pobj"
                && crate::lexical_context::lexical(r)
                && matches!(r.pos.as_str(), "NOUN" | "PROPN" | "PRON");
            let literal = crate::lexical_context::normalized_lemma(c).as_deref()
                == spec["frame_literal"].as_str();
            (
                shape && literal && c.dep == "prep",
                shape && literal && (c.dep == "prep" || (c.dep == "dative" && p.pos == "VERB")),
                shape,
                literal,
            )
        } else {
            (false, false, false, false)
        };
    Ok(
        json!({"specification":spec,"status":"observed","predicate":predicate,"carrier":carrier,"recipient":recipient,"exact_anchors_found":predicate.is_some()&&carrier.is_some()&&recipient.is_some(),"direct_prepositional_object_shape":shape,"frame_literal_matches":literal,"baseline_to_slot":base,"extended_to_slot":ext,"full_frame_compatibility_checked_separately":true}),
    )
}

fn compare_case(
    base: &Value,
    extended: &Value,
    parsed: &std::result::Result<Document, String>,
) -> Result<Value> {
    ensure!(
        base["case"] == extended["case"],
        "Mapping case identity differs"
    );
    ensure!(
        ledger_without_frames(base) == ledger_without_frames(extended),
        "Mapping altered raw ledger or predicate discovery"
    );
    ensure!(
        frame_keys(base) == frame_keys(extended),
        "Mapping changed resource alternatives"
    );
    ensure!(
        rows(base, "expectations")?.len() == rows(extended, "expectations")?.len(),
        "Expectation count changed"
    );
    let expectations = rows(base, "expectations")?.iter().zip(rows(extended, "expectations")?).map(|(a,b)| {
        ensure!(a["expectation"] == b["expectation"], "Mapping expectation identity differs");
        let e = &a["expectation"];
        Ok(json!({"expectation":e,"baseline":a["status"],"extended":b["status"],"transition":transition(text(a,"status")?,text(b,"status")?),"role_requirement":if e["kind"]=="role_binding" {if e["require_complete_syntax"]==true {"complete"} else {"partial_allowed"}} else {"not_role"}}))
    }).collect::<Result<Vec<_>>>()?;
    ensure!(
        rows(base, "target_predicates")?.len() == rows(extended, "target_predicates")?.len(),
        "Target count changed"
    );
    let targets = rows(base,"target_predicates")?.iter().zip(rows(extended,"target_predicates")?).map(|(a,b)| {
        ensure!(a["predicate_span"]==b["predicate_span"] && a["found"]==b["found"], "Target identity changed");
        Ok(json!({"predicate_span":a["predicate_span"],"baseline":a,"extended":b,"transition":transition(label(&a["any_complete_syntax"]),label(&b["any_complete_syntax"]))}))
    }).collect::<Result<Vec<_>>>()?;
    let datives = parsed.as_ref().ok().map(|doc| doc.tokens.iter().filter(|t|t.dep=="dative").map(|t| {
        let head = &doc.tokens[t.head];
        let nominal_objects = doc.tokens.iter().filter(|o|o.head==t.i && o.i!=t.i && o.dep=="pobj" && o.i>t.i && crate::lexical_context::lexical(o) && matches!(o.pos.as_str(),"NOUN"|"PROPN"|"PRON")).collect::<Vec<_>>();
        let eligible = head.pos=="VERB" && t.i>t.head && t.pos=="ADP" && crate::lexical_context::lexical(t) && !nominal_objects.is_empty();
        json!({"carrier":t,"governor":head,"nominal_objects":nominal_objects,"slot_shape_supported":eligible,"literal_frame_compatibility_checked_separately":true})
    }).collect::<Vec<_>>());
    let observed = base["observation"].is_object() && extended["observation"].is_object();
    Ok(
        json!({"case":base["case"],"baseline_status":base["status"],"extended_status":extended["status"],"annotation_available":parsed.is_ok(),"observations_available":observed,"observation_body_changed":observed.then(||stripped_observation(base)!=stripped_observation(extended)),"raw_ledger_equal":observed.then_some(true),"resource_alternatives_equal":observed.then_some(true),"expectations":expectations,"targets":targets,"dative_evidence":datives,"raw_mapping_applicability":applicability(&base["case"],parsed)?,"automatic_edit_license":false,"semantic_equivalence_certified":false}),
    )
}

fn comparison_summary(comparisons: &[Value]) -> Value {
    let mut expectations = BTreeMap::<String, usize>::new();
    let mut targets = BTreeMap::<String, usize>::new();
    let mut statuses = BTreeMap::<String, usize>::new();
    for c in comparisons {
        for e in c["expectations"].as_array().unwrap() {
            *expectations
                .entry(format!(
                    "{}:{}:{}",
                    e["expectation"]["kind"].as_str().unwrap(),
                    e["role_requirement"].as_str().unwrap(),
                    e["transition"].as_str().unwrap()
                ))
                .or_default() += 1;
        }
        for t in c["targets"].as_array().unwrap() {
            *targets
                .entry(t["transition"].as_str().unwrap().into())
                .or_default() += 1;
        }
        for side in ["baseline", "extended"] {
            *statuses
                .entry(format!(
                    "{side}:{}",
                    c[format!("{side}_status")].as_str().unwrap()
                ))
                .or_default() += 1;
        }
    }
    json!({"cases":comparisons.len(),"observation_bodies_changed":comparisons.iter().filter(|c|c["observation_body_changed"]==true).count(),"observation_comparisons_unavailable":comparisons.iter().filter(|c|c["observation_body_changed"].is_null()).count(),"expectation_transitions":expectations,"target_transitions":targets,"case_statuses":statuses})
}

fn save_comparison(
    out: &Path,
    name: &str,
    base: &Value,
    extended: &Value,
    comparison: &Value,
    baseline_binding: Value,
    artifacts: &mut Vec<Value>,
) -> Result<Value> {
    let mut record = comparison.clone();
    record["baseline_case"] = baseline_binding;
    if stripped_observation(base) != stripped_observation(extended)
        || base["status"] != extended["status"]
    {
        let binding = output(out, &format!("{name}/extended.json"), extended)?;
        artifacts.push(binding.clone());
        record["extended_case"] = binding;
    } else {
        record["extended_case"] = Value::Null;
        record["extension_storage"] = json!(
            "Baseline body retained; extended schema and mapping_identity are recorded by the protocol. No changed observation body."
        );
    }
    let binding = output(out, &format!("{name}/comparison.json"), &record)?;
    artifacts.push(binding);
    Ok(record)
}

fn summarize_run(
    out: &Path,
    name: &str,
    baseline: &[Value],
    extended: &[Value],
    comparisons: &[Value],
    artifacts: &mut Vec<Value>,
) -> Result<Value> {
    let pairs = json!({"baseline":study::pair_results(baseline)?,"extended":study::pair_results(extended)?});
    artifacts.push(output(out, &format!("{name}/pairs.json"), &pairs)?);
    let summary = json!({"run":name,"baseline":study::summary(&baseline.iter().collect::<Vec<_>>()),"extended":study::summary(&extended.iter().collect::<Vec<_>>()),"comparison":comparison_summary(comparisons)});
    artifacts.push(output(out, &format!("{name}/summary.json"), &summary)?);
    Ok(summary)
}

fn execute(args: &Args, protocol: &Value, artifacts: &mut Vec<Value>) -> Result<Value> {
    let resource = study::resource(
        &args.repo,
        &bound(&args.repo, &protocol["resource_manifest"])?,
    )?;
    let mut saved = Vec::new();
    let mut keys = BTreeSet::new();
    for descriptor in rows(protocol, "saved_runs")? {
        let run = load_saved(&args.repo, descriptor, &protocol["resource_manifest"])?;
        ensure!(
            VARIANTS.contains(&run.variant.as_str())
                && ["reused96", "reused64"].contains(&run.panel.as_str()),
            "Unknown saved run"
        );
        ensure!(
            keys.insert((run.panel.clone(), run.variant.clone())),
            "Duplicate saved run"
        );
        study::validate_fixture(&run.fixture, &resource)?;
        saved.push(run);
    }
    ensure!(keys.len() == 8, "Missing historical parser/panel run");
    let mut parity_cases = 0;
    for run in &saved {
        let expected = if run.panel == "reused96" { 96 } else { 64 };
        ensure!(
            rows(&run.fixture, "cases")?.len() == expected,
            "Historical case budget differs"
        );
        for case in rows(&run.fixture, "cases")? {
            let old = run.artifact(&format!("cases/{}.json", text(case, "id")?))?;
            let parsed = run.annotation(case)?;
            let replay = evaluate_case(case, &parsed, &resource, Mapping::BaselineV1)?;
            ensure!(
                replay == old,
                "Baseline replay differs: {}/{}/{}",
                run.panel,
                run.variant,
                text(case, "id")?
            );
            parity_cases += 1;
        }
        println!(
            "baseline replay passed: {} {} ({expected} cases)",
            run.panel, run.variant
        );
    }
    ensure!(parity_cases == 640, "Incomplete baseline replay");
    artifacts.push(output(
        &args.out,
        "baseline-replay.json",
        &json!({"whole_case_value_equal":true,"cases":parity_cases,"parser_calls":0}),
    )?);
    let mut summaries = Vec::new();
    for run in &saved {
        let name = format!("{}/{}", run.panel, run.variant);
        let mut bases = Vec::new();
        let mut extended = Vec::new();
        let mut comparisons = Vec::new();
        for case in rows(&run.fixture, "cases")? {
            let id = text(case, "id")?;
            let old_name = format!("cases/{id}.json");
            let base = run.artifact(&old_name)?;
            let parsed = run.annotation(case)?;
            let ext = evaluate_case(case, &parsed, &resource, Mapping::PrepositionalDativeV1)?;
            let comparison = compare_case(&base, &ext, &parsed)?;
            let old_binding = &run.artifacts[&old_name];
            let old_binding = json!({"path":run.directory.join(&old_name),"sha256":old_binding["sha256"],"scope":"historical_absolute_path"});
            comparisons.push(save_comparison(
                &args.out,
                &format!("{name}/{id}"),
                &base,
                &ext,
                &comparison,
                old_binding,
                artifacts,
            )?);
            bases.push(base);
            extended.push(ext);
        }
        let summary = summarize_run(&args.out, &name, &bases, &extended, &comparisons, artifacts)?;
        println!("mapping comparison: {name} {}", summary["comparison"]);
        summaries.push(summary);
    }
    let fixture = bound(&args.repo, &protocol["fresh_fixture"])?;
    let texts = study::validate_fixture(&fixture, &resource)?;
    ensure!(
        texts.len() == 24 && rows(&fixture, "cases")?.len() == 24,
        "Fresh case budget differs"
    );
    let old_texts = saved
        .iter()
        .flat_map(|r| {
            r.fixture["cases"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| hash(c["text"].as_str().unwrap().as_bytes()))
        })
        .collect::<BTreeSet<_>>();
    ensure!(
        texts.keys().all(|sha| !old_texts.contains(sha)),
        "Fresh text reused from historical panels"
    );
    let parsers = rows(protocol, "parsers")?;
    ensure!(
        parsers.len() == 4
            && parsers
                .iter()
                .map(|p| text(p, "id"))
                .collect::<Result<Vec<_>>>()?
                == VARIANTS,
        "Fresh parser catalog differs"
    );
    let mut new_annotations = 0;
    for parser in parsers {
        study::verify_parser(&args.repo, parser)?;
        let variant = text(parser, "id")?;
        let name = format!("fresh24/{variant}");
        let ordered = texts.iter().collect::<Vec<_>>();
        let input = ordered.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>();
        let began = Instant::now();
        let parsed = grammar_spacy::parse_batch(
            &input,
            &args
                .repo
                .join(text(&parser["parser_python"], "path")?)
                .to_string_lossy(),
        )?;
        ensure!(parsed.len() == 24, "Parser result count differs");
        let mut docs = BTreeMap::new();
        for ((sha, source), doc) in ordered.into_iter().zip(parsed) {
            let value = match &doc {
                Ok(d) => json!({"status":"ok","document":d}),
                Err(e) => json!({"status":"error","text":source,"error":e}),
            };
            let doc = annotation(&value, source, text(parser, "parser_identity")?)?;
            artifacts.push(output(
                &args.out,
                &format!("{name}/annotations/{sha}.json"),
                &value,
            )?);
            docs.insert(sha.clone(), doc);
            new_annotations += 1;
        }
        let timing = began.elapsed().as_secs_f64();
        study::verify_parser(&args.repo, parser)?;
        let mut bases = Vec::new();
        let mut extended = Vec::new();
        let mut comparisons = Vec::new();
        for case in rows(&fixture, "cases")? {
            let id = text(case, "id")?;
            let parsed = &docs[&hash(text(case, "text")?.as_bytes())];
            let base = evaluate_case(case, parsed, &resource, Mapping::BaselineV1)?;
            let ext = evaluate_case(case, parsed, &resource, Mapping::PrepositionalDativeV1)?;
            let binding = output(&args.out, &format!("{name}/{id}/baseline.json"), &base)?;
            artifacts.push(binding.clone());
            let comparison = compare_case(&base, &ext, parsed)?;
            comparisons.push(save_comparison(
                &args.out,
                &format!("{name}/{id}"),
                &base,
                &ext,
                &comparison,
                binding,
                artifacts,
            )?);
            bases.push(base);
            extended.push(ext);
        }
        let mut summary =
            summarize_run(&args.out, &name, &bases, &extended, &comparisons, artifacts)?;
        summary["parse_and_write_seconds"] = json!(timing);
        println!("mapping comparison: {name} {}", summary["comparison"]);
        summaries.push(summary);
    }
    Ok(
        json!({"schema":"slopninja-lexical-frame-dative-study-report-v1","baseline_replay_cases":parity_cases,"baseline_replay_whole_case_equal":true,"new_annotations":new_annotations,"summaries":summaries,"human_semantic_ratings":0,"llm_calls":0,"detector_calls":0,"model_fit_calls":0,"automatic_edit_license":false,"semantic_equivalence_certified":false}),
    )
}

pub fn main() -> Result<()> {
    let args = Args::parse();
    let bytes = checked(&args.protocol, &args.expected_protocol_sha256)?;
    let protocol: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        protocol["schema"] == "slopninja-lexical-frame-dative-study-protocol-v1"
            && protocol["mappings"]
                == json!([
                    Mapping::BaselineV1.identity(),
                    Mapping::PrepositionalDativeV1.identity()
                ]),
        "Unknown protocol/mapping"
    );
    ensure!(
        hash(&fs::read(std::env::current_exe()?)?) == text(&protocol, "executable_sha256")?,
        "Study executable differs"
    );
    for source in rows(&protocol, "source_bindings")? {
        safe_relative(text(source, "path")?)?;
        bound_bytes(&args.repo, source)?;
    }
    bound_bytes(&args.repo, &protocol["historical_executable"])?;
    for source in rows(&protocol, "historical_baseline_sources")? {
        bound_bytes(&args.repo, source)?;
    }
    fs::create_dir(&args.out).context("Create fresh study output")?;
    let result = (|| -> Result<()> {
        let mut artifacts = vec![output(&args.out, "protocol.json", &protocol)?];
        for source in rows(&protocol, "source_bindings")? {
            let name = format!("executed-source/{}", text(source, "path")?);
            let file = args.out.join(&name);
            fs::create_dir_all(file.parent().context("Source parent")?)?;
            let bytes = bound_bytes(&args.repo, source)?;
            fs::write(&file, &bytes)?;
            artifacts.push(json!({"path":name,"sha256":hash(&bytes)}));
        }
        let mut report = execute(&args, &protocol, &mut artifacts)?;
        report["protocol_sha256"] = json!(args.expected_protocol_sha256);
        report["artifacts"] = json!(artifacts);
        let report_binding = output(&args.out, "report.json", &report)?;
        output(
            &args.out,
            "receipt.json",
            &json!({"schema":"slopninja-lexical-frame-dative-study-receipt-v1","status":"completed","protocol_sha256":args.expected_protocol_sha256,"report":report_binding}),
        )?;
        Ok(())
    })();
    if let Err(error) = &result {
        output(
            &args.out,
            "failure.json",
            &json!({"status":"failed","error":format!("{error:#}"),"protocol_sha256":args.expected_protocol_sha256}),
        )?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_row(observation: Value) -> Value {
        json!({"case":{"id":"synthetic"},"status":"evaluated","observation":observation,"expectations":[],"target_predicates":[]})
    }

    #[test]
    fn absent_observations_are_not_evidence_of_unchanged_grammar() {
        let mut row = empty_row(Value::Null);
        row["status"] = json!("parse_error");
        let result = compare_case(&row, &row, &Err("synthetic parse failure".into())).unwrap();
        assert_eq!(result["annotation_available"], false);
        assert_eq!(result["observation_body_changed"], Value::Null);
        assert_eq!(result["raw_ledger_equal"], Value::Null);
        assert_eq!(result["dative_evidence"], Value::Null);
        let summary = comparison_summary(&[result]);
        assert_eq!(summary["observation_comparisons_unavailable"], 1);
        assert_eq!(summary["observation_bodies_changed"], 0);
    }

    #[test]
    fn version_metadata_is_excluded_but_altered_raw_parent_is_rejected() {
        let base = empty_row(
            json!({"schema":"slopninja-lexical-frame-observation-v1","predicates":[{"head":{"token":{"head":1}},"frame_possibilities":[]}]}),
        );
        let mut ext = base.clone();
        ext["observation"]["schema"] = json!("slopninja-lexical-frame-observation-v2");
        ext["observation"]["mapping_identity"] = json!("prepositional_dative_v1");
        assert_eq!(stripped_observation(&base), stripped_observation(&ext));
        ext["observation"]["predicates"][0]["head"]["token"]["head"] = json!(2);
        assert!(compare_case(&base, &ext, &Err("not needed".into())).is_err());
    }

    #[test]
    fn unmatched_expectation_count_is_rejected_instead_of_truncated() {
        let base = empty_row(Value::Null);
        let mut ext = base.clone();
        ext["expectations"] =
            json!([{"expectation":{"kind":"role_binding"},"status":"unavailable"}]);
        assert!(compare_case(&base, &ext, &Err("not needed".into())).is_err());
    }

    #[test]
    fn missing_declared_anchor_is_unavailable_instead_of_a_failed_mapping() {
        let doc = Document {
            text: String::new(),
            parser_identity: "synthetic".into(),
            tokens: vec![],
            sentences: vec![],
        };
        let case = json!({"raw_mapping_applicability":{"policy_ref":"direct_adp_recipient_v1","parser_label_is_asserted":false,"predicate_span":[0,4],"carrier_span":[5,7],"recipient_span":[8,11],"frame_literal":"to"}});
        let result = applicability(&case, &Ok(doc)).unwrap();
        assert_eq!(result["status"], "unavailable_anchor");
        assert_eq!(result["baseline_to_slot"], Value::Null);
        assert_eq!(result["extended_to_slot"], Value::Null);
    }
}

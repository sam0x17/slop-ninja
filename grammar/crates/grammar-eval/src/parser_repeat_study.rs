//! Frozen, matched-text controls for parser repeatability.
use crate::parser_repeat_observation as observation;
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_core::{
    features,
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

const PREVIOUS: &str = "data/author-corpora/blog-authorship-2004/dative-arguments-v1";
const SCHEMA: &str = "slopninja-parser-repeat-protocol-v1";
const PROCESSES: [&str; 3] = ["canonical_a", "canonical_b", "reversed_c"];
type Parsed = std::result::Result<Document, String>;

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
fn binding(repo: &Path, name: &str) -> Result<Value> {
    safe(name)?;
    Ok(json!({"path":name,"sha256":hash(&fs::read(repo.join(name))?)}))
}
fn bytes(repo: &Path, b: &Value) -> Result<Vec<u8>> {
    checked(&repo.join(text(b, "path")?), text(b, "sha256")?)
}
fn bound(repo: &Path, b: &Value) -> Result<Value> {
    Ok(serde_json::from_slice(&bytes(repo, b)?)?)
}
fn rebase(prefix: &str, b: &Value) -> Result<Value> {
    safe(text(b, "path")?)?;
    let mut b = b.clone();
    b["path"] = json!(format!("{prefix}/{}", text(&b, "path")?));
    Ok(b)
}
fn output(out: &Path, name: &str, v: &Value) -> Result<Value> {
    safe(name)?;
    let target = out.join(name);
    fs::create_dir_all(target.parent().unwrap())?;
    let mut bytes = serde_json::to_vec_pretty(v)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(target)?
        .write_all(&bytes)?;
    Ok(json!({"path":name,"sha256":hash(&bytes)}))
}
fn verify_parser(repo: &Path, p: &Value) -> Result<()> {
    bytes(repo, &p["parser_python"])?;
    bytes(repo, &p["environment_manifest"])?;
    ensure!(
        !rows(p, "asset_bindings")?.is_empty(),
        "Missing parser assets"
    );
    for b in rows(p, "asset_bindings")? {
        bytes(repo, b)?;
    }
    Ok(())
}
fn artifact_index(report: &Value) -> Result<BTreeMap<String, Value>> {
    let mut index = BTreeMap::new();
    for b in rows(report, "artifacts")? {
        let name = text(b, "path")?;
        safe(name)?;
        ensure!(
            index.insert(name.into(), b.clone()).is_none(),
            "Duplicate artifact path"
        );
    }
    Ok(index)
}
fn reference(mut b: Value, format: &str) -> Value {
    b["format"] = json!(format);
    b
}
fn read_reference(base: &Path, b: &Value, sha: &str, parser: &str) -> Result<(String, Parsed)> {
    let v = bound(base, b)?;
    let body = match text(b, "format")? {
        "document" => &v,
        "wrapper" => {
            if v["status"] == "error" || v["status"] == "process_error" {
                let s = text(&v, "text")?.to_owned();
                ensure!(hash(s.as_bytes()) == sha, "Failed annotation text differs");
                return Ok((s, Err(text(&v, "error")?.into())));
            }
            ensure!(v["status"] == "ok", "Unknown annotation wrapper status");
            &v["document"]
        }
        _ => anyhow::bail!("Unknown annotation reference format"),
    };
    let doc: Document = serde_json::from_value(body.clone())?;
    syntax::validate(&doc)?;
    ensure!(
        hash(doc.text.as_bytes()) == sha && doc.parser_identity == parser,
        "Annotation text/parser identity differs"
    );
    Ok((doc.text.clone(), Ok(doc)))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Case {
    sha256: String,
    kind: String,
    byte_count: usize,
    word_count: usize,
    origins: Vec<Value>,
    references: BTreeMap<String, Value>,
}
fn register(
    cases: &mut BTreeMap<String, Case>,
    sha: &str,
    kind: &str,
    source: &str,
    origin: Value,
    refs: BTreeMap<String, Value>,
) -> Result<()> {
    ensure!(hash(source.as_bytes()) == sha, "Case source digest differs");
    let c = cases.entry(sha.into()).or_insert_with(|| Case {
        sha256: sha.into(),
        kind: kind.into(),
        byte_count: source.len(),
        word_count: features::words(source).len(),
        origins: Vec::new(),
        references: refs.clone(),
    });
    ensure!(
        c.kind == kind && c.byte_count == source.len() && c.references == refs,
        "Conflicting case identity/references"
    );
    if !c.origins.contains(&origin) {
        c.origins.push(origin);
    }
    Ok(())
}
fn select(
    repo: &Path,
    previous_protocol: &Value,
    report: &Value,
) -> Result<(BTreeMap<String, Case>, Vec<Value>)> {
    let root = format!("{PREVIOUS}/run-v1/corpus");
    let artifacts = artifact_index(report)?;
    let parsers = rows(previous_protocol, "parsers")?;
    ensure!(
        parsers.len() == 2
            && parsers[0]["id"] == "incumbent_sm"
            && parsers[1]["id"] == "isolated_trf",
        "Parser catalog differs"
    );
    let small = text(&parsers[0], "parser_identity")?;
    let primary = text(&parsers[1], "parser_identity")?;
    let mut inputs = BTreeMap::new();
    let mut cases = BTreeMap::new();
    let mut proposal_ids = BTreeSet::new();
    for (name, b) in artifacts
        .iter()
        .filter(|(n, _)| n.starts_with("alignments/isolated_trf/"))
    {
        let ab = rebase(&root, b)?;
        let a = bound(repo, &ab)?;
        inputs.insert(name.clone(), ab.clone());
        let id = text(&a["proposal"], "id")?;
        let input = &a["input"];
        let source_sha = text(&input["post"], "text_sha256")?;
        ensure!(
            proposal_ids.insert((source_sha.to_owned(), id.to_owned())),
            "Repeated primary proposal occurrence"
        );
        ensure!(input["post"]["split"] == "train", "Non-TRAIN source");
        let ob_name = format!("observations/isolated_trf/{source_sha}.json");
        let ob = rebase(
            &root,
            artifacts
                .get(&ob_name)
                .context("Missing source observation")?,
        )?;
        let source_observation = bound(repo, &ob)?;
        inputs.insert(ob_name, ob);
        ensure!(
            source_observation["input"] == *input && source_observation["status"] == "observed",
            "Source metadata differs"
        );
        let primary_ref = reference(source_observation["source_annotation"].clone(), "wrapper");
        ensure!(
            primary_ref["storage"] == "bound_prior_corpus",
            "Expected pinned prior transformer source"
        );
        let small_ref = reference(
            json!({"path":input["annotation_path"],"sha256":input["annotation_sha256"]}),
            "document",
        );
        let (source, _) = read_reference(repo, &primary_ref, source_sha, primary)?;
        ensure!(
            read_reference(repo, &small_ref, source_sha, small)?.0 == source,
            "Parsers refer to different source bytes"
        );
        let source_refs = BTreeMap::from([
            ("isolated_trf".into(), primary_ref),
            ("incumbent_sm".into(), small_ref),
        ]);
        let origin =
            json!({"post":input["post"],"panel":input["panel"],"proposal_id":id,"alignment":ab});
        register(
            &mut cases,
            source_sha,
            "source",
            &source,
            origin.clone(),
            source_refs,
        )?;
        let candidate = text(&a["proposal"]["candidate"], "text")?;
        let candidate_sha = hash(candidate.as_bytes());
        let candidate_name = text(&a["candidate_annotation"], "path")?;
        ensure!(
            artifacts.get(candidate_name) == Some(&a["candidate_annotation"]),
            "Candidate binding not in executed report"
        );
        let candidate_ref = reference(rebase(&root, &a["candidate_annotation"])?, "wrapper");
        ensure!(
            read_reference(repo, &candidate_ref, &candidate_sha, primary)?.0 == candidate,
            "Candidate annotation bytes differ"
        );
        let mut refs = BTreeMap::from([("isolated_trf".into(), candidate_ref)]);
        let small_name = format!("candidate-annotations/incumbent_sm/{candidate_sha}.json");
        if let Some(b) = artifacts.get(&small_name) {
            let r = reference(rebase(&root, b)?, "wrapper");
            ensure!(
                read_reference(repo, &r, &candidate_sha, small)?.0 == candidate,
                "Small candidate bytes differ"
            );
            refs.insert("incumbent_sm".into(), r);
        }
        register(
            &mut cases,
            &candidate_sha,
            "candidate",
            candidate,
            origin,
            refs,
        )?;
    }
    ensure!(
        proposal_ids.len() == 60
            && cases.len() == 117
            && cases.values().filter(|c| c.kind == "source").count() == 57
            && cases.values().filter(|c| c.kind == "candidate").count() == 60,
        "Control selection budget differs"
    );
    Ok((cases, inputs.into_values().collect()))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Item {
    sha256: String,
    repeat: usize,
    index: usize,
}
fn schedule(cases: &BTreeMap<String, Case>, reversed: bool) -> Vec<Item> {
    let mut keys = cases.keys().cloned().collect::<Vec<_>>();
    if reversed {
        keys.reverse();
    }
    keys.into_iter()
        .flat_map(|sha| (0..2).map(move |repeat| (sha.clone(), repeat)))
        .enumerate()
        .map(|(index, (sha256, repeat))| Item {
            sha256,
            repeat,
            index,
        })
        .collect()
}
fn freeze(repo: &Path, out: &Path) -> Result<()> {
    let predecessor = binding(repo, &format!("{PREVIOUS}/final-checks-v1.json"))?;
    let final_checks = bound(repo, &predecessor)?;
    ensure!(
        final_checks["stage_completed"] == true,
        "Predecessor incomplete"
    );
    let old_binding = rebase(PREVIOUS, &final_checks["protocol"])?;
    let old = bound(repo, &old_binding)?;
    let old_report_binding = rebase(PREVIOUS, &final_checks["report"])?;
    let old_report = bound(repo, &old_report_binding)?;
    let corpus_binding = rebase(&format!("{PREVIOUS}/run-v1"), &old_report["corpus_report"])?;
    let corpus_report = bound(repo, &corpus_binding)?;
    let (cases, selection_inputs) = select(repo, &old, &corpus_report)?;
    let mut names = BTreeSet::new();
    for b in rows(&old, "source_bindings")? {
        bytes(repo, b)?;
        names.insert(text(b, "path")?.to_owned());
    }
    for name in [
        "parser_repeat_study.rs",
        "parser_repeat_observation.rs",
        "bin/slop_ninja-parser-repeat.rs",
    ] {
        names.insert(format!("grammar/crates/grammar-eval/src/{name}"));
    }
    let p = json!({"schema":SCHEMA,"executable_sha256":hash(&fs::read(std::env::current_exe()?)?),
        "source_bindings":names.iter().map(|n|binding(repo,n)).collect::<Result<Vec<_>>>()?,
        "predecessor":predecessor,"previous_protocol":old_binding,"previous_report":old_report_binding,"previous_corpus_report":corpus_binding,
        "selection_inputs":selection_inputs,"parsers":old["parsers"],"cases":cases.values().collect::<Vec<_>>(),
        "processes":PROCESSES.iter().enumerate().map(|(i,id)|json!({"id":id,"order":if i==2{"descending_sha256"}else{"ascending_sha256"},"schedule":schedule(&cases,i==2)})).collect::<Vec<_>>(),
        "case_count":117,"source_texts":57,"candidate_texts":60,"requests_per_process":234,"annotations_per_parser":702,"total_annotation_requests":1404,
        "selection":"Every primary proposal source and candidate from the completed dative argument corpus run, irrespective of comparison outcome; matched text set for both parsers",
        "process_contract":"One unchanged parse_batch call per schedule; adjacent duplicates retained, no cache reuse, chunking or parallel interpreter calls",
        "comparisons":["Adjacent repeat0/repeat1 for every text in every process","Canonical_b versus canonical_a at both repeat positions","Reversed_c versus canonical_a at both repeat positions; fresh-process order-associated contrast","Saved historical reference versus canonical_a repeat0; unavailable references retained"],
        "primary_measure":"Exact validated Document value equality on identical text under identical parser identity",
        "interpretation":"Observed repeatability on a selected text set; reversed schedule does not isolate order causation from a fresh-process effect; no general determinism or edit-causation claim",
        "runtime_changes":false,"fit_models":false,"later_posts":0,"llm_calls":0,"detector_calls":0,"automatic_edit_license":false,"semantic_equivalence_certified":false});
    fs::create_dir(out)?;
    println!("{}", output(out, "protocol.json", &p)?);
    Ok(())
}

fn parsed_value(parsed: &Parsed, source: &str) -> Value {
    match parsed {
        Ok(d) => json!({"status":"ok","document":d}),
        Err(e) => json!({"status":"error","text":source,"error":e}),
    }
}
#[allow(clippy::too_many_arguments)]
fn compare_pair(
    repo: &Path,
    out: &Path,
    parser: &Value,
    case: &Case,
    kind: &str,
    process: &str,
    repeat: usize,
    left: Option<(&Path, &Value)>,
    right: &Value,
    artifacts: &mut Vec<Value>,
) -> Result<Value> {
    let identity = text(parser, "parser_identity")?;
    let right_doc = read_reference(out, right, &case.sha256, identity)?.1;
    let left_doc = left
        .map(|(base, b)| read_reference(base, b, &case.sha256, identity).map(|(_, d)| d))
        .transpose()?;
    let result = match (&left_doc, &right_doc) {
        (Some(Ok(a)), Ok(b)) => {
            json!({"status":"compared","observation":observation::compare(a,b)?})
        }
        _ => {
            json!({"status":"unavailable","left_reference_missing":left.is_none(),"left_error":left_doc.as_ref().and_then(|d|d.as_ref().err()),"right_error":right_doc.as_ref().err()})
        }
    };
    let left_scope = left.map(|(base, _)| if base == repo { "repository" } else { "run" });
    let pointer = output(
        out,
        &format!(
            "comparisons/{}/{kind}/{process}/{}-r{repeat}.json",
            text(parser, "id")?,
            case.sha256
        ),
        &json!({"case_sha256":case.sha256,"case_kind":case.kind,"contrast":kind,"process":process,"repeat":repeat,"left":left.map(|(_,b)|b),"left_root":left_scope,"right":right,"result":result}),
    )?;
    artifacts.push(pointer.clone());
    Ok(
        json!({"case_sha256":case.sha256,"case_kind":case.kind,"contrast":kind,"process":process,"repeat":repeat,"status":result["status"],"exact_document_equal":result["observation"]["exact_document_equal"],"counts":result["observation"]["counts"],"report":pointer}),
    )
}
fn summaries(comparisons: &[Value]) -> Value {
    let mut groups = BTreeMap::new();
    for name in [
        "within_process",
        "same_schedule_restart",
        "reversed_order_fresh_process",
        "historical_reference",
    ] {
        let selected = comparisons
            .iter()
            .filter(|c| c["contrast"] == name)
            .collect::<Vec<_>>();
        groups.insert(name,json!({"requested":selected.len(),"compared":selected.iter().filter(|c|c["status"]=="compared").count(),"identical":selected.iter().filter(|c|c["exact_document_equal"]==true).count(),"different":selected.iter().filter(|c|c["exact_document_equal"]==false).count(),"unavailable":selected.iter().filter(|c|c["status"]!="compared").count()}));
    }
    json!(groups)
}
fn execute(repo: &Path, out: &Path, p: &Value, artifacts: &mut Vec<Value>) -> Result<Value> {
    let cases: Vec<Case> = serde_json::from_value(p["cases"].clone())?;
    let cases = cases
        .into_iter()
        .map(|c| (c.sha256.clone(), c))
        .collect::<BTreeMap<_, _>>();
    ensure!(cases.len() == 117, "Case catalog differs");
    let parsers = rows(p, "parsers")?;
    ensure!(
        parsers.len() == 2
            && parsers[0]["id"] == "incumbent_sm"
            && parsers[1]["id"] == "isolated_trf",
        "Parser order differs"
    );
    let mut texts = BTreeMap::new();
    for c in cases.values() {
        let (s, _) = read_reference(
            repo,
            c.references
                .get("isolated_trf")
                .context("Missing primary content reference")?,
            &c.sha256,
            text(&parsers[1], "parser_identity")?,
        )?;
        ensure!(
            s.len() == c.byte_count && features::words(&s).len() == c.word_count,
            "Case size differs"
        );
        texts.insert(c.sha256.clone(), s);
    }
    let processes = rows(p, "processes")?;
    ensure!(processes.len() == 3, "Process budget differs");
    let mut reports = Vec::new();
    for parser in parsers {
        let variant = text(parser, "id")?;
        let mut comparisons = Vec::new();
        let mut process_reports = Vec::new();
        let mut canonical = BTreeMap::<(String, usize), Value>::new();
        for (process_index, process) in processes.iter().enumerate() {
            let id = text(process, "id")?;
            ensure!(id == PROCESSES[process_index], "Process order differs");
            let requested: Vec<Item> = serde_json::from_value(process["schedule"].clone())?;
            ensure!(
                requested == schedule(&cases, process_index == 2) && requested.len() == 234,
                "Repeat schedule differs"
            );
            verify_parser(repo, parser)?;
            println!("{variant}/{id}: parsing all 234 scheduled occurrences in one interpreter");
            let started = Instant::now();
            let input = requested
                .iter()
                .map(|i| texts[&i.sha256].as_str())
                .collect::<Vec<_>>();
            let batch = grammar_spacy::parse_batch(
                &input,
                &repo
                    .join(text(&parser["parser_python"], "path")?)
                    .to_string_lossy(),
            );
            let parse_seconds = started.elapsed().as_secs_f64();
            let (parsed, process_error) = match batch {
                Ok(v) => (v, None),
                Err(e) => {
                    let s = format!("{e:#}");
                    (
                        (0..requested.len()).map(|_| Err(s.clone())).collect(),
                        Some(s),
                    )
                }
            };
            ensure!(
                parsed.len() == requested.len(),
                "Parser batch count differs"
            );
            verify_parser(repo, parser)?;
            let mut refs = BTreeMap::new();
            let mut annotation_errors = 0;
            for (item, doc) in requested.iter().zip(&parsed) {
                if let Ok(d) = doc {
                    syntax::validate(d)?;
                    ensure!(
                        d.text == texts[&item.sha256]
                            && d.parser_identity == parser["parser_identity"],
                        "Fresh parser output identity differs"
                    );
                } else {
                    annotation_errors += 1;
                }
                let mut value = parsed_value(doc, &texts[&item.sha256]);
                value["schedule_item"] = json!(item);
                value["process"] = json!(id);
                if process_error.is_some() {
                    value["status"] = json!("process_error");
                }
                let b = output(
                    out,
                    &format!(
                        "annotations/{variant}/{id}/{}-r{}.json",
                        item.sha256, item.repeat
                    ),
                    &value,
                )?;
                artifacts.push(b.clone());
                ensure!(
                    refs.insert((item.sha256.clone(), item.repeat), reference(b, "wrapper"))
                        .is_none(),
                    "Repeated schedule occurrence key"
                );
            }
            drop(parsed);
            for c in cases.values() {
                comparisons.push(compare_pair(
                    repo,
                    out,
                    parser,
                    c,
                    "within_process",
                    id,
                    1,
                    Some((out, &refs[&(c.sha256.clone(), 0)])),
                    &refs[&(c.sha256.clone(), 1)],
                    artifacts,
                )?);
                if process_index == 0 {
                    comparisons.push(compare_pair(
                        repo,
                        out,
                        parser,
                        c,
                        "historical_reference",
                        id,
                        0,
                        c.references.get(variant).map(|b| (repo, b)),
                        &refs[&(c.sha256.clone(), 0)],
                        artifacts,
                    )?);
                } else {
                    let kind = if process_index == 1 {
                        "same_schedule_restart"
                    } else {
                        "reversed_order_fresh_process"
                    };
                    for repeat in 0..2 {
                        comparisons.push(compare_pair(
                            repo,
                            out,
                            parser,
                            c,
                            kind,
                            id,
                            repeat,
                            Some((out, &canonical[&(c.sha256.clone(), repeat)])),
                            &refs[&(c.sha256.clone(), repeat)],
                            artifacts,
                        )?);
                    }
                }
            }
            if process_index == 0 {
                canonical = refs;
            }
            let progress = json!({"id":id,"annotation_requests":234,"annotation_errors":annotation_errors,"process_error":process_error,"parse_seconds":parse_seconds,"elapsed_seconds":started.elapsed().as_secs_f64(),"cumulative_comparisons":summaries(&comparisons)});
            artifacts.push(output(
                out,
                &format!("processes/{variant}/{id}.json"),
                &progress,
            )?);
            println!(
                "{variant}/{id}: retained 234 annotations ({annotation_errors} unavailable); comparisons {}",
                summaries(&comparisons)
            );
            process_reports.push(progress);
        }
        ensure!(comparisons.len() == 936, "Comparison allocation differs");
        let report = json!({"variant":variant,"primary":variant=="isolated_trf","case_count":117,"source_texts":57,"candidate_texts":60,"annotation_requests":702,"processes":process_reports,"summary":summaries(&comparisons),"comparisons":comparisons});
        let b = output(out, &format!("{variant}/report.json"), &report)?;
        artifacts.push(b.clone());
        reports.push(json!({"variant":variant,"summary":report["summary"],"processes":report["processes"],"report":b}));
    }
    Ok(
        json!({"schema":"slopninja-parser-repeat-report-v1","reports":reports,"matched_texts":117,"total_annotation_requests":1404,"repeatability_is_not_general_determinism":true,"edit_causation_established":false,"fit_models":false,"later_posts":0,"llm_calls":0,"detector_calls":0,"automatic_edit_license":false,"semantic_equivalence_certified":false}),
    )
}
fn run(repo: &Path, file: &Path, expected: &str, out: &Path) -> Result<()> {
    let p: Value = serde_json::from_slice(&checked(file, expected)?)?;
    ensure!(
        p["schema"] == SCHEMA
            && p["total_annotation_requests"] == 1404
            && p["runtime_changes"] == false
            && p["fit_models"] == false
            && p["later_posts"] == 0
            && p["executable_sha256"] == hash(&fs::read(std::env::current_exe()?)?),
        "Protocol/executable differs"
    );
    for b in rows(&p, "source_bindings")? {
        safe(text(b, "path")?)?;
        bytes(repo, b)?;
    }
    ensure!(
        bound(repo, &p["predecessor"])?["stage_completed"] == true,
        "Predecessor incomplete"
    );
    for b in rows(&p, "selection_inputs")? {
        bytes(repo, b)?;
    }
    fs::create_dir(out)?;
    let result = (|| -> Result<()> {
        let mut artifacts = vec![output(out, "protocol.json", &p)?];
        for b in rows(&p, "source_bindings")? {
            let name = format!("executed-source/{}", text(b, "path")?);
            let target = out.join(&name);
            fs::create_dir_all(target.parent().unwrap())?;
            let source = bytes(repo, b)?;
            fs::write(target, &source)?;
            artifacts.push(json!({"path":name,"sha256":hash(&source)}));
        }
        let mut report = execute(repo, out, &p, &mut artifacts)?;
        report["protocol_sha256"] = json!(expected);
        report["artifacts"] = json!(artifacts);
        let b = output(out, "report.json", &report)?;
        output(
            out,
            "receipt.json",
            &json!({"schema":"slopninja-parser-repeat-receipt-v1","status":"completed","protocol_sha256":expected,"report":b}),
        )?;
        Ok(())
    })();
    if let Err(e) = &result {
        output(
            out,
            "failure.json",
            &json!({"status":"failed","protocol_sha256":expected,"error":format!("{e:#}")}),
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
    use grammar_core::edits;
    fn cases() -> BTreeMap<String, Case> {
        ["b", "a"]
            .into_iter()
            .map(|s| {
                (
                    s.into(),
                    Case {
                        sha256: s.into(),
                        kind: "source".into(),
                        byte_count: 1,
                        word_count: 1,
                        origins: vec![],
                        references: BTreeMap::new(),
                    },
                )
            })
            .collect()
    }
    #[test]
    fn schedules_preserve_adjacent_repetitions_and_restart_positions() {
        let c = cases();
        let a = schedule(&c, false);
        let b = schedule(&c, false);
        let r = schedule(&c, true);
        assert_eq!(a, b);
        assert_eq!(
            a.iter()
                .map(|i| (i.sha256.as_str(), i.repeat, i.index))
                .collect::<Vec<_>>(),
            vec![("a", 0, 0), ("a", 1, 1), ("b", 0, 2), ("b", 1, 3)]
        );
        assert_eq!(
            r.iter()
                .map(|i| (i.sha256.as_str(), i.repeat))
                .collect::<Vec<_>>(),
            vec![("b", 0), ("b", 1), ("a", 0), ("a", 1)]
        );
    }
    #[test]
    fn failed_saved_annotation_is_available_as_error_not_zero_difference() {
        let d = tempfile::tempdir().unwrap();
        let source = "A failed control.";
        let sha = edits::digest(source);
        let b = reference(
            output(
                d.path(),
                "error.json",
                &json!({"status":"error","text":source,"error":"unavailable"}),
            )
            .unwrap(),
            "wrapper",
        );
        let (s, result) = read_reference(d.path(), &b, &sha, "parser").unwrap();
        assert_eq!(s, source);
        assert_eq!(result.unwrap_err(), "unavailable");
        assert!(read_reference(d.path(), &b, &edits::digest("different"), "parser").is_err());
    }
    #[test]
    fn missing_comparisons_and_observed_differences_have_separate_counts() {
        let values = vec![
            json!({"contrast":"within_process","status":"unavailable","exact_document_equal":null}),
            json!({"contrast":"within_process","status":"compared","exact_document_equal":false}),
            json!({"contrast":"within_process","status":"compared","exact_document_equal":true}),
        ];
        let s = summaries(&values);
        assert_eq!(
            s["within_process"],
            json!({"requested":3,"compared":2,"identical":1,"different":1,"unavailable":1})
        );
    }
}

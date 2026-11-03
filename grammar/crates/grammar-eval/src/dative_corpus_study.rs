//! Corpus opportunity audit; construction preference fitting is a later gate.
use crate::{
    dative_alternation as alternation, dative_corpus_observation as observation,
    dative_corpus_support as support, lexical_frame_study as study, verbnet_resource::Resource,
};
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_core::{
    edits,
    syntax::{self, Document},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Instant,
};
use study::{bound, bound_bytes, checked, hash, output, rows, safe_relative, text};

const BASE: &str = "data/author-corpora/blog-authorship-2004";
const SCHEMA: &str = "slopninja-dative-corpus-protocol-v1";
const BATCH: usize = 64;
pub(crate) type Parsed = std::result::Result<Document, String>;

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
fn binding(repo: &Path, name: &str) -> Result<Value> {
    Ok(json!({"path":name,"sha256":hash(&fs::read(repo.join(name))?)}))
}
fn freeze(repo: &Path, out: &Path) -> Result<()> {
    let predecessor = binding(
        repo,
        &format!("{BASE}/dative-alternation-v1/final-checks-v1.json"),
    )?;
    let old_binding = binding(
        repo,
        &format!("{BASE}/dative-alternation-v1/protocol-v1/protocol.json"),
    )?;
    let old = bound(repo, &old_binding)?;
    let mut names = BTreeSet::new();
    for source in rows(&old, "source_bindings")? {
        bound_bytes(repo, source)?;
        names.insert(text(source, "path")?.to_owned());
    }
    for name in [
        "dative_corpus_study.rs",
        "dative_corpus_observation.rs",
        "dative_corpus_support.rs",
        "bin/slop_ninja-dative-corpus.rs",
    ] {
        names.insert(format!("grammar/crates/grammar-eval/src/{name}"));
    }
    let parsers = rows(&old, "parsers")?
        .iter()
        .filter(|p| matches!(p["id"].as_str(), Some("incumbent_sm" | "isolated_trf")))
        .cloned()
        .collect::<Vec<_>>();
    ensure!(parsers.len() == 2, "Parser catalog differs");
    let resource = study::resource(repo, &bound(repo, &old["resource_manifest"])?)?;
    let p = json!({"schema":SCHEMA,"executable_sha256":hash(&fs::read(std::env::current_exe()?)?),
        "source_bindings":names.iter().map(|n|binding(repo,n)).collect::<Result<Vec<_>>>()?,
        "predecessor":predecessor,"prior_protocol":old_binding,"resource_manifest":old["resource_manifest"],"registry":observation::registry(&resource)?,
        "training_metadata":binding(repo,&format!("{BASE}/preference-chronology-v1/metadata-v1/training-posts.json"))?,
        "training_metadata_receipt":binding(repo,&format!("{BASE}/preference-chronology-v1/metadata-v1/receipt.json"))?,
        "expected_posts":3889,"expected_authors":300,"expected_source_groups":3375,
        "expected_panel_posts":{"original":1224,"replication":1363,"confirmation":1302},
        "parsers":parsers,"primary_parser":"isolated_trf","sensitivity_parser":"incumbent_sm",
        "source_selection":"All 3889 bound TRAIN whole posts for both parsers; no exposure or proposal screen",
        "source_cache":"Reuse exact bound incumbent annotations; parse all transformer source texts",
        "parser_batch_size":BATCH,"candidate_selection":"Every original single proposal; unique candidate bytes parsed once per parser; retain every membership and occurrence",
        "eligibility":"One unique complete registered source frame form, original source proposal, forward observed_preserved alignment, actual reverse proposal restoring exact original bytes under the same membership, reverse observed_preserved alignment",
        "event_counting":"One anchored source predicate per post and parser; preserve all duplicate resource/proposal hypotheses separately",
        "support_policy":support::policy(),"fit_models":false,"later_posts":0,"llm_calls":0,"detector_calls":0,"automatic_edit_license":false});
    fs::create_dir(out)?;
    println!("{}", output(out, "protocol.json", &p)?);
    Ok(())
}

fn metadata(value: &Value, p: &Value) -> Result<Vec<Value>> {
    let values = value.as_array().context("TRAIN metadata array")?;
    let mut ids = BTreeSet::new();
    let mut shas = BTreeSet::new();
    let mut groups = BTreeSet::new();
    let mut authors = BTreeMap::new();
    let mut panels = BTreeMap::<String, usize>::new();
    for b in values {
        let post = &b["post"];
        let author = text(post, "author")?;
        let date = text(post, "date")?;
        let panel = text(b, "panel")?;
        grammar_eval::validate_date(date)?;
        ensure!(
            post["split"] == "train"
                && post["source_group"] == format!("blog:{author}:date:{date}"),
            "TRAIN source group differs"
        );
        ensure!(
            ids.insert(text(post, "id")?) && shas.insert(text(post, "text_sha256")?),
            "Repeated TRAIN identity/hash"
        );
        safe_relative(text(b, "annotation_path")?)?;
        text(b, "annotation_sha256")?;
        if let Some(previous) = authors.insert(author, panel) {
            ensure!(previous == panel, "Author crosses panels");
        }
        groups.insert(text(post, "source_group")?);
        *panels.entry(panel.into()).or_default() += 1;
    }
    ensure!(
        values.len()
            == p["expected_posts"]
                .as_u64()
                .context("Expected post count")? as usize
            && authors.len()
                == p["expected_authors"].as_u64().context("Expected authors")? as usize
            && groups.len()
                == p["expected_source_groups"]
                    .as_u64()
                    .context("Expected groups")? as usize
            && json!(panels) == p["expected_panel_posts"],
        "Bound TRAIN coverage differs"
    );
    Ok(values.clone())
}
fn cached(repo: &Path, input: &Value, parser: &str) -> Result<Document> {
    let d: Document = serde_json::from_slice(&checked(
        &repo.join(text(input, "annotation_path")?),
        text(input, "annotation_sha256")?,
    )?)?;
    syntax::validate(&d)?;
    ensure!(
        d.parser_identity == parser && hash(d.text.as_bytes()) == input["post"]["text_sha256"],
        "Cached document identity differs"
    );
    Ok(d)
}
fn saved_document(
    repo: &Path,
    out: &Path,
    input: &Value,
    source: &Value,
    parser: &str,
) -> Result<Document> {
    saved_parsed(repo, out, input, source, parser)?
        .map_err(|e| anyhow::anyhow!("Expected available saved source annotation: {e}"))
}
fn saved_parsed(
    repo: &Path,
    out: &Path,
    input: &Value,
    source: &Value,
    parser: &str,
) -> Result<Parsed> {
    if source["storage"] == "bound_incumbent_cache" {
        return cached(repo, input, parser).map(Ok);
    }
    let value = bound(
        if source["storage"] == "bound_prior_corpus" {
            repo
        } else {
            out
        },
        source,
    )?;
    if value["status"] == "error" {
        ensure!(
            hash(text(&value, "text")?.as_bytes()) == input["post"]["text_sha256"],
            "Saved failed source identity differs"
        );
        return Ok(Err(text(&value, "error")?.to_owned()));
    }
    ensure!(
        value["status"] == "ok",
        "Unknown saved source annotation status"
    );
    let doc: Document = serde_json::from_value(value["document"].clone())?;
    syntax::validate(&doc)?;
    ensure!(
        doc.parser_identity == parser && hash(doc.text.as_bytes()) == input["post"]["text_sha256"],
        "Saved source identity differs"
    );
    Ok(Ok(doc))
}
pub(crate) fn parse(
    repo: &Path,
    parser: &Value,
    requested: &[(String, String)],
) -> Result<Vec<Parsed>> {
    let input = requested
        .iter()
        .map(|(_, s)| s.as_str())
        .collect::<Vec<_>>();
    let parsed = grammar_spacy::parse_batch(
        &input,
        &repo
            .join(text(&parser["parser_python"], "path")?)
            .to_string_lossy(),
    )?;
    ensure!(
        parsed.len() == requested.len(),
        "Parser output count differs"
    );
    for ((sha, source), result) in requested.iter().zip(&parsed) {
        if let Ok(d) = result {
            syntax::validate(d)?;
            ensure!(
                d.text == *source
                    && hash(d.text.as_bytes()) == *sha
                    && d.parser_identity == parser["parser_identity"],
                "Fresh annotation identity differs"
            );
        }
    }
    Ok(parsed)
}
fn save_annotation(
    out: &Path,
    name: &str,
    source: &str,
    parsed: &Parsed,
    artifacts: &mut Vec<Value>,
) -> Result<Value> {
    let value = match parsed {
        Ok(d) => json!({"status":"ok","document":d}),
        Err(e) => json!({"status":"error","text":source,"error":e}),
    };
    let b = output(out, name, &value)?;
    artifacts.push(b.clone());
    Ok(b)
}
fn support_post(input: &Value, available: bool) -> Result<support::SupportPost> {
    let p = &input["post"];
    Ok(support::SupportPost {
        id: text(p, "id")?.into(),
        author: text(p, "author")?.into(),
        date: text(p, "date")?.into(),
        source_group: text(p, "source_group")?.into(),
        panel: text(input, "panel")?.into(),
        annotation_available: available,
        assessment_complete: available,
        choices: Vec::new(),
    })
}
fn choice(
    post: &Value,
    doc: &Document,
    index: usize,
    double_object: bool,
) -> Result<support::Choice> {
    let token = &doc.tokens[index];
    Ok(support::Choice {
        event_id: serde_json::to_string(&(text(post, "id")?, token.start_byte, token.end_byte))?,
        lemma: crate::lexical_context::normalized_lemma(token)
            .context("Missing registered event lemma")?,
        double_object,
    })
}
struct Pending {
    post_index: usize,
    source: Value,
    proposals: Vec<alternation::Proposal>,
}
struct Panel {
    frames: Vec<support::SupportPost>,
    source_proposals: Vec<support::SupportPost>,
    verified: Vec<support::SupportPost>,
    pending: Vec<Pending>,
    source_summaries: Vec<Value>,
    alignments: Vec<Value>,
    source_errors: usize,
    candidate_errors: usize,
    comparison_errors: usize,
    source_annotations: usize,
    candidate_annotations: usize,
}
impl Panel {
    fn new() -> Self {
        Self {
            frames: Vec::new(),
            source_proposals: Vec::new(),
            verified: Vec::new(),
            pending: Vec::new(),
            source_summaries: Vec::new(),
            alignments: Vec::new(),
            source_errors: 0,
            candidate_errors: 0,
            comparison_errors: 0,
            source_annotations: 0,
            candidate_annotations: 0,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn inspect(
    input: &Value,
    post_index: usize,
    parsed: &Parsed,
    source: Value,
    resource: &Resource,
    variant: &str,
    out: &Path,
    artifacts: &mut Vec<Value>,
    panel: &mut Panel,
    policy: alternation::ArgumentPolicy,
) -> Result<()> {
    let mut frames = support_post(input, parsed.is_ok())?;
    let mut proposals = frames.clone();
    let verified = frames.clone();
    match parsed {
        Ok(doc) => {
            let observed = observation::observe_with_policy(doc, resource, policy)?;
            let mut frame_events = BTreeMap::new();
            for event in rows(&observed.report, "event_summaries")? {
                let form = text(event, "frame_form")?;
                if !matches!(form, "prepositional" | "double_object") {
                    continue;
                }
                let index = event["predicate_index"]
                    .as_u64()
                    .context("Event predicate index")? as usize;
                ensure!(
                    frame_events
                        .insert(index, form == "double_object")
                        .is_none(),
                    "Duplicate observed event"
                );
                frames
                    .choices
                    .push(choice(&input["post"], doc, index, form == "double_object")?);
            }
            let mut proposal_events = BTreeSet::new();
            for p in &observed.proposals {
                if let Some(&double_object) = frame_events.get(&p.predicate_index)
                    && proposal_events.insert(p.predicate_index)
                {
                    proposals.choices.push(choice(
                        &input["post"],
                        doc,
                        p.predicate_index,
                        double_object,
                    )?);
                }
            }
            let saved = output(
                out,
                &format!(
                    "observations/{variant}/{}.json",
                    text(&input["post"], "text_sha256")?
                ),
                &json!({"input":input,"source_annotation":source,"status":"observed","observation":observed.report}),
            )?;
            artifacts.push(saved.clone());
            panel.source_summaries.push(json!({"post":input["post"],"panel":input["panel"],"status":"observed","report":saved,"lemma_exposures":observed.report["registered_lemma_token_count"],"frame_events":frames.choices.len(),"proposal_events":proposals.choices.len(),"proposals":observed.proposals.len()}));
            if !observed.proposals.is_empty() {
                panel.pending.push(Pending {
                    post_index,
                    source,
                    proposals: observed.proposals,
                });
            }
        }
        Err(error) => {
            panel.source_errors += 1;
            let b = output(
                out,
                &format!(
                    "observations/{variant}/{}.json",
                    text(&input["post"], "text_sha256")?
                ),
                &json!({"input":input,"source_annotation":source,"status":"parse_error","error":error}),
            )?;
            artifacts.push(b.clone());
            panel.source_summaries.push(json!({"post":input["post"],"panel":input["panel"],"status":"parse_error","report":b,"frame_events":null,"proposal_events":null,"proposals":null}));
        }
    }
    panel.frames.push(frames);
    panel.source_proposals.push(proposals);
    panel.verified.push(verified);
    Ok(())
}

pub(crate) fn match_pair_with_policy(
    source: &Document,
    candidate: &Document,
    proposal: &alternation::Proposal,
    resource: &Resource,
    policy: alternation::ArgumentPolicy,
) -> Result<Value> {
    let forward = alternation::compare(source, candidate, proposal, resource)?;
    let reverse_report = alternation::propose_with_policy(candidate, resource, policy)?;
    let mapped_head = forward
        .token_alignment
        .iter()
        .find(|t| t.source_index == proposal.predicate_index)
        .map(|t| t.candidate_index);
    let mut reverses = Vec::new();
    let mut eligible = false;
    let mut assessment_complete = !forward
        .findings
        .iter()
        .any(|f| f.outcome == alternation::Outcome::Unresolved);
    for reverse in &reverse_report.proposals {
        let restores = reverse.candidate.text == source.text
            && reverse.member_class_id == proposal.member_class_id
            && reverse.member_ordinal == proposal.member_ordinal
            && reverse.source_frame_id == proposal.target_frame_id
            && reverse.target_frame_id == proposal.source_frame_id
            && Some(reverse.predicate_index) == mapped_head;
        if restores {
            ensure!(
                edits::apply(&candidate.text, &reverse.candidate.edits)? == source.text,
                "Restoring reverse bytes differ"
            );
            let comparison = alternation::compare(candidate, source, reverse, resource)?;
            assessment_complete &= !comparison
                .findings
                .iter()
                .any(|f| f.outcome == alternation::Outcome::Unresolved);
            eligible |= forward.outcome == alternation::Outcome::ObservedPreserved
                && comparison.outcome == alternation::Outcome::ObservedPreserved;
            reverses.push(json!({"proposal_id":reverse.id,"alignment":comparison}));
        }
    }
    Ok(
        json!({"forward":forward,"reverse_proposal_report":reverse_report,"exact_same_membership_reverse_matches":reverses,"reciprocally_observed_preserved":eligible,"assessment_complete":assessment_complete,"automatic_edit_license":false,"semantic_equivalence_certified":false}),
    )
}

#[allow(clippy::too_many_arguments)]
fn candidates(
    repo: &Path,
    out: &Path,
    parser: &Value,
    training: &[Value],
    resource: &Resource,
    panel: &mut Panel,
    artifacts: &mut Vec<Value>,
    policy: alternation::ArgumentPolicy,
) -> Result<()> {
    let variant = text(parser, "id")?;
    let identity = text(parser, "parser_identity")?;
    let mut texts = BTreeMap::<String, String>::new();
    let mut origins = BTreeMap::<String, Vec<(usize, usize)>>::new();
    for (i, pending) in panel.pending.iter().enumerate() {
        for (j, p) in pending.proposals.iter().enumerate() {
            let sha = hash(p.candidate.text.as_bytes());
            texts.insert(sha.clone(), p.candidate.text.clone());
            origins.entry(sha).or_default().push((i, j));
        }
    }
    let mut verified_events = BTreeSet::new();
    let ordered = texts.into_iter().collect::<Vec<_>>();
    study::verify_parser(repo, parser)?;
    for (batch_index, batch) in ordered.chunks(BATCH).enumerate() {
        let parsed = parse(repo, parser, batch)?;
        panel.candidate_annotations += parsed.len();
        for ((sha, text_value), candidate) in batch.iter().zip(parsed) {
            let annotation = save_annotation(
                out,
                &format!("candidate-annotations/{variant}/{sha}.json"),
                text_value,
                &candidate,
                artifacts,
            )?;
            panel.candidate_errors += usize::from(candidate.is_err());
            for &(i, j) in &origins[sha] {
                let pending = &panel.pending[i];
                let input = &training[pending.post_index];
                let proposal = &pending.proposals[j];
                let source = saved_document(repo, out, input, &pending.source, identity)?;
                let result = match &candidate {
                    Ok(doc) => {
                        match match_pair_with_policy(&source, doc, proposal, resource, policy) {
                            Ok(value) => value,
                            Err(error) => {
                                json!({"status":"comparison_error","error":format!("{error:#}"),"reciprocally_observed_preserved":false})
                            }
                        }
                    }
                    Err(error) => {
                        json!({"status":"parse_error","error":error,"reciprocally_observed_preserved":false})
                    }
                };
                let pointer = output(
                    out,
                    &format!(
                        "alignments/{variant}/{}/{}.json",
                        text(&input["post"], "text_sha256")?,
                        proposal.id
                    ),
                    &json!({"input":input,"proposal":proposal,"candidate_annotation":annotation,"result":result}),
                )?;
                artifacts.push(pointer.clone());
                let form = match proposal.direction {
                    alternation::Direction::ToDoubleObject => false,
                    alternation::Direction::ToPrepositional => true,
                };
                let original_choice =
                    choice(&input["post"], &source, proposal.predicate_index, form)?;
                let source_unique_form = panel.source_proposals[pending.post_index]
                    .choices
                    .iter()
                    .any(|c| c.event_id == original_choice.event_id && c.double_object == form);
                let eligible =
                    result["reciprocally_observed_preserved"] == true && source_unique_form;
                panel.comparison_errors += usize::from(result["status"] == "comparison_error");
                if result["assessment_complete"] != true {
                    panel.verified[pending.post_index].assessment_complete = false;
                }
                if eligible
                    && verified_events
                        .insert((pending.post_index, original_choice.event_id.clone()))
                {
                    panel.verified[pending.post_index]
                        .choices
                        .push(original_choice);
                }
                panel.alignments.push(json!({"post_id":input["post"]["id"],"source_sha256":input["post"]["text_sha256"],"predicate_index":proposal.predicate_index,"proposal_id":proposal.id,"candidate_sha256":sha,"source_unique_form":source_unique_form,"reciprocally_eligible":eligible,"assessment_complete":result["assessment_complete"]==true,"forward_outcome":result["forward"]["outcome"],"status":result["status"],"report":pointer}));
            }
        }
        println!(
            "{variant}: checked candidate batch {} / {}",
            batch_index + 1,
            ordered.len().div_ceil(BATCH)
        );
    }
    study::verify_parser(repo, parser)?;
    Ok(())
}

fn execute(repo: &Path, out: &Path, p: &Value, artifacts: &mut Vec<Value>) -> Result<Value> {
    execute_with_policy(
        repo,
        out,
        p,
        artifacts,
        alternation::ArgumentPolicy::LegacyV1,
        None,
    )
}

pub(crate) fn execute_with_policy(
    repo: &Path,
    out: &Path,
    p: &Value,
    artifacts: &mut Vec<Value>,
    policy: alternation::ArgumentPolicy,
    prior_sources: Option<&BTreeMap<String, Value>>,
) -> Result<Value> {
    let previous = bound(repo, &p["predecessor"])?;
    ensure!(
        previous["stage_completed"] == true,
        "Predecessor stage incomplete"
    );
    bound(repo, &p["training_metadata_receipt"])?;
    let training = metadata(&bound(repo, &p["training_metadata"])?, p)?;
    let resource = study::resource(repo, &bound(repo, &p["resource_manifest"])?)?;
    ensure!(
        observation::registry(&resource)? == p["registry"],
        "Resource registry differs"
    );
    let parsers = rows(p, "parsers")?;
    ensure!(
        parsers.len() == 2
            && parsers[0]["id"] == "incumbent_sm"
            && parsers[1]["id"] == "isolated_trf",
        "Parser order differs"
    );
    let small = text(&parsers[0], "parser_identity")?;
    let mut reports = Vec::new();
    for parser in parsers {
        study::verify_parser(repo, parser)?;
        let variant = text(parser, "id")?;
        let mut panel = Panel::new();
        let started = Instant::now();
        for (batch_index, batch) in training.chunks(BATCH).enumerate() {
            let old = batch
                .iter()
                .map(|input| cached(repo, input, small))
                .collect::<Result<Vec<_>>>()?;
            if variant == "incumbent_sm" {
                for (offset, (input, doc)) in batch.iter().zip(old).enumerate() {
                    let source = json!({"path":input["annotation_path"],"sha256":input["annotation_sha256"],"storage":"bound_incumbent_cache"});
                    inspect(
                        input,
                        batch_index * BATCH + offset,
                        &Ok(doc),
                        source,
                        &resource,
                        variant,
                        out,
                        artifacts,
                        &mut panel,
                        policy,
                    )?;
                }
            } else if let Some(cache) = prior_sources {
                for (offset, input) in batch.iter().enumerate() {
                    let sha = text(&input["post"], "text_sha256")?;
                    let source = cache
                        .get(sha)
                        .context("Missing prior source annotation")?
                        .clone();
                    let doc =
                        saved_parsed(repo, out, input, &source, text(parser, "parser_identity")?)?;
                    inspect(
                        input,
                        batch_index * BATCH + offset,
                        &doc,
                        source,
                        &resource,
                        variant,
                        out,
                        artifacts,
                        &mut panel,
                        policy,
                    )?;
                }
            } else {
                let requested = old
                    .iter()
                    .map(|d| (hash(d.text.as_bytes()), d.text.clone()))
                    .collect::<Vec<_>>();
                let parsed = parse(repo, parser, &requested)?;
                panel.source_annotations += parsed.len();
                for (offset, ((input, (_, source)), doc)) in
                    batch.iter().zip(&requested).zip(parsed).enumerate()
                {
                    let annotation = save_annotation(
                        out,
                        &format!(
                            "source-annotations/{variant}/{}.json",
                            text(&input["post"], "text_sha256")?
                        ),
                        source,
                        &doc,
                        artifacts,
                    )?;
                    inspect(
                        input,
                        batch_index * BATCH + offset,
                        &doc,
                        annotation,
                        &resource,
                        variant,
                        out,
                        artifacts,
                        &mut panel,
                        policy,
                    )?;
                }
            }
            println!(
                "{variant}: observed {} / {} full source posts; {} posts with proposals",
                panel.frames.len(),
                training.len(),
                panel.pending.len()
            );
        }
        study::verify_parser(repo, parser)?;
        let source_elapsed = started.elapsed().as_secs_f64();
        artifacts.push(output(
            out,
            &format!("{variant}/source-summary.json"),
            &json!({"posts":panel.source_summaries,"source_elapsed_seconds":source_elapsed}),
        )?);
        candidates(
            repo, out, parser, &training, &resource, &mut panel, artifacts, policy,
        )?;
        let mut tiers = BTreeMap::new();
        for (name, posts) in [
            ("complete_frame", &panel.frames),
            ("source_proposal", &panel.source_proposals),
            ("reciprocally_checked", &panel.verified),
        ] {
            let input = output(
                out,
                &format!("{variant}/{name}/model-input.json"),
                &serde_json::to_value(posts)?,
            )?;
            artifacts.push(input.clone());
            let summary = support::report(posts)?;
            let b = output(out, &format!("{variant}/{name}/support.json"), &summary)?;
            artifacts.push(b.clone());
            tiers.insert(
                name,
                json!({"model_input":input,"support_report":b,"summary":summary}),
            );
        }
        artifacts.push(output(
            out,
            &format!("{variant}/alignment-summary.json"),
            &json!(panel.alignments),
        )?);
        let mut report = json!({"variant":variant,"primary":variant=="isolated_trf","source_posts":training.len(),"source_annotation_errors":panel.source_errors,"candidate_annotation_errors":panel.candidate_errors,"comparison_errors":panel.comparison_errors,"incomplete_reciprocal_assessments":panel.alignments.iter().filter(|a|a["assessment_complete"]!=true).count(),"new_source_annotations":panel.source_annotations,"new_candidate_annotations":panel.candidate_annotations,"source_proposals":panel.alignments.len(),"reciprocally_eligible_proposals":panel.alignments.iter().filter(|a|a["reciprocally_eligible"]==true).count(),"tiers":tiers,"elapsed_seconds":started.elapsed().as_secs_f64()});
        if !policy.is_legacy() {
            report["argument_policy"] = json!(policy);
        }
        artifacts.push(output(out, &format!("{variant}/report.json"), &report)?);
        reports.push(report);
    }
    let mut report = json!({"schema":"slopninja-dative-corpus-report-v1","primary_parser":"isolated_trf","reports":reports,"fit_models":false,"later_posts":0,"llm_calls":0,"detector_calls":0,"automatic_edit_license":false,"semantic_equivalence_certified":false});
    if !policy.is_legacy() {
        report["schema"] = json!("slopninja-dative-arguments-corpus-report-v1");
        report["argument_policy"] = json!(policy);
    }
    Ok(report)
}
fn run(repo: &Path, file: &Path, expected: &str, out: &Path) -> Result<()> {
    let p: Value = serde_json::from_slice(&checked(file, expected)?)?;
    ensure!(
        p["schema"] == SCHEMA
            && p["parser_batch_size"] == BATCH
            && p["support_policy"] == support::policy()
            && p["fit_models"] == false
            && p["later_posts"] == 0,
        "Protocol design differs"
    );
    ensure!(
        p["executable_sha256"] == hash(&fs::read(std::env::current_exe()?)?),
        "Executable differs"
    );
    for b in rows(&p, "source_bindings")? {
        safe_relative(text(b, "path")?)?;
        bound_bytes(repo, b)?;
    }
    fs::create_dir(out)?;
    let result = (|| -> Result<()> {
        let mut artifacts = vec![output(out, "protocol.json", &p)?];
        for b in rows(&p, "source_bindings")? {
            let name = format!("executed-source/{}", text(b, "path")?);
            let target = out.join(&name);
            fs::create_dir_all(target.parent().unwrap())?;
            let bytes = bound_bytes(repo, b)?;
            fs::write(target, &bytes)?;
            artifacts.push(json!({"path":name,"sha256":hash(&bytes)}));
        }
        let mut report = execute(repo, out, &p, &mut artifacts)?;
        report["protocol_sha256"] = json!(expected);
        report["artifacts"] = json!(artifacts);
        let b = output(out, "report.json", &report)?;
        output(
            out,
            "receipt.json",
            &json!({"schema":"slopninja-dative-corpus-receipt-v1","status":"completed","protocol_sha256":expected,"report":b}),
        )?;
        Ok(())
    })();
    if let Err(error) = &result {
        output(
            out,
            "failure.json",
            &json!({"status":"failed","protocol_sha256":expected,"error":format!("{error:#}")}),
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
    fn spec() -> Value {
        json!({"expected_posts":2,"expected_authors":1,"expected_source_groups":1,"expected_panel_posts":{"original":2}})
    }
    fn inputs() -> Value {
        json!([{"panel":"original","annotation_path":"cache/a.json","annotation_sha256":"a","post":{"id":"a","author":"author","date":"2004-01-01","source_group":"blog:author:date:2004-01-01","split":"train","text_sha256":"first"}},{"panel":"original","annotation_path":"cache/b.json","annotation_sha256":"b","post":{"id":"b","author":"author","date":"2004-01-01","source_group":"blog:author:date:2004-01-01","split":"train","text_sha256":"second"}}])
    }
    #[test]
    fn saved_source_errors_remain_unavailable_and_keep_text_identity() {
        let temp = tempfile::tempdir().unwrap();
        let mut binding = output(
            temp.path(),
            "prior/error.json",
            &json!({"status":"error","text":"A failed source.","error":"parser failed"}),
        )
        .unwrap();
        binding["storage"] = json!("bound_prior_corpus");
        let mut input = json!({"post":{"text_sha256":hash(b"A failed source.")}});
        let loaded = saved_parsed(
            temp.path(),
            &temp.path().join("new"),
            &input,
            &binding,
            "parser",
        )
        .unwrap();
        assert_eq!(loaded.unwrap_err(), "parser failed");
        assert!(saved_document(temp.path(), temp.path(), &input, &binding, "parser").is_err());
        input["post"]["text_sha256"] = json!(hash(b"Different source."));
        assert!(saved_parsed(temp.path(), temp.path(), &input, &binding, "parser").is_err());
    }
    #[test]
    fn metadata_keeps_sibling_posts_but_rejects_duplicate_identity_and_other_splits() {
        assert_eq!(metadata(&inputs(), &spec()).unwrap().len(), 2);
        let mut bad = inputs();
        bad[1]["post"]["id"] = json!("a");
        assert!(metadata(&bad, &spec()).is_err());
        let mut bad = inputs();
        bad[1]["post"]["split"] = json!("test");
        assert!(metadata(&bad, &spec()).is_err());
        let mut bad = inputs();
        bad[1]["post"]["source_group"] = json!("other");
        assert!(metadata(&bad, &spec()).is_err());
    }
}

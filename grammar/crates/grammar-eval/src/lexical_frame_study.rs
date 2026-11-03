//! Reproducible resource import and a fixed synthetic lexical-frame study.
use crate::{lexical_frame_observation as observation, verbnet_resource};
use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use grammar_core::syntax::Document;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    time::Instant,
};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Verify and index XML without invoking a parser or choosing a verb sense.
    Import {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        expected_manifest_sha256: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Parse every distinct frozen synthetic text once, then save all alternatives.
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
pub(crate) fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub(crate) fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("Missing string {key}"))
}
pub(crate) fn rows<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    v[key]
        .as_array()
        .with_context(|| format!("Missing array {key}"))
}
pub(crate) fn checked(file: &Path, expected: &str) -> Result<Vec<u8>> {
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
pub(crate) fn bound_bytes(repo: &Path, binding: &Value) -> Result<Vec<u8>> {
    checked(&repo.join(text(binding, "path")?), text(binding, "sha256")?)
}
pub(crate) fn bound(repo: &Path, binding: &Value) -> Result<Value> {
    Ok(serde_json::from_slice(&bound_bytes(repo, binding)?)?)
}
fn fresh(file: &Path, bytes: &[u8]) -> Result<String> {
    fs::create_dir_all(file.parent().context("Output parent")?)?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(file)?
        .write_all(bytes)?;
    Ok(hash(bytes))
}
pub(crate) fn output(out: &Path, name: &str, value: &Value) -> Result<Value> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(json!({"path":name,"sha256":fresh(&out.join(name),&bytes)?}))
}
pub(crate) fn safe_relative(file: &str) -> Result<()> {
    ensure!(
        !file.is_empty()
            && Path::new(file)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "Expected normal relative path"
    );
    Ok(())
}
fn safe_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
        "Unsafe case ID"
    );
    Ok(())
}
pub(crate) fn resource(repo: &Path, manifest: &Value) -> Result<verbnet_resource::Resource> {
    ensure!(
        manifest["schema"] == "slopninja-verbnet-resource-manifest-v1"
            && manifest["version"] == "3.4",
        "Unknown resource manifest"
    );
    for key in ["archive", "license", "readme"] {
        bound_bytes(repo, &manifest[key])?;
    }
    for binding in rows(manifest, "supporting_files")? {
        bound_bytes(repo, binding)?;
    }
    let mut sources = Vec::new();
    for binding in rows(manifest, "files")? {
        sources.push(verbnet_resource::XmlSource {
            path: text(binding, "path")?.into(),
            sha256: text(binding, "sha256")?.into(),
            xml: String::from_utf8(bound_bytes(repo, binding)?)?,
        });
    }
    verbnet_resource::Resource::from_sources(sources)
}
fn resource_summary(r: &verbnet_resource::Resource) -> Value {
    json!({"schema":r.schema,"source_files":r.sources.len(),"classes":r.classes.len(),
        "root_classes":r.classes.values().filter(|c|c.parent_id.is_none()).count(),
        "explicit_members":r.classes.values().map(|c|c.own_members.len()).sum::<usize>(),
        "own_frames":r.classes.values().map(|c|c.own_frames.len()).sum::<usize>(),
        "effective_frames_by_class":r.classes.values().map(|c|c.effective_frames.len()).sum::<usize>(),
        "lemmas":r.lemma_index.len(),"lemma_index":r.lemma_index,
        "classes_index":r.classes.values().map(|c|json!({"id":c.id,"parent_id":c.parent_id,"ancestor_ids":c.ancestor_ids,
            "own_members":c.own_members.iter().map(|m|json!({"name":m.name,"attributes":m.attributes,"origin":m.origin})).collect::<Vec<_>>(),
            "own_frame_ids":c.own_frames.iter().map(|f|&f.id).collect::<Vec<_>>(),"effective_frame_ids":c.effective_frames.iter().map(|f|&f.id).collect::<Vec<_>>() })).collect::<Vec<_>>(),
        "sense_selected":false,"automatic_edit_license":false})
}
fn span(value: &Value) -> Result<[usize; 2]> {
    Ok(serde_json::from_value(value.clone())?)
}
fn validate_span(source: &str, value: &Value) -> Result<()> {
    let [start, end] = span(value)?;
    ensure!(
        start < end && source.get(start..end).is_some(),
        "Invalid literal UTF-8 anchor"
    );
    Ok(())
}
fn word_bag(source: &str) -> Vec<String> {
    let mut words = grammar_core::features::words(source);
    words.sort();
    words
}
pub(crate) fn validate_fixture(
    fixture: &Value,
    r: &verbnet_resource::Resource,
) -> Result<BTreeMap<String, String>> {
    ensure!(
        fixture["schema"] == "slopninja-lexical-frame-fixtures-v1",
        "Unknown fixture schema"
    );
    let mut ids = BTreeSet::new();
    let mut texts = BTreeMap::new();
    let mut memberships = BTreeMap::<String, BTreeSet<String>>::new();
    let mut split_lemmas = BTreeMap::<String, BTreeSet<String>>::new();
    for case in rows(fixture, "cases")? {
        let id = text(case, "id")?;
        safe_id(id)?;
        ensure!(ids.insert(id), "Repeated case ID");
        let split = text(case, "split")?;
        ensure!(
            ["development", "confirmation", "masked"].contains(&split),
            "Unknown split"
        );
        let source = text(case, "text")?;
        ensure!(!source.trim().is_empty(), "Empty fixture");
        ensure!(!text(case, "category")?.is_empty(), "Empty category");
        let lemma = text(case, "lemma")?;
        ensure!(
            verbnet_resource::normalize_lemma(lemma).as_deref() == Some(lemma),
            "Unnormalized fixture lemma"
        );
        split_lemmas
            .entry(split.into())
            .or_default()
            .insert(lemma.into());
        let member_classes = r.lookup(lemma).members;
        ensure!(
            !member_classes.is_empty(),
            "Selected lemma missing from resource"
        );
        let selected = text(case, "selected_class")?;
        ensure!(
            member_classes.iter().any(|m| m.class_id == selected
                || r.classes[&m.class_id]
                    .ancestor_ids
                    .iter()
                    .any(|p| p == selected)),
            "Selected class not a lemma membership/ancestor"
        );
        for m in member_classes {
            let set = memberships.entry(split.into()).or_default();
            set.insert(m.class_id.clone());
            set.extend(r.classes[&m.class_id].ancestor_ids.clone());
        }
        let mask: Vec<String> = serde_json::from_value(case["mask_lemmas"].clone())?;
        ensure!(
            if split == "masked" {
                mask == vec![lemma]
            } else {
                mask.is_empty()
            },
            "Mask scope differs"
        );
        let expectations = rows(case, "expectations")?;
        ensure!(!expectations.is_empty(), "No case expectations");
        for e in expectations {
            validate_span(source, &e["predicate_span"])?;
            let classes = if let Some(value) = e.get("member_class") {
                let id = value.as_str().context("Expected member_class string")?;
                ensure!(
                    r.lookup(lemma).members.iter().any(|m| m.class_id == id),
                    "Expectation class not a declared lemma membership"
                );
                vec![
                    r.classes
                        .get(id)
                        .context("Unknown expectation member class")?,
                ]
            } else {
                r.classes.values().collect::<Vec<_>>()
            };
            let mut frame_ids = Vec::new();
            if let Some(value) = e.get("frame_id") {
                frame_ids.push(value.as_str().context("Expected frame_id string")?);
            }
            if let Some(value) = e.get("frame_ids") {
                let values = value.as_array().context("Expected frame_ids array")?;
                ensure!(!values.is_empty(), "Empty expected frame list");
                for value in values {
                    frame_ids.push(value.as_str().context("Expected frame ID string")?);
                }
            }
            for id in frame_ids {
                ensure!(
                    classes
                        .iter()
                        .any(|c| c.effective_frames.iter().any(|f| f.id == id)),
                    "Unknown expected effective frame ID"
                );
            }
            match text(e, "kind")? {
                "predicate_span" => {
                    text(e, "lemma")?;
                }
                "role_binding" => {
                    validate_span(source, &e["argument_span"])?;
                    text(e, "role")?;
                    ensure!(
                        e["require_complete_syntax"].is_boolean(),
                        "Missing role completeness requirement"
                    );
                }
                "syntax_binding_complete" => {
                    ensure!(e["expected"].is_boolean(), "Missing syntax expectation");
                }
                "unknown_lookup" | "all_frames_retained" | "unsupported" => {}
                _ => bail!("Unknown fixture expectation"),
            }
        }
        let sha = hash(source.as_bytes());
        if let Some(old) = texts.insert(sha, source.into()) {
            ensure!(old == source, "Text hash collision");
        }
    }
    let empty = BTreeSet::new();
    ensure!(
        memberships
            .get("development")
            .unwrap_or(&empty)
            .is_disjoint(memberships.get("confirmation").unwrap_or(&empty)),
        "Development/confirmation class memberships overlap"
    );
    let splits: Vec<_> = split_lemmas.values().collect();
    for (i, a) in splits.iter().enumerate() {
        for b in &splits[i + 1..] {
            ensure!(a.is_disjoint(b), "Lemma split overlap");
        }
    }
    for case in rows(fixture, "cases")? {
        let Some(link) = case.get("controlled_relation").filter(|v| !v.is_null()) else {
            continue;
        };
        let source = rows(fixture, "cases")?
            .iter()
            .find(|c| c["id"] == link["source_case_id"])
            .context("Missing controlled source case")?;
        ensure!(
            source["id"] != case["id"]
                && source["split"] == case["split"]
                && source["lemma"] == case["lemma"]
                && source["mask_lemmas"] == case["mask_lemmas"],
            "Controlled pair identity/scope differs"
        );
        let patches: Vec<grammar_core::edits::Edit> =
            serde_json::from_value(link["edits"].clone())?;
        ensure!(
            !patches.is_empty()
                && grammar_core::edits::apply(text(source, "text")?, &patches)?
                    == text(case, "text")?,
            "Controlled byte patches differ"
        );
        ensure!(
            link["expect_anchored_ledger_change"] == true
                && link["meaning_equivalence_expected"] == false,
            "Unsupported controlled-pair interpretation"
        );
        ensure!(
            link["alphabetic_word_multiset_preserved"]
                == json!(word_bag(text(source, "text")?) == word_bag(text(case, "text")?)),
            "Controlled word-count annotation differs"
        );
    }
    Ok(texts)
}

// These diagnostic signatures omit byte offsets and document hashes. Token
// identities remain because the question is which participant fills each role.
// They are not author-style coordinates or certificates of equivalent meaning.
fn token_identity(anchor: &Value) -> Value {
    json!({"lemma":anchor["normalized_lemma"],"text":anchor["token"]["text"],"pos":anchor["token"]["pos"]})
}
fn sorted_values(mut values: Vec<Value>) -> Vec<Value> {
    values.sort_by_cached_key(|v| serde_json::to_string(v).unwrap());
    values
}
fn raw_signature(predicate: &Value) -> Value {
    let deps = predicate["direct_dependents"].as_array().unwrap();
    let anchors = std::iter::once(&predicate["head"])
        .chain(
            deps.iter()
                .flat_map(|d| d["subtree"].as_array().unwrap().iter()),
        )
        .collect::<Vec<_>>();
    let dependents=deps.iter().map(|d| {
        let subtree=d["subtree"].as_array().unwrap().iter().map(|a| {
            let parent=anchors.iter().find(|p|p["token"]["i"]==a["token"]["head"]);
            json!({"token":token_identity(a),"dep":a["token"]["dep"],"parent":parent.map(|p|token_identity(p))})
        }).collect();
        json!({"token":token_identity(&d["anchor"]),"dep":d["anchor"]["token"]["dep"],"subtree":sorted_values(subtree)})
    }).collect();
    json!({"head":token_identity(&predicate["head"]),"dependents":sorted_values(dependents)})
}
fn pair_signature(row: &Value, lexical: bool) -> Option<Value> {
    let predicates = row["observation"]["predicates"].as_array()?;
    let targets = row["target_predicates"].as_array()?;
    let mut signatures = Vec::new();
    for target in targets {
        if target["found"] != true {
            return None;
        }
        let wanted = span(&target["predicate_span"]).ok()?;
        let p = predicates.iter().find(|p| {
            p["head"]["token"]["start_byte"] == wanted[0]
                && p["head"]["token"]["end_byte"] == wanted[1]
        })?;
        if !lexical {
            signatures.push(raw_signature(p));
            continue;
        }
        let complete = p["frame_possibilities"]
            .as_array()?
            .iter()
            .filter(|f| f["syntax_binding_complete"] == true)
            .collect::<Vec<_>>();
        if complete.is_empty() {
            return None;
        }
        let frames=complete.iter().map(|f| {
            let bindings=f["bindings"].as_array().unwrap().iter().filter(|b|!b["role_name"].is_null()).map(|b| {
                let alternatives=b["alternatives"].as_array().unwrap().iter().map(|a| {
                    let proposition=a["proposition_head_index"].as_u64().and_then(|head|predicates.iter().find(|q|q["head"]["token"]["i"]==head)).map(raw_signature);
                    json!({"kind":a["kind"],"argument":token_identity(&a["anchor"]),"proposition":proposition})
                }).collect();
                json!({"role":b["role_name"],"syntax_index":b["syntax_index"],"alternatives":sorted_values(alternatives)})
            }).collect();
            json!({"member_class":f["member_class_id"],"member_ordinal":f["member_ordinal"],"frame_id":f["frame"]["id"],"bindings":sorted_values(bindings)})
        }).collect();
        signatures.push(
            json!({"predicate_lemma":p["head"]["normalized_lemma"],"frames":sorted_values(frames)}),
        );
    }
    (!signatures.is_empty()).then(|| json!(sorted_values(signatures)))
}
pub(crate) fn pair_results(cases: &[Value]) -> Result<Vec<Value>> {
    let mut pairs = Vec::new();
    for candidate in cases {
        let case = &candidate["case"];
        let Some(link) = case.get("controlled_relation").filter(|v| !v.is_null()) else {
            continue;
        };
        let source = cases
            .iter()
            .find(|c| c["case"]["id"] == link["source_case_id"])
            .context("Missing evaluated pair source")?;
        let raw_a = pair_signature(source, false);
        let raw_b = pair_signature(candidate, false);
        let lexical_a = pair_signature(source, true);
        let lexical_b = pair_signature(candidate, true);
        pairs.push(json!({"source_case_id":source["case"]["id"],"candidate_case_id":case["id"],"split":case["split"],"lemma":case["lemma"],
            "word_occurrences_equal":word_bag(text(&source["case"],"text")?)==word_bag(text(case,"text")?),
            "raw_argument_signature_changed":raw_a.as_ref().zip(raw_b.as_ref()).map(|(a,b)|a!=b),
            "complete_frame_binding_signature_changed":lexical_a.as_ref().zip(lexical_b.as_ref()).map(|(a,b)|a!=b),
            "source_raw_signature":raw_a,"candidate_raw_signature":raw_b,"source_complete_binding_signature":lexical_a,"candidate_complete_binding_signature":lexical_b,
            "semantic_equivalence_certified":false,"automatic_edit_license":false}));
    }
    Ok(pairs)
}
fn anchored(anchor: &observation::Anchor, wanted: [usize; 2]) -> bool {
    [anchor.token.start_byte, anchor.token.end_byte] == wanted
}
fn expected_frames(
    p: &observation::PredicateLedger,
    r: &verbnet_resource::Resource,
    masked: &BTreeSet<String>,
) -> Vec<(String, usize, String)> {
    let mut expected = Vec::new();
    if let Some(lemma) = &p.head.normalized_lemma
        && !masked.contains(lemma)
    {
        for member in r.lookup(lemma).members {
            for frame in &r.classes[&member.class_id].effective_frames {
                expected.push((
                    member.class_id.clone(),
                    member.member_ordinal,
                    frame.id.clone(),
                ));
            }
        }
    }
    expected.sort();
    expected
}
fn evaluate(
    e: &Value,
    report: Option<&observation::ObservationReport>,
    r: &verbnet_resource::Resource,
    masked: &BTreeSet<String>,
) -> Result<Value> {
    let Some(report) = report else {
        return Ok(
            json!({"expectation":e,"status":"unavailable","reason":"parse_or_observation_error"}),
        );
    };
    let target = span(&e["predicate_span"])?;
    let found: Vec<_> = report
        .predicates
        .iter()
        .filter(|p| anchored(&p.head, target))
        .collect();
    if found.len() != 1 {
        return Ok(
            json!({"expectation":e,"status":"mismatch","reason":"predicate_anchor_not_unique","count":found.len()}),
        );
    }
    let p = found[0];
    let frame_allowed = |f: &&observation::FrameObservation| {
        e.get("member_class")
            .is_none_or(|v| v == &f.member_class_id)
            && e.get("frame_id").is_none_or(|v| v == &f.frame.id)
            && e.get("frame_ids")
                .is_none_or(|v| v.as_array().is_some_and(|a| a.contains(&json!(f.frame.id))))
    };
    let (met, evidence) = match text(e, "kind")? {
        "predicate_span" => (
            p.head.normalized_lemma.as_deref() == Some(text(e, "lemma")?),
            json!({"head":p.head}),
        ),
        "all_frames_retained" => {
            let expected = expected_frames(p, r, masked);
            let mut actual: Vec<_> = p
                .frame_possibilities
                .iter()
                .map(|f| {
                    (
                        f.member_class_id.clone(),
                        f.member_ordinal,
                        f.frame.id.clone(),
                    )
                })
                .collect();
            actual.sort();
            (
                expected == actual,
                json!({"expected":expected,"actual":actual}),
            )
        }
        "unknown_lookup" => (
            p.frame_possibilities.is_empty()
                && p.open_world_unknown_use_possible
                && p.lookup["status"] != "found",
            json!({"lookup":p.lookup}),
        ),
        "unsupported" => (
            p.head_support
                .iter()
                .any(|f| f.status == observation::Status::Unresolved)
                && !p
                    .frame_possibilities
                    .iter()
                    .any(|f| f.syntax_binding_complete),
            json!({"head_support":p.head_support}),
        ),
        "syntax_binding_complete" => {
            let matching: Vec<_> = p
                .frame_possibilities
                .iter()
                .filter(frame_allowed)
                .filter(|f| f.syntax_binding_complete)
                .map(|f| json!({"member_class":f.member_class_id,"frame_id":f.frame.id}))
                .collect();
            (
                !matching.is_empty() == e["expected"].as_bool().context("Expected boolean")?,
                json!({"complete_frames":matching}),
            )
        }
        "role_binding" => {
            let argument = span(&e["argument_span"])?;
            let role = text(e, "role")?;
            let complete = e["require_complete_syntax"]
                .as_bool()
                .context("Expected boolean")?;
            let mut matched = Vec::new();
            for f in p.frame_possibilities.iter().filter(frame_allowed) {
                if f.syntactic_binding_status == observation::Status::Contradicted
                    || (complete && !f.syntax_binding_complete)
                {
                    continue;
                }
                for b in &f.bindings {
                    if b.role_name.as_deref() == Some(role)
                        && b.alternatives.iter().any(|a| anchored(&a.anchor, argument))
                    {
                        matched.push(json!({"member_class":f.member_class_id,"frame_id":f.frame.id,"binding":b,"syntax_binding_status":f.syntactic_binding_status}));
                    }
                }
            }
            (!matched.is_empty(), json!({"matching_bindings":matched}))
        }
        _ => bail!("Unknown expectation kind"),
    };
    Ok(json!({"expectation":e,"status":if met{"met"}else{"mismatch"},"evidence":evidence}))
}
/// Evaluate one immutable annotation under an explicit mapping version.
pub(crate) fn evaluate_case(
    case: &Value,
    parsed: &std::result::Result<Document, String>,
    r: &verbnet_resource::Resource,
    mapping: observation::Mapping,
) -> Result<Value> {
    let source = text(case, "text")?;
    if let Ok(doc) = parsed {
        ensure!(doc.text == source, "Case annotation text differs");
    }
    let sha = hash(source.as_bytes());
    let masked: BTreeSet<String> = serde_json::from_value(case["mask_lemmas"].clone())?;
    let observed = parsed
        .as_ref()
        .ok()
        .map(|doc| observation::observe_with_mapping(doc, r, &masked, mapping))
        .transpose();
    let report = observed.as_ref().ok().and_then(Option::as_ref);
    let expectations = rows(case, "expectations")?
        .iter()
        .map(|e| evaluate(e, report, r, &masked))
        .collect::<Result<Vec<_>>>()?;
    let target_spans = rows(case, "expectations")?
        .iter()
        .map(|e| span(&e["predicate_span"]))
        .collect::<Result<BTreeSet<_>>>()?;
    let targets = target_spans.iter().map(|wanted| {
        let matching = report.map(|r|r.predicates.iter().filter(|p|anchored(&p.head,*wanted)).collect::<Vec<_>>()).unwrap_or_default();
        let found = matching.len()==1;
        json!({"predicate_span":wanted,"found":found,
            "any_complete_syntax":found.then(||matching[0].frame_possibilities.iter().any(|f|f.syntax_binding_complete)),
            "observed_lemma":if found {matching[0].head.normalized_lemma.clone()} else {None}})
    }).collect::<Vec<_>>();
    let retained = report.map(|rpt| {
        rpt.predicates.iter().all(|p| {
            let mut actual = p
                .frame_possibilities
                .iter()
                .map(|f| {
                    (
                        f.member_class_id.clone(),
                        f.member_ordinal,
                        f.frame.id.clone(),
                    )
                })
                .collect::<Vec<_>>();
            actual.sort();
            actual == expected_frames(p, r, &masked)
        })
    });
    Ok(
        json!({"case":case,"source_sha256":sha,"status":if parsed.is_err(){"parse_error"}else if observed.is_err(){"observation_error"}else{"evaluated"},
        "parse_error":parsed.as_ref().err(),"observation_error":observed.as_ref().err().map(|e|format!("{e:#}")),"observation":report,"expectations":expectations,
        "target_predicates":targets,"all_observed_frames_retained":retained}),
    )
}

pub(crate) fn summary(cases: &[&Value]) -> Value {
    let mut c = BTreeMap::<String, usize>::new();
    for key in [
        "cases",
        "parse_errors",
        "observation_errors",
        "expectations_met",
        "expectations_mismatch",
        "expectations_unavailable",
        "predicates",
        "predicates_with_complete_syntax",
        "predicates_with_no_resource_frames",
        "frames_checked",
        "frames_contradicted",
        "frames_unresolved",
        "full_checked",
        "full_contradicted",
        "full_unresolved",
        "target_predicates_expected",
        "target_predicates_found",
        "target_predicates_unavailable",
        "target_predicates_complete_syntax",
        "target_predicates_incomplete_syntax",
        "frame_retention_invariant_failures",
    ] {
        c.insert(key.into(), 0);
    }
    for row in cases {
        c.entry("cases".into()).and_modify(|n| *n += 1);
        if row["status"] == "parse_error" {
            *c.get_mut("parse_errors").unwrap() += 1;
        }
        if row["status"] == "observation_error" {
            *c.get_mut("observation_errors").unwrap() += 1;
        }
        if row["all_observed_frames_retained"] == false {
            *c.get_mut("frame_retention_invariant_failures").unwrap() += 1;
        }
        if let Some(targets) = row["target_predicates"].as_array() {
            for target in targets {
                *c.get_mut("target_predicates_expected").unwrap() += 1;
                let found = target["found"] == true;
                *c.get_mut(if found {
                    "target_predicates_found"
                } else {
                    "target_predicates_unavailable"
                })
                .unwrap() += 1;
                if found {
                    *c.get_mut(if target["any_complete_syntax"] == true {
                        "target_predicates_complete_syntax"
                    } else {
                        "target_predicates_incomplete_syntax"
                    })
                    .unwrap() += 1;
                }
            }
        }
        for e in row["expectations"].as_array().unwrap() {
            *c.get_mut(&format!("expectations_{}", e["status"].as_str().unwrap()))
                .unwrap() += 1;
        }
        if let Some(predicates) = row["observation"]["predicates"].as_array() {
            for p in predicates {
                *c.get_mut("predicates").unwrap() += 1;
                let frames = p["frame_possibilities"].as_array().unwrap();
                *c.get_mut("predicates_with_no_resource_frames").unwrap() +=
                    usize::from(frames.is_empty());
                *c.get_mut("predicates_with_complete_syntax").unwrap() +=
                    usize::from(frames.iter().any(|f| f["syntax_binding_complete"] == true));
                for f in frames {
                    *c.get_mut(&format!(
                        "frames_{}",
                        f["syntactic_binding_status"].as_str().unwrap()
                    ))
                    .unwrap() += 1;
                    *c.get_mut(&format!(
                        "full_{}",
                        f["full_compatibility_status"].as_str().unwrap()
                    ))
                    .unwrap() += 1;
                }
            }
        }
    }
    json!({"counts":c,"automatic_edit_license":false,"semantic_equivalence_certified":false})
}
pub(crate) fn verify_parser(repo: &Path, parser: &Value) -> Result<()> {
    bound_bytes(repo, &parser["parser_python"])?;
    bound_bytes(repo, &parser["environment_manifest"])?;
    ensure!(
        !rows(parser, "asset_bindings")?.is_empty(),
        "No parser asset bindings"
    );
    for binding in rows(parser, "asset_bindings")? {
        bound_bytes(repo, binding)?;
    }
    Ok(())
}
fn run(repo: &Path, protocol_file: &Path, expected: &str, out: &Path) -> Result<()> {
    let protocol: Value = serde_json::from_slice(&checked(protocol_file, expected)?)?;
    ensure!(
        protocol["schema"] == "slopninja-lexical-frame-study-protocol-v1"
            && protocol["automatic_edit_license"] == false,
        "Unknown protocol"
    );
    ensure!(
        protocol["parser_batch_size"] == 64,
        "Parser batch size differs"
    );
    for key in [
        "training_corpus_reads",
        "model_fit_calls",
        "llm_calls",
        "detector_calls",
    ] {
        ensure!(protocol[key] == 0, "Unsupported study activity");
    }
    let exe = fs::read(std::env::current_exe()?)?;
    ensure!(
        hash(&exe) == text(&protocol, "executable_sha256")?,
        "Executable hash differs"
    );
    let sources = rows(&protocol, "source_bindings")?;
    ensure!(!sources.is_empty(), "No source bindings");
    for b in sources {
        safe_relative(text(b, "path")?)?;
        bound_bytes(repo, b)?;
    }
    let parser = &protocol["parser"];
    verify_parser(repo, parser)?;
    let resource_manifest = bound(repo, &protocol["resource_manifest"])?;
    let r = resource(repo, &resource_manifest)?;
    let fixture = bound(repo, &protocol["fixture_manifest"])?;
    let texts = validate_fixture(&fixture, &r)?;
    for split in ["development", "confirmation", "masked"] {
        ensure!(
            rows(&fixture, "cases")?
                .iter()
                .filter(|c| c["split"] == split)
                .count()
                == protocol["expected_split_cases"][split]
                    .as_u64()
                    .context("Missing expected split count")? as usize,
            "Split budget differs"
        );
    }
    ensure!(
        texts.len()
            == protocol["expected_distinct_texts"]
                .as_u64()
                .context("Expected text count")? as usize,
        "Text budget differs"
    );
    ensure!(
        rows(&fixture, "cases")?.len()
            == protocol["expected_cases"]
                .as_u64()
                .context("Expected case count")? as usize,
        "Case budget differs"
    );
    fs::create_dir(out).context("Create fresh run directory")?;
    let result = (|| -> Result<()> {
        let mut artifacts = vec![
            output(out, "protocol.json", &protocol)?,
            output(out, "fixtures.json", &fixture)?,
            output(out, "resource-manifest.json", &resource_manifest)?,
            output(out, "resource-index.json", &resource_summary(&r))?,
        ];
        for binding in sources {
            let name = format!("executed-source/{}", text(binding, "path")?);
            let bytes = bound_bytes(repo, binding)?;
            artifacts.push(json!({"path":name,"sha256":fresh(&out.join(&name),&bytes)?}));
        }
        let began = Instant::now();
        let ordered: Vec<_> = texts.iter().collect();
        let mut docs = BTreeMap::<String, std::result::Result<Document, String>>::new();
        let mut annotations = Vec::new();
        for batch in ordered.chunks(64) {
            let batch_texts: Vec<_> = batch.iter().map(|(_, s)| s.as_str()).collect();
            let parsed = grammar_spacy::parse_batch(
                &batch_texts,
                &repo
                    .join(text(&parser["parser_python"], "path")?)
                    .to_string_lossy(),
            )?;
            ensure!(parsed.len() == batch.len(), "Batch length differs");
            for ((sha, source), doc) in batch.iter().zip(parsed) {
                let value = match &doc {
                    Ok(doc) => {
                        grammar_core::syntax::validate(doc)?;
                        ensure!(
                            doc.text == **source
                                && doc.parser_identity == text(parser, "parser_identity")?,
                            "Annotation binding differs"
                        );
                        json!({"status":"ok","document":doc})
                    }
                    Err(error) => json!({"status":"error","text":source,"error":error}),
                };
                let binding = output(out, &format!("annotations/{sha}.json"), &value)?;
                annotations
                    .push(json!({"source_sha256":sha,"artifact":binding,"status":value["status"]}));
                artifacts.push(binding);
                docs.insert((*sha).clone(), doc);
            }
        }
        let timing = json!({"distinct_texts":texts.len(),"batches":ordered.len().div_ceil(64),"text_order":"lexical SHA256","parse_and_write_seconds":began.elapsed().as_secs_f64()});
        artifacts.push(output(
            out,
            "annotation-manifest.json",
            &json!({"parser":parser,"documents":annotations,"timing":timing}),
        )?);
        verify_parser(repo, parser)?;
        let mut evaluated = Vec::new();
        for case in rows(&fixture, "cases")? {
            let source = text(case, "text")?;
            let sha = hash(source.as_bytes());
            let parsed = &docs[&sha];
            let row = evaluate_case(case, parsed, &r, observation::Mapping::BaselineV1)?;
            artifacts.push(output(
                out,
                &format!("cases/{}.json", text(case, "id")?),
                &row,
            )?);
            evaluated.push(row);
        }
        let mut groups = Vec::new();
        for key in ["split", "lemma", "selected_class", "category"] {
            let mut grouped = BTreeMap::<String, Vec<&Value>>::new();
            for row in &evaluated {
                grouped
                    .entry(text(&row["case"], key)?.into())
                    .or_default()
                    .push(row);
            }
            for (value, members) in grouped {
                groups.push(json!({"field":key,"value":value,"summary":summary(&members)}));
            }
        }
        let pairs = pair_results(&evaluated)?;
        artifacts.push(output(out, "controlled-pairs.json", &json!(pairs))?);
        let pair_summary = json!({"pairs":pairs.len(),"equal_word_occurrences":pairs.iter().filter(|p|p["word_occurrences_equal"]==true).count(),
            "raw_changed":pairs.iter().filter(|p|p["raw_argument_signature_changed"]==true).count(),
            "raw_unchanged":pairs.iter().filter(|p|p["raw_argument_signature_changed"]==false).count(),
            "raw_unavailable":pairs.iter().filter(|p|p["raw_argument_signature_changed"].is_null()).count(),
            "complete_bindings_changed":pairs.iter().filter(|p|p["complete_frame_binding_signature_changed"]==true).count(),
            "complete_bindings_unchanged":pairs.iter().filter(|p|p["complete_frame_binding_signature_changed"]==false).count(),
            "complete_bindings_unavailable":pairs.iter().filter(|p|p["complete_frame_binding_signature_changed"].is_null()).count()});
        let report = json!({"schema":"slopninja-lexical-frame-study-report-v1","protocol_sha256":expected,"resource_manifest":protocol["resource_manifest"],"timing":timing,
            "summary":summary(&evaluated.iter().collect::<Vec<_>>()),"groups":groups,"pair_summary":pair_summary,"artifacts":artifacts,
            "human_semantic_ratings":0,"automatic_edit_license":false,"semantic_equivalence_certified":false,"detector_calls":0});
        let report_binding = output(out, "report.json", &report)?;
        output(
            out,
            "receipt.json",
            &json!({"schema":"slopninja-lexical-frame-study-receipt-v1","status":"completed","protocol_sha256":expected,"report":report_binding,"automatic_edit_license":false}),
        )?;
        println!("{}", serde_json::to_string_pretty(&report["summary"])?);
        Ok(())
    })();
    if let Err(error) = &result {
        output(
            out,
            "failure.json",
            &json!({"status":"failed","error":format!("{error:#}"),"protocol_sha256":expected}),
        )?;
    }
    result
}
pub fn main() -> Result<()> {
    match Args::parse().command {
        Command::Import {
            repo,
            manifest,
            expected_manifest_sha256,
            out,
        } => {
            let manifest: Value =
                serde_json::from_slice(&checked(&manifest, &expected_manifest_sha256)?)?;
            let resource = resource(&repo, &manifest)?;
            let mut summary = resource_summary(&resource);
            summary["manifest_sha256"] = json!(expected_manifest_sha256);
            output(
                out.parent().context("Output parent")?,
                out.file_name()
                    .context("Output filename")?
                    .to_str()
                    .context("UTF-8 filename")?,
                &summary,
            )?;
            Ok(())
        }
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
    fn diagnostic_row(subject: &str, object: &str, offset: usize, masked: bool) -> Value {
        let anchor = |name: &str, i: usize, head: usize, start: usize, dep: &str| json!({"normalized_lemma":name.to_lowercase(),"token":{"text":name,"i":i,"head":head,"start_byte":start+offset,"end_byte":start+offset+name.len(),"dep":dep,"pos":if i==1{"VERB"}else{"PROPN"}}});
        let subject = anchor(subject, 0, 1, 0, "nsubj");
        let object = anchor(object, 2, 1, 12, "dobj");
        let head = anchor("admires", 1, 1, 4, "ROOT");
        let frames = if masked {
            vec![]
        } else {
            vec![
                json!({"syntax_binding_complete":true,"member_class_id":"synthetic","member_ordinal":0,"frame":{"id":"synthetic-frame"},
            "bindings":[{"role_name":"Experiencer","syntax_index":0,"alternatives":[{"kind":"active_subject","anchor":subject,"proposition_head_index":null}]},
            {"role_name":"Stimulus","syntax_index":2,"alternatives":[{"kind":"nominal_object","anchor":object,"proposition_head_index":null}]}]}),
            ]
        };
        json!({"target_predicates":[{"predicate_span":[offset+4,offset+11],"found":true}],"observation":{"source_sha256":format!("deliberately-vary-{offset}"),"predicates":[{
            "head":head,"frame_possibilities":frames,"direct_dependents":[{"anchor":subject,"subtree":[subject]},{"anchor":object,"subtree":[object]}]}]}})
    }
    #[test]
    fn pair_signatures_ignore_offsets_but_detect_equal_word_bag_role_swaps() {
        let original = diagnostic_row("Ada", "Ben", 0, false);
        let shifted = diagnostic_row("Ada", "Ben", 200, false);
        let swapped = diagnostic_row("Ben", "Ada", 0, false);
        assert_eq!(word_bag("Ada admires Ben."), word_bag("Ben admires Ada."));
        for lexical in [false, true] {
            assert!(pair_signature(&original, lexical).is_some());
            assert_eq!(
                pair_signature(&original, lexical),
                pair_signature(&shifted, lexical)
            );
            assert_ne!(
                pair_signature(&original, lexical),
                pair_signature(&swapped, lexical)
            );
        }
        let masked = diagnostic_row("Ada", "Ben", 0, true);
        assert!(pair_signature(&masked, true).is_none());
        assert_eq!(
            pair_signature(&masked, false),
            pair_signature(&original, false)
        );
    }
    #[test]
    fn proposition_signature_retains_internal_participants() {
        let mut original = diagnostic_row("Ada", "Ben", 0, false);
        let mut complement =
            diagnostic_row("Nora", "Eli", 100, true)["observation"]["predicates"][0].clone();
        // Use disjoint token indices for the nested predicate and its arguments.
        complement["head"]["token"]["i"] = json!(4);
        complement["head"]["token"]["head"] = json!(4);
        for (i, d) in complement["direct_dependents"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            d["anchor"]["token"]["i"] = json!(3 + i * 2);
            d["anchor"]["token"]["head"] = json!(4);
            d["subtree"] = json!([d["anchor"].clone()]);
        }
        original["observation"]["predicates"]
            .as_array_mut()
            .unwrap()
            .push(complement);
        original["observation"]["predicates"][0]["frame_possibilities"][0]["bindings"][1]["alternatives"]
            [0]["proposition_head_index"] = json!(4);
        let mut changed = original.clone();
        let d = &mut changed["observation"]["predicates"][1]["direct_dependents"][0];
        d["anchor"]["normalized_lemma"] = json!("lee");
        d["anchor"]["token"]["text"] = json!("Lee");
        d["subtree"] = json!([d["anchor"].clone()]);
        assert_ne!(
            pair_signature(&original, true),
            pair_signature(&changed, true)
        );
    }
    #[test]
    fn byte_anchors_reject_partial_unicode_and_empty_ranges() {
        assert!(validate_span("Éva", &json!([0, 2])).is_ok());
        assert!(validate_span("Éva", &json!([0, 1])).is_err());
        assert!(validate_span("Éva", &json!([2, 2])).is_err());
    }
    #[test]
    fn failed_annotations_keep_expectations_unavailable() {
        let empty = verbnet_resource::Resource {
            schema: verbnet_resource::SCHEMA.into(),
            sources: vec![],
            documents: BTreeMap::new(),
            classes: BTreeMap::new(),
            lemma_index: BTreeMap::new(),
        };
        let e = json!({"kind":"role_binding","predicate_span":[4,8],"argument_span":[0,3],"role":"Agent","require_complete_syntax":true});
        assert_eq!(
            evaluate(&e, None, &empty, &BTreeSet::new()).unwrap()["status"],
            "unavailable"
        );
        let row = json!({"status":"parse_error","expectations":[{"status":"unavailable"}],"observation":null});
        let s = summary(&[&row]);
        assert_eq!(s["counts"]["expectations_unavailable"], 1);
        assert_eq!(s["counts"]["expectations_met"], 0);
    }
}

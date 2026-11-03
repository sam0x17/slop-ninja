//! Synthetic rule coverage and measurement, without semantic certification.
use crate::{edit_utility_scoring::Scoring, function_word_roles, predicate_operators};
use anyhow::{Context, Result, ensure};
use grammar_core::{
    features::{self, Family},
    rules,
    syntax::Document,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};

const PROTOCOL_SHA: &str = "e05adc3b0cc75248dd65be5a6ee3712d7599ff792facddd347156985a0fb3d3f";

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("missing string {key}"))
}
fn rows<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    v[key]
        .as_array()
        .with_context(|| format!("missing array {key}"))
}
fn read_bound(path: &Path, digest: &str) -> Result<Value> {
    let bytes = fs::read(path)?;
    ensure!(
        hash(&bytes) == digest,
        "bound artifact changed: {}",
        path.display()
    );
    Ok(serde_json::from_slice(&bytes)?)
}
fn write_json(path: &Path, value: &Value) -> Result<String> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?
        .write_all(&bytes)?;
    Ok(hash(&bytes))
}

fn expected_candidate(case: &Value) -> Option<&str> {
    case["explicit_candidate"]["text"].as_str().or_else(|| {
        case["rule_expectations"]
            .as_array()?
            .iter()
            .find_map(|r| r["expected_candidate_text"].as_str())
    })
}
fn validate_anchor(text: &str, anchor: &Value) -> Result<(usize, usize)> {
    let start = anchor["start_byte"].as_u64().context("anchor start")? as usize;
    let end = anchor["end_byte"].as_u64().context("anchor end")? as usize;
    ensure!(
        start < end
            && end <= text.len()
            && text.is_char_boundary(start)
            && text.is_char_boundary(end),
        "anchor must bound UTF-8 source text"
    );
    ensure!(
        &text[start..end] == string(anchor, "expected")?,
        "anchor source mismatch"
    );
    Ok((start, end))
}
fn validate_manifest(manifest: &Value) -> Result<()> {
    ensure!(
        manifest["schema"] == "slopninja-edit-utility-fixtures-v1"
            && manifest["rule_catalog_version"] == rules::LEGACY_CATALOG_VERSION,
        "fixture schema/rule catalog differs"
    );
    ensure!(
        rows(manifest, "cases")?.len() == 16
            && manifest["provenance"]["semantic_equivalence_certified"] == false,
        "fixture scope/certification differs"
    );
    let mut ids = BTreeSet::new();
    for case in rows(manifest, "cases")? {
        ensure!(ids.insert(string(case, "id")?), "duplicate case ID");
        let source = string(case, "source_text")?;
        let candidate = expected_candidate(case);
        let mut rule_ids = BTreeSet::new();
        for rule in rows(case, "rule_expectations")? {
            let id = string(rule, "rule")?;
            ensure!(
                rules::BUILTIN_RULES.contains(&id) && rule_ids.insert(id),
                "unknown/repeated expected rule"
            );
            ensure!(
                ["suggest", "abstain"].contains(&string(rule, "expectation")?),
                "unknown rule expectation"
            );
            if rule["expectation"] == "suggest" {
                ensure!(
                    rule["expected_candidate_text"].as_str() == candidate,
                    "ambiguous fixture candidate"
                );
            }
        }
        for expectation in rows(case, "protected_expectations")? {
            for anchor in rows(expectation, "source_anchors")? {
                validate_anchor(source, anchor)?;
            }
            for anchor in rows(expectation, "candidate_anchors")? {
                validate_anchor(
                    candidate.context("candidate anchor lacks candidate")?,
                    anchor,
                )?;
            }
        }
    }
    Ok(())
}

fn observations(doc: &Document) -> Result<Value> {
    let legacy = features::extract(doc)?;
    let function_roles = function_word_roles::extract(doc)?;
    let operators = predicate_operators::document_family(doc)?;
    let mut families = legacy.families.clone();
    families.insert(
        function_word_roles::FAMILY.into(),
        function_roles.family.clone(),
    );
    families.insert(predicate_operators::FAMILY.into(), operators);
    Ok(
        json!({"families":families,"legacy_features":legacy,"function_word_events":function_roles.events,"function_word_missing_lemmas":function_roles.missing_lemma_events,"predicate_observations":predicate_operators::document_observations(doc)?}),
    )
}
fn family_changes(before: &Value, after: &Value) -> Result<Value> {
    let left: BTreeMap<String, Family> = serde_json::from_value(before["families"].clone())?;
    let right: BTreeMap<String, Family> = serde_json::from_value(after["families"].clone())?;
    ensure!(
        left.keys().eq(right.keys()),
        "diagnostic family catalog changed"
    );
    let mut changes = BTreeMap::new();
    for (name, source) in &left {
        let candidate = &right[name];
        let values = match (
            grammar_eval::family_values(source),
            grammar_eval::family_values(candidate),
        ) {
            (Ok(a), Ok(b)) => {
                let keys: BTreeSet<_> = a.keys().chain(b.keys()).cloned().collect();
                let movements: Vec<_> = keys
                    .into_iter()
                    .filter_map(|key| {
                        let old = a.get(&key).copied().unwrap_or(0.);
                        let new = b.get(&key).copied().unwrap_or(0.);
                        (old != new).then(
                            || json!({"feature":key,"source":old,"candidate":new,"delta":new-old}),
                        )
                    })
                    .collect();
                json!({"status":"available","movements":movements})
            }
            (a, b) => {
                json!({"status":"unavailable","source_error":a.err().map(|e|e.to_string()),"candidate_error":b.err().map(|e|e.to_string())})
            }
        };
        changes.insert(
            name.clone(),
            json!({"exact_family_equal":source==candidate,"values":values}),
        );
    }
    Ok(json!(changes))
}
fn anchor_observations(doc: Option<&Document>, anchors: &[Value]) -> Result<Value> {
    let Some(doc) = doc else {
        return Ok(json!({"status":"no_annotation","anchors":anchors}));
    };
    let roles = function_word_roles::document_events(doc)?;
    let operators = predicate_operators::document_observations(doc)?;
    let mut result = Vec::new();
    for anchor in anchors {
        let (start, end) = validate_anchor(&doc.text, anchor)?;
        let tokens: Vec<_> = doc
            .tokens
            .iter()
            .filter(|t| t.start_byte < end && start < t.end_byte)
            .collect();
        let ids: BTreeSet<_> = tokens.iter().map(|t| t.i).collect();
        result.push(json!({"anchor":anchor,"tokens":tokens,"related_function_word_events":roles.iter().filter(|r|ids.contains(&r.token_index)||ids.contains(&r.head_index)).collect::<Vec<_>>(),"related_predicate_observations":operators.iter().filter(|o|ids.contains(&o.head_index)).collect::<Vec<_>>()}));
    }
    Ok(json!({"status":"observed","anchors":result}))
}

fn parse_into(
    texts: &[String],
    python: &str,
    identity: &str,
    cache: &mut BTreeMap<String, std::result::Result<Document, String>>,
) -> Result<()> {
    let missing: Vec<_> = texts
        .iter()
        .filter(|text| !cache.contains_key(*text))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let refs: Vec<_> = missing.iter().map(String::as_str).collect();
    for (text, result) in missing
        .iter()
        .zip(grammar_spacy::parse_batch(&refs, python)?)
    {
        if let Ok(doc) = &result {
            ensure!(
                doc.text == *text && doc.parser_identity == identity,
                "parser identity/source differs"
            );
        }
        cache.insert(text.clone(), result);
    }
    Ok(())
}
fn good_doc<'a>(
    cache: &'a BTreeMap<String, std::result::Result<Document, String>>,
    text: &str,
) -> Option<&'a Document> {
    cache.get(text)?.as_ref().ok()
}
fn add_variant(variants: &mut Vec<Value>, text: &str, origin: &str) {
    if let Some(row) = variants.iter_mut().find(|v| v["text"] == text) {
        let origins = row["origins"].as_array_mut().unwrap();
        if !origins.contains(&json!(origin)) {
            origins.push(json!(origin));
        }
    } else {
        variants.push(json!({"id":if variants.is_empty(){"source".into()}else{format!("variant-{:03}",variants.len())},"text":text,"sha256":hash(text.as_bytes()),"origins":[origin]}));
    }
}

fn score_rankings(variants: &[Value], context: &str) -> Value {
    let mut groups = BTreeMap::<(u64, String), Vec<Value>>::new();
    for variant in variants {
        if let Some(scores) = variant["contexts"][context]["score"]["scores"].as_array() {
            for score in scores {
                groups
                    .entry((
                        score["seed"].as_u64().unwrap(),
                        score["author"].as_str().unwrap().into(),
                    ))
                    .or_default()
                    .push(json!({"variant_id":variant["id"],"logit":score["logit"]}));
            }
        }
    }
    json!(groups.into_iter().map(|((seed,author),mut rows)| {
        rows.sort_by(|a,b|b["logit"].as_f64().unwrap().total_cmp(&a["logit"].as_f64().unwrap()).then_with(||a["variant_id"].as_str().cmp(&b["variant_id"].as_str())));
        let source=rows.iter().find(|r|r["variant_id"]=="source").and_then(|r|r["logit"].as_f64());
        for row in &mut rows { row["improvement_from_source"]=json!(source.map(|s|row["logit"].as_f64().unwrap()-s)); }
        json!({"seed":seed,"author":author,"ordered_variants":rows,"interpretation":"proximity only; no semantic/readability certification"})
    }).collect::<Vec<_>>())
}

pub fn run(protocol_path: &Path, expected_sha: &str, out: &Path) -> Result<()> {
    ensure!(
        expected_sha == PROTOCOL_SHA && !out.exists(),
        "requires fixed protocol and fresh output"
    );
    let protocol = read_bound(protocol_path, expected_sha)?;
    let fixture = read_bound(
        Path::new(string(&protocol["fixture_manifest"], "path")?),
        string(&protocol["fixture_manifest"], "sha256")?,
    )?;
    validate_manifest(&fixture)?;
    ensure!(
        protocol["rules"]["catalog"] == rules::LEGACY_CATALOG_VERSION,
        "rule catalog mismatch"
    );
    let python = string(&protocol, "parser_python")?;
    let identity = string(&protocol, "parser_identity")?;
    // Only now may frozen reference caches be opened; protocol and all fixture
    // anchors have been validated before any target/profile or parser outcome.
    let scoring = Scoring::load(&protocol)?;
    fs::create_dir(out)?;
    write_json(&out.join("protocol.json"), &protocol)?;
    write_json(&out.join("target-audit.json"), &scoring.target_audit())?;
    let cases = rows(&fixture, "cases")?;
    let mut parse_cache = BTreeMap::new();
    parse_into(
        &cases
            .iter()
            .map(|c| string(c, "source_text").unwrap().to_string())
            .collect::<Vec<_>>(),
        python,
        identity,
        &mut parse_cache,
    )?;
    let mut prepared = Vec::new();
    let mut parse_texts = Vec::new();
    for case in cases {
        let source = string(case, "source_text")?;
        let selected = rows(case, "rule_expectations")?
            .iter()
            .map(|r| string(r, "rule").map(str::to_string))
            .collect::<Result<Vec<_>>>()?;
        let generated = if selected.is_empty() {
            Ok(Vec::new())
        } else if let Some(doc) = good_doc(&parse_cache, source) {
            rules::generate_selected_at_version(doc, 256, &selected, rules::LEGACY_CATALOG_VERSION)
        } else {
            Err(anyhow::anyhow!("source annotation unavailable"))
        };
        let mut variants = Vec::new();
        add_variant(&mut variants, source, "source");
        let mut findings = Vec::new();
        for expected in rows(case, "rule_expectations")? {
            let suggestions = generated.as_ref().ok().map(|g| {
                g.iter()
                    .filter(|c| c.rule == expected["rule"])
                    .collect::<Vec<_>>()
            });
            let met = suggestions.as_ref().map(|g| {
                if expected["expectation"] == "abstain" {
                    g.is_empty()
                } else {
                    g.iter()
                        .any(|c| c.text == expected["expected_candidate_text"])
                }
            });
            findings.push(json!({"expectation":expected,"expectation_met":met,"observed_suggestions":suggestions,"status":if met.is_none(){"not_evaluated"}else if met==Some(true){"matched"}else{"mismatch"}}));
        }
        if let Ok(generated) = &generated {
            for candidate in generated {
                add_variant(&mut variants, &candidate.text, "generated");
            }
        }
        if let Some(candidate) = expected_candidate(case) {
            add_variant(
                &mut variants,
                candidate,
                if case["explicit_candidate"].is_null() {
                    "expected_candidate"
                } else {
                    "explicit_counterfactual"
                },
            );
        }
        for variant in &variants {
            for context in rows(&protocol["scoring"], "contexts")? {
                parse_texts.push(format!(
                    "{}{}{}",
                    string(context, "prefix")?,
                    string(variant, "text")?,
                    string(context, "suffix")?
                ));
            }
        }
        prepared.push((case,variants,findings,json!({"candidates":generated.as_ref().ok(),"error":generated.as_ref().err().map(|e|e.to_string())})));
    }
    parse_into(&parse_texts, python, identity, &mut parse_cache)?;
    fs::create_dir(out.join("annotations"))?;
    for (text, result) in &parse_cache {
        let digest = hash(text.as_bytes());
        write_json(
            &out.join("annotations").join(format!("{digest}.json")),
            &json!({"text_sha256":digest,"text":text,"annotation":result.as_ref().ok(),"error":result.as_ref().err()}),
        )?;
    }
    let mut reports = Vec::new();
    for (case, mut variants, findings, generation) in prepared {
        let source = string(case, "source_text")?;
        let mut base = BTreeMap::new();
        for context in rows(&protocol["scoring"], "contexts")? {
            let text = format!(
                "{}{}{}",
                string(context, "prefix")?,
                source,
                string(context, "suffix")?
            );
            if let Some(doc) = good_doc(&parse_cache, &text) {
                base.insert(string(context, "id")?.to_string(), observations(doc)?);
            }
        }
        for variant in &mut variants {
            let mut contexts = serde_json::Map::new();
            for context in rows(&protocol["scoring"], "contexts")? {
                let name = string(context, "id")?;
                let text = format!(
                    "{}{}{}",
                    string(context, "prefix")?,
                    string(variant, "text")?,
                    string(context, "suffix")?
                );
                let result = if let Some(doc) = good_doc(&parse_cache, &text) {
                    let observed = observations(doc)?;
                    let changes = base
                        .get(name)
                        .map(|b| family_changes(b, &observed))
                        .transpose()?;
                    json!({"status":"parsed","text_sha256":hash(text.as_bytes()),"observations":observed,"changes_from_source":changes,"score":scoring.score(doc)?})
                } else {
                    json!({"status":"parse_error","text_sha256":hash(text.as_bytes()),"error":parse_cache.get(&text).and_then(|r|r.as_ref().err())})
                };
                contexts.insert(name.into(), result);
            }
            variant["contexts"] = json!(contexts);
        }
        for context in rows(&protocol["scoring"], "contexts")? {
            let name = string(context, "id")?;
            let source_score = variants[0]["contexts"][name]["score"]["mean_logit"].as_f64();
            for variant in &mut variants {
                let candidate = variant["contexts"][name]["score"]["mean_logit"].as_f64();
                variant["contexts"][name]["mean_logit_improvement_from_source"] =
                    json!(candidate.zip(source_score).map(|(a, b)| a - b));
            }
        }
        let mut protected = Vec::new();
        for expectation in rows(case, "protected_expectations")? {
            protected.push(json!({"declared_expectation":expectation,"source":anchor_observations(good_doc(&parse_cache,source),rows(expectation,"source_anchors")?)?,"candidate":anchor_observations(expected_candidate(case).and_then(|c|good_doc(&parse_cache,c)),rows(expectation,"candidate_anchors")?)?,"semantic_judgment":"not_performed","readability":"unjudged"}));
        }
        let rankings = rows(&protocol["scoring"], "contexts")?
            .iter()
            .map(|c| {
                Ok((
                    string(c, "id")?.to_string(),
                    score_rankings(&variants, string(c, "id")?),
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        reports.push(json!({"id":case["id"],"category":case["category"],"rule_findings":findings,"generation":generation,"variants":variants,"protected_expectations":protected,"rankings":rankings,"semantic_equivalence_certified":false,"readability":"unjudged","tone":"unjudged"}));
    }
    let mut scoring_counts = BTreeMap::<String, BTreeMap<String, usize>>::new();
    for case in &reports {
        for variant in rows(case, "variants")? {
            for context in rows(&protocol["scoring"], "contexts")? {
                let name = string(context, "id")?;
                let entry = &variant["contexts"][name];
                let status = entry["score"]["status"].as_str().unwrap_or("parse_error");
                *scoring_counts
                    .entry(name.into())
                    .or_default()
                    .entry(status.into())
                    .or_default() += 1;
            }
        }
    }
    let rule_findings: Vec<_> = reports
        .iter()
        .flat_map(|c| c["rule_findings"].as_array().unwrap())
        .collect();
    let aggregate = json!({"cases":reports.len(),"variants":reports.iter().map(|c|c["variants"].as_array().unwrap().len()).sum::<usize>(),"rule_expectations":rule_findings.len(),"rule_expectations_matched":rule_findings.iter().filter(|r|r["expectation_met"]==true).count(),"rule_expectations_mismatched":rule_findings.iter().filter(|r|r["expectation_met"]==false).count(),"rule_expectations_not_evaluated":rule_findings.iter().filter(|r|r["expectation_met"].is_null()).count(),"unique_parsed_texts":parse_cache.values().filter(|r|r.is_ok()).count(),"parse_failures":parse_cache.values().filter(|r|r.is_err()).count(),"scoring":scoring_counts});
    let source_hashes = json!({"edit_utility.rs":hash(include_bytes!("edit_utility.rs")),"edit_utility_scoring.rs":hash(include_bytes!("edit_utility_scoring.rs")),"function_word_roles.rs":hash(include_bytes!("function_word_roles.rs")),"predicate_operators.rs":hash(include_bytes!("predicate_operators.rs")),"lexical_context.rs":hash(include_bytes!("lexical_context.rs")),"coordinate_inference.rs":hash(include_bytes!("coordinate_inference.rs")),"bin/slop_ninja-edit-utility.rs":hash(include_bytes!("bin/slop_ninja-edit-utility.rs"))});
    let report = json!({"schema":"slopninja-edit-utility-result-v1","protocol_sha256":expected_sha,"fixture_manifest_sha256":protocol["fixture_manifest"]["sha256"],"parser_identity":identity,"aggregate":aggregate,"cases":reports,"source_sha256":source_hashes,"executed_binary_sha256":hash(&fs::read(std::env::current_exe()?)?),"target_audit_sha256":hash(&fs::read(out.join("target-audit.json"))?),"model_calls":0,"detector_calls":0,"parameter_updates":0,"semantic_equivalence_certified":false,"readability":"unjudged","tone":"unjudged"});
    let digest = write_json(&out.join("report.json"), &report)?;
    println!(
        "{}",
        serde_json::to_string(&json!({"report_sha256":digest,"aggregate":aggregate}))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anchors_preserve_exact_utf8_bytes_and_reject_stale_spans() {
        assert!(
            validate_anchor("écho", &json!({"start_byte":0,"end_byte":2,"expected":"é"})).is_ok()
        );
        assert!(
            validate_anchor("écho", &json!({"start_byte":1,"end_byte":2,"expected":"é"})).is_err()
        );
        assert!(
            validate_anchor("echo", &json!({"start_byte":0,"end_byte":1,"expected":"x"})).is_err()
        );
    }
    #[test]
    fn duplicate_expected_generated_candidate_retains_both_origins() {
        let mut variants = Vec::new();
        add_variant(&mut variants, "source", "source");
        add_variant(&mut variants, "other", "generated");
        add_variant(&mut variants, "other", "expected_candidate");
        assert_eq!(variants.len(), 2);
        assert_eq!(
            variants[1]["origins"],
            json!(["generated", "expected_candidate"])
        );
    }
    #[test]
    fn fixture_anchors_and_expected_rule_catalog_validate() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../fixtures/edit-utility-v1/manifest.json"
        ))
        .unwrap();
        validate_manifest(&fixture).unwrap();
    }
}

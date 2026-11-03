//! Compare bound, already executed lexical-frame reports. No NLP is invoked.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::syntax::{self, Document, Token};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};
use unicode_normalization::UnicodeNormalization;

const VARIANTS: [&str; 4] = ["incumbent_sm", "isolated_sm", "isolated_md", "isolated_trf"];
const CONTRASTS: [(&str, &str); 3] = [
    ("incumbent_sm", "isolated_sm"),
    ("isolated_sm", "isolated_md"),
    ("isolated_sm", "isolated_trf"),
];
#[derive(Parser)]
struct Args {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    expected_protocol_sha256: String,
    #[arg(long)]
    inputs: PathBuf,
    #[arg(long)]
    expected_inputs_sha256: String,
    #[arg(long)]
    out: PathBuf,
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("Missing string {key}"))
}
fn array<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    v[key]
        .as_array()
        .with_context(|| format!("Missing array {key}"))
}
fn safe(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && Path::new(path)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "Unsafe relative path"
    );
    Ok(())
}
fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    ensure!(
        expected.len() == 64
            && expected
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "Invalid digest"
    );
    let bytes = fs::read(path).with_context(|| format!("Read {}", path.display()))?;
    ensure!(
        digest(&bytes) == expected,
        "Digest differs: {}",
        path.display()
    );
    Ok(bytes)
}
fn bound(repo: &Path, b: &Value) -> Result<Vec<u8>> {
    checked(&repo.join(string(b, "path")?), string(b, "sha256")?)
}
fn bound_json(repo: &Path, b: &Value) -> Result<Value> {
    Ok(serde_json::from_slice(&bound(repo, b)?)?)
}
fn write(out: &Path, name: &str, bytes: &[u8]) -> Result<Value> {
    safe(name)?;
    let path = out.join(name);
    fs::create_dir_all(path.parent().context("Output parent")?)?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)?;
    Ok(json!({"path":name,"sha256":digest(bytes)}))
}
fn output(out: &Path, name: &str, value: &Value) -> Result<Value> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write(out, name, &bytes)
}
fn normalize(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| {
        value
            .nfc()
            .collect::<String>()
            .to_lowercase()
            .nfc()
            .collect()
    })
}
fn span(value: &Value) -> Result<[usize; 2]> {
    Ok(serde_json::from_value(value.clone())?)
}
fn token_at<'a>(doc: &'a Document, wanted: &Value) -> Result<Option<&'a Token>> {
    let [a, b] = span(wanted)?;
    Ok(doc
        .tokens
        .iter()
        .find(|t| t.start_byte == a && t.end_byte == b))
}
fn predicate_at<'a>(row: &'a Value, wanted: &Value) -> Option<&'a Value> {
    let [a, b] = span(wanted).ok()?;
    row["observation"]["predicates"]
        .as_array()?
        .iter()
        .find(|p| p["head"]["token"]["start_byte"] == a && p["head"]["token"]["end_byte"] == b)
}
fn bool_status(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "met",
        Some(false) => "mismatch",
        None => "unavailable",
    }
}
fn expectation_status(value: &Value) -> Result<&str> {
    let s = string(value, "status")?;
    ensure!(
        ["met", "mismatch", "unavailable"].contains(&s),
        "Unknown expectation result"
    );
    Ok(s)
}
fn transition(a: &str, b: &str) -> Value {
    json!({"baseline":a,"candidate":b,"transition":format!("{a}_to_{b}"),"improved_to_met":a!="met"&&b=="met","regressed_from_met":a=="met"&&b!="met","availability_gained":a=="unavailable"&&b!="unavailable","availability_lost":a!="unavailable"&&b=="unavailable"})
}
fn increment(map: &mut BTreeMap<String, usize>, key: &str) {
    *map.entry(key.into()).or_default() += 1;
}

struct SavedRun {
    variant: String,
    protocol: Value,
    report: Value,
    cases: BTreeMap<String, Value>,
    annotations: BTreeMap<String, Option<Document>>,
    annotation_errors: BTreeMap<String, Value>,
    pairs: BTreeMap<(String, String), Value>,
    ancestors: BTreeMap<String, BTreeSet<String>>,
    provenance: Value,
}

fn verify_sources(repo: &Path, protocol: &Value) -> Result<()> {
    for b in array(protocol, "source_bindings")? {
        safe(string(b, "path")?)?;
        bound(repo, b)?;
    }
    for key in ["parser_python", "environment_manifest"] {
        bound(repo, &protocol["parser"][key])?;
    }
    for b in array(&protocol["parser"], "asset_bindings")? {
        bound(repo, b)?;
    }
    let resource = bound_json(repo, &protocol["resource_manifest"])?;
    for key in ["archive", "license", "readme"] {
        bound(repo, &resource[key])?;
    }
    for key in ["files", "supporting_files"] {
        for b in array(&resource, key)? {
            bound(repo, b)?;
        }
    }
    Ok(())
}

fn load_run(repo: &Path, descriptor: &Value, input: &Value, fixture: &Value) -> Result<SavedRun> {
    let variant = string(descriptor, "variant")?.to_owned();
    let protocol = bound_json(repo, &descriptor["protocol"])?;
    ensure!(
        protocol["schema"] == "slopninja-lexical-frame-study-protocol-v1"
            && protocol["parser"]["id"] == variant,
        "Producer protocol identity"
    );
    ensure!(
        protocol["automatic_edit_license"] == false
            && protocol["semantic_equivalence_certified"] == false,
        "Producer license changed"
    );
    verify_sources(repo, &protocol)?;
    ensure!(
        bound_json(repo, &protocol["fixture_manifest"])? == *fixture,
        "Producer fixture differs"
    );
    let directory = repo.join(string(descriptor, "run_directory")?);
    let receipt = bound_json(repo, &input["receipt"])?;
    ensure!(
        repo.join(string(&input["receipt"], "path")?)
            .canonicalize()?
            == directory.join("receipt.json").canonicalize()?,
        "Receipt is outside its declared run"
    );
    ensure!(
        receipt["schema"] == "slopninja-lexical-frame-study-receipt-v1"
            && receipt["status"] == "completed",
        "Producer run incomplete"
    );
    let expected = string(&descriptor["protocol"], "sha256")?;
    ensure!(
        receipt["protocol_sha256"] == expected,
        "Receipt protocol differs"
    );
    safe(string(&receipt["report"], "path")?)?;
    let report = bound_json(&directory, &receipt["report"])?;
    ensure!(
        report["schema"] == "slopninja-lexical-frame-study-report-v1"
            && report["protocol_sha256"] == expected,
        "Report protocol differs"
    );
    ensure!(
        report["resource_manifest"] == protocol["resource_manifest"],
        "Report resource differs"
    );
    let mut artifacts = BTreeMap::new();
    for b in array(&report, "artifacts")? {
        let path = string(b, "path")?;
        safe(path)?;
        ensure!(
            artifacts.insert(path.to_owned(), b.clone()).is_none(),
            "Duplicate artifact path"
        );
        bound(&directory, b)?;
    }
    let artifact = |name: &str| -> Result<Value> {
        bound_json(
            &directory,
            artifacts
                .get(name)
                .with_context(|| format!("Missing artifact {name}"))?,
        )
    };
    ensure!(
        artifact("protocol.json")? == protocol && artifact("fixtures.json")? == *fixture,
        "Saved protocol/fixture differs"
    );
    for source in array(&protocol, "source_bindings")? {
        let name = format!("executed-source/{}", string(source, "path")?);
        ensure!(
            artifacts.get(&name).context("Missing source snapshot")?["sha256"] == source["sha256"],
            "Source snapshot differs"
        );
    }
    let annotation_manifest = artifact("annotation-manifest.json")?;
    ensure!(
        annotation_manifest["parser"] == protocol["parser"],
        "Annotation parser binding differs"
    );
    let mut annotations = BTreeMap::new();
    let mut annotation_errors = BTreeMap::new();
    let texts = array(fixture, "cases")?
        .iter()
        .map(|c| {
            Ok((
                string(c, "text_sha256")?.to_owned(),
                string(c, "text")?.to_owned(),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    for a in array(&annotation_manifest, "documents")? {
        let sha = string(a, "source_sha256")?;
        let raw = artifact(string(&a["artifact"], "path")?)?;
        ensure!(
            artifacts.get(string(&a["artifact"], "path")?) == Some(&a["artifact"]),
            "Annotation artifact reference differs"
        );
        let source = texts.get(sha).context("Unexpected annotation text")?;
        ensure!(digest(source.as_bytes()) == sha, "Source digest differs");
        ensure!(a["status"] == raw["status"], "Annotation status differs");
        let doc = if raw["status"] == "ok" {
            let doc: Document = serde_json::from_value(raw["document"].clone())?;
            syntax::validate(&doc)?;
            ensure!(
                doc.text == *source
                    && doc.parser_identity == string(&protocol["parser"], "parser_identity")?,
                "Saved document identity differs"
            );
            Some(doc)
        } else {
            ensure!(
                raw["status"] == "error" && raw["text"] == *source && !raw["error"].is_null(),
                "Invalid saved parser error"
            );
            annotation_errors.insert(sha.to_owned(), raw["error"].clone());
            None
        };
        ensure!(
            annotations.insert(sha.into(), doc).is_none(),
            "Duplicate annotation"
        );
    }
    ensure!(
        annotations.keys().eq(texts.keys()),
        "Annotation coverage differs"
    );
    let index = artifact("resource-index.json")?;
    let ancestors = array(&index, "classes_index")?
        .iter()
        .map(|c| {
            Ok((
                string(c, "id")?.to_owned(),
                serde_json::from_value(c["ancestor_ids"].clone())?,
            ))
        })
        .collect::<Result<BTreeMap<String, BTreeSet<String>>>>()?;
    let mut cases = BTreeMap::new();
    for case in array(fixture, "cases")? {
        let id = string(case, "id")?;
        let row = artifact(&format!("cases/{id}.json"))?;
        validate_case(&row, case)?;
        ensure!(
            row["source_sha256"] == case["text_sha256"],
            "Case source digest differs"
        );
        let doc = &annotations[string(case, "text_sha256")?];
        ensure!(
            (row["status"] == "parse_error") == doc.is_none(),
            "Case/annotation availability differs"
        );
        if row["status"] == "evaluated" {
            ensure!(
                row["observation"]["parser_identity"] == protocol["parser"]["parser_identity"]
                    && row["observation"]["source_sha256"] == case["text_sha256"],
                "Observation identity differs"
            );
        }
        ensure!(cases.insert(id.into(), row).is_none(), "Duplicate case");
    }
    let recorded_case_paths = artifacts.keys().filter(|p| p.starts_with("cases/")).count();
    ensure!(
        recorded_case_paths == cases.len(),
        "Unexpected recorded cases"
    );
    let mut pairs = BTreeMap::new();
    for pair in artifact("controlled-pairs.json")?
        .as_array()
        .context("Pair array")?
    {
        let key = (
            string(pair, "source_case_id")?.to_owned(),
            string(pair, "candidate_case_id")?.to_owned(),
        );
        ensure!(
            cases.contains_key(&key.0) && cases.contains_key(&key.1),
            "Pair case absent"
        );
        ensure!(
            cases[&key.1]["case"]["controlled_relation"]["source_case_id"] == key.0,
            "Pair declaration differs"
        );
        ensure!(pairs.insert(key, pair.clone()).is_none(), "Duplicate pair");
    }
    ensure!(
        pairs.len()
            == cases
                .values()
                .filter(|r| r["case"]["controlled_relation"].is_object())
                .count(),
        "Pair coverage differs"
    );
    let provenance = json!({"variant":variant,"run_directory":descriptor["run_directory"],"protocol":descriptor["protocol"],"receipt":input["receipt"],"report":receipt["report"],"reuse_saved":descriptor["reuse_saved"],"artifact_count":artifacts.len(),"parser_identity":protocol["parser"]["parser_identity"]});
    Ok(SavedRun {
        variant,
        protocol,
        report,
        cases,
        annotations,
        annotation_errors,
        pairs,
        ancestors,
        provenance,
    })
}

fn validate_case(row: &Value, case: &Value) -> Result<()> {
    ensure!(row["case"] == *case, "Case declaration differs");
    ensure!(
        ["evaluated", "parse_error", "observation_error"].contains(&string(row, "status")?),
        "Unknown case state"
    );
    let actual = array(row, "expectations")?;
    let expected = array(case, "expectations")?;
    ensure!(actual.len() == expected.len(), "Expectation count differs");
    for (a, e) in actual.iter().zip(expected) {
        ensure!(a["expectation"] == *e, "Expectation identity/order differs");
        expectation_status(a)?;
    }
    let expected_spans = expected
        .iter()
        .map(|e| span(&e["predicate_span"]))
        .collect::<Result<BTreeSet<_>>>()?;
    let actual_spans = array(row, "target_predicates")?
        .iter()
        .map(|e| span(&e["predicate_span"]))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        expected_spans == actual_spans
            && actual_spans.len() == array(row, "target_predicates")?.len(),
        "Target coverage differs"
    );
    if let Some(linked) = case.get("linked_proposition_expectations") {
        for e in linked.as_array().context("Linked expectation array")? {
            let mut spans = BTreeSet::new();
            for key in [
                "matrix_predicate_span",
                "proposition_span",
                "subject_span",
                "object_span",
            ] {
                let [start, end] = span(&e[key])?;
                ensure!(
                    start < end
                        && string(case, "text")?.get(start..end).is_some()
                        && spans.insert([start, end]),
                    "Invalid or repeated linked occurrence anchor"
                );
            }
            ensure!(
                expected_spans.contains(&span(&e["matrix_predicate_span"])?),
                "Linked matrix is not a declared target"
            );
            ensure!(
                normalize(string(e, "proposition_lemma")?).as_deref()
                    == e["proposition_lemma"].as_str(),
                "Unnormalized proposition lemma"
            );
        }
    }
    Ok(())
}

fn raw_token(doc: &Document, t: &Token) -> Value {
    let parent = &doc.tokens[t.head];
    json!({"span":[t.start_byte,t.end_byte],"index":t.i,"text":t.text,"lemma":t.lemma,"normalized_lemma":normalize(&t.lemma),"pos":t.pos,"tag":t.tag,"dep":t.dep,"head_index":t.head,"head_span":[parent.start_byte,parent.end_byte],"head_text":parent.text,"morph":t.morph,"sentence":t.sentence,"is_punct":t.is_punct,"is_space":t.is_space})
}
fn marker_records(doc: &Document) -> Vec<Value> {
    doc.tokens
        .iter()
        .filter(|t| matches!(t.dep.as_str(), "mark" | "complm"))
        .map(|t| raw_token(doc, t))
        .collect()
}
fn raw_diff(a: Option<&Document>, b: Option<&Document>) -> Value {
    let (Some(a), Some(b)) = (a, b) else {
        return json!({"status":"unavailable","baseline_annotation_available":a.is_some(),"candidate_annotation_available":b.is_some(),"token_changes":null});
    };
    let am = a
        .tokens
        .iter()
        .map(|t| ([t.start_byte, t.end_byte], t))
        .collect::<BTreeMap<_, _>>();
    let bm = b
        .tokens
        .iter()
        .map(|t| ([t.start_byte, t.end_byte], t))
        .collect::<BTreeMap<_, _>>();
    let slots = am.keys().chain(bm.keys()).copied().collect::<BTreeSet<_>>();
    let mut changes = Vec::new();
    for slot in slots {
        let left = am.get(&slot).map(|t| raw_token(a, t));
        let right = bm.get(&slot).map(|t| raw_token(b, t));
        let fields = [
            "text",
            "lemma",
            "normalized_lemma",
            "pos",
            "tag",
            "dep",
            "head_span",
            "morph",
            "sentence",
            "is_punct",
            "is_space",
        ];
        let differing = fields
            .iter()
            .filter(|f| left.as_ref().map(|v| &v[**f]) != right.as_ref().map(|v| &v[**f]))
            .copied()
            .collect::<Vec<_>>();
        if !differing.is_empty() {
            changes.push(
                json!({"span":slot,"baseline":left,"candidate":right,"changed_fields":differing}),
            );
        }
    }
    json!({"status":"compared","documents_equal":a==b,"tokens_equal":a.tokens==b.tokens,"sentences_equal":a.sentences==b.sentences,"token_changes":changes,"baseline_markers":marker_records(a),"candidate_markers":marker_records(b),"baseline_sentences":a.sentences,"candidate_sentences":b.sentences})
}

fn linked_check(row: &Value, doc: Option<&Document>, expected: &Value) -> Result<Value> {
    let mut raw_findings = Vec::new();
    let mut ledger_findings = Vec::new();
    let mut frame_links = Vec::new();
    let frame_assertions = array(&row["case"], "expectations")?
        .iter()
        .filter(|e| {
            e["kind"] == "role_binding"
                && e["predicate_span"] == expected["matrix_predicate_span"]
                && e["argument_span"] == expected["proposition_span"]
        })
        .collect::<Vec<_>>();
    let frame_requested = !frame_assertions.is_empty();
    let frame_evidence_available = doc.is_some() && row["observation"].is_object();
    if let Some(doc) = doc {
        let matrix = token_at(doc, &expected["matrix_predicate_span"])?;
        let proposition = token_at(doc, &expected["proposition_span"])?;
        let subject = token_at(doc, &expected["subject_span"])?;
        let object = token_at(doc, &expected["object_span"])?;
        raw_findings.push(json!({"check":"exact_token_anchors","met":matrix.is_some()&&proposition.is_some()&&subject.is_some()&&object.is_some()}));
        if let (Some(m), Some(p), Some(s), Some(o)) = (matrix, proposition, subject, object) {
            for (check, met) in [
                (
                    "proposition_lemma",
                    normalize(&p.lemma).as_deref() == expected["proposition_lemma"].as_str(),
                ),
                ("matrix_direct_ccomp", p.head == m.i && p.dep == "ccomp"),
                ("own_direct_subject", s.head == p.i && s.dep == "nsubj"),
                (
                    "own_direct_object",
                    o.head == p.i && matches!(o.dep.as_str(), "dobj" | "obj"),
                ),
            ] {
                raw_findings.push(json!({"check":check,"met":met}));
            }
            let mp = predicate_at(row, &expected["matrix_predicate_span"]);
            let pp = predicate_at(row, &expected["proposition_span"]);
            if row["observation"].is_object() {
                ledger_findings.push(
                    json!({"check":"anchored_predicate_rows","met":mp.is_some()&&pp.is_some()}),
                );
                if let (Some(mp), Some(pp)) = (mp, pp) {
                    let direct = mp["direct_dependents"].as_array().is_some_and(|ds| {
                        ds.iter().any(|d| {
                            d["anchor"]["token"]["i"] == p.i
                                && d["anchor"]["token"]["head"] == m.i
                                && d["anchor"]["token"]["dep"] == "ccomp"
                                && d["proposition_head_indices"]
                                    .as_array()
                                    .is_some_and(|v| v.contains(&json!(p.i)))
                        })
                    });
                    ledger_findings.push(json!({"check":"matrix_proposition_link","met":direct}));
                    for (label, t, dependencies) in [
                        ("proposition_subject", s, vec!["nsubj"]),
                        ("proposition_object", o, vec!["dobj", "obj"]),
                    ] {
                        let met = pp["direct_dependents"].as_array().is_some_and(|ds| {
                            ds.iter().any(|d| {
                                d["anchor"]["token"]["i"] == t.i
                                    && d["anchor"]["token"]["head"] == p.i
                                    && d["anchor"]["token"]["dep"]
                                        .as_str()
                                        .is_some_and(|v| dependencies.contains(&v))
                            })
                        });
                        ledger_findings.push(json!({"check":label,"met":met}));
                    }
                    for e in &frame_assertions {
                        for f in array(mp, "frame_possibilities")?.iter().filter(|f| {
                            e.get("frame_id").is_none_or(|v| *v == f["frame"]["id"])
                                && e.get("member_class")
                                    .is_none_or(|v| *v == f["member_class_id"])
                                && e.get("frame_ids").is_none_or(|v| {
                                    v.as_array()
                                        .is_some_and(|ids| ids.contains(&f["frame"]["id"]))
                                })
                        }) {
                            for binding in array(f, "bindings")?
                                .iter()
                                .filter(|b| b["role_name"] == e["role"])
                            {
                                let linked = array(binding, "alternatives")?.iter().any(|a| {
                                    a["proposition_head_index"] == p.i
                                        && a["anchor"]["token"]["start_byte"] == p.start_byte
                                        && a["anchor"]["token"]["end_byte"] == p.end_byte
                                });
                                if linked {
                                    frame_links.push(json!({"member_class":f["member_class_id"],"frame_id":f["frame"]["id"],"role":binding["role_name"],"syntax_binding_complete":f["syntax_binding_complete"],"syntactic_binding_status":f["syntactic_binding_status"],"full_compatibility_status":f["full_compatibility_status"]}));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let raw_status = if doc.is_none() {
        "unavailable"
    } else {
        bool_status(Some(raw_findings.iter().all(|f| f["met"] == true)))
    };
    let ledger_status = if row["observation"].is_null() {
        "unavailable"
    } else {
        bool_status(Some(
            !ledger_findings.is_empty() && ledger_findings.iter().all(|f| f["met"] == true),
        ))
    };
    let frame_partial = if !frame_requested || !frame_evidence_available {
        None
    } else {
        Some(
            frame_links
                .iter()
                .any(|f| f["syntactic_binding_status"] != "contradicted"),
        )
    };
    let frame_complete = if !frame_requested || !frame_evidence_available {
        None
    } else {
        Some(
            frame_links
                .iter()
                .any(|f| f["syntax_binding_complete"] == true),
        )
    };
    Ok(
        json!({"expectation":expected,"raw_status":raw_status,"raw_findings":raw_findings,"ledger_status":ledger_status,"ledger_findings":ledger_findings,"frame_link_requested":frame_requested,"frame_evidence_available":frame_evidence_available,"frame_link_partial_status":bool_status(frame_partial),"frame_link_complete_status":bool_status(frame_complete),"frame_links":frame_links,"semantic_equivalence_certified":false,"automatic_edit_license":false}),
    )
}

fn target_summary(
    row: &Value,
    ancestors: &BTreeMap<String, BTreeSet<String>>,
) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    let selected = string(&row["case"], "selected_class")?;
    for target in array(row, "target_predicates")? {
        let predicate = predicate_at(row, &target["predicate_span"]);
        let found = predicate.is_some();
        ensure!(target["found"] == found, "Saved target presence differs");
        let complete = predicate.map(|p| {
            p["frame_possibilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["syntax_binding_complete"] == true)
        });
        ensure!(
            target["any_complete_syntax"] == json!(complete),
            "Saved target completion differs"
        );
        let selected_complete = predicate.map(|p| {
            p["frame_possibilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| {
                    f["syntax_binding_complete"] == true
                        && f["member_class_id"].as_str().is_some_and(|c| {
                            c == selected || ancestors.get(c).is_some_and(|a| a.contains(selected))
                        })
                })
        });
        result.push(json!({"predicate_span":target["predicate_span"],"found":found,"observed_lemma":target["observed_lemma"],"any_complete_syntax":complete,"selected_root_complete_syntax":selected_complete}));
    }
    Ok(result)
}

fn compare_case(a: &SavedRun, b: &SavedRun, id: &str) -> Result<Value> {
    let left = &a.cases[id];
    let right = &b.cases[id];
    ensure!(left["case"] == right["case"], "Contrast cases differ");
    let case = &left["case"];
    let sha = string(case, "text_sha256")?;
    let da = a.annotations[sha].as_ref();
    let db = b.annotations[sha].as_ref();
    let mut expectations = Vec::new();
    let mut failed_roles = false;
    for (index, (l, r)) in array(left, "expectations")?
        .iter()
        .zip(array(right, "expectations")?)
        .enumerate()
    {
        ensure!(
            l["expectation"] == r["expectation"],
            "Contrast expectation identity differs"
        );
        let baseline = expectation_status(l)?;
        let candidate = expectation_status(r)?;
        let e = &l["expectation"];
        let role = e["kind"] == "role_binding";
        failed_roles |= role && (baseline != "met" || candidate != "met");
        expectations.push(json!({"index":index,"expectation":e,"role_requirement":if role {if e["require_complete_syntax"]==true {"complete"} else {"partial_allowed"}} else {"not_role"},"comparison":transition(baseline,candidate),"baseline_reason":l["reason"],"candidate_reason":r["reason"]}));
    }
    let ta = target_summary(left, &a.ancestors)?;
    let tb = target_summary(right, &b.ancestors)?;
    ensure!(ta.len() == tb.len(), "Target count differs");
    let targets=ta.iter().zip(&tb).map(|(l,r)|{ensure!(l["predicate_span"]==r["predicate_span"],"Target order differs");Ok(json!({"predicate_span":l["predicate_span"],"baseline":l,"candidate":r,"any_completion":transition(bool_status(l["any_complete_syntax"].as_bool()),bool_status(r["any_complete_syntax"].as_bool())),"selected_root_completion":transition(bool_status(l["selected_root_complete_syntax"].as_bool()),bool_status(r["selected_root_complete_syntax"].as_bool()))}))}).collect::<Result<Vec<_>>>()?;
    let mut linked = Vec::new();
    if let Some(items) = case.get("linked_proposition_expectations") {
        for e in items
            .as_array()
            .context("Linked expectations must be an array")?
        {
            let l = linked_check(left, da, e)?;
            let r = linked_check(right, db, e)?;
            linked.push(json!({"expectation":e,"baseline":l,"candidate":r,"raw_comparison":transition(string(&l,"raw_status")?,string(&r,"raw_status")?),"ledger_comparison":transition(string(&l,"ledger_status")?,string(&r,"ledger_status")?)}));
        }
    }
    let any_link_failure = linked.iter().any(|l| {
        l["baseline"]["raw_status"] != "met"
            || l["candidate"]["raw_status"] != "met"
            || l["baseline"]["ledger_status"] != "met"
            || l["candidate"]["ledger_status"] != "met"
    });
    Ok(
        json!({"case_id":id,"split":case["split"],"lemma":case["lemma"],"selected_class":case["selected_class"],"category":case["category"],"source_sha256":sha,
        "baseline_variant":a.variant,"candidate_variant":b.variant,"baseline_case_status":left["status"],"candidate_case_status":right["status"],
        "baseline_parse_error":left["parse_error"],"candidate_parse_error":right["parse_error"],"baseline_observation_error":left["observation_error"],"candidate_observation_error":right["observation_error"],
        "baseline_saved_annotation_error":a.annotation_errors.get(sha),"candidate_saved_annotation_error":b.annotation_errors.get(sha),
        "documents_equal":da.zip(db).map(|(a,b)|a==b),"tokens_equal":da.zip(db).map(|(a,b)|a.tokens==b.tokens),"sentences_equal":da.zip(db).map(|(a,b)|a.sentences==b.sentences),
        "expectations":expectations,"targets":targets,"linked_propositions":linked,
        "failed_role_check_in_either_variant":failed_roles,"raw_annotation_comparison":if failed_roles||any_link_failure {Some(raw_diff(da,db))}else{None},
        "automatic_edit_license":false,"semantic_equivalence_certified":false}),
    )
}

fn summarize_cases(cases: &[&Value]) -> Value {
    let mut counts = BTreeMap::<String, usize>::new();
    let mut expectation_groups = BTreeMap::<(String, String), BTreeMap<String, usize>>::new();
    let mut target_transitions = BTreeMap::new();
    let mut linked_transitions = BTreeMap::new();
    for c in cases {
        increment(&mut counts, "cases");
        *counts.entry("targets".into()).or_default() += c["targets"].as_array().unwrap().len();
        *counts
            .entry("linked_proposition_expectations".into())
            .or_default() += c["linked_propositions"].as_array().unwrap().len();
        for (key, value) in [
            ("documents_equal", &c["documents_equal"]),
            ("tokens_equal", &c["tokens_equal"]),
            ("sentences_equal", &c["sentences_equal"]),
        ] {
            increment(
                &mut counts,
                &format!(
                    "{key}_{}",
                    if value.is_null() {
                        "unavailable"
                    } else if value == true {
                        "true"
                    } else {
                        "false"
                    }
                ),
            );
        }
        for side in ["baseline", "candidate"] {
            increment(
                &mut counts,
                &format!(
                    "{side}_{}",
                    c[format!("{side}_case_status")].as_str().unwrap()
                ),
            );
        }
        for e in c["expectations"].as_array().unwrap() {
            let key = (
                e["expectation"]["kind"].as_str().unwrap().to_owned(),
                e["role_requirement"].as_str().unwrap().to_owned(),
            );
            let group = expectation_groups.entry(key).or_default();
            increment(group, "expectations");
            increment(group, e["comparison"]["transition"].as_str().unwrap());
            for key in [
                "improved_to_met",
                "regressed_from_met",
                "availability_gained",
                "availability_lost",
            ] {
                if e["comparison"][key] == true {
                    increment(group, key);
                }
            }
        }
        for target in c["targets"].as_array().unwrap() {
            for field in ["any_completion", "selected_root_completion"] {
                increment(
                    &mut target_transitions,
                    &format!("{field}:{}", target[field]["transition"].as_str().unwrap()),
                );
            }
        }
        for link in c["linked_propositions"].as_array().unwrap() {
            for field in ["raw_comparison", "ledger_comparison"] {
                increment(
                    &mut linked_transitions,
                    &format!("{field}:{}", link[field]["transition"].as_str().unwrap()),
                );
            }
        }
    }
    json!({"counts":counts,"expectation_groups":expectation_groups.into_iter().map(|((kind,requirement),counts)|json!({"kind":kind,"role_requirement":requirement,"counts":counts})).collect::<Vec<_>>(),"target_transitions":target_transitions,"linked_proposition_transitions":linked_transitions})
}

fn compare_pairs(a: &SavedRun, b: &SavedRun) -> Result<Vec<Value>> {
    ensure!(
        a.pairs.keys().eq(b.pairs.keys()),
        "Pair coverage differs between variants"
    );
    a.pairs.iter().map(|(key,l)|{
        let r=&b.pairs[key];ensure!(l["word_occurrences_equal"]==r["word_occurrences_equal"],"Pair word counts differ");
        let mut comparisons=BTreeMap::new();
        for field in ["raw_argument_signature_changed","complete_frame_binding_signature_changed"] {
            let labels=|v:&Value|if v.is_null(){"unavailable"}else if v==true{"changed"}else{"unchanged"};
            comparisons.insert(field,json!({"baseline":l[field],"candidate":r[field],"transition":format!("{}_to_{}",labels(&l[field]),labels(&r[field]))}));
        }
        let linked=|id:&str|a.cases[id]["case"]["linked_proposition_expectations"].as_array().is_some_and(|e|!e.is_empty());
        Ok(json!({"source_case_id":key.0,"candidate_case_id":key.1,"split":l["split"],"lemma":l["lemma"],"word_occurrences_equal":l["word_occurrences_equal"],"linked_proposition_pair":linked(&key.0)&&linked(&key.1),"comparisons":comparisons,"automatic_edit_license":false}))
    }).collect()
}

fn compare_panel(
    runs: &BTreeMap<String, SavedRun>,
    out: &Path,
    panel: &str,
    artifacts: &mut Vec<Value>,
) -> Result<Value> {
    let mut contrasts = Vec::new();
    for (baseline, candidate) in CONTRASTS {
        let a = &runs[baseline];
        let b = &runs[candidate];
        ensure!(
            a.cases.keys().eq(b.cases.keys()),
            "Contrast case coverage differs"
        );
        let cases = a
            .cases
            .keys()
            .map(|id| compare_case(a, b, id))
            .collect::<Result<Vec<_>>>()?;
        let pairs = compare_pairs(a, b)?;
        let name = format!("{panel}/{baseline}-to-{candidate}");
        let case_binding = output(out, &format!("{name}-cases.json"), &json!(cases))?;
        artifacts.push(case_binding.clone());
        let pair_binding = output(out, &format!("{name}-pairs.json"), &json!(pairs))?;
        artifacts.push(pair_binding.clone());
        let mut groups = Vec::new();
        for field in ["split", "lemma", "selected_class", "category"] {
            let mut grouped = BTreeMap::<String, Vec<&Value>>::new();
            for row in &cases {
                grouped
                    .entry(string(row, field)?.into())
                    .or_default()
                    .push(row);
            }
            for (value, rows) in grouped {
                groups.push(json!({"field":field,"value":value,"summary":summarize_cases(&rows)}));
            }
        }
        let mut pair_counts = BTreeMap::new();
        for pair in &pairs {
            increment(&mut pair_counts, "pairs");
            if pair["word_occurrences_equal"] == true {
                increment(&mut pair_counts, "equal_word_occurrence_pairs");
            }
            if pair["linked_proposition_pair"] == true {
                increment(&mut pair_counts, "linked_proposition_pairs");
            }
            for field in [
                "raw_argument_signature_changed",
                "complete_frame_binding_signature_changed",
            ] {
                increment(
                    &mut pair_counts,
                    &format!(
                        "{field}:{}",
                        pair["comparisons"][field]["transition"].as_str().unwrap()
                    ),
                );
            }
        }
        contrasts.push(json!({"baseline":baseline,"candidate":candidate,"summary":summarize_cases(&cases.iter().collect::<Vec<_>>()),"groups":groups,"pair_counts":pair_counts,"case_artifact":case_binding,"pair_artifact":pair_binding}));
    }
    Ok(
        json!({"panel":panel,"variants":runs.values().map(|r|json!({"variant":r.variant,"provenance":r.provenance,"original_summary":r.report["summary"],"original_pair_summary":r.report["pair_summary"],"timing":r.report["timing"]})).collect::<Vec<_>>(),"contrasts":contrasts}),
    )
}

fn run(args: Args) -> Result<()> {
    let protocol_bytes = checked(&args.protocol, &args.expected_protocol_sha256)?;
    let protocol: Value = serde_json::from_slice(&protocol_bytes)?;
    let inputs_bytes = checked(&args.inputs, &args.expected_inputs_sha256)?;
    let inputs: Value = serde_json::from_slice(&inputs_bytes)?;
    ensure!(
        protocol["schema"] == "slopninja-lexical-frame-parser-comparison-protocol-v1"
            && inputs["schema"] == "slopninja-lexical-frame-parser-comparison-inputs-v1",
        "Unknown comparison schema"
    );
    ensure!(
        inputs["protocol_sha256"] == args.expected_protocol_sha256,
        "Inputs protocol differs"
    );
    ensure!(
        protocol["variant_ids"] == json!(VARIANTS)
            && protocol["baseline_variant"] == "incumbent_sm",
        "Variant catalog differs"
    );
    ensure!(
        protocol["contrasts"]
            == json!(
                CONTRASTS.map(
                    |(baseline, candidate)| json!({"baseline":baseline,"candidate":candidate})
                )
            ),
        "Contrasts differ"
    );
    ensure!(
        protocol["parser_calls"] == 0 && protocol["automatic_edit_license"] == false,
        "Unsupported comparison activity"
    );
    ensure!(
        digest(&fs::read(std::env::current_exe()?)?) == string(&protocol, "executable_sha256")?,
        "Comparator executable differs"
    );
    let source_files = array(&protocol, "source_files")?;
    ensure!(!source_files.is_empty(), "No comparator source bindings");
    for source in source_files {
        safe(string(source, "path")?)?;
        bound(&args.repo, source)?;
    }
    fs::create_dir(&args.out).context("Create fresh comparison output")?;
    let result = (|| -> Result<()> {
        let mut artifacts = vec![
            write(&args.out, "protocol.json", &protocol_bytes)?,
            write(&args.out, "inputs.json", &inputs_bytes)?,
        ];
        for source in source_files {
            artifacts.push(write(
                &args.out,
                &format!("executed-source/{}", string(source, "path")?),
                &bound(&args.repo, source)?,
            )?);
        }
        let mut input_map = BTreeMap::new();
        for input in array(&inputs, "runs")? {
            let key = (
                string(input, "panel")?.to_owned(),
                string(input, "variant")?.to_owned(),
            );
            ensure!(
                input_map.insert(key, input).is_none(),
                "Duplicate run input"
            );
        }
        let mut panels = Vec::new();
        let mut producer_settings = None;
        let mut panel_ids = BTreeSet::new();
        let mut fixture_texts = BTreeSet::new();
        let mut reused = 0;
        for panel in array(&protocol, "panels")? {
            let id = string(panel, "id")?;
            ensure!(
                ["reused_development", "fresh_texts"].contains(&id) && panel_ids.insert(id),
                "Unexpected panel"
            );
            let fixture = bound_json(&args.repo, &panel["fixture"])?;
            let cases = array(&fixture, "cases")?;
            ensure!(
                (id == "reused_development" && cases.len() == 96)
                    || (id == "fresh_texts" && cases.len() == 64),
                "Panel case budget differs"
            );
            for case in cases {
                ensure!(
                    fixture_texts.insert(string(case, "text_sha256")?.to_owned()),
                    "Fixture duplicate or cross-panel text reuse"
                );
            }
            let mut runs = BTreeMap::new();
            for descriptor in array(panel, "runs")? {
                let variant = string(descriptor, "variant")?;
                ensure!(VARIANTS.contains(&variant), "Unexpected variant");
                if descriptor["reuse_saved"] == true {
                    ensure!(
                        id == "reused_development" && variant == "incumbent_sm",
                        "Unexpected reuse declaration"
                    );
                    reused += 1;
                }
                let input = input_map
                    .remove(&(id.into(), variant.into()))
                    .context("Missing run input")?;
                let loaded = load_run(&args.repo, descriptor, input, &fixture)?;
                let settings = json!({"source_bindings":loaded.protocol["source_bindings"],"resource_manifest":loaded.protocol["resource_manifest"],"executable_sha256":loaded.protocol["executable_sha256"],"parser_batch_size":loaded.protocol["parser_batch_size"]});
                if let Some(previous) = &producer_settings {
                    ensure!(
                        *previous == settings,
                        "Producer implementation/resource changed across parsers"
                    );
                } else {
                    producer_settings = Some(settings);
                }
                ensure!(
                    runs.insert(variant.into(), loaded).is_none(),
                    "Duplicate variant"
                );
            }
            ensure!(runs.len() == 4, "Incomplete four-variant panel");
            panels.push(compare_panel(&runs, &args.out, id, &mut artifacts)?);
        }
        ensure!(
            panel_ids.len() == 2 && input_map.is_empty() && reused == 1,
            "Incomplete panel/input coverage or reuse mismatch"
        );
        let report = json!({"schema":"slopninja-lexical-frame-parser-comparison-report-v1","protocol_sha256":args.expected_protocol_sha256,"inputs_sha256":args.expected_inputs_sha256,"panels":panels,"artifacts":artifacts,"parser_calls":0,"model_fit_calls":0,"detector_calls":0,"semantic_equivalence_certified":false,"automatic_edit_license":false,"interpretation":"Descriptive comparison of fixed engineering expectations on reused development texts and new texts with known lemmas; parser acceptance is not semantic truth or detector progress."});
        let report_binding = output(&args.out, "report.json", &report)?;
        output(
            &args.out,
            "receipt.json",
            &json!({"schema":"slopninja-lexical-frame-parser-comparison-receipt-v1","status":"completed","protocol_sha256":args.expected_protocol_sha256,"inputs_sha256":args.expected_inputs_sha256,"report":report_binding,"parser_calls":0,"automatic_edit_license":false}),
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
pub fn main() -> Result<()> {
    run(Args::parse())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::syntax::Sentence;

    fn example() -> (Document, Value, Value) {
        let words = ["Ada", "states", "that", "Ada", "respects", "Ben", "."];
        let lemmas = ["Ada", "state", "that", "Ada", "respect", "Ben", "."];
        let dependencies = ["nsubj", "ROOT", "mark", "nsubj", "ccomp", "dobj", "punct"];
        let positions = ["PROPN", "VERB", "SCONJ", "PROPN", "VERB", "PROPN", "PUNCT"];
        let heads = [1, 1, 4, 4, 1, 4, 1];
        let text = words.join(" ");
        let mut offset = 0;
        let tokens = words
            .iter()
            .enumerate()
            .map(|(i, word)| {
                let start_byte = offset;
                offset += word.len() + 1;
                Token {
                    i,
                    start_byte,
                    end_byte: offset - 1,
                    text: (*word).into(),
                    lemma: lemmas[i].into(),
                    pos: positions[i].into(),
                    tag: positions[i].into(),
                    dep: dependencies[i].into(),
                    head: heads[i],
                    sentence: 0,
                    morph: BTreeMap::new(),
                    is_punct: i == 6,
                    is_space: false,
                }
            })
            .collect::<Vec<_>>();
        let doc = Document {
            text: text.clone(),
            parser_identity: "synthetic-a".into(),
            sentences: vec![Sentence {
                start_byte: 0,
                end_byte: text.len(),
                root: 1,
                token_start: 0,
                token_end: tokens.len(),
            }],
            tokens,
        };
        syntax::validate(&doc).unwrap();
        let s = |i: usize| json!([doc.tokens[i].start_byte, doc.tokens[i].end_byte]);
        let anchor = |i: usize| json!({"token": doc.tokens[i], "normalized_lemma": normalize(&doc.tokens[i].lemma)});
        let expected = json!({"matrix_predicate_span": s(1), "proposition_span": s(4),
            "subject_span": s(3), "object_span": s(5), "proposition_lemma": "respect"});
        let assertion = json!({"kind":"role_binding", "predicate_span":s(1), "argument_span":s(4),
            "role":"Topic", "member_class":"root", "frame_id":"frame-a", "require_complete_syntax":true});
        let second = json!({"kind":"predicate_span", "predicate_span":s(4), "lemma":"respect"});
        let case = json!({"text":text, "selected_class":"root", "expectations":[assertion,second],
            "linked_proposition_expectations":[expected]});
        let row = json!({"case":case, "status":"evaluated", "expectations":[
        {"expectation":assertion, "status":"met"}, {"expectation":second, "status":"met"}],
        "target_predicates":[
            {"predicate_span":s(1),"found":true,"any_complete_syntax":true,"observed_lemma":"state"},
            {"predicate_span":s(4),"found":true,"any_complete_syntax":false,"observed_lemma":"respect"}],
        "observation":{"predicates":[
            {"head":anchor(1), "direct_dependents":[{"anchor":anchor(4),"proposition_head_indices":[4]}],
             "frame_possibilities":[{"member_class_id":"root", "frame":{"id":"frame-a"},
                 "syntax_binding_complete":true, "syntactic_binding_status":"checked", "full_compatibility_status":"unresolved",
                 "bindings":[{"role_name":"Topic","alternatives":[{"anchor":anchor(4),"proposition_head_index":4}]}]}]},
            {"head":anchor(4), "direct_dependents":[{"anchor":anchor(3)},{"anchor":anchor(5)}],"frame_possibilities":[]}
        ]}});
        (doc, row, expected)
    }

    #[test]
    fn missing_evidence_is_unavailable_but_observed_missing_links_mismatch() {
        let (doc, mut row, expected) = example();
        row["observation"] = Value::Null;
        let missing = linked_check(&row, None, &expected).unwrap();
        assert_eq!(missing["frame_link_requested"], true);
        for field in [
            "raw_status",
            "ledger_status",
            "frame_link_partial_status",
            "frame_link_complete_status",
        ] {
            assert_eq!(missing[field], "unavailable");
        }
        let parsed = linked_check(&row, Some(&doc), &expected).unwrap();
        assert_eq!(parsed["raw_status"], "met");
        assert_eq!(parsed["ledger_status"], "unavailable");
        assert_eq!(parsed["frame_link_complete_status"], "unavailable");
        row["observation"] = json!({"predicates":[]});
        let absent = linked_check(&row, Some(&doc), &expected).unwrap();
        assert_eq!(absent["ledger_status"], "mismatch");
        assert_eq!(absent["frame_link_partial_status"], "mismatch");
        assert_eq!(absent["frame_link_complete_status"], "mismatch");
    }

    #[test]
    fn repeated_names_bind_the_declared_occurrence() {
        let (doc, row, mut expected) = example();
        let correct = linked_check(&row, Some(&doc), &expected).unwrap();
        assert_eq!(correct["raw_status"], "met");
        assert_eq!(correct["ledger_status"], "met");
        expected["subject_span"] = json!([doc.tokens[0].start_byte, doc.tokens[0].end_byte]);
        assert_eq!(doc.tokens[0].lemma, doc.tokens[3].lemma);
        let wrong = linked_check(&row, Some(&doc), &expected).unwrap();
        assert_eq!(wrong["raw_status"], "mismatch");
        assert_eq!(wrong["ledger_status"], "mismatch");
    }

    #[test]
    fn partial_frame_link_does_not_imply_completion_and_honors_frame_filters() {
        let (doc, mut row, expected) = example();
        let frame = &mut row["observation"]["predicates"][0]["frame_possibilities"][0];
        frame["syntax_binding_complete"] = json!(false);
        frame["syntactic_binding_status"] = json!("unresolved");
        let partial = linked_check(&row, Some(&doc), &expected).unwrap();
        assert_eq!(partial["frame_link_partial_status"], "met");
        assert_eq!(partial["frame_link_complete_status"], "mismatch");
        assert_eq!(
            partial["frame_links"][0]["full_compatibility_status"],
            "unresolved"
        );
        row["case"]["expectations"][0]["frame_ids"] = json!(["different-frame"]);
        let filtered = linked_check(&row, Some(&doc), &expected).unwrap();
        assert_eq!(filtered["frame_link_partial_status"], "mismatch");
        assert_eq!(filtered["frame_links"], json!([]));
    }

    #[test]
    fn multiple_heads_are_distinct_targets_and_duplicate_coverage_is_rejected() {
        let (_, mut row, _) = example();
        validate_case(&row, &row["case"]).unwrap();
        let targets = target_summary(&row, &BTreeMap::new()).unwrap();
        assert_eq!(targets.len(), 2);
        assert_ne!(targets[0]["predicate_span"], targets[1]["predicate_span"]);
        assert_eq!(targets[0]["selected_root_complete_syntax"], true);
        assert_eq!(targets[1]["selected_root_complete_syntax"], false);
        row["target_predicates"][1] = row["target_predicates"][0].clone();
        assert!(validate_case(&row, &row["case"]).is_err());
    }

    #[test]
    fn annotation_identity_and_exact_parent_changes_are_separate() {
        let (doc, _, _) = example();
        let mut other = doc.clone();
        other.parser_identity = "synthetic-b".into();
        let identity = raw_diff(Some(&doc), Some(&other));
        assert_eq!(identity["documents_equal"], false);
        assert_eq!(identity["tokens_equal"], true);
        assert_eq!(identity["sentences_equal"], true);
        assert_eq!(identity["token_changes"], json!([]));
        other.tokens[3].head = 1;
        syntax::validate(&other).unwrap();
        let changed = raw_diff(Some(&doc), Some(&other));
        assert_eq!(changed["token_changes"].as_array().unwrap().len(), 1);
        assert_eq!(
            changed["token_changes"][0]["span"],
            json!([doc.tokens[3].start_byte, doc.tokens[3].end_byte])
        );
        assert_eq!(
            changed["token_changes"][0]["changed_fields"],
            json!(["head_span"])
        );
        assert_eq!(
            changed["baseline_markers"][0]["head_span"],
            json!([doc.tokens[4].start_byte, doc.tokens[4].end_byte])
        );
    }

    #[test]
    fn summaries_separate_role_requirements_missingness_and_target_counts() {
        let lost = transition("mismatch", "unavailable");
        assert_eq!(lost["improved_to_met"], false);
        assert_eq!(lost["regressed_from_met"], false);
        assert_eq!(lost["availability_lost"], true);
        let row = json!({"documents_equal":null,"tokens_equal":null,"sentences_equal":null,
            "baseline_case_status":"evaluated","candidate_case_status":"parse_error",
            "expectations":[
                {"expectation":{"kind":"role_binding"},"role_requirement":"complete","comparison":lost},
                {"expectation":{"kind":"role_binding"},"role_requirement":"partial_allowed","comparison":transition("met","unavailable")}],
            "targets":[
                {"any_completion":transition("met","unavailable"),"selected_root_completion":transition("met","unavailable")},
                {"any_completion":transition("mismatch","unavailable"),"selected_root_completion":transition("mismatch","unavailable")}],
            "linked_propositions":[]});
        let summary = summarize_cases(&[&row]);
        assert_eq!(summary["counts"]["cases"], 1);
        assert_eq!(summary["counts"]["targets"], 2);
        assert_eq!(summary["counts"]["tokens_equal_unavailable"], 1);
        let groups = summary["expectation_groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["role_requirement"], "complete");
        assert_eq!(groups[0]["counts"]["mismatch_to_unavailable"], 1);
        assert_eq!(groups[1]["role_requirement"], "partial_allowed");
        assert_eq!(groups[1]["counts"]["regressed_from_met"], 1);
    }
}

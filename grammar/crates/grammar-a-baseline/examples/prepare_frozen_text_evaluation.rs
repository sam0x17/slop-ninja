//! Frozen Test-query join; no fitting, inference, acquisition or provider access.
//! SELECTION CANDIDATE RECORDS MANIFEST CFG_BUNDLE EMBEDDINGS NEW_OUTPUT
//! EMBEDDINGS uses the retained CPU v6_embeddings manifest/requests/rows layout.
//! References require their separate authorization/transform route and are rejected.
//! MANIFEST must bind selection_receipt_sha256, records_sha256, exact test_ids,
//! records/requested_records, split_assignments, excluded and generation_reports.
//! Every report has complete/requested/accepted/failed/failures. Upstream terminal
//! IDs and admitted IDs must reconcile exactly; no synthetic success is inferred.
//! Text mode needs V6 success only; CFG execution remains diagnostic. Combined
//! mode requires both. Text's zero CFG block is the frozen architecture constant.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};

#[allow(dead_code)]
#[path = "compare_supervised_pangram/test_binding.rs"]
mod evaluation_binding;
#[path = "final_test_binding/mod.rs"]
mod final_test_binding;
#[path = "prepare_frozen_text_evaluation/inputs.rs"]
mod inputs;
#[allow(dead_code)]
#[path = "prepare_supervised_forest/supervision.rs"]
mod supervision;
#[cfg(test)]
#[path = "prepare_frozen_text_evaluation/tests.rs"]
mod tests;

const PARTS: [&str; 3] = ["train", "development", "calibration"];
const TEXT: usize = 768;
const ARTIFACT: &str = "506b1c66a6a2ffc4eb4e50b9b057657ce0e5b6f04cff8caf3df496c956dc0d5f";
type Rows = BTreeMap<String, Value>;
type Files = BTreeMap<String, Vec<u8>>;
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn str_at<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("missing {k}"))
}
fn digest_at<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    let s = str_at(v, k)?;
    ensure!(
        s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid {k}"
    );
    Ok(s)
}
fn counter(v: &Value, k: &str) -> Result<u64> {
    v[k].as_u64().with_context(|| format!("count {k}"))
}
fn finite(v: &Value) -> Result<f64> {
    let n = v.as_f64().context("number")?;
    ensure!(n.is_finite() && (n as f32).is_finite(), "nonfinite number");
    Ok(n)
}
// Same nonnegative semantics required by the reused supervision module.
fn number(v: &Value) -> Result<f64> {
    let n = finite(v)?;
    ensure!(n >= 0., "negative supervision measurement");
    Ok(n)
}
fn array<'a>(v: &'a Value, k: &str) -> Result<&'a Vec<Value>> {
    v[k].as_array().with_context(|| format!("array {k}"))
}
fn vector(v: &Value, n: usize) -> Result<Vec<f64>> {
    let a = v.as_array().context("vector")?;
    ensure!(a.len() == n, "vector dimension");
    a.iter().map(finite).collect()
}
fn read(p: &Path) -> Result<(Vec<u8>, Value)> {
    let b = fs::read(p).with_context(|| format!("read {}", p.display()))?;
    let v = serde_json::from_slice(&b)?;
    Ok((b, v))
}
fn bound(dir: &Path, file: &str, hash: &Value) -> Result<Vec<u8>> {
    let b = fs::read(dir.join(file))?;
    ensure!(hash.as_str() == Some(sha(&b).as_str()), "changed {file}");
    Ok(b)
}
fn rows(b: &[u8]) -> Result<Rows> {
    let mut out = Rows::new();
    for line in std::str::from_utf8(b)?.lines() {
        let v: Value = serde_json::from_str(line)?;
        ensure!(
            out.insert(str_at(&v, "id")?.into(), v).is_none(),
            "duplicate row ID"
        );
    }
    Ok(out)
}
fn jsonl(values: impl IntoIterator<Item = Value>) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    for v in values {
        serde_json::to_writer(&mut b, &v)?;
        b.push(b'\n');
    }
    Ok(b)
}
fn pretty(v: &Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(v)?)
}

pub(crate) struct Parent {
    manifest: Value,
    contract: Value,
    population: Value,
    files: Files,
    ids: BTreeSet<String>,
    groups: BTreeSet<String>,
    warm: bool,
    mode: String,
    cfg_dimension: usize,
}
impl Parent {
    pub(crate) fn load(candidate: &Value) -> Result<Self> {
        let dir = Path::new(str_at(candidate, "numeric_path")?);
        ensure!(dir.is_absolute(), "selection numeric_path must be absolute");
        let mb = bound(dir, "manifest.json", &candidate["numeric_manifest_sha256"])?;
        let cb = bound(dir, "contract.json", &candidate["contract_file_sha256"])?;
        let m: Value = serde_json::from_slice(&mb)?;
        let c: Value = serde_json::from_slice(&cb)?;
        ensure!(
            m["schema"] == "slop_ninja_shared_a_dataset_v1"
                && m["final_test_opened"] == false
                && c["final_test_opened"] == false
                && m["contract_sha256"] == candidate["contract_sha256"]
                && m["contract_sha256"] == sha(&serde_json::to_vec(&c)?)
                && m["contract_file_sha256"] == sha(&cb),
            "frozen parent binding"
        );
        let mode = str_at(&c, "ablation")?.to_owned();
        let warm = c.get("warm_start_schema").is_some();
        ensure!(
            c["schema"] == "slop_ninja_shared_a_numeric_contract_v1"
                && c["fusion_schema"] == "slop_ninja_v6_fusion_numeric_contract_v1"
                && matches!(mode.as_str(), "text" | "combined"),
            "only frozen text/combined candidates supported"
        );
        if warm {
            ensure!(
                c["warm_start_schema"] == "slop_ninja_raw_v6_cfg_residual_input_v1"
                    && c["feature_policy"]
                        == "raw_v6_classifier_input_plus_unchanged_train_frozen_cfg_v1"
                    && c["feature_policy_version"] == 2,
                "unsupported warm layout"
            );
        } else {
            ensure!(
                c["feature_policy_version"] == 1
                    && matches!(
                        c["feature_policy"].as_str(),
                        Some(
                            "slop_ninja_v6_fusion_existing450_v1"
                                | "slop_ninja_v6_fusion_manifest_cohort_v1"
                        )
                    ),
                "unsupported MLP layout"
            );
        }
        let n = counter(&c, "input_dimension")? as usize;
        ensure!(
            n > TEXT && c["cfg_parent_contract"]["input_dimension"] == n - TEXT,
            "CFG dimension"
        );
        ensure!(
            c["cfg_parent_contract_sha256"] == sha(&serde_json::to_vec(&c["cfg_parent_contract"])?)
                && c["representation_contract"]["artifact_id"] == ARTIFACT
                && c["representation_contract"]["dimension"] == TEXT
                && c["representation_contract"]["embedding_dtype"] == "float32"
                && c["representation_contract"]["runtime"]["device"] == "cpu"
                && c["representation_contract"]["hook"]["normalization"] == "none"
                && c["representation_contract"]["hook"]["projection"] == "none"
                && c["representation_contract"]["input"]["truncation"] == false,
            "frozen representation/CFG contract differs"
        );
        let coordinates = array(&c, "input_coordinates")?;
        ensure!(coordinates.len() == n, "coordinate count");
        for (i, key) in coordinates.iter().enumerate() {
            let expected = if i < TEXT {
                format!(
                    "v6_penultimate_{}/{i}",
                    if warm { "raw" } else { "standardized" }
                )
            } else {
                format!("frozen_cfg/x/{}", i - TEXT)
            };
            ensure!(key == &json!(expected), "coordinate order differs");
        }
        let norm = &c["embedding_normalization"];
        ensure!(
            norm["dimension"] == TEXT
                && norm["transform"] == "(embedding[i]-mean[i])/scale[i]"
                && norm["fit"] == "train_query_only_equal_source_family_weight"
                && [
                    "reference_used_for_fit",
                    "development_used_for_fit",
                    "calibration_used_for_fit",
                    "final_test_opened"
                ]
                .iter()
                .all(|k| norm[*k] == false),
            "normalization scope"
        );
        vector(&norm["mean"], TEXT)?;
        ensure!(
            vector(&norm["scale"], TEXT)?.iter().all(|v| *v > 0.),
            "normalization scale"
        );
        for key in ["human_profiles", "ai_profiles", "origin_order"] {
            ensure!(
                c[key] == c["cfg_parent_contract"][key],
                "frozen {key} changed"
            );
        }
        let pb = bound(dir, "population.json", &m["population_file_sha256"])?;
        let population: Value = serde_json::from_slice(&pb)?;
        let mut files = Files::from([
            ("contract.json".into(), cb),
            ("parent-manifest.json".into(), mb),
            ("parent-population.json".into(), pb),
        ]);
        if m.get("baseline_origin_reference_sha256").is_some() {
            files.insert(
                "baseline-origin-reference.jsonl".into(),
                bound(
                    dir,
                    "baseline-origin-reference.jsonl",
                    &m["baseline_origin_reference_sha256"],
                )?,
            );
        }
        if let Some(name) = m.get("dataset_manifest_file") {
            ensure!(
                name == "training-population.json",
                "unsupported parent dataset sidecar"
            );
            files.insert(
                "training-population.json".into(),
                bound(
                    dir,
                    "training-population.json",
                    &m["dataset_manifest_sha256"],
                )?,
            );
        }
        let (mut ids, mut groups, mut parts) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        for binding in array(&m, "splits")? {
            let part = str_at(binding, "split")?;
            ensure!(
                (PARTS.contains(&part) || part == "reference") && parts.insert(part),
                "parent split roster"
            );
            let filename = format!("{part}.jsonl");
            let bytes = bound(dir, &filename, &binding["sha256"])?;
            let rr = rows(&bytes)?;
            ensure!(
                rr.len() as u64 == counter(binding, "rows")?,
                "parent row count"
            );
            for (id, r) in rr {
                ensure!(
                    ids.insert(id)
                        && r["input"]["contract_sha256"] == m["contract_sha256"]
                        && r["frozen_split"] == if part == "reference" { "train" } else { part }
                        && r["purpose"]
                            == if part == "reference" {
                                "reference"
                            } else {
                                "query"
                            },
                    "parent row binding/scope"
                );
                groups.insert(str_at(&r, "source_group")?.to_owned());
            }
            files.insert(filename, bytes);
        }
        ensure!(
            parts == BTreeSet::from(["train", "development", "calibration", "reference"]),
            "parent splits missing"
        );
        for r in array(&population, "rows")? {
            groups.insert(str_at(r, "group")?.to_owned());
            ids.insert(str_at(r, "id")?.to_owned());
        }
        for (g, part) in c["split_assignments"]
            .as_object()
            .context("parent assignments")?
        {
            if part != "test" {
                groups.insert(g.clone());
            }
        }
        Ok(Self {
            manifest: m,
            contract: c,
            population,
            files,
            ids,
            groups,
            warm,
            mode,
            cfg_dimension: n - TEXT,
        })
    }
    pub(crate) fn input(&self, raw: &Value, cfg: Option<&Value>) -> Result<Value> {
        let values = vector(raw, TEXT)?;
        let mut x = if self.warm {
            raw.as_array().unwrap().clone() // raw bypass: no normalization or float32 reconstruction
        } else {
            let means = vector(&self.contract["embedding_normalization"]["mean"], TEXT)?;
            let scales = vector(&self.contract["embedding_normalization"]["scale"], TEXT)?;
            values
                .iter()
                .zip(means)
                .zip(scales)
                .map(|((v, m), s)| {
                    let value = json!((v - m) / s);
                    finite(&value)?;
                    Ok(value)
                })
                .collect::<Result<Vec<_>>>()?
        };
        x.extend(if self.mode == "text" {
            // Fixed by the text architecture regardless of parse availability.
            vec![json!(0.); self.cfg_dimension]
        } else {
            let cfg = cfg.context("combined input requires frozen CFG coordinates")?;
            vector(cfg, self.cfg_dimension)?;
            cfg.as_array().unwrap().clone()
        });
        Ok(json!({"contract_sha256":self.manifest["contract_sha256"],"x":x}))
    }
}

// Caller validation is deliberately separate so focused tests need no 16-model
// fixture. The only production entry below always calls the shared validator.
fn prepare_validated(
    selection: &Value,
    selection_bytes: &[u8],
    name: &str,
    record_path: &Path,
    dataset_path: &Path,
    cfg_dir: &Path,
    embedding_dir: &Path,
    out: &Path,
) -> Result<()> {
    ensure!(!out.try_exists()?, "output must be new");
    ensure!(
        selection["schema"] == "slop_ninja_balanced_a_selection_freeze_v3"
            && serde_json::from_slice::<Value>(selection_bytes)? == *selection,
        "selection snapshot differs"
    );
    let matches = array(selection, "candidates")?
        .iter()
        .filter(|c| c["candidate"] == name)
        .collect::<Vec<_>>();
    ensure!(matches.len() == 1, "candidate absent/ambiguous");
    let mut parent = Parent::load(matches[0])?;
    let (db, dataset_value) = read(dataset_path)?;
    ensure!(
        dataset_value["complete"] == true
            && dataset_value["test_text_opened"] == true
            && dataset_value["test_used_for_selection"] == false
            && dataset_value["selection_receipt_sha256"] == sha(selection_bytes),
        "unauthorized Test dataset"
    );
    let dataset = supervision::Dataset::parse_test(&db)?;
    let rb = fs::read(record_path)?;
    ensure!(dataset.records_sha256 == sha(&rb), "records digest differs");
    if dataset_value.get("test_records_sha256").is_some() {
        ensure!(
            dataset_value["test_records_sha256"] == sha(&rb),
            "Test records binding differs"
        );
    }
    let records = rows(&rb)?;
    dataset.require_exact_ids(records.keys().map(String::as_str))?;
    // Shared downstream contract: no lost generation/extraction denominator.
    let upstream = evaluation_binding::upstream_accounting(
        &dataset_value,
        &records.keys().cloned().collect(),
    )?;
    let profiles = supervision::Profiles::from_contract(&parent.contract)?;
    for (id, r) in &records {
        dataset.validate_metadata(id, r)?;
        ensure!(
            r["purpose"] == "query" && r["frozen_split"] == "test",
            "Test queries only; references are separate"
        );
        ensure!(
            !parent.ids.contains(id) && !parent.groups.contains(str_at(r, "source_group")?),
            "Test overlaps frozen fitting/gallery population"
        );
        ensure!(
            digest_at(r, "text_sha256")? == sha(str_at(r, "text")?.as_bytes()),
            "text hash differs"
        );
        dataset.supervision(r)?; // includes the existing exact realized-span checker
    }
    let cfg = inputs::cfg(cfg_dir, &parent, &db, &dataset, &records, &profiles)?;
    let embeddings = inputs::embeddings(embedding_dir, &parent.contract, &records)?;
    let (mut included, mut excluded, mut audit_rows, mut baseline) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let cfg_required = parent.mode == "combined";
    for (id, record) in &records {
        let e = &embeddings.rows[id];
        let cfg_available = cfg.rows.contains_key(id);
        let mut reasons = Vec::new();
        if cfg_required && !cfg_available {
            reasons.push(json!({"stage":"cfg","reason":cfg.population[id]["exclusion_reason"]}));
        }
        if e["status"] != "ok" {
            reasons.push(json!({"stage":"v6","status":e["status"],"reason":e["reason"],"token_count":e["token_count"]}));
        }
        if reasons.is_empty() {
            let row = if cfg_required {
                let mut row = cfg.rows[id].clone();
                row["input"] = parent.input(&e["embedding"], Some(&row["input"]["x"]))?;
                row
            } else {
                // Labels and masks come from validated production history and
                // frozen profiles, independently of parser success or output.
                let s = dataset.supervision(record)?;
                json!({"id":id,"source_group":record["source_group"],
                    "purpose":s.purpose,"frozen_split":record["frozen_split"],
                    "exposed_to_grammar":s.exposed,"roles":profiles.roles(&s),
                    "targets":profiles.targets(&s),"input":parent.input(&e["embedding"], None)?})
            };
            included.push(row);
        } else {
            excluded.push(json!({"id":id,"source_group":record["source_group"],"text_sha256":record["text_sha256"],
                "reason":"frozen_input_unavailable","reasons":reasons}));
        }
        let mut population_row = cfg.population[id].clone();
        population_row["cfg_diagnostic"] = json!({"available":cfg_available,
            "exclusion_reason":cfg.population[id]["exclusion_reason"],"required_for_model":cfg_required});
        population_row["available"] = json!(reasons.is_empty());
        population_row["status"] = json!(if reasons.is_empty() {
            "supported"
        } else {
            "excluded"
        });
        population_row["exclusion_reason"] = if reasons.is_empty() {
            Value::Null
        } else {
            json!("frozen_input_unavailable")
        };
        population_row["evaluation_exclusions"] = json!(reasons);
        audit_rows.push(population_row);
        // Includes explicit unsupported rows; no claim V6 supplies share/attribution.
        baseline.push(json!({"id":id,"text_sha256":record["text_sha256"],"status":e["status"],"reason":e["reason"],
            "artifact_id":e["artifact_id"],"logits":e["logits"],"probabilities":e["probabilities"],"token_count":e["token_count"]}));
    }
    let requested = records.len();
    let supported = included.len();
    let tb = jsonl(included.clone())?;
    let population = json!({"schema":"slop_ninja_frozen_text_evaluation_population_v1",
        "rows":audit_rows,"requested":requested,"available":supported,"excluded":excluded,
        "contract_sha256":parent.manifest["contract_sha256"],"frozen_manifest_sha256":matches[0]["numeric_manifest_sha256"],
        "dataset_manifest_sha256":sha(&db),"records_sha256":sha(&rb),"fit_performed":false,"final_test_opened":true});
    let audit_bytes = pretty(&population)?;
    let bindings = json!({"selection_receipt_sha256":sha(selection_bytes),"candidate":name,
        "frozen_manifest_sha256":matches[0]["numeric_manifest_sha256"],"dataset_manifest_sha256":sha(&db),
        "records_sha256":sha(&rb),"cfg":cfg.bindings,"embeddings":embeddings.bindings});
    let transform = json!({"schema":"slop_ninja_frozen_text_evaluation_transform_v1","bindings":bindings,
        "selection_receipt_sha256":sha(selection_bytes),
        "frozen_manifest_sha256":matches[0]["numeric_manifest_sha256"],"dataset_manifest_sha256":sha(&db),
        "audit_file":"test_population.json","audit_file_sha256":sha(&audit_bytes),"rows_sha256":sha(&tb),
        "requested":requested,"available":supported,"supported":supported,"excluded":excluded.len(),
        "upstream_requested_records":dataset_value["requested_records"],"cfg_supported":cfg.rows.len(),
        "cfg_unavailable":requested-cfg.rows.len(),"cfg_required_for_model":cfg_required,
        "cfg_v6_common_supported":cfg.rows.keys().filter(|id| embeddings.rows[*id]["status"] == "ok").count(),
        "upstream_accounting":upstream,
        "v6_supported":embeddings.rows.values().filter(|r| r["status"] == "ok").count(),
        "input_mode":parent.mode,"raw_text_bypass":parent.warm,"fit_performed":false,"normalization_fitted":false,
        "vocabulary_changed":false,"profiles_changed":false,"final_test_opened":true,
        "support_policy":if cfg_required {
            "intersection of explicit CFG and V6 success; every admitted requested ID accounted once, no truncation/imputation/resampling"
        } else {
            "V6 success only; CFG failures retained as diagnostics; zero CFG block is the frozen architecture constant, not imputation; every admitted requested ID accounted once, no truncation/resampling"
        },
        "upstream_accounting_note":"exact input dataset manifest retained separately, including extraction/generation failures not present in RECORDS"});
    let mut combined_population = parent.population.clone();
    let rr = combined_population["rows"]
        .as_array_mut()
        .context("parent population rows")?;
    rr.extend(audit_rows);
    let total = rr.len();
    let available = rr.iter().filter(|r| r["available"] == true).count();
    combined_population["requested"] = json!(total);
    combined_population["available"] = json!(available);
    combined_population["test_transform"] = transform.clone();
    combined_population["final_test_opened"] = json!(true);
    if combined_population["excluded"].is_array() {
        combined_population["excluded"]
            .as_array_mut()
            .unwrap()
            .extend(excluded);
    }
    let pb = pretty(&combined_population)?;
    parent.manifest["splits"].as_array_mut().unwrap().push(json!({"split":"test","rows":supported,
        "families":included.iter().map(|r|r["source_group"].as_str().unwrap()).collect::<BTreeSet<_>>().len(),"sha256":sha(&tb)}));
    parent.manifest["population_file_sha256"] = json!(sha(&pb));
    parent.manifest["final_test_opened"] = json!(true);
    parent.manifest["test_used_for_selection"] = json!(false);
    parent.manifest["test_transform"] = transform;
    parent.manifest["evaluation_only"] = json!(true);
    parent.manifest["evaluation_complete"] = json!(supported == requested);
    parent.manifest["source_sha256"] =
        json!(sha(include_bytes!("prepare_frozen_text_evaluation.rs")));
    parent.manifest["input_validator_sha256"] = json!(sha(include_bytes!(
        "prepare_frozen_text_evaluation/inputs.rs"
    )));
    parent.manifest["supervision_validator_sha256"] = json!(sha(include_bytes!(
        "prepare_supervised_forest/supervision.rs"
    )));
    parent.manifest["selection_validator_sha256"] =
        json!(sha(include_bytes!("final_test_binding/mod.rs")));
    parent.manifest["upstream_validator_sha256"] = json!(sha(include_bytes!(
        "compare_supervised_pangram/test_binding.rs"
    )));
    parent.files.extend(cfg.snapshots);
    parent.files.extend(embeddings.snapshots);
    parent.files.extend([
        ("test.jsonl".into(), tb),
        ("test_population.json".into(), audit_bytes),
        ("population.json".into(), pb),
        ("selection-receipt.json".into(), selection_bytes.to_vec()),
        ("test_dataset_manifest.json".into(), db),
        (
            "test-baseline-origin-reference.jsonl".into(),
            jsonl(baseline)?,
        ),
    ]);
    parent.manifest["evaluation_files_sha256"] = json!(
        parent
            .files
            .iter()
            .map(|(k, b)| (k, sha(b)))
            .collect::<BTreeMap<_, _>>()
    );
    let mb = pretty(&parent.manifest)?;
    fs::create_dir(out)?;
    for (name, bytes) in parent
        .files
        .into_iter()
        .chain(std::iter::once(("manifest.json".into(), mb)))
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(out.join(name))?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::File::open(out)?.sync_all()?;
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 7,
        "prepare_frozen_text_evaluation SELECTION CANDIDATE RECORDS MANIFEST CFG_BUNDLE EMBEDDINGS NEW_OUTPUT"
    );
    let path = Path::new(&a[0]);
    let selection = final_test_binding::validate_selection_receipt(path)?;
    let sb = fs::read(path)?;
    prepare_validated(
        &selection,
        &sb,
        &a[1],
        Path::new(&a[2]),
        Path::new(&a[3]),
        Path::new(&a[4]),
        Path::new(&a[5]),
        Path::new(&a[6]),
    )
}

// Reuse the identical CPU representation validator for separately authorized references.
pub(crate) fn reference_embeddings(
    dir: &Path, contract: &Value, records: &Rows,
) -> Result<(Rows, Value, Files)> {
    let e = inputs::embeddings(dir, contract, records)?;
    Ok((e.rows, e.bindings, e.snapshots))
}

//! Preserve exact raw V6 activations for a function-preserving residual encoder.
//! prepare_warm_start FROZEN_FUSION_BUNDLE EMBEDDINGS PROTOCOL NEW_OUT
//! prepare_warm_start --verify-parent-bindings FROZEN_FUSION_BUNDLE EMBEDDINGS NEW_RECEIPT
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
#[path = "prepare_warm_start/train_extension.rs"]
mod train_extension;
const DIM: usize = 768;
const ARTIFACT: &str = "506b1c66a6a2ffc4eb4e50b9b057657ce0e5b6f04cff8caf3df496c956dc0d5f";
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn hash(p: &Path) -> Result<String> {
    Ok(sha(&fs::read(p)?))
}
fn rows(b: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(b)?
        .lines()
        .map(|s| Ok(serde_json::from_str(s)?))
        .collect()
}
fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn numbers(v: &Value, len: usize) -> Result<Vec<f64>> {
    let a = v.as_array().context("numeric vector")?;
    ensure!(a.len() == len, "dimension mismatch");
    a.iter()
        .map(|v| {
            let x = v.as_f64().context("numeric coordinate")?;
            ensure!(
                x.is_finite() && (x as f32).is_finite(),
                "nonfinite coordinate"
            );
            Ok(x)
        })
        .collect()
}
fn put(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn jsonl(v: &[Value]) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    for r in v {
        serde_json::to_writer(&mut b, r)?;
        b.push(b'\n');
    }
    Ok(b)
}
struct EmbeddingEvidence {
    manifest: Value,
    rows: Vec<u8>,
    pins: Value,
}
impl EmbeddingEvidence {
    fn load(dir: &Path) -> Result<Self> {
        Self::from_bytes(
            &fs::read(dir.join("manifest.json"))?,
            fs::read(dir.join("rows.jsonl"))?,
            &fs::read(dir.join("contract/manifest.json"))?,
        )
    }
    fn from_bytes(manifest_bytes: &[u8], rows: Vec<u8>, snapshot: &[u8]) -> Result<Self> {
        let manifest: Value = serde_json::from_slice(manifest_bytes)?;
        let rows_sha = sha(&rows);
        ensure!(
            manifest["status"] == "complete"
                && manifest["unsupported"] == 0
                && manifest["rows_sha256"] == rows_sha,
            "incomplete/unsupported/changed embeddings"
        );
        let representation = &manifest["representation_contract"];
        ensure!(
            representation["artifact_id"] == ARTIFACT,
            "wrong frozen V6 runtime"
        );
        // Preserve the extractor's canonical bytes, including its float/escape
        // encoding; its representation digest excludes the final newline.
        let canonical = snapshot
            .strip_suffix(b"\n")
            .context("representation snapshot final newline")?;
        ensure!(
            manifest["representation_contract_sha256"] == sha(canonical)
                && serde_json::from_slice::<Value>(canonical)? == *representation,
            "changed representation snapshot or inline contract"
        );
        let pins = json!({
            "embedding_manifest_sha256":sha(manifest_bytes),
            "embedding_rows_sha256":rows_sha,
            "embedding_contract_file_sha256":sha(snapshot),
            "representation_contract_sha256":sha(canonical),
            "representation_contract_compact_rust_sha256":sha(&serde_json::to_vec(representation)?)
        });
        Ok(Self {
            manifest,
            rows,
            pins,
        })
    }
}

// Both preparation and read-only qualification must use the same checks and
// the same bytes that were read. A self-rehashed embedding manifest is not a
// substitute for the pins already frozen into each fusion parent.
fn bound_parent(
    dir: &Path,
    modality: &str,
    evidence: &EmbeddingEvidence,
) -> Result<(Value, Value, Value)> {
    let manifest_bytes = fs::read(dir.join("manifest.json"))?;
    let contract_bytes = fs::read(dir.join("contract.json"))?;
    check_parent_bindings(&manifest_bytes, &contract_bytes, modality, evidence)
}
fn check_parent_bindings(
    manifest_bytes: &[u8],
    contract_bytes: &[u8],
    modality: &str,
    evidence: &EmbeddingEvidence,
) -> Result<(Value, Value, Value)> {
    let manifest: Value = serde_json::from_slice(manifest_bytes)?;
    let contract: Value = serde_json::from_slice(contract_bytes)?;
    ensure!(
        manifest["final_test_opened"] == false
            && manifest["excluded"] == 0
            && manifest["contract_file_sha256"] == sha(contract_bytes)
            && manifest["contract_sha256"] == sha(&serde_json::to_vec(&contract)?),
        "invalid {modality} parent contract"
    );
    ensure!(contract["ablation"] == modality, "modality mismatch");
    for key in [
        "embedding_rows_sha256",
        "embedding_contract_file_sha256",
        "representation_contract_sha256",
        "representation_contract_compact_rust_sha256",
        "embedding_manifest_sha256",
    ] {
        ensure!(
            s(&contract["parents"], key)? == s(&evidence.pins, key)?,
            "{modality} frozen parent binding differs: {key}"
        );
    }
    let receipt = json!({
        "passed":true,
        "parent_manifest_sha256":sha(manifest_bytes),
        "parent_contract_file_sha256":sha(contract_bytes),
        "parent_contract_sha256":manifest["contract_sha256"],
        "embedding_bindings":evidence.pins
    });
    Ok((manifest, contract, receipt))
}
fn verify_parent_bindings(parent: &Path, embeddings: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new receipt required");
    let evidence = EmbeddingEvidence::load(embeddings)?;
    let mut parents = BTreeMap::new();
    for modality in ["text", "combined"] {
        let (_, _, receipt) = bound_parent(&parent.join(modality), modality, &evidence)?;
        parents.insert(modality, receipt);
    }
    put(
        out,
        &serde_json::to_vec_pretty(&json!({
            "schema":"slop_ninja_warm_start_parent_binding_verification_v1",
            "passed":true,
            "scope":"supplied embedding bytes and representation match both frozen fusion parent contracts; no numeric regeneration or inference",
            "implementation_sha256":sha(include_bytes!("prepare_warm_start.rs")),
            "parents":parents,
            "final_test_opened":false,
            "numeric_outputs_written":false,
            "normalization_fitted":false,
            "fitting_performed":false
        }))?,
    )
}
fn replace_text(x: &Value, raw: &Value, norm: &Value, dimension: usize) -> Result<Value> {
    let input = numbers(x, dimension)?;
    let h = numbers(raw, DIM)?;
    let mean = numbers(&norm["mean"], DIM)?;
    let scale = numbers(&norm["scale"], DIM)?;
    for i in 0..DIM {
        ensure!(scale[i] >= 0.01, "normalization scale");
        let standardized = (h[i] - mean[i]) / scale[i];
        ensure!(
            (standardized - input[i]).abs() < 1e-10,
            "raw embedding differs from frozen standardized input"
        );
    }
    let mut v = raw.as_array().unwrap().clone();
    v.extend(x.as_array().unwrap()[DIM..].iter().cloned());
    Ok(Value::Array(v))
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    if a.first().is_some_and(|s| s == "--extend-train") {
        ensure!(
            a.len() == 8,
            "prepare_warm_start --extend-train FROZEN_WARM_BUNDLE CFG_TRAIN_FRAGMENT EMBEDDINGS DATASET_MANIFEST GENERATION_PROTOCOL PROTOCOL.json NEW_OUT"
        );
        return train_extension::run(
            Path::new(&a[1]),
            Path::new(&a[2]),
            Path::new(&a[3]),
            Path::new(&a[4]),
            Path::new(&a[5]),
            Path::new(&a[6]),
            Path::new(&a[7]),
        );
    }
    ensure!(
        a.len() == 4,
        "prepare_warm_start FROZEN_FUSION_BUNDLE EMBEDDINGS PROTOCOL NEW_OUT\nprepare_warm_start --verify-parent-bindings FROZEN_FUSION_BUNDLE EMBEDDINGS NEW_RECEIPT"
    );
    if a[0] == "--verify-parent-bindings" {
        return verify_parent_bindings(Path::new(&a[1]), Path::new(&a[2]), Path::new(&a[3]));
    }
    let parent = Path::new(&a[0]);
    let embeddings = Path::new(&a[1]);
    let protocol = Path::new(&a[2]);
    let out = Path::new(&a[3]);
    ensure!(!out.exists(), "new output required");
    let evidence = EmbeddingEvidence::load(embeddings)?;
    let em = &evidence.manifest;
    let mut activations = BTreeMap::new();
    for r in rows(&evidence.rows)? {
        ensure!(
            r["status"] == "ok" && r["artifact_id"] == ARTIFACT,
            "embedding status/runtime"
        );
        numbers(&r["embedding"], DIM)?;
        ensure!(
            activations.insert(s(&r, "id")?.to_owned(), r).is_none(),
            "duplicate activation"
        );
    }
    let mut bundles = Vec::new();
    let mut baseline = None;
    for modality in ["text", "combined"] {
        let dir = parent.join(modality);
        let (mut manifest, mut contract, _) = bound_parent(&dir, modality, &evidence)?;
        let dimension = contract["input_dimension"].as_u64().context("dimension")? as usize;
        ensure!(dimension > DIM, "CFG block absent");
        let normalization = contract["embedding_normalization"].clone();
        ensure!(
            normalization["fit"] == "train_query_only_equal_source_family_weight"
                && normalization["dimension"] == DIM
                && normalization["development_used_for_fit"] == false
                && normalization["calibration_used_for_fit"] == false,
            "unfrozen normalization"
        );
        let population_bytes = fs::read(dir.join("population.json"))?;
        ensure!(
            manifest["population_file_sha256"] == sha(&population_bytes),
            "population changed"
        );
        let population: Value = serde_json::from_slice(&population_bytes)?;
        let mut hashes = BTreeMap::new();
        for row in population["rows"].as_array().context("population")? {
            hashes.insert(
                s(row, "id")?.to_owned(),
                s(&row["metadata"], "text_sha256")?.to_owned(),
            );
        }
        contract["warm_start_schema"] = json!("slop_ninja_raw_v6_cfg_residual_input_v1");
        contract["feature_policy_version"] = json!(2);
        contract["feature_policy"] =
            json!("raw_v6_classifier_input_plus_unchanged_train_frozen_cfg_v1");
        contract["layout"] = json!(format!(
            "{DIM} exact raw V6 penultimate activations followed by {} unchanged CFG coordinates",
            dimension - DIM
        ));
        contract["coordinate_blocks"][0]["meaning"] = json!(
            "exact raw V6 penultimate float32 values; frozen normalization applies only inside the residual adapter"
        );
        let coordinates = contract["input_coordinates"]
            .as_array_mut()
            .context("coordinates")?;
        ensure!(coordinates.len() == dimension, "coordinate count");
        for (i, v) in coordinates.iter_mut().take(DIM).enumerate() {
            *v = json!(format!("v6_penultimate_raw/{i}"));
        }
        let experiment = json!({"schema":"slop_ninja_v6_residual_warm_start_experiment_v1","protocol_sha256":hash(protocol)?,"shared_encoding":"h + residual_out(tanh(residual_in(concat(normalized_h,cfg))))","raw_text_dimension":DIM,"residual_hidden":64,"shared_dimension":DIM,"initial_origin":"copied frozen V6 classifier and historical temperature; residual_out weights/bias zero","seeds":[17,29,41],"epochs":250,"controls":["text","combined"],"weighting":"family_origin_balanced","final_test_opened":false});
        contract["experiment"] = experiment.clone();
        contract["parents"]["warm_start_fusion_contract_sha256"] =
            manifest["contract_sha256"].clone();
        contract["warm_start_bindings"] = json!({"parent_manifest_sha256":hash(&dir.join("manifest.json"))?,"embedding_rows_sha256":em["rows_sha256"],"protocol_sha256":hash(protocol)?,"implementation_sha256":sha(include_bytes!("prepare_warm_start.rs"))});
        let contract_hash = sha(&serde_json::to_vec(&contract)?);
        let contract_bytes = serde_json::to_vec_pretty(&contract)?;
        let mut files = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let mut reference = Vec::new();
        for part in ["train", "development", "calibration", "reference"] {
            let file = format!("{part}.jsonl");
            let raw = fs::read(dir.join(&file))?;
            let bindings = manifest["splits"].as_array_mut().context("splits")?;
            let binding = bindings
                .iter_mut()
                .find(|v| v["split"] == part)
                .context("split binding")?;
            ensure!(binding["sha256"] == sha(&raw), "split changed");
            let mut values = rows(&raw)?;
            for row in &mut values {
                let id = s(row, "id")?.to_owned();
                ensure!(seen.insert(id.clone()), "duplicate numeric ID");
                let r = activations.get(&id).context("missing activation")?;
                ensure!(
                    r["text_sha256"].as_str() == hashes.get(&id).map(String::as_str),
                    "activation text hash differs"
                );
                ensure!(
                    row["frozen_split"] == if part == "reference" { "train" } else { part },
                    "partition changed"
                );
                ensure!(
                    row["purpose"]
                        == if part == "reference" {
                            "reference"
                        } else {
                            "query"
                        },
                    "reference/query changed"
                );
                row["input"]["x"] = replace_text(
                    &row["input"]["x"],
                    &r["embedding"],
                    &normalization,
                    dimension,
                )?;
                if modality == "text" {
                    ensure!(
                        row["input"]["x"].as_array().unwrap()[DIM..]
                            .iter()
                            .all(|v| v.as_f64() == Some(0.)),
                        "text-only contains CFG"
                    );
                }
                row["input"]["contract_sha256"] = json!(contract_hash);
                reference.push(json!({"id":id,"text_sha256":r["text_sha256"],"logits":r["logits"],"probabilities":r["probabilities"],"artifact_id":r["artifact_id"]}));
            }
            let bytes = jsonl(&values)?;
            binding["sha256"] = json!(sha(&bytes));
            files.insert(file, bytes);
        }
        ensure!(
            seen == activations.keys().cloned().collect(),
            "requested population mismatch"
        );
        reference.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        let reference_bytes = jsonl(&reference)?;
        if let Some(ref old) = baseline {
            ensure!(*old == reference_bytes, "modality references differ");
        } else {
            baseline = Some(reference_bytes.clone());
        }
        manifest["contract_sha256"] = json!(contract_hash);
        manifest["contract_file_sha256"] = json!(sha(&contract_bytes));
        manifest["experiment"] = experiment;
        manifest["warm_start_schema"] = contract["warm_start_schema"].clone();
        manifest["warm_start_bindings"] = contract["warm_start_bindings"].clone();
        manifest["baseline_origin_reference_sha256"] = json!(sha(&reference_bytes));
        files.insert("contract.json".into(), contract_bytes);
        files.insert(
            "manifest.json".into(),
            serde_json::to_vec_pretty(&manifest)?,
        );
        files.insert("population.json".into(), population_bytes);
        files.insert("baseline-origin-reference.jsonl".into(), reference_bytes);
        bundles.push((modality, files));
    }
    fs::create_dir(out)?;
    for (modality, files) in bundles {
        let dir = out.join(modality);
        fs::create_dir(&dir)?;
        for (name, bytes) in files {
            put(&dir.join(name), &bytes)?;
        }
    }
    put(
        &out.join("manifest.json"),
        &serde_json::to_vec_pretty(
            &json!({"schema":"slop_ninja_warm_start_numeric_bundle_v1","requested":activations.len(),"embedding_rows_sha256":em["rows_sha256"],"protocol_sha256":hash(protocol)?,"final_test_opened":false,"normalization_fitted":false}),
        )?,
    )?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_rows() -> Value {
        json!({"id":"synthetic","embedding":[0.25],"logits":[1.,0.,0.],
            "probabilities":[0.6,0.2,0.2]})
    }
    fn synthetic_representation() -> Value {
        json!({"artifact_id":ARTIFACT,"hook":{"normalization":"none"}})
    }
    fn evidence_bytes(row: &Value, representation: &Value) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let rows = jsonl(&[row.clone()]).unwrap();
        let canonical = serde_json::to_vec(representation).unwrap();
        let manifest = serde_json::to_vec(&json!({
            "status":"complete","unsupported":0,"rows_sha256":sha(&rows),
            "representation_contract":representation,"representation_contract_sha256":sha(&canonical)
        })).unwrap();
        let mut snapshot = canonical;
        snapshot.push(b'\n');
        (manifest, rows, snapshot)
    }
    fn evidence(row: &Value, representation: &Value) -> EmbeddingEvidence {
        let (manifest, rows, snapshot) = evidence_bytes(row, representation);
        EmbeddingEvidence::from_bytes(&manifest, rows, &snapshot).unwrap()
    }
    fn parent_bytes(contract: &Value) -> (Vec<u8>, Vec<u8>) {
        let bytes = serde_json::to_vec_pretty(contract).unwrap();
        let manifest = serde_json::to_vec(&json!({
            "final_test_opened":false,"excluded":0,
            "contract_file_sha256":sha(&bytes),
            "contract_sha256":sha(&serde_json::to_vec(contract).unwrap())
        }))
        .unwrap();
        (manifest, bytes)
    }
    fn parent_contract(e: &EmbeddingEvidence, modality: &str) -> Value {
        json!({"ablation":modality,"parents":e.pins})
    }

    #[test]
    fn self_rehashed_predictions_cannot_replace_frozen_evidence() {
        let row = synthetic_rows();
        let representation = synthetic_representation();
        let frozen = evidence(&row, &representation);
        for modality in ["text", "combined"] {
            let (manifest, contract) = parent_bytes(&parent_contract(&frozen, modality));
            check_parent_bindings(&manifest, &contract, modality, &frozen).unwrap();
            for (field, changed) in [
                ("logits", json!([0., 1., 0.])),
                ("probabilities", json!([0.2, 0.6, 0.2])),
            ] {
                let mut altered = row.clone();
                altered[field] = changed;
                assert_eq!(altered["id"], row["id"]);
                assert_eq!(altered["embedding"], row["embedding"]);
                // Its own manifest checksum is valid, as in the reported gap.
                let supplied = evidence(&altered, &representation);
                let error = check_parent_bindings(&manifest, &contract, modality, &supplied)
                    .unwrap_err()
                    .to_string();
                assert!(error.contains("embedding_rows_sha256"), "{error}");
            }
        }
    }

    #[test]
    fn self_rehashed_representation_cannot_replace_frozen_evidence() {
        let row = synthetic_rows();
        let mut representation = synthetic_representation();
        let frozen = evidence(&row, &representation);
        representation["hook"]["normalization"] = json!("changed");
        let supplied = evidence(&row, &representation);
        assert_eq!(
            supplied.pins["embedding_rows_sha256"],
            frozen.pins["embedding_rows_sha256"]
        );
        for modality in ["text", "combined"] {
            let (manifest, contract) = parent_bytes(&parent_contract(&frozen, modality));
            let error = check_parent_bindings(&manifest, &contract, modality, &supplied)
                .unwrap_err()
                .to_string();
            assert!(error.contains("embedding_contract_file_sha256"), "{error}");
        }
    }

    #[test]
    fn every_frozen_pin_is_required_and_checked() {
        let e = evidence(&synthetic_rows(), &synthetic_representation());
        for modality in ["text", "combined"] {
            for key in e.pins.as_object().unwrap().keys() {
                for missing in [false, true] {
                    let mut parent = parent_contract(&e, modality);
                    if missing {
                        parent["parents"].as_object_mut().unwrap().remove(key);
                    } else {
                        parent["parents"][key] = json!("0".repeat(64));
                    }
                    // Even a self-consistent parent must have all matching pins.
                    let (manifest, contract) = parent_bytes(&parent);
                    let error = check_parent_bindings(&manifest, &contract, modality, &e)
                        .unwrap_err()
                        .to_string();
                    assert!(error.contains(key), "{error}");
                }
            }
        }
    }

    #[test]
    fn representation_snapshot_and_inline_contract_must_agree() {
        let (manifest, rows, mut snapshot) =
            evidence_bytes(&synthetic_rows(), &synthetic_representation());
        let mut altered: Value = serde_json::from_slice(&manifest).unwrap();
        altered["representation_contract"]["hook"]["normalization"] = json!("changed");
        assert!(
            EmbeddingEvidence::from_bytes(
                &serde_json::to_vec(&altered).unwrap(),
                rows.clone(),
                &snapshot
            )
            .is_err()
        );
        snapshot.pop();
        assert!(EmbeddingEvidence::from_bytes(&manifest, rows, &snapshot).is_err());
    }

    // Only the three embedding evidence files and the two parent manifests and
    // contracts exist. Verification must not require prose/numeric split files.
    fn fixture(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
        let (manifest, rows, snapshot) =
            evidence_bytes(&synthetic_rows(), &synthetic_representation());
        let e = EmbeddingEvidence::from_bytes(&manifest, rows.clone(), &snapshot).unwrap();
        let mut files = vec![
            (root.join("embeddings/manifest.json"), manifest),
            (root.join("embeddings/rows.jsonl"), rows),
            (root.join("embeddings/contract/manifest.json"), snapshot),
        ];
        for modality in ["text", "combined"] {
            let (manifest, contract) = parent_bytes(&parent_contract(&e, modality));
            let dir = root.join("fusion").join(modality);
            files.push((dir.join("manifest.json"), manifest));
            files.push((dir.join("contract.json"), contract));
        }
        for (path, bytes) in &files {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            put(path, bytes).unwrap();
        }
        files
    }

    #[test]
    fn verification_preserves_inputs_and_never_overwrites_receipt() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let files = fixture(root);
        let out = root.join("receipt.json");
        verify_parent_bindings(&root.join("fusion"), &root.join("embeddings"), &out).unwrap();
        let bytes = fs::read(&out).unwrap();
        let receipt: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(receipt["passed"], true);
        assert_eq!(receipt["parents"].as_object().unwrap().len(), 2);
        for modality in ["text", "combined"] {
            assert_eq!(receipt["parents"][modality]["passed"], true);
        }
        assert_eq!(receipt["numeric_outputs_written"], false);
        assert_eq!(receipt["final_test_opened"], false);
        for (path, expected) in files {
            assert_eq!(fs::read(path).unwrap(), expected);
        }
        assert!(
            verify_parent_bindings(&root.join("fusion"), &root.join("embeddings"), &out).is_err()
        );
        assert_eq!(fs::read(out).unwrap(), bytes);
    }

    #[test]
    fn verification_rejects_changed_second_parent_without_receipt() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fixture(root);
        let dir = root.join("fusion/combined");
        let mut contract: Value =
            serde_json::from_slice(&fs::read(dir.join("contract.json")).unwrap()).unwrap();
        contract["parents"]["embedding_rows_sha256"] = json!("0".repeat(64));
        let (manifest, contract) = parent_bytes(&contract);
        fs::write(dir.join("manifest.json"), manifest).unwrap();
        fs::write(dir.join("contract.json"), contract).unwrap();
        let out = root.join("receipt.json");
        let error = verify_parent_bindings(&root.join("fusion"), &root.join("embeddings"), &out)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("combined") && error.contains("embedding_rows_sha256"),
            "{error}"
        );
        assert!(!out.exists());
    }

    #[test]
    fn raw_activations_are_exact_and_changed_parents_fail() {
        let norm = json!({"mean":vec![0.2;DIM],"scale":vec![0.5;DIM]});
        let h = json!(vec![0.7; DIM]);
        let mut x = vec![(0.7 - 0.2) / 0.5; DIM];
        x.push(3.);
        let result = replace_text(&json!(x), &h, &norm, DIM + 1).unwrap();
        assert_eq!(result[0], 0.7);
        assert_eq!(result[DIM], 3.);
        x[0] = 0.;
        assert!(replace_text(&json!(x), &h, &norm, DIM + 1).is_err());
    }
}

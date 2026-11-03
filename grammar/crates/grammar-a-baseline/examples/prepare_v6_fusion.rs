//! Bounded, exposed-development V6/CFG preparation; never opens Test data.
//!
//! prepare_v6_fusion requests CFG_NUMERIC RECORDS DATA_MANIFEST NEW_OUT
//! prepare_v6_fusion join CFG_NUMERIC PREPARED_DIR EMBEDDINGS_DIR NEW_OUT
//!
//! Requests contain only id/text. Roster split means frozen_split (references
//! have split=train, purpose=reference, and live in reference.jsonl). Outputs
//! must be new directories below Git-ignored data/grammar-private. No fitting,
//! inference, provider calls, or source paths from manifests are executed here.
//! Cohort size, split counts and CFG dimension come from the digest-bound numeric
//! manifest/contract. V6 identity and its 768 activation coordinates stay fixed.
#![recursion_limit = "256"]

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

const PARTS: [&str; 4] = ["train", "development", "calibration", "reference"];
const PILOT_COUNTS: [usize; 4] = [229, 106, 107, 8];
const TEXT_DIM: usize = 768;
const PILOT_CFG_DIM: usize = 2062;
const PREP_SCHEMA: &str = "slop_ninja_v6_fusion_requests_v1";
const V6_PARENT: &str = "4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138";
const V6_PARENT_MANIFEST: &str = "1f4700f2a1855ca9cec5d0b9fbbe830a2a07588a4e95a77a9a754bd6cc3de73f";
const V6_COMPATIBLE: &str = "506b1c66a6a2ffc4eb4e50b9b057657ce0e5b6f04cff8caf3df496c956dc0d5f";

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    let s = v[key]
        .as_str()
        .with_context(|| format!("missing string {key}"))?;
    ensure!(!s.is_empty(), "empty {key}");
    Ok(s)
}
fn digest<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    let s = string(v, key)?;
    ensure!(
        s.len() == 64
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "invalid SHA256 {key}"
    );
    Ok(s)
}
fn verify(bytes: &[u8], v: &Value, key: &str) -> Result<()> {
    ensure!(sha(bytes) == digest(v, key)?, "binding mismatch: {key}");
    Ok(())
}
fn read_json(path: &Path) -> Result<(Vec<u8>, Value)> {
    let bytes = fs::read(path).context("cannot read bound JSON file")?;
    let v = serde_json::from_slice(&bytes).context("invalid bound JSON file")?;
    Ok((bytes, v))
}
fn rows(bytes: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(bytes)?
        .lines()
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line).with_context(|| format!("invalid JSONL row {}", i + 1))
        })
        .collect()
}
fn index(rows: &[Value]) -> Result<BTreeMap<String, &Value>> {
    let mut result = BTreeMap::new();
    for row in rows {
        ensure!(
            result.insert(string(row, "id")?.to_owned(), row).is_none(),
            "duplicate ID"
        );
    }
    Ok(result)
}
fn id_set(v: &Value, key: &str) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for id in v[key].as_array().context("missing ID allow-list")? {
        let id = id.as_str().context("allow-list must contain IDs only")?;
        ensure!(
            !id.is_empty() && ids.insert(id.to_owned()),
            "empty or duplicate allow-list ID"
        );
    }
    Ok(ids)
}
fn jsonl(rows: impl IntoIterator<Item = Value>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, &row)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}
fn new_private_output(out: &Path) -> Result<PathBuf> {
    ensure!(!out.try_exists()?, "output must be new");
    let parent = out
        .parent()
        .context("output parent required")?
        .canonicalize()?;
    let candidate = parent.join(out.file_name().context("output name required")?);
    let components = candidate
        .components()
        .map(|c| c.as_os_str())
        .collect::<Vec<_>>();
    ensure!(
        components
            .windows(2)
            .any(|c| c[0] == "data" && c[1] == "grammar-private"),
        "output must be below data/grammar-private"
    );
    ensure!(
        Command::new("git")
            .arg("-C")
            .arg(&parent)
            .args(["check-ignore", "--quiet", "--"])
            .arg(&candidate)
            .status()?
            .success(),
        "output must be Git ignored"
    );
    Ok(candidate)
}
fn finite_vector(v: &Value, dimension: usize) -> Result<Vec<f64>> {
    let a = v.as_array().context("missing numeric vector")?;
    ensure!(a.len() == dimension, "numeric dimension mismatch");
    a.iter()
        .map(|v| {
            let n = v.as_f64().context("nonnumeric coordinate")?;
            ensure!(
                n.is_finite() && (n as f32).is_finite(),
                "nonfinite trainer coordinate"
            );
            Ok(n)
        })
        .collect()
}

#[derive(Debug)]
struct Cfg {
    requested: usize,
    dimension: usize,
    contract: Value,
    manifest: Value,
    manifest_bytes: Vec<u8>,
    population: Value,
    parts: BTreeMap<String, Vec<Value>>,
    roster: BTreeMap<String, Value>,
    binding: Value,
}

impl Cfg {
    fn pilot_shape(&self) -> bool {
        self.dimension == PILOT_CFG_DIM
            && PARTS
                .iter()
                .zip(PILOT_COUNTS)
                .all(|(part, count)| self.parts[*part].len() == count)
    }
}

fn count(v: &Value, key: &str) -> Result<usize> {
    usize::try_from(
        v[key]
            .as_u64()
            .with_context(|| format!("invalid count: {key}"))?,
    )
    .with_context(|| format!("count exceeds platform size: {key}"))
}

fn fusion_dimension(cfg_dimension: usize) -> Result<usize> {
    ensure!(cfg_dimension > 0, "empty CFG coordinates");
    TEXT_DIM
        .checked_add(cfg_dimension)
        .context("fusion dimension overflow")
}

fn roster_row(row: &Value, text_sha256: &str) -> Value {
    json!({"id":row["id"],"text_sha256":text_sha256,"split":row["frozen_split"],
        "source_group":row["source_group"],"purpose":row["purpose"]})
}

fn load_cfg(root: &Path) -> Result<Cfg> {
    let (manifest_bytes, manifest) = read_json(&root.join("manifest.json"))?;
    ensure!(
        manifest["schema"] == "slop_ninja_shared_a_dataset_v1",
        "CFG dataset schema"
    );
    ensure!(
        count(&manifest, "requested")? > 0
            && manifest["available"] == manifest["requested"]
            && manifest["excluded"] == 0,
        "requires every manifest-requested CFG row, without exclusions"
    );
    let requested = count(&manifest, "requested")?;
    ensure!(
        manifest["final_test_opened"] == false,
        "requires non-Test CFG parent"
    );
    let (contract_bytes, contract) = read_json(&root.join("contract.json"))?;
    verify(&contract_bytes, &manifest, "contract_file_sha256")?;
    verify(
        &serde_json::to_vec(&contract)?,
        &manifest,
        "contract_sha256",
    )?;
    let dimension = count(&contract, "input_dimension")?;
    fusion_dimension(dimension)?;
    ensure!(
        manifest["feature_dimension"] == dimension,
        "CFG manifest/contract dimension mismatch"
    );
    ensure!(
        contract["labels_not_inputs"] == true && contract["metadata_not_inputs"] == true,
        "CFG feature isolation required"
    );
    ensure!(
        contract["final_test_opened"] == false,
        "CFG contract used Test"
    );
    for role in ["human", "ai"] {
        id_set(&contract, &format!("{role}_profiles"))?;
    }
    let (population_bytes, population) = read_json(&root.join("population.json"))?;
    verify(&population_bytes, &manifest, "population_file_sha256")?;
    ensure!(
        population["requested"] == requested
            && population["available"] == requested
            && population["excluded"].as_array().is_some_and(Vec::is_empty),
        "CFG population excluded rows"
    );
    let pop = index(
        population["rows"]
            .as_array()
            .context("CFG population rows")?,
    )?;
    ensure!(pop.len() == requested, "CFG population roster size");
    let bindings = manifest["splits"]
        .as_array()
        .context("CFG split bindings")?;
    let mut declared = BTreeSet::new();
    for b in bindings {
        let part = string(b, "split")?;
        ensure!(
            PARTS.contains(&part) && declared.insert(part),
            "unexpected/duplicate CFG split; Test forbidden"
        );
    }
    ensure!(declared.len() == PARTS.len(), "missing CFG split");
    let mut parts = BTreeMap::new();
    let mut roster = BTreeMap::new();
    let mut families = BTreeMap::new();
    for part in PARTS {
        let b = bindings
            .iter()
            .find(|b| b["split"] == part)
            .context("missing split")?;
        // Fixed allow-list of filenames: never follow a manifest path or Test.
        let bytes = fs::read(root.join(format!("{part}.jsonl")))?;
        verify(&bytes, b, "sha256")?;
        let subset = rows(&bytes)?;
        ensure!(
            subset.len() == count(b, "rows")?,
            "manifest-bound split count mismatch"
        );
        for row in &subset {
            let id = string(row, "id")?;
            let group = string(row, "source_group")?;
            let split = string(row, "frozen_split")?;
            let purpose = string(row, "purpose")?;
            ensure!(
                if part == "reference" {
                    split == "train" && purpose == "reference"
                } else {
                    split == part && purpose == "query"
                },
                "CFG split/purpose mismatch"
            );
            if let Some(old) = families.insert(group.to_owned(), part) {
                ensure!(
                    old == part,
                    "source family crosses query/reference partitions"
                );
            }
            let p = pop.get(id).context("CFG row absent from population")?;
            ensure!(
                p["available"] == true
                    && p["group"] == group
                    && p["split"] == split
                    && p["purpose"] == purpose,
                "CFG population metadata mismatch"
            );
            let metadata = &p["metadata"];
            for key in ["id", "source_group", "frozen_split", "purpose"] {
                ensure!(
                    metadata[key] == row[key],
                    "CFG source metadata mismatch: {key}"
                );
            }
            ensure!(
                contract["split_assignments"][group] == split,
                "CFG frozen family assignment mismatch"
            );
            let source_hash = digest(p, "source_sha256")?;
            ensure!(
                digest(metadata, "text_sha256")? == source_hash,
                "CFG source text binding mismatch"
            );
            let input = row["input"].as_object().context("CFG input")?;
            ensure!(
                input.len() == 2
                    && input.contains_key("x")
                    && row["input"]["contract_sha256"] == manifest["contract_sha256"],
                "CFG input contract mismatch"
            );
            finite_vector(&row["input"]["x"], dimension)?;
            ensure!(
                row["targets"].is_object() && row["roles"].is_object(),
                "missing original targets/roles"
            );
            ensure!(
                roster
                    .insert(id.to_owned(), roster_row(row, source_hash))
                    .is_none(),
                "duplicate CFG ID"
            );
        }
        parts.insert(part.to_owned(), subset);
    }
    ensure!(
        roster.len() == requested && roster.keys().eq(pop.keys()),
        "CFG population ID set mismatch"
    );
    ensure!(!parts["train"].is_empty(), "no Train query rows");
    let binding = json!({"manifest_sha256":sha(&manifest_bytes),"contract_file_sha256":sha(&contract_bytes),
        "contract_sha256":manifest["contract_sha256"],"population_file_sha256":sha(&population_bytes),
        "splits":bindings,"records_sha256":contract["records_sha256"],
        "dataset_manifest_sha256":contract["dataset_manifest_sha256"]});
    Ok(Cfg {
        requested,
        dimension,
        contract,
        manifest,
        manifest_bytes,
        population,
        parts,
        roster,
        binding,
    })
}

// Before materializing source prose, reject reserved/unknown IDs and partitions.
#[derive(Deserialize)]
struct Header {
    id: String,
    source_group: String,
    frozen_split: String,
    purpose: String,
}

fn requests(cfg_root: &Path, records: &Path, dataset_path: &Path, out: &Path) -> Result<()> {
    let out = new_private_output(out)?;
    let cfg = load_cfg(cfg_root)?;
    let (dataset_bytes, dataset) = read_json(dataset_path)?;
    ensure!(
        dataset["schema"] == "slop_ninja_a_supervised_dataset_v1",
        "source dataset schema"
    );
    verify(&dataset_bytes, &cfg.contract, "dataset_manifest_sha256")?;
    verify(&dataset_bytes, &cfg.manifest, "dataset_manifest_sha256")?;
    let allowed = id_set(&dataset, "non_test_ids")?;
    let reserved = id_set(&dataset, "test_ids")?; // IDs only; never Test records.
    ensure!(
        allowed.len() == cfg.requested
            && allowed.is_disjoint(&reserved)
            && allowed.iter().eq(cfg.roster.keys()),
        "exact manifest-bound non-Test ID allow-list required"
    );
    let bytes = fs::read(records)?;
    verify(&bytes, &dataset, "records_sha256")?;
    verify(&bytes, &cfg.contract, "records_sha256")?;
    let mut seen = BTreeSet::new();
    for line in std::str::from_utf8(&bytes)?.lines() {
        let h: Header = serde_json::from_str(line).context("invalid source header")?;
        ensure!(
            allowed.contains(&h.id) && !reserved.contains(&h.id) && seen.insert(h.id.clone()),
            "duplicate or unauthorized source ID"
        );
        let r = &cfg.roster[&h.id];
        ensure!(
            r["source_group"] == h.source_group
                && r["split"] == h.frozen_split
                && r["purpose"] == h.purpose,
            "source header differs from CFG"
        );
        ensure!(
            dataset["split_assignments"][&h.source_group] == h.frozen_split,
            "source frozen family assignment mismatch"
        );
    }
    ensure!(seen == allowed, "source records omit requested IDs");
    let source = rows(&bytes)?;
    let source = index(&source)?;
    let pop = index(
        cfg.population["rows"]
            .as_array()
            .context("population rows")?,
    )?;
    let mut request_rows = Vec::new();
    for (id, roster) in &cfg.roster {
        let row = source[id];
        let text = string(row, "text")?;
        verify(text.as_bytes(), row, "text_sha256")?;
        ensure!(
            row["text_sha256"] == roster["text_sha256"],
            "source text differs from CFG"
        );
        verify(&serde_json::to_vec(row)?, pop[id], "metadata_sha256")?;
        ensure!(
            row["rights"]["commercial_training"] == true,
            "requires admitted public/generated cohort"
        );
        request_rows.push(json!({"id":id,"text":text}));
    }
    let request_bytes = jsonl(request_rows)?;
    let roster_bytes = jsonl(cfg.roster.values().cloned())?;
    let manifest = json!({"schema":PREP_SCHEMA,"requested":cfg.requested,
        "requests_sha256":sha(&request_bytes),"roster_sha256":sha(&roster_bytes),
        "records_sha256":sha(&bytes),"dataset_manifest_sha256":sha(&dataset_bytes),
        "cfg_parent":cfg.binding,"preparation_implementation_sha256":sha(include_bytes!("prepare_v6_fusion.rs")),
        "source_files":{"records":records.canonicalize()?,"dataset_manifest":dataset_path.canonicalize()?,"cfg_numeric":cfg_root.canonicalize()?},
        "request_fields":["id","text"],"request_contains_labels":false,
        "roster_split":"original frozen_split; reference purpose has train split",
        "coverage_policy":if cfg.pilot_shape() {"all450_exact_nonTest_CFG_supported;all450_embeddings_required;no_silent_drop_or_truncation"}
            else {"exact_manifest_bound_nonTest_CFG_roster;all_requested_embeddings_required;no_silent_drop_or_truncation"},
        "input_exclusion_policy":"no new exclusions; any unsupported embedding fails join before output",
        "evidence":"public_source_or_generated_exposed_development_diagnostic",
        "clean_holdout":false,"final_test_opened":false,"test_used_for_selection":false});
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    fs::create_dir(&out)?;
    write_new(&out.join("requests.jsonl"), &request_bytes)?;
    write_new(&out.join("roster.jsonl"), &roster_bytes)?;
    write_new(&out.join("manifest.json"), &manifest_bytes)?;
    println!(
        "{}",
        json!({"mode":"requests","requested":cfg.requested,"requests_sha256":sha(&request_bytes),"out":out})
    );
    Ok(())
}

fn match_requests(
    requests: &[Value],
    roster: &[Value],
    expected: &BTreeMap<String, Value>,
) -> Result<()> {
    let request_map = index(requests)?;
    let roster_map = index(roster)?;
    ensure!(
        request_map.keys().eq(expected.keys()) && roster_map.keys().eq(expected.keys()),
        "request/roster/CFG ID sets differ"
    );
    for (id, expected_row) in expected {
        let request = request_map[id];
        let object = request.as_object().context("request object")?;
        ensure!(
            object.len() == 2 && object.contains_key("id") && object.contains_key("text"),
            "request must contain only id and text"
        );
        ensure!(
            *roster_map[id] == *expected_row,
            "roster/CFG binding mismatch"
        );
        verify(
            string(request, "text")?.as_bytes(),
            expected_row,
            "text_sha256",
        )?;
    }
    Ok(())
}

type Embeddings = BTreeMap<String, Vec<f64>>;

fn validate_representation(
    manifest: &Value,
    snapshot_bytes: &[u8],
    requested: usize,
) -> Result<()> {
    ensure!(
        manifest["schema"] == "slop_ninja_v6_embeddings_run_v1" && manifest["status"] == "complete",
        "extractor must finish with a complete run manifest"
    );
    ensure!(
        manifest["processed"] == requested
            && manifest["requested"] == requested
            && manifest["unsupported"] == 0,
        "extractor incomplete or unsupported inputs; bounded fit stopped"
    );
    let representation = &manifest["representation_contract"];
    // The extractor's canonical Python bytes plus one newline are retained in
    // its contract snapshot. Hash those bytes, avoiding cross-language float or
    // ASCII-escape reserialization assumptions.
    let canonical = snapshot_bytes
        .strip_suffix(b"\n")
        .context("representation snapshot final newline")?;
    verify(canonical, manifest, "representation_contract_sha256")?;
    ensure!(
        serde_json::from_slice::<Value>(canonical)? == *representation,
        "inline/snapshot representation mismatch"
    );
    ensure!(
        representation["schema"] == "slop_ninja_v6_penultimate_contract_v1"
            && representation["dimension"] == TEXT_DIM
            && representation["embedding_dtype"] == "float32",
        "representation schema/dimension/dtype mismatch"
    );
    ensure!(
        representation["parent_artifact_id"] == V6_PARENT
            && representation["parent_manifest_sha256"] == V6_PARENT_MANIFEST,
        "representation is not from frozen V6 parent"
    );
    digest(representation, "artifact_id")?;
    ensure!(
        representation["model"]["architecture"] == "ModernBertForSequenceClassification"
            && representation["model"]["frozen"] == true
            && representation["model"]["training"] == false,
        "frozen evaluation model required"
    );
    ensure!(
        representation["hook"]["module"] == "model.classifier"
            && representation["hook"]["api"] == "register_forward_pre_hook"
            && representation["hook"]["argument"] == 0
            && representation["hook"]["shape"] == json!([1, TEXT_DIM])
            && representation["hook"]["normalization"] == "none"
            && representation["hook"]["projection"] == "none",
        "penultimate activation hook differs"
    );
    ensure!(
        representation["input"]["keys"] == json!(["id", "text"])
            && representation["input"]["learned_inputs"] == json!(["text"])
            && representation["input"]["truncation"] == false
            && representation["input"]["max_tokens_including_special_tokens"] == 2048,
        "exact text-only nontruncating extraction required"
    );
    let runtime = &representation["runtime"];
    let expected_artifact = if runtime["profile"] == "cpu_float32_eager_parent_v1" {
        V6_PARENT
    } else {
        V6_COMPATIBLE
    };
    ensure!(
        representation["artifact_id"] == expected_artifact,
        "embedding artifact differs from pinned CPU profile"
    );
    ensure!(
        matches!(
            string(runtime, "profile")?,
            "cpu_float32_eager_parent_v1" | "cpu_float32_eager_mkl_compatible_aten_default_v1"
        ) && runtime["device"] == "cpu"
            && runtime["attention"] == "eager"
            && runtime["autocast"] == false
            && runtime["compile"] == false
            && runtime["batch_size"] == 1
            && runtime["threads"] == 1
            && runtime["interop_threads"] == 1,
        "unqualified embedding runtime profile"
    );
    ensure!(
        runtime["packages"] == runtime["reference_runtime"]["packages"],
        "runtime package pins differ from reference"
    );
    for key in [
        "extractor_sha256",
        "model_class_source_sha256",
        "classifier_class_source_sha256",
    ] {
        digest(&representation["source"], key)?;
    }
    let files = &representation["artifact_files_sha256"];
    ensure!(
        files.as_object().is_some_and(|o| !o.is_empty()),
        "missing frozen model file bindings"
    );
    for key in files.as_object().context("model file hashes")?.keys() {
        digest(files, key)?;
    }
    for key in [
        "classifier/model.safetensors",
        "classifier/config.json",
        "classifier/tokenizer.json",
        "runner/common.py",
        "runner/requirements.lock",
    ] {
        digest(files, key)?;
    }
    let evidence = &manifest["runtime_evidence"];
    ensure!(
        evidence["original_dependency_and_reference_checks_passed"] == true,
        "missing original runtime qualification"
    );
    let replay = evidence["hook_reference_replay"]
        .as_array()
        .context("missing hook replay")?;
    ensure!(
        !replay.is_empty()
            && replay.iter().all(
                |r| r["hook_preserves_logits_and_probabilities_exactly"] == true
                    && r["reference_probability_max_absolute_error"]
                        .as_f64()
                        .is_some_and(|v| v.is_finite() && (0.0..=1e-6).contains(&v))
            ),
        "hook reference qualification failed"
    );
    Ok(())
}

fn validate_embeddings(
    raw: &[Value],
    expected: &BTreeMap<String, Value>,
    manifest: &Value,
) -> Result<Embeddings> {
    let indexed = index(raw)?;
    ensure!(
        indexed.keys().eq(expected.keys()),
        "embedding/request ID sets differ"
    );
    ensure!(
        manifest["dimension"] == TEXT_DIM && manifest["requested"] == expected.len(),
        "embedding dimension/requested count mismatch"
    );
    let mut output = BTreeMap::new();
    let mut unsupported = 0;
    for (id, roster) in expected {
        let row = indexed[id];
        ensure!(
            digest(row, "text_sha256")? == digest(roster, "text_sha256")?,
            "embedding text hash differs from request/CFG"
        );
        // Both explicit success spellings are allowed. A non-success status is
        // never rescued by the presence of a numeric embedding.
        let status = string(row, "status")?;
        if !matches!(status, "ok" | "supported") {
            ensure!(
                row["embedding"].is_null(),
                "failed embedding row contains coordinates"
            );
            unsupported += 1;
            continue;
        }
        ensure!(
            row["token_count"].as_u64().is_some_and(|n| n > 0),
            "supported embedding needs token count"
        );
        ensure!(
            row.get("truncated").is_none_or(|v| v == false),
            "truncated embedding is unsupported"
        );
        ensure!(
            row["token_count"].as_u64().is_some_and(|n| n <= 2048),
            "over-context embedding is unsupported"
        );
        let vector = finite_vector(&row["embedding"], TEXT_DIM)?;
        let mut float_bytes = Vec::with_capacity(TEXT_DIM * 4);
        for v in &vector {
            ensure!((*v as f32) as f64 == *v, "embedding is not exact float32");
            float_bytes.extend_from_slice(&(*v as f32).to_le_bytes());
        }
        verify(&float_bytes, row, "embedding_sha256")?;
        output.insert(id.to_owned(), vector);
    }
    ensure!(
        manifest["supported"] == output.len(),
        "embedding supported count mismatch"
    );
    ensure!(
        unsupported == 0 && output.len() == expected.len(),
        "bounded fit requires every requested embedding: {unsupported} unsupported; explicit population decision required, no output written"
    );
    Ok(output)
}

struct Normalization {
    mean: Vec<f64>,
    scale: Vec<f64>,
    fit_rows: usize,
    fit_families: usize,
    fit_roster_sha256: String,
}

impl Normalization {
    fn transform(&self, values: &[f64]) -> Result<Vec<f64>> {
        ensure!(
            values.len() == self.mean.len() && self.mean.len() == self.scale.len(),
            "normalization dimension mismatch"
        );
        values
            .iter()
            .zip(&self.mean)
            .zip(&self.scale)
            .map(|((v, m), s)| {
                let n = (v - m) / s;
                ensure!(
                    n.is_finite() && (n as f32).is_finite(),
                    "nonfinite normalized embedding"
                );
                Ok(n)
            })
            .collect()
    }

    fn contract(&self) -> Value {
        json!({"schema":"slop_ninja_v6_fusion_embedding_normalization_v1",
            "dimension":self.mean.len(),"transform":"(embedding[i]-mean[i])/scale[i]",
            "fit":"train_query_only_equal_source_family_weight",
            "weight":"1 / (Train_query_source_families * Train_query_rows_in_this_source_family)",
            "variance":"weighted_population_sum(weight * (x-mean)^2); two passes; no Bessel correction",
            "scale_rule":"max(sqrt(weighted_population_variance),0.01)","scale_floor":0.01,
            "mean":self.mean,"scale":self.scale,
            "fit_rows":self.fit_rows,"fit_families":self.fit_families,
            "fit_roster_sha256":self.fit_roster_sha256,
            "fit_roster_encoding":"compact sorted-key JSONL id/source_group, sorted by id, final newline",
            "reference_used_for_fit":false,"development_used_for_fit":false,"calibration_used_for_fit":false,
            "labels_used_for_fit":false,"final_test_opened":false})
    }
}

fn fit_normalization(
    rows: &[Value],
    embeddings: &Embeddings,
    dimension: usize,
) -> Result<Normalization> {
    let mut eligible = rows
        .iter()
        .filter(|r| r["frozen_split"] == "train" && r["purpose"] == "query")
        .collect::<Vec<_>>();
    eligible.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    ensure!(
        !eligible.is_empty() && dimension > 0,
        "no Train query embeddings"
    );
    let mut groups = BTreeMap::<String, usize>::new();
    let mut seen = BTreeSet::new();
    for row in &eligible {
        ensure!(seen.insert(string(row, "id")?), "duplicate Train query ID");
        *groups
            .entry(string(row, "source_group")?.into())
            .or_default() += 1;
    }
    let mut mean = vec![0.; dimension];
    let mut variance = vec![0.; dimension];
    let mut fit_roster = Vec::new();
    // Stable order and two passes; evaluation/reference values never enter even
    // dimension validation or arithmetic for the fitted normalization.
    for row in &eligible {
        let id = string(row, "id")?;
        let group = string(row, "source_group")?;
        let x = embeddings
            .get(id)
            .context("missing Train query embedding")?;
        ensure!(
            x.len() == dimension && x.iter().all(|v| v.is_finite()),
            "invalid Train embedding"
        );
        let weight = 1. / (groups.len() as f64 * groups[group] as f64);
        for (m, x) in mean.iter_mut().zip(x) {
            *m += weight * x;
        }
        fit_roster.push(json!({"id":id,"source_group":group}));
    }
    for row in &eligible {
        let id = string(row, "id")?;
        let group = string(row, "source_group")?;
        let weight = 1. / (groups.len() as f64 * groups[group] as f64);
        for ((v, x), m) in variance.iter_mut().zip(&embeddings[id]).zip(&mean) {
            *v += weight * (x - m).powi(2);
        }
    }
    ensure!(
        mean.iter().chain(&variance).all(|n| n.is_finite()),
        "nonfinite fitted normalization"
    );
    let scale = variance.iter().map(|v| v.sqrt().max(0.01)).collect();
    Ok(Normalization {
        mean,
        scale,
        fit_rows: eligible.len(),
        fit_families: groups.len(),
        fit_roster_sha256: sha(&jsonl(fit_roster)?),
    })
}

fn ablated_input(text: &[f64], cfg: &Value, ablation: &str, cfg_dimension: usize) -> Result<Value> {
    ensure!(text.len() == TEXT_DIM, "text dimension mismatch");
    finite_vector(cfg, cfg_dimension)?;
    ensure!(
        matches!(ablation, "text" | "combined" | "cfg"),
        "unknown fusion ablation"
    );
    let mut x = Vec::with_capacity(fusion_dimension(cfg_dimension)?);
    for &v in text {
        ensure!(
            v.is_finite() && (v as f32).is_finite(),
            "invalid text coordinate"
        );
        x.push(json!(if ablation == "cfg" { 0. } else { v }));
    }
    if ablation == "text" {
        x.extend((0..cfg_dimension).map(|_| json!(0.)));
    } else {
        // Clone original JSON numbers: no re-normalization, rescaling, feature
        // selection, or float32 conversion of the frozen CFG coordinates.
        x.extend(cfg.as_array().context("CFG vector")?.iter().cloned());
    }
    Ok(Value::Array(x))
}

fn implementations() -> Result<Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut bindings = BTreeMap::new();
    for relative in [
        "crates/grammar-eval/python/shared_a.py",
        "crates/grammar-a-baseline/examples/evaluate_supervised.rs",
        "crates/grammar-a-baseline/examples/compare_supervised_pangram.rs",
        "crates/grammar-a-baseline/Cargo.toml",
        "Cargo.toml",
        "Cargo.lock",
    ] {
        bindings.insert(relative, sha(&fs::read(root.join(relative))?));
    }
    Ok(
        json!({"join_compiled_source_sha256":sha(include_bytes!("prepare_v6_fusion.rs")),
        "files_sha256":bindings,"policy":"join source at build time; trainer/evaluators/manifests at preparation time; extractor implementation and runtime in bound embedding manifest"}),
    )
}

fn experiment(dimension: usize) -> Value {
    json!({"schema":"slop_ninja_v6_fusion_bounded_experiment_v1",
        "seeds":[17,29,41],"controls":{"text":["joint","origin_only","attribution_only"],
            "combined":["joint","origin_only","attribution_only"],"cfg":["joint"]},
        "trainer":"grammar/crates/grammar-eval/python/shared_a.py",
        "architecture":{"dimension":dimension,"hidden":64,"embedding":32,"activation":"tanh",
            "heads":["origin","mixed_share","human","ai"]},
        "epochs":250,"optimizer":"AdamW","learning_rate":0.001,"weight_decay":0.01,
        "seed_policy":"same seed and identical initial architecture/head sizes across modalities and controls; report every seed",
        "selection":"Development involvement Brier, except attribution_only uses existing trainer conditional-role objective",
        "calibration":"Calibration only; existing trainer temperature and conditional named thresholds",
        "masks_and_losses":"unchanged shared_a.py; null targets masked; all original roles retained",
        "reference_policy":"encode after fitting with --export-encodings; never fit references",
        "architecture_matching":"identical total inputs and parameters; zero padding changes active inputs/parameters",
        "embedding_availability_coordinate":false,"test_used_for_selection":false,
        "clean_holdout":false,"final_test_opened":false})
}

fn join(cfg_root: &Path, prepared: &Path, embeddings_root: &Path, out: &Path) -> Result<()> {
    let out = new_private_output(out)?;
    let cfg = load_cfg(cfg_root)?;
    let dimension = fusion_dimension(cfg.dimension)?;
    let (prep_bytes, prep) = read_json(&prepared.join("manifest.json"))?;
    ensure!(
        prep["schema"] == PREP_SCHEMA && prep["requested"] == cfg.requested,
        "prepared manifest schema/size"
    );
    ensure!(
        prep["cfg_parent"] == cfg.binding,
        "prepared CFG parent changed"
    );
    ensure!(
        prep["records_sha256"] == cfg.contract["records_sha256"]
            && prep["dataset_manifest_sha256"] == cfg.contract["dataset_manifest_sha256"],
        "prepared source binding mismatch"
    );
    ensure!(
        prep["final_test_opened"] == false && prep["request_contains_labels"] == false,
        "prepared request policy mismatch"
    );
    // Requests may have been prepared by an earlier build while join was added.
    // Retain that hash as evidence; do not compare it with this build's source.
    digest(&prep, "preparation_implementation_sha256")?;
    let request_bytes = fs::read(prepared.join("requests.jsonl"))?;
    let roster_bytes = fs::read(prepared.join("roster.jsonl"))?;
    verify(&request_bytes, &prep, "requests_sha256")?;
    verify(&roster_bytes, &prep, "roster_sha256")?;
    match_requests(&rows(&request_bytes)?, &rows(&roster_bytes)?, &cfg.roster)?;

    let (embedding_manifest_bytes, embedding_manifest) =
        read_json(&embeddings_root.join("manifest.json"))?;
    verify(&request_bytes, &embedding_manifest, "requests_sha256")?;
    let embedding_bytes = fs::read(embeddings_root.join("rows.jsonl"))?;
    verify(&embedding_bytes, &embedding_manifest, "rows_sha256")?;
    let (representation_bytes, _) = read_json(&embeddings_root.join("contract/manifest.json"))?;
    validate_representation(&embedding_manifest, &representation_bytes, cfg.requested)?;
    let representation = &embedding_manifest["representation_contract"];
    let representation_sha = sha(&serde_json::to_vec(representation)?);
    let extraction_requests = fs::read(embeddings_root.join("requests.jsonl"))?;
    verify(&extraction_requests, &embedding_manifest, "requests_sha256")?;
    ensure!(
        extraction_requests == request_bytes,
        "extractor request snapshot differs"
    );
    let embedding_rows = rows(&embedding_bytes)?;
    let embeddings = validate_embeddings(&embedding_rows, &cfg.roster, &embedding_manifest)?;
    let raw_lines = request_bytes
        .split_inclusive(|b| *b == b'\n')
        .collect::<Vec<_>>();
    let requests = rows(&request_bytes)?;
    let request_bindings = requests
        .iter()
        .zip(raw_lines)
        .enumerate()
        .map(|(i, (r, line))| Ok((string(r, "id")?, (i + 1, sha(line)))))
        .collect::<Result<BTreeMap<_, _>>>()?;
    for row in &embedding_rows {
        let (position, line_hash) = &request_bindings[string(row, "id")?];
        ensure!(
            row["request_index"] == *position && row["request_sha256"] == *line_hash,
            "embedding request-line binding mismatch"
        );
        ensure!(
            row["representation_contract_sha256"]
                == embedding_manifest["representation_contract_sha256"]
                && row["artifact_id"] == representation["artifact_id"],
            "mixed embedding model/representation"
        );
    }
    let all_rows = PARTS
        .iter()
        .flat_map(|part| cfg.parts[*part].iter().cloned())
        .collect::<Vec<_>>();
    let norm = fit_normalization(&all_rows, &embeddings, TEXT_DIM)?;
    ensure!(
        norm.fit_rows == cfg.parts["train"].len(),
        "normalization Train query roster mismatch"
    );
    let normalization = norm.contract();
    let normalization_bytes = serde_json::to_vec_pretty(&normalization)?;
    let normalized = embeddings
        .iter()
        .map(|(id, v)| Ok((id.clone(), norm.transform(v)?)))
        .collect::<Result<Embeddings>>()?;
    let impls = implementations()?;
    let plan = experiment(dimension);
    let parents = json!({"cfg":cfg.binding,"prepared_manifest_sha256":sha(&prep_bytes),
        "requests_sha256":sha(&request_bytes),"roster_sha256":sha(&roster_bytes),
        "embedding_manifest_sha256":sha(&embedding_manifest_bytes),"embedding_rows_sha256":sha(&embedding_bytes),
        "embedding_contract_file_sha256":sha(&representation_bytes),
        "representation_contract_sha256":embedding_manifest["representation_contract_sha256"],
        "representation_contract_compact_rust_sha256":representation_sha,
        "preparation_implementation_sha256":prep["preparation_implementation_sha256"]});
    let coverage = json!({"requested":cfg.requested,"cfg_supported":cfg.requested,"embedding_supported":cfg.requested,
        "included":cfg.requested,"excluded":[],"reference_rows":cfg.parts["reference"].len(),
        "policy":if cfg.pilot_shape() {"exact existing450 roster; all modalities required; no input exclusions or truncation; any unsupported result fails before output"}
            else {"exact manifest-bound non-Test roster; all modalities required; no input exclusions or truncation; any unsupported result fails before output"},
        "request_policy":"id and exact text only; no labels, splits, source groups or purpose sent to extractor",
        "embedding_availability_coordinate":false,"metadata_not_inputs":true,
        "evidence":"public_source_or_generated_exposed_development_diagnostic",
        "historical_v6_exposure":"not a new clean holdout; use separately bound exposure audit for strata; pretraining exposure unknown",
        "test_text_read":false,"final_test_opened":false,"test_used_for_selection":false,"clean_holdout":false});
    let mut population = cfg.population.clone();
    population["cfg_parent_population_file_sha256"] = cfg.binding["population_file_sha256"].clone();
    population["coverage"] = coverage.clone();
    population["parents"] = parents.clone();
    population["clean_holdout"] = json!(false);
    population["final_test_opened"] = json!(false);
    let population_bytes = serde_json::to_vec_pretty(&population)?;
    let coordinates = (0..TEXT_DIM)
        .map(|i| format!("v6_penultimate_standardized/{i}"))
        .chain((0..cfg.dimension).map(|i| format!("frozen_cfg/x/{i}")))
        .collect::<Vec<_>>();
    let mut artifacts = BTreeMap::<String, Vec<u8>>::new();
    let mut dataset_bindings = Vec::new();
    for ablation in ["text", "combined", "cfg"] {
        let contract = json!({"schema":"slop_ninja_shared_a_numeric_contract_v1",
            "fusion_schema":"slop_ninja_v6_fusion_numeric_contract_v1",
            "feature_policy":if cfg.pilot_shape() {"slop_ninja_v6_fusion_existing450_v1"} else {"slop_ninja_v6_fusion_manifest_cohort_v1"},"feature_policy_version":1,
            "evidence":"frozen_V6_penultimate_text_representation_and_original_native_CFG;public_exposed_development_only",
            "ablation":ablation,"input_dimension":dimension,"input_coordinates":coordinates,
            "layout":format!("768 standardized text activations followed by {} frozen original CFG x coordinates", cfg.dimension),
            "coordinate_blocks":[{"start_inclusive":0,"end_exclusive":TEXT_DIM,"meaning":"V6 representation_contract activation index i; frozen Train-query normalization"},
                {"start_inclusive":TEXT_DIM,"end_exclusive":dimension,"meaning":"cfg_parent_contract original input.x[i-768], including original centered masks; unchanged numeric values"}],
            "ablation_meanings":{"text":"text transformed; every CFG position exact zero",
                "combined":"text transformed; every original CFG coordinate unchanged",
                "cfg":"every text position exact zero; every original CFG coordinate unchanged"},
            "embedding_availability_coordinate":false,
            "representation_contract":representation,"representation_contract_compact_rust_sha256":representation_sha,
            "embedding_normalization":normalization,"embedding_normalization_file_sha256":sha(&normalization_bytes),
            "cfg_parent_contract":cfg.contract,"cfg_parent_contract_sha256":cfg.manifest["contract_sha256"],
            "cfg_parent_contract_file_sha256":cfg.manifest["contract_file_sha256"],
            "cfg_transform_policy":"no CFG refit, reselection, truncation, reordering, scaling or conversion; original input values cloned",
            "parents":parents,"implementations":impls,"experiment":plan,"coverage":coverage,
            "dataset_manifest_sha256":cfg.contract["dataset_manifest_sha256"],"records_sha256":cfg.contract["records_sha256"],
            "human_profiles":cfg.contract["human_profiles"],"ai_profiles":cfg.contract["ai_profiles"],
            "origin_order":cfg.contract["origin_order"],"split_assignments":cfg.contract["split_assignments"],
            "profile_policy":"exact frozen CFG profiles; no enrollment/refit",
            "target_policy":"copy entire numeric row unchanged except input; all targets, roles, masks and metadata retained",
            "reference_policy":"original reference rows included under reference.jsonl; never in Train normalization or losses",
            "feature_leak_guards":{"allowed":"frozen text activation and original numeric CFG only",
                "forbidden":"labels, metadata, IDs, source groups, splits, purpose, profiles, role presence, target masks, token counts, hashes, availability flags added by fusion"},
            "labels_not_inputs":true,"metadata_not_inputs":true,"clean_holdout":false,"final_test_opened":false,
            "contract_hash_policy":"SHA256 of serde_json compact Value serialization; BTree/sorted object keys; separate pretty-file hash",
            "limitations":["cached embeddings do not qualify Rust/serverless encoder inference or hide extraction cost",
                "exposed diagnostic cannot establish parity or clean holdout performance",
                "equal architecture and total parameters do not imply equal active inputs or parameters"]});
        let contract_hash = sha(&serde_json::to_vec(&contract)?);
        let contract_bytes = serde_json::to_vec_pretty(&contract)?;
        let mut splits = Vec::new();
        for part in PARTS {
            let mut transformed = Vec::new();
            for row in &cfg.parts[part] {
                let mut output = row.clone();
                let id = string(row, "id")?;
                output["input"] = json!({"contract_sha256":contract_hash,
                    "x":ablated_input(&normalized[id], &row["input"]["x"], ablation, cfg.dimension)?});
                transformed.push(output);
            }
            let bytes = jsonl(transformed)?;
            splits.push(json!({"split":part,"rows":cfg.parts[part].len(),"sha256":sha(&bytes),
                "families":cfg.parts[part].iter().map(|r| string(r,"source_group")).collect::<Result<BTreeSet<_>>>()?.len()}));
            artifacts.insert(format!("{ablation}/{part}.jsonl"), bytes);
        }
        let manifest = json!({"schema":"slop_ninja_shared_a_dataset_v1",
            "fusion_schema":"slop_ninja_v6_fusion_dataset_v1","contract_sha256":contract_hash,
            "contract_file_sha256":sha(&contract_bytes),"population_file_sha256":sha(&population_bytes),
            "splits":splits,"requested":cfg.requested,"available":cfg.requested,"excluded":0,
            "feature_dimension":dimension,"reference_rows":cfg.parts["reference"].len(),"ablation":ablation,
            "dataset_manifest_sha256":cfg.contract["dataset_manifest_sha256"],
            "parents":parents,"implementations":impls,"coverage":coverage,"experiment":plan,
            "implementation_sha256":sha(include_bytes!("prepare_v6_fusion.rs")),
            "clean_holdout":false,"final_test_opened":false,"test_used_for_selection":false});
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        dataset_bindings.push(
            json!({"ablation":ablation,"manifest_sha256":sha(&manifest_bytes),
            "contract_sha256":contract_hash,"feature_dimension":dimension}),
        );
        artifacts.insert(format!("{ablation}/contract.json"), contract_bytes);
        artifacts.insert(
            format!("{ablation}/population.json"),
            population_bytes.clone(),
        );
        artifacts.insert(format!("{ablation}/manifest.json"), manifest_bytes);
    }
    artifacts.insert("embedding-normalization.json".into(), normalization_bytes);
    artifacts.insert("prepared-manifest.json".into(), prep_bytes);
    artifacts.insert("embedding-manifest.json".into(), embedding_manifest_bytes);
    artifacts.insert("embedding-contract.json".into(), representation_bytes);
    artifacts.insert("cfg-manifest.json".into(), cfg.manifest_bytes);
    let file_hashes = artifacts
        .iter()
        .map(|(name, bytes)| (name, sha(bytes)))
        .collect::<BTreeMap<_, _>>();
    let manifest = json!({"schema":"slop_ninja_v6_fusion_bundle_v1","parents":parents,
        "files_sha256":file_hashes,"datasets":dataset_bindings,"coverage":coverage,"experiment":plan,
        "implementations":impls,"clean_holdout":false,"final_test_opened":false,
        "input_paths":{"cfg_numeric":cfg_root.canonicalize()?,"prepared":prepared.canonicalize()?,"embeddings":embeddings_root.canonicalize()?}});
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    // Validate and serialize the complete bundle before creating any output.
    // Child manifests are published only after data; bundle manifest is last.
    fs::create_dir(&out)?;
    for ablation in ["text", "combined", "cfg"] {
        fs::create_dir(out.join(ablation))?;
    }
    for (name, bytes) in artifacts
        .iter()
        .filter(|(name, _)| !name.ends_with("/manifest.json"))
    {
        write_new(&out.join(name), bytes)?;
    }
    for (name, bytes) in artifacts
        .iter()
        .filter(|(name, _)| name.ends_with("/manifest.json"))
    {
        write_new(&out.join(name), bytes)?;
    }
    write_new(&out.join("manifest.json"), &manifest_bytes)?;
    println!(
        "{}",
        json!({"mode":"join","requested":cfg.requested,"supported":cfg.requested,
        "feature_dimension":dimension,"normalization_train_queries":norm.fit_rows,"out":out})
    );
    Ok(())
}

fn main() -> Result<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(
        args.len() == 5,
        "prepare_v6_fusion requests CFG_NUMERIC RECORDS DATA_MANIFEST NEW_OUT\nprepare_v6_fusion join CFG_NUMERIC PREPARED_DIR EMBEDDINGS_DIR NEW_OUT"
    );
    match args[0].to_str() {
        Some("requests") => requests(
            Path::new(&args[1]),
            Path::new(&args[2]),
            Path::new(&args[3]),
            Path::new(&args[4]),
        ),
        Some("join") => join(
            Path::new(&args[1]),
            Path::new(&args[2]),
            Path::new(&args[3]),
            Path::new(&args[4]),
        ),
        _ => bail!("expected requests or join"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct SyntheticCfg {
        contract: Value,
        population: Value,
        parts: BTreeMap<&'static str, Vec<Value>>,
        dimension: usize,
    }

    impl SyntheticCfg {
        fn new(counts: [usize; 4], dimension: usize) -> Self {
            let mut parts = BTreeMap::new();
            let mut population = Vec::new();
            let mut assignments = BTreeMap::new();
            for (part, count) in PARTS.into_iter().zip(counts) {
                let mut rows = Vec::new();
                for i in 0..count {
                    let id = format!("synthetic-{part}-{i}");
                    let split = if part == "reference" { "train" } else { part };
                    let purpose = if part == "reference" {
                        "reference"
                    } else {
                        "query"
                    };
                    let metadata = json!({"id":id,"source_group":id,"frozen_split":split,
                        "purpose":purpose,"text_sha256":sha(id.as_bytes())});
                    assignments.insert(id.clone(), split);
                    population.push(json!({"id":id,"group":id,"split":split,"purpose":purpose,
                        "available":true,"source_sha256":sha(id.as_bytes()),"metadata":metadata,
                        "metadata_sha256":sha(&serde_json::to_vec(&metadata).unwrap())}));
                    rows.push(json!({"id":id,"source_group":id,"frozen_split":split,"purpose":purpose,
                        "targets":{},"roles":{},"input":{"x":vec![0.;dimension],"contract_sha256":"pending"}}));
                }
                parts.insert(part, rows);
            }
            let requested: usize = counts.iter().sum();
            Self {
                contract: json!({"input_dimension":dimension,"labels_not_inputs":true,"metadata_not_inputs":true,
                    "final_test_opened":false,"human_profiles":[],"ai_profiles":[],"split_assignments":assignments,
                    "records_sha256":sha(b"synthetic records"),"dataset_manifest_sha256":sha(b"synthetic dataset")}),
                population: json!({"requested":requested,"available":requested,"excluded":[],"rows":population}),
                parts,
                dimension,
            }
        }

        fn save(&mut self, root: &Path) {
            let contract_hash = sha(&serde_json::to_vec(&self.contract).unwrap());
            let contract_bytes = serde_json::to_vec_pretty(&self.contract).unwrap();
            let population_bytes = serde_json::to_vec_pretty(&self.population).unwrap();
            let mut bindings = Vec::new();
            for (part, rows) in &mut self.parts {
                for row in rows.iter_mut() {
                    row["input"]["contract_sha256"] = json!(contract_hash);
                }
                let bytes = jsonl(rows.iter().cloned()).unwrap();
                bindings.push(json!({"split":part,"rows":rows.len(),"sha256":sha(&bytes)}));
                fs::write(root.join(format!("{part}.jsonl")), bytes).unwrap();
            }
            let manifest = json!({"schema":"slop_ninja_shared_a_dataset_v1",
                "requested":self.population["requested"],"available":self.population["available"],"excluded":0,
                "feature_dimension":self.dimension,"final_test_opened":false,
                "contract_sha256":contract_hash,"contract_file_sha256":sha(&contract_bytes),
                "population_file_sha256":sha(&population_bytes),"splits":bindings});
            fs::write(root.join("contract.json"), contract_bytes).unwrap();
            fs::write(root.join("population.json"), population_bytes).unwrap();
            fs::write(
                root.join("manifest.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
        }
    }

    #[test]
    fn manifest_bound_counts_dimensions_and_normalization_accept_new_cohorts() {
        for (counts, dimension) in [([2, 1, 2, 1], 3), ([4, 2, 1, 0], 11)] {
            let dir = TempDir::new().unwrap();
            SyntheticCfg::new(counts, dimension).save(dir.path());
            let cfg = load_cfg(dir.path()).unwrap();
            assert_eq!(cfg.dimension, dimension);
            assert_eq!(cfg.requested, counts.iter().sum::<usize>());
            assert!(!cfg.pilot_shape());
            let total = fusion_dimension(dimension).unwrap();
            assert_eq!(total, TEXT_DIM + dimension);
            assert_eq!(experiment(total)["architecture"]["dimension"], total);
            let rows = cfg.parts.values().flatten().cloned().collect::<Vec<_>>();
            let embeddings = rows
                .iter()
                .map(|r| {
                    let id = r["id"].as_str().unwrap().to_owned();
                    // Only Train query embeddings are present: normalization must
                    // not even attempt lookup of reference/Dev/Cal values.
                    (id, vec![2.; TEXT_DIM])
                })
                .filter(|(id, _)| id.starts_with("synthetic-train-"))
                .collect::<Embeddings>();
            let norm = fit_normalization(&rows, &embeddings, TEXT_DIM).unwrap();
            assert_eq!(norm.fit_rows, counts[0]);
            assert_eq!(norm.mean, vec![2.; TEXT_DIM]);
            assert_eq!(norm.scale, vec![0.01; TEXT_DIM]);
        }
    }

    #[test]
    fn original_pilot_shape_remains_recognized() {
        let dir = TempDir::new().unwrap();
        SyntheticCfg::new(PILOT_COUNTS, PILOT_CFG_DIM).save(dir.path());
        let cfg = load_cfg(dir.path()).unwrap();
        assert!(cfg.pilot_shape());
        assert_eq!(cfg.requested, 450);
        assert_eq!(fusion_dimension(cfg.dimension).unwrap(), 2830);
    }

    #[test]
    fn bound_numeric_cohorts_reject_test_reference_and_cross_split_corruption() {
        let dir = TempDir::new().unwrap();
        let baseline = || SyntheticCfg::new([2, 1, 1, 1], 3);
        let mut fixture = baseline();
        fixture.parts.get_mut("development").unwrap()[0]["source_group"] =
            json!("synthetic-train-0");
        fixture.save(dir.path());
        assert!(
            load_cfg(dir.path())
                .unwrap_err()
                .to_string()
                .contains("family crosses")
        );
        for split in ["development", "calibration", "test"] {
            let mut fixture = baseline();
            fixture.parts.get_mut("reference").unwrap()[0]["frozen_split"] = json!(split);
            fixture.save(dir.path());
            assert!(
                load_cfg(dir.path())
                    .unwrap_err()
                    .to_string()
                    .contains("split/purpose")
            );
        }
        let mut fixture = baseline();
        fixture.parts.insert("test", Vec::new());
        fixture.save(dir.path());
        assert!(
            load_cfg(dir.path())
                .unwrap_err()
                .to_string()
                .contains("Test forbidden")
        );
        let mut fixture = baseline();
        fixture.parts.get_mut("calibration").unwrap()[0]["id"] = json!("synthetic-train-0");
        fixture.save(dir.path());
        assert!(load_cfg(dir.path()).is_err());
    }

    #[test]
    fn bound_numeric_cohorts_reject_count_dimension_digest_and_population_mismatches() {
        let dir = TempDir::new().unwrap();
        for case in 0..6 {
            let mut fixture = SyntheticCfg::new([2, 1, 1, 1], 3);
            match case {
                0 => fixture.dimension = 4,
                1 => fixture.population["available"] = json!(4),
                2 => fixture.population["rows"]
                    .as_array_mut()
                    .unwrap()
                    .pop()
                    .map(|_| ())
                    .unwrap(),
                3 => fixture.parts.get_mut("train").unwrap()[0]["input"]["x"] = json!([1., 2.]),
                _ => (),
            }
            fixture.save(dir.path());
            if case == 4 {
                let path = dir.path().join("manifest.json");
                let (_, mut manifest) = read_json(&path).unwrap();
                manifest["splits"][0]["rows"] = json!(123);
                fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            } else if case == 5 {
                fs::write(dir.path().join("train.jsonl"), b"changed bytes\n").unwrap();
            }
            assert!(load_cfg(dir.path()).is_err(), "case {case}");
        }
        assert!(fusion_dimension(0).is_err());
        assert!(fusion_dimension(usize::MAX).is_err());
    }

    fn row(id: &str, group: &str, split: &str, purpose: &str) -> Value {
        json!({"id":id,"source_group":group,"frozen_split":split,"purpose":purpose})
    }

    fn normalization_fixture() -> (Vec<Value>, Embeddings) {
        let rows = vec![
            row("a1", "a", "train", "query"),
            row("a2", "a", "train", "query"),
            row("b1", "b", "train", "query"),
            row("dev", "dev", "development", "query"),
            row("cal", "cal", "calibration", "query"),
            row("ref", "ref", "train", "reference"),
        ];
        let embeddings = BTreeMap::from([
            ("a1".into(), vec![0., 7.]),
            ("a2".into(), vec![2., 7.]),
            ("b1".into(), vec![10., 7.]),
            ("dev".into(), vec![1000., 0.]),
            ("cal".into(), vec![-1000., 0.]),
            ("ref".into(), vec![1e9, 0.]),
        ]);
        (rows, embeddings)
    }

    #[test]
    fn normalization_uses_train_queries_and_equal_family_population_variance() {
        let (rows, embeddings) = normalization_fixture();
        let n = fit_normalization(&rows, &embeddings, 2).unwrap();
        // Family a has weight 1/2 despite containing two rows; b has 1/2.
        assert_eq!(n.mean, vec![5.5, 7.]);
        assert!((n.scale[0] - 20.75f64.sqrt()).abs() < 1e-12);
        assert_eq!(n.scale[1], 0.01);
        assert_eq!(n.fit_rows, 3);
        assert_eq!(n.fit_families, 2);
        let mut changed = embeddings;
        changed.insert("dev".into(), vec![f64::NAN]);
        changed.insert("cal".into(), vec![f64::INFINITY]);
        let again = fit_normalization(&rows, &changed, 2).unwrap();
        assert_eq!(n.contract(), again.contract());
        assert_eq!(n.transform(&[5.5, 7.]).unwrap(), vec![0., 0.]);
    }

    #[test]
    fn references_never_fit_even_when_their_frozen_split_is_train() {
        let (rows, mut embeddings) = normalization_fixture();
        let baseline = fit_normalization(&rows, &embeddings, 2).unwrap();
        embeddings.remove("ref"); // Fitting must not even access its values.
        let mut changed = rows.clone();
        changed.push(row("another-ref", "a", "train", "reference"));
        let again = fit_normalization(&changed, &embeddings, 2).unwrap();
        assert_eq!(baseline.contract(), again.contract());
        assert!(
            fit_normalization(&[row("ref", "ref", "train", "reference")], &embeddings, 2).is_err()
        );
        // Encoding a reference uses the frozen transform, after fitting.
        assert_eq!(baseline.transform(&[5.5, 7.01]).unwrap()[0], 0.);
    }

    #[test]
    fn zero_padding_preserves_dimensions_order_and_every_original_cfg_number() {
        for cfg_dimension in [3, PILOT_CFG_DIM] {
            let text = (0..TEXT_DIM).map(|i| i as f64 - 300.).collect::<Vec<_>>();
            let cfg = json!(
                (0..cfg_dimension)
                    .map(|i| i as f64 / 7. - 99.)
                    .collect::<Vec<_>>()
            );
            let both = ablated_input(&text, &cfg, "combined", cfg_dimension).unwrap();
            let text_only = ablated_input(&text, &cfg, "text", cfg_dimension).unwrap();
            let cfg_only = ablated_input(&text, &cfg, "cfg", cfg_dimension).unwrap();
            for v in [&both, &text_only, &cfg_only] {
                assert_eq!(v.as_array().unwrap().len(), TEXT_DIM + cfg_dimension);
            }
            assert_eq!(
                &both.as_array().unwrap()[TEXT_DIM..],
                cfg.as_array().unwrap()
            );
            assert_eq!(
                &cfg_only.as_array().unwrap()[TEXT_DIM..],
                cfg.as_array().unwrap()
            );
            assert_eq!(
                &both.as_array().unwrap()[..TEXT_DIM],
                &text_only.as_array().unwrap()[..TEXT_DIM]
            );
            assert!(
                text_only.as_array().unwrap()[TEXT_DIM..]
                    .iter()
                    .all(|v| v.as_f64() == Some(0.))
            );
            assert!(
                cfg_only.as_array().unwrap()[..TEXT_DIM]
                    .iter()
                    .all(|v| v.as_f64() == Some(0.))
            );
            assert!(ablated_input(&text[..2], &cfg, "combined", cfg_dimension).is_err());
            assert!(ablated_input(&text, &cfg, "combined", cfg_dimension + 1).is_err());
        }
    }

    fn request_fixture() -> (Vec<Value>, Vec<Value>, BTreeMap<String, Value>) {
        let request = json!({"id":"synthetic","text":"A synthetic example."});
        let roster = json!({"id":"synthetic","text_sha256":sha(b"A synthetic example."),
            "source_group":"synthetic-family","split":"train","purpose":"query"});
        (
            vec![request],
            vec![roster.clone()],
            BTreeMap::from([("synthetic".into(), roster)]),
        )
    }

    #[test]
    fn bindings_reject_changed_text_ids_roster_hashes_and_request_labels() {
        let (requests, roster, expected) = request_fixture();
        match_requests(&requests, &roster, &expected).unwrap();
        let mut changed = requests.clone();
        changed[0]["text"] = json!("An altered synthetic example.");
        assert!(match_requests(&changed, &roster, &expected).is_err());
        changed = requests.clone();
        changed[0]["origin"] = json!("human_only");
        assert!(match_requests(&changed, &roster, &expected).is_err());
        changed = requests.clone();
        changed[0]["id"] = json!("different");
        assert!(match_requests(&changed, &roster, &expected).is_err());
        for key in ["source_group", "split", "purpose", "text_sha256"] {
            let mut changed = roster.clone();
            changed[0][key] = json!("changed");
            assert!(match_requests(&requests, &changed, &expected).is_err());
        }
        let bytes = jsonl(requests.clone()).unwrap();
        let binding = json!({"requests_sha256":sha(&bytes)});
        verify(&bytes, &binding, "requests_sha256").unwrap();
        let mut corrupted = bytes;
        corrupted.push(b' ');
        assert!(verify(&corrupted, &binding, "requests_sha256").is_err());
        assert!(
            match_requests(
                &[requests[0].clone(), requests[0].clone()],
                &roster,
                &expected
            )
            .is_err()
        );
    }

    #[test]
    fn embeddings_require_exact_full_coverage_and_finite_bound_float32_values() {
        let (_, _, expected) = request_fixture();
        let bytes = vec![0u8; TEXT_DIM * 4];
        let row = json!({"id":"synthetic","text_sha256":sha(b"A synthetic example."),
            "status":"ok","token_count":7,"embedding":vec![0.;TEXT_DIM],"embedding_sha256":sha(&bytes)});
        let manifest = json!({"dimension":TEXT_DIM,"requested":1,"supported":1});
        validate_embeddings(&[row.clone()], &expected, &manifest).unwrap();
        let mut changed = row.clone();
        changed["status"] = json!("unsupported_input");
        changed["embedding"] = Value::Null;
        let none_supported = json!({"dimension":TEXT_DIM,"requested":1,"supported":0});
        assert!(validate_embeddings(&[changed], &expected, &none_supported).is_err());
        assert!(validate_embeddings(&[], &expected, &manifest).is_err());
        let mut changed = row.clone();
        changed["embedding"][0] = json!(1.);
        assert!(validate_embeddings(&[changed], &expected, &manifest).is_err());
        let mut changed = row.clone();
        changed["token_count"] = json!(2049);
        assert!(validate_embeddings(&[changed], &expected, &manifest).is_err());
        let mut changed = row;
        changed["text_sha256"] = json!(sha(b"Changed synthetic text."));
        assert!(validate_embeddings(&[changed], &expected, &manifest).is_err());
    }

    #[test]
    fn representation_rejects_unfinished_runs_and_contract_snapshot_mismatch() {
        let snapshot = b"{\"schema\":\"synthetic\"}\n";
        let manifest = json!({"schema":"slop_ninja_v6_embeddings_run_v1","status":"running"});
        assert!(
            validate_representation(&manifest, snapshot, 5)
                .unwrap_err()
                .to_string()
                .contains("complete")
        );
        let manifest = json!({"schema":"slop_ninja_v6_embeddings_run_v1","status":"complete",
            "requested":5,"processed":5,"unsupported":0,"representation_contract":{"schema":"different"},
            "representation_contract_sha256":sha(&snapshot[..snapshot.len()-1])});
        assert!(
            validate_representation(&manifest, snapshot, 5)
                .unwrap_err()
                .to_string()
                .contains("inline/snapshot")
        );
    }

    #[test]
    fn representation_varies_only_cohort_count_and_keeps_v6_identity_and_dimension() {
        let hash = sha(b"synthetic evidence, not a model artifact");
        let representation = json!({"schema":"slop_ninja_v6_penultimate_contract_v1","dimension":TEXT_DIM,
            "embedding_dtype":"float32","parent_artifact_id":V6_PARENT,"parent_manifest_sha256":V6_PARENT_MANIFEST,
            "artifact_id":V6_PARENT,"model":{"architecture":"ModernBertForSequenceClassification","frozen":true,"training":false},
            "hook":{"module":"model.classifier","api":"register_forward_pre_hook","argument":0,"shape":[1,TEXT_DIM],"normalization":"none","projection":"none"},
            "input":{"keys":["id","text"],"learned_inputs":["text"],"truncation":false,"max_tokens_including_special_tokens":2048},
            "runtime":{"profile":"cpu_float32_eager_parent_v1","device":"cpu","attention":"eager","autocast":false,"compile":false,
                "batch_size":1,"threads":1,"interop_threads":1,"packages":{},"reference_runtime":{"packages":{}}},
            "source":{"extractor_sha256":hash,"model_class_source_sha256":hash,"classifier_class_source_sha256":hash},
            "artifact_files_sha256":{"classifier/model.safetensors":hash,"classifier/config.json":hash,"classifier/tokenizer.json":hash,
                "runner/common.py":hash,"runner/requirements.lock":hash}});
        let bound = |representation: &Value, count: usize| {
            let mut snapshot = serde_json::to_vec(representation).unwrap();
            let digest = sha(&snapshot);
            snapshot.push(b'\n');
            let manifest = json!({"schema":"slop_ninja_v6_embeddings_run_v1","status":"complete","requested":count,
                "processed":count,"unsupported":0,"representation_contract":representation,"representation_contract_sha256":digest,
                "runtime_evidence":{"original_dependency_and_reference_checks_passed":true,"hook_reference_replay":[{
                    "hook_preserves_logits_and_probabilities_exactly":true,"reference_probability_max_absolute_error":0.0}]}});
            (manifest, snapshot)
        };
        for requested in [5, 7, 450] {
            let (manifest, snapshot) = bound(&representation, requested);
            validate_representation(&manifest, &snapshot, requested).unwrap();
            assert!(validate_representation(&manifest, &snapshot, requested + 1).is_err());
        }
        let mut changed = representation.clone();
        changed["dimension"] = json!(767);
        let (manifest, snapshot) = bound(&changed, 5);
        assert!(validate_representation(&manifest, &snapshot, 5).is_err());
        let mut changed = representation;
        changed["parent_artifact_id"] = json!(hash);
        let (manifest, snapshot) = bound(&changed, 5);
        assert!(validate_representation(&manifest, &snapshot, 5).is_err());
    }
}

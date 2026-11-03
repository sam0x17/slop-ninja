//! Selection-bound wrapper around the existing frozen CFG query transformer.
use super::*;
use std::{ffi::OsString, path::PathBuf};

#[allow(dead_code)]
#[path = "../compare_supervised_pangram/test_binding.rs"]
mod evaluation_binding;
#[allow(dead_code)]
#[path = "../final_test_binding/mod.rs"]
mod final_test_binding;
pub(crate) mod ordered;
#[cfg(test)]
mod tests;

type Files = BTreeMap<String, Vec<u8>>;
type Rows = BTreeMap<String, Value>;
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn indexed(bytes: &[u8]) -> Result<Rows> {
    let mut rows = Rows::new();
    for line in std::str::from_utf8(bytes)?.lines() {
        let row: Value = serde_json::from_str(line)?;
        let id = str_at(&row, "id")?.to_owned();
        ensure!(rows.insert(id, row).is_none(), "duplicate input ID");
    }
    Ok(rows)
}
fn jsonl(rows: impl IntoIterator<Item = Value>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, &row)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}
fn candidate<'a>(selection: &'a Value, name: &str) -> Result<&'a Value> {
    let matches = selection["candidates"]
        .as_array()
        .context("candidates")?
        .iter()
        .filter(|c| c["candidate"] == name)
        .collect::<Vec<_>>();
    ensure!(matches.len() == 1, "candidate absent/ambiguous");
    Ok(matches[0])
}

pub(crate) struct Parent {
    pub(crate) path: PathBuf,
    pub(crate) manifest: Value,
    pub(crate) contract: Value,
    pub(crate) files: Files,
    pub(crate) ids: BTreeSet<String>,
    pub(crate) groups: BTreeSet<String>,
}
impl Parent {
    pub(crate) fn load(selection: &Value, c: &Value) -> Result<Self> {
        let path = PathBuf::from(str_at(c, "numeric_path")?);
        ensure!(path.is_absolute(), "absolute frozen numeric path required");
        let mut files = Files::new();
        for (name, key) in [
            ("manifest.json", "numeric_manifest_sha256"),
            ("contract.json", "contract_file_sha256"),
        ] {
            let bytes = fs::read(path.join(name))?;
            ensure!(sha(&bytes) == digest_at(c, key)?, "changed frozen {name}");
            ensure!(
                evaluation_binding::bound(selection, &path.join(name))? == sha(&bytes),
                "parent absent from selection"
            );
            files.insert(name.into(), bytes);
        }
        let manifest: Value = serde_json::from_slice(&files["manifest.json"])?;
        let contract: Value = serde_json::from_slice(&files["contract.json"])?;
        ensure!(
            manifest["schema"] == "slop_ninja_shared_a_dataset_v1"
                && manifest["final_test_opened"] == false
                && contract["final_test_opened"] == false
                && manifest["contract_file_sha256"] == c["contract_file_sha256"]
                && manifest["contract_sha256"] == c["contract_sha256"]
                && manifest["contract_sha256"] == sha(&serde_json::to_vec(&contract)?),
            "frozen numeric contract differs"
        );
        let pb = fs::read(path.join("population.json"))?;
        ensure!(
            sha(&pb) == digest_at(&manifest, "population_file_sha256")?,
            "parent population changed"
        );
        let population: Value = serde_json::from_slice(&pb)?;
        let mut ids = BTreeSet::new();
        let mut groups = BTreeSet::new();
        for row in population["rows"].as_array().context("parent population")? {
            ensure!(
                ids.insert(str_at(row, "id")?.to_owned()),
                "duplicate parent population ID"
            );
            groups.insert(str_at(row, "group")?.to_owned());
        }
        for (group, part) in contract["split_assignments"]
            .as_object()
            .context("parent splits")?
        {
            if part != "test" {
                groups.insert(group.clone());
            }
        }
        files.insert("population.json".into(), pb);
        let mut parts = BTreeSet::new();
        let mut numeric_ids = BTreeSet::new();
        for split in manifest["splits"].as_array().context("split bindings")? {
            let part = str_at(split, "split")?;
            ensure!(
                ["train", "development", "calibration", "reference"].contains(&part)
                    && parts.insert(part),
                "unexpected/duplicate parent split"
            );
            let filename = format!("{part}.jsonl");
            let bytes = fs::read(path.join(&filename))?;
            ensure!(
                sha(&bytes) == digest_at(split, "sha256")?,
                "parent split changed"
            );
            let rows = indexed(&bytes)?;
            ensure!(
                rows.len() as u64 == counter(split, "rows")?,
                "parent row count"
            );
            for (id, row) in &rows {
                ensure!(
                    numeric_ids.insert(id.clone())
                        && ids.contains(id)
                        && row["input"]["contract_sha256"] == manifest["contract_sha256"]
                        && row["frozen_split"] == if part == "reference" { "train" } else { part }
                        && row["purpose"]
                            == if part == "reference" {
                                "reference"
                            } else {
                                "query"
                            },
                    "parent row binding differs"
                );
                groups.insert(str_at(row, "source_group")?.to_owned());
            }
            files.insert(filename, bytes);
        }
        ensure!(parts.len() == 4, "parent split missing");
        files.remove("manifest.json"); // The output manifest adds evaluation bindings.
        Ok(Self {
            path,
            manifest,
            contract,
            files,
            ids,
            groups,
        })
    }
}

pub(super) fn cli(args: &[OsString]) -> Result<()> {
    ensure!(
        args.len() == 6,
        "SELECTION CANDIDATE RECORDS MANIFEST CFG_EXPORT NEW_OUTPUT"
    );
    let selection_path = Path::new(&args[0]);
    let selection = final_test_binding::validate_selection_receipt(selection_path)?;
    let sb = fs::read(selection_path)?;
    ensure!(
        serde_json::from_slice::<Value>(&sb)? == selection,
        "selection changed during validation"
    );
    prepare_validated(
        &selection,
        &sb,
        args[1].to_str().context("candidate UTF8")?,
        Path::new(&args[2]),
        Path::new(&args[3]),
        Path::new(&args[4]),
        Path::new(&args[5]),
    )
}

// Tests supply a caller-validated synthetic selection; production always calls cli.
fn prepare_validated(
    selection: &Value,
    sb: &[u8],
    name: &str,
    records_path: &Path,
    dataset_path: &Path,
    export: &Path,
    out: &Path,
) -> Result<()> {
    ensure!(!out.try_exists()?, "output must be new");
    ensure!(
        selection["schema"] == "slop_ninja_balanced_a_selection_freeze_v3"
            && serde_json::from_slice::<Value>(sb)? == *selection,
        "selection snapshot differs"
    );
    ensure!(
        ["cfg_all", "cfg_leaf", "ordered_cfg", "global_cfg_control"].contains(&name),
        "only the four frozen CFG query candidates are supported"
    );
    let c = candidate(selection, name)?;
    let mut parent = Parent::load(selection, c)?;
    let ordered = matches!(name, "ordered_cfg" | "global_cfg_control");
    let leaf = if ordered {
        let leaf = Parent::load(selection, candidate(selection, "cfg_leaf")?)?;
        ordered::validate(&parent, &leaf, name)?;
        Some(leaf)
    } else {
        ensure!(
            parent.contract["ablation"]
                == if name == "cfg_all" {
                    "all"
                } else {
                    "leaf_grammar"
                },
            "candidate/ablation differs"
        );
        None
    };
    let db = fs::read(dataset_path)?;
    let d: Value = serde_json::from_slice(&db)?;
    ensure!(
        d["complete"] == true
            && d["test_text_opened"] == true
            && d["test_used_for_selection"] == false
            && d["selection_receipt_sha256"] == sha(sb),
        "dataset selection/complete binding differs"
    );
    let dataset = Dataset::parse_test(&db)?;
    ensure!(
        dataset.non_test_ids.is_empty() && dataset.split_assignments.values().all(|s| s == "test"),
        "Test-only dataset required"
    );
    let rb = fs::read(records_path)?;
    ensure!(sha(&rb) == dataset.records_sha256, "records hash differs");
    if d.get("test_records_sha256").is_some() {
        ensure!(
            d["test_records_sha256"] == sha(&rb),
            "Test records hash differs"
        );
    }
    let records = indexed(&rb)?;
    dataset.require_exact_ids(records.keys().map(String::as_str))?;
    let upstream = evaluation_binding::upstream_accounting(&d, &records.keys().cloned().collect())?;
    for (id, row) in &records {
        dataset.validate_metadata(id, row)?;
        ensure!(
            row["frozen_split"] == "test" && row["purpose"] == "query",
            "Test queries only"
        );
        ensure!(
            digest_at(row, "text_sha256")? == sha(str_at(row, "text")?.as_bytes()),
            "record text hash"
        );
        dataset.supervision(row)?;
        for p in std::iter::once(&parent).chain(leaf.iter()) {
            ensure!(
                !p.ids.contains(id) && !p.groups.contains(str_at(row, "source_group")?),
                "query overlaps frozen fitting/gallery population"
            );
        }
    }
    // Reuse the original engine/policy, export, span, vocabulary and normalization
    // checks. Temporary output is task-owned; only the completed bundle is published.
    let tmp = tempfile::tempdir_in(
        out.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    let base = leaf.as_ref().unwrap_or(&parent);
    let stage = tmp.path().join("base");
    super::transform_scope(&base.path, export, dataset_path, &stage, false, false, true)?;
    let mut audit = read(&stage.join("test_population.json"))?;
    let mut rows = indexed(&fs::read(stage.join("test.jsonl"))?)?;
    let windows = if ordered {
        ordered::apply(&parent.contract, export, &audit, &mut rows)?
    } else {
        Vec::new()
    };
    let bytes = jsonl(rows.values().cloned())?;
    audit["contract_sha256"] = parent.manifest["contract_sha256"].clone();
    audit["frozen_manifest_sha256"] = c["numeric_manifest_sha256"].clone();
    audit["selection_receipt_sha256"] = json!(sha(sb));
    audit["candidate"] = json!(name);
    audit["windows_bindings"] = json!(windows);
    let ab = serde_json::to_vec_pretty(&audit)?;
    let requested = records.len();
    let available = rows.len();
    let groups = rows
        .values()
        .map(|r| str_at(r, "source_group"))
        .collect::<Result<BTreeSet<_>>>()?;
    parent.manifest["splits"]
        .as_array_mut()
        .context("splits")?
        .push(json!({
        "split":"test","rows":available,"families":groups.len(),"sha256":sha(&bytes)}));
    let transform = json!({"schema":"slop_ninja_final_cfg_test_transform_v1","candidate":name,
        "selection_receipt_sha256":sha(sb),"frozen_manifest_sha256":c["numeric_manifest_sha256"],
        "dataset_manifest_sha256":sha(&db),"records_sha256":sha(&rb),
        "audit_file":"test_population.json","audit_file_sha256":sha(&ab),"rows_sha256":sha(&bytes),
        "requested":requested,"available":available,"supported":available,"excluded":requested-available,
        "upstream_accounting":upstream,"fit_performed":false,"normalization_fitted":false,
        "vocabulary_changed":false,"profiles_changed":false,"final_test_opened":true,
        "source_report_sha256":audit["source_report_sha256"],"source_rows_sha256":audit["source_rows_sha256"],
        "source_contract_file_sha256":audit["source_contract_file_sha256"],
        "support_policy":"retain execution failures; available partial projections retain the frozen masks; no truncation, refitting or imputation"});
    parent.manifest["test_transform"] = transform;
    parent.manifest["final_test_opened"] = json!(true);
    parent.manifest["test_used_for_selection"] = json!(false);
    parent.manifest["evaluation_only"] = json!(true);
    parent.manifest["evaluation_complete"] = json!(available == requested);
    parent.manifest["final_cfg_source_sha256"] = json!(sha(include_bytes!("mod.rs")));
    parent.manifest["ordered_source_sha256"] = json!(sha(include_bytes!("ordered.rs")));
    parent.files.extend([
        ("test.jsonl".into(), bytes),
        ("test_population.json".into(), ab),
        ("test_dataset_manifest.json".into(), db),
        ("selection-receipt.json".into(), sb.to_vec()),
    ]);
    parent.manifest["evaluation_files_sha256"] = json!(
        parent
            .files
            .iter()
            .map(|(k, b)| (k, sha(b)))
            .collect::<BTreeMap<_, _>>()
    );
    parent.files.insert(
        "manifest.json".into(),
        serde_json::to_vec_pretty(&parent.manifest)?,
    );
    let publish = tmp.path().join("complete");
    fs::create_dir(&publish)?;
    for (file, bytes) in parent.files {
        write_new(&publish.join(file), &bytes)?;
    }
    // Exercise the consumer's actual metadata-only admission before publishing.
    evaluation_binding::load_data(&publish, selection, &sha(sb))?;
    ensure!(!out.try_exists()?, "output appeared during transformation");
    fs::rename(&publish, out)?;
    fs::File::open(
        out.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()?;
    println!(
        "{}",
        json!({"candidate":name,"requested":requested,"available":available,
        "excluded":requested-available,"manifest":out.join("manifest.json"),"fit_performed":false})
    );
    Ok(())
}

//! A frozen transformation path. It never calls prepare or Profiles::fit.
use super::*;

#[path = "../transform_final_cfg/mod.rs"]
mod final_queries;

#[path = "../prepare_final_test_references/mod.rs"]
mod final_references;

pub(crate) fn transform_cli() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if matches!(
        args.first().and_then(|v| v.to_str()),
        Some("select" | "project" | "encode")
    ) {
        return final_references::cli(&args);
    }
    if args.len() == 6 {
        return final_queries::cli(&args);
    }
    ensure!(
        args.len() == 4,
        "transform_supervised_forest FROZEN_NUMERIC_DIR TEST_EXPORT_DIR TEST_DATASET_MANIFEST NEW_OUTPUT"
    );
    transform(
        Path::new(&args[0]),
        Path::new(&args[1]),
        Path::new(&args[2]),
        Path::new(&args[3]),
    )
}

fn transform(frozen: &Path, input: &Path, dataset_path: &Path, out: &Path) -> Result<()> {
    transform_scope(frozen, input, dataset_path, out, false, false, false)
}

pub(crate) fn transform_references(
    frozen: &Path,
    input: &Path,
    dataset_path: &Path,
    out: &Path,
) -> Result<()> {
    transform_scope(frozen, input, dataset_path, out, true, false, false)
}

/// Apply existing feature/profile meanings to disjoint additional Train queries.
/// Produces a separate fragment; never changes fitted normalization or galleries.
pub(crate) fn transform_train_extension(
    frozen: &Path,
    input: &Path,
    dataset_path: &Path,
    out: &Path,
) -> Result<()> {
    transform_scope(frozen, input, dataset_path, out, false, true, false)
}

fn transform_scope(
    frozen: &Path,
    input: &Path,
    dataset_path: &Path,
    out: &Path,
    references_only: bool,
    train_extension: bool,
    final_selection: bool,
) -> Result<()> {
    ensure!(!out.try_exists()?, "output must be new");
    let manifest_bytes = fs::read(frozen.join("manifest.json"))?;
    let mut manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    ensure!(
        manifest["schema"] == "slop_ninja_shared_a_dataset_v1",
        "frozen manifest schema"
    );
    ensure!(
        manifest["final_test_opened"] == false,
        "frozen parent already opened Test"
    );
    let contract_bytes = fs::read(frozen.join("contract.json"))?;
    ensure!(
        sha(&contract_bytes) == digest_at(&manifest, "contract_file_sha256")?,
        "frozen contract file changed"
    );
    let contract: Value = serde_json::from_slice(&contract_bytes)?;
    ensure!(
        contract["schema"] == "slop_ninja_shared_a_numeric_contract_v1"
            && contract["feature_policy"] == FEATURE_POLICY
            && contract["feature_policy_version"] == 1,
        "unsupported frozen numeric contract"
    );
    let contract_hash = digest_at(&manifest, "contract_sha256")?.to_owned();
    ensure!(
        sha(&serde_json::to_vec(&contract)?) == contract_hash,
        "frozen contract semantic hash changed"
    );
    let frozen_population = fs::read(frozen.join("population.json"))?;
    ensure!(
        sha(&frozen_population) == digest_at(&manifest, "population_file_sha256")?,
        "frozen population changed"
    );
    let split_bindings = manifest["splits"]
        .as_array()
        .context("frozen split bindings")?;
    let mut files = Vec::new();
    let mut splits = BTreeSet::new();
    let mut frozen_ids = BTreeSet::new();
    let mut fitted_groups = BTreeSet::new();
    for binding in split_bindings {
        let split = str_at(binding, "split")?;
        ensure!(
            PARTS.contains(&split) || split == "reference",
            "frozen dataset must not already contain Test"
        );
        ensure!(
            splits.insert(split.to_owned()),
            "duplicate frozen split binding"
        );
        let bytes = fs::read(frozen.join(format!("{split}.jsonl")))?;
        ensure!(
            sha(&bytes) == digest_at(binding, "sha256")?,
            "changed frozen split {split}"
        );
        let mut row_count = 0u64;
        let mut groups = BTreeSet::new();
        for line in std::str::from_utf8(&bytes)?.lines() {
            let row: Value = serde_json::from_str(line)?;
            ensure!(
                row["input"]["contract_sha256"] == contract_hash,
                "frozen row contract mismatch"
            );
            let id = str_at(&row, "id")?.to_owned();
            ensure!(frozen_ids.insert(id), "duplicate frozen row ID");
            let group = str_at(&row, "source_group")?.to_owned();
            let expected_split = if split == "reference" { "train" } else { split };
            ensure!(
                row["frozen_split"] == expected_split
                    && contract["split_assignments"][&group] == expected_split,
                "frozen family split mismatch"
            );
            ensure!(
                row["purpose"]
                    == if split == "reference" {
                        "reference"
                    } else {
                        "query"
                    },
                "reference leakage in frozen splits"
            );
            groups.insert(group.clone());
            fitted_groups.insert(group);
            row_count += 1;
        }
        ensure!(
            row_count == counter(binding, "rows")?
                && groups.len() as u64 == counter(binding, "families")?,
            "frozen split count mismatch"
        );
        files.push((format!("{split}.jsonl"), bytes));
    }
    ensure!(
        PARTS.iter().all(|part| splits.contains(*part)) && splits.contains("reference"),
        "missing frozen split bindings"
    );
    // Include families excluded for execution failure: they must not migrate to
    // Test simply because they never reached a numeric split file.
    for (group, split) in contract["split_assignments"]
        .as_object()
        .context("frozen source-family assignments")?
    {
        if split != "test" {
            fitted_groups.insert(group.clone());
        }
    }
    let dataset_bytes = fs::read(dataset_path)?;
    let dataset = if train_extension {
        let d = Dataset::parse(&dataset_bytes)?;
        ensure!(
            d.split_assignments.values().all(|s| s == "train") && d.test_ids.is_empty(),
            "training extension must contain only Train families and no Test IDs"
        );
        d
    } else if references_only && final_selection {
        Dataset::parse_final_references(&dataset_bytes)?
    } else if references_only {
        Dataset::parse_references(&dataset_bytes)?
    } else {
        Dataset::parse_test(&dataset_bytes)?
    };
    let incoming_ids = if (references_only && !final_selection) || train_extension {
        &dataset.non_test_ids
    } else {
        &dataset.test_ids
    };
    ensure!(
        incoming_ids.is_disjoint(&frozen_ids),
        "Test ID reused from a frozen fitting/gallery split"
    );
    for (group, split) in &dataset.split_assignments {
        if split == "test" || references_only || train_extension {
            ensure!(
                !fitted_groups.contains(group),
                "Test source family crosses a frozen fitting/gallery split"
            );
        }
        if let Some(previous) = contract["split_assignments"].get(group) {
            ensure!(
                previous == split,
                "Test manifest changes a frozen source-family assignment"
            );
        }
    }
    // Compare engine, parser, resolver and policy before opening Test rows.
    let test_contract: Value = serde_json::from_slice(&fs::read(input.join("contract.json"))?)?;
    ensure!(
        test_contract["engine_identity"] == contract["grammar_identities"]["document_engine"],
        "Test engine differs from frozen engine"
    );
    ensure!(
        test_contract["parser_identity"] == contract["grammar_identities"]["native_parser"],
        "Test parser differs from frozen parser"
    );
    ensure!(
        test_contract["policy"] == contract["document_policy"],
        "Test document policy differs from frozen policy"
    );
    ensure!(
        contract["projection"]["feature_schema"] == "slop_ninja_cfg_forest_expectations_v1"
            && contract["projection"]["resolver"]
                == "acyclic-uniform-derivations-max-covered-min-islands-v1",
        "unsupported frozen projection/resolver"
    );
    let exported = read_export(input, &dataset)?;
    ensure!(
        exported.source_contract == test_contract,
        "Test contract changed during transformation"
    );
    let keys = contract["coordinates"]
        .as_array()
        .context("frozen coordinates")?
        .iter()
        .map(|key| {
            key.as_str()
                .map(str::to_owned)
                .context("frozen coordinate must be string")
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        !keys.is_empty() && keys.windows(2).all(|pair| pair[0] < pair[1]),
        "frozen coordinates must be nonempty sorted unique keys"
    );
    let ablation = Ablation::parse(str_at(&contract, "ablation")?)?;
    for key in &keys {
        if !FIXED.contains(&key.as_str()) {
            validate_key(key)?;
        }
        ensure!(ablation.includes(key), "coordinate outside frozen ablation");
    }
    ensure!(
        contract["layout"] == "standardized_values_then_train_centered_availability_masks"
            && contract["normalization"]["transform"] == "log1p(count/denominator)"
            && contract["normalization"]["fit"] == "train_query_only_equal_source_family_weight"
            && contract["masked_standardized_value"] == 0.0,
        "unsupported frozen transform semantics"
    );
    let vector = |value: &Value| -> Result<Vec<f64>> {
        let values = value.as_array().context("frozen normalization vector")?;
        ensure!(
            values.len() == keys.len(),
            "frozen normalization dimension mismatch"
        );
        values
            .iter()
            .map(|value| {
                let n = value.as_f64().context("frozen numeric coordinate")?;
                ensure!(n.is_finite(), "nonfinite frozen parameter");
                Ok(n)
            })
            .collect()
    };
    let means = vector(&contract["normalization"]["mean"])?;
    let scales = vector(&contract["normalization"]["scale"])?;
    let centers = vector(&contract["availability_mask_center"])?;
    ensure!(
        scales.iter().all(|s| *s > 0.0) && centers.iter().all(|c| (0.0..=1.0).contains(c)),
        "invalid frozen scales/mask centers"
    );
    ensure!(
        counter(&contract, "input_dimension")? == (keys.len() * 2) as u64
            && counter(&manifest, "feature_dimension")? == (keys.len() * 2) as u64,
        "frozen input dimension mismatch"
    );
    let profiles = Profiles::from_contract(&contract)?;
    let mut test_bytes = Vec::new();
    let mut test_groups = BTreeSet::new();
    let mut available = 0usize;
    for row in exported.rows.iter().filter(|row| row.available) {
        ensure!(
            if train_extension {
                row.split == "train" && row.supervision.purpose == "query"
            } else if references_only {
                (if final_selection {
                    row.split == "test"
                } else {
                    matches!(row.split.as_str(), "development" | "calibration")
                }) && row.supervision.purpose == "reference"
            } else {
                row.split == "test" && row.supervision.purpose == "query"
            },
            "row outside frozen transformation scope"
        );
        let numeric = numeric_row(
            row,
            &contract_hash,
            &keys,
            &means,
            &scales,
            &centers,
            &profiles,
        )?;
        serde_json::to_writer(&mut test_bytes, &numeric)?;
        test_bytes.push(b'\n');
        test_groups.insert(&row.group);
        available += 1;
    }
    // The selection-gated route must retain an all-failed requested population.
    ensure!(available > 0 || final_selection, "no available Test rows");
    let audit = serde_json::to_vec_pretty(&json!({
        "schema":if train_extension {"slop_ninja_train_extension_transform_v1"} else if references_only {"slop_ninja_partition_reference_transform_v1"} else {"slop_ninja_supervised_a_test_transform_v1"},"contract_sha256":contract_hash,
        "frozen_manifest_sha256":sha(&manifest_bytes),"dataset_manifest_sha256":sha(&dataset_bytes),
        "source_contract_file_sha256":sha(&exported.contract_bytes),"source_report_sha256":sha(&exported.report_bytes),
        "source_rows_sha256":sha(&exported.bytes),"records_sha256":dataset.records_sha256,
        "requested":exported.rows.len(),"available":available,"excluded":exported.exclusions,"rows":exported.population,
        "fit_performed":false,"final_test_opened":final_selection || (!references_only && !train_extension),
        "implementation_sha256":sha(include_bytes!("frozen.rs")),
        "pipeline_implementation_sha256":sha(include_bytes!("pipeline.rs")),
        "supervision_implementation_sha256":sha(include_bytes!("supervision.rs"))
    }))?;
    let (rows_file, audit_file) = if train_extension {
        ("train_extension.jsonl", "train_extension_population.json")
    } else if references_only {
        ("partition_references.jsonl", "reference_population.json")
    } else {
        ("test.jsonl", "test_population.json")
    };
    if train_extension {
        manifest["training_extension_transform"] = json!({"audit_file":audit_file,"audit_file_sha256":sha(&audit),
            "rows_file":rows_file,"rows_sha256":sha(&test_bytes),"frozen_manifest_sha256":sha(&manifest_bytes),
            "dataset_manifest_sha256":sha(&dataset_bytes),"requested":exported.rows.len(),"available":available,
            "excluded":exported.rows.len()-available,"fit_performed":false,"final_test_opened":false,
            "profiles_changed":false,"normalization_fitted":false,"requires_explicit_training_bundle_assembly":true});
    } else if references_only {
        manifest["partition_reference_transform"] = json!({"audit_file":audit_file,"audit_file_sha256":sha(&audit),
            "rows_file":rows_file,"rows_sha256":sha(&test_bytes),"frozen_manifest_sha256":sha(&manifest_bytes),
            "dataset_manifest_sha256":sha(&dataset_bytes),"requested":exported.rows.len(),"available":available,
            "excluded":exported.rows.len()-available,"fit_performed":false,"final_test_opened":final_selection});
    } else {
        manifest["splits"].as_array_mut().unwrap().push(json!({"split":"test","rows":available,"families":test_groups.len(),"sha256":sha(&test_bytes)}));
        // Preserve every original binding and count; additions describe the separate
        // Test population. The frozen contract's own final_test_opened remains false.
        manifest["final_test_opened"] = json!(true);
        manifest["test_transform"] = json!({"audit_file":"test_population.json","audit_file_sha256":sha(&audit),
        "frozen_manifest_sha256":sha(&manifest_bytes),"dataset_manifest_sha256":sha(&dataset_bytes),
        "requested":exported.rows.len(),"available":available,"excluded":exported.rows.len()-available,"fit_performed":false});
    }
    let output_manifest = serde_json::to_vec_pretty(&manifest)?;
    fs::create_dir(out)?;
    write_new(&out.join("contract.json"), &contract_bytes)?;
    write_new(&out.join("population.json"), &frozen_population)?;
    for (name, bytes) in files {
        write_new(&out.join(name), &bytes)?;
    }
    write_new(&out.join(rows_file), &test_bytes)?;
    write_new(&out.join(audit_file), &audit)?;
    write_new(&out.join("manifest.json"), &output_manifest)?;
    if !final_selection {
        println!("{}", serde_json::to_string(&manifest)?);
    }
    Ok(())
}

#[cfg(test)]
#[path = "frozen_tests.rs"]
mod tests;

use super::super::final_queries as cfg;
use super::*;

pub(super) fn exclusion_reasons(
    cfg_required: bool,
    cfg_available: bool,
    population: &Value,
    embedding: Option<&Value>,
) -> Vec<Value> {
    let mut reasons = Vec::new();
    if cfg_required && !cfg_available {
        reasons.push(json!({"stage":"cfg","reason":population["exclusion_reason"]}));
    }
    if let Some(e) = embedding {
        if e["status"] != "ok" {
            reasons.push(json!({"stage":"v6","status":e["status"],"reason":e["reason"],"token_count":e["token_count"]}));
        }
    }
    reasons
}

pub(super) fn project(
    selection: &Value,
    sb: &[u8],
    name: &str,
    records_path: &Path,
    manifest_path: &Path,
    export: &Path,
    embeddings_path: &Path,
    out: &Path,
) -> Result<()> {
    ensure!(!out.try_exists()?, "new reference projection required");
    let c = candidate(selection, name)?;
    let parent = cfg::Parent::load(selection, c)?;
    let ordered = matches!(name, "ordered_cfg" | "global_cfg_control");
    let plain_cfg = matches!(name, "cfg_all" | "cfg_leaf");
    if plain_cfg {
        ensure!(
            parent.contract["ablation"]
                == if name == "cfg_all" {
                    "all"
                } else {
                    "leaf_grammar"
                },
            "CFG candidate/ablation differs"
        );
    }
    let text = if ordered || plain_cfg {
        None
    } else {
        Some(text_projection::Parent::load(c)?)
    };
    ensure!(
        (text.is_none() && embeddings_path == Path::new("-"))
            || (text.is_some() && embeddings_path != Path::new("-")),
        "embedding argument does not match candidate"
    );
    let base_candidate = candidate(
        selection,
        if name == "cfg_all" {
            "cfg_all"
        } else {
            "cfg_leaf"
        },
    )?;
    let base = cfg::Parent::load(selection, base_candidate)?;
    if ordered {
        cfg::ordered::validate(&parent, &base, name)?;
    }
    if text.is_some() {
        ensure!(
            parent.contract["cfg_parent_contract"] == base.contract
                && parent.contract["cfg_parent_contract_sha256"]
                    == base.manifest["contract_sha256"]
                && parent.contract["cfg_parent_contract_file_sha256"]
                    == base.manifest["contract_file_sha256"]
                && parent.contract["parents"]["cfg"]["manifest_sha256"]
                    == base_candidate["numeric_manifest_sha256"],
            "text candidate CFG parent differs"
        );
    }
    let (mb, m) = read(manifest_path)?;
    let record_bytes = fs::read(records_path)?;
    let (qb, q) = read(
        &manifest_path
            .parent()
            .context("reference manifest parent")?
            .join("query_dataset_manifest.json"),
    )?;
    ensure!(
        m["selection_receipt_sha256"] == sha(sb)
            && m["records_sha256"] == sha(&record_bytes)
            && m["query_dataset_manifest_sha256"] == sha(&qb)
            && m["reference_transform_only"] == true
            && m["final_test_reference_transform_only"] == true,
        "reference manifest binding differs"
    );
    let (records, query_groups) = admission::validate(selection, sb, &q, &record_bytes)?;
    let dataset = Dataset::parse_final_references(&mb)?;
    dataset.require_exact_ids(records.keys().map(String::as_str))?;
    let profiles = Profiles::from_contract(&parent.contract)?;
    for (id, r) in &records {
        dataset.validate_metadata(id, r)?;
        let s = dataset.supervision(r)?;
        ensure!(
            profiles.roles(&s)["human"]["known"] == false
                && !parent.ids.contains(id)
                && !parent.groups.contains(str_at(r, "source_group")?)
                && !query_groups.contains(str_at(r, "source_group")?),
            "reference intersects frozen/query enrollment"
        );
    }
    let tmp = tempfile::tempdir_in(
        out.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    let stage = tmp.path().join("cfg");
    super::super::transform_scope(&base.path, export, manifest_path, &stage, true, false, true)?;
    let cfg_bytes = fs::read(stage.join("partition_references.jsonl"))?;
    let mut cfg_rows = rows(&cfg_bytes)?;
    let (_, cfg_audit) = read(&stage.join("reference_population.json"))?;
    let windows = if ordered {
        cfg::ordered::apply(&parent.contract, export, &cfg_audit, &mut cfg_rows)?
    } else {
        Vec::new()
    };
    let cfg_population = array(&cfg_audit, "rows")?
        .iter()
        .map(|r| Ok((str_at(r, "id")?.to_owned(), r)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    ensure!(
        cfg_population.keys().eq(records.keys()),
        "reference CFG roster differs"
    );
    let embeddings = if text.is_some() {
        Some(text_projection::reference_embeddings(
            embeddings_path,
            &parent.contract,
            &records,
        )?)
    } else {
        None
    };
    let cfg_required = text.is_none() || parent.contract["ablation"] == "combined";
    let mut numeric = Vec::new();
    let mut audit_rows = Vec::new();
    let mut excluded = Vec::new();
    for (id, r) in &records {
        let cp = cfg_population[id];
        let embedding = embeddings.as_ref().map(|(rr, _, _)| &rr[id]);
        let reasons = exclusion_reasons(cfg_required, cfg_rows.contains_key(id), cp, embedding);
        if reasons.is_empty() {
            let row = if let Some(text) = &text {
                let s = dataset.supervision(r)?;
                json!({"id":id,"source_group":r["source_group"],"frozen_split":"test","purpose":"reference",
                    "exposed_to_grammar":s.exposed,"roles":profiles.roles(&s),"targets":profiles.targets(&s),
                    "input":text.input(&embedding.context("reference embedding")?["embedding"],cfg_rows.get(id).map(|r| &r["input"]["x"]))?})
            } else {
                cfg_rows[id].clone()
            };
            numeric.push(row);
        } else {
            excluded.push(
                json!({"id":id,"source_group":r["source_group"],"text_sha256":r["text_sha256"],
                "reason":"frozen_reference_input_unavailable","reasons":reasons}),
            );
        }
        let mut row = (*cp).clone();
        row["available"] = json!(reasons.is_empty());
        row["cfg_diagnostic"] = json!({"available":cfg_rows.contains_key(id),"exclusion_reason":cp["exclusion_reason"],"required_for_model":cfg_required});
        row["evaluation_exclusions"] = json!(reasons);
        audit_rows.push(row);
    }
    let numeric_bytes = jsonl(numeric)?;
    let known_bytes = fs::read(parent.path.join("reference.jsonl"))?;
    ensure!(
        super::records(&known_bytes)?.len() == 12,
        "twelve frozen Train references required"
    );
    let audit = json!({"schema":"slop_ninja_final_test_reference_population_v1","selection_receipt_sha256":sha(sb),
        "candidate":name,"frozen_manifest_sha256":c["numeric_manifest_sha256"],"contract_sha256":c["contract_sha256"],
        "reference_records_sha256":sha(&record_bytes),"reference_dataset_manifest_sha256":sha(&mb),
        "query_dataset_manifest_sha256":sha(&qb),"requested":4,"available":4-excluded.len(),"excluded":excluded,
        "rows":audit_rows,"windows_bindings":windows,"fit_performed":false,"final_test_opened":true});
    let ab = pretty(&audit)?;
    let result = json!({"schema":"slop_ninja_final_test_reference_projection_v1","candidate":name,
        "selection_receipt_sha256":sha(sb),"frozen_manifest_sha256":c["numeric_manifest_sha256"],
        "contract_sha256":c["contract_sha256"],"contract_file_sha256":c["contract_file_sha256"],
        "known_reference_sha256":sha(&known_bytes),"partition_reference_sha256":sha(&numeric_bytes),
        "reference_records_sha256":sha(&record_bytes),"reference_dataset_manifest_sha256":sha(&mb),
        "query_dataset_manifest_sha256":sha(&qb),"reference_population_sha256":sha(&ab),
        "requested":16,"known_reference_rows":12,"requested_test_references":4,
        "supported_test_references":4-excluded.len(),"supported":16-excluded.len(),"complete":excluded.is_empty(),"excluded":excluded,
        "cfg_supported":cfg_rows.len(),"cfg_required_for_model":cfg_required,
        "embedding_bindings":embeddings.as_ref().map(|(_,b,_)|b),"cfg_transform":cfg_audit,
        "fit_performed":false,"normalization_fitted":false,"profiles_changed":false,"test_used_for_selection":false,
        "final_test_opened":true,"implementation_sha256":sha(include_bytes!("project.rs"))});
    let mut files = Files::from([
        ("manifest.json".into(), pretty(&result)?),
        (
            "contract.json".into(),
            parent.files["contract.json"].clone(),
        ),
        ("reference.jsonl".into(), known_bytes),
        ("partition_references.jsonl".into(), numeric_bytes),
        ("reference_population.json".into(), ab),
        ("reference_dataset_manifest.json".into(), mb),
        ("query_dataset_manifest.json".into(), qb),
        ("selection-receipt.json".into(), sb.to_vec()),
        ("reference_records.jsonl".into(), record_bytes),
    ]);
    if let Some((_, _, snapshots)) = embeddings {
        files.extend(snapshots);
    }
    publish(out, files)
}

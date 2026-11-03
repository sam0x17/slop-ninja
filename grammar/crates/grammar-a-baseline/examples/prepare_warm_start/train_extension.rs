//! Explicit Train-only append. Feature meanings, normalization and galleries stay frozen.
use super::*;
const PARTS: [&str; 4] = ["train", "development", "calibration", "reference"];
const SCALE_SCHEMA: &str = "slop_ninja_v6_residual_train_scale_v1";

fn read_json(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn bound_bytes(dir: &Path, name: &str, expected: &Value) -> Result<Vec<u8>> {
    let b = fs::read(dir.join(name))?;
    ensure!(
        expected.as_str() == Some(sha(&b).as_str()),
        "changed {name}"
    );
    Ok(b)
}
fn disjoint_train(old: &[Value], incoming: &[Value]) -> Result<()> {
    let mut ids: BTreeSet<_> = old
        .iter()
        .map(|r| s(r, "id").map(str::to_owned))
        .collect::<Result<_>>()?;
    ensure!(ids.len() == old.len(), "duplicate parent ID");
    let groups: BTreeSet<_> = old
        .iter()
        .map(|r| s(r, "source_group").map(str::to_owned))
        .collect::<Result<_>>()?;
    for r in incoming {
        ensure!(
            r["frozen_split"] == "train" && r["purpose"] == "query",
            "Train queries only"
        );
        ensure!(ids.insert(s(r, "id")?.into()), "reused ID");
        ensure!(
            !groups.contains(s(r, "source_group")?),
            "reused source family"
        );
    }
    Ok(())
}
fn joined_row(
    row: &Value,
    activation: &Value,
    text_hash: &str,
    modality: &str,
    cfg_hash: &str,
) -> Result<Value> {
    ensure!(
        row["input"]["contract_sha256"] == cfg_hash,
        "CFG fragment contract differs"
    );
    ensure!(
        row["id"] == activation["id"] && activation["text_sha256"] == text_hash,
        "activation join differs"
    );
    ensure!(
        activation["status"] == "ok" && activation["artifact_id"] == ARTIFACT,
        "unqualified activation"
    );
    let mut x = numbers(&activation["embedding"], DIM)?;
    let cfg = numbers(&row["input"]["x"], 1820)?;
    x.extend(
        cfg.into_iter()
            .map(|v| if modality == "text" { 0. } else { v }),
    );
    let mut out = row.clone();
    out["input"]["x"] = json!(x);
    Ok(out)
}

pub(super) fn run(
    parent: &Path,
    fragment: &Path,
    embeddings: &Path,
    dataset_path: &Path,
    generation_protocol_path: &Path,
    protocol_path: &Path,
    out: &Path,
) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let protocol = read_json(protocol_path)?;
    ensure!(
        protocol["schema"] == SCALE_SCHEMA
            && protocol["epochs"] == 250
            && protocol["seeds"] == json!([17, 29, 41])
            && protocol["controls"] == json!(["text", "combined"])
            && protocol["final_test_opened"] == false
            && protocol["normalization_fitted"] == false,
        "unsupported scale protocol"
    );
    let evidence = EmbeddingEvidence::load(embeddings)?;
    let em = &evidence.manifest;
    let mut acts = BTreeMap::new();
    for r in rows(&evidence.rows)? {
        ensure!(
            acts.insert(s(&r, "id")?.to_owned(), r).is_none(),
            "duplicate activation"
        );
    }
    let fm = read_json(&fragment.join("manifest.json"))?;
    let fc_bytes = bound_bytes(fragment, "contract.json", &fm["contract_file_sha256"])?;
    let fc: Value = serde_json::from_slice(&fc_bytes)?;
    ensure!(
        fm["contract_sha256"] == sha(&serde_json::to_vec(&fc)?),
        "CFG semantic contract mismatch"
    );
    let t = &fm["training_extension_transform"];
    ensure!(
        t["final_test_opened"] == false
            && t["fit_performed"] == false
            && t["profiles_changed"] == false
            && t["normalization_fitted"] == false,
        "unqualified frozen transformation"
    );
    let incoming_bytes = bound_bytes(fragment, "train_extension.jsonl", &t["rows_sha256"])?;
    let incoming = rows(&incoming_bytes)?;
    let audit_bytes = bound_bytes(
        fragment,
        "train_extension_population.json",
        &t["audit_file_sha256"],
    )?;
    let audit: Value = serde_json::from_slice(&audit_bytes)?;
    // Retain exclusions in preprocessing evidence; this comparison requires a
    // common complete population rather than silently changing either modality.
    ensure!(
        t["excluded"] == 0
            && t["available"].as_u64() == Some(incoming.len() as u64)
            && t["requested"] == t["available"]
            && em["requested"] == t["requested"]
            && acts.len() == incoming.len()
            && !incoming.is_empty(),
        "incomplete matched extension"
    );
    ensure!(
        protocol["original_rows"] == 929
            && protocol["requested_new_roots"]
                .as_u64()
                .is_some_and(|n| n > 0)
            && protocol["requested_new_generations"].as_u64().is_some()
            && incoming.len() as u64
                <= protocol["requested_new_roots"].as_u64().unwrap()
                    + protocol["requested_new_generations"].as_u64().unwrap(),
        "extension outside frozen requested population"
    );
    let pop_rows = audit["rows"].as_array().context("extension population")?;
    let mut text_hashes = BTreeMap::new();
    for r in pop_rows {
        ensure!(
            r["available"] == true && r["split"] == "train" && r["purpose"] == "query",
            "population scope"
        );
        ensure!(
            text_hashes
                .insert(
                    s(r, "id")?.to_owned(),
                    s(&r["metadata"], "text_sha256")?.to_owned()
                )
                .is_none(),
            "duplicate population ID"
        );
    }
    ensure!(
        text_hashes.keys().eq(acts.keys()),
        "extension population/activation roster mismatch"
    );
    let dataset_bytes = fs::read(dataset_path)?;
    let dataset: Value = serde_json::from_slice(&dataset_bytes)?;
    ensure!(
        t["dataset_manifest_sha256"] == sha(&dataset_bytes)
            && audit["dataset_manifest_sha256"] == sha(&dataset_bytes)
            && audit["records_sha256"] == dataset["records_sha256"],
        "assembly/CFG population binding mismatch"
    );
    let generation_bytes = fs::read(generation_protocol_path)?;
    check_admission(&protocol, &dataset, &sha(&generation_bytes), &incoming)?;
    let mut bundles = Vec::new();
    let mut peer_original = None;
    let mut peer_baseline = None;
    for mode in ["text", "combined"] {
        let dir = parent.join(mode);
        let mb = bound_bytes(
            &dir,
            "manifest.json",
            &protocol["parent_manifest_sha256"][mode],
        )?;
        let mut m: Value = serde_json::from_slice(&mb)?;
        ensure!(
            m["final_test_opened"] == false && m["excluded"] == 0 && m["requested"] == 929,
            "invalid original parent population"
        );
        let cb = bound_bytes(&dir, "contract.json", &m["contract_file_sha256"])?;
        let mut c: Value = serde_json::from_slice(&cb)?;
        ensure!(
            m["contract_sha256"] == sha(&serde_json::to_vec(&c)?)
                && c["ablation"] == mode
                && c["warm_start_schema"] == "slop_ninja_raw_v6_cfg_residual_input_v1"
                && c["input_dimension"] == 2588,
            "wrong warm parent"
        );
        ensure!(
            c["cfg_parent_contract"] == fc
                && c["parents"]["cfg"]["manifest_sha256"] == t["frozen_manifest_sha256"],
            "fragment uses different CFG parent"
        );
        ensure!(
            c["parents"]["representation_contract_sha256"] == em["representation_contract_sha256"],
            "V6 representation changed"
        );
        for role in ["human_profiles", "ai_profiles"] {
            ensure!(c[role] == fc[role], "profile roster differs");
        }
        let pb = bound_bytes(&dir, "population.json", &m["population_file_sha256"])?;
        let mut pop: Value = serde_json::from_slice(&pb)?;
        let baseline_bytes = bound_bytes(
            &dir,
            "baseline-origin-reference.jsonl",
            &m["baseline_origin_reference_sha256"],
        )?;
        let mut baseline = rows(&baseline_bytes)?;
        let mut partitions = BTreeMap::new();
        let mut old = Vec::new();
        let mut originals = BTreeMap::new();
        for part in PARTS {
            let binding = m["splits"]
                .as_array()
                .context("splits")?
                .iter()
                .find(|v| v["split"] == part)
                .context("missing parent split")?;
            let b = bound_bytes(&dir, &format!("{part}.jsonl"), &binding["sha256"])?;
            let rs = rows(&b)?;
            ensure!(
                binding["rows"].as_u64() == Some(rs.len() as u64),
                "parent row count"
            );
            for r in &rs {
                ensure!(
                    r["frozen_split"] == if part == "reference" { "train" } else { part }
                        && r["purpose"]
                            == if part == "reference" {
                                "reference"
                            } else {
                                "query"
                            }
                        && r["input"]["contract_sha256"] == m["contract_sha256"],
                    "parent scope/contract mismatch"
                );
                numbers(&r["input"]["x"], 2588)?;
            }
            originals.insert(part, json!({"rows":rs.len(),"file_sha256":sha(&b)}));
            old.extend(rs.iter().cloned());
            partitions.insert(part, rs);
        }
        ensure!(
            old.len() == 929 && baseline.len() == old.len(),
            "complete original population required"
        );
        let old_ids = old
            .iter()
            .map(|r| s(r, "id").map(str::to_owned))
            .collect::<Result<BTreeSet<_>>>()?;
        let baseline_ids = baseline
            .iter()
            .map(|r| s(r, "id").map(str::to_owned))
            .collect::<Result<BTreeSet<_>>>()?;
        ensure!(
            old_ids == baseline_ids && old_ids.len() == old.len(),
            "parent baseline roster"
        );
        if let Some(ref previous) = peer_original {
            ensure!(*previous == old_ids, "parent modality populations differ");
        } else {
            peer_original = Some(old_ids);
        }
        disjoint_train(&old, &incoming)?;
        for row in &incoming {
            let id = s(row, "id")?;
            let a = acts.get(id).context("missing activation")?;
            let r = joined_row(
                row,
                a,
                text_hashes.get(id).context("missing text hash")?,
                mode,
                s(&fm, "contract_sha256")?,
            )?;
            partitions.get_mut("train").unwrap().push(r);
            baseline.push(json!({"id":id,"text_sha256":a["text_sha256"],"logits":a["logits"],"probabilities":a["probabilities"],"artifact_id":a["artifact_id"]}));
        }
        baseline.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        let bb = jsonl(&baseline)?;
        if let Some(ref previous) = peer_baseline {
            ensure!(*previous == bb, "modality baselines differ");
        } else {
            peer_baseline = Some(bb.clone());
        }
        let extension = json!({"schema":SCALE_SCHEMA,"protocol_sha256":hash(protocol_path)?,
            "parent_root":dir,"parent_manifest_sha256":sha(&mb),"parent_contract_file_sha256":sha(&cb),
            "parent_contract_sha256":m["contract_sha256"],"parent_population_sha256":sha(&pb),
            "original_rows":929,"added_train_rows":incoming.len(),"total_rows":baseline.len(),
            "original_partitions":originals,"original_numeric_values_and_targets_preserved":true,
            "extension_cfg_manifest_sha256":hash(&fragment.join("manifest.json"))?,"extension_population_sha256":sha(&audit_bytes),
            "extension_dataset_manifest_sha256":t["dataset_manifest_sha256"],"extension_embedding_bindings":evidence.pins,
            "selected_roots_manifest_sha256":protocol["selected_roots_manifest_sha256"],
            "generation_protocol_sha256":protocol["generation_protocol_sha256"],
            "normalization_fitted":false,"profiles_changed":false,"final_test_opened":false,
            "implementation_sha256":sha(include_bytes!("train_extension.rs"))});
        c["training_extension"] = extension.clone();
        c["experiment"]["schema"] = json!(SCALE_SCHEMA);
        c["experiment"]["protocol_sha256"] = json!(hash(protocol_path)?);
        // Names, offsets, units and frozen normalizations remain exact. The new
        // hash records the changed fitting population; only row binding changes.
        let ch = sha(&serde_json::to_vec(&c)?);
        let cb_out = serde_json::to_vec_pretty(&c)?;
        let mut files = BTreeMap::new();
        let mut bindings = Vec::new();
        for part in PARTS {
            let rs = partitions.get_mut(part).unwrap();
            for r in rs.iter_mut() {
                r["input"]["contract_sha256"] = json!(ch);
            }
            let b = jsonl(rs)?;
            let groups = rs
                .iter()
                .map(|r| s(r, "source_group"))
                .collect::<Result<BTreeSet<_>>>()?;
            bindings.push(
                json!({"split":part,"rows":rs.len(),"families":groups.len(),"sha256":sha(&b)}),
            );
            files.insert(format!("{part}.jsonl"), b);
        }
        pop["rows"]
            .as_array_mut()
            .context("parent population rows")?
            .extend(pop_rows.iter().cloned());
        for key in ["requested", "available"] {
            pop[key] = json!(baseline.len());
            m[key] = json!(baseline.len());
        }
        pop["training_extension"] = extension.clone();
        let population_union = json!({"schema":"slop_ninja_a_training_population_union_v1",
            "parent_dataset_manifest_sha256":m["dataset_manifest_sha256"],
            "extension_dataset_manifest_sha256":t["dataset_manifest_sha256"],
            "parent_numeric_manifest_sha256":sha(&mb),"extension_population_sha256":sha(&audit_bytes),
            "original_rows":929,"added_train_rows":incoming.len(),"available_rows":baseline.len(),
            "evaluation_and_reference_populations_unchanged":true,"final_test_opened":false});
        let union_bytes = serde_json::to_vec_pretty(&population_union)?;
        let union_hash = sha(&union_bytes);
        pop["dataset_manifest_sha256"] = json!(union_hash);
        m["dataset_manifest_sha256"] = json!(union_hash);
        m["dataset_manifest_file"] = json!("training-population.json");
        for (field, row_flag) in [
            ("grammar_exposed_rows", "exposed_to_grammar"),
            ("exposure_unspecified_rows", "exposure_was_unspecified"),
        ] {
            let count = pop["rows"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r[row_flag] == true)
                .count();
            pop[field] = json!(count);
        }
        pop["coverage"] = json!({"parent":pop["coverage"].clone(),"extension_requested":incoming.len(),"extension_available":incoming.len()});
        let pb_out = serde_json::to_vec_pretty(&pop)?;
        m["coverage"] = pop["coverage"].clone();
        m["training_extension"] = extension;
        m["experiment"] = c["experiment"].clone();
        m["splits"] = json!(bindings);
        m["contract_sha256"] = json!(ch);
        m["contract_file_sha256"] = json!(sha(&cb_out));
        m["population_file_sha256"] = json!(sha(&pb_out));
        m["baseline_origin_reference_sha256"] = json!(sha(&bb));
        files.insert("contract.json".into(), cb_out);
        files.insert("population.json".into(), pb_out);
        files.insert("training-population.json".into(), union_bytes);
        files.insert(
            "extension-dataset-manifest.json".into(),
            dataset_bytes.clone(),
        );
        files.insert("generation-protocol.md".into(), generation_bytes.clone());
        files.insert("fit-protocol.json".into(), fs::read(protocol_path)?);
        files.insert("baseline-origin-reference.jsonl".into(), bb);
        files.insert("manifest.json".into(), serde_json::to_vec_pretty(&m)?);
        bundles.push((mode, files));
    }
    fs::create_dir(out)?;
    for (mode, files) in bundles {
        let dir = out.join(mode);
        fs::create_dir(&dir)?;
        for (name, bytes) in files {
            put(&dir.join(name), &bytes)?;
        }
    }
    put(
        &out.join("manifest.json"),
        &serde_json::to_vec_pretty(&json!({"schema":SCALE_SCHEMA,
        "original_rows":929,"added_train_rows":incoming.len(),"protocol_sha256":hash(protocol_path)?,
        "normalization_fitted":false,"final_test_opened":false}))?,
    )
}

fn check_admission(
    protocol: &Value,
    dataset: &Value,
    generation_sha: &str,
    incoming: &[Value],
) -> Result<()> {
    ensure!(
        dataset["schema"] == "slop_ninja_a_supervised_dataset_v1"
            && dataset["complete"] == true
            && dataset["train_extension_only"] == true
            && dataset["test_text_opened"] == false
            && dataset["test_ids"] == json!([]),
        "unqualified assembly"
    );
    ensure!(
        s(dataset, "source_manifest_sha256")? == s(protocol, "selected_roots_manifest_sha256")?
            && generation_sha == s(protocol, "generation_protocol_sha256")?,
        "frozen roots/generation protocol differs"
    );
    let roots = protocol["requested_new_roots"]
        .as_u64()
        .context("requested roots")?;
    let tasks = protocol["requested_new_generations"]
        .as_u64()
        .context("requested tasks")?;
    ensure!(
        dataset["requested_human_records"] == roots
            && dataset["requested_generation_records"] == tasks
            && dataset["requested_records"] == roots + tasks
            && dataset["records"] == incoming.len(),
        "assembly count differs"
    );
    let allowed = dataset["non_test_ids"]
        .as_array()
        .context("admitted roster")?;
    let allowed_ids = allowed
        .iter()
        .map(|v| v.as_str().context("admitted ID").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    let actual = incoming
        .iter()
        .map(|r| s(r, "id").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        allowed_ids == actual && actual.len() == incoming.len() && allowed.len() == actual.len(),
        "admitted roster differs"
    );
    let assignments = dataset["split_assignments"]
        .as_object()
        .context("family assignments")?;
    ensure!(
        assignments.values().all(|v| v == "train")
            && incoming.iter().all(|r| r["frozen_split"] == "train"
                && r["purpose"] == "query"
                && r["source_group"]
                    .as_str()
                    .is_some_and(|g| assignments.get(g) == Some(&json!("train")))),
        "assembly scope differs"
    );
    let mut accepted = 0u64;
    let mut requested = 0u64;
    let mut failed = 0u64;
    let mut terminal_ids = actual.clone();
    for report in dataset["generation_reports"]
        .as_array()
        .context("generation reports")?
    {
        ensure!(report["complete"] == true, "unfinished generation");
        let a = report["accepted"].as_u64().context("accepted tasks")?;
        let n = report["requested"].as_u64().context("requested tasks")?;
        let f = report["failed"].as_u64().context("failed tasks")?;
        let failures = report["failures"].as_array().context("failure roster")?;
        ensure!(
            a + f == n && failures.len() as u64 == f,
            "unaccounted generation tasks"
        );
        for r in failures {
            ensure!(
                terminal_ids.insert(s(r, "id")?.into()),
                "duplicate failure ID"
            );
        }
        accepted += a;
        requested += n;
        failed += f;
    }
    let exclusions = dataset["excluded"].as_array().context("exclusion roster")?;
    for r in exclusions {
        ensure!(
            terminal_ids.insert(s(r, "id")?.into()),
            "duplicate exclusion ID"
        );
    }
    ensure!(
        requested == tasks
            && dataset["accepted_before_exclusions"] == roots + accepted
            && roots + accepted == incoming.len() as u64 + exclusions.len() as u64
            && roots + tasks == incoming.len() as u64 + exclusions.len() as u64 + failed,
        "incomplete assembly accounting"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admission_binds_roots_protocol_full_roster_and_terminal_accounting() {
        let protocol = json!({"selected_roots_manifest_sha256":"roots","generation_protocol_sha256":"generation",
            "requested_new_roots":1,"requested_new_generations":1});
        let incoming = vec![
            json!({"id":"human","source_group":"work","purpose":"query","frozen_split":"train"}),
        ];
        let d = json!({"schema":"slop_ninja_a_supervised_dataset_v1","complete":true,"train_extension_only":true,
            "test_text_opened":false,"test_ids":[],"source_manifest_sha256":"roots","requested_human_records":1,
            "requested_generation_records":1,"requested_records":2,"records":1,"non_test_ids":["human"],
            "split_assignments":{"work":"train"},"accepted_before_exclusions":2,"excluded":[{"id":"excluded"}],
            "generation_reports":[{"complete":true,"requested":1,"accepted":1,"failed":0,"failures":[]}]});
        check_admission(&protocol, &d, "generation", &incoming).unwrap();
        for (key, v) in [
            ("source_manifest_sha256", json!("changed")),
            ("non_test_ids", json!(["substituted"])),
            ("excluded", json!([])),
            ("excluded", json!([{"id":"human"}])),
        ] {
            let mut bad = d.clone();
            bad[key] = v;
            assert!(check_admission(&protocol, &bad, "generation", &incoming).is_err());
        }
        assert!(check_admission(&protocol, &d, "changed-generation", &incoming).is_err());
    }
    #[test]
    fn train_extension_rejects_family_identity_and_scope_reuse() {
        let old = vec![json!({"id":"old","source_group":"protected"})];
        let r = json!({"id":"new","source_group":"fresh","frozen_split":"train","purpose":"query"});
        disjoint_train(&old, &[r.clone()]).unwrap();
        for (key, v) in [
            ("id", "old"),
            ("source_group", "protected"),
            ("frozen_split", "test"),
            ("purpose", "reference"),
        ] {
            let mut bad = r.clone();
            bad[key] = json!(v);
            assert!(disjoint_train(&old, &[bad]).is_err());
        }
        assert!(disjoint_train(&old, &[r.clone(), r]).is_err());
    }
    #[test]
    fn frozen_join_preserves_targets_masks_and_modality() {
        let r = json!({"id":"synthetic","target":{"share":0.27,"human_identity":null},
            "input":{"contract_sha256":"cfg","x":vec![0.25;1820]}});
        let a = json!({"id":"synthetic","status":"ok","artifact_id":ARTIFACT,"text_sha256":"text","embedding":vec![0.5;768]});
        for mode in ["text", "combined"] {
            let joined = joined_row(&r, &a, "text", mode, "cfg").unwrap();
            assert_eq!(joined["target"], r["target"]);
            assert_eq!(
                joined["input"]["x"][768],
                json!(if mode == "text" { 0. } else { 0.25 })
            );
            assert_eq!(joined["input"]["x"][0], json!(0.5));
        }
        assert!(joined_row(&r, &a, "changed", "text", "cfg").is_err());
        assert!(joined_row(&r, &a, "text", "text", "changed").is_err());
    }
}

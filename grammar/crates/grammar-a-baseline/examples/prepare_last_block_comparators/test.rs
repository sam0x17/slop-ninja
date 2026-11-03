//! Apply the already sealed comparator artifacts to new Test numeric inputs.
use super::*;
fn sealed_file(sealed: &Value, p: &Path) -> Result<String> {
    let p = p.canonicalize()?;
    let h = sha(&p)?;
    ensure!(
        sealed["binding_files"]
            .as_array()
            .context("seal files")?
            .iter()
            .any(|v| v["path"] == p.to_str().unwrap() && v["sha256"] == h),
        "comparator file not sealed"
    );
    Ok(h)
}
pub(super) fn run(stage: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let root = stage.join("final-test-v1");
    let sp = root.join("seal.json");
    let sealed = seal::validate(&sp)?;
    let seal_hash = sha(&sp)?;
    let data = root.join("numeric-v1");
    let source = root.join("dataset-v1");
    let dm = read(&data.join("manifest.json"))?;
    let contract = read(&data.join("contract.json"))?;
    ensure!(
        dm["schema"] == "slop_ninja_last_block_test_dataset_v1"
            && dm["complete"] == true
            && dm["final_test_opened"] == true
            && dm["test_used_for_selection"] == false
            && dm["selection_receipt_sha256"] == seal_hash
            && dm["contract_sha256"] == sha(&data.join("contract.json"))?
            && dm["contract_sha256"]
                == sealed_file(&sealed, &stage.join("fit-bundle-v1/contract.json"))?
            && dm["source_dataset"]["sha256"] == sha(&source.join("manifest.json"))?,
        "frozen Test bundle required"
    );
    let pm = stage.join("fixed-comparators-v1/manifest.json");
    sealed_file(&sealed, &pm)?;
    let prior = read(&pm)?;
    let pc = stage.join("fixed-comparators-v1/prior-contract.json");
    sealed_file(&sealed, &pc)?;
    let init = Path::new(s(&prior, "v6_initializer_path")?);
    ensure!(
        sealed_file(&sealed, init)? == prior["v6_initializer_sha256"],
        "V6 initializer changed"
    );
    let initializer = read(init)?;
    ensure!(
        initializer["artifact_id"] == contract["parent_artifact_id"]
            && initializer["dimension"] == 768,
        "different original encoder"
    );
    let request_hash = sha(&source.join("requests.jsonl"))?;
    let prefixes = prefix::Prefixes::load(&root.join("prefixes-v1"), &request_hash)?;
    ensure!(
        prefixes.contract == contract["prefix_contract"],
        "Test prefix profile changed"
    );
    let mut models = Vec::new();
    let mut seeds = BTreeSet::new();
    for r in prior["fixed_prior_runs"]
        .as_array()
        .context("fixed prior runs")?
    {
        let p = Path::new(s(r, "weights_path")?);
        ensure!(
            sealed_file(&sealed, p)? == r["weights_sha256"],
            "prior weights changed"
        );
        let w = read(p)?;
        let seed = r["seed"].as_u64().context("seed")?;
        ensure!(
            seeds.insert(seed)
                && w["input_mode"] == "text"
                && w["control"] == "joint"
                && w["contract_sha256"] == r["contract_sha256"]
                && w["profiles"] == r["profiles"]
                && w["thresholds"] == r["thresholds"]
                && w["temperature"] == r["temperature"],
            "fixed prior identity"
        );
        models.push((seed, WarmSharedModel::from_json(&fs::read(p)?)?, w));
    }
    ensure!(seeds == BTreeSet::from([17, 29, 41]), "fixed prior seeds");
    let fitting = stage.join("fit-bundle-v1");
    let fm = read(&fitting.join("manifest.json"))?;
    let rp = fitting.join("reference.jsonl");
    ensure!(
        sha(&rp)? == fm["files_sha256"]["reference.jsonl"],
        "known references changed"
    );
    let known_refs = rows(&rp)?
        .into_iter()
        .filter(|r| r["frozen_split"] == "train")
        .collect::<Vec<_>>();
    let mut references = known_refs.clone();
    let mut roster = Vec::new();
    let mut parts = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut predictions = BTreeMap::<(u64, String), BTreeMap<String, Value>>::new();
    for part in ["test", "reference"] {
        let name = format!("{part}.jsonl");
        ensure!(
            sha(&data.join(&name))? == dm["files_sha256"][&name],
            "numeric partition changed"
        );
        let truth = rows(&data.join(name))?;
        for r in &truth {
            let id = s(r, "id")?;
            ensure!(
                r["frozen_split"] == "test"
                    && r["purpose"] == if part == "test" { "query" } else { "reference" },
                "Test scope"
            );
            ensure!(ids.insert(id.to_owned()), "duplicate query/reference ID");
            let (binding, h) = prefixes
                .get(id, s(r, "text_sha256")?)?
                .context("unsupported comparator prefix")?;
            ensure!(binding == r["prefix"], "different prefix from A evaluation");
            roster.push(json!({"id":id,"part":part,"text_sha256":r["text_sha256"],"prefix_sha256":binding["sha256"],"h_raw_f32":h}));
            for (seed, model, w) in &models {
                let mut x = h.clone();
                x.resize(2588, 0.);
                let input = SharedInput {
                    contract_sha256: s(w, "contract_sha256")?.into(),
                    x,
                };
                let mut pred = serde_json::to_value(model.predict(&input)?)?;
                pred["encoding"] = json!(model.encode(&input)?);
                predictions
                    .entry((*seed, part.into()))
                    .or_default()
                    .insert(id.into(), pred);
            }
        }
        parts.insert(
            part,
            json!({"requested":truth.len(),"supported":truth.len(),"unsupported":0,
            "truth_sha256":sha(&data.join(format!("{part}.jsonl")))?}),
        );
        if part == "reference" {
            references.extend(truth);
        }
    }
    prefixes.require_ids(&ids)?;
    let all_refs = references
        .iter()
        .map(|r| Ok(s(r, "id")?.to_owned()))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        all_refs.len() == references.len(),
        "duplicate known/Test reference"
    );
    for (seed, _, _) in &models {
        let n = format!("prior-{seed}-reference.json");
        let p = stage.join("fixed-comparators-v1").join(&n);
        ensure!(
            sha(&p)? == prior["files_sha256"][&n],
            "known reference prediction changed"
        );
        let old = read(&p)?;
        let destination = predictions.entry((*seed, "reference".into())).or_default();
        for r in &known_refs {
            let id = s(r, "id")?;
            let pred = old.get(id).context("missing known reference prediction")?;
            ensure!(
                destination.insert(id.into(), pred.clone()).is_none(),
                "reference ID collision"
            );
        }
        ensure!(
            destination.keys().cloned().collect::<BTreeSet<_>>() == all_refs,
            "reference support incomplete"
        );
    }
    fs::create_dir(out)?;
    fs::copy(&pc, out.join("prior-contract.json"))?;
    for ((seed, part), pred) in predictions {
        put(&out.join(format!("prior-{seed}-{part}.json")), &json!(pred))?;
    }
    jsonl(&out.join("references.jsonl"), &references)?;
    jsonl(&out.join("v6-numeric-requests.jsonl"), &roster)?;
    let files = fs::read_dir(out)?
        .map(|e| {
            let p = e?.path();
            Ok((
                p.file_name().unwrap().to_string_lossy().into_owned(),
                sha(&p)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    put(
        &out.join("manifest.json"),
        &json!({"schema":"slop_ninja_last_block_test_fixed_comparators_v1","complete":true,
        "dataset_manifest_sha256":sha(&data.join("manifest.json"))?,"current_contract_sha256":dm["contract_sha256"],
        "selection_receipt_sha256":seal_hash,"original_prepared_manifest_sha256":sha(&pm)?,
        "v6_initializer_path":init,"v6_initializer_sha256":prior["v6_initializer_sha256"],"fixed_prior_runs":prior["fixed_prior_runs"],
        "prefix_contract":prefixes.contract,"partitions":parts,"files_sha256":files,"final_test_opened":true,
        "fitting_or_calibration_performed":false,"test_used_for_selection":false,
        "reference_policy":"known Train references plus independent Test references; no gallery normalization refit",
        "implementation_sha256":hex::encode(Sha256::digest(include_bytes!("test.rs")))}),
    )?;
    println!(
        "{}",
        json!({"complete":true,"parts":parts,"fixed_prior_seeds":seeds})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn comparator_binding_rejects_changed_weights_even_at_same_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("synthetic-weights.json");
        fs::write(&path, b"{\"weight\":1}").unwrap();
        let sealed = json!({"binding_files":[{"path":path.canonicalize().unwrap(),"sha256":sha(&path).unwrap()}]});
        sealed_file(&sealed, &path).unwrap();
        fs::write(&path, b"{\"weight\":2}").unwrap();
        assert!(sealed_file(&sealed, &path).is_err());
    }
}

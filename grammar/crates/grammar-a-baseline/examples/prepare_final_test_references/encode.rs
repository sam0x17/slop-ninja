use super::*;

fn roster(rows: &[Value]) -> Result<BTreeSet<(String, String, String)>> {
    rows.iter()
        .map(|r| {
            Ok((
                str_at(r, "id")?.to_owned(),
                str_at(r, "source_group")?.to_owned(),
                str_at(&r["roles"]["human"], "identity")?.to_owned(),
            ))
        })
        .collect()
}
pub(super) fn numeric_refs(
    rows: &[Value],
    contract: &Value,
    part: &str,
    queries: &BTreeSet<String>,
    query_ids: &BTreeSet<String>,
) -> Result<()> {
    let profiles = array(contract, "human_profiles")?;
    ensure!(matches!(part, "train" | "test"), "gallery partition");
    let mut ids = BTreeSet::new();
    let mut identities = BTreeMap::<String, (String, usize)>::new();
    let mut groups = BTreeMap::<String, String>::new();
    for r in rows {
        let id = str_at(r, "id")?;
        let group = str_at(r, "source_group")?;
        let human = &r["roles"]["human"];
        let identity = str_at(human, "identity")?;
        let known = profiles.contains(&json!(identity));
        ensure!(
            ids.insert(id) && !query_ids.contains(id) && !queries.contains(group),
            "reference overlaps query population"
        );
        ensure!(
            r["purpose"] == "reference"
                && r["frozen_split"] == part
                && human["present"] == true
                && human["known"] == known
                && known == (part == "train")
                && r["roles"]["ai"]["present"] == false
                && r["roles"]["ai"]["identity"].is_null()
                && r["roles"]["ai"]["known"] == false,
            "gallery contributor/enrollment differs"
        );
        ensure!(
            r["targets"]
                == json!({"origin":null,"ai_involvement":null,"share":null,"human":null,"ai":null}),
            "reference has loss targets"
        );
        let entry = identities
            .entry(identity.to_owned())
            .or_insert((group.to_owned(), 0));
        ensure!(entry.0 == group, "reference writer uses unplanned work");
        entry.1 += 1;
        if let Some(old) = groups.insert(group.to_owned(), identity.to_owned()) {
            ensure!(old == identity, "reference work changes writer");
        }
    }
    ensure!(
        identities.values().all(|(_, n)| *n <= 2),
        "too many references per writer"
    );
    if part == "train" {
        ensure!(
            rows.len() == 12
                && identities.len() == 6
                && groups.len() == 6
                && identities.values().all(|(_, n)| *n == 2)
                && profiles.len() == 6,
            "exact six-writer/twelve-Train-reference gallery required"
        );
    }
    Ok(())
}

pub(super) fn encode_rows(
    model: &NumericSharedModel,
    rows: impl IntoIterator<Item = Value>,
) -> Result<Vec<Value>> {
    rows.into_iter()
        .map(|mut row| {
            let input: SharedInput = serde_json::from_value(row["input"].clone())?;
            let encoding = model.encode(&input)?;
            ensure!(
                !encoding.is_empty() && encoding.iter().all(|v| v.is_finite()),
                "invalid gallery encoding"
            );
            row["encoding"] = json!(encoding);
            Ok(row)
        })
        .collect()
}

pub(super) fn encode(
    selection: &Value,
    sb: &[u8],
    name: &str,
    seed: u64,
    dir: &Path,
    out: &Path,
) -> Result<()> {
    ensure!(!out.try_exists()?, "new gallery output required");
    let candidate = candidate(selection, name)?;
    let run = run_for(candidate, seed)?;
    let frozen = Path::new(str_at(candidate, "numeric_path")?);
    let (mb, m) = read(&dir.join("manifest.json"))?;
    let (cb, c) = read(&dir.join("contract.json"))?;
    ensure!(
        m["schema"] == "slop_ninja_final_test_reference_projection_v1"
            && m["candidate"] == name
            && m["selection_receipt_sha256"] == sha(sb)
            && m["frozen_manifest_sha256"] == candidate["numeric_manifest_sha256"]
            && m["contract_sha256"] == candidate["contract_sha256"]
            && m["contract_sha256"] == sha(&serde_json::to_vec(&c)?)
            && m["contract_file_sha256"] == sha(&cb)
            && candidate["contract_file_sha256"] == sha(&cb)
            && m["fit_performed"] == false
            && m["normalization_fitted"] == false
            && m["profiles_changed"] == false
            && m["test_used_for_selection"] == false,
        "reference projection/frozen contract mismatch"
    );
    let (fmb, fm) = read(&frozen.join("manifest.json"))?;
    ensure!(
        sha(&fmb) == candidate["numeric_manifest_sha256"],
        "frozen parent changed"
    );
    let known_bytes = fs::read(dir.join("reference.jsonl"))?;
    let extra_bytes = fs::read(dir.join("partition_references.jsonl"))?;
    ensure!(
        known_bytes == fs::read(frozen.join("reference.jsonl"))?
            && sha(&known_bytes) == m["known_reference_sha256"]
            && sha(&extra_bytes) == m["partition_reference_sha256"],
        "reference numeric bytes changed"
    );
    let binding = array(&fm, "splits")?
        .iter()
        .filter(|s| s["split"] == "reference")
        .collect::<Vec<_>>();
    ensure!(
        binding.len() == 1 && binding[0]["sha256"] == sha(&known_bytes),
        "known reference binding"
    );
    let (pb, pop) = read(&frozen.join("population.json"))?;
    ensure!(
        sha(&pb) == fm["population_file_sha256"],
        "frozen population changed"
    );
    let (qb, q) = read(&dir.join("query_dataset_manifest.json"))?;
    ensure!(
        sha(&qb) == m["query_dataset_manifest_sha256"] && q["selection_receipt_sha256"] == sha(sb),
        "query population binding differs"
    );
    let (rb, rm) = read(&dir.join("reference_dataset_manifest.json"))?;
    let raw = fs::read(dir.join("reference_records.jsonl"))?;
    ensure!(
        sha(&rb) == m["reference_dataset_manifest_sha256"]
            && sha(&raw) == m["reference_records_sha256"]
            && rm["records_sha256"] == sha(&raw)
            && rm["query_dataset_manifest_sha256"] == sha(&qb)
            && rm["selection_receipt_sha256"] == sha(sb),
        "reference admission snapshot changed"
    );
    let (raw_rows, mut query_groups) = admission::validate(selection, sb, &q, &raw)?;
    let dataset = Dataset::parse_final_references(&rb)?;
    dataset.require_exact_ids(raw_rows.keys().map(String::as_str))?;
    let profiles = Profiles::from_contract(&c)?;
    let (planned, _) = admission::roster(selection)?;
    let mut query_ids = ids(&q, "test_ids")?;
    for r in array(&pop, "rows")? {
        if r["purpose"] == "query" {
            query_ids.insert(str_at(r, "id")?.to_owned());
            query_groups.insert(str_at(r, "group")?.to_owned());
        }
    }
    let known = records(&known_bytes)?;
    let extra = records(&extra_bytes)?;
    ensure!(
        sha(&jsonl(extra.clone())?) == sha(&extra_bytes)
            && extra
                .windows(2)
                .all(|w| w[0]["id"].as_str() < w[1]["id"].as_str()),
        "canonical sorted additional numeric reference rows required"
    );
    numeric_refs(&known, &c, "train", &query_groups, &query_ids)?;
    numeric_refs(&extra, &c, "test", &query_groups, &query_ids)?;
    let baseline = super::candidate(selection, "cfg_leaf")?;
    let baseline_path = Path::new(str_at(baseline, "numeric_path")?);
    let (bmb, bm) = read(&baseline_path.join("manifest.json"))?;
    let bb = fs::read(baseline_path.join("reference.jsonl"))?;
    let b = array(&bm, "splits")?
        .iter()
        .find(|s| s["split"] == "reference")
        .context("baseline references")?;
    ensure!(
        sha(&bmb) == baseline["numeric_manifest_sha256"]
            && b["sha256"] == sha(&bb)
            && roster(&known)? == roster(&records(&bb)?)?,
        "candidate changed the twelve known Train references"
    );
    let (ab, audit) = read(&dir.join("reference_population.json"))?;
    ensure!(
        sha(&ab) == m["reference_population_sha256"],
        "reference audit changed"
    );
    let excluded = array(&m, "excluded")?;
    let population = array(&audit, "rows")?
        .iter()
        .map(|r| Ok((str_at(r, "id")?.to_owned(), r)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    ensure!(
        population.len() == 4
            && array(&audit, "rows")?.len() == 4
            && population.keys().eq(planned.keys())
            && audit["schema"] == "slop_ninja_final_test_reference_population_v1"
            && audit["candidate"] == name
            && audit["selection_receipt_sha256"] == sha(sb)
            && audit["frozen_manifest_sha256"] == candidate["numeric_manifest_sha256"]
            && audit["contract_sha256"] == candidate["contract_sha256"]
            && audit["reference_records_sha256"] == sha(&raw)
            && audit["reference_dataset_manifest_sha256"] == sha(&rb)
            && audit["query_dataset_manifest_sha256"] == sha(&qb)
            && audit["fit_performed"] == false
            && audit["final_test_opened"] == true,
        "reference population binding differs"
    );
    let supported_ids = extra
        .iter()
        .map(|r| str_at(r, "id"))
        .collect::<Result<BTreeSet<_>>>()?;
    for (id, r) in &raw_rows {
        let p = population[id];
        dataset.validate_metadata(id, r)?;
        ensure!(
            p["group"] == r["source_group"]
                && p["split"] == "test"
                && p["purpose"] == "reference"
                && p["metadata"] == supervision::without_text(r)
                && p["metadata_sha256"] == sha(&serde_json::to_vec(r)?)
                && p["source_sha256"] == r["text_sha256"]
                && p["available"] == supported_ids.contains(id.as_str()),
            "reference metadata/support join differs"
        );
    }
    let mut accounted = BTreeSet::new();
    for row in &extra {
        let id = str_at(row, "id")?;
        let p = planned.get(id).context("unplanned Test reference")?;
        let s = dataset.supervision(&raw_rows[id])?;
        ensure!(
            accounted.insert(id.to_owned())
                && row["source_group"] == p["underlying_work_key"]
                && row["roles"]["human"]["identity"] == p["author_key"]
                && row["roles"] == profiles.roles(&s)
                && row["targets"] == profiles.targets(&s)
                && row["exposed_to_grammar"] == s.exposed,
            "Test reference roster/labels differ"
        );
    }
    for e in excluded {
        let id = str_at(e, "id")?;
        ensure!(
            accounted.insert(id.to_owned()) && planned.contains_key(id),
            "duplicate/unplanned reference exclusion"
        );
        ensure!(
            e["source_group"] == raw_rows[id]["source_group"]
                && e["text_sha256"] == raw_rows[id]["text_sha256"]
                && !array(e, "reasons")?.is_empty(),
            "reference exclusion binding differs"
        );
        str_at(e, "reason")?;
    }
    ensure!(
        accounted == planned.keys().cloned().collect()
            && m["requested_test_references"] == 4
            && m["supported_test_references"] == extra.len()
            && m["known_reference_rows"] == 12
            && m["requested"] == 16
            && m["supported"] == 12 + extra.len()
            && audit["requested"] == 4
            && audit["available"] == extra.len()
            && m["complete"] == (extra.len() == 4)
            && audit["excluded"].as_array() == Some(excluded),
        "incomplete or inconsistent gallery accounting"
    );
    let fit = Path::new(str_at(run, "model_path")?);
    let (fb, f) = read(&fit.join("fit.json"))?;
    let (wb, w) = read(&fit.join("weights.json"))?;
    ensure!(
        sha(&fb) == run["fit_sha256"]
            && sha(&wb) == run["weights_sha256"]
            && f["seed"] == seed
            && f["control"] == "joint"
            && f["final_test_opened"] == false
            && f["data_manifest_sha256"] == candidate["numeric_manifest_sha256"]
            && w["contract_sha256"] == candidate["contract_sha256"]
            && w["profiles"]["human"] == c["human_profiles"]
            && w["profiles"]["ai"] == c["ai_profiles"],
        "frozen gallery encoder binding differs"
    );
    let model = NumericSharedModel::from_json(&wb)?;
    let output = encode_rows(&model, known.into_iter().chain(extra))?;
    let bundle = json!({"schema":"slop_ninja_final_test_reference_encodings_v1","candidate":name,"seed":seed,
        "selection_receipt_sha256":sha(sb),"contract_sha256":candidate["contract_sha256"],
        "weights_sha256":sha(&wb),"fit_sha256":sha(&fb),"frozen_manifest_sha256":candidate["numeric_manifest_sha256"],
        "known_reference_sha256":sha(&known_bytes),"partition_reference_sha256":sha(&extra_bytes),
        "partition_reference_manifest_sha256":sha(&mb),"reference_population_sha256":sha(&ab),
        "query_dataset_manifest_sha256":sha(&qb),"reference_records_sha256":m["reference_records_sha256"],
        "requested":16,"supported":output.len(),"reference_rows":output.len(),"complete":output.len()==16,
        "partition_counts":{"train":12,"test":output.len()-12},"excluded":excluded,
        "fit_performed":false,"normalization_fitted":false,"profiles_changed":false,"final_test_opened":true,
        "test_used_for_selection":false,"pooling":"existing arithmetic mean of reference encodings then L2 normalization",
        "rows":output,"implementation_sha256":sha(include_bytes!("encode.rs")),
        "scope":"retrieval only; Test-withheld head identities remain other_human; incomplete galleries are explicitly incomplete"});
    write_new(out, &pretty(&bundle)?)
}

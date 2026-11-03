use super::*;

// Read the frozen catalog plan only, never any acquisition text path.
pub(super) fn roster(selection: &Value) -> Result<(Rows, BTreeSet<String>)> {
    let pin = &selection["source_plan"];
    let bytes = fs::read(Path::new(str_at(pin, "path")?))?;
    ensure!(
        sha(&bytes) == digest_at(pin, "sha256")?,
        "frozen source plan changed"
    );
    let mut lines = std::str::from_utf8(&bytes)?.lines();
    let header = lines
        .next()
        .context("source plan header")?
        .split('\t')
        .collect::<Vec<_>>();
    let mut refs = Rows::new();
    let mut query_groups = BTreeSet::new();
    for line in lines {
        let cells = line.split('\t').collect::<Vec<_>>();
        ensure!(cells.len() == header.len(), "source plan width");
        let r = Value::Object(
            header
                .iter()
                .zip(cells)
                .map(|(k, v)| (k.to_string(), json!(v)))
                .collect(),
        );
        if r["split"] == "test" {
            ensure!(r["role"] == "query", "planned query role");
            query_groups.insert(str_at(&r, "underlying_work_key")?.to_owned());
        } else if r["split"] == "test_reference" {
            ensure!(
                r["role"] == "reference"
                    && r["enrollment"] == "withheld"
                    && r["author_partition"] == "test",
                "planned reference enrollment"
            );
            let pg = str_at(&r, "pg_id")?.parse::<u64>()?;
            for fraction in [20, 60] {
                ensure!(
                    refs.insert(format!("author-pg{pg}-p{fraction}"), r.clone())
                        .is_none(),
                    "duplicate planned reference"
                );
            }
        }
    }
    ensure!(
        refs.len() == 4 && query_groups.len() == 16,
        "frozen reference/query roster"
    );
    Ok((refs, query_groups))
}

pub(super) fn validate(
    selection: &Value,
    sb: &[u8],
    dataset: &Value,
    bytes: &[u8],
) -> Result<(Rows, BTreeSet<String>)> {
    ensure!(
        dataset["schema"] == "slop_ninja_a_supervised_dataset_v1"
            && dataset["complete"] == true
            && dataset["test_text_opened"] == true
            && dataset["test_used_for_selection"] == false
            && dataset["selection_receipt_sha256"] == sha(sb)
            && dataset["requested_reference_records"] == 4
            && dataset["separate_reference_records"] == 4
            && dataset["reference_exclusions"] == json!([])
            && dataset["references_sha256"] == sha(bytes),
        "complete four-reference assembler binding required"
    );
    let (planned, query_groups) = roster(selection)?;
    let rows = rows(bytes)?;
    ensure!(
        rows.len() == 4
            && rows.keys().eq(planned.keys())
            && ids(dataset, "reference_ids")? == rows.keys().cloned().collect()
            && ids(dataset, "test_ids")?.is_disjoint(&rows.keys().cloned().collect()),
        "reference/query ID roster differs"
    );
    let mut works = BTreeMap::<String, (String, usize)>::new();
    let mut hashes = BTreeSet::new();
    for (id, r) in &rows {
        let plan = &planned[id];
        let source = &r["source"]["record"];
        let group = str_at(r, "source_group")?;
        let human = str_at(r, "human_identity")?;
        ensure!(
            r["purpose"] == "reference"
                && r["frozen_split"] == "test"
                && r["origin"] == "human_only"
                && r["human_role_present"] == true
                && r["ai_role_present"] == false
                && r["ai_identity"].is_null()
                && r["parent_id"].is_null()
                && r.get("generation").is_none()
                && r["share_target"] == 0.0,
            "reference contributor/history scope differs"
        );
        ensure!(
            r["text_sha256"] == sha(str_at(r, "text")?.as_bytes())
                && hashes.insert(digest_at(r, "text_sha256")?.to_owned()),
            "changed/duplicate reference text"
        );
        ensure!(
            !query_groups.contains(group)
                && dataset["split_assignments"][group] == "test"
                && group == str_at(plan, "underlying_work_key")?
                && human == str_at(plan, "author_key")?
                && source["author_key"] == human
                && source["source_group"] == group
                && source["pg_id"].as_u64() == Some(str_at(plan, "pg_id")?.parse()?)
                && source["preassigned_partition"] == "test_reference"
                && source["purpose"] == "reference"
                && source["enrollment"] == "withheld"
                && source["author_partition"] == "test"
                && r["source"]["source_manifest_sha256"] == dataset["source_manifest_sha256"],
            "reference source/identity/partition differs"
        );
        ensure!(
            r["rights"] == source["rights"]
                && r["rights"]["external_evaluation"] == true
                && r["rights"]["commercial_training"] == true,
            "reference rights binding differs"
        );
        let entry = works
            .entry(group.to_owned())
            .or_insert((human.to_owned(), 0));
        ensure!(entry.0 == human, "reference work changes writer");
        entry.1 += 1;
    }
    ensure!(
        works.len() == 2
            && works.values().all(|(_, n)| *n == 2)
            && works
                .values()
                .map(|(a, _)| a)
                .collect::<BTreeSet<_>>()
                .len()
                == 2,
        "two independent withheld reference works required"
    );
    Ok((rows, query_groups))
}

pub(super) fn select(
    selection: &Value,
    sb: &[u8],
    dataset_path: &Path,
    refs: &Path,
    out: &Path,
) -> Result<()> {
    ensure!(!out.try_exists()?, "new reference output required");
    let (db, d) = read(dataset_path)?;
    let bytes = fs::read(refs)?;
    let (rows, query_groups) = validate(selection, sb, &d, &bytes)?;
    let requests = jsonl(
        rows.values()
            .map(|r| json!({"id":r["id"],"text":r["text"]})),
    )?;
    let request_manifest = json!({"schema":"slop_ninja_final_test_reference_requests_v1",
        "selection_receipt_sha256":sha(sb),"query_dataset_manifest_sha256":sha(&db),
        "reference_records_sha256":sha(&bytes),"requests_sha256":sha(&requests),
        "requested":4,"ids":rows.keys().collect::<Vec<_>>(),"label_free":true,
        "fit_performed":false,"test_used_for_selection":false,"purpose":"Test-withheld references only"});
    let assignments = rows
        .values()
        .map(|r| (str_at(r, "source_group").unwrap(), json!("test")))
        .collect::<BTreeMap<_, _>>();
    let manifest = json!({"schema":"slop_ninja_a_supervised_dataset_v1",
        "reference_transform_only":true,"final_test_reference_transform_only":true,"complete":true,
        "selection_receipt_sha256":sha(sb),"records_sha256":sha(&bytes),"records":4,"requested_records":4,
        "non_test_ids":[],"test_ids":rows.keys().collect::<Vec<_>>(),"split_assignments":assignments,
        "query_dataset_manifest_sha256":sha(&db),"query_source_families":query_groups,
        "requests_sha256":sha(&requests),
        "source_manifest_sha256":d["source_manifest_sha256"],"sources":[{"sha256":sha(&db),"kind":"final_Test_dataset"}],
        "test_text_opened":true,"test_used_for_selection":false,"fit_performed":false,
        "profile_enrollment":"withheld retrieval references only; no head/profile expansion"});
    publish(
        out,
        Files::from([
            ("records.jsonl".into(), bytes),
            ("manifest.json".into(), pretty(&manifest)?),
            ("requests.jsonl".into(), requests),
            ("request-manifest.json".into(), pretty(&request_manifest)?),
            ("query_dataset_manifest.json".into(), db),
            ("selection-receipt.json".into(), sb.to_vec()),
        ]),
    )
}

//! Validate a separate frozen Test reference bundle; pooling/scoring reuse the
//! evaluator's established helpers. No encoding or fitting happens here.
use super::*;
pub(super) fn load(
    path: &Path,
    numeric: &Path,
    a: &prediction_binding::Admitted,
) -> Result<(Gallery, Value)> {
    let bundle = read(path)?;
    ensure!(
        bundle["schema"] == "slop_ninja_final_test_reference_encodings_v1"
            && bundle["complete"] == true
            && bundle["selection_receipt_sha256"] == a.data.selection_sha
            && bundle["frozen_manifest_sha256"] == a.data.parent_sha
            && bundle["contract_sha256"] == a.data.contract_sha
            && bundle["weights_sha256"] == a.run["weights_sha256"]
            && bundle["fit_sha256"] == a.run["fit_sha256"]
            && bundle["fit_performed"] == false
            && bundle["normalization_fitted"] == false
            && bundle["profiles_changed"] == false
            && bundle["test_used_for_selection"] == false,
        "frozen Test gallery binding"
    );
    let parent = Path::new(b::text(&a.candidate, "numeric_path")?);
    let original = parent.join("reference.jsonl");
    ensure!(
        bundle["known_reference_sha256"] == b::bound(&a.selection, &original)?,
        "known references changed"
    );
    let known = b::indexed(&rows(&original)?)?;
    let source = read(&numeric.join("test_dataset_manifest.json"))?;
    ensure!(
        bundle["reference_records_sha256"] == source["references_sha256"]
            && bundle["query_dataset_manifest_sha256"] == a.data.dataset_sha
            && source["separate_reference_records"] == 4,
        "gallery Test reference source binding/incomplete extraction"
    );
    let plan = Path::new(b::text(&a.selection["source_plan"], "path")?);
    b::bound(&a.selection, plan)?;
    let tsv = fs::read_to_string(plan)?;
    let mut lines = tsv.lines();
    let header = lines
        .next()
        .context("source plan header")?
        .split('\t')
        .collect::<Vec<_>>();
    let mut reserved = BTreeMap::new();
    for line in lines {
        let fields = line.split('\t').collect::<Vec<_>>();
        ensure!(fields.len() == header.len(), "source plan width");
        let r = header
            .iter()
            .copied()
            .zip(fields)
            .collect::<BTreeMap<_, _>>();
        if r["split"] == "test_reference" {
            for fraction in [20, 60] {
                reserved.insert(
                    format!("author-pg{}-p{fraction}", r["pg_id"]),
                    (
                        r["underlying_work_key"].to_owned(),
                        r["author_key"].to_owned(),
                    ),
                );
            }
        }
    }
    ensure!(
        known.len() == 12 && reserved.len() == 4,
        "frozen 12+4 reference roster"
    );
    let accepted = b::array(&source, "reference_ids")?
        .iter()
        .map(|v| v.as_str().context("reference ID").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        accepted == reserved.keys().cloned().collect(),
        "assembled Test reference IDs differ from reservations"
    );
    let query_ids = a.data.rows.keys().cloned().collect::<BTreeSet<_>>();
    let query_groups = a
        .data
        .rows
        .values()
        .map(|r| b::text(r, "source_group").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    let dimension = b::count(&a.weights["architecture"], "embedding")?;
    let head = roster(&a.contract, "human")?;
    let mut seen = BTreeSet::new();
    let mut samples = BTreeMap::<String, Vec<Vec<f64>>>::new();
    let mut additional = Vec::new();
    for row in b::array(&bundle, "rows")? {
        let id = b::text(row, "id")?;
        let group = b::text(row, "source_group")?;
        ensure!(
            seen.insert(id.to_owned()) && !query_ids.contains(id) && !query_groups.contains(group),
            "duplicate/query-overlapping reference"
        );
        ensure!(
            row["purpose"] == "reference"
                && row["input"]["contract_sha256"] == a.data.contract_sha
                && row["targets"]
                    .as_object()
                    .is_some_and(|v| v.values().all(Value::is_null)),
            "reference contract/loss target"
        );
        let x = vector(&row["input"]["x"])?;
        ensure!(
            x.len() == b::count(&a.contract, "input_dimension")?,
            "reference numeric dimension"
        );
        let identity = b::text(&row["roles"]["human"], "identity")?;
        ensure!(
            row["roles"]["human"]["present"] == true && row["roles"]["ai"]["present"] == false,
            "reference contributor roles"
        );
        let mut bare = row.clone();
        bare.as_object_mut()
            .context("reference object")?
            .remove("encoding");
        if let Some(k) = known.get(id) {
            ensure!(
                bare == *k && row["frozen_split"] == "train",
                "known reference numeric row changed"
            );
        } else {
            let (family, writer) = reserved.get(id).context("unreserved Test reference")?;
            ensure!(
                group == family
                    && identity == writer
                    && row["frozen_split"] == "test"
                    && !head.contains(&identity.to_owned())
                    && row["roles"]["human"]["known"] == false,
                "wrong withheld reference family/writer/split"
            );
            if let Some(split) = a.contract["split_assignments"].get(group) {
                ensure!(
                    split == "test",
                    "Test reference overlaps frozen non-Test family"
                );
            }
            additional.push(bare);
        }
        let encoding = vector(&row["encoding"])?;
        ensure!(encoding.len() == dimension, "reference encoding dimension");
        samples
            .entry(identity.to_owned())
            .or_default()
            .push(encoding);
    }
    ensure!(
        seen == known.keys().chain(reserved.keys()).cloned().collect()
            && samples.len() == 8
            && samples.values().all(|v| v.len() == 2),
        "complete eight-writer/16-excerpt gallery required"
    );
    additional.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let mut bytes = Vec::new();
    for r in additional {
        serde_json::to_writer(&mut bytes, &r)?;
        bytes.push(b'\n');
    }
    ensure!(
        bundle["partition_reference_sha256"] == b::digest(&bytes),
        "Test reference numeric digest differs"
    );
    let mut human = BTreeMap::new();
    let mut zero = Vec::new();
    for (id, values) in samples {
        if let Some(p) = pool(&values)? {
            human.insert(id, p);
        } else {
            zero.push(id);
        }
    }
    let gallery = BTreeMap::from([("human".into(), human)]);
    Ok((
        gallery,
        json!({"schema":"slop_ninja_reference_normalized_mean_cosine_v1","bundle_sha256":sha(path)?,
        "pooling":"l2_normalize_arithmetic_mean_of_raw_encodings_equal_reference_row_weights","query":"l2_normalize_raw_encoding_then_cosine_rank",
        "reference_rows":16,"known_reference_rows":12,"withheld_Test_reference_rows":4,"zero_norm_profiles_excluded":zero,
        "evaluation_partition":"test","encoding_path":"same frozen NumericSharedModel::encode on queries and references",
        "reference_used_for_training_or_normalization":false,"classification_head_scores":false,"identity_authentication":false,
        "open_set_threshold_calibrated":false,"generator_reference_gallery_available":false}),
    ))
}

//! Strict transitive file bindings and complete support accounting; no prose files.
use super::*;

#[derive(Default)]
pub(super) struct Pins(BTreeMap<PathBuf, String>);
#[derive(Default)]
pub(super) struct ProviderEvidence {
    observations: Pred,
    support: BTreeMap<String, bool>,
}
impl ProviderEvidence {
    pub(super) fn supported(&mut self, id: &str, supported: bool) -> Result<()> {
        if let Some(old) = self.support.insert(id.into(), supported) {
            ensure!(
                old == supported,
                "provider support changes across candidate reports"
            );
        }
        Ok(())
    }
}
impl Pins {
    pub(super) fn add(&mut self, path: PathBuf, hash: &str) -> Result<()> {
        ensure!(
            hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()),
            "SHA256 required"
        );
        ensure!(
            b::sha(&path)? == hash,
            "input digest differs: {}",
            path.display()
        );
        let path = path.canonicalize()?;
        if let Some(old) = self.0.insert(path, hash.into()) {
            ensure!(old == hash, "conflicting file pins");
        }
        Ok(())
    }
    pub(super) fn file(&mut self, v: &Value) -> Result<PathBuf> {
        let p = PathBuf::from(b::text(v, "path")?);
        ensure!(p.is_absolute(), "PLAN paths must be absolute");
        self.add(p.clone(), b::text(v, "sha256")?)?;
        Ok(p)
    }
    pub(super) fn unchanged(&self) -> Result<()> {
        for (p, h) in &self.0 {
            ensure!(
                b::sha(p)? == *h,
                "input changed during summary: {}",
                p.display()
            );
        }
        Ok(())
    }
}

pub(super) fn roster(rows: &[Value]) -> Result<()> {
    let expected = final_test_binding::CANDIDATE_NAMES
        .into_iter()
        .flat_map(|n| SEEDS.map(|s| (n.to_owned(), s)))
        .collect::<BTreeSet<_>>();
    let mut actual = BTreeSet::new();
    for r in rows {
        ensure!(
            actual.insert((
                b::text(r, "candidate")?.to_owned(),
                b::count(r, "seed")? as u64
            )),
            "duplicate candidate/seed"
        );
    }
    ensure!(
        actual == expected,
        "all sixteen frozen candidates and all three seeds required"
    );
    Ok(())
}

pub(super) fn public_upstream(v: &Value) -> Result<Value> {
    let requested = b::count(v, "requested")?;
    let assembled = b::count(v, "assembled")?;
    let excluded = b::array(v, "excluded")?.len();
    let failed = b::array(v, "terminal_generation_failures")?.len();
    ensure!(
        v["accounted"] == true && requested == assembled + excluded + failed,
        "upstream reconciliation"
    );
    Ok(
        json!({"requested_queries":requested,"assembled_queries":assembled,
        "assembly_excluded":excluded,"terminal_generation_failures":failed,"accounted":true,
        "references_are_queries":false}),
    )
}

fn base(report: &Value, schema: &str, data: &b::TestData) -> Result<()> {
    ensure!(
        report["schema"] == schema
            && report["complete"] == true
            && report["split"] == "test"
            && report["test_used_for_selection"] == false
            && report["bindings"]["selection_receipt_sha256"] == data.selection_sha
            && report["bindings"]["numeric_manifest_sha256"] == data.manifest_sha
            && report["bindings"]["test_population_sha256"] == data.population_sha
            && report["bindings"]["test_dataset_manifest_sha256"] == data.dataset_sha
            && report["support"]["upstream"] == data.upstream
            && b::count(report, "requested")? == data.rows.len(),
        "report scope/parent/support binding"
    );
    Ok(())
}
fn model_binding(report: &Value, p: &b::Predictions, c: &Value, r: &Value) -> Result<()> {
    ensure!(
        report["candidate"] == c["candidate"]
            && report["seed"] == r["seed"]
            && report["bindings"]["prediction_receipt_sha256"] == p.receipt_sha
            && report["bindings"]["predictions_sha256"] == p.file_sha,
        "report candidate/prediction binding"
    );
    Ok(())
}
pub(super) fn evaluation(
    report: &Value,
    data: &b::TestData,
    p: &b::Predictions,
    c: &Value,
    r: &Value,
    fit: &Value,
) -> Result<()> {
    base(report, "slop_ninja_shared_a_frozen_test_report_v1", data)?;
    model_binding(report, p, c, r)?;
    ensure!(
        report["fit_performed"] == false
            && report["fit"] == *fit
            && report["bindings"]["query_sha256"] == data.query_sha
            && report["bindings"]["contract_sha256"] == data.contract_sha
            && report["bindings"]["weights_sha256"] == r["weights_sha256"]
            && report["bindings"]["fit_sha256"] == r["fit_sha256"]
            && b::count(report, "supported")? == p.values.len()
            && report["support"]["projection_excluded"] == json!(data.projection_excluded)
            && report["support"]["inference_excluded"] == json!(p.excluded)
            && report["support"]["numeric_supported"] == data.numeric.len()
            && report["support"]["model_supported"] == p.values.len()
            && report["support"]["all_inference_failed"]
                == (!data.numeric.is_empty() && p.values.is_empty()),
        "evaluation fit/support differs"
    );
    let rows = data
        .numeric
        .iter()
        .filter(|(id, _)| p.values.contains_key(*id))
        .map(|(_, r)| r.clone())
        .collect::<Vec<_>>();
    // Reuse the authoritative metric functions, including their support masks.
    metrics::equal(
        &report["model"],
        &supervised::origin_share_metrics(&rows, &p.values, None)?,
    )?;
    metrics::equal(
        &report["family_origin_balanced"],
        &supervised::balanced_origin_metrics(&rows, &p.values, None)?,
    )?;
    Ok(())
}

// Apply only the frozen knot interpolation, never PAV or a Test fit. The source
// comparison file and complete calibration description are selection-bound.
pub(super) fn mapped(cal: &Value, x: f64) -> Result<f64> {
    ensure!((0.0..=1.).contains(&x), "fraction outside unit range");
    let knots = b::array(cal, "knots")?;
    ensure!(!knots.is_empty(), "empty frozen knots");
    let mut prev = None;
    for k in knots {
        let a = number(&k["fraction"])?;
        let y = number(&k["involvement"])?;
        ensure!(
            (0.0..=1.).contains(&a) && (0.0..=1.).contains(&y),
            "knot range"
        );
        if let Some((px, py)) = prev {
            ensure!(a > px && y >= py, "nonmonotone frozen knots");
        }
        prev = Some((a, y));
    }
    if x <= number(&knots[0]["fraction"])? {
        return number(&knots[0]["involvement"]);
    }
    for pair in knots.windows(2) {
        let (a, z) = (number(&pair[0]["fraction"])?, number(&pair[1]["fraction"])?);
        if x <= z {
            let p = number(&pair[0]["involvement"])?;
            let q = number(&pair[1]["involvement"])?;
            return Ok(p + (q - p) * (x - a) / (z - a));
        }
    }
    number(&knots.last().unwrap()["involvement"])
}

#[allow(clippy::too_many_arguments)]
pub(super) fn pangram(
    report: &Value,
    data: &b::TestData,
    p: &b::Predictions,
    c: &Value,
    r: &Value,
    mapping: &Value,
    map_sha: &str,
    observations: &mut ProviderEvidence,
) -> Result<Pred> {
    base(report, "slop_ninja_frozen_test_pangram_comparison_v1", data)?;
    model_binding(report, p, c, r)?;
    ensure!(
        report["fitting_performed"] == false
            && report["mapping_applied_without_refit"] == true
            && report["native_pangram_confidence"] == false
            && report["bindings"]["frozen_mapping_sha256"] == map_sha
            && report["calibration"] == mapping["calibration"]
            && report["calibration_requested"] == mapping["calibration_requested"]
            && report["calibration_paired"] == mapping["calibration_paired"]
            && report["weights_sha256"] == r["weights_sha256"]
            && report["fit_sha256"] == r["fit_sha256"]
            && report["contract_sha256"] == data.contract_sha,
        "Pangram frozen map/weights differ"
    );
    let versions = b::array(mapping, "records")?
        .iter()
        .map(|r| {
            Ok((
                b::text(r, "selector")?.to_owned(),
                b::text(r, "response_version")?.to_owned(),
            ))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(versions.len() == 1, "frozen provider selector/version");
    let records = b::indexed(b::array(report, "records")?)?;
    let excluded = b::indexed(b::array(report, "excluded")?)?;
    ensure!(
        records.len() == b::count(report, "paired")?
            && records.len() + excluded.len() == data.rows.len()
            && records.keys().all(|k| !excluded.contains_key(k))
            && records
                .keys()
                .chain(excluded.keys())
                .all(|k| data.rows.contains_key(k)),
        "Pangram exact requested support"
    );
    let mut pg = Pred::new();
    for (id, v) in records {
        let row = &data.rows[&id];
        let model = p.values.get(&id).context("PG pair lacks model support")?;
        ensure!(
            v["source_group"] == row["source_group"]
                && v["text_sha256"] == data.hashes[&id]
                && v["origin_target"] == row["targets"]["origin"]
                && v["share_target"] == row["targets"]["share"]
                && versions.contains(&(
                    b::text(&v, "selector")?.into(),
                    b::text(&v, "response_version")?.into()
                )),
            "Pangram exact pair binding"
        );
        b::hash_text(&v, "observation_sha256")?;
        let fraction = number(&v["pangram_fraction"])?;
        let q = number(&v["mapped_pangram_involvement"])?;
        ensure!(
            (mapped(&mapping["calibration"], fraction)? - q).abs() < 1e-10
                && (number(&v["slop_ninja_involvement"])?
                    - number(&model["ai_confidence_percent"])? / 100.)
                    .abs()
                    < 1e-10,
            "Pangram changed mapping or paired model output"
        );
        let observed = warm::fusion::selected(
            &v,
            &[
                "source_group",
                "text_sha256",
                "origin_target",
                "share_target",
                "pangram_fraction",
                "mapped_pangram_involvement",
                "selector",
                "response_version",
                "observation_sha256",
            ],
        );
        observations.supported(&id, true)?;
        if let Some(old) = observations
            .observations
            .insert(id.clone(), observed.clone())
        {
            ensure!(old == observed, "Pangram observation changes across runs");
        }
        pg.insert(id, json!({"ai_confidence_percent":q*100.}));
    }
    let mut pg_failures = 0;
    for (id, e) in excluded {
        let reasons = b::array(&e, "reasons")?;
        ensure!(!reasons.is_empty(), "unexplained Pangram exclusion");
        let mut stages = BTreeSet::new();
        for reason in reasons {
            let stage = b::text(reason, "stage")?;
            ensure!(
                ["projection", "shared_model", "pangram"].contains(&stage) && stages.insert(stage),
                "unknown/repeated exclusion stage"
            );
        }
        ensure!(
            stages.contains("projection") == !data.numeric.contains_key(&id)
                && stages.contains("shared_model") == p.excluded.iter().any(|e| e["id"] == id),
            "PG/model failure accounting differs"
        );
        if stages.contains("pangram") {
            pg_failures += 1;
        }
        observations.supported(&id, !stages.contains("pangram"))?;
    }
    ensure!(
        report["support"]["pangram_supported"] == data.rows.len() - pg_failures
            && report["support"]["model_supported"] == p.values.len()
            && report["support"]["numeric_supported"] == data.numeric.len()
            && report["support"]["paired_documents"] == pg.len(),
        "Pangram support counts differ"
    );
    let ids = pg.keys().cloned().collect::<BTreeSet<_>>();
    let rows = metrics::rows(&data.rows, &ids);
    let model = ids
        .iter()
        .map(|id| (id.clone(), p.values[id].clone()))
        .collect::<Pred>();
    let truth = rows
        .iter()
        .map(|r| {
            Ok((
                b::text(r, "id")?.to_owned(),
                json!({"ai_confidence_percent":if r["targets"]["origin"]==0{0.}else{100.}}),
            ))
        })
        .collect::<Result<Pred>>()?;
    let pg_loss = difference(&rows, &pg, &truth, "involvement_brier")?;
    let model_loss = difference(&rows, &model, &truth, "involvement_brier")?;
    let gap = difference(&rows, &model, &pg, "involvement_brier")?;
    for (name, values) in [
        ("pangram_brier", &pg_loss),
        ("model_brier", &model_loss),
        ("model_minus_pangram_brier", &gap),
    ] {
        metrics::equal(&report["family"][name], &family_summary(values)["estimate"])?;
    }
    for (name, values) in [
        ("pangram_brier", &pg_loss),
        ("slop_ninja_brier", &model_loss),
        ("paired_gap", &gap),
    ] {
        metrics::equal(
            &report["family_origin_balanced"][name],
            &family_summary(&balance_origins(&rows, values)?)["estimate"],
        )?;
    }
    ensure!(
        report["support"]["paired_source_families"] == metrics::families(&rows)
            && report["support"]["paired_family_origin_cells"]
                == balance_origins(&rows, &gap)?.len(),
        "PG paired family/cell counts"
    );
    Ok(pg)
}

pub(super) fn v6(
    plan: &Value,
    selection: &Value,
    selection_sha: &str,
    canonical: &b::TestData,
    pins: &mut Pins,
) -> Result<(Pred, Value)> {
    let report = b::read(&pins.file(&plan["report"])?)?;
    let numeric = pins.file(&plan["numeric_manifest"])?;
    ensure!(
        numeric.file_name().is_some_and(|s| s == "manifest.json"),
        "V6 numeric manifest basename"
    );
    let root = numeric.parent().context("V6 numeric parent")?;
    let data = b::load_data(root, selection, selection_sha)?;
    for (path, hash) in [
        (root.join("test.jsonl"), data.query_sha.clone()),
        (
            root.join("contract.json"),
            b::hash_text(&data.manifest, "contract_file_sha256")?,
        ),
        (
            b::member(
                root,
                b::text(&data.manifest["test_transform"], "audit_file")?,
            )?,
            data.population_sha.clone(),
        ),
        (
            root.join("test_dataset_manifest.json"),
            data.dataset_sha.clone(),
        ),
    ] {
        pins.add(path, &hash)?;
    }
    ensure!(
        data.rows == canonical.rows
            && data.hashes == canonical.hashes
            && data.upstream == canonical.upstream,
        "V6 population differs"
    );
    base(&report, "slop_ninja_frozen_v6_test_origin_report_v1", &data)?;
    let frozen_path = pins.file(&plan["frozen_embedding_manifest"])?;
    let frozen_sha = b::bound(selection, &frozen_path)?;
    let frozen = b::read(&frozen_path)?;
    let manifest_path = pins.file(&plan["embedding_manifest"])?;
    let manifest = b::read(&manifest_path)?;
    let rows_path = pins.file(&plan["embedding_rows"])?;
    let rows = b::indexed(&b::jsonl(&rows_path)?)?;
    let art = "506b1c66a6a2ffc4eb4e50b9b057657ce0e5b6f04cff8caf3df496c956dc0d5f";
    ensure!(
        manifest["schema"] == "slop_ninja_v6_embeddings_run_v1"
            && manifest["status"] == "complete"
            && manifest["representation_contract"] == frozen["representation_contract"]
            && manifest["representation_contract_sha256"]
                == frozen["representation_contract_sha256"]
            && manifest["representation_contract"]["artifact_id"] == art
            && manifest["representation_contract"]["input"]["truncation"] == false
            && manifest["representation_contract"]["input"]["max_tokens_including_special_tokens"]
                == 2048
            && report["artifact_id"] == art
            && report["recalibrated"] == false
            && report["temperature"] == json!(1.8776105792643432_f64)
            && report["max_tokens_including_special_tokens"] == 2048
            && report["truncation"] == false
            && report["fitting_performed"] == false,
        "V6 identity/representation/calibration"
    );
    for (key, value) in [
        ("frozen_embedding_manifest_sha256", json!(frozen_sha)),
        (
            "embedding_manifest_sha256",
            plan["embedding_manifest"]["sha256"].clone(),
        ),
        (
            "embedding_rows_sha256",
            plan["embedding_rows"]["sha256"].clone(),
        ),
        (
            "representation_contract_sha256",
            manifest["representation_contract_sha256"].clone(),
        ),
        ("query_sha256", json!(data.query_sha)),
    ] {
        ensure!(report["bindings"][key] == value, "V6 report binding {key}");
    }
    let eb = &data.manifest["test_transform"]["bindings"]["embeddings"];
    ensure!(
        eb["manifest_sha256"] == plan["embedding_manifest"]["sha256"]
            && eb["rows_sha256"] == manifest["rows_sha256"]
            && manifest["rows_sha256"] == plan["embedding_rows"]["sha256"]
            && rows.keys().eq(data.rows.keys())
            && b::count(&manifest, "requested")? == rows.len()
            && b::count(&manifest, "processed")? == rows.len(),
        "V6 exact input roster"
    );
    let excluded = b::indexed(b::array(&report, "excluded")?)?;
    let mut pred = Pred::new();
    let mut unsupported = 0;
    let mut failed = 0;
    for (id, row) in rows {
        ensure!(
            row["text_sha256"] == data.hashes[&id]
                && row["artifact_id"] == art
                && row["representation_contract_sha256"]
                    == manifest["representation_contract_sha256"]
                && row["labels"] == json!(["human_only", "model_only", "mixed"])
                && row["runtime"] == "cpu_float32_eager"
                && row.get("truncated").is_none_or(|v| v == false),
            "V6 row identity"
        );
        let tokens = b::count(&row, "token_count")?;
        if row["status"] == "ok" {
            ensure!(
                !excluded.contains_key(&id) && (1..=2048).contains(&tokens),
                "V6 successful support"
            );
            let p = b::array(&row, "probabilities")?
                .iter()
                .map(number)
                .collect::<Result<Vec<_>>>()?;
            ensure!(
                p.len() == 3
                    && p.iter().all(|v| (0.0..=1.).contains(v))
                    && (p.iter().sum::<f64>() - 1.).abs() < 1e-7
                    && (number(&row["model_or_assistance_probability"])? - p[1] - p[2]).abs()
                        < 1e-7,
                "V6 probabilities"
            );
            pred.insert(id,json!({"origin":{"human_only":p[0],"model_only":p[1],"mixed":p[2]},"ai_confidence_percent":100.*(p[1]+p[2])}));
        } else {
            let e = excluded.get(&id).context("unaccounted V6 exclusion")?;
            for key in ["status", "reason", "token_count", "text_sha256"] {
                ensure!(e[key] == row[key], "V6 exclusion detail");
            }
            b::text(&row, "reason")?;
            ensure!(
                row["probabilities"].is_null()
                    && row["logits"].is_null()
                    && row["embedding"].is_null(),
                "failed V6 invented output"
            );
            match b::text(&row, "status")? {
                "unsupported_input" => {
                    unsupported += 1;
                    ensure!(tokens == 0 || tokens > 2048, "V6 unsupported boundary");
                }
                "error" | "failed" => failed += 1,
                _ => anyhow::bail!("V6 status"),
            }
        }
    }
    ensure!(
        b::count(&report, "supported")? == pred.len()
            && b::count(&report, "unsupported")? == unsupported
            && b::count(&report, "failed")? == failed
            && pred.len() + excluded.len() == data.rows.len()
            && excluded.len() == unsupported + failed
            && manifest["supported"] == pred.len(),
        "V6 failed/support counts"
    );
    ensure!(
        if manifest.get("failed").is_some() {
            manifest["failed"] == failed && manifest["unsupported"] == unsupported
        } else {
            manifest["unsupported"] == unsupported + failed
        },
        "V6 producer failure denominator"
    );
    for (key, origin) in [
        ("all_origins", None),
        ("human_model_only", Some(false)),
        ("mixed_only", Some(true)),
    ] {
        let rows = data
            .rows
            .iter()
            .filter(|(id, r)| {
                pred.contains_key(*id)
                    && origin.is_none_or(|mixed| (r["targets"]["origin"] == 2) == mixed)
            })
            .map(|(_, r)| {
                let mut r = r.clone();
                r["targets"]["share"] = Value::Null;
                r
            })
            .collect::<Vec<_>>();
        let mut m = supervised::origin_share_metrics(&rows, &pred, None)?;
        m.as_object_mut().unwrap().remove("share");
        metrics::equal(&report[key]["family"], &m)?;
        metrics::equal(
            &report[key]["family_origin_balanced"],
            &supervised::balanced_origin_metrics(&rows, &pred, None)?,
        )?;
    }
    Ok((pred, report))
}

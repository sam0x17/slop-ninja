//! Bind permanent Pangram fixtures, freeze a CAL-only involvement mapping and
//! compare all fixed candidates on exact Dev/CAL queries. No network or Test.
//! STAGE PREPARED FIXTURE_ROOT NEW_OUTPUT
#[allow(dead_code, unused_imports)]
#[path = "summarize_warm_start.rs"]
pub(crate) mod aggregate;
#[allow(dead_code, unused_imports)]
#[path = "evaluate_supervised.rs"]
pub(crate) mod evaluation;
#[allow(dead_code, unused_imports)]
#[path = "compare_supervised_pangram.rs"]
pub(crate) mod pangram;
use aggregate::fusion::{balance_origins, number, read, sha, text};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
type Scores = BTreeMap<String, f64>;
fn rows(p: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(p)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn put(p: &Path, v: &Value) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn labelled(row: &Value) -> Result<(String, String, usize, f64)> {
    let origin = row["targets"]["origin"].as_u64().context("origin target")? as usize;
    ensure!(
        origin < 3 && row["targets"]["ai_involvement"].as_bool() == Some(origin != 0),
        "label inconsistency"
    );
    Ok((
        text(row, "id")?.into(),
        text(row, "source_group")?.into(),
        origin,
        if origin == 0 { 0. } else { 1. },
    ))
}
pub(crate) fn samples(data: &[Value], scores: &Scores) -> Result<Vec<(String, f64)>> {
    ensure!(data.len() == scores.len(), "complete support required");
    data.iter()
        .map(|r| {
            let (id, g, _, y) = labelled(r)?;
            let p = *scores.get(&id).context("missing score")?;
            ensure!(
                p.is_finite() && (0.0..=1.).contains(&p),
                "probability range"
            );
            Ok((g, (p - y).powi(2)))
        })
        .collect()
}
fn balanced_summary(data: &[Value], v: &[(String, f64)]) -> Result<Value> {
    let cells = balance_origins(data, v)?;
    let mut result = aggregate::fusion::family_summary(&cells);
    result.as_object_mut().unwrap().remove("documents");
    result["documents"] = json!(data.len());
    result["family_origin_cells"] = json!(cells.len());
    Ok(result)
}
pub(crate) fn binary_metrics(data: &[Value], scores: &Scores) -> Result<Value> {
    let values = samples(data, scores)?;
    let mut fp = BTreeMap::new();
    for cut in [0.5, 0.9, 0.95, 0.99] {
        let mut v = Vec::new();
        for r in data {
            let (id, g, origin, _) = labelled(r)?;
            if origin == 0 {
                v.push((g, if scores[&id] >= cut { 1. } else { 0. }));
            }
        }
        fp.insert(cut.to_string(), evaluation::summary(&v));
    }
    let mut bins = vec![Vec::<(String, f64, f64)>::new(); 10];
    for r in data {
        let (id, g, _, y) = labelled(r)?;
        let p = scores[&id];
        bins[((p * 10.).floor() as usize).min(9)].push((g, p, y));
    }
    let calibration_bins=bins.iter().enumerate().map(|(i,bin)|{
        json!({"index":i,"documents":bin.len(),"mean_predicted":evaluation::summary(&bin.iter().map(|(g,p,_)|(g.clone(),*p)).collect()),
            "observed_involvement":evaluation::summary(&bin.iter().map(|(g,_,y)|(g.clone(),*y)).collect())})}).collect::<Vec<_>>();
    Ok(
        json!({"family_brier":evaluation::summary(&values),"family_origin_balanced_brier":balanced_summary(data,&values)?,
        "human_false_positive_rates":fp,"calibration_bins":calibration_bins,"rare_false_positive_guarantee":false}),
    )
}
fn predicted(path: &Path) -> Result<Scores> {
    let v = read(path)?;
    v.as_object()
        .context("prediction map")?
        .iter()
        .map(|(id, p)| Ok((id.clone(), number(&p["ai_confidence_percent"])? / 100.)))
        .collect()
}
fn run(stage: &Path, prepared: &Path, fixtures: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let data_root = stage.join("fit-bundle-v1");
    let dm = read(&data_root.join("manifest.json"))?;
    let pm = read(&prepared.join("manifest.json"))?;
    ensure!(
        dm["schema"] == "slop_ninja_last_block_a_dataset_v1"
            && dm["complete"] == true
            && dm["final_test_opened"] == false
            && pm["schema"] == "slop_ninja_last_block_fixed_comparators_v1"
            && pm["dataset_manifest_sha256"] == sha(&data_root.join("manifest.json"))?
            && pm["complete"] == true
            && pm["final_test_opened"] == false,
        "bound non-Test inputs required"
    );
    let mut partitions = BTreeMap::new();
    let mut scores = BTreeMap::new();
    let mut receipt_rows = Vec::new();
    let mut versions = BTreeSet::new();
    let mut groups = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut cal_pairs = Vec::new();
    for part in ["calibration", "development"] {
        let path = data_root.join(format!("{part}.jsonl"));
        ensure!(
            sha(&path)? == dm["files_sha256"][format!("{part}.jsonl")],
            "data changed"
        );
        let data = rows(&path)?;
        let hashes = data
            .iter()
            .map(|r| Ok((text(r, "id")?.into(), text(r, "text_sha256")?.into())))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mut raw = Scores::new();
        for r in &data {
            let (id, g, origin, y) = labelled(r)?;
            ensure!(
                r["frozen_split"] == part && r["purpose"] == "query" && ids.insert(id.clone()),
                "scope/duplicate"
            );
            if let Some(previous) = groups.insert(g.clone(), part) {
                ensure!(previous == part, "CAL/Dev family overlap");
            }
            let observation = pangram::observation(&fixtures.join(part), r, &hashes)?;
            let input_name = format!("pangram-{part}.jsonl");
            let manifest_name = format!("pangram-{part}-manifest.json");
            ensure!(
                observation["split"] == part
                    && observation["model"] == "pangram-4"
                    && observation["input_sha256"] == pm["files_sha256"][&input_name]
                    && observation["manifest_sha256"] == pm["files_sha256"][&manifest_name],
                "fixture request/split/selector binding"
            );
            let fraction = pangram::score(&observation);
            let version = text(&observation["response"], "version")?;
            versions.insert(version.to_owned());
            raw.insert(id.clone(), fraction);
            if part == "calibration" {
                cal_pairs.push((g, fraction, y, origin));
            }
            receipt_rows.push(json!({"id":id,"part":part,"text_sha256":r["text_sha256"],"version":version,
                "result_sha256":sha(&fixtures.join(part).join(text(r,"id")?).join("result.json"))?}));
        }
        scores.insert(part, raw);
        partitions.insert(part, data);
    }
    ensure!(
        versions.len() == 1,
        "mixed Pangram versions require an explicit protocol; do not pool silently"
    );
    let mapping = pangram::Isotonic::fit("calibration", &cal_pairs, true)?;
    fs::create_dir(out)?;
    put(
        &out.join("mapping.json"),
        &json!({"schema":"slop_ninja_last_block_pangram_mapping_v1","complete":true,
        "mapping":mapping.description(true),"selector":"pangram-4","versions":versions,"calibration_documents":cal_pairs.len(),
        "calibration_population_sha256":dm["files_sha256"]["calibration.jsonl"],"dataset_manifest_sha256":sha(&data_root.join("manifest.json"))?,
        "fitting_split":"calibration","development_used_for_fit":false,"final_test_opened":false}),
    )?;
    put(&out.join("fixture-bindings.json"), &json!(receipt_rows))?;
    let fixed = stage.join("fixed-comparator-evaluation-v1");
    let fm = read(&fixed.join("manifest.json"))?;
    ensure!(
        fm["schema"] == "slop_ninja_fixed_last_block_comparator_evaluation_v1"
            && fm["complete"] == true
            && fm["prepared_manifest_sha256"] == sha(&prepared.join("manifest.json"))?,
        "fixed comparator binding"
    );
    let mut reports = BTreeMap::new();
    for part in ["calibration", "development"] {
        let data = &partitions[part];
        let raw = &scores[part];
        let mapped = raw
            .iter()
            .map(|(id, p)| (id.clone(), mapping.predict(*p)))
            .collect::<Scores>();
        let pg_errors = samples(data, &mapped)?;
        let mut comparisons = BTreeMap::new();
        for candidate in ["adapt_last_block", "frozen_last_block", "prior_a", "v6"] {
            let mut runs = Vec::new();
            let mut differences = Vec::new();
            for seed in [17, 29, 41] {
                let path = if candidate == "prior_a" {
                    let name = format!("prior-{seed}-{part}.json");
                    let path = prepared.join(&name);
                    ensure!(
                        sha(&path)? == pm["files_sha256"][name],
                        "prior prediction changed"
                    );
                    path
                } else if candidate == "v6" {
                    let v6 = stage.join("fixed-v6-v1");
                    let vm = read(&v6.join("manifest.json"))?;
                    ensure!(
                        sha(&v6.join("manifest.json"))? == fm["v6_manifest_sha256"]
                            && vm["predictions_sha256"] == sha(&v6.join("predictions.json"))?,
                        "V6 binding changed"
                    );
                    // Recreate the bound scalar view directly, rather than trusting an unbound copy.
                    let rows = read(&v6.join("predictions.json"))?;
                    let s = rows
                        .as_array()
                        .context("V6 array")?
                        .iter()
                        .filter(|r| r["part"] == part)
                        .map(|r| {
                            Ok((
                                text(r, "id")?.into(),
                                number(&r["prediction"]["ai_confidence_percent"])? / 100.,
                            ))
                        })
                        .collect::<Result<Scores>>()?;
                    let errors = samples(data, &s)?;
                    differences.push(
                        errors
                            .iter()
                            .zip(&pg_errors)
                            .map(|((g, a), (_, b))| (g.clone(), a - b))
                            .collect::<Vec<_>>(),
                    );
                    runs.push(json!({"seed":null,"fixed_original":true,"metrics":binary_metrics(data,&s)?}));
                    continue;
                } else {
                    let root = stage
                        .join("evaluation-job-v1/results")
                        .join(format!("{candidate}-seed-{seed}"));
                    let m = read(&root.join("manifest.json"))?;
                    ensure!(
                        sha(&root.join("manifest.json"))?
                            == fm["current_evaluations"][format!("{candidate}-{seed}")],
                        "current evaluation changed"
                    );
                    let path = root.join(format!("{part}-predictions.json"));
                    ensure!(
                        sha(&path)? == m["reports"][part]["predictions_sha256"],
                        "current prediction changed"
                    );
                    path
                };
                let predictions = predicted(&path)?;
                let errors = samples(data, &predictions)?;
                differences.push(
                    errors
                        .iter()
                        .zip(&pg_errors)
                        .map(|((g, a), (_, b))| (g.clone(), a - b))
                        .collect::<Vec<_>>(),
                );
                runs.push(json!({"seed":seed,"metrics":binary_metrics(data,&predictions)?}));
            }
            let balanced = differences
                .iter()
                .map(|d| balance_origins(data, d))
                .collect::<Result<Vec<_>>>()?;
            let mut summary = aggregate::fixed_seed_summary(&balanced)?;
            let cells = summary
                .as_object_mut()
                .unwrap()
                .remove("documents")
                .unwrap();
            summary["family_origin_cells"] = cells;
            summary["documents"] = json!(data.len());
            if candidate == "v6" {
                runs.truncate(1);
            }
            comparisons.insert(candidate,json!({"runs":runs,"candidate_minus_mapped_pangram_brier":{
                "family":aggregate::fixed_seed_summary(&differences)?,"family_origin_balanced":summary},"negative_favors_candidate":true}));
        }
        let report = json!({"schema":"slop_ninja_last_block_pangram_comparison_v1","partition":part,"requested":data.len(),"supported":mapped.len(),"excluded":0,
            "mapped_pangram":binary_metrics(data,&mapped)?,"comparators":comparisons,"mapping_sha256":sha(&out.join("mapping.json"))?,
            "Pangram_fraction_is_not_native_involvement_confidence_or_share_ground_truth":true,"CAL_metrics_are_in_sample_mapping_diagnostics":part=="calibration",
            "origin_and_share_ground_truth":"retained production histories and measured replacement-span word counts; no detector labels",
            "three_way_origin_share_attribution":"not imputed for Pangram; all-output A evaluation is separate","final_test_opened":false,"model_reselected":false});
        put(&out.join(format!("{part}.json")), &report)?;
        put(
            &out.join(format!("{part}-scores.json")),
            &json!({"fraction":raw,"CAL_mapped_involvement":mapped}),
        )?;
        reports.insert(part, sha(&out.join(format!("{part}.json")))?);
    }
    put(
        &out.join("manifest.json"),
        &json!({"schema":"slop_ninja_last_block_pangram_evaluation_v1","complete":true,
        "dataset_manifest_sha256":sha(&data_root.join("manifest.json"))?,"prepared_manifest_sha256":sha(&prepared.join("manifest.json"))?,
        "fixed_comparator_manifest_sha256":sha(&fixed.join("manifest.json"))?,"mapping_sha256":sha(&out.join("mapping.json"))?,
        "fixture_bindings_sha256":sha(&out.join("fixture-bindings.json"))?,"reports":reports,"final_test_opened":false,"training_labels_from_detector":false,
        "implementation_sha256":hex::encode(Sha256::digest(include_bytes!("compare_last_block_pangram.rs")))}),
    )?;
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 4, "STAGE PREPARED FIXTURES NEW_OUT");
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
    )
}

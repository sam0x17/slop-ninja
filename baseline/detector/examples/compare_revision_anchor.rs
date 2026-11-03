//! Join validated Pangram observations to reused v5/v6 Test predictions.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde::Serialize;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{self, Split, sha256},
    evaluation_view,
    metrics::{self, OperatingPoint, ProbabilityRow},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const RECORDS_SHA: &str = "1b06538d11fc770fbf6c68b5b9cdb95f12463ebbc6f37e7d518ad8aa08859475";
const PLAN_SHA: &str = "5699a1348650f14d5d5747428c25bcc1760c76eb40651ddb139d789d63db6df3";
const PAYLOAD_SHA: &str = "405e6fe5dc3211529e3aea5a9810258c8f5a1e6f6b19201b21ab94828132ad1c";

#[derive(Parser)]
struct Args {
    #[arg(long)]
    subset_dir: PathBuf,
    #[arg(long)]
    plan_dir: PathBuf,
    #[arg(long)]
    collection_dir: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
}
#[derive(Default)]
struct Inputs(BTreeMap<PathBuf, String>);
impl Inputs {
    fn bytes(&mut self, path: &Path) -> Result<Vec<u8>> {
        let bytes = fs::read(path).with_context(|| format!("Read {}", path.display()))?;
        let hash = sha256(&bytes);
        if let Some(old) = self.0.insert(path.to_owned(), hash.clone()) {
            ensure!(old == hash, "Input changed");
        }
        Ok(bytes)
    }
    fn json(&mut self, path: &Path) -> Result<Value> {
        Ok(serde_json::from_slice(&self.bytes(path)?)?)
    }
    fn checked(&mut self, path: &Path, hash: &str) -> Result<Value> {
        let bytes = self.bytes(path)?;
        ensure!(
            sha256(&bytes) == hash,
            "Frozen input differs: {}",
            path.display()
        );
        Ok(serde_json::from_slice(&bytes)?)
    }
    fn finish(&self) -> Result<()> {
        for (p, h) in &self.0 {
            ensure!(sha256(fs::read(p)?) == *h, "Input changed: {}", p.display());
        }
        Ok(())
    }
}
fn field<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("Missing {k}"))
}
fn save(path: &Path, value: &Value) -> Result<()> {
    let mut f = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
struct Pangram {
    fraction_human: f64,
    fraction_ai: f64,
    fraction_ai_assisted: f64,
    combined_fraction: f64,
    version: String,
    echo_match: String,
}
fn observation(
    a: &Value,
    member: &Value,
    request: &Value,
    summary: &Value,
) -> Result<Option<Pangram>> {
    ensure!(
        a["schema"] == "slop_ninja_pangram_corpus_annotation_v1"
            && a["model_selector"] == "pangram-4"
            && a["member"] == *member
            && a["bulk_id"] == summary["bulk_id"],
        "Annotation identity differs"
    );
    let submitted = field(request, "text")?;
    ensure!(
        a["submitted_text_sha256"] == sha256(submitted)
            && member["text_sha256"] == a["submitted_text_sha256"],
        "Submitted text hash differs"
    );
    let item = &a["observation"];
    ensure!(
        item["id"] == request["id"] && item["index"] == member["item_index"],
        "Provider item identity differs"
    );
    if item["stage"] == "STAGE_FAILED" {
        ensure!(a["echo_match"] == "failed", "Failed echo state differs");
        return Ok(None);
    }
    ensure!(
        item["stage"] == "STAGE_SUCCESS" && !field(item, "task_id")?.is_empty(),
        "Unresolved or unidentified provider task"
    );
    let result = &item["result"];
    let returned = field(result, "text")?;
    ensure!(
        a["returned_text_sha256"] == sha256(returned),
        "Returned text hash differs"
    );
    let whitespace = summary["whitespace_echo_allowed"] == true;
    let soft_hyphen = summary["source_soft_hyphen_removal_allowed"] == true;
    let echo = if returned == submitted {
        "exact"
    } else if whitespace && returned.split_whitespace().eq(submitted.split_whitespace()) {
        "whitespace_only"
    } else {
        let stripped = submitted.replace('\u{00ad}', "");
        ensure!(
            soft_hyphen
                && submitted.contains('\u{00ad}')
                && (returned == stripped
                    || (whitespace && returned.split_whitespace().eq(stripped.split_whitespace()))),
            "Provider changed substantive input text"
        );
        "source_soft_hyphens_removed"
    };
    ensure!(a["echo_match"] == echo, "Echo classification differs");
    let fractions = ["fraction_human", "fraction_ai", "fraction_ai_assisted"]
        .map(|k| result[k].as_f64().context("Missing content fraction"));
    let [human, ai, assisted] = fractions;
    let (human, ai, assisted) = (human?, ai?, assisted?);
    ensure!(
        [human, ai, assisted]
            .iter()
            .all(|p| p.is_finite() && (0.0..=1.0).contains(p))
            && (human + ai + assisted - 1.0).abs() < 1e-5,
        "Invalid content fractions"
    );
    let version = field(result, "version")?;
    ensure!(!version.is_empty(), "Missing provider version");
    Ok(Some(Pangram {
        fraction_human: human,
        fraction_ai: ai,
        fraction_ai_assisted: assisted,
        combined_fraction: ai + assisted,
        version: version.into(),
        echo_match: echo.into(),
    }))
}

#[derive(Clone, Serialize)]
struct Joined {
    id: String,
    text_sha256: String,
    group: String,
    origin: String,
    collection: String,
    probabilities: [[f64; 3]; 2],
    pangram: Option<Pangram>,
}
fn bin(f: f64) -> &'static str {
    if (1.0 - f).abs() <= 1e-5 {
        "100"
    } else if f < 0.6 {
        "below_60"
    } else if f < 0.7 {
        "60_70"
    } else if f < 0.8 {
        "70_80"
    } else if f < 0.9 {
        "80_90"
    } else {
        "90_below_100"
    }
}
fn rate(events: usize, rows: usize) -> Value {
    json!({"events":events,"rows":rows,"rate":(rows>0).then(||events as f64/rows as f64)})
}
fn stats(rows: &[&Joined], points: &[Vec<OperatingPoint>; 2]) -> Value {
    let success: Vec<_> = rows
        .iter()
        .filter_map(|r| r.pangram.as_ref().map(|p| (*r, p)))
        .collect();
    let mut bins = BTreeMap::from([
        ("below_60", 0),
        ("60_70", 0),
        ("70_80", 0),
        ("80_90", 0),
        ("90_below_100", 0),
        ("100", 0),
    ]);
    for (_, p) in &success {
        *bins.get_mut(bin(p.combined_fraction)).unwrap() += 1;
    }
    let pangram_flags = success
        .iter()
        .filter(|(_, p)| p.combined_fraction >= 0.10)
        .count();
    let mut detector_stats = BTreeMap::new();
    for (d, name) in ["v5", "v6"].into_iter().enumerate() {
        let summaries:Vec<_>=points[d].iter().map(|point|{
            let flag=|r:&Joined|r.probabilities[d][1]+r.probabilities[d][2]>point.threshold;
            let mut paired=[0usize;4];
            for (r,p) in &success {match (p.combined_fraction>=0.10,flag(r)) {(true,true)=>paired[0]+=1,(true,false)=>paired[1]+=1,(false,true)=>paired[2]+=1,(false,false)=>paired[3]+=1}}
            json!({"calibration":point,"flags_on_all_rows":rate(rows.iter().filter(|r|flag(r)).count(),rows.len()),"paired_with_pangram":{"rows":success.len(),"both":paired[0],"pangram_only":paired[1],"detector_only":paired[2],"neither":paired[3]}})
        }).collect();
        detector_stats.insert(name, summaries);
    }
    let means = if success.is_empty() {
        Value::Null
    } else {
        let n = success.len() as f64;
        json!({"human":success.iter().map(|(_,p)|p.fraction_human).sum::<f64>()/n,"ai":success.iter().map(|(_,p)|p.fraction_ai).sum::<f64>()/n,"ai_assisted":success.iter().map(|(_,p)|p.fraction_ai_assisted).sum::<f64>()/n,"combined":success.iter().map(|(_,p)|p.combined_fraction).sum::<f64>()/n})
    };
    json!({"rows":rows.len(),"source_groups":rows.iter().map(|r|&r.group).collect::<BTreeSet<_>>().len(),"pangram_successful":success.len(),"pangram_failed":rows.len()-success.len(),"pangram_flags_at_10_percent":rate(pangram_flags,success.len()),"pangram_fraction_bins":bins,"mean_pangram_fractions":means,"detectors":detector_stats})
}
fn by_origin(rows: &[&Joined], points: &[Vec<OperatingPoint>; 2]) -> Value {
    let mut result = BTreeMap::new();
    for origin in ["human_only", "model_only", "mixed"] {
        result.insert(
            origin,
            stats(
                &rows
                    .iter()
                    .copied()
                    .filter(|r| r.origin == origin)
                    .collect::<Vec<_>>(),
                points,
            ),
        );
    }
    json!(result)
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new output directory");
    let mut inputs = Inputs::default();
    let plan = inputs.checked(&args.plan_dir.join("plan.json"), PLAN_SHA)?;
    let payload = inputs.checked(&args.plan_dir.join("requests/batch-0000.json"), PAYLOAD_SHA)?;
    let corpus_path = args.subset_dir.join("records.jsonl");
    ensure!(
        sha256(inputs.bytes(&corpus_path)?) == RECORDS_SHA,
        "Subset corpus differs"
    );
    let records = dataset::read_records(&corpus_path)?;
    ensure!(
        records.len() == 162 && records.iter().all(|r| r.split == Some(Split::Test)),
        "Unexpected subset corpus"
    );
    let index: BTreeMap<_, _> = records.iter().map(|r| (r.id.as_str(), r)).collect();
    let cohort: BTreeSet<_> = records.iter().map(|r| r.text_sha256.as_str()).collect();
    ensure!(
        cohort.len() == 162,
        "This frozen sample requires unique exact text hashes"
    );
    let job = args
        .collection_dir
        .parent()
        .context("Missing job directory")?;
    let binding = inputs.json(&job.join("binding.json"))?;
    ensure!(
        binding["plan_sha256"] == PLAN_SHA
            && binding["payload_sha256"] == PAYLOAD_SHA
            && binding["batch_index"] == 0
            && binding["runner_sha256"]
                == "ce24b15a8f080202097baa7e8d7c17cd2fb7a2fcda3c815042088bd462fef487",
        "Provider job binding differs"
    );
    inputs.checked(&job.join("plan.json"), PLAN_SHA)?;
    inputs.checked(&job.join("request.json"), PAYLOAD_SHA)?;
    let collection = inputs.json(&args.collection_dir.join("summary.json"))?;
    ensure!(
        collection["schema"] == "slop_ninja_pangram_corpus_collection_v1"
            && collection["status"] == "complete"
            && collection["items"] == 162,
        "Pangram collection is incomplete"
    );
    let annotations_path = args.collection_dir.join("annotations.jsonl");
    let bytes = inputs.bytes(&annotations_path)?;
    ensure!(
        collection["annotations_sha256"] == sha256(&bytes),
        "Annotation archive differs"
    );
    let members = plan["members"].as_array().context("Missing plan members")?;
    let requests = payload["items"]
        .as_array()
        .context("Missing request items")?;
    ensure!(
        members.len() == 162
            && requests.len() == 162
            && plan["repeats"] == 1
            && plan["input_sha256"] == RECORDS_SHA,
        "Plan coverage differs"
    );
    let mut pangram = BTreeMap::new();
    let mut failures = Vec::new();
    let mut versions = BTreeSet::new();
    for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        let a: Value = serde_json::from_slice(line)?;
        let n = a["member"]["item_index"]
            .as_u64()
            .context("Missing item index")? as usize;
        let member = members.get(n).context("Unexpected item index")?;
        let request = requests.get(n).context("Unexpected request index")?;
        let hash = field(&a, "submitted_text_sha256")?;
        ensure!(
            cohort.contains(hash),
            "Annotation text is outside the frozen sample"
        );
        let member_records = member["records"]
            .as_array()
            .context("Missing origin records")?;
        ensure!(
            member_records.len() == 1,
            "This sample has one record per text"
        );
        let reference = &member_records[0];
        let record = index
            .get(field(reference, "record_id")?)
            .context("Unknown origin record")?;
        ensure!(
            record.text_sha256 == hash
                && reference["source_group"] == record.source_group
                && reference["origin"] == json!(record.origin)
                && reference["split"] == "test",
            "Origin metadata differs"
        );
        let parsed = observation(&a, member, request, &collection)?;
        if let Some(p) = &parsed {
            versions.insert(p.version.clone());
        } else {
            failures.push(a.clone());
        }
        ensure!(
            pangram.insert(hash.to_owned(), parsed).is_none(),
            "Duplicate annotation"
        );
    }
    ensure!(
        pangram.len() == 162
            && collection["failed"] == failures.len()
            && collection["succeeded"] == 162 - failures.len(),
        "Annotation result coverage differs"
    );
    let provenance = inputs.json(&args.subset_dir.join("input-bindings.json"))?;
    ensure!(
        provenance["model_calls"] == 0
            && provenance["pangram_calls"] == 0
            && provenance["membership_sha256"]
                == "808b495cde2d4a6c41b2ef0f6c353eb19d3fe8d0b710b641bfceb3412872f8f1",
        "Unexpected subset derivation"
    );
    let mut detector_bindings: [Option<Value>; 2] = [None, None];
    let mut points: [Vec<OperatingPoint>; 2] = [vec![], vec![]];
    let mut exports = BTreeMap::<String, Vec<(String, Joined)>>::new();
    let mut view_summaries = BTreeMap::new();
    for route in ["primary", "phi4"] {
        for stage in ["R2", "R0", "R1"] {
            let key = format!("{route}-{stage}");
            let view_path = args.subset_dir.join(format!("views/{key}.json"));
            inputs.bytes(&view_path)?;
            let view = evaluation_view::load_view(&view_path, &corpus_path, Split::Test)?;
            ensure!(
                view.view.families.len() == 18 && view.selected.len() == 54,
                "Subset view coverage differs"
            );
            let expected: BTreeMap<_, _> =
                view.selected.iter().map(|r| (r.id.as_str(), r)).collect();
            let mut predictions: [BTreeMap<String, [f64; 3]>; 2] =
                [BTreeMap::new(), BTreeMap::new()];
            for (d, name) in ["v5", "v6"].into_iter().enumerate() {
                let report =
                    inputs.json(&args.subset_dir.join(format!("reports/{key}-{name}.json")))?;
                ensure!(
                    report["schema"] == "slop_ninja_frozen_encoder_evaluation_v1"
                        && report["records_sha256"] == RECORDS_SHA
                        && report["split"] == "test"
                        && report["final_test_opened"] == true
                        && report["evaluation_view"] == json!(view.binding())
                        && report["derivation"]["mode"] == "frozen_prediction_subset_v1"
                        && report["derivation"]["additional_inference"] == false,
                    "Subset report binding differs"
                );
                let declared: Vec<OperatingPoint> = report["report"]["operating_points"]
                    .as_array()
                    .context("Missing operating points")?
                    .iter()
                    .map(|p| serde_json::from_value(p["calibration"].clone()))
                    .collect::<serde_json::Result<_>>()?;
                ensure!(
                    declared.len() == 2
                        && report["artifact_id"] == report["report"]["artifact_sha256"],
                    "Detector metadata differs"
                );
                for (point, rate) in declared.iter().zip([0.01, 0.05]) {
                    point.validate()?;
                    ensure!(
                        point.target_human_false_positive_rate == rate,
                        "Unexpected target FPR"
                    );
                }
                let frozen = json!({"artifact_id":report["artifact_id"],"thresholds_sha256":report["thresholds_sha256"],"points":declared});
                if let Some(previous) = &detector_bindings[d] {
                    ensure!(*previous == frozen, "Detector changed across stage reports");
                } else {
                    detector_bindings[d] = Some(frozen);
                    points[d] = declared;
                }
                let rows: Vec<ProbabilityRow> =
                    serde_json::from_value(report["report"]["predictions"].clone())?;
                metrics::summarize(&rows)?;
                for r in rows {
                    let record = expected
                        .get(r.id.as_str())
                        .context("Unexpected subset prediction")?;
                    ensure!(
                        r.group == record.source_group
                            && r.label == record.origin.index()
                            && predictions[d].insert(r.id, r.probabilities).is_none(),
                        "Prediction group, label or uniqueness differs"
                    );
                }
                ensure!(predictions[d].len() == 54, "Missing subset prediction");
            }
            let mut joined = Vec::new();
            for record in &view.selected {
                let row = Joined {
                    id: record.id.clone(),
                    text_sha256: record.text_sha256.clone(),
                    group: record.source_group.clone(),
                    origin: serde_json::to_value(record.origin)?
                        .as_str()
                        .unwrap()
                        .into(),
                    collection: record.source.collection.clone(),
                    probabilities: [predictions[0][&record.id], predictions[1][&record.id]],
                    pangram: pangram[&record.text_sha256].clone(),
                };
                exports
                    .entry(row.id.clone())
                    .or_default()
                    .push((key.clone(), row.clone()));
                joined.push(row);
            }
            let rows: Vec<_> = joined.iter().collect();
            let families: BTreeMap<_, _> = view
                .view
                .families
                .iter()
                .map(|f| (f.source_group.as_str(), f))
                .collect();
            let mut slices = BTreeMap::<String, Vec<&Joined>>::new();
            for r in &joined {
                let f = families[r.group.as_str()];
                for label in [
                    format!("collection:{}", r.collection),
                    format!("original_composer:{}", f.chain.composer.model_revision),
                    format!(
                        "initial_profile:{}",
                        f.chain
                            .initial_profile_id
                            .as_deref()
                            .unwrap_or("legacy-unprofiled")
                    ),
                ] {
                    slices.entry(label).or_default().push(r);
                }
            }
            view_summaries.insert(key,json!({"all":stats(&rows,&points),"by_origin":by_origin(&rows,&points),"slices":slices.iter().map(|(k,v)|(k,json!({"all":stats(v,&points),"by_origin":by_origin(v,&points)}))).collect::<BTreeMap<_,_>>()}));
        }
    }
    ensure!(
        detector_bindings[0].as_ref().unwrap()["artifact_id"]
            == "da3a604d7bd1708f5ba1c4a4a4be946d77670492b702e41e267fb86237dfb8c9"
            && detector_bindings[0].as_ref().unwrap()["artifact_id"]
                != detector_bindings[1].as_ref().unwrap()["artifact_id"],
        "Unexpected detector pair"
    );
    ensure!(
        exports.len() == 162,
        "Incomplete unique-text prediction coverage"
    );
    let mut consistency = BTreeMap::new();
    for (id, versions) in &exports {
        let canonical = &versions[0].1;
        let expected = match canonical.origin.as_str() {
            "human_only" => 6,
            "mixed" => 3,
            "model_only" => 1,
            _ => anyhow::bail!("Unknown origin"),
        };
        ensure!(
            versions.len() == expected,
            "Unexpected repeated-export count"
        );
        let mut max_delta = [0.0_f64; 2];
        let mut disagreements = [[0usize; 2]; 2];
        for (_, r) in versions.iter().skip(1) {
            for d in 0..2 {
                for c in 0..3 {
                    max_delta[d] = max_delta[d]
                        .max((r.probabilities[d][c] - canonical.probabilities[d][c]).abs());
                }
                for (p, point) in points[d].iter().enumerate() {
                    let flag = |r: &Joined| {
                        r.probabilities[d][1] + r.probabilities[d][2] > point.threshold
                    };
                    disagreements[d][p] += usize::from(flag(r) != flag(canonical));
                }
            }
        }
        consistency.insert(id,json!({"canonical_view":versions[0].0,"export_count":versions.len(),"max_absolute_probability_delta_v5_v6":max_delta,"flag_disagreements_v5_v6":disagreements}));
    }
    let mut consistency_summary = json!({"canonical_policy":"First primary R2 export for shared humans; the route's R2 export for its direct human edit; the corresponding stage for each distinct model draft/revision.","texts_with_probability_variation":[0,0],"max_absolute_probability_delta":[0.0,0.0],"flag_disagreements":[[0,0],[0,0]]});
    for diagnostic in consistency.values() {
        for d in 0..2 {
            let delta = diagnostic["max_absolute_probability_delta_v5_v6"][d]
                .as_f64()
                .unwrap();
            consistency_summary["texts_with_probability_variation"][d] = json!(
                consistency_summary["texts_with_probability_variation"][d]
                    .as_u64()
                    .unwrap()
                    + u64::from(delta > 0.0)
            );
            consistency_summary["max_absolute_probability_delta"][d] = json!(
                consistency_summary["max_absolute_probability_delta"][d]
                    .as_f64()
                    .unwrap()
                    .max(delta)
            );
            for p in 0..2 {
                consistency_summary["flag_disagreements"][d][p] = json!(
                    consistency_summary["flag_disagreements"][d][p]
                        .as_u64()
                        .unwrap()
                        + diagnostic["flag_disagreements_v5_v6"][d][p]
                            .as_u64()
                            .unwrap()
                );
            }
        }
    }
    let unique: Vec<_> = exports.values().map(|v| &v[0].1).collect();
    inputs.finish()?;
    fs::create_dir(&args.output_dir)?;
    save(
        &args.output_dir.join("per-record.json"),
        &json!({"exports":exports,"consistency":consistency,"failed_annotations":failures}),
    )?;
    save(
        &args.output_dir.join("summary.json"),
        &json!({"schema":"slop_ninja_pangram_revision_comparison_v1","records_sha256":RECORDS_SHA,"plan_sha256":PLAN_SHA,"annotations_sha256":sha256(&bytes),"returned_versions":versions,"detector_bindings":detector_bindings,"unique_texts":{"all":stats(&unique,&points),"by_origin":by_origin(&unique,&points)},"views":view_summaries,"repeated_export_consistency":consistency_summary,"limits":"Content fractions and document-origin probabilities have different meanings. Pangram comparisons use successful observations only, with failed denominators retained. Detector counts include all assigned texts. Stage views share roots and edited texts; this is not matched-population-FPR evidence. The separate subset comparisons preserve 512 paired-family v6-minus-v5 bootstrap.","report_model_calls":0,"report_pangram_calls":0}),
    )?;
    save(
        &args.output_dir.join("bindings.json"),
        &json!({"inputs":inputs.0,"source_sha256":sha256(include_bytes!("compare_revision_anchor.rs")),"executable_sha256":sha256(fs::read(std::env::current_exe()?)?)}),
    )?;
    println!("Compared all 162 assigned texts and six repeated stage views; failures retained.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn point() -> OperatingPoint {
        OperatingPoint {
            target_human_false_positive_rate: 0.01,
            threshold: 0.8,
            comparison: "strictly_greater_than".into(),
            calibration_human_rows: 100,
            calibration_human_groups: 100,
            calibration_false_positives: 1,
        }
    }
    #[test]
    fn failed_observations_preserve_denominators_and_strict_thresholds() {
        let human = Joined {
            id: "human".into(),
            text_sha256: sha256("human fixture"),
            group: "a".into(),
            origin: "human_only".into(),
            collection: "synthetic".into(),
            probabilities: [[0.1, 0.6, 0.3]; 2],
            pangram: None,
        };
        let mut model = human.clone();
        model.id = "model".into();
        model.origin = "model_only".into();
        model.probabilities = [[0.2, 0.5, 0.3]; 2];
        model.pangram = Some(Pangram {
            fraction_human: 0.9,
            fraction_ai: 0.1,
            fraction_ai_assisted: 0.0,
            combined_fraction: 0.1,
            version: "synthetic".into(),
            echo_match: "exact".into(),
        });
        let report = stats(&[&human, &model], &[vec![point()], vec![point()]]);
        assert_eq!(report["rows"], 2);
        assert_eq!(report["pangram_failed"], 1);
        assert_eq!(report["pangram_flags_at_10_percent"]["rows"], 1);
        assert_eq!(
            report["detectors"]["v5"][0]["flags_on_all_rows"]["events"],
            1
        );
        assert_eq!(
            report["detectors"]["v5"][0]["paired_with_pangram"]["pangram_only"],
            1
        );
        let empty = stats(&[&human], &[vec![point()], vec![point()]]);
        assert!(empty["pangram_flags_at_10_percent"]["rate"].is_null());
    }
    #[test]
    fn echo_changes_and_unresolved_results_do_not_become_valid_scores() {
        let text = "An invented fixture.";
        let request = json!({"id":"fixture","text":text});
        let member = json!({"item_index":0,"text_sha256":sha256(text)});
        let summary = json!({"bulk_id":"synthetic","whitespace_echo_allowed":false});
        let mut a = json!({"schema":"slop_ninja_pangram_corpus_annotation_v1","model_selector":"pangram-4","member":member,"bulk_id":"synthetic","submitted_text_sha256":sha256(text),"returned_text_sha256":sha256(text),"echo_match":"exact","observation":{"id":"fixture","index":0,"stage":"STAGE_SUCCESS","task_id":"synthetic-task","result":{"text":text,"version":"synthetic","fraction_human":1.0,"fraction_ai":0.0,"fraction_ai_assisted":0.0}}});
        assert!(
            observation(&a, &member, &request, &summary)
                .unwrap()
                .is_some()
        );
        a["observation"]["result"]["fraction_ai"] = json!(0.5);
        assert!(observation(&a, &member, &request, &summary).is_err());
        a["observation"]["result"]["fraction_ai"] = json!(0.0);
        a["observation"]["result"]["text"] = json!("A changed fixture.");
        assert!(observation(&a, &member, &request, &summary).is_err());
        a["observation"]["stage"] = json!("STAGE_PENDING");
        assert!(observation(&a, &member, &request, &summary).is_err());
        a["observation"]["stage"] = json!("STAGE_FAILED");
        a["echo_match"] = json!("failed");
        assert!(
            observation(&a, &member, &request, &summary)
                .unwrap()
                .is_none()
        );
    }
}

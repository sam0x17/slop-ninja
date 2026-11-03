//! Offline integrity and scope audit of the completed Stage3 experiment.
//! STAGE NEW_REPORT.json [PANGRAM_REPORT_DIR]. No fitting, inference or API calls.
#[allow(dead_code)]
#[path = "freeze_last_block_test.rs"]
mod seal;
use anyhow::{Context, Result, ensure};
use seal::{read, sha, text};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};

const SEAL_SHA: &str = "bd52f22386b367c69fc7136e03410fe7a5c24bf5e3d23e8e12e8cfe79fca8673";
type Rows = BTreeMap<String, Value>;
#[derive(Default)]
struct Audit {
    files: BTreeMap<String, String>,
}
impl Audit {
    fn pin(&mut self, p: &Path) -> Result<String> {
        let key = p.canonicalize()?.to_str().context("UTF8 path")?.to_owned();
        let digest = sha(p)?;
        if let Some(previous) = self.files.insert(key, digest.clone()) {
            ensure!(previous == digest, "file changed during audit");
        }
        Ok(digest)
    }
    fn require(&mut self, p: &Path, digest: &Value) -> Result<()> {
        ensure!(
            digest.as_str() == Some(self.pin(p)?.as_str()),
            "changed evidence: {}",
            p.display()
        );
        Ok(())
    }
    fn json(&mut self, p: &Path) -> Result<Value> {
        self.pin(p)?;
        read(p)
    }
}
fn number(v: &Value) -> Result<f64> {
    v.as_f64()
        .filter(|x| x.is_finite())
        .context("finite number required")
}
fn close(a: f64, b: f64) -> Result<()> {
    ensure!((a - b).abs() <= 1e-8, "probability identity differs");
    Ok(())
}
fn probabilities(v: &Value) -> Result<()> {
    let values = v
        .as_object()
        .context("probability object")?
        .values()
        .map(number)
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        !values.is_empty() && values.iter().all(|x| (0.0..=1.0).contains(x)),
        "probability range"
    );
    close(values.iter().sum(), 1.0)
}
fn coverage(report: &Value, requested: usize, supported: usize) -> Result<()> {
    ensure!(
        supported <= requested
            && report["requested_queries"] == requested
            && report["supported_queries"] == supported
            && report["model_reselected"] == false
            && report["calibration_fitted_on_this_population"] == false,
        "report coverage"
    );
    Ok(())
}
fn prediction(p: &Value, contract: &Value) -> Result<()> {
    let origin = &p["origin"];
    ensure!(
        origin.as_object().context("origin")?.len() == 3,
        "three origin classes"
    );
    probabilities(origin)?;
    let human = number(&origin["human_only"])?;
    let model = number(&origin["model_only"])?;
    let mixed = number(&origin["mixed"])?;
    let confidence = number(&p["ai_confidence_percent"])?;
    let share = number(&p["ai_share_percent"])?;
    close(confidence, 100.0 * (model + mixed))?;
    ensure!(
        0.0 <= share
            && share + 1e-8 >= model * 100.0
            && share <= confidence + 1e-8
            && confidence <= 100.0 + 1e-8,
        "bounded share contract"
    );
    ensure!(
        p["calibrated"] == true,
        "final calibrated predictions required"
    );
    for (role, involvement) in [("human", human + mixed), ("ai", model + mixed)] {
        let a = &p["attribution"][role];
        close(number(&a["involvement_probability"])?, involvement)?;
        ensure!(
            a["fallback_asserts_presence"] == false,
            "conditional fallback semantics"
        );
        let scores = &a["conditional_named_scores"];
        probabilities(scores)?;
        let names = contract[format!("{role}_profiles")]
            .as_array()
            .context("profile roster")?
            .iter()
            .map(|v| v.as_str().context("profile name"))
            .collect::<Result<BTreeSet<_>>>()?;
        ensure!(
            scores
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                == names,
            "conditional roster changed"
        );
        let profile = text(a, "profile")?;
        ensure!(
            profile == format!("other_{role}") || names.contains(profile),
            "unsupported attribution output"
        );
    }
    Ok(())
}
fn rows(p: &Path) -> Result<Rows> {
    let mut out = Rows::new();
    for line in fs::read_to_string(p)?.lines() {
        let row: Value = serde_json::from_str(line)?;
        ensure!(
            out.insert(text(&row, "id")?.to_owned(), row).is_none(),
            "duplicate document ID"
        );
    }
    Ok(out)
}
fn register_families(
    data: &Rows,
    part: &str,
    families: &mut BTreeMap<String, String>,
) -> Result<usize> {
    let mut here = BTreeSet::new();
    for r in data.values() {
        let group = text(r, "source_group")?;
        if let Some(previous) = families.insert(group.into(), part.into()) {
            ensure!(previous == part, "source family crosses populations");
        }
        here.insert(group);
    }
    Ok(here.len())
}
fn bundle(
    a: &mut Audit,
    dir: &Path,
    families: &mut BTreeMap<String, String>,
    test: bool,
) -> Result<(Value, BTreeMap<String, Rows>)> {
    let m = a.json(&dir.join("manifest.json"))?;
    ensure!(
        m["complete"] == true && m["final_test_opened"] == test,
        "numeric dataset scope"
    );
    a.require(&dir.join("contract.json"), &m["contract_sha256"])?;
    let mut data = BTreeMap::new();
    for (file, digest) in m["files_sha256"].as_object().context("dataset files")? {
        ensure!(
            Path::new(file).components().count() == 1,
            "local dataset file"
        );
        a.require(&dir.join(file), digest)?;
    }
    for (part, population) in m["populations"].as_object().context("populations")? {
        ensure!(
            ["train", "development", "calibration", "reference", "test"].contains(&part.as_str()),
            "unknown partition"
        );
        let r = rows(&dir.join(format!("{part}.jsonl")))?;
        let label = if part == "reference" {
            if test {
                "test_reference"
            } else {
                "non_test_reference"
            }
        } else {
            part
        };
        let count = register_families(&r, label, families)?;
        ensure!(
            population["rows"] == r.len() && population["families"] == count,
            "population count differs"
        );
        ensure!(
            r.values().all(|v| v["purpose"]
                == if part == "reference" {
                    "reference"
                } else {
                    "query"
                }),
            "query/reference scope"
        );
        data.insert(part.clone(), r);
    }
    Ok((m, data))
}
fn run(stage: &Path, out: &Path, provider_report: Option<&Path>) -> Result<Value> {
    let stage = stage.canonicalize()?;
    ensure!(
        !out.exists()
            && out
                .parent()
                .context("report parent")?
                .canonicalize()?
                .starts_with(&stage),
        "new report under private task root required"
    );
    let mut a = Audit::default();
    let seal_path = stage.join("final-test-v1/seal.json");
    a.require(&seal_path, &json!(SEAL_SHA))?;
    let sealed = seal::validate(&seal_path)?;
    ensure!(
        sealed["stage_root"] == stage.to_str().context("stage path")?,
        "sealed task root differs"
    );
    // validate has just reconstructed the seal by hashing these exact files.
    // Retain its verified bindings without rescanning the multi-GB generators.
    for b in sealed["binding_files"]
        .as_array()
        .context("sealed bindings")?
    {
        a.files
            .insert(text(b, "path")?.into(), text(b, "sha256")?.into());
    }
    let mut families = BTreeMap::new();
    let (fit, _) = bundle(&mut a, &stage.join("fit-bundle-v1"), &mut families, false)?;
    let (test, data) = bundle(
        &mut a,
        &stage.join("final-test-v1/numeric-v1"),
        &mut families,
        true,
    )?;
    ensure!(
        test["fitting_dataset_manifest_sha256"] == sealed["dataset"]["sha256"]
            && test["selection_receipt_sha256"] == SEAL_SHA
            && test["contract_sha256"] == fit["contract_sha256"]
            && test["normalization_refit"] == false
            && test["profile_enrollment_changed"] == false
            && test["test_used_for_selection"] == false,
        "Test changed frozen preprocessing"
    );
    let contract = a.json(&stage.join("fit-bundle-v1/contract.json"))?;
    let source_path = Path::new(text(&test["source_dataset"], "path")?);
    a.require(source_path, &test["source_dataset"]["sha256"])?;
    let source = read(source_path)?;
    let requested = source["requested_records"]
        .as_u64()
        .context("requested queries")? as usize;
    let excluded = source["excluded"]
        .as_array()
        .context("upstream exclusions")?
        .len();
    ensure!(
        source["selection_receipt_sha256"] == SEAL_SHA
            && source["records"] == data["test"].len()
            && requested == data["test"].len() + excluded
            && test["upstream_requested_queries"] == requested,
        "query exclusion accounting"
    );
    let eval = stage.join("final-test-v1/evaluation-v2");
    let m = a.json(&eval.join("manifest.json"))?;
    ensure!(
        m["complete"] == true
            && m["fitting_performed"] == false
            && m["test_used_for_selection"] == false
            && m["selection_receipt_sha256"] == SEAL_SHA
            && m["dataset_manifest_sha256"]
                == a.pin(&stage.join("final-test-v1/numeric-v1/manifest.json"))?,
        "evaluation lineage"
    );
    a.require(&eval.join("comparison.json"), &m["comparison_sha256"])?;
    let mut runs = Vec::new();
    let mut seen = BTreeSet::new();
    for r in m["reports"].as_array().context("six reports")? {
        let condition = text(r, "condition")?;
        let seed = r["seed"].as_u64().context("seed")?;
        ensure!(
            ["adapt_last_block", "frozen_last_block"].contains(&condition)
                && [17, 29, 41].contains(&seed)
                && seen.insert((condition.to_owned(), seed)),
            "unexpected/duplicate run"
        );
        let name = format!("{condition}-seed-{seed}");
        let dir = eval.join(&name);
        a.require(&dir.join("test.json"), &r["report_sha256"])?;
        a.require(&dir.join("test-predictions.json"), &r["predictions_sha256"])?;
        let p = read(&dir.join("test-predictions.json"))?;
        let p = p.as_object().context("prediction map")?;
        ensure!(
            p.keys().eq(data["test"].keys()),
            "requested Test population differs"
        );
        for value in p.values() {
            prediction(value, &contract)?;
        }
        let report = read(&dir.join("test.json"))?;
        coverage(&report, requested, p.len())?;
        for role in ["human", "ai"] {
            let present = data["test"]
                .values()
                .filter(|v| v["roles"][role]["present"] == true)
                .count();
            ensure!(
                report["metrics"]["attribution"][role]["documented_present"] == present
                    && report["metrics"]["attribution"][role]["presence_cutoff"].is_null(),
                "contributor scoring scope"
            );
        }
        let qual = stage.join("boundary-qualification-v1/runs").join(&name);
        let c = a.json(&qual.join("comparison.json"))?;
        let receipt = a.json(&qual.join("rust/receipt.json"))?;
        let export = a.json(&qual.join("export/manifest.json"))?;
        let frozen_export = read(
            &stage
                .join("calibration-run-v1/runs")
                .join(&name)
                .join("export/manifest.json"),
        )?;
        for (key, file) in [
            ("weights", "weights.json"),
            ("last_layer", "last-layer.safetensors"),
            ("contract", "contract.json"),
            ("calibration", "calibration.json"),
        ] {
            ensure!(
                export[key]["sha256"] == frozen_export[key]["sha256"],
                "qualification uses different fitted artifact"
            );
            a.require(&qual.join("export").join(file), &export[key]["sha256"])?;
        }
        a.require(&qual.join("export/manifest.json"), &c["bindings"]["export"])?;
        a.require(&qual.join("rust/receipt.json"), &c["bindings"]["runtime"])?;
        a.require(
            &qual.join("export/reference-predictions.json"),
            &c["bindings"]["oracle"],
        )?;
        a.require(
            &qual.join("export/reference-requests.jsonl"),
            &receipt["bindings"]["requests_sha256"],
        )?;
        a.require(
            &qual.join("rust/predictions.jsonl"),
            &receipt["predictions_sha256"],
        )?;
        ensure!(
            c["passed"] == true
                && c["complete"] == true
                && c["synthetic_cases"] == 44
                && c["supported_cases"] == 39
                && receipt["complete"] == true
                && receipt["unsupported"] == 5
                && receipt["numeric_profile"] == "linux_mkl_aten_2_9_1_default_reductions_v1",
            "native qualification incomplete"
        );
        for (key, gate) in [
            ("raw_h", 1e-4),
            ("shared_z", 1e-3),
            ("outputs_on_0_1_scale", 1e-5),
        ] {
            ensure!(
                number(&c["fixed_gates"][key])? == gate
                    && number(&c["maximum_absolute_error"][key])? <= gate,
                "qualification gate differs/fails"
            );
        }
        runs.push(json!({"condition":condition,"seed":seed,"predictions":p.len(),"output_contract_checked":true,
            "native_cases":44,"native_supported":39,"maximum_absolute_error":c["maximum_absolute_error"],
            "human_reference_top1":report["reference_retrieval"]["human"]["top1"]["estimate"],
            "generator_reference_gallery":report["reference_retrieval"]["ai"]["gallery_identities"]}));
    }
    ensure!(seen.len() == 6, "missing matched run");
    let pg = provider_report
        .map(Path::to_path_buf)
        .unwrap_or_else(|| stage.join("final-test-v1/pangram-evaluation-v1"));
    ensure!(
        pg.canonicalize()?
            .starts_with(stage.join("final-test-v1").canonicalize()?),
        "provider report must be inside this frozen Test root"
    );
    let pm = a.json(&pg.join("manifest.json"))?;
    a.require(&pg.join("test.json"), &pm["report_sha256"])?;
    a.require(&pg.join("scores.json"), &pm["scores_sha256"])?;
    a.require(
        &pg.join("fixture-bindings.json"),
        &pm["fixture_bindings_sha256"],
    )?;
    let pr = read(&pg.join("test.json"))?;
    ensure!(
        pm["mapping_sha256"] == sealed["pangram_mapping"]["sha256"]
            && pm["selection_receipt_sha256"] == SEAL_SHA
            && pm["fitting_or_calibration_performed"] == false
            && pm["model_reselected"] == false,
        "provider comparison changed selection"
    );
    let supported = pr["pangram_supported"]
        .as_u64()
        .context("provider supported")?;
    let missing = pr["pangram_missing"].as_u64().context("provider missing")?;
    let bindings = read(&pg.join("fixture-bindings.json"))?;
    let mut bound = 0u64;
    for b in bindings.as_array().context("provider fixture bindings")? {
        let id = text(b, "id")?;
        ensure!(data["test"].contains_key(id), "unlisted provider fixture");
        if b["status"] == "complete" {
            let path = stage
                .join("final-test-v1/pangram-job-v1/fixtures/test")
                .join(id)
                .join("result.json");
            a.require(&path, &b["result_sha256"])?;
            let receipt = read(&path)?;
            ensure!(
                receipt["id"] == id
                    && receipt["text_sha256"] == b["text_sha256"]
                    && receipt["response"]["version"] == b["version"]
                    && receipt["response"]["stage"] == "STAGE_SUCCESS",
                "provider fixture binding differs"
            );
            bound += 1;
        }
    }
    ensure!(bound == supported, "provider receipt count differs");
    ensure!(
        supported + missing == data["test"].len() as u64
            && pr["requested"] == data["test"].len()
            && pr["pangram_population_complete"] == (missing == 0)
            && pm["pangram_population_complete"] == (missing == 0),
        "provider denominator"
    );
    let report = json!({"schema":"slop_ninja_last_block_evidence_audit_v1","audit_completed":true,
        "goal_completion_asserted":false,"selection_seal_sha256":SEAL_SHA,"sealed_files":sealed["binding_files"].as_array().unwrap().len(),
        "source_families_disjoint_across_query_and_reference_populations":true,"populations":{"fit":fit["populations"],"test":test["populations"]},
        "runs":runs,"predictions_checked":data["test"].len()*6,
        "upstream_queries":{"requested":requested,"excluded":excluded,"evaluated":data["test"].len()},
        "pangram":{"requested":pr["requested"],"supported":supported,"missing":missing,"population_complete":missing==0,"report_directory":pg,"receipts_rehashed":bound},
        "scope":"Rehash retained evidence, reconstruct original seal, inspect numeric rosters and every current calibrated Test output. No new inference, fitting, parser work or provider calls. Native comparison is retained evidence, not a fresh runtime replay.",
        "not_established":["Pangram parity","Useful general attribution or generator-reference retrieval","Authenticated contemporary human-only performance","New CFG benefit (all CFG inputs zero)","Workers/Wasm or Container serving","Remote R2 backup"],
        "files":a.files.iter().map(|(p,h)|json!({"path":p,"sha256":h})).collect::<Vec<_>>()});
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)?;
    serde_json::to_writer_pretty(&mut f, &report)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(
        json!({"audit_completed":true,"files":a.files.len(),"predictions_checked":data["test"].len()*6,"pangram_missing":missing,"goal_completion_asserted":false}),
    )
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        matches!(a.len(), 2 | 3),
        "STAGE NEW_REPORT.json [PANGRAM_REPORT_DIR]"
    );
    println!(
        "{}",
        run(Path::new(&a[0]), Path::new(&a[1]), a.get(2).map(Path::new))?
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusions_remain_in_the_requested_denominator() {
        let report = json!({"requested_queries":4,"supported_queries":3,"model_reselected":false,"calibration_fitted_on_this_population":false});
        coverage(&report, 4, 3).unwrap();
        assert!(coverage(&report, 3, 3).is_err());
        assert!(coverage(&report, 4, 4).is_err());
    }
    #[test]
    fn low_involvement_still_requires_both_roles_and_valid_share() -> Result<()> {
        let c = json!({"human_profiles":["writer"],"ai_profiles":["model"]});
        let p = json!({"origin":{"human_only":0.99,"model_only":0.001,"mixed":0.009},"ai_confidence_percent":1.0,"ai_share_percent":0.2,"calibrated":true,
            "attribution":{"human":{"involvement_probability":0.999,"conditional_named_scores":{"writer":1.0},"fallback_asserts_presence":false,"profile":"other_human"},
            "ai":{"involvement_probability":0.01,"conditional_named_scores":{"model":1.0},"fallback_asserts_presence":false,"profile":"other_ai"}}});
        prediction(&p, &c)?;
        let mut bad = p.clone();
        bad["ai_share_percent"] = 2.0.into();
        assert!(prediction(&bad, &c).is_err());
        let mut bad = p.clone();
        bad["attribution"]["ai"] = Value::Null;
        assert!(prediction(&bad, &c).is_err());
        let mut bad = p;
        bad["attribution"]["human"]["fallback_asserts_presence"] = true.into();
        assert!(prediction(&bad, &c).is_err());
        Ok(())
    }
    #[test]
    fn descendant_leakage_is_rejected() {
        let mut families = BTreeMap::new();
        let data = BTreeMap::from([("synthetic".into(), json!({"source_group":"one-work"}))]);
        register_families(&data, "train", &mut families).unwrap();
        assert!(register_families(&data, "test", &mut families).is_err());
    }
}

//! Shared verification for frozen evaluation. No corpus acquisition or fitting.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Read, path::Path};

pub fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
pub fn sha(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut block = [0u8; 65536];
    loop {
        let n = file.read(&mut block)?;
        if n == 0 {
            break;
        }
        hash.update(&block[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}
pub fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing string {key}"))
}
pub const CANDIDATE_NAMES: [&str; 16] = [
    "cfg_all",
    "cfg_leaf",
    "text",
    "combined",
    "ordered_cfg",
    "global_cfg_control",
    "warm_text",
    "warm_combined",
    "expanded_text",
    "expanded_combined",
    "mixture_text",
    "mixture_combined",
    "scale_12_text",
    "scale_24_text",
    "scale_all_text",
    "scale_all_combined",
];
fn probability(value: &Value) -> Result<f64> {
    let x = value.as_f64().context("missing candidate metric")?;
    ensure!(
        x.is_finite() && (0.0..=1.).contains(&x),
        "invalid candidate metric"
    );
    Ok(x)
}
fn ranking(a: &Value, b: &Value) -> std::cmp::Ordering {
    a["mean_balanced_development_brier"]
        .as_f64()
        .unwrap()
        .total_cmp(&b["mean_balanced_development_brier"].as_f64().unwrap())
        .then_with(|| {
            a["mean_human_fpr_at_0_5"]
                .as_f64()
                .unwrap()
                .total_cmp(&b["mean_human_fpr_at_0_5"].as_f64().unwrap())
        })
        .then_with(|| {
            a["active_input_dimension"]
                .as_u64()
                .unwrap()
                .cmp(&b["active_input_dimension"].as_u64().unwrap())
        })
        .then_with(|| {
            a["candidate"]
                .as_str()
                .unwrap()
                .cmp(b["candidate"].as_str().unwrap())
        })
}
pub fn validate_selection_receipt(path: &Path) -> Result<Value> {
    let receipt = read(path)?;
    ensure!(
        receipt["schema"] == "slop_ninja_balanced_a_selection_freeze_v3"
            && receipt["final_test_opened"] == false
            && receipt["test_used_for_selection"] == false
            && receipt["model_calibration_frozen"] == true
            && receipt["grammar_frozen"] == true
            && receipt["source_size_comparison_complete"] == true
            && receipt["representative_seed"] == 17
            && receipt["evaluation_seeds"] == serde_json::json!([17, 29, 41]),
        "incomplete final selection receipt"
    );
    let mut files = BTreeMap::new();
    for binding in receipt["binding_files"]
        .as_array()
        .context("binding_files")?
    {
        let p = text(binding, "path")?;
        let hash = text(binding, "sha256")?;
        ensure!(
            Path::new(p).is_absolute()
                && hash.len() == 64
                && hash.bytes().all(|c| c.is_ascii_hexdigit())
                && files.insert(p.to_owned(), hash.to_owned()).is_none(),
            "invalid/duplicate binding"
        );
        ensure!(sha(Path::new(p))? == hash, "frozen file changed: {p}");
    }
    let bound = |p: &Path, expected: &Value| -> Result<()> {
        ensure!(
            files
                .get(p.to_str().context("UTF8 path")?)
                .map(String::as_str)
                == Some(expected.as_str().context("expected digest")?),
            "required frozen file unbound: {}",
            p.display()
        );
        Ok(())
    };
    let mut names = std::collections::BTreeSet::new();
    let mut fit_paths = std::collections::BTreeSet::new();
    let candidates = receipt["candidates"].as_array().context("candidates")?;
    ensure!(
        candidates.len() == 16,
        "all sixteen eligible candidates required"
    );
    let mut selected = None;
    for candidate in candidates {
        let name = text(candidate, "candidate")?;
        ensure!(names.insert(name), "duplicate candidate");
        let numeric = Path::new(text(candidate, "numeric_path")?);
        bound(
            &numeric.join("manifest.json"),
            &candidate["numeric_manifest_sha256"],
        )?;
        bound(
            &numeric.join("contract.json"),
            &candidate["contract_file_sha256"],
        )?;
        if receipt["selected_candidate"] == name {
            selected = Some(candidate);
        }
        let mut seeds = std::collections::BTreeSet::new();
        let mut brier = 0.;
        let mut fpr = 0.;
        for run in candidate["runs"].as_array().context("runs")? {
            let seed = run["seed"].as_u64().context("seed")?;
            ensure!(
                [17, 29, 41].contains(&seed) && seeds.insert(seed),
                "wrong/duplicate seed"
            );
            let fit = Path::new(text(run, "model_path")?);
            ensure!(fit_paths.insert(fit), "fit reused across candidates");
            for (file, key) in [
                ("weights.json", "weights_sha256"),
                ("fit.json", "fit_sha256"),
                ("model.pt", "checkpoint_sha256"),
            ] {
                bound(&fit.join(file), &run[key])?;
            }
            bound(
                Path::new(text(run, "development_report_path")?),
                &run["development_report_sha256"],
            )?;
            bound(
                Path::new(text(run, "calibration_report_path")?),
                &run["calibration_report_sha256"],
            )?;
            let actual_fit = read(&fit.join("fit.json"))?;
            let report = read(Path::new(text(run, "development_report_path")?))?;
            ensure!(
                actual_fit["seed"] == seed
                    && actual_fit["control"] == "joint"
                    && actual_fit["final_test_opened"] == false
                    && actual_fit["weights_sha256"] == run["weights_sha256"]
                    && actual_fit["checkpoint_sha256"] == run["checkpoint_sha256"]
                    && actual_fit["data_manifest_sha256"] == candidate["numeric_manifest_sha256"]
                    && report["split"] == "development"
                    && report["test_used_for_selection"] == false
                    && report["fit"] == actual_fit,
                "frozen fit/report mismatch"
            );
            let score = probability(&report["family_origin_balanced"]["involvement_brier"])?;
            let fp = probability(&report["model"]["human_fpr_at_0_5"])?;
            ensure!(
                probability(&run["development_balanced_brier"])? == score
                    && probability(&run["human_fpr_at_0_5"])? == fp,
                "changed selection metric"
            );
            brier += score / 3.;
            fpr += fp / 3.;
        }
        ensure!(seeds.len() == 3, "all seeds required");
        ensure!(
            (probability(&candidate["mean_balanced_development_brier"])? - brier).abs() < 1e-12
                && (probability(&candidate["mean_human_fpr_at_0_5"])? - fpr).abs() < 1e-12
                && candidate["active_input_dimension"]
                    .as_u64()
                    .is_some_and(|n| n > 0),
            "candidate means/dimension differ"
        );
    }
    ensure!(
        names == CANDIDATE_NAMES.into_iter().collect(),
        "eligible candidate roster changed"
    );
    let selected = selected.context("selected candidate missing")?;
    ensure!(
        candidates
            .iter()
            .min_by(|a, b| ranking(a, b))
            .context("empty candidates")?["candidate"]
            == selected["candidate"],
        "selected candidate violates frozen ranking"
    );
    ensure!(
        receipt["selected_numeric_path"] == selected["numeric_path"]
            && receipt["selected_contract_sha256"] == selected["contract_sha256"],
        "selected candidate binding differs"
    );
    for key in [
        "pangram_mapping",
        "source_plan",
        "candidate_disposition",
        "source_size_results",
    ] {
        let value = &receipt[key];
        bound(Path::new(text(value, "path")?), &value["sha256"])?;
    }
    ensure!(
        receipt["pangram_mapping"]["method"] == "isotonic_v1"
            && receipt["pangram_mapping"]["fit_split"] == "calibration",
        "frozen CAL isotonic mapping required"
    );
    Ok(receipt)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn put(path: &Path, value: &Value, files: &mut BTreeMap<String, String>) -> String {
        fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
        let h = sha(path).unwrap();
        files.insert(path.to_str().unwrap().into(), h.clone());
        h
    }
    fn fixture() -> (tempfile::TempDir, PathBuf, Value) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        let numeric = path.join("numeric");
        fs::create_dir(&numeric).unwrap();
        let mut files = BTreeMap::new();
        let mh = put(&numeric.join("manifest.json"), &json!({}), &mut files);
        let ch = put(&numeric.join("contract.json"), &json!({}), &mut files);
        let evidence = path.join("evidence.json");
        let eh = put(&evidence, &json!({}), &mut files);
        let mut candidates = Vec::new();
        for (index, name) in CANDIDATE_NAMES.iter().enumerate() {
            let score = 0.1 + index as f64 / 100.;
            let mut runs = Vec::new();
            for seed in [17, 29, 41] {
                let fit = path.join(format!("{name}-{seed}"));
                fs::create_dir(&fit).unwrap();
                let wh = put(&fit.join("weights.json"), &json!({}), &mut files);
                let ph = put(&fit.join("model.pt"), &json!({}), &mut files);
                let f = json!({"seed":seed,"control":"joint","final_test_opened":false,
                    "weights_sha256":wh,"checkpoint_sha256":ph,"data_manifest_sha256":mh});
                let fh = put(&fit.join("fit.json"), &f, &mut files);
                let dr = json!({"split":"development","test_used_for_selection":false,
                    "fit":f,"family_origin_balanced":{"involvement_brier":score},
                    "model":{"human_fpr_at_0_5":0.1}});
                let dp = fit.join("development-report.json");
                let cp = fit.join("calibration-report.json");
                let dh = put(&dp, &dr, &mut files);
                let mut cr = dr;
                cr["split"] = json!("calibration");
                let cph = put(&cp, &cr, &mut files);
                runs.push(json!({"seed":seed,"model_path":fit,"weights_sha256":wh,
                    "checkpoint_sha256":ph,"fit_sha256":fh,"development_report_path":dp,
                    "development_report_sha256":dh,"calibration_report_path":cp,
                    "calibration_report_sha256":cph,"development_balanced_brier":score,
                    "human_fpr_at_0_5":0.1}));
            }
            candidates.push(json!({"candidate":name,"numeric_path":numeric,
                "numeric_manifest_sha256":mh,"contract_file_sha256":ch,"contract_sha256":"synthetic",
                "active_input_dimension":768,"mean_balanced_development_brier":score,
                "mean_human_fpr_at_0_5":0.1,"runs":runs}));
        }
        let b = json!({"path":evidence,"sha256":eh});
        let mut pg = b.clone();
        pg["method"] = json!("isotonic_v1");
        pg["fit_split"] = json!("calibration");
        let r = json!({"schema":"slop_ninja_balanced_a_selection_freeze_v3",
            "final_test_opened":false,"test_used_for_selection":false,
            "model_calibration_frozen":true,"grammar_frozen":true,"source_size_comparison_complete":true,
            "representative_seed":17,"evaluation_seeds":[17,29,41],"candidates":candidates,
            "selected_candidate":"cfg_all","selected_numeric_path":numeric,
            "selected_contract_sha256":"synthetic","pangram_mapping":pg,"source_plan":b,
            "candidate_disposition":b,"source_size_results":b,
            "binding_files":files.iter().map(|(p,h)|json!({"path":p,"sha256":h})).collect::<Vec<_>>()});
        let rp = path.join("selection.json");
        fs::write(&rp, serde_json::to_vec(&r).unwrap()).unwrap();
        (root, rp, r)
    }
    use std::path::PathBuf;
    #[test]
    fn complete_frozen_roster_validates_and_weight_changes_fail() {
        let (_root, p, r) = fixture();
        validate_selection_receipt(&p).unwrap();
        let weights = Path::new(
            r["candidates"][0]["runs"][0]["model_path"]
                .as_str()
                .unwrap(),
        )
        .join("weights.json");
        fs::write(weights, b"changed").unwrap();
        assert!(validate_selection_receipt(&p).is_err());
    }
    #[test]
    fn selected_seed_or_candidate_cannot_be_changed() {
        let (_root, p, r) = fixture();
        let mut changed = r.clone();
        changed["selected_candidate"] = json!("warm_text");
        fs::write(&p, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(validate_selection_receipt(&p).is_err());
        let mut changed = r.clone();
        changed["candidates"][0]["runs"][0]["seed"] = json!(29);
        fs::write(&p, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(validate_selection_receipt(&p).is_err());
        let mut changed = r;
        changed["candidates"].as_array_mut().unwrap().pop();
        fs::write(&p, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(validate_selection_receipt(&p).is_err());
    }
}

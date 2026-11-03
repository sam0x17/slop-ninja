//! Fit the pre-declared full-A involvement calibration and the AI-role
//! named threshold on CAL only (shared-contract v2 artifacts).
//!
//! calibrate_full_a BUNDLE CONFIG FIT RAW_CAL_RUN SEALED_SELECTION POLICY.json NEW_CALIBRATION.json
//!     [NEW_CALIBRATED_CAL_PREDICTIONS.jsonl]
//!
//! BUNDLE holds the CAL rows (metadata only is read), CONFIG its
//! `contract.json`, FIT the completed v2 fit, RAW_CAL_RUN the
//! `export_full_a.py raw --part cal` run of that fit's selected checkpoint and
//! SEALED_SELECTION a `slop_ninja_full_a_selection_v2` record that froze the
//! checkpoint on Dev-iter or Dev-select without reading CAL.
//!
//! Involvement: `involvement_calibration::select` under the declared policy,
//! or `select_protocol` for policy v3 (protocol v1.2: origin temperature, then
//! `isotonic_v1_clipped`), which reads the raw origin logits.
//!
//! The optional last argument writes the CAL documents' calibrated
//! shared-contract predictions (the frozen map and the arm's share rule
//! applied to the raw heads) for the evaluator's CAL operating points.
//! Roles: the shared-contract human roster is empty, so the human role is
//! recorded `roster_empty` (it always returns `other_human`; no threshold is
//! fitted). The AI role's named threshold uses the stage-3 objective: half
//! known-correct acceptance (CAL positives whose generator label is in the
//! roster), half withheld rejection (positives of generators outside it),
//! equal source-family weight within each population, higher threshold on
//! ties. When CAL holds no withheld generator the rejection half is absent:
//! the threshold maximizes known-correct acceptance alone (recorded).
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::full_a_v2::{
    CAL_SPLIT, CALIBRATION_SCHEMA, RowFacts, completed_fit, joined, raw_run, sealed_selection, split_rows,
};
use grammar_a_baseline::involvement_calibration::{
    Map, Observation, OriginObservation, Policy, PolicyVersion, select, select_protocol,
};
use grammar_a_baseline::mining::bundle_digests;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}
fn softmax(logits: &Value, n: usize) -> Result<Vec<f64>> {
    let v = logits
        .as_array()
        .context("logits")?
        .iter()
        .map(|x| x.as_f64().filter(|x| x.is_finite()).context("finite logit"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(v.len() == n && n > 1, "logit dimension");
    let m = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let e = v.iter().map(|x| (x - m).exp()).collect::<Vec<_>>();
    let s = e.iter().sum::<f64>();
    Ok(e.into_iter().map(|x| x / s).collect())
}

type Sample = (String, bool, bool, f64);

fn threshold_objective(samples: &[Sample], t: f64) -> Result<(f64, Option<f64>)> {
    let mut groups = [BTreeMap::<&str, (f64, usize)>::new(), BTreeMap::new()];
    for (g, known, correct, score) in samples {
        let value = if *known { *correct && *score >= t } else { *score < t };
        let s = groups[usize::from(!known)].entry(g).or_default();
        s.0 += f64::from(value);
        s.1 += 1;
    }
    ensure!(!groups[0].is_empty(), "known CAL families required");
    let rate = |g: &BTreeMap<&str, (f64, usize)>| g.values().map(|(s, n)| s / *n as f64).sum::<f64>() / g.len() as f64;
    Ok((rate(&groups[0]), (!groups[1].is_empty()).then(|| rate(&groups[1]))))
}

/// AI-role named threshold from CAL positives (generator label = identity).
fn ai_threshold(rows: &[(&RowFacts, &Value)], profiles: &[String], trained: bool) -> Result<Value> {
    let mut samples = Vec::<Sample>::new();
    for (r, p) in rows {
        let Some(identity) = r.generator_label.as_deref() else { continue };
        let known = profiles.iter().any(|x| x == identity);
        let logits = softmax(&p["ai_logits"], profiles.len())?;
        let winner = (0..logits.len())
            .max_by(|a, b| logits[*a].total_cmp(&logits[*b]).then_with(|| b.cmp(a)))
            .unwrap();
        samples.push((r.source_group.clone(), known, profiles[winner] == identity, logits[winner]));
    }
    let families = |known| samples.iter().filter(|s| s.1 == known).map(|s| &s.0).collect::<BTreeSet<_>>().len();
    let qualified = trained && families(true) > 0;
    let withheld = families(false) > 0;
    let mut report = json!({"qualified":qualified,"roster_empty":false,"threshold":1.,
        "known_families":families(true),"withheld_families":families(false),
        "known_rows":samples.iter().filter(|s| s.1).count(),"withheld_rows":samples.iter().filter(|s| !s.1).count(),
        "objective":if withheld {"half_known_correct_named_acceptance_plus_half_withheld_rejection"}
                    else {"known_correct_named_acceptance_only; no withheld generator in CAL"},
        "weighting":"equal_source_family_within_each_known_or_withheld_population",
        "presence_gate":"documented_role_presence_only; no_predicted_probability_cutoff",
        "identity":"generator_label of CAL positives; known iff in the AI roster",
        "tie_break":"higher_threshold","objective_value":null});
    if !qualified {
        return Ok(report);
    }
    let mut observed = samples.iter().map(|s| s.3).collect::<Vec<_>>();
    observed.sort_by(f64::total_cmp);
    observed.dedup();
    let mut candidates = vec![0., 1.];
    candidates.extend(observed.windows(2).map(|p| (p[0] + p[1]) / 2.));
    let mut best = (-1., 1., (0., None));
    for t in &candidates {
        let rates = threshold_objective(&samples, *t)?;
        let score = match rates.1 { Some(w) => (rates.0 + w) / 2., None => rates.0 };
        if score > best.0 || (score == best.0 && *t > best.1) {
            best = (score, *t, rates);
        }
    }
    report["threshold"] = json!(best.1);
    report["objective_value"] = json!(best.0);
    report["known_correct_named_rate"] = json!(best.2.0);
    report["withheld_rejection_rate"] = json!(best.2.1);
    report["candidate_count"] = json!(candidates.len());
    Ok(report)
}

fn profiles(contract: &Value, role: &str) -> Result<Vec<String>> {
    contract[format!("{role}_profiles")].as_array().context("roster")?.iter()
        .map(|v| Ok(v.as_str().context("profile")?.to_owned())).collect()
}

/// Calibrated shared-contract prediction of one CAL document.
fn calibrated_row(id: &str, p: &Value, map: &Map, share_mode: &str, ai: &[String]) -> Result<Value> {
    let logits = p["origin_logits"].as_array().context("origin logits")?.iter()
        .map(|v| v.as_f64().context("origin logit")).collect::<Result<Vec<_>>>()?;
    let g = p["involvement_logit"].as_f64().context("logit")?;
    ensure!(logits.len() == 3, "three origin logits");
    let (inv, r) = map.apply_origin(&logits, g)?;
    let origin = [1. - inv, inv * r, inv * (1. - r)];
    let share = match share_mode {
        "head" => origin[1] + origin[2] * p["conditional_mixed_share"].as_f64().context("mixed share")?,
        _ => p["token_mean"].as_f64().context("token mean")?,
    }
    .clamp(origin[1], inv);
    let scores = softmax(&p["ai_logits"], ai.len())?;
    let attribution: serde_json::Map<String, Value> = ai.iter().cloned().zip(scores.into_iter().map(Value::from)).collect();
    Ok(json!({"id":id,"p_human":origin[0],"p_model_only":origin[1],"p_mixed":origin[2],"share":share,
        "attribution":attribution,"attribution_basis":"conditional_on_ai_involvement","calibrated":true}))
}

fn run(a: &[PathBuf]) -> Result<()> {
    let (bundle, config, fit_dir, raw_dir, selected, policy_path, out, preds_out) = match a {
        [b, c, f, r, s, p, o] => (b, c, f, r, s, p, o, None),
        [b, c, f, r, s, p, o, q] => (b, c, f, r, s, p, o, Some(q)),
        _ => anyhow::bail!("BUNDLE CONFIG FIT RAW_CAL_RUN SEALED_SELECTION POLICY.json NEW_CALIBRATION.json \
                            [NEW_CALIBRATED_CAL_PREDICTIONS.jsonl]"),
    };
    ensure!(!out.exists(), "new calibration output required");
    ensure!(preds_out.is_none_or(|q| !q.exists()), "new calibrated predictions output required");
    let contract = read(&config.join("contract.json"))?;
    ensure!(contract["schema"] == "slop_ninja_full_a_contract_v2" &&
            contract["bundle_manifest_sha256"].as_str() == Some(sha(&bundle.join("MANIFEST.json"))?.as_str()),
            "v2 contract of this bundle required");
    let fit = completed_fit(fit_dir, bundle, config)?;
    let selection = sealed_selection(selected, bundle, &fit)?;
    let raw = raw_run(raw_dir, CAL_SPLIT, bundle, Some(&fit))?;
    let policy = Policy::from_json(&read(policy_path)?)?;
    let rows = split_rows(bundle, CAL_SPLIT)?;
    let joined = joined(&rows, &raw)?;
    let (map, involvement) = if policy.version == PolicyVersion::V3 {
        let mut observations = Vec::new();
        for (r, p) in &joined {
            let l = p["origin_logits"].as_array().context("origin logits")?.iter()
                .map(|v| v.as_f64().context("origin logit")).collect::<Result<Vec<_>>>()?;
            ensure!(l.len() == 3, "three origin logits");
            observations.push(OriginObservation { family: r.source_group.clone(), origin: r.origin_index()?,
                                                  logits: [l[0], l[1], l[2]],
                                                  logit: p["involvement_logit"].as_f64().context("logit")? });
        }
        select_protocol(&observations, &policy)?
    } else {
        let mut observations = Vec::new();
        for (r, p) in &joined {
            observations.push(Observation { family: r.source_group.clone(), origin: r.origin_index()?,
                                            logit: p["involvement_logit"].as_f64().context("logit")? });
        }
        select(&observations, &policy)?
    };
    let trained = &fit.report["trained_selected_heads"];
    ensure!(trained["origin"] == true, "calibration needs a trained origin head");
    let human = profiles(&contract, "human")?;
    ensure!(human.is_empty(), "shared-contract bundles carry no human identities; the human roster must be empty");
    let ai = profiles(&contract, "ai")?;
    let roles = json!({
        "human": {"roster_empty": true, "qualified": false, "threshold": 1.0,
                  "rule": "empty roster: the role always returns other_human; no named threshold is fitted"},
        "ai": ai_threshold(&joined, &ai, trained["ai"] == true)?});
    let b = &fit.report["binding"];
    let report = json!({"schema":CALIBRATION_SCHEMA,"complete":true,"fit_split":CAL_SPLIT,
        "final_test_opened":false,"arm":b["arm"],"seed":b["seed"],
        "selected_checkpoint_sha256":fit.report["selected_checkpoint_sha256"],"contract_sha256":b["contract_sha256"],
        "fit_sha256":fit.fit_sha256,"bundle_sha256":bundle_digests(bundle)?,"selection_sha256":sha(selected)?,
        "selection_split":selection["selection_split"],"raw_manifest_sha256":raw.manifest_sha256,
        "policy_sha256":sha(policy_path)?,"cal_rows":rows.len(),
        "involvement":{"selected":map.description(),"report":involvement},"roles":roles,
        "implementation_sha256":hex::encode(Sha256::digest(include_bytes!("calibrate_full_a.rs"))),
        "library_sha256":hex::encode(Sha256::digest(include_bytes!("../src/involvement_calibration.rs"))),
        "limits":["CAL scores are fitted diagnostics, never independent evaluation.",
            "Share is recomputed from the calibrated involvement and the frozen share rule.",
            "Pangram content fractions are not targets for this calibration."]});
    let mut f = fs::OpenOptions::new().create_new(true).write(true).open(out)?;
    serde_json::to_writer_pretty(&mut f, &report)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    if let Some(q) = preds_out {
        let share_mode = b["arm_definition"]["share_mode"].as_str().context("arm share mode")?;
        let mut w = std::io::BufWriter::new(fs::OpenOptions::new().create_new(true).write(true).open(q)?);
        for (r, p) in &joined {
            serde_json::to_writer(&mut w, &calibrated_row(&r.id, p, &map, share_mode, &ai)?)?;
            w.write_all(b"\n")?;
        }
        w.into_inner()?.sync_all()?;
    }
    println!("{}", json!({"arm":b["arm"],"seed":b["seed"],"method":map.method(),
        "ai_qualified":report["roles"]["ai"]["qualified"],"ai_threshold":report["roles"]["ai"]["threshold"]}));
    Ok(())
}

fn main() -> Result<()> {
    run(&std::env::args_os().skip(1).map(PathBuf::from).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_threshold_weights_families_and_handles_absent_withheld_population() {
        let s = vec![
            ("a".into(), true, true, 0.8),
            ("b".into(), true, false, 0.8),
            ("b".into(), true, false, 0.8),
            ("c".into(), false, false, 0.6),
        ];
        assert_eq!(threshold_objective(&s, 0.7).unwrap(), (0.5, Some(1.)));
        assert_eq!(threshold_objective(&s[..3], 0.7).unwrap(), (0.5, None));
        assert!(threshold_objective(&s[3..], 0.7).is_err(), "known families are required");
    }

    fn facts(id: &str, label: Option<&str>, group: &str) -> RowFacts {
        RowFacts { id: id.into(), split: "cal".into(), text_sha256: format!("t{id}"),
                   origin: if label.is_some() { "model_only" } else { "human_only" }.into(),
                   generator_label: label.map(str::to_owned), source_group: group.into() }
    }

    #[test]
    fn ai_threshold_uses_generator_labels_as_identities() {
        let profiles = vec!["gen-a".to_owned(), "gen-b".to_owned()];
        let rows = [facts("1", Some("gen-a"), "g1"), facts("2", Some("gen-b"), "g2"), facts("3", None, "g3"),
                    facts("4", Some("olmo2"), "g4")];
        let preds = [json!({"ai_logits": [3.0, 0.0]}), json!({"ai_logits": [0.0, 2.0]}),
                     json!({"ai_logits": [0.0, 0.0]}), json!({"ai_logits": [0.2, 0.0]})];
        let joined: Vec<(&RowFacts, &Value)> = rows.iter().zip(&preds).collect();
        let r = ai_threshold(&joined, &profiles, true).unwrap();
        assert_eq!((r["qualified"].as_bool(), r["known_families"].as_u64(), r["withheld_families"].as_u64()),
                   (Some(true), Some(2), Some(1)));
        // The withheld generator's top score (0.55) is rejected, both known rows accepted.
        let t = r["threshold"].as_f64().unwrap();
        assert!(t > 0.55 && t <= 0.88, "{t}");
        assert_eq!(r["objective_value"], json!(1.0));
        let known_only = ai_threshold(&joined[..3], &profiles, true).unwrap();
        assert_eq!(known_only["withheld_families"], json!(0));
        assert!(known_only["objective"].as_str().unwrap().contains("no withheld"));
        assert_eq!(known_only["known_correct_named_rate"], json!(1.0));
        assert_eq!(ai_threshold(&joined, &profiles, false).unwrap()["qualified"], json!(false));
    }

    fn write(path: &Path, v: &Value) {
        fs::write(path, serde_json::to_vec(v).unwrap()).unwrap();
    }

    #[test]
    fn calibrates_a_v2_fit_end_to_end_and_refuses_unsealed_or_foreign_inputs() {
        use grammar_a_baseline::mining::sha256_file;
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let (bundle, config, fit, raw) = (d.join("bundle"), d.join("config"), d.join("fit"), d.join("raw"));
        for p in [&bundle, &config, &fit, &raw] {
            fs::create_dir(p).unwrap();
        }
        let mut rows = String::new();
        let mut raws = String::new();
        for i in 0..40 {
            let human = i < 20;
            let label = if human { Value::Null } else { json!(if i % 2 == 0 { "gen-a" } else { "gen-b" }) };
            let t = format!("t{i}");
            rows.push_str(&format!("{}\n", json!({"id": format!("c{i}"), "split": "cal", "text": "never read",
                "text_sha256": t, "origin": if human { "human_only" } else { "model_only" },
                "generator_label": label, "source_group": format!("g{}", i / 2)})));
            let g = if human { -2.0 + 0.05 * i as f64 } else { 1.0 + 0.05 * i as f64 };
            let ai = if i % 2 == 0 { [2.0, 0.0] } else { [0.0, 2.0] };
            raws.push_str(&format!("{}\n", json!({"id": format!("c{i}"), "text_sha256": t, "involvement_logit": g,
                                                 "ai_logits": ai, "origin_logits": [0.0, g, g - 30.0],
                                                 "conditional_mixed_share": 0.25})));
        }
        rows.push_str(&format!("{}\n", json!({"id": "tr", "split": "train", "text_sha256": "x", "origin": "human_only",
            "generator_label": null, "source_group": "gt"})));
        fs::write(bundle.join("rows.jsonl"), rows).unwrap();
        fs::write(bundle.join("weights.json"), "{}").unwrap();
        fs::write(bundle.join("MANIFEST.json"), "{\"m\":1}").unwrap();
        let digests = bundle_digests(&bundle).unwrap();
        write(&config.join("contract.json"), &json!({"schema": "slop_ninja_full_a_contract_v2",
            "bundle_manifest_sha256": sha256_file(&bundle.join("MANIFEST.json")).unwrap(),
            "human_profiles": [], "ai_profiles": ["gen-a", "gen-b"]}));
        let contract_sha = sha256_file(&config.join("contract.json")).unwrap();
        write(&fit.join("fit.json"), &json!({"schema": "slop_ninja_full_a_fit_v2", "complete": true,
            "final_test_opened": false, "cohort_weight_log": {"complete": true}, "selected_checkpoint_sha256": "ck",
            "binding": {"bundle_sha256": digests, "contract_sha256": contract_sha, "arm": "E1", "seed": 17,
                        "arm_definition": {"share_mode": "head"}},
            "trained_selected_heads": {"origin": true, "share": true, "ai": true, "human": false}}));
        let fit_sha = sha256_file(&fit.join("fit.json")).unwrap();
        fs::write(raw.join("raw.jsonl"), raws).unwrap();
        let raw_manifest = json!({"schema": "slop_ninja_full_a_raw_predictions_v2", "complete": true,
            "partition": "cal", "calibrated": false, "final_test_opened": false, "rows": 40,
            "involvement_logit_semantics": grammar_a_baseline::full_a_v2::LOGIT_SEMANTICS,
            "bundle_sha256": digests, "raw_sha256": sha256_file(&raw.join("raw.jsonl")).unwrap(),
            "fit_sha256": fit_sha, "selected_checkpoint_sha256": "ck", "contract_sha256": contract_sha,
            "arm": "E1", "seed": 17});
        write(&raw.join("manifest.json"), &raw_manifest);
        let selection = json!({"schema": "slop_ninja_full_a_selection_v2", "complete": true,
            "final_test_opened": false, "calibration_read_for_selection": false, "selection_split": "dev_select",
            "bundle_sha256": digests, "runs": [{"arm": "E1", "seed": 17, "fit_sha256": fit_sha,
                                                "selected_checkpoint_sha256": "ck"}]});
        write(&d.join("selection.json"), &selection);
        write(&d.join("policy.json"), &json!({"schema": "slop_ninja_involvement_calibration_policy_v2",
            "primary": "isotonic_bounded_v2",
            "bound": "epsilon=1/(2*(min(human_rows,positive_rows)+1)) over the fitting rows; every PAV level clamped to [epsilon,1-epsilon]",
            "fallback": "involvement_temperature_v1", "selection": "isotonic_unless_recorded_mechanical_failure",
            "min_human_rows": 10, "min_positive_rows": 10, "min_human_families": 5, "min_positive_families": 5,
            "crossfit_folds": 5}));
        let args = |out: &str| -> Vec<PathBuf> {
            vec![bundle.clone(), config.clone(), fit.clone(), raw.clone(), d.join("selection.json"),
                 d.join("policy.json"), d.join(out)]
        };
        run(&args("cal.json")).unwrap();
        let c = read(&d.join("cal.json")).unwrap();
        assert_eq!((c["schema"].as_str(), c["fit_split"].as_str()), (Some(CALIBRATION_SCHEMA), Some("cal")));
        assert_eq!(c["roles"]["human"]["roster_empty"], true);
        assert_eq!(c["roles"]["ai"]["qualified"], true);
        assert_eq!(c["cal_rows"], 40);
        assert!(run(&args("cal.json")).is_err(), "existing output");
        // A selection that read CAL, or a raw run of another checkpoint, is refused.
        write(&d.join("selection.json"), &json!(dict_with(&selection, "calibration_read_for_selection", json!(true))));
        assert!(run(&args("c2.json")).is_err());
        write(&d.join("selection.json"), &selection);
        write(&raw.join("manifest.json"), &dict_with(&raw_manifest, "selected_checkpoint_sha256", json!("other")));
        assert!(run(&args("c3.json")).is_err());
        write(&raw.join("manifest.json"), &dict_with(&raw_manifest, "partition", json!("dev_iter")));
        assert!(run(&args("c4.json")).is_err());
        // Policy v3 (protocol v1.2) with the calibrated CAL predictions.
        write(&raw.join("manifest.json"), &raw_manifest);
        let v3 = grammar_a_baseline::involvement_calibration::Policy {
            version: PolicyVersion::V3, min_human_rows: 0, min_positive_rows: 0, min_human_families: 0,
            min_positive_families: 0, folds: 2 };
        write(&d.join("policy.json"), &v3.description());
        let mut a = args("c5.json");
        a.push(d.join("c5.jsonl"));
        run(&a).unwrap();
        let c = read(&d.join("c5.json")).unwrap();
        assert_eq!(c["involvement"]["selected"]["method"], "isotonic_v1_clipped");
        assert_eq!(c["involvement"]["selected"]["lo"], json!(1.0 / 40.0));
        let map = Map::from_description(&c["involvement"]["selected"]).unwrap();
        let lines = fs::read_to_string(d.join("c5.jsonl")).unwrap();
        let preds: Vec<Value> = lines.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(preds.len(), 40);
        for (i, q) in preds.iter().enumerate() {
            let g = if i < 20 { -2.0 + 0.05 * i as f64 } else { 1.0 + 0.05 * i as f64 };
            let (inv, _) = map.apply_origin(&[0.0, g, g - 30.0], g).unwrap();
            let sum = q["p_human"].as_f64().unwrap() + q["p_model_only"].as_f64().unwrap() + q["p_mixed"].as_f64().unwrap();
            assert!((sum - 1.).abs() < 1e-12 && (1. - q["p_human"].as_f64().unwrap() - inv).abs() < 1e-12);
            let share = q["share"].as_f64().unwrap();
            assert!(share >= q["p_model_only"].as_f64().unwrap() - 1e-12 && share <= inv + 1e-12);
            assert_eq!(q["calibrated"], true);
        }
        assert!(run(&a).is_err(), "existing outputs");
    }

    fn dict_with(v: &Value, key: &str, value: Value) -> Value {
        let mut v = v.clone();
        v[key] = value;
        v
    }
}

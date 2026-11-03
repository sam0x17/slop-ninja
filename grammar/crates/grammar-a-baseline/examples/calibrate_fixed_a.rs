//! CAL calibration of a fixed comparator (frozen V6, frozen stage-3 adapted A)
//! under the same protocol rule as `calibrate_full_a` policy v3.
//!
//! calibrate_fixed_a BUNDLE RAW_CAL_RUN COMPARATOR.json POLICY.json NEW_CALIBRATION.json
//!     NEW_CALIBRATED_CAL_PREDICTIONS.jsonl
//!
//! The fixed comparators have no full-A v2 fit or selection record: their
//! weights are frozen and recorded in COMPARATOR.json, which also names the raw
//! run's manifest schema, the AI and human rosters, which heads were trained,
//! the share mode, and manifest fields that must hold exact values. The raw run
//! (one uncalibrated CAL pass of the frozen weights over BUNDLE, rows in the
//! `export_full_a.py raw` row format) is bound to the bundle digests and its own
//! raw.jsonl digest.
//!
//! The involvement map, the calibrated rows and the AI-role named threshold use
//! exactly the code of `calibrate_full_a`: `select_protocol` (origin
//! temperature, then `isotonic_v1_clipped`), the same calibrated-row
//! projection, and the same stage-3 named-threshold objective, where a CAL
//! positive's generator label is known iff it is in the comparator's AI roster
//! verbatim. A nonempty human roster cannot be fitted on CAL rows that carry no
//! human identities: the role is recorded unqualified (always `other_human`).
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::full_a_v2::{CAL_SPLIT, LOGIT_SEMANTICS, RawRun, RowFacts, joined, split_rows};
use grammar_a_baseline::involvement_calibration::{Map, OriginObservation, Policy, PolicyVersion, select_protocol};
use grammar_a_baseline::mining::bundle_digests;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

const SCHEMA: &str = "slop_ninja_a_comp_fixed_calibration_v1";
const COMPARATOR_SCHEMA: &str = "slop_ninja_a_comp_fixed_comparator_v1";
const RAW_SCHEMAS: [&str; 2] = ["slop_ninja_rc_v6_control_raw_v1", "slop_ninja_a_comp_fixed_raw_v1"];

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

// Same objective as calibrate_full_a.
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

// Same AI-role named threshold as calibrate_full_a.
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
        "tie_break":"higher_threshold","objective_value":null,"head_trained":trained});
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

// Same projection as calibrate_full_a.
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

fn strings(v: &Value, what: &str) -> Result<Vec<String>> {
    v.as_array().with_context(|| what.to_owned())?.iter()
        .map(|x| Ok(x.as_str().with_context(|| what.to_owned())?.to_owned())).collect()
}

fn fixed_raw(raw: &Path, bundle: &Path, comparator: &Value) -> Result<RawRun> {
    let manifest = read(&raw.join("manifest.json"))?;
    let schema = manifest["schema"].as_str().context("raw schema")?;
    ensure!(RAW_SCHEMAS.contains(&schema) && comparator["raw_manifest_schema"] == schema,
            "raw manifest schema {schema} is not the comparator's");
    ensure!(manifest["complete"] == true && manifest["partition"] == CAL_SPLIT && manifest["calibrated"] == false &&
            manifest["final_test_opened"] == false, "{} is not a complete uncalibrated CAL run", raw.display());
    ensure!(manifest["bundle_sha256"] == bundle_digests(bundle)? &&
            manifest["raw_sha256"].as_str() == Some(sha(&raw.join("raw.jsonl"))?.as_str()),
            "raw CAL run is not bound to this bundle or changed");
    for (k, v) in comparator["raw_manifest_checks"].as_object().context("raw_manifest_checks")? {
        ensure!(&manifest[k] == v, "raw manifest {k} differs from the comparator record");
    }
    let mut rows = BTreeMap::new();
    for line in BufReader::new(fs::File::open(raw.join("raw.jsonl"))?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line)?;
        let id = v["id"].as_str().context("raw id")?.to_owned();
        ensure!(v["involvement_logit"].as_f64().is_some_and(f64::is_finite), "raw {id}: involvement logit");
        ensure!(rows.insert(id.clone(), v).is_none(), "duplicate raw row {id}");
    }
    ensure!(manifest["rows"].as_u64() == Some(rows.len() as u64), "raw row count differs from its manifest");
    Ok(RawRun { manifest_sha256: sha(&raw.join("manifest.json"))?, manifest, rows })
}

fn run(a: &[PathBuf]) -> Result<()> {
    let [bundle, raw_dir, comparator_path, policy_path, out, preds_out] = a else {
        anyhow::bail!("BUNDLE RAW_CAL_RUN COMPARATOR.json POLICY.json NEW_CALIBRATION.json NEW_CALIBRATED.jsonl")
    };
    ensure!(!out.exists() && !preds_out.exists(), "new outputs required");
    let comparator = read(comparator_path)?;
    ensure!(comparator["schema"] == COMPARATOR_SCHEMA, "fixed comparator record required");
    let ai = strings(&comparator["ai_profiles"], "ai_profiles")?;
    let human = strings(&comparator["human_profiles"], "human_profiles")?;
    let share_mode = comparator["share_mode"].as_str().context("share mode")?;
    let trained = &comparator["trained_heads"];
    ensure!(trained["origin"] == true, "calibration needs a trained origin head");
    let raw = fixed_raw(raw_dir, bundle, &comparator)?;
    if raw.manifest.get("involvement_logit_semantics").is_some() {
        ensure!(raw.manifest["involvement_logit_semantics"] == LOGIT_SEMANTICS, "involvement logit semantics");
    }
    let policy = Policy::from_json(&read(policy_path)?)?;
    ensure!(policy.version == PolicyVersion::V3, "protocol v1.2 calibration (policy v3) required");
    let rows = split_rows(bundle, CAL_SPLIT)?;
    let joined = joined(&rows, &raw)?;
    let mut observations = Vec::new();
    for (r, p) in &joined {
        let l = p["origin_logits"].as_array().context("origin logits")?.iter()
            .map(|v| v.as_f64().context("origin logit")).collect::<Result<Vec<_>>>()?;
        ensure!(l.len() == 3, "three origin logits");
        ensure!(p["ai_logits"].as_array().map(Vec::len) == Some(ai.len()), "raw {}: AI roster width", r.id);
        observations.push(OriginObservation { family: r.source_group.clone(), origin: r.origin_index()?,
                                              logits: [l[0], l[1], l[2]],
                                              logit: p["involvement_logit"].as_f64().context("logit")? });
    }
    let (map, involvement) = select_protocol(&observations, &policy)?;
    let human_role = if human.is_empty() {
        json!({"roster_empty": true, "qualified": false, "threshold": 1.0,
               "rule": "empty roster: the role always returns other_human; no named threshold is fitted"})
    } else {
        json!({"roster_empty": false, "roster_size": human.len(), "qualified": false, "threshold": 1.0,
               "rule": "CAL rows carry no human identities for this roster; no named threshold can be fitted on CAL, so the role always returns other_human"})
    };
    let roles = json!({"human": human_role, "ai": ai_threshold(&joined, &ai, trained["ai"] == true)?});
    let report = json!({"schema":SCHEMA,"complete":true,"fit_split":CAL_SPLIT,"final_test_opened":false,
        "comparator":comparator["name"],"seed":comparator["seed"],"comparator_sha256":sha(comparator_path)?,
        "bundle_sha256":bundle_digests(bundle)?,"raw_manifest_sha256":raw.manifest_sha256,
        "policy_sha256":sha(policy_path)?,"cal_rows":rows.len(),
        "involvement":{"selected":map.description(),"report":involvement},"roles":roles,
        "implementation_sha256":hex::encode(Sha256::digest(include_bytes!("calibrate_fixed_a.rs"))),
        "library_sha256":hex::encode(Sha256::digest(include_bytes!("../src/involvement_calibration.rs"))),
        "limits":["CAL scores are fitted diagnostics, never independent evaluation.",
            "Share is recomputed from the calibrated involvement and the frozen share rule.",
            "Pangram content fractions are not targets for this calibration.",
            "Weights are frozen; only the CAL map, operating points and named thresholds are fitted here."]});
    let mut f = fs::OpenOptions::new().create_new(true).write(true).open(out)?;
    serde_json::to_writer_pretty(&mut f, &report)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    let mut w = std::io::BufWriter::new(fs::OpenOptions::new().create_new(true).write(true).open(preds_out)?);
    for (r, p) in &joined {
        serde_json::to_writer(&mut w, &calibrated_row(&r.id, p, &map, share_mode, &ai)?)?;
        w.write_all(b"\n")?;
    }
    w.into_inner()?.sync_all()?;
    println!("{}", json!({"comparator":comparator["name"],"seed":comparator["seed"],"method":map.method(),
        "ai_qualified":report["roles"]["ai"]["qualified"],"ai_threshold":report["roles"]["ai"]["threshold"]}));
    Ok(())
}

fn main() -> Result<()> {
    run(&std::env::args_os().skip(1).map(PathBuf::from).collect::<Vec<_>>())
}

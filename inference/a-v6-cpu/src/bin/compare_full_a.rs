//! Compare a Rust full-A diagnostic run with the export's Python references.
//! EXPORT_DIR RUST_RUN NEW_REPORT.json [--local] [--fp32]
//!
//! The reference path is keyed off the export's declared schema only.
//!
//! Export v2 (the R0 release package, heads v1): the recorded synthetic
//! reference predictions of the export (`reference-predictions.json`, the
//! trainer's FP32 function on the reference device, raw h as `h_raw_f32`),
//! with the original gates and report (`slop_ninja_full_a_replay_comparison_v1`)
//! unchanged; `--local` and `--fp32` do not apply.
//!
//! Export v4 (spans-S2 span tower) is compared exactly as v3; the Rust run must
//! also bind the export's tower file (receipt `tower_sha256`, equal to the
//! manifest's digest and to the digest of the tower file read from the export
//! now) and the tower runtime source (`tower_runtime_sha256`).
//!
//! Export v3: `--local` compares the export's local (private Dev-iter) reference set; the
//! default is the synthetic reference set. The gate reference is the export's
//! FP64 Python reference: the trained function (HF's float32 rotary tables,
//! widened) with every other operation in FP64; `--fp32` compares the FP32
//! trainer function instead, as a drift diagnostic (same gates, reported). Gates: raw h 1e-4, shared z 1e-3,
//! document outputs on the 0..1 scale 1e-5, involvement logit 1e-4, token mean
//! 1e-5 (unchanged); spans-v1 (protocol R1/R2): token offsets exactly equal
//! (R2); token, raw and tilted word probabilities, sentence shares and
//! confidences and span shares and confidences each 1e-5 (0..1 scale); word,
//! sentence and span offsets exactly equal, except that a case whose decision
//! values lie within 1e-6 of a decoder threshold (either side) is listed and
//! excluded from span offset equality only.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

/// Export manifest value naming the FP64 gate reference's rotary tables.
const REFERENCE_ROTARY: &str = "hf_float32_tables_widened_v1";

fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}
fn num(v: &Value) -> Result<f64> {
    v.as_f64()
        .filter(|x| x.is_finite())
        .context("finite number")
}
fn max_abs(a: &Value, b: &Value) -> Result<f64> {
    let (a, b) = (
        a.as_array().context("array")?,
        b.as_array().context("array")?,
    );
    ensure!(a.len() == b.len() && !a.is_empty(), "vector length differs");
    a.iter()
        .zip(b)
        .try_fold(0f64, |m, (x, y)| Ok(m.max((num(x)? - num(y)?).abs())))
}
/// Like max_abs, for arrays with nulls at the same positions.
fn max_abs_optional(a: &Value, b: &Value) -> Result<Option<f64>> {
    let (a, b) = (
        a.as_array().context("array")?,
        b.as_array().context("array")?,
    );
    if a.len() != b.len() {
        return Ok(None);
    }
    let mut m = 0f64;
    for (x, y) in a.iter().zip(b) {
        match (x.is_null(), y.is_null()) {
            (true, true) => {}
            (false, false) => m = m.max((num(x)? - num(y)?).abs()),
            _ => return Ok(None),
        }
    }
    Ok(Some(m))
}
fn outputs(p: &Value) -> Result<Vec<f64>> {
    let mut v = ["human_only", "model_only", "mixed"]
        .iter()
        .map(|k| num(&p["origin"][*k]))
        .collect::<Result<Vec<_>>>()?;
    v.push(num(&p["ai_confidence_percent"])? / 100.);
    v.push(num(&p["ai_share_percent"])? / 100.);
    for role in ["human", "ai"] {
        v.push(num(&p["attribution"][role]["involvement_probability"])?);
        for s in p["attribution"][role]["conditional_named_scores"]
            .as_object()
            .context("scores")?
            .values()
        {
            v.push(num(s)?);
        }
    }
    Ok(v)
}
const OFFSETS: [&str; 4] = ["start_char", "end_char", "start_byte", "end_byte"];

/// (offsets equal, max abs error of share and confidence on the 0..1 scale).
fn rows(a: &Value, b: &Value) -> Result<(bool, f64)> {
    let (a, b) = (
        a.as_array().context("rows")?,
        b.as_array().context("rows")?,
    );
    let mut same = a.len() == b.len();
    let mut err = 0f64;
    for (x, y) in a.iter().zip(b) {
        same &= OFFSETS.iter().chain(["words"].iter()).all(|k| x[*k] == y[*k]);
        if OFFSETS.iter().all(|k| x[*k] == y[*k]) {
            for k in ["ai_share_percent", "ai_confidence_percent"] {
                err = err.max((num(&x[k])? - num(&y[k])?).abs() / 100.);
            }
        }
    }
    Ok((same, err))
}

#[derive(Default)]
struct Spans {
    cases: usize,
    token: f64,
    word_raw: f64,
    word: f64,
    sentence: f64,
    span: f64,
    offsets_equal: bool,
    span_offsets_equal: bool,
    near_threshold: Vec<String>,
    status_match: bool,
    spans_compared: usize,
    spans_reference: usize,
}

/// The comparison a manifest selects.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    /// Export v2: the recorded FP32 reference predictions, original report.
    V2,
    /// Export v3: (requests key, predictions key, reference precision).
    V3(&'static str, &'static str, &'static str),
}

fn plan(manifest: &Value, local: bool, fp32: bool) -> Result<Plan> {
    match manifest["schema"].as_str() {
        Some("slop_ninja_full_a_export_v2") => {
            ensure!(
                !local && !fp32,
                "a v2 export has only its recorded synthetic reference predictions (no --local or --fp32)"
            );
            Ok(Plan::V2)
        }
        Some("slop_ninja_full_a_export_v3") | Some("slop_ninja_full_a_export_v4") => {
            let (req_key, pred_key) = match (local, fp32) {
                (true, false) => ("local_reference_requests", "local_reference_predictions"),
                (true, true) => ("local_reference_requests", "local_reference_predictions_fp32"),
                (false, false) => ("reference_requests", "reference_predictions"),
                (false, true) => ("reference_requests", "reference_predictions_fp32"),
            };
            ensure!(
                fp32 || manifest["reference_precision"] == "fp64",
                "the gate reference must be the FP64 Python reference"
            );
            // The gate reference is the trained function in FP64: HF's float32 rotary
            // tables widened, never angles recomputed in float64 (PROTOCOL-v1.1 A1).
            ensure!(
                fp32 || manifest["reference_rotary"] == REFERENCE_ROTARY,
                "the FP64 gate reference must use HF's float32 rotary tables ({REFERENCE_ROTARY})"
            );
            Ok(Plan::V3(req_key, pred_key, if fp32 { "fp32" } else { "fp64" }))
        }
        _ => anyhow::bail!("unsupported full-A export schema"),
    }
}

/// Export v2: the original comparison, unchanged.
fn compare_v2(export: &Path, run: &Path, out: &Path, manifest: &Value) -> Result<()> {
    let receipt = read(&run.join("receipt.json"))?;
    ensure!(
        receipt["schema"] == "slop_ninja_full_a_rust_run_v1"
            && receipt["complete"] == true
            && receipt["bindings"]["export_manifest_sha256"] == sha(&export.join("manifest.json"))?
            && receipt["bindings"]["requests_sha256"] == manifest["reference_requests"]["sha256"]
            && sha(&export.join("reference-predictions.json"))?
                == manifest["reference_predictions"]["sha256"]
            && receipt["predictions_sha256"] == sha(&run.join("predictions.jsonl"))?,
        "run does not bind this export's reference requests"
    );
    let mut rust = BTreeMap::new();
    for line in fs::read_to_string(run.join("predictions.jsonl"))?.lines() {
        let v: Value = serde_json::from_str(line)?;
        rust.insert(v["id"].as_str().context("id")?.to_owned(), v);
    }
    let mut cases = Vec::new();
    let (mut h_err, mut z_err, mut out_err, mut g_err, mut m_err) = (0f64, 0f64, 0f64, 0f64, 0f64);
    let mut labels_match = true;
    for r in read(&export.join("reference-predictions.json"))?
        .as_array()
        .context("references")?
    {
        let id = r["id"].as_str().context("reference id")?;
        let x = rust.get(id).context("reference missing from Rust run")?;
        ensure!(
            x["status"] == "ok" && x["text_sha256"] == r["text_sha256"],
            "reference case unsupported/unbound"
        );
        let d = &x["diagnostics"];
        ensure!(
            d["token_ids"] == r["token_ids"],
            "token ids differ for {id}"
        );
        if !r["word_mask"].is_null() {
            ensure!(
                d["word_mask"] == r["word_mask"],
                "word-token masks differ for {id}"
            );
        }
        let h = max_abs(&d["h_raw"], &r["h_raw_f32"])?;
        let z = max_abs(&d["z_shared_f64"], &r["z_shared"])?;
        let o = outputs(&x["prediction"])?
            .iter()
            .zip(outputs(&r["prediction"])?)
            .try_fold(0f64, |m, (p, q)| {
                Ok::<_, anyhow::Error>(m.max((p - q).abs()))
            })?;
        let g = (num(&d["raw"]["involvement_logit"])? - num(&r["raw"]["involvement_logit"])?).abs();
        let m = match (&d["raw"]["token_mean"], &r["raw"]["token_mean"]) {
            (Value::Null, Value::Null) => 0.,
            (p, q) => (num(p)? - num(q)?).abs(),
        };
        for role in ["human", "ai"] {
            labels_match &= x["prediction"]["attribution"][role]["profile"]
                == r["prediction"]["attribution"][role]["profile"];
        }
        h_err = h_err.max(h);
        z_err = z_err.max(z);
        out_err = out_err.max(o);
        g_err = g_err.max(g);
        m_err = m_err.max(m);
        cases.push(json!({"id":id,"tokens":r["token_ids"].as_array().map(|v| v.len()),"raw_h":h,"shared_z":z,
            "outputs":o,"involvement_logit":g,"token_mean":m}));
    }
    ensure!(!cases.is_empty(), "no reference cases");
    let passed = h_err <= 1e-4
        && z_err <= 1e-3
        && out_err <= 1e-5
        && g_err <= 1e-4
        && m_err <= 1e-5
        && labels_match;
    let report = json!({"schema":"slop_ninja_full_a_replay_comparison_v1","complete":true,"passed":passed,
        "export_manifest_sha256":sha(&export.join("manifest.json"))?,"run_receipt_sha256":sha(&run.join("receipt.json"))?,
        "fixed_gates":{"raw_h":1e-4,"shared_z":1e-3,"outputs_on_0_1_scale":1e-5,"involvement_logit":1e-4,"token_mean":1e-5},
        "max_errors":{"raw_h":h_err,"shared_z":z_err,"outputs":out_err,"involvement_logit":g_err,"token_mean":m_err},
        "attribution_labels_match":labels_match,"cases":cases,
        "rust_numeric_profile":receipt["numeric_profile"],
        "limits":["Synthetic reference cases only; not all supported inputs.","Reference device is recorded in the export."]});
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(out)?;
    serde_json::to_writer_pretty(&mut f, &report)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!(
        "{}",
        json!({"passed":passed,"max_errors":report["max_errors"]})
    );
    ensure!(passed, "replay gates failed; report retained");
    Ok(())
}

/// A v4 export's run binds its tower file (the manifest's digest, which the file read from the export
/// now must also have) and the tower runtime source; a v2/v3 run has no tower binding.
fn tower_bound(manifest: &Value, receipt: &Value, tower_file_sha256: Option<&str>) -> bool {
    let tower = receipt["bindings"].get("tower_sha256");
    if manifest["schema"] == "slop_ninja_full_a_export_v4" {
        manifest["span_tower"]["sha256"].is_string()
            && tower_file_sha256.is_some_and(|d| manifest["span_tower"]["sha256"] == d)
            && tower == Some(&manifest["span_tower"]["sha256"])
            && receipt["bindings"]["tower_runtime_sha256"].is_string()
            && receipt["span_tower"] == true
    } else {
        tower.is_none() && receipt.get("span_tower").is_none()
    }
}

/// The digest of a v4 export's tower file as read from the export now (a plain file name inside it); no
/// other export schema has a tower file.
fn tower_file_sha256(export: &Path, manifest: &Value) -> Result<Option<String>> {
    if manifest["schema"] != "slop_ninja_full_a_export_v4" {
        return Ok(None);
    }
    let name = manifest["span_tower"]["file"].as_str().context("span tower file")?;
    ensure!(
        !name.is_empty()
            && Path::new(name)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
        "unsafe span tower file"
    );
    Ok(Some(sha(&export.join(name))?))
}

fn main() -> Result<()> {
    let a = std::env::args_os().skip(1).collect::<Vec<_>>();
    let flags = a.get(3..).unwrap_or_default();
    ensure!(
        a.len() >= 3
            && flags.iter().all(|f| f == "--local" || f == "--fp32")
            && flags.iter().collect::<std::collections::BTreeSet<_>>().len() == flags.len(),
        "EXPORT_DIR RUST_RUN NEW_REPORT.json [--local] [--fp32]"
    );
    let local = flags.iter().any(|f| f == "--local");
    let fp32 = flags.iter().any(|f| f == "--fp32");
    let (export, run, out) = (Path::new(&a[0]), Path::new(&a[1]), Path::new(&a[2]));
    ensure!(!out.exists(), "new report required");
    let manifest = read(&export.join("manifest.json"))?;
    let (req_key, pred_key, precision) = match plan(&manifest, local, fp32)? {
        Plan::V2 => return compare_v2(export, run, out, &manifest),
        Plan::V3(r, p, precision) => (r, p, precision),
    };
    let pred_file = manifest[pred_key]["file"]
        .as_str()
        .context("reference predictions member")?;
    let receipt = read(&run.join("receipt.json"))?;
    ensure!(
        receipt["schema"] == "slop_ninja_full_a_rust_run_v1"
            && receipt["complete"] == true
            && receipt["bindings"]["export_manifest_sha256"] == sha(&export.join("manifest.json"))?
            && receipt["bindings"]["requests_sha256"] == manifest[req_key]["sha256"]
            && sha(&export.join(pred_file))? == manifest[pred_key]["sha256"]
            && receipt["predictions_sha256"] == sha(&run.join("predictions.jsonl"))?,
        "run does not bind this export's reference requests"
    );
    ensure!(
        tower_bound(&manifest, &receipt, tower_file_sha256(export, &manifest)?.as_deref()),
        "a v4 run must bind the export's span tower file, and only a v4 run does"
    );
    let mut rust = BTreeMap::new();
    for line in fs::read_to_string(run.join("predictions.jsonl"))?.lines() {
        let v: Value = serde_json::from_str(line)?;
        rust.insert(v["id"].as_str().context("id")?.to_owned(), v);
    }
    let mut cases = Vec::new();
    let (mut h_err, mut z_err, mut out_err, mut g_err, mut m_err) = (0f64, 0f64, 0f64, 0f64, 0f64);
    let mut labels_match = true;
    let mut token_offsets_equal = true;
    let mut sp = Spans {
        offsets_equal: true,
        span_offsets_equal: true,
        status_match: true,
        ..Default::default()
    };
    for r in read(&export.join(pred_file))?
        .as_array()
        .context("references")?
    {
        let id = r["id"].as_str().context("reference id")?;
        let x = rust.get(id).context("reference missing from Rust run")?;
        ensure!(
            x["status"] == "ok"
                && x["text_sha256"] == r["text_sha256"]
                && r["reference_precision"] == precision,
            "reference case unsupported/unbound or of another precision"
        );
        let d = &x["diagnostics"];
        ensure!(
            d["token_ids"] == r["token_ids"],
            "token ids differ for {id}"
        );
        if !r["word_mask"].is_null() {
            ensure!(
                d["word_mask"] == r["word_mask"],
                "word-token masks differ for {id}"
            );
        }
        let offsets_same = d["token_offsets"] == r["token_offsets"];
        token_offsets_equal &= offsets_same;
        let h = max_abs(&d["h_raw"], &r["h_raw"])?;
        let z = max_abs(&d["z_shared_f64"], &r["z_shared"])?;
        let o = outputs(&x["prediction"])?
            .iter()
            .zip(outputs(&r["prediction"])?)
            .try_fold(0f64, |m, (p, q)| {
                Ok::<_, anyhow::Error>(m.max((p - q).abs()))
            })?;
        let g = (num(&d["raw"]["involvement_logit"])? - num(&r["raw"]["involvement_logit"])?).abs();
        let m = match (&d["raw"]["token_mean"], &r["raw"]["token_mean"]) {
            (Value::Null, Value::Null) => 0.,
            (p, q) => (num(p)? - num(q)?).abs(),
        };
        for role in ["human", "ai"] {
            labels_match &= x["prediction"]["attribution"][role]["profile"]
                == r["prediction"]["attribution"][role]["profile"];
        }
        h_err = h_err.max(h);
        z_err = z_err.max(z);
        out_err = out_err.max(o);
        g_err = g_err.max(g);
        m_err = m_err.max(m);
        let mut case = json!({"id":id,"tokens":r["token_ids"].as_array().map(|v| v.len()),"raw_h":h,"shared_z":z,
            "outputs":o,"involvement_logit":g,"token_mean":m,"token_offsets_equal":offsets_same});
        let (re, xe) = (&r["span_emission"], &d["span_emission"]);
        if !re.is_null() {
            sp.cases += 1;
            let status = re["status"] == xe["status"];
            sp.status_match &= status;
            if status && re["status"] == "ok" {
                let (rd, xd) = (&re["diagnostics"], &xe["diagnostics"]);
                let token = max_abs_optional(&rd["token_probabilities"], &xd["token_probabilities"])?;
                let words_same = rd["word_offsets"] == xd["word_offsets"];
                let (raw, tilted) = if words_same {
                    (
                        max_abs(&rd["word_probabilities_raw"], &xd["word_probabilities_raw"])?,
                        max_abs(&rd["word_probabilities"], &xd["word_probabilities"])?,
                    )
                } else {
                    (f64::INFINITY, f64::INFINITY)
                };
                let (sent_same, sent_err) =
                    rows(&r["prediction"]["sentences"], &x["prediction"]["sentences"])?;
                let near = re["near_threshold"] == true || xe["near_threshold"] == true;
                let (span_same, span_err) = rows(&r["prediction"]["spans"], &x["prediction"]["spans"])?;
                if near {
                    sp.near_threshold.push(id.to_owned());
                } else {
                    sp.span_offsets_equal &= span_same;
                }
                sp.offsets_equal &= words_same && sent_same && token.is_some();
                sp.token = sp.token.max(token.unwrap_or(f64::INFINITY));
                sp.word_raw = sp.word_raw.max(raw);
                sp.word = sp.word.max(tilted);
                sp.sentence = sp.sentence.max(sent_err);
                sp.span = sp.span.max(span_err);
                let n_ref = r["prediction"]["spans"].as_array().map_or(0, |v| v.len());
                sp.spans_reference += n_ref;
                if span_same {
                    sp.spans_compared += n_ref;
                }
                case["spans"] = json!({"token_probability":token,"word_probability_raw":raw,
                    "word_probability":tilted,"sentence":sent_err,"span":span_err,"word_offsets_equal":words_same,
                    "sentence_offsets_equal":sent_same,"span_offsets_equal":span_same,"near_threshold":near,
                    "reference_spans":n_ref,"rust_spans":x["prediction"]["spans"].as_array().map(|v| v.len())});
            } else {
                case["spans"] = json!({"status_reference":re["status"],"status_rust":xe["status"]});
            }
        }
        cases.push(case);
    }
    ensure!(!cases.is_empty(), "no reference cases");
    let document_passed = h_err <= 1e-4
        && z_err <= 1e-3
        && out_err <= 1e-5
        && g_err <= 1e-4
        && m_err <= 1e-5
        && labels_match;
    let spans_passed = sp.cases == 0
        || (sp.status_match
            && sp.offsets_equal
            && sp.span_offsets_equal
            && sp.token <= 1e-5
            && sp.word_raw <= 1e-5
            && sp.word <= 1e-5
            && sp.sentence <= 1e-5
            && sp.span <= 1e-5);
    let passed = document_passed && token_offsets_equal && spans_passed;
    let report = json!({"schema":"slop_ninja_full_a_replay_comparison_v2","complete":true,"passed":passed,
        "export_schema":manifest["schema"],"span_tower_sha256":manifest["span_tower"]["sha256"],
        "reference_set":if local {"local"} else {"synthetic"},"reference_precision":precision,
        "reference_rotary":manifest["reference_rotary"],
        "role":if fp32 {"fp32_drift_diagnostic"} else {"gate"},
        "document_passed":document_passed,"tokenizer_offsets_passed":token_offsets_equal,"spans_passed":spans_passed,
        "export_manifest_sha256":sha(&export.join("manifest.json"))?,"run_receipt_sha256":sha(&run.join("receipt.json"))?,
        "fixed_gates":{"raw_h":1e-4,"shared_z":1e-3,"outputs_on_0_1_scale":1e-5,"involvement_logit":1e-4,"token_mean":1e-5,
            "token_probability":1e-5,"word_probability":1e-5,"sentence_share_and_confidence":1e-5,
            "span_share_and_confidence":1e-5,"offsets":"exact; span offsets exempt within 1e-6 of a decoder threshold"},
        "max_errors":{"raw_h":h_err,"shared_z":z_err,"outputs":out_err,"involvement_logit":g_err,"token_mean":m_err,
            "token_probability":sp.token,"word_probability_raw":sp.word_raw,"word_probability":sp.word,
            "sentence":sp.sentence,"span":sp.span},
        "span_cases":sp.cases,"span_status_match":sp.status_match,"word_and_sentence_offsets_equal":sp.offsets_equal,
        "span_offsets_equal_outside_threshold_cases":sp.span_offsets_equal,
        "near_threshold_cases":sp.near_threshold,"reference_spans":sp.spans_reference,
        "reference_spans_with_equal_offsets":sp.spans_compared,
        "attribution_labels_match":labels_match,"cases":cases,
        "rust_numeric_profile":receipt["numeric_profile"],
        "limits":["Reference cases only; not all supported inputs.","Reference device is recorded in the export."]});
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(out)?;
    serde_json::to_writer_pretty(&mut f, &report)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!(
        "{}",
        json!({"passed":passed,"document_passed":document_passed,"spans_passed":spans_passed,
            "tokenizer_offsets_passed":token_offsets_equal,"max_errors":report["max_errors"],
            "near_threshold_cases":report["near_threshold_cases"]})
    );
    ensure!(passed, "replay gates failed; report retained");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_schema_selects_the_reference_path() {
        let v2 = json!({"schema":"slop_ninja_full_a_export_v2","reference_device":"cuda"});
        assert_eq!(plan(&v2, false, false).unwrap(), Plan::V2);
        for (local, fp32) in [(true, false), (false, true), (true, true)] {
            assert!(plan(&v2, local, fp32).is_err());
        }
        let v3 = json!({"schema":"slop_ninja_full_a_export_v3","reference_precision":"fp64",
            "reference_rotary":REFERENCE_ROTARY});
        assert_eq!(
            plan(&v3, false, false).unwrap(),
            Plan::V3("reference_requests", "reference_predictions", "fp64")
        );
        assert_eq!(
            plan(&v3, true, true).unwrap(),
            Plan::V3("local_reference_requests", "local_reference_predictions_fp32", "fp32")
        );
        // A v3 gate needs the FP64 reference with HF's float32 rotary tables.
        let mut wrong = v3.clone();
        wrong["reference_rotary"] = json!("float64_angles");
        assert!(plan(&wrong, false, false).is_err());
        assert!(plan(&wrong, false, true).is_ok());
        let mut wrong = v3;
        wrong["reference_precision"] = json!("fp32");
        assert!(plan(&wrong, true, false).is_err());
        assert!(plan(&json!({"schema":"slop_ninja_full_a_export_v1"}), false, false).is_err());
        // v4 (span tower) takes the v3 comparison and gates.
        let v4 = json!({"schema":"slop_ninja_full_a_export_v4","reference_precision":"fp64",
            "reference_rotary":REFERENCE_ROTARY,"span_tower":{"sha256":"t"}});
        assert_eq!(
            plan(&v4, true, false).unwrap(),
            Plan::V3("local_reference_requests", "local_reference_predictions", "fp64")
        );
        let bound = json!({"bindings":{"tower_sha256":"t","tower_runtime_sha256":"r"},"span_tower":true});
        assert!(tower_bound(&v4, &bound, Some("t")));
        // The tower file read from the export must have the manifest's digest.
        assert!(!tower_bound(&v4, &bound, Some("u")));
        assert!(!tower_bound(&v4, &bound, None));
        assert!(!tower_bound(&v4, &json!({"bindings":{"tower_sha256":"u","tower_runtime_sha256":"r"},"span_tower":true}), Some("t")));
        assert!(!tower_bound(&v4, &json!({"bindings":{"tower_sha256":"t"},"span_tower":true}), Some("t")));
        assert!(!tower_bound(&v4, &json!({"bindings":{}}), Some("t")));
        let v3 = json!({"schema":"slop_ninja_full_a_export_v3"});
        assert!(tower_bound(&v3, &json!({"bindings":{}}), None));
        assert!(!tower_bound(&v3, &bound, None));
    }
    #[test]
    fn a_v4_tower_file_digest_is_read_from_the_export() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("compare-full-a-tower-{}-{nanos}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("tower.safetensors"), b"tower").unwrap();
        let v4 = |file: &str| json!({"schema":"slop_ninja_full_a_export_v4","span_tower":{"file":file}});
        let got = tower_file_sha256(&dir, &v4("tower.safetensors"));
        let unsafe_names = ["", "../tower.safetensors", "./tower.safetensors"].map(|n| tower_file_sha256(&dir, &v4(n)).is_err());
        let missing = tower_file_sha256(&dir, &v4("missing.safetensors")).is_err();
        let v3 = tower_file_sha256(&dir, &json!({"schema":"slop_ninja_full_a_export_v3","span_tower":{"file":"tower.safetensors"}}));
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(got.unwrap(), Some(hex::encode(Sha256::digest(b"tower"))));
        assert_eq!(unsafe_names, [true; 3]);
        assert!(missing);
        assert_eq!(v3.unwrap(), None);
    }
}

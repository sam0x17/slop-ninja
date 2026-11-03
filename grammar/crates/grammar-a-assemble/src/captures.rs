//! The earlier hosted captures that `docs/a-hosted-output-admission.json`
//! admits by record hash: cohort `frontier-captures-v1` (CLI capture reviews,
//! model-only source-conditioned drafts) and `hosted-style-v1` (span rewrites
//! of stage-3 Train passages). A record enters only when its `text_sha256`
//! and record id are listed in the manifest; its attribution label is the
//! manifest's. Captures have no split-human parent: a capture row is kept in
//! Train only when its source group is a Train or Train-mining source group
//! of the current splits (or a stage-3 legacy Train group) and passes the
//! usual evaluation closure (`select`).
use crate::*;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::HashMap;

pub const ADMISSION_SCHEMA: &str = "slop_ninja_a_hosted_output_admission_v1";

/// One admitted capture: cohort, label, record id, origin.
#[derive(Clone, Debug)]
pub struct Admitted {
    pub cohort: String,
    pub label: String,
    pub record_id: String,
    pub origin: String,
}

/// text_sha256 -> admitted capture.
pub fn admitted(manifest: &Value) -> Result<HashMap<String, Admitted>> {
    ensure!(manifest["schema"] == ADMISSION_SCHEMA, "hosted admission manifest schema");
    let mut out = HashMap::new();
    for (cohort, c) in manifest["cohorts"].as_object().context("cohorts")? {
        for r in c["records"].as_array().into_iter().flatten() {
            let t = str_req(r, "text_sha256")?.to_string();
            let a = Admitted {
                cohort: cohort.clone(),
                label: str_req(&r["identity"], "attribution_label")?.into(),
                record_id: str_req(r, "record_id")?.into(),
                origin: str_req(r, "origin")?.into(),
            };
            ensure!(out.insert(t, a).is_none(), "capture text admitted twice");
        }
    }
    Ok(out)
}

fn base_capture(v: &Value, a: &Admitted, o: &Origin) -> Result<Row> {
    let (text, digest) = {
        let text = str_req(v, "text")?.to_string();
        let d = sha_hex(text.as_bytes());
        ensure!(v["text_sha256"].as_str() == Some(d.as_str()), "capture text digest");
        (text, d)
    };
    let id = format!("capture:{}", a.record_id);
    let mut r = Row {
        schema: ROW_SCHEMA, id, split: "train".into(), text, text_sha256: digest, origin: a.origin.clone(),
        share: 0.0, ai_byte_spans: Vec::new(), generator_label: Some(a.label.clone()), generator_key: None,
        cohort_id: a.cohort.clone(), cohort_weight: 1.0, balanced_weight: 0.0, weight: 0.0,
        source_group: str_req(v, "source_group")?.into(), writer_key: None, domain: String::new(), genre: None,
        collection: None, parent_id: None, voice_id: None, brief_id: None, voice_kind: None, voice_cluster_id: None,
        voice_origin: None, style_axes: json!({}), style_partition: None, operation: String::new(),
        held_out_style: false, population: "frontier".into(), source_kind: SourceKind::HostedCapture,
        provenance_sha256: sha_hex(o.bytes), provenance: json!({"input": o.input, "line": o.line}),
        record_domain: None, style_text_match: None, demographics: None, ocr_tier: None,
        removed_markup_per_100_words: None, block: None,
    };
    r.generator_key = Some(a.record_id.clone());
    r.writer_key = legacy_author(&r.source_group).map(|a| format!("author:{a}"));
    Ok(r)
}

/// A CLI capture review (`slop_ninja_cli_capture_review_v1`): a model-only draft.
pub fn review_row(v: &Value, adm: &HashMap<String, Admitted>, o: &Origin) -> Result<Option<Row>> {
    ensure!(v["schema"] == "slop_ninja_cli_capture_review_v1", "capture review schema");
    let Some(a) = adm.get(str_req(v, "text_sha256")?) else { return Ok(None) };
    ensure!(a.record_id == format!("cli-capture:{}", str_req(v, "request_sha256")?), "capture record id differs from the manifest");
    ensure!(a.origin == "model_only", "capture review origin");
    let mut r = base_capture(v, a, o)?;
    r.share = 1.0;
    r.ai_byte_spans = vec![[0, r.text.len()]];
    r.operation = str_req(v, "operation")?.into();
    r.collection = str_opt(&v["source"], "collection");
    r.record_domain = r.collection.clone();
    check_spans(&r.text, &r.ai_byte_spans)?;
    Ok(Some(r))
}

/// A hosted style rewrite (`slop_ninja_a_supervised_record_v1`, mixed with one replacement span).
pub fn style_row(v: &Value, adm: &HashMap<String, Admitted>, o: &Origin) -> Result<Option<Row>> {
    ensure!(v["schema"] == "slop_ninja_a_supervised_record_v1", "hosted style record schema");
    let Some(a) = adm.get(str_req(v, "text_sha256")?) else { return Ok(None) };
    ensure!(a.record_id == str_req(v, "id")?, "hosted style record id differs from the manifest");
    ensure!(a.origin == "mixed" && v["origin"] == "mixed", "hosted style origin");
    let mut r = base_capture(v, a, o)?;
    let ev = &v["share_evidence"];
    let sp = ev["final_byte_span"].as_array().context("final_byte_span")?;
    ensure!(sp.len() == 2, "final_byte_span");
    r.ai_byte_spans = vec![[sp[0].as_u64().context("span")? as usize, sp[1].as_u64().context("span")? as usize]];
    check_spans(&r.text, &r.ai_byte_spans)?;
    r.share = v["share_target"].as_f64().context("share_target")?;
    ensure!(r.share > 0.0 && r.share <= 1.0, "hosted style share");
    r.operation = "span".into();
    r.parent_id = str_opt(v, "parent_id").map(|p| format!("stage3:{p}"));
    r.collection = str_opt(&v["metadata"], "source_collection");
    r.style_axes = json!({"style_name": v["generation"]["style_name"]});
    Ok(Some(r))
}

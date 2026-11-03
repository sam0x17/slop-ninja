//! Retained per-document predictions of our own detectors (evidence only).
//! Role `predictions`: JSONL lines `{id, p_human, p_mixed, p_model_only, share,
//! attribution, calibrated, ...}`. The detector name comes from `params.detector`
//! (a `v6` path component selects `params.detector_v6`), the version from the
//! file's directory. Texts are joined later by record id (`link`).

use super::util::{f, s, strip_long, without};
use super::Adapter;
use crate::archive::Line;
use crate::runner::Input;
use crate::sink::{Observation, Sink};
use anyhow::Result;
use serde_json::Value;

pub struct Predictions {
    detector: String,
    detector_v6: String,
}

impl Predictions {
    pub fn new(p: &Value) -> Result<Predictions> {
        Ok(Predictions {
            detector: p.get("detector").and_then(|x| x.as_str()).unwrap_or("slop_ninja_a").into(),
            detector_v6: p.get("detector_v6").and_then(|x| x.as_str()).unwrap_or("slop_ninja_v6").into(),
        })
    }
}

impl Adapter for Predictions {
    fn line(&mut self, sink: &mut Sink, input: &Input, l: &Line) -> Result<()> {
        if input.role != "predictions" {
            return Ok(());
        }
        let Some(v) = l.value else { return Ok(()) };
        let Some(id) = s(v, &["id", "record_id", "query_id"]) else { return Ok(()) };
        // Version: the run directory and file stem (one file per arm and seed in some layouts).
        let run = input.rel.strip_suffix(".jsonl").unwrap_or(&input.rel).to_string();
        let det = if run.split('/').any(|c| c.to_lowercase() == "v6" || c.to_lowercase().starts_with("v6-")) { &self.detector_v6 } else { &self.detector };
        let p_model = f(v, &["p_model_only"]);
        let p_mixed = f(v, &["p_mixed"]);
        let p_human = f(v, &["p_human"]);
        sink.observation(Some(&l.at), &Observation {
            obs_key: format!("pred:{}:{}", l.at.file_sha256, l.at.line_no),
            detector: det.clone(),
            detector_version: Some(run.clone()),
            text_sha256: s(v, &["text_sha256"]),
            item_id: Some(id),
            job_ref: Some(input.rel.clone()),
            fraction_ai: f(v, &["share"]),
            fraction_human: p_human,
            score: match (p_model, p_mixed) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                _ => f(v, &["involvement", "p_involvement"]),
            },
            prediction: Some(strip_long(&without(v, &["text"]), 160)),
            status: "success".into(),
            context: Some(sink.source_id.clone()),
            ..Default::default()
        })
    }
}

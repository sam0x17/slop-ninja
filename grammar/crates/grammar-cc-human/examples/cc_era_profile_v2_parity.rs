//! Profile v2 ≡ v1 parity on recorded journals (protocol A2 §6.2's
//! equivalence check; counts, ids and digests only, never text).
//!
//! Part A replays `slop_ninja_cc_era renorm` under `cc_cohort_profile_v1` on
//! one request set's input journals (the same completion order, overlap
//! index, acceptance path and window) and anchors every verdict to the
//! recorded journal's, by request id and attempt. Part B takes every other
//! recorded response (those generated after the re-verdict), runs the same
//! acceptance path on a fresh overlap index and anchors its post-processing
//! record to the recorded one. At every post-processing call of either part,
//! profile v2's child step (`child_v2`) also runs on the same parent, span
//! and raw response with the same conventions. It reports, per generator:
//! * the anchors (parts A and B; part A compares each replayed verdict with
//!   the recorded one whole, byte for byte as JSON values: `Dual` returns
//!   v1's output and is not profile v2's step, so its verdicts keep no
//!   `raw_text`, as the recorded cohort-v3 verdicts keep none; the replayed
//!   verdicts that carry one are counted and must be none);
//! * v2 ≡ v1: the v2 child's bytes, form reasons and conventions equal v1's
//!   (amendment 7 item 7.7's premise, unchanged by amendment 10: on CC
//!   excerpt-layout sources that pass the screen the licence is empty and
//!   there is no frame, so `layout_transfer_v2` and the licence screen give
//!   v1's child and verdict);
//! * the source layout classes, the sources passing the form screen, the
//!   calls where the source and the raw child both use paired emphasis, and
//!   the echo screen's outcomes (a v2 screen that v1 lacks, reported
//!   separately).
//!
//! No model, network or detector call: the acceptance path reads journaled
//! response bodies. The output JSON holds counts, request ids, attempts,
//! classes, reason codes and SHA-256 digests.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_a_positives::{
    generation::{self, PostOut, PostProcess},
    overlap,
    plan::Request,
};
use grammar_cc_human::{era_batch, era_norm, measure, store};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{BufRead, BufReader},
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    batch: PathBuf,
    /// The v1 normaliser prior the recorded run used.
    #[arg(long)]
    norm: PathBuf,
    /// Journals to replay: `<runs>/<generator>/journal-*.jsonl`.
    #[arg(long)]
    runs: PathBuf,
    /// Request set directory (a top-up); default: the batch.
    #[arg(long)]
    requests: Option<PathBuf>,
    /// The recorded profile v1 journals to anchor the replay against.
    #[arg(long)]
    recorded: PathBuf,
    #[arg(long)]
    tokenizer: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

#[derive(Default)]
struct Stats {
    calls: usize,
    text_equal: usize,
    form_equal: usize,
    conventions_equal: usize,
    differ: Vec<Value>,
    classes: BTreeMap<String, usize>,
    paths: BTreeMap<String, usize>,
    emphasis_kept: usize,
    /// Calls whose source uses paired emphasis and whose raw child has some:
    /// the only calls where v2 keeps emphasis that v1 strips (amendment 7
    /// item 7.7).
    emphasis_kept_child_pairs: usize,
    /// Calls whose source passes the form screen (so v2 screens the child).
    source_passes_screen: usize,
    echo_rejected: usize,
    echo_reasons: BTreeMap<String, usize>,
}

struct Dual {
    v1: era_norm::EraPost,
    stats: Mutex<Stats>,
}

impl PostProcess for Dual {
    fn apply(&self, r: &Request, parent: Option<&str>, clean: &str) -> PostOut {
        let o1 = self.v1.apply(r, parent, clean);
        if let Some(p) = parent {
            let role = r.cohort.clone().unwrap_or_default();
            let doc = era_batch::doc_id_of(&r.parent_id, &role);
            let conv = era_norm::resolve(&era_norm::counts(p), &self.v1.prior, &self.v1.seed, &doc);
            let c2 = era_norm::child_v2(p, r.source_span, &conv, clean);
            let form2: Vec<String> = c2.record["form_reasons"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect();
            let mut s = self.stats.lock().expect("stats");
            s.calls += 1;
            let text_eq = c2.text == o1.text;
            let form_eq = form2 == o1.reasons;
            let conv_eq = c2.record["conventions"] == o1.record["conventions"];
            s.text_equal += text_eq as usize;
            s.form_equal += form_eq as usize;
            s.conventions_equal += conv_eq as usize;
            let class = c2.record["source"]["class"].as_str().unwrap_or("none").to_string();
            *s.classes.entry(class.clone()).or_default() += 1;
            *s.paths.entry(c2.record["transfer"]["path"].as_str().unwrap_or("none").to_string()).or_default() += 1;
            s.emphasis_kept += (c2.record["transfer"]["emphasis_kept"] == true) as usize;
            let child_pairs = clean.lines().any(|l| grammar_a_form::measure::emphasis_pairs(l) > 0);
            s.emphasis_kept_child_pairs += (c2.record["transfer"]["emphasis_kept"] == true && child_pairs) as usize;
            s.source_passes_screen += (c2.record["source"]["passes_screen"] == true) as usize;
            if c2.record["echo"]["rejected"] == true {
                s.echo_rejected += 1;
                for x in c2.reasons.iter().filter(|x| x.starts_with("echo_")) {
                    *s.echo_reasons.entry(x.clone()).or_default() += 1;
                }
            }
            if !(text_eq && form_eq && conv_eq) {
                s.differ.push(json!({"request_id": r.request_id, "doc_id": doc, "span": r.source_span.is_some(), "text_equal": text_eq,
                    "form_equal": form_eq, "conventions_equal": conv_eq, "source_class": class, "emphasis_kept": c2.record["transfer"]["emphasis_kept"],
                    "v1_reasons": o1.reasons, "v2_form_reasons": form2, "v1_output_sha256": o1.record["output_sha256"], "v2_output_sha256": c2.record["output_sha256"]}));
            }
        }
        o1
    }

    /// Not profile v2's step: it returns v1's output, so its verdicts are v1
    /// renorm's (no `raw_text`).
    fn is_profile_v2(&self) -> bool {
        false
    }
}

/// A generator directory's journaled responses and the journal files' digests.
struct Journals {
    responses: Vec<Value>,
    files: BTreeMap<String, String>,
}

fn response_lines(dir: &std::path::Path) -> Result<Journals> {
    let mut responses = Vec::new();
    let mut files = BTreeMap::new();
    for p in generation::journal_files(dir)? {
        files.insert(p.display().to_string(), measure::file_sha256(&p)?);
        for line in BufReader::new(fs::File::open(&p)?).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(&line).with_context(|| format!("journal line in {}", p.display()))?;
            match v["kind"].as_str() {
                Some("intent") => {}
                Some("response") => responses.push(v),
                _ => anyhow::bail!("journal kind"),
            }
        }
    }
    Ok(Journals { responses, files })
}

fn main() -> Result<()> {
    let a = Args::parse();
    ensure!(!a.out.exists(), "output exists: {}", a.out.display());
    let set = a.requests.clone().unwrap_or_else(|| a.batch.clone());
    let meta_path = if set.join("TOPUP.json").exists() { set.join("TOPUP.json") } else { set.join("BATCH.json") };
    let meta: Value = store::read_json(&meta_path)?;
    if meta_path.ends_with("TOPUP.json") {
        ensure!(meta["batch_sha256"] == measure::file_sha256(&a.batch.join("BATCH.json"))?.as_str(), "top-up belongs to another batch");
    }
    era_norm::check_profile_prior(era_norm::Profile::CohortV1, Some(&a.norm))?;
    let v1 = era_norm::EraPost::load(&a.norm, &a.batch)?;
    ensure!(v1.v2.is_none(), "a v1 prior is required");
    let parents = era_batch::read_parents(&a.batch)?;
    let ptext: HashMap<String, String> = parents.iter().map(|p| (p.id.clone(), p.text.clone())).collect();
    let mut tok = tokenizers::Tokenizer::from_file(&a.tokenizer).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tok.with_truncation(None).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tok.with_padding(None);
    let mut by = BTreeMap::new();
    let mut all_ok = true;
    for g in meta["request_files"].as_object().context("request_files")?.keys() {
        let rp = set.join("requests").join(format!("{g}.jsonl"));
        ensure!(meta["request_files"][g]["sha256"] == measure::file_sha256(&rp)?.as_str(), "request file changed: {g}");
        let reqs: HashMap<String, Request> = measure::read_jsonl(&rp)?
            .into_iter()
            .map(|v| serde_json::from_value::<Request>(v).map(|r| (r.request_id.clone(), r)))
            .collect::<Result<_, _>>()?;
        let Journals { mut responses, files } = response_lines(&a.runs.join(g))?;
        responses.retain(|v| reqs.contains_key(v["request_id"].as_str().unwrap_or("")));
        responses.sort_by(|x, y| {
            let k = |v: &Value| (v["t_end"].as_u64().unwrap_or(0), v["request_id"].as_str().unwrap_or("").to_string(), v["attempt"].as_u64().unwrap_or(0));
            k(x).cmp(&k(y))
        });
        let Journals { responses: recorded_lines, files: recorded_files } = response_lines(&a.recorded.join(g))?;
        let mut recorded: HashMap<(String, u64), Value> = HashMap::new();
        for v in &recorded_lines {
            let key = (v["request_id"].as_str().unwrap_or("").to_string(), v["attempt"].as_u64().unwrap_or(u64::MAX));
            ensure!(recorded.insert(key, v["verdict"].clone()).is_none(), "duplicate recorded response");
        }
        let recorded_full = recorded_lines;
        let dual = Arc::new(Dual { v1: era_norm::EraPost::load(&a.norm, &a.batch)?, stats: Mutex::new(Stats::default()) });
        let ctx = generation::Ctx {
            endpoint: String::new(),
            alias: String::new(),
            config: Value::Null,
            tokenizer: tok.clone(),
            overlap: overlap::Overlap::new(overlap::PriorIndex::empty()),
            parents: ptext.clone(),
            client: generation::client()?,
            post: Some(dual.clone() as Arc<dyn PostProcess>),
            doc_words: era_norm::Profile::CohortV1.window(),
        };
        let (mut anchored, mut unanchored, mut accepted, mut raw_text_fields) = (0usize, 0usize, 0usize, 0usize);
        let replayed: std::collections::HashSet<(String, u64)> =
            responses.iter().map(|v| (v["request_id"].as_str().unwrap_or("").to_string(), v["attempt"].as_u64().unwrap_or(u64::MAX))).collect();
        for v in &responses {
            let rid = v["request_id"].as_str().unwrap_or("").to_string();
            let old = v["verdict"].clone();
            let transport = old["reasons"].as_array().is_some_and(|x| x.iter().any(|r| r == "transport_error"));
            let new = if transport {
                old.clone()
            } else {
                let parsed: Value = serde_json::from_str(v["raw_body"].as_str().unwrap_or("")).unwrap_or(Value::Null);
                let status = v["status"].as_u64().unwrap_or(0) as u16;
                generation::verdict(&ctx, &reqs[&rid], status, &parsed)
            };
            if new["accepted"] == true {
                ctx.overlap.add_generated(new["text"].as_str().unwrap_or(""));
                accepted += 1;
            }
            // The whole verdict is anchored: a v1 step keeps no `raw_text` (B1c),
            // as the recorded cohort-v3 verdicts keep none; any that carries
            // one is counted and cannot anchor.
            if new.get("raw_text").is_some() {
                raw_text_fields += 1;
            }
            match recorded.get(&(rid, v["attempt"].as_u64().unwrap_or(u64::MAX))) {
                Some(r) if *r == new => anchored += 1,
                _ => unanchored += 1,
            }
        }
        let part_a_calls = dual.stats.lock().expect("stats").calls;
        // Part B: the recorded responses the replay does not cover.
        let ctx_b = generation::Ctx {
            endpoint: String::new(),
            alias: String::new(),
            config: Value::Null,
            tokenizer: tok.clone(),
            overlap: overlap::Overlap::new(overlap::PriorIndex::empty()),
            parents: ptext.clone(),
            client: generation::client()?,
            post: Some(dual.clone() as Arc<dyn PostProcess>),
            doc_words: era_norm::Profile::CohortV1.window(),
        };
        let mut rest: Vec<&Value> = recorded_full.iter().filter(|v| !replayed.contains(&(v["request_id"].as_str().unwrap_or("").to_string(), v["attempt"].as_u64().unwrap_or(u64::MAX)))).collect();
        rest.sort_by(|x, y| {
            let k = |v: &Value| (v["t_end"].as_u64().unwrap_or(0), v["request_id"].as_str().unwrap_or("").to_string(), v["attempt"].as_u64().unwrap_or(0));
            k(x).cmp(&k(y))
        });
        let (mut b_post_anchored, mut b_post_differ, mut b_no_post, mut b_outside_set) = (0usize, 0usize, 0usize, 0usize);
        for v in rest {
            let rid = v["request_id"].as_str().unwrap_or("").to_string();
            let Some(req) = reqs.get(&rid) else {
                b_outside_set += 1;
                continue;
            };
            let old = &v["verdict"];
            if old["diag"]["post"].is_null() {
                b_no_post += 1;
                continue;
            }
            let parsed: Value = serde_json::from_str(v["raw_body"].as_str().unwrap_or("")).unwrap_or(Value::Null);
            let status = v["status"].as_u64().unwrap_or(0) as u16;
            let new = generation::verdict(&ctx_b, req, status, &parsed);
            if new["diag"]["post"] == old["diag"]["post"] {
                b_post_anchored += 1;
            } else {
                b_post_differ += 1;
            }
        }
        let s = dual.stats.lock().expect("stats");
        let ok = unanchored == 0
            && raw_text_fields == 0
            && b_post_differ == 0
            && b_outside_set == 0
            && s.text_equal == s.calls
            && s.form_equal == s.calls
            && s.conventions_equal == s.calls
            && anchored + b_post_anchored + b_no_post == recorded.len();
        all_ok &= ok;
        by.insert(g.clone(), json!({"requests": reqs.len(), "responses": responses.len(), "accepted_v1": accepted,
            "v1_verdicts_equal_recorded": anchored, "v1_verdicts_not_equal_recorded": unanchored, "recorded_responses": recorded.len(),
            "anchor": "whole verdict", "verdicts_with_raw_text": raw_text_fields,
            "part_b": {"post_records_equal_recorded": b_post_anchored, "post_records_not_equal_recorded": b_post_differ,
                "recorded_without_post": b_no_post, "outside_request_set": b_outside_set, "post_calls": s.calls - part_a_calls},
            "post_calls_part_a": part_a_calls, "post_calls": s.calls, "v2_text_equal_v1": s.text_equal, "v2_form_reasons_equal_v1": s.form_equal, "conventions_equal": s.conventions_equal,
            "source_classes": s.classes, "transfer_paths": s.paths, "emphasis_kept": s.emphasis_kept,
            "emphasis_kept_child_pairs": s.emphasis_kept_child_pairs, "source_passes_screen": s.source_passes_screen,
            "echo_rejected": s.echo_rejected, "echo_reasons": s.echo_reasons, "differ": s.differ, "ok": ok,
            "journals_sha256": files, "recorded_journals_sha256": recorded_files}));
    }
    // Schema v2 (B1c): part A anchors whole verdicts (`anchor`, with
    // `verdicts_with_raw_text` counted and required to be 0); v1 excluded
    // `raw_text` from the comparison (`anchor_excludes`).
    let rec = json!({"schema": "slop_ninja_a_r2_b2_profile_v2_parity_v2",
        "rule": "part A: v1 renorm (cc_cohort_profile_v1) replayed on the request set's input journals, every verdict anchored whole (equal as a JSON value, raw_text included) to the recorded journal's, and a replayed verdict carrying raw_text counted (verdicts_with_raw_text, which must be 0); part B: every other recorded response through the same acceptance path on a fresh overlap index, its post-processing record anchored to the recorded one; at every post-processing call profile v2's child step (child_v2) on the same parent, span and raw response with the same conventions; v2 equals v1 when every child's bytes, form reasons and conventions match; echo outcomes are a separate v2 screen, reported",
        "profile_v2": era_norm::Profile::CohortV2.record(), "batch_sha256": measure::file_sha256(&a.batch.join("BATCH.json"))?,
        "norm_sha256": measure::file_sha256(&a.norm)?, "tokenizer_sha256": measure::file_sha256(&a.tokenizer)?, "request_set": meta_path,
        "by_generator": by, "v2_equals_v1": all_ok});
    store::write_json(&a.out, &rec)?;
    println!("{}", json!({"v2_equals_v1": all_ok, "out": a.out}));
    Ok(())
}

//! Evaluator for the frozen `cfg-vs-encoder-20261031` writer-profile protocol:
//! version 1 (PROTOCOL.md, FROZEN.json) extended only by version 2
//! (PROTOCOL-v2.md, FROZEN-v2.json), which adds arm F, the score fusion of B,
//! C and A, and its single contrast F - D. Inputs derive from exact private
//! text: keep every path under ignored `data/`. Only IDs, counts, vectors and
//! scores are read; no text.
//!
//! Subcommands (ROOT is the protocol directory holding FROZEN.json and
//! FROZEN-v2.json; each verifies the v1 and v2 bindings first):
//!   select ROOT RUN_DIR
//!       Requires RUN_DIR/EVALUATOR.json to record this binary's sha256.
//!       Training authors only: recomputes the v1 training phase, requires its
//!       record to equal v1's selection record exactly (reproduction gate),
//!       fits arm F and its T2 thresholds, and writes
//!       RUN_DIR/selection/selection.json (v1's record unchanged plus the v2
//!       record) before any evaluation author is scored.
//!   rehearse ROOT NEW_OUT_DIR
//!       Training authors only (in-sample, synthetic T3 from training authors,
//!       as in v1). Applies the same reproduction gate to v1's recorded
//!       rehearsal (eval-rehearsal-v2), then writes the v2 rehearsal results.
//!       Its numbers carry no evidential weight.
//!   evaluate ROOT RUN_DIR
//!       Recomputes the training phase and requires it to equal the recorded
//!       selection; scores every arm once on the evaluation authors and T3
//!       strata; requires every v1 metric, contrast, decision and pair-file
//!       digest for arms A to E to equal eval-v1 exactly before any arm-F
//!       output is written; then writes RUN_DIR/results (new).
//!   verify-pairs ROOT RUN_DIR
//!       After an evaluation: checks row by row that the v2 pair files equal
//!       v1's pair files once arm F's scores are removed.
//!
//! Environment: CVE_THREADS (default 4).
#![recursion_limit = "256"]

#[allow(dead_code)]
#[path = "cfg_vs_encoder_features/common.rs"]
mod common;
#[path = "cfg_vs_encoder_evaluate/load.rs"]
mod load;
#[path = "cfg_vs_encoder_evaluate/run.rs"]
mod run;
#[path = "cfg_vs_encoder_evaluate/space.rs"]
mod space;
#[path = "cfg_vs_encoder_evaluate/stats.rs"]
mod stats;
#[path = "cfg_vs_encoder_evaluate/tasks.rs"]
mod tasks;
#[cfg(test)]
#[path = "cfg_vs_encoder_evaluate/tests.rs"]
mod tests;
#[path = "cfg_vs_encoder_evaluate/v2.rs"]
mod v2;

use anyhow::{Context, Result, bail, ensure};
use common::*;
use serde_json::{Map, Value, json};
use std::{fs, path::Path};

const SCHEMA: &str = "slop_ninja_cfg_vs_encoder_evaluation_v1";

fn threads() -> usize {
    std::env::var("CVE_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4)
}

fn binary_sha() -> Result<String> {
    hash_file(&std::env::current_exe()?)
}

struct Phase {
    inp: load::Inputs,
    train_full: run::Trained,
    train_restricted: run::Trained,
    /// v1's selection record, built by the v1 code.
    record: Value,
    /// Protocol v2 additions (arm F).
    record_v2: Value,
}

fn training_phase(root: &Path) -> Result<Phase> {
    let inp = load::load(root)?;
    let th = threads();
    let (pop, fail, _) = tasks::t12_pop(&inp, "train", false)?;
    let (pop_r, fail_r, unq_r) = tasks::t12_pop(&inp, "train", true)?;
    eprintln!("training authors: {} (restricted {})", pop.authors.len(), pop_r.authors.len());
    let train_full = run::train(&inp, &pop, load::Text::Full, th)?;
    let train_restricted = run::train(&inp, &pop_r, load::Text::Restricted, th)?;
    let record = json!({
        "schema":SCHEMA,"protocol_id":PROTOCOL_ID,"frozen_sha256":FROZEN_SHA256,
        "inputs":inp.bindings,
        "training_authors_excluded_for_execution_failure":fail,
        "restricted":{"training_authors_excluded_for_execution_failure":fail_r,"training_authors_not_qualifying":unq_r},
        "primary":train_full.record,
        "coverage_restricted":train_restricted.record,
    });
    let record_v2 = json!({
        "schema":v2::SCHEMA_V2,"protocol_id":PROTOCOL_ID,"version":2,
        "frozen_v2_sha256":v2::FROZEN_V2_SHA256,
        "primary":train_full.record_v2,
        "coverage_restricted":train_restricted.record_v2,
    });
    Ok(Phase {
        inp,
        train_full,
        train_restricted,
        record,
        record_v2,
    })
}

fn wrapper(root: &Path, extra: Value) -> Result<Value> {
    let dev2 = root.join("DEVIATIONS-v2.md");
    let dev2_sha = if dev2.exists() {
        json!(hash_file(&dev2)?)
    } else {
        Value::Null
    };
    let mut v = json!({
        "evaluator_binary_sha256":binary_sha()?,
        "deviations_sha256":hash_file(&root.join("DEVIATIONS.md"))?,
        "deviations_v2_sha256":dev2_sha,
        "frozen_v2_sha256":v2::FROZEN_V2_SHA256,
        "protocol_version":2,
        "threads":threads(),
        "created":now_utc(),
    });
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    Ok(v)
}

fn require_evaluator(run_dir: &Path) -> Result<String> {
    let me = binary_sha()?;
    let rec: Value = serde_json::from_slice(
        &fs::read(run_dir.join("EVALUATOR.json")).context("RUN_DIR/EVALUATOR.json")?,
    )?;
    ensure!(
        rec["evaluator_binary_sha256"] == me.as_str(),
        "EVALUATOR.json does not record this binary"
    );
    ensure!(
        rec["frozen_v2_sha256"] == v2::FROZEN_V2_SHA256,
        "EVALUATOR.json binds another FROZEN-v2.json"
    );
    Ok(me)
}

/// Reproduction gate, training phase (PROTOCOL-v2.md section 8): the
/// recomputed v1 record equals `v1_record` exactly (its sha256 and as JSON).
fn training_gate(ph: &Phase, v1_record: &Value, v1_record_sha256: &str) -> Result<Value> {
    let rsha = sha_hex(&serde_json::to_vec(&ph.record)?);
    let cmp = v2::gate_compare(&ph.record, v1_record)?;
    let passed = rsha == v1_record_sha256 && cmp["equal"] == true;
    Ok(json!({"v1_record_sha256":v1_record_sha256,"recomputed_record_sha256":rsha,
        "json_equal":cmp["equal"],"differing_pointers":cmp["differing_pointers"],"passed":passed}))
}

fn v1_selection_record(root: &Path, b: &v2::Bindings) -> Result<Value> {
    let sel = v2::read_bound_json(&root.join("eval-v1/selection/selection.json"), &b.v1_selection_sha256)?;
    ensure!(sel["record_sha256"] == b.v1_record_sha256.as_str(), "v1 selection record digest");
    Ok(sel["record"].clone())
}

/// Requires the training gate, writing only the gate outcome (no arm-F
/// value) to `fail_path` when it fails.
fn require_training_gate(root: &Path, b: &v2::Bindings, ph: &Phase, fail_path: &Path) -> Result<Value> {
    let gate = training_gate(ph, &v1_selection_record(root, b)?, &b.v1_record_sha256)?;
    if gate["passed"] != true {
        write_new_json(fail_path, &json!({"reproduction_gate":{"training_phase":gate}}))?;
        bail!("reproduction gate failed in the training phase; no arm-F output written");
    }
    // Arm F uses exactly the trials on which v1 fitted D and E.
    for part in ["primary", "coverage_restricted"] {
        let f = &ph.record_v2[part]["fusion"];
        let d = &ph.record[part]["fusion"]["D"];
        ensure!(
            f["trials"] == d["trials"] && f["genuine_trials"] == d["genuine_trials"],
            "arm F fit trials differ from v1's D trials ({part})"
        );
    }
    let f = &ph.record_v2["primary"]["fusion"];
    ensure!(
        f["trials"] == b.expected_trials && f["genuine_trials"] == b.expected_genuine_trials,
        "arm F fit trials differ from the frozen counts"
    );
    Ok(gate)
}

/// Reproduction gate, evaluation: the v1-arm results document recomputed
/// by the v1 code path equals the recorded one exactly.
fn evaluation_gate(recomputed: &Value, recorded: &Value, extra: &[(&str, Value, Value)]) -> Result<Value> {
    let cmp = v2::gate_compare(recomputed, &recorded["results"])?;
    let mut others = Map::new();
    let mut all = cmp["equal"] == true;
    for (name, got, want) in extra {
        let eq = v2::roundtrip(got)? == *want;
        all &= eq;
        others.insert(name.to_string(), json!(eq));
    }
    Ok(json!({"results_json_equal":cmp["equal"],"differing_pointers":cmp["differing_pointers"],
        "other_fields_equal":others,"passed":all}))
}

fn select_cli(root: &Path, run_dir: &Path) -> Result<()> {
    require_evaluator(run_dir)?;
    let b = v2::bindings(root)?;
    let out = &run_dir.join("selection");
    create_new_dir(out)?;
    let ph = training_phase(root)?;
    let gate = require_training_gate(root, &b, &ph, &out.join("REPRODUCTION-GATE-FAILED.json"))?;
    let rsha = sha_hex(&serde_json::to_vec(&ph.record)?);
    let v2sha = sha_hex(&serde_json::to_vec(&ph.record_v2)?);
    let sha = write_new_json(
        &out.join("selection.json"),
        &wrapper(
            root,
            json!({"schema":v2::SCHEMA_V2,"record_sha256":rsha,"record":ph.record,
                "v2_record_sha256":v2sha,"v2_record":ph.record_v2,
                "reproduction_gate":{"training_phase":gate}}),
        )?,
    )?;
    println!(
        "{}",
        json!({"selection_sha256":sha,"v1_record_reproduced":true,
            "arm_F_available":ph.record_v2["primary"]["available"],
            "arm_F_restricted_available":ph.record_v2["coverage_restricted"]["available"]})
    );
    Ok(())
}

fn rehearsal_strata(pop: &tasks::Pop) -> Vec<tasks::T3Stratum> {
    let n = pop.authors.len();
    let mut targets: Vec<tasks::T3Target> = pop
        .authors
        .iter()
        .enumerate()
        .map(|(i, a)| tasks::T3Target {
            writer: a.writer.clone(),
            profile: a.refs.clone(),
            has_train: i % 2 == 0,
            sources: Vec::new(),
        })
        .collect();
    let mut records = Vec::new();
    for (i, a) in pop.authors.iter().enumerate() {
        let next = &pop.authors[(i + 1) % n];
        for (j, &q) in a.queries.iter().enumerate() {
            let full = j % 2 == 0;
            let source = full.then_some(next.refs[0]);
            if let Some(s) = source
                && !targets[i].sources.contains(&s)
            {
                targets[i].sources.push(s);
            }
            records.push(tasks::T3Rec {
                doc: q,
                target: i,
                kind: if full { "full" } else { "mirror" }.into(),
                family: if i % 2 == 0 { "f1" } else { "f2" }.into(),
                parent_writer: Some(next.writer.clone()),
                source,
            });
        }
    }
    for t in &mut targets {
        t.sources.sort_unstable();
    }
    vec![tasks::T3Stratum {
        name: "rehearsal".into(),
        targets,
        records,
    }]
}

/// v1's recorded rehearsal results (training authors only), bound through
/// v1's EVALUATOR.json, which FROZEN-v2.json binds.
fn v1_rehearsal_results(root: &Path, b: &v2::Bindings) -> Result<Value> {
    let rec = v2::read_bound_json(&root.join("eval-v1/EVALUATOR.json"), &b.v1_evaluator_record_sha256)?;
    let r = &rec["rehearsals"]["eval-rehearsal-v2"];
    ensure!(r["binary_sha256"] == b.v1_binary_sha256.as_str(), "v1 rehearsal binary");
    let sha = r["results_sha256"].as_str().context("v1 rehearsal digest")?;
    v2::read_bound_json(&root.join("eval-rehearsal-v2/results.json"), sha)
}

fn rehearse_cli(root: &Path, out: &Path) -> Result<()> {
    let b = v2::bindings(root)?;
    create_new_dir(out)?;
    let ph = training_phase(root)?;
    let tgate = require_training_gate(root, &b, &ph, &out.join("REPRODUCTION-GATE-FAILED.json"))?;
    let (pop, _, _) = tasks::t12_pop(&ph.inp, "train", false)?;
    let (pop_r, _, _) = tasks::t12_pop(&ph.inp, "train", true)?;
    let strata = rehearsal_strata(&pop);
    let ev = run::EvalInputs {
        label: "rehearsal",
        trained: &ph.train_full,
        trained_restricted: &ph.train_restricted,
        pop: &pop,
        pop_restricted: &pop_r,
        strata: &strata,
    };
    let th = threads();
    let scored = run::score_all(&ph.inp, &ev, th)?;
    let v1res = run::analyze(&ph.inp, &ev, &scored, run::Spec::V1, None, th)?;
    let egate = evaluation_gate(&v1res, &v1_rehearsal_results(root, &b)?, &[])?;
    if egate["passed"] != true {
        write_new_json(
            &out.join("REPRODUCTION-GATE-FAILED.json"),
            &json!({"reproduction_gate":{"training_phase":tgate,"evaluation":egate}}),
        )?;
        bail!("rehearsal reproduction gate failed; no arm-F output written");
    }
    let gate = json!({"training_phase":tgate,"evaluation_against_v1_rehearsal":egate});
    let rsha = sha_hex(&serde_json::to_vec(&ph.record)?);
    let v2sha = sha_hex(&serde_json::to_vec(&ph.record_v2)?);
    write_new_json(
        &out.join("selection.json"),
        &wrapper(
            root,
            json!({"schema":v2::SCHEMA_V2,"record_sha256":rsha,"record":ph.record,
                "v2_record_sha256":v2sha,"v2_record":ph.record_v2,"reproduction_gate":{"training_phase":tgate}}),
        )?,
    )?;
    let results = if ph.train_full.fusion_f.is_some() {
        run::analyze(&ph.inp, &ev, &scored, run::Spec::V2, Some(out), th)?
    } else {
        json!({"decision":{"outcome":"not_evaluable","reason":"arm F fit failed"}})
    };
    let sha = write_new_json(
        &out.join("results.json"),
        &wrapper(
            root,
            json!({"mode":"rehearsal_training_authors_in_sample","reproduction_gate":gate,"results":results}),
        )?,
    )?;
    println!("{}", json!({"rehearsal_results_sha256":sha,"reproduction_gate_passed":true}));
    Ok(())
}

fn evaluate_cli(root: &Path, run_dir: &Path) -> Result<()> {
    let me = require_evaluator(run_dir)?;
    let b = v2::bindings(root)?;
    let sel_bytes = fs::read(run_dir.join("selection/selection.json"))?;
    let sel: Value = serde_json::from_slice(&sel_bytes)?;
    ensure!(
        sel["evaluator_binary_sha256"] == me.as_str(),
        "this binary is not the evaluator recorded with the selection"
    );
    ensure!(sel["schema"] == v2::SCHEMA_V2, "selection schema");
    let out = &run_dir.join("results");
    create_new_dir(out)?;
    let ph = training_phase(root)?;
    let tgate = require_training_gate(root, &b, &ph, &out.join("REPRODUCTION-GATE-FAILED.json"))?;
    ensure!(
        sel["record_sha256"] == sha_hex(&serde_json::to_vec(&ph.record)?).as_str()
            && sel["v2_record_sha256"] == sha_hex(&serde_json::to_vec(&ph.record_v2)?).as_str(),
        "recomputed training phase differs from the recorded selection"
    );
    eprintln!("training phase reproduces v1's record and the recorded v2 selection");
    let (pop, fail, _) = tasks::t12_pop(&ph.inp, "eval", false)?;
    let (pop_r, fail_r, unq_r) = tasks::t12_pop(&ph.inp, "eval", true)?;
    let strata = tasks::t3_strata(&ph.inp)?;
    let ev = run::EvalInputs {
        label: "evaluation",
        trained: &ph.train_full,
        trained_restricted: &ph.train_restricted,
        pop: &pop,
        pop_restricted: &pop_r,
        strata: &strata,
    };
    let th = threads();
    let scored = run::score_all(&ph.inp, &ev, th)?;
    // Reproduction gate before any arm-F output: the v1 code path, pair files
    // hashed but not written.
    let v1res = run::analyze(&ph.inp, &ev, &scored, run::Spec::V1, None, th)?;
    let v1rec = v2::read_bound_json(&root.join("eval-v1/results/results.json"), &b.v1_results_sha256)?;
    let restricted = json!({"evaluation_authors_excluded_for_execution_failure":fail_r,"evaluation_authors_not_qualifying":unq_r});
    let egate = evaluation_gate(
        &v1res,
        &v1rec,
        &[
            ("evaluation_authors_excluded_for_execution_failure", json!(fail), v1rec["evaluation_authors_excluded_for_execution_failure"].clone()),
            ("restricted", restricted.clone(), v1rec["restricted"].clone()),
            ("pair_files", v1res["files"].clone(), b.frozen_v2["extends_v1"]["eval_v1"]["pair_files"].clone()),
        ],
    )?;
    if egate["passed"] != true {
        write_new_json(
            &out.join("REPRODUCTION-GATE-FAILED.json"),
            &json!({"reproduction_gate":{"training_phase":tgate,"evaluation":egate}}),
        )?;
        bail!("reproduction gate failed in the evaluation; no arm-F output written");
    }
    eprintln!("evaluation reproduces v1's results for arms A to E");
    let gate = json!({"training_phase":tgate,"evaluation":egate});
    let results = if ph.train_full.fusion_f.is_some() {
        run::analyze(&ph.inp, &ev, &scored, run::Spec::V2, Some(out), th)?
    } else {
        json!({"decision":{"outcome":"not_evaluable","reason":"arm F fit failed (see selection v2_record)"}})
    };
    let sha = write_new_json(
        &out.join("results.json"),
        &wrapper(
            root,
            json!({"mode":"evaluation","selection_sha256":sha_hex(&sel_bytes),
                "evaluation_authors_excluded_for_execution_failure":fail,
                "restricted":restricted,
                "reproduction_gate":gate,
                "results":results}),
        )?,
    )?;
    println!("{}", json!({"results_sha256":sha,"decision":results["decision"]["outcome"]}));
    Ok(())
}

fn verify_pairs_cli(root: &Path, run_dir: &Path) -> Result<()> {
    let b = v2::bindings(root)?;
    let res = v2::read_bound_json(
        &run_dir.join("results/results.json"),
        &hash_file(&run_dir.join("results/results.json"))?,
    )?;
    let v1dir = root.join("eval-v1/results");
    let mut report = Map::new();
    let files = b.frozen_v2["extends_v1"]["eval_v1"]["pair_files"]
        .as_object()
        .context("v1 pair files")?;
    for (name, meta) in files {
        let mut a = HashedLines::open(&v1dir.join(name))?;
        let mut c = HashedLines::open(&run_dir.join("results").join(name))?;
        let mut rows = 0usize;
        loop {
            match (a.next_line()?, c.next_line()?) {
                (None, None) => break,
                (Some(x), Some(y)) => {
                    let x: Value = serde_json::from_slice(x)?;
                    let mut y: Value = serde_json::from_slice(y)?;
                    v2::strip_f(&mut y)?;
                    ensure!(x == y, "{name}: row {} differs once arm F is removed", rows + 1);
                    rows += 1;
                }
                _ => bail!("{name}: row counts differ"),
            }
        }
        let (sa, sc) = (a.finish(), c.finish());
        ensure!(meta["rows"] == rows && meta["sha256"] == sa.as_str(), "{name}: v1 file binding");
        ensure!(
            res["results"]["files"][name]["sha256"] == sc.as_str() && res["results"]["files"][name]["rows"] == rows,
            "{name}: v2 file binding"
        );
        report.insert(
            name.clone(),
            json!({"rows":rows,"v1_sha256":sa,"v2_sha256":sc,"v1_arm_scores_and_keys_equal":true}),
        );
    }
    let sha = write_new_json(
        &run_dir.join("PAIRS-VERIFIED.json"),
        &wrapper(root, json!({"mode":"verify_pairs","results_sha256":hash_file(&run_dir.join("results/results.json"))?,"files":report}))?,
    )?;
    println!("{}", json!({"pairs_verified_sha256":sha}));
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let p = |i: usize| -> Result<&Path> {
        Ok(Path::new(
            args.get(i).with_context(|| format!("missing argument {i}"))?,
        ))
    };
    match args.first().map(String::as_str) {
        Some("select") if args.len() == 3 => select_cli(p(1)?, p(2)?),
        Some("rehearse") if args.len() == 3 => rehearse_cli(p(1)?, p(2)?),
        Some("evaluate") if args.len() == 3 => evaluate_cli(p(1)?, p(2)?),
        Some("verify-pairs") if args.len() == 3 => verify_pairs_cli(p(1)?, p(2)?),
        _ => bail!("usage: see module documentation"),
    }
}

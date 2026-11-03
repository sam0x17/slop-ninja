//! Round-2 two-block evaluator CLI (protocol A2; build B10, parts 1 and 2).
//! Reads a `slop_ninja_a_r2_evaluate_config_v1` configuration, writes the
//! margins file first (from comparator outputs only), then the evaluation.
//! `slice-plan` writes S4-1's T2 plan from rows only (pipeline counts), before
//! any prediction exists for the scoring pass. Numeric only: no text field is
//! read or printed. See `grammar_a_baseline::r2_eval`.
//!
//! usage: a_r2_evaluate CONFIG.json MARGINS.json OUT.json
//!        a_r2_evaluate slice-plan PLAN-CONFIG.json PLAN.json

use anyhow::{Context, Result, ensure};
use grammar_a_baseline::r2_eval::{self, parse_config, part2, sha256_hex, write_new};
use serde_json::json;
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 4 && args[1] == "slice-plan" {
        let bytes = std::fs::read(&args[2]).with_context(|| format!("read {}", args[2]))?;
        let rec = part2::slice_plan(&part2::parse_slice_plan_config(&bytes)?)?;
        let mut out = serde_json::to_vec_pretty(&rec)?;
        out.push(b'\n');
        write_new(Path::new(&args[3]), &out)?;
        println!(
            "{}",
            json!({"config_sha256": sha256_hex(&bytes), "plan_sha256": sha256_hex(&out), "k": rec["k"], "z_k": rec["z_k"],
                   "gated": rec["slices"].as_array().map(|s| s.iter().filter(|x| x["gated"] == true).count())})
        );
        return Ok(());
    }
    ensure!(
        args.len() == 4,
        "usage: a_r2_evaluate CONFIG.json MARGINS.json OUT.json | a_r2_evaluate slice-plan PLAN-CONFIG.json PLAN.json"
    );
    let bytes = std::fs::read(&args[1]).with_context(|| format!("read {}", args[1]))?;
    let cfg = parse_config(&bytes)?;
    let out = r2_eval::run(&cfg, Path::new(&args[2]), Path::new(&args[3]))?;
    let status: Vec<_> = out["gates"]
        .as_array()
        .map(|g| {
            g.iter()
                .map(|x| json!({"id": x["id"], "status": x["status"]}))
                .collect()
        })
        .unwrap_or_default();
    println!(
        "{}",
        json!({
            "config_sha256": sha256_hex(&bytes),
            "margins_sha256": out["margins"]["sha256"],
            "evaluation_sha256": sha256_hex(&std::fs::read(&args[3])?),
            "per_seed_equality": out["per_seed_equality"]["pass"],
            "values_checked": out["per_seed_equality"]["values_checked"],
            "gates": status,
        })
    );
    Ok(())
}

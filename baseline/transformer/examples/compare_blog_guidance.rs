//! Compare fixed style query orders with uniform sampling at the same A-call budget.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long, required = true)]
    plan: Vec<PathBuf>,
    /// Complete, already validated MPS/CUDA screening observations.
    #[arg(long)]
    observations: PathBuf,
    #[arg(long, default_value_t = 8)]
    budget: usize,
    #[arg(long)]
    out: PathBuf,
}
fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
fn field<'a>(v: &'a Value, name: &str) -> Result<&'a str> {
    v[name].as_str().with_context(|| format!("Missing {name}"))
}
fn number(v: &Value, name: &str) -> Result<f64> {
    let x = v[name]
        .as_f64()
        .with_context(|| format!("Missing {name}"))?;
    ensure!(x.is_finite(), "Nonfinite {name}");
    Ok(x)
}
fn random_best(mut values: Vec<f64>, root: f64, k: usize) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    let mut probability = k as f64 / n as f64;
    let mut expected = 0.0;
    for (i, value) in values.iter().enumerate().take(n - k + 1) {
        expected += probability * root.min(*value);
        if i < n - k {
            probability *= (n - i - k) as f64 / (n - i - 1) as f64;
        }
    }
    expected
}

fn main() -> Result<()> {
    use std::io::Write;
    let args = Args::parse();
    ensure!(
        !args.out.exists() && args.budget > 0,
        "Use a new output and positive budget"
    );
    let observations = fs::read(&args.observations)?;
    let mut scores = BTreeMap::new();
    let mut artifact = None;
    for line in std::str::from_utf8(&observations)?.lines() {
        let row: Value = serde_json::from_str(line)?;
        ensure!(
            row["schema"] == "slop_ninja_b_screen_observation_v1"
                && row["canonical_evidence"] == false
                && row["status"] == "ok"
                && row["labels"] == json!(["human_only", "model_only", "mixed"]),
            "Incomplete/wrong screening observations"
        );
        let identity = field(&row, "weight_artifact_id")?.to_owned();
        if let Some(prior) = artifact.replace(identity.clone()) {
            ensure!(prior == identity, "Mixed detector artifacts");
        }
        let score = number(&row, "model_or_assistance_probability")?;
        ensure!((0.0..=1.0).contains(&score), "Invalid probability");
        ensure!(
            scores
                .insert(field(&row, "id")?.to_owned(), score)
                .is_none(),
            "Duplicate observation"
        );
    }
    let mut baseline_binding = None;
    let mut baseline_states = None;
    let mut comparisons = vec![];
    for path in args.plan {
        let bytes = fs::read(&path)?;
        let plan: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            plan["schema"] == "slop_ninja_b_edit_menu_plan_v1"
                && Some(field(&plan["input"], "detector_artifact_id")?) == artifact.as_deref(),
            "Wrong menu or detector"
        );
        let mut binding = plan["walk_binding"].clone();
        binding
            .as_object_mut()
            .context("Walk binding")?
            .remove("references_sha256");
        if let Some(prior) = baseline_binding.replace(binding.clone()) {
            ensure!(
                prior == binding,
                "Source, intent, claims, limits or parser differ between profiles"
            );
        }
        let states = plan["states"].as_array().context("Menu states")?;
        let mut catalog = BTreeMap::new();
        for state in states {
            let id = field(state, "id")?;
            ensure!(
                id == hash(field(state, "text")?)
                    && catalog
                        .insert(id.to_owned(), state["choices"].clone())
                        .is_none(),
                "Invalid or repeated state"
            );
        }
        ensure!(
            catalog.keys().eq(scores.keys()),
            "Incomplete scores or different text universe"
        );
        if let Some(prior) = baseline_states.replace(catalog.clone()) {
            ensure!(
                prior == catalog,
                "Different edit sets across target profiles"
            );
        }
        let roots: Vec<_> = states
            .iter()
            .filter(|s| s["choices"] == json!([]))
            .collect();
        ensure!(roots.len() == 1, "Missing/repeated unchanged state");
        let root = field(roots[0], "id")?;
        let candidates: Vec<_> = states.iter().filter(|s| s["id"] != root).collect();
        ensure!(
            args.budget <= candidates.len(),
            "Budget exceeds candidate count"
        );
        let mut orders = vec![];
        for metric in [
            "full_word_grammar_distance",
            "function_grammar_distance",
            "function_without_sentence_load",
        ] {
            let mut ranked = vec![];
            for state in &candidates {
                let distance = if metric == "function_without_sentence_load" {
                    let changes = state["movements"].as_array().context("Feature movements")?;
                    -changes
                        .iter()
                        .filter(|m| m["family"] != "syntax_sentence_load")
                        .map(|m| number(m, "squared_distance_improvement"))
                        .collect::<Result<Vec<_>>>()?
                        .iter()
                        .sum::<f64>()
                } else {
                    number(state, metric)?
                };
                ranked.push((distance, field(state, "id")?));
            }
            ranked.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(b.1)));
            let selected: Vec<_> = ranked.iter().take(args.budget).map(|(_, id)| *id).collect();
            let mut best = (scores[root], root);
            for id in &selected {
                if scores[*id] < best.0 {
                    best = (scores[*id], id);
                }
            }
            orders.push(json!({"metric":metric,"selected":selected,"best_including_root":{"id":best.1,"A":best.0},"reduction_pp":100.0*(scores[root]-best.0)}));
        }
        let best = scores
            .iter()
            .min_by(|a, b| a.1.total_cmp(b.1).then_with(|| a.0.cmp(b.0)))
            .unwrap();
        comparisons.push(json!({"plan":path,"plan_sha256":hash(bytes),"states":states.len(),"root_A":scores[root],
            "uniform_without_replacement_expected_best_A":random_best(candidates.iter().map(|s|scores[field(s,"id").unwrap()]).collect(),scores[root],args.budget),
            "exhaustive_best":{"id":best.0,"A":best.1},"orders":orders}));
    }
    let report = json!({"schema":"slop_ninja_blog_guidance_comparison_v1","observations_sha256":hash(observations),
        "artifact":artifact,"candidate_call_budget":args.budget,"root_call_additional":true,"comparisons":comparisons,
        "random_baseline":"exact expected minimum over uniform subsets without replacement, retaining the unchanged source",
        "scope":"offline query-order diagnostic on a completely measured development menu; no adaptive search, editorial approval, Pangram result or held-out-author claim",
        "canonical_evidence":false});
    let mut bytes = serde_json::to_vec_pretty(&report)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(args.out)?
        .write_all(&bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn random_subset_expectation_matches_enumerated_pairs_with_root_retention() {
        let expected = (0.1 + 0.1 + 0.15) / 3.0;
        assert!((random_best(vec![0.1, 0.2, 0.3], 0.15, 2) - expected).abs() < 1e-15);
        assert!((random_best(vec![0.1, 0.2, 0.3], 1.0, 3) - 0.1).abs() < 1e-15);
    }
}

//! Pre-registered Pangram subsample power calculation (competitive A protocol,
//! amendment 2). STAGE3_FINAL_TEST_DIR PLAN_JSON NEW_OUTPUT DEV_ITER_METADATA...
//!
//! Reads stage-3 per-document A and CAL-mapped Pangram involvement scores and
//! record metadata (origin, realised share, source group), plus Dev-iter word
//! counts, and writes counts, moments and the planned allocations only. It
//! reads no prose field, makes no provider calls and fits nothing. Costs are
//! in billed units (started 100-word blocks, at least one per document); no
//! currency figure is read or written.
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::pangram_subsample::{
    Cell, SHARE_BINS, STRATUM_FLOOR, Stratum, Z_80, Z_90, Z_975, clopper_pearson_upper,
    cluster_deff, max_flags, neyman_allocation, noninferiority_power, normal_cdf, required_total,
    share_bin, stratified_se,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path};

const SEEDS: [u64; 3] = [17, 29, 41];
/// Planned strata, `(documents per group, groups)` sizes, domain masses and,
/// per stratum, the expected in-slice shares (OLMo-2 instruct, frontier).
type SplitPlan = (
    Vec<Stratum>,
    Vec<(usize, usize)>,
    BTreeMap<String, f64>,
    Vec<(f64, f64)>,
);
/// Below this many stage-3 documents a mixed bin's SD is replaced by the
/// larger SD of its nearest populated neighbour bins.
const MIN_STAGE3_STRATUM: usize = 20;

fn read(p: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(p).with_context(|| p.display().to_string())?)
        .with_context(|| p.display().to_string())
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}
fn mean_sd(v: &[f64]) -> (f64, f64) {
    let n = v.len() as f64;
    let m = v.iter().sum::<f64>() / n;
    let var = if v.len() > 1 {
        v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0)
    } else {
        0.0
    };
    (m, var.sqrt())
}

struct Doc {
    group: String,
    cell: Cell,
    bin: Option<usize>,
    recipe: f64,
    seed17: f64,
}

fn planning_key(cell: Cell, bin: Option<usize>) -> String {
    match bin {
        Some(b) => format!("mixed:{}", SHARE_BINS[b]),
        None => cell.name().to_string(),
    }
}

fn balanced(docs: &[Doc], f: impl Fn(&Doc) -> f64) -> f64 {
    let mut cells: BTreeMap<(&str, Cell), Vec<f64>> = BTreeMap::new();
    for d in docs {
        cells.entry((&d.group, d.cell)).or_default().push(f(d));
    }
    let mut groups: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for ((g, _), v) in cells {
        groups
            .entry(g)
            .or_default()
            .push(v.iter().sum::<f64>() / v.len() as f64);
    }
    groups
        .values()
        .map(|v| v.iter().sum::<f64>() / v.len() as f64)
        .sum::<f64>()
        / groups.len() as f64
}

/// One-way ANOVA intraclass correlation of per-document residuals within groups.
fn icc(groups: &BTreeMap<&str, Vec<f64>>) -> f64 {
    let n: usize = groups.values().map(Vec::len).sum();
    let k = groups.len();
    let grand = groups.values().flatten().sum::<f64>() / n as f64;
    let (mut ssb, mut ssw) = (0.0, 0.0);
    for v in groups.values() {
        let m = v.iter().sum::<f64>() / v.len() as f64;
        ssb += v.len() as f64 * (m - grand).powi(2);
        ssw += v.iter().map(|x| (x - m).powi(2)).sum::<f64>();
    }
    let msb = ssb / (k - 1) as f64;
    let msw = ssw / (n - k) as f64;
    let n0 = (n as f64
        - groups
            .values()
            .map(|v| (v.len() * v.len()) as f64)
            .sum::<f64>()
            / n as f64)
        / (k - 1) as f64;
    (msb - msw) / (msb + (n0 - 1.0) * msw)
}

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        a.len() >= 4,
        "usage: STAGE3_FINAL_TEST_DIR PLAN_JSON NEW_OUTPUT DEV_ITER_METADATA..."
    );
    let (stage, plan_path, out) = (Path::new(&a[0]), Path::new(&a[1]), Path::new(&a[2]));
    let dev_metas: Vec<&Path> = a[3..].iter().map(Path::new).collect();
    ensure!(!out.exists(), "output exists: {}", out.display());
    let plan = read(plan_path)?;

    // Stage-3 inputs, metadata fields only.
    let records = stage.join("dataset-v1/records.jsonl");
    let scores_path = stage.join("pangram-evaluation-v2/scores.json");
    let mut inputs = BTreeMap::new();
    inputs.insert("records".to_string(), sha(&records)?);
    inputs.insert("pangram_scores".to_string(), sha(&scores_path)?);
    let pangram = read(&scores_path)?["CAL_mapped_involvement"].clone();
    let mut preds = Vec::new();
    for s in SEEDS {
        let p = stage.join(format!(
            "evaluation-v2/adapt_last_block-seed-{s}/test-predictions.json"
        ));
        inputs.insert(format!("adapt_last_block_seed_{s}"), sha(&p)?);
        preds.push(read(&p)?);
    }
    let mut docs = Vec::new();
    for line in fs::read_to_string(&records)?.lines() {
        let r: Value = serde_json::from_str(line)?;
        if r["purpose"] != "query" {
            continue;
        }
        let id = r["id"].as_str().context("id")?;
        let cell = Cell::parse(r["origin"].as_str().context("origin")?)?;
        let y = if cell == Cell::HumanOnly { 0.0 } else { 1.0 };
        let bin = match cell {
            Cell::Mixed => Some(share_bin(r["share_target"].as_f64().context("share")?)?),
            _ => None,
        };
        let p = pangram[id]
            .as_f64()
            .with_context(|| format!("pangram {id}"))?;
        let mut diffs = Vec::new();
        for pr in &preds {
            let row = &pr[id];
            ensure!(row["calibrated"] == true, "uncalibrated prediction {id}");
            let q = row["ai_confidence_percent"]
                .as_f64()
                .context("confidence")?
                / 100.0;
            ensure!(
                q.is_finite() && (0.0..=1.0).contains(&q),
                "bad A score {id}"
            );
            diffs.push((q - y).powi(2) - (p - y).powi(2));
        }
        docs.push(Doc {
            group: r["source_group"]
                .as_str()
                .context("source_group")?
                .to_string(),
            cell,
            bin,
            recipe: diffs.iter().sum::<f64>() / 3.0,
            seed17: diffs[0],
        });
    }
    ensure!(
        docs.len() == 636,
        "expected 636 stage-3 Test queries, got {}",
        docs.len()
    );
    let gap_recipe = balanced(&docs, |d| d.recipe);
    let gap_seed17 = balanced(&docs, |d| d.seed17);
    ensure!(
        (gap_recipe - 0.024521236206711237).abs() < 1e-9,
        "stage-3 balanced gap not reproduced: {gap_recipe}"
    );

    // Per-stratum moments (origin cell x realised share bin, pooled over works).
    let mut by: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
    for d in &docs {
        let e = by.entry(planning_key(d.cell, d.bin)).or_default();
        e.0.push(d.recipe);
        e.1.push(d.seed17);
    }
    let mut stage3 = serde_json::Map::new();
    let mut raw_sd: BTreeMap<String, f64> = BTreeMap::new();
    for (k, (r, s)) in &by {
        let (mr, sr) = mean_sd(r);
        let (ms, ss) = mean_sd(s);
        stage3.insert(
            k.clone(),
            json!({"documents": r.len(), "recipe_mean": mr, "recipe_sd": sr, "seed17_mean": ms, "seed17_sd": ss}),
        );
        raw_sd.insert(k.clone(), sr.max(ss));
    }
    let count = |k: &str| by.get(k).map_or(0, |v| v.0.len());
    let bin_key = |b: usize| format!("mixed:{}", SHARE_BINS[b]);
    let sd_floor = plan["planning_sd_floor"]
        .as_f64()
        .context("planning_sd_floor")?;
    let mut planning = serde_json::Map::new();
    let mut planning_sd: BTreeMap<String, f64> = BTreeMap::new();
    for key in ["human_only", "model_only"].into_iter().map(String::from) {
        ensure!(count(&key) >= MIN_STAGE3_STRATUM, "too few stage-3 {key}");
        let raw = raw_sd[&key];
        let sd = raw.max(sd_floor);
        planning.insert(key.clone(), json!({"stage3_documents": count(&key), "stage3_sd": raw, "basis": "max(recipe_sd, seed17_sd)", "planning_sd": sd}));
        planning_sd.insert(key, sd);
    }
    for b in 0..SHARE_BINS.len() {
        let key = bin_key(b);
        let (raw, basis) = if count(&key) >= MIN_STAGE3_STRATUM {
            (raw_sd[&key], "max(recipe_sd, seed17_sd)".to_string())
        } else {
            let left = (0..b)
                .rev()
                .find(|&j| count(&bin_key(j)) >= MIN_STAGE3_STRATUM);
            let right =
                (b + 1..SHARE_BINS.len()).find(|&j| count(&bin_key(j)) >= MIN_STAGE3_STRATUM);
            let used: Vec<usize> = left.into_iter().chain(right).collect();
            ensure!(!used.is_empty(), "no populated neighbour for {key}");
            (
                used.iter()
                    .map(|&j| raw_sd[&bin_key(j)])
                    .fold(0.0, f64::max),
                format!(
                    "fewer than {MIN_STAGE3_STRATUM} stage-3 documents: larger SD of nearest populated bins {:?}",
                    used.iter().map(|&j| SHARE_BINS[j]).collect::<Vec<_>>()
                ),
            )
        };
        let sd = raw.max(sd_floor);
        planning.insert(key.clone(), json!({"stage3_documents": count(&key), "stage3_sd": raw, "basis": basis, "planning_sd": sd}));
        planning_sd.insert(key, sd);
    }

    // Within-work correlation of residuals from stratum means.
    let means: BTreeMap<String, (f64, f64)> = by
        .iter()
        .map(|(k, (r, s))| (k.clone(), (mean_sd(r).0, mean_sd(s).0)))
        .collect();
    let mut res_r: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    let mut res_s: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for d in &docs {
        let m = means[&planning_key(d.cell, d.bin)];
        res_r.entry(&d.group).or_default().push(d.recipe - m.0);
        res_s.entry(&d.group).or_default().push(d.seed17 - m.1);
    }
    let (icc_r, icc_s) = (icc(&res_r), icc(&res_s));
    let rho = icc_r.max(icc_s).max(0.0);

    // Cost basis: Dev-iter human word counts (exposed by design; counts only).
    let mut units = Vec::new();
    let mut words = Vec::new();
    for dev_meta in &dev_metas {
        for line in fs::read_to_string(dev_meta)?.lines() {
            let r: Value = serde_json::from_str(line)?;
            let w = r["word_count"].as_u64().context("word_count")?;
            words.push(w as f64);
            units.push(w.div_ceil(100).max(1) as f64);
        }
        inputs.insert(
            format!("dev_iter_metadata:{}", dev_meta.display()),
            sha(dev_meta)?,
        );
    }
    let mean_units = units.iter().sum::<f64>() / units.len() as f64;

    let frontier_fraction = plan["frontier"]["fraction_of_humans"]
        .as_f64()
        .context("frontier.fraction_of_humans")?;
    let frontier_mirror = plan["frontier"]["mirror_share"]
        .as_f64()
        .context("frontier.mirror_share")?;
    let olmo_share = plan["olmo2_share_of_open_positives"]
        .as_f64()
        .context("olmo2_share_of_open_positives")?;
    let scenarios: BTreeMap<String, BTreeMap<String, f64>> = plan["mixed_bin_mass_scenarios"]
        .as_object()
        .context("mixed_bin_mass_scenarios")?
        .iter()
        .map(|(name, m)| {
            let masses: BTreeMap<String, f64> = m["mass"]
                .as_object()
                .expect("scenario mass")
                .iter()
                .map(|(k, v)| (k.clone(), v.as_f64().expect("mass")))
                .collect();
            (name.clone(), masses)
        })
        .collect();
    for (name, m) in &scenarios {
        ensure!(
            (m.values().sum::<f64>() - 1.0).abs() < 1e-9,
            "bin mass of {name} must sum to 1"
        );
        for k in m.keys() {
            ensure!(
                SHARE_BINS.contains(&k.as_str()),
                "unknown bin {k} in {name}"
            );
        }
    }

    // Planned strata for a split: Test strata are domain x cell x realised
    // share bin; CAL strata are domain x origin cell. Each human has two open
    // primary positives (a mirror and a mixed rewrite); a `frontier_fraction`
    // of humans has one frontier positive, a mirror with `frontier_mirror`
    // probability. Frontier and OLMo-2 positives share their cell's mass.
    let strata_for = |split: &str, bins: Option<&BTreeMap<String, f64>>| -> Result<SplitPlan> {
        let domains = plan["splits"][split].as_array().context("split domains")?;
        let total_groups: f64 = domains.iter().map(|d| d["groups"].as_f64().unwrap()).sum();
        let mut strata = Vec::new();
        let mut sizes = Vec::new();
        let mut dmass = BTreeMap::new();
        let mut shares = Vec::new();
        let fm = frontier_fraction * frontier_mirror;
        let fx = frontier_fraction * (1.0 - frontier_mirror);
        for d in domains {
            let name = d["domain"].as_str().context("domain")?;
            let humans = d["humans"].as_u64().context("humans")? as usize;
            let groups = d["groups"].as_f64().context("groups")?;
            let cap = d["max_docs_per_group"].as_u64().context("cap")? as usize;
            ensure!(humans > 0 && groups > 0.0, "empty domain {name} in {split}");
            let m = groups / total_groups;
            dmass.insert(name.to_string(), m);
            let per_group = 3 * cap + (frontier_fraction * cap as f64).ceil() as usize;
            sizes.push((per_group, humans.div_ceil(cap)));
            let h = humans as f64;
            let sd = |k: &str| planning_sd[k];
            let model_pop = (h * (1.0 + fm)).round() as usize;
            let mixed_pop = h * (1.0 + fx);
            let model_shares = (olmo_share / (1.0 + fm), fm / (1.0 + fm));
            let mixed_shares = (olmo_share / (1.0 + fx), fx / (1.0 + fx));
            strata.push(Stratum {
                key: format!("{name}|human_only"),
                population: humans,
                weight: m / 3.0,
                sd: if bins.is_some() {
                    sd("human_only")
                } else {
                    1.0
                },
            });
            shares.push((1.0, 1.0));
            strata.push(Stratum {
                key: format!("{name}|model_only"),
                population: model_pop,
                weight: m / 3.0,
                sd: if bins.is_some() {
                    sd("model_only")
                } else {
                    1.0
                },
            });
            shares.push(model_shares);
            match bins {
                None => {
                    strata.push(Stratum {
                        key: format!("{name}|mixed"),
                        population: mixed_pop.round() as usize,
                        weight: m / 3.0,
                        sd: 1.0,
                    });
                    shares.push(mixed_shares);
                }
                Some(bins) => {
                    for (b, mass) in bins {
                        if *mass <= 0.0 {
                            continue;
                        }
                        let k = format!("mixed:{b}");
                        strata.push(Stratum {
                            key: format!("{name}|{k}"),
                            population: ((mixed_pop * mass).round() as usize).max(1),
                            weight: m / 3.0 * mass,
                            sd: sd(&k),
                        });
                        shares.push(mixed_shares);
                    }
                }
            }
        }
        Ok((strata, sizes, dmass, shares))
    };

    let delta = plan["delta"].as_f64().context("delta")?;
    let rounding = plan["n_test_rounding"]
        .as_u64()
        .context("n_test_rounding")? as usize;
    let target = |z: f64, true_diff: f64| (delta - true_diff) / (Z_975 + z);

    // Expected-count slice SE: the slice keeps the primary cell structure, so
    // its strata keep their primary masses (renormalised over the slice) and
    // a positive stratum contributes `share * n_h` expected sampled documents.
    let slice_se = |strata: &[Stratum],
                    alloc: &[usize],
                    idx: &[usize],
                    share: &dyn Fn(usize) -> f64,
                    deff: f64|
     -> f64 {
        let total: f64 = idx.iter().map(|&i| strata[i].weight).sum();
        let mut v = 0.0;
        for &i in idx {
            let s = share(i);
            if s <= 0.0 || strata[i].weight <= 0.0 {
                continue;
            }
            let w = strata[i].weight / total;
            v += w * w * strata[i].sd * strata[i].sd / (alloc[i] as f64 * s);
        }
        (deff * v).sqrt()
    };

    let mut scenario_reports = serde_json::Map::new();
    let mut n_min = 0usize;
    let mut test_pop = 0usize;
    let mut built = BTreeMap::new();
    for (name, bins) in &scenarios {
        let (test, test_sizes, test_mass, shares) = strata_for("test", Some(bins))?;
        let pop: usize = test.iter().map(|s| s.population).sum();
        test_pop = test_pop.max(pop);
        let deff = |f: f64| cluster_deff(rho, f, &test_sizes);
        let n80 = required_total(&test, STRATUM_FLOOR, target(Z_80, 0.0), deff)?;
        let n90 = required_total(&test, STRATUM_FLOOR, target(Z_90, 0.0), deff)?;
        let n80_half = required_total(&test, STRATUM_FLOOR, target(Z_80, delta / 2.0), deff)?;
        let n90v = n90.with_context(|| format!("90% power unreachable in {name}"))?;
        n_min = n_min.max(n90v.div_ceil(rounding) * rounding);
        scenario_reports.insert(
            name.clone(),
            json!({
                "bin_mass": bins,
                "planned_population": pop,
                "strata": test.len(),
                "n_required_80_power_true_diff_0": n80,
                "n_required_90_power_true_diff_0": n90,
                "n_required_80_power_true_diff_half_delta": n80_half,
            }),
        );
        built.insert(name.clone(), (test, test_sizes, test_mass, shares, pop));
    }
    ensure!(n_min <= test_pop, "n_min above planned population");

    let design = |name: &str, n: usize| -> Result<Value> {
        let (test, test_sizes, test_mass, shares, pop) = &built[name];
        let n = n.min(*pop);
        let alloc = neyman_allocation(test, n, STRATUM_FLOOR)?;
        let f = n as f64 / *pop as f64;
        let deff = cluster_deff(rho, f, test_sizes);
        let se = stratified_se(test, &alloc, deff)?;
        let mut slices = serde_json::Map::new();
        let all: Vec<usize> = (0..test.len()).collect();
        for dname in test_mass.keys() {
            let idx: Vec<usize> = (0..test.len())
                .filter(|&i| test[i].key.starts_with(&format!("{dname}|")))
                .collect();
            let s = slice_se(test, &alloc, &idx, &|_| 1.0, deff);
            slices.insert(dname.clone(), json!({
                "documents": idx.iter().map(|&i| alloc[i]).sum::<usize>(),
                "balanced_mass": test_mass[dname],
                "planning_se": s,
                "p_point_estimate_above_0_030_if_true_slice_gap_0": 1.0 - normal_cdf(0.030 / s),
                "p_point_estimate_above_0_030_if_true_slice_gap_0_010": 1.0 - normal_cdf(0.020 / s),
            }));
        }
        for (slice, pick) in [("held_out_generator", 0usize), ("frontier", 1usize)] {
            let share = |i: usize| if pick == 0 { shares[i].0 } else { shares[i].1 };
            let s = slice_se(test, &alloc, &all, &share, deff);
            let expected: f64 = all.iter().map(|&i| alloc[i] as f64 * share(i)).sum();
            slices.insert(slice.to_string(), json!({
                "expected_sampled_documents": expected,
                "planning_se": s,
                "p_point_estimate_above_0_030_if_true_slice_gap_0": 1.0 - normal_cdf(0.030 / s),
                "p_point_estimate_above_0_030_if_true_slice_gap_0_010": 1.0 - normal_cdf(0.020 / s),
            }));
        }
        let allocation: serde_json::Map<String, Value> = test
            .iter()
            .zip(&alloc)
            .map(|(s, &k)| (s.key.clone(), json!({"planned_population": s.population, "balanced_mass": s.weight, "planning_sd": s.sd, "allocated": k})))
            .collect();
        Ok(json!({
            "n_test": n,
            "sampling_fraction": f,
            "deff": deff,
            "planning_se": se,
            "power_true_diff_0": noninferiority_power(se, delta, 0.0),
            "power_true_diff_0_005": noninferiority_power(se, delta, 0.005),
            "power_true_diff_0_0245_stage3_gap": noninferiority_power(se, delta, gap_recipe),
            "planned_billed_units": n as f64 * mean_units,
            "slices": slices,
            "allocation": allocation,
        }))
    };

    let n_cal = plan["n_cal"].as_u64().context("n_cal")? as usize;
    // Illustration only: the private planning budget in billed units, if the
    // plan gives one. n_test itself is fixed at the balance check.
    let n_plan = plan["planning_budget_units"].as_f64().map(|b| {
        let afford = ((b - n_cal as f64 * mean_units) / mean_units)
            .floor()
            .max(0.0) as usize;
        ((afford / rounding) * rounding).clamp(n_min, test_pop)
    });
    let mut designs = serde_json::Map::new();
    for name in scenarios.keys() {
        let mut d = serde_json::Map::new();
        d.insert("at_n_min".to_string(), design(name, n_min)?);
        if let Some(n) = n_plan {
            d.insert("at_planning_budget".to_string(), design(name, n)?);
        }
        designs.insert(name.clone(), Value::Object(d));
    }

    let (cal, _, _, _) = strata_for("cal", None)?;
    let cal_alloc = neyman_allocation(&cal, n_cal, STRATUM_FLOOR)?;
    let cal_allocation: serde_json::Map<String, Value> = cal
        .iter()
        .zip(&cal_alloc)
        .map(|(s, &n)| (s.key.clone(), json!({"planned_population": s.population, "balanced_mass": s.weight, "allocated": n})))
        .collect();
    let cal_pop: usize = cal.iter().map(|s| s.population).sum();

    let mut flag_limits = serde_json::Map::new();
    for n in [
        1600u64, 1700, 2000, 2500, 2800, 3000, 3100, 3170, 3200, 3250, 3300, 3350, 3400, 3500,
    ] {
        let k = max_flags(n, 0.95, 0.025).context("flag limit")?;
        flag_limits.insert(n.to_string(), json!({"max_flags": k, "bound_at_max": clopper_pearson_upper(k, n, 0.95), "bound_at_next": clopper_pearson_upper(k + 1, n, 0.95)}));
    }
    let report = json!({
        "schema": "slop_ninja_a_comp_pangram_power_v2",
        "meaning": "planning power calculation for the paired A-minus-Pangram balanced involvement Brier on a stratified Pangram Test subsample; counts and moments only; no prose read; no provider calls; costs in billed units only",
        "inputs_sha256": inputs,
        "plan_sha256": sha(plan_path)?,
        "plan": plan,
        "stage3": {
            "documents": docs.len(),
            "works": res_r.len(),
            "balanced_gap_recipe": gap_recipe,
            "balanced_gap_seed17": gap_seed17,
            "reproduces_published_gap": true,
            "per_document_paired_difference": "(A_cal - y)^2 - (Pangram_CAL_mapped - y)^2; recipe = mean over seeds 17/29/41; seed17 = released-artifact analogue",
            "strata": stage3,
            "icc_recipe": icc_r,
            "icc_seed17": icc_s,
            "rho_used": rho,
        },
        "planning_sd": planning,
        "cost": {
            "basis": "Dev-iter human word counts; positives assumed the same length distribution; one started 100-word block per item, at least one",
            "documents": words.len(),
            "mean_words": words.iter().sum::<f64>() / words.len() as f64,
            "mean_billed_units": mean_units,
        },
        "test": {
            "planned_population_max": test_pop,
            "se_target_80": target(Z_80, 0.0),
            "se_target_90": target(Z_90, 0.0),
            "scenarios": scenario_reports,
            "n_min": n_min,
            "n_min_rule": "largest over the bin-mass scenarios of the n required for 90% power at true difference 0, rounded up to a multiple of n_test_rounding",
            "n_at_planning_budget": n_plan,
            "n_test_rule": "at the balance check: floor to a multiple of n_test_rounding of (spendable units - CAL units) / mean CAL billed units per document, clamped to [n_min, primary Test population]; below n_min Tier C is not evaluated",
            "designs": designs,
        },
        "cal": {
            "planned_population": cal_pop,
            "n_cal": n_cal,
            "strata": cal.len(),
            "rule": "domain x origin cell, proportional to balanced CAL mass (S_h = 1) with the same floor, caps and rounding",
            "planned_billed_units": n_cal as f64 * mean_units,
            "allocation": cal_allocation,
        },
        "human_fpr_flag_limits": flag_limits,
        "minimum_billed_units": (n_cal + n_min) as f64 * mean_units,
    });
    fs::create_dir_all(out.parent().context("parent")?)?;
    fs::write(out, serde_json::to_vec_pretty(&report)?)?;
    let brief: serde_json::Map<String, Value> = designs
        .iter()
        .map(|(k, v)| {
            let m = &v["at_n_min"];
            (k.clone(), json!({"se": m["planning_se"], "deff": m["deff"], "power0": m["power_true_diff_0"], "power005": m["power_true_diff_0_005"], "slices": m["slices"].as_object().map(|o| o.iter().map(|(s, x)| (s.clone(), x["planning_se"].clone())).collect::<serde_json::Map<_, _>>()), "at_plan": v.get("at_planning_budget").map(|p| json!({"n": p["n_test"], "se": p["planning_se"], "power0": p["power_true_diff_0"], "power005": p["power_true_diff_0_005"]}))}))
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "gap_recipe": gap_recipe, "gap_seed17": gap_seed17, "rho": rho, "icc": [icc_r, icc_s],
            "cost": report["cost"], "scenarios": report["test"]["scenarios"], "n_min": n_min, "n_plan": n_plan,
            "designs": brief, "cal_strata": cal.len(),
            "flags": report["human_fpr_flag_limits"], "min_units": report["minimum_billed_units"],
        }))?
    );
    Ok(())
}

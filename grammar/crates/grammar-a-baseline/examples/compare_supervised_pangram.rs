//! Paired provenance-labelled comparison. Pangram fractions are not confidence.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
#[path = "compare_supervised_pangram/final_test.rs"]
mod final_test;
#[allow(dead_code)]
#[path = "final_test_binding/mod.rs"]
mod final_test_binding;
#[allow(dead_code)]
#[path = "compare_supervised_pangram/test_binding.rs"]
mod test_binding;
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}
fn rows(p: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(p)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn avg(v: &[(String, f64)]) -> Option<f64> {
    let mut g = BTreeMap::<&str, Vec<f64>>::new();
    for (k, x) in v {
        g.entry(k).or_default().push(*x);
    }
    (!g.is_empty()).then(|| {
        g.values()
            .map(|x| x.iter().sum::<f64>() / x.len() as f64)
            .sum::<f64>()
            / g.len() as f64
    })
}
fn ci(v: &[(String, f64)]) -> Value {
    let mut g = BTreeMap::<&str, Vec<f64>>::new();
    for (k, x) in v {
        g.entry(k).or_default().push(*x);
    }
    let means = g
        .values()
        .map(|v| v.iter().sum::<f64>() / v.len() as f64)
        .collect::<Vec<_>>();
    if means.len() < 2 {
        return Value::Null;
    }
    let mut seed = 20261026u64;
    let mut draws = Vec::new();
    for _ in 0..1000 {
        let mut total = 0.;
        for _ in &means {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            total += means[seed as usize % means.len()];
        }
        draws.push(total / means.len() as f64);
    }
    draws.sort_by(f64::total_cmp);
    json!({"lower":draws[24],"upper":draws[974],"families":means.len(),"method":"1000_paired_source_family_bootstraps_seed20261026"})
}
fn map(s: f64, t: f64) -> f64 {
    let p = s.clamp(0.001, 0.999);
    1. / (1. + (-((p / (1. - p)).ln() / t)).exp())
}
fn origin_balanced(values: &[(String, usize, f64)]) -> Vec<(String, f64)> {
    let mut cells = BTreeMap::<(String, usize), Vec<f64>>::new();
    for (g, y, v) in values {
        cells.entry((g.clone(), *y)).or_default().push(*v);
    }
    cells
        .into_iter()
        .map(|((g, _), v)| (g, v.iter().sum::<f64>() / v.len() as f64))
        .collect()
}
fn target(row: &Value) -> Result<usize> {
    let y = row["targets"]["origin"]
        .as_u64()
        .context("unavailable origin target")? as usize;
    ensure!(
        y < 3 && row["targets"]["ai_involvement"].as_bool() == Some(y != 0),
        "inconsistent origin/involvement targets"
    );
    Ok(y)
}
pub(crate) fn observation(
    root: &Path,
    row: &Value,
    hashes: &BTreeMap<String, String>,
) -> Result<Value> {
    let id = row["id"].as_str().context("id")?;
    let v = read(&root.join(id).join("result.json"))?;
    ensure!(
        v["id"] == id && v["source_group"] == row["source_group"],
        "Pangram binding mismatch"
    );
    ensure!(
        v["text_sha256"].as_str() == Some(hashes.get(id).context("source hash absent")?.as_str()),
        "Pangram exact input mismatch"
    );
    ensure!(
        v["response"]["stage"] == "STAGE_SUCCESS",
        "provider failure"
    );
    for k in ["fraction_ai", "fraction_ai_assisted"] {
        ensure!(
            v["response"][k]
                .as_f64()
                .is_some_and(|x| x.is_finite() && (0.0..=1.).contains(&x)),
            "invalid fraction"
        );
    }
    let s = v["response"]["fraction_ai"].as_f64().unwrap()
        + v["response"]["fraction_ai_assisted"].as_f64().unwrap();
    ensure!(s <= 1.000001, "fraction sum");
    Ok(v)
}
pub(crate) fn score(v: &Value) -> f64 {
    (v["response"]["fraction_ai"].as_f64().unwrap()
        + v["response"]["fraction_ai_assisted"].as_f64().unwrap())
    .min(1.)
}

type CalPair = (String, f64, f64, usize); // family, content fraction, involvement, origin

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mapping {
    Temperature,
    Isotonic,
}

fn mapping_args(a: &[String]) -> Result<Mapping> {
    match a.len() {
        6 => Ok(Mapping::Temperature),
        8 if a[6] == "--mapping" => match a[7].as_str() {
            "temperature_v1" => Ok(Mapping::Temperature),
            "isotonic_v1" => Ok(Mapping::Isotonic),
            _ => anyhow::bail!("unknown mapping"),
        },
        _ => anyhow::bail!(
            "compare_supervised_pangram DATA FIT SPLIT SPLIT_RESULTS CAL_RESULTS NEW_REPORT [--mapping temperature_v1|isotonic_v1]"
        ),
    }
}

fn calibration_weights(pairs: &[CalPair], balanced: bool) -> Result<Vec<f64>> {
    ensure!(!pairs.is_empty(), "no Calibration observations");
    let mut families = BTreeMap::<&str, usize>::new();
    let mut cells = BTreeMap::<(&str, usize), usize>::new();
    for (family, score, target, origin) in pairs {
        ensure!(
            !family.is_empty()
                && score.is_finite()
                && (0.0..=1.).contains(score)
                && *origin < 3
                && *target == if *origin != 0 { 1. } else { 0. },
            "invalid Calibration pair"
        );
        *families.entry(family).or_default() += 1;
        *cells.entry((family, *origin)).or_default() += 1;
    }
    let mut strata = BTreeMap::<&str, usize>::new();
    for (family, _) in cells.keys() {
        *strata.entry(family).or_default() += 1;
    }
    Ok(pairs
        .iter()
        .map(|(family, _, _, origin)| {
            let n = if balanced {
                strata[family.as_str()] * cells[&(family.as_str(), *origin)]
            } else {
                families[family.as_str()]
            };
            1.0 / (families.len() * n) as f64
        })
        .collect())
}

#[derive(Clone, Debug)]
struct Knot {
    x: f64,
    fitted: f64,
    weight: f64,
    positives: f64,
    rows: usize,
}
#[derive(Clone, Debug)]
struct Block {
    first: usize,
    last: usize,
    weight: f64,
    positives: f64,
}
#[derive(Clone, Debug)]
pub(crate) struct Isotonic {
    knots: Vec<Knot>,
    blocks: Vec<Block>,
}

impl Isotonic {
    /// Load an already frozen Calibration mapping without reading labels or fitting.
    pub(crate) fn from_frozen_description(v: &Value) -> Result<Self> {
        ensure!(
            v["method"] == "isotonic_v1"
                && v["fit_split"] == "calibration"
                && v["input"] == "fraction_ai_plus_fraction_ai_assisted_clamped_to_1"
                && v["target"] == "binary_any_AI_involvement_from_origin"
                && v["interpolation"]
                    == "linear_between_adjacent_unique_CAL_fraction_knots; constant_at_each_end_outside_CAL_support"
                && v["native_pangram_confidence"] == false,
            "unqualified frozen mapping meaning"
        );
        let scalar = |v: &Value| -> Result<f64> {
            let x = v.as_f64().context("mapping scalar")?;
            ensure!(x.is_finite(), "nonfinite mapping");
            Ok(x)
        };
        let mut knots = Vec::new();
        for k in v["knots"].as_array().context("mapping knots")? {
            let x = scalar(&k["fraction"])?;
            let fitted = scalar(&k["involvement"])?;
            let weight = scalar(&k["weight"])?;
            let positives = scalar(&k["weighted_positives"])?;
            let rows = k["rows"].as_u64().context("knot rows")? as usize;
            ensure!(
                (0.0..=1.).contains(&x)
                    && (0.0..=1.).contains(&fitted)
                    && weight > 0.
                    && positives >= 0.
                    && positives <= weight + 1e-12
                    && rows > 0,
                "invalid frozen knot"
            );
            knots.push(Knot {
                x,
                fitted,
                weight,
                positives,
                rows,
            });
        }
        ensure!(
            !knots.is_empty()
                && knots
                    .windows(2)
                    .all(|p| p[0].x < p[1].x && p[0].fitted <= p[1].fitted),
            "frozen mapping must be sorted and monotone"
        );
        let mut blocks = Vec::new();
        let mut next = 0;
        for b in v["blocks"].as_array().context("mapping blocks")? {
            let first = b["first_knot"].as_u64().context("block first")? as usize;
            let last = b["last_knot"].as_u64().context("block last")? as usize;
            let weight = scalar(&b["weight"])?;
            let positives = scalar(&b["weighted_positives"])?;
            ensure!(
                first == next
                    && first <= last
                    && last < knots.len()
                    && weight > 0.
                    && positives >= 0.
                    && positives <= weight + 1e-12,
                "invalid frozen block"
            );
            ensure!(
                knots[first..=last]
                    .iter()
                    .all(|k| (k.fitted - (positives / weight).clamp(0., 1.)).abs() <= 1e-12),
                "block/knots disagree"
            );
            ensure!(
                (knots[first..=last].iter().map(|k| k.weight).sum::<f64>() - weight).abs() <= 1e-12
                    && (knots[first..=last].iter().map(|k| k.positives).sum::<f64>() - positives)
                        .abs()
                        <= 1e-12,
                "block/knot mass differs"
            );
            blocks.push(Block {
                first,
                last,
                weight,
                positives,
            });
            next = last + 1;
        }
        ensure!(next == knots.len(), "incomplete frozen mapping blocks");
        Ok(Self { knots, blocks })
    }
    // Equal scores must have one target before PAV, irrespective of row order.
    pub(crate) fn fit(split: &str, pairs: &[CalPair], balanced: bool) -> Result<Self> {
        ensure!(
            split == "calibration",
            "isotonic fitting is Calibration-only"
        );
        let weights = calibration_weights(pairs, balanced)?;
        let mut samples = pairs.iter().zip(weights).collect::<Vec<_>>();
        samples.sort_by(|(a, _), (b, _)| {
            a.1.total_cmp(&b.1)
                .then_with(|| a.0.cmp(&b.0))
                .then_with(|| a.3.cmp(&b.3))
        });
        let mut knots = Vec::<Knot>::new();
        for ((_, x, y, _), weight) in samples {
            if let Some(last) = knots.last_mut().filter(|last| last.x == *x) {
                last.weight += weight;
                last.positives += weight * y;
                last.rows += 1;
            } else {
                knots.push(Knot {
                    x: *x,
                    fitted: 0.,
                    weight,
                    positives: weight * y,
                    rows: 1,
                });
            }
        }
        let mut blocks = Vec::<Block>::new();
        for (index, knot) in knots.iter().enumerate() {
            blocks.push(Block {
                first: index,
                last: index,
                weight: knot.weight,
                positives: knot.positives,
            });
            while blocks.len() >= 2 {
                let n = blocks.len();
                let a = &blocks[n - 2];
                let b = &blocks[n - 1];
                if a.positives / a.weight <= b.positives / b.weight {
                    break;
                }
                let right = blocks.pop().unwrap();
                let left = blocks.last_mut().unwrap();
                left.last = right.last;
                left.weight += right.weight;
                left.positives += right.positives;
            }
        }
        for block in &blocks {
            for knot in &mut knots[block.first..=block.last] {
                knot.fitted = (block.positives / block.weight).clamp(0., 1.);
            }
        }
        Ok(Self { knots, blocks })
    }
    pub(crate) fn predict(&self, x: f64) -> f64 {
        let first = &self.knots[0];
        let last = self.knots.last().unwrap();
        if x <= first.x {
            return first.fitted;
        }
        if x >= last.x {
            return last.fitted;
        }
        let upper = self.knots.partition_point(|k| k.x <= x);
        let a = &self.knots[upper - 1];
        let b = &self.knots[upper];
        (a.fitted + (b.fitted - a.fitted) * ((x - a.x) / (b.x - a.x))).clamp(0., 1.)
    }
    pub(crate) fn description(&self, balanced: bool) -> Value {
        json!({"method":"isotonic_v1","fit_split":"calibration",
            "input":"fraction_ai_plus_fraction_ai_assisted_clamped_to_1","target":"binary_any_AI_involvement_from_origin",
            "objective":"weighted_squared_error","algorithm":"exact_score_tie_aggregation_then_weighted_pool_adjacent_violators",
            "weighting":if balanced {"family_origin_balanced"} else {"family"},
            "weight_formula":if balanced {"1/(families * observed_origin_strata_in_family * rows_in_family_origin_cell)"} else {"1/(families * rows_in_family)"},
            "interpolation":"linear_between_adjacent_unique_CAL_fraction_knots; constant_at_each_end_outside_CAL_support",
            "target_clip":null,"output_range":[0,1],"native_pangram_confidence":false,
            "meaning":"our CAL-fitted involvement proxy; not native Pangram confidence or an AI-share estimate",
            "knots":self.knots.iter().map(|k|json!({"fraction":k.x,"involvement":k.fitted,"weight":k.weight,"weighted_positives":k.positives,"rows":k.rows})).collect::<Vec<_>>(),
            "blocks":self.blocks.iter().map(|b|json!({"first_knot":b.first,"last_knot":b.last,"weight":b.weight,"weighted_positives":b.positives})).collect::<Vec<_>>()})
    }
}

fn isotonic_roster(cal: &[Value], eval: &[Value], population: &Value) -> Result<()> {
    let roster = population["rows"]
        .as_array()
        .context("population rows")?
        .iter()
        .map(|r| Ok((r["id"].as_str().context("population ID")?, r)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut ids = BTreeSet::new();
    let mut groups = BTreeMap::new();
    for (split, rows) in [("calibration", cal), ("development", eval)] {
        for row in rows {
            ensure!(
                row["frozen_split"] == split && row["purpose"] == "query",
                "isotonic refuses Test/reference/cross-split rows"
            );
            let id = row["id"].as_str().context("ID")?;
            ensure!(ids.insert(id), "duplicate or cross-split ID");
            let original = roster.get(id).context("ID absent from bound population")?;
            ensure!(
                original["split"] == split
                    && original["purpose"] == "query"
                    && original["group"] == row["source_group"],
                "numeric/population partition binding differs"
            );
            let group = row["source_group"].as_str().context("family")?;
            if let Some(previous) = groups.insert(group, split) {
                ensure!(previous == split, "CAL/Dev source-family overlap");
            }
        }
    }
    Ok(())
}

fn compare(a: &[String]) -> Result<()> {
    let mapping = mapping_args(a)?;
    ensure!(
        matches!(a[2].as_str(), "development" | "test"),
        "comparison split"
    );
    ensure!(
        mapping != Mapping::Isotonic || a[2] == "development",
        "isotonic_v1 currently supports non-Test Development evaluation only"
    );
    let data = Path::new(&a[0]);
    let fit = Path::new(&a[1]);
    let manifest = read(&data.join("manifest.json"))?;
    let fit_report = read(&fit.join("fit.json"))?;
    let balanced = fit_report["query_weighting"] == "family_origin_balanced";
    ensure!(
        fit_report["weights_sha256"] == sha(&fit.join("weights.json"))?,
        "fit/weights mismatch"
    );
    ensure!(
        manifest["population_file_sha256"] == sha(&data.join("population.json"))?,
        "population changed"
    );
    let population = read(&data.join("population.json"))?;
    let mut hashes = BTreeMap::new();
    for row in population["rows"].as_array().context("population rows")? {
        let id = row["id"].as_str().context("population id")?;
        let digest = row["metadata"]["text_sha256"]
            .as_str()
            .context("population source hash")?;
        ensure!(
            hashes.insert(id.to_owned(), digest.to_owned()).is_none(),
            "duplicate source hash binding"
        );
    }
    let cal = rows(&data.join("calibration.jsonl"))?;
    let eval = rows(&data.join(format!("{}.jsonl", a[2])))?;
    if mapping == Mapping::Isotonic {
        isotonic_roster(&cal, &eval, &population)?;
    }
    for part in ["calibration", a[2].as_str()] {
        let binding = manifest["splits"]
            .as_array()
            .context("splits")?
            .iter()
            .find(|b| b["split"] == part)
            .context("split binding")?;
        ensure!(
            binding["sha256"] == sha(&data.join(format!("{part}.jsonl")))?,
            "numeric data changed"
        );
    }
    let predictions = read(&fit.join(format!("{}-predictions.json", a[2])))?;
    let mut pred = BTreeMap::new();
    for row in predictions.as_array().context("predictions")? {
        ensure!(
            pred.insert(
                row["id"].as_str().context("prediction id")?.to_owned(),
                row["prediction"].clone()
            )
            .is_none(),
            "duplicate prediction"
        );
    }
    if mapping == Mapping::Isotonic {
        ensure!(
            pred.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == eval
                    .iter()
                    .map(|r| r["id"].as_str().context("evaluation ID"))
                    .collect::<Result<BTreeSet<_>>>()?,
            "prediction IDs differ from evaluation cohort"
        );
    }
    let mut cal_pairs = Vec::new();
    let mut exclusions = Vec::new();
    for row in &cal {
        let y = match target(row) {
            Ok(y) => y,
            Err(e) => {
                exclusions
                    .push(json!({"split":"calibration","id":row["id"],"reason":e.to_string()}));
                continue;
            }
        };
        match observation(Path::new(&a[4]), row, &hashes) {
            Ok(v) => {
                ensure!(
                    mapping != Mapping::Isotonic || v["split"] == "calibration",
                    "CAL observation split differs"
                );
                cal_pairs.push((
                    row["source_group"].as_str().context("group")?.to_owned(),
                    score(&v),
                    if row["targets"]["ai_involvement"] == true {
                        1.
                    } else {
                        0.
                    },
                    y,
                ));
            }
            Err(error) => exclusions
                .push(json!({"split":"calibration","id":row["id"],"reason":error.to_string()})),
        }
    }
    ensure!(!cal_pairs.is_empty(), "no Calibration comparisons");
    let objective = |t: f64| {
        let values = cal_pairs
            .iter()
            .map(|(g, s, y, origin)| (g.clone(), *origin, (map(*s, t) - y).powi(2)))
            .collect::<Vec<_>>();
        avg(&if balanced {
            origin_balanced(&values)
        } else {
            values.into_iter().map(|(g, _, v)| (g, v)).collect()
        })
        .unwrap()
    };
    let temp = std::iter::once(1.)
        .chain((-30..=30).map(|k| (k as f64 * 0.1).exp()))
        .min_by(|x, y| {
            objective(*x)
                .total_cmp(&objective(*y))
                .then_with(|| (x - 1.).abs().total_cmp(&(y - 1.).abs()))
        })
        .unwrap();
    let isotonic = if mapping == Mapping::Isotonic {
        Some(Isotonic::fit("calibration", &cal_pairs, balanced)?)
    } else {
        None
    };
    let mut legacy_pg = Vec::new();
    let mut legacy_balanced_pg = Vec::new();
    let mut legacy_gap = Vec::new();
    let mut legacy_balanced_gap = Vec::new();
    let mut pg = Vec::new();
    let mut sn = Vec::new();
    let mut gap = Vec::new();
    let mut balanced_pg = Vec::new();
    let mut balanced_sn = Vec::new();
    let mut balanced_gap = Vec::new();
    let mut versions = std::collections::BTreeSet::new();
    let mut pg_fp = Vec::new();
    let mut sn_fp = Vec::new();
    let mut raw_share_error = Vec::new();
    let mut model_share_error = Vec::new();
    let mut paired = Vec::new();
    let mut pg_bins = vec![Vec::<(String, f64, f64)>::new(); 10];
    let mut slices = BTreeMap::<String, Vec<(String, f64, f64)>>::new();
    for row in &eval {
        let y = match target(row) {
            Ok(y) => y,
            Err(e) => {
                exclusions.push(json!({"split":a[2],"id":row["id"],"reason":e.to_string()}));
                continue;
            }
        };
        let observed = match observation(Path::new(&a[3]), row, &hashes) {
            Ok(v) => v,
            Err(error) => {
                exclusions.push(json!({"split":a[2],"id":row["id"],"reason":error.to_string()}));
                continue;
            }
        };
        let id = row["id"].as_str().context("id")?;
        let model = pred.get(id).context("missing model prediction")?;
        let group = row["source_group"].as_str().context("group")?.to_owned();
        let target = if row["targets"]["ai_involvement"] == true {
            1.
        } else {
            0.
        };
        let fraction = score(&observed);
        ensure!(
            mapping != Mapping::Isotonic || observed["split"] == a[2],
            "evaluation observation split differs"
        );
        let legacy_p = map(fraction, temp);
        let p = isotonic.as_ref().map_or(legacy_p, |m| m.predict(fraction));
        let m = model["ai_confidence_percent"]
            .as_f64()
            .context("model confidence")?
            / 100.;
        ensure!((0.0..=1.).contains(&m), "model confidence range");
        let pe = (p - target).powi(2);
        let me = (m - target).powi(2);
        legacy_pg.push((group.clone(), (legacy_p - target).powi(2)));
        legacy_balanced_pg.push((group.clone(), y, (legacy_p - target).powi(2)));
        legacy_gap.push((group.clone(), me - (legacy_p - target).powi(2)));
        legacy_balanced_gap.push((group.clone(), y, me - (legacy_p - target).powi(2)));
        balanced_pg.push((group.clone(), y, pe));
        balanced_sn.push((group.clone(), y, me));
        balanced_gap.push((group.clone(), y, me - pe));
        versions.insert((
            observed["model"].as_str().context("selector")?.to_owned(),
            observed["response"]["version"]
                .as_str()
                .context("provider version")?
                .to_owned(),
        ));
        pg.push((group.clone(), pe));
        sn.push((group.clone(), me));
        gap.push((group.clone(), me - pe));
        pg_bins[((p * 10.).floor() as usize).min(9)].push((group.clone(), p, target));
        if target == 0. {
            pg_fp.push((group.clone(), if p >= 0.5 { 1. } else { 0. }));
            sn_fp.push((group.clone(), if m >= 0.5 { 1. } else { 0. }));
        }
        if let Some(truth) = row["targets"]["share"].as_f64() {
            let model_share = model["ai_share_percent"].as_f64().context("model share")? / 100.;
            raw_share_error.push((group.clone(), (fraction - truth).abs() * 100.));
            model_share_error.push((group.clone(), (model_share - truth).abs() * 100.));
            let key = if truth == 0. {
                "0"
            } else if truth == 1. {
                "100"
            } else if truth <= 0.25 {
                "0-25"
            } else if truth <= 0.5 {
                "25-50"
            } else if truth <= 0.75 {
                "50-75"
            } else {
                "75-100"
            };
            slices.entry(key.into()).or_default().push((
                group.clone(),
                (fraction - truth).abs() * 100.,
                (model_share - truth).abs() * 100.,
            ));
        }
        paired.push(json!({"id":id,"source_group":group,"origin_target":row["targets"]["origin"],"share_target":row["targets"]["share"],"pangram_fraction":fraction,"mapped_pangram_involvement":p,"slop_ninja_involvement":m,"response_version":observed["response"]["version"],"selector":observed["model"],"observation_sha256":sha(&Path::new(&a[3]).join(id).join("result.json"))?}));
        if isotonic.is_some() {
            paired.last_mut().unwrap()["legacy_temperature_pangram_involvement"] = json!(legacy_p);
        }
    }
    ensure!(!paired.is_empty(), "no paired observations");
    ensure!(
        versions.len() == 1,
        "mixed Pangram provider versions/selectors require separate reports"
    );
    for row in &cal {
        if target(row).is_ok() {
            if let Ok(v) = observation(Path::new(&a[4]), row, &hashes) {
                ensure!(
                    versions.contains(&(
                        v["model"].as_str().context("CAL selector")?.into(),
                        v["response"]["version"]
                            .as_str()
                            .context("CAL version")?
                            .into()
                    )),
                    "Calibration provider version differs"
                );
            }
        }
    }
    let calibration_bins=pg_bins.iter().enumerate().filter(|(_,v)|!v.is_empty()).map(|(i,v)|json!({"bin":i,"rows":v.len(),"predicted":avg(&v.iter().map(|(g,p,_)|(g.clone(),*p)).collect::<Vec<_>>()),"observed":avg(&v.iter().map(|(g,_,y)|(g.clone(),*y)).collect::<Vec<_>>())})).collect::<Vec<_>>();
    let mut result = json!({"schema":"slop_ninja_a_supervised_pangram_comparison_v1","split":a[2],"requested":eval.len(),"paired":paired.len(),"excluded":exclusions,"calibration_requested":cal.len(),"calibration_paired":cal_pairs.len(),
        "ground_truth":{"origin":"retained production histories, with historical-human proxy status preserved","share":"measured final-word replacement-span fractions where provenance exists; unavailable targets masked","detector_outputs_are_labels":false},
        "calibration":{"method":"clipped_nonhuman_fraction_logodds_temperature;Calibration_only","weighting":if balanced {"family_origin_balanced"} else {"family"},"clip":[0.001,0.999],"grid":"exp(k*0.1),k=-30..30,includes1","temperature":temp,"native_pangram_confidence":false,"meaning":"our Calibration-fitted involvement proxy; Pangram does not separately report our share and confidence targets"},
        "family_origin_balanced":{"pangram_brier":avg(&origin_balanced(&balanced_pg)),"slop_ninja_brier":avg(&origin_balanced(&balanced_sn)),"paired_gap":avg(&origin_balanced(&balanced_gap)),"paired_gap_95ci":ci(&origin_balanced(&balanced_gap))},
        "pangram":{"involvement_brier":avg(&pg),"brier_95ci":ci(&pg),"human_fpr_at_0_5":avg(&pg_fp),"human_rows":pg_fp.len(),"calibration_bins":calibration_bins,"nonhuman_fraction_share_mae_percentage_points":avg(&raw_share_error)},
        "slop_ninja_paired":{"involvement_brier":avg(&sn),"brier_95ci":ci(&sn),"human_fpr_at_0_5":avg(&sn_fp),"share_mae_percentage_points":avg(&model_share_error)},
        "paired_brier_gap_slop_minus_pangram":avg(&gap),"paired_brier_gap_95ci":ci(&gap),"by_realized_share":slices.iter().map(|(k,v)|(k,json!({"rows":v.len(),"pangram_mae":avg(&v.iter().map(|(g,p,_)|(g.clone(),*p)).collect::<Vec<_>>()),"slop_ninja_mae":avg(&v.iter().map(|(g,_,m)|(g.clone(),*m)).collect::<Vec<_>>())}))).collect::<BTreeMap<_,_>>(),
        "limits":["Pangram text fractions are not native any-AI probabilities","our mapping uses Calibration labels only","nonhuman text fraction and exact replacement-span share have different measurement semantics","source-family intervals do not establish rare false-positive guarantees","historical human proxies and conservative grammar exposure remain explicit"],
        "records":paired,"contract_sha256":manifest["contract_sha256"],"weights_sha256":sha(&fit.join("weights.json"))?,"parity_established":false,"test_used_for_selection":false});
    if let Some(isotonic) = isotonic {
        let weights = calibration_weights(&cal_pairs, balanced)?;
        let cal_loss = cal_pairs
            .iter()
            .zip(weights)
            .map(|((_, s, y, _), w)| w * (isotonic.predict(*s) - y).powi(2))
            .sum::<f64>();
        let legacy_calibration = result["calibration"].clone();
        result["calibration"] = isotonic.description(balanced);
        result["calibration"]["weighted_CAL_brier_in_sample"] = json!(cal_loss);
        result["calibration"]["provenance"] = json!({"calibration_numeric_sha256":sha(&data.join("calibration.jsonl"))?,
            "dataset_manifest_sha256":sha(&data.join("manifest.json"))?,"population_sha256":sha(&data.join("population.json"))?,
            "fit_report_sha256":sha(&fit.join("fit.json"))?,"development_used_for_fitting":false,
            "calibration_ids_sha256":hex::encode(Sha256::digest(serde_json::to_vec(&cal.iter().map(|r|r["id"].clone()).collect::<Vec<_>>())?)),
            "comparator_source_sha256":hex::encode(Sha256::digest(include_bytes!("compare_supervised_pangram.rs")))});
        result["legacy_temperature_diagnostic"] = json!({"calibration":legacy_calibration,
            "pangram_family_brier":avg(&legacy_pg),"pangram_family_origin_balanced_brier":avg(&origin_balanced(&legacy_balanced_pg)),
            "paired_family_gap":avg(&legacy_gap),"paired_family_origin_balanced_gap":avg(&origin_balanced(&legacy_balanced_gap)),
            "decision_boundary_fraction":0.5,"native_pangram_confidence":false});
        result["limits"].as_array_mut().unwrap().push(json!("isotonic_v1 was added after legacy Development comparisons; this is an exposed research diagnostic, not new held-out evidence"));
    }
    ensure!(!Path::new(&a[5]).exists(), "new output required");
    fs::write(&a[5], serde_json::to_vec_pretty(&result)?)?;
    println!(
        "{}",
        json!({"paired":result["paired"],"pangram_brier":result["pangram"]["involvement_brier"],"model_brier":result["slop_ninja_paired"]["involvement_brier"],"gap":result["paired_brier_gap_slop_minus_pangram"]})
    );
    Ok(())
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|a| a == "--frozen-test") {
        final_test::run(&args[1..])
    } else {
        compare(&args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frozen_mapping_round_trip_and_tamper_rejection() {
        let fit = Isotonic::fit(
            "calibration",
            &[
                pair("a", 0.1, 0),
                pair("a", 0.3, 1),
                pair("b", 0.5, 0),
                pair("b", 0.9, 2),
            ],
            true,
        )
        .unwrap();
        let description = fit.description(true);
        let serialized = serde_json::to_vec(&description).unwrap();
        let loaded =
            Isotonic::from_frozen_description(&serde_json::from_slice(&serialized).unwrap())
                .unwrap();
        for i in 0..=1000 {
            assert_eq!(
                fit.predict(i as f64 / 1000.),
                loaded.predict(i as f64 / 1000.)
            );
        }
        for pointer in [
            "/fit_split",
            "/knots/0/fraction",
            "/knots/1/involvement",
            "/blocks/0/weight",
            "/blocks/0/last_knot",
        ] {
            let mut bad = description.clone();
            *bad.pointer_mut(pointer).unwrap() = match pointer {
                "/fit_split" => json!("test"),
                "/knots/0/fraction" => json!(0.8),
                "/knots/1/involvement" => json!(0.),
                "/blocks/0/weight" => json!(1.),
                _ => json!(999),
            };
            assert!(
                Isotonic::from_frozen_description(&bad).is_err(),
                "{pointer}"
            );
        }
    }
    fn pair(family: &str, score: f64, origin: usize) -> CalPair {
        (
            family.into(),
            score,
            if origin == 0 { 0. } else { 1. },
            origin,
        )
    }
    #[test]
    fn isotonic_aggregates_ties_before_pav_and_is_order_invariant() {
        let mut pairs = vec![pair("a", 0.2, 1), pair("a", 0.2, 0), pair("a", 0.8, 2)];
        let fitted = Isotonic::fit("calibration", &pairs, true).unwrap();
        assert_eq!(fitted.knots.len(), 2);
        assert_eq!(fitted.knots[0].rows, 2);
        assert!((fitted.predict(0.2) - 0.5).abs() < 1e-12);
        assert_eq!(fitted.predict(0.8), 1.);
        pairs.reverse();
        assert_eq!(
            fitted.description(true),
            Isotonic::fit("calibration", &pairs, true)
                .unwrap()
                .description(true)
        );
        let one = Isotonic::fit(
            "calibration",
            &[pair("a", 0.2, 0), pair("a", 0.2, 1)],
            false,
        )
        .unwrap();
        assert_eq!(one.predict(0.), 0.5);
        assert_eq!(one.predict(1.), 0.5);
    }
    #[test]
    fn isotonic_weights_match_equal_family_and_origin_strata() {
        let mut pairs = vec![pair("a", 0., 0), pair("a", 0.2, 1)];
        pairs.extend((0..30).map(|_| pair("a", 0.6, 2)));
        pairs.push(pair("b", 0., 0));
        let weights = calibration_weights(&pairs, true).unwrap();
        assert!((weights.iter().sum::<f64>() - 1.).abs() < 1e-12);
        assert!((weights[0] - 1. / 6.).abs() < 1e-12);
        assert!((weights[1] - 1. / 6.).abs() < 1e-12);
        assert!((weights[2..32].iter().sum::<f64>() - 1. / 6.).abs() < 1e-12);
        assert_eq!(weights[32], 0.5);
        let pooled = vec![
            pair("a", 0.1, 1),
            pair("a", 0.2, 1),
            pair("a", 0.3, 0),
            pair("b", 0.9, 0),
        ];
        let family = Isotonic::fit("calibration", &pooled, false).unwrap();
        let balanced = Isotonic::fit("calibration", &pooled, true).unwrap();
        assert_eq!(family.blocks.len(), 1);
        assert!((family.predict(0.5) - 1. / 3.).abs() < 1e-12);
        assert!((balanced.predict(0.5) - 0.25).abs() < 1e-12);
    }
    #[test]
    fn isotonic_interpolates_monotonically_clamps_ends_and_can_move_boundary() {
        let fitted =
            Isotonic::fit("calibration", &[pair("a", 0.1, 0), pair("a", 0.2, 1)], true).unwrap();
        assert_eq!(fitted.predict(0.), 0.);
        assert_eq!(fitted.predict(1.), 1.);
        assert!((fitted.predict(0.15) - 0.5).abs() < 1e-12);
        assert_eq!(fitted.predict(0.2), 1.);
        assert!(map(0.2, 1.) < 0.5);
        let values = (0..=1000)
            .map(|i| fitted.predict(i as f64 / 1000.))
            .collect::<Vec<_>>();
        assert!(values.windows(2).all(|w| w[0] <= w[1]));
        assert!(Isotonic::fit("development", &[pair("a", 0.2, 1)], true).is_err());
        assert!(Isotonic::fit("test", &[pair("a", 0.2, 1)], true).is_err());
        assert!(Isotonic::fit("calibration", &[], true).is_err());
        assert!(Isotonic::fit("calibration", &[pair("a", f64::NAN, 1)], true).is_err());
    }
    #[test]
    fn isotonic_rejects_reference_and_cross_split_calibration_sources() {
        let cal = vec![
            json!({"id":"cal","source_group":"c","frozen_split":"calibration","purpose":"query"}),
        ];
        let mut dev = vec![
            json!({"id":"dev","source_group":"d","frozen_split":"development","purpose":"query"}),
        ];
        let population = json!({"rows":[{"id":"cal","group":"c","split":"calibration","purpose":"query"},
            {"id":"dev","group":"d","split":"development","purpose":"query"}]});
        isotonic_roster(&cal, &dev, &population).unwrap();
        dev[0]["purpose"] = "reference".into();
        assert!(isotonic_roster(&cal, &dev, &population).is_err());
        dev[0]["purpose"] = "query".into();
        dev[0]["source_group"] = "c".into();
        let mut overlapping = population.clone();
        overlapping["rows"][1]["group"] = "c".into();
        assert!(isotonic_roster(&cal, &dev, &overlapping).is_err());
        dev[0]["frozen_split"] = "test".into();
        assert!(isotonic_roster(&cal, &dev, &population).is_err());
    }
    #[test]
    fn legacy_mapping_is_default_and_only_explicit_isotonic_enables_new_behavior() {
        let mut args = vec!["x".to_owned(); 6];
        assert_eq!(mapping_args(&args).unwrap(), Mapping::Temperature);
        args.extend(["--mapping".into(), "isotonic_v1".into()]);
        assert_eq!(mapping_args(&args).unwrap(), Mapping::Isotonic);
        args[7] = "temperature_v1".into();
        assert_eq!(mapping_args(&args).unwrap(), Mapping::Temperature);
        args[7] = "unknown".into();
        assert!(mapping_args(&args).is_err());
    }
    #[test]
    fn end_to_end_development_labels_cannot_fit_isotonic() {
        fn write(p: &Path, v: &Value) {
            fs::write(p, serde_json::to_vec(v).unwrap()).unwrap();
        }
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let fit = temp.path().join("fit");
        let cal_results = temp.path().join("cal");
        let dev_results = temp.path().join("dev");
        for p in [&data, &fit, &cal_results, &dev_results] {
            fs::create_dir(p).unwrap();
        }
        let make = |id: &str, split: &str, group: &str, origin: usize| {
            json!({"id":id,"frozen_split":split,"source_group":group,
            "purpose":"query","targets":{"origin":origin,"ai_involvement":origin!=0,"share":null}})
        };
        let cal = [
            make("cal-human", "calibration", "c", 0),
            make("cal-ai", "calibration", "c", 1),
        ];
        let mut dev = make("dev", "development", "d", 0);
        let mut population = Vec::new();
        for (r, fraction, root) in [
            (&cal[0], 0.1, &cal_results),
            (&cal[1], 0.2, &cal_results),
            (&dev, 0.15, &dev_results),
        ] {
            let id = r["id"].as_str().unwrap();
            let folder = root.join(id);
            fs::create_dir(&folder).unwrap();
            write(
                &folder.join("result.json"),
                &json!({"id":id,"source_group":r["source_group"],"split":r["frozen_split"],
                "text_sha256":"a".repeat(64),"model":"pangram-4","response":{"stage":"STAGE_SUCCESS","version":"synthetic",
                "fraction_ai":fraction,"fraction_ai_assisted":0.}}),
            );
            population.push(json!({"id":id,"group":r["source_group"],"split":r["frozen_split"],"purpose":"query",
                "metadata":{"text_sha256":"a".repeat(64)}}));
        }
        write(&data.join("population.json"), &json!({"rows":population}));
        fs::write(
            data.join("calibration.jsonl"),
            format!("{}\n{}\n", cal[0], cal[1]),
        )
        .unwrap();
        write(&fit.join("weights.json"), &json!({}));
        write(
            &fit.join("fit.json"),
            &json!({"weights_sha256":sha(&fit.join("weights.json")).unwrap(),"query_weighting":"family_origin_balanced"}),
        );
        write(
            &fit.join("development-predictions.json"),
            &json!([{"id":"dev","prediction":{"ai_confidence_percent":25.,"ai_share_percent":0.}}]),
        );
        let mut results = Vec::new();
        for origin in [0, 1] {
            dev["targets"]["origin"] = origin.into();
            dev["targets"]["ai_involvement"] = (origin != 0).into();
            fs::write(data.join("development.jsonl"), format!("{dev}\n")).unwrap();
            write(
                &data.join("manifest.json"),
                &json!({"population_file_sha256":sha(&data.join("population.json")).unwrap(),
                "splits":[{"split":"calibration","sha256":sha(&data.join("calibration.jsonl")).unwrap()},
                    {"split":"development","sha256":sha(&data.join("development.jsonl")).unwrap()}]}),
            );
            let output = temp.path().join(format!("report-{origin}.json"));
            compare(&[
                data.display().to_string(),
                fit.display().to_string(),
                "development".into(),
                dev_results.display().to_string(),
                cal_results.display().to_string(),
                output.display().to_string(),
                "--mapping".into(),
                "isotonic_v1".into(),
            ])
            .unwrap();
            results.push(read(&output).unwrap());
        }
        assert_eq!(
            results[0]["calibration"]["knots"],
            results[1]["calibration"]["knots"]
        );
        assert_eq!(
            results[0]["calibration"]["blocks"],
            results[1]["calibration"]["blocks"]
        );
        assert_eq!(
            results[0]["records"][0]["mapped_pangram_involvement"],
            results[1]["records"][0]["mapped_pangram_involvement"]
        );
        assert_ne!(
            results[0]["slop_ninja_paired"]["involvement_brier"],
            results[1]["slop_ninja_paired"]["involvement_brier"]
        );
    }
    #[test]
    fn descendant_multiplicity_does_not_change_balanced_metric() {
        let mut rows = vec![("family".into(), 0, 1.), ("family".into(), 1, 0.)];
        rows.extend((0..30).map(|_| ("family".into(), 2, 0.)));
        assert!((avg(&origin_balanced(&rows)).unwrap() - 1. / 3.).abs() < 1e-12);
        assert!(ci(&origin_balanced(&rows)).is_null());
    }
    #[test]
    fn missing_target_is_never_a_human_label() {
        assert!(target(&json!({"targets":{"origin":null,"ai_involvement":null}})).is_err());
        assert!(target(&json!({"targets":{"origin":2,"ai_involvement":false}})).is_err());
        assert_eq!(
            target(&json!({"targets":{"origin":2,"ai_involvement":true}})).unwrap(),
            2
        );
    }
    #[test]
    fn exact_source_hash_is_required_before_scoring_provider_output() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("synthetic")).unwrap();
        let v = json!({"id":"synthetic","source_group":"work","text_sha256":"old-text","response":{"stage":"STAGE_SUCCESS","fraction_ai":0.5,"fraction_ai_assisted":0.1}});
        fs::write(
            dir.path().join("synthetic/result.json"),
            serde_json::to_vec(&v).unwrap(),
        )
        .unwrap();
        let row = json!({"id":"synthetic","source_group":"work"});
        assert!(
            observation(
                dir.path(),
                &row,
                &BTreeMap::from([("synthetic".into(), "new-text".into())])
            )
            .is_err()
        );
        assert!(
            observation(
                dir.path(),
                &row,
                &BTreeMap::from([("synthetic".into(), "old-text".into())])
            )
            .is_ok()
        );
    }
}

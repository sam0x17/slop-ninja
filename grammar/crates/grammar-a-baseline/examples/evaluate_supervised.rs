//! Frozen richer-A evaluation; no fitting, selection, provider calls, or prose.
//! Usage: evaluate_supervised DATA FIT SPLIT NEW_REPORT.json
//! Query predictions may contain `encoding` from shared_a.py --export-encodings.
//! Independent reference encodings use slop_ninja_shared_a_encoding_v1. Pooling
//! is normalize(mean(raw reference encodings)), with equal weight per row. It is
//! a separate cosine-ranking diagnostic, never a classification-head score.
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::shared::{SharedInput, SharedModel};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};

const ROLES: [&str; 2] = ["human", "ai"];
const ORIGINS: [&str; 3] = ["human_only", "model_only", "mixed"];
type Samples = Vec<(String, f64)>;
type Gallery = BTreeMap<String, BTreeMap<String, Vec<f64>>>;

#[path = "predict_final_test/evaluate.rs"]
mod final_test;
#[allow(dead_code)]
#[path = "final_test_binding/mod.rs"]
mod final_test_binding;
#[allow(dead_code)]
#[path = "predict_final_test/binding.rs"]
mod prediction_binding;
#[allow(dead_code)]
#[path = "compare_supervised_pangram/test_binding.rs"]
mod test_binding;

fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn rows(path: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(path)?
        .lines()
        .map(|s| Ok(serde_json::from_str(s)?))
        .collect()
}
fn sha(path: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(path)?)))
}
fn string<'a>(row: &'a Value, key: &str) -> Result<&'a str> {
    row[key]
        .as_str()
        .with_context(|| format!("missing string {key}"))
}
fn number(value: &Value) -> Result<f64> {
    let n = value.as_f64().context("missing number")?;
    ensure!(n.is_finite(), "nonfinite number");
    Ok(n)
}
fn origin(row: &Value) -> Result<Option<usize>> {
    let target = &row["targets"]["origin"];
    if target.is_null() {
        return Ok(None);
    }
    let y = target.as_u64().context("origin target")? as usize;
    ensure!(y < 3, "origin outside roster");
    Ok(Some(y))
}
fn family_means(values: &Samples) -> Vec<f64> {
    let mut groups = BTreeMap::<&str, (f64, usize)>::new();
    for (g, v) in values {
        let entry = groups.entry(g).or_default();
        entry.0 += v;
        entry.1 += 1;
    }
    groups.values().map(|(sum, n)| sum / *n as f64).collect()
}
fn weighted(values: &Samples) -> Option<f64> {
    let means = family_means(values);
    (!means.is_empty()).then(|| means.iter().sum::<f64>() / means.len() as f64)
}
fn bootstrap(values: &Samples) -> Value {
    let means = family_means(values);
    if means.len() < 2 {
        return Value::Null;
    }
    let mut seed = 20261026u64;
    let mut samples = Vec::with_capacity(1000);
    for _ in 0..1000 {
        let mut total = 0.;
        for _ in 0..means.len() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            total += means[seed as usize % means.len()];
        }
        samples.push(total / means.len() as f64);
    }
    samples.sort_by(f64::total_cmp);
    json!({"lower":samples[24],"upper":samples[974],"replicates":1000,"seed":20261026,
           "method":"percentile_resample_source_family_means"})
}
pub(crate) fn summary(values: &Samples) -> Value {
    json!({"estimate":weighted(values),"documents":values.len(),
           "source_families":family_means(values).len(),"ci95":bootstrap(values)})
}
fn push(values: &mut Samples, group: &str, value: f64) {
    values.push((group.to_owned(), value));
}
fn rate(value: bool) -> f64 {
    if value { 1. } else { 0. }
}

#[derive(Clone, Debug)]
struct RoleTruth {
    present: Option<bool>,
    identity: Option<String>,
    known: bool,
}
fn role_truth(row: &Value, role: &str, profiles: &[String]) -> Result<RoleTruth> {
    if let Some(roles) = row.get("roles") {
        let label = roles.get(role).context("role label absent")?;
        let present = if label["present"].is_null() {
            None
        } else {
            Some(label["present"].as_bool().context("presence label")?)
        };
        let identity = if label["identity"].is_null() {
            None
        } else {
            Some(string(label, "identity")?.to_owned())
        };
        let known = label["known"].as_bool().context("known label")?;
        if let Some(name) = &identity {
            ensure!(
                known == profiles.contains(name),
                "known label disagrees with frozen profile roster"
            );
        }
        return Ok(RoleTruth {
            present,
            identity,
            known,
        });
    }
    // Compatibility: legacy null identity does not document a withheld identity.
    let identity = if row["targets"][role].is_null() {
        None
    } else {
        let index = row["targets"][role]
            .as_u64()
            .context("legacy identity index")? as usize;
        Some(
            profiles
                .get(index)
                .context("legacy identity outside roster")?
                .clone(),
        )
    };
    Ok(RoleTruth {
        present: origin(row)?.map(|y| y == 2 || y == if role == "human" { 0 } else { 1 }),
        known: identity.is_some(),
        identity,
    })
}
fn roster(contract: &Value, role: &str) -> Result<Vec<String>> {
    contract[format!("{role}_profiles")]
        .as_array()
        .context("profile roster")?
        .iter()
        .map(|v| Ok(v.as_str().context("profile name")?.to_owned()))
        .collect()
}
fn ranking(scores: Vec<(String, f64)>) -> Vec<(String, f64)> {
    // Stable sort keeps contract/gallery order at exact ties.
    let mut scores = scores;
    scores.sort_by(|a, b| b.1.total_cmp(&a.1));
    scores
}
fn classification_ranking(
    pred: &Value,
    role: &str,
    profiles: &[String],
) -> Result<Vec<(String, f64)>> {
    let scores = pred["attribution"][role]["conditional_named_scores"]
        .as_object()
        .context("conditional scores")?;
    ensure!(scores.len() == profiles.len(), "conditional roster differs");
    let scores = profiles
        .iter()
        .map(|name| {
            let score = number(scores.get(name).context("missing named score")?)?;
            ensure!(
                (0.0..=1.0).contains(&score),
                "invalid conditional probability"
            );
            Ok((name.clone(), score))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        scores.is_empty() || (scores.iter().map(|(_, v)| v).sum::<f64>() - 1.).abs() < 1e-5,
        "conditional probabilities do not sum to one"
    );
    Ok(ranking(scores))
}
fn probabilities(pred: &Value) -> Result<[f64; 3]> {
    let p = ORIGINS.map(|key| number(&pred["origin"][key]));
    let [a, b, c] = p;
    let p = [a?, b?, c?];
    ensure!(
        p.iter().all(|v| (0.0..=1.0).contains(v)) && (p.iter().sum::<f64>() - 1.).abs() < 1e-6,
        "origin probabilities"
    );
    let confidence = number(&pred["ai_confidence_percent"])? / 100.;
    ensure!(
        (confidence - p[1] - p[2]).abs() < 1e-6,
        "incoherent involvement"
    );
    Ok(p)
}

pub(crate) fn origin_share_metrics(
    data: &[Value],
    predictions: &BTreeMap<String, Value>,
    prior: Option<[f64; 3]>,
) -> Result<Value> {
    let mut brier = Samples::new();
    let mut three = Samples::new();
    let mut accuracy = Samples::new();
    let mut fp = BTreeMap::<&str, Samples>::from([
        ("0.5", vec![]),
        ("0.9", vec![]),
        ("0.95", vec![]),
        ("0.99", vec![]),
    ]);
    let mut share_abs = Samples::new();
    let mut share_sq = Samples::new();
    let mut share_bias = Samples::new();
    let mut share_bins = BTreeMap::<&str, (Samples, Samples, Samples)>::new();
    let mut bins = vec![Vec::<(String, f64, f64)>::new(); 10];
    let mut counts = [0usize; 3];
    let mut confusion = [[0usize; 3]; 3];
    for row in data {
        let id = string(row, "id")?;
        let group = string(row, "source_group")?;
        let pred = predictions.get(id).context("missing prediction")?;
        let p = if let Some(p) = prior {
            p
        } else {
            probabilities(pred)?
        };
        let score = p[1] + p[2];
        if let Some(y) = origin(row)? {
            let label = rate(y != 0);
            push(&mut brier, group, (score - label).powi(2));
            push(
                &mut three,
                group,
                (0..3).map(|i| (p[i] - rate(i == y)).powi(2)).sum(),
            );
            let winner = (0..3)
                .max_by(|a, b| p[*a].total_cmp(&p[*b]).then_with(|| b.cmp(a)))
                .unwrap();
            push(&mut accuracy, group, rate(winner == y));
            counts[y] += 1;
            confusion[y][winner] += 1;
            bins[((score * 10.).floor() as usize).min(9)].push((group.into(), score, label));
            if y == 0 {
                for (threshold, values) in &mut fp {
                    push(values, group, rate(score >= threshold.parse::<f64>()?));
                }
            }
        }
        if !row["targets"]["share"].is_null() {
            let truth = number(&row["targets"]["share"])?;
            ensure!((0.0..=1.0).contains(&truth), "share target outside [0,1]");
            let estimate = if prior.is_some() {
                p[1] + p[2] * 0.5
            } else {
                number(&pred["ai_share_percent"])? / 100.
            };
            ensure!(
                estimate >= p[1] - 1e-6 && estimate <= score + 1e-6,
                "incoherent share"
            );
            let error = (estimate - truth) * 100.;
            push(&mut share_abs, group, error.abs());
            push(&mut share_sq, group, error.powi(2));
            push(&mut share_bias, group, error);
            let bin = if truth == 0. {
                "0"
            } else if truth == 1. {
                "100"
            } else if truth <= 0.25 {
                "(0,25]"
            } else if truth <= 0.5 {
                "(25,50]"
            } else if truth <= 0.75 {
                "(50,75]"
            } else {
                "(75,100)"
            };
            let (abs, bias, squared) = share_bins.entry(bin).or_default();
            push(abs, group, error.abs());
            push(bias, group, error);
            push(squared, group, error.powi(2));
        }
    }
    let mut groups = BTreeMap::<&str, usize>::new();
    for (g, _) in &brier {
        *groups.entry(g).or_default() += 1;
    }
    let mut ece = 0.;
    let mut calibration = Vec::new();
    for (i, bin) in bins.iter().enumerate() {
        let (mut mass, mut predicted, mut observed) = (0., 0., 0.);
        for (g, p, y) in bin {
            let w = 1. / groups[g.as_str()] as f64 / groups.len() as f64;
            mass += w;
            predicted += w * p;
            observed += w * y;
        }
        if mass > 0. {
            ece += (predicted - observed).abs();
            calibration.push(json!({"bin":i,"documents":bin.len(),"weight":mass,
                                   "mean_score":predicted/mass,"observed_ai":observed/mass}));
        }
    }
    Ok(
        json!({"documents":data.len(),"origin_labelled_documents":brier.len(),"source_families":groups.len(),
        "origin_counts":counts,"origin_order":ORIGINS,"confusion_counts_truth_by_predicted":confusion,
        "involvement_brier":weighted(&brier),"involvement_brier_95ci":bootstrap(&brier),
        "origin_brier_sum3":summary(&three),"origin_accuracy":summary(&accuracy),
        "human_fpr_at_0_5":weighted(&fp["0.5"]),"human_false_positive_rates":fp.iter().map(|(k,v)|(*k,summary(v))).collect::<BTreeMap<_,_>>(),
        "ece10":if brier.is_empty() { None } else { Some(ece) },"calibration_bins":calibration,
        "share":{"labelled_documents":share_abs.len(),"mae_percentage_points":summary(&share_abs),
                 "mse_percentage_points_squared":summary(&share_sq),"bias_percentage_points":summary(&share_bias),
                 "by_realized_share":share_bins.iter().map(|(k,(a,b,s))|(*k,json!({"mae_percentage_points":summary(a),
                    "bias_percentage_points":summary(b),"mse_percentage_points_squared":summary(s)}))).collect::<BTreeMap<_,_>>()},
        "false_positive_guarantee":false}),
    )
}

pub(crate) fn balanced_origin_metrics(
    data: &[Value],
    predictions: &BTreeMap<String, Value>,
    prior: Option<[f64; 3]>,
) -> Result<Value> {
    let mut cells = BTreeMap::<(String, usize), Vec<(f64, f64, [f64; 3])>>::new();
    for row in data {
        if let Some(y) = origin(row)? {
            let p = if let Some(p) = prior {
                p
            } else {
                probabilities(
                    predictions
                        .get(string(row, "id")?)
                        .context("missing prediction")?,
                )?
            };
            cells
                .entry((string(row, "source_group")?.into(), y))
                .or_default()
                .push((p[1] + p[2], rate(y != 0), p));
        }
    }
    let families = cells.keys().map(|(g, _)| g).collect::<BTreeSet<_>>();
    let mut strata = BTreeMap::<&str, usize>::new();
    for (g, _) in cells.keys() {
        *strata.entry(g).or_default() += 1;
    }
    let mut brier = Samples::new();
    let mut three = Samples::new();
    let mut accuracy = Samples::new();
    let mut bins = vec![(0.0, 0.0, 0.0, 0usize); 10];
    for ((group, y), values) in &cells {
        let n = values.len() as f64;
        push(
            &mut brier,
            group,
            values.iter().map(|(p, t, _)| (p - t).powi(2)).sum::<f64>() / n,
        );
        push(
            &mut three,
            group,
            values
                .iter()
                .map(|(_, _, p)| (0..3).map(|k| (p[k] - rate(*y == k)).powi(2)).sum::<f64>())
                .sum::<f64>()
                / n,
        );
        push(
            &mut accuracy,
            group,
            values
                .iter()
                .map(|(_, _, p)| {
                    rate(
                        (0..3)
                            .max_by(|a, b| p[*a].total_cmp(&p[*b]).then_with(|| b.cmp(a)))
                            .unwrap()
                            == *y,
                    )
                })
                .sum::<f64>()
                / n,
        );
        let weight = 1.0 / (families.len() as f64 * strata[group.as_str()] as f64 * n);
        for (p, t, _) in values {
            let bin = &mut bins[((p * 10.).floor() as usize).min(9)];
            bin.0 += weight;
            bin.1 += weight * p;
            bin.2 += weight * t;
            bin.3 += 1;
        }
    }
    Ok(
        json!({"weighting":"equal_source_family_then_equal_observed_origin_within_family",
        "prevalence":"explicit_research_population_not_deployment",
        "labelled_documents":cells.values().map(Vec::len).sum::<usize>(),"source_families":families.len(),"family_origin_cells":cells.len(),
        "involvement_brier":weighted(&brier),"involvement_brier_95ci":bootstrap(&brier),
        "origin_brier_sum3":weighted(&three),"origin_accuracy":weighted(&accuracy),
        "ece10":(!cells.is_empty()).then(||bins.iter().map(|b|(b.1-b.2).abs()).sum::<f64>()),
        "calibration_bins":bins.iter().enumerate().filter(|(_,b)|b.0>0.).map(|(i,b)|json!({"bin":i,"weight":b.0,"mean_score":b.1/b.0,"observed_ai":b.2/b.0,"documents":b.3})).collect::<Vec<_>>() }),
    )
}

pub(crate) fn attribution_metrics(
    data: &[Value],
    predictions: &BTreeMap<String, Value>,
    role: &str,
    profiles: &[String],
    threshold: f64,
) -> Result<Value> {
    let (mut present, mut absent, mut unspecified, mut unidentified) = (0, 0, 0, 0);
    let mut top1 = Samples::new();
    let mut mrr = Samples::new();
    let mut top3 = Samples::new();
    let mut ce = Samples::new();
    let mut correct_named = Samples::new();
    let mut known_fallback = Samples::new();
    let mut wrong_named = Samples::new();
    let mut unknown_rejected = Samples::new();
    let mut accepted_correct = Samples::new();
    let mut per_identity = BTreeMap::<String, Samples>::new();
    for row in data {
        let truth = role_truth(row, role, profiles)?;
        match truth.present {
            Some(false) => {
                absent += 1;
                continue;
            }
            None => {
                unspecified += 1;
                continue;
            }
            Some(true) => {
                present += 1;
            }
        }
        let Some(identity) = truth.identity else {
            unidentified += 1;
            continue;
        };
        let group = string(row, "source_group")?;
        let pred = predictions.get(string(row, "id")?).context("prediction")?;
        let ranked = classification_ranking(pred, role, profiles)?;
        let actual = string(&pred["attribution"][role], "profile")?;
        let fallback = format!("other_{role}");
        let expected = ranked
            .first()
            .filter(|(_, score)| *score >= threshold)
            .map(|(name, _)| name.as_str())
            .unwrap_or(&fallback);
        ensure!(
            actual == expected,
            "named/fallback differs from frozen threshold for {role}"
        );
        ensure!(
            pred["attribution"][role]["fallback_asserts_presence"] == false,
            "fallback must not assert presence"
        );
        // Deliberately never read involvement_probability when scoring contributors.
        if truth.known {
            let rank = ranked
                .iter()
                .position(|(name, _)| name == &identity)
                .context("known identity absent from head")?;
            push(&mut top1, group, rate(rank == 0));
            push(&mut mrr, group, 1. / (rank + 1) as f64);
            push(&mut top3, group, rate(rank < 3));
            push(&mut ce, group, -ranked[rank].1.max(1e-15).ln());
            push(&mut correct_named, group, rate(actual == identity));
            push(&mut known_fallback, group, rate(actual == fallback));
            push(
                &mut wrong_named,
                group,
                rate(actual != fallback && actual != identity),
            );
            push(
                per_identity.entry(identity.clone()).or_default(),
                group,
                rate(rank == 0),
            );
            if actual != fallback {
                push(&mut accepted_correct, group, rate(actual == identity));
            }
        } else {
            push(&mut unknown_rejected, group, rate(actual == fallback));
            if actual != fallback {
                push(&mut accepted_correct, group, 0.);
            }
        }
    }
    let balanced = weighted(&correct_named)
        .zip(weighted(&unknown_rejected))
        .map(|(a, b)| (a + b) / 2.);
    Ok(
        json!({"documented_present":present,"documented_absent_not_scored":absent,
        "presence_unspecified_not_scored":unspecified,"present_identity_unspecified_not_scored":unidentified,
        "threshold":threshold,"presence_cutoff":null,"conditional_accuracy":summary(&top1),
        "conditional_mrr":summary(&mrr),"conditional_top3":summary(&top3),"conditional_ce":summary(&ce),
        "conditional_top1_by_identity":per_identity.iter().map(|(k,v)|(k,summary(v))).collect::<BTreeMap<_,_>>(),
        "open_set":{"known_correct_named":summary(&correct_named),"known_fallback":summary(&known_fallback),
            "known_wrong_named":summary(&wrong_named),"withheld_rejection":summary(&unknown_rejected),
            "accepted_named_precision":summary(&accepted_correct),"balanced_correct_named_vs_withheld_rejection":balanced},
        "fallback_asserts_role_presence_or_gallery_absence":false}),
    )
}

fn vector(value: &Value) -> Result<Vec<f64>> {
    let values = value
        .as_array()
        .context("encoding array")?
        .iter()
        .map(number)
        .collect::<Result<Vec<_>>>()?;
    ensure!(!values.is_empty(), "empty encoding");
    Ok(values)
}
fn normalized(mut values: Vec<f64>) -> Option<Vec<f64>> {
    let norm = values.iter().map(|v| v * v).sum::<f64>().sqrt();
    if norm <= 1e-12 || !norm.is_finite() {
        return None;
    }
    for v in &mut values {
        *v /= norm;
    }
    Some(values)
}
pub(crate) fn pool(samples: &[Vec<f64>]) -> Result<Option<Vec<f64>>> {
    ensure!(!samples.is_empty(), "empty reference identity");
    let mut mean = vec![0.; samples[0].len()];
    for sample in samples {
        ensure!(sample.len() == mean.len(), "encoding dimensions differ");
        for (a, b) in mean.iter_mut().zip(sample) {
            *a += b / samples.len() as f64;
        }
    }
    Ok(normalized(mean))
}
fn cosine_ranking(
    query: &[f64],
    gallery: &BTreeMap<String, Vec<f64>>,
) -> Result<Vec<(String, f64)>> {
    let Some(query) = normalized(query.to_vec()) else {
        return Ok(vec![]);
    };
    let scores = gallery
        .iter()
        .map(|(name, reference)| {
            ensure!(
                reference.len() == query.len(),
                "query/reference encoding dimensions differ"
            );
            Ok((
                name.clone(),
                query
                    .iter()
                    .zip(reference)
                    .map(|(a, b)| a * b)
                    .sum::<f64>()
                    .clamp(-1., 1.),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ranking(scores))
}
fn bundle_references(bundle: &Value, split: &str, contract: &Value) -> Result<(Vec<Value>, Value)> {
    ensure!(
        matches!(split, "development" | "calibration"),
        "partition galleries exclude Test"
    );
    let profiles = roster(contract, "human")?;
    let mut known = Vec::new();
    let mut additional = Vec::new();
    let mut selected = Vec::new();
    let mut encodings = Vec::new();
    let mut ids = BTreeSet::new();
    let mut groups = BTreeMap::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut identities = BTreeMap::<String, usize>::new();
    for value in bundle["rows"]
        .as_array()
        .context("partition reference rows")?
    {
        let part = string(value, "frozen_split")?;
        let id = string(value, "id")?;
        let group = string(value, "source_group")?;
        ensure!(
            matches!(part, "train" | "development" | "calibration")
                && value["purpose"] == "reference"
                && ids.insert(id.to_owned()),
            "Test/query/duplicate gallery reference"
        );
        ensure!(
            value["input"]["contract_sha256"] == bundle["contract_sha256"]
                && value["targets"]
                    .as_object()
                    .is_some_and(|v| v.values().all(Value::is_null)),
            "reference contract/loss targets"
        );
        if let Some(old) = groups.insert(group.to_owned(), part.to_owned()) {
            ensure!(old == part, "reference family crosses galleries");
        }
        let human = &value["roles"]["human"];
        let identity = string(human, "identity")?;
        let trained = profiles.contains(&identity.to_owned());
        ensure!(
            human["present"] == true
                && value["roles"]["ai"]["present"] == false
                && human["known"] == trained
                && trained == (part == "train"),
            "gallery/head enrollment mismatch"
        );
        if part != "train" {
            ensure!(
                contract["split_assignments"].get(group).is_none(),
                "additional reference overlaps frozen population"
            );
        }
        *counts.entry(part.to_owned()).or_default() += 1;
        *identities.entry(format!("{part}/{identity}")).or_default() += 1;
        let encoding = vector(&value["encoding"])?;
        let mut row = value.clone();
        row.as_object_mut()
            .context("reference object")?
            .remove("encoding");
        if part == "train" {
            known.push(row.clone());
        } else {
            additional.push(row.clone());
        }
        if part == "train" || part == split {
            selected.push(row);
            encodings.push(json!({"id":id,"encoding":encoding}));
        }
    }
    ensure!(
        counts.get("train") == Some(&12)
            && counts.get("development") == Some(&4)
            && counts.get("calibration") == Some(&4)
            && identities.len() == 10
            && identities.values().all(|n| *n == 2),
        "partition gallery coverage"
    );
    let digest_rows = |rows: &[Value]| -> Result<String> {
        let mut b = Vec::new();
        for r in rows {
            serde_json::to_writer(&mut b, r)?;
            b.push(b'\n');
        }
        Ok(hex::encode(Sha256::digest(&b)))
    };
    ensure!(
        bundle["known_reference_sha256"] == digest_rows(&known)?
            && bundle["partition_reference_sha256"] == digest_rows(&additional)?,
        "reference numeric row digest changed"
    );
    Ok((
        selected,
        json!({"schema":"slop_ninja_shared_a_encoding_v1","contract_sha256":bundle["contract_sha256"],"weights_sha256":bundle["weights_sha256"],"reference_sha256":bundle["known_reference_sha256"],"rows":encodings}),
    ))
}

fn reference_gallery(
    data: &Path,
    fit: &Path,
    eval: &[Value],
    contract: &Value,
    manifest: &Value,
    partition_bundle: Option<&Path>,
    split: &str,
) -> Result<Option<(Gallery, Value)>> {
    if partition_bundle.is_none() && !fit.join("reference-encodings.json").exists() {
        return Ok(None);
    }
    let (refs, exported) = if let Some(path) = partition_bundle {
        let bundle = read(path)?;
        ensure!(
            bundle["schema"] == "slop_ninja_partition_reference_encodings_v1"
                && bundle["contract_sha256"] == manifest["contract_sha256"]
                && bundle["frozen_manifest_sha256"] == sha(&data.join("manifest.json"))?
                && bundle["fit_performed"] == false
                && bundle["normalization_fitted"] == false
                && bundle["profiles_changed"] == false
                && bundle["final_test_opened"] == false,
            "partition reference bundle binding"
        );
        let binding = manifest["splits"]
            .as_array()
            .context("split bindings")?
            .iter()
            .find(|b| b["split"] == "reference")
            .context("known reference binding")?;
        ensure!(
            binding["sha256"] == bundle["known_reference_sha256"],
            "known reference binding differs"
        );
        bundle_references(&bundle, split, contract)?
    } else {
        (
            rows(&data.join("reference.jsonl"))?,
            read(&fit.join("reference-encodings.json"))?,
        )
    };
    ensure!(
        exported["schema"] == "slop_ninja_shared_a_encoding_v1"
            && exported["contract_sha256"] == manifest["contract_sha256"],
        "encoding contract mismatch"
    );
    ensure!(
        exported["weights_sha256"].as_str() == Some(sha(&fit.join("weights.json"))?.as_str()),
        "encoding/model mismatch"
    );
    ensure!(
        exported["reference_sha256"].as_str() == Some(sha(&data.join("reference.jsonl"))?.as_str()),
        "reference inputs changed"
    );
    let mut query_groups = BTreeSet::<String>::new();
    let mut query_ids = BTreeSet::<String>::new();
    // Never open Test while evaluating Development or Calibration.
    for row in rows(&data.join("train.jsonl"))?.iter().chain(eval) {
        query_groups.insert(string(row, "source_group")?.to_owned());
        query_ids.insert(string(row, "id")?.to_owned());
    }
    let mut encodings = BTreeMap::new();
    for row in exported["rows"].as_array().context("reference encodings")? {
        ensure!(
            encodings
                .insert(string(row, "id")?.to_owned(), vector(&row["encoding"])?)
                .is_none(),
            "duplicate reference encoding"
        );
    }
    ensure!(
        encodings.len() == refs.len(),
        "reference encoding roster mismatch"
    );
    let mut samples = BTreeMap::<String, BTreeMap<String, Vec<Vec<f64>>>>::new();
    let mut used_ids = BTreeSet::new();
    for row in &refs {
        let id = string(row, "id")?;
        ensure!(used_ids.insert(id), "duplicate reference row");
        ensure!(row["purpose"] == "reference", "reference purpose required");
        ensure!(
            !query_groups.contains(string(row, "source_group")?) && !query_ids.contains(id),
            "reference work overlaps queries"
        );
        ensure!(
            row["input"]["contract_sha256"] == manifest["contract_sha256"],
            "reference contract mismatch"
        );
        let encoding = encodings.get(id).context("reference encoding absent")?;
        for role in ROLES {
            let truth = role_truth(row, role, &roster(contract, role)?)?;
            if truth.present != Some(true) {
                continue;
            }
            if let Some(identity) = truth.identity {
                samples
                    .entry(role.into())
                    .or_default()
                    .entry(identity)
                    .or_default()
                    .push(encoding.clone());
            }
        }
    }
    let mut gallery = Gallery::new();
    let mut counts = BTreeMap::new();
    let mut zero_norm = Vec::new();
    for (role, identities) in samples {
        let mut role_gallery = BTreeMap::new();
        for (identity, vectors) in identities {
            counts.insert(format!("{role}/{identity}"), vectors.len());
            if let Some(pooled) = pool(&vectors)? {
                role_gallery.insert(identity, pooled);
            } else {
                zero_norm.push(format!("{role}/{identity}"));
            }
        }
        gallery.insert(role, role_gallery);
    }
    Ok(Some((
        gallery,
        json!({"schema":"slop_ninja_reference_normalized_mean_cosine_v1",
        "pooling":"l2_normalize_arithmetic_mean_of_raw_encodings_equal_reference_row_weights",
        "query":"l2_normalize_raw_encoding_then_cosine_rank","ties":"lexicographic_identity",
        "reference_rows":refs.len(),"per_identity_reference_rows":counts,"zero_norm_profiles_excluded":zero_norm,
        "evaluation_partition":split,"partition_bundle_sha256":partition_bundle.map(sha).transpose()?,
        "encoding_path":if partition_bundle.is_some(){"Rust SharedModel::encode for both references and queries"}else{"fit-exported encodings"},
        "reference_used_for_training_or_normalization":false,"classification_head_scores":false,
        "identity_authentication":false,"open_set_threshold_calibrated":false}),
    )))
}
pub(crate) fn retrieval_metrics(
    data: &[Value],
    predictions: &BTreeMap<String, Value>,
    gallery: &Gallery,
    contract: &Value,
) -> Result<Value> {
    let mut results = BTreeMap::new();
    for role in ROLES {
        let empty = BTreeMap::new();
        let profiles = gallery.get(role).unwrap_or(&empty);
        let mut top1 = Samples::new();
        let mut mrr = Samples::new();
        let mut top3 = Samples::new();
        let mut known_top1 = Samples::new();
        let mut withheld_top1 = Samples::new();
        let (mut missing_gallery, mut missing_encoding, mut zero_norm) = (0, 0, 0);
        let mut diagnostics = Vec::new();
        for row in data {
            let truth = role_truth(row, role, &roster(contract, role)?)?;
            if truth.present != Some(true) {
                continue;
            }
            let Some(identity) = truth.identity else {
                continue;
            };
            if !profiles.contains_key(&identity) {
                missing_gallery += 1;
                continue;
            }
            let id = string(row, "id")?;
            let group = string(row, "source_group")?;
            let pred = predictions.get(id).context("query prediction")?;
            let Some(encoding) = pred.get("encoding") else {
                missing_encoding += 1;
                continue;
            };
            let ranked = cosine_ranking(&vector(encoding)?, profiles)?;
            if ranked.is_empty() {
                zero_norm += 1;
                continue;
            }
            let rank = ranked
                .iter()
                .position(|(name, _)| name == &identity)
                .context("retrieval identity")?;
            push(&mut top1, group, rate(rank == 0));
            push(&mut mrr, group, 1. / (rank + 1) as f64);
            push(&mut top3, group, rate(rank < 3));
            push(
                if truth.known {
                    &mut known_top1
                } else {
                    &mut withheld_top1
                },
                group,
                rate(rank == 0),
            );
            diagnostics.push(json!({"id":id,"target_rank":rank+1,"top1":ranked[0].0,
                "top1_cosine":ranked[0].1,"target_cosine":ranked[rank].1,"head_known_identity":truth.known}));
        }
        results.insert(
            role,
            json!({"gallery_identities":profiles.len(),"top1":summary(&top1),
            "mrr":summary(&mrr),"top3":summary(&top3),"head_known_top1":summary(&known_top1),
            "head_withheld_top1":summary(&withheld_top1),"target_not_in_gallery":missing_gallery,
            "query_encoding_missing":missing_encoding,"zero_norm_queries":zero_norm,
            "query_diagnostics":diagnostics,"diagnostic_only":true}),
        );
    }
    Ok(json!(results))
}
fn query_rows(path: &Path) -> Result<Vec<Value>> {
    let rows = rows(path)?;
    let mut ids = BTreeSet::new();
    for row in &rows {
        ensure!(
            row.get("purpose").is_none() || row["purpose"] == "query",
            "references in query split"
        );
        ensure!(
            ids.insert(string(row, "id")?.to_owned()),
            "duplicate query id"
        );
    }
    Ok(rows)
}
fn prior(train: &[Value]) -> Result<Option<[f64; 3]>> {
    let mut classes = [Samples::new(), Samples::new(), Samples::new()];
    for row in train {
        if let Some(y) = origin(row)? {
            for (i, values) in classes.iter_mut().enumerate() {
                push(values, string(row, "source_group")?, rate(y == i));
            }
        }
    }
    Ok(weighted(&classes[0]).map(|a| {
        [
            a,
            weighted(&classes[1]).unwrap(),
            weighted(&classes[2]).unwrap(),
        ]
    }))
}
fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|a| a == "--frozen-test") {
        return final_test::run(&args[1..]);
    }
    ensure!(
        args.len() == 4 || args.len() == 5,
        "evaluate_supervised DATA FIT SPLIT NEW_REPORT.json [PARTITION_REFERENCE_BUNDLE.json]"
    );
    ensure!(
        matches!(args[2].as_str(), "development" | "calibration" | "test"),
        "evaluation split"
    );
    let (data, fit) = (Path::new(&args[0]), Path::new(&args[1]));
    let partition_bundle = args.get(4).map(Path::new);
    ensure!(
        partition_bundle.is_none() || matches!(args[2].as_str(), "development" | "calibration"),
        "partition reference route excludes Test"
    );
    let manifest = read(&data.join("manifest.json"))?;
    let contract = read(&data.join("contract.json"))?;
    ensure!(
        manifest["contract_file_sha256"].as_str()
            == Some(sha(&data.join("contract.json"))?.as_str()),
        "changed contract"
    );
    let weights = read(&fit.join("weights.json"))?;
    ensure!(
        weights["contract_sha256"] == manifest["contract_sha256"],
        "model contract mismatch"
    );
    let fit_report = read(&fit.join("fit.json"))?;
    ensure!(
        fit_report["weights_sha256"].as_str() == Some(sha(&fit.join("weights.json"))?.as_str()),
        "changed fitted weights"
    );
    ensure!(fit_report["final_test_opened"] == false, "fit used Test");
    let eval_path = data.join(format!("{}.jsonl", args[2]));
    let binding = manifest["splits"]
        .as_array()
        .context("split bindings")?
        .iter()
        .find(|v| v["split"] == args[2])
        .context("split absent from manifest")?;
    ensure!(
        binding["sha256"].as_str() == Some(sha(&eval_path)?.as_str()),
        "changed evaluation inputs"
    );
    let eval = query_rows(&eval_path)?;
    let pred_path = fit.join(format!("{}-predictions.json", args[2]));
    let mut predictions = BTreeMap::new();
    for row in read(&pred_path)?.as_array().context("predictions")? {
        ensure!(
            predictions
                .insert(string(row, "id")?.to_owned(), row["prediction"].clone())
                .is_none(),
            "duplicate prediction"
        );
    }
    ensure!(
        predictions.len() == eval.len()
            && eval
                .iter()
                .all(|r| predictions.contains_key(r["id"].as_str().unwrap_or(""))),
        "prediction roster differs"
    );
    for row in &eval {
        ensure!(
            row["input"]["contract_sha256"] == manifest["contract_sha256"],
            "query contract differs"
        );
    }
    let train_binding = manifest["splits"]
        .as_array()
        .context("split bindings")?
        .iter()
        .find(|v| v["split"] == "train")
        .context("Train binding missing")?;
    ensure!(
        train_binding["sha256"].as_str() == Some(sha(&data.join("train.jsonl"))?.as_str()),
        "changed Train inputs"
    );
    let train = query_rows(&data.join("train.jsonl"))?;
    let prior = prior(&train)?;
    let mut attribution = BTreeMap::new();
    for role in ROLES {
        let profiles = roster(&contract, role)?;
        ensure!(
            weights["profiles"][role] == json!(profiles),
            "model profile roster differs"
        );
        let threshold = number(&weights["thresholds"][role])?;
        ensure!((0.0..=1.0).contains(&threshold), "threshold bounds");
        let mut metrics = attribution_metrics(&eval, &predictions, role, &profiles, threshold)?;
        metrics["training_status"] = fit_report["training_status"]
            .get(role)
            .cloned()
            .unwrap_or_else(|| {
                json!(if weights["control"] == "origin_only"
                    || weights["supervision"][role].as_u64().unwrap_or(0) == 0
                {
                    "untrained"
                } else {
                    "legacy_status_unspecified"
                })
            });
        metrics["calibration"] = fit_report["conditional_named_calibration"][role].clone();
        attribution.insert(role, metrics);
    }
    if partition_bundle.is_some() {
        // Retrieval uses the same frozen Rust encoder for both sides. Existing
        // classification/share predictions and fitted thresholds remain untouched.
        let encoder = SharedModel::from_json(&fs::read(fit.join("weights.json"))?)?;
        for row in &eval {
            let input: SharedInput = serde_json::from_value(row["input"].clone())?;
            predictions
                .get_mut(string(row, "id")?)
                .context("prediction")?["encoding"] = json!(encoder.encode(&input)?);
        }
    }
    let reference_matching = if let Some((gallery, pool_contract)) = reference_gallery(
        data,
        fit,
        &eval,
        &contract,
        &manifest,
        partition_bundle,
        &args[2],
    )? {
        json!({"contract":pool_contract,"metrics":retrieval_metrics(&eval,&predictions,&gallery,&contract)?})
    } else {
        json!({"available":false,"reason":"independent_reference_encodings_not_exported"})
    };
    let result = json!({"schema":"slop_ninja_shared_a_supervised_report_v1","split":args[2],
        "clean_holdout":manifest.get("clean_holdout").and_then(Value::as_bool).unwrap_or(false),
        "weighting":"equal_source_family_within_each_metric_eligible_rows",
        "uncertainty":"source_family_sampling_only_conditional_on_fixed_model_grammar_and_seed;not_clean_holdout_or_repeated_seed_uncertainty",
        "model":origin_share_metrics(&eval,&predictions,None)?,"attribution":attribution,
        "family_origin_balanced":balanced_origin_metrics(&eval,&predictions,None)?,
        "uniform_origin_prior_balanced":balanced_origin_metrics(&eval,&predictions,Some([1./3.;3]))?,
        "training_status":fit_report["training_status"],
        "reference_matching":reference_matching,"train_family_prior":prior,
        "prior_baseline":prior.map(|p|origin_share_metrics(&eval,&predictions,Some(p))).transpose()?,
        "fit":fit_report,"population":manifest,"test_used_for_selection":false,
        "prediction_file_sha256":sha(&pred_path)?,"evaluation_source_sha256":hex::encode(Sha256::digest(include_bytes!("evaluate_supervised.rs"))),
        "limitations":["consult fit training_status before interpreting untrained outputs",
            "CAL threshold objective and CAL evaluation share data",
            "withheld-head identity may have an independent retrieval reference; these galleries are distinct",
            "cosine ranking is not calibrated identity confidence or authentication"]});
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[3])?;
    out.write_all(&serde_json::to_vec_pretty(&result)?)?;
    out.write_all(b"\n")?;
    println!(
        "{}",
        json!({"report":args[3],"model_brier":result["model"]["involvement_brier"]})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn partition_bundle_fixture() -> (Value, Value) {
        let profiles = (0..6).map(|i| format!("train-{i}")).collect::<Vec<_>>();
        let mut encoded = Vec::new();
        let mut known = Vec::new();
        let mut additional = Vec::new();
        for (part, n) in [("train", 6), ("development", 2), ("calibration", 2)] {
            for author in 0..n {
                for i in 0..2 {
                    let row = json!({"id":format!("{part}-{author}-{i}"),"source_group":format!("work-{part}-{author}"),"purpose":"reference","frozen_split":part,"input":{"contract_sha256":"frozen","x":[1.,0.]},"targets":{"human":null,"ai":null,"origin":null,"share":null,"ai_involvement":null},"roles":{"human":{"identity":format!("{part}-{author}"),"present":true,"known":part=="train"},"ai":{"identity":null,"present":false,"known":false}}});
                    if part == "train" {
                        known.push(row.clone());
                    } else {
                        additional.push(row.clone());
                    }
                    let mut row = row;
                    row["encoding"] = json!([1., 0.]);
                    encoded.push(row);
                }
            }
        }
        let digest = |rows: &[Value]| {
            let mut b = Vec::new();
            for r in rows {
                serde_json::to_writer(&mut b, r).unwrap();
                b.push(b'\n');
            }
            hex::encode(Sha256::digest(b))
        };
        (
            json!({"contract_sha256":"frozen","rows":encoded,"known_reference_sha256":digest(&known),"partition_reference_sha256":digest(&additional)}),
            json!({"human_profiles":profiles,"split_assignments":{}}),
        )
    }
    #[test]
    fn partition_gallery_has_known_plus_only_its_own_withheld_references() {
        let (bundle, contract) = partition_bundle_fixture();
        for split in ["development", "calibration"] {
            let (refs, encodings) = bundle_references(&bundle, split, &contract).unwrap();
            assert_eq!(refs.len(), 16);
            assert_eq!(encodings["rows"].as_array().unwrap().len(), 16);
            assert_eq!(
                refs.iter().filter(|r| r["frozen_split"] == "train").count(),
                12
            );
            assert_eq!(
                refs.iter().filter(|r| r["frozen_split"] == split).count(),
                4
            );
            assert!(
                refs.iter()
                    .filter(|r| r["frozen_split"] == split)
                    .all(|r| r["roles"]["human"]["known"] == false)
            );
        }
    }
    #[test]
    fn partition_gallery_rejects_test_profile_expansion_and_work_leakage() {
        let (bundle, contract) = partition_bundle_fixture();
        assert!(bundle_references(&bundle, "test", &contract).is_err());
        let mut bad = bundle.clone();
        bad["rows"][12]["roles"]["human"]["known"] = json!(true);
        assert!(bundle_references(&bad, "development", &contract).is_err());
        let mut bad_contract = contract.clone();
        bad_contract["split_assignments"]["work-development-0"] = json!("train");
        assert!(bundle_references(&bundle, "development", &bad_contract).is_err());
        let mut bad = bundle;
        bad["rows"].as_array_mut().unwrap().pop();
        assert!(bundle_references(&bad, "calibration", &contract).is_err());
    }
    fn row(id: &str, present: Option<bool>, identity: Option<&str>, known: bool) -> Value {
        json!({"id":id,"source_group":id,"targets":{"origin":null,"share":null,"human":null,"ai":null},
            "roles":{"human":{"present":present,"identity":identity,"known":known},
                     "ai":{"present":false,"identity":null,"known":false}}})
    }
    fn prediction(profile: &str, a: f64) -> Value {
        json!({"origin":{"human_only":1.,"model_only":0.,"mixed":0.},"ai_confidence_percent":0.,"ai_share_percent":0.,
            "attribution":{"human":{"profile":profile,"involvement_probability":0.,
                "conditional_named_scores":{"a":a,"b":1.-a},"fallback_asserts_presence":false}}})
    }
    #[test]
    fn absent_roles_excluded_present_scored_even_at_zero_involvement() {
        let rows = vec![
            row("known", Some(true), Some("a"), true),
            row("absent", Some(false), Some("a"), true),
            row("withheld", Some(true), Some("c"), false),
            row("masked", None, None, false),
        ];
        let predictions = BTreeMap::from([
            ("known".into(), prediction("a", 0.9)),
            ("absent".into(), prediction("b", 0.1)),
            ("withheld".into(), prediction("other_human", 0.6)),
        ]);
        let result =
            attribution_metrics(&rows, &predictions, "human", &["a".into(), "b".into()], 0.8)
                .unwrap();
        assert_eq!(result["conditional_accuracy"]["estimate"], 1.);
        assert_eq!(result["documented_absent_not_scored"], 1);
        assert_eq!(result["open_set"]["withheld_rejection"]["estimate"], 1.);
        assert_eq!(
            result["open_set"]["balanced_correct_named_vs_withheld_rejection"],
            1.
        );
    }
    #[test]
    fn nullable_origin_does_not_hide_supervised_share() {
        let mut row = row("x", Some(true), Some("a"), true);
        row["targets"]["share"] = json!(0.25);
        let pred = BTreeMap::from([("x".into(), prediction("a", 0.9))]);
        let result = origin_share_metrics(&[row], &pred, None).unwrap();
        assert!(result["involvement_brier"].is_null());
        assert!(result["ece10"].is_null());
        assert_eq!(result["share"]["mae_percentage_points"]["estimate"], 25.);
    }
    #[test]
    fn family_weighting_does_not_overcount_excerpts() {
        assert_eq!(
            weighted(&vec![("a".into(), 1.), ("a".into(), 1.), ("b".into(), 0.)]),
            Some(0.5)
        );
        assert!(summary(&vec![("a".into(), 1.)])["ci95"].is_null());
    }
    #[test]
    fn balancing_does_not_reward_artificial_generation_prevalence() {
        let mut records = Vec::new();
        for (y, count) in [(0, 1), (1, 2), (2, 20)] {
            for i in 0..count {
                records.push(json!({"id":format!("{y}-{i}"),"source_group":"one-work","targets":{"origin":y}}));
            }
        }
        let result =
            balanced_origin_metrics(&records, &BTreeMap::new(), Some([0., 1., 0.])).unwrap();
        assert!((result["involvement_brier"].as_f64().unwrap() - 1. / 3.).abs() < 1e-12);
        assert_eq!(result["labelled_documents"], 23);
        assert_eq!(result["family_origin_cells"], 3);
        assert!(result["involvement_brier_95ci"].is_null());
        assert!(balanced_origin_metrics(&[], &BTreeMap::new(), None).unwrap()["ece10"].is_null());
    }
    #[test]
    fn pooling_is_normalized_raw_mean_and_retrieval_is_separate() {
        let pooled = pool(&[vec![2., 0.], vec![0., 1.]]).unwrap().unwrap();
        assert!((pooled[0] - 2. / 5f64.sqrt()).abs() < 1e-12);
        let gallery = BTreeMap::from([("a".into(), pooled), ("b".into(), vec![0., 1.])]);
        assert_eq!(cosine_ranking(&[1., 0.], &gallery).unwrap()[0].0, "a");
        assert!(pool(&[vec![1., 0.], vec![-1., 0.]]).unwrap().is_none());
        assert!(cosine_ranking(&[1.], &gallery).is_err());
    }
    #[test]
    fn legacy_null_identity_is_not_withheld_evidence() {
        let truth =
            role_truth(&json!({"targets":{"origin":0,"human":null}}), "human", &[]).unwrap();
        assert_eq!(truth.present, Some(true));
        assert!(truth.identity.is_none());
    }
}

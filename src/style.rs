//! Descriptive author references and explicit, measurable style targets.
//! Source groups receive equal weight; none of these distances prove authorship.

use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

use crate::util::digest;

const SCHEMA: &str = "unslop-author-style-v1";
const RATE_FLOOR: f64 = 0.02;
const DEVIATION_CAP: f64 = 10.0;
type Coordinates = BTreeMap<String, f64>;

#[derive(Clone)]
struct Family {
    kind: String,
    opportunities: u64,
    values: Coordinates,
    floors: Coordinates,
}

struct Vector {
    extractor: String,
    families: BTreeMap<String, Family>,
}

fn object<'a>(value: &'a Value, label: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .with_context(|| format!("{label} must be an object"))
}

fn identifier<'a>(value: &'a Value, label: &str) -> Result<&'a str> {
    value
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("{label} must be a nonempty string"))
}

fn finite(value: &Value, label: &str) -> Result<f64> {
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .with_context(|| format!("{label} must be finite"))
}

fn coordinates(value: &Value, label: &str) -> Result<Coordinates> {
    object(value, label)?
        .iter()
        .map(|(key, value)| {
            ensure!(!key.trim().is_empty(), "Empty feature name in {label}");
            Ok((key.clone(), finite(value, label)?))
        })
        .collect()
}

fn sha(value: &Value, label: &str) -> Result<String> {
    let hash = identifier(value, label)?;
    ensure!(
        hash.len() == 64
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{label} must be a lowercase SHA-256 digest"
    );
    Ok(hash.into())
}

// serde_json::Map uses sorted keys in this crate. Rebuilding objects recursively
// makes the canonicalization rule explicit, including future map-feature changes.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<_, _> = map.iter().collect();
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(k, v)| (k.clone(), canonical(v)))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}

fn json_hash(value: &Value) -> Result<String> {
    Ok(digest(&serde_json::to_string(&canonical(value))?))
}

fn upper_bound(family: &str, feature: &str, kind: &str) -> Option<f64> {
    (kind != "metrics"
        || (family == "lexical_shape"
            && ["long_word_fraction_ge7", "contraction_fraction"].contains(&feature)))
    .then_some(1.0)
}

fn parse_vector(value: &Value) -> Result<Vector> {
    let extractor = identifier(&value["extractor"], "extractor")?.to_owned();
    let raw = object(&value["families"], "families")?;
    ensure!(!raw.is_empty(), "A style vector needs feature families");
    let mut families = BTreeMap::new();
    for (name, family) in raw {
        ensure!(!name.trim().is_empty(), "Empty family name");
        let kind = identifier(&family["kind"], "family kind")?;
        ensure!(
            ["distribution", "rates", "metrics"].contains(&kind),
            "Unsupported family kind {kind}"
        );
        let opportunities = family["opportunities"]
            .as_u64()
            .context("Family opportunities must be a nonnegative integer")?;
        let values = coordinates(&family["values"], "feature values")?;
        ensure!(
            values.values().all(|v| *v >= 0.0),
            "Feature values cannot be negative"
        );
        if kind != "metrics" {
            ensure!(
                values.values().all(|v| *v <= 1.0),
                "Distribution and rate values must be in [0, 1]"
            );
        }
        for (feature, value) in &values {
            ensure!(
                upper_bound(name, feature, kind).is_none_or(|upper| *value <= upper),
                "Feature {name}.{feature} exceeds its supported upper bound"
            );
        }
        if opportunities == 0 {
            ensure!(
                values.values().all(|v| *v == 0.0),
                "Zero opportunities cannot support nonzero values"
            );
        } else if kind == "distribution" {
            ensure!(
                (values.values().sum::<f64>() - 1.0).abs() <= 1e-6,
                "Distribution values must sum to one"
            );
        } else {
            ensure!(
                !values.is_empty(),
                "Observed rates and metrics need coordinates"
            );
        }
        let floors = if kind == "metrics" {
            let floors = coordinates(&family["scale_floors"], "metric scale floors")?;
            ensure!(
                floors.keys().eq(values.keys()) && floors.values().all(|v| *v > 0.0),
                "Each metric needs one positive scale floor"
            );
            floors
        } else {
            if let Some(floors) = family.get("scale_floors") {
                ensure!(
                    object(floors, "scale floors")?.is_empty(),
                    "Only metrics accept extractor scale floors"
                );
            }
            values
                .keys()
                .filter(|_| kind == "rates")
                .map(|key| (key.clone(), RATE_FLOOR))
                .collect()
        };
        families.insert(
            name.clone(),
            Family {
                kind: kind.into(),
                opportunities,
                values,
                floors,
            },
        );
    }
    Ok(Vector {
        extractor,
        families,
    })
}

fn compatible(reference: &Vector, other: &Vector) -> Result<()> {
    ensure!(
        reference.extractor == other.extractor,
        "Incompatible style extractors"
    );
    ensure!(
        reference.families.keys().eq(other.families.keys()),
        "Incompatible feature family inventories"
    );
    for (name, family) in &reference.families {
        let right = &other.families[name];
        ensure!(
            family.kind == right.kind,
            "Incompatible kind for family {name}"
        );
        if family.kind != "distribution" {
            ensure!(
                family.values.keys().eq(right.values.keys()),
                "Incompatible coordinates for family {name}"
            );
            ensure!(
                family.floors == right.floors,
                "Incompatible scale floors for family {name}"
            );
        }
    }
    Ok(())
}

fn moments(rows: &[Coordinates], keys: &BTreeSet<String>) -> (Coordinates, Coordinates) {
    if rows.is_empty() {
        return (
            keys.iter().map(|k| (k.clone(), 0.0)).collect(),
            keys.iter().map(|k| (k.clone(), 0.0)).collect(),
        );
    }
    let n = rows.len() as f64;
    let means: Coordinates = keys
        .iter()
        .map(|key| {
            (
                key.clone(),
                rows.iter()
                    .map(|row| row.get(key).copied().unwrap_or(0.0) / n)
                    .sum(),
            )
        })
        .collect();
    let sd = keys
        .iter()
        .map(|key| {
            let deviation = rows.iter().fold(0.0_f64, |norm, row| {
                norm.hypot((row.get(key).copied().unwrap_or(0.0) - means[key]) / n.sqrt())
            });
            (key.clone(), deviation)
        })
        .collect();
    (means, sd)
}

fn distance(
    kind: &str,
    observed: &Coordinates,
    target: &Coordinates,
    sd: &Coordinates,
    floors: &Coordinates,
) -> f64 {
    let keys: BTreeSet<_> = observed.keys().chain(target.keys()).collect();
    if kind == "distribution" {
        // Do not divide by vocabulary size: new categorical mass must count.
        (0.5 * keys
            .iter()
            .map(|key| {
                (observed.get(*key).copied().unwrap_or(0.0).sqrt()
                    - target.get(*key).copied().unwrap_or(0.0).sqrt())
                .powi(2)
            })
            .sum::<f64>())
        .sqrt()
        // Valid distributions permit tiny mass-rounding errors. Keep the
        // diagnostic in [0,1]; this does not clamp requested target coordinates.
        .min(1.0)
    } else if keys.is_empty() {
        0.0
    } else {
        let squares = keys
            .iter()
            .map(|key| {
                let scale = sd.get(*key).copied().unwrap_or(0.0).max(floors[*key]);
                let delta = (observed.get(*key).copied().unwrap_or(0.0)
                    - target.get(*key).copied().unwrap_or(0.0))
                    / scale;
                (delta.clamp(-DEVIATION_CAP, DEVIATION_CAP) / DEVIATION_CAP).powi(2)
            })
            .sum::<f64>();
        (squares / keys.len() as f64).sqrt()
    }
}

/// Fit a descriptive reference from independent source groups, not token counts.
pub fn fit(author_id: &str, samples: &[Value]) -> Result<Value> {
    ensure!(!author_id.trim().is_empty(), "author_id must be nonempty");
    ensure!(
        !samples.is_empty(),
        "Provide author samples from at least two groups"
    );
    let mut parsed = Vec::new();
    let mut ids = BTreeSet::new();
    let mut hashes = BTreeSet::new();
    let mut groups = BTreeSet::new();
    for sample in samples {
        let id = identifier(&sample["id"], "sample id")?.to_owned();
        let group = identifier(&sample["group_id"], "sample group_id")?.to_owned();
        let hash = sha(&sample["sha256"], "sample sha256")?;
        ensure!(ids.insert(id.clone()), "Duplicate sample id {id}");
        ensure!(hashes.insert(hash.clone()), "Duplicate sample text hash");
        groups.insert(group.clone());
        let vector = parse_vector(&sample["vector"])?;
        parsed.push((id, group, hash, json_hash(&sample["vector"])?, vector));
    }
    ensure!(
        groups.len() >= 2,
        "At least two independent source groups are required"
    );
    parsed.sort_by(|left, right| left.0.cmp(&right.0));
    let reference = &parsed[0].4;
    for sample in &parsed[1..] {
        compatible(reference, &sample.4)?;
    }
    let mut families = Map::new();
    let mut weights = Map::new();
    for (name, family) in &reference.families {
        let keys: BTreeSet<String> = parsed
            .iter()
            .flat_map(|sample| sample.4.families[name].values.keys().cloned())
            .collect();
        let mut group_means = BTreeMap::new();
        let mut document_counts = BTreeMap::new();
        let mut opportunity_counts = BTreeMap::new();
        for group in &groups {
            let observed: Vec<_> = parsed
                .iter()
                .filter(|sample| &sample.1 == group && sample.4.families[name].opportunities > 0)
                .collect();
            if observed.is_empty() {
                continue;
            }
            let rows: Vec<_> = observed
                .iter()
                .map(|sample| sample.4.families[name].values.clone())
                .collect();
            group_means.insert(group.clone(), moments(&rows, &keys).0);
            document_counts.insert(group.clone(), observed.len());
            opportunity_counts.insert(
                group.clone(),
                observed
                    .iter()
                    .map(|sample| sample.4.families[name].opportunities)
                    .collect::<Vec<_>>(),
            );
        }
        let rows: Vec<_> = group_means.values().cloned().collect();
        let (mean, sd) = moments(&rows, &keys);
        ensure!(
            mean.values().chain(sd.values()).all(|v| v.is_finite()),
            "Reference moments overflowed for {name}"
        );
        let spread: Vec<_> = rows
            .iter()
            .map(|row| distance(&family.kind, row, &mean, &sd, &family.floors))
            .collect();
        families.insert(name.clone(), json!({
            "kind": family.kind, "available": !rows.is_empty(),
            "group_count": rows.len(), "document_count": document_counts.values().sum::<usize>(),
            "mean": mean, "sd": sd, "scale_floors": family.floors,
            "group_means": group_means, "group_document_counts": document_counts,
            "group_opportunity_counts": opportunity_counts,
            "within_reference_normalized_distances": spread,
        }));
        weights.insert(
            name.clone(),
            json!(if name == "lexicon" { 0.0 } else { 1.0 }),
        );
    }
    let provenance: Vec<_> = parsed.iter().map(|(id, group, hash, vector_hash, _)| json!({"id":id,"group_id":group,"sha256":hash,"vector_sha256":vector_hash})).collect();
    let mut profile = json!({
        "schema_version": SCHEMA, "author_id": author_id, "extractor": reference.extractor,
        "sample_count": samples.len(), "group_count": groups.len(), "group_ids": groups,
        "samples": provenance, "families": families, "default_family_weights": weights,
        "method": {
            "group_weighting": "Average observed documents within each group, then weight observed groups equally within each family.",
            "sd": "Population standard deviation of group means; descriptive spread, not a standard error.",
            "zero_opportunities": "No observation for that family; excluded explicitly, not imputed as evidence of zero.",
            "distribution_distance": "Hellinger distance over union vocabulary, without division by dimension count; distance is capped at one for accepted probability-mass rounding errors.",
            "rates_metrics_distance": "RMS of standardized deviations, capped at 10 in magnitude, divided by 10.",
            "lexicon_default_weight": "Zero because topic vocabulary can confound author style; explicit opt-in is supported.",
        },
        "interpretation": "A reference distribution for these source groups, not a unique author signature or proof of authorship.",
        "authorship_certified": false, "meaning_certified": false, "detector_score_certified": false,
    });
    profile["profile_sha256"] = json!(json_hash(&profile)?);
    Ok(profile)
}

fn profile_reference(profile: &Value) -> Result<Vector> {
    ensure!(
        profile["schema_version"] == SCHEMA,
        "Unsupported author style profile schema"
    );
    let expected = sha(&profile["profile_sha256"], "profile_sha256")?;
    let mut body = profile.clone();
    object(&body, "profile")?;
    body.as_object_mut().unwrap().remove("profile_sha256");
    ensure!(
        json_hash(&body)? == expected,
        "Profile content does not match its frozen hash"
    );
    identifier(&profile["author_id"], "author_id")?;
    ensure!(
        profile["group_count"].as_u64().is_some_and(|n| n >= 2),
        "Invalid reference group count"
    );
    let mut families = Map::new();
    for (name, family) in object(&profile["families"], "profile families")? {
        let available = family["available"]
            .as_bool()
            .context("Invalid family availability")?;
        let kind = identifier(&family["kind"], "family kind")?;
        let mut reconstructed =
            json!({"kind":kind,"opportunities":u64::from(available),"values":family["mean"]});
        if kind == "metrics" {
            reconstructed["scale_floors"] = family["scale_floors"].clone();
        }
        let mean = coordinates(&family["mean"], "reference mean")?;
        let sd = coordinates(&family["sd"], "reference SD")?;
        ensure!(
            mean.keys().eq(sd.keys()) && sd.values().all(|v| *v >= 0.0),
            "Invalid reference SD coordinates"
        );
        families.insert(name.clone(), reconstructed);
    }
    let reference = parse_vector(&json!({"extractor":profile["extractor"],"families":families}))?;
    for (name, family) in &reference.families {
        ensure!(
            coordinates(
                &profile["families"][name]["scale_floors"],
                "reference scale floors"
            )? == family.floors,
            "Stored scale floors disagree with the family schema"
        );
    }
    Ok(reference)
}

/// Validate a frozen core profile before reading its measurements or controls.
pub fn validate_profile(profile: &Value) -> Result<()> {
    profile_reference(profile)?;
    Ok(())
}

fn only_keys(value: &Value, allowed: &[&str], label: &str) -> Result<()> {
    ensure!(
        object(value, label)?
            .keys()
            .all(|key| allowed.contains(&key.as_str())),
        "Unknown controls in {label}"
    );
    Ok(())
}

/// Compare with the frozen reference or an explicitly shifted measurable target.
pub fn compare(profile: &Value, vector: &Value, target: Option<&Value>) -> Result<Value> {
    let reference = profile_reference(profile)?;
    let candidate = parse_vector(vector)?;
    compatible(&reference, &candidate)?;
    let mut weights = coordinates(&profile["default_family_weights"], "default family weights")?;
    ensure!(
        weights.keys().eq(reference.families.keys()) && weights.values().all(|v| *v >= 0.0),
        "Invalid default family weights"
    );
    let mut targets: BTreeMap<_, _> = reference
        .families
        .iter()
        .map(|(name, family)| (name.clone(), family.values.clone()))
        .collect();
    let mut shifts = BTreeMap::new();
    if let Some(target) = target {
        only_keys(target, &["family_weights", "shifts"], "style target")?;
        if let Some(overrides) = target.get("family_weights") {
            for (name, weight) in coordinates(overrides, "target family weights")? {
                ensure!(
                    weights.contains_key(&name) && weight >= 0.0,
                    "Unknown family or negative target weight: {name}"
                );
                weights.insert(name, weight);
            }
        }
        if let Some(raw_shifts) = target.get("shifts") {
            for shift in raw_shifts
                .as_array()
                .context("Target shifts must be an array")?
            {
                only_keys(
                    shift,
                    &["family", "feature", "delta_sd"],
                    "coordinate shift",
                )?;
                let family_name = identifier(&shift["family"], "shift family")?;
                let feature = identifier(&shift["feature"], "shift feature")?;
                let delta_sd = finite(&shift["delta_sd"], "delta_sd")?;
                let family = reference
                    .families
                    .get(family_name)
                    .context("Unknown shift family")?;
                ensure!(
                    family.kind != "distribution",
                    "Categorical shifts require renormalization and are unsupported"
                );
                ensure!(
                    family.opportunities > 0,
                    "Cannot shift an unobserved reference family"
                );
                let mean = *family
                    .values
                    .get(feature)
                    .context("Unknown shift coordinate")?;
                let sd = finite(
                    &profile["families"][family_name]["sd"][feature],
                    "reference SD",
                )?;
                let scale = sd.max(family.floors[feature]);
                let shifted = mean + delta_sd * scale;
                ensure!(
                    shifted.is_finite()
                        && shifted >= 0.0
                        && upper_bound(family_name, feature, &family.kind)
                            .is_none_or(|upper| shifted <= upper),
                    "Shift for {family_name}.{feature} is outside its supported range; no clamping was applied"
                );
                ensure!(
                    shifts
                        .insert(
                            (family_name.to_owned(), feature.to_owned()),
                            json!({"family":family_name,"feature":feature,"delta_sd":delta_sd})
                        )
                        .is_none(),
                    "Duplicate coordinate shift"
                );
                targets
                    .get_mut(family_name)
                    .unwrap()
                    .insert(feature.into(), shifted);
            }
        }
    }
    let max_weight = weights.values().copied().fold(0.0_f64, f64::max);
    ensure!(
        max_weight > 0.0,
        "At least one family must have positive weight"
    );
    let effective_target = json!({"profile_sha256":profile["profile_sha256"],"family_weights":weights,"shifts":shifts.values().collect::<Vec<_>>()});
    let mut families = Map::new();
    let mut top_gaps = Vec::new();
    let mut numerator = 0.0;
    let mut denominator = 0.0;
    let mut complete = true;
    for (name, family) in &reference.families {
        let observed = &candidate.families[name];
        let available = family.opportunities > 0 && observed.opportunities > 0;
        let weight = weights[name];
        let sd = coordinates(&profile["families"][name]["sd"], "reference SD")?;
        let target_values = &targets[name];
        if !available {
            if weight > 0.0 {
                complete = false;
            }
            families.insert(name.clone(), json!({"kind":family.kind,"available":false,"weight":weight,"normalized_distance":null,"coordinate_gaps":[],"reference_observed":family.opportunities>0,"candidate_opportunities":observed.opportunities,"reason":"Family has no observations on one or both sides; omitted from aggregate with incomplete coverage reported."}));
            continue;
        }
        let normalized_distance = distance(
            &family.kind,
            &observed.values,
            target_values,
            &sd,
            &family.floors,
        );
        ensure!(normalized_distance.is_finite(), "Nonfinite family distance");
        numerator += normalized_distance * (weight / max_weight);
        denominator += weight / max_weight;
        let keys: BTreeSet<_> = family.values.keys().chain(observed.values.keys()).collect();
        let mut gaps = Vec::new();
        for feature in keys {
            let value = observed.values.get(feature).copied().unwrap_or(0.0);
            let target_value = target_values.get(feature).copied().unwrap_or(0.0);
            let delta = value - target_value;
            let mut gap = json!({"family":name,"feature":feature,"candidate_value":value,"reference_mean":family.values.get(feature).copied().unwrap_or(0.0),"target_value":target_value,"difference":delta,"within_reference_sd":sd.get(feature).copied().unwrap_or(0.0),"direction_to_target":if delta>0.0 {"decrease"} else if delta<0.0 {"increase"} else {"on_target"}});
            let magnitude = if family.kind == "distribution" {
                let contribution = 0.5 * (value.sqrt() - target_value.sqrt()).powi(2);
                gap["hellinger_squared_contribution"] = json!(contribution);
                contribution.sqrt()
            } else {
                let scale = sd[feature].max(family.floors[feature]);
                let z = delta / scale;
                let capped = z.clamp(-DEVIATION_CAP, DEVIATION_CAP);
                gap["scale_floor"] = json!(family.floors[feature]);
                gap["standardization_scale"] = json!(scale);
                gap["standardized_difference"] = if z.is_finite() { json!(z) } else { Value::Null };
                gap["standardized_difference_overflow"] = json!(!z.is_finite());
                gap["capped_standardized_difference"] = json!(capped);
                capped.abs() / DEVIATION_CAP
            };
            gap["priority"] = json!(magnitude * (weight / max_weight));
            if weight > 0.0 && delta != 0.0 {
                top_gaps.push(gap.clone());
            }
            gaps.push(gap);
        }
        families.insert(name.clone(), json!({"kind":family.kind,"available":true,"weight":weight,"normalized_distance":normalized_distance,"candidate_opportunities":observed.opportunities,"reference_group_count":profile["families"][name]["group_count"],"within_reference_normalized_distances":profile["families"][name]["within_reference_normalized_distances"],"coordinate_gaps":gaps}));
    }
    ensure!(
        denominator > 0.0,
        "No observed family has positive comparison weight"
    );
    top_gaps.sort_by(|a, b| {
        b["priority"]
            .as_f64()
            .unwrap()
            .total_cmp(&a["priority"].as_f64().unwrap())
            .then_with(|| a["family"].as_str().cmp(&b["family"].as_str()))
            .then_with(|| a["feature"].as_str().cmp(&b["feature"].as_str()))
    });
    top_gaps.truncate(20);
    Ok(json!({
        "schema_version":"unslop-style-comparison-v1", "profile_sha256":profile["profile_sha256"],
        "target_sha256":json_hash(&effective_target)?, "effective_target":effective_target,
        "vector_sha256":json_hash(vector)?, "extractor":reference.extractor,
        "normalized_score":numerator / denominator, "lower_is_better":true,
        "complete_family_coverage":complete, "families":families, "top_actionable_gaps":top_gaps,
        "interpretation":"Family-balanced descriptive distance to the chosen reference target, not a calibrated probability, authorship test, readability judgment, or semantic-preservation score. Incomplete family coverage limits comparisons across texts.",
        "normalized_deviation_cap":DEVIATION_CAP,
        "authorship_certified":false,"meaning_certified":false,"detector_score_certified":false,"statistical_confidence_claimed":false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector(rate: f64, metric: f64, distribution: Value) -> Value {
        json!({"extractor":"test-v1","families":{
            "function_word":{"kind":"rates","opportunities":100,"values":{"first_person":rate}},
            "rhythm":{"kind":"metrics","opportunities":100,"values":{"sentence_words":metric},"scale_floors":{"sentence_words":2.0}},
            "sentence_length":{"kind":"distribution","opportunities":10,"values":distribution},
            "lexicon":{"kind":"distribution","opportunities":100,"values":{"topic":1.0}}
        },"diagnostics":{}})
    }

    fn sample(id: &str, group: &str, vector: Value) -> Value {
        json!({"id":id,"group_id":group,"sha256":digest(id),"vector":vector})
    }

    fn reference() -> Value {
        let v = vector(0.1, 20.0, json!({"short":1.0}));
        fit(
            "author",
            &[sample("a", "g1", v.clone()), sample("b", "g2", v)],
        )
        .unwrap()
    }

    #[test]
    fn weights_groups_equally_and_freezes_order_independently() {
        let samples = vec![
            sample("a", "g1", vector(0.0, 10.0, json!({"short":1.0}))),
            sample("b", "g1", vector(0.2, 20.0, json!({"short":1.0}))),
            sample("c", "g2", vector(0.9, 30.0, json!({"short":1.0}))),
        ];
        let profile = fit("author", &samples).unwrap();
        assert!(
            (profile["families"]["function_word"]["mean"]["first_person"]
                .as_f64()
                .unwrap()
                - 0.5)
                .abs()
                < 1e-12
        );
        assert!(
            (profile["families"]["function_word"]["sd"]["first_person"]
                .as_f64()
                .unwrap()
                - 0.4)
                .abs()
                < 1e-12
        );
        assert_eq!(
            profile["families"]["rhythm"]["mean"]["sentence_words"],
            22.5
        );
        assert_eq!(
            profile,
            fit("author", &samples.into_iter().rev().collect::<Vec<_>>()).unwrap()
        );
    }

    #[test]
    fn rejects_duplicates_single_group_and_bad_provenance() {
        let first = sample("a", "g1", vector(0.1, 20.0, json!({"short":1.0})));
        let mut second = first.clone();
        second["id"] = json!("b");
        second["group_id"] = json!("g2");
        assert!(fit("author", &[first.clone(), second]).is_err());
        assert!(
            fit(
                "author",
                &[first.clone(), sample("b", "g1", first["vector"].clone())]
            )
            .is_err()
        );
        let mut malformed = sample("b", "g2", first["vector"].clone());
        malformed["sha256"] = json!("");
        assert!(fit("author", &[first, malformed]).is_err());
    }

    #[test]
    fn identity_is_zero_and_new_distribution_mass_is_not_diluted() {
        let profile = reference();
        let mut v = vector(0.1, 20.0, json!({"short":1.0}));
        assert_eq!(
            compare(&profile, &v, None).unwrap()["normalized_score"],
            0.0
        );
        v["families"]["sentence_length"]["values"] = json!({"unseen":1.0});
        let result = compare(&profile, &v, None).unwrap();
        assert_eq!(
            result["families"]["sentence_length"]["normalized_distance"],
            1.0
        );
        assert!((result["normalized_score"].as_f64().unwrap() - 1.0 / 3.0).abs() < 1e-12);
        assert_eq!(
            result["families"]["sentence_length"]["coordinate_gaps"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn floors_keep_zero_spread_finite_and_closer_candidate_scores_lower() {
        let profile = reference();
        let far = compare(&profile, &vector(0.3, 30.0, json!({"short":1.0})), None).unwrap();
        let close = compare(&profile, &vector(0.12, 22.0, json!({"short":1.0})), None).unwrap();
        assert!(close["normalized_score"].as_f64().unwrap().is_finite());
        assert!(
            close["normalized_score"].as_f64().unwrap() < far["normalized_score"].as_f64().unwrap()
        );
        assert_eq!(
            close["families"]["rhythm"]["coordinate_gaps"][0]["standardization_scale"],
            2.0
        );
    }

    #[test]
    fn explicit_shift_changes_the_target_without_changing_profile() {
        let profile = reference();
        let saved = profile.clone();
        let target =
            json!({"shifts":[{"family":"rhythm","feature":"sentence_words","delta_sd":-2.0}]});
        let candidate = vector(0.1, 16.0, json!({"short":1.0}));
        assert_eq!(
            compare(&profile, &candidate, Some(&target)).unwrap()["normalized_score"],
            0.0
        );
        assert!(
            compare(&profile, &candidate, None).unwrap()["normalized_score"]
                .as_f64()
                .unwrap()
                > 0.0
        );
        assert_eq!(profile, saved);
    }

    #[test]
    fn rejects_unknown_categorical_duplicate_and_out_of_range_targets() {
        let profile = reference();
        let v = vector(0.1, 20.0, json!({"short":1.0}));
        for target in [
            json!({"audience":"general"}),
            json!({"family_weights":{"unknown":1}}),
            json!({"family_weights":{"rhythm":-1}}),
            json!({"shifts":[{"family":"sentence_length","feature":"short","delta_sd":1}]}),
            json!({"shifts":[{"family":"rhythm","feature":"unknown","delta_sd":1}]}),
            json!({"shifts":[{"family":"rhythm","feature":"sentence_words","delta_sd":-11}]}),
            json!({"shifts":[{"family":"function_word","feature":"first_person","delta_sd":100}]}),
            json!({"shifts":[{"family":"rhythm","feature":"sentence_words","delta_sd":1},{"family":"rhythm","feature":"sentence_words","delta_sd":2}]}),
        ] {
            assert!(
                compare(&profile, &v, Some(&target)).is_err(),
                "accepted {target}"
            );
        }
    }

    #[test]
    fn rejects_incompatible_extractors_and_tampered_profiles() {
        let mut profile = reference();
        let mut v = vector(0.1, 20.0, json!({"short":1.0}));
        v["extractor"] = json!("other-v1");
        assert!(compare(&profile, &v, None).is_err());
        assert!(
            fit(
                "author",
                &[
                    sample("a", "g1", v),
                    sample("b", "g2", vector(0.1, 20.0, json!({"short":1.0})))
                ]
            )
            .is_err()
        );
        profile["families"]["rhythm"]["mean"]["sentence_words"] = json!(99);
        assert!(validate_profile(&profile).is_err());
        assert!(compare(&profile, &vector(0.1, 20.0, json!({"short":1.0})), None).is_err());
    }

    #[test]
    fn lexicon_is_opt_in_and_zero_opportunities_are_explicit() {
        let profile = reference();
        let mut v = vector(0.1, 20.0, json!({"short":1.0}));
        v["families"]["lexicon"]["values"] = json!({"other_topic":1.0});
        assert_eq!(
            compare(&profile, &v, None).unwrap()["normalized_score"],
            0.0
        );
        let target = json!({"family_weights":{"lexicon":1}});
        assert!(
            compare(&profile, &v, Some(&target)).unwrap()["normalized_score"]
                .as_f64()
                .unwrap()
                > 0.0
        );
        v["families"]["sentence_length"]["opportunities"] = json!(0);
        v["families"]["sentence_length"]["values"] = json!({});
        let compared = compare(&profile, &v, None).unwrap();
        assert_eq!(compared["complete_family_coverage"], false);
        assert_eq!(
            compared["families"]["sentence_length"]["normalized_distance"],
            Value::Null
        );
    }

    #[test]
    fn rejects_invalid_values_floors_and_family_kinds() {
        let good = vector(0.1, 20.0, json!({"short":1.0}));
        for bad in [
            vector(1.1, 20.0, json!({"short":1.0})),
            vector(0.1, -1.0, json!({"short":1.0})),
            vector(0.1, 20.0, json!({"short":0.8})),
        ] {
            assert!(
                fit(
                    "author",
                    &[sample("a", "g1", good.clone()), sample("b", "g2", bad)]
                )
                .is_err()
            );
        }
        let mut bad = good.clone();
        bad["families"]["rhythm"]["scale_floors"]["sentence_words"] = json!(0);
        assert!(fit("author", &[sample("a", "g1", good), sample("b", "g2", bad)]).is_err());
    }

    #[test]
    fn real_extractor_profile_survives_json_roundtrip() {
        let texts = [
            "I write short notes. I read them aloud, and then I revise.",
            "We keep the old records. However, a long explanation may help new readers.",
        ];
        let samples: Vec<_> = texts.iter().enumerate().map(|(i, text)| {
            json!({"id":format!("sample-{i}"),"group_id":format!("group-{i}"),"sha256":digest(text),
                "vector":crate::style_features::extract(text,false,"unused-python").unwrap()})
        }).collect();
        let profile = fit("author", &samples).unwrap();
        let stored: Value =
            serde_json::from_str(&serde_json::to_string_pretty(&profile).unwrap()).unwrap();
        validate_profile(&stored).unwrap();
        let comparison = compare(&stored, &samples[0]["vector"], None).unwrap();
        assert!(comparison["normalized_score"].as_f64().unwrap().is_finite());
        assert_eq!(comparison["profile_sha256"], profile["profile_sha256"]);
        assert_eq!(comparison["complete_family_coverage"], true);
    }

    #[test]
    fn metric_fractions_reject_impossible_observations_and_targets() {
        let mut v = vector(0.1, 20.0, json!({"short":1.0}));
        v["families"]["lexical_shape"] = json!({"kind":"metrics","opportunities":100,
            "values":{"long_word_fraction_ge7":0.5,"contraction_fraction":0.02},
            "scale_floors":{"long_word_fraction_ge7":0.02,"contraction_fraction":0.02}});
        let profile = fit(
            "author",
            &[sample("a", "g1", v.clone()), sample("b", "g2", v.clone())],
        )
        .unwrap();
        for feature in ["long_word_fraction_ge7", "contraction_fraction"] {
            let mut impossible = v.clone();
            impossible["families"]["lexical_shape"]["values"][feature] = json!(1.1);
            assert!(compare(&profile, &impossible, None).is_err());
            let target =
                json!({"shifts":[{"family":"lexical_shape","feature":feature,"delta_sd":60}]});
            assert!(compare(&profile, &v, Some(&target)).is_err());
        }
    }

    #[test]
    fn probability_mass_rounding_cannot_exceed_distance_range() {
        let v = vector(0.1, 20.0, json!({"a":0.5000001,"b":0.5000001}));
        let profile = fit(
            "author",
            &[sample("a", "g1", v.clone()), sample("b", "g2", v)],
        )
        .unwrap();
        let candidate = vector(0.1, 20.0, json!({"c":0.5000001,"d":0.5000001}));
        let target = json!({"family_weights":{"function_word":0,"rhythm":0}});
        assert_eq!(
            compare(&profile, &candidate, Some(&target)).unwrap()["normalized_score"],
            1.0
        );
    }
}

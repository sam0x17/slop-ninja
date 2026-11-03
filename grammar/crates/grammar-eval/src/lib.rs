//! Fixed-protocol author retrieval in shared grammar-core geometry.
//!
//! Profiles average raw probabilities within source groups, then across groups.
//! Averaging square-root coordinates would produce a different target. Sparse
//! distances below are algebraically identical to dense core-space distances.
use anyhow::{Result, ensure};
use grammar_core::{
    features::{Family, Features},
    space::{Kind, Space},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub mod weights;

pub const PROTOCOL: &str = "unslop-author-retrieval-v1";
/// Calendar validity is independent of a corpus's historical age restriction.
pub const DATE_POLICY: &str = "valid-supplied-calendar-date-v2";
pub type Values = BTreeMap<String, f64>;
pub type Profile = BTreeMap<String, Values>;

pub struct DistanceGeometry {
    pub families: BTreeMap<String, Kind>,
    pub numerical_scales: BTreeMap<String, Vec<(String, f64)>>,
}

impl DistanceGeometry {
    pub fn new(space: &Space) -> Self {
        let mut numerical_scales: BTreeMap<String, Vec<(String, f64)>> = BTreeMap::new();
        for axis in &space.axes {
            if axis.kind != Kind::Distribution {
                numerical_scales
                    .entry(axis.family.clone())
                    .or_default()
                    .push((axis.feature.clone(), axis.scale));
            }
        }
        Self {
            families: space
                .family_schemas
                .iter()
                .map(|(name, schema)| (name.clone(), schema.kind.clone()))
                .collect(),
            numerical_scales,
        }
    }
}

pub fn family_values(family: &Family) -> Result<Values> {
    match family {
        Family::Distribution {
            counts,
            opportunities,
        }
        | Family::Rates {
            counts,
            opportunities,
        } => {
            ensure!(*opportunities > 0, "unavailable feature family");
            Ok(counts
                .iter()
                .map(|(k, n)| (k.clone(), *n as f64 / *opportunities as f64))
                .collect())
        }
        Family::Metrics {
            values,
            opportunities,
            ..
        } => {
            ensure!(*opportunities > 0, "unavailable metric family");
            Ok(values.clone())
        }
    }
}

pub fn document_values(features: &Features) -> Result<Profile> {
    features
        .families
        .iter()
        .map(|(name, family)| Ok((name.clone(), family_values(family)?)))
        .collect()
}

/// Equal weight for date groups, then equal weight for posts within each date.
pub fn grouped_profile(documents: &[(&str, &Features)]) -> Result<Profile> {
    ensure!(!documents.is_empty(), "empty author profile");
    let mut groups: BTreeMap<&str, Vec<&Features>> = BTreeMap::new();
    for (group, features) in documents {
        groups.entry(group).or_default().push(features);
    }
    let mut profile = Profile::new();
    for documents in groups.values() {
        for features in documents {
            for (family, values) in document_values(features)? {
                let out = profile.entry(family).or_default();
                for (key, value) in values {
                    *out.entry(key).or_default() +=
                        value / documents.len() as f64 / groups.len() as f64;
                }
            }
        }
    }
    Ok(profile)
}

/// Unweighted per-family squared distances. Numerical scales are always shared.
pub fn family_distances(
    geometry: &DistanceGeometry,
    query: &Profile,
    target: &Profile,
) -> Result<Values> {
    let mut result = Values::new();
    for (name, kind) in &geometry.families {
        let q = query
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("missing query family {name}"))?;
        let p = target
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("missing profile family {name}"))?;
        let value = match kind {
            Kind::Distribution => {
                ensure!(
                    (q.values().sum::<f64>() - 1.0).abs() < 1e-9,
                    "query distribution mass"
                );
                ensure!(
                    (p.values().sum::<f64>() - 1.0).abs() < 1e-9,
                    "profile distribution mass"
                );
                let (small, large) = if q.len() <= p.len() { (q, p) } else { (p, q) };
                let overlap = small
                    .iter()
                    .map(|(key, value)| (value * large.get(key).copied().unwrap_or(0.0)).sqrt())
                    .sum::<f64>();
                ensure!(overlap <= 1.0 + 1e-9, "invalid Hellinger overlap");
                (1.0 - overlap).max(0.0)
            }
            Kind::Metric | Kind::Rate => {
                let axes = &geometry.numerical_scales[name];
                ensure!(!axes.is_empty(), "missing numerical axes");
                axes.iter()
                    .map(|(feature, scale)| {
                        ((q.get(feature).copied().unwrap_or(0.0)
                            - p.get(feature).copied().unwrap_or(0.0))
                            / scale)
                            .powi(2)
                    })
                    .sum::<f64>()
                    / axes.len() as f64
            }
        };
        ensure!(value.is_finite(), "nonfinite distance");
        result.insert(name.clone(), value);
    }
    Ok(result)
}

pub fn ablations(defaults: &Values) -> BTreeMap<String, Values> {
    ["combined", "word", "grammar"]
        .into_iter()
        .map(|variant| {
            let mut weights: Values = defaults
                .iter()
                .filter(|(name, _)| {
                    let lexical = matches!(name.as_str(), "word" | "word_bigram");
                    variant == "combined" || (variant == "word") == lexical
                })
                .map(|(name, weight)| (name.clone(), *weight))
                .collect();
            let total = weights.values().sum::<f64>();
            for weight in weights.values_mut() {
                *weight /= total;
            }
            (variant.to_owned(), weights)
        })
        .collect()
}

pub fn weighted_distance(families: &Values, weights: &Values) -> f64 {
    weights
        .iter()
        .map(|(name, weight)| families[name] * weight)
        .sum::<f64>()
        .sqrt()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RankedAuthor {
    pub author_id: String,
    pub distance: f64,
    pub rank_min: usize,
    pub rank_max: usize,
}

/// Exact ties retain author-ID display order; metrics average over tied orders.
pub fn rank(mut scores: Vec<(String, f64)>) -> Result<Vec<RankedAuthor>> {
    ensure!(
        scores.iter().all(|(_, score)| score.is_finite()),
        "nonfinite retrieval score"
    );
    scores.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let mut out = Vec::new();
    let mut begin = 0;
    while begin < scores.len() {
        let mut end = begin + 1;
        while end < scores.len() && scores[end].1 == scores[begin].1 {
            end += 1;
        }
        for (author_id, distance) in &scores[begin..end] {
            out.push(RankedAuthor {
                author_id: author_id.clone(),
                distance: *distance,
                rank_min: begin + 1,
                rank_max: end,
            });
        }
        begin = end;
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub top1: f64,
    pub top5: f64,
    pub mrr: f64,
    pub own_vs_content_impostor: f64,
}

impl Metrics {
    fn add_scaled(&mut self, other: Self, scale: f64) {
        self.top1 += other.top1 * scale;
        self.top5 += other.top5 * scale;
        self.mrr += other.mrr * scale;
        self.own_vs_content_impostor += other.own_vs_content_impostor * scale;
    }
}

pub fn query_metrics(ranking: &[RankedAuthor], own: &str, impostor: &str) -> Result<Metrics> {
    let mine = ranking
        .iter()
        .find(|row| row.author_id == own)
        .ok_or_else(|| anyhow::anyhow!("missing true author"))?;
    let other = ranking
        .iter()
        .find(|row| row.author_id == impostor)
        .ok_or_else(|| anyhow::anyhow!("missing impostor"))?;
    let ties = (mine.rank_max - mine.rank_min + 1) as f64;
    let top = |k: usize| (k.min(mine.rank_max) + 1).saturating_sub(mine.rank_min) as f64 / ties;
    Ok(Metrics {
        top1: top(1),
        top5: top(5),
        mrr: (mine.rank_min..=mine.rank_max)
            .map(|rank| 1.0 / rank as f64)
            .sum::<f64>()
            / ties,
        own_vs_content_impostor: if mine.distance < other.distance {
            1.0
        } else if mine.distance == other.distance {
            0.5
        } else {
            0.0
        },
    })
}

#[derive(Debug, Serialize)]
pub struct SummaryMetrics {
    pub query_count: usize,
    pub author_count: usize,
    pub micro: Metrics,
    pub macro_author: Metrics,
    pub macro_author_bootstrap_95: BTreeMap<String, [f64; 2]>,
    pub per_author: BTreeMap<String, Metrics>,
}

/// Resample authors, preserving their per-author mean, with fixed xorshift seed.
pub fn summarize(rows: &[(String, Metrics)]) -> Result<SummaryMetrics> {
    ensure!(!rows.is_empty(), "no eligible queries");
    let mut grouped: BTreeMap<String, Vec<Metrics>> = BTreeMap::new();
    let mut micro = Metrics::default();
    for (author, metrics) in rows {
        grouped.entry(author.clone()).or_default().push(*metrics);
        micro.add_scaled(*metrics, 1.0 / rows.len() as f64);
    }
    let per_author: BTreeMap<_, _> = grouped
        .into_iter()
        .map(|(author, values)| {
            let mut average = Metrics::default();
            for value in &values {
                average.add_scaled(*value, 1.0 / values.len() as f64);
            }
            (author, average)
        })
        .collect();
    let values: Vec<_> = per_author.values().copied().collect();
    let mut macro_author = Metrics::default();
    for value in &values {
        macro_author.add_scaled(*value, 1.0 / values.len() as f64);
    }
    let mut state = 0x6a09e667f3bcc909_u64;
    let mut samples = Vec::new();
    for _ in 0..2000 {
        let mut sample = Metrics::default();
        for _ in 0..values.len() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            sample.add_scaled(
                values[(state % values.len() as u64) as usize],
                1.0 / values.len() as f64,
            );
        }
        samples.push(sample);
    }
    let mut intervals = BTreeMap::new();
    for key in ["top1", "top5", "mrr", "own_vs_content_impostor"] {
        let mut series: Vec<_> = samples
            .iter()
            .map(|m| match key {
                "top1" => m.top1,
                "top5" => m.top5,
                "mrr" => m.mrr,
                _ => m.own_vs_content_impostor,
            })
            .collect();
        series.sort_by(f64::total_cmp);
        intervals.insert(key.into(), [series[49], series[1949]]);
    }
    Ok(SummaryMetrics {
        query_count: rows.len(),
        author_count: values.len(),
        micro,
        macro_author,
        macro_author_bootstrap_95: intervals,
        per_author,
    })
}

/// IDF uses training documents only, never query frequencies or labels.
pub fn fit_idf(training: &[&BTreeMap<String, u64>]) -> Values {
    let mut df = BTreeMap::<String, usize>::new();
    for document in training {
        for key in document.keys() {
            *df.entry(key.clone()).or_default() += 1;
        }
    }
    df.into_iter()
        .map(|(key, count)| {
            (
                key,
                ((training.len() + 1) as f64 / (count + 1) as f64).ln() + 1.0,
            )
        })
        .collect()
}

pub fn normalize(values: &mut Values) {
    let norm = values.values().map(|v| v * v).sum::<f64>().sqrt();
    if norm > 0.0 {
        for value in values.values_mut() {
            *value /= norm;
        }
    }
}

pub fn tfidf(counts: &BTreeMap<String, u64>, idf: &Values) -> Values {
    let mut out: Values = counts
        .iter()
        .filter_map(|(key, count)| idf.get(key).map(|idf| (key.clone(), *count as f64 * idf)))
        .collect();
    normalize(&mut out);
    out
}

pub fn cosine_distance(left: &Values, right: &Values) -> f64 {
    (1.0 - left
        .iter()
        .map(|(key, value)| value * right.get(key).copied().unwrap_or(0.0))
        .sum::<f64>())
    .max(0.0)
}

/// Validate true calendar dates, not lexical year prefixes or release dates.
pub fn validate_date(date: &str) -> Result<()> {
    ensure!(
        date.len() == 10 && date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-',
        "invalid composition date {date}"
    );
    ensure!(
        date.bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit()),
        "invalid calendar date digits"
    );
    let year: u32 = date.get(..4).unwrap_or("").parse()?;
    let month: u32 = date.get(5..7).unwrap_or("").parse()?;
    let day: u32 = date.get(8..).unwrap_or("").parse()?;
    ensure!(
        year > 0 && (1..=12).contains(&month),
        "invalid calendar year or month"
    );
    let leap = year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100));
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    ensure!(
        (1..=days[(month - 1) as usize]).contains(&day),
        "invalid calendar day"
    );
    Ok(())
}

/// Exactly reproduce the export's chronological 80/10/10 date-group assignment.
pub fn validate_splits(rows: &[(&str, &str)]) -> Result<()> {
    let dates: BTreeSet<_> = rows.iter().map(|(date, _)| *date).collect();
    ensure!(dates.len() >= 3, "need three independent dates");
    let train_end = (dates.len() * 8 / 10).clamp(1, dates.len() - 2);
    let dev_end = (dates.len() * 9 / 10).clamp(train_end + 1, dates.len() - 1);
    for (index, date) in dates.into_iter().enumerate() {
        validate_date(date)?;
        let expected = if index < train_end {
            "train"
        } else if index < dev_end {
            "dev"
        } else {
            "test"
        };
        ensure!(
            rows.iter()
                .filter(|(d, _)| *d == date)
                .all(|(_, split)| *split == expected),
            "date group crosses split or is out of chronological order: {date}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::space::{self, Config, Reference};
    use sha2::{Digest, Sha256};

    fn fixture(label: &str, counts: [u64; 2], metric: f64) -> Features {
        Features {
            schema: "synthetic-v1".into(),
            parser_identity: "synthetic-parser-v1".into(),
            source_sha256: hex::encode(Sha256::digest(label.as_bytes())),
            families: BTreeMap::from([
                (
                    "word".into(),
                    Family::Distribution {
                        counts: BTreeMap::from([("a".into(), counts[0]), ("b".into(), counts[1])]),
                        opportunities: counts.iter().sum(),
                    },
                ),
                (
                    "syntax".into(),
                    Family::Metrics {
                        values: BTreeMap::from([("length".into(), metric)]),
                        scale_floors: BTreeMap::from([("length".into(), 1.0)]),
                        opportunities: 1,
                    },
                ),
            ]),
        }
    }

    #[test]
    fn profile_weights_dates_then_posts_before_square_root() {
        let a = fixture("a", [10, 0], 2.0);
        let b = fixture("b", [0, 10], 4.0);
        let c = fixture("c", [0, 100], 12.0);
        let profile = grouped_profile(&[("day1", &a), ("day1", &b), ("day2", &c)]).unwrap();
        assert_eq!(profile["word"]["a"], 0.25);
        assert_eq!(profile["word"]["b"], 0.75);
        assert_eq!(profile["syntax"]["length"], 7.5);
        assert_ne!(
            profile["word"]["a"].sqrt(),
            (1.0_f64.sqrt() + 0.0 + 0.0) / 3.0
        );
    }

    #[test]
    fn sparse_distance_matches_core_projection_in_one_shared_space() {
        let a = fixture("a", [8, 2], 2.0);
        let b = fixture("b", [2, 8], 8.0);
        let query = fixture("q", [7, 3], 12.0);
        let refs = vec![
            Reference {
                source_group: "a".into(),
                features: a.clone(),
            },
            Reference {
                source_group: "b".into(),
                features: b.clone(),
            },
        ];
        let space = space::fit(&refs, std::slice::from_ref(&query), &Config::default()).unwrap();
        let target = grouped_profile(&[("a", &a), ("b", &b)]).unwrap();
        let geometry = DistanceGeometry::new(&space);
        let distances =
            family_distances(&geometry, &document_values(&query).unwrap(), &target).unwrap();
        let sparse = weighted_distance(&distances, &space.family_weights);
        let point = space.project(&query).unwrap();
        let dense = point
            .coordinates
            .iter()
            .zip(&space.target)
            .map(|(q, p)| (q - p).powi(2))
            .sum::<f64>()
            .sqrt();
        assert!((sparse - dense).abs() < 1e-12);
        let author = grouped_profile(&[("a", &a)]).unwrap();
        let author_distance = weighted_distance(
            &family_distances(&geometry, &document_values(&query).unwrap(), &author).unwrap(),
            &space.family_weights,
        );
        let author_point = space.project(&a).unwrap();
        let expected = point
            .coordinates
            .iter()
            .zip(author_point.coordinates)
            .map(|(q, p)| (q - p).powi(2))
            .sum::<f64>()
            .sqrt();
        assert!((author_distance - expected).abs() < 1e-12);
    }

    #[test]
    fn ablation_renormalizes_without_refitting_scales() {
        let weights = BTreeMap::from([
            ("word".into(), 0.25),
            ("word_bigram".into(), 0.25),
            ("syntax".into(), 0.5),
        ]);
        let variants = ablations(&weights);
        assert_eq!(variants["word"]["word"], 0.5);
        assert_eq!(variants["grammar"]["syntax"], 1.0);
        assert_eq!(variants["combined"], weights);
    }

    #[test]
    fn tied_ranks_do_not_reward_author_id_order() {
        let rows = rank(vec![
            ("a".into(), 1.0),
            ("b".into(), 1.0),
            ("c".into(), 2.0),
        ])
        .unwrap();
        let a = query_metrics(&rows, "a", "b").unwrap();
        let b = query_metrics(&rows, "b", "a").unwrap();
        assert_eq!(a.top1, 0.5);
        assert_eq!(a.mrr, 0.75);
        assert_eq!(a.mrr, b.mrr);
        assert_eq!(a.own_vs_content_impostor, 0.5);
    }

    #[test]
    fn dates_and_cross_split_groups_are_rejected() {
        assert!(validate_date("1998-12-31").is_ok());
        assert!(validate_date("1997-01-01").is_ok());
        assert!(validate_date("0000-01-01").is_err());
        assert!(validate_date("+123-01-01").is_err());
        assert!(validate_date("1900-02-29").is_err());
        assert!(validate_date("2003-02-29").is_err());
        assert!(validate_date("2004-02-29").is_ok());
        let valid = [
            ("2004-01-01", "train"),
            ("2004-01-02", "dev"),
            ("2004-01-03", "test"),
        ];
        assert!(validate_splits(&valid).is_ok());
        let mut leaked = valid.to_vec();
        leaked.push(("2004-01-01", "test"));
        assert!(validate_splits(&leaked).is_err());
    }

    #[test]
    fn idf_excludes_unseen_query_terms_and_bootstrap_is_repeatable() {
        let counts = BTreeMap::from([("bird".into(), 2)]);
        let idf = fit_idf(&[&counts]);
        assert_eq!(idf["bird"], 1.0);
        assert!(tfidf(&BTreeMap::from([("query_only".into(), 3)]), &idf).is_empty());
        let rows = vec![
            (
                "a".into(),
                Metrics {
                    top1: 1.0,
                    ..Metrics::default()
                },
            ),
            ("b".into(), Metrics::default()),
        ];
        let one = summarize(&rows).unwrap();
        let two = summarize(&rows).unwrap();
        assert_eq!(one.macro_author.top1, 0.5);
        assert_eq!(one.macro_author_bootstrap_95, two.macro_author_bootstrap_95);
    }
}

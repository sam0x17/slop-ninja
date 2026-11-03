//! Whole-date-out retrieval with fixed mixtures and complete probability tails.
use crate::association_projection::{Counts, Random, VIEW_NAMES};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const VARIANTS: [&str; 9] = [
    "function_marginals",
    "function_joint_mix",
    "function_shuffle_mix_0",
    "function_shuffle_mix_17",
    "function_shuffle_mix_29",
    "function_shuffle_mix_101",
    "function_shuffle_mix_1009",
    "operator_inventory",
    "operator_joint_mix",
];
pub const PANELS: [&str; 3] = ["original", "replication", "confirmation"];

pub struct Post {
    pub id: String,
    pub author: String,
    pub date: String,
    pub views: BTreeMap<String, Counts>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub top1: f64,
    pub top5: f64,
    pub mrr: f64,
}
impl Metrics {
    pub fn add(&mut self, other: Self, weight: f64) {
        self.top1 += other.top1 * weight;
        self.top5 += other.top5 * weight;
        self.mrr += other.mrr * weight;
    }
    pub fn difference(self, other: Self) -> Self {
        Self {
            top1: self.top1 - other.top1,
            top5: self.top5 - other.top5,
            mrr: self.mrr - other.mrr,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryMetric {
    pub metrics: Metrics,
    pub rank_min: usize,
    pub rank_max: usize,
}

pub fn ranking(distances: &[f64], own: usize) -> Result<QueryMetric> {
    ensure!(
        own < distances.len() && distances.iter().all(|d| d.is_finite() && *d >= 0.0),
        "Invalid retrieval distances or own label"
    );
    let best = 1 + distances.iter().filter(|d| **d < distances[own]).count();
    let equal = distances.iter().filter(|d| **d == distances[own]).count();
    let worst = best + equal - 1;
    Ok(QueryMetric {
        metrics: Metrics {
            top1: if best == 1 { 1.0 / equal as f64 } else { 0.0 },
            top5: (5_usize.min(worst) + 1).saturating_sub(best) as f64 / equal as f64,
            mrr: (best..=worst).map(|rank| 1.0 / rank as f64).sum::<f64>() / equal as f64,
        },
        rank_min: best,
        rank_max: worst,
    })
}

type Sparse = Vec<(usize, f64)>;
fn average(profiles: &[&Sparse]) -> Sparse {
    let mut mean = BTreeMap::<usize, f64>::new();
    for profile in profiles {
        for &(index, value) in *profile {
            *mean.entry(index).or_default() += value / profiles.len() as f64;
        }
    }
    mean.into_iter().collect()
}
fn overlap(query: &Sparse, target: &Sparse) -> f64 {
    let (mut a, mut b, mut sum) = (0, 0, 0.0);
    while a < query.len() && b < target.len() {
        match query[a].0.cmp(&target[b].0) {
            std::cmp::Ordering::Less => a += 1,
            std::cmp::Ordering::Greater => b += 1,
            std::cmp::Ordering::Equal => {
                sum += query[a].1.sqrt() * target[b].1.sqrt();
                a += 1;
                b += 1;
            }
        }
    }
    sum
}
fn distance(overlap: f64) -> Result<f64> {
    ensure!(
        overlap.is_finite() && (0.0..=1.0 + 1e-9).contains(&overlap),
        "Invalid Hellinger overlap"
    );
    Ok((1.0 - overlap).max(0.0))
}

/// Return Q-by-A distances for one complete categorical view. Only an ID
/// dictionary is constructed; no vocabulary cutoff or geometry is fitted.
pub fn view_distances(posts: &[Post], authors: &[String], view: &str) -> Result<Vec<Vec<f64>>> {
    ensure!(
        !posts.is_empty() && !authors.is_empty() && authors.windows(2).all(|p| p[0] < p[1]),
        "Empty or repeated retrieval catalog"
    );
    let vocabulary = posts
        .iter()
        .flat_map(|p| p.views[view].keys().cloned())
        .collect::<BTreeSet<_>>();
    let indices = vocabulary
        .into_iter()
        .enumerate()
        .map(|(i, k)| (k, i))
        .collect::<BTreeMap<_, _>>();
    let probabilities = posts
        .iter()
        .map(|post| {
            let counts = &post.views[view];
            let n = counts.values().sum::<u64>();
            ensure!(
                n > 0 && counts.values().all(|c| *c > 0),
                "Unavailable view or explicit zero count"
            );
            Ok(counts
                .iter()
                .map(|(k, c)| (indices[k], *c as f64 / n as f64))
                .collect::<Sparse>())
        })
        .collect::<Result<Vec<_>>>()?;
    let mut groups = BTreeMap::<&str, BTreeMap<&str, Vec<usize>>>::new();
    for (index, post) in posts.iter().enumerate() {
        groups
            .entry(&post.author)
            .or_default()
            .entry(&post.date)
            .or_default()
            .push(index);
    }
    ensure!(
        groups
            .keys()
            .copied()
            .eq(authors.iter().map(String::as_str)),
        "Query author coverage differs"
    );
    let mut by_date = Vec::<BTreeMap<&str, Sparse>>::new();
    let mut targets = Vec::new();
    for author in authors {
        let dates = &groups[author.as_str()];
        ensure!(
            dates.len() >= 2,
            "Whole-date exclusion lacks a remaining date"
        );
        let means = dates
            .iter()
            .map(|(&date, rows)| {
                (
                    date,
                    average(&rows.iter().map(|i| &probabilities[*i]).collect::<Vec<_>>()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        targets.push(average(&means.values().collect::<Vec<_>>()));
        by_date.push(means);
    }
    let mut postings = vec![Vec::<(usize, f64)>::new(); indices.len()];
    for (author, target) in targets.iter().enumerate() {
        for &(key, p) in target {
            postings[key].push((author, p.sqrt()));
        }
    }
    let mut held = BTreeMap::<(usize, &str), Sparse>::new();
    let mut result = Vec::new();
    for (row, post) in posts.iter().enumerate() {
        let own = authors.binary_search(&post.author).unwrap();
        let query = &probabilities[row];
        let mut overlaps = vec![0.0; authors.len()];
        for &(key, q) in query {
            let q = q.sqrt();
            for &(author, p) in &postings[key] {
                overlaps[author] += q * p;
            }
        }
        let target = held.entry((own, post.date.as_str())).or_insert_with(|| {
            average(
                &by_date[own]
                    .iter()
                    .filter(|(date, _)| **date != post.date)
                    .map(|(_, p)| p)
                    .collect::<Vec<_>>(),
            )
        });
        overlaps[own] = overlap(query, target);
        result.push(
            overlaps
                .into_iter()
                .map(distance)
                .collect::<Result<Vec<_>>>()?,
        );
    }
    Ok(result)
}

pub fn mixtures(views: &[Vec<f64>]) -> Result<Vec<Vec<f64>>> {
    ensure!(
        views.len() == 10
            && !views[0].is_empty()
            && views.iter().all(|v| v.len() == views[0].len()),
        "View shape differs"
    );
    let mut out = vec![vec![0.0; views[0].len()]; 9];
    for author in 0..views[0].len() {
        let marginal = 0.5 * views[0][author] + 0.5 * views[1][author];
        out[0][author] = marginal;
        out[1][author] = 0.5 * marginal + 0.5 * views[2][author];
        for seed in 0..5 {
            out[seed + 2][author] = 0.5 * marginal + 0.5 * views[seed + 3][author];
        }
        out[7][author] = views[8][author];
        out[8][author] = 0.5 * views[8][author] + 0.5 * views[9][author];
    }
    Ok(out)
}

pub struct PanelResult {
    pub summary: Value,
    pub per_author: BTreeMap<String, BTreeMap<String, Metrics>>,
}

/// The caller stores complete view distances and exact ranks for later replay.
pub fn evaluate_panel(
    posts: &[Post],
    mut emit: impl FnMut(Value) -> Result<()>,
) -> Result<PanelResult> {
    let authors = posts
        .iter()
        .map(|p| p.author.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    ensure!(
        authors.len() == 100,
        "Frozen gallery must retain exactly100 authors"
    );
    let mut all = Vec::new();
    for view in VIEW_NAMES {
        all.push(view_distances(posts, &authors, view)?);
    }
    let mut by_author = BTreeMap::<String, BTreeMap<String, Vec<Vec<Metrics>>>>::new();
    for (index, post) in posts.iter().enumerate() {
        let views = all.iter().map(|v| v[index].clone()).collect::<Vec<_>>();
        let ds = mixtures(&views)?;
        let own = authors.binary_search(&post.author).unwrap();
        let metrics = ds
            .iter()
            .map(|v| ranking(v, own))
            .collect::<Result<Vec<_>>>()?;
        let mut values = metrics.iter().map(|v| v.metrics).collect::<Vec<_>>();
        let mut shuffle_mean = Metrics::default();
        for m in &values[2..7] {
            shuffle_mean.add(*m, 0.2);
        }
        values.push(shuffle_mean);
        by_author
            .entry(post.author.clone())
            .or_default()
            .entry(post.date.clone())
            .or_default()
            .push(values);
        emit(
            json!({"id":post.id,"author":post.author,"date":post.date,"own":own,"candidate_authors":authors,
            "view_names":VIEW_NAMES,"view_distances":views,"variant_names":VARIANTS,"variant_metrics":metrics,
            "mean_shuffle_metrics":shuffle_mean}),
        )?;
    }
    let names = VARIANTS
        .iter()
        .copied()
        .chain(std::iter::once("function_shuffle_metric_mean"))
        .collect::<Vec<_>>();
    let mut per_author = BTreeMap::<String, BTreeMap<String, Metrics>>::new();
    for (author, dates) in by_author {
        for (variant, name) in names.iter().enumerate() {
            let mut metric = Metrics::default();
            for posts in dates.values() {
                for post in posts {
                    metric.add(post[variant], 1.0 / dates.len() as f64 / posts.len() as f64);
                }
            }
            per_author
                .entry((*name).into())
                .or_default()
                .insert(author.clone(), metric);
        }
    }
    let models = per_author
        .iter()
        .map(|(name, authors)| {
            let mut mean = Metrics::default();
            for metric in authors.values() {
                mean.add(*metric, 1.0 / authors.len() as f64);
            }
            (
                name.clone(),
                json!({"macro_author":mean,"per_author":authors}),
            )
        })
        .collect::<BTreeMap<_, _>>();
    Ok(PanelResult {
        summary: json!({"authors":authors.len(),"posts":posts.len(),"models":models,
        "aggregation":"equal posts within date, equal dates within author, equal authors"}),
        per_author,
    })
}

pub fn comparisons(panels: &BTreeMap<String, PanelResult>) -> Result<Value> {
    let mut pairs = vec![(
        "function_joint_minus_marginals".to_owned(),
        "function_joint_mix",
        "function_marginals",
    )];
    for (index, seed) in crate::association_projection::SEEDS.iter().enumerate() {
        pairs.push((
            format!("function_shuffle_{seed}_minus_marginals"),
            VARIANTS[index + 2],
            "function_marginals",
        ));
    }
    pairs.push((
        "function_joint_minus_shuffle_metric_mean".into(),
        "function_joint_mix",
        "function_shuffle_metric_mean",
    ));
    pairs.push((
        "operator_joint_minus_inventory".into(),
        "operator_joint_mix",
        "operator_inventory",
    ));
    let mut out = BTreeMap::new();
    for (name, left, right) in pairs {
        let mut differences = Vec::new();
        let mut per_panel = BTreeMap::new();
        for panel in PANELS {
            let result = panels.get(panel).context("Missing fixed panel")?;
            let a = &result.per_author[left];
            let b = &result.per_author[right];
            ensure!(a.keys().eq(b.keys()), "Unpaired author comparison");
            let ds = a
                .iter()
                .map(|(author, m)| m.difference(b[author]))
                .collect::<Vec<_>>();
            let mut mean = Metrics::default();
            for d in &ds {
                mean.add(*d, 1.0 / ds.len() as f64);
            }
            per_panel.insert(panel, mean);
            differences.push(ds);
        }
        let total = differences.iter().map(Vec::len).sum::<usize>();
        let mut point = Metrics::default();
        for ds in &differences {
            for d in ds {
                point.add(*d, 1.0 / total as f64);
            }
        }
        let mut rng = Random(0x6a09e667f3bcc909);
        let mut series = [Vec::new(), Vec::new(), Vec::new()];
        for _ in 0..2000 {
            let mut m = Metrics::default();
            for ds in &differences {
                for _ in 0..ds.len() {
                    m.add(
                        ds[(rng.next() % ds.len() as u64) as usize],
                        1.0 / total as f64,
                    );
                }
            }
            for (i, v) in [m.top1, m.top5, m.mrr].into_iter().enumerate() {
                series[i].push(v);
            }
        }
        let mut intervals = BTreeMap::new();
        for (i, key) in ["top1", "top5", "mrr"].into_iter().enumerate() {
            series[i].sort_by(f64::total_cmp);
            intervals.insert(key, [series[i][49], series[i][1949]]);
        }
        out.insert(name,json!({"left":left,"right":right,"mean_difference":point,"per_panel_difference":per_panel,
            "paired_author_bootstrap_95":intervals,"replicates":2000,"percentile_indices_zero_based":[49,1949],
            "stratum_order":PANELS,"authors_per_stratum":differences.iter().map(Vec::len).collect::<Vec<_>>(),
            "interpretation":"descriptive intervals on previously used TRAIN authors; no adoption or fresh-confirmation test"}));
    }
    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn post(id: &str, author: &str, date: &str, word: &str) -> Post {
        Post {
            id: id.into(),
            author: author.into(),
            date: date.into(),
            views: BTreeMap::from([("test".into(), BTreeMap::from([(word.into(), 1)]))]),
        }
    }
    #[test]
    fn own_entire_date_is_excluded_and_reference_dates_have_equal_weight() {
        let posts = vec![
            post("a1", "A", "one", "alpha"),
            post("a2", "A", "one", "alpha"),
            post("a3", "A", "two", "beta"),
            post("b1", "B", "one", "alpha"),
            post("b2", "B", "two", "alpha"),
        ];
        let distances = view_distances(&posts, &["A".into(), "B".into()], "test").unwrap();
        assert_eq!(distances[0][0], 1.0);
        assert_eq!(distances[1][0], 1.0);
        assert_eq!(distances[0][1], 0.0);
        assert!((distances[3][0] - (1.0 - 0.5_f64.sqrt())).abs() < 1e-14);
        assert_eq!(distances[3][1], 0.0);
        assert_eq!(distances[2], [1.0, 1.0]);
        let p = vec![post("single", "A", "same", "alpha")];
        assert!(view_distances(&p, &["A".into()], "test").is_err());
    }
    #[test]
    fn exact_ties_and_mixtures_follow_frozen_weights() {
        let ranks = ranking(&[0.2, 0.2, 0.8], 0).unwrap();
        assert_eq!((ranks.rank_min, ranks.rank_max), (1, 2));
        assert_eq!(ranks.metrics.top1, 0.5);
        assert_eq!(ranks.metrics.mrr, 0.75);
        let tied = ranking(&[0.3; 10], 0).unwrap();
        assert_eq!(tied.metrics.top1, 0.1);
        assert_eq!(tied.metrics.top5, 0.5);
        let views = (0..10).map(|i| vec![i as f64 / 10.0]).collect::<Vec<_>>();
        let mixed = mixtures(&views).unwrap();
        assert_eq!(mixed[0], [0.05]);
        assert_eq!(mixed[1], [0.125]);
        assert_eq!(mixed[7], [0.8]);
        assert!((mixed[8][0] - 0.85).abs() < 1e-14);
    }
    #[test]
    fn paired_bootstrap_preserves_panel_sizes_and_averages_metrics() {
        let mut panels = BTreeMap::new();
        for (i, panel) in PANELS.iter().enumerate() {
            let mut values = BTreeMap::new();
            for name in VARIANTS.into_iter().chain(["function_shuffle_metric_mean"]) {
                let authors = (0..i + 1)
                    .map(|j| {
                        (
                            format!("{panel}-{j}"),
                            Metrics {
                                top1: if name == "function_joint_mix" {
                                    i as f64 / 2.0
                                } else {
                                    0.0
                                },
                                top5: 0.0,
                                mrr: 0.0,
                            },
                        )
                    })
                    .collect();
                values.insert(name.to_owned(), authors);
            }
            panels.insert(
                (*panel).into(),
                PanelResult {
                    summary: json!({}),
                    per_author: values,
                },
            );
        }
        let result = comparisons(&panels).unwrap();
        let first = &result["function_joint_minus_marginals"];
        assert_eq!(first["authors_per_stratum"], json!([1, 2, 3]));
        let expected = 4.0 / 6.0;
        assert!((first["mean_difference"]["top1"].as_f64().unwrap() - expected).abs() < 1e-14);
        assert!(
            (first["paired_author_bootstrap_95"]["top1"][0]
                .as_f64()
                .unwrap()
                - expected)
                .abs()
                < 1e-14
        );
    }
}

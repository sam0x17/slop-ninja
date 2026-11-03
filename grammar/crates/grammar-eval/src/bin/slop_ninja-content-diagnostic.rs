//! Descriptive content-proxy strata for already evaluated author-retrieval scores.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

#[derive(Deserialize)]
struct Binding {
    path: PathBuf,
    sha256: String,
}

#[derive(Deserialize)]
struct ScoreBinding {
    model: String,
    seed: u64,
    #[serde(flatten)]
    file: Binding,
}

#[derive(Deserialize)]
struct Gallery {
    name: String,
    queries: Binding,
    prepared_implementation_sha256: String,
    scores: Vec<ScoreBinding>,
}

#[derive(Deserialize)]
struct Protocol {
    schema: String,
    analysis: String,
    selection_or_training_allowed: bool,
    report: Binding,
    model_names: Vec<String>,
    inherited_seeds: Vec<u64>,
    strata: Vec<String>,
    galleries: Vec<Gallery>,
}

#[derive(Deserialize)]
struct Post {
    id: String,
    author_id: String,
    split: String,
}

#[derive(Deserialize)]
struct Ranked {
    author_id: String,
    distance: f64,
}

#[derive(Deserialize)]
struct Ranking {
    ranked_authors: Vec<Ranked>,
}

#[derive(Deserialize)]
struct Query {
    post: Post,
    content_proxy_available: bool,
    rankings: BTreeMap<String, Ranking>,
    equally_close_content_impostors: Vec<String>,
}

#[derive(Deserialize)]
struct Scores {
    candidate_authors: Vec<String>,
    query_ids: Vec<String>,
    logits: Vec<Vec<f64>>,
    per_query_metrics: Vec<Vec<f64>>,
}

#[derive(Clone, Copy, Default, Serialize)]
struct Measures {
    top1: f64,
    top5: f64,
    mrr: f64,
    nearest_content_impostor_concordance: Option<f64>,
}

impl Measures {
    fn add_scaled(&mut self, other: &Self, scale: f64) {
        self.top1 += other.top1 * scale;
        self.top5 += other.top5 * scale;
        self.mrr += other.mrr * scale;
        if let Some(value) = other.nearest_content_impostor_concordance {
            *self.nearest_content_impostor_concordance.get_or_insert(0.0) += value * scale;
        }
    }
}

#[derive(Default)]
struct AuthorSum {
    values: Measures,
    queries: usize,
    concordance_queries: usize,
}

impl AuthorSum {
    fn add(&mut self, values: Measures) {
        self.values.add_scaled(&values, 1.0);
        self.queries += 1;
        self.concordance_queries +=
            usize::from(values.nearest_content_impostor_concordance.is_some());
    }

    fn mean(&self) -> Measures {
        Measures {
            top1: self.values.top1 / self.queries as f64,
            top5: self.values.top5 / self.queries as f64,
            mrr: self.values.mrr / self.queries as f64,
            nearest_content_impostor_concordance: self
                .values
                .nearest_content_impostor_concordance
                .map(|sum| sum / self.concordance_queries as f64),
        }
    }
}

type Groups = BTreeMap<(String, String, String), BTreeMap<String, AuthorSum>>;

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn read_bound(root: &Path, binding: &Binding) -> Result<Vec<u8>> {
    let path = root.join(&binding.path);
    let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        digest(&bytes) == binding.sha256,
        "hash mismatch: {}",
        path.display()
    );
    Ok(bytes)
}

fn measures(scores: &[f64], own: usize, impostors: &[usize]) -> Result<Measures> {
    ensure!(
        own < scores.len() && scores.iter().all(|v| v.is_finite()),
        "invalid scores"
    );
    ensure!(
        impostors.iter().all(|i| *i < scores.len() && *i != own),
        "invalid impostor index"
    );
    let mine = scores[own];
    let better = scores.iter().filter(|v| **v > mine).count();
    let tied = scores.iter().filter(|v| **v == mine).count();
    let expected_top = |k: usize| k.saturating_sub(better).min(tied) as f64 / tied as f64;
    let mrr = ((better + 1)..=(better + tied))
        .map(|rank| 1.0 / rank as f64)
        .sum::<f64>()
        / tied as f64;
    let concordance = if impostors.is_empty() {
        None
    } else {
        Some(
            impostors
                .iter()
                .map(|i| {
                    if mine > scores[*i] {
                        1.0
                    } else if mine == scores[*i] {
                        0.5
                    } else {
                        0.0
                    }
                })
                .sum::<f64>()
                / impostors.len() as f64,
        )
    };
    Ok(Measures {
        top1: expected_top(1),
        top5: expected_top(5),
        mrr,
        nearest_content_impostor_concordance: concordance,
    })
}

fn content_group(
    distances: &[f64],
    own: usize,
    available: bool,
) -> Result<(&'static str, Vec<usize>)> {
    ensure!(
        distances.len() >= 2 && own < distances.len(),
        "invalid content gallery"
    );
    ensure!(
        distances.iter().all(|v| v.is_finite()),
        "nonfinite content distance"
    );
    if !available {
        return Ok(("content_unavailable", Vec::new()));
    }
    let nearest = distances
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != own)
        .map(|(_, v)| *v)
        .min_by(f64::total_cmp)
        .context("no impostor")?;
    let impostors = distances
        .iter()
        .enumerate()
        .filter(|(i, v)| *i != own && **v == nearest)
        .map(|(i, _)| i)
        .collect();
    let stratum = if distances.iter().all(|v| *v == distances[0]) {
        "content_uninformative"
    } else if distances[own] < nearest {
        "content_advantage"
    } else if distances[own] == nearest {
        "content_tie"
    } else {
        "content_disadvantage"
    };
    Ok((stratum, impostors))
}

fn add(
    groups: &mut Groups,
    model: &str,
    stratum: &str,
    gallery: &str,
    author: &str,
    value: Measures,
) {
    for group in [stratum, "overall"] {
        for location in [gallery, "pooled"] {
            groups
                .entry((model.into(), group.into(), location.into()))
                .or_default()
                .entry(author.into())
                .or_default()
                .add(value);
        }
    }
}

fn summarize(authors: &BTreeMap<String, AuthorSum>) -> Value {
    let mut result = Measures::default();
    let concordance_authors = authors
        .values()
        .filter(|v| v.concordance_queries > 0)
        .count();
    for author in authors.values() {
        let values = author.mean();
        result.add_scaled(
            &Measures {
                nearest_content_impostor_concordance: None,
                ..values
            },
            1.0 / authors.len() as f64,
        );
        if let Some(value) = values.nearest_content_impostor_concordance {
            *result
                .nearest_content_impostor_concordance
                .get_or_insert(0.0) += value / concordance_authors as f64;
        }
    }
    json!({"author_count":authors.len(),"query_count":authors.values().map(|v| v.queries).sum::<usize>(),
        "concordance_author_count":concordance_authors,
        "concordance_query_count":authors.values().map(|v| v.concordance_queries).sum::<usize>(),
        "macro_author":result})
}

fn run(args: Args) -> Result<()> {
    ensure!(!args.out.exists(), "choose a fresh output directory");
    let root = args.protocol.parent().context("protocol parent")?;
    let protocol_bytes = fs::read(&args.protocol)?;
    let protocol: Protocol = serde_json::from_slice(&protocol_bytes)?;
    ensure!(
        protocol.schema == "slopninja-content-proxy-diagnostic-v1"
            && protocol.analysis == "descriptive_after_test_evaluation"
            && !protocol.selection_or_training_allowed,
        "unsupported diagnostic protocol"
    );
    ensure!(
        protocol.inherited_seeds == [0, 17, 29]
            && protocol.model_names.len() == 7
            && protocol.model_names.iter().collect::<BTreeSet<_>>().len() == 7
            && protocol.galleries.len() == 6,
        "unexpected model, seed or gallery design"
    );
    let report: Value = serde_json::from_slice(&read_bound(root, &protocol.report)?)?;
    ensure!(
        report["schema"] == "slopninja-training-author-scale-test-result-v1",
        "report schema"
    );
    let mut groups = Groups::new();
    let mut rows = Vec::new();
    let mut all_authors = BTreeSet::new();
    let mut all_queries = BTreeSet::new();
    let mut score_files = 0;
    let mut score_metrics = 0;
    for gallery in &protocol.galleries {
        let query_path = root.join(&gallery.queries.path);
        let implementation_bytes = fs::read(
            query_path
                .parent()
                .context("query parent")?
                .join("implementation.json"),
        )?;
        ensure!(
            digest(&implementation_bytes) == gallery.prepared_implementation_sha256,
            "prepared implementation changed"
        );
        let implementation: Value = serde_json::from_slice(&implementation_bytes)?;
        ensure!(
            implementation["artifacts_sha256"]["test-queries.jsonl"] == gallery.queries.sha256,
            "unbound content queries"
        );
        let queries: Vec<Query> = String::from_utf8(read_bound(root, &gallery.queries)?)?
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
        ensure!(!queries.is_empty(), "empty gallery");
        let query_ids: Vec<_> = queries.iter().map(|q| q.post.id.clone()).collect();
        ensure!(
            query_ids.iter().collect::<BTreeSet<_>>().len() == query_ids.len(),
            "duplicate query"
        );
        for id in &query_ids {
            ensure!(
                all_queries.insert(id.clone()),
                "query reused across galleries"
            );
        }
        let candidate_authors: Vec<_> = queries[0].rankings["content_lemma_tfidf"]
            .ranked_authors
            .iter()
            .map(|r| r.author_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        ensure!(candidate_authors.len() == 100, "unexpected candidate count");
        for author in &candidate_authors {
            ensure!(
                all_authors.insert(author.clone()),
                "author reused across galleries"
            );
        }
        let positions: BTreeMap<_, _> = candidate_authors
            .iter()
            .enumerate()
            .map(|(i, a)| (a.as_str(), i))
            .collect();
        let mut loaded = BTreeMap::new();
        for binding in &gallery.scores {
            ensure!(
                protocol.model_names.contains(&binding.model)
                    && protocol.inherited_seeds.contains(&binding.seed),
                "unexpected model/seed"
            );
            let seed = report["models"][&binding.model]["seeds"]
                .as_array()
                .context("report seeds")?
                .iter()
                .find(|s| s["inherited_seed"].as_u64() == Some(binding.seed))
                .context("missing report seed")?;
            ensure!(
                seed["galleries"][&gallery.name]["scores_sha256"] == binding.file.sha256,
                "score file not bound to final report"
            );
            let scores: Scores = serde_json::from_slice(&read_bound(root, &binding.file)?)?;
            ensure!(
                scores.candidate_authors == candidate_authors && scores.query_ids == query_ids,
                "candidate/query identity or order differs"
            );
            ensure!(
                scores.logits.len() == queries.len()
                    && scores.per_query_metrics.len() == queries.len(),
                "score row shape"
            );
            ensure!(
                scores
                    .logits
                    .iter()
                    .all(|row| row.len() == candidate_authors.len()),
                "candidate score shape"
            );
            ensure!(
                loaded
                    .insert((binding.model.clone(), binding.seed), scores)
                    .is_none(),
                "duplicate model/seed"
            );
            score_files += 1;
        }
        ensure!(loaded.len() == 21, "missing model/seed scores");
        for (index, query) in queries.iter().enumerate() {
            ensure!(query.post.split == "test", "non-test query");
            let own = *positions
                .get(query.post.author_id.as_str())
                .context("true author absent")?;
            let ranked = &query
                .rankings
                .get("content_lemma_tfidf")
                .context("missing content ranking")?
                .ranked_authors;
            let content: BTreeMap<_, _> = ranked
                .iter()
                .map(|r| (r.author_id.clone(), r.distance))
                .collect();
            ensure!(
                ranked.len() == candidate_authors.len()
                    && content.keys().eq(candidate_authors.iter()),
                "content candidate mismatch/duplicate"
            );
            let distances: Vec<_> = candidate_authors.iter().map(|a| content[a]).collect();
            let (stratum, impostors) =
                content_group(&distances, own, query.content_proxy_available)?;
            ensure!(
                protocol.strata.iter().any(|s| s == stratum),
                "unsupported content stratum"
            );
            if query.content_proxy_available {
                let actual: BTreeSet<_> =
                    impostors.iter().map(|i| &candidate_authors[*i]).collect();
                let saved: BTreeSet<_> = query.equally_close_content_impostors.iter().collect();
                ensure!(
                    actual == saved && saved.len() == query.equally_close_content_impostors.len(),
                    "content impostor tie set differs"
                );
            }
            let mut model_values = BTreeMap::new();
            for model in &protocol.model_names {
                let mut values = Measures::default();
                for seed in &protocol.inherited_seeds {
                    let scores = &loaded[&(model.clone(), *seed)];
                    let measured = measures(&scores.logits[index], own, &impostors)?;
                    let saved = &scores.per_query_metrics[index];
                    ensure!(
                        saved.len() == 3 && saved.iter().all(|v| v.is_finite()),
                        "saved metric shape"
                    );
                    for (actual, expected) in [measured.top1, measured.top5, measured.mrr]
                        .iter()
                        .zip(saved)
                    {
                        ensure!(
                            (actual - expected).abs() <= 1e-12,
                            "saved ranking metric mismatch"
                        );
                    }
                    values.add_scaled(&measured, 1.0 / protocol.inherited_seeds.len() as f64);
                    score_metrics += 1;
                }
                add(
                    &mut groups,
                    model,
                    stratum,
                    &gallery.name,
                    &query.post.author_id,
                    values,
                );
                model_values.insert(model.clone(), values);
            }
            if query.content_proxy_available {
                let content_scores: Vec<_> = distances.iter().map(|d| -*d).collect();
                let values = measures(&content_scores, own, &impostors)?;
                add(
                    &mut groups,
                    "content_lemma_tfidf",
                    stratum,
                    &gallery.name,
                    &query.post.author_id,
                    values,
                );
                model_values.insert("content_lemma_tfidf".into(), values);
            }
            rows.push(json!({"gallery":gallery.name,"query_id":query.post.id,"author_id":query.post.author_id,
                "stratum":stratum,"own_content_distance":distances[own],
                "nearest_content_impostor_ids":impostors.iter().map(|i| &candidate_authors[*i]).collect::<Vec<_>>(),
                "metrics":model_values}));
        }
    }
    let summaries: Vec<_> = groups
        .iter()
        .map(|((model, stratum, gallery), authors)| {
            let mut value = summarize(authors);
            value["model"] = json!(model);
            value["stratum"] = json!(stratum);
            value["gallery"] = json!(gallery);
            value
        })
        .collect();
    for model in &protocol.model_names {
        let actual = summaries
            .iter()
            .find(|s| s["model"] == *model && s["stratum"] == "overall" && s["gallery"] == "pooled")
            .context("missing overall summary")?;
        let expected = &report["models"][model]["seed_mean"];
        ensure!(
            actual["author_count"] == expected["author_count"]
                && actual["query_count"] == expected["query_count"],
            "overall support differs"
        );
        for metric in ["top1", "top5", "mrr"] {
            ensure!(
                (actual["macro_author"][metric]
                    .as_f64()
                    .context("summary metric")?
                    - expected["macro_author"][metric]
                        .as_f64()
                        .context("report metric")?)
                .abs()
                    <= 1e-12,
                "overall metric differs"
            );
        }
    }
    fs::create_dir(&args.out)?;
    fs::write(args.out.join("protocol.json"), &protocol_bytes)?;
    fs::write(
        args.out.join("source.rs"),
        include_bytes!("slop_ninja-content-diagnostic.rs"),
    )?;
    let query_output = rows
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join("\n")
        + "\n";
    fs::write(args.out.join("queries.jsonl"), &query_output)?;
    let output = json!({"schema":"slopninja-content-proxy-diagnostic-result-v1","status":"pass",
        "analysis":"descriptive_after_test_evaluation","protocol_sha256":digest(&protocol_bytes),
        "source_sha256":digest(include_bytes!("slop_ninja-content-diagnostic.rs")),
        "executed_binary_sha256":digest(&fs::read(std::env::current_exe()?)?),
        "source_report_sha256":protocol.report.sha256,"queries_sha256":digest(query_output.as_bytes()),
        "score_files_verified":score_files,"per_query_metrics_reconstructed":score_metrics,
        "authors":all_authors.len(),"queries":all_queries.len(),"summaries":summaries,
        "checks":{"all_file_hashes_bound":true,"candidate_query_identity":true,"exact_content_impostor_ties":true,"all_original_report_aggregates_replayed":true},
        "fitting_or_selection_performed":false,"external_model_calls":0,"detector_calls":0});
    fs::write(
        args.out.join("report.json"),
        serde_json::to_vec_pretty(&output)?,
    )?;
    println!(
        "Content diagnostic complete: {} authors, {} queries, {} score files",
        all_authors.len(),
        all_queries.len(),
        score_files
    );
    Ok(())
}

fn main() -> Result<()> {
    run(Args::parse())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_ranks_and_all_nearest_impostor_ties() {
        let result = measures(&[2.0, 2.0, 3.0, 1.0], 0, &[1, 2, 3]).unwrap();
        assert_eq!(result.top1, 0.0);
        assert_eq!(result.top5, 1.0);
        assert!((result.mrr - (0.5 + 1.0 / 3.0) / 2.0).abs() < 1e-15);
        assert_eq!(result.nearest_content_impostor_concordance, Some(0.5));
        assert_eq!(measures(&[0.0; 10], 0, &[1]).unwrap().top5, 0.5);
        assert!(measures(&[f64::NAN, 0.0], 0, &[1]).is_err());
        assert!(measures(&[1.0, 0.0], 0, &[0]).is_err());
    }

    #[test]
    fn content_strata_separate_all_ties_and_missing_proxy() {
        assert_eq!(
            content_group(&[0.1, 0.2, 0.2], 0, true).unwrap(),
            ("content_advantage", vec![1, 2])
        );
        assert_eq!(
            content_group(&[0.2, 0.1, 0.1], 0, true).unwrap(),
            ("content_disadvantage", vec![1, 2])
        );
        assert_eq!(
            content_group(&[0.1, 0.1, 0.2], 0, true).unwrap().0,
            "content_tie"
        );
        assert_eq!(
            content_group(&[1.0, 1.0, 1.0], 0, true).unwrap().0,
            "content_uninformative"
        );
        assert_eq!(
            content_group(&[1.0, 1.0, 1.0], 0, false).unwrap(),
            ("content_unavailable", vec![])
        );
    }

    #[test]
    fn macro_author_weighting_and_missing_concordance_support() {
        let mut authors = BTreeMap::new();
        let mut first = AuthorSum::default();
        first.add(Measures {
            top1: 1.0,
            nearest_content_impostor_concordance: Some(1.0),
            ..Measures::default()
        });
        first.add(Measures {
            top1: 1.0,
            ..Measures::default()
        });
        let mut second = AuthorSum::default();
        second.add(Measures::default());
        authors.insert("a".into(), first);
        authors.insert("b".into(), second);
        let summary = summarize(&authors);
        assert_eq!(summary["macro_author"]["top1"], 0.5);
        assert_eq!(
            summary["macro_author"]["nearest_content_impostor_concordance"],
            1.0
        );
        assert_eq!(summary["query_count"], 3);
        assert_eq!(summary["concordance_author_count"], 1);
        assert_eq!(summary["concordance_query_count"], 1);
    }

    #[test]
    fn altered_file_binding_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("fixture.json"), b"{}").unwrap();
        let binding = Binding {
            path: "fixture.json".into(),
            sha256: digest(b"[]"),
        };
        assert!(read_bound(root.path(), &binding).is_err());
    }
}

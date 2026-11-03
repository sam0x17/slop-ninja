//! Descriptive removal of complete calibrated family terms from a frozen model.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};

#[path = "family_ablation_inputs.rs"]
mod inputs;
use inputs::{Artifact, Metric, TOLERANCE, hash};

const VARIANTS: [&str; 3] = ["full", "lexical_family_only", "grammar_family_only"];
const METRICS: [&str; 4] = [
    "top1",
    "top5",
    "mrr",
    "nearest_content_impostor_concordance",
];
const STRATA: [&str; 6] = [
    "overall",
    "content_advantage",
    "content_tie",
    "content_disadvantage",
    "content_uninformative",
    "content_unavailable",
];

fn field<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn bound(root: &Path, binding: &Value) -> Result<Vec<u8>> {
    let bytes = fs::read(root.join(field(binding, "path")?))?;
    ensure!(
        binding["sha256"] == hash(&bytes),
        "input hash changed: {}",
        field(binding, "path")?
    );
    Ok(bytes)
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    Ok(())
}
fn write_lines(path: &Path, rows: &[Value]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    for row in rows {
        serde_json::to_writer(&mut f, row)?;
        f.write_all(b"\n")?;
    }
    Ok(())
}
fn write_array(path: &Path, values: &[f64], shape: &[usize]) -> Result<Value> {
    ensure!(
        values.len() == shape.iter().product::<usize>() && values.iter().all(|v| v.is_finite()),
        "invalid output array"
    );
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("UTF-8 array name")?;
    let bytes: Vec<_> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    fs::write(path, &bytes)?;
    Ok(
        json!({"path":name,"shape":shape,"dtype":"float64-le","bytes":bytes.len(),"sha256":hash(&bytes)}),
    )
}

#[derive(Clone, Copy, Default, Serialize, Deserialize, Debug)]
struct Measures {
    top1: f64,
    top5: f64,
    mrr: f64,
    nearest_content_impostor_concordance: Option<f64>,
}
impl Measures {
    fn values(self) -> [Option<f64>; 4] {
        [
            Some(self.top1),
            Some(self.top5),
            Some(self.mrr),
            self.nearest_content_impostor_concordance,
        ]
    }
    fn mean(seeds: &[Self]) -> Result<Self> {
        ensure!(!seeds.is_empty(), "empty seed metrics");
        ensure!(
            seeds
                .iter()
                .all(|s| s.nearest_content_impostor_concordance.is_some()
                    == seeds[0].nearest_content_impostor_concordance.is_some()),
            "seed support differs"
        );
        let n = seeds.len() as f64;
        Ok(Self {
            top1: seeds.iter().map(|s| s.top1 / n).sum(),
            top5: seeds.iter().map(|s| s.top5 / n).sum(),
            mrr: seeds.iter().map(|s| s.mrr / n).sum(),
            nearest_content_impostor_concordance: seeds[0]
                .nearest_content_impostor_concordance
                .map(|_| {
                    seeds
                        .iter()
                        .map(|s| s.nearest_content_impostor_concordance.unwrap() / n)
                        .sum()
                }),
        })
    }
}
fn measures(scores: &[f64], own: usize, impostors: &[usize]) -> Result<Measures> {
    ensure!(
        own < scores.len() && scores.iter().all(|x| x.is_finite()),
        "invalid ranking scores"
    );
    ensure!(
        impostors.iter().all(|i| *i < scores.len() && *i != own)
            && impostors.iter().collect::<BTreeSet<_>>().len() == impostors.len(),
        "invalid or duplicated impostors"
    );
    let mine = scores[own];
    let better = scores.iter().filter(|x| **x > mine).count();
    let tied = scores.iter().filter(|x| **x == mine).count();
    let top = |k: usize| k.saturating_sub(better).min(tied) as f64 / tied as f64;
    let mrr = ((better + 1)..=(better + tied))
        .map(|r| 1.0 / r as f64)
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
        top1: top(1),
        top5: top(5),
        mrr,
        nearest_content_impostor_concordance: concordance,
    })
}

/// Return signed family contributions and block logits with the original temperature.
fn family_terms(artifact: &Artifact, distances: &[f64]) -> Result<(Vec<f64>, [f64; 2])> {
    ensure!(
        distances.len() == artifact.families.len(),
        "family distance width differs"
    );
    let mut terms = Vec::new();
    let mut lexical = 0.0;
    let mut grammar = 0.0;
    for (i, distance) in distances.iter().enumerate() {
        ensure!(
            distance.is_finite() && *distance >= 0.0,
            "invalid adjusted distance"
        );
        let interval = artifact.knots.partition_point(|k| *k < *distance);
        let mut area = 0.0;
        let mut left = 0.0;
        for j in 0..interval {
            area += artifact.slopes[j] * (artifact.knots[j] - left);
            left = artifact.knots[j];
        }
        let calibrated = area + artifact.slopes[interval] * (*distance - left);
        let weighted = calibrated * artifact.family_weights[i];
        if ["word", "word_bigram"].contains(&artifact.families[i].name.as_str()) {
            lexical += weighted;
        } else {
            grammar += weighted;
        }
        terms.push(-weighted / artifact.temperature);
    }
    ensure!(
        terms.iter().all(|x| x.is_finite() && *x <= 0.0),
        "invalid signed family term"
    );
    Ok((
        terms,
        [
            -lexical / artifact.temperature,
            -grammar / artifact.temperature,
        ],
    ))
}

#[derive(Default)]
struct Acc {
    sums: [f64; 4],
    counts: [usize; 4],
    queries: usize,
}
impl Acc {
    fn add(&mut self, m: Measures) {
        self.queries += 1;
        for (i, v) in m.values().iter().enumerate() {
            if let Some(v) = v {
                self.sums[i] += v;
                self.counts[i] += 1;
            }
        }
    }
    fn means(&self) -> [Option<f64>; 4] {
        std::array::from_fn(|i| {
            if self.counts[i] > 0 {
                Some(self.sums[i] / self.counts[i] as f64)
            } else {
                None
            }
        })
    }
}
type Authors = BTreeMap<String, Acc>;
type Groups = BTreeMap<(String, String, String), Authors>;
fn value_map(values: [Option<f64>; 4]) -> Value {
    json!(METRICS.into_iter().zip(values).collect::<BTreeMap<_, _>>())
}
fn counts_map(counts: [usize; 4]) -> Value {
    json!(METRICS.into_iter().zip(counts).collect::<BTreeMap<_, _>>())
}
fn add(
    groups: &mut Groups,
    variant: &str,
    stratum: &str,
    gallery: &str,
    author: &str,
    m: Measures,
) {
    for s in [stratum, "overall"] {
        for g in [gallery, "pooled"] {
            groups
                .entry((variant.into(), s.into(), g.into()))
                .or_default()
                .entry(author.into())
                .or_default()
                .add(m);
        }
    }
}
fn summarize(authors: &Authors) -> Value {
    let mut sums = [0.0; 4];
    let mut counts = [0usize; 4];
    let mut queries = [0usize; 4];
    let mut micro = [0.0; 4];
    for a in authors.values() {
        for (i, v) in a.means().iter().enumerate() {
            if let Some(v) = v {
                sums[i] += v;
                counts[i] += 1;
                queries[i] += a.counts[i];
                micro[i] += a.sums[i];
            }
        }
    }
    let mean = std::array::from_fn(|i| {
        if counts[i] > 0 {
            Some(sums[i] / counts[i] as f64)
        } else {
            None
        }
    });
    let micro = std::array::from_fn(|i| {
        if queries[i] > 0 {
            Some(micro[i] / queries[i] as f64)
        } else {
            None
        }
    });
    let support: BTreeMap<_, _> = METRICS
        .iter()
        .enumerate()
        .map(|(i, n)| {
            (
                *n,
                json!({"author_count":counts[i],"query_count":queries[i]}),
            )
        })
        .collect();
    json!({"author_count":authors.len(),"query_count":authors.values().map(|a|a.queries).sum::<usize>(),"macro_author":value_map(mean),"micro":value_map(micro),"metric_support":support})
}
fn paired(left: &Authors, right: &Authors) -> Result<(Value, Vec<Value>)> {
    ensure!(left.keys().eq(right.keys()), "paired authors differ");
    let mut sums = [0.0; 4];
    let mut counts = [0usize; 4];
    let mut queries = [0usize; 4];
    let mut positive = [0usize; 4];
    let mut zero = [0usize; 4];
    let mut negative = [0usize; 4];
    let mut rows = Vec::new();
    for (author, a) in left {
        let b = &right[author];
        ensure!(
            a.counts == b.counts && a.queries == b.queries,
            "paired metric support differs"
        );
        let am = a.means();
        let bm = b.means();
        let mut differences = [None; 4];
        for i in 0..4 {
            if let (Some(a), Some(b)) = (am[i], bm[i]) {
                let d = a - b;
                differences[i] = Some(d);
                sums[i] += d;
                counts[i] += 1;
                queries[i] += left[author].counts[i];
                if d > 0.0 {
                    positive[i] += 1;
                } else if d == 0.0 {
                    zero[i] += 1;
                } else {
                    negative[i] += 1;
                }
            }
        }
        rows.push(json!({"author_id":author,"difference":value_map(differences),"metric_query_counts":counts_map(a.counts)}));
    }
    let means = std::array::from_fn(|i| {
        if counts[i] > 0 {
            Some(sums[i] / counts[i] as f64)
        } else {
            None
        }
    });
    let support:BTreeMap<_,_>=METRICS.iter().enumerate().map(|(i,m)|(*m,json!({"author_count":counts[i],"query_count":queries[i],"positive_authors":positive[i],"zero_authors":zero[i],"negative_authors":negative[i]}))).collect();
    Ok((
        json!({"mean_difference":value_map(means),"metric_support":support}),
        rows,
    ))
}
fn close(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.is_finite() && b.is_finite() && (a - b).abs() <= 1e-12,
        _ => false,
    }
}
fn verify_full_summary(authors: &Authors, previous: &Value) -> Result<()> {
    let actual = summarize(authors);
    ensure!(
        actual["author_count"] == previous["author_count"]
            && actual["query_count"] == previous["query_count"],
        "previous full summary support differs"
    );
    for key in ["macro_author", "micro"] {
        for metric in &METRICS[..3] {
            ensure!(
                close(actual[key][metric].as_f64(), previous[key][metric].as_f64()),
                "previous full aggregate differs: {key}/{metric}"
            );
        }
    }
    ensure!(
        previous["per_author"]
            .as_object()
            .context("previous authors")?
            .len()
            == authors.len(),
        "previous author catalog differs"
    );
    for (author, acc) in authors {
        for (i, metric) in METRICS[..3].iter().enumerate() {
            ensure!(
                close(
                    acc.means()[i],
                    previous["per_author"][author][metric].as_f64()
                ),
                "previous full author metric differs"
            );
        }
    }
    Ok(())
}

pub fn run(protocol_path: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "choose a fresh output directory");
    let protocol_bytes = fs::read(protocol_path)?;
    let protocol: Value = serde_json::from_slice(&protocol_bytes)?;
    ensure!(
        protocol["schema"] == "slopninja-full-family-ablation-protocol-v1"
            && protocol["analysis"] == "descriptive_after_model_fit_and_test"
            && protocol["training_or_selection_allowed"] == false,
        "unsupported ablation protocol"
    );
    ensure!(
        protocol["inherited_seeds"] == json!([0, 17, 29])
            && protocol["strata"] == json!(STRATA)
            && protocol["metrics"] == json!(METRICS)
            && protocol["lexical_families"] == json!(["word", "word_bigram"]),
        "ablation design differs"
    );
    ensure!(
        protocol["variants"]
            .as_object()
            .context("variants")?
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == VARIANTS.into_iter().collect(),
        "ablation variants differ"
    );
    ensure!(
        protocol["subset"] == "all_m2"
            && protocol["coordinate_count"] == 3557
            && protocol["maximal_coordinate_count"] == 7353,
        "coordinate policy differs"
    );
    let root = protocol_path.parent().context("protocol parent")?;
    let report: Value = serde_json::from_slice(&bound(root, &protocol["source_report"])?)?;
    ensure!(
        report["schema"] == "slopninja-training-author-scale-test-result-v1"
            && report["selection_sha256"] == protocol["source_selection_sha256"],
        "source report identity differs"
    );
    let model_manifest: Value = serde_json::from_slice(&bound(root, &protocol["model_manifest"])?)?;
    ensure!(
        model_manifest["selection_sha256"] == protocol["source_selection_sha256"],
        "model selection differs"
    );
    let mut models = BTreeMap::new();
    for binding in protocol["models"].as_array().context("models")? {
        let seed = binding["inherited_seed"].as_u64().context("model seed")?;
        let artifact: Artifact = serde_json::from_slice(&bound(root, binding)?)?;
        ensure!(
            artifact
                .provenance_sha256
                .get("selection")
                .map(String::as_str)
                == protocol["source_selection_sha256"].as_str()
                && artifact
                    .provenance_sha256
                    .get("catalog")
                    .map(String::as_str)
                    == protocol["catalog_sha256"].as_str()
                && artifact
                    .provenance_sha256
                    .get("fixed_subset_indices")
                    .map(String::as_str)
                    == protocol["fixed_subset_indices_sha256"].as_str(),
            "portable source bindings differ"
        );
        let rows = model_manifest["models"]
            .as_array()
            .context("model manifest")?;
        let matched: Vec<_> = rows
            .iter()
            .filter(|r| r["inherited_seed"].as_u64() == Some(seed))
            .collect();
        ensure!(
            matched.len() == 1
                && matched[0]["sha256"] == binding["sha256"]
                && matched[0]["selected_epoch"] == binding["selected_epoch"],
            "model manifest binding differs"
        );
        ensure!(
            models.insert(seed, Metric::new(artifact)?).is_none(),
            "duplicate model seed"
        );
    }
    ensure!(
        models.keys().copied().collect::<Vec<_>>() == [0, 17, 29],
        "missing seed"
    );
    let content_bytes = bound(root, &protocol["content_queries"])?;
    let content_report_path = root
        .join(field(&protocol["content_queries"], "path")?)
        .parent()
        .context("content query parent")?
        .join("report.json");
    let content_report_bytes = fs::read(content_report_path)?;
    ensure!(
        protocol["content_report_sha256"] == hash(&content_report_bytes),
        "preceding content report changed"
    );
    let content_report: Value = serde_json::from_slice(&content_report_bytes)?;
    ensure!(
        content_report["queries_sha256"] == hash(&content_bytes)
            && content_report["source_report_sha256"] == protocol["source_report"]["sha256"],
        "preceding content report inputs differ"
    );
    let mut content = BTreeMap::new();
    for line in std::str::from_utf8(&content_bytes)?.lines() {
        let row: Value = serde_json::from_str(line)?;
        let key = (
            field(&row, "gallery")?.to_owned(),
            field(&row, "query_id")?.to_owned(),
        );
        ensure!(
            content.insert(key, row).is_none(),
            "duplicate content query"
        );
    }
    let galleries = protocol["galleries"].as_array().context("galleries")?;
    ensure!(
        galleries.len() == 6
            && galleries
                .iter()
                .map(|g| field(g, "name"))
                .collect::<Result<BTreeSet<_>>>()?
                .len()
                == 6,
        "gallery design differs"
    );
    fs::create_dir(out)?;
    fs::write(out.join("protocol.json"), &protocol_bytes)?;
    let mut groups = Groups::new();
    let mut query_rows = Vec::new();
    let mut runs = Vec::new();
    let mut all_authors = BTreeSet::new();
    let mut all_queries = BTreeSet::new();
    let mut replay_error = 0.0_f64;
    let mut decomposition_error = 0.0_f64;
    let mut full_pairs = 0usize;
    for gallery in galleries {
        let name = field(gallery, "name")?;
        for v in VARIANTS {
            for s in STRATA {
                for g in [name, "pooled"] {
                    groups.entry((v.into(), s.into(), g.into())).or_default();
                }
            }
        }
        let mut seed_metrics: BTreeMap<u64, Vec<[Measures; 3]>> = BTreeMap::new();
        let mut query_metadata = Vec::new();
        for (seed, metric) in &models {
            let data = inputs::load(
                metric,
                &root.join(field(&gallery["export"], "path")?),
                field(&gallery["export"], "manifest_sha256")?,
            )?;
            let (q, a, k, f) = (
                data.queries.len(),
                data.authors.len(),
                metric.artifact().coordinates.len(),
                metric.artifact().families.len(),
            );
            if *seed == 0 {
                for author in &data.authors {
                    ensure!(
                        all_authors.insert(author.as_str().context("author")?.to_owned()),
                        "reused author across galleries"
                    );
                }
            }
            let mut impostor_indices = Vec::new();
            for query in &data.queries {
                let id = field(query, "id")?;
                let row = content
                    .get(&(name.to_owned(), id.to_owned()))
                    .context("missing frozen content query")?;
                ensure!(
                    row["author_id"] == query["author"],
                    "content author differs"
                );
                let stratum = field(row, "stratum")?;
                ensure!(STRATA[1..].contains(&stratum), "content stratum differs");
                let ids: Vec<String> =
                    serde_json::from_value(row["nearest_content_impostor_ids"].clone())?;
                ensure!(
                    ids.iter().collect::<BTreeSet<_>>().len() == ids.len()
                        && (stratum == "content_unavailable") == ids.is_empty(),
                    "invalid frozen impostor support"
                );
                let indices = ids
                    .iter()
                    .map(|id| {
                        data.authors
                            .iter()
                            .position(|a| a.as_str() == Some(id))
                            .context("content impostor absent")
                    })
                    .collect::<Result<Vec<_>>>()?;
                impostor_indices.push(indices);
                if *seed == 0 {
                    ensure!(
                        all_queries.insert(id.to_owned()),
                        "reused query across galleries"
                    );
                    query_metadata.push(row.clone());
                }
            }
            let mut logits: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::with_capacity(q * a));
            let mut terms = Vec::with_capacity(q * a * f);
            let mut local_decomposition = 0.0_f64;
            for i in 0..q {
                for j in 0..a {
                    let score = metric.score_pair(
                        &data.raw[(i * a + j) * f..(i * a + j + 1) * f],
                        &data.query[i * k..(i + 1) * k],
                        &data.target[j * k..(j + 1) * k],
                    )?;
                    let (contributions, blocks) =
                        family_terms(metric.artifact(), &score.adjusted_family_distances)?;
                    local_decomposition = local_decomposition
                        .max((score.logit - blocks.iter().sum::<f64>()).abs())
                        .max((score.logit - contributions.iter().sum::<f64>()).abs());
                    logits[0].push(score.logit);
                    logits[1].push(blocks[0]);
                    logits[2].push(blocks[1]);
                    terms.extend(contributions);
                }
            }
            ensure!(
                local_decomposition <= TOLERANCE,
                "full family decomposition changed score"
            );
            decomposition_error = decomposition_error.max(local_decomposition);
            let references = gallery["full_reference_scores"]
                .as_array()
                .context("reference scores")?;
            ensure!(references.len() == 3, "reference seed support differs");
            let matched: Vec<_> = references
                .iter()
                .filter(|r| r["inherited_seed"].as_u64() == Some(*seed))
                .collect();
            ensure!(matched.len() == 1, "reference seed missing or duplicated");
            let reference = matched[0];
            let previous: Value = serde_json::from_slice(&bound(root, reference)?)?;
            let saved_seed = report["models"]["treatment_300"]["seeds"]
                .as_array()
                .context("report seeds")?
                .iter()
                .find(|s| s["inherited_seed"].as_u64() == Some(*seed))
                .context("report seed absent")?;
            ensure!(
                saved_seed["galleries"][name]["scores_sha256"] == reference["sha256"],
                "reference scores not bound to source report"
            );
            let error =
                inputs::compare_reference(&logits[0], &data.queries, &data.authors, &previous)?;
            replay_error = replay_error.max(error);
            full_pairs += q * a;
            let mut rows = Vec::new();
            for (i, impostors) in impostor_indices.iter().enumerate() {
                let values: Vec<_> = logits
                    .iter()
                    .map(|v| measures(&v[i * a..(i + 1) * a], data.own[i], impostors))
                    .collect::<Result<_>>()?;
                let saved = previous["per_query_metrics"][i]
                    .as_array()
                    .context("reference query metrics")?;
                ensure!(saved.len() == 3, "reference metric width");
                for (actual, saved) in values[0].values()[..3].iter().zip(saved) {
                    ensure!(
                        close(*actual, saved.as_f64()),
                        "full per-query retrieval metric differs"
                    );
                }
                rows.push(
                    values
                        .try_into()
                        .map_err(|_| anyhow::anyhow!("variant count"))?,
                );
            }
            seed_metrics.insert(*seed, rows);
            let directory_name = format!("seed-{seed}-{name}");
            let directory = out.join(&directory_name);
            fs::create_dir(&directory)?;
            let mut arrays = BTreeMap::new();
            arrays.insert(
                "family_contributions",
                write_array(
                    &directory.join("family-contributions.f64"),
                    &terms,
                    &[q, a, f],
                )?,
            );
            for (v, values) in VARIANTS.iter().zip(&logits) {
                arrays.insert(
                    v,
                    write_array(&directory.join(format!("{v}-logits.f64")), values, &[q, a])?,
                );
            }
            let manifest = json!({"schema":"slopninja-full-family-ablation-scores-v1","seed":seed,"gallery":name,"query_count":q,"authors":data.authors,"query_ids":data.queries.iter().map(|r|&r["id"]).collect::<Vec<_>>(),"families":metric.artifact().families.iter().map(|f|&f.name).collect::<Vec<_>>(),"model_sha256":protocol["models"].as_array().unwrap().iter().find(|m|m["inherited_seed"].as_u64()==Some(*seed)).unwrap()["sha256"],"export_manifest_sha256":gallery["export"]["manifest_sha256"],"catalog_sha256":data.manifest["catalog_sha256"],"fixed_subset_indices_sha256":protocol["fixed_subset_indices_sha256"],"reference_scores_sha256":reference["sha256"],"arrays":arrays,"maximum_full_replay_logit_error":error,"maximum_decomposition_error":local_decomposition,"all_full_rankings_and_ties_identical":true,"contribution_definition":"-(original_family_weight * shared_spline(adjusted_complete_family_distance)) / original_temperature"});
            write_json(&directory.join("manifest.json"), &manifest)?;
            runs.push(json!({"seed":seed,"gallery":name,"path":directory_name,"manifest_sha256":hash(&fs::read(directory.join("manifest.json"))?),"query_count":q,"candidate_count":a}));
            println!(
                "Scored seed {seed}, {name}: {q} queries, full replay and family decomposition passed"
            );
        }
        for (i, row) in query_metadata.iter().enumerate() {
            let mut by_seed = BTreeMap::new();
            let mut means = BTreeMap::new();
            for (seed, rows) in &seed_metrics {
                by_seed.insert(
                    seed.to_string(),
                    VARIANTS
                        .iter()
                        .zip(rows[i])
                        .map(|(v, m)| (*v, m))
                        .collect::<BTreeMap<_, _>>(),
                );
            }
            for (j, variant) in VARIANTS.iter().enumerate() {
                let mean =
                    Measures::mean(&seed_metrics.values().map(|r| r[i][j]).collect::<Vec<_>>())?;
                if *variant == "full" {
                    let previous: Measures =
                        serde_json::from_value(row["metrics"]["treatment_300"].clone())?;
                    for (actual, old) in mean.values().iter().zip(previous.values()) {
                        ensure!(
                            close(*actual, old),
                            "full seed-mean content diagnostic metric differs"
                        );
                    }
                }
                add(
                    &mut groups,
                    variant,
                    field(row, "stratum")?,
                    name,
                    field(row, "author_id")?,
                    mean,
                );
                means.insert(*variant, mean);
            }
            query_rows.push(json!({"gallery":name,"query_id":row["query_id"],"author_id":row["author_id"],"stratum":row["stratum"],"own_content_distance":row["own_content_distance"],"nearest_content_impostor_ids":row["nearest_content_impostor_ids"],"metrics_by_seed":by_seed,"seed_mean_metrics":means}));
        }
        verify_full_summary(
            &groups[&("full".into(), "overall".into(), name.into())],
            &report["models"]["treatment_300"]["galleries"][name],
        )?;
    }
    ensure!(
        all_queries.len() == content.len(),
        "unused frozen content query"
    );
    verify_full_summary(
        &groups[&("full".into(), "overall".into(), "pooled".into())],
        &report["models"]["treatment_300"]["seed_mean"],
    )?;
    for ((variant, stratum, gallery), authors) in &groups {
        if variant != "full" {
            continue;
        }
        let matches: Vec<_> = content_report["summaries"]
            .as_array()
            .context("previous content summaries")?
            .iter()
            .filter(|s| {
                s["model"] == "treatment_300"
                    && s["stratum"] == *stratum
                    && s["gallery"] == *gallery
            })
            .collect();
        if authors.is_empty() {
            ensure!(
                matches.is_empty(),
                "previous content stratum support differs"
            );
            continue;
        }
        ensure!(
            matches.len() == 1,
            "previous content stratum missing or duplicated"
        );
        let old = matches[0];
        let new = summarize(authors);
        ensure!(
            new["author_count"] == old["author_count"]
                && new["query_count"] == old["query_count"]
                && new["metric_support"]["nearest_content_impostor_concordance"]["author_count"]
                    == old["concordance_author_count"]
                && new["metric_support"]["nearest_content_impostor_concordance"]["query_count"]
                    == old["concordance_query_count"],
            "previous content stratum support differs"
        );
        for metric in METRICS {
            ensure!(
                close(
                    new["macro_author"][metric].as_f64(),
                    old["macro_author"][metric].as_f64()
                ),
                "previous full content-stratum metric differs"
            );
        }
    }
    let mut summaries = Vec::new();
    let mut author_rows = Vec::new();
    let mut comparisons = Vec::new();
    let mut paired_rows = Vec::new();
    for ((variant, stratum, gallery), authors) in &groups {
        let mut summary = summarize(authors);
        summary["variant"] = json!(variant);
        summary["stratum"] = json!(stratum);
        summary["gallery"] = json!(gallery);
        summaries.push(summary);
        for (author, a) in authors {
            author_rows.push(json!({"variant":variant,"stratum":stratum,"gallery":gallery,"author_id":author,"metrics":value_map(a.means()),"metric_query_counts":counts_map(a.counts)}));
        }
        if variant == "full" {
            for (name, right) in [
                ("full_minus_lexical", "lexical_family_only"),
                ("full_minus_grammar", "grammar_family_only"),
            ] {
                let (mut summary, rows) = paired(
                    authors,
                    &groups[&(right.into(), stratum.clone(), gallery.clone())],
                )?;
                summary["comparison"] = json!(name);
                summary["stratum"] = json!(stratum);
                summary["gallery"] = json!(gallery);
                comparisons.push(summary);
                for mut row in rows {
                    row["comparison"] = json!(name);
                    row["stratum"] = json!(stratum);
                    row["gallery"] = json!(gallery);
                    paired_rows.push(row);
                }
            }
        }
    }
    write_lines(&out.join("queries.jsonl"), &query_rows)?;
    write_lines(&out.join("authors.jsonl"), &author_rows)?;
    write_lines(&out.join("paired-authors.jsonl"), &paired_rows)?;
    let artifacts: BTreeMap<_, _> = ["queries.jsonl", "authors.jsonl", "paired-authors.jsonl"]
        .into_iter()
        .map(|n| Ok((n, hash(&fs::read(out.join(n))?))))
        .collect::<Result<_>>()?;
    let result = json!({"schema":"slopninja-full-family-ablation-result-v1","analysis":"descriptive_after_model_fit_and_test","protocol_sha256":hash(&protocol_bytes),"source_report_sha256":protocol["source_report"]["sha256"],"content_queries_sha256":protocol["content_queries"]["sha256"],"query_count":all_queries.len(),"author_count":all_authors.len(),"runs":runs,"summaries":summaries,"comparisons":comparisons,"artifacts_sha256":artifacts,"correctness":{"compared_full_scores":full_pairs,"compared_full_rankings":query_rows.len()*models.len(),"all_full_rankings_and_ties_identical":true,"maximum_full_replay_logit_error":replay_error,"maximum_decomposition_error":decomposition_error,"absolute_tolerance":TOLERANCE,"full_query_content_and_author_aggregates_reproduced":true},"source_sha256":{"family_ablation.rs":hash(include_bytes!("family_ablation.rs")),"family_ablation_inputs.rs":hash(include_bytes!("family_ablation_inputs.rs")),"scorer.rs":hash(include_bytes!("bin/slop_ninja-ablate-families.rs")),"coordinate_inference.rs":hash(include_bytes!("coordinate_inference.rs")),"copied_loader_origin.rs":hash(include_bytes!("coordinate_subset_replay.rs")),"grammar_eval_lib.rs":hash(include_bytes!("lib.rs"))},"executed_binary_sha256":hash(&fs::read(std::env::current_exe()?)?),"training_calls":0,"external_model_calls":0,"detector_calls":0,"confidence_intervals":false});
    write_json(&out.join("report.json"), &result)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::space::Kind;
    use inputs::coordinate_inference::{Coordinate, FamilyGeometry};

    #[test]
    fn array_manifest_path_is_a_relative_utf8_string() {
        let root = tempfile::tempdir().unwrap();
        let manifest = write_array(
            &root.path().join("family-contributions.f64"),
            &[-0.5, -1.0],
            &[1, 2],
        )
        .unwrap();
        assert_eq!(manifest["path"].as_str(), Some("family-contributions.f64"));
        let file = root.path().join(manifest["path"].as_str().unwrap());
        assert_eq!(manifest["sha256"], hash(&fs::read(file).unwrap()));
        assert_eq!(manifest["shape"], json!([1, 2]));
    }
    fn metric() -> Metric {
        let families = ["grammar", "word"]
            .into_iter()
            .map(|n| FamilyGeometry {
                name: n.into(),
                kind: Kind::Distribution,
                original_axis_count: 2,
                numerical_axes: vec![],
            })
            .collect();
        let coordinates = ["grammar", "word"]
            .into_iter()
            .enumerate()
            .map(|(i, n)| Coordinate {
                family_index: i,
                family: n.into(),
                feature: "selected".into(),
                kind: Kind::Distribution,
                scale: 1.0,
                original_family_axis_count: 2,
                log_multiplier: 2.0_f64.ln(),
                multiplier: 2.0,
            })
            .collect();
        let provenance = [
            "catalog",
            "space",
            "selection",
            "protocol",
            "coordinate_checkpoint",
            "baseline_checkpoint",
        ]
        .into_iter()
        .map(|n| (n.into(), "0".repeat(64)))
        .collect();
        Metric::new(Artifact {
            schema: inputs::coordinate_inference::SCHEMA.into(),
            source_space_id: "fixture".into(),
            feature_schema: "fixture".into(),
            parser_identity: "fixture".into(),
            families,
            coordinates,
            family_weights: vec![0.3, 0.7],
            knots: vec![0.5, 1.0],
            slopes: vec![1.0, 2.0, 0.5],
            temperature: 0.25,
            provenance_sha256: provenance,
        })
        .unwrap()
    }
    #[test]
    fn complete_family_removal_preserves_original_weights_and_differs_from_eta_reset() {
        let m = metric();
        let raw = [0.4, 0.7];
        let q = [0.2, 0.3];
        let p = [0.0, 0.0];
        let score = m.score_pair(&raw, &q, &p).unwrap();
        let (terms, blocks) = family_terms(m.artifact(), &score.adjusted_family_distances).unwrap();
        assert!((score.logit - terms.iter().sum::<f64>()).abs() < 1e-12);
        assert!((score.logit - blocks.iter().sum::<f64>()).abs() < 1e-12);
        assert!((blocks[0] - terms[1]).abs() < 1e-12);
        assert!((blocks[1] - terms[0]).abs() < 1e-12);
        let mut reset = m.artifact().clone();
        reset.coordinates[0].log_multiplier = 0.0;
        reset.coordinates[0].multiplier = 1.0;
        let reset = Metric::new(reset)
            .unwrap()
            .score_pair(&raw, &q, &p)
            .unwrap();
        assert!(reset.logit < blocks[0]);
        assert!((blocks[0] - (-(0.5 + 2.0 * (0.79 - 0.5)) * 0.7 / 0.25)).abs() < 1e-12);
    }
    #[test]
    fn expected_ties_and_all_nearest_impostors_have_exact_support() {
        let m = measures(&[2.0, 2.0, 1.0], 0, &[1, 2]).unwrap();
        assert_eq!(m.top1, 0.5);
        assert_eq!(m.mrr, 0.75);
        assert_eq!(m.nearest_content_impostor_concordance, Some(0.75));
        assert!(measures(&[1.0, 2.0], 0, &[1, 1]).is_err());
        assert!(measures(&[1.0, 2.0], 0, &[0]).is_err());
        assert_eq!(
            measures(&[1.0, 2.0], 0, &[])
                .unwrap()
                .nearest_content_impostor_concordance,
            None
        );
    }
    #[test]
    fn average_seed_metrics_before_ranking_or_author_aggregation() {
        let a = measures(&[100.0, 0.0], 0, &[1]).unwrap();
        let b = measures(&[0.0, 1.0], 0, &[1]).unwrap();
        assert_eq!(Measures::mean(&[a, b]).unwrap().top1, 0.5);
        assert_eq!(measures(&[50.0, 0.5], 0, &[1]).unwrap().top1, 1.0);
    }
    #[test]
    fn authors_and_paired_deltas_use_metric_specific_support() {
        let mut left = Authors::new();
        let mut a = Acc::default();
        a.add(Measures {
            top1: 1.0,
            nearest_content_impostor_concordance: Some(1.0),
            ..Measures::default()
        });
        a.add(Measures {
            top1: 1.0,
            ..Measures::default()
        });
        left.insert("a".into(), a);
        let mut b = Acc::default();
        b.add(Measures::default());
        left.insert("b".into(), b);
        let summary = summarize(&left);
        assert_eq!(summary["macro_author"]["top1"], 0.5);
        assert_eq!(summary["metric_support"]["top1"]["query_count"], 3);
        assert_eq!(
            summary["metric_support"]["nearest_content_impostor_concordance"]["query_count"],
            1
        );
        let (zero, rows) = paired(&left, &left).unwrap();
        assert_eq!(zero["metric_support"]["top1"]["zero_authors"], 2);
        assert_eq!(
            zero["metric_support"]["nearest_content_impostor_concordance"]["zero_authors"],
            1
        );
        assert_eq!(rows.len(), 2);
        let empty = summarize(&Authors::new());
        assert!(empty["macro_author"]["top1"].is_null());
        assert_eq!(empty["metric_support"]["top1"]["query_count"], 0);
    }
    #[test]
    fn altered_input_binding_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a.json"), b"{}").unwrap();
        assert!(bound(root.path(), &json!({"path":"a.json","sha256":hash(b"[]")})).is_err());
    }

    #[test]
    fn copied_full_replay_rejects_changed_reference_ties_and_query_order() {
        let queries = vec![json!({"id":"q"})];
        let authors = vec![json!("a"), json!("b")];
        let reference = json!({"query_ids":["q"],"candidate_authors":authors,"logits":[[1.0,1.0]]});
        assert_eq!(
            inputs::compare_reference(&[1.0, 1.0], &queries, &authors, &reference).unwrap(),
            0.0
        );
        assert!(
            inputs::compare_reference(&[1.0, 1.0 - 1e-12], &queries, &authors, &reference).is_err()
        );
        let mut changed = reference;
        changed["query_ids"] = json!(["other"]);
        assert!(inputs::compare_reference(&[1.0, 1.0], &queries, &authors, &changed).is_err());
    }

    #[test]
    fn paired_direction_counts_and_empty_support_are_explicit() {
        let mut left = Authors::new();
        let mut right = Authors::new();
        for (name, a, b) in [("a", 1.0, 0.0), ("b", 0.0, 1.0), ("c", 1.0, 1.0)] {
            let mut l = Acc::default();
            l.add(Measures {
                top1: a,
                ..Measures::default()
            });
            left.insert(name.into(), l);
            let mut r = Acc::default();
            r.add(Measures {
                top1: b,
                ..Measures::default()
            });
            right.insert(name.into(), r);
        }
        let (summary, _) = paired(&left, &right).unwrap();
        assert_eq!(summary["mean_difference"]["top1"], 0.0);
        for key in ["positive_authors", "zero_authors", "negative_authors"] {
            assert_eq!(summary["metric_support"]["top1"][key], 1);
        }
        assert!(summary["mean_difference"]["nearest_content_impostor_concordance"].is_null());
        let (empty, rows) = paired(&Authors::new(), &Authors::new()).unwrap();
        assert!(rows.is_empty());
        assert!(empty["mean_difference"]["top1"].is_null());
        assert_eq!(empty["metric_support"]["top1"]["author_count"], 0);
    }
}

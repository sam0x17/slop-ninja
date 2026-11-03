//! Hash-bound TRAIN support audit. This does not fit or score an author model.
use crate::{function_word_roles, predicate_operators};
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::{features, syntax::Document};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(about = "Audit support for two new grammar families on frozen TRAIN annotations")]
struct Args {
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    expected_protocol_sha256: String,
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    corpus: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

#[derive(Deserialize)]
struct Protocol {
    schema: String,
    parser_identity: String,
    legacy_feature_schema: String,
    source_files: BTreeMap<String, String>,
    panels: Vec<Panel>,
    support: SupportThreshold,
    expected_posts: usize,
    expected_authors: usize,
}

#[derive(Deserialize, Serialize)]
struct Panel {
    name: String,
    export: String,
    manifest_sha256: String,
    posts: usize,
    authors: usize,
}

#[derive(Deserialize, Serialize)]
struct SupportThreshold {
    posts: usize,
    authors: usize,
    author_dates: usize,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
struct Post {
    author: String,
    date: String,
    id: String,
    source_group: String,
    split: String,
    text_sha256: String,
}

#[derive(Deserialize)]
struct Binding {
    annotation_sha256: String,
    feature_cache_sha256: String,
    post: Post,
}

#[derive(Deserialize)]
struct Audit {
    schema: String,
    loaded_splits: Vec<String>,
    loaded_post_count: usize,
    training_author_count: usize,
    training_post_count: usize,
    posts: Vec<Binding>,
}

#[derive(Default)]
struct KeySupport {
    occurrences: u64,
    posts: usize,
    authors: BTreeSet<String>,
    author_dates: BTreeSet<(String, String)>,
}

#[derive(Default)]
struct FamilySupport {
    opportunities: u64,
    unavailable_posts: usize,
    keys: BTreeMap<String, KeySupport>,
}

impl FamilySupport {
    fn add(&mut self, family: &features::Family, post: &Post) -> Result<()> {
        let features::Family::Distribution {
            counts,
            opportunities,
        } = family
        else {
            anyhow::bail!("New grammar family must be a distribution")
        };
        ensure!(
            counts.values().sum::<u64>() == *opportunities,
            "Count denominator differs"
        );
        ensure!(
            counts.values().all(|count| *count > 0),
            "Explicit zero categorical count"
        );
        self.opportunities += opportunities;
        self.unavailable_posts += usize::from(*opportunities == 0);
        for (key, count) in counts {
            let support = self.keys.entry(key.clone()).or_default();
            support.occurrences += count;
            support.posts += 1;
            support.authors.insert(post.author.clone());
            support
                .author_dates
                .insert((post.author.clone(), post.date.clone()));
        }
        Ok(())
    }

    fn report(&self, threshold: &SupportThreshold) -> Value {
        let supported: Vec<_> = self
            .keys
            .values()
            .filter(|key| {
                key.posts >= threshold.posts
                    && key.authors.len() >= threshold.authors
                    && key.author_dates.len() >= threshold.author_dates
            })
            .collect();
        let supported_occurrences: u64 = supported.iter().map(|key| key.occurrences).sum();
        let single_post_types = self.keys.values().filter(|key| key.posts == 1).count();
        let single_author_types = self
            .keys
            .values()
            .filter(|key| key.authors.len() == 1)
            .count();
        json!({
            "opportunities": self.opportunities,
            "unavailable_posts": self.unavailable_posts,
            "distinct_types": self.keys.len(),
            "single_post_types": single_post_types,
            "single_post_type_fraction": ratio(single_post_types as u64, self.keys.len() as u64),
            "single_author_types": single_author_types,
            "supported_types": supported.len(),
            "supported_occurrences": supported_occurrences,
            "supported_occurrence_fraction": ratio(supported_occurrences, self.opportunities),
        })
    }

    fn unseen_against(&self, reference: &Self) -> Value {
        let unseen: Vec<_> = self
            .keys
            .iter()
            .filter(|(key, _)| !reference.keys.contains_key(*key))
            .collect();
        let occurrences: u64 = unseen.iter().map(|(_, support)| support.occurrences).sum();
        json!({"unseen_types": unseen.len(), "unseen_occurrences": occurrences,
            "unseen_occurrence_fraction": ratio(occurrences, self.opportunities)})
    }
}

fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator != 0).then(|| numerator as f64 / denominator as f64)
}

#[derive(Default, Serialize)]
struct ObservationTotals {
    missing_function_lemmas: u64,
    missing_operator_lemmas: u64,
    operators: u64,
    heads_without_operators: u64,
    finite_none: u64,
    finite_one: u64,
    finite_multiple: u64,
    finite_carriers: u64,
    finite_morph_tag_disagreement: u64,
    finite_with_other_verb_forms: u64,
}

impl ObservationTotals {
    fn add(
        &mut self,
        missing_function_lemmas: u64,
        observations: &[predicate_operators::PredicateObservation],
    ) {
        use predicate_operators::{FiniteSignature, OperatorSequence};
        self.missing_function_lemmas += missing_function_lemmas;
        for observation in observations {
            match &observation.event.operators {
                OperatorSequence::None => self.heads_without_operators += 1,
                OperatorSequence::Present { items } => {
                    self.operators += items.len() as u64;
                    self.missing_operator_lemmas +=
                        items.iter().filter(|op| op.lemma.is_none()).count() as u64;
                }
            }
            let carriers = match &observation.event.finite {
                FiniteSignature::None => {
                    self.finite_none += 1;
                    &[][..]
                }
                FiniteSignature::One { carrier } => {
                    self.finite_one += 1;
                    std::slice::from_ref(carrier)
                }
                FiniteSignature::Multiple { carriers } => {
                    self.finite_multiple += 1;
                    carriers.as_slice()
                }
            };
            for carrier in carriers {
                self.finite_carriers += 1;
                self.finite_morph_tag_disagreement +=
                    u64::from(carrier.verb_form_fin != carrier.finite_tag);
                self.finite_with_other_verb_forms +=
                    u64::from(!carrier.other_verb_forms.is_empty());
            }
        }
    }
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn read_bound(path: &Path, expected: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("Read {}", path.display()))?;
    ensure!(
        hash(&bytes) == expected,
        "SHA256 differs: {}",
        path.display()
    );
    Ok(bytes)
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("Missing string {key}"))
}

fn write_fresh(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

pub fn main() -> Result<()> {
    run(Args::parse())
}

fn run(args: Args) -> Result<()> {
    let protocol_bytes = read_bound(&args.protocol, &args.expected_protocol_sha256)?;
    let protocol: Protocol = serde_json::from_slice(&protocol_bytes)?;
    ensure!(
        protocol.schema == "slopninja-grammar-choice-support-protocol-v1",
        "Wrong protocol schema"
    );
    ensure!(
        protocol.legacy_feature_schema == features::FEATURE_VERSION,
        "Wrong legacy schema"
    );
    ensure!(
        !protocol.panels.is_empty() && !protocol.source_files.is_empty(),
        "Empty input binding"
    );
    ensure!(
        protocol.support.posts > 0
            && protocol.support.authors > 0
            && protocol.support.author_dates > 0,
        "Invalid support threshold"
    );
    for (path, expected) in &protocol.source_files {
        read_bound(&args.repo.join(path), expected)?;
    }
    ensure!(!args.out.exists(), "Output directory must be new");
    fs::create_dir_all(&args.out)?;
    let mut per_post = BufWriter::new(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(args.out.join("posts.jsonl"))?,
    );
    let mut bindings_receipt = Vec::new();
    let mut all_authors = BTreeSet::new();
    let mut all_ids = BTreeSet::new();
    let mut all_texts = BTreeSet::new();
    let mut combined = BTreeMap::<String, FamilySupport>::new();
    let mut panel_supports = Vec::new();
    let mut panel_reports = Vec::new();
    let mut totals = ObservationTotals::default();
    let cache = args.corpus.join("vector-feature-cache-v1");
    for panel in &protocol.panels {
        let export = args.corpus.join(&panel.export);
        let manifest: Value = serde_json::from_slice(&read_bound(
            &export.join("manifest.json"),
            &panel.manifest_sha256,
        )?)?;
        ensure!(
            manifest["schema"] == "slopninja-named-family-tensor-v1"
                && manifest["split"] == "train",
            "Expected named TRAIN export"
        );
        ensure!(
            manifest["parser_identity"] == protocol.parser_identity,
            "Manifest parser differs"
        );
        let audit: Audit = serde_json::from_slice(&read_bound(
            &export.join("audit.json"),
            string(&manifest, "audit_sha256")?,
        )?)?;
        let queries: Vec<Post> = serde_json::from_slice(&read_bound(
            &export.join("queries.json"),
            string(&manifest, "queries_sha256")?,
        )?)?;
        ensure!(
            audit.schema == "slopninja-named-family-tensor-audit-v1"
                && audit.loaded_splits == ["train"],
            "Audit not TRAIN only"
        );
        ensure!(
            audit.posts.len() == panel.posts
                && audit.loaded_post_count == panel.posts
                && audit.training_post_count == panel.posts
                && queries.len() == panel.posts,
            "Panel post count differs"
        );
        ensure!(
            audit.training_author_count == panel.authors && manifest["query_count"] == panel.posts,
            "Panel author/query count differs"
        );
        let query_map: BTreeMap<_, _> = queries
            .iter()
            .map(|post| (post.id.as_str(), post))
            .collect();
        ensure!(query_map.len() == queries.len(), "Duplicate query ID");
        let mut support = BTreeMap::<String, FamilySupport>::new();
        let mut observations_total = ObservationTotals::default();
        let mut panel_authors = BTreeSet::new();
        for binding in audit.posts {
            let post = binding.post;
            ensure!(
                post.split == "train" && query_map.get(post.id.as_str()) == Some(&&post),
                "Post not a bound TRAIN query"
            );
            ensure!(
                all_ids.insert(post.id.clone()) && all_texts.insert(post.text_sha256.clone()),
                "Repeated query ID or text across panels"
            );
            panel_authors.insert(post.author.clone());
            ensure!(
                post.text_sha256.len() == 64
                    && post.text_sha256.bytes().all(|c| c.is_ascii_hexdigit()),
                "Invalid text SHA256"
            );
            let annotation_bytes = read_bound(
                &cache.join(format!("{}.annotation.json", post.text_sha256)),
                &binding.annotation_sha256,
            )?;
            let doc: Document = serde_json::from_slice(&annotation_bytes)?;
            ensure!(
                doc.parser_identity == protocol.parser_identity
                    && hash(doc.text.as_bytes()) == post.text_sha256,
                "Annotation provenance differs"
            );
            let feature_bytes = read_bound(
                &cache.join(format!("{}.features.json", post.text_sha256)),
                &binding.feature_cache_sha256,
            )?;
            let cached_value: Value = serde_json::from_slice(&feature_bytes)?;
            let cached: features::Features =
                serde_json::from_value(cached_value["features"].clone())?;
            let legacy = features::extract(&doc)?;
            let legacy_bytes = serde_json::to_vec(&legacy)?;
            ensure!(
                legacy.families.len() == 14 && legacy_bytes == serde_json::to_vec(&cached)?,
                "Legacy14 canonical bytes differ"
            );
            let roles = function_word_roles::extract(&doc)?;
            let operator_family = predicate_operators::document_family(&doc)?;
            let observations = predicate_operators::document_observations(&doc)?;
            let features::Family::Distribution {
                opportunities: old_heads,
                ..
            } = &legacy.families["clause_head_frame"]
            else {
                anyhow::bail!("Wrong legacy head family kind")
            };
            ensure!(
                *old_heads == observations.len() as u64,
                "Predicate head support differs from legacy"
            );
            let added = BTreeMap::from([
                (function_word_roles::FAMILY.to_string(), roles.family),
                (predicate_operators::FAMILY.to_string(), operator_family),
            ]);
            for (name, family) in &added {
                support
                    .entry(name.clone())
                    .or_default()
                    .add(family, &post)?;
                combined
                    .entry(name.clone())
                    .or_default()
                    .add(family, &post)?;
            }
            observations_total.add(roles.missing_lemma_events, &observations);
            totals.add(roles.missing_lemma_events, &observations);
            let added_sha256 = hash(&serde_json::to_vec(&added)?);
            serde_json::to_writer(
                &mut per_post,
                &json!({
                    "schema":"slopninja-grammar-choice-post-v1", "panel":panel.name,
                    "post":post, "annotation_sha256":binding.annotation_sha256,
                    "feature_cache_sha256":binding.feature_cache_sha256,
                    "legacy14_canonical_sha256":hash(&legacy_bytes),
                    "added_families_sha256":added_sha256, "added_families":added,
                    "missing_function_lemmas":roles.missing_lemma_events,
                    "source_aligned_function_events":roles.events,
                    "source_aligned_predicates":observations,
                }),
            )?;
            per_post.write_all(b"\n")?;
        }
        ensure!(
            panel_authors.len() == panel.authors && panel_authors.is_disjoint(&all_authors),
            "Authors duplicate or count differs"
        );
        all_authors.extend(panel_authors);
        let families: BTreeMap<_, _> = support
            .iter()
            .map(|(name, stats)| (name, stats.report(&protocol.support)))
            .collect();
        panel_reports.push(json!({"panel":panel.name, "posts":panel.posts, "authors":panel.authors, "families":families, "observations":observations_total}));
        bindings_receipt.push(json!({"panel":panel, "audit_sha256":manifest["audit_sha256"], "queries_sha256":manifest["queries_sha256"]}));
        panel_supports.push((panel.name.clone(), support));
        eprintln!(
            "Audited {}: {} TRAIN posts, {} authors",
            panel.name, panel.posts, panel.authors
        );
    }
    per_post.flush()?;
    ensure!(
        all_ids.len() == protocol.expected_posts && all_authors.len() == protocol.expected_authors,
        "Combined input counts differ"
    );
    let first = &panel_supports[0];
    let unseen: Vec<_> = panel_supports
        .iter()
        .skip(1)
        .map(|(name, stats)| {
            let families: BTreeMap<_, _> = stats
                .iter()
                .map(|(family, support)| (family, support.unseen_against(&first.1[family])))
                .collect();
            json!({"panel":name, "reference_panel":first.0, "families":families})
        })
        .collect();
    let families: BTreeMap<_, _> = combined
        .iter()
        .map(|(name, stats)| (name, stats.report(&protocol.support)))
        .collect();
    let report = json!({
        "schema":"slopninja-grammar-choice-support-report-v1", "status":"pass",
        "protocol_sha256":args.expected_protocol_sha256,
        "panels":panel_reports, "combined":{"posts":all_ids.len(),"authors":all_authors.len(),"families":families,"observations":totals},
        "support_threshold":protocol.support, "unseen_against_first_panel":unseen,
        "legacy14_canonical_bytes_exact":true, "predicate_head_denominators_exact":true,
        "loaded_splits":["train"], "parser_calls":0, "model_fits":0, "detector_calls":0,
        "inference":"Support is a TRAIN description; no retrieval, semantic fidelity, readability or detector benefit is established.",
        "new_families_used_by_working_metric":false,
    });
    write_fresh(&args.out.join("report.json"), &report)?;
    write_fresh(
        &args.out.join("receipt.json"),
        &json!({
            "schema":"slopninja-grammar-choice-support-receipt-v1", "protocol_sha256":args.expected_protocol_sha256,
            "sources":protocol.source_files, "inputs":bindings_receipt,
            "executable_sha256":hash(&fs::read(std::env::current_exe()?)?),
            "posts_sha256":hash(&fs::read(args.out.join("posts.jsonl"))?),
            "report_sha256":hash(&fs::read(args.out.join("report.json"))?),
        }),
    )?;
    println!(
        "{}",
        serde_json::to_string(
            &json!({"status":"pass","posts":all_ids.len(),"authors":all_authors.len(),"out":args.out})
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(author: &str, date: &str) -> Post {
        Post {
            author: author.into(),
            date: date.into(),
            id: format!("{author}/{date}"),
            source_group: date.into(),
            split: "train".into(),
            text_sha256: String::new(),
        }
    }

    #[test]
    fn support_counts_posts_authors_and_dates_separately() {
        let mut support = FamilySupport::default();
        let family = features::Family::Distribution {
            counts: BTreeMap::from([("a".into(), 3), ("b".into(), 1)]),
            opportunities: 4,
        };
        support.add(&family, &post("A", "day1")).unwrap();
        support.add(&family, &post("A", "day1")).unwrap();
        support.add(&family, &post("A", "day2")).unwrap();
        let threshold = SupportThreshold {
            posts: 3,
            authors: 2,
            author_dates: 2,
        };
        assert_eq!(support.report(&threshold)["supported_types"], 0);
        support.add(&family, &post("B", "day1")).unwrap();
        let report = support.report(&threshold);
        assert_eq!(report["supported_types"], 2);
        assert_eq!(report["opportunities"], 16);
        assert_eq!(report["supported_occurrence_fraction"], 1.0);
    }

    #[test]
    fn unavailable_and_unseen_counts_use_opportunities() {
        let mut support = FamilySupport::default();
        support
            .add(
                &features::Family::Distribution {
                    counts: BTreeMap::new(),
                    opportunities: 0,
                },
                &post("A", "d1"),
            )
            .unwrap();
        assert_eq!(
            support.report(&SupportThreshold {
                posts: 1,
                authors: 1,
                author_dates: 1
            })["supported_occurrence_fraction"],
            Value::Null
        );
        support
            .add(
                &features::Family::Distribution {
                    counts: BTreeMap::from([("a".into(), 4)]),
                    opportunities: 4,
                },
                &post("A", "d2"),
            )
            .unwrap();
        assert_eq!(
            support.unseen_against(&FamilySupport::default())["unseen_occurrence_fraction"],
            1.0
        );
        assert_eq!(support.unavailable_posts, 1);
        assert!(
            support
                .add(
                    &features::Family::Distribution {
                        counts: BTreeMap::from([("a".into(), 1)]),
                        opportunities: 2
                    },
                    &post("A", "d3")
                )
                .is_err()
        );
    }

    #[test]
    fn bound_synthetic_train_audit_rejects_changed_cache_and_nontrain_input() {
        use grammar_core::syntax::{Sentence, Token};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let cache = root.join("vector-feature-cache-v1");
        let export = root.join("named");
        fs::create_dir(&cache).unwrap();
        fs::create_dir(&export).unwrap();
        fs::write(root.join("source.rs"), b"synthetic source binding").unwrap();
        let doc = Document {
            text: "Go".into(),
            parser_identity: "synthetic-audit-v1".into(),
            tokens: vec![Token {
                i: 0,
                start_byte: 0,
                end_byte: 2,
                text: "Go".into(),
                lemma: "go".into(),
                pos: "VERB".into(),
                tag: "VB".into(),
                dep: "ROOT".into(),
                head: 0,
                sentence: 0,
                morph: BTreeMap::new(),
                is_punct: false,
                is_space: false,
            }],
            sentences: vec![Sentence {
                start_byte: 0,
                end_byte: 2,
                root: 0,
                token_start: 0,
                token_end: 1,
            }],
        };
        let annotation_bytes = serde_json::to_vec(&doc).unwrap();
        let cache_bytes =
            serde_json::to_vec(&json!({"features": features::extract(&doc).unwrap()})).unwrap();
        let text_sha = hash(doc.text.as_bytes());
        let annotation_path = cache.join(format!("{text_sha}.annotation.json"));
        fs::write(&annotation_path, &annotation_bytes).unwrap();
        fs::write(
            cache.join(format!("{text_sha}.features.json")),
            &cache_bytes,
        )
        .unwrap();
        let mut post = post("A", "day1");
        post.text_sha256 = text_sha;
        let audit = json!({
            "schema":"slopninja-named-family-tensor-audit-v1", "loaded_splits":["train"],
            "loaded_post_count":1, "training_author_count":1, "training_post_count":1,
            "posts":[{"post":post, "annotation_sha256":hash(&annotation_bytes), "feature_cache_sha256":hash(&cache_bytes)}]
        });
        let audit_bytes = serde_json::to_vec(&audit).unwrap();
        let query_bytes = serde_json::to_vec(&vec![&post]).unwrap();
        fs::write(export.join("audit.json"), &audit_bytes).unwrap();
        fs::write(export.join("queries.json"), &query_bytes).unwrap();
        let manifest = json!({"schema":"slopninja-named-family-tensor-v1", "split":"train",
            "parser_identity":doc.parser_identity, "query_count":1,
            "audit_sha256":hash(&audit_bytes), "queries_sha256":hash(&query_bytes)});
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        fs::write(export.join("manifest.json"), &manifest_bytes).unwrap();
        let mut protocol = json!({
            "schema":"slopninja-grammar-choice-support-protocol-v1", "parser_identity":doc.parser_identity,
            "legacy_feature_schema":features::FEATURE_VERSION,
            "source_files":{"source.rs":hash(b"synthetic source binding")},
            "panels":[{"name":"synthetic","export":"named","manifest_sha256":hash(&manifest_bytes),"posts":1,"authors":1}],
            "support":{"posts":1,"authors":1,"author_dates":1},"expected_posts":1,"expected_authors":1
        });
        let protocol_path = root.join("protocol.json");
        let invoke = |value: &Value, output: &str| {
            let bytes = serde_json::to_vec(value).unwrap();
            fs::write(&protocol_path, &bytes).unwrap();
            run(Args {
                protocol: protocol_path.clone(),
                expected_protocol_sha256: hash(&bytes),
                repo: root.into(),
                corpus: root.into(),
                out: root.join(output),
            })
        };
        invoke(&protocol, "success").unwrap();
        let report: Value =
            serde_json::from_slice(&fs::read(root.join("success/report.json")).unwrap()).unwrap();
        assert_eq!(
            report["combined"]["families"][function_word_roles::FAMILY]["unavailable_posts"],
            1
        );
        assert_eq!(
            report["combined"]["families"][predicate_operators::FAMILY]["opportunities"],
            1
        );
        fs::write(&annotation_path, b"changed cache").unwrap();
        assert!(
            invoke(&protocol, "bad-cache")
                .unwrap_err()
                .to_string()
                .contains("SHA256 differs")
        );
        fs::write(&annotation_path, &annotation_bytes).unwrap();
        let mut changed = manifest.clone();
        changed["split"] = json!("test");
        let bytes = serde_json::to_vec(&changed).unwrap();
        fs::write(export.join("manifest.json"), &bytes).unwrap();
        protocol["panels"][0]["manifest_sha256"] = json!(hash(&bytes));
        assert!(
            invoke(&protocol, "nontrain")
                .unwrap_err()
                .to_string()
                .contains("TRAIN export")
        );
    }
}

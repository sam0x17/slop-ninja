//! Reused-TRAIN association diagnostic; no parameter fitting or heldout access.
use crate::{
    association_projection::{self as projection, SEEDS},
    association_retrieval::{self as retrieval, Metrics, Post},
};
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::features::Family;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    expected_protocol_sha256: String,
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}
fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(path)?;
    ensure!(hash(&bytes) == expected, "Hash mismatch {}", path.display());
    Ok(bytes)
}
fn bound(repo: &Path, definition: &Value) -> Result<Vec<u8>> {
    checked(
        &repo.join(text(definition, "path")?),
        text(definition, "sha256")?,
    )
}
fn json_file(path: &Path, value: &Value) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

pub fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !args.out.exists(),
        "Use a fresh output directory; prior attempts must be retained"
    );
    let protocol: Value =
        serde_json::from_slice(&checked(&args.protocol, &args.expected_protocol_sha256)?)?;
    validate_protocol(&protocol)?;
    for (path, digest) in protocol["source_files"]
        .as_object()
        .context("Missing source bindings")?
    {
        checked(
            &args.repo.join(path),
            digest.as_str().context("Source hash")?,
        )?;
    }
    let receipt: Value =
        serde_json::from_slice(&bound(&args.repo, &protocol["inputs"]["receipt"])?)?;
    let prior: Value =
        serde_json::from_slice(&bound(&args.repo, &protocol["inputs"]["support_protocol"])?)?;
    let support: Value =
        serde_json::from_slice(&bound(&args.repo, &protocol["inputs"]["support_report"])?)?;
    ensure!(
        receipt["posts_sha256"] == protocol["inputs"]["posts"]["sha256"]
            && receipt["protocol_sha256"] == protocol["inputs"]["support_protocol"]["sha256"]
            && receipt["report_sha256"] == protocol["inputs"]["support_report"]["sha256"]
            && receipt["sources"] == prior["source_files"]
            && support["status"] == "pass"
            && support["loaded_splits"] == json!(["train"]),
        "Prior support evidence differs"
    );
    for (path, digest) in prior["source_files"].as_object().unwrap() {
        checked(&args.repo.join(path), digest.as_str().unwrap())?;
    }
    let posts_path = args.repo.join(text(&protocol["inputs"]["posts"], "path")?);
    drop(checked(
        &posts_path,
        text(&protocol["inputs"]["posts"], "sha256")?,
    )?);
    fs::create_dir_all(&args.out)?;
    let snapshot = args.out.join("executed-source");
    for path in protocol["source_files"].as_object().unwrap().keys() {
        let target = snapshot.join(path);
        fs::create_dir_all(target.parent().unwrap())?;
        fs::copy(args.repo.join(path), target)?;
    }
    json_file(&args.out.join("protocol.json"), &protocol)?;
    let outcome = run(&args, &protocol, &prior);
    if let Err(error) = &outcome {
        json_file(
            &args.out.join("failure.json"),
            &json!({"status":"failed; complete study stopped","error":format!("{error:#}"),"protocol_sha256":args.expected_protocol_sha256}),
        )?;
    }
    outcome
}

pub fn validate_protocol(p: &Value) -> Result<()> {
    ensure!(
        p["schema"] == "slopninja-grammar-association-protocol-v1"
            && p["panels"] == json!(retrieval::PANELS)
            && p["expected_panel_posts"] == json!([1224, 1363, 1302])
            && p["authors_per_panel"] == 100
            && p["shuffle_seeds"] == json!(SEEDS)
            && p["variants"] == json!(retrieval::VARIANTS)
            && p["distance"]
                == "complete squared Hellinger; dot product of square-root probabilities; no vocabulary truncation"
            && p["mixture_weights"]
                == json!({"marginal_lemma_pos":0.5,"marginal_role_context":0.5,"joint_mixture_marginal":0.5,"joint_mixture_joint":0.5,"operator_inventory":0.5,"operator_sequence":0.5})
            && p["missingness"]
                == "Abort the complete diagnostic if any required post/view has zero opportunities or any author lacks two dates; no post or author attrition"
            && p["ties"]
                == "Exact equal f64 distances; expected random-order top1/top5/MRR; no tolerance tie merging"
            && p["aggregation"]
                == "Equal posts within date, equal dates within author, equal authors; fixed100-author galleries"
            && p["bootstrap"]
                == json!({"replicates":2000,"seed":"0x6a09e667f3bcc909","draw":"xorshift64(13,7,17) modulo actual stratum author count","stratum_order":retrieval::PANELS,"percentile_indices_zero_based":[49,1949],"interpretation":"descriptive reused-TRAIN paired author intervals; no superiority or adoption threshold"}),
        "Frozen association design differs"
    );
    Ok(())
}

fn run(args: &Args, protocol: &Value, prior: &Value) -> Result<()> {
    let started = Instant::now();
    let corpus = args.repo.join("data/author-corpora/blog-authorship-2004");
    let mut source_bindings = BTreeMap::<String, (String, Value)>::new();
    for panel in prior["panels"].as_array().unwrap() {
        let directory = corpus.join(text(panel, "export")?);
        let m: Value = serde_json::from_slice(&checked(
            &directory.join("manifest.json"),
            text(panel, "manifest_sha256")?,
        )?)?;
        let a: Value = serde_json::from_slice(&checked(
            &directory.join("audit.json"),
            text(&m, "audit_sha256")?,
        )?)?;
        let q: Value = serde_json::from_slice(&checked(
            &directory.join("queries.json"),
            text(&m, "queries_sha256")?,
        )?)?;
        ensure!(
            m["split"] == "train"
                && a["loaded_splits"] == json!(["train"])
                && m["parser_identity"] == prior["parser_identity"],
            "Bound panel identity differs"
        );
        let ids = q
            .as_array()
            .unwrap()
            .iter()
            .map(|v| text(v, "id").unwrap())
            .collect::<BTreeSet<_>>();
        ensure!(
            ids.len() == panel["posts"].as_u64().unwrap() as usize,
            "Panel query scope differs"
        );
        for binding in a["posts"].as_array().unwrap() {
            let id = text(&binding["post"], "id")?.to_owned();
            ensure!(
                ids.contains(id.as_str())
                    && source_bindings
                        .insert(id, (text(panel, "name")?.into(), binding.clone()))
                        .is_none(),
                "Duplicate or nonquery input binding"
            );
        }
    }
    let mut panels = BTreeMap::<String, Vec<Post>>::new();
    let mut authors = BTreeMap::<String, BTreeSet<String>>::new();
    let mut seen = BTreeSet::new();
    let mut seen_texts = BTreeSet::new();
    let mut control_changed_events = BTreeMap::<u64, u64>::new();
    let mut control_changed_posts = BTreeMap::<u64, u64>::new();
    let mut functions = 0u64;
    let mut heads = 0u64;
    let mut projected = BufWriter::new(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(args.out.join("projection-audit.jsonl"))?,
    );
    let source = BufReader::new(fs::File::open(
        args.repo.join(text(&protocol["inputs"]["posts"], "path")?),
    )?);
    for line in source.lines() {
        let row: Value = serde_json::from_str(&line?)?;
        let post = &row["post"];
        let id = text(post, "id")?;
        let panel = text(&row, "panel")?;
        let author = text(post, "author")?;
        let date = text(post, "date")?;
        ensure!(
            seen.insert(id.to_owned())
                && seen_texts.insert(text(post, "text_sha256")?.to_owned())
                && post["split"] == "train",
            "Repeated/non-TRAIN record"
        );
        let (expected_panel, binding) = source_bindings.get(id).context("Unbound input post")?;
        ensure!(
            panel == expected_panel
                && binding["post"] == *post
                && binding["annotation_sha256"] == row["annotation_sha256"]
                && binding["feature_cache_sha256"] == row["feature_cache_sha256"],
            "Source record binding differs"
        );
        grammar_eval::validate_date(date)?;
        ensure!(
            post["source_group"] == format!("blog:{author}:date:{date}"),
            "Source date group differs"
        );
        let events: Vec<crate::function_word_roles::Event> =
            serde_json::from_value(row["source_aligned_function_events"].clone())?;
        let predicates: Vec<crate::predicate_operators::PredicateObservation> =
            serde_json::from_value(row["source_aligned_predicates"].clone())?;
        let value = projection::project(id, &events, &predicates)?;
        let added: BTreeMap<String, Family> =
            serde_json::from_value(row["added_families"].clone())?;
        ensure!(
            hash(&serde_json::to_vec(&added)?) == row["added_families_sha256"],
            "Saved family binding differs"
        );
        for (old, new) in [
            ("function_word_role_v1", "function_joint"),
            ("predicate_operator_sequence_v1", "operator_sequence"),
        ] {
            let Family::Distribution {
                counts,
                opportunities,
            } = &added[old]
            else {
                anyhow::bail!("Expected original distribution")
            };
            ensure!(
                counts == &value.views[new] && *opportunities == value.opportunities[new],
                "Trace projection disagrees with saved family"
            );
        }
        functions += events.len() as u64;
        heads += predicates.len() as u64;
        for seed in SEEDS {
            *control_changed_events.entry(seed).or_default() +=
                value.shuffle_changed_lemma_events[&seed];
            *control_changed_posts.entry(seed).or_default() +=
                u64::from(value.shuffle_joint_counts_changed[&seed]);
        }
        let hashes = value
            .views
            .iter()
            .map(|(name, counts)| Ok((name.clone(), hash(&serde_json::to_vec(counts)?))))
            .collect::<Result<BTreeMap<_, _>>>()?;
        serde_json::to_writer(
            &mut projected,
            &json!({"panel":panel,"post":post,"opportunities":value.opportunities,"view_counts_sha256":hashes,
            "shuffle_changed_lemma_events":value.shuffle_changed_lemma_events,"shuffle_joint_counts_changed":value.shuffle_joint_counts_changed}),
        )?;
        projected.write_all(b"\n")?;
        authors
            .entry(panel.into())
            .or_default()
            .insert(author.into());
        panels.entry(panel.into()).or_default().push(Post {
            id: id.into(),
            author: author.into(),
            date: date.into(),
            views: value.views,
        });
    }
    projected.flush()?;
    ensure!(
        seen.len() == 3889 && source_bindings.len() == seen.len(),
        "Complete input post scope differs"
    );
    let mut all_authors = BTreeSet::new();
    for (index, panel) in retrieval::PANELS.iter().enumerate() {
        ensure!(
            panels[*panel].len() == [1224, 1363, 1302][index]
                && authors[*panel].len() == 100
                && all_authors.is_disjoint(&authors[*panel]),
            "Panel author/post scope or independence differs"
        );
        all_authors.extend(authors[*panel].clone());
    }
    json_file(
        &args.out.join("projection-summary.json"),
        &json!({"posts":seen.len(),"authors":all_authors.len(),"function_events":functions,"clause_heads":heads,
        "shuffle_changed_lemma_events":control_changed_events,"shuffle_joint_counts_changed_posts":control_changed_posts,
        "marginal_invariance_post_seed_checks":seen.len()*5,"full_original_joint_trace_matches":seen.len()*2,"unavailable_posts":0}),
    )?;
    let mut result = BTreeMap::new();
    let mut query_writer = BufWriter::new(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(args.out.join("queries.jsonl"))?,
    );
    for panel in retrieval::PANELS {
        eprintln!(
            "Scoring fixed TRAIN panel {panel}: {} queries,100 authors",
            panels[panel].len()
        );
        let row = retrieval::evaluate_panel(&panels[panel], |mut v| {
            v["panel"] = json!(panel);
            serde_json::to_writer(&mut query_writer, &v)?;
            query_writer.write_all(b"\n")?;
            Ok(())
        })?;
        json_file(&args.out.join(format!("panel-{panel}.json")), &row.summary)?;
        result.insert(panel.to_owned(), row);
    }
    query_writer.flush()?;
    let mut combined = BTreeMap::new();
    for name in result["original"].per_author.keys() {
        let mut all = BTreeMap::new();
        for panel in retrieval::PANELS {
            for (author, m) in &result[panel].per_author[name] {
                ensure!(
                    all.insert(author.clone(), *m).is_none(),
                    "Duplicate pooled author"
                );
            }
        }
        let mut mean = Metrics::default();
        for m in all.values() {
            mean.add(*m, 1.0 / all.len() as f64);
        }
        combined.insert(name.clone(), json!({"macro_author":mean,"per_author":all}));
    }
    let comparisons = retrieval::comparisons(&result)?;
    let report = json!({"schema":"slopninja-grammar-association-report-v1","status":"pass","protocol_sha256":args.expected_protocol_sha256,
        "panels":result.iter().map(|(name,p)|(name.clone(),p.summary.clone())).collect::<BTreeMap<_,_>>(),"combined":{"authors":300,"posts":3889,"models":combined},
        "comparisons":comparisons,"shuffle_control_interpretation":"Five fixed perturbations; query metrics are averaged for the paired mean control, never scores or independent-replication counts",
        "elapsed_seconds":started.elapsed().as_secs_f64(),"loaded_splits":["train"],"model_fits":0,"parser_calls":0,"detector_calls":0,
        "fresh_confirmation":false,"adoption_decision":"none; descriptive reuse of previously examined TRAIN authors"});
    json_file(&args.out.join("report.json"), &report)?;
    let artifacts = fs::read_dir(&args.out)?
        .map(|e| e.map(|v| v.path()))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|p| p.is_file())
        .map(|p| {
            Ok((
                p.file_name().unwrap().to_string_lossy().into_owned(),
                hash(&fs::read(p)?),
            ))
        })
        .collect::<Result<BTreeMap<String, String>>>()?;
    json_file(
        &args.out.join("receipt.json"),
        &json!({"schema":"slopninja-grammar-association-receipt-v1","protocol_sha256":args.expected_protocol_sha256,"sources":protocol["source_files"],
        "executable_sha256":hash(&fs::read(std::env::current_exe()?)?),"artifacts_sha256":artifacts,"inputs":protocol["inputs"],"parameter_updates":0}),
    )?;
    println!(
        "{}",
        serde_json::to_string(
            &json!({"status":"pass","report_sha256":hash(&fs::read(args.out.join("report.json"))?),"posts":3889,"authors":300})
        )?
    );
    Ok(())
}

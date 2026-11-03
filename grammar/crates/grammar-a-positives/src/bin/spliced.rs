#![recursion_limit = "512"]
//! Spliced-rewrite span data generator (A span protocol spans-v1, Section 3).
//!
//! `plan` builds the keyed plan from the release trainer bundle's Train and
//! Dev-iter humans; `generate` runs one generator's pending requests in an
//! order window against a running llama-server; `progress` reports state;
//! `finalize` assembles accepted documents, retains failures and writes the
//! manifests and reports. All outputs hold private prose: write them only to
//! ignored storage.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_a_positives::splice::{self, Edit, Item, Parent};
use grammar_a_positives::{diverse, form_post, generation, library, overlap, plan, text, write_json, write_jsonl};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Parser)]
#[command(name = "slop_ninja_a_spliced")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build the spliced-rewrite plan (Train and the Dev-iter span-evaluation set).
    Plan {
        /// Release trainer bundle rows.jsonl (Train and Dev-iter humans).
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        bundle_sha256: String,
        /// splits-v5 directory (train/ and train-mining/ manifests).
        #[arg(long)]
        splits_v5: PathBuf,
        /// Dev-iter human documents (eval-humans merged dev-iter/documents.jsonl).
        #[arg(long)]
        dev_iter_humans: PathBuf,
        /// Style library snapshot (voices.accepted.jsonl, briefs.accepted.jsonl).
        #[arg(long)]
        library: PathBuf,
        /// Frozen style partitions JSONL.
        #[arg(long)]
        partitions: PathBuf,
        #[arg(long)]
        partitions_sha256: String,
        #[arg(long)]
        fix_slop: PathBuf,
        #[arg(long)]
        tokenizer: PathBuf,
        /// RAID evaluation lineage check (matched groups are excluded).
        #[arg(long)]
        raid_lineage: PathBuf,
        #[arg(long, default_value_t = 40000)]
        train_target: usize,
        #[arg(long, default_value_t = 300)]
        floor: usize,
        #[arg(long, default_value_t = 4000)]
        cap: usize,
        #[arg(long, default_value_t = 1.5)]
        overprovision: f64,
        #[arg(long, default_value_t = 4)]
        dev_variants: usize,
        #[arg(long)]
        out: PathBuf,
    },
    /// spans-v1.1 `+` trainer bundle: release Train/Dev-iter/CAL rows plus the frozen v1.1 spliced Train
    /// records (Dev-select and Test lines are skipped unparsed).
    BundlePlus {
        #[arg(long)]
        rows: PathBuf,
        #[arg(long)]
        rows_sha256: String,
        #[arg(long)]
        weights: PathBuf,
        #[arg(long)]
        manifest: PathBuf,
        /// spans-v1 data directory holding SNAPSHOT.json (label v1.1).
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Trainer-row projection of one half (span_select or span_report) of the Dev-iter span-evaluation set.
    ProjectDev {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        half: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Run one generator's pending requests of one population in an order window.
    Generate {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        population: String,
        #[arg(long)]
        generator: String,
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        runs: PathBuf,
        #[arg(long)]
        index: PathBuf,
        #[arg(long, default_value_t = 4)]
        workers: usize,
        #[arg(long, default_value_t = 0)]
        from: usize,
        #[arg(long, default_value_t = usize::MAX)]
        to: usize,
        #[arg(long)]
        dry_run: bool,
        /// Form profile in the acceptance path: `cc_cohort_profile_v2`
        /// (protocol A2 §6.2 and §7.5) or `raw` (the default: no post-processing).
        #[arg(long)]
        profile: Option<String>,
        /// Round-2 population id of a `cc_cohort_profile_v2` run (its seed and
        /// prior), e.g. `span_select2`; distinct from the plan's `--population`.
        #[arg(long)]
        profile_population: Option<String>,
    },
    /// Requests and documents done, accepted and pending per population and generator.
    Progress {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        runs: PathBuf,
        #[arg(long, default_value_t = 0)]
        from: usize,
        #[arg(long, default_value_t = usize::MAX)]
        to: usize,
    },
    /// Assemble accepted documents and write records, failures, reports and manifests.
    Finalize {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        runs: PathBuf,
        #[arg(long)]
        configs: PathBuf,
        #[arg(long)]
        tokenizer: PathBuf,
        /// Accepted Train documents to keep, in plan order (Phase A 20000, Phase B 40000).
        #[arg(long)]
        train_target: usize,
        /// Only items with order below this bound are assembled (default: all).
        #[arg(long, default_value_t = usize::MAX)]
        train_through: usize,
        #[arg(long)]
        out: PathBuf,
        /// Label recorded in the manifest (for example phase-a).
        #[arg(long)]
        label: String,
        /// Accepted Train documents per domain at most (protocol 3.8 cap); later items of a
        /// capped domain are skipped, not failed.
        #[arg(long, default_value_t = usize::MAX)]
        domain_cap: usize,
    },
    /// spans-v1.1 amendment of the frozen snapshot: drop two-generator and meta-screen-v2 documents,
    /// normalize typography in the keyed half and record copied-word masks (no generation, no refill).
    Amend {
        /// Spliced data directory holding SNAPSHOT.json, records/ and plan/.
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        tokenizer: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        label: String,
    },
    /// Flag release-bundle mixed rows whose AI spans hit the meta screen v2 (identities only).
    ScreenBundle {
        #[arg(long)]
        rows: PathBuf,
        #[arg(long)]
        rows_sha256: String,
        #[arg(long)]
        out: PathBuf,
    },
}

fn read_jsonl(path: &Path) -> Result<Vec<Value>> {
    let f = BufReader::new(fs::File::open(path).with_context(|| path.display().to_string())?);
    let mut v = Vec::new();
    for l in f.lines() {
        let l = l?;
        if !l.trim().is_empty() {
            v.push(serde_json::from_str(&l)?);
        }
    }
    Ok(v)
}

fn file_sha(p: &Path) -> Result<String> {
    Ok(text::sha_hex(&fs::read(p).with_context(|| p.display().to_string())?))
}

fn tokenizer(p: &Path) -> Result<tokenizers::Tokenizer> {
    let mut tok = tokenizers::Tokenizer::from_file(p).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tok.with_truncation(None).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tok.with_padding(None);
    Ok(tok)
}

fn a_tokens(tok: &tokenizers::Tokenizer, s: &str) -> Result<usize> {
    generation::a_tokens(tok, s)
}

#[derive(Deserialize)]
struct ManifestRow {
    id: Option<String>,
    split: Option<String>,
    text_sha256: Option<String>,
    source_group: Option<String>,
    writer_key: Option<String>,
}

fn bump(c: &mut BTreeMap<String, BTreeMap<String, usize>>, dim: &str, val: &str) {
    *c.entry(dim.into()).or_default().entry(val.into()).or_default() += 1;
}

#[allow(clippy::too_many_arguments)]
fn run_plan(
    bundle: &Path, bundle_sha256: &str, splits_v5: &Path, dev_iter_humans: &Path, library_dir: &Path, partitions: &Path, partitions_sha256: &str,
    fix_slop: &Path, tok_path: &Path, raid_lineage: &Path, train_target: usize, floor: usize, cap: usize, overprovision: f64, dev_variants: usize,
    out: &Path,
) -> Result<()> {
    ensure!(!out.exists(), "plan directory exists: {}", out.display());
    let bsha = file_sha(bundle)?;
    ensure!(bsha == bundle_sha256, "bundle rows SHA-256 {bsha} != {bundle_sha256}");
    let psha = file_sha(partitions)?;
    ensure!(psha == partitions_sha256, "partitions SHA-256 {psha} != {partitions_sha256}");
    let tok = tokenizer(tok_path)?;
    let mut excl: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    // RAID-matched groups
    let lineage: Value = serde_json::from_slice(&fs::read(raid_lineage)?)?;
    let mut raid_groups: HashSet<String> = HashSet::new();
    for g in lineage["matched_reserved_groups"].as_array().context("matched_reserved_groups")? {
        if let Some(k) = g["reserved_group_key"].as_str() {
            raid_groups.insert(k.to_owned());
        }
        for s in g["raid_source_groups"].as_array().into_iter().flatten() {
            if let Some(s) = s.as_str() {
                raid_groups.insert(s.to_owned());
            }
        }
    }
    // bundle humans (Dev-select rows are skipped before parsing their text)
    let mut train_c: Vec<Value> = Vec::new();
    let mut dev_c: Vec<Value> = Vec::new();
    let f = BufReader::new(fs::File::open(bundle)?);
    for line in f.lines() {
        let line = line?;
        if line.contains("\"split\":\"dev_select\"") {
            continue;
        }
        let v: Value = serde_json::from_str(&line)?;
        if v["origin"] != "human_only" {
            continue;
        }
        let split = v["split"].as_str().unwrap_or("");
        let dom = v["domain"].as_str().unwrap_or("");
        let t = v["text"].as_str().context("text")?;
        ensure!(text::sha_hex(t.as_bytes()) == v["text_sha256"].as_str().unwrap_or(""), "bundle text digest {}", v["id"]);
        match split {
            "train" => {
                if splice::RAID_DOMAINS.contains(&dom) {
                    bump(&mut excl, "train_excluded", "raid_domain");
                } else if splice::HELD_DOMAINS.contains(&dom) {
                    bump(&mut excl, "train_excluded", "ellipse_held");
                } else if v["cohort_weight"].as_f64().unwrap_or(0.0) <= 0.0 {
                    bump(&mut excl, "train_excluded", "zero_cohort_weight");
                } else if v["source_group"].as_str().is_some_and(|g| raid_groups.contains(g)) {
                    bump(&mut excl, "train_excluded", "raid_matched_group");
                } else {
                    train_c.push(v);
                }
            }
            "dev_iter" => dev_c.push(v),
            _ => {}
        }
    }
    // splits-v5 Train / Train-mining membership, writer keys
    let want: HashSet<String> = train_c.iter().map(|v| v["id"].as_str().unwrap().to_owned()).collect();
    let mut man: HashMap<String, ManifestRow> = HashMap::new();
    let mut man_files = serde_json::Map::new();
    for sub in ["train", "train-mining"] {
        let p = splits_v5.join(sub).join("manifest.jsonl");
        man_files.insert(sub.into(), json!({"path":p.display().to_string(),"bytes":fs::metadata(&p)?.len()}));
        let f = BufReader::with_capacity(1 << 20, fs::File::open(&p)?);
        for line in f.lines() {
            let line = line?;
            let r: ManifestRow = serde_json::from_str(&line)?;
            if let Some(id) = &r.id {
                if want.contains(id) {
                    man.insert(id.clone(), r);
                }
            }
        }
    }
    let mut train: Vec<Parent> = Vec::new();
    let mut dom_counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for v in &train_c {
        let id = v["id"].as_str().unwrap();
        let dom = v["domain"].as_str().unwrap().to_owned();
        let Some(m) = man.get(id) else {
            bump(&mut excl, "train_excluded", "not_in_splits_v5_train_or_mining");
            continue;
        };
        if !matches!(m.split.as_deref(), Some("train" | "train_mining")) || m.text_sha256.as_deref() != v["text_sha256"].as_str() {
            bump(&mut excl, "train_excluded", "splits_v5_split_or_digest_mismatch");
            continue;
        }
        if m.source_group.as_deref() != v["source_group"].as_str() {
            bump(&mut excl, "train_excluded", "source_group_mismatch");
            continue;
        }
        let t = v["text"].as_str().unwrap();
        let n = a_tokens(&tok, t)?;
        if n > splice::MAX_PARENT_TOKENS {
            bump(&mut excl, "train_excluded", "over_1800_a_tokens");
            continue;
        }
        *dom_counts.entry(dom.clone()).or_default().entry(m.split.clone().unwrap()).or_default() += 1;
        train.push(Parent {
            id: id.into(),
            split: "train".into(),
            domain: dom,
            genre: v["genre"].as_str().map(String::from),
            source_group: v["source_group"].as_str().unwrap().into(),
            writer_key: m.writer_key.clone(),
            cohort_id: v["cohort_id"].as_str().unwrap_or("").into(),
            provenance_sha256: v["provenance_sha256"].as_str().map(String::from),
            text: t.into(),
            text_sha256: v["text_sha256"].as_str().unwrap().into(),
            a_tokens: n,
            words: splice::py_words(t),
        });
    }
    // Dev-iter humans
    let mut dh: HashMap<String, Value> = HashMap::new();
    for r in read_jsonl(dev_iter_humans)? {
        ensure!(r["frozen_split"] == "dev_iter", "Dev-iter human file holds another split");
        dh.insert(r["id"].as_str().unwrap().to_owned(), r);
    }
    let mut dev: Vec<Parent> = Vec::new();
    for v in &dev_c {
        let id = v["id"].as_str().unwrap();
        let r = dh.get(id).with_context(|| format!("Dev-iter bundle human {id} not in the Dev-iter documents"))?;
        ensure!(r["text_sha256"] == v["text_sha256"] && r["source_group"] == v["source_group"], "Dev-iter human {id} differs");
        let t = v["text"].as_str().unwrap();
        let n = a_tokens(&tok, t)?;
        if n > splice::MAX_PARENT_TOKENS {
            bump(&mut excl, "dev_iter_excluded", "over_1800_a_tokens");
            continue;
        }
        dev.push(Parent {
            id: id.into(),
            split: "dev_iter".into(),
            domain: v["domain"].as_str().unwrap().into(),
            genre: v["genre"].as_str().map(String::from),
            source_group: v["source_group"].as_str().unwrap().into(),
            writer_key: r["writer_key"].as_str().map(String::from),
            cohort_id: v["cohort_id"].as_str().unwrap_or("").into(),
            provenance_sha256: v["provenance_sha256"].as_str().map(String::from),
            text: t.into(),
            text_sha256: v["text_sha256"].as_str().unwrap().into(),
            a_tokens: n,
            words: splice::py_words(t),
        });
    }
    // no source group spans Train and Dev-iter
    let dev_groups: HashSet<&str> = dev.iter().map(|p| p.source_group.as_str()).collect();
    let leak = train.iter().filter(|p| dev_groups.contains(p.source_group.as_str())).count();
    ensure!(leak == 0, "{leak} Train parents share a source group with Dev-iter humans");
    // style library with frozen partitions
    let mut part: HashMap<String, String> = HashMap::new();
    for r in read_jsonl(partitions)? {
        part.insert(r["id"].as_str().context("id")?.into(), r["partition"].as_str().context("partition")?.into());
    }
    let mut lib = Vec::new();
    let mut lib_files = serde_json::Map::new();
    let mut lib_dropped = 0usize;
    for f in ["voices.accepted.jsonl", "briefs.accepted.jsonl"] {
        let p = library_dir.join(f);
        lib_files.insert(f.into(), json!({"sha256":file_sha(&p)?}));
        for a in read_jsonl(&p)? {
            let id = a["id"].as_str().context("id")?;
            let Some(pt) = part.get(id) else {
                lib_dropped += 1;
                continue;
            };
            let kind = a["type"].as_str().context("type")?.to_owned();
            lib.push(diverse::Brief {
                id: id.into(),
                genre: if kind == "voice" { "any".into() } else { a["genre"].as_str().context("genre")?.into() },
                kind,
                inventor: a["inventor"].as_str().context("inventor")?.into(),
                instruction: a["text"].as_str().context("text")?.into(),
                partition: pt.clone(),
                axis: a["axis"].as_str().unwrap_or("other").into(),
                imitated_author: a["imitated_author"].as_str().map(String::from),
                rewrite_safe: a["rewrite_safe"] == true,
                eval_author_conflict: a["eval_author_conflicts"].as_array().is_some_and(|v| !v.is_empty()),
                origin: a["origin"].as_str().unwrap_or("invented").into(),
                source_family: a["source"]["source_group"].as_str().map(String::from),
                cluster: a["cluster"].as_str().context("cluster")?.into(),
            });
        }
    }
    let fs_bundle = diverse::load_fix_slop(fix_slop)?;
    let p = splice::build(&splice::PlanInputs {
        train: &train,
        dev: &dev,
        briefs: &lib,
        fix_slop: &fs_bundle,
        train_target,
        floor,
        cap,
        overprovision,
        dev_variants,
        tokenizer: &tok,
    })?;
    fs::create_dir_all(out.join("requests"))?;
    // parents actually used
    let used: HashSet<&str> = p.items.iter().map(|i| i.parent_id.as_str()).collect();
    let parents: Vec<&Parent> = train.iter().chain(dev.iter()).filter(|x| used.contains(x.id.as_str())).collect();
    let parents_sha = write_jsonl(&out.join("parents.jsonl"), &parents)?;
    let items_sha = write_jsonl(&out.join("items.jsonl"), &p.items)?;
    let fail_sha = write_jsonl(&out.join("plan-failures.jsonl"), &p.failures)?;
    let mut by_gen: BTreeMap<String, Vec<&plan::Request>> = BTreeMap::new();
    for r in &p.requests {
        by_gen.entry(r.generator.clone()).or_default().push(r);
    }
    let mut req_files = serde_json::Map::new();
    for (g, v) in by_gen {
        let sha = write_jsonl(&out.join("requests").join(format!("{g}.jsonl")), &v)?;
        req_files.insert(g, json!({"requests":v.len(),"sha256":sha}));
    }
    // coverage
    let mut cov: BTreeMap<String, BTreeMap<String, BTreeMap<String, usize>>> = BTreeMap::new();
    for it in &p.items {
        let c = cov.entry(it.population.clone()).or_default();
        bump(c, "policy", &it.policy);
        bump(c, "policy_requested", &it.policy_requested);
        bump(c, "policy_fallback", if it.policy_fallback_steps == 0 { "none" } else { "fallback" });
        bump(c, "domain", &it.domain);
        bump(c, "edits_per_doc", &it.edits.len().to_string());
        bump(c, "two_generators", &it.two_generators.to_string());
        bump(c, "voice_scope", &it.voice_scope);
        bump(c, "reserve", if it.order_t > 1.0 { "reserve" } else { "nominal" });
        if let Some(h) = &it.span_half {
            bump(c, "span_half", h);
        }
        for e in &it.edits {
            bump(c, "operation", &e.operation);
            bump(c, "family", &e.family);
            bump(c, "generator", &e.generator);
            bump(c, "kind", &e.kind);
            bump(c, "component", &e.component);
            bump(c, "context_included", &e.context_included.to_string());
            bump(c, "style_kind", e.style["style_kind"].as_str().unwrap_or("-"));
            if e.operation != e.operation_requested {
                bump(c, "operation_fallback", &format!("{}->{}", e.operation_requested, e.operation));
            }
        }
    }
    let n_train = p.items.iter().filter(|i| i.population == splice::TRAIN).count();
    let n_dev = p.items.len() - n_train;
    let meta = json!({
        "schema": splice::PLAN_SCHEMA, "code": splice::CODE_ID, "crate_code": grammar_a_positives::CODE_ID,
        "producer_sha256": {"splice.rs": text::sha_hex(include_bytes!("../splice.rs")), "diverse.rs": text::sha_hex(include_bytes!("../diverse.rs")),
            "text.rs": text::sha_hex(include_bytes!("../text.rs")), "generation.rs": text::sha_hex(include_bytes!("../generation.rs")),
            "spliced.rs": text::sha_hex(include_bytes!("spliced.rs"))},
        "binary": std::env::current_exe()?.display().to_string(),
        "inputs": {"bundle_rows": {"path": bundle.display().to_string(), "sha256": bsha}, "splits_v5_manifests": man_files,
            "dev_iter_humans": {"path": dev_iter_humans.display().to_string(), "sha256": file_sha(dev_iter_humans)?},
            "library": {"path": library_dir.display().to_string(), "files": lib_files, "items": lib.len(), "dropped_without_frozen_partition": lib_dropped, "policy": library::POLICY},
            "partitions": {"path": partitions.display().to_string(), "sha256": psha},
            "fix_slop": {"revision": diverse::FIX_SLOP_REVISION, "files": diverse::FIX_SLOP_FILES.iter().map(|(n, h)| json!({"path": n, "sha256": h})).collect::<Vec<_>>()},
            "tokenizer": {"path": tok_path.display().to_string(), "sha256": file_sha(tok_path)?},
            "raid_lineage": {"path": raid_lineage.display().to_string(), "sha256": file_sha(raid_lineage)?, "groups": raid_groups.len()}},
        "parameters": {"train_target": train_target, "floor": floor, "cap": cap, "overprovision": overprovision, "dev_variants": dev_variants,
            "max_parent_tokens": splice::MAX_PARENT_TOKENS, "max_doc_tokens": splice::MAX_DOC_TOKENS, "key_prefix": splice::KEY_PREFIX,
            "splitter": text::SPLITTER_ID, "word_policy": splice::WORD_POLICY},
        "parents": {"train_eligible": train.len(), "dev_iter_eligible": dev.len(), "used": parents.len(), "sha256": parents_sha, "train_by_domain_and_split": dom_counts},
        "exclusions": excl,
        "allocation": p.allocation,
        "templates": splice::template_hashes(),
        "items": {"train": n_train, "dev_iter_span_eval": n_dev, "sha256": items_sha},
        "requests": p.requests.len(), "request_files": req_files,
        "plan_failures": p.failures.len(), "plan_failures_sha256": fail_sha,
        "coverage": cov,
        "rules": {
            "parents": "Train: release trainer-bundle Train humans (all splits-v5 Train or Train-mining by id and text SHA-256), minus RAID domains, RAID-matched groups, ELLIPSE (held, zero weight) and parents over 1,800 A tokens; Dev-iter: the bundle Dev-iter humans",
            "allocation": "Train items per domain: square root of eligible parents scaled to train_target, floor 300, cap 4,000, at most two variants per parent; planned items = round(overprovision x allocation) capped at two per parent; order by (j + 0.5) / allocation then domain, so every prefix is stratified and items with t > 1 are reserve",
            "variants": "parents by key; 20% of used parents get a second variant (different policy, generator +4 in the 8-family cycle) where availability allows",
            "policy": "50-slot interleaved cycle one_sentence 7, scattered 9, paragraph 7, most 4, clause 8, insert 6, condense 5, compound 4; slot = rank(k_policy) mod 50 (second variant +25); an infeasible policy for the parent text advances along the cycle at plan time (recorded), never after generation",
            "operations": "per edit, keyed: runs 44 slots (paraphrase 9, voiced paraphrase 3, casual 3, formalize 3, simplify 2, polish 4, spelling 2, fix_slop 2, expand 5, condense 5, humanize 6); clauses 40 slots (paraphrase 10, formalize 4, casual 4, simplify 3, polish 8, humanize 6, expand 5); inserts: continue at document end, else bridge or elaborate 50/50; condense policy always condense; fix_slop over 150 words -> polish, expand that could pass 2,000 tokens -> paraphrase",
            "generators": "Train: rank(k_generator) mod 8 over the eight Train instruct families; Dev-iter: 4 consecutive of the 7 Dev-iter families per human; 10% of multi-edit documents alternate a second generator",
            "styles": "frozen partitions (train / dev_iter); voice per document for 70% of documents, per edit for 30%; plan-v2 style_kind composition (25% none) for style-capable operations; voiced paraphrase always voiced; polish, spelling, fix_slop and formalize operation-only; round-robin library assignment skipping the parent's own source family",
            "decoding": "plan-v2 sample_decoding keyed by item and generator; max_tokens = min(3 x target words + 64, 6000); seed from the request SHA-256",
            "context": "50% of run and clause edits show the neighbouring sentences read-only (keyed); inserts always see their neighbours",
            "dev_halves": "Dev-iter source groups ranked by key within domain; even ranks span_select, odd span_report"
        }
    });
    write_json(&out.join("plan.json"), &meta)?;
    println!("{}", serde_json::to_string(&json!({"items_train":n_train,"items_dev":n_dev,"requests":p.requests.len(),"plan_failures":p.failures.len(),
        "train_parents":train.len(),"dev_parents":dev.len(),"exclusions":excl}))?);
    Ok(())
}

fn load_items(pdir: &Path) -> Result<Vec<Item>> {
    read_jsonl(&pdir.join("items.jsonl"))?.into_iter().map(|v| serde_json::from_value(v).map_err(Into::into)).collect()
}

fn load_parents(pdir: &Path) -> Result<HashMap<String, Parent>> {
    let mut m = HashMap::new();
    for v in read_jsonl(&pdir.join("parents.jsonl"))? {
        let p: Parent = serde_json::from_value(v)?;
        ensure!(text::sha_hex(p.text.as_bytes()) == p.text_sha256, "parent digest {}", p.id);
        m.insert(p.id.clone(), p);
    }
    Ok(m)
}

/// Profile v2's acceptance step for a spliced run of plan population
/// `population` under round-2 population `pop`: the prior of the parents of
/// every plan item of that population.
fn form_for_population(items: &[Item], pdir: &Path, population: &str, pop: &str) -> Result<form_post::FormPost> {
    let ids: HashSet<&str> = items.iter().filter(|i| i.population == population).map(|i| i.parent_id.as_str()).collect();
    let parents = load_parents(pdir)?;
    let mut rows: Vec<&Parent> = Vec::new();
    for id in &ids {
        rows.push(parents.get(*id).with_context(|| format!("parent {id} of a {population} item"))?);
    }
    form_post::FormPost::new(pop, rows.iter().map(|p| (p.id.as_str(), p.domain.as_str(), p.text.as_str())))
}

#[allow(clippy::too_many_arguments)]
fn run_generate(
    pdir: &Path,
    population: &str,
    generator: &str,
    config: &Path,
    runs: &Path,
    index: &Path,
    workers: usize,
    from: usize,
    to: usize,
    dry: bool,
    form_population: Option<&str>,
) -> Result<()> {
    let meta: Value = serde_json::from_slice(&fs::read(pdir.join("plan.json"))?)?;
    let cfg_bytes = fs::read(config)?;
    let cfg: Value = serde_json::from_slice(&cfg_bytes)?;
    ensure!(cfg["key"] == generator, "config key mismatch");
    let rf = pdir.join("requests").join(format!("{generator}.jsonl"));
    ensure!(meta["request_files"][generator]["sha256"] == file_sha(&rf)?, "request file changed");
    let items = load_items(pdir)?;
    let form = match form_population {
        Some(pop) => {
            ensure!(meta["items"]["sha256"] == file_sha(&pdir.join("items.jsonl"))?, "items.jsonl changed since planning");
            ensure!(meta["parents"]["sha256"] == file_sha(&pdir.join("parents.jsonl"))?, "parents.jsonl changed since planning");
            Some(form_for_population(&items, pdir, population, pop)?)
        }
        None => None,
    };
    let form_rec = form.as_ref().map(|f| f.record());
    let mut window: HashMap<String, usize> = HashMap::new();
    let mut edits: HashMap<String, Edit> = HashMap::new();
    for it in &items {
        if it.population == population && it.order >= from && it.order < to {
            window.insert(it.item_id.clone(), it.order);
            for e in &it.edits {
                if e.generator == generator {
                    edits.insert(e.request_id.clone(), e.clone());
                }
            }
        }
    }
    let allowed: Vec<String> = cfg["allowed_splits"].as_array().context("allowed_splits")?.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let mut reqs: Vec<plan::Request> = Vec::new();
    for v in read_jsonl(&rf)? {
        let r: plan::Request = serde_json::from_value(v)?;
        if window.contains_key(&r.item_id) {
            ensure!(allowed.contains(&r.split), "generator {generator} is not allowed on split {}", r.split);
            reqs.push(r);
        }
    }
    reqs.sort_by_key(|r| (window[&r.item_id], r.span_index));
    // The batch is the plan population: its binding names one profile
    // record. A v2 run writes it and holds its lock (an item's edits can
    // come from two generators' runs), then reads the journals again under
    // the lock.
    let batch_path = form_post::binding_path(runs, &format!("population-{population}"));
    let batch_ids: Vec<&str> = items.iter().filter(|i| i.population == population).flat_map(|i| i.edits.iter().map(|e| e.request_id.as_str())).collect();
    let earlier = |st: &HashMap<String, generation::ReqState>| batch_ids.iter().any(|id| st.get(*id).is_some_and(|s| !s.intents.is_empty() || !s.responses.is_empty()));
    let mut states = splice::load_states_light(&splice::run_dirs(runs)?)?;
    form_post::check_binding(&batch_path, form_rec.as_ref(), earlier(&states))?;
    let _lock = match &form {
        Some(f) if !dry => {
            form_post::write_binding(&batch_path, runs, f)?;
            let l = form_post::lock_binding(&batch_path)?;
            states = splice::load_states_light(&splice::run_dirs(runs)?)?;
            Some(l)
        }
        _ => None,
    };
    // items with several edits in the window, every generator's edits
    let specs: Vec<form_post::ItemSpec> = items
        .iter()
        .filter(|it| window.contains_key(&it.item_id) && it.edits.len() > 1)
        .map(|it| form_post::ItemSpec {
            item_id: it.item_id.clone(),
            parts: it.edits.iter().map(|e| (e.request_id.clone(), e.parent_span, e.insert_sep.clone())).collect(),
        })
        .collect();
    let form = form.map(|f| f.with_items(specs, &states)).transpose()?;
    let mut pending = Vec::new();
    for r in reqs {
        let st = states.get(&r.request_id).cloned().unwrap_or_default();
        if !st.done() {
            let first = st.used();
            pending.push((r, first));
        }
    }
    if dry {
        let mut rec = json!({"generator":generator,"population":population,"from":from,"to":to,"pending":pending.len(),"items_in_window":window.len()});
        if let (Some(r), Some(f)) = (form_rec, &form) {
            rec["profile"] = r;
            rec["profile_prior"] = f.prior_record();
            rec["profile_items"] = f.items_record();
            rec["profile_binding"] = json!(batch_path.display().to_string());
        }
        println!("{}", serde_json::to_string(&rec)?);
        return Ok(());
    }
    let out = runs.join(generator);
    fs::create_dir_all(&out)?;
    if pending.is_empty() {
        println!("{}", serde_json::to_string(&json!({"generator":generator,"population":population,"from":from,"to":to,"pending":0}))?);
        return Ok(());
    }
    let endpoint = cfg["endpoint"].as_str().context("endpoint")?.trim_end_matches('/').to_owned();
    let client = generation::client()?;
    let props: Value = client.get(format!("{endpoint}/props")).send()?.error_for_status()?.json()?;
    ensure!(props["model_path"] == cfg["model_path"], "server model path mismatch: {}", props["model_path"]);
    let models: Value = client.get(format!("{endpoint}/v1/models")).send()?.error_for_status()?.json()?;
    ensure!(models["data"].as_array().is_some_and(|a| a.iter().any(|m| m["id"] == cfg["alias"])), "server alias mismatch");
    let t0 = generation::now_ms();
    write_json(&out.join(format!("server-{t0}.json")), &json!({"props":props,"models":models,"config":cfg,"config_sha256":text::sha_hex(&cfg_bytes),
        "chat_template_sha256":props["chat_template"].as_str().map(|t| text::sha_hex(t.as_bytes()))}))?;
    let parents = load_parents(pdir)?;
    let mut ov = overlap::Overlap::new(overlap::PriorIndex::load(index)?);
    let _ = &mut ov;
    let mut preloaded = 0;
    for st in states.values() {
        if let Some(v) = st.accepted() {
            ov.add_generated(v["verdict"]["text"].as_str().unwrap_or(""));
            preloaded += 1;
        }
    }
    let n = pending.len();
    eprintln!("{generator} {population} [{from},{to}): {n} pending, {preloaded} accepted texts preloaded");
    let tok_path = PathBuf::from(meta["inputs"]["tokenizer"]["path"].as_str().context("tokenizer path")?);
    ensure!(file_sha(&tok_path)? == meta["inputs"]["tokenizer"]["sha256"], "tokenizer changed");
    let items_rec = form.as_ref().map(|f| f.items_record());
    let ((post, doc_words), vf) = form_post::spliced_wiring(form, edits)?;
    let ctx = Arc::new(generation::Ctx {
        endpoint,
        alias: cfg["alias"].as_str().unwrap().to_owned(),
        config: cfg.clone(),
        tokenizer: tokenizer(&tok_path)?,
        overlap: ov,
        parents: parents.into_iter().map(|(k, p)| (k, p.text)).collect(),
        client,
        post,
        doc_words,
    });
    let stats = generation::run_with(ctx.clone(), pending, &out, workers, vf)?;
    let mut rec = json!({"generator":generator,"population":population,"from":from,"to":to,"t0":t0,"t_end":generation::now_ms(),"ran":n,"stats":stats,
        "index_sha256":ctx.overlap.prior.sha256,"plan_sha256":file_sha(&pdir.join("plan.json"))?,"binary":std::env::current_exe()?.display().to_string(),
        "code":splice::CODE_ID,"journal_sync_policy":generation::JOURNAL_SYNC_POLICY});
    if let Some(f) = form_rec {
        rec["profile"] = f;
        rec["profile_items"] = items_rec.unwrap_or(Value::Null);
    }
    write_json(&out.join(format!("run-{t0}.json")), &rec)?;
    println!("{}", serde_json::to_string(&rec)?);
    Ok(())
}

fn run_progress(pdir: &Path, runs: &Path, from: usize, to: usize) -> Result<()> {
    let items = load_items(pdir)?;
    let states = splice::load_states_light(&splice::run_dirs(runs)?)?;
    let mut c: BTreeMap<String, BTreeMap<String, BTreeMap<String, usize>>> = BTreeMap::new();
    for it in items.iter().filter(|i| i.order >= from && i.order < to) {
        let pc = c.entry(it.population.clone()).or_default();
        let mut doc = "accepted";
        for e in &it.edits {
            let st = states.get(&e.request_id).cloned().unwrap_or_default();
            let s = if st.accepted().is_some() {
                "accepted"
            } else if st.done() {
                "failed"
            } else {
                "pending"
            };
            bump(pc, &format!("requests_{s}"), &e.generator);
            if s == "failed" {
                doc = "failed";
            } else if s == "pending" && doc == "accepted" {
                doc = "pending";
            }
        }
        bump(pc, "documents", doc);
    }
    println!("{}", serde_json::to_string_pretty(&c)?);
    Ok(())
}

// ---------------------------------------------------------------- finalize

struct Counters(BTreeMap<String, BTreeMap<String, BTreeMap<String, f64>>>);
impl Counters {
    fn add(&mut self, pop: &str, dim: &str, val: &str, x: f64) {
        *self.0.entry(pop.into()).or_default().entry(dim.into()).or_default().entry(val.into()).or_default() += x;
    }
    fn inc(&mut self, pop: &str, dim: &str, val: &str) {
        self.add(pop, dim, val, 1.0);
    }
}

fn words_bin(w: usize) -> &'static str {
    match w {
        0..=2 => "1-2",
        3..=5 => "3-5",
        6..=10 => "6-10",
        11..=20 => "11-20",
        21..=40 => "21-40",
        41..=80 => "41-80",
        81..=160 => "81-160",
        161..=320 => "161-320",
        _ => ">320",
    }
}

fn tokens_bin(n: usize) -> &'static str {
    match n {
        0..=255 => "<256",
        256..=511 => "256-511",
        512..=1023 => "512-1023",
        1024..=1535 => "1024-1535",
        _ => "1536-2048",
    }
}

#[allow(clippy::too_many_arguments)]
fn run_finalize(pdir: &Path, runs: &Path, configs: &Path, tok_path: &Path, train_target: usize, train_through: usize, out: &Path, label: &str, domain_cap: usize) -> Result<()> {
    let meta: Value = serde_json::from_slice(&fs::read(pdir.join("plan.json"))?)?;
    let plan_sha = file_sha(&pdir.join("plan.json"))?;
    ensure!(file_sha(tok_path)? == meta["inputs"]["tokenizer"]["sha256"], "tokenizer differs from the plan's");
    let tok = tokenizer(tok_path)?;
    let mut items = load_items(pdir)?;
    items.sort_by_key(|i| (i.population != splice::TRAIN, i.order));
    let parents = load_parents(pdir)?;
    let mut reqs: HashMap<String, plan::Request> = HashMap::new();
    for e in fs::read_dir(pdir.join("requests"))? {
        for v in read_jsonl(&e?.path())? {
            let r: plan::Request = serde_json::from_value(v)?;
            reqs.insert(r.request_id.clone(), r);
        }
    }
    let mut gens: HashMap<String, Value> = HashMap::new();
    for e in fs::read_dir(configs)? {
        let p = e?.path();
        if p.extension().is_some_and(|x| x == "json") {
            let v: Value = serde_json::from_slice(&fs::read(&p)?)?;
            let mut g = json!({"key":v["key"],"family":v["family"],"identity":v["identity"],"repo":v["repo"],"revision":v["revision"],"filename":v["filename"],
                "sha256":v["sha256"],"quantization":v["quantization"],"licence":v["licence"],"runtime":v["runtime"]});
            g["config_sha256"] = json!(file_sha(&p)?);
            gens.insert(v["key"].as_str().context("key")?.to_owned(), g);
        }
    }
    let states = splice::load_states_light(&splice::run_dirs(runs)?)?;
    let mut records: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut failures: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut q = Counters(BTreeMap::new());
    let mut dist = Counters(BTreeMap::new());
    let mut seen_norm: HashSet<u64> = HashSet::new();
    let mut seen_ws: HashSet<u64> = HashSet::new();
    let mut accepted_train = 0usize;
    let mut last_train_order: Option<usize> = None;
    let mut noop = (0usize, 0usize);
    // Train is a prefix in plan order: after the cut (target reached, bound
    // passed or the first item not yet generated) every later Train item is
    // skipped; the Dev-iter set is processed in full either way.
    let mut train_done = false;
    let mut accepted_by_domain: HashMap<String, usize> = HashMap::new();
    let mut capped_skips = 0usize;
    for it in &items {
        let pop = it.population.as_str();
        if pop == splice::TRAIN && (train_done || accepted_train >= train_target || it.order >= train_through) {
            train_done = true;
            continue;
        }
        if pop == splice::TRAIN && accepted_by_domain.get(&it.domain).copied().unwrap_or(0) >= domain_cap {
            q.inc(pop, "documents_skipped_domain_cap", &it.domain);
            capped_skips += 1;
            continue;
        }
        let parent = parents.get(&it.parent_id).context("parent")?;
        // no-op control: original spans reproduce the parent exactly
        let replaced: Vec<&Edit> = it.edits.iter().filter(|e| e.parent_span.0 < e.parent_span.1).collect();
        if !replaced.is_empty() {
            let orig: Vec<&str> = replaced.iter().map(|e| &parent.text[e.parent_span.0..e.parent_span.1]).collect();
            match splice::assemble(&parent.text, &replaced, &orig) {
                Ok(a) if a.text == parent.text => noop.0 += 1,
                _ => noop.1 += 1,
            }
        }
        let mut reps: Vec<String> = Vec::new();
        let mut status = "accepted";
        let mut edit_hist = Vec::new();
        let mut attempt_rows = Vec::new();
        for e in &it.edits {
            let st = states.get(&e.request_id).cloned().unwrap_or_default();
            for (a, v) in &st.responses {
                let acc = v["verdict"]["accepted"] == true;
                q.inc(pop, "attempts", if acc { "accepted" } else { "rejected" });
                q.inc(pop, &format!("attempt_{a}"), if acc { "accepted" } else { "rejected" });
                for r in v["verdict"]["reasons"].as_array().into_iter().flatten() {
                    let r = r.as_str().unwrap_or("?");
                    q.inc(pop, "rejection_reason", r);
                    q.inc(pop, &format!("rejection_reason:{}", e.generator), r);
                    q.inc(pop, &format!("rejection_reason_kind:{}", e.kind), r);
                    q.inc(pop, &format!("rejection_reason_op:{}", e.operation), r);
                }
                q.inc(pop, &format!("attempts_by_generator:{}", if acc { "accepted" } else { "rejected" }), &e.generator);
                q.inc(pop, &format!("attempts_by_operation:{}", if acc { "accepted" } else { "rejected" }), &e.operation);
                q.inc(pop, &format!("attempts_by_kind:{}", if acc { "accepted" } else { "rejected" }), &e.kind);
            }
            let tried: Vec<Value> = st.responses.iter().map(|(a, v)| {
                let mut d = v["verdict"]["diag"].clone();
                if let Some(o) = d.as_object_mut() {
                    o.remove("overlap");
                }
                json!({"attempt":a,"seed":v["seed"],"status":v["status"],"elapsed_ms":v["elapsed_ms"],"accepted":v["verdict"]["accepted"],
                    "reasons":v["verdict"]["reasons"],"diag":d,"usage":v["usage"]})
            }).collect();
            let uncertain: Vec<u32> = st.intents.iter().copied().filter(|a| !st.responses.contains_key(a)).collect();
            attempt_rows.push(json!({"edit_index":e.edit_index,"request_id":e.request_id,"generator":e.generator,"operation":e.operation,"kind":e.kind,
                "attempts":tried,"uncertain_attempts":uncertain}));
            match st.accepted() {
                Some(v) => {
                    reps.push(v["verdict"]["text"].as_str().context("accepted text")?.to_owned());
                    edit_hist.push((v["attempt"].as_u64().unwrap_or(0), v["seed"].clone(), v["verdict"]["diag"].clone()));
                }
                None => {
                    if st.done() {
                        status = "terminal_failure";
                    } else if status == "accepted" {
                        status = "not_run";
                    }
                }
            }
        }
        let fail = |status: &str, reasons: Vec<String>| json!({"schema":splice::FAILURE_SCHEMA,"item_id":it.item_id,"population":it.population,
            "split":it.split,"parent_id":it.parent_id,"pair_id":it.parent_id,"source_group":it.source_group,"domain":it.domain,"order":it.order,
            "policy":it.policy,"generators":it.generators,"stage":"generation","status":status,"reasons":reasons,"edits":attempt_rows});
        if status != "accepted" {
            q.inc(pop, "documents", status);
            q.inc(pop, &format!("documents_{status}_by_policy"), &it.policy);
            if status == "not_run" {
                // never generated: outside the generated window; not a failure of the item
                if pop == splice::TRAIN {
                    train_done = true;
                }
                continue;
            }
            failures.entry(pop.into()).or_default().push(fail(status, vec![]));
            if pop == splice::TRAIN {
                last_train_order = Some(it.order);
            }
            continue;
        }
        let eds: Vec<&Edit> = it.edits.iter().collect();
        let rr: Vec<&str> = reps.iter().map(String::as_str).collect();
        let asm = match splice::assemble(&parent.text, &eds, &rr) {
            Ok(a) => a,
            Err(err) => {
                q.inc(pop, "documents", "excluded_in_finalize");
                q.inc(pop, "finalize_exclusion", &format!("assembly:{err}"));
                failures.entry(pop.into()).or_default().push(fail("excluded_in_finalize", vec![format!("assembly:{err}")]));
                continue;
            }
        };
        let ntok = a_tokens(&tok, &asm.text)?;
        let mut why = Vec::new();
        if ntok > splice::MAX_DOC_TOKENS {
            why.push("assembled_over_2048_a_tokens".to_owned());
        }
        for r in &rr {
            if seen_norm.contains(&grammar_a_pools::text::text_keys(r)[1]) {
                why.push("generated_normalized_text_duplicate".into());
            }
            if grammar_a_pools::text::shingles_ws(r).iter().any(|x| seen_ws.contains(x)) {
                why.push("generated_50_word_sequence_duplicate".into());
            }
        }
        if !why.is_empty() {
            why.sort();
            why.dedup();
            q.inc(pop, "documents", "excluded_in_finalize");
            for w in &why {
                q.inc(pop, "finalize_exclusion", w);
            }
            failures.entry(pop.into()).or_default().push(fail("excluded_in_finalize", why));
            continue;
        }
        for r in &rr {
            seen_norm.insert(grammar_a_pools::text::text_keys(r)[1]);
            seen_ws.extend(grammar_a_pools::text::shingles_ws(r));
        }
        let share = asm.ai_words as f64 / asm.total_words as f64;
        let ai_char_spans: Vec<(usize, usize)> = asm.ai_byte_spans.iter().map(|&(a, b)| (splice::char_index(&asm.text, a), splice::char_index(&asm.text, b))).collect();
        let mut hist = Vec::new();
        for (k, e) in it.edits.iter().enumerate() {
            let r = &reqs[&e.request_id];
            let (fs, fe) = asm.edit_spans[k];
            let src = &parent.text[e.parent_span.0..e.parent_span.1];
            let rep = rr[k];
            let (attempt, seed, diag) = &edit_hist[k];
            let g = gens.get(&e.generator).cloned().unwrap_or(Value::Null);
            let cf = if e.kind == "insert" { None } else { Some(diverse::change_fraction(src, rep)) };
            hist.push(json!({
                "edit_index": e.edit_index, "policy": it.policy, "component": e.component, "kind": e.kind, "operation": e.operation,
                "operation_family": e.family, "operation_requested": e.operation_requested, "generator": e.generator, "generator_family": g["family"],
                "model_sha256": g["sha256"], "config_sha256": g["config_sha256"],
                "voice_id": e.style["voice_id"], "brief_id": e.style["brief_id"], "style_kind": e.style["style_kind"], "style_scope": e.style["style_scope"],
                "style_text_sha256": e.style["style_text"].as_str().map(|s| text::sha_hex(s.as_bytes())),
                "template_id": e.template_id, "prompt_sha256": text::sha_hex(r.prompt.as_bytes()), "request_id": e.request_id, "request_sha256": r.request_sha256,
                "decoding": r.decoding, "seed": seed, "accepted_attempt": attempt, "context_included": e.context_included,
                "parent_span_bytes": e.parent_span, "insert_separator": e.insert_sep,
                "final_span_bytes": [fs, fe], "final_span_chars": [splice::char_index(&asm.text, fs), splice::char_index(&asm.text, fe)],
                "before_sha256": text::sha_hex(src.as_bytes()), "after_sha256": text::sha_hex(rep.as_bytes()),
                "words_before": splice::py_words(src), "words_after": splice::py_words(rep), "change_fraction": cf,
                "finish_reason": diag["finish_reason"], "verdict": "accepted"
            }));
            dist.inc(pop, "edit_operation", &e.operation);
            dist.inc(pop, "edit_family", &e.family);
            dist.inc(pop, "edit_kind", &e.kind);
            dist.inc(pop, "edit_component", &e.component);
            dist.inc(pop, "edit_generator", &e.generator);
            dist.inc(pop, "edit_style_kind", e.style["style_kind"].as_str().unwrap_or("-"));
            dist.inc(pop, "edit_context_included", &e.context_included.to_string());
            dist.inc(pop, "edit_words_after", words_bin(splice::py_words(rep)));
            if let Some(cf) = cf {
                dist.inc(pop, "edit_change_fraction_band", splice::cf_band(cf));
                dist.inc(pop, &format!("edit_change_fraction_band:{}", e.operation), splice::cf_band(cf));
                dist.add(pop, "edit_change_fraction_sum", &e.operation, cf);
            }
        }
        let gl: Vec<String> = it.generators.iter().map(|k| gens.get(k).and_then(|g| g["family"].as_str()).unwrap_or(k).to_owned()).collect();
        let generator_label = if gl.len() == 1 { Some(gl[0].clone()) } else { None };
        let hist_bytes = serde_json::to_vec(&hist)?;
        let rec = json!({
            "schema": splice::RECORD_SCHEMA, "id": it.item_id, "population": it.population, "split": it.split, "frozen_split": it.split,
            "span_half": it.span_half, "parent_id": it.parent_id, "parent_text_sha256": it.parent_text_sha256, "pair_id": it.parent_id,
            "pair_policy": "the untouched parent is the paired human-only negative (already a release-bundle row: reuse it, no duplicate)",
            "parent_cohort_id": parent.cohort_id, "parent_provenance_sha256": parent.provenance_sha256,
            "source_group": it.source_group, "writer_key": it.writer_key, "domain": it.domain, "genre": parent.genre,
            "text": asm.text, "text_sha256": text::sha_hex(asm.text.as_bytes()), "origin": "mixed", "human_role_present": true, "ai_role_present": true,
            "source_kind": splice::SOURCE_KIND, "cohort_id": splice::COHORT_ID, "operation": format!("spliced:{}", it.policy),
            "policy": it.policy, "policy_requested": it.policy_requested, "variant": it.variant, "plan_order": it.order,
            "share": share, "share_bin": splice::share_bin(share), "word_policy": splice::WORD_POLICY,
            "words": asm.total_words, "ai_words": asm.ai_words, "a_tokens_with_specials": ntok,
            "ai_byte_spans": asm.ai_byte_spans, "ai_char_spans": ai_char_spans,
            "generator_label": generator_label, "generator_labels": gl, "generators": it.generators, "two_generators": it.two_generators,
            "voice_scope": it.voice_scope, "edits": hist, "edit_history_sha256": text::sha_hex(&hist_bytes),
            "context_basis": "original", "plan_sha256": plan_sha,
            "rights": {"inherits_parent": true, "generated_text": "local open-weight generation; model licence in the generator config; no detector output used"},
            "detector_outputs_used": false,
        });
        dist.inc(pop, "documents_policy", &it.policy);
        dist.inc(pop, "documents_domain", &it.domain);
        dist.inc(pop, "documents_share_bin", splice::share_bin(share));
        dist.inc(pop, "documents_edits", &it.edits.len().to_string());
        dist.inc(pop, "documents_spans", &asm.ai_byte_spans.len().to_string());
        dist.inc(pop, "documents_generator", generator_label.as_deref().unwrap_or("two_generators"));
        dist.inc(pop, "documents_voice_scope", &it.voice_scope);
        dist.inc(pop, "documents_a_tokens", tokens_bin(ntok));
        dist.inc(pop, "documents_variant", &it.variant.to_string());
        dist.add(pop, "sums", "share", share);
        dist.add(pop, "sums", "ai_words", asm.ai_words as f64);
        dist.add(pop, "sums", "words", asm.total_words as f64);
        dist.add(pop, "sums", "documents", 1.0);
        if let Some(h) = &it.span_half {
            dist.inc(pop, "documents_span_half", h);
        }
        q.inc(pop, "documents", "accepted");
        records.entry(pop.into()).or_default().push(rec);
        if pop == splice::TRAIN {
            accepted_train += 1;
            *accepted_by_domain.entry(it.domain.clone()).or_default() += 1;
            last_train_order = Some(it.order);
        }
    }
    // write
    let sp = |p: &Path| -> Result<()> {
        fs::create_dir_all(p)?;
        Ok(())
    };
    let mut files: BTreeMap<String, Value> = BTreeMap::new();
    let mut sums: Vec<(String, String)> = Vec::new();
    for pop in [splice::TRAIN, splice::DEV_SET] {
        let d = out.join("records").join(pop);
        sp(&d)?;
        let empty = Vec::new();
        let r = records.get(pop).unwrap_or(&empty);
        let s = write_jsonl(&d.join("records.jsonl"), r)?;
        sums.push((s.clone(), format!("records/{pop}/records.jsonl")));
        files.insert(format!("records/{pop}/records.jsonl"), json!({"rows":r.len(),"sha256":s}));
        let fdir = out.join("failures");
        sp(&fdir)?;
        let f = failures.get(pop).unwrap_or(&empty);
        let s = write_jsonl(&fdir.join(format!("{pop}.jsonl")), f)?;
        sums.push((s.clone(), format!("failures/{pop}.jsonl")));
        files.insert(format!("failures/{pop}.jsonl"), json!({"rows":f.len(),"sha256":s}));
    }
    // pairs: spliced id -> paired parent
    let mut pairs = Vec::new();
    for (pop, rs) in &records {
        for r in rs {
            pairs.push(json!({"id":r["id"],"population":pop,"pair_id":r["pair_id"],"parent_text_sha256":r["parent_text_sha256"],"split":r["split"],
                "source_group":r["source_group"],"span_half":r["span_half"]}));
        }
    }
    let s = write_jsonl(&out.join("records").join("pairs.jsonl"), &pairs)?;
    sums.push((s.clone(), "records/pairs.jsonl".into()));
    files.insert("records/pairs.jsonl".into(), json!({"rows":pairs.len(),"sha256":s}));
    let rdir = out.join("reports");
    sp(&rdir)?;
    let quality = json!({"schema":"slop_ninja_a_spliced_quality_report_v1","label":label,"plan_sha256":plan_sha,"counts":q.0,
        "noop_splice_control":{"exact_reconstructions":noop.0,"mismatches":noop.1},
        "checks":{"assembly":"every accepted document: human characters outside spans byte-identical to the parent minus replaced spans; spans on Python-whitespace boundaries; no word straddles a span edge; share = words inside spans / words; <= 2048 A tokens"}});
    let s = write_json(&rdir.join("quality.json"), &quality)?;
    sums.push((s.clone(), "reports/quality.json".into()));
    files.insert("reports/quality.json".into(), json!({"sha256":s}));
    let distribution = json!({"schema":"slop_ninja_a_spliced_distribution_report_v1","label":label,"plan_sha256":plan_sha,"counts":dist.0});
    let s = write_json(&rdir.join("distribution.json"), &distribution)?;
    sums.push((s.clone(), "reports/distribution.json".into()));
    files.insert("reports/distribution.json".into(), json!({"sha256":s}));
    sums.sort_by(|a, b| a.1.cmp(&b.1));
    let body: String = sums.iter().map(|(h, p)| format!("{h}  {p}\n")).collect();
    let tmp = out.join("SHA256SUMS.tmp");
    fs::write(&tmp, body.as_bytes())?;
    fs::rename(&tmp, out.join("SHA256SUMS"))?;
    let manifest = json!({"schema":"slop_ninja_a_spliced_data_manifest_v1","label":label,"code":splice::CODE_ID,"plan":pdir.display().to_string(),"plan_sha256":plan_sha,
        "runs":runs.display().to_string(),"tokenizer_sha256":file_sha(tok_path)?,"binary":std::env::current_exe()?.display().to_string(),
        "binary_sha256":file_sha(&std::env::current_exe()?)?,"train_target":train_target,"train_through":train_through,
        "train_accepted":accepted_train,"train_last_order_processed":last_train_order,"domain_cap":(domain_cap != usize::MAX).then_some(domain_cap),"skipped_domain_cap":capped_skips,
        "dev_iter_span_eval_accepted":records.get(splice::DEV_SET).map(Vec::len).unwrap_or(0),"files":files,
        "generators":gens,"record_schema":splice::RECORD_SCHEMA,"source_kind":splice::SOURCE_KIND,"cohort_id":splice::COHORT_ID,
        "note":"Train records are the first train_target accepted documents in plan order; the Dev-iter span-evaluation set is never a Train input"});
    write_json(&out.join("MANIFEST.json"), &manifest)?;
    println!("{}", serde_json::to_string(&json!({"train_accepted":accepted_train,"dev_accepted":manifest["dev_iter_span_eval_accepted"],
        "train_failures":failures.get(splice::TRAIN).map(Vec::len).unwrap_or(0),"dev_failures":failures.get(splice::DEV_SET).map(Vec::len).unwrap_or(0),
        "noop":[noop.0,noop.1],"train_last_order":last_train_order}))?);
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Plan { bundle, bundle_sha256, splits_v5, dev_iter_humans, library, partitions, partitions_sha256, fix_slop, tokenizer, raid_lineage, train_target, floor, cap, overprovision, dev_variants, out } => run_plan(
            &bundle, &bundle_sha256, &splits_v5, &dev_iter_humans, &library, &partitions, &partitions_sha256, &fix_slop, &tokenizer, &raid_lineage, train_target, floor, cap,
            overprovision, dev_variants, &out,
        ),
        Cmd::Generate { plan, population, generator, config, runs, index, workers, from, to, dry_run, profile, profile_population } => {
            let form_population = form_post::profile_population(profile.as_deref(), profile_population.as_deref())?;
            run_generate(&plan, &population, &generator, &config, &runs, &index, workers, from, to, dry_run, form_population.as_deref())
        }
        Cmd::Progress { plan, runs, from, to } => run_progress(&plan, &runs, from, to),
        Cmd::Finalize { plan, runs, configs, tokenizer, train_target, train_through, out, label, domain_cap } => {
            run_finalize(&plan, &runs, &configs, &tokenizer, train_target, train_through, &out, &label, domain_cap)
        }
        Cmd::Amend { data, tokenizer, out, label } => run_amend(&data, &tokenizer, &out, &label),
        Cmd::ScreenBundle { rows, rows_sha256, out } => run_screen_bundle(&rows, &rows_sha256, &out),
        Cmd::BundlePlus { rows, rows_sha256, weights, manifest, data, out } => {
            run_bundle_plus(&rows, &rows_sha256, &weights, &manifest, &data, &out)
        }
        Cmd::ProjectDev { data, half, out } => run_project_dev(&data, &half, &out),
    }
}

// ---------------------------------------------------------------- spans-v1.1 amendment

const AMEND_SCHEMA: &str = "slop_ninja_a_spliced_amendment_v1";

fn read_lines(path: &Path) -> Result<Vec<String>> {
    let f = BufReader::new(fs::File::open(path).with_context(|| path.display().to_string())?);
    let mut v = Vec::new();
    for l in f.lines() {
        let l = l?;
        if !l.trim().is_empty() {
            v.push(l);
        }
    }
    Ok(v)
}

fn span_pair(v: &Value) -> Result<(usize, usize)> {
    Ok((v[0].as_u64().context("span")? as usize, v[1].as_u64().context("span")? as usize))
}

/// spans-v1.1 amendment of the frozen snapshot (PROTOCOL-v1.1 A2-A5): drops
/// two-generator documents (A2) and documents with any meta-screen-v2 hit
/// (A3), normalizes typography in the keyed half (A4) and records copied-word
/// masks (A5). Exclusions carry identities and reasons, never text.
fn run_amend(data: &Path, tok_path: &Path, out: &Path, label: &str) -> Result<()> {
    use grammar_a_positives::splice_amend as am;
    ensure!(!out.exists(), "new output directory required");
    let snap: Value = serde_json::from_slice(&fs::read(data.join("SNAPSHOT.json"))?)?;
    let snap_sha = file_sha(&data.join("SNAPSHOT.json"))?;
    let rels = ["records/train/records.jsonl", "records/dev_iter_span_eval/records.jsonl", "plan/parents.jsonl", "plan/plan.json"];
    for rel in rels {
        ensure!(file_sha(&data.join(rel))? == snap["files"][rel]["sha256"].as_str().unwrap_or(""), "{rel} differs from SNAPSHOT.json");
    }
    let meta: Value = serde_json::from_slice(&fs::read(data.join("plan/plan.json"))?)?;
    ensure!(file_sha(tok_path)? == meta["inputs"]["tokenizer"]["sha256"], "tokenizer differs from the plan's");
    let tok = tokenizer(tok_path)?;
    let parents = load_parents(&data.join("plan"))?;
    let mut records: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    let mut excl: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    let mut c = Counters(BTreeMap::new());
    for pop in [splice::TRAIN, splice::DEV_SET] {
        for line in read_lines(&data.join("records").join(pop).join("records.jsonl"))? {
            let rec: Value = serde_json::from_str(&line)?;
            let id = rec["id"].as_str().context("id")?;
            let parent = parents.get(rec["parent_id"].as_str().context("parent_id")?).context("parent")?;
            ensure!(rec["parent_text_sha256"] == parent.text_sha256.as_str(), "{id}: parent digest");
            let text = rec["text"].as_str().context("text")?;
            let mut reasons: Vec<String> = Vec::new();
            let mut hits = Vec::new();
            if rec["two_generators"] == true {
                ensure!(rec["generator_label"].is_null(), "{id}: two generators with a label");
                reasons.push("two_generators_null_attribution_target".into());
            }
            for e in rec["edits"].as_array().context("edits")? {
                let (fs_, fe) = span_pair(&e["final_span_bytes"])?;
                for r in am::meta_screen(&text[fs_..fe], &parent.text) {
                    c.inc(pop, "meta_v2_edit_hits", &r);
                    c.inc(pop, &format!("meta_v2_edit_hits_by_generator:{r}"), e["generator"].as_str().unwrap_or("?"));
                    hits.push(json!({"edit_index":e["edit_index"],"generator":e["generator"],"operation":e["operation"],"kind":e["kind"],"reason":r}));
                    reasons.push(r);
                }
            }
            reasons.sort();
            reasons.dedup();
            let base = |k: &str| rec[k].clone();
            if !reasons.is_empty() {
                for r in &reasons {
                    c.inc(pop, "documents_excluded_by_reason", r);
                }
                c.inc(pop, "documents", "excluded");
                for g in rec["generators"].as_array().into_iter().flatten() {
                    c.inc(pop, "documents_excluded_by_generator", g.as_str().unwrap_or("?"));
                }
                excl.entry(pop).or_default().push(json!({"schema":AMEND_SCHEMA,"amendment":am::AMENDMENT,"id":id,"population":pop,"split":base("split"),
                    "parent_id":base("parent_id"),"pair_id":base("pair_id"),"source_group":base("source_group"),"domain":base("domain"),"policy":base("policy"),
                    "generators":base("generators"),"span_half":base("span_half"),"text_sha256":base("text_sha256"),
                    "source_record_sha256":text::sha_hex(line.as_bytes()),"reasons":reasons,"edit_hits":hits}));
                continue;
            }
            let mut new = am::amend_record(&rec, &parent.text)?;
            if new["text"] != rec["text"] {
                let n = a_tokens(&tok, new["text"].as_str().unwrap())?;
                ensure!(n <= splice::MAX_DOC_TOKENS, "{id}: normalized text over 2048 A tokens");
                new["a_tokens_with_specials"] = json!(n);
            }
            new["amendment"] = json!({"protocol":am::AMENDMENT,"schema":AMEND_SCHEMA,"source_snapshot":snap["label"],"source_snapshot_sha256":snap_sha,
                "source_record_sha256":text::sha_hex(line.as_bytes()),"source_text_sha256":rec["text_sha256"],"meta_screen":am::META_SCREEN,
                "typography_policy":am::TYPOGRAPHY_POLICY,"copy_rule":am::COPY_RULE});
            let stratum = new["typography"]["stratum"].as_str().unwrap_or("?").to_owned();
            c.inc(pop, "documents", "kept");
            c.inc(pop, "documents_policy", rec["policy"].as_str().unwrap_or("?"));
            c.inc(pop, "documents_typography_stratum", &stratum);
            c.inc(pop, &format!("documents_typography_changed:{stratum}"), &(new["typography"]["edits_changed"].as_u64().unwrap_or(0) > 0).to_string());
            if let Some(h) = rec["span_half"].as_str() {
                c.inc(pop, "documents_span_half", h);
                c.inc(pop, &format!("documents_span_half_stratum:{h}"), &stratum);
            }
            for e in new["edits"].as_array().into_iter().flatten() {
                let g = e["generator"].as_str().unwrap_or("?");
                let op = e["operation"].as_str().unwrap_or("?");
                c.inc(pop, "edits", &stratum);
                c.inc(pop, "edits_by_generator", g);
                c.inc(pop, &format!("edits_by_generator:{stratum}"), g);
                if e["typography"]["markers_raw"].as_object().is_some_and(|m| !m.is_empty()) {
                    c.inc(pop, &format!("edits_with_raw_marker:{stratum}"), g);
                }
                if e["typography"]["markers"].as_object().is_some_and(|m| !m.is_empty()) {
                    c.inc(pop, &format!("edits_with_final_marker:{stratum}"), g);
                }
                for (k, v) in e["typography"]["markers_raw"].as_object().into_iter().flatten() {
                    c.add(pop, &format!("raw_marker_chars:{g}"), k, v.as_f64().unwrap_or(0.0));
                }
                for (k, v) in e["typography"]["substitutions"].as_object().into_iter().flatten() {
                    c.add(pop, "substitutions", k, v.as_f64().unwrap_or(0.0));
                }
                let cw = e["copied"]["words"].as_f64().unwrap_or(0.0);
                let wa = e["copied"]["words_after"].as_f64().unwrap_or(0.0);
                let lr = e["copied"]["longest_run"].as_u64().unwrap_or(0);
                c.add(pop, "copied_words", g, cw);
                c.add(pop, "ai_words", g, wa);
                c.add(pop, "copied_words_by_operation", op, cw);
                c.add(pop, "ai_words_by_operation", op, wa);
                if lr >= 8 {
                    c.inc(pop, "edits_copied_run_ge_8", g);
                    c.inc(pop, "edits_copied_run_ge_8_by_operation", op);
                }
                if wa > 0.0 && 2 * lr as usize >= wa as usize {
                    c.inc(pop, "edits_copied_run_ge_half_output", g);
                }
            }
            records.entry(pop).or_default().push(new);
        }
    }
    // write
    let mut sums: Vec<(String, String)> = Vec::new();
    let mut files: BTreeMap<String, Value> = BTreeMap::new();
    let mut put = |rel: String, sha: String, rows: Option<usize>| {
        files.insert(rel.clone(), json!({"sha256":sha,"rows":rows}));
        sums.push((sha, rel));
    };
    let empty = Vec::new();
    for pop in [splice::TRAIN, splice::DEV_SET] {
        let d = out.join("records").join(pop);
        fs::create_dir_all(&d)?;
        let r = records.get(pop).unwrap_or(&empty);
        put(format!("records/{pop}/records.jsonl"), write_jsonl(&d.join("records.jsonl"), r)?, Some(r.len()));
        let x = out.join("exclusions");
        fs::create_dir_all(&x)?;
        let e = excl.get(pop).unwrap_or(&empty);
        put(format!("exclusions/{pop}.jsonl"), write_jsonl(&x.join(format!("{pop}.jsonl")), e)?, Some(e.len()));
    }
    let mut pairs = Vec::new();
    for (pop, rs) in &records {
        for r in rs {
            pairs.push(json!({"id":r["id"],"population":pop,"pair_id":r["pair_id"],"parent_text_sha256":r["parent_text_sha256"],"split":r["split"],
                "source_group":r["source_group"],"span_half":r["span_half"],"typography_stratum":r["typography"]["stratum"]}));
        }
    }
    put("records/pairs.jsonl".into(), write_jsonl(&out.join("records").join("pairs.jsonl"), &pairs)?, Some(pairs.len()));
    fs::create_dir_all(out.join("reports"))?;
    let report = json!({"schema":"slop_ninja_a_spliced_amendment_report_v1","amendment":am::AMENDMENT,"label":label,"source_snapshot":snap["label"],
        "source_snapshot_sha256":snap_sha,"meta_screen":am::META_SCREEN,"meta_patterns":am::META_V2.iter().map(|(k, p)| json!({"id":k,"pattern":p})).collect::<Vec<_>>(),
        "typography_policy":am::TYPOGRAPHY_POLICY,"copy_rule":am::COPY_RULE,"counts":c.0,
        "rules":{"A2":"two-generator documents (null attribution target) are dropped: the trainer row contract requires one generator label for every AI-involved row",
            "A3":"a document is dropped when any edit's output hits the meta screen v2 (matched text absent from the parent) or uses a script absent from the parent",
            "A4":"keyed half (ukey(typography, id) even) normalized to the parent's typography; whitespace stays whitespace, so words and share are unchanged",
            "A5":"per-edit copied-word runs: word LCS against the replaced human span after a typographic fold; inserts have none"}});
    put("reports/amend.json".into(), write_json(&out.join("reports").join("amend.json"), &report)?, None);
    sums.sort_by(|a, b| a.1.cmp(&b.1));
    let body: String = sums.iter().map(|(h, p)| format!("{h}  {p}\n")).collect();
    fs::write(out.join("SHA256SUMS"), body.as_bytes())?;
    let manifest = json!({"schema":"slop_ninja_a_spliced_data_manifest_v1","label":label,"amendment":am::AMENDMENT,"code":splice::CODE_ID,
        "source_snapshot":{"file":data.join("SNAPSHOT.json").display().to_string(),"sha256":snap_sha,"label":snap["label"]},
        "tokenizer_sha256":file_sha(tok_path)?,"binary":std::env::current_exe()?.display().to_string(),"binary_sha256":file_sha(&std::env::current_exe()?)?,
        "train_records":records.get(splice::TRAIN).map(Vec::len).unwrap_or(0),"dev_iter_span_eval_records":records.get(splice::DEV_SET).map(Vec::len).unwrap_or(0),
        "train_excluded":excl.get(splice::TRAIN).map(Vec::len).unwrap_or(0),"dev_iter_span_eval_excluded":excl.get(splice::DEV_SET).map(Vec::len).unwrap_or(0),
        "files":files,"record_schema":splice::RECORD_SCHEMA,"source_kind":splice::SOURCE_KIND,"cohort_id":splice::COHORT_ID,
        "note":"Amended copy of the frozen snapshot: no new generation and no refill; the Dev-iter span-evaluation set is never a Train input"});
    write_json(&out.join("MANIFEST.json"), &manifest)?;
    println!("{}", serde_json::to_string(&json!({"train":manifest["train_records"],"dev":manifest["dev_iter_span_eval_records"],
        "train_excluded":manifest["train_excluded"],"dev_excluded":manifest["dev_iter_span_eval_excluded"]}))?);
    Ok(())
}

/// Meta screen v2 over the release trainer bundle's mixed rows (Train,
/// Dev-iter, CAL). Dev-select and Test lines are skipped before parsing. A
/// span's exemption context is its row's human text plus the human-only rows
/// of the same split and source group. Output: identities and reasons only.
fn run_screen_bundle(rows: &Path, rows_sha: &str, out: &Path) -> Result<()> {
    use grammar_a_positives::splice_amend as am;
    ensure!(!out.exists(), "new report required");
    ensure!(file_sha(rows)? == rows_sha, "bundle rows differ from the given SHA-256");
    let wanted = ["train", "dev_iter", "cal"];
    let mut humans: HashMap<(String, String), String> = HashMap::new();
    let mut mixed: Vec<Value> = Vec::new();
    let mut skipped = 0usize;
    let f = BufReader::new(fs::File::open(rows)?);
    for line in f.lines() {
        let line = line?;
        if line.contains("\"split\":\"dev_select\"") || line.contains("\"split\":\"test\"") {
            skipped += 1;
            continue;
        }
        let r: Value = serde_json::from_str(&line)?;
        let split = r["split"].as_str().context("split")?;
        ensure!(wanted.contains(&split), "unexpected split {split}");
        match r["origin"].as_str() {
            Some("human_only") => {
                let k = (split.to_owned(), r["source_group"].as_str().context("group")?.to_owned());
                let h = humans.entry(k).or_default();
                h.push('\n');
                h.push_str(r["text"].as_str().context("text")?);
            }
            Some("mixed") => mixed.push(r),
            _ => {}
        }
    }
    let mut c = Counters(BTreeMap::new());
    let mut flagged = Vec::new();
    for r in &mixed {
        let split = r["split"].as_str().unwrap();
        let t = r["text"].as_str().context("text")?;
        let spans: Vec<(usize, usize)> = r["ai_byte_spans"].as_array().context("spans")?.iter().map(span_pair).collect::<Result<_>>()?;
        let mut ctx = String::new();
        let mut cur = 0;
        for &(s, e) in &spans {
            ctx.push_str(&t[cur..s]);
            ctx.push('\n');
            cur = e;
        }
        ctx.push_str(&t[cur..]);
        if let Some(h) = humans.get(&(split.to_owned(), r["source_group"].as_str().unwrap_or("").to_owned())) {
            ctx.push_str(h);
        }
        let mut reasons: Vec<String> = spans.iter().flat_map(|&(s, e)| am::meta_screen(&t[s..e], &ctx)).collect();
        reasons.sort();
        reasons.dedup();
        c.inc(split, "mixed_rows", "all");
        if reasons.is_empty() {
            continue;
        }
        c.inc(split, "mixed_rows", "flagged");
        for x in &reasons {
            c.inc(split, "flagged_by_reason", x);
        }
        let cohort = r["cohort_id"].as_str().unwrap_or("?");
        c.inc(split, "flagged_by_cohort", cohort);
        flagged.push(json!({"split":split,"id":r["id"],"text_sha256":r["text_sha256"],"cohort_id":cohort,"generator_label":r["generator_label"],
            "operation":r["operation"],"domain":r["domain"],"reasons":reasons}));
    }
    let report = json!({"schema":"slop_ninja_a_spans_bundle_meta_flags_v1","amendment":am::AMENDMENT,"rows":rows.display().to_string(),"rows_sha256":rows_sha,
        "meta_screen":am::META_SCREEN,"skipped_unparsed_dev_select_or_test_lines":skipped,
        "exemption":"matched text present in the row's human text or in the same split and source group's human-only rows; a script present there",
        "use":"Dev-iter and CAL flagged rows are excluded from span metrics and from the iso_sent/iso_span fits; Train flagged rows stay in training and are reported (PROTOCOL-v1.1 A3)",
        "counts":c.0,"flagged":flagged});
    write_json(out, &report)?;
    println!("{}", serde_json::to_string(&report["counts"])?);
    Ok(())
}

/// The v1.1 snapshot's records file for `key` ("train" or "dev_iter_span_eval"), hash-checked.
fn snapshot_records(data: &Path, key: &str) -> Result<(PathBuf, String, Value)> {
    let snap_path = data.join("SNAPSHOT.json");
    let snap: Value = serde_json::from_slice(&fs::read(&snap_path)?)?;
    ensure!(snap["label"] == "v1.1" && snap["schema"] == "slop_ninja_a_spans_data_snapshot_v1", "snapshot v1.1 required");
    let rel = format!("v1.1/records/{key}/records.jsonl");
    let want = snap["files"][&rel]["sha256"].as_str().context("snapshot records hash")?.to_owned();
    let path = data.join(&rel);
    ensure!(file_sha(&path)? == want, "{rel} differs from SNAPSHOT.json");
    Ok((path, want, json!({"file": snap_path, "sha256": file_sha(&snap_path)?, "label": "v1.1"})))
}

fn run_bundle_plus(rows: &Path, rows_sha: &str, weights: &Path, manifest: &Path, data: &Path, out: &Path) -> Result<()> {
    use grammar_a_positives::splice_bundle as sb;
    use std::io::Write;
    ensure!(!out.exists(), "new output directory required");
    ensure!(file_sha(rows)? == rows_sha, "bundle rows differ from the given SHA-256");
    let (rec_path, rec_sha, snap) = snapshot_records(data, "train")?;
    let release_weights: Value = serde_json::from_slice(&fs::read(weights)?)?;
    ensure!(release_weights["base_weighting"] == "assembler_row_weight_v1", "release weights must be row-form");
    // Pass 1: release Train rows (weights only), other kept lines verbatim.
    let mut existing: Vec<sb::Existing> = Vec::new();
    let mut train_lines: Vec<Value> = Vec::new();
    let mut eval_lines: Vec<String> = Vec::new();
    let mut skipped = 0usize;
    let mut counts = Counters(BTreeMap::new());
    let mut ids: HashSet<String> = HashSet::new();
    let mut humans: HashMap<String, (String, String, String)> = HashMap::new(); // id -> (split, group, domain)
    for line in BufReader::new(fs::File::open(rows)?).lines() {
        let line = line?;
        if line.contains("\"split\":\"dev_select\"") || line.contains("\"split\":\"test\"") {
            skipped += 1;
            continue;
        }
        let r: Value = serde_json::from_str(&line)?;
        let split = r["split"].as_str().context("split")?.to_owned();
        ensure!(!["dev_select", "test"].contains(&split.as_str()), "unexpected split text in a kept line");
        let id = r["id"].as_str().context("id")?.to_owned();
        ensure!(ids.insert(id.clone()), "duplicate id {id}");
        let origin = r["origin"].as_str().context("origin")?;
        counts.inc(&split, "origin", origin);
        if split == "train" || split == "train_mining" {
            if origin == "human_only" {
                humans.insert(id, (split.clone(), r["source_group"].as_str().context("group")?.into(),
                                   r["domain"].as_str().context("domain")?.into()));
            }
            existing.push(sb::Existing {
                domain: r["domain"].as_str().context("domain")?.into(),
                ai_involved: origin != "human_only",
                weight: r["cohort_weight"].as_f64().context("cohort weight")?,
            });
            train_lines.push(r);
        } else {
            ensure!(split == "dev_iter" || split == "cal", "unexpected split {split}");
            eval_lines.push(line);
        }
    }
    // Spliced Train records: pair, split and group checks.
    let mut spliced: Vec<Value> = Vec::new();
    for line in BufReader::new(fs::File::open(&rec_path)?).lines() {
        let r: Value = serde_json::from_str(&line?)?;
        let id = r["id"].as_str().context("id")?.to_owned();
        ensure!(ids.insert(id.clone()), "spliced id {id} collides with a bundle id");
        let pair = r["pair_id"].as_str().context("pair_id")?;
        let (ps, pg, pd) = humans.get(pair).with_context(|| format!("{id}: pair {pair} is not a release Train human"))?;
        ensure!(r["split"] == "train" && ps == "train", "{id}: spliced and paired rows must be Train");
        ensure!(r["source_group"] == pg.as_str() && r["domain"] == pd.as_str(), "{id}: pair group/domain differ");
        spliced.push(r);
    }
    let domains: Vec<String> = spliced.iter().map(|r| r["domain"].as_str().unwrap().to_owned()).collect();
    let w = sb::plus_weights(&existing, &domains)?;
    fs::create_dir_all(out)?;
    let tmp = out.join("rows.jsonl.tmp");
    let mut f = std::io::BufWriter::new(fs::File::create(&tmp)?);
    // Per cohort/label weight statistics for weights.json (row form).
    let mut stats: BTreeMap<String, BTreeMap<String, (usize, f64, f64, f64)>> = BTreeMap::new();
    let mut note = |cohort: &str, label: &str, x: f64| {
        let e = stats.entry(cohort.into()).or_default().entry(label.into()).or_insert((0, 0., f64::INFINITY, f64::NEG_INFINITY));
        e.0 += 1;
        e.1 += x;
        e.2 = e.2.min(x);
        e.3 = e.3.max(x);
    };
    for (i, mut r) in train_lines.into_iter().enumerate() {
        r["cohort_weight"] = json!(w.existing[i]);
        r["token_weight"] = json!(w.existing_token[i]);
        let label = r["generator_label"].as_str().unwrap_or("human").to_owned();
        note(r["cohort_id"].as_str().context("cohort")?, &label, w.existing[i]);
        serde_json::to_writer(&mut f, &r)?;
        f.write_all(b"\n")?;
    }
    for line in &eval_lines {
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
    }
    let mut pol = Counters(BTreeMap::new());
    for (i, r) in spliced.iter().enumerate() {
        let row = sb::project(r, "train", w.spliced[i], Some(w.spliced_token[i]))?;
        note(sb::SPLICED_COHORT, r["generator_label"].as_str().unwrap(), w.spliced[i]);
        pol.inc("train", "policy", r["policy"].as_str().unwrap_or("?"));
        pol.inc("train", "generator", r["generator_label"].as_str().unwrap());
        counts.inc("train", "origin", "mixed(spliced)");
        serde_json::to_writer(&mut f, &row)?;
        f.write_all(b"\n")?;
    }
    f.flush()?;
    f.into_inner()?.sync_all()?;
    fs::rename(&tmp, out.join("rows.jsonl"))?;
    let mut cohorts = serde_json::Map::new();
    let release_cohorts = release_weights["cohorts"].as_object().context("release cohorts")?;
    for (cohort, labels) in &stats {
        let base = match release_cohorts.get(cohort) {
            Some(c) => json!({"policy": c["policy"], "default": c["default"], "cohort_weight": c["cohort_weight"]}),
            None => json!({"policy": sb::RULE, "default": false, "cohort_weight": 1.0}),
        };
        let mut c = base;
        c["labels"] = json!(labels.iter().map(|(k, (n, s, lo, hi))| (k.clone(),
            json!({"records": n, "weight_sum": s, "weight_min": lo, "weight_max": hi}))).collect::<BTreeMap<_, _>>());
        cohorts.insert(cohort.clone(), c);
    }
    let train_sum: f64 = w.existing.iter().sum::<f64>() + w.spliced.iter().sum::<f64>();
    let weights_doc = json!({"schema": "slop_ninja_a_trainer_weights_v1", "base_weighting": "assembler_row_weight_v1",
        "rule": format!("{}: release assembler row weights; spliced_rewrite_v1 rows take 25% of each spliced domain's AI-involved mass, \
                         that domain's release AI-involved rows keep 75%; token_weight: spliced 60%, other rows their release row-weight proportions", sb::RULE),
        "release_weights_sha256": file_sha(weights)?, "train_weight_sum": train_sum, "cohorts": cohorts});
    let wsha = write_json(&out.join("weights.json"), &weights_doc)?;
    let rsha = file_sha(&out.join("rows.jsonl"))?;
    let m = json!({"schema": "slop_ninja_a_spans_plus_bundle_v1", "protocol": "spans-v1.1 Sections 3.2, 3.9, 5.1",
        "inputs": {"release_rows": rows, "release_rows_sha256": rows_sha, "release_weights_sha256": file_sha(weights)?,
                   "release_manifest_sha256": file_sha(manifest)?, "spliced_records": rec_path, "spliced_records_sha256": rec_sha,
                   "snapshot": snap},
        "outputs": {"rows.jsonl": rsha, "weights.json": wsha},
        "skipped_unparsed_dev_select_or_test_lines": skipped, "rows_by_split_origin": counts.0, "spliced": pol.0,
        "weighting": w.report, "dev_select_included": false, "test_included": false, "detector_outputs_used": false,
        "note": "Train lines are re-serialized with the new cohort_weight and a token_weight; Dev-iter and CAL lines are byte-identical to the release bundle; Dev-select and Test lines are absent"});
    write_json(&out.join("MANIFEST.json"), &m)?;
    println!("{}", serde_json::to_string(&json!({"rows_sha256": rsha, "weights_sha256": wsha, "spliced": spliced.len(),
        "weighting": w.report["spliced_share_of_ai_involved_mass"], "human_share": [w.report["human_share_release"], w.report["human_share_plus"]]}))?);
    Ok(())
}

fn run_project_dev(data: &Path, half: &str, out: &Path) -> Result<()> {
    use grammar_a_positives::splice_bundle as sb;
    ensure!(half == "span_select" || half == "span_report", "half must be span_select or span_report");
    ensure!(!out.exists(), "new output required");
    let (rec_path, rec_sha, snap) = snapshot_records(data, "dev_iter_span_eval")?;
    let mut rows = Vec::new();
    for line in BufReader::new(fs::File::open(&rec_path)?).lines() {
        let line = line?;
        // Only this half's records are parsed; the other half's lines are skipped unparsed.
        if !line.contains(&format!("\"span_half\":\"{half}\"")) {
            continue;
        }
        let r: Value = serde_json::from_str(&line)?;
        ensure!(r["span_half"] == half, "span half marker inside text");
        rows.push(sb::project(&r, "dev_iter", 1.0, None)?);
    }
    let sha = write_jsonl(out, &rows)?;
    let receipt = json!({"schema": "slop_ninja_a_spans_dev_projection_v1", "half": half, "rows": rows.len(), "rows_sha256": sha,
        "records": rec_path, "records_sha256": rec_sha, "snapshot": snap, "use": "span evaluation only; never a Train input"});
    write_json(&out.with_extension("receipt.json"), &receipt)?;
    println!("{}", serde_json::to_string(&receipt)?);
    Ok(())
}

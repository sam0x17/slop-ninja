#![recursion_limit = "512"]
//! Spliced-rewrite span data generator (A span protocol spans-v1, Section 3).
//!
//! `plan` builds the keyed plan from the release trainer bundle's Train and
//! Dev-iter humans; `generate` runs one generator's pending requests in an
//! order window against a running llama-server; `progress` reports state;
//! `finalize` assembles accepted documents, retains failures and writes the
//! manifests and reports. All outputs hold private prose: write them only to
//! ignored storage.
//!
//! **R-form** (protocol A2 §7.2 and §7.5, amendment 19; B3b). `finalize`
//! emits raw twins and `rform_screen_fail_v1` whenever its journals'
//! verdicts carry profile v2's raw-response digest, as the local `finalize`
//! does, through its helpers (`finalize::raw_twin_of`, `screen_fail_of`,
//! `first_raw_response`, `failed_form_screen_only`); with the profile off its
//! outputs are byte for byte those of the code before. A raw twin is the
//! accepted raw responses (each verdict's `raw_text`, digest-checked)
//! assembled into the parent as the accepted texts are
//! (`splice::assemble_parts`, insertions with their separator), under the
//! accepted record's `raw_twin` with the N-form record's labels; it takes the
//! N-form document's window verdict. The stratum document is every edit's
//! first raw response, accepted ones included, assembled the same way and
//! window-checked here, under the record's or failure's
//! `rform_screen_fail_v1` (a left-out note without text on a window or
//! assembly failure). An item not yet generated (`not_run`) has no entry and
//! so no stratum outcome (amendment 25 item 25.4). The R-form documents
//! of one run are unique by text.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_a_positives::splice::{self, Edit, Item, Parent};
use grammar_a_positives::{diverse, finalize, form_post, generation, library, overlap, plan, text, write_json, write_jsonl};
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
        /// Item-id list (one per line; `{GEN}` is the generator key) that
        /// replaces the order window: only the listed items of the population
        /// run, in list order, each with an edit of this generator (D8's
        /// pilot and bulk, B5b part 2). The prior stays the population's.
        #[arg(long)]
        request_order: Option<PathBuf>,
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
    request_order: Option<&Path>,
) -> Result<Option<Value>> {
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
    // an order file (D8's pilot or bulk) replaces the window: the listed
    // items, in list order, each an item of the population with an edit of
    // this generator
    let listed: Option<(HashMap<String, usize>, Value)> = match request_order {
        Some(p) => {
            let b = fs::read(p).with_context(|| p.display().to_string())?;
            let ids = grammar_a_positives::order::parse_order(std::str::from_utf8(&b)?)?;
            let mut pos = HashMap::new();
            for (k, id) in ids.iter().enumerate() {
                ensure!(pos.insert(id.clone(), k).is_none(), "{}: item {id} is listed twice", p.display());
            }
            Some((pos, json!({"path": p.display().to_string(), "sha256": text::sha_hex(&b), "items": ids.len()})))
        }
        None => None,
    };
    for it in &items {
        let at = match &listed {
            Some((pos, _)) => pos.get(&it.item_id).copied().filter(|_| it.population == population),
            None => (it.population == population && it.order >= from && it.order < to).then_some(it.order),
        };
        if let Some(at) = at {
            window.insert(it.item_id.clone(), at);
            for e in &it.edits {
                if e.generator == generator {
                    edits.insert(e.request_id.clone(), e.clone());
                }
            }
        }
    }
    if let Some((pos, rec)) = &listed {
        for id in pos.keys() {
            let it = items.iter().find(|i| &i.item_id == id && i.population == population);
            ensure!(it.is_some_and(|i| i.edits.iter().any(|e| e.generator == generator)), "{}: item {id} is not a {population} item with an edit of {generator}", rec["path"]);
        }
    }
    let order_rec = listed.as_ref().map(|(_, r)| r.clone());
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
        if let Some(o) = &order_rec {
            rec["request_order"] = o.clone();
            let mut its: Vec<(&usize, &String)> = window.iter().map(|(k, v)| (v, k)).collect();
            its.sort();
            rec["item_order"] = json!(its.into_iter().map(|(_, k)| k).collect::<Vec<_>>());
            rec["pending_items"] = json!(pending.iter().map(|(r, _)| r.item_id.as_str()).collect::<std::collections::BTreeSet<_>>());
        }
        if let (Some(r), Some(f)) = (form_rec, &form) {
            rec["profile"] = r;
            rec["profile_prior"] = f.prior_record();
            rec["profile_items"] = f.items_record();
            rec["profile_binding"] = json!(batch_path.display().to_string());
        }
        println!("{}", serde_json::to_string(&rec)?);
        return Ok(Some(rec));
    }
    let out = runs.join(generator);
    fs::create_dir_all(&out)?;
    if pending.is_empty() {
        println!("{}", serde_json::to_string(&json!({"generator":generator,"population":population,"from":from,"to":to,"pending":0}))?);
        return Ok(None);
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
    if let Some(o) = order_rec {
        rec["request_order"] = o;
    }
    write_json(&out.join(format!("run-{t0}.json")), &rec)?;
    println!("{}", serde_json::to_string(&rec)?);
    Ok(None)
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

/// A spliced raw twin's rule, as its record states it (protocol A2 §7.2 and
/// §7.5; amendment 19).
const RAW_TWIN_RULE: &str = "protocol A2 sections 7.2 and 7.5 raw twin of a spliced document, as finalize's (amendment 19): the item's accepted responses spliced raw into the same parent by splice::assemble_parts (insertions with their separator), as finalize assembles the accepted laid-out texts; each raw child is the accepted verdict's raw_text, read exactly and refused unless its SHA-256 is the verdict's profile v2 raw_response_sha256 (finalize::raw_twin_of); the parent must match parent_text_sha256 and an item check's record must name the same raw digests; the N-form record's labels, with text, spans, share, words, tokens and edit history measured on this document; the window verdict is the N-form document's; the R-form documents of one run (both populations) are unique by text, a duplicate refusing the run (item 19.6)";
/// The spliced screen-fail stratum's rule, as its record states it.
const SCREEN_FAIL_RULE: &str = "protocol A2 sections 7.2 and 7.5 rform_screen_fail_v1 of a spliced item, as read by amendment 19 items 19.1, 19.2 and 19.4 (finalize::screen_fail_of): each edit's request's first raw response is its lowest attempt whose response verdict carries none of transport_error, http_status_<code> or no_content, accepted or not (a response rejected before the profile step, or with empty content, is one; an intent with no response is not); an item whose first raw responses include a rejection, every rejected one failing the form screen only (each reason one of the post record's form_reasons and a form_line_, form_paragraph_ or form_frame_ code; the spliced generator's own checks are outside the screen), gets one document: every edit's first raw response, accepted ones included, spliced into the parent by splice::assemble_parts (insertions with their separator), window-checked at finalize (cc_final_length_window_v1); a request with no raw response, or any other rejection, keeps the item out; a window or assembly failure leaves the document out, noted without text; no N-form twin; an item not yet generated (not_run) has no entry and no stratum outcome, since its entry is where that outcome rides (amendment 25 item 25.4); the R-form documents of one run are unique by text, a duplicate refusing the run (item 19.6)";

/// What a spliced document's record reads besides the item and its text.
struct RecCtx<'a> {
    reqs: &'a HashMap<String, plan::Request>,
    gens: &'a HashMap<String, Value>,
    plan_sha: &'a str,
}

/// Per edit: the attempt, seed and `diag` of the response the document
/// splices there, and that response's verdict (`accepted` or `rejected`).
type EditOutcome = (u64, Value, Value, &'static str);

/// The record of one spliced document of item `it`: `asm`, assembled from
/// `rr` (one text per edit, in edit order), with `outcomes` per edit and
/// `ntok` A tokens. The N-form record and every R-form document take it.
fn doc_record(c: &RecCtx, it: &Item, parent: &Parent, asm: &splice::Assembled, rr: &[&str], outcomes: &[EditOutcome], ntok: usize) -> Value {
    let share = asm.ai_words as f64 / asm.total_words as f64;
    let ai_char_spans: Vec<(usize, usize)> = asm.ai_byte_spans.iter().map(|&(a, b)| (splice::char_index(&asm.text, a), splice::char_index(&asm.text, b))).collect();
    let mut hist = Vec::new();
    for (k, e) in it.edits.iter().enumerate() {
        let r = &c.reqs[&e.request_id];
        let (fs, fe) = asm.edit_spans[k];
        let src = &parent.text[e.parent_span.0..e.parent_span.1];
        let rep = rr[k];
        let (attempt, seed, diag, verdict) = &outcomes[k];
        let g = c.gens.get(&e.generator).cloned().unwrap_or(Value::Null);
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
            "finish_reason": diag["finish_reason"], "verdict": verdict
        }));
    }
    let gl: Vec<String> = it.generators.iter().map(|k| c.gens.get(k).and_then(|g| g["family"].as_str()).unwrap_or(k).to_owned()).collect();
    let generator_label = if gl.len() == 1 { Some(gl[0].clone()) } else { None };
    let hist_bytes = serde_json::to_vec(&hist).expect("edit history serialises");
    json!({
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
        "context_basis": "original", "plan_sha256": c.plan_sha,
        "rights": {"inherits_parent": true, "generated_text": "local open-weight generation; model licence in the generator config; no detector output used"},
        "detector_outputs_used": false,
    })
}

/// The edits' parts as assembly reads them, in edit order.
fn parts_of(it: &Item) -> Vec<splice::Part<'_>> {
    it.edits.iter().map(|e| splice::Part { parent_span: e.parent_span, insert_sep: e.insert_sep.as_deref() }).collect()
}

/// An R-form document's splice: raw children assembled into the parent as
/// finalize assembles accepted texts (`splice::assemble_parts`), with the
/// realised word share.
fn raw_splice(parent: &str, parts: &[splice::Part], raws: &[&str]) -> Result<finalize::Spliced> {
    let a = splice::assemble_parts(parent, parts, raws)?;
    let share = a.ai_words as f64 / a.total_words as f64;
    Ok((a.text, a.ai_byte_spans, share))
}

fn check_parent(it: &Item, parent: &Parent) -> Result<()> {
    ensure!(text::sha_hex(parent.text.as_bytes()) == it.parent_text_sha256, "{}: the parent text does not match parent_text_sha256", it.item_id);
    Ok(())
}

/// The record's assembly (with each edit's span) is the document the shared
/// rule gave: the same text, spans and share.
fn same_doc(it: &Item, d: &finalize::RawDoc, asm: &splice::Assembled) -> Result<()> {
    let share = asm.ai_words as f64 / asm.total_words as f64;
    ensure!(asm.text == d.text && asm.ai_byte_spans == d.ai_spans && share == d.share, "{}: the R-form record's assembly differs from its document", it.item_id);
    Ok(())
}

/// Marks a spliced document record as an R-form document.
fn rform_fields(d: &mut Value, id: String, stratum: &str, twin_of: Option<&str>, evidence: Value) {
    d["id"] = json!(id);
    d["form"] = json!(finalize::R_FORM);
    d["stratum"] = json!(stratum);
    d["twin_of"] = json!(twin_of);
    d["rform"] = evidence;
}

/// The raw twin of an accepted spliced item whose N-form text is `n_text`
/// (`finalize::raw_twin_of` with the spliced assembly). `Ok(None)` when its
/// accepted verdicts carry no profile v2 raw-response digest.
fn twin_record(c: &RecCtx, it: &Item, parent: &Parent, states: &HashMap<String, generation::ReqState>, tok: &tokenizers::Tokenizer, n_text: &str, edit_hist: &[EditOutcome]) -> Result<Option<Value>> {
    let rids: Vec<String> = it.edits.iter().map(|e| e.request_id.clone()).collect();
    let parts = parts_of(it);
    let Some(d) = finalize::raw_twin_of(&it.item_id, &rids, states, || check_parent(it, parent), |raws| raw_splice(&parent.text, &parts, raws))? else {
        return Ok(None);
    };
    let raws: Vec<&str> = d.raws.iter().map(String::as_str).collect();
    let asm = splice::assemble_parts(&parent.text, &parts, &raws)?;
    same_doc(it, &d, &asm)?;
    let mut t = doc_record(c, it, parent, &asm, &raws, edit_hist, a_tokens(tok, &asm.text)?);
    let same = grammar_a_pools::text::folded_tokens(&asm.text) == grammar_a_pools::text::folded_tokens(n_text);
    let ev = json!({"rule": RAW_TWIN_RULE, "parent_text_sha256": it.parent_text_sha256, "raw_responses": finalize::responses_record(&d.responses),
        "n_form_text_sha256": text::sha_hex(n_text.as_bytes()), "folded_tokens_equal_n_form": same, "window": "the N-form document's verdict (protocol A2 section 7.2)"});
    rform_fields(&mut t, format!("{}{}", finalize::RAW_TWIN_ID_PREFIX, it.item_id), finalize::RAW_TWIN_STRATUM, Some(&it.item_id), ev);
    Ok(Some(t))
}

/// The spliced item's `rform_screen_fail_v1` outcome for its entry
/// (`finalize::screen_fail_of` with the spliced assembly): the document, a
/// left-out note without text, or `None` when the item is not in the
/// stratum. Counted in `q`.
fn stratum_entry(c: &RecCtx, it: &Item, parent: &Parent, states: &HashMap<String, generation::ReqState>, tok: &tokenizers::Tokenizer, q: &mut Counters) -> Result<Option<Value>> {
    use finalize::{SCREEN_FAIL_ID, ScreenFail};
    use grammar_a_form::window::{LENGTH_WINDOW, LENGTH_WINDOW_ID};
    let pop = it.population.as_str();
    let rids: Vec<String> = it.edits.iter().map(|e| e.request_id.clone()).collect();
    let parts = parts_of(it);
    let Some(sf) = finalize::screen_fail_of(&it.item_id, &rids, states, || check_parent(it, parent), |raws| raw_splice(&parent.text, &parts, raws))? else {
        return Ok(None);
    };
    let id = format!("{SCREEN_FAIL_ID}:{}", it.item_id);
    Ok(Some(match sf {
        ScreenFail::Doc(d) => {
            q.inc(pop, SCREEN_FAIL_ID, "emitted");
            let raws: Vec<&str> = d.raws.iter().map(String::as_str).collect();
            let asm = splice::assemble_parts(&parent.text, &parts, &raws)?;
            same_doc(it, &d, &asm)?;
            let outcomes: Vec<EditOutcome> = d
                .responses
                .iter()
                .map(|r| {
                    let line = &states[&r.request_id].responses[&r.attempt];
                    (u64::from(r.attempt), line["seed"].clone(), line["verdict"]["diag"].clone(), if r.accepted { "accepted" } else { "rejected" })
                })
                .collect();
            let mut doc = doc_record(c, it, parent, &asm, &raws, &outcomes, a_tokens(tok, &asm.text)?);
            let ev = json!({"rule": SCREEN_FAIL_RULE, "parent_text_sha256": it.parent_text_sha256, "raw_responses": finalize::responses_record(&d.responses),
                "length_window": {"id": LENGTH_WINDOW_ID, "min_words": LENGTH_WINDOW.0, "max_words": LENGTH_WINDOW.1, "words": text::words(&asm.text), "pass": true}});
            rform_fields(&mut doc, id, SCREEN_FAIL_ID, None, ev);
            doc
        }
        ScreenFail::LeftOut { reasons, words, responses } => {
            for r in &reasons {
                q.inc(pop, SCREEN_FAIL_ID, &format!("left_out:{}", r.split(':').next().unwrap_or(r)));
            }
            json!({"stratum": SCREEN_FAIL_ID, "id": id, "left_out": true, "reasons": reasons, "words": words,
                "length_window": {"id": LENGTH_WINDOW_ID, "min_words": LENGTH_WINDOW.0, "max_words": LENGTH_WINDOW.1},
                "raw_responses": finalize::responses_record(&responses), "parent_text_sha256": it.parent_text_sha256, "rule": SCREEN_FAIL_RULE})
        }
    }))
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
    // R-form text digests -> the first document's id (amendment 19 item 19.6)
    let mut seen_rform: HashMap<String, String> = HashMap::new();
    let rc = RecCtx { reqs: &reqs, gens: &gens, plan_sha: &plan_sha };
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
                    edit_hist.push((v["attempt"].as_u64().unwrap_or(0), v["seed"].clone(), v["verdict"]["diag"].clone(), "accepted"));
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
        // The screen-fail stratum reads the item's first raw responses,
        // whatever the item's outcome (protocol A2 §7.2; amendment 19); it
        // rides with the item's entry, and an item not yet generated has
        // none (amendment 25 item 25.4).
        let stratum = if status == "not_run" { None } else { stratum_entry(&rc, it, parent, &states, &tok, &mut q)? };
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
            let mut f = fail(status, vec![]);
            finalize::put_stratum(&mut f, stratum, &mut seen_rform)?;
            failures.entry(pop.into()).or_default().push(f);
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
                let mut f = fail("excluded_in_finalize", vec![format!("assembly:{err}")]);
                finalize::put_stratum(&mut f, stratum, &mut seen_rform)?;
                failures.entry(pop.into()).or_default().push(f);
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
            let mut f = fail("excluded_in_finalize", why);
            finalize::put_stratum(&mut f, stratum, &mut seen_rform)?;
            failures.entry(pop.into()).or_default().push(f);
            continue;
        }
        for r in &rr {
            seen_norm.insert(grammar_a_pools::text::text_keys(r)[1]);
            seen_ws.extend(grammar_a_pools::text::shingles_ws(r));
        }
        let share = asm.ai_words as f64 / asm.total_words as f64;
        let mut rec = doc_record(&rc, it, parent, &asm, &rr, &edit_hist, ntok);
        for (k, e) in it.edits.iter().enumerate() {
            let src = &parent.text[e.parent_span.0..e.parent_span.1];
            let rep = rr[k];
            let cf = if e.kind == "insert" { None } else { Some(diverse::change_fraction(src, rep)) };
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
        let generator_label = rec["generator_label"].as_str().map(String::from);
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
        // The raw twin (protocol A2 §7.2): the accepted raw responses spliced into the parent.
        if let Some(twin) = twin_record(&rc, it, parent, &states, &tok, &asm.text, &edit_hist)? {
            q.inc(pop, "rform_raw_twin", "emitted");
            q.inc(pop, "rform_raw_twin_folded_tokens", if twin["rform"]["folded_tokens_equal_n_form"] == true { "equal_n_form" } else { "differ_from_n_form" });
            finalize::unique_rform(&mut seen_rform, &twin)?;
            rec[finalize::RAW_TWIN_FIELD] = twin;
        }
        finalize::put_stratum(&mut rec, stratum, &mut seen_rform)?;
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
        Cmd::Generate { plan, population, generator, config, runs, index, workers, from, to, dry_run, profile, profile_population, request_order } => {
            let form_population = form_post::profile_population(profile.as_deref(), profile_population.as_deref())?;
            ensure!(request_order.is_none() || (from == 0 && to == usize::MAX), "--request-order replaces the order window: no --from or --to");
            let order = request_order.map(|p| PathBuf::from(p.to_string_lossy().replace(grammar_a_positives::order::GEN_PLACEHOLDER, &generator)));
            run_generate(&plan, &population, &generator, &config, &runs, &index, workers, from, to, dry_run, form_population.as_deref(), order.as_deref()).map(|_| ())
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

#[cfg(test)]
mod tests {
    //! `finalize`'s R-form outputs (protocol A2 §7.2 and §7.5, amendment 19)
    //! on a synthetic plan: synthetic text only, no model, detector or
    //! network call. Verdicts come from the spliced acceptance functions
    //! themselves (`form_post::spliced_wiring`), journaled as `generate`
    //! journals them.
    use super::*;
    use grammar_a_positives::finalize::{self, R_FORM, RAW_TWIN_FIELD, RAW_TWIN_STRATUM, SCREEN_FAIL_ID};
    use grammar_a_positives::generation::Ctx;
    use grammar_a_positives::overlap::{Overlap, PriorIndex};

    const SYN: [&str; 20] = [
        "amber", "basalt", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper", "kestrel", "lantern", "meadow",
        "nectar", "orchid", "pebble", "quartz", "river", "saffron", "timber",
    ];
    const ALT: [&str; 20] = [
        "acorn", "bramble", "canyon", "dune", "estuary", "fern", "glacier", "heather", "inlet", "jasper", "knoll", "lichen", "marsh",
        "nettle", "oasis", "prairie", "quarry", "ridge", "sorrel", "thicket",
    ];

    /// `n` sentences of `len` words drawn by a seeded LCG, capitalised and
    /// ending with a full stop; in the first `dashed` sentences words 2 and 3
    /// are joined by a closed em dash.
    fn prose_d(vocab: &[&str], seed: u64, n: usize, len: usize, dashed: usize) -> String {
        let mut st = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut out = Vec::new();
        for k in 0..n {
            let mut s = String::new();
            for i in 0..len {
                st = st.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                if i > 0 {
                    s.push_str(if i == 2 && k < dashed { "\u{2014}" } else { " " });
                }
                s.push_str(vocab[((st >> 33) as usize) % vocab.len()]);
            }
            out.push(format!("{}{}.", s[..1].to_uppercase(), &s[1..]));
        }
        out.join(" ")
    }

    fn prose(vocab: &[&str], seed: u64, n: usize, len: usize) -> String {
        prose_d(vocab, seed, n, len, 0)
    }

    /// P1: three paragraphs, about 194 words; curly double quotes and spaced
    /// em dashes are its majorities.
    fn parent_one() -> String {
        let a = format!("{} The keeper called it \u{201C}amber harbor\u{201D} again \u{2014} twice. {}", prose(&SYN, 1, 3, 11), prose(&SYN, 2, 2, 10));
        let b = prose(&SYN, 3, 6, 11);
        let c = format!("{} They named it \u{201C}quiet fjord\u{201D} later \u{2014} much later. {}", prose(&SYN, 4, 2, 12), prose(&SYN, 5, 3, 11));
        format!("{a}\n\n{b}\n\n{c}")
    }

    /// P2: two paragraphs, about 165 words; spaced em dashes are its majority.
    fn parent_two() -> String {
        let a = format!("{} The pier stayed open \u{2014} briefly. {}", prose(&SYN, 61, 4, 11), prose(&SYN, 62, 3, 11));
        let b = format!("{} The tide turned \u{2014} slowly. {}", prose(&SYN, 63, 3, 11), prose(&SYN, 64, 4, 11));
        format!("{a}\n\n{b}")
    }

    fn sha(s: &str) -> String {
        text::sha_hex(s.as_bytes())
    }

    fn sentence(p: &str, k: usize) -> (usize, usize) {
        let s = text::sentences(p)[k];
        (s.start, s.end)
    }

    /// Sentences `a..=b` of `p` as one run.
    fn run_of(p: &str, a: usize, b: usize) -> (usize, usize) {
        (sentence(p, a).0, sentence(p, b).1)
    }

    const TOKENIZER: &str = r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#;

    fn tok() -> tokenizers::Tokenizer {
        TOKENIZER.parse().unwrap()
    }

    fn parent(id: &str, split: &str, text: String) -> Parent {
        Parent {
            id: id.into(),
            split: split.into(),
            domain: "synthetic_domain".into(),
            genre: None,
            source_group: format!("g-{id}"),
            writer_key: Some(format!("w-{id}")),
            cohort_id: "synthetic_cohort".into(),
            provenance_sha256: None,
            text_sha256: sha(&text),
            a_tokens: 0,
            words: splice::py_words(&text),
            text,
        }
    }

    /// A run edit over `span`, or an insertion at `span.0` (separator one
    /// space) when `span.0 == span.1`, with permissive spliced checks.
    fn edit(k: usize, rid: &str, p: &str, span: (usize, usize)) -> Edit {
        let insert = span.0 == span.1;
        let words = splice::py_words(&p[span.0..span.1]);
        let (kind, op) = if insert { ("insert", "elaborate") } else { ("run", "paraphrase") };
        Edit {
            edit_index: k,
            component: if insert { "insert" } else { "one_sentence" }.into(),
            kind: kind.into(),
            operation: op.into(),
            family: op.into(),
            operation_requested: op.into(),
            generator: "g".into(),
            parent_span: span,
            insert_sep: insert.then(|| " ".to_string()),
            sentence_range: (0, 0),
            context_included: false,
            context_spans: vec![],
            style: json!({"style_kind": "none", "style_text": null, "voice_id": null, "brief_id": null, "style_scope": null}),
            template_id: "t".into(),
            source_words: words,
            target_words: words.max(10),
            len_lo: 1.0,
            len_hi: 400.0,
            min_change: None,
            start_rule: "any".into(),
            end_rule: "any".into(),
            newline_ok: true,
            request_id: rid.into(),
        }
    }

    /// One plan item with its edits' replies, one per attempt.
    struct Case {
        item: Item,
        replies: Vec<Vec<String>>,
    }

    fn case(id: &str, pop: &str, order: usize, p: &Parent, spans: &[(usize, usize)], replies: Vec<Vec<String>>) -> Case {
        assert_eq!(spans.len(), replies.len());
        let edits: Vec<Edit> = spans.iter().enumerate().map(|(k, &sp)| edit(k, &format!("{id}-e{k}"), &p.text, sp)).collect();
        let item = Item {
            item_id: id.into(),
            population: pop.into(),
            split: if pop == splice::TRAIN { "train" } else { "dev_iter" }.into(),
            domain: p.domain.clone(),
            parent_id: p.id.clone(),
            parent_text_sha256: p.text_sha256.clone(),
            source_group: p.source_group.clone(),
            writer_key: p.writer_key.clone(),
            variant: 0,
            order,
            order_t: 0.5,
            policy: "one_sentence".into(),
            policy_requested: "one_sentence".into(),
            policy_fallback_steps: 0,
            generators: vec!["g".into()],
            two_generators: false,
            voice_scope: "document".into(),
            span_half: (pop == splice::DEV_SET).then(|| "span_select".to_string()),
            edits,
        };
        Case { item, replies }
    }

    /// The plan request of edit `e`, read as `finalize` reads a request line,
    /// so a field that a later build adds with a serde default needs no change
    /// here (amendment 25 item 25.5).
    fn request(it: &Item, e: &Edit) -> plan::Request {
        serde_json::from_value(json!({
            "request_id": e.request_id, "item_id": it.item_id, "parent_id": it.parent_id, "split": it.split, "generator": "g", "endpoint": "chat",
            "template_id": "t", "prompt": format!("prompt {}", e.request_id), "span_index": e.edit_index, "source_span": [e.parent_span.0, e.parent_span.1],
            "target_words": e.target_words, "cut_budget": null, "decoding": plan::profile("A", 0.7, 0.9, 512), "request_sha256": sha(&e.request_id),
            "seed": 7, "op": null, "cohort": null,
        }))
        .unwrap()
    }

    /// A reply that stands for a chat body with no content (`no_content`).
    const NO_CONTENT_REPLY: &str = "\u{0}no content";

    fn body(reply: &str) -> (u16, Value) {
        match reply {
            NO_CONTENT_REPLY => (200, json!({"choices":[{"message":{},"finish_reason":"stop"}]})),
            _ => (200, json!({"choices":[{"message":{"content":reply},"finish_reason":"stop"}]})),
        }
    }

    fn s(x: &str) -> Vec<String> {
        vec![x.to_string()]
    }

    fn title(t: &str, body: String) -> String {
        format!("{t}\n{body}")
    }

    /// A title line over a short run whose `k` word pairs are joined by
    /// closed em dashes (P2's laid-out child spaces them: two words each).
    fn dashed(t: &str, seed: u64, k: usize) -> String {
        title(t, dashed_run(seed, k))
    }

    /// `dashed`'s run, without the title line.
    fn dashed_run(seed: u64, k: usize) -> String {
        let words: Vec<&str> = (0..2 * k + 4).map(|i| ALT[(seed as usize + 3 * i) % ALT.len()]).collect();
        let mut out = String::new();
        for (i, w) in words.iter().enumerate() {
            if i > 0 {
                out.push_str(if i % 2 == 1 && i < 2 * k { "\u{2014}" } else { " " });
            }
            out.push_str(w);
        }
        format!("{}{}.", out[..1].to_uppercase(), &out[1..])
    }

    /// The battery over P1 (Train) and P2, plus one Dev-iter item over P3.
    fn battery(p1: &Parent, p2: &Parent, p3: &Parent) -> Vec<Case> {
        let t1 = &p1.text;
        let tr = splice::TRAIN;
        vec![
            // one run edit in ASCII quotes: accepted at attempt 0
            case("sa", tr, 0, p1, &[sentence(t1, 1)], vec![s("The ranger said \"marsh nettle\" twice near the quiet ridge today.")]),
            // a run edit and an insertion: each accepted at attempt 0, the insertion judged on the item
            case("sb", tr, 1, p1, &[sentence(t1, 2), (sentence(t1, 7).1, sentence(t1, 7).1)], vec![
                s("Lichen and fern covered the old marsh wall near the ridge."),
                s("Heather and sorrel grew by the quiet inlet near the thicket."),
            ]),
            // a run edit whose first response has a title line (the form screen only), then accepted
            case("sc", tr, 2, p1, &[sentence(t1, 9)], vec![vec![title("Quiet Harbor Notes", prose(&ALT, 31, 1, 11)), prose(&ALT, 32, 1, 11)]]),
            // a run edit accepted at once, and an insertion whose first response has a title line, then accepted
            case("sd", tr, 3, p1, &[sentence(t1, 4), (sentence(t1, 13).1, sentence(t1, 13).1)], vec![
                s(&prose(&ALT, 41, 1, 10)),
                vec![title("Quiet Fern Valley", prose(&ALT, 42, 1, 11)), prose(&ALT, 43, 1, 11)],
            ]),
            // a run edit whose first response has markdown emphasis (the spliced check), then accepted
            case("se", tr, 4, p1, &[sentence(t1, 15)], vec![vec![format!("**Acorn** {}", &prose(&ALT, 51, 1, 10)), prose(&ALT, 52, 1, 11)]]),
            // P2: a long run replaced by a short titled reply twice; the raw document is below 150 words,
            // the laid-out one (spaced dashes) is not
            case("sf", tr, 5, p2, &[run_of(&p2.text, 0, 3)], vec![vec![dashed("Quiet Harbor Notes", 1, 8), dashed("Quiet Harbor Notes", 2, 8)]]),
            // no content first, then the form screen only: attempt 1 is the first raw response
            case("sg", tr, 6, p1, &[sentence(t1, 16)], vec![vec![NO_CONTENT_REPLY.into(), title("Quiet Dune Night", prose(&ALT, 71, 1, 11))]]),
            // a reasoning block first (content rejected before the profile step), then the form screen only
            case("sh", tr, 7, p1, &[sentence(t1, 17)], vec![vec![format!("<think>Plan it.</think>\n{}", prose(&ALT, 81, 1, 11)), title("Quiet Marsh Dawn", prose(&ALT, 82, 1, 11))]]),
            // Dev-iter: one run edit over P3 whose first response has a title line, then accepted
            case("dv", splice::DEV_SET, 0, p3, &[sentence(&p3.text, 3)], vec![vec![title("Quiet Pier Evening", prose(&ALT, 91, 1, 11)), prose(&ALT, 92, 1, 11)]]),
        ]
    }

    fn parents() -> Vec<Parent> {
        vec![
            parent("p1", "train", parent_one()),
            parent("p2", "train", parent_two()),
            parent("p3", "dev_iter", format!("{}\n\n{}", prose(&SYN, 21, 7, 12), prose(&SYN, 22, 7, 12))),
        ]
    }

    /// Journal lines of every request of `cases` (intents and responses), each
    /// request's attempts judged in plan order until one is accepted, with the
    /// profile off (`pop` None) or under profile v2 for population `pop`.
    fn judge(ps: &[Parent], cases: &[&Case], pop: Option<&str>) -> Vec<Value> {
        let edits: HashMap<String, Edit> = cases.iter().flat_map(|c| c.item.edits.iter().map(|e| (e.request_id.clone(), e.clone()))).collect();
        let form = pop.map(|p| {
            let ids: HashSet<&str> = cases.iter().map(|c| c.item.parent_id.as_str()).collect();
            let rows = ps.iter().filter(|x| ids.contains(x.id.as_str())).map(|x| (x.id.as_str(), x.domain.as_str(), x.text.as_str()));
            let specs: Vec<form_post::ItemSpec> = cases
                .iter()
                .filter(|c| c.item.edits.len() > 1)
                .map(|c| form_post::ItemSpec { item_id: c.item.item_id.clone(), parts: c.item.edits.iter().map(|e| (e.request_id.clone(), e.parent_span, e.insert_sep.clone())).collect() })
                .collect();
            form_post::FormPost::new(p, rows).unwrap().with_items(specs, &HashMap::new()).unwrap()
        });
        let ((post, doc_words), vf) = form_post::spliced_wiring(form, edits).unwrap();
        let ctx = Ctx {
            endpoint: String::new(),
            alias: "m".into(),
            config: json!({}),
            tokenizer: tok(),
            overlap: Overlap::new(PriorIndex::empty()),
            parents: ps.iter().map(|p| (p.id.clone(), p.text.clone())).collect(),
            client: generation::client().unwrap(),
            post,
            doc_words,
        };
        let mut lines = Vec::new();
        for c in cases {
            for (e, replies) in c.item.edits.iter().zip(&c.replies) {
                let r = request(&c.item, e);
                for (a, reply) in replies.iter().enumerate().take(generation::ATTEMPTS as usize) {
                    let a = a as u32;
                    lines.push(json!({"kind": "intent", "request_id": r.request_id, "item_id": r.item_id, "attempt": a}));
                    let (status, parsed) = body(reply);
                    let v = vf(&ctx, &r, status, &parsed);
                    let ok = v["accepted"] == true;
                    lines.push(json!({"kind": "response", "request_id": r.request_id, "item_id": r.item_id, "split": r.split, "generator": "g",
                        "attempt": a, "seed": r.seed + a, "status": status, "elapsed_ms": 0, "raw_body": "", "verdict": v}));
                    if ok {
                        break;
                    }
                }
            }
        }
        lines
    }

    /// A plan directory, its runs and its finalize output.
    struct Run {
        dir: tempfile::TempDir,
        ps: Vec<Parent>,
        cases: Vec<Case>,
        lines: Vec<Value>,
    }

    impl Run {
        fn pdir(&self) -> PathBuf {
            self.dir.path().join("plan")
        }
        fn out(&self) -> PathBuf {
            self.dir.path().join("out")
        }
    }

    fn write_lines(p: &Path, rows: &[Value]) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut b = String::new();
        for r in rows {
            b.push_str(&serde_json::to_string(r).unwrap());
            b.push('\n');
        }
        fs::write(p, b).unwrap();
    }

    /// Builds the plan for `cases`, judges every request (`v2`: Train under
    /// `train2`, Dev-iter under `span_select2`) and writes the journals.
    fn setup(ps: Vec<Parent>, cases: Vec<Case>, v2: bool) -> Run {
        let dir = tempfile::tempdir().unwrap();
        let pdir = dir.path().join("plan");
        fs::create_dir_all(pdir.join("requests")).unwrap();
        fs::write(pdir.join("tokenizer.json"), TOKENIZER).unwrap();
        write_json(&pdir.join("plan.json"), &json!({"schema": splice::PLAN_SCHEMA, "inputs": {"tokenizer": {"sha256": sha(TOKENIZER)}}})).unwrap();
        write_jsonl(&pdir.join("parents.jsonl"), &ps).unwrap();
        let items: Vec<&Item> = cases.iter().map(|c| &c.item).collect();
        write_jsonl(&pdir.join("items.jsonl"), &items).unwrap();
        let reqs: Vec<plan::Request> = cases.iter().flat_map(|c| c.item.edits.iter().map(|e| request(&c.item, e))).collect();
        write_jsonl(&pdir.join("requests").join("g.jsonl"), &reqs).unwrap();
        let cfg = dir.path().join("configs");
        fs::create_dir_all(&cfg).unwrap();
        write_json(&cfg.join("g.json"), &json!({"key": "g", "family": "fam-g", "identity": "synthetic", "repo": "r", "revision": "v", "filename": "f",
            "sha256": "0".repeat(64), "quantization": "q", "licence": "synthetic", "runtime": "rt"})).unwrap();
        let train: Vec<&Case> = cases.iter().filter(|c| c.item.population == splice::TRAIN).collect();
        let dev: Vec<&Case> = cases.iter().filter(|c| c.item.population == splice::DEV_SET).collect();
        let mut lines = judge(&ps, &train, v2.then_some("train2"));
        lines.extend(judge(&ps, &dev, v2.then_some("span_select2")));
        write_lines(&dir.path().join("runs").join("g").join("journal-0.jsonl"), &lines);
        Run { dir, ps, cases, lines }
    }

    fn finalize_run(r: &Run) -> Result<()> {
        let d = r.dir.path();
        run_finalize(&r.pdir(), &d.join("runs"), &d.join("configs"), &r.pdir().join("tokenizer.json"), 1000, usize::MAX, &r.out(), "test", usize::MAX)
    }

    fn battery_run(v2: bool) -> Run {
        let ps = parents();
        let cases = battery(&ps[0], &ps[1], &ps[2]);
        setup(ps, cases, v2)
    }

    fn read_rows(p: &Path) -> Vec<Value> {
        fs::read_to_string(p).unwrap().lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    /// The output files `SHA256SUMS` names (records, failures, pairs and
    /// reports), in its order.
    fn outputs(out: &Path) -> Vec<String> {
        fs::read_to_string(out.join("SHA256SUMS")).unwrap().lines().map(|l| l.split("  ").nth(1).unwrap().to_string()).collect()
    }

    /// Every output `SHA256SUMS` names with the R-form parts taken out (each
    /// entry's `raw_twin` and `rform_screen_fail_v1`, and the quality
    /// report's `rform` counters), re-serialised: the N-form outputs.
    fn n_form_digest(out: &Path) -> String {
        let mut b = String::new();
        for rel in outputs(out) {
            b.push_str(&rel);
            b.push('\n');
            let p = out.join(&rel);
            if rel.ends_with(".jsonl") {
                for mut v in read_rows(&p) {
                    let o = v.as_object_mut().unwrap();
                    o.remove(RAW_TWIN_FIELD);
                    o.remove(SCREEN_FAIL_ID);
                    b.push_str(&serde_json::to_string(&v).unwrap());
                    b.push('\n');
                }
            } else {
                let mut v: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
                for (_, dims) in v["counts"].as_object_mut().into_iter().flatten() {
                    if let Some(o) = dims.as_object_mut() {
                        o.retain(|k, _| !k.starts_with("rform"));
                    }
                }
                b.push_str(&serde_json::to_string(&v).unwrap());
                b.push('\n');
            }
        }
        sha(&b)
    }

    /// The digest of `SHA256SUMS` (every output but the manifest, which names
    /// the binary) with the profile off, and of the N-form outputs under
    /// profile v2, both recorded by these tests from the code before raw twins
    /// existed in the spliced finalize.
    const OFF_GOLDEN: &str = "4c0aa6753c72f489fc6cb34bc9de930b57f8377bb936550f23f5c4665c33446e";
    const V2_N_FORM_GOLDEN: &str = "d95a7e6dea9e8897802b844f9f54bbbf0bdcd7b5bf3a0eccff330daec7938602";

    #[test]
    fn spliced_finalize_profile_off_outputs_are_the_pre_twin_bytes() {
        let r = battery_run(false);
        finalize_run(&r).unwrap();
        let d = sha(&fs::read_to_string(r.out().join("SHA256SUMS")).unwrap());
        eprintln!("OFF_DIGEST {d}");
        assert_eq!(d, OFF_GOLDEN);
        for rel in outputs(&r.out()) {
            let s = fs::read_to_string(r.out().join(&rel)).unwrap();
            assert!(!s.contains("rform") && !s.contains(RAW_TWIN_FIELD), "{rel}");
        }
    }

    #[test]
    fn spliced_finalize_v2_n_form_outputs_are_unchanged() {
        let r = battery_run(true);
        finalize_run(&r).unwrap();
        let d = n_form_digest(&r.out());
        eprintln!("V2_N_FORM_DIGEST {d}");
        assert_eq!(d, V2_N_FORM_GOLDEN);
    }

    fn rows(out: &Path, kind: &str, pop: &str) -> Vec<Value> {
        let p = if kind == "records" { out.join("records").join(pop).join("records.jsonl") } else { out.join("failures").join(format!("{pop}.jsonl")) };
        read_rows(&p)
    }

    /// The record (by `id`) or failure (by `item_id`) of item `id`.
    fn entry<'a>(rs: &'a [Value], id: &str) -> &'a Value {
        rs.iter().find(|x| x["id"] == id || x["item_id"] == id).unwrap_or_else(|| panic!("no entry {id}"))
    }

    /// A request's journal response line at `attempt`.
    fn response<'a>(r: &'a Run, rid: &str, attempt: u64) -> &'a Value {
        r.lines.iter().find(|l| l["kind"] == "response" && l["request_id"] == rid && l["attempt"] == attempt).unwrap_or_else(|| panic!("no response {rid} {attempt}"))
    }

    fn accepted_raw(r: &Run, rid: &str) -> String {
        let l = r.lines.iter().find(|l| l["kind"] == "response" && l["request_id"] == rid && l["verdict"]["accepted"] == true).unwrap();
        l["verdict"]["raw_text"].as_str().unwrap().to_string()
    }

    fn parent_of<'a>(r: &'a Run, c: &Case) -> &'a Parent {
        r.ps.iter().find(|p| p.id == c.item.parent_id).unwrap()
    }

    /// No sentence of four or more words from `texts` appears in `v`.
    fn assert_no_text(v: &Value, texts: &[&str]) {
        let s = v.to_string();
        for t in texts {
            for sent in text::sentences(t) {
                let x = &t[sent.start..sent.end];
                if text::words(x) >= 4 {
                    assert!(!s.contains(x), "holds text: {x:?}");
                }
            }
        }
    }

    /// The labels an R-form document shares with its item's N-form record.
    const LABELS: [&str; 20] = [
        "schema", "population", "split", "frozen_split", "span_half", "parent_id", "parent_text_sha256", "pair_id", "source_group", "writer_key", "domain",
        "origin", "source_kind", "cohort_id", "operation", "policy", "generator_label", "generators", "plan_sha256", "rights",
    ];

    #[test]
    fn spliced_finalize_v2_raw_twins_rebuild_byte_exactly_insertions_included() {
        let r = battery_run(true);
        finalize_run(&r).unwrap();
        let out = r.out();
        let all: Vec<Value> = [splice::TRAIN, splice::DEV_SET].iter().flat_map(|p| rows(&out, "records", p)).collect();
        let ids: Vec<&str> = all.iter().map(|x| x["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["sa", "sb", "sc", "sd", "se", "dv"]);
        for n in &all {
            let id = n["id"].as_str().unwrap();
            let c = r.cases.iter().find(|c| c.item.item_id == id).unwrap();
            let p = &parent_of(&r, c).text;
            let tw = &n[RAW_TWIN_FIELD];
            // the twin is the parent with the accepted raw responses spliced in, insertions with their separator
            let raws: Vec<String> = c.item.edits.iter().map(|e| accepted_raw(&r, &e.request_id)).collect();
            let rr: Vec<&str> = raws.iter().map(String::as_str).collect();
            let eds: Vec<&Edit> = c.item.edits.iter().collect();
            let want = splice::assemble(p, &eds, &rr).unwrap();
            assert_eq!(tw["text"].as_str(), Some(want.text.as_str()), "{id}");
            assert_eq!(tw["text_sha256"].as_str(), Some(sha(&want.text).as_str()));
            assert_eq!((tw["ai_byte_spans"].clone(), tw["words"].clone(), tw["ai_words"].clone()), (json!(want.ai_byte_spans), json!(want.total_words), json!(want.ai_words)));
            assert_eq!(tw["share"].as_f64(), Some(want.ai_words as f64 / want.total_words as f64));
            for (k, raw) in rr.iter().enumerate() {
                let e = &tw["edits"][k];
                let (a, b) = (e["final_span_bytes"][0].as_u64().unwrap() as usize, e["final_span_bytes"][1].as_u64().unwrap() as usize);
                assert_eq!(&want.text[a..b], *raw);
                assert_eq!(e["after_sha256"].as_str(), Some(sha(raw).as_str()));
                assert_eq!((e["verdict"].as_str(), e["request_id"].as_str()), (Some("accepted"), Some(c.item.edits[k].request_id.as_str())));
                let ev = &tw["rform"]["raw_responses"][k];
                assert_eq!((ev["raw_response_sha256"].as_str(), ev["request_id"].as_str(), ev["accepted"].as_bool()), (Some(sha(raw).as_str()), e["request_id"].as_str(), Some(true)));
                assert_eq!(e["accepted_attempt"], n["edits"][k]["accepted_attempt"]);
            }
            // ids, form, the pair and the N-form record's labels
            assert_eq!(tw["id"].as_str(), Some(format!("rform:{id}").as_str()));
            assert_eq!((tw["form"].as_str(), tw["stratum"].as_str(), tw["twin_of"].as_str()), (Some(R_FORM), Some(RAW_TWIN_STRATUM), Some(id)));
            assert_eq!(tw["rform"]["n_form_text_sha256"], n["text_sha256"]);
            assert_eq!(tw["rform"]["parent_text_sha256"].as_str(), Some(sha(p).as_str()));
            for k in LABELS {
                assert_eq!(tw[k], n[k], "{id} {k}");
            }
            assert!(tw.get(RAW_TWIN_FIELD).is_none() && tw.get(SCREEN_FAIL_ID).is_none());
            assert_eq!(finalize::raw_twin_doc(n), Some(tw));
            assert!(finalize::screen_fail_doc(n).is_none_or(|d| d != tw));
        }
        // the twin is raw: ASCII quotes stay where the N-form record took the parent's curly ones
        let sa = entry(&all, "sa");
        assert!(sa[RAW_TWIN_FIELD]["text"].as_str().unwrap().contains("\"marsh nettle\"") && !sa["text"].as_str().unwrap().contains("\"marsh nettle\""));
        // the insertion: its separator before it, the parent's text after it
        let sb = entry(&all, "sb");
        let at = sentence(&r.ps[0].text, 7).1;
        let ins = accepted_raw(&r, "sb-e1");
        let t = sb[RAW_TWIN_FIELD]["text"].as_str().unwrap();
        let fs_ = sb[RAW_TWIN_FIELD]["edits"][1]["final_span_bytes"][0].as_u64().unwrap() as usize;
        assert_eq!(&t[fs_ - 1..fs_], " ");
        assert!(t[fs_..].starts_with(&format!("{ins}{}", &r.ps[0].text[at..at + 1])));
        assert_eq!(sb[RAW_TWIN_FIELD]["edits"][1]["insert_separator"], " ");
        // counted; the pairs pair N-form records with their twins and never the stratum
        let q: Value = serde_json::from_slice(&fs::read(out.join("reports").join("quality.json")).unwrap()).unwrap();
        assert_eq!(q["counts"]["train"]["rform_raw_twin"]["emitted"], 5.0);
        assert_eq!(q["counts"][splice::DEV_SET]["rform_raw_twin"]["emitted"], 1.0);
        let fails: Vec<Value> = [splice::TRAIN, splice::DEV_SET].iter().flat_map(|p| rows(&out, "failures", p)).collect();
        let docs: Vec<&Value> = all.iter().chain(&fails).flat_map(|e| finalize::raw_twin_doc(e).into_iter().chain(finalize::screen_fail_doc(e))).collect();
        let pairs = finalize::fs_pairs(all.iter(), docs.iter().copied());
        let want: Vec<(String, String)> = ids.iter().map(|i| (i.to_string(), format!("rform:{i}"))).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        assert_eq!(pairs, want);
    }

    #[test]
    fn spliced_finalize_v2_screen_fail_stratum_follows_amendment_19() {
        let r = battery_run(true);
        finalize_run(&r).unwrap();
        let out = r.out();
        let recs: Vec<Value> = [splice::TRAIN, splice::DEV_SET].iter().flat_map(|p| rows(&out, "records", p)).collect();
        let fails = rows(&out, "failures", splice::TRAIN);
        let first_raw = |rid: &str, a: u64| response(&r, rid, a)["verdict"]["raw_text"].as_str().unwrap().to_string();
        let doc_of = |id: &str, raws: Vec<String>| {
            let c = r.cases.iter().find(|c| c.item.item_id == id).unwrap();
            let eds: Vec<&Edit> = c.item.edits.iter().collect();
            let rr: Vec<&str> = raws.iter().map(String::as_str).collect();
            splice::assemble(&parent_of(&r, c).text, &eds, &rr).unwrap()
        };
        // one edit: its first raw response (a title line), spliced in; the record also keeps its twin
        let sc = &entry(&recs, "sc")[SCREEN_FAIL_ID];
        let want = doc_of("sc", vec![first_raw("sc-e0", 0)]);
        assert_eq!(sc["text"].as_str(), Some(want.text.as_str()));
        assert!(sc["text"].as_str().unwrap().contains("Quiet Harbor Notes\n"));
        assert_eq!((sc["id"].as_str(), sc["form"].as_str(), sc["stratum"].as_str()), (Some("rform_screen_fail_v1:sc"), Some(R_FORM), Some(SCREEN_FAIL_ID)));
        assert!(sc["twin_of"].is_null());
        assert_eq!(sc["rform"]["raw_responses"], json!([{"request_id": "sc-e0", "attempt": 0, "raw_response_sha256": sha(&first_raw("sc-e0", 0)), "accepted": false, "reasons": ["form_line_no_sentence_end"]}]));
        assert_eq!((sc["rform"]["length_window"]["words"].as_u64(), sc["rform"]["length_window"]["pass"].as_bool()), (Some(text::words(&want.text) as u64), Some(true)));
        assert_eq!((sc["edits"][0]["verdict"].as_str(), sc["edits"][0]["accepted_attempt"].as_u64()), (Some("rejected"), Some(0)));
        assert_ne!(entry(&recs, "sc")[RAW_TWIN_FIELD]["text"], sc["text"]);
        assert_eq!(finalize::screen_fail_doc(entry(&recs, "sc")), Some(sc));
        for k in LABELS {
            assert_eq!(sc[k], entry(&recs, "sc")[k], "{k}");
        }
        // several edits (item 19.2): every request's first raw response, the accepted one included, and the insertion's separator
        let sd = &entry(&recs, "sd")[SCREEN_FAIL_ID];
        let want = doc_of("sd", vec![first_raw("sd-e0", 0), first_raw("sd-e1", 0)]);
        assert_eq!(sd["text"].as_str(), Some(want.text.as_str()));
        let rs = &sd["rform"]["raw_responses"];
        assert_eq!((rs[0]["accepted"].as_bool(), rs[1]["accepted"].as_bool(), rs[1]["reasons"].clone()), (Some(true), Some(false), json!(["form_line_no_sentence_end"])));
        assert_eq!((sd["edits"][0]["verdict"].as_str(), sd["edits"][1]["verdict"].as_str(), sd["edits"][1]["insert_separator"].as_str()), (Some("accepted"), Some("rejected"), Some(" ")));
        assert_eq!(response(&r, "sd-e1", 0)["verdict"]["diag"]["post"]["item_check"]["last"], true);
        // no content first (item 19.1): attempt 1 is the first raw response; the failure keeps the document
        let sg = entry(&fails, "sg");
        assert_eq!(sg["status"], "terminal_failure");
        assert_eq!(sg[SCREEN_FAIL_ID]["text"].as_str(), Some(doc_of("sg", vec![first_raw("sg-e0", 1)]).text.as_str()));
        assert_eq!(sg[SCREEN_FAIL_ID]["rform"]["raw_responses"][0]["attempt"], 1);
        assert_eq!((sg[SCREEN_FAIL_ID]["edits"][0]["accepted_attempt"].as_u64(), sg[SCREEN_FAIL_ID]["edits"][0]["verdict"].as_str()), (Some(1), Some("rejected")));
        assert_eq!(finalize::screen_fail_doc(sg), Some(&sg[SCREEN_FAIL_ID]));
        // a window failure leaves it out: the raw document has 136 words, its laid-out one 152
        let sf = entry(&fails, "sf");
        let note = &sf[SCREEN_FAIL_ID];
        assert_eq!((note["left_out"].as_bool(), note["reasons"].clone(), note["words"].as_u64()), (Some(true), json!(["final_document_below_150_words"]), Some(136)));
        assert_eq!(response(&r, "sf-e0", 0)["verdict"]["diag"]["post"]["length_window"]["words"], 152);
        assert!(note.get("text").is_none() && finalize::screen_fail_doc(sf).is_none());
        assert_no_text(note, &[&first_raw("sf-e0", 0), &r.ps[1].text]);
        // outside the stratum: a reasoning block first (rejected before the profile step), the spliced generator's
        // own check (markdown), and first raw responses that were accepted
        assert_eq!(response(&r, "sh-e0", 0)["verdict"]["reasons"], json!(["nonempty_reasoning_in_content"]));
        assert!(entry(&fails, "sh").get(SCREEN_FAIL_ID).is_none());
        assert!(response(&r, "se-e0", 0)["verdict"]["reasons"].to_string().contains("markdown:bold"));
        for id in ["se", "sa", "sb"] {
            assert!(entry(&recs, id).get(SCREEN_FAIL_ID).is_none(), "{id}");
        }
        // the Dev-iter population
        assert_eq!(entry(&recs, "dv")[SCREEN_FAIL_ID]["text"].as_str(), Some(doc_of("dv", vec![first_raw("dv-e0", 0)]).text.as_str()));
        let q: Value = serde_json::from_slice(&fs::read(out.join("reports").join("quality.json")).unwrap()).unwrap();
        assert_eq!(q["counts"]["train"][SCREEN_FAIL_ID], json!({"emitted": 3.0, "left_out:final_document_below_150_words": 1.0}));
        assert_eq!(q["counts"][splice::DEV_SET][SCREEN_FAIL_ID], json!({"emitted": 1.0}));
    }

    /// The run's journal rewritten with `edit` applied to response line
    /// (`rid`, `attempt`), finalized into a new directory; the error, if any.
    fn tampered(r: &Run, rid: &str, attempt: u64, edit: &dyn Fn(&mut Value)) -> String {
        let mut lines = r.lines.clone();
        let l = lines.iter_mut().find(|l| l["kind"] == "response" && l["request_id"] == rid && l["attempt"] == attempt).unwrap();
        edit(l);
        let j = r.dir.path().join("runs").join("g").join("journal-0.jsonl");
        write_lines(&j, &lines);
        let o = r.dir.path().join(format!("out-{rid}-{attempt}"));
        let d = r.dir.path();
        let e = run_finalize(&r.pdir(), &d.join("runs"), &d.join("configs"), &r.pdir().join("tokenizer.json"), 1000, usize::MAX, &o, "test", usize::MAX)
            .err()
            .map(|e| format!("{e:#}"))
            .unwrap_or_default();
        write_lines(&j, &r.lines);
        e
    }

    #[test]
    fn spliced_finalize_v2_refuses_a_digest_mismatch() {
        let r = battery_run(true);
        // an accepted raw text that is not the digested one
        let e = tampered(&r, "sa-e0", 0, &|l| {
            let t = format!("{} again", l["verdict"]["raw_text"].as_str().unwrap());
            l["verdict"]["raw_text"] = json!(t);
        });
        assert!(e.contains("sa") && e.contains("raw_text does not match"), "{e}");
        // a digest with no raw text
        let e = tampered(&r, "sb-e0", 0, &|l| {
            l["verdict"].as_object_mut().unwrap().remove("raw_text");
        });
        assert!(e.contains("sb") && e.contains("no raw_text"), "{e}");
        // a first raw response (the stratum) that is not the digested one
        let e = tampered(&r, "sc-e0", 0, &|l| {
            l["verdict"]["raw_text"] = json!("Quiet Harbor Notes\nSomething else entirely.");
        });
        assert!(e.contains("sc") && e.contains("raw_text does not match"), "{e}");
        // an item check that read another raw response
        let e = tampered(&r, "sb-e1", 0, &|l| {
            l["verdict"]["diag"]["post"]["edits"][0]["raw_response_sha256"] = json!(sha("another"));
        });
        assert!(e.contains("sb") && e.contains("item check read other raw children"), "{e}");
        // an item whose parent is not the planned one
        let items = r.pdir().join("items.jsonl");
        let orig = fs::read(&items).unwrap();
        let mut its = read_rows(&items);
        its[0]["parent_text_sha256"] = json!(sha("another parent"));
        write_lines(&items, &its);
        let e = tampered(&r, "sa-e0", 0, &|_| {});
        assert!(e.contains("sa") && e.contains("parent_text_sha256"), "{e}");
        fs::write(&items, orig).unwrap();
        // the untouched journals finalize
        assert!(finalize_run(&r).is_ok());
    }

    #[test]
    fn spliced_finalize_v2_refuses_a_duplicate_rform_text() {
        let ps = parents();
        let mut cases = battery(&ps[0], &ps[1], &ps[2]);
        // over P1's sentence 9, sc's first reply again, then another accepted reply: two stratum documents with one text
        let first = cases[2].replies[0][0].clone();
        cases.push(case("si", splice::TRAIN, 8, &ps[0], &[sentence(&ps[0].text, 9)], vec![vec![first, prose(&ALT, 99, 1, 11)]]));
        let r = setup(ps, cases, true);
        let e = finalize_run(&r).err().map(|e| format!("{e:#}")).unwrap_or_default();
        assert!(e.contains("duplicate R-form text: rform_screen_fail_v1:si has the text of rform_screen_fail_v1:sc"), "{e}");
        // with another first reply the run finalizes
        let ps = parents();
        let mut cases = battery(&ps[0], &ps[1], &ps[2]);
        cases.push(case("si", splice::TRAIN, 8, &ps[0], &[sentence(&ps[0].text, 9)], vec![vec![title("Quiet Ridge Evening", prose(&ALT, 98, 1, 11)), prose(&ALT, 99, 1, 11)]]));
        let r = setup(ps, cases, true);
        assert!(finalize_run(&r).is_ok());
        // a raw twin is checked too: sa2, over sa's span, journaled with sa's accepted raw response
        // (its digest made to match) has sa's twin text
        let ps = parents();
        let mut cases = battery(&ps[0], &ps[1], &ps[2]);
        cases.push(case("sa2", splice::TRAIN, 8, &ps[0], &[sentence(&ps[0].text, 1)], vec![s(&prose(&ALT, 97, 1, 11))]));
        let r = setup(ps, cases, true);
        let raw = accepted_raw(&r, "sa-e0");
        let e = tampered(&r, "sa2-e0", 0, &|l| {
            l["verdict"]["raw_text"] = json!(raw);
            l["verdict"]["diag"]["post"]["edits"][0]["raw_response_sha256"] = json!(sha(&raw));
        });
        assert!(e.contains("duplicate R-form text: rform:sa2 has the text of rform:sa"), "{e}");
    }

    #[test]
    fn spliced_finalize_v2_stratum_rides_with_items_excluded_in_finalize() {
        let ps = parents();
        let mut cases = battery(&ps[0], &ps[1], &ps[2]);
        // a title line first (the form screen only), then sa's accepted reply again over another sentence: excluded by the generated-text dedup
        let again = cases[0].replies[0][0].clone();
        cases.push(case("sj", splice::TRAIN, 8, &ps[0], &[sentence(&ps[0].text, 11)], vec![vec![title("Quiet Ridge Evening", prose(&ALT, 101, 1, 11)), again]]));
        let r = setup(ps, cases, true);
        finalize_run(&r).unwrap();
        let first_doc = |r: &Run, id: &str, rid: &str| {
            let c = r.cases.iter().find(|c| c.item.item_id == id).unwrap();
            let eds: Vec<&Edit> = c.item.edits.iter().collect();
            let raw = response(r, rid, 0)["verdict"]["raw_text"].as_str().unwrap().to_string();
            splice::assemble(&parent_of(r, c).text, &eds, &[raw.as_str()]).unwrap().text
        };
        let fails = rows(&r.out(), "failures", splice::TRAIN);
        let sj = entry(&fails, "sj");
        assert_eq!((sj["status"].as_str(), sj["reasons"].clone()), (Some("excluded_in_finalize"), json!(["generated_normalized_text_duplicate"])));
        // the exclusion keeps the item's stratum document, and no twin
        let d = &sj[SCREEN_FAIL_ID];
        assert_eq!((d["text"].as_str(), d["id"].as_str()), (Some(first_doc(&r, "sj", "sj-e0").as_str()), Some("rform_screen_fail_v1:sj")));
        assert_eq!(finalize::screen_fail_doc(sj), Some(d));
        assert!(sj.get(RAW_TWIN_FIELD).is_none());
        // an assembly error: sc's accepted laid-out text made to end with a space; its exclusion keeps sc's stratum document
        let e = tampered(&r, "sc-e0", 1, &|l| {
            let t = format!("{} ", l["verdict"]["text"].as_str().unwrap());
            l["verdict"]["text"] = json!(t);
        });
        assert!(e.is_empty(), "{e}");
        let fails = rows(&r.dir.path().join("out-sc-e0-1"), "failures", splice::TRAIN);
        let sc = entry(&fails, "sc");
        assert_eq!((sc["status"].as_str(), sc["reasons"].clone()), (Some("excluded_in_finalize"), json!(["assembly:replacement layout"])));
        assert_eq!(sc[SCREEN_FAIL_ID]["text"].as_str(), Some(first_doc(&r, "sc", "sc-e0").as_str()));
        assert_eq!(finalize::screen_fail_doc(sc), Some(&sc[SCREEN_FAIL_ID]));
        assert!(sc.get(RAW_TWIN_FIELD).is_none());
    }

    #[test]
    fn spliced_finalize_v2_rform_texts_are_unique_across_both_populations() {
        let ps = parents();
        let mut cases = battery(&ps[0], &ps[1], &ps[2]);
        // a Dev-iter item over sc's parent and sentence whose first reply is sc's: its stratum document has sc's (Train) text
        let first = cases[2].replies[0][0].clone();
        cases.push(case("dx", splice::DEV_SET, 1, &ps[0], &[sentence(&ps[0].text, 9)], vec![vec![first, prose(&ALT, 102, 1, 11)]]));
        let r = setup(ps, cases, true);
        let e = finalize_run(&r).err().map(|e| format!("{e:#}")).unwrap_or_default();
        assert!(e.contains("duplicate R-form text: rform_screen_fail_v1:dx has the text of rform_screen_fail_v1:sc"), "{e}");
    }

    #[test]
    fn spliced_finalize_v2_raw_twin_takes_the_n_form_window_verdict() {
        let ps = parents();
        let mut cases = battery(&ps[0], &ps[1], &ps[2]);
        // P2: a long run replaced by a short reply with closed dashes, accepted: the raw twin has 135 words, the laid-out record (spaced) 155
        cases.push(case("sk", splice::TRAIN, 8, &ps[1], &[run_of(&ps[1].text, 0, 3)], vec![s(&dashed_run(3, 10))]));
        let r = setup(ps, cases, true);
        finalize_run(&r).unwrap();
        let recs = rows(&r.out(), "records", splice::TRAIN);
        let sk = entry(&recs, "sk");
        let tw = &sk[RAW_TWIN_FIELD];
        let w = |d: &Value| text::words(d["text"].as_str().unwrap());
        assert_eq!((w(sk), w(tw)), (155, 135));
        assert_eq!((tw["twin_of"].as_str(), tw["rform"]["window"].as_str()), (Some("sk"), Some("the N-form document's verdict (protocol A2 section 7.2)")));
        assert_eq!(finalize::raw_twin_doc(sk), Some(tw));
    }

    #[test]
    fn spliced_finalize_v2_an_item_not_yet_generated_has_no_stratum() {
        let ps = parents();
        let mut cases = battery(&ps[0], &ps[1], &ps[2]);
        // a Dev-iter item with one attempt so far, rejected by the form screen only: not_run, so no entry
        cases.push(case("dw", splice::DEV_SET, 1, &ps[2], &[sentence(&ps[2].text, 6)], vec![vec![title("Quiet Knoll Morning", prose(&ALT, 96, 1, 11))]]));
        let r = setup(ps, cases, true);
        assert_eq!(response(&r, "dw-e0", 0)["verdict"]["reasons"], json!(["form_line_no_sentence_end"]));
        finalize_run(&r).unwrap();
        let out = r.out();
        assert!(rows(&out, "records", splice::DEV_SET).iter().chain(&rows(&out, "failures", splice::DEV_SET)).all(|e| e["id"] != "dw" && e["item_id"] != "dw"));
        let q: Value = serde_json::from_slice(&fs::read(out.join("reports").join("quality.json")).unwrap()).unwrap();
        assert_eq!(q["counts"][splice::DEV_SET][SCREEN_FAIL_ID], json!({"emitted": 1.0}));
        assert_eq!(q["counts"][splice::DEV_SET]["documents"]["not_run"], 1.0);
    }

    /// D8 end to end on synthetic text (B5b part 2; amendment 27; the part
    /// 2 review's finding 9): renorm's real spliced re-verdict of a
    /// profile-off journal, then `pilot`'s D8 rows (renorm's `regenerate`
    /// items), then `generate --dry-run --profile` with the pilot's order
    /// file: the run lists only the pilot's items, in list order, under the
    /// population's prior, the same record as a run without the order file.
    #[test]
    fn d8_pilot_on_renorms_real_output_keeps_the_populations_prior() {
        use std::collections::BTreeSet;
        let r = battery_run(false);
        let d = r.dir.path();
        let pdir = r.pdir();
        // the plan's digests, as the spliced planner records them
        let mut meta: Value = serde_json::from_slice(&fs::read(pdir.join("plan.json")).unwrap()).unwrap();
        meta["items"] = json!({"sha256": file_sha(&pdir.join("items.jsonl")).unwrap()});
        meta["parents"] = json!({"sha256": file_sha(&pdir.join("parents.jsonl")).unwrap()});
        meta["request_files"] = json!({"g": {"sha256": file_sha(&pdir.join("requests").join("g.jsonl")).unwrap()}});
        write_json(&pdir.join("plan.json"), &meta).unwrap();
        // renorm, as `renorm --population train --profile cc_cohort_profile_v2 --profile-population train2` runs it
        let rn = d.join("renorm");
        let b = grammar_a_positives::renorm::spliced_batch(&pdir, splice::TRAIN, "train2").unwrap();
        let sum = grammar_a_positives::renorm::run_batch(&b, &d.join("runs"), &rn).unwrap();
        assert_eq!(sum["plan_kind"], "spliced");
        let rg = read_rows(&rn.join("regeneration-plan-population-train.jsonl"));
        let regen: BTreeSet<String> = rg.iter().filter(|x| x["action"] == "regenerate").map(|x| x["item_id"].as_str().unwrap().to_string()).collect();
        assert!(!regen.is_empty() && regen.len() < r.cases.iter().filter(|c| c.item.population == splice::TRAIN).count(), "{regen:?}");
        // D8's pilot: renorm's regenerate items, one row per item; one cell below 59 rows, so piloted whole
        let pp = d.join("pilot");
        let pj = grammar_a_positives::pilot::run_spliced(&pdir, splice::TRAIN, &rn, "D8", &pp).unwrap();
        assert_eq!((pj["rows"].as_u64(), pj["whole_step_is_pilot"].as_bool()), (Some(regen.len() as u64), Some(true)));
        let mut by_order: Vec<&Item> = r.cases.iter().map(|c| &c.item).filter(|i| regen.contains(&i.item_id)).collect();
        by_order.sort_by_key(|i| i.order);
        let listed: Vec<String> = fs::read_to_string(pp.join("pilot-order-g.txt")).unwrap().lines().map(String::from).collect();
        assert_eq!(listed, by_order.iter().map(|i| i.item_id.clone()).collect::<Vec<_>>());
        // the spliced generator on renorm's output, the pilot's order file reversed (list order, not plan order)
        let cfg = d.join("g-gen.json");
        write_json(&cfg, &json!({"key": "g", "allowed_splits": ["train"]})).unwrap();
        let rev = d.join("order-reversed.txt");
        fs::write(&rev, listed.iter().rev().map(|i| format!("{i}\n")).collect::<String>()).unwrap();
        let dry = |order: Option<&Path>| run_generate(&pdir, splice::TRAIN, "g", &cfg, &rn, &d.join("no-index"), 1, 0, usize::MAX, true, Some("train2"), order).unwrap().unwrap();
        let with = dry(Some(&rev));
        let without = dry(None);
        assert_eq!(with["item_order"], json!(listed.iter().rev().collect::<Vec<_>>()));
        assert_eq!(with["pending_items"], json!(regen));
        assert_eq!((with["items_in_window"].as_u64(), without["items_in_window"].as_u64()), (Some(regen.len() as u64), Some(8)));
        // the profile and its prior are the population's either way, and renorm's binding holds
        assert_eq!(with["profile"], without["profile"]);
        assert_eq!(with["profile_prior"], without["profile_prior"]);
        assert_eq!(with["profile"], b.form.record());
        assert!(without.get("item_order").is_none());
    }
}

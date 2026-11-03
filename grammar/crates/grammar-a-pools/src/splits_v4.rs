//! Split extension v4: keep every splits-v3 assignment and every blog split
//! assignment byte for byte, add the Train-side rows of the new human pools
//! (one cross-source deduplication output), and reserve new evaluation
//! domains from metadata-only scans of their unrendered contributor buckets.
//!
//! * **Base.** Every splits-v3 file listed in its `SHA256SUMS` is verified
//!   (a listed file that does not exist is allowed only when the config names
//!   it as a known defect) and copied unchanged: evaluation files to the same
//!   relative path, metadata to `base-v3/`. The blog split directory is
//!   verified the same way; its evaluation-reserved files go to `blogs-v3/`.
//! * **Train side.** `train/`, `train-mining/` and `held/` files start with the
//!   exact splits-v3 bytes, then the exact blog-split bytes, then new rows.
//! * **New rows.** Each pool collection is mapped to a source with a use:
//!   `train` (seeded Train quota with a per-contributor cap, the rest
//!   Train-mining), `held` (never Train or mined; optionally also listed in an
//!   ungated diagnostic slice) or `diagnostic` (an evaluation-only source:
//!   ungated slice, never Train-side). Collections the config excludes must not
//!   appear in the pool.
//! * **New evaluation domains.** Groups are the scan rows' group keys in the
//!   domain's unrendered bucket range; units are their source families with
//!   the domain's expected yield per unit. Groups are allocated by deficit in
//!   seeded rank order (`splits::allocate`) to `target * oversample`; the rest
//!   stay reserved and unused. Contributor closure: no reserved group key of a
//!   domain is a Train-side group key of its collections, and no reserved
//!   family is a Train-side family, a base evaluation family or another
//!   split's family.
use crate::{
    adapter::bucket,
    io::{self, JsonlWriter},
    splits::{EVAL_SPLITS, Group, Unit, DOCUMENT_SCHEMA, GROUP_SCHEMA, RESERVATION_SCHEMA, allocate, rank},
    text,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

pub const SUMMARY_SCHEMA: &str = "slop_ninja_a_competitive_split_extension_summary_v4";
pub const SLICE_SCHEMA: &str = "slop_ninja_a_competitive_diagnostic_slice_v1";
pub const NEAR_CHECK_SCHEMA: &str = "slop_ninja_a_competitive_near_window_check_v1";
const VERSION: &str = "v4";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Use {
    Train,
    Held,
    Diagnostic,
}

struct Source {
    label: String,
    style_domain: String,
    use_: Use,
    held_reason: Option<String>,
    slice: Option<String>,
    quota: u64,
    cap: Option<u64>,
    group_ptr: String,
    eval_domain: Option<String>,
}

struct Scan {
    kind: String,
    path: PathBuf,
    family_prefix: String,
    drop_ids_from: Option<PathBuf>,
    yield_per_unit: f64,
}

struct EvalDomain {
    name: String,
    style_domain: String,
    collections: Vec<String>,
    scans: Vec<Scan>,
    targets: [f64; 4],
    cap: f64,
    oversample: f64,
    range: (u32, u32),
}

struct NearCheck {
    train_pool: PathBuf,
    collection: String,
    window: usize,
    eval: Vec<(String, PathBuf)>,
}

struct Cfg {
    raw: Value,
    seed: String,
    base: PathBuf,
    blogs: PathBuf,
    out: PathBuf,
    pool: PathBuf,
    known_missing: BTreeSet<String>,
    sources: BTreeMap<String, Source>,
    excluded: BTreeMap<String, String>,
    eval: Vec<EvalDomain>,
    near: Option<NearCheck>,
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn path(v: &Value, k: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(v[k].as_str().with_context(|| format!("config {k}"))?))
}

fn load(p: &Path) -> Result<Cfg> {
    let v: Value = serde_json::from_slice(&std::fs::read(p)?)?;
    let mut sources = BTreeMap::new();
    for (coll, x) in v["sources"].as_object().context("sources")? {
        let use_ = match s(x, "use") {
            "train" => Use::Train,
            "held" => Use::Held,
            "diagnostic" => Use::Diagnostic,
            u => bail!("source {coll}: unknown use {u}"),
        };
        ensure!(use_ != Use::Held || x["held_reason"].is_string(), "held source {coll} needs held_reason");
        ensure!(use_ != Use::Diagnostic || x["slice"].is_string(), "diagnostic source {coll} needs slice");
        sources.insert(coll.clone(), Source {
            label: x["label"].as_str().context("label")?.to_owned(),
            style_domain: x["style_domain"].as_str().context("style_domain")?.to_owned(),
            use_,
            held_reason: x["held_reason"].as_str().map(str::to_owned),
            slice: x["slice"].as_str().map(str::to_owned),
            quota: x["train_quota"].as_u64().unwrap_or(0),
            cap: x["train_max_docs_per_contributor"].as_u64(),
            group_ptr: x["group_pointer"].as_str().unwrap_or("/writer_key").to_owned(),
            eval_domain: x["eval_domain"].as_str().map(str::to_owned),
        });
    }
    let mut excluded = BTreeMap::new();
    for (coll, why) in v["excluded_collections"].as_object().into_iter().flatten() {
        excluded.insert(coll.clone(), why.as_str().unwrap_or("").to_owned());
    }
    let mut eval = Vec::new();
    for (name, x) in v["evaluation_domains"].as_object().context("evaluation_domains")? {
        let mut targets = [0.0; 4];
        for (j, sp) in EVAL_SPLITS.iter().enumerate() {
            targets[j] = x["targets"][sp].as_f64().with_context(|| format!("target {name} {sp}"))?;
        }
        let cap = x["max_docs_per_group"].as_f64().context("max_docs_per_group")?;
        ensure!(cap >= 1.0 && cap.fract() == 0.0, "cap of {name}");
        let oversample = x["oversample"].as_f64().context("oversample")?;
        ensure!(oversample >= 1.0, "oversample of {name}");
        let r = x["bucket_range"].as_array().context("bucket_range")?;
        let range = (r[0].as_u64().context("range")? as u32, r[1].as_u64().context("range")? as u32);
        ensure!(range.0 < range.1 && range.1 <= 1000, "range of {name}");
        let mut scans = Vec::new();
        for sc in x["scans"].as_array().context("scans")? {
            let y = sc["yield_per_unit"].as_f64().context("yield_per_unit")?;
            ensure!(y > 0.0, "yield of {name}");
            scans.push(Scan {
                kind: sc["kind"].as_str().context("scan kind")?.to_owned(),
                path: path(sc, "path")?,
                family_prefix: sc["family_prefix"].as_str().unwrap_or("").to_owned(),
                drop_ids_from: sc["drop_ids_from"].as_str().map(PathBuf::from),
                yield_per_unit: y,
            });
        }
        eval.push(EvalDomain {
            name: name.clone(),
            style_domain: x["style_domain"].as_str().context("style_domain")?.to_owned(),
            collections: x["collections"].as_array().context("collections")?.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            scans,
            targets,
            cap,
            oversample,
            range,
        });
    }
    for src in sources.values() {
        if let Some(d) = &src.eval_domain {
            ensure!(eval.iter().any(|e| &e.name == d), "source {} names unknown evaluation domain {d}", src.label);
        }
    }
    let near = if v["near_window_check"].is_object() {
        let n = &v["near_window_check"];
        Some(NearCheck {
            train_pool: path(n, "train_pool")?,
            collection: n["collection"].as_str().context("near collection")?.to_owned(),
            window: n["window"].as_u64().context("near window")? as usize,
            eval: n["evaluation_candidates"].as_object().context("near evaluation_candidates")?.iter()
                .map(|(k, p)| Ok((k.clone(), PathBuf::from(p.as_str().context("near path")?)))).collect::<Result<_>>()?,
        })
    } else {
        None
    };
    Ok(Cfg {
        seed: v["seed"].as_str().context("seed")?.to_owned(),
        base: path(&v, "base")?,
        blogs: path(&v, "blogs")?,
        out: path(&v, "out")?,
        pool: path(&v, "pool")?,
        known_missing: v["base_sha256sums_known_missing"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect(),
        sources,
        excluded,
        eval,
        near,
        raw: v,
    })
}

/// Verify a directory's `SHA256SUMS`; listed files in `known_missing` that do
/// not exist are reported instead of failing.
fn verify_sums(dir: &Path, known_missing: &BTreeSet<String>) -> Result<(BTreeMap<String, String>, Vec<String>)> {
    let mut m = BTreeMap::new();
    let mut missing = Vec::new();
    for line in std::fs::read_to_string(dir.join("SHA256SUMS"))?.lines() {
        let Some((h, p)) = line.split_once("  ") else { continue };
        let rel = p.trim_start_matches("./").to_owned();
        if !dir.join(&rel).exists() && known_missing.contains(&rel) {
            missing.push(rel);
            continue;
        }
        let (got, _) = io::file_sha(&dir.join(&rel)).with_context(|| format!("{} lists {rel}", dir.display()))?;
        ensure!(got == h, "{}: {rel} differs from SHA256SUMS", dir.display());
        m.insert(rel, h.to_owned());
    }
    ensure!(!m.is_empty(), "empty SHA256SUMS in {}", dir.display());
    Ok((m, missing))
}

fn copy_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d)?;
    }
    ensure!(!to.exists(), "{} exists", to.display());
    std::fs::copy(from, to)?;
    Ok(())
}

/// True for an empty file or one whose last byte is a newline.
fn ends_with_newline(p: &Path) -> Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(p)?;
    let n = f.metadata()?.len();
    if n == 0 {
        return Ok(true);
    }
    f.seek(SeekFrom::Start(n - 1))?;
    let mut b = [0u8; 1];
    f.read_exact(&mut b)?;
    Ok(b[0] == b'\n')
}

/// Writer that starts with exact copies of earlier files and appends rows.
struct Appender {
    w: std::io::BufWriter<std::fs::File>,
    rows: u64,
    parts: Vec<(String, u64)>,
}

impl Appender {
    fn start(out: &Path) -> Result<Self> {
        if let Some(d) = out.parent() {
            std::fs::create_dir_all(d)?;
        }
        let f = std::fs::OpenOptions::new().write(true).create_new(true).open(out).with_context(|| format!("create {}", out.display()))?;
        Ok(Appender { w: std::io::BufWriter::with_capacity(1 << 20, f), rows: 0, parts: Vec::new() })
    }
    /// Append a file's exact bytes (it must end with a newline).
    fn copy(&mut self, label: &str, from: &Path) -> Result<u64> {
        let r = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(from).with_context(|| format!("{}", from.display()))?);
        let mut n = 0u64;
        for line in r.split(b'\n') {
            let line = line?;
            self.w.write_all(&line)?;
            self.w.write_all(b"\n")?;
            n += 1;
        }
        ensure!(ends_with_newline(from)?, "{} does not end with a newline", from.display());
        self.rows += n;
        self.parts.push((label.to_owned(), n));
        Ok(n)
    }
    fn write(&mut self, v: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.w, v)?;
        self.w.write_all(b"\n")?;
        self.rows += 1;
        Ok(())
    }
    fn finish(mut self) -> Result<(u64, Vec<(String, u64)>)> {
        self.w.flush()?;
        self.w.get_ref().sync_all()?;
        Ok((self.rows, self.parts))
    }
}

struct NewDoc {
    id: String,
    coll: String,
    family: String,
    component: String,
    contributor: Option<String>,
    group: Option<String>,
    writer: Option<String>,
    words: u64,
    hashes: [String; 3],
    rank: u64,
    held: Vec<String>,
}

/// Reserved units of one scan: (group key, family key, metadata).
fn scan_units(sc: &Scan, range: (u32, u32), counts: &mut BTreeMap<String, u64>) -> Result<Vec<(String, String, Value)>> {
    let mut out = Vec::new();
    match sc.kind.as_str() {
        "adapter_metadata_scan" => {
            let rep: Value = serde_json::from_slice(&std::fs::read(sc.path.join("report.json")).with_context(|| format!("{}", sc.path.display()))?)?;
            ensure!(rep["complete"] == true && rep["metadata_only"].is_object() && rep["candidates"] == 0, "{} is not a complete metadata-only scan", sc.path.display());
            let (sha, _) = io::file_sha(&sc.path.join("reserved-metadata.jsonl"))?;
            ensure!(rep["metadata_only"]["sha256"] == sha.as_str(), "{} metadata changed since its report", sc.path.display());
            for r in io::jsonl_rows(&sc.path.join("reserved-metadata.jsonl"))? {
                let r = r?;
                ensure!(r["prose_opened"] == false, "scan row with prose_opened != false");
                let fam = s(&r, "family_key").to_owned();
                let g = if r["group_key"].is_string() { s(&r, "group_key").to_owned() } else { fam.clone() };
                let b = r["bucket"].as_u64().map(|b| b as u32).unwrap_or_else(|| bucket(&g));
                ensure!(b == bucket(&g), "scan row {fam}: bucket is not the group key's bucket");
                if !(range.0..range.1).contains(&b) {
                    *counts.entry("scan_row_outside_reserved_range".into()).or_default() += 1;
                    continue;
                }
                if r["eligible_metadata"] == false {
                    *counts.entry("metadata_not_eligible".into()).or_default() += 1;
                    continue;
                }
                out.push((g, fam, r));
            }
        }
        "essay_ids" => {
            let drop: HashSet<String> = match &sc.drop_ids_from {
                Some(p) => io::jsonl_rows(p)?.filter_map(|r| r.ok()).filter_map(|r| r["essay_id_comp"].as_str().map(str::to_owned)).collect(),
                None => HashSet::new(),
            };
            for r in io::jsonl_rows(&sc.path)? {
                let r = r?;
                ensure!(r.get("text").is_none(), "essay id rows carry no text");
                let g = s(&r, "prompt_key").to_owned();
                let b = bucket(&g);
                ensure!(r["bucket"].as_u64() == Some(b as u64), "essay id row bucket is not its prompt key's bucket");
                if !(range.0..range.1).contains(&b) {
                    *counts.entry("scan_row_outside_reserved_range".into()).or_default() += 1;
                    continue;
                }
                if drop.contains(s(&r, "id")) || drop.contains(s(&r, "essay_id")) {
                    *counts.entry("essay_is_a_copy_of_a_listed_corpus_essay".into()).or_default() += 1;
                    continue;
                }
                let fam = format!("{}:essay:{}", sc.family_prefix, s(&r, "id"));
                out.push((g, fam, r));
            }
        }
        "blog_reserved_units" => {
            let mut linked: HashSet<String> = HashSet::new();
            for g in io::jsonl_rows(&sc.path.join("groups.jsonl"))? {
                let g = g?;
                if g["linked_fineweb_evaluation_splits"].as_array().is_some_and(|a| !a.is_empty()) {
                    linked.insert(s(&g, "group_key").to_owned());
                }
            }
            for r in io::jsonl_rows(&sc.path.join("units.jsonl"))? {
                let r = r?;
                let g = s(&r, "group_key").to_owned();
                let b = bucket(&g);
                if !(range.0..range.1).contains(&b) {
                    *counts.entry("scan_row_outside_reserved_range".into()).or_default() += 1;
                    continue;
                }
                if linked.contains(&g) {
                    *counts.entry("group_linked_to_sealed_fineweb_evaluation_left_unused".into()).or_default() += 1;
                    continue;
                }
                if r["eligible_metadata"] == false {
                    *counts.entry("metadata_not_eligible".into()).or_default() += 1;
                    continue;
                }
                out.push((g, s(&r, "family_key").to_owned(), r));
            }
        }
        k => bail!("unknown scan kind {k}"),
    }
    Ok(out)
}

/// Base evaluation and spare family keys of splits-v3 (base and new domains).
fn base_eval_families(base: &Path) -> Result<HashSet<String>> {
    let mut f = HashSet::new();
    for root in [base.to_path_buf(), base.join("new-domains")] {
        for sp in EVAL_SPLITS {
            let p = root.join(sp.replace('_', "-")).join("families.txt");
            for l in std::fs::read_to_string(&p).with_context(|| format!("{}", p.display()))?.lines() {
                f.insert(l.trim().to_owned());
            }
        }
        for g in io::jsonl_rows(&root.join("reserved-unassigned").join("groups.jsonl"))? {
            for k in g?["family_keys"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                f.insert(k.to_owned());
            }
        }
    }
    Ok(f)
}

/// Group keys of every splits-v3 evaluation and spare group (base and new
/// domains). Key namespaces can be shared across sources (for example
/// `author:` for Gutenberg writers and Wikisource authors), so a new domain
/// never reuses one.
fn base_group_keys(base: &Path) -> Result<HashSet<String>> {
    let mut k = HashSet::new();
    for root in [base.to_path_buf(), base.join("new-domains")] {
        for sp in EVAL_SPLITS.iter().map(|s| s.replace('_', "-")).chain(["reserved-unassigned".to_owned()]) {
            for g in io::jsonl_rows(&root.join(&sp).join("groups.jsonl"))? {
                k.insert(g?["group_key"].as_str().context("group_key")?.to_owned());
            }
        }
    }
    Ok(k)
}

/// Folded `n`-word windows (the splits-v4 Global Voices near-window check).
pub fn windows(t: &str, n: usize) -> Vec<u64> {
    let toks = text::folded_tokens(t);
    if toks.len() < n {
        return Vec::new();
    }
    (0..=toks.len() - n).map(|i| text::h64(toks[i..i + n].join(" ").as_bytes())).collect()
}

/// Folded `window`-word overlap of acquired evaluation candidates of one
/// collection with that collection's Train-side rows (text-free output).
fn near_check(n: &NearCheck, out: &Path) -> Result<Value> {
    let mut idx: HashMap<u64, u32> = HashMap::new();
    let mut ids: Vec<String> = Vec::new();
    for r in io::jsonl_rows(&n.train_pool)? {
        let r = r?;
        if s(&r, "collection") != n.collection {
            continue;
        }
        let k = ids.len() as u32;
        ids.push(s(&r, "id").to_owned());
        for w in windows(s(&r, "text"), n.window) {
            idx.entry(w).or_insert(k);
        }
    }
    io::fresh_dir(out)?;
    let mut w = JsonlWriter::create(&out.join("global-voices-near-window-hits.jsonl"))?;
    let mut per = serde_json::Map::new();
    for (split, p) in &n.eval {
        let (mut seen, mut hit) = (0u64, 0u64);
        for r in io::jsonl_rows(p)? {
            let r = r?;
            if s(&r, "collection") != n.collection {
                continue;
            }
            seen += 1;
            let mut shared = 0u64;
            let mut with: BTreeSet<&str> = BTreeSet::new();
            for x in windows(s(&r, "text"), n.window) {
                if let Some(&k) = idx.get(&x) {
                    shared += 1;
                    with.insert(&ids[k as usize]);
                }
            }
            if shared > 0 {
                hit += 1;
                w.write(&json!({"schema": NEAR_CHECK_SCHEMA, "split": split, "evaluation_id": s(&r, "id"), "source_group": s(&r, "source_group"),
                    "shared_windows": shared, "train_side_ids": with.iter().take(10).collect::<Vec<_>>(),
                    "action": "exclude this acquired candidate at evaluation selection (shares a folded near-window with Train-side text)"}))?;
            }
        }
        per.insert(split.clone(), json!({"path": p, "candidates_checked": seen, "candidates_with_shared_window": hit}));
    }
    let (sha, rows) = w.finish()?;
    let rep = json!({"schema": NEAR_CHECK_SCHEMA, "collection": n.collection, "window": n.window, "window_basis": "folded alphanumeric tokens",
        "train_side": {"path": n.train_pool, "rows": ids.len(), "distinct_windows": idx.len()}, "by_split": per,
        "hits": {"file": "global-voices-near-window-hits.jsonl", "rows": rows, "sha256": sha}, "text_written": false});
    io::write_json(&out.join("global-voices-near-window-check.json"), &rep)?;
    Ok(rep)
}

pub fn run(config: &Path) -> Result<Value> {
    let c = load(config)?;
    let started = io::now_utc();
    let (base_sums, base_missing) = verify_sums(&c.base, &c.known_missing)?;
    let (blog_sums, _) = verify_sums(&c.blogs, &BTreeSet::new())?;
    io::fresh_dir(&c.out)?;
    let base_cfg: Value = serde_json::from_slice(&std::fs::read(c.base.join("config.json"))?)?;
    ensure!(base_cfg["seed"] == c.seed.as_str(), "seed differs from the base split");
    let mut notes: Vec<String> = Vec::new();

    // ------------------------------------------------------------ earlier ids
    let mut earlier_ids: HashSet<String> = HashSet::new();
    let mut earlier_rows: BTreeMap<String, u64> = BTreeMap::new();
    for (label, dir) in [("splits_v3", &c.base), ("blogs", &c.blogs)] {
        for sp in ["train", "train-mining", "held"] {
            let mut n = 0u64;
            for r in io::jsonl_rows(&dir.join(sp).join("manifest.jsonl"))? {
                let id = r?["id"].as_str().context("manifest id")?.to_owned();
                ensure!(earlier_ids.insert(id.clone()), "id {id} assigned twice in the earlier splits");
                n += 1;
            }
            earlier_rows.insert(format!("{label}/{sp}"), n);
        }
    }
    let foreign_families = base_eval_families(&c.base)?;
    let foreign_groups = base_group_keys(&c.base)?;

    // ------------------------------------------------------------ new rows
    let mut docs: Vec<NewDoc> = Vec::new();
    let mut ids: HashSet<String> = HashSet::new();
    let mut by_coll: BTreeMap<String, u64> = BTreeMap::new();
    let mut train_groups: HashMap<String, HashSet<String>> = HashMap::new();
    let mut train_families: HashSet<String> = HashSet::new();
    let mut pool_rows = 0u64;
    for r in io::jsonl_rows(&c.pool)? {
        let r = r?;
        pool_rows += 1;
        ensure!(r["pool_status"] == "admitted", "pool row not admitted");
        let coll = s(&r, "collection").to_owned();
        if let Some(why) = c.excluded.get(&coll) {
            bail!("pool row of excluded collection {coll} ({why})");
        }
        let src = c.sources.get(&coll).with_context(|| format!("pool collection {coll} has no source entry"))?;
        let id = s(&r, "id").to_owned();
        ensure!(!earlier_ids.contains(&id), "pool id {id} already assigned in splits-v3 or the blog splits");
        ensure!(ids.insert(id.clone()), "duplicate pool id {id}");
        let family = s(&r, "source_group").to_owned();
        ensure!(!foreign_families.contains(&family), "pool family {family} is reserved for evaluation in splits-v3");
        let group = r.pointer(&src.group_ptr).and_then(Value::as_str).map(str::to_owned);
        if src.eval_domain.is_some() {
            let g = group.clone().with_context(|| format!("row {id} lacks its group key {}", src.group_ptr))?;
            let d = c.eval.iter().find(|e| Some(&e.name) == src.eval_domain.as_ref()).context("eval domain")?;
            ensure!(bucket(&g) < d.range.0, "row {id}: group key {g} lies in the reserved range of {}", d.name);
            if src.use_ != Use::Diagnostic {
                train_groups.entry(d.name.clone()).or_default().insert(g);
            }
        }
        if src.use_ != Use::Diagnostic {
            train_families.insert(family.clone());
        }
        let mut held = Vec::new();
        if src.use_ == Use::Held {
            held.push(src.held_reason.clone().unwrap_or_default());
        }
        *by_coll.entry(coll.clone()).or_default() += 1;
        docs.push(NewDoc {
            rank: rank(&c.seed, "train_document_v4", &id),
            component: r["pool_admission"]["split_component"].as_str().unwrap_or(&family).to_owned(),
            writer: r["writer_key"].as_str().map(str::to_owned),
            contributor: r["writer_key"].as_str().map(str::to_owned).or_else(|| group.clone()),
            group,
            words: r["word_count"].as_u64().unwrap_or(0),
            hashes: [s(&r, "text_sha256").to_owned(), s(&r, "normalized_text_sha256").to_owned(), s(&r, "folded_text_sha256").to_owned()],
            id,
            coll,
            family,
            held,
        });
    }
    for (coll, src) in &c.sources {
        if !by_coll.contains_key(coll) {
            notes.push(format!("source {} ({coll}) has no pool rows", src.label));
        }
    }

    // ------------------------------------------------------------ evaluation domains
    let mut assigned: Vec<[Vec<Group>; 4]> = Vec::new();
    let mut spare: Vec<Vec<Group>> = Vec::new();
    let mut universe = serde_json::Map::new();
    for d in &c.eval {
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        let mut by: BTreeMap<String, Vec<Unit>> = BTreeMap::new();
        let mut fams: HashSet<String> = HashSet::new();
        let tg = train_groups.get(&d.name).cloned().unwrap_or_default();
        let mut rows_in = 0u64;
        for sc in &d.scans {
            for (g, fam, r) in scan_units(sc, d.range, &mut counts)? {
                rows_in += 1;
                if tg.contains(&g) {
                    bail!("{}: reserved group {g} has Train-side text", d.name);
                }
                if foreign_groups.contains(&g) {
                    *counts.entry("group_key_reserved_in_splits_v3_left_unused".into()).or_default() += 1;
                    continue;
                }
                if train_families.contains(&fam) || foreign_families.contains(&fam) {
                    *counts.entry("family_with_train_side_or_base_evaluation_text".into()).or_default() += 1;
                    continue;
                }
                if !fams.insert(fam.clone()) {
                    *counts.entry("unit_repeated_counted_once".into()).or_default() += 1;
                    continue;
                }
                let mut meta = r;
                if let Some(m) = meta.as_object_mut() {
                    for k in ["schema", "prose_opened", "family_key", "group_key", "bucket", "bucket_basis", "eligible_metadata"] {
                        m.remove(k);
                    }
                }
                meta["scan"] = json!(sc.path);
                by.entry(g).or_default().push(Unit { family_key: fam, expected_docs: sc.yield_per_unit, meta });
            }
        }
        let mut gs: Vec<Group> = by.into_iter().map(|(k, units)| Group { rank: rank(&c.seed, &format!("v4:{}", d.name), &k), contributors: BTreeSet::from([k.clone()]), key: k, units, train_side_docs: 0 }).collect();
        gs.sort_by(|a, b| (a.rank, &a.key).cmp(&(b.rank, &b.key)));
        let exp: Vec<f64> = gs.iter().map(|g| g.expected(d.cap)).collect();
        let total: f64 = exp.iter().sum();
        let mut a: [Vec<Group>; 4] = Default::default();
        let mut sp = Vec::new();
        for (g, x) in gs.into_iter().zip(allocate(&exp, &d.targets, d.oversample)) {
            match x {
                Some(i) => a[i].push(g),
                None => sp.push(g),
            }
        }
        universe.insert(d.name.clone(), json!({"scan_rows_in_range_eligible": rows_in, "counts": counts, "groups": exp.len(), "expected_capped_capacity_all_groups": total,
            "train_side_group_keys": tg.len()}));
        assigned.push(a);
        spare.push(sp);
    }
    // No key or family in two splits.
    let mut owner: HashMap<String, usize> = HashMap::new();
    let mut cross = 0u64;
    for a in &assigned {
        for (sx, gs) in a.iter().enumerate() {
            for g in gs {
                for k in g.contributors.iter().cloned().chain(g.units.iter().map(|u| u.family_key.clone())) {
                    if owner.insert(k, sx).is_some_and(|p| p != sx) {
                        cross += 1;
                    }
                }
            }
        }
    }
    ensure!(cross == 0, "new evaluation splits share {cross} group or family keys");

    // ------------------------------------------------------------ Train selection
    let mut comp: BTreeMap<(String, String), Vec<u32>> = BTreeMap::new();
    for (i, d) in docs.iter().enumerate() {
        if d.held.is_empty() && c.sources[&d.coll].use_ == Use::Train {
            comp.entry((d.coll.clone(), d.component.clone())).or_default().push(i as u32);
        }
    }
    let mut comps: Vec<((String, String), Vec<u32>)> = comp.into_iter().collect();
    comps.sort_by_key(|((coll, k), _)| (coll.clone(), rank(&c.seed, "train_component_v4", k), k.clone()));
    let mut in_train: HashSet<u32> = HashSet::new();
    let mut train_n: BTreeMap<String, u64> = BTreeMap::new();
    let mut per: HashMap<(String, String), u64> = HashMap::new();
    let mut cap_skipped: BTreeMap<String, u64> = BTreeMap::new();
    for ((coll, _), members) in &comps {
        let src = &c.sources[coll];
        let n = train_n.entry(coll.clone()).or_default();
        if *n + members.len() as u64 > src.quota {
            continue;
        }
        if let Some(cap) = src.cap {
            let mut need: HashMap<String, u64> = HashMap::new();
            for m in members {
                if let Some(k) = &docs[*m as usize].contributor {
                    *need.entry(k.clone()).or_default() += 1;
                }
            }
            if need.iter().any(|(k, x)| per.get(&(coll.clone(), k.clone())).copied().unwrap_or(0) + x > cap) {
                *cap_skipped.entry(coll.clone()).or_default() += members.len() as u64;
                continue;
            }
            for (k, x) in need {
                *per.entry((coll.clone(), k)).or_default() += x;
            }
        }
        *n += members.len() as u64;
        in_train.extend(members.iter().copied());
    }
    let split_of = |i: usize| -> &'static str {
        let src = &c.sources[&docs[i].coll];
        if src.use_ == Use::Diagnostic {
            "diagnostic"
        } else if !docs[i].held.is_empty() {
            "held"
        } else if in_train.contains(&(i as u32)) {
            "train"
        } else {
            "train_mining"
        }
    };

    // ------------------------------------------------------------ outputs
    let out = &c.out;
    let mut copied = Vec::new();
    for rel in base_sums.keys() {
        let top = rel.split('/').next().unwrap_or("");
        if ["test", "cal", "dev-select", "dev-iter", "reserved-unassigned", "new-domains", "base-v2"].contains(&top) {
            copy_file(&c.base.join(rel), &out.join(rel))?;
            copied.push(rel.clone());
        } else if !rel.contains('/') {
            copy_file(&c.base.join(rel), &out.join("base-v3").join(rel))?;
        }
    }
    for rel in blog_sums.keys() {
        if rel.starts_with("evaluation-reserved/") {
            copy_file(&c.blogs.join(rel), &out.join("blogs-v3").join(rel))?;
        }
    }
    copy_file(&c.base.join("SHA256SUMS"), &out.join("base-v3").join("SHA256SUMS"))?;
    for rel in ["SHA256SUMS", "summary.json"] {
        copy_file(&c.blogs.join(rel), &out.join("blogs-v3").join(rel))?;
    }
    if !base_missing.is_empty() {
        io::write_json(&out.join("base-v3").join("SHA256SUMS-defect.json"), &json!({"listed_but_absent": base_missing,
            "rule": "splits-v3/SHA256SUMS lists these paths, which never existed in splits-v3 (a temporary file of its own checksum step); every other listed file was verified; splits-v4/SHA256SUMS lists only files that exist"}))?;
    }

    // Evaluation reservations.
    let mut eval_summary = serde_json::Map::new();
    let mut shortfalls = Vec::new();
    for (sx, sname) in EVAL_SPLITS.iter().enumerate() {
        let dir = out.join("new-domains-v4").join(sname.replace('_', "-"));
        io::fresh_dir(&dir)?;
        let mut uw = JsonlWriter::create(&dir.join("manifest.jsonl"))?;
        let mut gw = JsonlWriter::create(&dir.join("groups.jsonl"))?;
        let mut fams = String::new();
        let mut per_dom = serde_json::Map::new();
        let status = match *sname {
            "dev_iter" => "not_acquired_acquirable_now_exposed_by_design",
            "test" => "sealed_acquire_only_after_protocol_seal",
            _ => "sealed_not_acquired_until_protocol_step",
        };
        for (di, d) in c.eval.iter().enumerate() {
            let (target, cap) = (d.targets[sx], d.cap);
            let (mut exp_sum, mut nunits) = (0.0, 0u64);
            for (order, g) in assigned[di][sx].iter().enumerate() {
                let e = g.expected(cap);
                exp_sum += e;
                nunits += g.units.len() as u64;
                gw.write(&json!({"schema": GROUP_SCHEMA, "split": sname, "domain": d.name, "style_domain": d.style_domain, "group_key": g.key,
                    "group_rank": format!("{:016x}", g.rank), "reserved_bucket_range": [d.range.0, d.range.1], "order_in_split_domain": order,
                    "units": g.units.len(), "expected_docs_capped": e, "max_docs_per_group": cap,
                    "contributor_key_sha256": g.contributors.iter().map(|k| text::sha_hex(k.as_bytes())).collect::<Vec<_>>()}))?;
                let mut us: Vec<&Unit> = g.units.iter().collect();
                us.sort_by_key(|u| (rank(&c.seed, "unit", &u.family_key), u.family_key.clone()));
                for (uo, u) in us.into_iter().enumerate() {
                    uw.write(&json!({"schema": RESERVATION_SCHEMA, "split": sname, "domain": d.name, "group_key": g.key, "group_order": order,
                        "unit_order_in_group": uo, "family_key": u.family_key, "bucket": bucket(&g.key), "bucket_basis": "group_key",
                        "expected_docs": u.expected_docs, "source_unit": u.meta, "prose_status": status, "text_sha256": null,
                        "text_hashes_note": "excerpt text hashes are recorded at acquisition; source-unit identifiers above bind the reserved inputs"}))?;
                    fams.push_str(&u.family_key);
                    fams.push('\n');
                }
            }
            let ngroups = assigned[di][sx].len();
            if target > 0.0 && exp_sum < target * d.oversample * 0.999 {
                shortfalls.push(json!({"split": sname, "domain": d.name, "target_human_docs": target, "expected_capped_capacity": exp_sum,
                    "issue": if exp_sum < target { "expected capacity below target (reserved groups exhausted)" } else { "expected capacity below target x oversample" }}));
            }
            if *sname == "test" && ngroups < 15 {
                shortfalls.push(json!({"split": sname, "domain": d.name, "groups": ngroups, "issue": "fewer than 15 Test groups (slice gate minimum)"}));
            }
            per_dom.insert(d.name.clone(), json!({"style_domain": d.style_domain, "groups": ngroups, "source_units": nunits, "target_human_docs": target,
                "reserved_bucket_range": [d.range.0, d.range.1], "expected_capped_capacity": exp_sum, "capacity_over_target": exp_sum / target.max(1.0),
                "max_docs_per_group": cap, "reserve_oversample": d.oversample}));
        }
        let (usha, urows) = uw.finish()?;
        let (gsha, grows) = gw.finish()?;
        std::fs::write(dir.join("families.txt"), &fams)?;
        let seal = json!({"split": sname, "prose_status": status, "prose_opened": false, "text_read_or_printed": false,
            "domains": c.eval.iter().map(|d| &d.name).collect::<Vec<_>>(),
            "rule": match *sname {
                "test" => "Test prose is acquired only after the protocol seal; manifest is metadata only",
                "dev_iter" => "Dev-iter is exposed by design and may be acquired for iterative decisions; nothing is acquired by this reservation",
                "dev_select" => "Dev-select prose stays sealed until finalists are bound; opened once to choose among at most four finalists",
                _ => "CAL prose stays sealed until the protocol calibration step",
            },
            "acquisition_rule": "process groups in group_order; within a group take units in unit_order_in_group; keep at most max_docs_per_group passing documents per group; stop a domain at its target; record every skip; no backfill: a domain that exhausts its reserved groups is evaluated at its realised size; unreached groups stay reserved and unused (never Train, never mined, never mirror topics); acquisition deduplicates against every Train-side pool, the prior index and the other splits",
            "earlier_domains": "the base and splits-v3 new domains keep their reservation unchanged in ../../<split>/ and ../../new-domains/<split>/",
            "manifest_sha256": usha, "manifest_rows": urows, "groups_sha256": gsha, "groups_rows": grows});
        io::write_json(&dir.join("SEALED.json"), &seal)?;
        eval_summary.insert((*sname).to_owned(), json!({"by_domain": per_dom, "manifest_sha256": usha, "groups_sha256": gsha, "prose_status": status}));
    }
    {
        let dir = out.join("new-domains-v4").join("reserved-unassigned");
        io::fresh_dir(&dir)?;
        let mut w = JsonlWriter::create(&dir.join("groups.jsonl"))?;
        let mut per_dom = serde_json::Map::new();
        for (di, d) in c.eval.iter().enumerate() {
            for g in &spare[di] {
                w.write(&json!({"schema": GROUP_SCHEMA, "split": "reserved_unassigned", "domain": d.name, "group_key": g.key,
                    "group_rank": format!("{:016x}", g.rank), "units": g.units.len(), "family_keys": g.units.iter().map(|u| &u.family_key).collect::<Vec<_>>()}))?;
            }
            per_dom.insert(d.name.clone(), json!({"groups": spare[di].len(), "source_units": spare[di].iter().map(|g| g.units.len()).sum::<usize>()}));
        }
        let (sha, rows) = w.finish()?;
        eval_summary.insert("reserved_unassigned".into(), json!({"by_domain": per_dom, "groups_sha256": sha, "rows": rows}));
    }

    // Train-side manifests: splits-v3 lines, blog lines, then new rows.
    let manifest_row = |i: usize, sp: &str| -> Value {
        let d = &docs[i];
        let src = &c.sources[&d.coll];
        json!({"schema": DOCUMENT_SCHEMA, "split": sp, "id": d.id, "domain": src.label, "style_domain": src.style_domain, "collection": d.coll,
            "source_group": d.family, "split_component": d.component, "writer_key": d.writer, "word_count": d.words, "text_sha256": d.hashes[0],
            "normalized_text_sha256": d.hashes[1], "folded_text_sha256": d.hashes[2], "rank": format!("{:016x}", d.rank),
            "held_reasons": if d.held.is_empty() { json!(null) } else { json!(d.held) }, "splits_version": VERSION})
    };
    let mut ts = serde_json::Map::new();
    for name in ["train", "train_mining", "held"] {
        let dn = name.replace('_', "-");
        let mut w = Appender::start(&out.join(&dn).join("manifest.jsonl"))?;
        w.copy("splits_v3", &c.base.join(&dn).join("manifest.jsonl"))?;
        w.copy("blogs_v3", &c.blogs.join(&dn).join("manifest.jsonl"))?;
        let mut order: Vec<usize> = (0..docs.len()).filter(|&i| split_of(i) == name).collect();
        order.sort_by_key(|&i| (docs[i].coll.clone(), docs[i].rank, i));
        let mut by: BTreeMap<&str, (u64, HashSet<&str>, HashSet<&str>, u64)> = BTreeMap::new();
        for &i in &order {
            let d = &docs[i];
            let e = by.entry(c.sources[&d.coll].label.as_str()).or_default();
            e.0 += 1;
            e.1.insert(&d.family);
            if let Some(k) = &d.contributor {
                e.2.insert(k);
            }
            e.3 += d.words;
            w.write(&manifest_row(i, name))?;
        }
        let (rows, parts) = w.finish()?;
        ts.insert(name.to_owned(), json!({"rows": rows, "earlier_parts": parts.iter().map(|(l, n)| json!({"part": l, "rows": n})).collect::<Vec<_>>(),
            "new_rows": order.len(), "new_by_source": by.iter().map(|(k, v)| ((*k).to_owned(), json!({"documents": v.0, "source_families": v.1.len(), "contributor_keys": v.2.len(), "mean_words": v.3 as f64 / v.0.max(1) as f64}))).collect::<serde_json::Map<_, _>>()}));
    }
    // Text rows and diagnostic slices.
    let pos: HashMap<&str, usize> = docs.iter().enumerate().map(|(i, d)| (d.id.as_str(), i)).collect();
    let mut slices: BTreeMap<String, (JsonlWriter, JsonlWriter)> = BTreeMap::new();
    for src in c.sources.values() {
        if let Some(sl) = &src.slice {
            if !slices.contains_key(sl) {
                let dir = out.join("diagnostics").join(sl);
                io::fresh_dir(&dir)?;
                slices.insert(sl.clone(), (JsonlWriter::create(&dir.join("documents.jsonl"))?, JsonlWriter::create(&dir.join("manifest.jsonl"))?));
            }
        }
    }
    {
        let mut wt = Appender::start(&out.join("train").join("documents.jsonl"))?;
        let mut wm = Appender::start(&out.join("train-mining").join("documents.jsonl"))?;
        let (tb, mb) = (wt.copy("splits_v3", &c.base.join("train").join("documents.jsonl"))?, wm.copy("splits_v3", &c.base.join("train-mining").join("documents.jsonl"))?);
        let (tbb, mbb) = (wt.copy("blogs_v3", &c.blogs.join("train").join("documents.jsonl"))?, wm.copy("blogs_v3", &c.blogs.join("train-mining").join("documents.jsonl"))?);
        ensure!(tb == earlier_rows["splits_v3/train"] && mb == earlier_rows["splits_v3/train-mining"], "splits-v3 text rows differ from its manifests");
        ensure!(tbb == earlier_rows["blogs/train"] && mbb == earlier_rows["blogs/train-mining"], "blog text rows differ from its manifests");
        for r in io::jsonl_rows(&c.pool)? {
            let mut r = r?;
            let i = pos[s(&r, "id")];
            let sp = split_of(i);
            let src = &c.sources[&docs[i].coll];
            if sp == "train" || sp == "train_mining" {
                r["frozen_split"] = json!(sp);
                r["split_assigned"] = json!(true);
                r["split_assignment"] = json!({"schema": DOCUMENT_SCHEMA, "split": sp, "seed": c.seed, "splits_version": VERSION, "source": src.label, "style_domain": src.style_domain,
                    "rule": "read only splits-v4 train/documents.jsonl or train-mining/documents.jsonl; pool files include held and evaluation-only documents and are not training inputs"});
                if sp == "train" { wt.write(&r)? } else { wm.write(&r)? }
            }
            if let Some(sl) = &src.slice {
                let tag = format!("diagnostic_{sl}");
                r["frozen_split"] = json!(tag);
                r["split_assigned"] = json!(true);
                r["split_assignment"] = json!({"schema": SLICE_SCHEMA, "split": tag, "splits_version": VERSION, "source": src.label,
                    "rule": "ungated evaluation-only diagnostic slice; never Train, never mined, never a generation topic outside this slice"});
                let (dw, mw) = slices.get_mut(sl).context("slice writer")?;
                dw.write(&r)?;
                mw.write(&manifest_row(i, &tag))?;
            }
        }
        let (tr, _) = wt.finish()?;
        let (mr, _) = wm.finish()?;
        ensure!(tr == ts["train"]["rows"].as_u64().unwrap_or(u64::MAX), "train text rows differ from its manifest");
        ensure!(mr == ts["train_mining"]["rows"].as_u64().unwrap_or(u64::MAX), "train-mining text rows differ from its manifest");
    }
    let mut slice_summary = serde_json::Map::new();
    for (sl, (dw, mw)) in slices {
        let (dsha, drows) = dw.finish()?;
        let (msha, _) = mw.finish()?;
        let spec = &c.raw["diagnostic_slices"][&sl];
        let rep = json!({"schema": SLICE_SCHEMA, "slice": sl, "gated": false, "documents": drows, "documents_sha256": dsha, "manifest_sha256": msha,
            "spec": spec, "rule": "ungated diagnostic: reported separately, outside the primary population and every gate; never Train or mining"});
        io::write_json(&out.join("diagnostics").join(&sl).join("SLICE.json"), &rep)?;
        slice_summary.insert(sl, rep);
    }
    let near = match &c.near {
        Some(n) => near_check(n, &out.join("checks"))?,
        None => json!(null),
    };

    // ------------------------------------------------------------ summary
    let mut sources = serde_json::Map::new();
    for (coll, src) in &c.sources {
        let mut n: BTreeMap<&str, u64> = BTreeMap::new();
        for (i, d) in docs.iter().enumerate() {
            if &d.coll == coll {
                *n.entry(split_of(i)).or_default() += 1;
            }
        }
        sources.insert(src.label.clone(), json!({"collection": coll, "style_domain": src.style_domain, "use": format!("{:?}", src.use_).to_lowercase(),
            "eval_domain": src.eval_domain, "admitted_pool_rows": by_coll.get(coll).copied().unwrap_or(0), "by_split": n,
            "train_quota": src.quota, "train_max_docs_per_contributor": src.cap, "documents_skipped_by_cap": cap_skipped.get(coll).copied().unwrap_or(0)}));
    }
    let summary = json!({
        "schema": SUMMARY_SCHEMA,
        "started_utc": started,
        "finished_utc": io::now_utc(),
        "config": c.raw,
        "base": {"dir": c.base, "files_verified": base_sums.len(), "listed_but_absent": base_missing, "files_copied_unchanged": copied.len(), "earlier_rows": earlier_rows},
        "blogs": {"dir": c.blogs, "files_verified": blog_sums.len()},
        "pool": {"path": c.pool, "rows": pool_rows, "by_collection": by_coll},
        "excluded_collections": c.excluded,
        "sources": sources,
        "train_side": ts,
        "new_domain_evaluation": eval_summary,
        "evaluation_universe": universe,
        "diagnostic_slices": slice_summary,
        "near_window_check": near,
        "checks": {"new_ids_disjoint_from_earlier_splits": true, "new_families_disjoint_from_base_evaluation_families": true,
            "train_side_group_keys_outside_reserved_ranges": true, "new_evaluation_cross_split_conflicts": cross},
        "shortfalls": shortfalls,
        "notes": notes,
        "detector_calls": 0,
        "model_calls": 0,
        "evaluation_prose_opened": false,
    });
    io::write_json(&out.join("summary.json"), &summary)?;
    io::write_json(&out.join("config.json"), &c.raw)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jsonl(p: &Path, rows: &[Value]) -> String {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut w = JsonlWriter::create(p).unwrap();
        for r in rows {
            w.write(r).unwrap();
        }
        w.finish().unwrap().0
    }

    fn key_in(prefix: &str, lo: u32, hi: u32, skip: usize) -> String {
        (0..).map(|i| format!("{prefix}{i}")).filter(|k| (lo..hi).contains(&bucket(k))).nth(skip).unwrap()
    }

    fn sums(dir: &Path, extra: &[&str]) {
        let mut out = String::new();
        let mut stack = vec![dir.to_path_buf()];
        let mut files = Vec::new();
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() { stack.push(p) } else { files.push(p) }
            }
        }
        files.sort();
        for p in files {
            let rel = p.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
            out.push_str(&format!("{}  ./{rel}\n", io::file_sha(&p).unwrap().0));
        }
        for x in extra {
            out.push_str(&format!("{}  ./{x}\n", "0".repeat(64)));
        }
        std::fs::write(dir.join("SHA256SUMS"), out).unwrap();
    }

    fn row(id: &str, coll: &str, fam: &str, writer: &str) -> Value {
        let t = format!("text of {id} with some words");
        json!({"id": id, "collection": coll, "source_group": fam, "writer_key": writer, "pool_status": "admitted", "word_count": 200,
            "text": t, "text_sha256": text::sha_hex(t.as_bytes()), "normalized_text_sha256": "n", "folded_text_sha256": "f",
            "pool_admission": {"split_component": fam}, "rights": {"license": "x"}})
    }

    #[test]
    fn extension_keeps_earlier_bytes_reserves_closed_groups_and_routes_uses() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().join("v3");
        let blogs = t.path().join("blogs");
        for (dir, tag) in [(&base, "b"), (&blogs, "g")] {
            for sp in ["train", "train-mining", "held"] {
                let r = json!({"id": format!("{tag}-{sp}"), "split": sp});
                jsonl(&dir.join(sp).join("manifest.jsonl"), &[r.clone()]);
                if sp != "held" {
                    jsonl(&dir.join(sp).join("documents.jsonl"), &[json!({"id": format!("{tag}-{sp}"), "text": "x"})]);
                }
            }
        }
        for sp in ["test", "cal", "dev-select", "dev-iter"] {
            for root in [base.clone(), base.join("new-domains")] {
                std::fs::create_dir_all(root.join(sp)).unwrap();
                std::fs::write(root.join(sp).join("families.txt"), "fam:base\n").unwrap();
                jsonl(&root.join(sp).join("groups.jsonl"), &[json!({"group_key": key_in("author:b", 900, 1000, 0)})]);
            }
        }
        for root in [base.clone(), base.join("new-domains")] {
            jsonl(&root.join("reserved-unassigned").join("groups.jsonl"), &[json!({"group_key": "author:spare_writer", "family_keys": ["fam:spare"]})]);
        }
        std::fs::write(base.join("config.json"), json!({"seed": "s"}).to_string()).unwrap();
        std::fs::write(blogs.join("summary.json"), "{}").unwrap();
        jsonl(&blogs.join("evaluation-reserved").join("x").join("units.jsonl"), &[]);
        sums(&base, &["SHA256SUMS.tmp"]);
        sums(&blogs, &[]);
        // Scan: reserved groups g_r (two units) and a Train-side group g_t.
        let g_r = key_in("user:r", 900, 1000, 0);
        let g_r2 = key_in("user:r", 900, 1000, 1);
        let g_t = key_in("user:t", 0, 900, 0);
        let scan = t.path().join("scan");
        let rows = vec![
            json!({"family_key": "post:1", "group_key": g_r, "bucket": bucket(&g_r), "bucket_basis": "group_key", "prose_opened": false}),
            json!({"family_key": "post:2", "group_key": g_r, "bucket": bucket(&g_r), "bucket_basis": "group_key", "prose_opened": false}),
            json!({"family_key": "post:3", "group_key": g_r2, "bucket": bucket(&g_r2), "bucket_basis": "group_key", "prose_opened": false, "eligible_metadata": false}),
            json!({"family_key": "fam:base", "group_key": g_r2, "bucket": bucket(&g_r2), "bucket_basis": "group_key", "prose_opened": false}),
            json!({"family_key": "post:5", "group_key": key_in("author:b", 900, 1000, 0), "bucket": bucket(&key_in("author:b", 900, 1000, 0)), "bucket_basis": "group_key", "prose_opened": false}),
        ];
        let sha = jsonl(&scan.join("reserved-metadata.jsonl"), &rows);
        std::fs::write(scan.join("report.json"), json!({"complete": true, "candidates": 0, "metadata_only": {"sha256": sha}}).to_string()).unwrap();
        let pool = t.path().join("pool.jsonl");
        jsonl(&pool, &[row("n1", "forum", "post:9", &g_t), row("n2", "forum", "post:10", &g_t), row("n3", "talk", "talk:1", "w:1"), row("n4", "spoken", "rec:1", "rec:1")]);
        let out = t.path().join("v4");
        let cfg = json!({"seed": "s", "base": base, "blogs": blogs, "out": out, "pool": pool, "base_sha256sums_known_missing": ["SHA256SUMS.tmp"],
            "sources": {
                "forum": {"label": "forum", "style_domain": "modern_informal", "use": "train", "train_quota": 1, "train_max_docs_per_contributor": 1, "eval_domain": "forum"},
                "talk": {"label": "talk", "style_domain": "modern_informal", "use": "held", "held_reason": "pending", "slice": "talk_diag"},
                "spoken": {"label": "spoken", "style_domain": "spoken", "use": "diagnostic", "slice": "spoken_diag"}},
            "excluded_collections": {"nc": "non-commercial"},
            "diagnostic_slices": {"talk_diag": {}, "spoken_diag": {}},
            "evaluation_domains": {"forum": {"style_domain": "modern_informal", "collections": ["forum"], "bucket_range": [900, 1000], "max_docs_per_group": 2, "oversample": 1.0,
                "targets": {"test": 1, "cal": 0, "dev_select": 0, "dev_iter": 0},
                "scans": [{"kind": "adapter_metadata_scan", "path": scan, "yield_per_unit": 0.5}]}}});
        let cp = t.path().join("cfg.json");
        std::fs::write(&cp, cfg.to_string()).unwrap();
        let sum = run(&cp).unwrap();
        // Earlier bytes first, then new rows.
        let tr = std::fs::read_to_string(out.join("train").join("manifest.jsonl")).unwrap();
        let lines: Vec<&str> = tr.lines().collect();
        assert!(lines[0].contains("b-train") && lines[1].contains("g-train"));
        assert_eq!(lines.len(), 3, "one new Train row under quota 1 and cap 1");
        let tm = std::fs::read_to_string(out.join("train-mining").join("manifest.jsonl")).unwrap();
        assert_eq!(tm.lines().count(), 3);
        let held = std::fs::read_to_string(out.join("held").join("manifest.jsonl")).unwrap();
        assert!(held.contains("\"n3\"") && !held.contains("\"n4\""));
        assert!(std::fs::read_to_string(out.join("diagnostics/spoken_diag/documents.jsonl")).unwrap().contains("\"n4\""));
        assert!(std::fs::read_to_string(out.join("diagnostics/talk_diag/documents.jsonl")).unwrap().contains("\"n3\""));
        // Reservation: g_r with two units in Test; ineligible and base-family units dropped.
        let test = std::fs::read_to_string(out.join("new-domains-v4/test/families.txt")).unwrap();
        let got: BTreeSet<&str> = test.lines().collect();
        assert_eq!(got, BTreeSet::from(["post:1", "post:2"]));
        assert_eq!(sum["evaluation_universe"]["forum"]["counts"]["metadata_not_eligible"], 1);
        assert_eq!(sum["evaluation_universe"]["forum"]["counts"]["group_key_reserved_in_splits_v3_left_unused"], 1);
        assert_eq!(sum["evaluation_universe"]["forum"]["counts"]["family_with_train_side_or_base_evaluation_text"], 1);
        assert!(out.join("base-v3/SHA256SUMS-defect.json").exists());
        // A Train-side row in a reserved group is refused.
        let bad = t.path().join("pool-bad.jsonl");
        jsonl(&bad, &[row("m1", "forum", "post:11", &g_r)]);
        let mut cfg2 = cfg.clone();
        cfg2["pool"] = json!(bad);
        cfg2["out"] = json!(t.path().join("v4b"));
        std::fs::write(&cp, cfg2.to_string()).unwrap();
        assert!(run(&cp).is_err());
        // An excluded collection in the pool is refused.
        let ex = t.path().join("pool-ex.jsonl");
        jsonl(&ex, &[row("x1", "nc", "nc:1", "w")]);
        cfg2["pool"] = json!(ex);
        cfg2["out"] = json!(t.path().join("v4c"));
        std::fs::write(&cp, cfg2.to_string()).unwrap();
        assert!(run(&cp).unwrap_err().to_string().contains("excluded collection"));
    }
}

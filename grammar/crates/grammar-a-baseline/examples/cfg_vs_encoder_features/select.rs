//! Frozen population rules (PROTOCOL.md sections 4.1-4.5). Reads metadata and
//! exact text from the bound inputs; writes IDs/metadata to `selection/` and
//! {id, text} requests to `inputs/`. Prints aggregate counts only.
use crate::common::*;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::Path,
};

pub const ROLE_SALT: &str = "cfg-vs-encoder-20261031/author-role/v1";
pub const RANK_SALT: &str = "cfg-vs-encoder-20261031/doc-rank/v1";
pub const REFERENCES: usize = 5;
pub const MAX_QUERIES: usize = 3;
pub const CHUNK_DOCS: usize = 100;
const BUNDLE_ROWS: &str =
    "data/grammar-private/a-competitive-20261027/baseline-release/trainer-bundles/18b3682973982a3f-rc/rows.jsonl";
const LOCAL_RC: &str =
    "data/grammar-private/a-competitive-20261027/baseline-release/local-positives/plan-v2-rc";

pub fn role_hash(domain: &str, writer: &str) -> String {
    sha_hex(format!("{ROLE_SALT}\t{domain}\t{writer}").as_bytes())
}

pub fn doc_rank(id: &str) -> String {
    sha_hex(format!("{RANK_SALT}\t{id}").as_bytes())
}

pub fn bucket(h: &str) -> u32 {
    u32::from_str_radix(&h[0..8], 16).expect("hex") % 1000
}

pub fn fold(h: &str) -> u32 {
    u32::from_str_radix(&h[8..16], 16).expect("hex") % 5
}

pub fn role(bucket: u32) -> &'static str {
    if bucket < 500 {
        "train"
    } else if bucket < 990 {
        "eval"
    } else {
        "feasibility_only"
    }
}

#[derive(Clone, Debug)]
pub struct RankedDoc {
    pub id: String,
    pub group: String,
    pub rank: String,
}

impl RankedDoc {
    pub fn new(id: &str, group: &str) -> Self {
        Self {
            id: id.into(),
            group: group.into(),
            rank: doc_rank(id),
        }
    }
}

/// Groups ranked by their smallest document rank; documents within a group by
/// rank (ties by ID, then group name, which hash collisions alone would need).
pub fn ranked_groups(docs: &[RankedDoc]) -> Vec<Vec<&RankedDoc>> {
    let mut by: BTreeMap<&str, Vec<&RankedDoc>> = BTreeMap::new();
    for d in docs {
        by.entry(d.group.as_str()).or_default().push(d);
    }
    let mut groups: Vec<Vec<&RankedDoc>> = by
        .into_values()
        .map(|mut v| {
            v.sort_by(|a, b| (&a.rank, &a.id).cmp(&(&b.rank, &b.id)));
            v
        })
        .collect();
    groups.sort_by(|a, b| (&a[0].rank, &a[0].group).cmp(&(&b[0].rank, &b[0].group)));
    groups
}

/// Round-robin over groups in rank order, lowest rank first within a group.
pub fn round_robin(groups: &[Vec<&RankedDoc>], take: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while out.len() < take {
        let mut any = false;
        for g in groups {
            if let Some(d) = g.get(i) {
                any = true;
                if out.len() < take {
                    out.push(d.id.clone());
                }
            }
        }
        if !any {
            break;
        }
        i += 1;
    }
    out
}

pub struct AuthorSplit {
    pub groups: usize,
    pub queries: Vec<String>,
    pub references: Vec<String>,
}

impl AuthorSplit {
    pub fn eligible(&self) -> bool {
        !self.queries.is_empty() && self.references.len() == REFERENCES
    }
}

/// Section 4.2: q = min(3, G - 1) queries (lowest-rank document of each of the
/// q lowest-rank groups); references round-robin over the remaining groups.
pub fn split_author(docs: &[RankedDoc]) -> AuthorSplit {
    let groups = ranked_groups(docs);
    let q = MAX_QUERIES.min(groups.len().saturating_sub(1));
    AuthorSplit {
        groups: groups.len(),
        queries: groups[..q].iter().map(|g| g[0].id.clone()).collect(),
        references: round_robin(&groups[q..], REFERENCES),
    }
}

/// Section 4.5: profile documents outside the excluded source groups.
pub fn t3_profile(docs: &[RankedDoc], excluded: &BTreeSet<String>) -> Vec<String> {
    let kept: Vec<RankedDoc> = docs
        .iter()
        .filter(|d| !excluded.contains(&d.group))
        .cloned()
        .collect();
    round_robin(&ranked_groups(&kept), REFERENCES)
}

#[derive(Deserialize)]
struct ManRow {
    id: String,
    domain: String,
    #[serde(default)]
    writer_key: Option<String>,
    source_group: String,
    word_count: u64,
    text_sha256: String,
}

#[derive(Deserialize)]
struct DocRow {
    id: String,
    text: String,
    text_sha256: String,
}

struct DocMeta {
    id: Box<str>,
    domain: u32,
    writer: Option<u32>,
    group: u32,
    word_count: u32,
    sha: Box<str>,
    train_split: bool,
}

#[derive(Default)]
struct Interner {
    map: HashMap<Box<str>, u32>,
    names: Vec<Box<str>>,
}

impl Interner {
    fn get_or_insert(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.map.get(s) {
            return i;
        }
        let i = self.names.len() as u32;
        self.names.push(s.into());
        self.map.insert(s.into(), i);
        i
    }
    fn get(&self, s: &str) -> Option<u32> {
        self.map.get(s).copied()
    }
    fn name(&self, i: u32) -> &str {
        &self.names[i as usize]
    }
}

#[derive(Deserialize)]
struct StyleItem {
    id: String,
    origin: String,
    #[serde(default)]
    source: Option<StyleSource>,
}

#[derive(Deserialize)]
struct StyleSource {
    doc_id: String,
    domain: String,
}

#[derive(Deserialize)]
struct BundleRow {
    id: String,
    text_sha256: String,
}

#[derive(Deserialize)]
struct FrontierRec {
    id: String,
    frozen_split: String,
    text: String,
    text_sha256: String,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    ai_attribution_label: Option<String>,
    generation: FrontierGen,
}

#[derive(Deserialize)]
struct FrontierGen {
    kind: String,
    #[serde(default)]
    voice: Option<IdRef>,
}

#[derive(Deserialize)]
struct IdRef {
    #[serde(default)]
    id: Option<String>,
}

#[derive(Deserialize)]
struct LocalRec {
    id: String,
    #[serde(default)]
    frozen_split: Option<String>,
    kind: String,
    text: String,
    text_sha256: String,
    #[serde(default)]
    parent_id: Option<String>,
    generation: LocalGen,
}

#[derive(Deserialize)]
struct LocalGen {
    #[serde(default)]
    generator_key: Option<String>,
    #[serde(default)]
    style: Option<LocalStyle>,
}

#[derive(Deserialize)]
struct LocalStyle {
    #[serde(default)]
    voice_id: Option<String>,
}

struct T3Rec {
    id: String,
    stratum: &'static str,
    kind: String,
    generator: String,
    family: String,
    voice_id: String,
    voice_doc: String,
    target: String,
    voice_group: String,
    parent_id: Option<String>,
    parent_writer: Option<String>,
    text: String,
    text_sha256: String,
    in_bundle_id: bool,
    in_bundle_text: bool,
}

fn sums(path: &Path) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for line in fs::read_to_string(path)?.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let (hash, name) = line.split_once("  ").context("SHA256SUMS line")?;
        out.insert(name.trim_start_matches("./").to_string(), hash.to_string());
    }
    Ok(out)
}

fn whitespace_words(text: &str) -> usize {
    text.split_whitespace().count()
}

pub fn run(frozen_path: &Path, repo: &Path, out: &Path) -> Result<()> {
    let frozen = read_frozen(frozen_path)?;
    create_new_dir(out)?;
    fs::create_dir(out.join("selection"))?;
    fs::create_dir(out.join("inputs"))?;
    fs::create_dir(out.join("inputs/chunks"))?;
    let mut bindings = BTreeMap::<String, String>::new();
    let bind = |b: &mut BTreeMap<String, String>, p: &Path, h: &str| {
        b.insert(
            p.strip_prefix(repo).unwrap_or(p).display().to_string(),
            h.to_string(),
        );
    };

    // Frozen frame (16,205 authors) from the derived inventory.
    let inv = repo.join(frozen_str(&frozen, "/derived_inventories/dir")?);
    let frame_path = inv.join("frame-hashed.tsv");
    let frame_hash = frozen_str(&frozen, "/derived_inventories/frame-hashed.tsv")?;
    require_hash(&frame_path, frame_hash)?;
    bind(&mut bindings, &frame_path, frame_hash);
    struct FrameAuthor {
        domain: String,
        writer: String,
        listed_docs: usize,
        h: String,
    }
    let mut frame = Vec::new();
    for line in fs::read_to_string(&frame_path)?.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        ensure!(f.len() == 5, "frame columns");
        let h = role_hash(f[0], f[1]);
        ensure!(h[..16] == *f[3], "frame hash prefix mismatch");
        ensure!(bucket(&h).to_string() == f[4], "frame bucket mismatch");
        frame.push(FrameAuthor {
            domain: f[0].into(),
            writer: f[1].into(),
            listed_docs: f[2].parse()?,
            h,
        });
    }
    ensure!(
        frame.len() as u64 == frozen["populations"]["frame"]["frame_authors"].as_u64().unwrap_or(0),
        "frame size"
    );
    let frame_keys: HashSet<(String, String)> = frame
        .iter()
        .map(|a| (a.domain.clone(), a.writer.clone()))
        .collect();

    // Manifests (metadata only).
    let splits = repo.join(frozen_str(&frozen, "/inputs/splits_v5/dir")?);
    let mut domains = Interner::default();
    let mut writers = Interner::default();
    let mut groups = Interner::default();
    let mut docs: Vec<DocMeta> = Vec::new();
    let mut doc_index: HashMap<Box<str>, u32> = HashMap::new();
    let mut by_writer: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut frame_mining: HashMap<(String, String), Vec<u32>> = HashMap::new();
    let mut train_split_writers: HashSet<u32> = HashSet::new();
    for (part, key) in [
        ("train", "train_manifest_sha256"),
        ("train-mining", "train_mining_manifest_sha256"),
    ] {
        let path = splits.join(part).join("manifest.jsonl");
        let mut lines = HashedLines::open(&path)?;
        let train_split = part == "train";
        while let Some(line) = lines.next_line()? {
            if line.is_empty() {
                continue;
            }
            let row: ManRow = serde_json::from_slice(line)?;
            ensure!(
                !doc_index.contains_key(row.id.as_str()),
                "duplicate manifest id across Train and Train-mining"
            );
            let idx = docs.len() as u32;
            let writer = row.writer_key.as_deref().map(|w| writers.get_or_insert(w));
            if let Some(w) = writer {
                by_writer.entry(w).or_default().push(idx);
                if train_split {
                    train_split_writers.insert(w);
                } else if let Some(wk) = &row.writer_key {
                    let k = (row.domain.clone(), wk.clone());
                    if frame_keys.contains(&k) {
                        frame_mining.entry(k).or_default().push(idx);
                    }
                }
            }
            doc_index.insert(row.id.as_str().into(), idx);
            docs.push(DocMeta {
                id: row.id.into(),
                domain: domains.get_or_insert(&row.domain),
                writer,
                group: groups.get_or_insert(&row.source_group),
                word_count: row.word_count as u32,
                sha: row.text_sha256.into(),
                train_split,
            });
        }
        let got = lines.finish();
        let expected = frozen_str(&frozen, &format!("/inputs/splits_v5/{key}"))?;
        ensure!(got == expected, "{part} manifest sha256 mismatch");
        bind(&mut bindings, &path, expected);
    }
    let ranked = |idxs: &[u32]| -> Vec<RankedDoc> {
        idxs.iter()
            .map(|&i| {
                let d = &docs[i as usize];
                RankedDoc::new(&d.id, groups.name(d.group))
            })
            .collect()
    };

    // T1/T2 authors: roles, splits, caps.
    let caps = &frozen["populations"]["caps"];
    let mut authors_out = Vec::new();
    let mut eligible_by: BTreeMap<(String, &'static str), Vec<usize>> = BTreeMap::new();
    let mut splits_by_author = Vec::new();
    let mut listed_mismatch = 0usize;
    let mut train_split_violations = 0usize;
    for (ai, a) in frame.iter().enumerate() {
        let idxs = frame_mining
            .get(&(a.domain.clone(), a.writer.clone()))
            .cloned()
            .unwrap_or_default();
        if idxs.len() != a.listed_docs {
            listed_mismatch += 1;
        }
        if writers
            .get(&a.writer)
            .is_some_and(|w| train_split_writers.contains(&w))
        {
            train_split_violations += 1;
        }
        let r = role(bucket(&a.h));
        let split = split_author(&ranked(&idxs));
        if r != "feasibility_only" && split.eligible() {
            eligible_by.entry((a.domain.clone(), r)).or_default().push(ai);
        }
        splits_by_author.push((r, split, idxs.len()));
    }
    let mut selected_authors: Vec<(usize, usize)> = Vec::new(); // (author index, cap rank)
    let mut cap_summary = Vec::new();
    for ((domain, r), list) in &mut eligible_by {
        list.sort_by(|&x, &y| frame[x].h.cmp(&frame[y].h));
        let cap = &caps[*r][domain.as_str()];
        let n = if cap == "all" {
            list.len()
        } else {
            (cap.as_u64().context("cap")? as usize).min(list.len())
        };
        let requested = if cap == "all" { json!("all") } else { cap.clone() };
        cap_summary.push(json!({"domain":domain,"role":r,"eligible":list.len(),"cap":requested,"selected":n,
            "cap_shortfall": cap != "all" && (n as u64) < cap.as_u64().unwrap_or(0)}));
        for (rank, &ai) in list.iter().take(n).enumerate() {
            selected_authors.push((ai, rank));
        }
    }
    let selected_set: HashMap<usize, usize> = selected_authors.iter().copied().collect();
    let mut human_uses: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    let mut human_priority: HashMap<u32, u8> = HashMap::new();
    let mut author_writer = JsonlWriter::create(&out.join("selection/t12-authors.jsonl"))?;
    for (ai, a) in frame.iter().enumerate() {
        let (r, split, n) = &splits_by_author[ai];
        let f = fold(&a.h);
        let sel = selected_set.get(&ai).copied();
        let row = json!({"domain":a.domain,"writer_key":a.writer,"h":a.h,"bucket":bucket(&a.h),"role":r,
            "fold":f,"withheld":*r == "eval" && f == 0,"simulated_withheld":*r == "train" && f == 0,
            "train_mining_documents":n,"groups":split.groups,"eligible":split.eligible() && *r != "feasibility_only",
            "selected":sel.is_some(),"cap_rank":sel,
            "queries":if sel.is_some() {json!(split.queries)} else {json!([])},
            "references":if sel.is_some() {json!(split.references)} else {json!([])}});
        author_writer.write(&row)?;
        if sel.is_some() {
            for (purpose, list) in [("query", &split.queries), ("reference", &split.references)] {
                for id in list {
                    let idx = doc_index[id.as_str()];
                    human_uses
                        .entry(idx)
                        .or_default()
                        .insert(format!("t12:{r}:{purpose}"));
                    human_priority.insert(idx, 0);
                }
            }
        }
    }
    let (authors_rows, authors_sha) = author_writer.finish()?;
    authors_out.push(json!({"file":"selection/t12-authors.jsonl","rows":authors_rows,"sha256":authors_sha}));

    // Style library: human-derived items.
    let style_dir = repo.join(frozen_str(&frozen, "/inputs/style_library/dir")?);
    let mut hd: HashMap<String, (String, String)> = HashMap::new(); // id -> (doc_id, domain)
    for (file, key) in [
        ("voices.accepted.jsonl", "voices_accepted_sha256"),
        ("briefs.accepted.jsonl", "briefs_accepted_sha256"),
    ] {
        let path = style_dir.join(file);
        let mut lines = HashedLines::open(&path)?;
        while let Some(line) = lines.next_line()? {
            if line.is_empty() {
                continue;
            }
            let item: StyleItem = serde_json::from_slice(line)?;
            if item.origin == "human_derived" {
                let s = item.source.context("human-derived source")?;
                hd.insert(item.id, (s.doc_id, s.domain));
            }
        }
        let expected = frozen_str(&frozen, &format!("/inputs/style_library/{key}"))?;
        ensure!(lines.finish() == expected, "{file} sha256 mismatch");
        bind(&mut bindings, &path, expected);
    }

    // R0 bundle IDs and text hashes.
    let bundle_path = repo.join(BUNDLE_ROWS);
    let mut bundle_ids = HashSet::new();
    let mut bundle_texts = HashSet::new();
    {
        let mut lines = HashedLines::open(&bundle_path)?;
        while let Some(line) = lines.next_line()? {
            if line.is_empty() {
                continue;
            }
            let row: BundleRow = serde_json::from_slice(line)?;
            bundle_ids.insert(row.id);
            bundle_texts.insert(row.text_sha256);
        }
        let expected = frozen_str(&frozen, "/inputs/r0_trainer_bundle/rows_sha256")?;
        ensure!(lines.finish() == expected, "R0 bundle rows sha256 mismatch");
        bind(&mut bindings, &bundle_path, expected);
    }

    // T3 candidate records.
    let mut t3_counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut bump = |k: String| *t3_counts.entry(k).or_default() += 1;
    let resolve_voice = |voice: Option<&str>| -> Option<(String, String, String)> {
        let v = voice?;
        let (doc, dom) = hd.get(v)?;
        if dom != "project_gutenberg" {
            return None;
        }
        let &idx = doc_index.get(doc.as_str())?;
        let d = &docs[idx as usize];
        let w = d.writer?;
        Some((doc.clone(), writers.name(w).to_string(), groups.name(d.group).to_string()))
    };
    let writer_of = |id: &str| -> Option<String> {
        let &idx = doc_index.get(id)?;
        docs[idx as usize].writer.map(|w| writers.name(w).to_string())
    };
    let mut records: Vec<T3Rec> = Vec::new();
    let frontier = repo.join(frozen_str(&frozen, "/inputs/frontier_positives/dir")?);
    let top = frontier.join("SHA256SUMS");
    let top_hash = frozen_str(&frozen, "/inputs/frontier_positives/SHA256SUMS_sha256")?;
    require_hash(&top, top_hash)?;
    bind(&mut bindings, &top, top_hash);
    let top_sums = sums(&top)?;
    let mut frontier_files = 0usize;
    for (name, hash) in &top_sums {
        if !name.ends_with("/SHA256SUMS") {
            continue;
        }
        let sub = frontier.join(name);
        require_hash(&sub, hash)?;
        let dir = sub.parent().unwrap();
        let generator_dir = dir.file_name().unwrap().to_string_lossy().to_string();
        let sub_sums = sums(&sub)?;
        let rec_hash = sub_sums.get("records.jsonl").context("records.jsonl in sub sums")?;
        let path = dir.join("records.jsonl");
        let mut lines = HashedLines::open(&path)?;
        while let Some(line) = lines.next_line()? {
            if line.is_empty() {
                continue;
            }
            let r: FrontierRec = serde_json::from_slice(line)?;
            ensure!(sha_hex(r.text.as_bytes()) == r.text_sha256, "frontier text hash");
            if r.frozen_split != "train" {
                bump("frontier/not_train".into());
                continue;
            }
            let in_id = bundle_ids.contains(&r.id);
            let in_text = bundle_texts.contains(&r.text_sha256);
            let stratum = if in_id || in_text {
                "bundled_train"
            } else if matches!(r.generation.kind.as_str(), "full" | "mirror") {
                "primary"
            } else if r.generation.kind == "span" {
                "frontier_span"
            } else {
                bump(format!("frontier/unknown_kind/{}", r.generation.kind));
                continue;
            };
            let voice_id = r.generation.voice.as_ref().and_then(|v| v.id.clone());
            let Some((voice_doc, target, voice_group)) = resolve_voice(voice_id.as_deref()) else {
                bump(format!("{stratum}/no_gutenberg_human_voice"));
                continue;
            };
            let parent_writer = r.parent_id.as_deref().and_then(writer_of);
            if parent_writer.as_deref() == Some(target.as_str()) {
                bump(format!("{stratum}/parent_by_target"));
                continue;
            }
            bump(format!("{stratum}/candidate"));
            records.push(T3Rec {
                id: r.id,
                stratum,
                kind: r.generation.kind,
                generator: r.ai_attribution_label.unwrap_or_else(|| generator_dir.clone()),
                family: generator_dir.split('-').next().unwrap_or("").to_string(),
                voice_id: voice_id.unwrap(),
                voice_doc,
                target,
                voice_group,
                parent_id: r.parent_id,
                parent_writer,
                text: r.text,
                text_sha256: r.text_sha256,
                in_bundle_id: in_id,
                in_bundle_text: in_text,
            });
        }
        ensure!(lines.finish() == *rec_hash, "{generator_dir} records sha256 mismatch");
        frontier_files += 1;
    }
    let local = repo.join(LOCAL_RC);
    let mut dev_texts = HashSet::new();
    for (part, key, split_name) in [
        ("train", "train_records_sha256", "train"),
        ("dev-iter", "dev_iter_records_sha256", "dev_iter"),
    ] {
        let path = local.join(part).join("records.jsonl");
        let mut lines = HashedLines::open(&path)?;
        while let Some(line) = lines.next_line()? {
            if line.is_empty() {
                continue;
            }
            let r: LocalRec = serde_json::from_slice(line)?;
            ensure!(sha_hex(r.text.as_bytes()) == r.text_sha256, "local text hash");
            ensure!(
                r.frozen_split.as_deref().unwrap_or(split_name) == split_name,
                "local record split"
            );
            let in_id = bundle_ids.contains(&r.id);
            let in_text = bundle_texts.contains(&r.text_sha256);
            let stratum = if part == "train" {
                if !(in_id || in_text) {
                    bump("local_train/not_in_bundle".into());
                    continue;
                }
                "bundled_train"
            } else {
                if !dev_texts.insert(r.text_sha256.clone()) {
                    bump("dev_iter_local/duplicate_text".into());
                    continue;
                }
                "dev_iter_local"
            };
            let voice_id = r.generation.style.as_ref().and_then(|s| s.voice_id.clone());
            let Some((voice_doc, target, voice_group)) = resolve_voice(voice_id.as_deref()) else {
                bump(format!("{stratum}/no_gutenberg_human_voice"));
                continue;
            };
            let parent_writer = r.parent_id.as_deref().and_then(writer_of);
            if parent_writer.as_deref() == Some(target.as_str()) {
                bump(format!("{stratum}/parent_by_target"));
                continue;
            }
            bump(format!("{stratum}/candidate"));
            records.push(T3Rec {
                id: r.id,
                stratum,
                kind: r.kind,
                generator: r.generation.generator_key.unwrap_or_default(),
                family: "local".into(),
                voice_id: voice_id.unwrap(),
                voice_doc,
                target,
                voice_group,
                parent_id: r.parent_id,
                parent_writer,
                text: r.text,
                text_sha256: r.text_sha256,
                in_bundle_id: in_id,
                in_bundle_text: in_text,
            });
        }
        let expected = frozen_str(&frozen, &format!("/inputs/local_positives_plan_v2_rc/{key}"))?;
        ensure!(lines.finish() == expected, "local {part} records sha256 mismatch");
        bind(&mut bindings, &path, expected);
    }
    // Deduplicate record IDs within a stratum (a bundled record can only enter once).
    {
        let mut seen = HashSet::new();
        records.retain(|r| seen.insert((r.stratum, r.id.clone())));
    }

    // T3 targets per stratum: profile excludes every voice-source group used
    // for that target in the stratum.
    let mut targets: BTreeMap<(&'static str, String), BTreeSet<String>> = BTreeMap::new();
    for r in &records {
        targets
            .entry((r.stratum, r.target.clone()))
            .or_default()
            .insert(r.voice_group.clone());
    }
    let mut target_rows = JsonlWriter::create(&out.join("selection/t3-targets.jsonl"))?;
    let mut eligible_targets: HashSet<(&'static str, String)> = HashSet::new();
    let mut t3_profile_docs: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    let mut t3_summary: BTreeMap<&'static str, BTreeMap<&'static str, u64>> = BTreeMap::new();
    for ((stratum, target), excluded) in &targets {
        let w = writers.get(target).context("target writer")?;
        let idxs = by_writer.get(&w).cloned().unwrap_or_default();
        let profile = t3_profile(&ranked(&idxs), excluded);
        let eligible = profile.len() == REFERENCES;
        let has_train = idxs.iter().any(|&i| docs[i as usize].train_split);
        let profile_domains: BTreeSet<&str> = profile
            .iter()
            .map(|id| domains.name(docs[doc_index[id.as_str()] as usize].domain))
            .collect();
        let s = t3_summary.entry(stratum).or_default();
        *s.entry("targets_considered").or_default() += 1;
        if excluded.len() > 1 {
            *s.entry("targets_with_multiple_voice_groups").or_default() += 1;
        }
        if eligible {
            *s.entry("targets_eligible").or_default() += 1;
            if has_train {
                *s.entry("eligible_targets_with_train_split_documents").or_default() += 1;
            }
            eligible_targets.insert((*stratum, target.clone()));
            for id in &profile {
                let idx = doc_index[id.as_str()];
                t3_profile_docs
                    .entry(idx)
                    .or_default()
                    .insert(format!("t3:{stratum}:profile"));
                let p = if *stratum == "primary" { 1 } else { 2 };
                let e = human_priority.entry(idx).or_insert(p);
                *e = (*e).min(p);
            }
        }
        target_rows.write(&json!({"stratum":stratum,"target":target,"excluded_voice_groups":excluded,
            "writer_documents":idxs.len(),"has_train_split_documents":has_train,
            "profile":profile,"profile_domains":profile_domains,"eligible":eligible}))?;
    }
    let (target_n, target_sha) = target_rows.finish()?;
    authors_out.push(json!({"file":"selection/t3-targets.jsonl","rows":target_n,"sha256":target_sha}));
    for (idx, uses) in t3_profile_docs {
        human_uses.entry(idx).or_default().extend(uses);
    }

    let mut record_rows = JsonlWriter::create(&out.join("selection/t3-records.jsonl"))?;
    let mut kept_records = Vec::new();
    for r in &records {
        let eligible = eligible_targets.contains(&(r.stratum, r.target.clone()));
        let s = t3_summary.entry(r.stratum).or_default();
        if !eligible {
            *s.entry("records_target_ineligible").or_default() += 1;
            continue;
        }
        *s.entry("records").or_default() += 1;
        *s.entry(match r.kind.as_str() {
            "full" => "records_full",
            "mirror" => "records_mirror",
            "span" => "records_span",
            _ => "records_other_kind",
        })
        .or_default() += 1;
        let mut source_available = false;
        if r.kind == "full" {
            if let Some(&idx) = r.parent_id.as_deref().and_then(|p| doc_index.get(p)) {
                source_available = true;
                human_uses
                    .entry(idx)
                    .or_default()
                    .insert(format!("t3:{}:source", r.stratum));
                let p = if r.stratum == "primary" { 1 } else { 2 };
                let e = human_priority.entry(idx).or_insert(p);
                *e = (*e).min(p);
            } else {
                *s.entry("full_records_source_unavailable").or_default() += 1;
            }
        }
        record_rows.write(&json!({"id":r.id,"stratum":r.stratum,"kind":r.kind,"generator":r.generator,
            "family":r.family,"voice_id":r.voice_id,"voice_source_doc":r.voice_doc,"voice_source_group":r.voice_group,
            "target":r.target,"parent_id":r.parent_id,"parent_writer":r.parent_writer,
            "source_document_available":source_available,"text_sha256":r.text_sha256,
            "whitespace_words":whitespace_words(&r.text),
            "in_r0_bundle_by_id":r.in_bundle_id,"in_r0_bundle_by_text":r.in_bundle_text}))?;
        kept_records.push(r);
    }
    let (rec_n, rec_sha) = record_rows.finish()?;
    authors_out.push(json!({"file":"selection/t3-records.jsonl","rows":rec_n,"sha256":rec_sha}));

    // Exact texts for every selected human document.
    let needed: HashSet<&str> = human_uses.keys().map(|&i| &*docs[i as usize].id).collect();
    let mut texts: HashMap<String, String> = HashMap::new();
    for (part, key) in [
        ("train", "train_documents_sha256"),
        ("train-mining", "train_mining_documents_sha256"),
    ] {
        let path = splits.join(part).join("documents.jsonl");
        let mut lines = HashedLines::open(&path)?;
        while let Some(line) = lines.next_line()? {
            if line.is_empty() {
                continue;
            }
            let row: DocRow = serde_json::from_slice(line)?;
            if needed.contains(row.id.as_str()) {
                ensure!(sha_hex(row.text.as_bytes()) == row.text_sha256, "document text hash");
                let meta = &docs[doc_index[row.id.as_str()] as usize];
                ensure!(*meta.sha == row.text_sha256, "manifest/document text hash");
                texts.insert(row.id, row.text);
            }
        }
        let expected = frozen_str(&frozen, &format!("/inputs/splits_v5/{key}"))?;
        ensure!(lines.finish() == expected, "{part} documents sha256 mismatch");
        bind(&mut bindings, &path, expected);
    }
    ensure!(texts.len() == needed.len(), "missing selected document text");

    // Ordered unique documents: T1/T2 first, then T3 primary, then secondary.
    struct Out<'a> {
        id: &'a str,
        priority: u8,
        text: &'a str,
        meta: Value,
    }
    let mut ordered: Vec<Out> = Vec::new();
    for (&idx, uses) in &human_uses {
        let d = &docs[idx as usize];
        let text = texts[&*d.id].as_str();
        let writer = d.writer.map(|w| writers.name(w));
        ordered.push(Out {
            id: &d.id,
            priority: human_priority[&idx],
            text,
            meta: json!({"id":&*d.id,"kind":"human_document","domain":domains.name(d.domain),
                "writer_key":writer,"source_group":groups.name(d.group),"split":if d.train_split {"train"} else {"train_mining"},
                "word_count":d.word_count,"whitespace_words":whitespace_words(text),"text_sha256":&*d.sha,
                "bytes":text.len(),"uses":uses}),
        });
    }
    let mut record_uses: BTreeMap<&str, (u8, BTreeSet<String>, &T3Rec)> = BTreeMap::new();
    for r in &kept_records {
        let p = if r.stratum == "primary" { 1 } else { 2 };
        let e = record_uses
            .entry(r.id.as_str())
            .or_insert((p, BTreeSet::new(), r));
        e.0 = e.0.min(p);
        e.1.insert(format!("t3:{}:record", r.stratum));
    }
    for (id, (p, uses, r)) in &record_uses {
        ordered.push(Out {
            id,
            priority: *p,
            text: &r.text,
            meta: json!({"id":id,"kind":"t3_record","domain":"t3_record","writer_key":null,
                "source_group":null,"split":"train_side_generated","word_count":null,
                "whitespace_words":whitespace_words(&r.text),"text_sha256":r.text_sha256,
                "bytes":r.text.len(),"uses":uses}),
        });
    }
    ordered.sort_by(|a, b| (a.priority, a.id).cmp(&(b.priority, b.id)));
    {
        let mut seen = HashSet::new();
        for o in &ordered {
            ensure!(seen.insert(o.id), "duplicate document id across populations");
        }
    }
    let mut doc_rows = JsonlWriter::create(&out.join("selection/documents.jsonl"))?;
    let mut all_inputs = JsonlWriter::create(&out.join("inputs/documents.jsonl"))?;
    let mut chunks = Vec::new();
    let mut chunk: Option<JsonlWriter> = None;
    let mut chunk_meta: Vec<Value> = Vec::new();
    let mut by_priority: BTreeMap<u8, (u64, u64)> = BTreeMap::new();
    for (i, o) in ordered.iter().enumerate() {
        let c = i / CHUNK_DOCS;
        if i % CHUNK_DOCS == 0 {
            if let Some(w) = chunk.take() {
                let (n, sha) = w.finish()?;
                chunks.push(json!({"chunk":format!("c{:04}", c - 1),"documents":n,"sha256":sha}));
            }
            chunk = Some(JsonlWriter::create(
                &out.join(format!("inputs/chunks/c{c:04}.jsonl")),
            )?);
        }
        let req = Request {
            id: o.id.into(),
            text: o.text.into(),
        };
        chunk.as_mut().unwrap().write(&req)?;
        all_inputs.write(&req)?;
        let mut meta = o.meta.clone();
        meta["priority"] = json!(o.priority);
        meta["chunk"] = json!(format!("c{c:04}"));
        doc_rows.write(&meta)?;
        let e = by_priority.entry(o.priority).or_default();
        e.0 += 1;
        e.1 += whitespace_words(o.text) as u64;
        chunk_meta.push(json!(o.priority));
    }
    if let Some(w) = chunk.take() {
        let (n, sha) = w.finish()?;
        chunks.push(json!({"chunk":format!("c{:04}", (ordered.len() - 1) / CHUNK_DOCS),"documents":n,"sha256":sha}));
    }
    let (doc_n, doc_sha) = doc_rows.finish()?;
    let (inp_n, inp_sha) = all_inputs.finish()?;

    let mut t12_counts: BTreeMap<String, BTreeMap<&str, u64>> = BTreeMap::new();
    for &(ai, _) in &selected_authors {
        let a = &frame[ai];
        let (r, split, _) = &splits_by_author[ai];
        let e = t12_counts.entry(format!("{}/{}", a.domain, r)).or_default();
        *e.entry("authors").or_default() += 1;
        *e.entry("queries").or_default() += split.queries.len() as u64;
        *e.entry("references").or_default() += split.references.len() as u64;
        if fold(&a.h) == 0 {
            *e.entry(if *r == "eval" { "withheld" } else { "simulated_withheld" }).or_default() += 1;
        }
    }
    let summary = json!({
        "schema":"slop_ninja_cfg_vs_encoder_selection_v1","protocol_id":PROTOCOL_ID,"frozen_sha256":FROZEN_SHA256,
        "created":now_utc(),"input_bindings":bindings,"frontier_record_files":frontier_files,
        "frame":{"authors":frame.len(),"listed_document_count_mismatches":listed_mismatch,
            "authors_with_train_split_documents":train_split_violations},
        "t12":{"caps":cap_summary,"selected":t12_counts},
        "t3":{"filter_counts":t3_counts,"strata":t3_summary,
            "profile_rule":"round-robin over the target's Train and Train-mining documents outside every voice-source group used for that target in the stratum; first 5"},
        "documents":{"rows":doc_n,"sha256":doc_sha,"by_priority":by_priority.iter().map(|(p,(n,w))| json!({"priority":p,"documents":n,"whitespace_words":w})).collect::<Vec<_>>()},
        "inputs":{"file":"inputs/documents.jsonl","rows":inp_n,"sha256":inp_sha,"chunk_documents":CHUNK_DOCS,"chunks":chunks},
        "outputs":authors_out
    });
    write_new_json(&out.join("selection/summary.json"), &summary)?;
    println!(
        "{}",
        serde_json::to_string(&json!({"t12":summary["t12"],"t3":summary["t3"]["strata"],"documents":summary["documents"]}))?
    );
    Ok(())
}

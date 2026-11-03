//! Rights snapshot (`rights-jitems-v1.json`, later `rights-jitems-v2.json`),
//! the row filter `jitem_filter_v1`, the P0 row allow-list and the
//! `DATA_RIGHTS.json` generator (protocol A2: A7, S0-4, D2 and item 1.7).
//!
//! The code makes no rights judgement. Every item and every tag comes from a
//! private rules file (`RULES_SCHEMA`) written from the recorded rights
//! review. This module applies it and refuses whatever the rules do not name.
//!
//! - **Rules.** An item catalogue (ids such as `J1`, each with a title and a
//!   recorded answer or null); the items on every row; the items of each
//!   source collection (with optional extra items per domain inside a
//!   collection), generator label, source kind, style-library family (as
//!   inventor or reviewer), population role, derived label and model
//!   initialisation. Each basis document, each style-library file, each
//!   derived-label file and the reproduction source are bound by SHA-256 and
//!   re-hashed when read. The rules also bind the Stage-0 finding records
//!   they were checked against (`findings_checked`), the populations a run
//!   must supply (`populations`, with their splits and optionally their
//!   digests; required for version 2) and the row schemas version 2 reads.
//! - **Coverage.** Every collection, generator label, source kind, voice kind
//!   and style family that occurs in the inputs needs a rule (an empty item
//!   list is a rule). A voice or brief id that no style library lists is
//!   accepted only when its row's `voice_origin` is listed in
//!   `unlisted_style_origins`. The source of a human-derived style item
//!   resolves to the one human collection whose rows carry its domain, or to
//!   `style_source_domains`. The Enron collection must carry `ENRON_ITEM`
//!   (protocol A2, r2 F8).
//! - **Style lineage.** `voice_kinds` states, for each `voice_kind` (`null`
//!   when absent), whether its rows record their voice or brief id
//!   (`recorded`) or carry no style item (`none`). A row that breaks its rule
//!   is refused, so a style-conditioned row without an id is never read as
//!   style-free. Cohorts whose style ids were never recorded are listed in
//!   `unrecorded_style_cohorts` with the items their rows carry (route
//!   `style_unrecorded`); DATA_RIGHTS marks such rows as open.
//! - **Populations.** Rows come from bundle files. No role accepts a sealed
//!   Test split (`SEALED_SPLIT_PREFIX`); a Train population holds split
//!   `train` only. Version 1 reads `V1_ROW_SCHEMA` rows, pinned here.
//! - **Row tags.** A row's items are the union of the every-row items and the
//!   items of its collection (and domain), generator, source kind, population
//!   role, voice item and brief item (or its unrecorded-cohort items). A style
//!   item's items are those of its inventor family, its reviewer family and,
//!   for a human-derived item, its source collection. Each item records the
//!   routes by which it reached the row (`via`). Derived labels are
//!   recipe-level: they are listed, not put on rows.
//! - **`jitem_filter_v1`** keeps a row that carries none of J1, J2 and J4 by
//!   any route (EVIDENCE §3.2, "strict"). A dropped row's first reason is
//!   taken in the order of `DROP_REASONS`: J1; J2 by any route other than the
//!   style library; J4; J2 through the style library only.
//! - **Snapshot.** Version 1 holds Train populations only and must reproduce
//!   every count the rules record (`reproduce`: kept rows per origin, their
//!   total and dropped positives per first reason), or nothing is written.
//!   Version 2 (item 1.7) holds exactly the rules' populations, at least one
//!   of every role. A snapshot is `final` only when `STAGE0.json` records A3
//!   as done and the rules bind that A3 record; otherwise it is a draft. Pool
//!   files (rows that are not bundle rows yet, such as the Enron pool) are
//!   tagged by collection and bound by digest. Outputs are text-free and
//!   written once, mode 600, into a directory that appears only complete.
//! - **Trainer check.** The allow-list is checked against the Train block of
//!   a trainer bundle only: its MANIFEST, then exactly its first
//!   `rows_by_split.train` lines. No later line is parsed or hashed (old
//!   Dev-select and old CAL, protocol A2 §2.4).
//! - **DATA_RIGHTS.** Generated only from a final version-2 snapshot (item
//!   1.7). It carries counts, digests, licences and published notes only: no
//!   ids, text, paths, private notes or free-text findings. It lists as open
//!   every item in use that has no recorded answer, and any unrecorded style
//!   lineage.
use crate::bundle::file_sha;
use crate::consolidate::for_lines;
use crate::{ORIGINS, sha_hex};
use anyhow::{Context, Result, bail, ensure};
use grammar_a_pools::pile::ENRON_COLLECTION;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

pub const RULES_SCHEMA: &str = "slop_ninja_a_rights_rules_v2";
pub const SNAPSHOT_V1_SCHEMA: &str = "slop_ninja_a_rights_jitems_v1";
pub const SNAPSHOT_V2_SCHEMA: &str = "slop_ninja_a_rights_jitems_v2";
pub const ROW_TAGS_SCHEMA: &str = "slop_ninja_a_rights_row_tags_v1";
pub const ALLOWLIST_SCHEMA: &str = "slop_ninja_a_row_allowlist_v1";
pub const DATA_RIGHTS_SCHEMA: &str = "slop_ninja_a_data_rights_v1";
pub const POOL_TAGS_SCHEMA: &str = "slop_ninja_a_rights_pool_tags_v1";
pub const ROW_TAGS_FILE: &str = "row-tags.jsonl";
/// The bundle row schema a version-1 snapshot reads (R0's frozen rows). Pinned here, not
/// taken from the crate's current row schema, so a later bundle version cannot change v1.
pub const V1_ROW_SCHEMA: &str = "slop_ninja_a_bundle_row_v1";
/// The trainer-bundle manifest the trainer check reads.
pub const TRAINER_MANIFEST_SCHEMA: &str = "slop_ninja_a_trainer_bundle_manifest_v1";
/// Splits with this prefix are sealed Test splits (the consumed Test, Test-2): no role reads them.
pub const SEALED_SPLIT_PREFIX: &str = "test";
/// DATA_RIGHTS marker: some rows' style lineage was not recorded.
pub const MARKER_STYLE_LINEAGE_UNRECORDED: &str = "style_lineage_unrecorded";
/// Stage-0 findings a snapshot carries; the rules must bind each before a snapshot is final.
pub const FINDINGS: [&str; 1] = ["A3"];

pub const JITEM_FILTER_V1: &str = "jitem_filter_v1";
/// Items `jitem_filter_v1` excludes, by any route.
pub const JITEM_FILTER_V1_EXCLUDES: [&str; 3] = ["J1", "J2", "J4"];
/// First-reason order of a `jitem_filter_v1` drop.
pub const DROP_REASONS: [&str; 4] = ["j1", "j2_generator", "j4", "j2_style_library_only"];
/// The item every row of the Enron collection carries (protocol A2, r2 F8).
pub const ENRON_ITEM: &str = "J8";
pub const ROLE_TRAIN: &str = "train";
/// Population roles. Version 2 must hold at least one population of each.
pub const ROLES: [&str; 5] = [ROLE_TRAIN, "checkpoint_metric", "selection", "calibration", "mock"];
/// The `voice_kinds` key of a row without a `voice_kind`.
pub const NULL_KEY: &str = "null";

/// Routes by which an item reaches a row or a style item.
const VIA_EVERY_ROW: &str = "every_row";
const VIA_COLLECTION: &str = "collection";
const VIA_GENERATOR: &str = "generator";
const VIA_SOURCE_KIND: &str = "source_kind";
const VIA_ROLE: &str = "population_role";
const VIA_STYLE: &str = "style";
const VIA_STYLE_UNRECORDED: &str = "style_unrecorded";
const VIA_INVENTOR: &str = "inventor";
const VIA_REVIEWER: &str = "reviewer";
const VIA_SOURCE: &str = "source";
const HUMAN_DERIVED: &str = "human_derived";
const HUMAN_KIND: &str = "human";
/// Read buffer of the trainer check: the most bytes past the Train block that one read loads (never parsed).
const TRAINER_READ_BUFFER: usize = 1 << 16;

/// Items with the routes by which they arrived.
pub type Tags = BTreeMap<String, BTreeSet<&'static str>>;

fn add(t: &mut Tags, items: &[String], via: &'static str) {
    for i in items {
        t.entry(i.clone()).or_default().insert(via);
    }
}

fn merge(t: &mut Tags, from: &Tags, via: &'static str) {
    for i in from.keys() {
        t.entry(i.clone()).or_default().insert(via);
    }
}

fn origin_index(o: &str) -> Result<usize> {
    ORIGINS.iter().position(|x| *x == o).with_context(|| format!("unknown origin {o}"))
}

fn by_origin<T: Copy + Into<Value>>(a: &[T; 3]) -> Value {
    let mut m = Map::new();
    for (o, v) in ORIGINS.iter().zip(a) {
        m.insert((*o).into(), (*v).into());
    }
    Value::Object(m)
}

/// A name that is safe as a file-name component.
fn safe_name(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('.') && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

/// A sealed Test split, refused for every role.
pub fn sealed_split(s: &str) -> bool {
    s.starts_with(SEALED_SPLIT_PREFIX)
}

fn canonical(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            Value::Object(keys.into_iter().map(|k| (k.clone(), canonical(&m[k]))).collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(canonical).collect()),
        x => x.clone(),
    }
}

/// SHA-256 of a record's compact JSON with object keys sorted (the digest `findings_checked` binds).
pub fn record_sha256(v: &Value) -> String {
    sha_hex(&serde_json::to_vec(&canonical(v)).expect("JSON values serialise"))
}

/// `items.<item>` of a `STAGE0.json`: the file's SHA-256, the record and the record's digest.
pub fn stage0_record(stage0: &Path, item: &str) -> Result<(String, Value, String)> {
    let raw = fs::read(stage0).with_context(|| format!("read {}", stage0.display()))?;
    let v: Value = serde_json::from_slice(&raw).with_context(|| format!("parse {}", stage0.display()))?;
    let rec = v["items"][item].clone();
    ensure!(rec.is_object(), "{} has no items.{item}", stage0.display());
    let d = record_sha256(&rec);
    Ok((sha_hex(&raw), rec, d))
}

/// Snapshot version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    V1,
    V2,
}

impl Version {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "1" | "v1" => Ok(Self::V1),
            "2" | "v2" => Ok(Self::V2),
            _ => bail!("unknown snapshot version {s} (1 or 2)"),
        }
    }
    pub fn schema(self) -> &'static str {
        match self {
            Self::V1 => SNAPSHOT_V1_SCHEMA,
            Self::V2 => SNAPSHOT_V2_SCHEMA,
        }
    }
    pub fn file_name(self) -> &'static str {
        match self {
            Self::V1 => "rights-jitems-v1.json",
            Self::V2 => "rights-jitems-v2.json",
        }
    }
}

/// A file bound by its SHA-256.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bound {
    pub label: String,
    pub path: PathBuf,
    pub sha256: String,
    #[serde(default)]
    pub section: Option<String>,
}

impl Bound {
    fn verify(&self) -> Result<()> {
        let got = file_sha(&self.path).with_context(|| format!("{} ({})", self.label, self.path.display()))?;
        ensure!(got == self.sha256, "{} ({}) hashes to {got}, not the recorded {}", self.label, self.path.display(), self.sha256);
        Ok(())
    }
    fn record(&self) -> Value {
        json!({"label": self.label, "path": self.path, "sha256": self.sha256, "section": self.section})
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemDef {
    pub title: String,
    /// The recorded answer; null while the item is open.
    #[serde(default)]
    pub answer: Option<Value>,
}

/// The items of one collection, generator or model initialisation.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entity {
    /// Published in DATA_RIGHTS as written.
    #[serde(default)]
    pub licence: Option<String>,
    pub items: Vec<String>,
    /// Extra items for rows of these domains (collections only).
    #[serde(default)]
    pub by_domain: BTreeMap<String, Vec<String>>,
    /// Private: kept in the snapshot, never in DATA_RIGHTS.
    #[serde(default)]
    pub note: Option<String>,
    /// Published in DATA_RIGHTS.
    #[serde(default)]
    pub public_note: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedLabel {
    pub path: PathBuf,
    pub sha256: String,
    pub items: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub public_note: Option<String>,
}

/// Style lineage of the rows of one voice kind.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Lineage {
    /// The row names its voice or brief id.
    Recorded,
    /// The row carries no style item.
    None,
}

/// A population a run must supply.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PopulationRule {
    pub name: String,
    pub role: String,
    /// The splits its rows may be in.
    pub splits: Vec<String>,
    /// The rows file's SHA-256, when bound.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Counts a version-1 snapshot must reproduce, all of them.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reproduce {
    pub filter: String,
    pub population: String,
    pub basis: Bound,
    /// Kept rows per origin (every origin).
    pub kept: BTreeMap<String, u64>,
    /// The sum of `kept`.
    pub kept_total: u64,
    /// Dropped mixed and model-only rows per first reason (every reason).
    pub dropped_positives_by_first_reason: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    pub schema: String,
    pub basis: Vec<Bound>,
    pub items: BTreeMap<String, ItemDef>,
    pub every_row: Vec<String>,
    pub collections: BTreeMap<String, Entity>,
    pub generators: BTreeMap<String, Entity>,
    pub source_kinds: BTreeMap<String, Vec<String>>,
    /// The accepted-items files of the style libraries (`voices.accepted.jsonl`, `briefs.accepted.jsonl`).
    pub style_libraries: Vec<Bound>,
    pub style_families: BTreeMap<String, Vec<String>>,
    /// Style lineage by the row's `voice_kind` (`NULL_KEY` when absent).
    pub voice_kinds: BTreeMap<String, Lineage>,
    /// Cohorts whose style ids were never recorded -> the items each of their rows carries.
    #[serde(default)]
    pub unrecorded_style_cohorts: BTreeMap<String, Vec<String>>,
    /// Items for voice or brief ids that no library lists, by the row's `voice_origin`.
    #[serde(default)]
    pub unlisted_style_origins: BTreeMap<String, Vec<String>>,
    /// Source domain of a human-derived style item -> collection (overrides the human-row map).
    #[serde(default)]
    pub style_source_domains: BTreeMap<String, String>,
    pub population_roles: BTreeMap<String, Vec<String>>,
    /// The populations a run must supply, exactly (required for version 2).
    #[serde(default)]
    pub populations: Vec<PopulationRule>,
    /// Bundle row schemas a version-2 snapshot reads (version 1 reads `V1_ROW_SCHEMA` only).
    #[serde(default)]
    pub row_schemas: Vec<String>,
    /// Stage-0 finding -> the `record_sha256` of the record these rules were checked against.
    #[serde(default)]
    pub findings_checked: BTreeMap<String, String>,
    #[serde(default)]
    pub derived_labels: BTreeMap<String, DerivedLabel>,
    #[serde(default)]
    pub model_inits: BTreeMap<String, Entity>,
    #[serde(default)]
    pub reproduce: Option<Reproduce>,
}

impl Rules {
    /// Parse, validate and re-hash every bound file except the style libraries (re-hashed as they
    /// are read). Returns the rules and the file's SHA-256.
    pub fn load(p: &Path) -> Result<(Self, String)> {
        let raw = fs::read(p).with_context(|| format!("read {}", p.display()))?;
        let r: Self = serde_json::from_slice(&raw).with_context(|| format!("parse {}", p.display()))?;
        r.validate()?;
        Ok((r, sha_hex(&raw)))
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == RULES_SCHEMA, "rules schema {} (want {RULES_SCHEMA})", self.schema);
        let known = |ctx: &str, items: &[String]| -> Result<()> {
            for i in items {
                ensure!(self.items.contains_key(i), "{ctx}: item {i} is not in the catalogue");
            }
            Ok(())
        };
        known("every_row", &self.every_row)?;
        for (kind, m) in [("collection", &self.collections), ("generator", &self.generators), ("model_init", &self.model_inits)] {
            for (k, e) in m {
                known(&format!("{kind} {k}"), &e.items)?;
                for (d, l) in &e.by_domain {
                    known(&format!("{kind} {k} domain {d}"), l)?;
                }
            }
        }
        for (kind, m) in [
            ("source_kind", &self.source_kinds),
            ("style_family", &self.style_families),
            ("unlisted_style_origin", &self.unlisted_style_origins),
            ("unrecorded_style_cohort", &self.unrecorded_style_cohorts),
            ("population_role", &self.population_roles),
        ] {
            for (k, l) in m {
                known(&format!("{kind} {k}"), l)?;
            }
        }
        for (c, l) in &self.unrecorded_style_cohorts {
            ensure!(!l.is_empty(), "unrecorded style cohort {c} needs the items its rows carry");
        }
        for (k, d) in &self.derived_labels {
            known(&format!("derived_label {k}"), &d.items)?;
        }
        for r in self.population_roles.keys() {
            ensure!(ROLES.contains(&r.as_str()), "unknown population role {r}");
        }
        for (d, c) in &self.style_source_domains {
            ensure!(self.collections.contains_key(c), "style source domain {d} names collection {c}, which has no rule");
        }
        for i in JITEM_FILTER_V1_EXCLUDES {
            ensure!(self.items.contains_key(i), "the catalogue lacks {i}, which {JITEM_FILTER_V1} excludes");
        }
        let enron = self
            .collections
            .get(ENRON_COLLECTION)
            .with_context(|| format!("the rules give no items for {ENRON_COLLECTION} (the protocol requires {ENRON_ITEM})"))?;
        ensure!(enron.items.iter().any(|i| i == ENRON_ITEM), "{ENRON_COLLECTION} must carry {ENRON_ITEM} on every row");
        ensure!(!self.style_libraries.is_empty(), "the rules bind no style library");
        let mut paths = BTreeSet::new();
        for b in &self.style_libraries {
            ensure!(paths.insert(&b.path), "style library {} is bound twice", b.path.display());
        }
        let mut names = BTreeSet::new();
        for p in &self.populations {
            ensure!(safe_name(&p.name), "population name {:?} is not a safe file-name component", p.name);
            ensure!(names.insert(p.name.as_str()), "the rules list population {} twice", p.name);
            ensure!(ROLES.contains(&p.role.as_str()), "population {}: unknown role {}", p.name, p.role);
            ensure!(!p.splits.is_empty(), "population {} lists no split", p.name);
            for s in &p.splits {
                ensure!(!sealed_split(s), "population {} lists sealed split {s}", p.name);
            }
            ensure!(p.role != ROLE_TRAIN || p.splits == [ROLE_TRAIN], "Train population {} may list split train only", p.name);
        }
        ensure!(self.row_schemas.iter().all(|s| !s.is_empty()), "an empty row schema");
        for (f, d) in &self.findings_checked {
            ensure!(FINDINGS.contains(&f.as_str()), "findings_checked names {f} (known: {FINDINGS:?})");
            ensure!(d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()), "findings_checked.{f} is not a SHA-256");
        }
        ensure!(!self.basis.is_empty(), "the rules name no basis document");
        for b in &self.basis {
            b.verify()?;
        }
        for (k, d) in &self.derived_labels {
            let got = file_sha(&d.path).with_context(|| format!("derived label {k}"))?;
            ensure!(got == d.sha256, "derived label {k} ({}) hashes to {got}, not the recorded {}", d.path.display(), d.sha256);
        }
        if let Some(rp) = &self.reproduce {
            ensure!(rp.filter == JITEM_FILTER_V1, "reproduce names filter {} (only {JITEM_FILTER_V1})", rp.filter);
            rp.basis.verify()?;
            let kept: BTreeSet<&str> = rp.kept.keys().map(String::as_str).collect();
            ensure!(kept == BTreeSet::from(ORIGINS), "reproduce.kept must give exactly the origins {ORIGINS:?} (it gives {kept:?})");
            ensure!(rp.kept_total == rp.kept.values().sum::<u64>(), "reproduce.kept_total {} is not the sum of reproduce.kept", rp.kept_total);
            let reasons: BTreeSet<&str> = rp.dropped_positives_by_first_reason.keys().map(String::as_str).collect();
            ensure!(
                reasons == BTreeSet::from(DROP_REASONS),
                "reproduce.dropped_positives_by_first_reason must give exactly the reasons {DROP_REASONS:?} (it gives {reasons:?})"
            );
        }
        Ok(())
    }

    fn collection(&self, c: &str) -> Result<&Entity> {
        self.collections.get(c).with_context(|| format!("no rule for collection {c}"))
    }

    fn family(&self, f: &str) -> Result<&[String]> {
        self.style_families.get(f).map(Vec::as_slice).with_context(|| format!("no rule for style family {f}"))
    }

    /// Items of one row of `collection` in `domain`.
    fn collection_tags(&self, t: &mut Tags, collection: &str, domain: Option<&str>, via: &'static str) -> Result<()> {
        let e = self.collection(collection)?;
        add(t, &e.items, via);
        if let Some(x) = domain.and_then(|d| e.by_domain.get(d)) {
            add(t, x, via);
        }
        Ok(())
    }
}

/// One accepted voice prompt or brief (attributes only; the text is not read into memory).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleItem {
    pub kind: String,
    pub family: String,
    pub reviewer_family: Option<String>,
    pub origin: String,
    pub source_domain: Option<String>,
    pub source_split: Option<String>,
}

#[derive(Deserialize)]
struct StyleLine {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    family: String,
    #[serde(default)]
    reviewer_family: Option<String>,
    origin: String,
    #[serde(default)]
    source: Option<StyleSource>,
}

#[derive(Deserialize)]
struct StyleSource {
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    split: Option<String>,
}

/// Accepted voices and briefs of the rules' style libraries.
#[derive(Debug, Default)]
pub struct StyleIndex {
    pub items: HashMap<String, StyleItem>,
    pub files: Vec<Value>,
    /// Ids listed more than once with identical attributes.
    pub duplicates: usize,
}

impl StyleIndex {
    /// Read each bound file; its digest, taken as it is read, must be the bound one.
    pub fn load(files: &[Bound]) -> Result<Self> {
        let mut s = Self::default();
        for b in files {
            let (sha, n) = for_lines(&b.path, |l| {
                let x: StyleLine = serde_json::from_slice(l)?;
                let src = x.source.unwrap_or(StyleSource { domain: None, split: None });
                ensure!(x.origin != HUMAN_DERIVED || src.domain.is_some(), "human-derived style item {} has no source domain", x.id);
                let it = StyleItem {
                    kind: x.kind,
                    family: x.family,
                    reviewer_family: x.reviewer_family,
                    origin: x.origin,
                    source_domain: src.domain,
                    source_split: src.split,
                };
                match s.items.get(&x.id) {
                    Some(old) => {
                        ensure!(*old == it, "style item {} is listed twice with different attributes", x.id);
                        s.duplicates += 1;
                    }
                    None => {
                        s.items.insert(x.id, it);
                    }
                }
                Ok(())
            })
            .with_context(|| format!("style library {} ({})", b.label, b.path.display()))?;
            ensure!(sha == b.sha256, "style library {} ({}) hashes to {sha}, not the recorded {}", b.label, b.path.display(), b.sha256);
            s.files.push(json!({"label": b.label, "path": b.path, "sha256": sha, "rows": n}));
        }
        Ok(s)
    }
}

/// The metadata of one bundle row; the text is skipped.
#[derive(Clone, Debug, Deserialize)]
pub struct MetaRow {
    pub schema: String,
    pub id: String,
    pub split: String,
    pub origin: String,
    pub text_sha256: String,
    #[serde(default)]
    pub generator_label: Option<String>,
    #[serde(default)]
    pub generator_key: Option<String>,
    pub cohort_id: String,
    #[serde(default)]
    pub collection: Option<String>,
    pub domain: String,
    pub source_kind: String,
    #[serde(default)]
    pub voice_id: Option<String>,
    #[serde(default)]
    pub brief_id: Option<String>,
    #[serde(default)]
    pub voice_kind: Option<String>,
    #[serde(default)]
    pub voice_origin: Option<String>,
    pub weight: f64,
}

impl MetaRow {
    fn collection(&self) -> Result<&str> {
        self.collection.as_deref().with_context(|| format!("row {} has no collection", self.id))
    }
}

/// `NAME:ROLE=ROWS`.
#[derive(Clone, Debug)]
pub struct PopulationSpec {
    pub name: String,
    pub role: String,
    pub rows: PathBuf,
}

impl PopulationSpec {
    pub fn parse(s: &str) -> Result<Self> {
        let (k, p) = s.split_once('=').context("population is NAME:ROLE=ROWS")?;
        let (name, role) = k.split_once(':').context("population is NAME:ROLE=ROWS")?;
        ensure!(!p.is_empty(), "population is NAME:ROLE=ROWS");
        ensure!(safe_name(name), "population name {name:?} is not a safe file-name component");
        ensure!(ROLES.contains(&role), "unknown population role {role}");
        Ok(Self { name: name.into(), role: role.into(), rows: p.into() })
    }
}

/// `NAME=ROWS`, or `NAME:ITEM=ROWS` when every row must carry ITEM.
#[derive(Clone, Debug)]
pub struct PoolSpec {
    pub name: String,
    pub expect_item: Option<String>,
    pub rows: PathBuf,
}

impl PoolSpec {
    pub fn parse(s: &str) -> Result<Self> {
        let (k, p) = s.split_once('=').context("pool is NAME[:ITEM]=ROWS")?;
        let (name, item) = match k.split_once(':') {
            Some((n, i)) => (n, Some(i.to_string())),
            None => (k, None),
        };
        ensure!(!p.is_empty() && safe_name(name) && item.as_deref() != Some(""), "pool is NAME[:ITEM]=ROWS");
        Ok(Self { name: name.into(), expect_item: item, rows: p.into() })
    }
}

struct Population {
    spec: PopulationSpec,
    sha256: String,
    rows: Vec<MetaRow>,
}

fn load_population(spec: &PopulationSpec, schemas: &[&str], rule: Option<&PopulationRule>) -> Result<Population> {
    let mut rows = Vec::new();
    let (sha, _) = for_lines(&spec.rows, |l| {
        let r: MetaRow = serde_json::from_slice(l)?;
        ensure!(schemas.contains(&r.schema.as_str()), "row {}: schema {} (accepted: {schemas:?})", r.id, r.schema);
        origin_index(&r.origin)?;
        ensure!(r.weight.is_finite(), "row {}: weight is not finite", r.id);
        ensure!(!sealed_split(&r.split), "row {} of population {} is in sealed split {}", r.id, spec.name, r.split);
        ensure!(spec.role != ROLE_TRAIN || r.split == ROLE_TRAIN, "row {} of Train population {} is in split {}", r.id, spec.name, r.split);
        if let Some(pr) = rule {
            ensure!(pr.splits.contains(&r.split), "row {} of population {} is in split {}, which the rules do not list for it", r.id, spec.name, r.split);
        }
        rows.push(r);
        Ok(())
    })
    .with_context(|| format!("population {}", spec.name))?;
    if let Some(want) = rule.and_then(|p| p.sha256.as_deref()) {
        ensure!(sha == want, "population {} ({}) hashes to {sha}, not the rules' {want}", spec.name, spec.rows.display());
    }
    let mut seen = BTreeSet::new();
    for r in &rows {
        ensure!(seen.insert(r.id.as_str()), "population {}: row id {} occurs twice", spec.name, r.id);
    }
    Ok(Population { spec: spec.clone(), sha256: sha, rows })
}

/// How a row refers to the style library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleRef {
    /// No style item.
    None,
    /// Every id it names is listed.
    Listed,
    /// It names an id that no library lists.
    Unlisted,
    /// Its cohort's style ids were never recorded.
    Unrecorded,
}

/// Rules joined with the style index.
pub struct Tagger<'a> {
    pub rules: &'a Rules,
    /// Tags of every listed style item.
    style: HashMap<&'a str, Tags>,
    /// Source domain -> collection for human-derived style items.
    pub sources: BTreeMap<String, String>,
}

impl<'a> Tagger<'a> {
    /// `human_collections`: domain -> collections of the human rows in the inputs.
    pub fn new(rules: &'a Rules, style: &'a StyleIndex, human_collections: &BTreeMap<String, BTreeSet<String>>) -> Result<Self> {
        let mut sources = BTreeMap::new();
        for it in style.items.values() {
            let Some(d) = &it.source_domain else { continue };
            if sources.contains_key(d) {
                continue;
            }
            let c = match rules.style_source_domains.get(d) {
                Some(c) => c.clone(),
                None => {
                    let cs = human_collections
                        .get(d)
                        .with_context(|| format!("style source domain {d} matches no human collection in the inputs"))?;
                    ensure!(cs.len() == 1, "style source domain {d} matches {} human collections", cs.len());
                    cs.iter().next().unwrap().clone()
                }
            };
            rules.collection(&c)?;
            sources.insert(d.clone(), c);
        }
        let mut tags = HashMap::with_capacity(style.items.len());
        for (id, it) in &style.items {
            let mut t = Tags::new();
            add(&mut t, rules.family(&it.family)?, VIA_INVENTOR);
            if let Some(r) = &it.reviewer_family {
                add(&mut t, rules.family(r)?, VIA_REVIEWER);
            }
            if let Some(d) = &it.source_domain {
                rules.collection_tags(&mut t, &sources[d], Some(d.as_str()), VIA_SOURCE)?;
            }
            tags.insert(id.as_str(), t);
        }
        Ok(Self { rules, style: tags, sources })
    }

    pub fn style_tags(&self, id: &str) -> Option<&Tags> {
        self.style.get(id)
    }

    /// Items of one row of a population with `role`, and how the row refers to the style library.
    pub fn tag_row(&self, r: &MetaRow, role: &str) -> Result<(Tags, StyleRef)> {
        let rules = self.rules;
        let mut t = Tags::new();
        add(&mut t, &rules.every_row, VIA_EVERY_ROW);
        rules.collection_tags(&mut t, r.collection()?, Some(r.domain.as_str()), VIA_COLLECTION)?;
        if let Some(g) = &r.generator_label {
            let e = rules.generators.get(g).with_context(|| format!("no rule for generator {g}"))?;
            add(&mut t, &e.items, VIA_GENERATOR);
        }
        let k = rules.source_kinds.get(&r.source_kind).with_context(|| format!("no rule for source kind {}", r.source_kind))?;
        add(&mut t, k, VIA_SOURCE_KIND);
        let p = rules.population_roles.get(role).with_context(|| format!("no rule for population role {role}"))?;
        add(&mut t, p, VIA_ROLE);
        let ids: Vec<&String> = [&r.voice_id, &r.brief_id].into_iter().flatten().collect();
        let vk = r.voice_kind.as_deref().unwrap_or(NULL_KEY);
        let lineage = rules.voice_kinds.get(vk).with_context(|| format!("no rule for voice kind {vk}"))?;
        if let Some(items) = rules.unrecorded_style_cohorts.get(&r.cohort_id) {
            ensure!(ids.is_empty(), "row {} of cohort {}, whose style ids the rules record as never recorded, names a style id", r.id, r.cohort_id);
            add(&mut t, items, VIA_STYLE_UNRECORDED);
            return Ok((t, StyleRef::Unrecorded));
        }
        match lineage {
            Lineage::Recorded => ensure!(
                !ids.is_empty(),
                "row {}: voice kind {vk} records its style id, but the row names none (style lineage unknown; a cohort whose ids were never recorded belongs in unrecorded_style_cohorts)",
                r.id
            ),
            Lineage::None => ensure!(ids.is_empty(), "row {}: voice kind {vk} carries no style item, but the row names one", r.id),
        }
        let mut sref = if ids.is_empty() { StyleRef::None } else { StyleRef::Listed };
        for id in ids {
            match self.style.get(id.as_str()) {
                Some(s) => merge(&mut t, s, VIA_STYLE),
                None => {
                    let o = r.voice_origin.as_deref().unwrap_or(NULL_KEY);
                    let items = rules
                        .unlisted_style_origins
                        .get(o)
                        .with_context(|| format!("row {}: style id {id} is in no library and its origin {o} has no rule", r.id))?;
                    add(&mut t, items, VIA_STYLE);
                    sref = StyleRef::Unlisted;
                }
            }
        }
        Ok((t, sref))
    }
}

fn style_route(via: &str) -> bool {
    via == VIA_STYLE || via == VIA_STYLE_UNRECORDED
}

/// `jitem_filter_v1`: None when the row is kept, else its first drop reason.
pub fn jitem_filter_v1(t: &Tags) -> Option<&'static str> {
    let [j1, j2, j4] = JITEM_FILTER_V1_EXCLUDES;
    if t.contains_key(j1) {
        return Some(DROP_REASONS[0]);
    }
    if let Some(v) = t.get(j2)
        && v.iter().any(|x| !style_route(x))
    {
        return Some(DROP_REASONS[1]);
    }
    if t.contains_key(j4) {
        return Some(DROP_REASONS[2]);
    }
    if t.contains_key(j2) {
        return Some(DROP_REASONS[3]);
    }
    None
}

/// Rows per population and origin.
type Counts = BTreeMap<String, [u64; 3]>;

fn bump(c: &mut Counts, pop: &str, o: usize) {
    c.entry(pop.to_string()).or_default()[o] += 1;
}

fn counts_value(c: &Counts) -> Value {
    Value::Object(c.iter().map(|(k, v)| (k.clone(), by_origin(v))).collect())
}

#[derive(Default)]
struct PopStats {
    rows: [u64; 3],
    weight: [f64; 3],
    splits: BTreeMap<String, u64>,
    items: BTreeMap<String, [u64; 3]>,
    kept: [u64; 3],
    kept_weight: [f64; 3],
    dropped: BTreeMap<&'static str, [u64; 3]>,
    voice_kinds: BTreeMap<String, u64>,
    style_listed: u64,
    style_unlisted: BTreeMap<String, u64>,
    style_unrecorded: u64,
}

/// Everything a snapshot run reads.
#[derive(Clone, Debug)]
pub struct SnapshotInput {
    pub version: Version,
    pub rules: PathBuf,
    pub populations: Vec<PopulationSpec>,
    /// Pool files tagged by collection (rows that are not bundle rows yet).
    pub pools: Vec<PoolSpec>,
    pub stage0: PathBuf,
    /// Populations whose `jitem_filter_v1` rows are written as an allow-list.
    pub allowlist: Vec<String>,
    /// Trainer-bundle directory whose Train block the allow-list is checked against (one allow-list only).
    pub trainer_bundle: Option<PathBuf>,
    /// Refuse a draft.
    pub require_final: bool,
    pub commit: String,
    pub executable_sha256: Option<String>,
}

/// Files of one snapshot, ready to write.
pub struct SnapshotOutput {
    pub version: Version,
    pub snapshot: Value,
    pub row_tags: Vec<u8>,
    pub allowlists: Vec<(String, Vec<u8>)>,
}

fn allowlist_file(pop: &str) -> String {
    format!("allowlist-{pop}-{JITEM_FILTER_V1}.json")
}

/// Check the allow-listed ids against the Train block of the trainer bundle in `bundle`.
///
/// The trainer writes Train rows first (then Train-mining, Dev-iter, Dev-select and CAL) and
/// records the Train row count and Train source digest in its MANIFEST. The check reads the
/// MANIFEST, requires its Train source to be `population_sha256`, then parses exactly the first
/// `rows_by_split.train` lines of `rows.jsonl`, each a Train row, and stops. No later line is
/// parsed or hashed (protocol A2 §2.4); at most one read buffer past the block is loaded and
/// dropped. The rows file's whole-file digest is the MANIFEST's, not recomputed.
pub fn check_trainer_bundle(
    kept: &BTreeMap<String, String>,
    population: &BTreeMap<String, String>,
    population_sha256: &str,
    bundle: &Path,
) -> Result<Value> {
    #[derive(Deserialize)]
    struct T {
        id: String,
        split: String,
        text_sha256: String,
    }
    let mp = bundle.join("MANIFEST.json");
    let raw = fs::read(&mp).with_context(|| format!("read {}", mp.display()))?;
    let m: Value = serde_json::from_slice(&raw).with_context(|| format!("parse {}", mp.display()))?;
    ensure!(m["schema"] == TRAINER_MANIFEST_SCHEMA, "{} is not a {TRAINER_MANIFEST_SCHEMA}", mp.display());
    ensure!(m["smoke_only"] == false, "{} is a smoke bundle", mp.display());
    let n = m["rows_by_split"][ROLE_TRAIN].as_u64().with_context(|| format!("{} has no rows_by_split.train", mp.display()))?;
    ensure!(n > 0, "{} records no Train rows", mp.display());
    let src = m["source_splits"][ROLE_TRAIN]["rows_sha256"].as_str().with_context(|| format!("{} has no Train source digest", mp.display()))?;
    ensure!(src == population_sha256, "the trainer bundle's Train rows come from {src}, not the population's {population_sha256}");
    let rows_path = bundle.join("rows.jsonl");
    let f = fs::File::open(&rows_path).with_context(|| format!("open {}", rows_path.display()))?;
    let mut rd = BufReader::with_capacity(TRAINER_READ_BUFFER, f);
    let (mut h, mut line) = (Sha256::new(), Vec::new());
    let mut train = BTreeMap::new();
    for i in 1..=n {
        line.clear();
        ensure!(rd.read_until(b'\n', &mut line)? > 0, "trainer rows end after {} of {n} Train lines", i - 1);
        ensure!(line.last() == Some(&b'\n'), "trainer line {i} has no newline");
        h.update(&line);
        let r: T = serde_json::from_slice(&line[..line.len() - 1]).with_context(|| format!("trainer Train line {i}"))?;
        ensure!(r.split == ROLE_TRAIN, "trainer line {i} is in split {}: the Train block is shorter than the MANIFEST's {n}", r.split);
        ensure!(train.insert(r.id.clone(), r.text_sha256).is_none(), "trainer row id {} occurs twice", r.id);
    }
    drop(rd);
    let (mut missing, mut mismatch) = (0u64, 0u64);
    for (id, d) in kept {
        match train.get(id) {
            None => missing += 1,
            Some(x) if x != d => mismatch += 1,
            Some(_) => {}
        }
    }
    let not_in_population = train.keys().filter(|k| !population.contains_key(*k)).count();
    let population_not_in_trainer = population.keys().filter(|k| !train.contains_key(*k)).count();
    ensure!(missing == 0 && mismatch == 0, "allow-list against trainer Train rows: {missing} ids missing, {mismatch} text digests differ");
    Ok(json!({
        "bundle": bundle, "read": "MANIFEST.json and the Train block of rows.jsonl only",
        "manifest": {"sha256": sha_hex(&raw), "schema": m["schema"], "rows_by_split": m["rows_by_split"],
            "rows_sha256": m["files"]["rows.jsonl"], "train_source_sha256": src},
        "train_block": {"lines": n, "sha256": hex::encode(h.finalize())},
        "train_rows": train.len(), "allowlisted_present": kept.len(), "allowlisted_missing": missing, "text_digest_mismatch": mismatch,
        "trainer_train_rows_not_in_population": not_in_population,
        "population_rows_not_in_trainer": population_not_in_trainer,
    }))
}

/// Tag the rows of a pool file by collection (and domain) only. With `expect_item`, the file
/// must hold rows and every row must carry the item. Every Enron row must carry `ENRON_ITEM`.
fn tag_pool_rows(rules: &Rules, rows: &Path, expect_item: Option<&str>) -> Result<Value> {
    #[derive(Deserialize)]
    struct P {
        #[serde(default)]
        collection: Option<String>,
        #[serde(default)]
        domain: Option<String>,
    }
    if let Some(i) = expect_item {
        ensure!(rules.items.contains_key(i), "item {i} is not in the catalogue");
    }
    let mut per: BTreeMap<String, (u64, BTreeMap<String, u64>)> = BTreeMap::new();
    let (mut n, mut with_item, mut enron, mut enron_item) = (0u64, 0u64, 0u64, 0u64);
    let (sha, _) = for_lines(rows, |l| {
        let p: P = serde_json::from_slice(l)?;
        let c = p.collection.context("pool row has no collection")?;
        let mut t = Tags::new();
        add(&mut t, &rules.every_row, VIA_EVERY_ROW);
        rules.collection_tags(&mut t, &c, p.domain.as_deref(), VIA_COLLECTION)?;
        n += 1;
        if expect_item.is_some_and(|i| t.contains_key(i)) {
            with_item += 1;
        }
        if c == ENRON_COLLECTION {
            enron += 1;
            enron_item += u64::from(t.contains_key(ENRON_ITEM));
        }
        let e = per.entry(c).or_default();
        e.0 += 1;
        for i in t.keys() {
            *e.1.entry(i.clone()).or_default() += 1;
        }
        Ok(())
    })?;
    if let Some(i) = expect_item {
        ensure!(n > 0, "{} holds no pool rows, so none carries {i}", rows.display());
        ensure!(with_item == n, "{with_item} of {n} pool rows carry {i}");
    }
    ensure!(enron_item == enron, "{enron_item} of {enron} {ENRON_COLLECTION} pool rows carry {ENRON_ITEM}");
    Ok(json!({
        "rows": {"path": rows, "sha256": sha, "rows": n},
        "collections": Value::Object(per.into_iter().map(|(c, (k, items))| (c, json!({"rows": k, "items": items}))).collect()),
        "expect_item": expect_item, "rows_with_expected_item": expect_item.map(|_| with_item),
        "enron_rows": enron, "enron_rows_with_item": enron_item,
    }))
}

/// Build a snapshot in memory. Nothing is written when any check fails.
pub fn build_snapshot(inp: &SnapshotInput) -> Result<SnapshotOutput> {
    let (rules, rules_sha) = Rules::load(&inp.rules)?;
    ensure!(!inp.populations.is_empty(), "no population");
    let mut names = BTreeSet::new();
    for p in &inp.populations {
        ensure!(names.insert(p.name.as_str()), "population {} named twice", p.name);
    }
    match inp.version {
        Version::V1 => {
            for p in &inp.populations {
                ensure!(p.role == ROLE_TRAIN, "a version-1 snapshot holds Train populations only ({} is {})", p.name, p.role);
            }
            ensure!(rules.reproduce.is_some(), "a version-1 snapshot needs the rules' reproduce counts (S0-4)");
            ensure!(
                rules.row_schemas.is_empty() || rules.row_schemas == [V1_ROW_SCHEMA],
                "a version-1 snapshot reads {V1_ROW_SCHEMA} rows only (the rules list {:?})",
                rules.row_schemas
            );
        }
        Version::V2 => {
            ensure!(!rules.populations.is_empty(), "a version-2 snapshot needs the rules' population list (item 1.7)");
            ensure!(!rules.row_schemas.is_empty(), "a version-2 snapshot needs the rules' row schemas");
            for r in ROLES {
                ensure!(inp.populations.iter().any(|p| p.role == r), "a version-2 snapshot needs a {r} population (item 1.7)");
            }
        }
    }
    if !rules.populations.is_empty() {
        let want: BTreeSet<(&str, &str)> = rules.populations.iter().map(|p| (p.name.as_str(), p.role.as_str())).collect();
        let got: BTreeSet<(&str, &str)> = inp.populations.iter().map(|p| (p.name.as_str(), p.role.as_str())).collect();
        ensure!(want == got, "the run supplies populations {got:?}; the rules require exactly {want:?}");
    }
    if let Some(rp) = &rules.reproduce {
        ensure!(names.contains(rp.population.as_str()), "reproduce names population {}, which is not an input", rp.population);
    }
    for a in &inp.allowlist {
        let p = inp.populations.iter().find(|p| &p.name == a).with_context(|| format!("allow-list population {a} is not an input"))?;
        ensure!(p.role == ROLE_TRAIN, "allow-lists are for Train populations ({a} is {})", p.role);
    }
    ensure!(inp.trainer_bundle.is_none() || inp.allowlist.len() == 1, "the trainer check needs exactly one allow-list");
    let mut pool_names = BTreeSet::new();
    for p in &inp.pools {
        ensure!(pool_names.insert(p.name.as_str()), "pool {} named twice", p.name);
    }

    let (stage0_sha, a3, a3_sha) = stage0_record(&inp.stage0, "A3")?;
    let a3_status = a3["status"].as_str().with_context(|| format!("{} has no items.A3.status", inp.stage0.display()))?;
    let a3_done = a3_status.starts_with("done");
    let checked = rules.findings_checked.get("A3");
    if let Some(c) = checked {
        ensure!(*c == a3_sha, "the rules were checked against A3 record {c}, but STAGE0.json's A3 record is {a3_sha}");
    }
    let status_reason = if !a3_done {
        Some(format!("STAGE0.json records A3 as {a3_status}"))
    } else if checked.is_none() {
        Some(format!("the rules do not bind the A3 record they were checked against (findings_checked.A3; the record is {a3_sha})"))
    } else {
        None
    };
    ensure!(status_reason.is_none() || !inp.require_final, "a final snapshot waits: {}", status_reason.as_deref().unwrap_or(""));

    let style = StyleIndex::load(&rules.style_libraries)?;
    let schemas: Vec<&str> = match inp.version {
        Version::V1 => vec![V1_ROW_SCHEMA],
        Version::V2 => rules.row_schemas.iter().map(String::as_str).collect(),
    };
    let pops: Vec<Population> = inp
        .populations
        .iter()
        .map(|s| load_population(s, &schemas, rules.populations.iter().find(|p| p.name == s.name)))
        .collect::<Result<_>>()?;

    let mut human_collections: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in &pops {
        for r in p.rows.iter().filter(|r| r.source_kind == HUMAN_KIND) {
            human_collections.entry(r.domain.clone()).or_default().insert(r.collection()?.to_string());
        }
    }
    let tagger = Tagger::new(&rules, &style, &human_collections)?;

    let mut coll_rows: BTreeMap<String, Counts> = BTreeMap::new();
    let mut coll_domains: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut gen_rows: BTreeMap<String, Counts> = BTreeMap::new();
    let mut gen_keys: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut gen_kinds: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut kind_rows: BTreeMap<String, Counts> = BTreeMap::new();
    let mut item_rows: BTreeMap<String, Counts> = BTreeMap::new();
    let mut stats: BTreeMap<String, PopStats> = BTreeMap::new();
    let mut tag_lines: Vec<(String, String, Vec<u8>)> = Vec::new();
    let mut kept_ids: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut all_ids: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    for p in &pops {
        let name = p.spec.name.as_str();
        let st = stats.entry(name.to_string()).or_default();
        for r in &p.rows {
            let o = origin_index(&r.origin)?;
            let (t, sref) = tagger.tag_row(r, &p.spec.role)?;
            let c = r.collection()?;
            bump(coll_rows.entry(c.to_string()).or_default(), name, o);
            *coll_domains.entry(c.to_string()).or_default().entry(r.domain.clone()).or_default() += 1;
            if let Some(g) = &r.generator_label {
                bump(gen_rows.entry(g.clone()).or_default(), name, o);
                gen_kinds.entry(g.clone()).or_default().insert(r.source_kind.clone());
                if let Some(k) = &r.generator_key {
                    gen_keys.entry(g.clone()).or_default().insert(k.clone());
                }
            }
            bump(kind_rows.entry(r.source_kind.clone()).or_default(), name, o);
            st.rows[o] += 1;
            st.weight[o] += r.weight;
            *st.splits.entry(r.split.clone()).or_default() += 1;
            *st.voice_kinds.entry(r.voice_kind.clone().unwrap_or_else(|| NULL_KEY.into())).or_default() += 1;
            match sref {
                StyleRef::None => {}
                StyleRef::Listed => st.style_listed += 1,
                StyleRef::Unlisted => *st.style_unlisted.entry(r.voice_origin.clone().unwrap_or_else(|| NULL_KEY.into())).or_default() += 1,
                StyleRef::Unrecorded => st.style_unrecorded += 1,
            }
            for i in t.keys() {
                st.items.entry(i.clone()).or_default()[o] += 1;
                bump(item_rows.entry(i.clone()).or_default(), name, o);
            }
            let verdict = jitem_filter_v1(&t);
            match verdict {
                None => {
                    st.kept[o] += 1;
                    st.kept_weight[o] += r.weight;
                    kept_ids.entry(name.to_string()).or_default().insert(r.id.clone(), r.text_sha256.clone());
                }
                Some(reason) => st.dropped.entry(reason).or_default()[o] += 1,
            }
            all_ids.entry(name.to_string()).or_default().insert(r.id.clone(), r.text_sha256.clone());
            let via: Map<String, Value> = t.iter().map(|(i, v)| (i.clone(), json!(v))).collect();
            let line = json!({
                "schema": ROW_TAGS_SCHEMA, "population": name, "id": r.id, "origin": r.origin,
                "items": t.keys().collect::<Vec<_>>(), "via": via, JITEM_FILTER_V1: verdict.unwrap_or("kept"),
            });
            let mut b = serde_json::to_vec(&line)?;
            b.push(b'\n');
            tag_lines.push((name.to_string(), r.id.clone(), b));
        }
    }

    // Reproduction (version 1 always; version 2 when the rules carry it). Every count must match.
    let mut reproduce = Value::Null;
    if let Some(rp) = &rules.reproduce {
        let st = stats.get(&rp.population).with_context(|| format!("reproduce names population {}, which is not an input", rp.population))?;
        let mut observed_kept = BTreeMap::new();
        let mut ok = true;
        for (i, o) in ORIGINS.iter().enumerate() {
            observed_kept.insert(o.to_string(), st.kept[i]);
            ok &= rp.kept.get(*o) == Some(&st.kept[i]);
        }
        let kept_total = st.kept.iter().sum::<u64>();
        ok &= rp.kept_total == kept_total;
        let mut observed_reasons = BTreeMap::new();
        for r in DROP_REASONS {
            let n = st.dropped.get(r).map_or(0, |a| a[1] + a[2]);
            observed_reasons.insert(r.to_string(), n);
            ok &= rp.dropped_positives_by_first_reason.get(r) == Some(&n);
        }
        ensure!(
            ok,
            "{JITEM_FILTER_V1} does not reproduce {} on {}: kept {:?} = {kept_total} (want {:?} = {}), dropped positives {:?} (want {:?})",
            rp.basis.label,
            rp.population,
            observed_kept,
            rp.kept,
            rp.kept_total,
            observed_reasons,
            rp.dropped_positives_by_first_reason
        );
        reproduce = json!({
            "filter": rp.filter, "population": rp.population, "basis": rp.basis.record(),
            "expected": {"kept": rp.kept, "kept_total": rp.kept_total, "dropped_positives_by_first_reason": rp.dropped_positives_by_first_reason},
            "observed": {"kept": observed_kept, "kept_total": kept_total, "dropped_positives_by_first_reason": observed_reasons},
            "reproduced": true,
        });
    }

    // Allow-lists and the trainer check.
    let mut allowlists = Vec::new();
    let mut allow_records = Vec::new();
    let mut trainer_check = Value::Null;
    for a in &inp.allowlist {
        let p = pops.iter().find(|p| &p.spec.name == a).with_context(|| format!("allow-list population {a} is not an input"))?;
        let kept = kept_ids.get(a.as_str()).cloned().unwrap_or_default();
        let ids: Vec<&String> = kept.keys().collect();
        let mut joined = String::new();
        for id in &ids {
            joined.push_str(id);
            joined.push('\n');
        }
        let st = &stats[a.as_str()];
        if let Some(tb) = &inp.trainer_bundle {
            trainer_check = check_trainer_bundle(&kept, all_ids.get(a.as_str()).unwrap_or(&BTreeMap::new()), &p.sha256, tb)?;
        }
        let v = json!({
            "schema": ALLOWLIST_SCHEMA, "filter": JITEM_FILTER_V1, "excludes": JITEM_FILTER_V1_EXCLUDES,
            "population": a, "split": ROLE_TRAIN,
            "rows": {"path": p.spec.rows, "sha256": p.sha256, "rows": p.rows.len()},
            "rules_sha256": rules_sha, "kept": by_origin(&st.kept), "count": ids.len(),
            "ids_sha256": sha_hex(joined.as_bytes()), "ids": ids,
        });
        let mut b = serde_json::to_vec_pretty(&v)?;
        b.push(b'\n');
        let file = allowlist_file(a);
        allow_records.push(json!({"population": a, "file": file, "sha256": sha_hex(&b), "count": kept.len(), "ids_sha256": v["ids_sha256"]}));
        allowlists.push((file, b));
    }

    let mut pools = Vec::new();
    for p in &inp.pools {
        let mut v = tag_pool_rows(&rules, &p.rows, p.expect_item.as_deref()).with_context(|| format!("pool {}", p.name))?;
        v["name"] = json!(p.name);
        pools.push(v);
    }

    tag_lines.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut row_tags = Vec::new();
    for (_, _, b) in &tag_lines {
        row_tags.extend_from_slice(b);
    }

    // Entity listings: every rule, observed or not, plus every observed name (all have rules).
    let collections: Vec<Value> = rules
        .collections
        .iter()
        .map(|(c, e)| {
            json!({
                "collection": c, "licence": e.licence, "items": e.items, "by_domain": e.by_domain,
                "note": e.note, "public_note": e.public_note,
                "rows": counts_value(coll_rows.get(c).unwrap_or(&Counts::new())),
                "domains": coll_domains.get(c).cloned().unwrap_or_default(),
            })
        })
        .collect();
    let generators: Vec<Value> = rules
        .generators
        .iter()
        .map(|(g, e)| {
            json!({
                "generator_label": g, "licence": e.licence, "items": e.items, "note": e.note, "public_note": e.public_note,
                "keys": gen_keys.get(g).cloned().unwrap_or_default(),
                "source_kinds": gen_kinds.get(g).cloned().unwrap_or_default(),
                "rows": counts_value(gen_rows.get(g).unwrap_or(&Counts::new())),
            })
        })
        .collect();
    let source_kinds: Vec<Value> = rules
        .source_kinds
        .iter()
        .map(|(k, items)| json!({"source_kind": k, "items": items, "rows": counts_value(kind_rows.get(k).unwrap_or(&Counts::new()))}))
        .collect();

    let mut fam_use: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    type GroupKey<'k> = (&'k str, &'k str, Option<&'k str>, &'k str, Option<&'k str>, Option<&'k str>, Option<&'k str>);
    let mut groups: BTreeMap<GroupKey<'_>, Vec<&str>> = BTreeMap::new();
    for (id, it) in &style.items {
        fam_use.entry(it.family.as_str()).or_default().0 += 1;
        if let Some(r) = &it.reviewer_family {
            fam_use.entry(r.as_str()).or_default().1 += 1;
        }
        let src = it.source_domain.as_deref().map(|d| tagger.sources[d].as_str());
        let key = (
            it.kind.as_str(),
            it.family.as_str(),
            it.reviewer_family.as_deref(),
            it.origin.as_str(),
            it.source_domain.as_deref(),
            it.source_split.as_deref(),
            src,
        );
        groups.entry(key).or_default().push(id.as_str());
    }
    let style_items: Vec<Value> = groups
        .into_iter()
        .map(|((kind, fam, rev, origin, dom, split, coll), mut ids)| {
            ids.sort_unstable();
            let t = tagger.style_tags(ids[0]).unwrap();
            let via: Map<String, Value> = t.iter().map(|(i, v)| (i.clone(), json!(v))).collect();
            json!({
                "type": kind, "family": fam, "reviewer_family": rev, "origin": origin,
                "source_domain": dom, "source_split": split, "source_collection": coll,
                "items": t.keys().collect::<Vec<_>>(), "via": via, "count": ids.len(), "ids": ids,
            })
        })
        .collect();
    let style_families: Vec<Value> = rules
        .style_families
        .iter()
        .map(|(f, items)| {
            let (inv, rev) = fam_use.get(f.as_str()).copied().unwrap_or_default();
            json!({"family": f, "items": items, "invented": inv, "reviewed": rev})
        })
        .collect();

    let items: Map<String, Value> = rules
        .items
        .iter()
        .map(|(i, d)| (i.clone(), json!({"title": d.title, "answer": d.answer, "rows": counts_value(item_rows.get(i).unwrap_or(&Counts::new()))})))
        .collect();
    let mut populations = Vec::new();
    let mut filter_pops = Map::new();
    for p in &pops {
        let st = &stats[p.spec.name.as_str()];
        populations.push(json!({
            "name": p.spec.name, "role": p.spec.role, "path": p.spec.rows, "sha256": p.sha256, "rows": p.rows.len(),
            "rows_by_origin": by_origin(&st.rows), "weight_by_origin": by_origin(&st.weight), "splits": st.splits,
            "items": Value::Object(st.items.iter().map(|(i, a)| (i.clone(), by_origin(a))).collect()),
            "voice_kinds": st.voice_kinds,
            "style_references": {"listed": st.style_listed, "unlisted_by_origin": st.style_unlisted, "unrecorded": st.style_unrecorded},
        }));
        let dropped: Map<String, Value> = DROP_REASONS.iter().map(|r| (r.to_string(), by_origin(st.dropped.get(r).unwrap_or(&[0; 3])))).collect();
        filter_pops.insert(
            p.spec.name.clone(),
            json!({
                "total": by_origin(&st.rows), "kept": by_origin(&st.kept), "kept_total": st.kept.iter().sum::<u64>(),
                "total_weight": by_origin(&st.weight), "kept_weight": by_origin(&st.kept_weight),
                "dropped_by_first_reason": dropped,
            }),
        );
    }
    let derived: Vec<Value> = rules
        .derived_labels
        .iter()
        .map(|(k, d)| json!({"label": k, "path": d.path, "sha256": d.sha256, "items": d.items, "note": d.note, "public_note": d.public_note, "tags_rows": false}))
        .collect();
    let inits: Vec<Value> = rules
        .model_inits
        .iter()
        .map(|(k, e)| json!({"name": k, "licence": e.licence, "items": e.items, "note": e.note, "public_note": e.public_note}))
        .collect();
    let population_rules: Vec<Value> =
        rules.populations.iter().map(|p| json!({"name": p.name, "role": p.role, "splits": p.splits, "sha256": p.sha256})).collect();

    let snapshot = json!({
        "schema": inp.version.schema(),
        "status": if status_reason.is_none() { "final" } else { "draft" },
        "status_reason": status_reason,
        "code": {"commit": inp.commit, "executable_sha256": inp.executable_sha256},
        "rules": {"path": inp.rules, "sha256": rules_sha},
        "basis": rules.basis.iter().map(Bound::record).collect::<Vec<_>>(),
        "stage0": {"path": inp.stage0, "sha256": stage0_sha},
        "findings": {"A3": {"record": a3, "record_sha256": a3_sha, "checked_by_rules": checked.is_some()}},
        "items": items,
        "every_row": rules.every_row,
        "population_roles": rules.population_roles,
        "population_rules": population_rules,
        "row_schemas": schemas,
        "populations": populations,
        "pools": pools,
        "collections": collections,
        "generators": generators,
        "source_kinds": source_kinds,
        "style_libraries": style.files,
        "style_duplicate_ids": style.duplicates,
        "style_families": style_families,
        "style_source_collections": tagger.sources,
        "voice_kinds": rules.voice_kinds,
        "unrecorded_style_cohorts": rules.unrecorded_style_cohorts,
        "unlisted_style_origins": rules.unlisted_style_origins,
        "style_items": style_items,
        "derived_labels": derived,
        "model_inits": inits,
        "filters": {JITEM_FILTER_V1: {"excludes": JITEM_FILTER_V1_EXCLUDES, "reason_order": DROP_REASONS, "populations": filter_pops}},
        "reproduce": reproduce,
        "row_tags": {"file": ROW_TAGS_FILE, "schema": ROW_TAGS_SCHEMA, "sha256": sha_hex(&row_tags), "rows": tag_lines.len()},
        "allowlists": allow_records,
        "trainer_check": trainer_check,
    });
    Ok(SnapshotOutput { version: inp.version, snapshot, row_tags, allowlists })
}

fn write_private(p: &Path, b: &[u8]) -> Result<String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(p)
        .with_context(|| format!("create {}", p.display()))?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(sha_hex(b))
}

/// Write a snapshot into the new directory `out` (mode 700, files mode 600). The files are
/// written and synced in a sibling temporary directory that is then renamed to `out`, so `out`
/// appears complete or not at all. Returns the snapshot's SHA-256.
pub fn write_snapshot(out: &Path, o: &SnapshotOutput) -> Result<String> {
    use std::os::unix::fs::DirBuilderExt;
    let name = out.file_name().and_then(|n| n.to_str()).with_context(|| format!("{} has no file name", out.display()))?;
    let parent = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    ensure!(fs::symlink_metadata(out).is_err(), "{} exists (written once)", out.display());
    let tmp = parent.join(format!(".{name}.partial-{}", std::process::id()));
    fs::DirBuilder::new().mode(0o700).create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    let res = (|| -> Result<String> {
        write_private(&tmp.join(ROW_TAGS_FILE), &o.row_tags)?;
        for (f, b) in &o.allowlists {
            write_private(&tmp.join(f), b)?;
        }
        let mut b = serde_json::to_vec_pretty(&o.snapshot)?;
        b.push(b'\n');
        let sha = write_private(&tmp.join(o.version.file_name()), &b)?;
        fs::File::open(&tmp)?.sync_all()?;
        ensure!(fs::symlink_metadata(out).is_err(), "{} exists (written once)", out.display());
        fs::rename(&tmp, out).with_context(|| format!("rename {} to {}", tmp.display(), out.display()))?;
        Ok(sha)
    })();
    if res.is_err() {
        let _ = fs::remove_dir_all(&tmp);
    }
    let sha = res?;
    fs::File::open(&parent)?.sync_all()?;
    Ok(sha)
}

/// Write one text-free JSON record, once, mode 600.
pub fn write_record(out: &Path, v: &Value) -> Result<String> {
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    write_private(out, &b)
}

/// Tag the rows of a pool file by collection (and domain) only, for rows that are not
/// bundle rows yet. With `expect_item`, the file must hold rows and every row must carry it.
pub fn tag_pool(rules_path: &Path, rows: &Path, expect_item: Option<&str>) -> Result<Value> {
    let (rules, rules_sha) = Rules::load(rules_path)?;
    let mut v = tag_pool_rows(&rules, rows, expect_item)?;
    v["schema"] = json!(POOL_TAGS_SCHEMA);
    v["rules_sha256"] = json!(rules_sha);
    Ok(v)
}

/// The named fields of a JSON object (null when absent).
fn keep_fields(e: &Value, keep: &[&str]) -> Value {
    Value::Object(keep.iter().map(|f| ((*f).to_string(), e[*f].clone())).collect())
}

/// `DATA_RIGHTS.json` content from a final version-2 snapshot (protocol item 1.7).
/// Counts, digests, licences and published notes only: no ids, text, paths, private notes or
/// free-text findings. Every item in use without a recorded answer is open, and so is any
/// unrecorded style lineage.
pub fn data_rights(snapshot: &Value, snapshot_sha256: &str) -> Result<Value> {
    let schema = snapshot["schema"].as_str().unwrap_or("");
    ensure!(
        schema == SNAPSHOT_V2_SCHEMA,
        "DATA_RIGHTS is generated from a {SNAPSHOT_V2_SCHEMA} snapshot (protocol item 1.7), not {schema}"
    );
    ensure!(snapshot["status"] == "final", "DATA_RIGHTS needs a final snapshot (status {})", snapshot["status"]);
    let total = |rows: &Value| -> u64 {
        rows.as_object()
            .map(|m| m.values().flat_map(|o| o.as_object().into_iter().flat_map(|x| x.values())).filter_map(Value::as_u64).sum())
            .unwrap_or(0)
    };
    let list = |key: &str| snapshot[key].as_array().into_iter().flatten();
    let in_rows = |key: &str, keep: &[&str]| -> Vec<Value> { list(key).filter(|e| total(&e["rows"]) > 0).map(|e| keep_fields(e, keep)).collect() };
    let all = |key: &str, keep: &[&str]| -> Vec<Value> { list(key).map(|e| keep_fields(e, keep)).collect() };
    let sources = in_rows("collections", &["collection", "licence", "items", "by_domain", "public_note", "rows"]);
    let generators = in_rows("generators", &["generator_label", "licence", "items", "public_note", "rows"]);
    let mut in_use: BTreeSet<String> = BTreeSet::new();
    let mut items = Vec::new();
    for (i, d) in snapshot["items"].as_object().context("snapshot items")? {
        if total(&d["rows"]) > 0 {
            in_use.insert(i.clone());
        }
        items.push((i.clone(), d.clone()));
    }
    for key in ["derived_labels", "model_inits"] {
        for e in list(key) {
            for i in e["items"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                in_use.insert(i.to_string());
            }
        }
    }
    let items: Vec<Value> = items
        .into_iter()
        .map(|(i, d)| json!({"item": i, "title": d["title"], "answer": d["answer"], "in_use": in_use.contains(&i), "rows": d["rows"]}))
        .collect();
    let open: Vec<&str> = items
        .iter()
        .filter(|x| x["in_use"] == true && x["answer"].is_null())
        .filter_map(|x| x["item"].as_str())
        .collect();
    let pops = snapshot["populations"].as_array().context("snapshot populations")?;
    let unrecorded: u64 = pops.iter().filter_map(|p| p["style_references"]["unrecorded"].as_u64()).sum();
    let markers: Vec<&str> = if unrecorded > 0 { vec![MARKER_STYLE_LINEAGE_UNRECORDED] } else { vec![] };
    let release = if open.is_empty() && markers.is_empty() { "no_open_items" } else { "blocked_by_open_items" };
    let populations: Vec<Value> = pops.iter().map(|p| keep_fields(p, &["name", "role", "sha256", "rows", "rows_by_origin"])).collect();
    let style_items: u64 = list("style_items").filter_map(|g| g["count"].as_u64()).sum();
    let mut findings = Map::new();
    for (k, f) in snapshot["findings"].as_object().into_iter().flatten() {
        let rec = &f["record"];
        let flags: Map<String, Value> =
            rec["finding"].as_object().into_iter().flatten().filter(|(_, v)| v.is_boolean()).map(|(k, v)| (k.clone(), v.clone())).collect();
        findings.insert(k.clone(), json!({"record_sha256": f["record_sha256"], "status": rec["status"], "utc": rec["utc"], "flags": flags}));
    }
    let content = json!({
        "schema": DATA_RIGHTS_SCHEMA,
        "snapshot": {"schema": schema, "sha256": snapshot_sha256, "rules_sha256": snapshot["rules"]["sha256"], "row_tags_sha256": snapshot["row_tags"]["sha256"]},
        "populations": populations,
        "sources": sources,
        "generators": generators,
        "style_library": {"files": all("style_libraries", &["sha256", "rows"]), "items": style_items, "families": snapshot["style_families"]},
        "style_lineage_unrecorded_rows": unrecorded,
        "derived_labels": all("derived_labels", &["label", "sha256", "items", "tags_rows", "public_note"]),
        "model_inits": all("model_inits", &["name", "licence", "items", "public_note"]),
        "findings": findings,
        "every_row": snapshot["every_row"],
        "items": items,
        "open_items": open,
        "open_markers": markers,
        "release": release,
    });
    Ok(json!({"content_sha256": sha_hex(&serde_json::to_vec(&content)?), "content": content}))
}

#[cfg(test)]
mod tests {
    //! Synthetic fixtures only: invented collections, generators, families, notes and texts.
    use super::*;

    const PRIVATE_NOTE: &str = "private synthetic note";
    const PUBLIC_NOTE: &str = "published synthetic note";
    const SPLICED: &str = "spliced:synthetic";

    fn write(p: &Path, b: &[u8]) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b).unwrap();
    }

    fn jsonl(p: &Path, rows: &[Value]) {
        let mut b = Vec::new();
        for r in rows {
            serde_json::to_writer(&mut b, r).unwrap();
            b.push(b'\n');
        }
        write(p, &b);
    }

    #[allow(clippy::too_many_arguments)]
    fn row(id: &str, origin: &str, coll: &str, domain: &str, kind: &str, generator: Option<&str>, voice: Option<&str>, brief: Option<&str>, voice_origin: Option<&str>) -> Value {
        let text = format!("Synthetic text for {id}.");
        let voice_kind = match (voice, brief) {
            (Some(_), _) => Some("voice"),
            (None, Some(_)) => Some("brief"),
            _ if kind == "human" => None,
            _ => Some("operation_only"),
        };
        json!({
            "schema": V1_ROW_SCHEMA, "id": id, "split": "train", "text": text, "text_sha256": sha_hex(text.as_bytes()),
            "origin": origin, "generator_label": generator, "generator_key": generator.map(|g| format!("{g}-key")),
            "cohort_id": format!("{kind}:{coll}"), "collection": coll, "domain": domain, "source_kind": kind,
            "voice_id": voice, "brief_id": brief, "voice_kind": voice_kind, "voice_origin": voice_origin, "weight": 0.5,
        })
    }

    fn rows() -> Vec<Value> {
        let (h, l, f) = ("human", "local_positive", "frontier");
        vec![
            row("h_web", "human_only", "coll_web", "web", h, None, None, None, None),
            row("h_wiki", "human_only", "coll_wiki", "wiki", h, None, None, None, None),
            row("h_essay", "human_only", "coll_essay", "essay", h, None, None, None, None),
            row("h_gov", "human_only", "coll_gov", "gov", h, None, None, None, None),
            row("h_bench", "human_only", "coll_bench", "bench_books", "external_pool", None, None, None, None),
            row("m_wiki_a", "mixed", "coll_wiki", "wiki", l, Some("gen_a"), Some("v_inv_a"), None, Some("invented")),
            row("m_wiki_b", "mixed", "coll_wiki", "wiki", l, Some("gen_b"), None, None, None),
            row("m_wiki_style", "mixed", "coll_wiki", "wiki", l, Some("gen_a"), Some("v_inv_b"), None, Some("invented")),
            row("m_wiki_rev", "model_only", "coll_wiki", "wiki", l, Some("gen_a"), None, Some("b_rev_b"), Some("invented")),
            row("m_web_a", "mixed", "coll_web", "web", l, Some("gen_a"), None, None, None),
            row("m_essay_style", "mixed", "coll_essay", "essay", l, Some("gen_a"), Some("v_inv_b"), None, Some("invented")),
            row("f_wiki", "model_only", "coll_wiki", "wiki", f, Some("gen_f"), None, None, None),
            row("p_pilot", "mixed", "coll_wiki", "wiki", l, Some("gen_a"), Some("pilot-1"), None, Some("pilot_seed")),
            row("m_gov_hd", "model_only", "coll_gov", "gov", l, Some("gen_a"), None, Some("b_hd"), Some("human_derived")),
        ]
    }

    /// A row of a cohort whose style ids were never recorded (no voice kind, no id).
    fn spliced_row(id: &str) -> Value {
        let mut r = row(id, "mixed", "coll_wiki", "wiki", "local_positive", Some("gen_a"), None, None, None);
        r["cohort_id"] = json!(SPLICED);
        r["voice_kind"] = Value::Null;
        r
    }

    fn style(id: &str, kind: &str, fam: &str, rev: &str, origin: &str, source: Value) -> Value {
        json!({"id": id, "type": kind, "family": fam, "reviewer_family": rev, "origin": origin, "source": source, "text": format!("synthetic style text {id}")})
    }

    fn voices() -> Vec<Value> {
        vec![style("v_inv_a", "voice", "fam_a", "fam_a", "invented", Value::Null), style("v_inv_b", "voice", "fam_b", "fam_a", "invented", Value::Null)]
    }

    fn briefs() -> Vec<Value> {
        vec![
            style("b_rev_b", "brief", "fam_a", "fam_b", "invented", Value::Null),
            style("b_hd", "brief", "fam_a", "fam_a", "human_derived", json!({"domain": "wiki", "split": "train_mining", "doc_id": "synthetic"})),
        ]
    }

    struct Fx {
        _dir: tempfile::TempDir,
        root: PathBuf,
        rules: PathBuf,
        lib: PathBuf,
        rows: PathBuf,
        stage0_pending: PathBuf,
        stage0_done: PathBuf,
    }

    fn items(list: &[&str]) -> Value {
        json!(list)
    }

    fn rules_value(root: &Path) -> Value {
        let basis = root.join("basis.md");
        let evidence = root.join("evidence.md");
        let derived = root.join("derived.jsonl");
        let lib = root.join("lib");
        let mut catalogue = Map::new();
        for i in 1..=8 {
            let answer = if i == 5 { json!({"recorded": "synthetic answer"}) } else { Value::Null };
            catalogue.insert(format!("J{i}"), json!({"title": format!("synthetic item {i}"), "answer": answer}));
        }
        let library: Vec<Value> = ["voices.accepted.jsonl", "briefs.accepted.jsonl"]
            .iter()
            .map(|f| json!({"label": format!("synthetic {f}"), "path": lib.join(f), "sha256": file_sha(&lib.join(f)).unwrap()}))
            .collect();
        json!({
            "schema": RULES_SCHEMA,
            "basis": [{"label": "synthetic review", "path": basis, "sha256": file_sha(&basis).unwrap(), "section": "7"}],
            "items": catalogue,
            "every_row": items(&["J6"]),
            "collections": {
                "coll_web": {"licence": "synthetic-a", "items": items(&["J1"]), "note": PRIVATE_NOTE, "public_note": PUBLIC_NOTE},
                "coll_wiki": {"licence": "synthetic-b", "items": items(&["J3"])},
                "coll_essay": {"items": items(&["J3", "J4"])},
                "coll_gov": {"items": items(&["J5"])},
                "coll_bench": {"items": items(&["J1"]), "by_domain": {"bench_books": items(&["J3"])}},
                "coll_unused": {"items": items(&[])},
                ENRON_COLLECTION: {"items": items(&[ENRON_ITEM])},
            },
            "generators": {"gen_a": {"items": items(&[]), "note": PRIVATE_NOTE}, "gen_b": {"items": items(&["J2"])}, "gen_f": {"items": items(&[])}},
            "source_kinds": {"human": items(&[]), "local_positive": items(&[]), "frontier": items(&["J2"]), "external_pool": items(&[])},
            "style_libraries": library,
            "style_families": {"fam_a": items(&[]), "fam_b": items(&["J2"])},
            "voice_kinds": {"voice": "recorded", "brief": "recorded", "operation_only": "none", NULL_KEY: "none"},
            "unlisted_style_origins": {"pilot_seed": items(&[])},
            "population_roles": {"train": items(&[]), "checkpoint_metric": items(&["J7"]), "selection": items(&["J7"]), "calibration": items(&["J7"]), "mock": items(&["J7"])},
            "derived_labels": {"labels_x": {"path": derived, "sha256": file_sha(&derived).unwrap(), "items": items(&["J2"]), "note": PRIVATE_NOTE}},
            "model_inits": {"init_a": {"licence": "synthetic-c", "items": items(&[]), "note": PRIVATE_NOTE}},
            "reproduce": {
                "filter": JITEM_FILTER_V1, "population": "train0",
                "basis": {"label": "synthetic evidence", "path": evidence, "sha256": file_sha(&evidence).unwrap(), "section": "3.2"},
                "kept": {"human_only": 2, "mixed": 2, "model_only": 1}, "kept_total": 5,
                "dropped_positives_by_first_reason": {"j1": 1, "j2_generator": 2, "j4": 1, "j2_style_library_only": 2},
            },
        })
    }

    fn fixture() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write(&root.join("basis.md"), b"synthetic rights review\n");
        write(&root.join("evidence.md"), b"synthetic evidence\n");
        write(&root.join("derived.jsonl"), b"{\"id\":\"synthetic\"}\n");
        let lib = root.join("lib");
        jsonl(&lib.join("voices.accepted.jsonl"), &voices());
        jsonl(&lib.join("briefs.accepted.jsonl"), &briefs());
        let rows_p = root.join("train/rows.jsonl");
        jsonl(&rows_p, &rows());
        let rules = root.join("rules.json");
        write(&rules, &serde_json::to_vec_pretty(&rules_value(&root)).unwrap());
        let stage0_pending = root.join("stage0-pending.json");
        write(&stage0_pending, br#"{"items":{"A3":{"status":"pending","fixes":"synthetic"}}}"#);
        let stage0_done = root.join("stage0-done.json");
        write(
            &stage0_done,
            br#"{"items":{"A3":{"status":"done","utc":"2026-01-01T00:00:00Z","finding":{"third_party_output_terms":"synthetic finding text","mentions_terms_of_use_or_output_restrictions":false}}}}"#,
        );
        Fx { _dir: dir, root, rules, lib, rows: rows_p, stage0_pending, stage0_done }
    }

    fn input(fx: &Fx) -> SnapshotInput {
        SnapshotInput {
            version: Version::V1,
            rules: fx.rules.clone(),
            populations: vec![PopulationSpec::parse(&format!("train0:train={}", fx.rows.display())).unwrap()],
            pools: vec![],
            stage0: fx.stage0_pending.clone(),
            allowlist: vec!["train0".into()],
            trainer_bundle: None,
            require_final: false,
            commit: "synthetic".into(),
            executable_sha256: None,
        }
    }

    const V2_POPULATIONS: [(&str, &str); 5] = [("train0", "train"), ("dev2", "checkpoint_metric"), ("sel2", "selection"), ("cal2", "calibration"), ("mock2", "mock")];

    /// A final version-2 input over the fixture rows (every role reads the same synthetic file).
    fn input_v2(fx: &Fx) -> SnapshotInput {
        let mut inp = input(fx);
        inp.version = Version::V2;
        inp.stage0 = fx.stage0_done.clone();
        for (n, r) in &V2_POPULATIONS[1..] {
            inp.populations.push(PopulationSpec::parse(&format!("{n}:{r}={}", fx.rows.display())).unwrap());
        }
        inp
    }

    fn a3_digest(fx: &Fx) -> String {
        stage0_record(&fx.stage0_done, "A3").unwrap().2
    }

    fn v2_rules(fx: &Fx, v: &mut Value) {
        v["populations"] = json!(V2_POPULATIONS.iter().map(|(n, r)| json!({"name": n, "role": r, "splits": ["train"]})).collect::<Vec<_>>());
        v["row_schemas"] = json!([V1_ROW_SCHEMA]);
        v["findings_checked"] = json!({"A3": a3_digest(fx)});
    }

    fn set_rules(fx: &Fx, f: impl FnOnce(&mut Value)) {
        let mut v = rules_value(&fx.root);
        f(&mut v);
        fs::write(&fx.rules, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }

    /// The full error chain of a refused build.
    fn refused(inp: &SnapshotInput) -> String {
        format!("{:#}", build_snapshot(inp).err().expect("the build is refused"))
    }

    fn tag_lines(o: &SnapshotOutput) -> BTreeMap<String, Value> {
        std::str::from_utf8(&o.row_tags)
            .unwrap()
            .lines()
            .map(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                (v["id"].as_str().unwrap().to_string(), v)
            })
            .collect()
    }

    #[test]
    fn rights_filter_keeps_rows_free_of_j1_j2_j4_and_orders_drop_reasons() {
        let fx = fixture();
        let o = build_snapshot(&input(&fx)).unwrap();
        let f = &o.snapshot["filters"][JITEM_FILTER_V1]["populations"]["train0"];
        assert_eq!(f["kept"], json!({"human_only": 2, "mixed": 2, "model_only": 1}));
        assert_eq!(f["kept_total"], 5);
        let d = &f["dropped_by_first_reason"];
        assert_eq!(d["j1"], json!({"human_only": 2, "mixed": 1, "model_only": 0}));
        assert_eq!(d["j2_generator"], json!({"human_only": 0, "mixed": 1, "model_only": 1}));
        assert_eq!(d["j4"], json!({"human_only": 1, "mixed": 1, "model_only": 0}));
        assert_eq!(d["j2_style_library_only"], json!({"human_only": 0, "mixed": 1, "model_only": 1}));
        assert_eq!(o.snapshot["reproduce"]["reproduced"], true);
        assert_eq!(o.snapshot["reproduce"]["observed"]["kept_total"], 5);
        let t = tag_lines(&o);
        let verdict = |id: &str| t[id][JITEM_FILTER_V1].as_str().unwrap().to_string();
        assert_eq!(verdict("m_essay_style"), "j4", "J4 precedes J2 through the style library");
        assert_eq!(verdict("f_wiki"), "j2_generator", "J2 by source kind is not style-only");
        assert_eq!(verdict("p_pilot"), "kept", "a listed unlisted-style origin with no items");
        assert_eq!(verdict("m_wiki_rev"), "j2_style_library_only", "a J2 reviewer family reaches the row");
        // A human-derived brief brings its source collection's items; a domain rule adds items.
        assert_eq!(t["m_gov_hd"]["items"], json!(["J3", "J5", "J6"]));
        assert_eq!(t["m_gov_hd"]["via"]["J3"], json!(["style"]));
        assert_eq!(t["h_bench"]["items"], json!(["J1", "J3", "J6"]));
        assert!(t.values().all(|v| v["via"]["J6"] == json!(["every_row"])));
        let allow: Value = serde_json::from_slice(&o.allowlists[0].1).unwrap();
        assert_eq!(allow["ids"], json!(["h_gov", "h_wiki", "m_gov_hd", "m_wiki_a", "p_pilot"]));
        assert_eq!(allow["ids_sha256"], sha_hex(b"h_gov\nh_wiki\nm_gov_hd\nm_wiki_a\np_pilot\n"));
        // Declared entities are listed even without rows; the Enron collection carries J8.
        let colls = o.snapshot["collections"].as_array().unwrap();
        let enron = colls.iter().find(|c| c["collection"] == ENRON_COLLECTION).unwrap();
        assert_eq!(enron["items"], json!([ENRON_ITEM]));
        assert!(colls.iter().any(|c| c["collection"] == "coll_unused" && c["rows"] == json!({})));
        assert_eq!(o.snapshot["style_source_collections"], json!({"wiki": "coll_wiki"}));
        assert_eq!(o.snapshot["derived_labels"][0]["tags_rows"], false);
        let p = &o.snapshot["populations"][0];
        assert_eq!(p["voice_kinds"], json!({"brief": 2, "null": 4, "operation_only": 4, "voice": 4}));
        assert_eq!(p["style_references"], json!({"listed": 5, "unlisted_by_origin": {"pilot_seed": 1}, "unrecorded": 0}));
        assert_eq!(o.snapshot["row_schemas"], json!([V1_ROW_SCHEMA]));
        assert_eq!(o.snapshot["style_libraries"].as_array().unwrap().len(), 2);
        assert_eq!(o.snapshot["status"], "draft");
        let text = String::from_utf8(serde_json::to_vec(&o.snapshot).unwrap()).unwrap() + std::str::from_utf8(&o.row_tags).unwrap();
        assert!(!text.contains("Synthetic text") && !text.contains("synthetic style text"), "outputs are text-free");
    }

    #[test]
    fn rights_v1_must_reproduce_every_recorded_count() {
        let fx = fixture();
        set_rules(&fx, |v| {
            v["reproduce"]["kept"]["mixed"] = json!(3);
            v["reproduce"]["kept_total"] = json!(6);
        });
        assert!(refused(&input(&fx)).contains("does not reproduce"));
        set_rules(&fx, |v| v["reproduce"]["dropped_positives_by_first_reason"]["j4"] = json!(0));
        assert!(refused(&input(&fx)).contains("does not reproduce"));
        set_rules(&fx, |v| v.as_object_mut().unwrap().remove("reproduce").map(drop).unwrap());
        assert!(refused(&input(&fx)).contains("reproduce counts"));
        // An incomplete or empty count map is refused, never passed vacuously.
        set_rules(&fx, |v| {
            v["reproduce"]["kept"].as_object_mut().unwrap().remove("mixed");
            v["reproduce"]["kept_total"] = json!(3);
        });
        assert!(format!("{:#}", Rules::load(&fx.rules).unwrap_err()).contains("exactly the origins"));
        set_rules(&fx, |v| v["reproduce"]["dropped_positives_by_first_reason"].as_object_mut().unwrap().remove("j4").map(drop).unwrap());
        assert!(format!("{:#}", Rules::load(&fx.rules).unwrap_err()).contains("exactly the reasons"));
        set_rules(&fx, |v| {
            v["reproduce"]["kept"] = json!({});
            v["reproduce"]["kept_total"] = json!(0);
            v["reproduce"]["dropped_positives_by_first_reason"] = json!({});
        });
        assert!(Rules::load(&fx.rules).is_err());
        set_rules(&fx, |v| v["reproduce"]["kept_total"] = json!(4));
        assert!(format!("{:#}", Rules::load(&fx.rules).unwrap_err()).contains("kept_total"));
        set_rules(&fx, |v| v["reproduce"].as_object_mut().unwrap().remove("kept_total").map(drop).unwrap());
        assert!(Rules::load(&fx.rules).is_err());
    }

    #[test]
    fn rights_refuses_unruled_collections_generators_kinds_families_and_style_ids() {
        let fx = fixture();
        let with = |extra: Value| {
            let mut r = rows();
            r.push(extra);
            jsonl(&fx.rows, &r);
            refused(&input(&fx))
        };
        assert!(with(row("x", "human_only", "coll_new", "new", "human", None, None, None, None)).contains("no rule for collection coll_new"));
        assert!(with(row("x", "mixed", "coll_wiki", "wiki", "local_positive", Some("gen_new"), None, None, None)).contains("no rule for generator gen_new"));
        assert!(with(row("x", "mixed", "coll_wiki", "wiki", "local_positive", Some("gen_a"), Some("v_missing"), None, Some("invented"))).contains("is in no library"));
        assert!(with(row("x", "mixed", "coll_wiki", "wiki", "hosted_capture", Some("gen_a"), None, None, None)).contains("no rule for source kind hosted_capture"));
        let mut combo = row("x", "mixed", "coll_wiki", "wiki", "local_positive", Some("gen_a"), Some("v_inv_a"), None, Some("invented"));
        combo["voice_kind"] = json!("persona_combo");
        assert!(with(combo).contains("no rule for voice kind persona_combo"));
        jsonl(&fx.rows, &rows());
        set_rules(&fx, |v| v["style_families"].as_object_mut().unwrap().remove("fam_b").map(drop).unwrap());
        assert!(refused(&input(&fx)).contains("no rule for style family fam_b"));
        set_rules(&fx, |v| v["generators"]["gen_a"]["items"] = json!(["J9"]));
        assert!(refused(&input(&fx)).contains("not in the catalogue"));
    }

    #[test]
    fn rights_style_lineage_is_never_read_as_style_free() {
        let fx = fixture();
        let with = |extra: Vec<Value>| {
            let mut r = rows();
            r.extend(extra);
            jsonl(&fx.rows, &r);
        };
        // A style-conditioned row without its id, and an id on a row with no style item.
        let mut no_id = row("x", "mixed", "coll_wiki", "wiki", "local_positive", Some("gen_a"), None, None, None);
        no_id["voice_kind"] = json!("voice");
        with(vec![no_id]);
        assert!(refused(&input(&fx)).contains("style lineage unknown"));
        let mut stray = row("x", "mixed", "coll_wiki", "wiki", "local_positive", Some("gen_a"), Some("v_inv_a"), None, Some("invented"));
        stray["voice_kind"] = json!("operation_only");
        with(vec![stray]);
        assert!(refused(&input(&fx)).contains("carries no style item"));
        // A cohort whose ids were never recorded carries the rules' items through `style_unrecorded`.
        with(vec![spliced_row("s1"), spliced_row("s2")]);
        set_rules(&fx, |v| {
            v["unrecorded_style_cohorts"] = json!({SPLICED: ["J2"]});
            v["reproduce"]["dropped_positives_by_first_reason"]["j2_style_library_only"] = json!(4);
        });
        let o = build_snapshot(&input(&fx)).unwrap();
        let t = tag_lines(&o);
        assert_eq!(t["s1"]["via"]["J2"], json!([VIA_STYLE_UNRECORDED]));
        assert_eq!(t["s1"][JITEM_FILTER_V1], "j2_style_library_only");
        assert_eq!(o.snapshot["populations"][0]["style_references"]["unrecorded"], 2);
        let mut named = spliced_row("s3");
        named["voice_id"] = json!("v_inv_a");
        with(vec![spliced_row("s1"), spliced_row("s2"), named]);
        assert!(refused(&input(&fx)).contains("never recorded"));
        set_rules(&fx, |v| v["unrecorded_style_cohorts"] = json!({SPLICED: []}));
        assert!(format!("{:#}", Rules::load(&fx.rules).unwrap_err()).contains("needs the items"));
    }

    #[test]
    fn rights_train_population_holds_only_unique_train_rows() {
        let fx = fixture();
        let mut r = rows();
        r[1]["split"] = json!("dev_iter");
        jsonl(&fx.rows, &r);
        assert!(refused(&input(&fx)).contains("is in split dev_iter"));
        let mut r = rows();
        r[0]["split"] = json!("test");
        jsonl(&fx.rows, &r);
        assert!(refused(&input(&fx)).contains("sealed split test"));
        let mut r = rows();
        r.push(rows()[0].clone());
        jsonl(&fx.rows, &r);
        assert!(refused(&input(&fx)).contains("row id h_web occurs twice"));
    }

    #[test]
    fn rights_v1_holds_train_populations_and_reproduces_an_input() {
        let fx = fixture();
        let mut inp = input(&fx);
        inp.populations.push(PopulationSpec::parse(&format!("sel:selection={}", fx.rows.display())).unwrap());
        assert!(refused(&inp).contains("Train populations only"));
        let mut inp = input(&fx);
        inp.populations = vec![PopulationSpec::parse(&format!("other:train={}", fx.rows.display())).unwrap()];
        inp.allowlist = vec!["other".into()];
        assert!(refused(&inp).contains("reproduce names population train0, which is not an input"));
        let mut inp = input(&fx);
        inp.allowlist = vec!["missing".into()];
        assert!(refused(&inp).contains("not an input"));
    }

    #[test]
    fn rights_v1_pins_its_row_schema() {
        assert_eq!(V1_ROW_SCHEMA, "slop_ninja_a_bundle_row_v1");
        let fx = fixture();
        let later = "slop_ninja_a_bundle_row_v9";
        let mut r = rows();
        r[0]["schema"] = json!(later);
        jsonl(&fx.rows, &r);
        assert!(refused(&input(&fx)).contains(&format!("schema {later}")));
        set_rules(&fx, |v| v["row_schemas"] = json!([later]));
        assert!(refused(&input(&fx)).contains("rows only"));
        // Version 2 reads the schemas its rules list.
        set_rules(&fx, |v| {
            v2_rules(&fx, v);
            v["row_schemas"] = json!([V1_ROW_SCHEMA, later]);
        });
        build_snapshot(&input_v2(&fx)).unwrap();
    }

    #[test]
    fn rights_style_library_ids_and_source_domains_are_unambiguous() {
        let fx = fixture();
        let mut b = briefs();
        b.push(style("v_inv_a", "voice", "fam_b", "fam_a", "invented", Value::Null));
        jsonl(&fx.lib.join("briefs.accepted.jsonl"), &b);
        set_rules(&fx, |_| {});
        assert!(refused(&input(&fx)).contains("listed twice with different attributes"));
        let mut b = briefs();
        b.push(voices()[0].clone());
        jsonl(&fx.lib.join("briefs.accepted.jsonl"), &b);
        set_rules(&fx, |_| {});
        assert_eq!(build_snapshot(&input(&fx)).unwrap().snapshot["style_duplicate_ids"], 1);
        jsonl(&fx.lib.join("briefs.accepted.jsonl"), &briefs());
        // Two human collections carry the human-derived brief's domain.
        let mut r = rows();
        r.push(row("h_gov_wiki", "human_only", "coll_gov", "wiki", "human", None, None, None, None));
        jsonl(&fx.rows, &r);
        set_rules(&fx, |v| {
            v["reproduce"]["kept"]["human_only"] = json!(3);
            v["reproduce"]["kept_total"] = json!(6);
        });
        assert!(refused(&input(&fx)).contains("matches 2 human collections"));
        set_rules(&fx, |v| {
            v["reproduce"]["kept"]["human_only"] = json!(3);
            v["reproduce"]["kept_total"] = json!(6);
            v["style_source_domains"] = json!({"wiki": "coll_wiki"});
        });
        let o = build_snapshot(&input(&fx)).unwrap();
        assert_eq!(o.snapshot["style_source_collections"], json!({"wiki": "coll_wiki"}));
    }

    #[test]
    fn rights_enron_rows_and_pools_carry_j8() {
        let fx = fixture();
        set_rules(&fx, |v| v["collections"][ENRON_COLLECTION]["items"] = json!([]));
        assert!(Rules::load(&fx.rules).err().unwrap().to_string().contains(ENRON_ITEM));
        set_rules(&fx, |v| v["collections"].as_object_mut().unwrap().remove(ENRON_COLLECTION).map(drop).unwrap());
        assert!(Rules::load(&fx.rules).is_err());
        set_rules(&fx, |_| {});
        let pool = fx.root.join("pool/metadata.jsonl");
        let meta = |i: usize| json!({"id": format!("synthetic-{i}"), "collection": ENRON_COLLECTION, "domain": "email", "word_count": 10});
        jsonl(&pool, &(0..4).map(meta).collect::<Vec<_>>());
        let s = tag_pool(&fx.rules, &pool, Some(ENRON_ITEM)).unwrap();
        assert_eq!(s["rows_with_expected_item"], 4);
        assert_eq!(s["enron_rows_with_item"], 4);
        assert_eq!(s["collections"][ENRON_COLLECTION]["items"], json!({"J6": 4, ENRON_ITEM: 4}));
        // The snapshot binds the pool and its J8 count.
        let mut inp = input(&fx);
        inp.pools = vec![PoolSpec::parse(&format!("enron:{ENRON_ITEM}={}", pool.display())).unwrap()];
        let o = build_snapshot(&inp).unwrap();
        assert_eq!(o.snapshot["pools"][0]["name"], "enron");
        assert_eq!(o.snapshot["pools"][0]["rows"]["sha256"], file_sha(&pool).unwrap());
        assert_eq!(o.snapshot["pools"][0]["rows_with_expected_item"], 4);
        // An empty pool proves nothing.
        write(&pool, b"");
        assert!(format!("{:#}", tag_pool(&fx.rules, &pool, Some(ENRON_ITEM)).unwrap_err()).contains("holds no pool rows"));
        assert!(refused(&inp).contains("holds no pool rows"));
        let mut mixed: Vec<Value> = (0..2).map(meta).collect();
        mixed.push(json!({"collection": "coll_gov"}));
        jsonl(&pool, &mixed);
        assert!(tag_pool(&fx.rules, &pool, Some(ENRON_ITEM)).err().unwrap().to_string().contains("2 of 3"));
        jsonl(&pool, &[json!({"collection": "coll_other"})]);
        assert!(tag_pool(&fx.rules, &pool, None).is_err());
        // An Enron bundle row carries J8 through its collection.
        let mut extra = rows();
        extra.push(row("e1", "human_only", ENRON_COLLECTION, "email", "human", None, None, None, None));
        jsonl(&fx.rows, &extra);
        set_rules(&fx, |v| {
            v["reproduce"]["kept"]["human_only"] = json!(3);
            v["reproduce"]["kept_total"] = json!(6);
        });
        let o = build_snapshot(&input(&fx)).unwrap();
        assert_eq!(tag_lines(&o)["e1"]["items"], json!(["J6", ENRON_ITEM]));
    }

    #[test]
    fn rights_bound_files_are_rehashed() {
        let fx = fixture();
        fs::write(fx.root.join("basis.md"), b"changed\n").unwrap();
        assert!(Rules::load(&fx.rules).err().unwrap().to_string().contains("not the recorded"));
        let fx = fixture();
        fs::write(fx.root.join("derived.jsonl"), b"changed\n").unwrap();
        assert!(Rules::load(&fx.rules).is_err());
        let fx = fixture();
        fs::write(fx.root.join("evidence.md"), b"changed\n").unwrap();
        assert!(Rules::load(&fx.rules).is_err());
        // The style library is bound by the rules, not chosen by the caller.
        let fx = fixture();
        let mut v = voices();
        v.push(style("v_new", "voice", "fam_a", "fam_a", "invented", Value::Null));
        jsonl(&fx.lib.join("voices.accepted.jsonl"), &v);
        assert!(refused(&input(&fx)).contains("not the recorded"));
        set_rules(&fx, |v| v["style_libraries"] = json!([]));
        assert!(Rules::load(&fx.rules).err().unwrap().to_string().contains("no style library"));
    }

    #[test]
    fn rights_snapshot_is_final_only_when_a3_is_done_and_bound() {
        let fx = fixture();
        let mut inp = input(&fx);
        inp.require_final = true;
        assert!(refused(&inp).contains("waits"));
        inp.stage0 = fx.stage0_done.clone();
        assert!(refused(&inp).contains("findings_checked.A3"));
        inp.require_final = false;
        let draft = build_snapshot(&inp).unwrap();
        assert_eq!(draft.snapshot["status"], "draft");
        assert_eq!(draft.snapshot["findings"]["A3"]["checked_by_rules"], false);
        set_rules(&fx, |v| v["findings_checked"] = json!({"A3": "0".repeat(64)}));
        assert!(refused(&inp).contains("were checked against A3 record"));
        let digest = a3_digest(&fx);
        set_rules(&fx, |v| v["findings_checked"] = json!({"A3": digest}));
        inp.require_final = true;
        let o = build_snapshot(&inp).unwrap();
        assert_eq!(o.snapshot["status"], "final");
        assert_eq!(o.snapshot["findings"]["A3"]["record_sha256"], digest.as_str());
        assert_eq!(o.snapshot["findings"]["A3"]["record"]["finding"]["third_party_output_terms"], "synthetic finding text");
        assert_eq!(o.snapshot["stage0"]["sha256"], file_sha(&fx.stage0_done).unwrap());
        // The record digest ignores key order.
        let rec: Value = serde_json::from_str(r#"{"utc":"2026-01-01T00:00:00Z","finding":{"mentions_terms_of_use_or_output_restrictions":false,"third_party_output_terms":"synthetic finding text"},"status":"done"}"#).unwrap();
        assert_eq!(record_sha256(&rec), digest);
    }

    /// A trainer bundle over the fixture rows: the Train block, then `tail` lines.
    fn trainer(fx: &Fx, train: &[Value], tail: &[&str], manifest_train: usize) -> PathBuf {
        let dir = fx.root.join("trainer");
        let _ = fs::remove_dir_all(&dir);
        let mut sorted: Vec<&Value> = train.iter().collect();
        sorted.sort_by_key(|r| r["id"].as_str().unwrap().to_string());
        let mut b = Vec::new();
        for r in sorted {
            let t = json!({"id": r["id"], "split": "train", "text": r["text"], "text_sha256": r["text_sha256"], "origin": r["origin"]});
            serde_json::to_writer(&mut b, &t).unwrap();
            b.push(b'\n');
        }
        for l in tail {
            b.extend_from_slice(l.as_bytes());
            b.push(b'\n');
        }
        write(&dir.join("rows.jsonl"), &b);
        let m = json!({
            "schema": TRAINER_MANIFEST_SCHEMA, "smoke_only": false,
            "rows_by_split": {"train": manifest_train, "dev_iter": 1, "dev_select": 1},
            "source_splits": {"train": {"rows_sha256": file_sha(&fx.rows).unwrap()}},
            "files": {"rows.jsonl": file_sha(&dir.join("rows.jsonl")).unwrap()},
        });
        write(&dir.join("MANIFEST.json"), &serde_json::to_vec_pretty(&m).unwrap());
        dir
    }

    #[test]
    fn rights_trainer_check_reads_only_the_train_block() {
        let fx = fixture();
        // Lines after the Train block are never parsed: these would fail if they were.
        let tail = [r#"{"id":"dev-1","split":"dev_iter"}"#, "not json"];
        let mut inp = input(&fx);
        inp.trainer_bundle = Some(trainer(&fx, &rows(), &tail, 14));
        let o = build_snapshot(&inp).unwrap();
        let c = &o.snapshot["trainer_check"];
        assert_eq!(c["train_rows"], 14);
        assert_eq!(c["train_block"]["lines"], 14);
        assert_eq!(c["allowlisted_present"], 5);
        assert_eq!(c["population_rows_not_in_trainer"], 0);
        assert_eq!(c["manifest"]["train_source_sha256"], file_sha(&fx.rows).unwrap());
        // A Train block shorter than the MANIFEST says is refused at its first non-Train line.
        let dev = format!(r#"{{"id":"dev-1","split":"dev_iter","text_sha256":"{}"}}"#, sha_hex(b"x"));
        inp.trainer_bundle = Some(trainer(&fx, &rows(), &[dev.as_str()], 15));
        assert!(refused(&inp).contains("shorter than the MANIFEST"));
        inp.trainer_bundle = Some(trainer(&fx, &rows(), &[], 15));
        assert!(refused(&inp).contains("end after 14 of 15"));
        // A missing allow-listed id and a differing text digest are refused.
        let short: Vec<Value> = rows().into_iter().filter(|r| r["id"] != "h_wiki").collect();
        inp.trainer_bundle = Some(trainer(&fx, &short, &tail, 13));
        assert!(refused(&inp).contains("1 ids missing"));
        let mut bad = rows();
        bad[1]["text_sha256"] = json!(sha_hex(b"other"));
        inp.trainer_bundle = Some(trainer(&fx, &bad, &tail, 14));
        assert!(refused(&inp).contains("1 text digests differ"));
        // The MANIFEST must name this population as its Train source, and not be a smoke bundle.
        let dir = trainer(&fx, &rows(), &tail, 14);
        let mut m: Value = serde_json::from_slice(&fs::read(dir.join("MANIFEST.json")).unwrap()).unwrap();
        m["source_splits"]["train"]["rows_sha256"] = json!(sha_hex(b"elsewhere"));
        write(&dir.join("MANIFEST.json"), &serde_json::to_vec(&m).unwrap());
        inp.trainer_bundle = Some(dir.clone());
        assert!(refused(&inp).contains("not the population's"));
        m["source_splits"]["train"]["rows_sha256"] = json!(file_sha(&fx.rows).unwrap());
        m["smoke_only"] = json!(true);
        write(&dir.join("MANIFEST.json"), &serde_json::to_vec(&m).unwrap());
        assert!(refused(&inp).contains("smoke bundle"));
    }

    #[test]
    fn rights_v2_holds_exactly_the_rules_populations_and_no_sealed_split() {
        let fx = fixture();
        assert!(refused(&input_v2(&fx)).contains("population list"));
        set_rules(&fx, |v| v2_rules(&fx, v));
        let v2 = build_snapshot(&input_v2(&fx)).unwrap();
        assert_eq!(v2.snapshot["schema"], SNAPSHOT_V2_SCHEMA);
        assert_eq!(v2.snapshot["status"], "final");
        let mut inp = input_v2(&fx);
        inp.populations.pop();
        assert!(refused(&inp).contains("needs a mock population"));
        // Every listed population is required, and no other.
        set_rules(&fx, |v| {
            v2_rules(&fx, v);
            v["populations"].as_array_mut().unwrap().push(json!({"name": "span2", "role": "selection", "splits": ["train"]}));
        });
        assert!(refused(&input_v2(&fx)).contains("require exactly"));
        set_rules(&fx, |v| v2_rules(&fx, v));
        let mut inp = input_v2(&fx);
        inp.populations.push(PopulationSpec::parse(&format!("extra:selection={}", fx.rows.display())).unwrap());
        assert!(refused(&inp).contains("require exactly"));
        // An allow-list is for a Train population.
        let mut inp = input_v2(&fx);
        inp.allowlist = vec!["sel2".into()];
        assert!(refused(&inp).contains("allow-lists are for Train populations"));
        // Non-Train roles: no sealed split, only the listed splits, and the bound digest.
        let sel = fx.root.join("sel/rows.jsonl");
        let with_split = |split: &str| {
            let mut r = rows();
            r[3]["split"] = json!(split);
            jsonl(&sel, &r);
            let mut inp = input_v2(&fx);
            inp.populations[2] = PopulationSpec::parse(&format!("sel2:selection={}", sel.display())).unwrap();
            refused(&inp)
        };
        assert!(with_split("test").contains("sealed split test"));
        assert!(with_split("test2").contains("sealed split test2"));
        assert!(with_split("dev_select").contains("which the rules do not list"));
        set_rules(&fx, |v| {
            v2_rules(&fx, v);
            v["populations"][1]["splits"] = json!(["train", "test"]);
        });
        assert!(format!("{:#}", Rules::load(&fx.rules).unwrap_err()).contains("sealed split test"));
        set_rules(&fx, |v| {
            v2_rules(&fx, v);
            v["populations"][1]["sha256"] = json!(sha_hex(b"other"));
        });
        assert!(refused(&input_v2(&fx)).contains("not the rules'"));
    }

    #[test]
    fn rights_data_rights_needs_a_final_v2_and_carries_counts_only() {
        let fx = fixture();
        let mut inp = input(&fx);
        inp.stage0 = fx.stage0_done.clone();
        let v1 = build_snapshot(&inp).unwrap();
        assert!(data_rights(&v1.snapshot, "x").err().unwrap().to_string().contains("item 1.7"));
        set_rules(&fx, |v| v2_rules(&fx, v));
        let v2 = build_snapshot(&input_v2(&fx)).unwrap();
        let mut draft = v2.snapshot.clone();
        draft["status"] = json!("draft");
        assert!(data_rights(&draft, "x").is_err());
        let d = data_rights(&v2.snapshot, "abc").unwrap();
        let c = &d["content"];
        assert_eq!(d["content_sha256"], sha_hex(&serde_json::to_vec(c).unwrap()));
        assert_eq!(c["schema"], DATA_RIGHTS_SCHEMA);
        // J5 has a recorded answer; J8 is declared but no row carries it.
        assert_eq!(c["open_items"], json!(["J1", "J2", "J3", "J4", "J6", "J7"]));
        assert_eq!(c["open_markers"], json!([]));
        assert_eq!(c["release"], "blocked_by_open_items");
        assert!(c["sources"].as_array().unwrap().iter().all(|s| s["collection"] != "coll_unused"));
        let web = c["sources"].as_array().unwrap().iter().find(|s| s["collection"] == "coll_web").unwrap();
        assert_eq!(web["public_note"], PUBLIC_NOTE);
        let mut keys: Vec<&str> = c["style_library"]["files"][0].as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["rows", "sha256"]);
        assert_eq!(c["findings"]["A3"]["record_sha256"], a3_digest(&fx).as_str());
        assert_eq!(c["findings"]["A3"]["flags"], json!({"mentions_terms_of_use_or_output_restrictions": false}));
        let text = serde_json::to_string(&d).unwrap();
        let root = fx.root.to_str().unwrap().to_string();
        for leak in ["Synthetic text", "synthetic style text", "h_wiki", "m_gov_hd", "v_inv_a", PRIVATE_NOTE, "synthetic finding text", "synthetic review", root.as_str()] {
            assert!(!text.contains(leak), "DATA_RIGHTS carries {leak}");
        }
        // Rows of a cohort whose style lineage was never recorded keep DATA_RIGHTS open.
        let mut r = rows();
        r.push(spliced_row("s1"));
        jsonl(&fx.rows, &r);
        set_rules(&fx, |v| {
            v2_rules(&fx, v);
            v.as_object_mut().unwrap().remove("reproduce");
            v["unrecorded_style_cohorts"] = json!({SPLICED: ["J3"]});
        });
        let d = data_rights(&build_snapshot(&input_v2(&fx)).unwrap().snapshot, "abc").unwrap();
        assert_eq!(d["content"]["open_markers"], json!([MARKER_STYLE_LINEAGE_UNRECORDED]));
        assert_eq!(d["content"]["style_lineage_unrecorded_rows"], 5);
    }

    #[test]
    fn rights_written_snapshot_is_private_deterministic_written_once_and_whole() {
        use std::os::unix::fs::PermissionsExt;
        let fx = fixture();
        let o = build_snapshot(&input(&fx)).unwrap();
        let (a, b) = (fx.root.join("out-a"), fx.root.join("out-b"));
        let sa = write_snapshot(&a, &o).unwrap();
        let sb = write_snapshot(&b, &build_snapshot(&input(&fx)).unwrap()).unwrap();
        assert_eq!(sa, sb);
        assert_eq!(file_sha(&a.join(Version::V1.file_name())).unwrap(), sa);
        assert_eq!(file_sha(&a.join(ROW_TAGS_FILE)).unwrap(), o.snapshot["row_tags"]["sha256"].as_str().unwrap());
        assert_eq!(fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o700);
        for f in [Version::V1.file_name().to_string(), ROW_TAGS_FILE.to_string(), allowlist_file("train0")] {
            assert_eq!(fs::metadata(a.join(&f)).unwrap().permissions().mode() & 0o777, 0o600, "{f}");
        }
        assert!(write_snapshot(&a, &o).is_err());
        let empty = fx.root.join("out-empty");
        fs::create_dir(&empty).unwrap();
        assert!(write_snapshot(&empty, &o).is_err(), "an existing directory is never replaced");
        // A failure part-way leaves neither the directory nor a partial copy.
        let broken = SnapshotOutput {
            version: Version::V1,
            snapshot: o.snapshot.clone(),
            row_tags: o.row_tags.clone(),
            allowlists: vec![("ok.json".into(), b"{}".to_vec()), ("no-such-dir/x.json".into(), b"{}".to_vec())],
        };
        let c = fx.root.join("out-c");
        assert!(write_snapshot(&c, &broken).is_err());
        assert!(!c.exists());
        let left: Vec<String> = fs::read_dir(&fx.root).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(left.iter().all(|n| !n.contains("out-c")), "{left:?}");
    }

    #[test]
    fn rights_population_and_pool_specs_parse_and_reject_unsafe_names() {
        let p = PopulationSpec::parse("r0_train:train=/x/rows.jsonl").unwrap();
        assert_eq!((p.name.as_str(), p.role.as_str(), p.rows.as_path()), ("r0_train", "train", Path::new("/x/rows.jsonl")));
        assert!(PopulationSpec::parse("a:test=/x").is_err());
        assert!(PopulationSpec::parse("a=/x").is_err());
        for bad in ["../a:train=/x", ".a:train=/x", "a/b:train=/x", ":train=/x"] {
            assert!(PopulationSpec::parse(bad).is_err(), "{bad}");
        }
        let q = PoolSpec::parse(&format!("enron:{ENRON_ITEM}=/x/metadata.jsonl")).unwrap();
        assert_eq!((q.name.as_str(), q.expect_item.as_deref()), ("enron", Some(ENRON_ITEM)));
        assert_eq!(PoolSpec::parse("p=/x").unwrap().expect_item, None);
        for bad in ["p:=/x", "p=", "a/b=/x"] {
            assert!(PoolSpec::parse(bad).is_err(), "{bad}");
        }
    }
}

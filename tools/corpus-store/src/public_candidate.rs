//! `public-candidate-v1`: a release candidate selected from the store's
//! records by provenance, reservation, holdout status, admission and rights.
//!
//! Selection never matches rows by path substring. A source counts as a
//! published snapshot only when its id is listed in the rules and its
//! canonical root equals `<repo_root>/datasets/<name>` exactly; every other
//! source root must lie under a listed corpus root (component-wise prefix
//! after canonicalisation) or its records are refused. Licence and basis come
//! from the populated record fields; attribution, licence URL, share-alike,
//! notes and `redistribute_text` are rebuilt from the archived source line
//! (file digest and line number, checked against the line digest). The store's
//! `rights` and `exposure` tables are not read. Model-generated texts inherit
//! the rights of every ancestor through `parent_text_sha256`.
//!
//! Every exported object is built from allowlists, then a denylist gate scans
//! the finished rows; any hit fails the export and its location goes to a
//! private log outside the export directory. The store is opened read-only.

use crate::archive::{self, BlobCache};
use crate::export::Out;
use crate::skeleton;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = "slop_ninja_public_candidate_v1";
pub const RULES_SCHEMA: &str = "slop_ninja_public_candidate_rules_v1";

type H = [u8; 32];

fn hx(s: &str) -> Option<H> {
    if s.len() != 64 {
        return None;
    }
    let mut b = [0u8; 32];
    hex::decode_to_slice(s, &mut b).ok()?;
    Some(b)
}

fn hs(h: &H) -> String {
    hex::encode(h)
}

fn sha_hex(b: &[u8]) -> String {
    hex::encode(skeleton::sha256(b))
}

// ---------------------------------------------------------------- rules

#[derive(Deserialize, Clone, Debug)]
pub struct LicenceRule {
    pub recorded: String,
    #[serde(default)]
    pub prefix: bool,
    /// public_domain | cc0 | cc_by | cc_by_sa | uspto_pre2019 | excluded
    pub class: String,
    #[serde(default)]
    pub licence_id: Option<String>,
    pub decision: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Reservation {
    /// A path-free name for the MANIFEST (paths of reservation files stay private).
    pub label: String,
    pub path: String,
    pub sha256: String,
    /// rows_manifest (JSON with `rows[].text_sha256/source_group`) | view_rows (JSONL rows)
    pub kind: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct GeneratorRule {
    pub key: String,
    /// Lowercase identity fragments; the first rule with a matching fragment wins.
    pub patterns: Vec<String>,
    pub family: String,
    pub display: String,
    pub kind: String,
    pub provider: String,
    #[serde(default)]
    pub hosted: bool,
    pub licence_id: String,
    pub notice_files: Vec<String>,
    #[serde(default)]
    pub checkpoint: Option<Value>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct PinnedFile {
    /// Path under the release (for example `LICENSES/sources/CC-BY-4.0.txt`).
    pub dest: String,
    /// Source path relative to the repository root.
    pub src: String,
    pub sha256: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct LicenceId {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    pub file: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct FlagOverride {
    pub decision: String,
    pub collections: Vec<String>,
}

/// A private JSON file whose strings (the array at `pointer`) must never appear in the release.
/// The strings are compared in memory and never printed or logged.
#[derive(Deserialize, Clone, Debug)]
pub struct PrivateStrings {
    pub path: String,
    pub pointer: String,
}

/// Public names that replace internal campaign, experiment, view-kind and role names.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct PublicNames {
    #[serde(default)]
    pub contexts: BTreeMap<String, String>,
    #[serde(default)]
    pub experiments: BTreeMap<String, String>,
    #[serde(default)]
    pub view_kinds: BTreeMap<String, String>,
    #[serde(default)]
    pub roles: BTreeMap<String, String>,
}

impl PublicNames {
    fn get<'b>(m: &'b BTreeMap<String, String>, k: &'b str) -> &'b str {
        m.get(k).map(String::as_str).unwrap_or(k)
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct Rules {
    pub schema: String,
    pub release: String,
    #[serde(default)]
    pub decisions: Value,
    pub private_sources: Vec<String>,
    pub hard_private_sources: Vec<String>,
    pub hard_private_record_prefixes: Vec<String>,
    pub private_adapters: Vec<String>,
    pub private_roles: Vec<String>,
    pub private_contexts: Vec<String>,
    pub private_roots: Vec<String>,
    pub corpus_roots: Vec<String>,
    pub published_sources: BTreeMap<String, String>,
    pub reservations: Vec<Reservation>,
    pub reservation_globs: Vec<String>,
    pub splits_experiment_prefix: String,
    pub used_roles: Vec<String>,
    pub sealed_roles: Vec<String>,
    pub licences: Vec<LicenceRule>,
    pub excluded_collections: BTreeMap<String, String>,
    pub flag_override: FlagOverride,
    pub record_roles: Vec<String>,
    pub skip_roles: Vec<String>,
    pub split_document_sources: Vec<String>,
    pub templates: Vec<String>,
    pub embed_full_templates: Vec<String>,
    pub embed_span_templates: Vec<String>,
    pub embed_prefix_templates: Vec<String>,
    pub generators: Vec<GeneratorRule>,
    pub licence_ids: BTreeMap<String, LicenceId>,
    pub files: Vec<PinnedFile>,
    #[serde(default)]
    pub collection_credits: BTreeMap<String, String>,
    #[serde(default)]
    pub experiment_descriptions: BTreeMap<String, String>,
    pub forbidden_substrings: Vec<String>,
    #[serde(default)]
    pub operator_values: Vec<String>,
    #[serde(default)]
    pub operator_value_files: Vec<String>,
    pub max_ancestor_depth: usize,
    pub uspto_cutoff: String,
    /// Private strings (user-authored seed examples and the like): any voice, prompt or style
    /// field containing one is withheld, and the gate fails on any occurrence in any field.
    #[serde(default)]
    pub private_string_files: Vec<PrivateStrings>,
    /// Voice-library origins that are never published (for example hand-written seed voices).
    #[serde(default)]
    pub withheld_voice_origins: Vec<String>,
    /// Style-library inventor labels (the local model alias that wrote a voice or brief) to roster keys.
    #[serde(default)]
    pub inventor_aliases: BTreeMap<String, String>,
    #[serde(default)]
    pub public_names: PublicNames,
}

/// The private strings named by the rules, lowercased. A listed file that is missing or yields no
/// string fails the export; the values are never printed.
pub(crate) fn load_private_strings(rules: &Rules, repo: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for f in &rules.private_string_files {
        let p = canon(repo, &f.path);
        let b = fs::read(&p).with_context(|| format!("private string file {}", f.path))?;
        let v: Value = serde_json::from_slice(&b).with_context(|| format!("private string file {} is not JSON", f.path))?;
        let a = v.pointer(&f.pointer).and_then(|x| x.as_array()).with_context(|| format!("private string file {}: no array at {}", f.path, f.pointer))?;
        let n = out.len();
        out.extend(a.iter().filter_map(|x| x.as_str()).map(|s| s.trim().to_lowercase()).filter(|s| s.len() >= 4));
        ensure!(out.len() > n, "private string file {} yields no strings", f.path);
    }
    Ok(out)
}

fn has_private(private: &[String], s: &str) -> bool {
    if private.is_empty() {
        return false;
    }
    let l = s.to_lowercase();
    private.iter().any(|p| l.contains(p.as_str()))
}

pub fn load_rules(path: &Path) -> Result<(Rules, String)> {
    let b = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let r: Rules = serde_json::from_slice(&b).context("parse rules")?;
    ensure!(r.schema == RULES_SCHEMA, "rules schema {} (expected {RULES_SCHEMA})", r.schema);
    Ok((r, sha_hex(&b)))
}

// ---------------------------------------------------------------- paths

/// Canonical form of `p` (relative paths resolve against `base`); a path that
/// does not exist is normalised lexically.
fn canon(base: &Path, p: &str) -> PathBuf {
    let raw = if Path::new(p).is_absolute() { PathBuf::from(p) } else { base.join(p) };
    fs::canonicalize(&raw).unwrap_or_else(|_| {
        let mut out = PathBuf::new();
        for c in raw.components() {
            match c {
                std::path::Component::ParentDir => {
                    out.pop();
                }
                std::path::Component::CurDir => {}
                x => out.push(x.as_os_str()),
            }
        }
        out
    })
}

/// Component-wise prefix test (`/a/b` is under `/a`, `/a/bc` is not).
fn under(p: &Path, root: &Path) -> bool {
    p.starts_with(root)
}

#[derive(Clone, Debug, Default)]
struct Src {
    /// Records of this source count as provenance.
    prov: bool,
    /// Every text of this source is refused (private root, unlisted root or hard-private source).
    refuse: bool,
    /// Listed published snapshot whose root is exactly `<repo>/datasets/<name>`.
    published: Option<String>,
    private_source: bool,
}

/// Classify every source by id, adapter and anchored root.
fn classify_sources(rows: &[(String, String, String)], repo: &Path, rules: &Rules) -> Result<HashMap<String, Src>> {
    let private_roots: Vec<PathBuf> = rules.private_roots.iter().map(|p| canon(repo, p)).collect();
    let corpus_roots: Vec<PathBuf> = rules.corpus_roots.iter().map(|p| canon(repo, p)).collect();
    let datasets = canon(repo, "datasets");
    let mut out = HashMap::new();
    for (id, adapter, root) in rows {
        let r = canon(repo, root);
        let mut s = Src::default();
        s.private_source = rules.private_sources.contains(id) || rules.hard_private_sources.contains(id);
        let in_private_root = private_roots.iter().any(|p| under(&r, p));
        let in_corpus = corpus_roots.iter().any(|p| under(&r, p)) || under(&r, &datasets);
        s.refuse = in_private_root || !in_corpus || rules.hard_private_sources.contains(id);
        if let Some(name) = rules.published_sources.get(id) {
            // Exact anchored equality, never a substring test.
            ensure!(!name.contains('/') && !name.is_empty() && name != "..", "published source {id}: bad name {name}");
            if r == datasets.join(name) && !s.refuse {
                s.published = Some(name.clone());
            }
        }
        s.prov = !s.private_source && !s.refuse && !rules.private_adapters.contains(adapter);
        out.insert(id.clone(), s);
    }
    Ok(out)
}

// ---------------------------------------------------------------- licences

const CLASS_ORDER: [&str; 5] = ["public_domain", "cc0", "uspto_pre2019", "cc_by", "cc_by_sa"];

fn class_rank(c: &str) -> u8 {
    match c {
        "public_domain" | "cc0" | "uspto_pre2019" => 0,
        "cc_by" => 1,
        "cc_by_sa" => 2,
        _ => 100,
    }
}

fn licence_rule(rules: &Rules, lic: &str) -> Option<usize> {
    rules.licences.iter().position(|r| if r.prefix { lic.starts_with(&r.recorded) } else { lic == r.recorded })
}

// ---------------------------------------------------------------- per-text aggregates

#[derive(Clone, Default)]
struct TA {
    prov: u32,
    hard_private: bool,
    published: bool,
    lic_mask: u64,
    lic_unknown: bool,
    excl_coll: Option<u16>,
    is_gen: bool,
    hosted: bool,
    raid: bool,
    withdrawn: bool,
    reddit: bool,
    reserved: bool,
    used_view: bool,
    sealed: bool,
    splits_roles: u8,
    split: u8,
    eo: bool,
    geo: bool,
    pangram: bool,
}

const SR_TRAIN: u8 = 1;
const SR_MINING: u8 = 2;
const SR_HELD: u8 = 4;
const SR_RESERVED: u8 = 8;
const SR_DIAG: u8 = 16;
const SR_OTHER: u8 = 32;

fn splits_roles_label(m: u8) -> String {
    if m == 0 {
        return "no_split_role".into();
    }
    let mut v = Vec::new();
    for (b, n) in [(SR_TRAIN, "train"), (SR_MINING, "train_mining"), (SR_HELD, "held"), (SR_RESERVED, "reserved_unassigned"), (SR_DIAG, "diagnostic"), (SR_OTHER, "other")] {
        if m & b != 0 {
            v.push(n);
        }
    }
    v.join("+")
}

fn kind_of(t: &TA) -> &'static str {
    if t.raid {
        "raid"
    } else if t.hosted {
        "hosted"
    } else if t.is_gen {
        "open"
    } else {
        "human"
    }
}

fn text_kind(k: &str) -> &'static str {
    match k {
        "hosted" => "hosted_generation",
        "open" => "open_generation",
        "raid" => "raid",
        _ => "human",
    }
}

/// Interned small strings (split names).
#[derive(Default)]
struct Intern {
    v: Vec<String>,
    m: HashMap<String, u8>,
}
impl Intern {
    fn id(&mut self, s: &str) -> u8 {
        if let Some(&i) = self.m.get(s) {
            return i;
        }
        self.v.push(s.into());
        let i = (self.v.len()) as u8;
        self.m.insert(s.into(), i);
        i
    }
    fn get(&self, i: u8) -> Option<&str> {
        if i == 0 { None } else { self.v.get(i as usize - 1).map(String::as_str) }
    }
}

// ---------------------------------------------------------------- full records

#[derive(Clone, Default, Debug)]
struct Rec {
    file: String,
    line_no: i64,
    role: String,
    line_sha: String,
    source_id: String,
    record_id: Option<String>,
    dataset: Option<String>,
    collection: Option<String>,
    domain: Option<String>,
    genre: Option<String>,
    source_work: Option<String>,
    family: Option<String>,
    source_group: Option<String>,
    writer_key: Option<String>,
    original_doc_id: Option<String>,
    doc_date: Option<String>,
    composition_date: Option<String>,
    licence: Option<String>,
    cohort_id: Option<String>,
    origin: Option<String>,
    share: Option<f64>,
    ai_byte_spans: Option<String>,
    generator_label: Option<String>,
    generator_key: Option<String>,
    operation: Option<String>,
    recipe_sha256: Option<String>,
    parent_record_id: Option<String>,
    parent_text_sha256: Option<String>,
    split_hint: Option<String>,
    extra: Option<Value>,
}

/// What the export needs from one archived source line.
#[derive(Clone, Default, Debug)]
struct LineInfo {
    whole_file: bool,
    flag: Option<bool>,
    collection: Option<String>,
    licence: Option<String>,
    licence_url: Option<String>,
    attribution: Option<String>,
    share_alike: Option<Value>,
    notes: Option<String>,
    evidence_url: Option<String>,
    conditions: Option<Value>,
    catalog_statement: Option<String>,
    jurisdiction: Option<String>,
    title: Option<String>,
    authors: Option<Value>,
    source_url: Option<String>,
    history_url: Option<String>,
    filing_date: Option<String>,
    publication_date: Option<String>,
    provenance: Option<String>,
    generation: Option<Value>,
    /// Identity strings of the generation block (model revision, generator, model id) for generator resolution.
    gen_ids: Vec<String>,
}

// ---------------------------------------------------------------- sanitising

/// Keys never copied from archived lines, parameters or extras.
const DENY_KEYS: &[&str] = &[
    "argv", "codex_argv", "cli_versions", "request_log", "call_log", "call_log_model", "transport_config", "transport_config_sha256",
    "service_tier", "inference_geo", "fallback_credit", "quota", "quota_file", "cost", "cost_units", "cost_usd", "costUSD",
    "cost_estimate", "cost_basis", "total_cost_usd", "budget", "credit", "credits", "remaining", "price", "balance",
    "receipt", "receipt_sha256", "receipt_file_sha256", "events_sha256", "stderr_sha256", "stdout_sha256", "task_id",
    "session_id", "message_id", "response_id", "job_id", "job_ref", "bulk_id", "obs_key", "plan_dir", "receipt_dir", "dir",
    "library", "local_path", "model_path", "runtime_path", "runtime_libraries", "path", "paths", "raw_path", "file_path",
    "headers_path", "transfer_receipt_path", "metadata_path", "text_path", "text_relative_path", "intent_line", "endpoint",
    "base_url", "response_path", "evidence", "evidence_paths", "rights_basis", "admission", "admission_decision",
    "admission_basis", "training_admission", "pool_admission", "exposure", "exposure_caveats", "selection_row",
    "raw_download", "primary_page_evidence", "catalog_evidence", "writer_demographics", "attempt_history", "host",
    "account", "org", "organization", "project", "workspace", "pool", "route", "routing", "operator", "operator_notes",
    "private_notes", "resume", "handoff", "scratch_implementation_sha256", "producer_sha256", "invocation_sha256",
    "collector_sha256", "run_manifest_sha256", "usage", "model_usage", "published_file", "retained_original_file",
    "response_file_sha256", "test_binding", "skill_bundle_sha256", "commercial_training_admitted",
];

fn deny_key(k: &str) -> bool {
    let kl = k.to_ascii_lowercase();
    DENY_KEYS.contains(&k)
        || kl.ends_with("_path")
        || kl.ends_with("_dir")
        || kl.ends_with("producer_sha256")
        || kl.contains("admission_manifest")
        || kl.contains("source_manifest")
        || kl.contains("cost")
        || kl.contains("quota")
        || kl.contains("balance")
        || kl.contains("proxy")
        || kl.contains("receipt")
        || kl.starts_with("price")
}

fn hostish(s: &str) -> bool {
    s.starts_with('/') || s.starts_with("~/") || s.contains("/home/") || s.contains("/Users/") || s.starts_with("file://") || s.contains("127.0.0.1") || s.contains("localhost:")
}

/// Recursive copy without denied keys and without host-path strings.
fn clean(v: &Value) -> Option<Value> {
    match v {
        Value::Object(m) => {
            let mut o = Map::new();
            for (k, x) in m {
                if deny_key(k) {
                    continue;
                }
                if let Some(y) = clean(x) {
                    o.insert(k.clone(), y);
                }
            }
            Some(Value::Object(o))
        }
        Value::Array(a) => Some(Value::Array(a.iter().filter_map(clean).collect())),
        Value::String(s) if hostish(s) => None,
        x => Some(x.clone()),
    }
}

fn pick(v: &Value, keys: &[&str]) -> Map<String, Value> {
    let mut o = Map::new();
    if let Value::Object(m) = v {
        for k in keys {
            if let Some(x) = m.get(*k) {
                if !x.is_null() {
                    if let Some(y) = clean(x) {
                        o.insert((*k).into(), y);
                    }
                }
            }
        }
    }
    o
}

/// Rights fields copied from archived lines. The free-text `basis` is not among them: it records
/// internal use rationales and decision references; the licence id and its file state the basis.
const RIGHTS_KEYS: &[&str] = &[
    "license", "license_url", "attribution", "share_alike", "redistribute_text", "commercial_training", "evidence_url",
    "evidence_sha256", "conditions", "catalog_statement", "jurisdiction", "notes", "permission_url", "terms_recorded",
    "licence_history", "licence_version_stated", "licence_statement_sha256", "source_acknowledgement",
];
const PROV_TOP_KEYS: &[&str] = &[
    "provenance", "date", "cutoff", "extraction", "aliases", "source_group_policy", "publication", "share_evidence", "share_target",
    "collection", "domain", "genre", "evidence_notes",
];
const SOURCE_KEYS: &[&str] = &["url", "published_at", "collection", "extraction", "version", "author_ids", "byte_span", "published_source"];
const SOURCE_RECORD_KEYS: &[&str] = &[
    "title", "author", "author_key", "catalog_author", "catalog_url", "pg_id", "dates", "source_url", "underlying_work_key", "source_group",
    "source",
];

fn rights_obj(v: &Value) -> Map<String, Value> {
    let mut o = pick(v, RIGHTS_KEYS);
    if let Some(src) = v.get("source") {
        if src.is_object() {
            let s = pick(src, RIGHTS_KEYS);
            if !s.is_empty() {
                o.insert("source".into(), Value::Object(s));
            }
        }
    }
    o
}

/// Sanitised provenance of an archived line (no text, allowlisted keys only).
fn provenance_of(v: &Value) -> Value {
    let mut o = pick(v, PROV_TOP_KEYS);
    if let Some(p) = o.get_mut("provenance") {
        if let Value::Object(m) = p {
            m.remove("generation");
        }
    }
    if let Some(r) = v.get("rights") {
        let ro = rights_obj(r);
        if !ro.is_empty() {
            o.insert("rights".into(), Value::Object(ro));
        }
    }
    if let Some(src) = v.get("source").filter(|x| x.is_object()) {
        let mut so = pick(src, SOURCE_KEYS);
        if let Some(rec) = src.get("record").filter(|x| x.is_object()) {
            let mut ro = pick(rec, SOURCE_RECORD_KEYS);
            if let Some(r) = rec.get("rights") {
                let rr = rights_obj(r);
                if !rr.is_empty() {
                    ro.insert("rights".into(), Value::Object(rr));
                }
            }
            if !ro.is_empty() {
                so.insert("record".into(), Value::Object(ro));
            }
        }
        if let Some(r) = src.get("rights") {
            let rr = rights_obj(r);
            if !rr.is_empty() {
                so.insert("rights".into(), Value::Object(rr));
            }
        }
        if !so.is_empty() {
            o.insert("source".into(), Value::Object(so));
        }
    }
    Value::Object(o)
}

fn as_flag(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn st(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn line_info(v: &Value) -> LineInfo {
    let r = v.get("rights").filter(|x| x.is_object());
    let rs = r.and_then(|x| x.get("source")).filter(|x| x.is_object());
    let src = v.get("source").filter(|x| x.is_object());
    let srec = src.and_then(|x| x.get("record")).filter(|x| x.is_object());
    let srr = srec.and_then(|x| x.get("rights")).or_else(|| src.and_then(|x| x.get("rights"))).filter(|x| x.is_object());
    let rights: Vec<&Value> = [r, rs, srr].into_iter().flatten().collect();
    let first = |k: &str| rights.iter().find_map(|x| x.get(k).filter(|y| !y.is_null()));
    let prov = v.get("provenance").filter(|x| x.is_object()).or(srec);
    let pf = |k: &str| prov.and_then(|p| p.get(k)).filter(|y| !y.is_null());
    let mut li = LineInfo {
        flag: as_flag(r.and_then(|x| x.get("redistribute_text")))
            .or_else(|| as_flag(rs.and_then(|x| x.get("redistribute_text"))))
            .or_else(|| as_flag(srr.and_then(|x| x.get("redistribute_text")))),
        collection: st(v.get("collection")),
        licence: st(first("license")),
        licence_url: st(first("license_url")),
        attribution: st(first("attribution")),
        share_alike: first("share_alike").and_then(clean),
        notes: st(first("notes")),
        evidence_url: st(first("evidence_url")),
        conditions: first("conditions").and_then(clean),
        catalog_statement: st(first("catalog_statement")),
        jurisdiction: st(first("jurisdiction")),
        title: st(pf("title")).or_else(|| st(v.get("title"))),
        authors: pf("authors").or_else(|| pf("catalog_author")).or_else(|| pf("author")).or_else(|| pf("owner_display_name")).and_then(clean),
        source_url: st(pf("source_url")).or_else(|| st(src.and_then(|s| s.get("url")))).or_else(|| st(first("evidence_url"))),
        history_url: st(pf("history_url")),
        filing_date: st(pf("filing_date")),
        publication_date: st(pf("publication_date")),
        provenance: None,
        generation: None,
        gen_ids: Vec::new(),
        whole_file: false,
    };
    if li.source_url.as_deref().is_some_and(hostish) {
        li.source_url = None;
    }
    li.provenance = Some(provenance_of(v).to_string());
    li
}

/// The generation block of a generation record line, built from an allowlist (the prompt is kept
/// under `prompt` for the prompt checks and removed before publication), and the identity strings
/// used to resolve its generator.
fn generation_of(v: &Value) -> Option<(Value, Vec<String>)> {
    let g = v.get("generation").filter(|x| x.is_object())?;
    let mut ids = Vec::new();
    if let Some(mut c) = clean(g) {
        if let Some(m) = c.as_object_mut() {
            for k in ["request", "response", "raw_response", "messages", "attempt_history", "metadata", "events", "stream"] {
                m.remove(k);
            }
        }
        for k in ["model_revision", "generator", "model_id"] {
            if let Some(s) = find_key(&c, k).and_then(|v| v.as_str()) {
                ids.push(s.to_string());
            }
        }
    }
    let mut o = allow(g, GENERATION);
    if let Some(r) = g.get("runtime").and_then(|x| x.as_str()) {
        o.insert("runtime".into(), runtime_struct(r));
    }
    if let Some(Value::String(p)) = g.get("prompt") {
        o.insert("prompt".into(), json!(p));
    }
    Some((Value::Object(o), ids))
}

// ---------------------------------------------------------------- allowlists

/// An allowlist node: a scalar (or array of scalars) leaf, or an object with its own allowlist.
#[derive(Clone, Copy)]
enum A {
    Leaf,
    Obj(&'static [(&'static str, A)]),
}
use A::{Leaf, Obj};

fn leaf(x: &Value) -> Option<Value> {
    let scalar = |e: &Value| match e {
        Value::Null | Value::Object(_) | Value::Array(_) => None,
        Value::String(s) if hostish(s) => None,
        e => Some(e.clone()),
    };
    match x {
        Value::Array(a) => Some(Value::Array(a.iter().filter_map(scalar).collect())),
        x => scalar(x),
    }
}

/// Copy only the allowlisted keys of `v` (recursively); objects in leaf positions (such as
/// externalised `{"$len", "$sha256"}` placeholders) are dropped.
fn allow(v: &Value, spec: &[(&str, A)]) -> Map<String, Value> {
    let mut o = Map::new();
    let Value::Object(m) = v else { return o };
    for (k, a) in spec {
        let Some(x) = m.get(*k) else { continue };
        let y = match a {
            Leaf => leaf(x),
            Obj(s) => Some(allow(x, s)).filter(|r| !r.is_empty()).map(Value::Object),
        };
        if let Some(y) = y {
            o.insert((*k).into(), y);
        }
    }
    o
}

const PROFILE_SOURCE: &[(&str, A)] =
    &[("url", Leaf), ("revision", Leaf), ("scope", Leaf), ("license", Leaf), ("license_sha256", Leaf), ("upstream_sha256", Leaf)];
const PROMPT_PROFILE: &[(&str, A)] = &[
    ("profile", Obj(&[("id", Leaf), ("instructions", Leaf), ("instructions_sha256", Leaf), ("source", Obj(PROFILE_SOURCE))])),
    ("selection", Obj(&[("set_id", Leaf), ("assignment_method", Leaf), ("selected_profile_ids", Leaf), ("guard", Leaf), ("guard_sha256", Leaf), ("catalog_sha256", Leaf)])),
    ("assignment", Obj(&[("split", Leaf), ("family_rank", Leaf), ("bucket_offset", Leaf), ("bucket_family_count", Leaf), ("collections", Leaf), ("input_sha256", Leaf), ("source_group_sha256", Leaf)])),
];
/// Published generation-record fields (`runtime` is parsed separately; `prompt` is checked separately).
const GENERATION: &[(&str, A)] = &[
    ("operation", Leaf), ("model_id", Leaf), ("model_revision", Leaf), ("model_license", Leaf), ("model_license_url", Leaf),
    ("model_license_sha256", Leaf), ("quantization", Leaf), ("temperature", Leaf), ("top_p", Leaf), ("max_tokens", Leaf), ("seed", Leaf),
    ("finish_reason", Leaf), ("created_at", Leaf), ("request_sha256", Leaf), ("response_sha256", Leaf), ("response_model", Leaf),
    ("provider", Leaf), ("model_requested", Leaf), ("model_reported", Leaf), ("style_name", Leaf), ("style_rationale", Leaf),
    ("style_compliance", Leaf), ("meaning_preservation", Leaf), ("instruction_style", Leaf), ("response_document_sha256", Leaf),
    ("prompt_profile", Obj(PROMPT_PROFILE)),
];
const GENERATOR_CONFIG: &[(&str, A)] = &[
    ("schema", Leaf), ("identity", Leaf), ("repo", Leaf), ("revision", Leaf), ("filename", Leaf), ("alias", Leaf), ("allowed_splits", Leaf),
    ("rights", Obj(&[("model_license", Leaf), ("generation_permitted", Leaf), ("model_weights_redistributed", Leaf)])),
];
const INSTRUCTION_MATRIX: &[(&str, A)] = &[
    ("styles", Leaf), ("selection", Leaf), ("recipe", Leaf), ("quality_limit", Leaf), ("prompt_checks", Leaf), ("prompt_policy", Leaf),
    ("seed", Leaf), ("replicates_per_source_model_extent", Leaf), ("requested_source_span_percentages", Leaf),
];
/// Published controlled-generation binding fields (`runtime` is parsed separately).
const BINDING: &[(&str, A)] = &[
    ("schema", Leaf), ("input_schema", Leaf), ("input_sha256", Leaf), ("generator", Leaf), ("seed", Leaf), ("temperature", Leaf),
    ("top_p", Leaf), ("max_tokens", Leaf), ("share_word_policy", Leaf), ("span_policy", Leaf), ("failures", Leaf), ("recipe", Leaf),
    ("scratch_policy", Leaf), ("rewrite_policy", Leaf), ("test_policy", Leaf), ("test_opened", Leaf), ("requested_span_percentages", Leaf),
    ("generator_config", Obj(GENERATOR_CONFIG)), ("instruction_matrix", Obj(INSTRUCTION_MATRIX)), ("revision", Leaf), ("repo", Leaf),
];
const DECODING_PROFILE: &[(&str, A)] =
    &[("profile", Leaf), ("temperature", Leaf), ("top_p", Leaf), ("top_k", Leaf), ("min_p", Leaf), ("repeat_penalty", Leaf), ("max_tokens", Leaf)];
const VOICE_LADDER: &[(&str, A)] = &[("ladder_id", Leaf), ("rung", Leaf), ("size", Leaf)];
const VOICE_AXES: &[(&str, A)] = &[
    ("axis", Leaf), ("era", Leaf), ("imitated_author", Leaf), ("inventor", Leaf), ("inventor_family", Leaf), ("persona_axis", Leaf),
    ("region", Leaf), ("style_name", Leaf),
];
/// Published style fields of a local generation. Source families and base-item ids are added
/// separately, and only when they name a released source group or record.
const STYLE: &[(&str, A)] = &[
    ("style_policy", Leaf), ("style_kind", Leaf), ("wrapper", Leaf), ("operation", Leaf), ("style_first", Leaf), ("default_voice", Leaf),
    ("document_type", Leaf), ("document_phrase", Leaf), ("native_document_type", Leaf), ("voice_id", Leaf), ("voice_axis", Leaf),
    ("voice_cluster", Leaf), ("voice_origin", Leaf), ("voice_partition", Leaf), ("voice_inventor", Leaf), ("voice_text", Leaf),
    ("voice_imitated_author", Leaf), ("style_text", Leaf), ("brief_id", Leaf), ("brief_axis", Leaf), ("brief_cluster", Leaf),
    ("brief_genre", Leaf), ("brief_origin", Leaf), ("brief_partition", Leaf), ("brief_inventor", Leaf), ("brief_text", Leaf),
    ("brief_imitated_author", Leaf), ("ladder_id", Leaf), ("ladder_rung", Leaf), ("cross_id", Leaf), ("cross_base_generator", Leaf),
    ("modifier", Obj(&[("type", Leaf), ("text", Leaf)])), ("setting", Leaf), ("mood", Leaf), ("humanize_variant", Leaf),
    ("spelling_variant", Leaf),
];

/// A recorded runtime description reduced to structured fields: runtime name and version, package
/// versions, allowlisted settings, upstream package file digests and a coarse platform. Host
/// hardware, OS builds, local ports, cache and lock-file notes, local script digests and operator
/// notes are dropped.
pub(crate) fn runtime_struct(s: &str) -> Value {
    let is_ver = |v: &str| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.') && v.chars().next().is_some_and(|c| c.is_ascii_digit());
    let is_hex = |v: &str| v.len() == 64 && v.bytes().all(|c| c.is_ascii_hexdigit());
    let mut o = Map::new();
    if let Some(rest) = s.trim().strip_prefix("llama.cpp-") {
        o.insert("name".into(), json!("llama.cpp"));
        let mut parts = rest.split('-');
        if let Some(b) = parts.next().filter(|b| b.starts_with('b') && is_ver(&b[1..])) {
            o.insert("build".into(), json!(b));
        }
        if let Some(be) = parts.next() {
            if let Some(v) = be.strip_prefix("CUDA").filter(|v| is_ver(v)) {
                o.insert("backend".into(), json!(format!("CUDA {v}")));
            }
        }
        return Value::Object(o);
    }
    let mut packages = Map::new();
    let mut settings: Vec<String> = Vec::new();
    let mut files = Map::new();
    let mut platform = false;
    const SETTINGS: &[&str] = &[
        "decode_concurrency=", "prompt_concurrency=", "prompt_cache_size=", "checkpoint context default ", "no truncation",
        "fix_mistral_regex=", "trust_remote_code=", "prompt cache disabled", "decode/prompt concurrency ", "request token cap", "HF offline",
    ];
    for seg in s.split(';').map(str::trim).filter(|x| !x.is_empty()) {
        if seg.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<Value>(seg) {
                if let Some(m) = v.get("packages").and_then(|p| p.as_object()) {
                    for (k, x) in m {
                        if let Some(ver) = x.as_str().filter(|v| is_ver(v)) {
                            packages.insert(k.to_ascii_lowercase(), json!(ver));
                        }
                    }
                }
                if let Some(ver) = v.get("python").and_then(|x| x.as_str()).filter(|v| is_ver(v)) {
                    packages.insert("python".into(), json!(ver));
                }
            }
        } else if let Some(r) = seg.strip_prefix("native ") {
            match r.rsplit_once(' ') {
                Some((n, v)) if is_ver(v) => {
                    o.insert("name".into(), json!(n));
                    o.insert("version".into(), json!(v));
                }
                _ if r.len() <= 32 && r.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-') => {
                    o.insert("name".into(), json!(r));
                }
                _ => {}
            }
        } else if let Some(v) = seg.strip_prefix("MLX/MLX-Metal ").filter(|v| is_ver(v)) {
            packages.insert("mlx".into(), json!(v));
            packages.insert("mlx-metal".into(), json!(v));
        } else if let Some((n, v)) = seg.split_once(' ').filter(|(n, v)| matches!(*n, "Python" | "Transformers" | "tokenizers") && is_ver(v)) {
            packages.insert(n.to_ascii_lowercase(), json!(v));
        } else if SETTINGS.iter().any(|p| seg.starts_with(p)) && seg.len() <= 48 && seg.chars().all(|c| c.is_ascii_alphanumeric() || " =_/".contains(c)) {
            if !settings.iter().any(|x| x == seg) {
                settings.push(seg.to_string());
            }
        } else if let Some((f, h)) = seg.split_once(" SHA256=").filter(|(f, h)| f.starts_with("mlx_lm/") && is_hex(h)) {
            files.insert(f.to_string(), json!(h));
        } else if let Some(h) = seg.strip_prefix("server_sha256=").filter(|h| is_hex(h)) {
            files.insert("mlx_lm/server.py".into(), json!(h));
        } else if seg.starts_with("Apple M") || seg.starts_with("macOS") || seg == "arm64" {
            platform = true;
        }
    }
    if !packages.is_empty() {
        o.insert("packages".into(), Value::Object(packages));
    }
    if !settings.is_empty() {
        o.insert("settings".into(), json!(settings));
    }
    if !files.is_empty() {
        o.insert("upstream_file_sha256".into(), Value::Object(files));
    }
    if platform {
        o.insert("platform".into(), json!("macOS arm64 (Apple silicon)"));
    }
    Value::Object(o)
}

/// Whether any string inside `v` contains a private string.
fn value_has_private(private: &[String], v: &Value) -> bool {
    match v {
        Value::String(s) => has_private(private, s),
        Value::Array(a) => a.iter().any(|x| value_has_private(private, x)),
        Value::Object(m) => m.values().any(|x| value_has_private(private, x)),
        _ => false,
    }
}

// ---------------------------------------------------------------- voices and styles

/// Whether a voice-library entry is withheld: a listed private origin (hand-written seed
/// voices) or a private string in any of its fields.
pub(crate) fn voice_withheld(rules: &Rules, v: &Value, private: &[String]) -> bool {
    let origin = v.get("origin").and_then(|x| x.as_str()).unwrap_or("");
    rules.withheld_voice_origins.iter().any(|o| o == origin) || value_has_private(private, v)
}

/// The roster index of a style-library inventor label (exact alias, never a substring match).
pub(crate) fn inventor(rules: &Rules, label: &str) -> Option<usize> {
    let key = rules.inventor_aliases.get(label)?;
    rules.generators.iter().position(|g| &g.key == key)
}

/// The inventor of a voice: `None` for a hand-written voice; an error when a model-written
/// voice names no resolvable inventor (its notice could not be attached).
pub(crate) fn voice_inventor(rules: &Rules, v: &Value) -> Result<Option<usize>> {
    let origin = v.get("origin").and_then(|x| x.as_str()).unwrap_or("");
    let label = v.get("axes").and_then(|a| a.get("inventor")).and_then(|x| x.as_str());
    match label {
        Some(l) => Ok(Some(inventor(rules, l).with_context(|| format!("voice inventor label {l} has no roster alias"))?)),
        None if origin.contains("hand_written") => Ok(None),
        None => bail!("model-written voice {} names no inventor", v.get("id").and_then(|x| x.as_str()).unwrap_or("?")),
    }
}

/// The published style block of a local generation: allowlisted fields; source families and
/// base-item ids only when they name a released source group or record; private strings
/// withheld; inventor labels resolved to roster keys with their notices.
#[allow(clippy::too_many_arguments)]
pub(crate) fn style_of(
    rules: &Rules, x: &Value, private: &[String], groups: &HashSet<String>, record_ids: &HashSet<String>, inventors: &mut BTreeSet<usize>,
    counts: &mut Counter,
) -> Result<Option<Value>> {
    if !x.is_object() {
        return Ok(None);
    }
    let mut o = allow(x, STYLE);
    for k in ["voice_source_family", "brief_source_family"] {
        if let Some(v) = x.get(k) {
            match v.as_str() {
                Some(f) if groups.contains(f) => {
                    o.insert(k.into(), json!(f));
                }
                _ => counts.add(format!("style_field_withheld:{k}:not_released"), 1),
            }
        }
    }
    for k in ["cross_base_item", "ladder_base_item"] {
        if let Some(v) = x.get(k) {
            match v.as_str() {
                Some(id) if record_ids.contains(id) => {
                    o.insert(k.into(), json!(id));
                }
                _ => counts.add(format!("style_field_withheld:{k}:not_released"), 1),
            }
        }
    }
    let hit: Vec<String> = o.iter().filter(|(_, v)| value_has_private(private, v)).map(|(k, _)| k.clone()).collect();
    if !hit.is_empty() {
        for k in &hit {
            o.remove(k);
        }
        o.insert("text_withheld".into(), json!("private_seed_voice"));
        counts.add("style_text_withheld:private_seed_voice", 1);
    }
    let mut notice: BTreeSet<String> = BTreeSet::new();
    for (k, out) in [("voice_inventor", "voice_inventor_generator"), ("brief_inventor", "brief_inventor_generator"), ("cross_base_generator", "cross_base_generator_key")] {
        if let Some(l) = x.get(k).and_then(|v| v.as_str()) {
            let i = inventor(rules, l).with_context(|| format!("style {k} label {l} has no roster alias"))?;
            o.insert(out.into(), json!(rules.generators[i].key));
            if k != "cross_base_generator" {
                inventors.insert(i);
                notice.extend(rules.generators[i].notice_files.iter().cloned());
            }
        }
    }
    if !notice.is_empty() {
        o.insert("inventor_notice_files".into(), json!(notice));
    }
    Ok(Some(Value::Object(o)))
}

// ---------------------------------------------------------------- the denylist gate

pub struct Gate {
    subs: Vec<String>,
    operator: Vec<String>,
    private: Vec<String>,
    pub hits: Vec<Value>,
}

/// A local port written as `port 18125` or `port18125` (lowercase, not inside a longer word).
pub(crate) fn local_port(s: &str) -> bool {
    let b = s.as_bytes();
    let mut start = 0;
    while let Some(k) = s[start..].find("port") {
        let at = start + k;
        let boundary = at == 0 || !b[at - 1].is_ascii_alphabetic();
        let mut j = at + 4;
        if j < b.len() && b[j] == b' ' {
            j += 1;
        }
        let digits = b[j..].iter().take_while(|c| c.is_ascii_digit()).count();
        let end_ok = j + digits == b.len() || !b[j + digits].is_ascii_digit();
        if boundary && (4..=5).contains(&digits) && end_ok {
            return true;
        }
        start = at + 4;
    }
    false
}

fn is_hex(b: &[u8]) -> bool {
    b.iter().all(|c| c.is_ascii_hexdigit())
}

pub(crate) fn private_ipv4(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !(b[i - 1].is_ascii_digit() || b[i - 1] == b'.')) {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
                j += 1;
            }
            let tok = &s[i..j];
            let parts: Vec<&str> = tok.trim_end_matches('.').split('.').collect();
            if parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u16>().is_ok_and(|x| x <= 255)) {
                let a: u16 = parts[0].parse().unwrap();
                let b2: u16 = parts[1].parse().unwrap();
                if a == 10 || a == 127 || (a == 192 && b2 == 168) || (a == 172 && (16..=31).contains(&b2)) {
                    return true;
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    false
}

/// Provider and credential identifiers (`blk_<32 hex>`, `sk-…`, `ghp_…`, `AKIA…`, `req_…`, `resp_…`, `chatcmpl-…`, `msg_01…`).
pub(crate) fn provider_id(s: &str) -> Option<&'static str> {
    let b = s.as_bytes();
    let alnum_run = |from: usize| b[from..].iter().take_while(|c| c.is_ascii_alphanumeric() || **c == b'_' || **c == b'-').count();
    for (pat, name, min) in [
        ("blk_", "pangram_bulk_id", 32usize),
        ("sk-", "secret_key", 20),
        ("ghp_", "github_token", 20),
        ("AKIA", "aws_key", 16),
        ("req_", "provider_request_id", 16),
        ("resp_", "provider_response_id", 16),
        ("chatcmpl-", "provider_completion_id", 10),
        ("msg_01", "provider_message_id", 16),
    ] {
        let mut start = 0;
        while let Some(k) = s[start..].find(pat) {
            let at = start + k;
            let boundary = at == 0 || !(b[at - 1].is_ascii_alphanumeric());
            let run = alnum_run(at + pat.len());
            if boundary && run >= min && (name != "pangram_bulk_id" || is_hex(&b[at + 4..at + 4 + 32.min(run)])) {
                return Some(name);
            }
            start = at + pat.len();
        }
    }
    None
}

impl Gate {
    pub fn new(rules: &Rules, repo: &Path) -> Result<Gate> {
        let mut operator: Vec<String> = rules.operator_values.iter().filter(|x| !x.is_empty()).map(|x| x.to_lowercase()).collect();
        // One value per non-empty line; the values are compared in memory and never printed.
        for f in &rules.operator_value_files {
            let p = canon(repo, f);
            let v = fs::read_to_string(&p).with_context(|| format!("operator value file {f}"))?;
            let n = operator.len();
            operator.extend(v.lines().map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty()));
            ensure!(operator.len() > n, "operator value file {f} is empty");
        }
        if let Ok(home) = std::env::var("HOME") {
            if home.len() > 1 {
                operator.push(home.to_lowercase());
            }
        }
        let private = load_private_strings(rules, repo)?;
        Ok(Gate { subs: rules.forbidden_substrings.clone(), operator, private, hits: Vec::new() })
    }

    fn hit(&mut self, table: &str, row: u64, path: &[String], what: &str) {
        if self.hits.len() < 10000 {
            self.hits.push(json!({"table": table, "row": row, "path": path.join("."), "pattern": what}));
        }
    }

    /// Patterns found in `s`: operator values always, the rest only outside text fields.
    fn findings(&self, s: &str, text: bool) -> Vec<String> {
        let mut f = Vec::new();
        let l = s.to_lowercase();
        for (i, o) in self.operator.iter().enumerate() {
            if l.contains(o.as_str()) {
                f.push(format!("operator_value_{i}"));
            }
        }
        // Private strings are checked in every field, corpus and model text included.
        for (i, o) in self.private.iter().enumerate() {
            if l.contains(o.as_str()) {
                f.push(format!("private_string_{i}"));
            }
        }
        if text {
            return f;
        }
        for p in ["/home/", "/Users/", "/tmp/", "/mnt/", "~/", "file://", "127.0.0.1", "localhost"] {
            if s.contains(p) {
                f.push(p.into());
            }
        }
        if l.contains("loopback") {
            f.push("loopback".into());
        }
        if local_port(s) {
            f.push("local_port".into());
        }
        for p in &self.subs {
            if s.contains(p.as_str()) {
                f.push(p.clone());
            }
        }
        if private_ipv4(s) {
            f.push("private_ipv4".into());
        }
        if let Some(n) = provider_id(s) {
            f.push(n.into());
        }
        f
    }

    /// Scan a finished row. Keys listed in `text_keys` hold corpus or model text.
    pub fn scan(&mut self, table: &str, row: u64, v: &Value, text_keys: &[&str]) {
        let mut path = Vec::new();
        self.walk(table, row, v, &mut path, text_keys, false);
    }

    fn walk(&mut self, table: &str, row: u64, v: &Value, path: &mut Vec<String>, text_keys: &[&str], in_text: bool) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    path.push(k.clone());
                    if deny_key(k) && !in_text {
                        self.hit(table, row, path, "denied_key");
                    }
                    self.walk(table, row, x, path, text_keys, in_text || text_keys.contains(&k.as_str()));
                    path.pop();
                }
            }
            Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    path.push(format!("[{i}]"));
                    self.walk(table, row, x, path, text_keys, in_text);
                    path.pop();
                }
            }
            Value::String(s) => {
                for f in self.findings(s, in_text) {
                    self.hit(table, row, path, &f);
                }
            }
            _ => {}
        }
    }
}

/// A checked table writer: every row passes the gate before it is written.
struct Table<'a> {
    name: &'static str,
    out: Out,
    path: PathBuf,
    gate: &'a mut Gate,
    text_keys: &'static [&'static str],
}

impl<'a> Table<'a> {
    fn new(dir: &Path, name: &'static str, gate: &'a mut Gate, text_keys: &'static [&'static str]) -> Result<Table<'a>> {
        let path = dir.join(format!("{name}.jsonl.zst"));
        Ok(Table { name, out: Out::new(&path)?, path, gate, text_keys })
    }
    fn row(&mut self, v: &Value) -> Result<()> {
        self.gate.scan(self.name, self.out.rows, v, self.text_keys);
        self.out.line(v)
    }
    fn finish(self, files: &mut BTreeMap<String, Value>) -> Result<u64> {
        let rows = self.out.rows;
        let mut meta = self.out.finish()?;
        meta["sha256"] = json!(archive::hash_file_raw(&self.path)?);
        eprintln!("public-candidate {}: {rows}", self.name);
        files.insert(format!("{}.jsonl.zst", self.name), meta);
        Ok(rows)
    }
}

// ---------------------------------------------------------------- prompts

/// How the parent passage appears in a prompt.
pub(crate) fn embedding(prompt: &[u8], parent: Option<&[u8]>, span: Option<(usize, usize)>) -> &'static str {
    let Some(p) = parent else { return "no_parent" };
    let find = |needle: &[u8]| !needle.is_empty() && prompt.windows(needle.len()).any(|w| w == needle);
    if find(p) {
        return "full";
    }
    if let Some((a, b)) = span {
        if a < b && b <= p.len() && find(&p[a..b]) {
            return "span";
        }
    }
    // A prompt that is itself a stretch of the parent (base-model continuation).
    let trimmed = std::str::from_utf8(prompt).map(|s| s.trim().as_bytes()).unwrap_or(prompt);
    if trimmed.len() >= 64 && p.windows(trimmed.len()).any(|w| w == trimmed) {
        return "prefix";
    }
    // A stretch of the parent inside the prompt (span rewrites without a recorded source span).
    if p.len() >= EMBED_WINDOW && prompt.len() >= EMBED_WINDOW {
        let set: HashSet<&[u8]> = p.windows(EMBED_WINDOW).collect();
        if prompt.windows(EMBED_WINDOW).any(|w| set.contains(w)) {
            return "partial";
        }
    }
    "missing"
}

/// Bytes of parent text that must occur contiguously in a prompt for a partial embedding.
const EMBED_WINDOW: usize = 64;

/// Whether a prompt with template `tp` and parent embedding `emb` may be published.
fn prompt_ok(rules: &Rules, tp: &str, emb: &str, has_parent: bool, parent_released: bool) -> bool {
    if rules.embed_full_templates.iter().any(|x| x == tp) {
        emb == "full"
    } else if rules.embed_span_templates.iter().any(|x| x == tp) {
        matches!(emb, "full" | "span" | "partial")
    } else if rules.embed_prefix_templates.iter().any(|x| x == tp) {
        matches!(emb, "full" | "span" | "prefix" | "partial")
    } else {
        !has_parent || parent_released
    }
}

// ---------------------------------------------------------------- run

struct Obs {
    obs_key: String,
    detector: String,
    version: Option<String>,
    text: Option<H>,
    item_id: Option<String>,
    observed_utc: Option<String>,
    fa: Option<f64>,
    faa: Option<f64>,
    fh: Option<f64>,
    score: Option<f64>,
    pred: Option<Value>,
    status: String,
    context: Option<String>,
}

#[derive(Clone)]
struct Ev {
    event_key: String,
    kind: String,
    source_id: String,
    generator_label: Option<String>,
    generator_key: Option<String>,
    requested_model: Option<String>,
    reported_model: Option<String>,
    operation: Option<String>,
    status: String,
    prompt_sha256: Option<String>,
    params: Option<Value>,
    response_sha256: Option<String>,
    request_id: Option<String>,
    attempt: Option<i64>,
    started_utc: Option<String>,
    output_text: Option<H>,
    extra: Option<Value>,
}

/// The extra-JSON fields the export reads from generation records (template, requests,
/// decoding, style, plan and identity); everything else stays in the store.
fn slim_extra(role: &str, x: Value) -> Value {
    let keys: &[&str] = match role {
        "local_positive" => &["template", "requests", "decoding", "style_ids", "plan_sha256", "prompt_sha256", "schema"],
        "frontier_positive" => &["ai_identity_basis", "model_reported", "selector_requested", "ai_identity", "template", "voice_id", "schema"],
        "local_output" => &["request_id", "attempt"],
        _ => &["ai_identity", "schema"],
    };
    let mut o = Map::new();
    if let Value::Object(mut m) = x {
        for k in keys {
            if let Some(v) = m.remove(*k) {
                o.insert((*k).into(), v);
            }
        }
    }
    Value::Object(o)
}

fn opt(r: &rusqlite::Row, i: usize) -> rusqlite::Result<Option<String>> {
    Ok(r.get_ref(i)?.as_str_or_null().map_err(|e| rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e)))?.map(String::from))
}

fn opt_json(r: &rusqlite::Row, i: usize) -> rusqlite::Result<Option<Value>> {
    Ok(opt(r, i)?.and_then(|s| serde_json::from_str(&s).ok()))
}

pub(crate) fn ms_to_iso(s: &str) -> Option<String> {
    let ms: i64 = s.parse().ok()?;
    if !(1_000_000_000_000..10_000_000_000_000).contains(&ms) {
        return None;
    }
    let secs = ms.div_euclid(1000);
    let milli = ms.rem_euclid(1000);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    Some(format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z", rem / 3600, (rem % 3600) / 60, rem % 60))
}

fn find_key<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m.get(k).filter(|x| !x.is_null()).or_else(|| m.values().find_map(|x| find_key(x, k))),
        Value::Array(a) => a.iter().find_map(|x| find_key(x, k)),
        _ => None,
    }
}

/// Selector, effort, turn limit and system prompt parsed from a recorded CLI argv.
fn parse_argv(argv: &[Value]) -> Map<String, Value> {
    let a: Vec<&str> = argv.iter().filter_map(|x| x.as_str()).collect();
    let mut o = Map::new();
    let mut i = 0;
    while i < a.len() {
        let next = a.get(i + 1).copied();
        match a[i] {
            "--model" => {
                if let Some(n) = next {
                    o.insert("model".into(), json!(n));
                }
            }
            "--effort" => {
                if let Some(n) = next {
                    o.insert("effort".into(), json!(n));
                }
            }
            "--max-turns" => {
                if let Some(n) = next {
                    o.insert("max_turns".into(), json!(n));
                }
            }
            "--system-prompt" => {
                if let Some(n) = next {
                    o.insert("system_prompt".into(), json!(n));
                }
            }
            "-c" => {
                if let Some(n) = next {
                    if let Some(v) = n.strip_prefix("model_reasoning_effort=") {
                        o.insert("effort".into(), json!(v.trim_matches('"')));
                    }
                    if n.starts_with("model_instructions_file=") {
                        o.insert("system_prompt_from_instructions_file".into(), json!(true));
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    o
}

pub struct Run<'a> {
    pub db: &'a Connection,
    pub store: &'a Path,
    pub repo: PathBuf,
    pub out_dir: &'a Path,
    pub rules: Rules,
    pub rules_sha256: String,
}

/// Exclusion reasons, interned (millions of texts share a few dozen reasons).
#[derive(Default)]
struct Reasons {
    names: Vec<String>,
    idx: HashMap<String, u32>,
    by_text: HashMap<H, u32>,
}
impl Reasons {
    fn insert(&mut self, t: H, r: impl Into<String>) {
        let r = r.into();
        let i = match self.idx.get(&r) {
            Some(&i) => i,
            None => {
                self.names.push(r.clone());
                self.idx.insert(r, self.names.len() as u32 - 1);
                self.names.len() as u32 - 1
            }
        };
        self.by_text.insert(t, i);
    }
    fn get(&self, t: &H) -> Option<&str> {
        self.by_text.get(t).map(|i| self.names[*i as usize].as_str())
    }
    fn count(&self, r: &str) -> usize {
        match self.idx.get(r) {
            Some(i) => self.by_text.values().filter(|x| *x == i).count(),
            None => 0,
        }
    }
}

#[derive(Default)]
pub(crate) struct Counter(pub(crate) BTreeMap<String, u64>);
impl Counter {
    pub(crate) fn add(&mut self, k: impl Into<String>, n: u64) {
        *self.0.entry(k.into()).or_default() += n;
    }
    pub(crate) fn json(&self) -> Value {
        json!(self.0)
    }
}

const FAIL_P: &str = "private_or_unprovenanced";
const FAIL_R: &str = "reserved_evaluation_stock";
const FAIL_S: &str = "sealed_or_unconsumed_test";

pub fn run(db: &Connection, store: &Path, out_dir: &Path, rules_path: &Path, repo_override: Option<&Path>) -> Result<Value> {
    ensure!(!out_dir.exists(), "{} exists", out_dir.display());
    let (rules, rules_sha256) = load_rules(rules_path)?;
    let repo = match repo_override {
        Some(p) => fs::canonicalize(p)?,
        None => crate::export::repo_root(store).context("no repository root above the store")?,
    };
    let r = Run { db, store, repo, out_dir, rules, rules_sha256 };
    r.go()
}

impl<'a> Run<'a> {
    fn go(&self) -> Result<Value> {
        let rules = &self.rules;
        let db = self.db;
        let t0 = std::time::Instant::now();
        let rss = || {
            fs::read_to_string("/proc/self/status").ok().and_then(|s| {
                s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|k| k.parse::<u64>().ok())
            }).map(|k| k / 1024).unwrap_or(0)
        };
        let log = |m: &str| eprintln!("[{:7.1}s {:6} MB] {m}", t0.elapsed().as_secs_f64(), rss());

        // Sources.
        let mut src_rows = Vec::new();
        {
            let mut st = db.prepare("SELECT source_id, adapter, root FROM source")?;
            let mut q = st.query([])?;
            while let Some(r) = q.next()? {
                src_rows.push((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?));
            }
        }
        let srcs = classify_sources(&src_rows, &self.repo, rules)?;
        let published_names: BTreeMap<String, String> =
            srcs.iter().filter_map(|(k, s)| s.published.clone().map(|n| (k.clone(), n))).collect();
        for id in rules.published_sources.keys() {
            if srcs.contains_key(id) && !published_names.contains_key(id) {
                bail!("listed published source {id} is not rooted at <repo>/datasets/<name>");
            }
        }

        // Reservations: pinned files, plus a check that no unpinned reservation file exists.
        let (reserved_texts, reserved_groups, reservation_meta) = self.reservations()?;
        log(&format!("reservations: {} texts, {} groups", reserved_texts.len(), reserved_groups.len()));

        // Pass 1: one sequential scan of record.
        let mut ta: HashMap<H, TA> = HashMap::with_capacity(4_000_000);
        let mut parents: HashMap<H, Vec<H>> = HashMap::new();
        let mut self_parent_records = 0u64;
        let excl_colls: Vec<&String> = rules.excluded_collections.keys().collect();
        {
            let mut st = db.prepare(
                "SELECT text_sha256, source_id, role, record_id, licence, collection, origin, generator_key, generator_label, cohort_id,
                        source_group, operation, parent_text_sha256, admission,
                        CASE WHEN origin IN ('mixed', 'model_only') OR generator_key IS NOT NULL OR generator_label IS NOT NULL
                             THEN instr(COALESCE(extra_json, ''), '\"withdrawn\":true') > 0 ELSE 0 END
                 FROM record NOT INDEXED WHERE text_sha256 IS NOT NULL",
            )?;
            let mut q = st.query([])?;
            let mut n = 0u64;
            while let Some(r) = q.next()? {
                n += 1;
                let Some(t) = r.get_ref(0)?.as_str_or_null()?.and_then(hx) else { continue };
                let source_id = r.get_ref(1)?.as_str()?;
                let role = r.get_ref(2)?.as_str()?;
                let record_id = r.get_ref(3)?.as_str_or_null()?;
                let licence = r.get_ref(4)?.as_str_or_null()?;
                let collection = r.get_ref(5)?.as_str_or_null()?;
                let origin = r.get_ref(6)?.as_str_or_null()?;
                let gk = r.get_ref(7)?.as_str_or_null()?;
                let gl = r.get_ref(8)?.as_str_or_null()?;
                let cohort = r.get_ref(9)?.as_str_or_null()?;
                let group = r.get_ref(10)?.as_str_or_null()?;
                let op = r.get_ref(11)?.as_str_or_null()?;
                let parent = r.get_ref(12)?.as_str_or_null()?.and_then(hx);
                let admission = r.get_ref(13)?.as_str_or_null()?;
                let withdrawn_extra = r.get::<_, i64>(14)? == 1;
                let s = srcs.get(source_id).cloned().unwrap_or_default();
                let a = ta.entry(t).or_default();
                let prov = s.prov && !rules.private_roles.iter().any(|x| x == role);
                if prov {
                    a.prov += 1;
                }
                if s.refuse
                    || (s.private_source
                        && record_id.is_some_and(|id| rules.hard_private_record_prefixes.iter().any(|p| id.starts_with(p.as_str()))))
                {
                    a.hard_private = true;
                }
                if s.published.is_some() {
                    a.published = true;
                }
                if prov {
                    if let Some(l) = licence {
                        match licence_rule(rules, l) {
                            Some(i) => a.lic_mask |= 1u64 << i,
                            None => a.lic_unknown = true,
                        }
                    }
                    if let Some(c) = collection {
                        if let Some(i) = excl_colls.iter().position(|x| x.as_str() == c) {
                            a.excl_coll = Some(i as u16);
                        }
                    }
                }
                let is_gen = matches!(origin, Some("mixed") | Some("model_only")) || gk.is_some() || gl.is_some();
                // A generation record whose recorded parent is its own text describes no change to
                // these bytes; it does not make the text a generation and is not published.
                let self_parent = is_gen && parent == Some(t);
                if self_parent {
                    self_parent_records += 1;
                }
                if is_gen && !self_parent {
                    a.is_gen = true;
                    let lk = gk.unwrap_or("").to_ascii_lowercase();
                    if lk.starts_with("raid:") || collection == Some("raid_benchmark_v1") || cohort.is_some_and(|c| c.starts_with("raid-")) {
                        a.raid = true;
                    }
                    if matches!(role, "frontier_positive" | "hosted_capture" | "hosted_style_positive")
                        || matches!(cohort, Some("hosted_frontier_v1") | Some("frontier-captures-v1") | Some("hosted-style-v1"))
                        || lk.starts_with("claude")
                        || (lk.starts_with("gpt-") && !lk.starts_with("gpt-oss"))
                    {
                        a.hosted = true;
                    }
                    if admission.is_some_and(|x| x.to_ascii_lowercase().contains("withdrawn")) || withdrawn_extra {
                        a.withdrawn = true;
                    }
                    let opl = op.unwrap_or("").to_ascii_lowercase();
                    let cl = cohort.unwrap_or("").to_ascii_lowercase();
                    if opl.contains("reddit") || opl.contains("style_transfer") || (cl.contains("reddit") && cl.contains("voice")) || cl.contains("imitation") {
                        a.reddit = true;
                    }
                } else if collection == Some("raid_benchmark_v1") {
                    a.raid = true;
                }
                if group.is_some_and(|g| reserved_groups.contains(g)) {
                    a.reserved = true;
                }
                if let Some(p) = parent {
                    if p != t {
                        let v = parents.entry(t).or_default();
                        if !v.contains(&p) {
                            v.push(p);
                        }
                    }
                }
                if n % 2_000_000 == 0 {
                    log(&format!("records: {n}"));
                }
            }
            log(&format!("records pass 1: {n} rows, {} texts", ta.len()));
        }
        for t in &reserved_texts {
            if let Some(a) = ta.get_mut(t) {
                a.reserved = true;
            }
        }

        // Split membership: use in a view (not a split-assignment manifest), sealed roles.
        let mut splits = Intern::default();
        {
            let mut st = db.prepare("SELECT text_sha256, experiment, role FROM split_member NOT INDEXED WHERE +text_sha256 IS NOT NULL")?;
            let mut q = st.query([])?;
            let mut n = 0u64;
            while let Some(r) = q.next()? {
                n += 1;
                let Some(t) = r.get_ref(0)?.as_str()?.get(..64).and_then(hx) else { continue };
                let exp = r.get_ref(1)?.as_str()?;
                let role = r.get_ref(2)?.as_str()?;
                let Some(a) = ta.get_mut(&t) else { continue };
                if rules.sealed_roles.iter().any(|x| x == role) {
                    a.sealed = true;
                }
                if exp.starts_with(rules.splits_experiment_prefix.as_str()) {
                    a.splits_roles |= match role {
                        "train" => SR_TRAIN,
                        "train_mining" => SR_MINING,
                        "held" => SR_HELD,
                        "reserved_unassigned" => SR_RESERVED,
                        x if x.contains("diagnostic") => SR_DIAG,
                        _ => SR_OTHER,
                    };
                } else if rules.used_roles.iter().any(|x| x == role) {
                    a.used_view = true;
                }
            }
            log(&format!("split_member pass 1: {n} rows"));
        }
        {
            let mut st = db.prepare("SELECT text_sha256, canonical_split, evaluation_only, group_evaluation_only FROM text_split")?;
            let mut q = st.query([])?;
            while let Some(r) = q.next()? {
                let Some(t) = hx(r.get_ref(0)?.as_str()?) else { continue };
                let Some(a) = ta.get_mut(&t) else { continue };
                if let Some(c) = r.get_ref(1)?.as_str_or_null()? {
                    if rules.sealed_roles.iter().any(|x| x == c) {
                        a.sealed = true;
                    }
                    a.split = splits.id(c);
                }
                a.eo = r.get::<_, i64>(2)? == 1;
                a.geo = r.get::<_, i64>(3)? == 1;
            }
            log("text_split read");
        }

        // Observations.
        let mut obs: Vec<Obs> = Vec::new();
        {
            let mut st = db.prepare(
                "SELECT obs_key, detector, detector_version, text_sha256, item_id, observed_utc, fraction_ai, fraction_ai_assisted,
                        fraction_human, score, prediction_json, status, context FROM observation WHERE detector = 'pangram' ORDER BY obs_uid",
            )?;
            let mut q = st.query([])?;
            while let Some(r) = q.next()? {
                obs.push(Obs {
                    obs_key: r.get(0)?,
                    detector: r.get(1)?,
                    version: opt(r, 2)?,
                    text: opt(r, 3)?.and_then(|s| hx(&s)),
                    item_id: opt(r, 4)?,
                    observed_utc: opt(r, 5)?,
                    fa: r.get(6)?,
                    faa: r.get(7)?,
                    fh: r.get(8)?,
                    score: r.get(9)?,
                    pred: opt_json(r, 10)?,
                    status: r.get(11)?,
                    context: opt(r, 12)?,
                });
            }
        }
        let mut private_obs_texts: HashSet<H> = HashSet::new();
        for o in &obs {
            if let Some(t) = o.text {
                if o.context.as_deref().is_some_and(|c| rules.private_contexts.iter().any(|x| x == c)) {
                    private_obs_texts.insert(t);
                } else if o.detector == "pangram" {
                    if let Some(a) = ta.get_mut(&t) {
                        a.pangram = true;
                    }
                }
            }
        }
        for t in &private_obs_texts {
            if let Some(a) = ta.get_mut(t) {
                a.hard_private = true;
            }
        }
        log(&format!("observations: {}", obs.len()));

        // Ancestry (transitive, bounded depth).
        let anc = |t: &H| -> Vec<H> {
            let mut out: Vec<H> = Vec::new();
            let mut frontier: Vec<H> = parents.get(t).cloned().unwrap_or_default();
            let mut depth = 0;
            while !frontier.is_empty() && depth < rules.max_ancestor_depth {
                let mut next = Vec::new();
                for p in frontier {
                    if p != *t && !out.contains(&p) {
                        out.push(p);
                        if let Some(pp) = parents.get(&p) {
                            next.extend(pp.iter().copied());
                        }
                    }
                }
                frontier = next;
                depth += 1;
            }
            out
        };

        // Gates P, R, S, U, A and L1 (record licence classes and excluded collections).
        let own_p = |a: &TA| a.prov == 0 || a.hard_private;
        let own_u = |a: &TA| !(a.used_view || a.published || a.pangram);
        let lic_class = |a: &TA| -> Result<(u8, Option<String>), String> {
            if let Some(i) = a.excl_coll {
                return Err(format!("excluded_collection:{}", excl_colls[i as usize]));
            }
            if a.lic_unknown {
                return Err("licence_unclassified".into());
            }
            let mut rank = None;
            for (i, lr) in rules.licences.iter().enumerate() {
                if a.lic_mask & (1u64 << i) != 0 {
                    if lr.class == "excluded" {
                        return Err(format!("licence:{}", lr.decision));
                    }
                    let k = class_rank(&lr.class);
                    if k == 100 {
                        return Err("licence_unclassified".into());
                    }
                    rank = Some(rank.map_or(k, |x: u8| x.max(k)));
                }
            }
            Ok((rank.unwrap_or(255), None))
        };
        let mut reason = Reasons::default();
        let mut candidates: Vec<H> = Vec::new();
        let mut hash_only_eligible: HashSet<H> = HashSet::new();
        let none = TA::default();
        for (t, a) in &ta {
            let an = anc(t);
            let get = |h: &H| ta.get(h).unwrap_or(&none);
            let r: Option<String> = if own_p(a) {
                Some(FAIL_P.into())
            } else if an.iter().any(|p| own_p(get(p))) {
                Some(format!("ancestor_{FAIL_P}"))
            } else if a.reserved {
                Some(FAIL_R.into())
            } else if an.iter().any(|p| get(p).reserved) {
                Some(format!("ancestor_{FAIL_R}"))
            } else if a.sealed || an.iter().any(|p| get(p).sealed) {
                Some(FAIL_S.into())
            } else if own_u(a) {
                Some(format!("unused_holdout:{}", splits_roles_label(a.splits_roles)))
            } else if an.iter().any(|p| own_u(get(p))) {
                Some("ancestor_unused_holdout".into())
            } else if a.is_gen && a.withdrawn {
                Some("withdrawn_generation".into())
            } else if a.is_gen && a.reddit {
                Some("reddit_imitation_route".into())
            } else {
                // L1 over the text and every ancestor.
                let mut rank: Option<u8> = None;
                let mut fail = None;
                for x in std::iter::once(t).chain(an.iter()) {
                    match lic_class(get(x)) {
                        Err(e) => {
                            fail = Some(if x == t { e } else { format!("ancestor_{e}") });
                            break;
                        }
                        Ok((k, _)) if k != 255 => rank = Some(rank.map_or(k, |y| y.max(k))),
                        Ok(_) => {}
                    }
                }
                match (fail, rank) {
                    (Some(e), _) => Some(e),
                    (None, None) => Some("licence_unrecorded".into()),
                    (None, Some(_)) if a.raid => Some("licence:raid_excluded".into()),
                    _ => None,
                }
            };
            match r {
                Some(x) => {
                    let rights_only = x.starts_with("licence") || x.starts_with("excluded_collection") || x.starts_with("ancestor_licence") || x.starts_with("ancestor_excluded");
                    if rights_only && a.pangram {
                        hash_only_eligible.insert(*t);
                    }
                    reason.insert(*t, x);
                }
                None => candidates.push(*t),
            }
        }
        candidates.sort();
        log(&format!("phase 1: {} candidates, {} hash-only eligible", candidates.len(), hash_only_eligible.len()));

        // Pass 2: full records of candidates, their ancestors and hash-only texts.
        let mut need: HashSet<H> = candidates.iter().copied().collect();
        for t in &candidates {
            need.extend(anc(t));
        }
        need.extend(hash_only_eligible.iter().copied());
        let mut recs: HashMap<H, Vec<Rec>> = HashMap::new();
        {
            let mut st = db.prepare(
                "SELECT file_sha256, line_no, role, line_sha256, source_id, record_id, text_sha256, dataset, collection, domain, genre,
                        source_work, family, source_group, writer_key, original_doc_id, doc_date, composition_date, licence, cohort_id,
                        origin, share, ai_byte_spans, generator_label, generator_key, operation, recipe_sha256, parent_record_id,
                        parent_text_sha256, split_hint, extra_json
                 FROM record INDEXED BY record_text WHERE text_sha256 = ?1",
            )?;
            let mut n = 0u64;
            let gen_roles = ["frontier_positive", "local_positive", "local_output", "local_record", "hosted_capture", "hosted_style_positive", "published_record", "parity_record", "laptop_labelled_record"];
            let mut order: Vec<H> = need.iter().copied().collect();
            order.sort();
            for t in order {
              let mut q = st.query([hs(&t)])?;
              while let Some(r) = q.next()? {
                let role = r.get_ref(2)?.as_str()?;
                let view_copy = rules.skip_roles.iter().any(|x| x == role);
                if view_copy && recs.get(&t).is_some_and(|v| !v.is_empty()) {
                    continue;
                }
                let source_id: String = r.get(4)?;
                let s = srcs.get(&source_id).cloned().unwrap_or_default();
                if !s.prov || rules.private_roles.iter().any(|x| x == role) {
                    continue;
                }
                {
                    let origin = r.get_ref(20)?.as_str_or_null()?;
                    let is_gen = matches!(origin, Some("mixed") | Some("model_only")) || r.get_ref(23)?.as_str_or_null()?.is_some() || r.get_ref(24)?.as_str_or_null()?.is_some();
                    if is_gen && r.get_ref(28)?.as_str_or_null()?.and_then(hx) == Some(t) {
                        continue;
                    }
                }
                n += 1;
                let v = recs.entry(t).or_default();
                if view_copy {
                    // Kept only while the text has no primary record (generator labels of view-only generations).
                    if v.iter().any(|x| x.role != role && !rules.skip_roles.contains(&x.role)) {
                        continue;
                    }
                } else {
                    v.retain(|x| !rules.skip_roles.contains(&x.role));
                }
                v.push(Rec {
                    file: r.get(0)?,
                    line_no: r.get(1)?,
                    role: role.into(),
                    line_sha: r.get(3)?,
                    source_id,
                    record_id: opt(r, 5)?,
                    dataset: opt(r, 7)?,
                    collection: opt(r, 8)?,
                    domain: opt(r, 9)?,
                    genre: opt(r, 10)?,
                    source_work: opt(r, 11)?,
                    family: opt(r, 12)?,
                    source_group: opt(r, 13)?,
                    writer_key: opt(r, 14)?,
                    original_doc_id: opt(r, 15)?,
                    doc_date: opt(r, 16)?,
                    composition_date: opt(r, 17)?,
                    licence: opt(r, 18)?,
                    cohort_id: opt(r, 19)?,
                    origin: opt(r, 20)?,
                    share: r.get(21)?,
                    ai_byte_spans: opt(r, 22)?,
                    generator_label: opt(r, 23)?,
                    generator_key: opt(r, 24)?,
                    operation: opt(r, 25)?,
                    recipe_sha256: opt(r, 26)?,
                    parent_record_id: opt(r, 27)?,
                    parent_text_sha256: opt(r, 28)?,
                    split_hint: opt(r, 29)?,
                    extra: if gen_roles.contains(&role) { opt_json(r, 30)?.map(|x| slim_extra(role, x)) } else { None },
                });
              }
            }
            log(&format!("records pass 2: {n} records of {} texts", recs.len()));
        }
        // Newest splits version only for split documents.
        for v in recs.values_mut() {
            let best = v
                .iter()
                .filter(|r| r.role == "split_document")
                .filter_map(|r| rules.split_document_sources.iter().position(|s| s == &r.source_id))
                .min();
            v.retain(|r| {
                r.role != "split_document" || rules.split_document_sources.iter().position(|s| s == &r.source_id) == best && best.is_some()
            });
        }

        // Rebuild the archived source lines of provenance records.
        let mut wanted: BTreeMap<String, BTreeMap<i64, String>> = BTreeMap::new();
        let mut gen_lines: HashSet<(String, i64)> = HashSet::new();
        for v in recs.values() {
            for r in v {
                if rules.record_roles.contains(&r.role) || r.licence.is_some() {
                    wanted.entry(r.file.clone()).or_default().insert(r.line_no, r.line_sha.clone());
                }
                if matches!(r.role.as_str(), "published_record" | "local_record" | "hosted_style_positive") {
                    gen_lines.insert((r.file.clone(), r.line_no));
                }
            }
        }
        let n_wanted: usize = wanted.values().map(|m| m.len()).sum();
        log(&format!("rebuilding {n_wanted} lines from {} files", wanted.len()));
        let mut lines: HashMap<(String, i64), LineInfo> = HashMap::new();
        let mut line_mismatch = 0u64;
        {
            let mut cache = BlobCache::new(&self.store.join("packs"));
            for (file, want) in &wanted {
                let set: BTreeSet<i64> = want.keys().copied().collect();
                archive::rebuild_lines(db, &mut cache, file, &set, &mut |ln, bytes, whole| {
                    let expect = &want[&ln];
                    if &sha_hex(bytes) != expect {
                        line_mismatch += 1;
                        return Ok(());
                    }
                    let li = if whole {
                        LineInfo { whole_file: true, ..Default::default() }
                    } else {
                        match serde_json::from_slice::<Value>(bytes) {
                            Ok(v) => {
                                let mut li = line_info(&v);
                                if gen_lines.contains(&(file.clone(), ln)) {
                                    if let Some((g, ids)) = generation_of(&v) {
                                        li.generation = Some(g);
                                        li.gen_ids = ids;
                                    }
                                }
                                li
                            }
                            Err(_) => LineInfo::default(),
                        }
                    };
                    lines.insert((file.clone(), ln), li);
                    Ok(())
                })?;
            }
        }
        ensure!(line_mismatch == 0, "{line_mismatch} archived lines differ from their recorded digest");
        log(&format!("rebuilt {} lines", lines.len()));

        // Events of candidate generations (by output digest), and the attempt states of their frontier requests.
        let cand_set: HashSet<H> = candidates.iter().copied().collect();
        let mut events: Vec<Ev> = Vec::new();
        let mut attempts_by_req: HashMap<(String, String), Vec<(i64, String)>> = HashMap::new();
        let mut requests: HashMap<(String, String), Ev> = HashMap::new();
        {
            let cols = "event_key, kind, source_id, generator_label, generator_key, requested_model, reported_model, operation, status,
                        prompt_sha256, params_json, response_sha256, request_id, attempt, started_utc, input_text_sha256,
                        output_text_sha256, extra_json";
            let mut st = db.prepare(&format!("SELECT {cols} FROM event INDEXED BY event_output WHERE output_text_sha256 = ?1"))?;
            let read = |r: &rusqlite::Row| -> Result<Ev> {
                Ok(Ev {
                    event_key: r.get(0)?,
                    kind: r.get(1)?,
                    source_id: r.get(2)?,
                    generator_label: opt(r, 3)?,
                    generator_key: opt(r, 4)?,
                    requested_model: opt(r, 5)?,
                    reported_model: opt(r, 6)?,
                    operation: opt(r, 7)?,
                    status: r.get(8)?,
                    prompt_sha256: opt(r, 9)?,
                    params: opt_json(r, 10)?,
                    response_sha256: opt(r, 11)?,
                    request_id: opt(r, 12)?,
                    attempt: r.get(13)?,
                    started_utc: opt(r, 14)?,
                    output_text: opt(r, 16)?.and_then(|s| hx(&s)),
                    extra: opt_json(r, 17)?,
                })
            };
            let kinds = ["frontier_request", "frontier_attempt", "hosted_capture", "local_generate", "local_generate_record"];
            let mut gens: Vec<H> = candidates.iter().copied().filter(|t| ta.get(t).is_some_and(|a| a.is_gen)).collect();
            gens.sort();
            for t in &gens {
                let mut q = st.query([hs(t)])?;
                while let Some(r) = q.next()? {
                    let e = read(r)?;
                    if !kinds.contains(&e.kind.as_str()) {
                        continue;
                    }
                    if e.kind == "frontier_request" {
                        requests.insert((e.source_id.clone(), e.request_id.clone().unwrap_or_default()), e);
                    } else {
                        events.push(e);
                    }
                }
            }
            let mut st = db.prepare("SELECT source_id, request_id, attempt, status FROM event INDEXED BY event_request WHERE request_id = ?1 AND kind = 'frontier_attempt'")?;
            let ids: BTreeSet<String> = requests.keys().map(|(_, r)| r.clone()).chain(events.iter().filter(|e| e.kind == "frontier_attempt").filter_map(|e| e.request_id.clone())).collect();
            for id in &ids {
                let mut q = st.query([id])?;
                while let Some(r) = q.next()? {
                    let src: String = r.get(0)?;
                    attempts_by_req.entry((src, id.clone())).or_default().push((r.get::<_, Option<i64>>(2)?.unwrap_or(0), r.get(3)?));
                }
            }
            let _ = &cand_set;
            log(&format!("events: {} kept, {} requests, {} request ids", events.len(), requests.len(), ids.len()));
        }

        // Voices referenced by frontier requests, and the private strings that must never be published.
        let voices = self.voices(&requests)?;
        let private = load_private_strings(rules, &self.repo)?;

        // Phase 2 (L2): archived-line flags, USPTO dates, attribution, voices and generators.
        let override_colls: HashSet<&str> = rules.flag_override.collections.iter().map(String::as_str).collect();
        let mut own_l2: HashMap<H, Result<RightsOwn, String>> = HashMap::new();
        let mut to_check: BTreeSet<H> = candidates.iter().copied().collect();
        for t in &candidates {
            to_check.extend(anc(t));
        }
        for t in &to_check {
            let a = ta.get(t).unwrap_or(&none);
            own_l2.insert(*t, self.rights_own(t, a, recs.get(t).map(|v| v.as_slice()).unwrap_or(&[]), &lines, &override_colls));
        }
        // Generator resolution for generations.
        let mut ev_by_out: HashMap<H, Vec<&Ev>> = HashMap::new();
        for e in &events {
            if let Some(o) = e.output_text {
                ev_by_out.entry(o).or_default().push(e);
            }
        }
        let no_events: Vec<&Ev> = Vec::new();
        let mut unresolved_ids: BTreeSet<String> = BTreeSet::new();
        let mut gen_of: HashMap<H, usize> = HashMap::new();
        let mut released: Vec<H> = Vec::new();
        let mut voice_fail = 0u64;
        for t in &candidates {
            let a = ta.get(t).unwrap_or(&none);
            let an = anc(t);
            let mut fail: Option<String> = None;
            match own_l2.get(t) {
                Some(Err(e)) => fail = Some(e.clone()),
                _ => {}
            }
            if fail.is_none() {
                for p in &an {
                    if let Some(Err(e)) = own_l2.get(p) {
                        fail = Some(format!("ancestor_{e}"));
                        break;
                    }
                }
            }
            if fail.is_none() && a.is_gen {
                match self.resolve_generator(recs.get(t).map(|v| v.as_slice()).unwrap_or(&[]), ev_by_out.get(t).unwrap_or(&no_events), &lines) {
                    Some(i) => {
                        gen_of.insert(*t, i);
                    }
                    None => {
                        let rr = recs.get(t).map(|v| v.as_slice()).unwrap_or(&[]);
                        unresolved_ids.insert(format!("text {}: {} records", &hs(t)[..16], rr.len()));
                        for r in rr {
                            unresolved_ids.insert(format!("{}|{}|{}|{}", r.role, r.generator_key.as_deref().unwrap_or("-"), r.generator_label.as_deref().unwrap_or("-"), r.dataset.as_deref().unwrap_or("-")));
                        }
                        fail = Some("generator_unresolved".into());
                    }
                }
            }
            if fail.is_none() && a.hosted {
                if let Some(e) = self.voice_failure(ev_by_out.get(t).unwrap_or(&no_events), &requests, &voices) {
                    voice_fail += 1;
                    fail = Some(e);
                }
            }
            match fail {
                Some(e) => {
                    if a.pangram && !e.contains("generator") {
                        hash_only_eligible.insert(*t);
                    }
                    reason.insert(*t, e);
                }
                None => released.push(*t),
            }
        }
        let released_set: HashSet<H> = released.iter().copied().collect();
        // A released generation needs every ancestor released.
        let mut dropped_anc = 0;
        loop {
            let before = released.len();
            let set: HashSet<H> = released.iter().copied().collect();
            released.retain(|t| {
                let ok = anc(t).iter().all(|p| set.contains(p));
                if !ok {
                    reason.insert(*t, "ancestor_not_released");
                    dropped_anc += 1;
                }
                ok
            });
            if released.len() == before {
                break;
            }
        }
        let released_set: HashSet<H> = if dropped_anc > 0 { released.iter().copied().collect() } else { released_set };
        let unresolved = reason.count("generator_unresolved");
        log(&format!("phase 2: {} released ({} voice failures, {} without released ancestors, {} unresolved generators)", released.len(), voice_fail, dropped_anc, unresolved));
        ensure!(unresolved == 0, "{unresolved} generations have no resolved generator notice (add a generator rule): {:?}", unresolved_ids.iter().take(40).collect::<Vec<_>>());

        // Write the release.
        fs::create_dir_all(self.out_dir)?;
        let mut gate = Gate::new(rules, &self.repo)?;
        let mut files: BTreeMap<String, Value> = BTreeMap::new();
        let mut counts = Counter::default();
        let kinds: HashMap<H, &'static str> = released.iter().map(|t| (*t, kind_of(ta.get(t).unwrap_or(&none)))).collect();

        self.write_texts(&released, &kinds, &ta, &splits, &recs, &published_names, &mut gate, &mut files, &mut counts)?;
        log("texts written");
        let record_ids = self.write_records(&released, &recs, &lines, &mut gate, &mut files, &mut counts)?;
        self.write_rights(&released, &kinds, &own_l2, &anc, &gen_of, &lines, &mut gate, &mut files, &mut counts)?;
        log("records and rights written");
        let mut groups = self.write_splits(&released_set, &mut gate, &mut files, &mut counts)?;
        for t in &released {
            for r in recs.get(t).into_iter().flatten() {
                groups.extend(r.source_group.clone());
                groups.extend(r.family.clone());
            }
        }
        log("splits and experiments written");
        let pangram = self.write_pangram(&obs, &released_set, &hash_only_eligible, &reason, &ta, &kinds, &splits, &recs, &record_ids, &mut gate, &mut files)?;
        log("pangram written");
        let (gens_meta, used_voices, prompt_counts, mut inventors) = self.write_generations(
            &released, &kinds, &recs, &events, &requests, &attempts_by_req, &voices, &gen_of, &lines, &groups, &record_ids, &private, &mut gate, &mut files,
        )?;
        log("generations written");
        let (voice_counts, voice_inventors) = self.write_voices(&used_voices, &voices, &private, &released_set, &mut gate, &mut files)?;
        inventors.extend(voice_inventors);
        let generators = self.write_generators(&released, &kinds, &gen_of, &inventors, &mut gate)?;
        let licenses = self.write_licenses(&generators)?;

        // Exclusions (counts only).
        let mut excl = Counter::default();
        for (t, ri) in &reason.by_text {
            let r = &reason.names[*ri as usize];
            let k = kind_of(ta.get(t).unwrap_or(&none));
            excl.add(format!("{r}|{k}"), 1);
        }
        let texts_without_records: u64 = {
            let mut n = 0;
            let mut st = db.prepare("SELECT COUNT(*) FROM text")?;
            let total: i64 = st.query_row([], |r| r.get(0))?;
            n += (total as u64).saturating_sub(ta.len() as u64);
            n
        };
        let mut by_reason: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        for (k, n) in &excl.0 {
            let (r, kind) = k.rsplit_once('|').unwrap();
            *by_reason.entry(r.into()).or_default().entry(kind.into()).or_default() += n;
        }
        let exclusions = json!({
            "schema": "slop_ninja_public_candidate_exclusions_v1",
            "texts_by_reason_and_kind": by_reason,
            "texts_without_records": texts_without_records,
            "pangram": pangram["dropped"],
            "prompts": prompt_counts,
            "voices": voice_counts,
            "note": "counts only; no identifiers",
        });
        let report_only = std::env::var("PUBLIC_CANDIDATE_GATE_REPORT_ONLY").is_ok_and(|v| v == "1");
        let manifest_counts = json!({
            "texts": counts.json(),
            "generations": gens_meta,
            "pangram": pangram["counts"],
        });
        let readme = self.readme(&counts, &pangram, &gens_meta, &generators);
        let store_meta = self.store_meta()?;
        let mut manifest = json!({
            "schema": SCHEMA,
            "export_schema": crate::export::EXPORT_SCHEMA,
            "release": rules.release,
            "created_utc": crate::runner::now_utc(),
            "store": store_meta,
            "tool": {"version": env!("CARGO_PKG_VERSION"), "binary_sha256": std::env::current_exe().ok().and_then(|p| archive::hash_file_raw(&p).ok()),
                     "commit": std::env::var("PUBLIC_CANDIDATE_TOOL_COMMIT").ok()},
            "rules": {"schema": RULES_SCHEMA, "sha256": self.rules_sha256},
            "reservations": reservation_meta,
            "decisions": rules.decisions,
            "files": files,
            "licenses": licenses,
            "counts": manifest_counts,
            "exclusions": exclusions["texts_by_reason_and_kind"],
            "gates": {
                "P_private_or_unprovenanced": "applied", "R_reserved_evaluation_stock": "applied", "S_sealed_test": "applied",
                "U_used_by_experiment": "applied", "A_admission": "applied", "L_rights": "applied",
                "archived_line_digests": {"checked": lines.len(), "mismatched": line_mismatch},
                "self_parent_generation_records_ignored": self_parent_records,
                "private_strings": {"files": rules.private_string_files.len(), "strings_checked": private.len(), "scope": "every field of every file"},
                "inventor_aliases": rules.inventor_aliases.len(),
                "generators_unresolved": unresolved,
                "prompts": prompt_counts,
            },
            "selection": "records, split roles, reservations and observations; never by path substring; published status only for listed sources rooted exactly at <repo>/datasets/<name>",
            "publication": "private R2 public-candidate/ only",
            "private": false,
        });
        // The gate covers every file of the release, not only the tables.
        gate.scan("exclusions.json", 0, &exclusions, &[]);
        gate.scan("MANIFEST.json", 0, &manifest, &[]);
        gate.scan("README.md", 0, &json!({"readme": readme}), &[]);
        for f in ["LICENSES/INDEX.json", "LICENSES/NOTICE.md", "LICENSES/DATASET.md"] {
            let b = fs::read_to_string(self.out_dir.join(f))?;
            let v = if f.ends_with(".json") { serde_json::from_str(&b)? } else { json!({"text_file": b}) };
            gate.scan(f, 0, &v, &[]);
        }
        // A trial run may set PUBLIC_CANDIDATE_GATE_REPORT_ONLY=1 to collect every hit at once;
        // its MANIFEST then records the hits and the release must not be used.
        let gate_log = self.out_dir.with_extension("gate-hits.jsonl");
        if !gate.hits.is_empty() {
            let mut f = fs::File::create(&gate_log)?;
            for h in &gate.hits {
                writeln!(f, "{h}")?;
            }
            if !report_only {
                bail!("denylist gate: {} hits (locations in {})", gate.hits.len(), gate_log.display());
            }
            log(&format!("denylist gate: {} hits (report only; locations in {})", gate.hits.len(), gate_log.display()));
        }
        manifest["gates"]["denylist"] = json!({"hits": gate.hits.len(), "report_only": report_only, "scanned": "every table row and every other file"});
        fs::write(self.out_dir.join("exclusions.json"), serde_json::to_vec_pretty(&exclusions)?)?;
        fs::write(self.out_dir.join("README.md"), readme.as_bytes())?;
        fs::write(self.out_dir.join("MANIFEST.json"), serde_json::to_vec_pretty(&manifest)?)?;
        let sums = sha256sums(self.out_dir)?;
        fs::write(self.out_dir.join("SHA256SUMS"), sums.as_bytes())?;
        log("done");
        Ok(manifest)
    }

    fn reservations(&self) -> Result<(HashSet<H>, HashSet<String>, Value)> {
        let rules = &self.rules;
        let mut texts = HashSet::new();
        let mut groups = HashSet::new();
        let mut meta = Vec::new();
        let pinned: HashSet<PathBuf> = rules.reservations.iter().map(|r| canon(&self.repo, &r.path)).collect();
        for g in &rules.reservation_globs {
            // `dir/*/name` or `dir/*.json`: one wildcard component.
            let (dir, pat) = g.rsplit_once('/').context("reservation glob needs a directory")?;
            let (base, sub) = match dir.split_once("/*") {
                Some((b, rest)) => (b.to_string(), Some(rest.trim_start_matches('/').to_string())),
                None => (dir.to_string(), None),
            };
            let base = canon(&self.repo, &base);
            let Ok(rd) = fs::read_dir(&base) else { continue };
            for e in rd.flatten() {
                let cand = match &sub {
                    Some(s) => {
                        let p = if s.is_empty() { e.path().join(pat) } else { e.path().join(s).join(pat) };
                        if !p.exists() {
                            continue;
                        }
                        p
                    }
                    None => {
                        let name = e.file_name().to_string_lossy().to_string();
                        if !crate::plan::glob_match(pat, &name) {
                            continue;
                        }
                        e.path()
                    }
                };
                let c = fs::canonicalize(&cand)?;
                ensure!(pinned.contains(&c), "reservation file {} is not pinned in the rules", c.strip_prefix(&self.repo).unwrap_or(&c).display());
            }
        }
        for r in &rules.reservations {
            let p = canon(&self.repo, &r.path);
            let b = fs::read(&p).with_context(|| format!("reservation {}", r.path))?;
            ensure!(sha_hex(&b) == r.sha256, "reservation {} changed (sha256 differs from the rules)", r.path);
            let before = texts.len();
            let mut add = |v: &Value| {
                let t = v.get("text_sha256").and_then(|x| x.as_str()).and_then(hx).or_else(|| v.get("text").and_then(|x| x.as_str()).map(|s| skeleton::sha256(s.as_bytes())));
                if let Some(t) = t {
                    texts.insert(t);
                }
                if let Some(g) = v.get("source_group").and_then(|x| x.as_str()) {
                    groups.insert(g.to_string());
                }
            };
            match r.kind.as_str() {
                "rows_manifest" => {
                    let v: Value = serde_json::from_slice(&b)?;
                    for row in v.get("rows").and_then(|x| x.as_array()).context("rows_manifest without rows")? {
                        add(row);
                    }
                }
                "view_rows" => {
                    for l in b.split(|c| *c == b'\n').filter(|l| !l.is_empty()) {
                        add(&serde_json::from_slice::<Value>(l)?);
                    }
                }
                k => bail!("unknown reservation kind {k}"),
            }
            meta.push(json!({"label": r.label, "sha256": r.sha256, "kind": r.kind, "texts": texts.len() - before}));
        }
        Ok((texts, groups, Value::Array(meta)))
    }

    fn voices(&self, requests: &HashMap<(String, String), Ev>) -> Result<HashMap<(String, String), Value>> {
        let mut out = HashMap::new();
        let sources: BTreeSet<&String> = requests.keys().map(|(s, _)| s).collect();
        let mut cache = BlobCache::new(&self.store.join("packs"));
        for s in sources {
            let files: Vec<String> = self
                .db
                .prepare("SELECT file_sha256 FROM source_input WHERE source_id = ?1 AND role = 'voices' ORDER BY rel_path")?
                .query_map([s], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for f in files {
                let n: i64 = self.db.query_row("SELECT COALESCE(n_lines, 0) FROM file WHERE file_sha256 = ?1", [&f], |r| r.get(0))?;
                let all: BTreeSet<i64> = (0..n).collect();
                archive::rebuild_lines(self.db, &mut cache, &f, &all, &mut |_, bytes, _| {
                    if let Ok(v) = serde_json::from_slice::<Value>(bytes) {
                        if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
                            out.insert((s.clone(), id.to_string()), v);
                        }
                    }
                    Ok(())
                })?;
            }
        }
        Ok(out)
    }

    fn rights_own(&self, _t: &H, a: &TA, recs: &[Rec], lines: &HashMap<(String, i64), LineInfo>, overrides: &HashSet<&str>) -> Result<RightsOwn, String> {
        let rules = &self.rules;
        let mut ro = RightsOwn::default();
        let mut rank: Option<u8> = None;
        for r in recs {
            if let Some(l) = &r.licence {
                if let Some(i) = licence_rule(rules, l) {
                    let lr = &rules.licences[i];
                    let k = class_rank(&lr.class);
                    if rank.is_none_or(|x| k > x) {
                        rank = Some(k);
                        ro.class = Some(lr.class.clone());
                        ro.licence_id = lr.licence_id.clone();
                    } else if rank == Some(k) && ro.licence_id.is_none() {
                        ro.licence_id = lr.licence_id.clone();
                    }
                    ro.recorded.insert(l.clone());
                    if lr.class == "uspto_pre2019" {
                        let li = lines.get(&(r.file.clone(), r.line_no));
                        let ok = li.is_some_and(|li| {
                            li.filing_date.as_deref().is_some_and(|d| d < rules.uspto_cutoff.as_str())
                                && li.publication_date.as_deref().is_some_and(|d| d < rules.uspto_cutoff.as_str())
                        });
                        if !ok {
                            return Err("uspto_date_rule".into());
                        }
                    }
                }
            }
            if let Some(li) = lines.get(&(r.file.clone(), r.line_no)) {
                let coll = li.collection.clone().or(r.collection.clone());
                if let Some(f) = li.flag {
                    ro.flags.insert(f);
                    if !f && !a.published {
                        match coll.as_deref() {
                            Some(c) if overrides.contains(c) => {
                                ro.flag_override = true;
                            }
                            _ => return Err("redistribute_text_false".into()),
                        }
                    }
                }
                if let Some(c) = &coll {
                    ro.collections.insert(c.clone());
                }
                // The best attribution line: the first with an attribution, by role preference.
                let pref = ["pool_document", "eval_human", "split_document", "frontier_source_passage", "published_record", "laptop_labelled_record", "parity_record", "local_record", "frontier_positive", "hosted_capture"];
                let rk = pref.iter().position(|x| *x == r.role).unwrap_or(pref.len());
                if !li.whole_file && (li.attribution.is_some() || li.source_url.is_some() || li.title.is_some()) && ro.best.as_ref().is_none_or(|(k, _)| rk < *k) {
                    ro.best = Some((rk, (r.file.clone(), r.line_no)));
                }
            } else if let Some(c) = &r.collection {
                ro.collections.insert(c.clone());
            }
        }
        if a.published {
            ro.published = true;
        }
        // Attribution for CC BY and CC BY-SA texts with their own licence.
        if matches!(ro.class.as_deref(), Some("cc_by") | Some("cc_by_sa")) && !a.is_gen {
            let credit = ro.collections.iter().any(|c| rules.collection_credits.contains_key(c));
            let ok = ro.best.as_ref().and_then(|(_, k)| lines.get(k)).is_some_and(|li| {
                (li.attribution.is_some() || li.authors.is_some() || credit) && (li.source_url.is_some() || li.evidence_url.is_some() || li.history_url.is_some())
            });
            if !ok {
                return Err("attribution_missing".into());
            }
        }
        Ok(ro)
    }

    fn resolve_generator(&self, recs: &[Rec], events: &[&Ev], lines: &HashMap<(String, i64), LineInfo>) -> Option<usize> {
        let mut ids: Vec<String> = Vec::new();
        for e in events {
            ids.extend(e.generator_key.clone());
            ids.extend(e.reported_model.clone());
            ids.extend(e.requested_model.clone());
            ids.extend(e.generator_label.clone());
        }
        for r in recs {
            ids.extend(r.generator_key.clone());
            ids.extend(r.generator_label.clone());
            if let Some(x) = &r.extra {
                for k in ["model_reported", "selector_requested", "ai_identity"] {
                    if let Some(s) = x.get(k).and_then(|v| v.as_str()) {
                        ids.push(s.into());
                    }
                }
            }
            if matches!(r.role.as_str(), "frontier_positive") {
                ids.extend(r.dataset.clone());
            }
            if let Some(li) = lines.get(&(r.file.clone(), r.line_no)) {
                ids.extend(li.gen_ids.iter().cloned());
            }
        }
        for id in &ids {
            let l = id.to_ascii_lowercase();
            if let Some(i) = self.rules.generators.iter().position(|g| g.patterns.iter().any(|p| l.contains(p.as_str()))) {
                return Some(i);
            }
        }
        None
    }

    fn voice_failure(&self, events: &[&Ev], requests: &HashMap<(String, String), Ev>, voices: &HashMap<(String, String), Value>) -> Option<String> {
        for e in events.iter().filter(|e| e.kind == "frontier_attempt") {
            let req = requests.get(&(e.source_id.clone(), e.request_id.clone().unwrap_or_default()))?;
            let vid = req.params.as_ref().and_then(|p| p.get("voice_id")).and_then(|x| x.as_str())?;
            let v = voices.get(&(e.source_id.clone(), vid.to_string()))?;
            if let Some(d) = v.get("derived_from").filter(|x| x.is_object()) {
                let doc = d.get("doc_id").and_then(|x| x.as_str())?;
                match self.doc_licence(doc) {
                    Some((lic, _, _)) => {
                        let ok = licence_rule(&self.rules, &lic).is_some_and(|i| class_rank(&self.rules.licences[i].class) != 100);
                        if !ok {
                            return Some("voice_derived_from_excluded_licence".into());
                        }
                    }
                    None => return Some("voice_derived_from_unlicensed".into()),
                }
            }
        }
        None
    }

    /// Licence, collection and text of a record id (through the record id index).
    fn doc_licence(&self, id: &str) -> Option<(String, Option<String>, Option<String>)> {
        self.db
            .prepare_cached("SELECT licence, collection, text_sha256 FROM record INDEXED BY record_id WHERE record_id = ?1 AND licence IS NOT NULL ORDER BY record_uid LIMIT 1")
            .ok()?
            .query_row([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?)))
            .ok()
    }

    #[allow(clippy::too_many_arguments)]
    fn write_texts(
        &self, released: &[H], kinds: &HashMap<H, &'static str>, ta: &HashMap<H, TA>, splits: &Intern, recs: &HashMap<H, Vec<Rec>>,
        published: &BTreeMap<String, String>, gate: &mut Gate, files: &mut BTreeMap<String, Value>, counts: &mut Counter,
    ) -> Result<()> {
        let db = self.db;
        let tok: Option<String> = db.query_row("SELECT value FROM meta WHERE key = 'tokenizer_sha256'", [], |r| r.get(0)).ok();
        let mut cache = BlobCache::new(&self.store.join("packs"));
        let mut tbl = Table::new(self.out_dir, "texts", gate, &["text"])?;
        let mut st = db.prepare_cached("SELECT n_bytes, n_chars, n_words, n_lines, language, language_score FROM text WHERE text_sha256 = ?1")?;
        let mut tk = db.prepare_cached("SELECT n_tokens FROM text_tokens WHERE text_sha256 = ?1 AND tokenizer_sha256 = ?2")?;
        let mut words = 0i64;
        let mut bytes_total = 0i64;
        for t in released {
            let h = hs(t);
            let (nb, nc, nw, nl, lang, ls): (i64, i64, i64, i64, String, f64) =
                st.query_row([&h], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
            let ntok: Option<i64> = tk.query_row([&h, tok.as_deref().unwrap_or("")], |r| r.get(0)).ok();
            let text = cache.text(db, t)?;
            ensure!(text.len() as i64 == nb, "text {h} length differs");
            let a = &ta[t];
            let kind = kinds[t];
            let pubs: BTreeSet<&String> = recs.get(t).into_iter().flatten().filter_map(|r| published.get(&r.source_id)).collect();
            let split = splits.get(a.split);
            tbl.row(&json!({
                "text_sha256": h, "text": text, "kind": text_kind(kind), "n_bytes": nb, "n_chars": nc, "n_words": nw, "n_lines": nl,
                "n_tokens": ntok, "language": lang, "language_score": ls, "canonical_split": split,
                "evaluation_only": a.eo, "group_evaluation_only": a.geo, "published_before": pubs,
            }))?;
            counts.add(format!("kind:{}", text_kind(kind)), 1);
            counts.add(format!("split:{}:{}", text_kind(kind), split.unwrap_or("none")), 1);
            counts.add(format!("words:{}", text_kind(kind)), nw as u64);
            counts.add(format!("bytes:{}", text_kind(kind)), nb as u64);
            if !pubs.is_empty() {
                counts.add(format!("published_before:{}", text_kind(kind)), 1);
            }
            if kind == "human" {
                let c = recs.get(t).into_iter().flatten().find_map(|r| r.collection.clone()).unwrap_or_else(|| "(none)".into());
                counts.add(format!("collection:{c}"), 1);
            }
            words += nw;
            bytes_total += nb;
        }
        counts.add("total:texts", released.len() as u64);
        counts.add("total:words", words as u64);
        counts.add("total:bytes", bytes_total as u64);
        tbl.finish(files)?;
        if let Some(m) = files.get_mut("texts.jsonl.zst") {
            m["tokenizer_sha256"] = json!(tok);
        }
        Ok(())
    }

    fn write_records(
        &self, released: &[H], recs: &HashMap<H, Vec<Rec>>, lines: &HashMap<(String, i64), LineInfo>, gate: &mut Gate,
        files: &mut BTreeMap<String, Value>, counts: &mut Counter,
    ) -> Result<HashSet<String>> {
        // Share evidence carries verbatim spans of released texts (`source_text`, `replacement(s)`): text fields,
        // checked for operator values and private strings like every other text.
        let mut tbl = Table::new(self.out_dir, "records", gate, &["source_text", "replacement", "replacements"])?;
        let mut seen: HashSet<String> = HashSet::new();
        let mut ids = HashSet::new();
        for t in released {
            for r in recs.get(t).into_iter().flatten() {
                if !self.rules.record_roles.contains(&r.role) || !seen.insert(r.line_sha.clone()) {
                    continue;
                }
                let li = lines.get(&(r.file.clone(), r.line_no));
                let prov: Value = match li {
                    Some(li) if !li.whole_file => li.provenance.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null),
                    _ => Value::Null,
                };
                if let Some(id) = &r.record_id {
                    ids.insert(id.clone());
                }
                tbl.row(&json!({
                    "record_key": sha_hex(format!("{}:{}:{}", r.file, r.line_no, r.role).as_bytes()),
                    "line_sha256": r.line_sha, "role": PublicNames::get(&self.rules.public_names.roles, &r.role), "record_id": r.record_id, "text_sha256": hs(t),
                    "dataset": r.dataset, "collection": r.collection, "domain": r.domain, "genre": r.genre, "source_work": r.source_work,
                    "family": r.family, "source_group": r.source_group, "writer_key": r.writer_key, "original_doc_id": r.original_doc_id,
                    "doc_date": r.doc_date, "composition_date": r.composition_date, "licence": r.licence, "cohort_id": r.cohort_id,
                    "origin": r.origin, "share": r.share,
                    "ai_byte_spans": r.ai_byte_spans.as_deref().and_then(|s| serde_json::from_str::<Value>(s).ok()),
                    "generator_label": r.generator_label, "generator_key": r.generator_key, "operation": r.operation,
                    "recipe_sha256": r.recipe_sha256, "parent_record_id": r.parent_record_id, "parent_text_sha256": r.parent_text_sha256,
                    "split_hint": r.split_hint, "provenance": prov,
                }))?;
                counts.add(format!("records:{}", PublicNames::get(&self.rules.public_names.roles, &r.role)), 1);
            }
        }
        tbl.finish(files)?;
        Ok(ids)
    }

    #[allow(clippy::too_many_arguments)]
    fn write_rights(
        &self, released: &[H], kinds: &HashMap<H, &'static str>, own: &HashMap<H, Result<RightsOwn, String>>, anc: &dyn Fn(&H) -> Vec<H>,
        gen_of: &HashMap<H, usize>, lines: &HashMap<(String, i64), LineInfo>, gate: &mut Gate, files: &mut BTreeMap<String, Value>, counts: &mut Counter,
    ) -> Result<()> {
        let rules = &self.rules;
        let mut tbl = Table::new(self.out_dir, "rights", gate, &[])?;
        let attribution = |ro: &RightsOwn| -> Value {
            let Some(li) = ro.best.as_ref().and_then(|(_, k)| lines.get(k)) else { return Value::Null };
            let credit: Vec<&String> = ro.collections.iter().filter_map(|c| rules.collection_credits.get(c)).collect();
            json!({
                "title": li.title, "authors": li.authors, "attribution": li.attribution, "source_url": li.source_url,
                "history_url": li.history_url, "evidence_url": li.evidence_url, "collection_credit": credit,
                "licence_line": li.licence, "licence_url": li.licence_url, "share_alike": li.share_alike, "notes": li.notes, "conditions": li.conditions,
                "catalog_statement": li.catalog_statement, "jurisdiction": li.jurisdiction,
            })
        };
        for t in released {
            let Some(Ok(ro)) = own.get(t) else { bail!("released text {} has no rights resolution", hs(t)) };
            let chain = anc(t);
            let mut class = ro.class.clone();
            let mut licence_id = ro.licence_id.clone();
            let mut share_alike = class.as_deref() == Some("cc_by_sa");
            let mut flag_override = ro.flag_override;
            let mut inherited = Vec::new();
            for p in &chain {
                let Some(Ok(pr)) = own.get(p) else { bail!("ancestor {} of {} has no rights resolution", hs(p), hs(t)) };
                if pr.class.as_deref().is_some_and(|c| class.as_deref().is_none_or(|x| class_rank(c) > class_rank(x))) {
                    class = pr.class.clone();
                    licence_id = pr.licence_id.clone();
                }
                share_alike |= pr.class.as_deref() == Some("cc_by_sa");
                flag_override |= pr.flag_override;
                inherited.push(json!({"text_sha256": hs(p), "licence_id": pr.licence_id, "class": pr.class, "attribution_ref": hs(p), "attribution": attribution(pr)}));
            }
            let Some(class) = class else { bail!("released text {} has no licence class", hs(t)) };
            ensure!(CLASS_ORDER.contains(&class.as_str()), "released text {} has class {class}", hs(t));
            let lid = licence_id.clone().unwrap_or_else(|| class.clone());
            let lfile = rules.licence_ids.get(&lid).map(|x| x.file.clone());
            ensure!(lfile.is_some(), "licence id {lid} has no LICENSES entry");
            let kind = kinds[t];
            let gm = gen_of.get(t).map(|i| &rules.generators[*i]);
            let generated = gm.map(|g| {
                json!({"licence_id": "CC-BY-SA-4.0", "licence_file": rules.licence_ids.get("CC-BY-SA-4.0").map(|x| x.file.clone()),
                       "changes": "model-written or model-rewritten text added to the source passage(s) listed in inherited_from",
                       "generator": g.key, "model_notice": g.notice_files})
            });
            if gm.is_some() {
                share_alike = true;
            }
            tbl.row(&json!({
                "text_sha256": hs(t), "kind": text_kind(kind), "licence_id": lid, "licence_recorded": ro.recorded,
                "licence_url": rules.licence_ids.get(&lid).and_then(|x| x.url.clone()), "licence_file": lfile, "class": class,
                "share_alike": share_alike, "attribution": attribution(ro), "inherited_from": inherited, "generated_additions": generated,
                "redistribute_text_recorded": ro.flags,
                "redistribute_text_basis": if flag_override { json!({"recorded": false, "decision": rules.flag_override.decision}) }
                    else if ro.published { json!({"published_before": true}) } else { Value::Null },
                "collections": ro.collections,
            }))?;
            counts.add(format!("class:{class}"), 1);
            if flag_override {
                counts.add("flag_override_texts", 1);
            }
        }
        tbl.finish(files)?;
        Ok(())
    }

    /// Opaque public id of a store view (store view ids embed relative file paths, which are not published).
    fn public_view_id(&self, exp: &str, vid: &str) -> String {
        let e = PublicNames::get(&self.rules.public_names.experiments, exp);
        format!("{e}:view-{}", &sha_hex(format!("slop_ninja_public_candidate_v1:view:{vid}").as_bytes())[..16])
    }

    fn write_splits(&self, released: &HashSet<H>, gate: &mut Gate, files: &mut BTreeMap<String, Value>, counts: &mut Counter) -> Result<HashSet<String>> {
        let db = self.db;
        // Ordinals from view membership.
        let mut ord: HashMap<(String, String), i64> = HashMap::new();
        let mut released_rows: HashMap<String, u64> = HashMap::new();
        {
            let mut st = db.prepare("SELECT view_id, ordinal, record_id FROM view_member INDEXED BY view_member_text WHERE text_sha256 = ?1")?;
            let mut order: Vec<&H> = released.iter().collect();
            order.sort();
            for t in order {
                let mut q = st.query([hs(t)])?;
                while let Some(r) = q.next()? {
                    let vid: String = r.get(0)?;
                    *released_rows.entry(vid.clone()).or_default() += 1;
                    if let Some(rid) = opt(r, 2)? {
                        ord.entry((vid, rid)).or_insert(r.get(1)?);
                    }
                }
            }
        }
        let mut rows: Vec<(String, String, String, String, String, Option<String>)> = Vec::new();
        {
            let mut st = db.prepare("SELECT experiment, record_id, text_sha256, source_group, role, view_id FROM split_member INDEXED BY split_member_text WHERE text_sha256 = ?1")?;
            let mut order: Vec<&H> = released.iter().collect();
            order.sort();
            for t in order {
                let mut q = st.query([hs(t)])?;
                while let Some(r) = q.next()? {
                    rows.push((r.get(0)?, r.get(5)?, r.get(4)?, r.get(2)?, r.get(1)?, opt(r, 3)?));
                }
            }
        }
        rows.sort();
        let names = &self.rules.public_names;
        let mut tbl = Table::new(self.out_dir, "splits", gate, &[])?;
        let mut exps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut groups = HashSet::new();
        for (exp, vid, role, t, rid, group) in &rows {
            tbl.row(&json!({"text_sha256": t, "experiment": PublicNames::get(&names.experiments, exp), "view_id": self.public_view_id(exp, vid),
                            "role": role, "record_id": rid, "ordinal": ord.get(&(vid.clone(), rid.clone())), "source_group": group}))?;
            exps.entry(exp.clone()).or_default().insert(vid.clone());
            groups.extend(group.clone());
            counts.add(format!("splits:{role}"), 1);
        }
        tbl.finish(files)?;
        let mut views: HashMap<String, (String, String, i64)> = HashMap::new();
        {
            let mut st = db.prepare("SELECT view_id, kind, file_sha256, n_rows FROM view")?;
            let mut q = st.query([])?;
            while let Some(r) = q.next()? {
                views.insert(r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?));
            }
        }
        let mut tbl = Table::new(self.out_dir, "experiments", gate, &[])?;
        for (exp, vids) in &exps {
            let vs: Vec<Value> = vids
                .iter()
                .map(|v| {
                    let (k, f, n) = views.get(v).cloned().unwrap_or_default();
                    json!({"view_id": self.public_view_id(exp, v), "kind": PublicNames::get(&names.view_kinds, &k), "file_sha256": f, "n_rows_total": n,
                           "n_rows_released": released_rows.get(v).copied().unwrap_or(0)})
                })
                .collect();
            let kind = vs.first().and_then(|v| v["kind"].as_str()).unwrap_or("").to_string();
            let pexp = PublicNames::get(&names.experiments, exp);
            let desc = self.rules.experiment_descriptions.iter().find(|(k, _)| pexp.starts_with(k.as_str())).map(|(_, d)| d.clone());
            tbl.row(&json!({"experiment": pexp, "kind": kind, "description": desc, "views": vs}))?;
        }
        tbl.finish(files)?;
        Ok(groups)
    }

    #[allow(clippy::too_many_arguments)]
    fn write_pangram(
        &self, obs: &[Obs], released: &HashSet<H>, hash_only: &HashSet<H>, reason: &Reasons, ta: &HashMap<H, TA>,
        kinds: &HashMap<H, &'static str>, splits: &Intern, recs: &HashMap<H, Vec<Rec>>, record_ids: &HashSet<String>, gate: &mut Gate,
        files: &mut BTreeMap<String, Value>,
    ) -> Result<Value> {
        let mut tbl = Table::new(self.out_dir, "pangram_observations", gate, &[])?;
        let mut dropped = Counter::default();
        let mut counts = Counter::default();
        let none = TA::default();
        for o in obs.iter().filter(|o| o.detector == "pangram") {
            let ctx = o.context.clone().unwrap_or_default();
            let Some(t) = o.text else {
                dropped.add("no_text_digest", 1);
                continue;
            };
            if self.rules.private_contexts.contains(&ctx) {
                dropped.add("private_context", 1);
                continue;
            }
            let included = released.contains(&t);
            if !included && !hash_only.contains(&t) {
                let r: String = reason.get(&t).map(String::from).unwrap_or_else(|| if ta.contains_key(&t) { "not_selected".into() } else { "no_provenance_record".into() });
                let r = if r.starts_with("unused_holdout") { "unused_holdout".into() } else { r };
                dropped.add(r, 1);
                continue;
            }
            let pctx = PublicNames::get(&self.rules.public_names.contexts, &ctx).to_string();
            let p = o.pred.clone().unwrap_or(Value::Null);
            let windows: Vec<Value> = p
                .get("windows")
                .and_then(|w| w.as_array())
                .map(|a| {
                    a.iter()
                        .map(|w| {
                            Value::Object(pick(w, &["start_index", "end_index", "label", "ai_assistance_score", "calibrated_confidence", "class_probabilities",
                                                    "confidence", "humanizer_score", "is_humanized", "token_length", "word_count", "score"]))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut row = json!({
                "observation_id": sha_hex(format!("slop_ninja_public_candidate_v1:observation:{}", o.obs_key).as_bytes()),
                "detector": o.detector, "detector_version": o.version, "campaign": pctx, "observed_utc": o.observed_utc,
                "text_sha256": hs(&t), "text_included": included,
                "fraction_ai": o.fa, "fraction_ai_assisted": o.faa, "fraction_human": o.fh, "score": o.score,
                "headline": p.get("headline"), "prediction": p.get("prediction"), "prediction_short": p.get("prediction_short"),
                "num_ai_segments": p.get("num_ai_segments"), "num_ai_assisted_segments": p.get("num_ai_assisted_segments"),
                "num_human_segments": p.get("num_human_segments"), "windows": windows, "status": o.status,
                "role": "comparator_evidence_only",
                "item_id": o.item_id.clone().filter(|i| included && record_ids.contains(i)),
            });
            if !included {
                let a = ta.get(&t).unwrap_or(&none);
                let rr = recs.get(&t).map(|v| v.as_slice()).unwrap_or(&[]);
                let first = |f: &dyn Fn(&Rec) -> Option<String>| rr.iter().find_map(f);
                let (nw, ntok): (Option<i64>, Option<i64>) = self
                    .db
                    .prepare_cached("SELECT t.n_words, (SELECT k.n_tokens FROM text_tokens k WHERE k.text_sha256 = t.text_sha256 LIMIT 1) FROM text t WHERE t.text_sha256 = ?1")?
                    .query_row([hs(&t)], |r| Ok((r.get(0)?, r.get(1)?)))
                    .unwrap_or((None, None));
                row["label_context"] = json!({
                    "kind": text_kind(kinds.get(&t).copied().unwrap_or_else(|| kind_of(a))),
                    "collection": first(&|r| r.collection.clone()), "domain": first(&|r| r.domain.clone()),
                    "origin": first(&|r| r.origin.clone()), "share": rr.iter().find_map(|r| r.share),
                    "generator_key": first(&|r| r.generator_key.clone()), "canonical_split": splits.get(a.split),
                    "n_words": nw, "n_tokens": ntok,
                    "source_group_sha256": first(&|r| r.source_group.clone()).map(|g| sha_hex(g.as_bytes())),
                    "exclusion_reason": reason.get(&t),
                });
            }
            tbl.row(&row)?;
            counts.add(if included { "with_text" } else { "hash_only" }, 1);
            counts.add(format!("{}:{pctx}", if included { "with_text" } else { "hash_only" }), 1);
        }
        tbl.finish(files)?;
        Ok(json!({"counts": counts.json(), "dropped": dropped.json()}))
    }

    #[allow(clippy::too_many_arguments)]
    fn write_generations(
        &self, released: &[H], kinds: &HashMap<H, &'static str>, recs: &HashMap<H, Vec<Rec>>, events: &[Ev],
        requests: &HashMap<(String, String), Ev>, attempts: &HashMap<(String, String), Vec<(i64, String)>>,
        voices: &HashMap<(String, String), Value>, gen_of: &HashMap<H, usize>, lines: &HashMap<(String, i64), LineInfo>,
        groups: &HashSet<String>, record_ids: &HashSet<String>, private: &[String], gate: &mut Gate, files: &mut BTreeMap<String, Value>,
    ) -> Result<(Value, BTreeSet<(String, String)>, Value, BTreeSet<usize>)> {
        let rules = &self.rules;
        let mut inventors: BTreeSet<usize> = BTreeSet::new();
        let mut style_counts = Counter::default();
        let db = self.db;
        let mut cache = BlobCache::new(&self.store.join("packs"));
        let released_set: HashSet<H> = released.iter().copied().collect();
        let mut by_out: HashMap<H, Vec<&Ev>> = HashMap::new();
        for e in events {
            if let Some(o) = e.output_text {
                if released_set.contains(&o) {
                    by_out.entry(o).or_default().push(e);
                }
            }
        }
        let mut prompts = Counter::default();
        let mut meta = Counter::default();
        let mut used_voices = BTreeSet::new();
        let mut hosted = Table::new(self.out_dir, "hosted_generations", gate, &["prompt", "system_prompt", "response_text"])?;
        let mut hosted_rows = Vec::new();
        let mut open_rows = Vec::new();
        let mut instructions: HashMap<String, Option<String>> = HashMap::new();
        for t in released {
            let kind = kinds[t];
            if kind == "human" {
                continue;
            }
            let g = gen_of.get(t).map(|i| &rules.generators[*i]);
            let rr = recs.get(t).map(|v| v.as_slice()).unwrap_or(&[]);
            let parent_h: Option<H> = rr.iter().find_map(|r| r.parent_text_sha256.as_deref().and_then(hx)).filter(|p| p != t);
            let parent_text: Option<String> = match parent_h {
                Some(p) if released_set.contains(&p) => Some(cache.text(db, &p)?),
                _ => None,
            };
            let gen_rec = rr.iter().find(|r| matches!(r.role.as_str(), "frontier_positive" | "local_positive" | "local_output" | "local_record" | "hosted_capture" | "hosted_style_positive" | "published_record" | "parity_record"));
            let evs = by_out.get(t).cloned().unwrap_or_default();
            let mut any = false;
            for e in &evs {
                if !matches!(e.status.as_str(), "accepted" | "captured" | "accepted_record") {
                    continue;
                }
                any = true;
                let req = requests.get(&(e.source_id.clone(), e.request_id.clone().unwrap_or_default()));
                let rp = req.and_then(|r| r.params.clone()).unwrap_or(Value::Null);
                let ep = e.params.clone().unwrap_or(Value::Null);
                // Template and operation.
                let local_pos = rr.iter().find(|r| r.role == "local_positive" && r.extra.as_ref().is_some_and(|x| {
                    x.get("requests").and_then(|a| a.as_array()).is_some_and(|a| a.iter().any(|q| q.get("request_id").and_then(|v| v.as_str()) == e.request_id.as_deref()))
                }));
                let template: Option<String> = match e.kind.as_str() {
                    "frontier_attempt" => rp.get("template").and_then(|x| x.as_str()).map(String::from),
                    "hosted_capture" => find_key(&ep, "profile").and_then(|p| p.get("id")).and_then(|x| x.as_str()).map(|p| format!("detector-hosted-v1:{p}")),
                    "local_generate" => local_pos.and_then(|r| r.extra.as_ref()).and_then(|x| x.get("template")).and_then(|x| x.as_str()).map(String::from)
                        .or_else(|| e.operation.clone()),
                    _ => e.operation.clone(),
                };
                let operation = rp.get("kind").and_then(|x| x.as_str()).map(String::from).or(e.operation.clone()).or_else(|| gen_rec.and_then(|r| r.operation.clone()));
                // Prompt: digest-checked blob, allowlisted template, embedded parent.
                let mut prompt: Option<String> = None;
                let mut withheld: Option<&str> = None;
                let mut emb = "not_checked";
                match (&e.prompt_sha256, &template) {
                    (None, _) => withheld = Some("prompt_not_archived"),
                    (_, None) => withheld = Some("template_unknown"),
                    (Some(ps), Some(tp)) => {
                        if !rules.templates.contains(tp) {
                            withheld = Some("template_not_allowlisted");
                        } else {
                            match hx(ps).map(|h| cache.bytes(db, &h)) {
                                Some(Ok(b)) => {
                                    // BlobCache checks the digest; recompute for the record.
                                    if sha_hex(&b) != *ps {
                                        withheld = Some("prompt_digest_mismatch");
                                    } else {
                                        let span = rp.get("source_byte_span").and_then(|x| x.as_array()).and_then(|a| Some((a.first()?.as_u64()? as usize, a.get(1)?.as_u64()? as usize)));
                                        emb = embedding(&b, parent_text.as_deref().map(str::as_bytes), span);
                                        let ok = prompt_ok(rules, tp, emb, parent_h.is_some(), parent_text.is_some());
                                        if !ok {
                                            withheld = Some("parent_not_embedded");
                                        } else {
                                            match String::from_utf8(b) {
                                                Ok(s) => prompt = Some(s),
                                                Err(_) => withheld = Some("prompt_not_utf8"),
                                            }
                                        }
                                    }
                                }
                                _ => withheld = Some("prompt_blob_missing"),
                            }
                        }
                    }
                }
                // Private seed material: a withheld voice, or a private string anywhere in the prompt.
                let voice_id = rp.get("voice_id").and_then(|x| x.as_str()).map(String::from);
                let voice = voice_id.as_ref().and_then(|v| voices.get(&(e.source_id.clone(), v.clone())));
                let voice_withheld = voice.is_some_and(|v| voice_withheld(&self.rules, v, private));
                if voice_withheld || prompt.as_deref().is_some_and(|p| has_private(private, p)) {
                    if prompt.take().is_some() || withheld.is_none() {
                        withheld = Some("private_seed_voice");
                    }
                }
                prompts.add(format!("{}:{}", template.clone().unwrap_or_else(|| "(none)".into()), withheld.unwrap_or("published")), 1);
                if template.as_deref().is_some_and(|x| x.ends_with("fix_slop") || x.ends_with("fix-slop")) && prompt.is_some() {
                    prompts.add("fix_slop_notice_required", 1);
                }
                let response_text = match (&e.response_sha256, operation.as_deref()) {
                    (Some(r), Some(op)) if ["span", "continuation", "splice", "continue"].contains(&op) => match hx(r).map(|h| cache.text(db, &h)) {
                        Some(Ok(s)) if s.as_bytes() != cache.bytes(db, t)?.as_slice() => Some(s),
                        _ => None,
                    },
                    _ => None,
                };
                let share_evidence = gen_rec.map(|r| json!({"share": r.share, "ai_byte_spans": r.ai_byte_spans.as_deref().and_then(|s| serde_json::from_str::<Value>(s).ok())}));
                if kind == "hosted" {
                    let argv = ep.get("argv").and_then(|x| x.as_array()).map(|a| parse_argv(a)).unwrap_or_default();
                    let transport = ep.get("transport").and_then(|x| x.as_str()).or_else(|| ep.get("backend").and_then(|x| x.as_str()).map(|b| if b == "codex" { "codex-cli" } else { "claude-cli" }));
                    let client = transport.filter(|x| *x == "claude-cli" || *x == "codex-cli");
                    let provider = g.map(|g| g.provider.clone());
                    let mut system_prompt = argv.get("system_prompt").and_then(|x| x.as_str()).map(String::from);
                    if system_prompt.is_none() && argv.get("system_prompt_from_instructions_file").is_some() {
                        let s = instructions.entry(e.source_id.clone()).or_insert_with(|| {
                            db.query_row("SELECT file_sha256 FROM source_input WHERE source_id = ?1 AND rel_path = 'instructions.md'", [&e.source_id], |r| r.get::<_, String>(0))
                                .ok()
                                .and_then(|f| hx(&f))
                                .and_then(|h| cache.text(db, &h).ok())
                        });
                        system_prompt = s.clone();
                    }
                    let system_prompt_withheld = system_prompt.as_deref().is_some_and(|p| has_private(private, p));
                    if system_prompt_withheld {
                        system_prompt = None;
                    }
                    let usage = ep.get("usage").cloned().unwrap_or(Value::Null);
                    let tokens = json!({
                        "input": find_key(&usage, "input_tokens"), "output": find_key(&usage, "output_tokens"),
                        "thinking": find_key(&usage, "thinking_tokens").or_else(|| find_key(&usage, "reasoning_output_tokens")),
                    });
                    let earlier: Vec<Value> = attempts
                        .get(&(e.source_id.clone(), e.request_id.clone().unwrap_or_default()))
                        .map(|v| v.iter().filter(|(a, _)| Some(*a) != e.attempt).map(|(a, s)| json!({"attempt": a, "status": s})).collect())
                        .unwrap_or_default();
                    // The voice: listed in voices.jsonl unless withheld; a model-written voice names its inventor's notice.
                    let mut vinv_idx: Option<usize> = None;
                    if let (Some(v), Some(vv)) = (&voice_id, voice) {
                        if voice_withheld {
                            prompts.add("voice_withheld:private_seed_voice", 1);
                        } else {
                            used_voices.insert((e.source_id.clone(), v.clone()));
                            vinv_idx = voice_inventor(&self.rules, vv)?;
                            inventors.extend(vinv_idx);
                        }
                    }
                    let vinv = vinv_idx.map(|i| &rules.generators[i]);
                    let fx = gen_rec.and_then(|r| r.extra.clone()).unwrap_or(Value::Null);
                    hosted_rows.push(json!({
                        "event_key": e.event_key, "request_id": e.request_id, "attempt": e.attempt,
                        "wave": e.extra.as_ref().and_then(|x| x.get("wave")).cloned(),
                        "provider": provider, "generator": g.map(|g| g.key.clone()),
                        "selector_requested": e.requested_model.clone().or_else(|| rp.get("selector").and_then(|x| x.as_str()).map(String::from)).or_else(|| ep.get("requested_model").and_then(|x| x.as_str()).map(String::from)),
                        "model_reported": e.reported_model,
                        "identity_basis": fx.get("ai_identity_basis").cloned().unwrap_or_else(|| json!(if e.reported_model.is_some() { "reported" } else { "requested_selector" })),
                        "client": client, "operation": operation, "template": template,
                        "effort": rp.get("effort").cloned().or_else(|| argv.get("effort").cloned()).or_else(|| ep.get("reasoning_effort").cloned()),
                        "max_turns": argv.get("max_turns"), "system_prompt": system_prompt,
                        "system_prompt_withheld": if system_prompt_withheld { json!("private_seed_voice") } else { Value::Null },
                        "prompt": prompt, "prompt_sha256": e.prompt_sha256, "prompt_withheld": withheld, "parent_embedding": emb,
                        "voice_id": voice_id, "voice_withheld": if voice_withheld { json!("private_seed_voice") } else { Value::Null },
                        "voice_inventor_generator": vinv.map(|g| g.key.clone()), "voice_notice_files": vinv.map(|g| g.notice_files.clone()),
                        "voice_ladder": rp.get("voice_ladder").map(|x| Value::Object(allow(x, VOICE_LADDER))),
                        "target_words": rp.get("target_words"),
                        "source_text_sha256": parent_h.map(|p| hs(&p)), "source_byte_span": rp.get("source_byte_span"),
                        "output_text_sha256": hs(t), "response_sha256": e.response_sha256, "response_text": response_text,
                        "share_evidence": share_evidence,
                        "started_utc": e.started_utc, "latency_ms": ep.get("latency_ms"), "tokens": tokens, "earlier_attempts": earlier,
                    }));
                    meta.add(format!("hosted:{}", e.kind), 1);
                } else {
                    let gx = gen_rec.and_then(|r| r.extra.clone()).unwrap_or(Value::Null);
                    let decoding = if e.kind == "local_generate" {
                        Value::Object(pick(&ep, &["seed", "temperature", "top_p", "top_k", "min_p", "repeat_penalty", "n_predict", "max_tokens"]))
                    } else {
                        let req = ep.get("generation.request").cloned().unwrap_or(Value::Null);
                        Value::Object(pick(&req, &["seed", "temperature", "top_p", "top_k", "min_p", "repeat_penalty", "max_tokens", "chat_template_kwargs", "reasoning_format"]))
                    };
                    let binding = ep.get("generation.binding").filter(|b| b.is_object()).map(|b| {
                        let mut o = allow(b, BINDING);
                        if let Some(r) = b.get("runtime").and_then(|x| x.as_str()) {
                            o.insert("runtime".into(), runtime_struct(r));
                        }
                        Value::Object(o)
                    });
                    let style = match local_pos.and_then(|r| r.extra.as_ref()).and_then(|x| x.get("style_ids")) {
                        Some(x) => style_of(&self.rules, x, private, groups, record_ids, &mut inventors, &mut style_counts)?,
                        None => None,
                    };
                    let resp = e.extra.as_ref().and_then(|x| x.get("response")).cloned().unwrap_or(Value::Null);
                    open_rows.push(json!({
                        "event_key": e.event_key, "event_kind": e.kind, "request_id": e.request_id, "attempt": e.attempt,
                        "plan_sha256": local_pos.and_then(|r| r.extra.as_ref()).and_then(|x| x.get("plan_sha256")).cloned(),
                        "generator": g.map(|g| g.key.clone()), "generator_label": e.generator_label, "generator_key": e.generator_key,
                        "model": g.map(|g| json!({"family": g.family, "display": g.display, "checkpoint": g.checkpoint, "licence_id": g.licence_id, "notice": g.notice_files})),
                        "binding": binding, "operation": operation, "template": template,
                        "prompt": prompt, "prompt_sha256": e.prompt_sha256, "prompt_withheld": withheld, "parent_embedding": emb,
                        "decoding": decoding,
                        "decoding_profile": local_pos.and_then(|r| r.extra.as_ref()).and_then(|x| x.get("decoding")).map(|x| Value::Object(allow(x, DECODING_PROFILE))),
                        "style": style,
                        "source_text_sha256": parent_h.map(|p| hs(&p)), "output_text_sha256": hs(t), "response_sha256": e.response_sha256,
                        "response_text": response_text, "share_evidence": share_evidence.clone(),
                        "verdict_reasons": resp.get("verdict_reasons"),
                        "timings": resp.get("timings").map(|x| json!({"prompt_n": x.get("prompt_n"), "predicted_n": x.get("predicted_n")})),
                        "started_utc": e.started_utc.as_deref().and_then(|s| ms_to_iso(s).or_else(|| Some(s.to_string()))),
                        "record_extra_schema": gx.get("schema"),
                    }));
                    meta.add(format!("open:{}", e.kind), 1);
                }
            }
            if !any {
                // A generation without an accepted event: the sanitised generation block of its record.
                let li = rr.iter().find_map(|r| lines.get(&(r.file.clone(), r.line_no)).and_then(|l| l.generation.clone().map(|g| (r, g))));
                let (rec, gblock) = match li {
                    Some((r, g)) => (Some(r), Some(g)),
                    None => (gen_rec, None),
                };
                let mut gblock = gblock;
                let mut prompt = None;
                let mut withheld = Some("no_event_prompt");
                let op = rec.and_then(|r| r.operation.clone()).or_else(|| gblock.as_ref().and_then(|g| g.get("operation")).and_then(|x| x.as_str()).map(String::from));
                if let Some(gb) = gblock.as_mut() {
                    if let Some(Value::String(p)) = gb.as_object_mut().and_then(|m| m.remove("prompt")) {
                        let tp = op.clone().unwrap_or_default();
                        if has_private(private, &p) {
                            withheld = Some("private_seed_voice");
                        } else if rules.templates.contains(&tp) {
                            let emb = embedding(p.as_bytes(), parent_text.as_deref().map(str::as_bytes), None);
                            if prompt_ok(rules, &tp, emb, parent_h.is_some(), parent_text.is_some()) {
                                prompt = Some(p);
                                withheld = None;
                            } else {
                                withheld = Some("parent_not_embedded");
                            }
                        } else {
                            withheld = Some("template_not_allowlisted");
                        }
                    }
                    if let Some(m) = gb.as_object_mut() {
                        m.remove("request");
                    }
                }
                prompts.add(format!("{}:{}", op.clone().unwrap_or_else(|| "(none)".into()), withheld.unwrap_or("published")), 1);
                let row = json!({
                    "event_key": Value::Null, "event_kind": "record", "request_id": Value::Null, "record_id": rec.and_then(|r| r.record_id.clone()),
                    "generator": g.map(|g| g.key.clone()), "generator_label": rec.and_then(|r| r.generator_label.clone()), "generator_key": rec.and_then(|r| r.generator_key.clone()),
                    "model": g.map(|g| json!({"family": g.family, "display": g.display, "checkpoint": g.checkpoint, "licence_id": g.licence_id, "notice": g.notice_files})),
                    "operation": op.clone(), "template": op, "prompt": prompt, "prompt_withheld": withheld, "generation": gblock,
                    "source_text_sha256": parent_h.map(|p| hs(&p)), "output_text_sha256": hs(t),
                    "share_evidence": rec.map(|r| json!({"share": r.share, "ai_byte_spans": r.ai_byte_spans.as_deref().and_then(|s| serde_json::from_str::<Value>(s).ok())})),
                });
                if kind == "hosted" {
                    hosted_rows.push(row);
                    meta.add("hosted:record", 1);
                } else {
                    open_rows.push(row);
                    meta.add("open:record", 1);
                }
            }
        }
        for r in &hosted_rows {
            hosted.row(r)?;
        }
        hosted.finish(files)?;
        let mut open = Table::new(self.out_dir, "open_generations", gate, &["prompt", "response_text"])?;
        for r in &open_rows {
            open.row(r)?;
        }
        open.finish(files)?;
        for (k, n) in style_counts.0 {
            prompts.add(k, n);
        }
        Ok((meta.json(), used_voices, prompts.json(), inventors))
    }

    #[allow(clippy::too_many_arguments)]
    fn write_voices(
        &self, used: &BTreeSet<(String, String)>, voices: &HashMap<(String, String), Value>, private: &[String], released: &HashSet<H>,
        gate: &mut Gate, files: &mut BTreeMap<String, Value>,
    ) -> Result<(Value, BTreeSet<usize>)> {
        let mut tbl = Table::new(self.out_dir, "voices", gate, &["text"])?;
        let mut seen = HashSet::new();
        let mut counts = Counter::default();
        let mut inventors = BTreeSet::new();
        for k in used {
            let Some(v) = voices.get(k) else { continue };
            if voice_withheld(&self.rules, v, private) {
                counts.add("withheld:private_seed_voice", 1);
                continue;
            }
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or_default();
            let text = v.get("text").and_then(|x| x.as_str()).unwrap_or_default();
            if !seen.insert(format!("{id}\u{0}{text}")) {
                continue;
            }
            let derived = v.get("derived_from").filter(|x| x.is_object()).map(|d| {
                let doc = d.get("doc_id").and_then(|x| x.as_str());
                let lic = doc.and_then(|id| self.doc_licence(id));
                let lid = lic.as_ref().and_then(|(l, _, _)| licence_rule(&self.rules, l)).and_then(|i| self.rules.licences[i].licence_id.clone());
                // The source document is named only when it is itself released (its attribution is then in rights.jsonl.zst).
                let doc_text = lic.as_ref().and_then(|(_, _, t)| t.as_deref()).and_then(hx).filter(|t| released.contains(t));
                counts.add(if doc_text.is_some() { "derived_from:released_document" } else { "derived_from:document_not_published" }, 1);
                json!({"domain": d.get("domain").and_then(leaf), "genre": d.get("genre").and_then(leaf), "licence_id": lid,
                       "collection": lic.as_ref().and_then(|(_, c, _)| c.clone()),
                       "text_sha256": doc_text.map(|t| hs(&t)),
                       "document": if doc_text.is_some() { "released in this dataset: its text, provenance and attribution are joined by text_sha256" }
                                   else { "not published; the voice is a style description written by the inventor model" }})
            });
            let origin = match v.get("origin").and_then(|x| x.as_str()).unwrap_or("") {
                o if o.contains("hand_written") => "hand_written",
                "human_derived" => "human_derived",
                "invented" => "invented",
                _ => "other",
            };
            let inv = voice_inventor(&self.rules, v)?;
            let g = inv.map(|i| &self.rules.generators[i]);
            inventors.extend(inv);
            tbl.row(&json!({
                "id": id, "wave_source": k.0, "kind": v.get("kind").and_then(leaf), "origin": origin, "text": text,
                "axes": v.get("axes").map(|a| Value::Object(allow(a, VOICE_AXES))),
                "inventor": v.get("axes").and_then(|a| a.get("inventor")).and_then(leaf),
                "inventor_generator": g.map(|g| g.key.clone()), "inventor_checkpoint": g.and_then(|g| g.checkpoint.clone()),
                "inventor_licence_id": g.map(|g| g.licence_id.clone()), "inventor_notice_files": g.map(|g| g.notice_files.clone()),
                "held_out_style": v.get("held_out_style").and_then(leaf), "genre": v.get("genre").and_then(leaf), "derived_from": derived,
            }))?;
            counts.add(format!("published:{origin}"), 1);
        }
        tbl.finish(files)?;
        Ok((counts.json(), inventors))
    }

    fn write_generators(&self, released: &[H], kinds: &HashMap<H, &'static str>, gen_of: &HashMap<H, usize>, inventors: &BTreeSet<usize>, gate: &mut Gate) -> Result<Vec<usize>> {
        let mut n: BTreeMap<usize, (u64, Option<&'static str>)> = BTreeMap::new();
        for t in released {
            if let Some(i) = gen_of.get(t) {
                let e = n.entry(*i).or_insert((0, Some(kinds[t])));
                e.0 += 1;
            }
        }
        for i in inventors {
            n.entry(*i).or_insert((0, None));
        }
        let mut out = Vec::new();
        let mut rows = Vec::new();
        for (i, (cnt, kind)) in &n {
            let g = &self.rules.generators[*i];
            ensure!(!g.notice_files.is_empty(), "generator {} has no notice file", g.key);
            for f in &g.notice_files {
                ensure!(self.rules.files.iter().any(|p| &p.dest == f), "generator {} notice {f} is not a pinned file", g.key);
            }
            let mut roles = Vec::new();
            if *cnt > 0 {
                roles.push("generator");
            }
            if inventors.contains(i) {
                roles.push("voice_or_brief_inventor");
            }
            let aliases: Vec<&String> = self.rules.inventor_aliases.iter().filter(|(_, k)| **k == g.key).map(|(l, _)| l).collect();
            rows.push(json!({"key": g.key, "family": g.family, "display": g.display, "kind": g.kind, "provider": g.provider, "hosted": g.hosted,
                             "checkpoint": g.checkpoint, "licence_id": g.licence_id, "notice_files": g.notice_files, "texts": cnt,
                             "text_kind": kind.map(text_kind), "roles": roles, "inventor_labels": aliases}));
            out.push(*i);
        }
        let v = json!({"schema": "slop_ninja_public_candidate_generators_v1", "generators": rows});
        gate.scan("generators", 0, &v, &[]);
        fs::write(self.out_dir.join("generators.json"), serde_json::to_vec_pretty(&v)?)?;
        Ok(out)
    }

    fn write_licenses(&self, generators: &[usize]) -> Result<Value> {
        let rules = &self.rules;
        let mut index = Map::new();
        for p in &rules.files {
            ensure!(p.dest.starts_with("LICENSES/") && !p.dest.contains(".."), "pinned file {} must go under LICENSES/", p.dest);
            let src = canon(&self.repo, &p.src);
            let b = fs::read(&src).with_context(|| format!("pinned file {}", p.src))?;
            ensure!(sha_hex(&b) == p.sha256, "pinned file {} changed (sha256 differs from the rules)", p.src);
            let dst = self.out_dir.join(&p.dest);
            fs::create_dir_all(dst.parent().unwrap())?;
            fs::write(&dst, &b)?;
            index.insert(p.dest.clone(), json!({"sha256": p.sha256, "bytes": b.len()}));
        }
        for (id, l) in &rules.licence_ids {
            ensure!(index.contains_key(&l.file), "licence id {id}: file {} is not pinned", l.file);
        }
        let gens: Vec<Value> = generators.iter().map(|i| {
            let g = &rules.generators[*i];
            json!({"key": g.key, "display": g.display, "licence_id": g.licence_id, "notice_files": g.notice_files})
        }).collect();
        let idx = json!({"schema": "slop_ninja_public_candidate_licenses_v1", "licence_ids": rules.licence_ids.iter().map(|(k, v)| (k.clone(), json!({"name": v.name, "url": v.url, "file": v.file}))).collect::<Map<_, _>>(),
                         "generators": gens, "files": index});
        let dataset = "# Licence of this release\n\n\
            This release is a collection. Each text keeps its own source licence, attribution and notices, given per row in\n\
            `rights.jsonl.zst` (`licence_id`, `licence_file`, `attribution`, `inherited_from`). Public-domain, CC0, CC BY and\n\
            CC BY-SA sources are included; CC BY-SA texts keep their own licence version.\n\n\
            Model-written and model-rewritten text (`generated_additions`), together with the curation, selection, labels and\n\
            metadata of this release, are offered under the Creative Commons Attribution-ShareAlike 4.0 International licence\n\
            (`sources/CC-BY-SA-4.0.txt`), to the extent Slop Ninja holds rights in them. Every generation also names its model's\n\
            licence or terms in `models/` or `hosted/`. Prompts that embed the Fix Slop adaptation carry its MIT notice\n\
            (`prompts/fix-slop.LICENSE`). Pangram results are comparator evidence published as observed.\n\n\
            No model weights are included.\n";
        fs::create_dir_all(self.out_dir.join("LICENSES"))?;
        fs::write(self.out_dir.join("LICENSES/DATASET.md"), dataset)?;
        let mut notice = String::from("# Notices\n\n## Source collections\n\n");
        for (c, credit) in &rules.collection_credits {
            notice.push_str(&format!("- `{c}`: {credit}\n"));
        }
        notice.push_str("\nPer-text attribution (titles, authors, source and history URLs) is in `rights.jsonl.zst`.\n\n## Generators\n\n");
        for i in generators {
            let g = &rules.generators[*i];
            notice.push_str(&format!("- {} (`{}`): {}; see {}\n", g.display, g.key, g.licence_id, g.notice_files.join(", ")));
        }
        notice.push_str("\n## Prompt material\n\n- Fix Slop prompt adaptation: MIT licence, `prompts/fix-slop.LICENSE`.\n");
        fs::write(self.out_dir.join("LICENSES/NOTICE.md"), notice)?;
        fs::write(self.out_dir.join("LICENSES/INDEX.json"), serde_json::to_vec_pretty(&idx)?)?;
        let mut out = Map::new();
        for f in ["LICENSES/DATASET.md", "LICENSES/NOTICE.md", "LICENSES/INDEX.json"] {
            let b = fs::read(self.out_dir.join(f))?;
            out.insert(f.into(), json!({"sha256": sha_hex(&b), "bytes": b.len()}));
        }
        for (k, v) in index {
            out.insert(k, v);
        }
        Ok(Value::Object(out))
    }

    fn store_meta(&self) -> Result<Value> {
        let verify = self.store.join("VERIFY.json");
        let backup = fs::read(self.store.join("STATUS.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.pointer("/r2/manifest_sha256").cloned());
        Ok(json!({
            "schema": crate::db::STORE_SCHEMA,
            "verify_sha256": archive::hash_file_raw(&verify).ok(),
            "r2_backup_manifest_sha256": backup,
            "opened": "read-only (immutable)",
        }))
    }

    fn readme(&self, counts: &Counter, pangram: &Value, gens: &Value, generators: &[usize]) -> String {
        let c = |k: &str| counts.0.get(k).copied().unwrap_or(0);
        let fmt = |n: u64| {
            let s = n.to_string();
            let mut o = String::new();
            for (i, ch) in s.chars().enumerate() {
                if i > 0 && (s.len() - i) % 3 == 0 {
                    o.push(',');
                }
                o.push(ch);
            }
            o
        };
        let with_text = pangram["counts"]["with_text"].as_u64().unwrap_or(0);
        let hash_only = pangram["counts"]["hash_only"].as_u64().unwrap_or(0);
        let gen_names: Vec<String> = generators.iter().map(|i| self.rules.generators[*i].display.clone()).collect();
        let _ = gens;
        format!(
"# Slop Ninja public corpus, candidate 1

This dataset contains {total} English texts from Slop Ninja's detector experiments: {human} human-written texts,
{hosted} texts written or rewritten by hosted OpenAI and Anthropic models, and {open} texts written or rewritten by
open-weight models. It totals {words} whitespace-delimited words. Each text is stored once, as exact UTF-8, with its
SHA-256. It is joined to its provenance, rights, experiment roles, generation records and Pangram results.

Every text here was used in at least one Slop Ninja experiment: training, development, calibration, a consumed Test
set, or an earlier published snapshot. Each text's source licence permits redistribution. Texts held for future
evaluation are not included.

| File | Contents |
|---|---|
| `texts.jsonl.zst` | Exact text, counts, token count, language score, canonical split |
| `records.jsonl.zst` | Source and generation records with titles, authors, URLs, dates and origin labels |
| `rights.jsonl.zst` | Licence, attribution, share-alike status and the ancestor chain of every text |
| `splits.jsonl.zst`, `experiments.jsonl.zst` | Each text's role and position in every experiment view |
| `hosted_generations.jsonl.zst` | Hosted model outputs with the prompt, model selector, reported identity and spans |
| `open_generations.jsonl.zst` | Open-weight model outputs with the prompt, checkpoint, decoding settings and spans |
| `voices.jsonl.zst` | The voice prompts and briefs used in hosted prompts |
| `pangram_observations.jsonl.zst` | {pobs} Pangram results; {phash} refer to texts outside this release by hash |
| `generators.json`, `LICENSES/` | Generator roster, model licences and terms, source licences and credits |

**Labels.** `origin` and `share` describe how each text was produced. Human labels rest on historical evidence such
as publication before 2022-11-30, public-domain status or archived revisions. They do not document a human-only
writing process. Generated texts record the operation (full rewrite, span rewrite, mirror, continuation, splice and
others), the replaced byte spans and the realized model share. A generation shares facts and topic with its source.
Keep each source group and its descendants on one side of any split.

**Splits.** `canonical_split` is the most restrictive role a text has held. Development, calibration and consumed
Test texts are marked `evaluation_only`. Consumed Test texts have already been used for a reported evaluation.

**Prompts.** A prompt is published verbatim when its digest matches the recorded event, its template is on the
release allowlist and, for templates that embed the source passage, the released parent passage occurs in it.
Otherwise `prompt` is null and `prompt_withheld` gives the reason. The style library's private seed voice examples
are not published: voices that contain them are left out of `voices.jsonl.zst`, and prompts or style fields that
contain them are withheld with reason `private_seed_voice`.

**Voices.** Voices and briefs were written by hand or by the open-weight models of the style library. Each
model-written voice names its inventor (`inventor_generator`, checkpoint and notice files), and generations whose
prompts embed such a voice name that model's notice too. A voice derived from a human document is a style
description; the document itself is not published unless it is released here.

**Raw responses.** Where `response_text` is present (span, continuation and splice operations), it is the raw model
response, which can extend beyond the released text. Model-written text, including base-model continuations, was
not screened for memorised third-party content.

**Pangram.** Pangram results are comparator evidence and never labels. Rows with `text_included: false` refer to a
text outside this release. They carry its hash, word count, domain, origin label and split, so reported comparisons
can be recomputed and holders of the upstream corpus can join them.

**Licences.** Each row of `rights.jsonl.zst` names the source licence (public domain, CC0, CC BY or CC BY-SA), its
attribution and any share-alike requirement. Generated text and curation are released under CC BY-SA 4.0, and each
component keeps its own licence and notices. Each generation names its model's licence or terms file in `LICENSES/`.
Generators: {gens}. No model weights are included.

**Checksums.** `MANIFEST.json` gives each table's row count and the SHA-256 of its compressed and decoded bytes;
`SHA256SUMS` covers every file. Decode with any zstd reader, such as `zstd -dc texts.jsonl.zst`.
",
            total = fmt(c("total:texts")), human = fmt(c("kind:human")), hosted = fmt(c("kind:hosted_generation")),
            open = fmt(c("kind:open_generation")), words = fmt(c("total:words")), pobs = fmt(with_text + hash_only),
            phash = fmt(hash_only), gens = gen_names.join(", "),
        )
    }
}

#[derive(Clone, Default, Debug)]
struct RightsOwn {
    class: Option<String>,
    licence_id: Option<String>,
    recorded: BTreeSet<String>,
    flags: BTreeSet<bool>,
    flag_override: bool,
    published: bool,
    collections: BTreeSet<String>,
    best: Option<(usize, (String, i64))>,
}

/// `sha256sum -c` lines for every file under `dir` except SHA256SUMS itself.
pub fn sha256sums(dir: &Path) -> Result<String> {
    fn walk(base: &Path, d: &Path, out: &mut Vec<String>) -> Result<()> {
        let mut es: Vec<_> = fs::read_dir(d)?.flatten().collect();
        es.sort_by_key(|e| e.file_name());
        for e in es {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out)?;
            } else {
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
                if rel != "SHA256SUMS" {
                    out.push(format!("{}  {rel}\n", archive::hash_file_raw(&p)?));
                }
            }
        }
        Ok(())
    }
    let mut v = Vec::new();
    walk(dir, dir, &mut v)?;
    Ok(v.concat())
}

#[cfg(test)]
pub(crate) mod testing {
    pub(crate) use super::{Gate, embedding, local_port, ms_to_iso, private_ipv4, provider_id, runtime_struct, style_of, voice_inventor, voice_withheld};
    pub(crate) use super::Counter;
}

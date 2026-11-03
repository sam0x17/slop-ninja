//! Bundle assembly for Slop Ninja A.
//!
//! A bundle is one directory per split with `rows.jsonl` (one contract row per
//! document), `weights.json` (the declared weighting and its counts) and
//! `MANIFEST.json` (inputs with SHA-256, counts per domain x origin x
//! generator, exclusions and checks). Rows come from four sources: split
//! humans, local open-weight positives, hosted frontier positives (v2 records)
//! and the stage-3 legacy Train rows. Every row keeps its exact text, its
//! replacement spans as UTF-8 byte offsets and a digest of the input line it
//! came from.
pub mod bundle;
pub mod captures;
pub mod consolidate;
pub mod covariate;
pub mod rights;
pub mod styles;
pub mod successor;
pub mod supervision;
pub mod test_split;
pub mod trainer;
pub mod weights;

pub use styles::FrozenIndex;
pub use weights::{CohortWeights, Weighting, assign_weights, balanced_weights};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const ROW_SCHEMA: &str = "slop_ninja_a_bundle_row_v1";
/// Rows of a round-2 build (any `round2` option on): v1 plus the optional
/// `block` tag, with raked Train weights when covariate balance is on
/// (protocol A2 amendment 5 item 5.4). A version-1 rights snapshot reads none.
pub const ROW_SCHEMA_R2: &str = "slop_ninja_a_bundle_row_v2";
pub const LOCAL_RECORD_SCHEMA: &str = "slop_ninja_a_comp_positive_v1";
pub const FRONTIER_RECORD_SCHEMA: &str = "slop_ninja_frontier_positive_v2";
pub const FRONTIER_POLICY: &str = "hosted_frontier_label_balanced_v1";
pub const LEGACY_COHORT: &str = "stage3_legacy_train";
pub const ORIGINS: [&str; 3] = ["human_only", "mixed", "model_only"];
/// Origin order of the stage-3 legacy targets (`targets.origin` index).
pub const LEGACY_ORIGIN_ORDER: [&str; 3] = ["human_only", "model_only", "mixed"];
pub const SPLITS: [&str; 4] = ["train", "dev_iter", "dev_select", "cal"];
/// The sealed Test split: assembled only after the written seal (`bundle::SealCheck`).
pub const TEST_SPLIT: &str = "test";

pub fn sha_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// Which pipeline produced a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Human,
    LocalPositive,
    Frontier,
    HostedCapture,
    Stage3Legacy,
    /// Rows of an admitted external Train pool (e.g. a public benchmark's train split).
    ExternalPool,
}

/// One document of a bundle, in the shared contract.
#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub schema: &'static str,
    pub id: String,
    pub split: String,
    pub text: String,
    pub text_sha256: String,
    /// human_only | mixed | model_only
    pub origin: String,
    /// Realised final-word fraction in model-produced spans (0 for humans, 1 for model-only).
    pub share: f64,
    /// Exact model-produced spans as UTF-8 byte offsets into `text`, sorted and disjoint.
    pub ai_byte_spans: Vec<[usize; 2]>,
    /// Canonical generator attribution label; null for humans.
    pub generator_label: Option<String>,
    /// Concrete generator (local checkpoint key, frontier export label or legacy profile).
    pub generator_key: Option<String>,
    pub cohort_id: String,
    pub cohort_weight: f64,
    pub balanced_weight: f64,
    pub weight: f64,
    pub source_group: String,
    pub writer_key: Option<String>,
    pub domain: String,
    pub genre: Option<String>,
    pub collection: Option<String>,
    pub parent_id: Option<String>,
    pub voice_id: Option<String>,
    pub brief_id: Option<String>,
    pub voice_kind: Option<String>,
    pub voice_cluster_id: Option<String>,
    pub voice_origin: Option<String>,
    pub style_axes: Value,
    pub style_partition: Option<String>,
    pub operation: String,
    pub held_out_style: bool,
    pub population: String,
    pub source_kind: SourceKind,
    pub provenance_sha256: String,
    pub provenance: Value,
    /// The positive record's own domain name when it differs from its parent's
    /// split-manifest domain (the row takes the parent's).
    #[serde(skip)]
    pub record_domain: Option<String>,
    /// The evaluation style item whose normalised text equals this row's voice or brief text.
    pub style_text_match: Option<String>,
    /// Test human rows only (diagnostic slices): the writer demographics the source releases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub demographics: Option<Value>,
    /// Test human rows only: the source's OCR tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocr_tier: Option<Value>,
    /// Test human rows only: transcription markup removed per 100 words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_markup_per_100_words: Option<f64>,
    /// Round-2 block tag (`T` or `C`), set only when the config asks for block tags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<String>,
}

pub fn str_opt(v: &Value, k: &str) -> Option<String> {
    v[k].as_str().map(String::from)
}

pub fn str_req<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing string field {k}"))
}

/// Evaluation domain of a human collection.
pub fn domain_of_collection(collection: &str, overrides: &BTreeMap<String, String>) -> String {
    if let Some(d) = overrides.get(collection) {
        return d.clone();
    }
    match collection {
        "project_gutenberg_historical" => "project_gutenberg",
        "wikipedia_enwiki_historical" => "wikipedia",
        "wikinews_enwikinews_historical" => "wikinews",
        "plos_historical" => "plos",
        "us_federal_register_historical" => "us_federal_register",
        "fineweb_cc_historical" => "fineweb",
        "global_voices_historical" => "global_voices",
        "stackexchange_historical" => "stackexchange",
        c => return c.strip_suffix("_historical").unwrap_or(c).to_string(),
    }
    .to_string()
}

/// Canonical split name (`dev-iter` and `dev_iter` are the same split).
pub fn split_name(s: &str) -> String {
    s.replace('-', "_")
}

/// Where a row came from, for the provenance digest.
#[derive(Clone, Debug)]
pub struct Origin<'a> {
    pub input: usize,
    pub line: usize,
    pub bytes: &'a [u8],
}

impl Origin<'_> {
    fn digest(&self) -> (String, Value) {
        (sha_hex(self.bytes), json!({"input": self.input, "line": self.line}))
    }
}

fn check_text(v: &Value) -> Result<(String, String)> {
    let text = str_req(v, "text")?.to_string();
    let digest = sha_hex(text.as_bytes());
    ensure!(
        v["text_sha256"].as_str() == Some(digest.as_str()),
        "text digest differs from text_sha256"
    );
    Ok((text, digest))
}

/// Validate spans: within the text, on character boundaries, sorted and disjoint.
pub fn check_spans(text: &str, spans: &[[usize; 2]]) -> Result<()> {
    let mut end = 0usize;
    for (i, s) in spans.iter().enumerate() {
        ensure!(s[0] < s[1], "empty or reversed span {i}");
        ensure!(s[1] <= text.len(), "span {i} beyond text");
        ensure!(i == 0 || s[0] >= end, "spans overlap or are unsorted");
        ensure!(
            text.is_char_boundary(s[0]) && text.is_char_boundary(s[1]),
            "span {i} not on a character boundary"
        );
        end = s[1];
    }
    Ok(())
}

fn spans_of(v: &Value) -> Result<Vec<[usize; 2]>> {
    let mut out = Vec::new();
    for s in v.as_array().context("span list")? {
        let a = s.as_array().context("span")?;
        ensure!(a.len() == 2, "span needs two offsets");
        out.push([
            a[0].as_u64().context("offset")? as usize,
            a[1].as_u64().context("offset")? as usize,
        ]);
    }
    Ok(out)
}

/// Labels that must agree with the origin class.
fn check_labels(origin: &str, share: f64, spans: &[[usize; 2]], text: &str) -> Result<()> {
    ensure!(ORIGINS.contains(&origin), "unknown origin {origin}");
    ensure!(share.is_finite() && (0.0..=1.0).contains(&share), "share outside [0,1]");
    check_spans(text, spans)?;
    match origin {
        "human_only" => ensure!(share == 0.0 && spans.is_empty(), "human row with AI share or spans"),
        "model_only" => ensure!(
            share == 1.0 && spans == [[0, text.len()]],
            "model-only row must be one whole-text span with share 1"
        ),
        _ => ensure!(share > 0.0 && !spans.is_empty(), "mixed row without share or spans"),
    }
    Ok(())
}

fn base_row(id: &str, split: &str, text: String, digest: String, origin: &str, o: &Origin) -> Row {
    let (p, prov) = o.digest();
    Row {
        schema: ROW_SCHEMA,
        id: id.into(),
        split: split.into(),
        text,
        text_sha256: digest,
        origin: origin.into(),
        share: 0.0,
        ai_byte_spans: Vec::new(),
        generator_label: None,
        generator_key: None,
        cohort_id: String::new(),
        cohort_weight: 1.0,
        balanced_weight: 0.0,
        weight: 0.0,
        source_group: String::new(),
        writer_key: None,
        domain: String::new(),
        genre: None,
        collection: None,
        parent_id: None,
        voice_id: None,
        brief_id: None,
        voice_kind: None,
        voice_cluster_id: None,
        voice_origin: None,
        style_axes: Value::Null,
        style_partition: None,
        operation: String::new(),
        held_out_style: false,
        population: String::new(),
        source_kind: SourceKind::Human,
        provenance_sha256: p,
        provenance: prov,
        record_domain: None,
        style_text_match: None,
        demographics: None,
        ocr_tier: None,
        removed_markup_per_100_words: None,
        block: None,
    }
}

/// Diagnostic-slice fields of a Test human document (protocol `diagnostic_slices`:
/// learner demographics, OCR tier and removed markup). Only Test rows carry them,
/// so earlier evaluation bundles keep their exact bytes.
fn test_fields(r: &mut Row, v: &Value) {
    if r.split != "test" {
        return;
    }
    let d = &v["provenance"]["writer_demographics"];
    if d.is_object() {
        r.demographics = Some(d.clone());
    }
    if !v["ocr_tier"].is_null() {
        r.ocr_tier = Some(v["ocr_tier"].clone());
    }
    r.removed_markup_per_100_words = v["extraction"]["unit"]["normalization"]["removed_markup_per_100_words"].as_f64();
}

/// A split human document (pool/split document schema).
pub fn human_row(
    v: &Value,
    split: &str,
    domain_by_id: &HashMap<String, String>,
    overrides: &BTreeMap<String, String>,
    o: &Origin,
) -> Result<Row> {
    let id = str_req(v, "id")?;
    ensure!(
        v["frozen_split"].as_str().map(split_name).as_deref() == Some(split),
        "human {id} frozen_split differs from {split}"
    );
    ensure!(v["origin"] == "human_only", "human {id} origin");
    let (text, digest) = check_text(v)?;
    let collection = str_req(v, "collection")?.to_string();
    let mut r = base_row(id, split, text, digest, "human_only", o);
    r.domain = domain_by_id
        .get(id)
        .cloned()
        .unwrap_or_else(|| domain_of_collection(&collection, overrides));
    r.genre = str_opt(v, "domain");
    r.collection = Some(collection.clone());
    r.source_group = str_req(v, "source_group")?.into();
    r.writer_key = str_opt(v, "writer_key");
    r.cohort_id = format!("human:{collection}");
    r.operation = "human".into();
    r.population = "primary".into();
    r.style_axes = json!({});
    test_fields(&mut r, v);
    Ok(r)
}

/// A human document of a diagnostic slice (`diagnostic_<name>` frozen split),
/// reported with split `split` (Test) in population `name`, outside the
/// primary population; its domain is the slice name. Never a parent.
pub fn diagnostic_row(v: &Value, name: &str, split: &str, o: &Origin) -> Result<Row> {
    let id = str_req(v, "id")?;
    ensure!(v["frozen_split"].as_str() == Some(format!("diagnostic_{name}").as_str()), "diagnostic {id} frozen_split differs from diagnostic_{name}");
    ensure!(v["origin"] == "human_only", "diagnostic {id} origin");
    ensure!(!name.is_empty() && !name.contains('|') && name != "primary" && name != "frontier" && !name.starts_with("robustness"),
        "diagnostic slice name {name}");
    let (text, digest) = check_text(v)?;
    let collection = str_req(v, "collection")?.to_string();
    let mut r = base_row(id, split, text, digest, "human_only", o);
    r.domain = name.into();
    r.genre = str_opt(v, "domain");
    r.collection = Some(collection);
    r.source_group = str_req(v, "source_group")?.into();
    r.writer_key = str_opt(v, "writer_key");
    r.cohort_id = format!("diagnostic:{name}");
    r.operation = "human".into();
    r.population = name.into();
    r.style_axes = json!({});
    test_fields(&mut r, v);
    Ok(r)
}

/// Facts about a parent human document that its positives inherit.
#[derive(Clone, Debug)]
pub struct Parent {
    pub split: String,
    pub source_group: String,
    pub writer_key: Option<String>,
    pub genre: Option<String>,
    pub collection: Option<String>,
    pub domain: String,
}

impl Parent {
    pub fn of(r: &Row) -> Parent {
        Parent {
            split: r.split.clone(),
            source_group: r.source_group.clone(),
            writer_key: r.writer_key.clone(),
            genre: r.genre.clone(),
            collection: r.collection.clone(),
            domain: r.domain.clone(),
        }
    }
}

/// Resolve a request's style against the frozen partitions, by id and by the
/// normalised text of the voice prompt and brief. An id that is not in the
/// frozen file is `unknown` (never Train); a text equal to an evaluation
/// item's makes the row held-out-style whatever its id says.
fn apply_style(r: &mut Row, styles: &FrozenIndex, voice: Option<&str>, brief: Option<&str>, texts: &[&str]) {
    r.voice_id = voice.map(String::from);
    r.brief_id = brief.map(String::from);
    // The voice prompt identifies the voice; a brief stands in when there is none.
    let key = voice.or(brief);
    let mut partitions = Vec::new();
    for id in [voice, brief].into_iter().flatten() {
        match styles.items.get(id) {
            Some(it) => partitions.push(it.partition.clone()),
            None => partitions.push("unknown".to_string()),
        }
    }
    for t in texts.iter().filter(|t| !t.trim().is_empty()) {
        if let Some((p, id)) = styles.text_partition(t) {
            partitions.push(p.clone());
            r.style_text_match = Some(id.clone());
        }
    }
    if let Some(it) = key.and_then(|k| styles.items.get(k)) {
        r.voice_cluster_id = it.cluster.clone();
        r.voice_origin = it.origin.clone();
        if r.voice_kind.is_none() && !it.kind.is_empty() {
            r.voice_kind = Some(it.kind.clone());
        }
        if let Some(a) = &it.axis {
            r.style_axes["library_axis"] = json!(a);
        }
    }
    // Held-out-style: any style item or text belongs to an evaluation partition.
    if partitions.iter().any(|p| p != "train" && p != "unknown") {
        r.held_out_style = true;
    }
    partitions.sort();
    partitions.dedup();
    r.style_partition = if partitions.is_empty() { None } else { Some(partitions.join("+")) };
}

/// True when a row's style could not be resolved in the frozen partitions.
pub fn style_unknown(r: &Row) -> bool {
    r.style_partition.as_deref().is_some_and(|p| p.split('+').any(|x| x == "unknown"))
}

/// A local open-weight positive (`slop_ninja_a_comp_positive_v1`).
pub fn local_row(v: &Value, styles: &FrozenIndex, o: &Origin) -> Result<Row> {
    ensure!(v["schema"] == LOCAL_RECORD_SCHEMA, "local positive schema");
    let id = str_req(v, "id")?;
    let split = split_name(str_req(v, "frozen_split")?);
    let (text, digest) = check_text(v)?;
    let origin = str_req(v, "origin")?;
    let share = v["share_target"].as_f64().context("share_target")?;
    let spans = if origin == "model_only" {
        vec![[0, text.len()]]
    } else {
        spans_of(&v["ai_byte_spans"])?
    };
    check_labels(origin, share, &spans, &text)?;
    let mut r = base_row(id, &split, text, digest, origin, o);
    r.share = share;
    r.ai_byte_spans = spans;
    let g = &v["generation"];
    r.generator_label = Some(str_req(&g["generator"], "family")?.into());
    r.generator_key = str_opt(g, "generator_key");
    r.cohort_id = format!("local:{}", str_req(v, "cohort")?);
    r.population = str_opt(v, "population").unwrap_or_else(|| "primary".into());
    r.operation = g["operation"]
        .as_str()
        .or(v["kind"].as_str())
        .context("operation")?
        .into();
    r.domain = str_req(v, "domain")?.into();
    r.source_group = str_req(v, "source_group")?.into();
    r.writer_key = str_opt(v, "writer_key");
    r.collection = str_opt(v, "collection");
    r.parent_id = str_opt(v, "parent_id");
    r.source_kind = SourceKind::LocalPositive;
    let st = &g["style"];
    r.style_axes = json!({});
    if st.is_object() {
        for k in ["style_kind", "style_policy", "operation"] {
            if let Some(x) = st[k].as_str() {
                r.style_axes[k] = json!(x);
            }
        }
        for k in ["setting", "mood", "modifier", "brief_genre"] {
            if !st[k].is_null() {
                r.style_axes[k] = st[k].clone();
            }
        }
        r.voice_kind = st["style_kind"].as_str().map(String::from);
        let texts: Vec<&str> = ["voice_text", "brief_text"].iter().filter_map(|k| st[*k].as_str()).collect();
        apply_style(&mut r, styles, st["voice_id"].as_str(), st["brief_id"].as_str(), &texts);
    }
    Ok(r)
}

/// A hosted frontier positive (`slop_ninja_frontier_positive_v2`).
pub fn frontier_row(v: &Value, export_label: &str, styles: &FrozenIndex, o: &Origin) -> Result<Row> {
    let id = str_req(v, "id")?;
    ensure!(
        v["schema"] == FRONTIER_RECORD_SCHEMA,
        "frontier record {id} is not {FRONTIER_RECORD_SCHEMA}; republish the export with the v2 collector"
    );
    ensure!(
        v["training_admission"]["schema"] == "slop_ninja_a_hosted_training_admission_v1",
        "frontier record {id} without an admission basis"
    );
    let split = split_name(str_req(v, "frozen_split")?);
    let (text, digest) = check_text(v)?;
    let origin = str_req(v, "origin")?;
    let share = v["share_target"].as_f64().context("share_target")?;
    let ev = &v["share_evidence"];
    let spans = if origin == "model_only" {
        vec![[0, text.len()]]
    } else if ev["final_byte_spans"].is_array() {
        spans_of(&ev["final_byte_spans"])?
    } else {
        spans_of(&json!([ev["final_byte_span"].clone()]))?
    };
    check_labels(origin, share, &spans, &text)?;
    let mut r = base_row(id, &split, text, digest, origin, o);
    r.share = share;
    r.ai_byte_spans = spans;
    r.generator_label = Some(str_req(v, "ai_attribution_label")?.into());
    r.generator_key = Some(export_label.into());
    r.cohort_id = str_req(v, "cohort_id")?.into();
    let policy = v["cohort_weight_policy"]["policy_id"].as_str().unwrap_or("");
    ensure!(policy == FRONTIER_POLICY, "frontier record {id}: unknown weight policy {policy:?}");
    r.population = "frontier".into();
    let g = &v["generation"];
    r.operation = str_req(g, "kind")?.into();
    r.domain = str_req(v, "domain")?.into();
    r.source_group = str_req(v, "source_group")?.into();
    r.collection = str_opt(v, "collection");
    r.parent_id = str_opt(v, "parent_id");
    r.source_kind = SourceKind::Frontier;
    let voice = &g["voice"];
    r.style_axes = json!({});
    if voice.is_object() {
        if let Some(a) = voice["axes"].as_object() {
            for (k, x) in a {
                r.style_axes[k] = x.clone();
            }
        }
        r.voice_kind = str_opt(voice, "kind");
        let vid = voice["id"].as_str();
        let kind = voice["kind"].as_str().unwrap_or("");
        let (vp, br) = if kind == "brief" { (None, vid) } else { (vid, None) };
        let texts: Vec<&str> = voice["text"].as_str().into_iter().collect();
        apply_style(&mut r, styles, vp, br, &texts);
        if r.voice_origin.is_none() {
            r.voice_origin = str_opt(voice, "origin");
        }
        if voice["held_out_style"] == true {
            r.held_out_style = true;
        }
    }
    if let Some(l) = g["voice_ladder"]["ladder_id"].as_str() {
        r.style_axes["voice_ladder"] = json!(l);
    }
    Ok(r)
}

/// A stage-3 legacy Train row (existing-data full-A bundle `train.jsonl`).
pub fn legacy_row(v: &Value, families: &BTreeMap<String, String>, o: &Origin) -> Result<Row> {
    let id = str_req(v, "id")?;
    ensure!(v["frozen_split"] == "train", "legacy row {id} is not Train");
    let (text, digest) = check_text(v)?;
    let t = &v["targets"];
    let oi = t["origin"].as_u64().context("legacy origin")? as usize;
    let origin = *LEGACY_ORIGIN_ORDER.get(oi).context("legacy origin index")?;
    let share = t["share"].as_f64().context("legacy share")?;
    let spans = match origin {
        "human_only" => Vec::new(),
        "model_only" => vec![[0, text.len()]],
        _ if t["ai_byte_spans"].is_array() => spans_of(&t["ai_byte_spans"])?,
        _ if share == 1.0 => vec![[0, text.len()]],
        _ => bail!("legacy mixed row {id} without spans"),
    };
    check_labels(origin, share, &spans, &text)?;
    let mut r = base_row(&format!("stage3:{id}"), "train", text, digest, origin, o);
    r.share = share;
    r.ai_byte_spans = spans;
    r.source_kind = SourceKind::Stage3Legacy;
    if origin != "human_only" {
        let ident = str_req(&v["roles"]["ai"], "identity")?;
        let repo = ident.split(':').next().unwrap_or(ident);
        r.generator_label = Some(
            families
                .get(repo)
                .with_context(|| format!("no family for legacy generator {repo}"))?
                .clone(),
        );
        r.generator_key = Some(ident.into());
    }
    r.operation = match origin {
        "human_only" => "human",
        "model_only" => "mirror",
        _ if t["ai_byte_spans"].is_array() => "span",
        _ => "full",
    }
    .into();
    r.cohort_id = LEGACY_COHORT.into();
    r.population = "stage3_legacy".into();
    r.source_group = str_req(v, "source_group")?.into();
    r.writer_key = legacy_author(&r.source_group).map(|a| format!("author:{a}"));
    r.domain = legacy_domain(&r.source_group).into();
    r.genre = str_opt(v, "acquisition_stratum");
    r.collection = Some(format!("stage3_legacy:{}", r.domain));
    r.style_axes = json!({});
    Ok(r)
}

pub const EXTERNAL_POOL_DOCUMENT_SCHEMA: &str = "slop_ninja_a_pool_document_v1";

/// One admitted external-pool document (`slop_ninja_a_pool_document_v1`,
/// `intended_use` train_only, `split_assigned` false) as a Train row. Humans
/// are `human_only` with no spans; model rows are `model_only` whose spans
/// cover the whole text. The row keeps the pool's id, cohort, source group,
/// domain and generator label.
pub fn external_row(v: &Value, pool: &str, o: &Origin) -> Result<Row> {
    let id = str_req(v, "id")?;
    ensure!(v["schema"] == EXTERNAL_POOL_DOCUMENT_SCHEMA, "external row {id}: schema");
    ensure!(v["pool_status"] == "admitted" && v["intended_use"] == "train_only" && v["split_assigned"] == false,
        "external row {id} is not an admitted, unassigned Train-only document");
    ensure!(v["rights"]["commercial_training"] == true, "external row {id}: commercial training not admitted");
    let (text, digest) = check_text(v)?;
    let origin = str_req(v, "origin")?;
    ensure!(origin == "human_only" || origin == "model_only", "external row {id}: origin {origin}");
    let spans = if origin == "model_only" { spans_of(&v["ai_byte_spans"])? } else { Vec::new() };
    ensure!(origin == "human_only" || spans == vec![[0, text.len()]], "external model row {id}: spans must cover the text");
    let share = if origin == "model_only" { 1.0 } else { 0.0 };
    ensure!(v["share_target"].as_f64() == Some(share), "external row {id}: share_target");
    check_labels(origin, share, &spans, &text)?;
    let mut r = base_row(id, "train", text, digest, origin, o);
    r.share = share;
    r.ai_byte_spans = spans;
    r.source_kind = SourceKind::ExternalPool;
    if origin == "model_only" {
        r.generator_label = Some(str_req(v, "generator_label")?.into());
        r.generator_key = str_opt(v, "generator_key");
        r.operation = str_req(v, "operation")?.into();
    } else {
        ensure!(v["generator_label"].is_null(), "external human row {id} has a generator label");
        r.operation = "human".into();
    }
    r.cohort_id = str_req(v, "cohort_id")?.into();
    r.population = format!("external:{pool}");
    r.source_group = str_req(v, "source_group")?.into();
    r.writer_key = str_opt(v, "writer_key");
    r.domain = str_req(v, "domain")?.into();
    r.genre = str_opt(v, "genre");
    r.collection = str_opt(v, "collection");
    r.style_axes = json!({});
    Ok(r)
}

/// First 8 bytes of SHA-256(`seed` NUL `key`) as a big-endian u64, mod 1000.
pub fn per_mille(seed: &str, key: &str) -> u64 {
    let h = Sha256::digest(format!("{seed}\0{key}").as_bytes());
    u64::from_be_bytes(h[..8].try_into().unwrap()) % 1000
}

/// `encoder_stage3_work_v1:<author>:pg<N>` or `author_work_v2:<author>:<work>` -> author key.
pub fn legacy_author(group: &str) -> Option<String> {
    let mut p = group.split(':');
    matches!(p.next()?, "encoder_stage3_work_v1" | "author_work_v2").then_some(())?;
    p.next().filter(|a| !a.is_empty()).map(String::from)
}

/// Evaluation domain of a stage-3 legacy source group, by its key prefix.
pub fn legacy_domain(group: &str) -> &'static str {
    match group.split(':').next().unwrap_or("") {
        "encoder_stage3_work_v1" | "author_work_v2" => "project_gutenberg",
        "doi" => "plos",
        "wikinews" => "wikinews",
        "wikipedia" => "wikipedia",
        _ => "stage3_other",
    }
}

/// `encoder_stage3_work_v1:<author>:pg<N>` -> N.
pub fn legacy_pg_id(group: &str) -> Option<u64> {
    group.rsplit(':').next()?.strip_prefix("pg")?.parse().ok()
}

/// Metadata of every evaluation split (reservation manifests; never prose).
#[derive(Default, Debug)]
pub struct EvalKeys {
    pub families: HashMap<String, String>,
    pub writers: HashMap<String, String>,
    pub pg_ids: HashMap<u64, String>,
    /// (first token, last token) of PG writer keys, for the loose legacy author match.
    pub pg_name_ends: HashMap<(String, String), String>,
}

impl EvalKeys {
    pub fn add_manifest_row(&mut self, split: &str, v: &Value) {
        // Reservation rows name `family_key`; diagnostic-slice document manifests name `source_group`.
        for k in ["family_key", "source_group"] {
            if let Some(f) = v[k].as_str() {
                self.families.entry(f.into()).or_insert_with(|| split.into());
            }
        }
        // Reservation group rows list their families.
        for f in v["family_keys"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            self.families.entry(f.into()).or_insert_with(|| split.into());
        }
        if let Some(g) = v["group_key"].as_str() {
            self.writers.entry(g.into()).or_insert_with(|| split.into());
        }
        let su = &v["source_unit"];
        if let Some(w) = su["writer_key"].as_str() {
            self.writers.entry(w.into()).or_insert_with(|| split.into());
            if let Some(ends) = name_ends(w.strip_prefix("author:").unwrap_or(w)) {
                self.pg_name_ends.entry(ends).or_insert_with(|| split.into());
            }
        }
        if let Some(n) = su["pg_id"].as_u64() {
            self.pg_ids.entry(n).or_insert_with(|| split.into());
        }
    }
    /// An acquired evaluation human closes its source group. Its writer key is
    /// not used: for some domains (Federal Register departments) it is coarser
    /// than the contributor key of the reservation, which `group_key` carries.
    pub fn add_human(&mut self, r: &Row) {
        self.families.entry(r.source_group.clone()).or_insert_with(|| r.split.clone());
    }
    /// The evaluation split a Train-side row collides with, and why.
    pub fn collision(&self, r: &Row) -> Option<(String, &'static str)> {
        if let Some(s) = self.families.get(&r.source_group) {
            return Some((s.clone(), "source_group"));
        }
        if let Some(s) = r.writer_key.as_ref().and_then(|w| self.writers.get(w)) {
            return Some((s.clone(), "writer_key"));
        }
        if matches!(r.source_kind, SourceKind::Stage3Legacy | SourceKind::HostedCapture) {
            if let Some(s) = legacy_pg_id(&r.source_group).and_then(|n| self.pg_ids.get(&n)) {
                return Some((s.clone(), "pg_id"));
            }
            if let Some(s) = legacy_author(&r.source_group)
                .and_then(|a| name_ends(&a))
                .and_then(|e| self.pg_name_ends.get(&e))
            {
                return Some((s.clone(), "pg_author_first_last_name"));
            }
        }
        None
    }
}

fn name_ends(key: &str) -> Option<(String, String)> {
    let t: Vec<&str> = key.split('_').filter(|x| !x.is_empty()).collect();
    (t.len() >= 2).then(|| (t[0].to_string(), t[t.len() - 1].to_string()))
}

/// Rows dropped from a bundle, with the reason.
#[derive(Clone, Debug, Serialize)]
pub struct Exclusion {
    pub id: String,
    pub split: String,
    pub source_kind: SourceKind,
    pub reason: String,
    pub detail: Value,
}

/// Apply the bundle rules to one split's candidate rows: parent membership,
/// Train-side source group of hosted captures, evaluation-group closure
/// (Train), held-out and unknown styles (Train), text overlap with evaluation
/// splits (Train) and duplicate texts. Rows are returned sorted by id.
/// `train_side_groups` maps every Train and Train-mining source group a
/// capture names to its split-manifest domain.
pub fn select(
    split: &str,
    candidates: Vec<Row>,
    parents: &HashMap<String, Parent>,
    eval_keys: &EvalKeys,
    eval_texts: &HashMap<String, String>,
    train_families: &HashSet<String>,
    train_side_groups: &HashMap<String, String>,
) -> (Vec<Row>, Vec<Exclusion>) {
    let train = split == "train";
    let mut kept: Vec<Row> = Vec::new();
    let mut ex = Vec::new();
    let drop = |r: &Row, reason: &str, detail: Value, ex: &mut Vec<Exclusion>| {
        ex.push(Exclusion {
            id: r.id.clone(),
            split: split.into(),
            source_kind: r.source_kind,
            reason: reason.into(),
            detail,
        })
    };
    let mut sorted = candidates;
    // Humans first, then local, frontier and legacy, each by id: earlier rows win duplicates.
    sorted.sort_by(|a, b| (a.source_kind, &a.id).cmp(&(b.source_kind, &b.id)));
    let mut seen_text: HashMap<String, String> = HashMap::new();
    let mut seen_id: HashSet<String> = HashSet::new();
    for mut r in sorted {
        if !seen_id.insert(r.id.clone()) {
            drop(&r, "duplicate_id", Value::Null, &mut ex);
            continue;
        }
        if matches!(r.source_kind, SourceKind::LocalPositive | SourceKind::Frontier) {
            let Some(pid) = r.parent_id.clone() else {
                drop(&r, "positive_without_parent", Value::Null, &mut ex);
                continue;
            };
            match parents.get(&pid) {
                Some(p) if p.split == split || (train && p.split == "train_mining") => {
                    if p.source_group != r.source_group {
                        drop(&r, "parent_source_group_differs", json!({"parent": pid}), &mut ex);
                        continue;
                    }
                    r.writer_key = r.writer_key.clone().or_else(|| p.writer_key.clone());
                    r.genre = p.genre.clone();
                    if r.collection.is_none() {
                        r.collection = p.collection.clone();
                    }
                    // The split manifest names the domain; pipelines spell some differently.
                    if r.domain != p.domain {
                        r.record_domain = Some(std::mem::replace(&mut r.domain, p.domain.clone()));
                    }
                }
                Some(p) => {
                    drop(&r, "parent_in_other_split", json!({"parent": pid, "parent_split": p.split}), &mut ex);
                    continue;
                }
                None => {
                    drop(&r, "parent_not_found", json!({"parent": pid}), &mut ex);
                    continue;
                }
            }
        }
        if r.source_kind == SourceKind::HostedCapture {
            match train_side_groups.get(&r.source_group) {
                Some(d) if train => r.domain = d.clone(),
                _ => {
                    drop(&r, "capture_source_not_train_family", json!({"source_group": r.source_group}), &mut ex);
                    continue;
                }
            }
        }
        if train {
            if let Some((s, why)) = eval_keys.collision(&r) {
                drop(&r, "train_row_in_evaluation_group", json!({"evaluation_split": s, "key": why}), &mut ex);
                continue;
            }
            if r.held_out_style {
                drop(&r, "held_out_style_in_train", json!({"style_partition": r.style_partition,
                    "text_match": r.style_text_match}), &mut ex);
                continue;
            }
            if style_unknown(&r) {
                drop(&r, "style_not_in_frozen_partitions", json!({"voice_id": r.voice_id, "brief_id": r.brief_id}), &mut ex);
                continue;
            }
            if let Some(s) = eval_texts.get(&r.text_sha256) {
                drop(&r, "text_in_evaluation_split", json!({"evaluation_split": s}), &mut ex);
                continue;
            }
        } else if train_families.contains(&r.source_group) {
            drop(&r, "evaluation_row_in_train_group", Value::Null, &mut ex);
            continue;
        }
        if let Some(first) = seen_text.get(&r.text_sha256) {
            drop(&r, "duplicate_text", json!({"kept": first}), &mut ex);
            continue;
        }
        seen_text.insert(r.text_sha256.clone(), r.id.clone());
        kept.push(r);
    }
    kept.sort_by(|a, b| a.id.cmp(&b.id));
    (kept, ex)
}

/// Counts per domain x origin x generator label.
pub fn counts(rows: &[Row]) -> Value {
    let mut m: BTreeMap<String, BTreeMap<String, BTreeMap<String, usize>>> = BTreeMap::new();
    for r in rows {
        *m.entry(r.domain.clone())
            .or_default()
            .entry(r.origin.clone())
            .or_default()
            .entry(r.generator_label.clone().unwrap_or_else(|| "human".into()))
            .or_default() += 1;
    }
    serde_json::to_value(m).unwrap()
}

#[cfg(test)]
mod tests;

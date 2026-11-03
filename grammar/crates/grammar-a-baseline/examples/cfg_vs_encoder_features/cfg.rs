//! CFG arm (A): window index per exporter chunk, then a verified merge of the
//! receipts into sparse per-document expected counts, coverage counts and the
//! coverage-restricted texts of PROTOCOL.md section 7. No vocabulary,
//! normalization or score is fitted here.
use crate::common::*;
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::Path,
};

pub const COMPLETE_TIERS: [&str; 2] = ["native_complete", "lexical_fallback_complete"];
pub const RESTRICTED_MIN_TOKENS: u64 = 100;
const RESTRICTED_CHUNK_DOCS: usize = 100;

#[derive(Deserialize)]
pub struct WindowSource {
    pub start: usize,
    pub end: usize,
}

#[derive(Deserialize)]
pub struct Window {
    pub source: WindowSource,
    pub tier: String,
    #[serde(default)]
    pub limit_reason: Option<Value>,
    #[serde(default)]
    pub execution_error: Option<IgnoredAny>,
    #[serde(default)]
    pub projection_error: Option<IgnoredAny>,
    #[serde(default)]
    pub tokens: Vec<IgnoredAny>,
}

/// Compact per-document window index: (start, end, tier, tokens).
#[derive(Deserialize, serde::Serialize, Clone, Debug, PartialEq)]
pub struct IndexRow {
    pub id: String,
    pub prefix: String,
    pub windows_sha256: String,
    pub windows: Vec<(usize, usize, String, u64)>,
    pub tier_tokens: BTreeMap<String, u64>,
    pub tier_windows: BTreeMap<String, u64>,
    pub limit_reasons: BTreeMap<String, u64>,
    pub execution_error_windows: u64,
    pub projection_error_windows: u64,
    pub window_tokens: u64,
}

impl IndexRow {
    pub fn complete_tokens(&self) -> u64 {
        COMPLETE_TIERS
            .iter()
            .map(|t| self.tier_tokens.get(*t).copied().unwrap_or(0))
            .sum()
    }
}

pub fn index_windows(id: &str, prefix: &str, sha: &str, lines: &[u8]) -> Result<IndexRow> {
    let mut row = IndexRow {
        id: id.into(),
        prefix: prefix.into(),
        windows_sha256: sha.into(),
        windows: Vec::new(),
        tier_tokens: BTreeMap::new(),
        tier_windows: BTreeMap::new(),
        limit_reasons: BTreeMap::new(),
        execution_error_windows: 0,
        projection_error_windows: 0,
        window_tokens: 0,
    };
    for line in lines.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let w: Window = serde_json::from_slice(line)?;
        ensure!(w.source.start <= w.source.end, "window span order");
        let n = w.tokens.len() as u64;
        *row.tier_tokens.entry(w.tier.clone()).or_default() += n;
        *row.tier_windows.entry(w.tier.clone()).or_default() += 1;
        if let Some(Value::String(r)) = &w.limit_reason {
            // Enumerated budget names, never text.
            ensure!(
                r.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "unexpected limit reason form"
            );
            *row.limit_reasons.entry(r.clone()).or_default() += 1;
        }
        row.execution_error_windows += w.execution_error.is_some() as u64;
        row.projection_error_windows += w.projection_error.is_some() as u64;
        row.window_tokens += n;
        row.windows.push((w.source.start, w.source.end, w.tier, n));
    }
    row.windows.sort_by_key(|w| (w.0, w.1));
    Ok(row)
}

/// Section 7 restricted text: complete-window source byte spans in source
/// order, joined by one newline.
pub fn restricted_text(text: &str, row: &IndexRow) -> Result<String> {
    let mut parts = Vec::new();
    for (start, end, tier, _) in &row.windows {
        if COMPLETE_TIERS.contains(&tier.as_str()) {
            ensure!(
                *end <= text.len() && text.is_char_boundary(*start) && text.is_char_boundary(*end),
                "window span outside text or not on a character boundary"
            );
            parts.push(&text[*start..*end]);
        }
    }
    Ok(parts.join("\n"))
}

#[derive(Deserialize)]
struct ReportEntry {
    id: String,
    prefix: String,
}

#[derive(Deserialize)]
struct ExportRow {
    id: String,
    receipt: Value,
    total_elapsed_micros: u64,
    windows_sha256: String,
    receipt_sha256: String,
}

pub fn index_cli(export: &Path, out: &Path) -> Result<()> {
    let report: Value = serde_json::from_slice(&fs::read(export.join("report.json"))?)?;
    ensure!(report["complete"] == true, "incomplete export");
    let rows_bytes = fs::read(export.join("rows.jsonl"))?;
    ensure!(
        sha_hex(&rows_bytes) == report["rows_sha256"].as_str().context("rows_sha256")?,
        "rows digest"
    );
    let mut expected = HashMap::new();
    for line in rows_bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let r: ExportRow = serde_json::from_slice(line)?;
        expected.insert(r.id, (r.windows_sha256, r.receipt["summary"].clone()));
    }
    let entries: Vec<ReportEntry> = serde_json::from_value(report["documents"].clone())?;
    let mut writer = JsonlWriter::create(out)?;
    for e in &entries {
        let path = export.join(format!("{}.windows.jsonl", e.prefix));
        let bytes = fs::read(&path)?;
        let sha = sha_hex(&bytes);
        let (want, summary) = expected.get(&e.id).context("report id without row")?;
        ensure!(sha == *want, "windows digest mismatch for {}", e.prefix);
        let row = index_windows(&e.id, &e.prefix, &sha, &bytes)?;
        ensure!(
            row.windows.len() as u64 == summary["windows"].as_u64().context("windows")?,
            "window count mismatch for {}",
            e.prefix
        );
        writer.write(&row)?;
    }
    let (n, sha) = writer.finish()?;
    println!("{}", json!({"indexed":n,"sha256":sha}));
    Ok(())
}

// Mirrors validate_key/validate_symbol in
// prepare_supervised_forest/pipeline.rs at 3aa51700 (frozen sha256
// 1213b225...): only structural grammar signatures and availability counts
// may become coordinates.
const POOLS: [&str; 6] = [
    "native_complete",
    "fallback_complete",
    "partial",
    "search_limited",
    "lexical_recovery",
    "unavailable",
];
const TIERS: [&str; 8] = [
    "NativeComplete",
    "LexicalFallbackComplete",
    "Partial",
    "ResourceLimit",
    "BudgetSkipped",
    "TokenFragment",
    "ExecutionFailure",
    "Layout",
];
const SYMBOL_KEYS: [&str; 13] = [
    "agreement", "count", "proper", "form", "tense", "degree", "voice", "case", "person", "number",
    "aspect", "mood", "polarity",
];
const SYMBOL_VALUES: [&str; 35] = [
    "first_singular",
    "second",
    "third_singular",
    "plural",
    "singular",
    "mass",
    "yes",
    "no",
    "any",
    "finite",
    "bare",
    "present_participle",
    "past_participle",
    "perfect_participle",
    "passive_participle",
    "gerund",
    "present",
    "past",
    "future",
    "positive",
    "comparative",
    "superlative",
    "active",
    "passive",
    "nominative",
    "accusative",
    "genitive",
    "first",
    "third",
    "perfect",
    "progressive",
    "indicative",
    "subjunctive",
    "imperative",
    "negative",
];

fn validate_symbol(value: &Value) -> Result<()> {
    let symbol = value.as_object().context("grammar symbol object")?;
    ensure!(
        symbol.len() == 2 && symbol.contains_key("category") && symbol.contains_key("features"),
        "non-structural symbol fields"
    );
    let category = value["category"].as_str().context("category")?;
    ensure!(
        !category.is_empty() && category.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_'),
        "invalid grammar category"
    );
    for (key, v) in value["features"].as_object().context("features")? {
        ensure!(
            SYMBOL_KEYS.contains(&key.as_str())
                && v.as_str().is_some_and(|v| SYMBOL_VALUES.contains(&v)),
            "symbol feature outside frozen grammatical vocabulary"
        );
    }
    Ok(())
}

pub fn validate_key(key: &str) -> Result<()> {
    if matches!(
        key,
        "availability/unresolved_token"
            | "availability/no_real_reading_token"
            | "availability/forced_window"
    ) || key
        .strip_prefix("availability/window/")
        .is_some_and(|t| TIERS.contains(&t))
    {
        return Ok(());
    }
    let (pool, rest) = key.split_once('/').context("unpooled structural key")?;
    let (kind, signature) = rest.split_once('/').context("missing signature")?;
    ensure!(
        POOLS.contains(&pool) && matches!(kind, "symbol" | "production" | "leaf") && !signature.is_empty(),
        "unsupported structural coordinate"
    );
    let signature: Value = serde_json::from_str(signature).context("non-JSON signature")?;
    match kind {
        "symbol" => validate_symbol(&signature)?,
        "production" => {
            let t = signature.as_array().context("production tuple")?;
            ensure!(t.len() == 3, "production fields");
            validate_symbol(&t[0])?;
            let children = t[1].as_array().context("children")?;
            for c in children {
                validate_symbol(c)?;
            }
            ensure!(
                t[2].is_null() || t[2].as_u64().is_some_and(|i| i < children.len() as u64),
                "production head child"
            );
        }
        "leaf" => {
            let t = signature.as_array().context("leaf tuple")?;
            ensure!(t.len() == 4, "leaf fields");
            ensure!(matches!(t[0].as_str(), Some("inventory" | "fallback")), "leaf marker");
            ensure!(
                t[1].as_str().is_some_and(|s| !s.is_empty()
                    && s.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_')),
                "leaf POS"
            );
            validate_symbol(&t[2])?;
            ensure!(
                t[3].is_null()
                    || t[3]
                        .as_str()
                        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphabetic())),
                "leaf morphology"
            );
        }
        _ => bail!("kind"),
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct SelectedDoc {
    pub id: String,
    pub kind: String,
    pub domain: String,
    pub text_sha256: String,
    pub chunk: String,
    pub uses: Vec<String>,
}

#[derive(Default)]
struct Tally {
    documents: u64,
    tokens: u64,
    tier_tokens: BTreeMap<String, u64>,
    unresolved_tokens: u64,
    documents_with_unresolved: u64,
    fully_complete_documents: u64,
    restricted_qualifying_documents: u64,
    restricted_tokens: u64,
    execution_failure_documents: u64,
    projection_failure_documents: u64,
    budget_skipped_documents: u64,
    resource_limited_documents: u64,
    parse_micros: u64,
}

impl Tally {
    fn json(&self) -> Value {
        let share = |n: u64| if self.tokens == 0 { 0.0 } else { n as f64 / self.tokens as f64 };
        json!({"documents":self.documents,"tokens":self.tokens,
            "tier_token_share":self.tier_tokens.iter().map(|(k,v)| (k.clone(), json!(share(*v)))).collect::<BTreeMap<_,_>>(),
            "complete_token_share":share(COMPLETE_TIERS.iter().map(|t| self.tier_tokens.get(*t).copied().unwrap_or(0)).sum()),
            "unresolved_tokens":self.unresolved_tokens,"unresolved_token_share":share(self.unresolved_tokens),
            "documents_with_unresolved_tokens":self.documents_with_unresolved,
            "fully_complete_documents":self.fully_complete_documents,
            "restricted_qualifying_documents":self.restricted_qualifying_documents,
            "restricted_tokens":self.restricted_tokens,
            "execution_failure_documents":self.execution_failure_documents,
            "projection_failure_documents":self.projection_failure_documents,
            "budget_skipped_documents":self.budget_skipped_documents,
            "resource_limited_documents":self.resource_limited_documents,
            "parse_seconds":self.parse_micros as f64 / 1e6})
    }
}

fn groups_of(doc: &SelectedDoc) -> BTreeSet<String> {
    let mut g = BTreeSet::new();
    g.insert("all".to_string());
    for u in &doc.uses {
        let parts: Vec<&str> = u.split(':').collect();
        g.insert(format!("{}:{}", parts[0], parts[1]));
        if parts[0] == "t12" {
            g.insert(format!("t12:{}:{}", parts[1], doc.domain));
        }
    }
    g
}

/// Writes section 7 restricted texts for every T1/T2 document with at least
/// 100 complete-window tokens: `documents.jsonl`, `selection.jsonl` (IDs and
/// counts) and 100-document request chunks. Returns the output record.
pub fn write_restricted(
    selected: &BTreeMap<String, SelectedDoc>,
    index: &HashMap<String, IndexRow>,
    inputs_path: &Path,
    summary: &Value,
    out: &Path,
) -> Result<Value> {
    create_new_dir(out)?;
    fs::create_dir(out.join("chunks"))?;
    let mut restricted_all = JsonlWriter::create(&out.join("documents.jsonl"))?;
    let mut restricted_chunks = Vec::new();
    let mut chunk: Option<(String, JsonlWriter)> = None;
    let mut restricted_meta = JsonlWriter::create(&out.join("selection.jsonl"))?;
    let mut produced = 0usize;
    let mut considered = 0usize;
    let mut inputs = HashedLines::open(inputs_path)?;
    while let Some(line) = inputs.next_line()? {
        if line.is_empty() {
            continue;
        }
        let req: Request = serde_json::from_slice(line)?;
        let doc = &selected[&req.id];
        if !doc.uses.iter().any(|u| u.starts_with("t12:")) {
            continue;
        }
        considered += 1;
        let ix = index.get(&req.id).context("T1/T2 document missing from window index")?;
        let complete = ix.complete_tokens();
        if complete < RESTRICTED_MIN_TOKENS {
            continue;
        }
        let text = restricted_text(&req.text, ix)?;
        let sha = sha_hex(text.as_bytes());
        if produced % RESTRICTED_CHUNK_DOCS == 0 {
            if let Some((name, w)) = chunk.take() {
                let (n, s) = w.finish()?;
                restricted_chunks.push(json!({"chunk":name,"documents":n,"sha256":s}));
            }
            let name = format!("r{:04}", produced / RESTRICTED_CHUNK_DOCS);
            let w = JsonlWriter::create(&out.join(format!("chunks/{name}.jsonl")))?;
            chunk = Some((name, w));
        }
        let r = Request { id: req.id.clone(), text };
        chunk.as_mut().unwrap().1.write(&r)?;
        restricted_all.write(&r)?;
        restricted_meta.write(&json!({"id":req.id,"restricted_text_sha256":sha,"complete_window_tokens":complete,
            "restricted_bytes":r.text.len(),"chunk":chunk.as_ref().unwrap().0}))?;
        produced += 1;
    }
    if let Some((name, w)) = chunk.take() {
        let (n, s) = w.finish()?;
        restricted_chunks.push(json!({"chunk":name,"documents":n,"sha256":s}));
    }
    ensure!(inputs.finish() == summary["inputs"]["sha256"].as_str().unwrap_or(""), "inputs digest changed");
    let (r_rows, r_sha) = restricted_all.finish()?;
    let (_, r_meta_sha) = restricted_meta.finish()?;
    Ok(json!({"file":"documents.jsonl","t12_documents_considered":considered,"rows":r_rows,"sha256":r_sha,
        "selection_sha256":r_meta_sha,"chunks":restricted_chunks,
        "rule":"T1/T2 documents with at least 100 parser tokens in native_complete or lexical_fallback_complete windows; those windows' source byte spans in source order joined by one newline"}))
}

/// Restricted texts as soon as every chunk holding a T1/T2 document is
/// exported and indexed; byte-identical to the restricted output of
/// `cfg-collect`.
pub fn restricted_cli(frozen_path: &Path, selection: &Path, exports: &Path, out: &Path) -> Result<()> {
    let frozen = read_frozen(frozen_path)?;
    let engine_identity = frozen_str(&frozen, "/pipelines/cfg/engine_identity")?;
    let parser_identity = frozen_str(&frozen, "/pipelines/cfg/parser_identity")?;
    let summary: Value = serde_json::from_slice(&fs::read(selection.join("selection/summary.json"))?)?;
    let docs_path = selection.join("selection/documents.jsonl");
    ensure!(hash_file(&docs_path)? == summary["documents"]["sha256"].as_str().context("documents sha")?, "selection digest");
    let mut selected: BTreeMap<String, SelectedDoc> = BTreeMap::new();
    let mut needed_chunks = BTreeSet::new();
    for line in fs::read(&docs_path)?.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let d: SelectedDoc = serde_json::from_slice(line)?;
        if d.uses.iter().any(|u| u.starts_with("t12:")) {
            needed_chunks.insert(d.chunk.clone());
        }
        selected.insert(d.id.clone(), d);
    }
    let mut index: HashMap<String, IndexRow> = HashMap::new();
    for c in summary["inputs"]["chunks"].as_array().context("chunks")? {
        let name = c["chunk"].as_str().context("chunk")?;
        if !needed_chunks.contains(name) {
            continue;
        }
        let dir = exports.join(name);
        let contract: Value = serde_json::from_slice(&fs::read(dir.join("contract.json"))?)?;
        ensure!(contract["engine_identity"] == engine_identity, "{name}: engine identity");
        ensure!(contract["parser_identity"] == parser_identity, "{name}: parser identity");
        ensure!(contract["input_sha256"] == c["sha256"], "{name}: input binding");
        let report: Value = serde_json::from_slice(&fs::read(dir.join("report.json"))?)?;
        ensure!(report["complete"] == true, "{name}: incomplete");
        let rows_bytes = fs::read(dir.join("rows.jsonl"))?;
        ensure!(sha_hex(&rows_bytes) == report["rows_sha256"], "{name}: rows digest");
        let mut want = HashMap::new();
        for line in rows_bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            let r: ExportRow = serde_json::from_slice(line)?;
            ensure!(r.receipt["source_sha256"] == selected[&r.id].text_sha256.as_str(), "receipt source hash");
            want.insert(r.id, r.windows_sha256);
        }
        for line in fs::read(exports.join(format!("{name}.index.jsonl")))?.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            let r: IndexRow = serde_json::from_slice(line)?;
            ensure!(want.get(&r.id) == Some(&r.windows_sha256), "{name}: index/row windows digest");
            index.insert(r.id.clone(), r);
        }
    }
    let inputs_path = selection.join("inputs/documents.jsonl");
    let record = write_restricted(&selected, &index, &inputs_path, &summary, out)?;
    write_new_json(&out.join("manifest.json"), &json!({"schema":"slop_ninja_cfg_vs_encoder_restricted_texts_v1",
        "protocol_id":PROTOCOL_ID,"frozen_sha256":FROZEN_SHA256,"created":now_utc(),
        "chunks_used":needed_chunks,"engine_identity":engine_identity,"parser_identity":parser_identity,"restricted":record}))?;
    println!("{}", serde_json::to_string(&json!({"considered":record["t12_documents_considered"],"rows":record["rows"],"chunks":record["chunks"].as_array().map(|a| a.len())}))?);
    Ok(())
}

pub fn collect_cli(frozen_path: &Path, selection: &Path, exports: &Path, out: &Path) -> Result<()> {
    let frozen = read_frozen(frozen_path)?;
    let engine_identity = frozen_str(&frozen, "/pipelines/cfg/engine_identity")?;
    let parser_identity = frozen_str(&frozen, "/pipelines/cfg/parser_identity")?;
    let summary: Value = serde_json::from_slice(&fs::read(selection.join("selection/summary.json"))?)?;
    let docs_path = selection.join("selection/documents.jsonl");
    ensure!(
        hash_file(&docs_path)? == summary["documents"]["sha256"].as_str().context("documents sha")?,
        "selection documents digest"
    );
    let mut selected: BTreeMap<String, SelectedDoc> = BTreeMap::new();
    let mut order = Vec::new();
    for line in fs::read(&docs_path)?.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let d: SelectedDoc = serde_json::from_slice(line)?;
        order.push(d.id.clone());
        selected.insert(d.id.clone(), d);
    }
    let inputs_path = selection.join("inputs/documents.jsonl");
    ensure!(
        hash_file(&inputs_path)? == summary["inputs"]["sha256"].as_str().context("inputs sha")?,
        "inputs digest"
    );
    create_new_dir(out)?;

    let mut key_ids: HashMap<String, u32> = HashMap::new();
    let mut keys: Vec<String> = Vec::new();
    struct Feat {
        row: Value,
        counts: Vec<(u32, f64)>,
    }
    let mut feats: HashMap<String, Feat> = HashMap::new();
    let mut index: HashMap<String, IndexRow> = HashMap::new();
    let mut chunk_records = Vec::new();
    let mut parser_load_millis = 0u64;
    let mut exporter_impl = BTreeSet::new();
    let mut policies = BTreeSet::new();
    for c in summary["inputs"]["chunks"].as_array().context("chunks")? {
        let name = c["chunk"].as_str().context("chunk")?;
        let chunk_sha = c["sha256"].as_str().context("chunk sha")?;
        let dir = exports.join(name);
        let contract: Value = serde_json::from_slice(&fs::read(dir.join("contract.json"))?)?;
        ensure!(contract["engine_identity"] == engine_identity, "{name}: engine identity");
        ensure!(contract["parser_identity"] == parser_identity, "{name}: parser identity");
        ensure!(contract["input_sha256"] == chunk_sha, "{name}: input binding");
        exporter_impl.insert(contract["implementation_sha256"].as_str().unwrap_or("").to_string());
        policies.insert(serde_json::to_string(&contract["policy"])?);
        parser_load_millis += contract["parser_load_millis"].as_u64().unwrap_or(0);
        let report_bytes = fs::read(dir.join("report.json"))?;
        let report: Value = serde_json::from_slice(&report_bytes)?;
        ensure!(report["complete"] == true, "{name}: incomplete");
        let rows_bytes = fs::read(dir.join("rows.jsonl"))?;
        ensure!(sha_hex(&rows_bytes) == report["rows_sha256"], "{name}: rows digest");
        let index_path = exports.join(format!("{name}.index.jsonl"));
        for line in fs::read(&index_path)?.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            let r: IndexRow = serde_json::from_slice(line)?;
            index.insert(r.id.clone(), r);
        }
        let mut n = 0usize;
        for line in rows_bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            let r: ExportRow = serde_json::from_slice(line)?;
            let doc = selected.get(&r.id).context("exported id not selected")?;
            ensure!(doc.chunk == name, "document in wrong chunk");
            let receipt = &r.receipt;
            ensure!(receipt["schema"] == "slop_ninja_cfg_document_fallback_v1", "receipt schema");
            ensure!(receipt["feature_schema"] == "slop_ninja_cfg_forest_expectations_v1", "feature schema");
            ensure!(
                receipt["resolver"] == "acyclic-uniform-derivations-max-covered-min-islands-v1",
                "resolver"
            );
            ensure!(receipt["engine_identity"] == engine_identity, "receipt engine");
            ensure!(receipt["parser_identity"] == parser_identity, "receipt parser");
            ensure!(receipt["source_sha256"] == doc.text_sha256.as_str(), "receipt source hash");
            let s = &receipt["summary"];
            let f = &receipt["features"];
            let tokens = f["token_opportunities"].as_u64().context("tokens")?;
            let represented = f["represented_tokens"].as_u64().context("represented")?;
            let unresolved = f["unresolved_tokens"].as_u64().context("unresolved")?;
            ensure!(tokens == s["tokens"].as_u64().unwrap_or(u64::MAX), "token denominator");
            ensure!(represented + unresolved == tokens, "token accounting");
            ensure!(s["input_bytes"] == s["accounted_bytes"], "byte accounting");
            let ix = index.get(&r.id).context("document missing from window index")?;
            ensure!(ix.windows_sha256 == r.windows_sha256, "index/row windows digest");
            let mut counts = Vec::new();
            for (k, v) in f["counts"].as_object().context("counts")? {
                validate_key(k).with_context(|| format!("{name}: invalid key"))?;
                let v = v.as_f64().context("count")?;
                ensure!(v.is_finite() && v >= 0.0, "count value");
                let id = *key_ids.entry(k.clone()).or_insert_with(|| {
                    keys.push(k.clone());
                    (keys.len() - 1) as u32
                });
                counts.push((id, v));
            }
            let complete = ix.complete_tokens();
            let row = json!({"id":r.id,"token_opportunities":tokens,"represented_tokens":represented,
                "unresolved_tokens":unresolved,"projection_available":f["projection_available"],
                "search_exhaustive":f["search_exhaustive"],"expected_fallback_leaves":f["expected_fallback_leaves"],
                "summary":s,"window_tokens_by_tier":ix.tier_tokens,"windows_by_tier":ix.tier_windows,
                "complete_window_tokens":complete,
                "native_complete_window_tokens":ix.tier_tokens.get("native_complete").copied().unwrap_or(0),
                "window_token_sum_matches_document":ix.window_tokens == tokens,
                "restricted_qualifies":complete >= RESTRICTED_MIN_TOKENS,
                "total_elapsed_micros":r.total_elapsed_micros,
                "receipt_sha256":r.receipt_sha256,"windows_sha256":r.windows_sha256,"chunk":name});
            feats.insert(r.id.clone(), Feat { row, counts });
            n += 1;
        }
        chunk_records.push(json!({"chunk":name,"documents":n,"report_sha256":sha_hex(&report_bytes),
            "index_sha256":hash_file(&index_path)?}));
    }
    ensure!(policies.len() == 1, "mixed document policies across chunks");

    // Sorted dictionary and remapped sparse rows, in selection order.
    let mut sorted: Vec<(String, u32)> = keys.iter().cloned().zip(0u32..).collect();
    sorted.sort();
    let mut remap = vec![0u32; keys.len()];
    for (new, (_, old)) in sorted.iter().enumerate() {
        remap[*old as usize] = new as u32;
    }
    let key_list: Vec<&str> = sorted.iter().map(|(k, _)| k.as_str()).collect();
    let keys_sha = write_new_json(&out.join("cfg-keys.json"), &json!({"schema":"slop_ninja_cfg_vs_encoder_cfg_keys_v1","keys":key_list}))?;
    let mut writer = JsonlWriter::create(&out.join("cfg-features.jsonl"))?;
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    let mut missing = 0u64;
    let mut latencies = Vec::new();
    for id in &order {
        let doc = &selected[id];
        let Some(feat) = feats.get(id) else {
            missing += 1;
            continue;
        };
        let mut counts: Vec<(u32, f64)> = feat.counts.iter().map(|(k, v)| (remap[*k as usize], *v)).collect();
        counts.sort_by_key(|c| c.0);
        let mut row = feat.row.clone();
        row["kind"] = json!(doc.kind);
        row["counts"] = json!(counts);
        writer.write(&row)?;
        let s = &row["summary"];
        let ix = &index[id];
        let tokens = row["token_opportunities"].as_u64().unwrap();
        let complete = ix.complete_tokens();
        latencies.push(row["total_elapsed_micros"].as_u64().unwrap());
        for g in groups_of(doc) {
            let t = tallies.entry(g).or_default();
            t.documents += 1;
            t.tokens += tokens;
            for (tier, n) in &ix.tier_tokens {
                *t.tier_tokens.entry(tier.clone()).or_default() += n;
            }
            let u = row["unresolved_tokens"].as_u64().unwrap();
            t.unresolved_tokens += u;
            t.documents_with_unresolved += (u > 0) as u64;
            t.fully_complete_documents += (complete == tokens && u == 0) as u64;
            if complete >= RESTRICTED_MIN_TOKENS {
                t.restricted_qualifying_documents += 1;
                t.restricted_tokens += complete;
            }
            t.execution_failure_documents += (s["execution_failures"].as_u64().unwrap_or(0) > 0) as u64;
            t.projection_failure_documents += (s["projection_failures"].as_u64().unwrap_or(0) > 0) as u64;
            t.budget_skipped_documents += (s["budget_skipped_windows"].as_u64().unwrap_or(0) > 0) as u64;
            t.resource_limited_documents += (s["resource_limited_windows"].as_u64().unwrap_or(0) > 0) as u64;
            t.parse_micros += row["total_elapsed_micros"].as_u64().unwrap();
        }
    }
    let (feat_rows, feat_sha) = writer.finish()?;
    latencies.sort_unstable();
    let pct = |q: usize| latencies.get(latencies.len().saturating_sub(1) * q / 100).map(|v| *v as f64 / 1e6);

    // Coverage-restricted texts for the T1/T2 population.
    let restricted = write_restricted(&selected, &index, &inputs_path, &summary, &out.join("restricted"))?;

    let exporter_binary = frozen_str(&frozen, "/pipelines/cfg/binary_sha256")?;
    let manifest = json!({
        "schema":"slop_ninja_cfg_vs_encoder_feature_manifest_v1","arm":"A","protocol_id":PROTOCOL_ID,
        "frozen_sha256":FROZEN_SHA256,"created":now_utc(),
        "inputs":{"selection_documents_sha256":summary["documents"]["sha256"],"inputs_sha256":summary["inputs"]["sha256"],
            "chunks":chunk_records},
        "pipeline":{"exporter_binary_sha256":exporter_binary,"exporter_implementation_sha256":exporter_impl,
            "engine_identity":engine_identity,"parser_identity":parser_identity,"policy":serde_json::from_str::<Value>(policies.iter().next().unwrap())?,
            "key_validation":"mirrors prepare_supervised_forest/pipeline.rs validate_key at 3aa51700"},
        "outputs":{"features":{"file":"cfg-features.jsonl","rows":feat_rows,"sha256":feat_sha,
                "layout":"per document: receipt token accounting, summary, window tokens by tier, sparse [key_index, expected_count] pairs"},
            "keys":{"file":"cfg-keys.json","keys":key_list.len(),"sha256":keys_sha},
            "restricted":restricted},
        "coverage":{"selected_documents":order.len(),"exported_documents":feat_rows,"missing_documents":missing,
            "by_group":tallies.iter().map(|(k,v)| (k.clone(), v.json())).collect::<BTreeMap<_,_>>()},
        "runtime":{"parse_seconds_total":latencies.iter().sum::<u64>() as f64 / 1e6,
            "parser_load_seconds_total":parser_load_millis as f64 / 1e3,
            "document_seconds_p50":pct(50),"document_seconds_p95":pct(95),"document_seconds_max":latencies.last().map(|v| *v as f64 / 1e6)},
        "metrics_computed":false
    });
    write_new_json(&out.join("manifest.json"), &manifest)?;
    println!("{}", serde_json::to_string(&json!({"coverage":manifest["coverage"]["by_group"]["all"],"missing":missing,"restricted":manifest["outputs"]["restricted"]["rows"],"keys":key_list.len()}))?);
    Ok(())
}

//! Verified loading of the frozen selection and the extracted features. Every
//! file is hashed while it is parsed and compared with the manifest that bound
//! it. Only IDs, metadata, counts and vectors are materialized; no text.
use crate::common::*;
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::Path,
};

pub const N_FW: usize = 203;
pub const ENC_DIM: usize = 768;
/// Window pools named by the exporter, in the frozen order.
pub const POOLS: [&str; 6] = [
    "native_complete",
    "fallback_complete",
    "partial",
    "search_limited",
    "lexical_recovery",
    "unavailable",
];
/// Pools summed by the `*_pool_merged_v1` projections (FROZEN.json arms.A).
pub const MERGED_POOLS: [u8; 5] = [0, 1, 2, 3, 4];
/// Pools kept by the coverage-restricted CFG arm.
pub const COMPLETE_POOLS: [u8; 2] = [0, 1];

#[derive(Clone, Debug)]
pub struct DocMeta {
    pub id: String,
    pub kind: String,
    pub domain: String,
    pub writer: Option<String>,
    pub words: u32,
}

#[derive(Clone, Debug, Default)]
pub struct CfgRow {
    pub tokens: f64,
    pub complete: f64,
    pub projection_available: bool,
    pub search_exhaustive: bool,
    pub execution_failures: u64,
    pub keys: Vec<u32>,
    pub vals: Vec<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    /// kind: 0 symbol, 1 production, 2 leaf; merged indexes the pool-free key.
    Pool { pool: u8, kind: u8, merged: u32 },
    /// doc_scope: denominator one (window counts and forced windows).
    Avail { doc_scope: bool },
}

#[derive(Clone, Debug, Default)]
pub struct EncRow {
    pub h_raw: Vec<f64>,
    pub z_shared: Vec<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct StyloRow {
    pub fw: Vec<u32>,
    pub words: u64,
    pub c3: Vec<(u32, u32)>,
    pub c3_total: u64,
}

#[derive(Clone, Debug)]
pub struct AuthorRow {
    pub domain: String,
    pub writer: String,
    pub h: String,
    pub role: String,
    pub fold: u32,
    pub withheld: bool,
    pub simulated_withheld: bool,
    pub selected: bool,
    pub queries: Vec<String>,
    pub refs: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct TargetRow {
    pub stratum: String,
    pub target: String,
    pub eligible: bool,
    pub profile: Vec<String>,
    pub has_train: bool,
}

#[derive(Clone, Debug)]
pub struct RecordRow {
    pub id: String,
    pub stratum: String,
    pub kind: String,
    pub family: String,
    pub target: String,
    pub parent_id: Option<String>,
    pub parent_writer: Option<String>,
    pub source_available: bool,
}

/// Text variant: all text, or the coverage-restricted complete-window text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Text {
    Full,
    Restricted,
}

impl Text {
    pub fn name(self) -> &'static str {
        match self {
            Text::Full => "all_text",
            Text::Restricted => "coverage_restricted",
        }
    }
    pub fn slot(self) -> usize {
        match self {
            Text::Full => 0,
            Text::Restricted => 1,
        }
    }
}

#[derive(Default)]
pub struct Inputs {
    pub docs: Vec<DocMeta>,
    pub index: HashMap<String, usize>,
    pub keys: Vec<KeyKind>,
    pub n_merged: usize,
    pub cfg: Vec<Option<CfgRow>>,
    pub enc: [Vec<Option<EncRow>>; 2],
    pub stylo: [Vec<Option<StyloRow>>; 2],
    pub char3: Vec<String>,
    pub authors: Vec<AuthorRow>,
    pub targets: Vec<TargetRow>,
    pub records: Vec<RecordRow>,
    /// Per document: coverage-restricted text exists (T1/T2, >= 100 complete tokens).
    pub restricted: Vec<bool>,
    pub bindings: BTreeMap<String, String>,
}

impl Inputs {
    pub fn doc(&self, id: &str) -> Result<usize> {
        self.index
            .get(id)
            .copied()
            .with_context(|| format!("document not in selection: {id}"))
    }
}

fn manifest(path: &Path, expected: &str) -> Result<Value> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    ensure!(
        sha_hex(&bytes) == expected,
        "manifest sha256 mismatch: {}",
        path.display()
    );
    let v: Value = serde_json::from_slice(&bytes)?;
    if let Some(f) = v.get("frozen_sha256") {
        ensure!(f == FROZEN_SHA256, "{} binds another protocol", path.display());
    }
    Ok(v)
}

fn s<'a>(v: &'a Value, pointer: &str) -> Result<&'a str> {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .with_context(|| format!("missing {pointer}"))
}

/// Parse a JSONL file, verifying its sha256 against `expected`.
fn jsonl<T: for<'de> Deserialize<'de>>(
    path: &Path,
    expected: &str,
    mut each: impl FnMut(T) -> Result<()>,
) -> Result<usize> {
    let mut lines = HashedLines::open(path)?;
    let mut n = 0usize;
    while let Some(line) = lines.next_line()? {
        if line.is_empty() {
            continue;
        }
        let row: T = serde_json::from_slice(line)
            .with_context(|| format!("{} row {}", path.display(), n + 1))?;
        each(row)?;
        n += 1;
    }
    ensure!(
        lines.finish() == expected,
        "sha256 mismatch for {}",
        path.display()
    );
    Ok(n)
}

pub fn parse_key(key: &str, merged: &mut HashMap<String, u32>) -> Result<KeyKind> {
    if let Some(rest) = key.strip_prefix("availability/") {
        ensure!(
            matches!(rest, "unresolved_token" | "no_real_reading_token" | "forced_window")
                || rest.starts_with("window/"),
            "unknown availability key"
        );
        return Ok(KeyKind::Avail {
            doc_scope: rest.starts_with("window/") || rest == "forced_window",
        });
    }
    let (pool, rest) = key.split_once('/').context("unpooled key")?;
    let (kind, sig) = rest.split_once('/').context("key without signature")?;
    let pool = POOLS
        .iter()
        .position(|p| *p == pool)
        .context("unknown pool")? as u8;
    let kind = match kind {
        "symbol" => 0u8,
        "production" => 1,
        "leaf" => 2,
        _ => bail!("unknown key kind"),
    };
    ensure!(!sig.is_empty(), "empty signature");
    let next = merged.len() as u32;
    let m = *merged.entry(rest.to_string()).or_insert(next);
    Ok(KeyKind::Pool { pool, kind, merged: m })
}

#[derive(Deserialize)]
struct SelDoc {
    id: String,
    kind: String,
    domain: String,
    #[serde(default)]
    writer_key: Option<String>,
    whitespace_words: u32,
    text_sha256: String,
}

#[derive(Deserialize)]
struct AuthorLine {
    domain: String,
    writer_key: String,
    h: String,
    role: String,
    fold: u32,
    withheld: bool,
    simulated_withheld: bool,
    selected: bool,
    queries: Vec<String>,
    references: Vec<String>,
}

#[derive(Deserialize)]
struct TargetLine {
    stratum: String,
    target: String,
    eligible: bool,
    profile: Vec<String>,
    has_train_split_documents: bool,
}

#[derive(Deserialize)]
struct RecordLine {
    id: String,
    stratum: String,
    kind: String,
    family: String,
    target: String,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    parent_writer: Option<String>,
    source_document_available: bool,
}

#[derive(Deserialize)]
struct CfgSummary {
    execution_failures: u64,
    projection_failures: u64,
    budget_skipped_windows: u64,
}

#[derive(Deserialize)]
struct CfgLine {
    id: String,
    token_opportunities: u64,
    complete_window_tokens: u64,
    projection_available: bool,
    search_exhaustive: bool,
    restricted_qualifies: bool,
    summary: CfgSummary,
    counts: Vec<(u32, f64)>,
}

#[derive(Deserialize)]
struct EncLine {
    id: String,
    status: String,
    text_sha256: String,
    h_raw: Vec<f64>,
    z_shared: Vec<f64>,
}

#[derive(Deserialize)]
struct StyloLine {
    id: String,
    text_sha256: String,
    function_word_counts: Vec<u32>,
    word_tokens: u64,
    char3_counts: Vec<(String, u32)>,
    char3_total: u64,
}

#[derive(Deserialize)]
struct RestrictedLine {
    id: String,
    restricted_text_sha256: String,
    complete_window_tokens: u64,
}

#[derive(Deserialize)]
struct KeysFile {
    schema: String,
    keys: Vec<String>,
}

/// Loads every input of the evaluator from the protocol directory `root`.
pub fn load(root: &Path) -> Result<Inputs> {
    let mut inp = Inputs::default();
    let frozen_path = root.join("FROZEN.json");
    read_frozen(&frozen_path)?;
    inp.bindings
        .insert("FROZEN.json".into(), FROZEN_SHA256.into());
    let feat_dir = root.join("features");
    let features_bytes = fs::read(feat_dir.join("FEATURES.json"))?;
    let features: Value = serde_json::from_slice(&features_bytes)?;
    ensure!(features["frozen_sha256"] == FROZEN_SHA256, "FEATURES.json protocol");
    ensure!(features["metrics_computed"] == false, "FEATURES.json metrics flag");
    inp.bindings
        .insert("features/FEATURES.json".into(), sha_hex(&features_bytes));
    let m = |name: &str| -> Result<Value> {
        let rel = s(&features, &format!("/manifests/{name}/path"))?;
        let sha = s(&features, &format!("/manifests/{name}/sha256"))?;
        manifest(&feat_dir.join(rel), sha)
    };
    let selection = m("selection")?;
    let cfg_m = m("A_cfg")?;
    let restricted_m = m("restricted_texts")?;
    let enc_full_m = m("B_encoder_full")?;
    let enc_restr_m = m("B_encoder_restricted")?;
    let stylo_full_m = m("C_stylometric_full")?;
    let stylo_restr_m = m("C_stylometric_restricted")?;
    for name in [
        "selection",
        "A_cfg",
        "restricted_texts",
        "B_encoder_full",
        "B_encoder_restricted",
        "C_stylometric_full",
        "C_stylometric_restricted",
    ] {
        inp.bindings.insert(
            format!("features/{}", s(&features, &format!("/manifests/{name}/path"))?),
            s(&features, &format!("/manifests/{name}/sha256"))?.to_string(),
        );
    }
    let out_sha = |file: &str| -> Result<String> {
        for o in selection["outputs"].as_array().context("selection outputs")? {
            if o["file"] == file {
                return Ok(o["sha256"].as_str().context("sha")?.to_string());
            }
        }
        bail!("selection output {file} not listed")
    };
    let sel = feat_dir.join("select-v1");

    // Documents (metadata only).
    let mut text_sha: Vec<String> = Vec::new();
    let docs_sha = s(&selection, "/documents/sha256")?.to_string();
    jsonl::<SelDoc>(&sel.join("selection/documents.jsonl"), &docs_sha, |d| {
        ensure!(!inp.index.contains_key(&d.id), "duplicate selected document");
        inp.index.insert(d.id.clone(), inp.docs.len());
        inp.docs.push(DocMeta {
            id: d.id,
            kind: d.kind,
            domain: d.domain,
            writer: d.writer_key,
            words: d.whitespace_words,
        });
        text_sha.push(d.text_sha256);
        Ok(())
    })?;
    let n = inp.docs.len();
    ensure!(n as u64 == selection["documents"]["rows"].as_u64().unwrap_or(0), "document rows");
    inp.bindings.insert("selection/documents.jsonl".into(), docs_sha);

    jsonl::<AuthorLine>(
        &sel.join("selection/t12-authors.jsonl"),
        &out_sha("selection/t12-authors.jsonl")?,
        |a| {
            inp.authors.push(AuthorRow {
                domain: a.domain,
                writer: a.writer_key,
                h: a.h,
                role: a.role,
                fold: a.fold,
                withheld: a.withheld,
                simulated_withheld: a.simulated_withheld,
                selected: a.selected,
                queries: a.queries,
                refs: a.references,
            });
            Ok(())
        },
    )?;
    jsonl::<TargetLine>(
        &sel.join("selection/t3-targets.jsonl"),
        &out_sha("selection/t3-targets.jsonl")?,
        |t| {
            inp.targets.push(TargetRow {
                stratum: t.stratum,
                target: t.target,
                eligible: t.eligible,
                profile: t.profile,
                has_train: t.has_train_split_documents,
            });
            Ok(())
        },
    )?;
    jsonl::<RecordLine>(
        &sel.join("selection/t3-records.jsonl"),
        &out_sha("selection/t3-records.jsonl")?,
        |r| {
            inp.records.push(RecordRow {
                id: r.id,
                stratum: r.stratum,
                kind: r.kind,
                family: r.family,
                target: r.target,
                parent_id: r.parent_id,
                parent_writer: r.parent_writer,
                source_available: r.source_document_available,
            });
            Ok(())
        },
    )?;

    // CFG keys and sparse counts.
    let keys_bytes = fs::read(feat_dir.join("cfg-v1/cfg-keys.json"))?;
    ensure!(
        sha_hex(&keys_bytes) == s(&cfg_m, "/outputs/keys/sha256")?,
        "cfg-keys.json sha256"
    );
    let keys: KeysFile = serde_json::from_slice(&keys_bytes)?;
    ensure!(keys.schema == "slop_ninja_cfg_vs_encoder_cfg_keys_v1", "keys schema");
    let mut merged = HashMap::new();
    for k in &keys.keys {
        inp.keys.push(parse_key(k, &mut merged)?);
    }
    inp.n_merged = merged.len();
    inp.cfg = vec![None; n];
    let mut cfg_qualifies = vec![false; n];
    let nkeys = inp.keys.len() as u32;
    let rows = jsonl::<CfgLine>(
        &feat_dir.join("cfg-v1/cfg-features.jsonl"),
        s(&cfg_m, "/outputs/features/sha256")?,
        |r| {
            let i = *inp.index.get(&r.id).context("cfg row not selected")?;
            ensure!(inp.cfg[i].is_none(), "duplicate cfg row");
            ensure!(
                r.summary.projection_failures == 0 && r.summary.budget_skipped_windows == 0,
                "projection or budget failure present"
            );
            let mut keys = Vec::with_capacity(r.counts.len());
            let mut vals = Vec::with_capacity(r.counts.len());
            let mut last: Option<u32> = None;
            for (k, v) in r.counts {
                ensure!(k < nkeys && last.is_none_or(|l| k > l), "cfg key order");
                ensure!(v.is_finite() && v >= 0.0, "cfg count");
                last = Some(k);
                keys.push(k);
                vals.push(v);
            }
            cfg_qualifies[i] = r.restricted_qualifies;
            ensure!(
                r.restricted_qualifies == (r.complete_window_tokens >= 100),
                "restricted qualification rule"
            );
            inp.cfg[i] = Some(CfgRow {
                tokens: r.token_opportunities as f64,
                complete: r.complete_window_tokens as f64,
                projection_available: r.projection_available,
                search_exhaustive: r.search_exhaustive,
                execution_failures: r.summary.execution_failures,
                keys,
                vals,
            });
            Ok(())
        },
    )?;
    ensure!(rows == n, "cfg rows cover the selection");

    // Coverage-restricted selection (T1/T2 documents with >= 100 complete tokens).
    let restricted_sha = s(&cfg_m, "/outputs/restricted/selection_sha256")?;
    ensure!(
        restricted_sha == s(&restricted_m, "/restricted/selection_sha256")?,
        "restricted selection differs between manifests"
    );
    inp.restricted = vec![false; n];
    let mut restricted_text_sha: Vec<Option<String>> = vec![None; n];
    let rrows = jsonl::<RestrictedLine>(
        &feat_dir.join("restricted-v1/selection.jsonl"),
        restricted_sha,
        |r| {
            let i = inp.doc(&r.id)?;
            ensure!(cfg_qualifies[i], "restricted document does not qualify");
            ensure!(
                inp.cfg[i].as_ref().unwrap().complete == r.complete_window_tokens as f64,
                "restricted complete-token count"
            );
            inp.restricted[i] = true;
            restricted_text_sha[i] = Some(r.restricted_text_sha256);
            Ok(())
        },
    )?;
    ensure!(
        rrows as u64 == restricted_m["restricted"]["rows"].as_u64().context("restricted rows")?,
        "restricted rows"
    );

    // R0 encoder rows.
    for (slot, man, file) in [
        (0usize, &enc_full_m, "enc-full-features-v1/encoder-features.jsonl"),
        (1usize, &enc_restr_m, "enc-restricted-features-v1/encoder-features.jsonl"),
    ] {
        let mut out: Vec<Option<EncRow>> = vec![None; n];
        jsonl::<EncLine>(&feat_dir.join(file), s(man, "/outputs/features/sha256")?, |r| {
            let i = inp.doc(&r.id)?;
            ensure!(r.status == "ok", "encoder status");
            let want = if slot == 0 {
                Some(&text_sha[i])
            } else {
                restricted_text_sha[i].as_ref()
            };
            ensure!(want == Some(&r.text_sha256), "encoder text binding");
            ensure!(
                r.h_raw.len() == ENC_DIM && r.z_shared.len() == ENC_DIM,
                "encoder dimension"
            );
            ensure!(
                r.h_raw.iter().chain(&r.z_shared).all(|x| x.is_finite()),
                "nonfinite encoder value"
            );
            ensure!(out[i].is_none(), "duplicate encoder row");
            out[i] = Some(EncRow {
                h_raw: r.h_raw,
                z_shared: r.z_shared,
            });
            Ok(())
        })?;
        inp.enc[slot] = out;
    }

    // Stylometric counts.
    let mut c3_ids: HashMap<String, u32> = HashMap::new();
    for (slot, man, file) in [
        (0usize, &stylo_full_m, "stylo-full-v1/stylometric-features.jsonl"),
        (1usize, &stylo_restr_m, "stylo-restricted-v1/stylometric-features.jsonl"),
    ] {
        let mut out: Vec<Option<StyloRow>> = vec![None; n];
        let mut names = std::mem::take(&mut inp.char3);
        jsonl::<StyloLine>(&feat_dir.join(file), s(man, "/outputs/features/sha256")?, |r| {
            let i = inp.doc(&r.id)?;
            let want = if slot == 0 {
                Some(&text_sha[i])
            } else {
                restricted_text_sha[i].as_ref()
            };
            ensure!(want == Some(&r.text_sha256), "stylometric text binding");
            ensure!(r.function_word_counts.len() == N_FW, "function word count");
            let mut c3 = Vec::with_capacity(r.char3_counts.len());
            let mut total = 0u64;
            for (g, c) in r.char3_counts {
                let next = names.len() as u32;
                let id = *c3_ids.entry(g.clone()).or_insert_with(|| {
                    names.push(g);
                    next
                });
                total += c as u64;
                c3.push((id, c));
            }
            ensure!(total == r.char3_total, "char3 total");
            c3.sort_unstable();
            ensure!(out[i].is_none(), "duplicate stylometric row");
            out[i] = Some(StyloRow {
                fw: r.function_word_counts,
                words: r.word_tokens,
                c3,
                c3_total: r.char3_total,
            });
            Ok(())
        })?;
        inp.char3 = names;
        inp.stylo[slot] = out;
    }

    // Coverage checks: every selected document has all-text features; every
    // restricted document has restricted features.
    for i in 0..n {
        ensure!(
            inp.enc[0][i].is_some() && inp.stylo[0][i].is_some(),
            "all-text features missing"
        );
        ensure!(
            inp.restricted[i] == inp.enc[1][i].is_some()
                && inp.restricted[i] == inp.stylo[1][i].is_some(),
            "restricted features do not match the restricted selection"
        );
    }
    Ok(inp)
}

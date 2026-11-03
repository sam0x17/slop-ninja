//! Frozen style partitions.
//!
//! `freeze-styles` writes one file that maps every voice prompt and brief id
//! to its partition at a fixed style-library snapshot. The assembler and the
//! generators read partitions only from this file. Partitions move one way:
//!
//! 1. **Snapshot.** Every accepted item of the snapshot library takes its
//!    library partition.
//! 2. **Explicit admissions.** Voice files listed as `explicit_train` (the
//!    hand-written pilot seed) are admitted to `train`.
//! 3. **History.** Items of earlier snapshots that the final snapshot no
//!    longer lists keep their earlier partition. An item that was in an
//!    evaluation partition in any earlier snapshot, or in the record of a
//!    generated record (`partition_at_generation`), never returns to `train`:
//!    it takes the first evaluation partition observed.
//! 4. **Text.** Items whose normalised text (lowercase, letters and digits,
//!    single spaces) equals that of an evaluation item join that item's
//!    partition.
//! 5. **Clusters** (`close_clusters`). A snapshot cluster that holds an
//!    evaluation item moves to that item's partition whole.
//!
//! Rules 4 and 5 repeat until nothing moves. An id that is not in the file is
//! `unknown` and never enters Train.
use crate::consolidate::for_lines;
use crate::sha_hex;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const CONFIG_SCHEMA: &str = "slop_ninja_a_style_freeze_config_v1";
pub const FROZEN_SCHEMA: &str = "slop_ninja_a_frozen_style_partition_v1";
pub const TRAIN: &str = "train";

/// Lowercase letters and digits with single spaces (curly quotes folded).
pub fn normalize_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = true;
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
            space = false;
        } else if !space && !matches!(c, '\'' | '\u{2019}') {
            out.push(' ');
            space = true;
        }
    }
    out.trim_end().to_string()
}

pub fn text_key(s: &str) -> String {
    sha_hex(normalize_text(s).as_bytes())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryInput {
    pub label: String,
    /// A compiled library directory (`voices.accepted.jsonl`, `briefs.accepted.jsonl`).
    #[serde(default)]
    pub library: Option<PathBuf>,
    /// A collector voice-row file (`id`, `kind`, `text`, `partition` or `partition_at_generation`).
    #[serde(default)]
    pub voices: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema: String,
    /// The fixed library snapshot (a copy that nothing writes to).
    pub library: PathBuf,
    /// Earlier snapshots and generation records, oldest first.
    #[serde(default)]
    pub history: Vec<HistoryInput>,
    /// Voice files admitted to Train explicitly (label, voices).
    #[serde(default)]
    pub explicit_train: Vec<HistoryInput>,
    #[serde(default)]
    pub close_clusters: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Frozen {
    pub schema: String,
    pub id: String,
    pub kind: String,
    pub partition: String,
    pub text_key: String,
    pub cluster: Option<String>,
    pub origin: Option<String>,
    pub axis: Option<String>,
    /// Why the partition is what it is: `snapshot`, `explicit_train:<label>`,
    /// `earlier_snapshot:<label>`, `was_evaluation_in:<label>`, `text_match:<id>`, `cluster:<cluster>`.
    pub basis: String,
    pub history: BTreeMap<String, String>,
}

fn kind_of(v: &Value) -> String {
    match v["type"].as_str().or(v["kind"].as_str()).unwrap_or("") {
        "voice" | "voice_prompt" => "voice_prompt".into(),
        "brief" => "brief".into(),
        k => k.into(),
    }
}

fn library_rows(dir: &Path) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for f in ["voices.accepted.jsonl", "briefs.accepted.jsonl"] {
        for_lines(&dir.join(f), |l| {
            rows.push(serde_json::from_slice(l)?);
            Ok(())
        })?;
    }
    Ok(rows)
}

fn input_rows(h: &HistoryInput) -> Result<Vec<Value>> {
    match (&h.library, &h.voices) {
        (Some(d), None) => library_rows(d),
        (None, Some(f)) => {
            let mut rows = Vec::new();
            for_lines(f, |l| {
                rows.push(serde_json::from_slice(l)?);
                Ok(())
            })?;
            Ok(rows)
        }
        _ => anyhow::bail!("history input {} needs exactly one of library or voices", h.label),
    }
}

fn partition_of(v: &Value) -> Option<String> {
    v["partition"].as_str().or(v["partition_at_generation"].as_str()).map(String::from)
}

fn digest_of(h: &HistoryInput) -> Result<Value> {
    Ok(match (&h.library, &h.voices) {
        (Some(d), _) => json!({"label": h.label, "library": d,
            "voices_sha256": crate::bundle::file_sha(&d.join("voices.accepted.jsonl"))?,
            "briefs_sha256": crate::bundle::file_sha(&d.join("briefs.accepted.jsonl"))?}),
        (_, Some(f)) => json!({"label": h.label, "voices": f, "sha256": crate::bundle::file_sha(f)?}),
        _ => Value::Null,
    })
}

/// Apply the freeze rules; returns rows sorted by id and the move counts.
pub fn freeze(snapshot: &[Value], history: &[(String, Vec<Value>)], explicit: &[(String, Vec<Value>)], close_clusters: bool)
    -> Result<(Vec<Frozen>, BTreeMap<String, usize>)> {
    let mut items: BTreeMap<String, Frozen> = BTreeMap::new();
    for v in snapshot {
        let id = v["id"].as_str().context("library id")?.to_string();
        let f = Frozen {
            schema: FROZEN_SCHEMA.into(), id: id.clone(), kind: kind_of(v),
            partition: partition_of(v).context("library partition")?,
            text_key: text_key(v["text"].as_str().context("library text")?),
            cluster: v["cluster"].as_str().map(String::from), origin: v["origin"].as_str().map(String::from),
            axis: v["axis"].as_str().map(String::from), basis: "snapshot".into(), history: BTreeMap::new(),
        };
        ensure!(items.insert(id.clone(), f).is_none(), "library id {id} twice");
    }
    for (label, rows) in explicit {
        for v in rows {
            let id = v["id"].as_str().context("voice id")?.to_string();
            ensure!(!items.contains_key(&id), "explicitly admitted voice {id} is also a library item");
            items.insert(id.clone(), Frozen {
                schema: FROZEN_SCHEMA.into(), id, kind: kind_of(v), partition: TRAIN.into(),
                text_key: text_key(v["text"].as_str().context("voice text")?), cluster: None,
                origin: v["origin"].as_str().map(String::from),
                axis: v["axes"]["persona_axis"].as_str().or(v["axis"].as_str()).map(String::from),
                basis: format!("explicit_train:{label}"), history: BTreeMap::new(),
            });
        }
    }
    let mut moves: BTreeMap<String, usize> = BTreeMap::new();
    for (label, rows) in history {
        for v in rows {
            let id = v["id"].as_str().context("history id")?.to_string();
            let Some(p) = partition_of(v) else { continue };
            match items.get_mut(&id) {
                Some(f) => {
                    f.history.insert(label.clone(), p.clone());
                    if f.partition == TRAIN && p != TRAIN {
                        f.partition = p;
                        f.basis = format!("was_evaluation_in:{label}");
                        *moves.entry("was_evaluation".into()).or_default() += 1;
                    }
                }
                None => {
                    let text = v["text"].as_str().context("history text")?;
                    let mut h = BTreeMap::new();
                    h.insert(label.clone(), p.clone());
                    items.insert(id.clone(), Frozen {
                        schema: FROZEN_SCHEMA.into(), id, kind: kind_of(v), partition: p,
                        text_key: text_key(text), cluster: None, origin: v["origin"].as_str().map(String::from),
                        axis: v["axis"].as_str().map(String::from), basis: format!("earlier_snapshot:{label}"), history: h,
                    });
                    *moves.entry("earlier_snapshot_only".into()).or_default() += 1;
                }
            }
        }
    }
    // Rules 4 and 5 to a fixed point: an evaluation partition spreads over equal texts and clusters.
    loop {
        let mut by_text: HashMap<&str, (&str, &str)> = HashMap::new();
        let mut by_cluster: HashMap<&str, (&str, &str)> = HashMap::new();
        for f in items.values().filter(|f| f.partition != TRAIN) {
            let e = by_text.entry(&f.text_key).or_insert((&f.partition, &f.id));
            if (f.partition.as_str(), f.id.as_str()) < *e {
                *e = (&f.partition, &f.id);
            }
            if let Some(c) = &f.cluster {
                let e = by_cluster.entry(c).or_insert((&f.partition, &f.id));
                if (f.partition.as_str(), f.id.as_str()) < *e {
                    *e = (&f.partition, &f.id);
                }
            }
        }
        let mut set: Vec<(String, String, String, &'static str)> = Vec::new();
        for f in items.values().filter(|f| f.partition == TRAIN) {
            if let Some((p, id)) = by_text.get(f.text_key.as_str()) {
                set.push((f.id.clone(), p.to_string(), format!("text_match:{id}"), "text_match"));
            } else if close_clusters {
                if let Some((p, _)) = f.cluster.as_deref().and_then(|c| by_cluster.get(c)) {
                    set.push((f.id.clone(), p.to_string(), format!("cluster:{}", f.cluster.as_deref().unwrap()), "cluster"));
                }
            }
        }
        if set.is_empty() {
            break;
        }
        for (id, p, basis, kind) in set {
            let f = items.get_mut(&id).unwrap();
            f.partition = p;
            f.basis = basis;
            *moves.entry(kind.into()).or_default() += 1;
        }
    }
    Ok((items.into_values().collect(), moves))
}

/// `freeze-styles --config C --out FILE`: writes FILE (JSONL) and FILE.json (the record).
pub fn freeze_styles(cfg_path: &Path, out: &Path, code: &Value) -> Result<Value> {
    let cfg_bytes = fs::read(cfg_path)?;
    let cfg: Config = serde_json::from_slice(&cfg_bytes).context("freeze config")?;
    ensure!(cfg.schema == CONFIG_SCHEMA, "config schema must be {CONFIG_SCHEMA}");
    ensure!(!out.exists(), "{} exists; a frozen partition file is written once", out.display());
    let snap = library_rows(&cfg.library)?;
    let mut hist = Vec::new();
    let mut inputs = Vec::new();
    for h in &cfg.history {
        hist.push((h.label.clone(), input_rows(h)?));
        inputs.push(digest_of(h)?);
    }
    let mut expl = Vec::new();
    for h in &cfg.explicit_train {
        expl.push((h.label.clone(), input_rows(h)?));
        inputs.push(digest_of(h)?);
    }
    let (rows, moves) = freeze(&snap, &hist, &expl, cfg.close_clusters)?;
    let mut b = Vec::new();
    let mut by_part: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut by_basis: BTreeMap<String, usize> = BTreeMap::new();
    for r in &rows {
        serde_json::to_writer(&mut b, r)?;
        b.push(b'\n');
        *by_part.entry(r.kind.clone()).or_default().entry(r.partition.clone()).or_default() += 1;
        *by_basis.entry(r.basis.split(':').next().unwrap().to_string()).or_default() += 1;
    }
    let sha = sha_hex(&b);
    fs::write(out, &b)?;
    let lib_sum = cfg.library.join("summary.json");
    let rec = json!({
        "schema": "slop_ninja_a_frozen_style_partitions_v1", "file": out, "sha256": sha, "rows": rows.len(),
        "snapshot": {"library": cfg.library,
            "voices_sha256": crate::bundle::file_sha(&cfg.library.join("voices.accepted.jsonl"))?,
            "briefs_sha256": crate::bundle::file_sha(&cfg.library.join("briefs.accepted.jsonl"))?,
            "summary_sha256": if lib_sum.is_file() { json!(crate::bundle::file_sha(&lib_sum)?) } else { Value::Null }},
        "inputs": inputs, "config": cfg, "config_sha256": sha_hex(&cfg_bytes),
        "rules": "snapshot partition; explicit Train admissions; earlier snapshots and generation records keep their partition and an evaluation partition never returns to train; equal normalised texts and (close_clusters) snapshot clusters join an evaluation item's partition, to a fixed point; ids not in the file are unknown and never enter Train",
        "rows_by_kind_partition": by_part, "rows_by_basis": by_basis, "moved_from_train": moves, "code": code,
    });
    let mut rb = serde_json::to_vec_pretty(&rec)?;
    rb.push(b'\n');
    fs::write(out.with_extension("json"), &rb)?;
    Ok(json!({"file": out, "sha256": sha, "rows": rows.len(), "rows_by_kind_partition": by_part, "moved_from_train": moves}))
}

/// The frozen partitions as read by the assembler.
#[derive(Default)]
pub struct FrozenIndex {
    pub items: HashMap<String, Frozen>,
    /// text key -> (partition, id) of evaluation items.
    pub eval_texts: HashMap<String, (String, String)>,
    pub sha256: String,
}

impl FrozenIndex {
    pub fn from_rows(rows: Vec<Frozen>, sha256: String) -> Result<Self> {
        let mut ix = FrozenIndex { sha256, ..Default::default() };
        for f in rows {
            ensure!(f.schema == FROZEN_SCHEMA, "frozen partition schema");
            if f.partition != TRAIN {
                ix.eval_texts.entry(f.text_key.clone()).or_insert((f.partition.clone(), f.id.clone()));
            }
            ensure!(ix.items.insert(f.id.clone(), f).is_none(), "frozen id twice");
        }
        Ok(ix)
    }
    pub fn read(p: &Path) -> Result<Self> {
        let mut rows = Vec::new();
        let (sha, _) = for_lines(p, |l| {
            rows.push(serde_json::from_slice(l)?);
            Ok(())
        })?;
        Self::from_rows(rows, sha)
    }
    /// The evaluation partition a style text belongs to, by normalised text.
    pub fn text_partition(&self, text: &str) -> Option<&(String, String)> {
        self.eval_texts.get(&text_key(text))
    }
    pub fn ids(&self) -> HashSet<&str> {
        self.items.keys().map(String::as_str).collect()
    }
}

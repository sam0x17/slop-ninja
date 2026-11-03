//! Trainer bundle: one directory that the full-A trainer reads.
//!
//! The trainer reads a single `rows.jsonl` holding every split it uses (Train,
//! Dev-iter and an optional report split), with exactly the trainer's row
//! fields, a `weights.json` in its schema and a `MANIFEST.json` whose digest
//! its contract binds. `trainer_bundle` projects the split directories of one
//! build into that layout. Rows keep their exact text, spans, labels and
//! provenance digest; only the field set and three encodings change:
//!
//! - `voice_id` is the voice prompt id, or the brief id when the request had
//!   no voice prompt (so a brief's cluster always has a style id);
//! - `style_axes` is the sorted list of persona-axis names of the request
//!   (`axis`, `library_axis`, `persona_axis` values), or null; request
//!   attributes such as the voice inventor, ladder or style name are not axes;
//! - `operation` is null for human rows.
//!
//! Optional selection, recorded in the manifest: `take` caps the rows of a
//! split and source kind (lowest SHA-256 of seed and id first), and the smoke
//! carve moves whole Train source groups (by SHA-256 of seed and group) into
//! Dev-iter, where they are capped as `carved_<kind>`, apart from Dev-iter's
//! own rows. A carved bundle is marked `smoke_only` and is never a protocol
//! Dev-iter.
//!
//! Round 2: when the build's Train `weights.json` carries a `round2` report
//! (protocol A2 §6.3–§6.4), the trainer `weights.json` repeats it as
//! `round2_build` and adds `round2_trainer_train` with the Kish ESS of the
//! projected Train rows under the trainer's weighting (`--weighting final`;
//! amendment 5 item 5.1), reported, not refused: the build refuses, and a
//! `take` selection is a diagnostic subset. When the build's rows carry block
//! tags, the manifest counts rows per split and block. Block tags are not
//! trainer fields. A build without either keeps the exact trainer-bundle bytes.
use crate::bundle::file_sha;
use crate::{sha_hex, split_name};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

pub const TRAINER_MANIFEST_SCHEMA: &str = "slop_ninja_a_trainer_bundle_manifest_v1";
pub const TRAINER_WEIGHTS_SCHEMA: &str = "slop_ninja_a_trainer_weights_v1";
/// The trainer's row fields (`full_a_bundle.ROW_FIELDS`, inline-text form).
pub const TRAINER_FIELDS: [&str; 20] = [
    "id", "split", "text", "text_sha256", "origin", "share", "ai_byte_spans", "generator_label",
    "cohort_id", "cohort_weight", "source_group", "domain", "genre", "voice_id", "voice_cluster_id",
    "voice_origin", "style_axes", "operation", "held_out_style", "provenance_sha256",
];
const AXIS_KEYS: [&str; 3] = ["axis", "library_axis", "persona_axis"];
pub const SOURCE_KINDS: [&str; 6] = ["human", "local_positive", "frontier", "hosted_capture", "stage3_legacy", "external_pool"];
/// Protocol uses that may open a sealed split, and the split each opens.
pub const UNSEAL_PURPOSES: [(&str, &str); 2] = [("final_selection", "dev_select"), ("calibration_fit", "cal")];

/// How the trainer bundle carries Train weights.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrainerWeighting {
    /// `cohort_weight` is the build's final row weight; the trainer uses it as is
    /// (`base_weighting: assembler_row_weight_v1`).
    #[default]
    Final,
    /// `cohort_weight` is the declared cohort factor, constant per cohort and label; the
    /// trainer recomputes family/origin balance (`base_weighting: family_origin_balanced`).
    CohortFactor,
}

/// Selection applied while projecting.
#[derive(Debug, Default)]
pub struct Selection {
    pub seed: String,
    /// (split, source kind) -> maximum rows.
    pub take: BTreeMap<(String, String), usize>,
    /// Move Train source groups with hash mod m < k into Dev-iter.
    pub smoke_carve: Option<(u64, u64)>,
    /// Protocol purposes that unseal Dev-select or CAL (one use each, recorded).
    pub unseal_for: Vec<String>,
    pub weighting: TrainerWeighting,
}

impl Selection {
    /// `SPLIT:KIND=N`.
    pub fn parse_take(&mut self, s: &str) -> Result<()> {
        let (k, n) = s.split_once('=').context("take is SPLIT:KIND=N")?;
        let (split, kind) = k.split_once(':').context("take is SPLIT:KIND=N")?;
        let split = split_name(split);
        ensure!(crate::SPLITS.contains(&split.as_str()), "unknown split {split}");
        ensure!(SOURCE_KINDS.contains(&kind.strip_prefix("carved_").unwrap_or(kind)), "unknown source kind {kind}");
        ensure!(self.take.insert((split, kind.into()), n.parse()?).is_none(), "take {k} twice");
        Ok(())
    }
    /// `K/M` with 0 < K < M.
    pub fn parse_carve(&mut self, s: &str) -> Result<()> {
        let (k, m) = s.split_once('/').context("carve is K/M")?;
        let (k, m): (u64, u64) = (k.parse()?, m.parse()?);
        ensure!(0 < k && k < m, "carve needs 0 < K < M");
        self.smoke_carve = Some((k, m));
        Ok(())
    }
}

fn hash_u64(seed: &str, key: &str) -> u64 {
    let h = sha_hex(format!("{seed}\x1f{key}").as_bytes());
    u64::from_str_radix(&h[..16], 16).unwrap()
}

fn read_json(p: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(p).with_context(|| format!("read {}", p.display()))?)
        .with_context(|| format!("parse {}", p.display()))
}

fn for_rows(p: &Path, mut f: impl FnMut(Value) -> Result<()>) -> Result<()> {
    let rd = BufReader::with_capacity(1 << 20, fs::File::open(p).with_context(|| format!("open {}", p.display()))?);
    for (n, line) in rd.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        f(serde_json::from_str(&line).with_context(|| format!("{} line {}", p.display(), n + 1))?)
            .with_context(|| format!("{} line {}", p.display(), n + 1))?;
    }
    Ok(())
}

/// Persona-axis names of a build row's `style_axes` object.
pub fn axis_names(v: &Value) -> Value {
    let mut out = BTreeSet::new();
    if let Some(m) = v.as_object() {
        for k in AXIS_KEYS {
            if let Some(s) = m.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                out.insert(s.to_string());
            }
        }
    }
    if out.is_empty() { Value::Null } else { json!(out) }
}

/// One build row in the trainer's field set, at split `split`.
pub fn project(r: &Value, split: &str, carved: bool, weighting: TrainerWeighting) -> Result<Value> {
    let id = r["id"].as_str().context("id")?;
    let text = r["text"].as_str().context("text")?;
    ensure!(r["text_sha256"].as_str() == Some(sha_hex(text.as_bytes()).as_str()), "row {id}: text digest");
    let origin = r["origin"].as_str().context("origin")?;
    let share = r["share"].as_f64().context("share")?;
    match origin {
        "human_only" => ensure!(share == 0.0 && r["generator_label"].is_null(), "row {id}: human labels"),
        "model_only" => ensure!(share == 1.0 && r["generator_label"].is_string(), "row {id}: model-only labels"),
        "mixed" => ensure!(share > 0.0 && share <= 1.0 && r["generator_label"].is_string(), "row {id}: mixed labels"),
        o => bail!("row {id}: origin {o}"),
    }
    let held = r["held_out_style"].as_bool().context("held_out_style")?;
    ensure!(!(split == "train" && held), "row {id}: held-out style in Train");
    let voice = r["voice_id"].as_str().or(r["brief_id"].as_str());
    let cluster = if voice.is_some() { r["voice_cluster_id"].clone() } else { Value::Null };
    let mut o = Map::new();
    o.insert("id".into(), json!(id));
    o.insert("split".into(), json!(split));
    o.insert("text".into(), json!(text));
    for k in ["text_sha256", "origin", "share", "ai_byte_spans", "generator_label", "cohort_id", "source_group",
        "domain", "genre", "voice_origin", "held_out_style", "provenance_sha256"] {
        o.insert(k.into(), r[k].clone());
    }
    // Evaluation splits carry the evaluation factor 1 (cohort weights apply to Train only).
    // In the final-weight contract a Train row carries its final weight.
    let w = if carved || split != "train" {
        json!(1.0)
    } else if weighting == TrainerWeighting::Final {
        r["weight"].clone()
    } else {
        r["cohort_weight"].clone()
    };
    o.insert("cohort_weight".into(), w);
    o.insert("voice_id".into(), json!(voice));
    o.insert("voice_cluster_id".into(), cluster);
    o.insert("style_axes".into(), axis_names(&r["style_axes"]));
    o.insert("operation".into(), if origin == "human_only" { Value::Null } else { r["operation"].clone() });
    debug_assert_eq!(o.len(), TRAINER_FIELDS.len());
    Ok(Value::Object(o))
}

/// Project `splits` of build directory `build` into the trainer bundle `out` (new).
pub fn trainer_bundle(build: &Path, splits: &[String], sel: &Selection, out: &Path, code: &Value) -> Result<Value> {
    ensure!(!out.exists(), "{} exists; a trainer bundle is written once", out.display());
    let b = read_json(&build.join("BUILD.json"))?;
    ensure!(b["schema"] == "slop_ninja_a_bundle_build_v1", "not an assembler build");
    let splits: Vec<String> = splits.iter().map(|s| split_name(s)).collect();
    let wanted: BTreeSet<&str> = splits.iter().map(String::as_str).collect();
    ensure!(wanted.len() == splits.len() && wanted.contains("train"), "splits must be distinct and include train");
    for s in &wanted {
        ensure!(crate::SPLITS.contains(s), "unknown split {s} (Test is never assembled)");
        ensure!(b["splits"][*s].is_object(), "build has no {s} split");
    }
    // Sealed splits open only for their protocol use, once, with a recorded purpose.
    let mut unseal: Vec<(String, String)> = Vec::new();
    for p in &sel.unseal_for {
        let Some((_, s)) = UNSEAL_PURPOSES.iter().find(|(q, _)| q == p) else {
            bail!("unknown --unseal-for purpose {p} (final_selection opens dev_select, calibration_fit opens cal)")
        };
        ensure!(wanted.contains(s), "--unseal-for {p} needs --split {s}");
        let rec = build.join(s.replace('_', "-")).join(format!("UNSEALED-{p}.json"));
        ensure!(!rec.exists(), "{s} was already unsealed for {p} ({}); it opens once", rec.display());
        unseal.push((p.clone(), s.to_string()));
    }
    for s in &wanted {
        let sealed = b["splits"][*s]["sealed"] == true || build.join(s.replace('_', "-")).join("SEALED.json").exists();
        if sealed {
            ensure!(unseal.iter().any(|(_, u)| u == s),
                "{s} is sealed: it opens only for its protocol use, with --unseal-for {}",
                UNSEAL_PURPOSES.iter().find(|(_, x)| x == s).map_or("?", |x| x.0));
        }
    }
    ensure!(sel.smoke_carve.is_none() || unseal.is_empty(), "a smoke bundle never opens a sealed split");
    if sel.smoke_carve.is_some() {
        ensure!(wanted.contains("dev_iter"), "the smoke carve needs the dev_iter split");
    }
    for (s, _) in sel.take.keys() {
        ensure!(wanted.contains(s.as_str()), "take names unprojected split {s}");
    }
    let dir = |s: &str| build.join(s.replace('_', "-"));
    // Pass 1: target split per row, then the take caps.
    let mut source = Map::new();
    let mut eval_groups: BTreeSet<String> = BTreeSet::new();
    let mut cands: BTreeMap<(String, String), Vec<(u64, String)>> = BTreeMap::new();
    let mut target: HashMap<String, (String, bool)> = HashMap::new();
    let mut carved_groups = BTreeSet::new();
    for s in &splits {
        let d = dir(s);
        let m = read_json(&d.join("MANIFEST.json"))?;
        let rows_sha = file_sha(&d.join("rows.jsonl"))?;
        ensure!(m["files"]["rows.jsonl"] == rows_sha.as_str(), "{s} rows differ from their manifest");
        source.insert(s.clone(), json!({"rows_sha256": rows_sha, "weights_sha256": file_sha(&d.join("weights.json"))?,
            "manifest_sha256": file_sha(&d.join("MANIFEST.json"))?, "sealed": m["sealed"]}));
        for_rows(&d.join("rows.jsonl"), |r| {
            ensure!(r["split"] == s.as_str(), "row in the wrong split directory");
            let id = r["id"].as_str().context("id")?.to_string();
            let group = r["source_group"].as_str().context("source_group")?;
            let kind = r["source_kind"].as_str().context("source_kind")?.to_string();
            let mut t = s.clone();
            let mut carved = false;
            if s == "train" {
                if let Some((k, m)) = sel.smoke_carve {
                    if hash_u64(&sel.seed, group) % m < k {
                        t = "dev_iter".into();
                        carved = true;
                        carved_groups.insert(group.to_string());
                    }
                }
            } else {
                eval_groups.insert(group.to_string());
            }
            // Carved rows are capped separately (`carved_<kind>`) from the split's own rows.
            let kind = if carved { format!("carved_{kind}") } else { kind };
            cands.entry((t.clone(), kind)).or_default().push((hash_u64(&sel.seed, &id), id.clone()));
            ensure!(target.insert(id.clone(), (t, carved)).is_none(), "id {id} in two splits");
            Ok(())
        })?;
    }
    ensure!(carved_groups.is_disjoint(&eval_groups), "a carved Train group is an evaluation group");
    let mut keep: HashSet<String> = HashSet::new();
    let mut taken = Map::new();
    for ((s, kind), mut v) in cands {
        let before = v.len();
        if let Some(&n) = sel.take.get(&(s.clone(), kind.clone())) {
            v.sort();
            v.truncate(n);
        }
        taken.insert(format!("{s}:{kind}"), json!({"available": before, "kept": v.len()}));
        keep.extend(v.into_iter().map(|(_, id)| id));
    }
    // Pass 2: write the selected rows, in split order, sorted by id within a split.
    let mut by_split: BTreeMap<String, Vec<(String, Vec<u8>)>> = BTreeMap::new();
    // (cohort, label) -> (records, first weight, weight sum, min, max)
    let mut weights: BTreeMap<(String, String), (usize, f64, f64, f64, f64)> = BTreeMap::new();
    let mut counts: BTreeMap<(String, String, String, String), usize> = BTreeMap::new();
    let mut by_block: BTreeMap<(String, String), usize> = BTreeMap::new();
    // Projected Train rows: (origin index, cohort id, weight), for the round-2 ESS.
    let mut train_w: Vec<(usize, String, f64)> = Vec::new();
    for s in &splits {
        for_rows(&dir(s).join("rows.jsonl"), |r| {
            let id = r["id"].as_str().unwrap().to_string();
            if !keep.contains(&id) {
                return Ok(());
            }
            let (t, carved) = &target[&id];
            let p = project(&r, t, *carved, sel.weighting)?;
            let label = p["generator_label"].as_str().unwrap_or("human").to_string();
            if let Some(b) = r["block"].as_str() {
                *by_block.entry((t.clone(), b.to_string())).or_default() += 1;
            }
            if t == "train" {
                let w = p["cohort_weight"].as_f64().context("cohort_weight")?;
                let o = crate::ORIGINS.iter().position(|x| p["origin"] == *x).context("origin")?;
                train_w.push((o, p["cohort_id"].as_str().unwrap_or("").to_string(), w));
                let e = weights.entry((p["cohort_id"].as_str().unwrap().into(), label.clone())).or_insert((0, w, 0.0, w, w));
                if sel.weighting == TrainerWeighting::CohortFactor {
                    ensure!(e.1 == w, "row {id}: cohort weight differs within its cohort and label");
                }
                e.0 += 1;
                e.2 += w;
                e.3 = e.3.min(w);
                e.4 = e.4.max(w);
            }
            *counts.entry((t.clone(), p["domain"].as_str().unwrap_or("").into(), p["origin"].as_str().unwrap().into(), label)).or_default() += 1;
            by_split.entry(t.clone()).or_default().push((id, serde_json::to_vec(&p)?));
            Ok(())
        })?;
    }
    let tw = read_json(&dir("train").join("weights.json"))?;
    let mut cohorts = Map::new();
    for ((c, l), (n, w, sum, lo, hi)) in &weights {
        let decl = &tw["cohorts"][c];
        ensure!(decl.is_object(), "cohort {c} absent from the build weights");
        let e = cohorts.entry(c.clone()).or_insert_with(|| json!({"policy": decl["policy"], "default": decl["default"],
            "cohort_weight": decl["cohort_weight"], "labels": {}}));
        e["labels"][l] = match sel.weighting {
            TrainerWeighting::CohortFactor => json!({"records": n, "weight": w}),
            TrainerWeighting::Final => json!({"records": n, "weight_sum": sum, "weight_min": lo, "weight_max": hi}),
        };
    }
    let tmp = out.with_extension("tmp-trainer");
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(&tmp)?;
    let mut rows = Vec::new();
    let mut rows_by_split = Map::new();
    for s in ["train", "train_mining", "dev_iter", "dev_select", "cal"] {
        if let Some(mut v) = by_split.remove(s) {
            v.sort();
            rows_by_split.insert(s.into(), json!(v.len()));
            for (_, b) in v {
                rows.extend(b);
                rows.push(b'\n');
            }
        }
    }
    let write = |name: &str, b: &[u8]| -> Result<String> {
        let mut f = fs::File::create(tmp.join(name))?;
        f.write_all(b)?;
        f.sync_all()?;
        Ok(sha_hex(b))
    };
    let pretty = |v: &Value| {
        let mut b = serde_json::to_vec_pretty(v).unwrap();
        b.push(b'\n');
        b
    };
    let rows_sha = write("rows.jsonl", &rows)?;
    let total: f64 = weights.values().map(|x| x.2).sum();
    let wdoc = match sel.weighting {
        TrainerWeighting::CohortFactor => json!({"schema": TRAINER_WEIGHTS_SCHEMA, "base_weighting": "family_origin_balanced",
            "rule": "the trainer multiplies its family/origin-balanced Train weights by each row's cohort_weight; counts are Train rows of this bundle; weights are the build's declared factors",
            "source_weights_sha256": source["train"]["weights_sha256"], "cohorts": cohorts}),
        TrainerWeighting::Final => json!({"schema": TRAINER_WEIGHTS_SCHEMA, "base_weighting": "assembler_row_weight_v1",
            "rule": "each Train row's cohort_weight is its final assembler weight (domain_origin_share_v1 with frontier label balance; see the build weights.json); the trainer uses it as the row's loss weight without recomputing family/origin balance; evaluation rows carry 1 and keep the protocol's balanced weighting",
            "train_weight_sum": total, "build_weights": tw.get("effective_mass").cloned().unwrap_or(Value::Null),
            "source_weights_sha256": source["train"]["weights_sha256"], "cohorts": cohorts}),
    };
    let mut wdoc = wdoc;
    if let Some(r2) = tw.get("round2") {
        wdoc["round2_build"] = r2.clone();
        let rows: Vec<(usize, &str, f64)> = train_w.iter().map(|(o, c, w)| (*o, c.as_str(), *w)).collect();
        wdoc["round2_trainer_train"] = json!({
            "rows_positive_weight": rows.iter().filter(|x| x.2 > 0.0).count(),
            "kish_ess": if sel.weighting == TrainerWeighting::Final {
                crate::covariate::ess_value(&crate::covariate::trainer_weighting(&rows)?)
            } else { Value::Null },
            "rule": "Kish ESS of the projected Train rows under the trainer's weighting (--weighting final only; reported, not refused)"});
    }
    let weights_sha = write("weights.json", &pretty(&wdoc))?;
    let counts: Vec<Value> = counts.into_iter().map(|((s, d, o, g), n)| json!({"split": s, "domain": d, "origin": o,
        "generator_label": if g == "human" { Value::Null } else { json!(g) }, "rows": n})).collect();
    let sealed: Vec<&str> = wanted.iter().copied().filter(|s| *s == "dev_select" || *s == "cal").collect();
    let manifest = json!({"schema": TRAINER_MANIFEST_SCHEMA, "build_id": b["build_id"], "build_dir": build,
        "row_fields": TRAINER_FIELDS, "files": {"rows.jsonl": rows_sha, "weights.json": weights_sha},
        "source_splits": source, "rows_by_split": rows_by_split,
        "counts_split_domain_origin_generator": counts,
        "selection": {"seed": sel.seed, "take": taken, "smoke_carve": sel.smoke_carve.map(|(k, m)| format!("{k}/{m}")),
            "carved_train_groups": carved_groups.len()},
        "smoke_only": sel.smoke_carve.is_some(),
        "sealed_splits_included": sealed, "unsealed_for": unseal.iter().map(|(p, s)| json!({"purpose": p, "split": s})).collect::<Vec<_>>(),
        "weighting": match sel.weighting { TrainerWeighting::Final => "assembler_row_weight_v1", TrainerWeighting::CohortFactor => "family_origin_balanced" },
        "code": code, "detector_outputs_used": false, "test_opened": false});
    let mut manifest = manifest;
    if !by_block.is_empty() {
        manifest["rows_by_split_block"] = json!(by_block.iter().map(|((s, b), n)| json!({"split": s, "block": b, "rows": n})).collect::<Vec<_>>());
    }
    let manifest_sha = write("MANIFEST.json", &pretty(&manifest))?;
    if !sealed.is_empty() {
        for f in ["rows.jsonl", "weights.json", "MANIFEST.json"] {
            crate::bundle::set_read_only(&tmp.join(f))?;
        }
    }
    // One recorded use per sealed split and purpose (create_new refuses a second).
    for (p, s) in &unseal {
        let rec = json!({"schema": "slop_ninja_a_unseal_record_v1", "purpose": p, "split": s, "trainer_bundle": out,
            "manifest_sha256": manifest_sha, "build_id": b["build_id"], "code": code,
            "rule": "a sealed split opens once for its protocol use; a mechanical rerun of the identical frozen evaluation needs this record removed by the operator with the reason recorded"});
        let path = build.join(s.replace('_', "-")).join(format!("UNSEALED-{p}.json"));
        let mut f = fs::File::create_new(&path).with_context(|| format!("{s} was already unsealed for {p} ({})", path.display()))?;
        f.write_all(&pretty(&rec))?;
        f.sync_all()?;
    }
    fs::rename(&tmp, out)?;
    Ok(json!({"out": out, "rows_by_split": manifest["rows_by_split"], "manifest_sha256": manifest_sha,
        "smoke_only": manifest["smoke_only"]}))
}

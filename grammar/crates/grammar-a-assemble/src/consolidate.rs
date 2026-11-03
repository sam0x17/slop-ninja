//! Data-layout consolidation: merging evaluation-human directories, checking
//! that a successor keeps every line of the version it replaces, the pools
//! index, and the frontier v1-to-v2 record migration check.
use crate::sha_hex;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// Stream a file's lines (without the newline) with its SHA-256.
pub fn for_lines(p: &Path, mut f: impl FnMut(&[u8]) -> Result<()>) -> Result<(String, usize)> {
    let mut rd = BufReader::with_capacity(1 << 20, fs::File::open(p).with_context(|| format!("open {}", p.display()))?);
    let mut h = Sha256::new();
    let (mut line, mut n) = (Vec::new(), 0usize);
    loop {
        line.clear();
        if rd.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        h.update(&line);
        let body = line.strip_suffix(b"\n").unwrap_or(&line);
        if !body.is_empty() {
            n += 1;
            f(body)?;
        }
    }
    Ok((hex::encode(h.finalize()), n))
}

fn digest(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}

/// Every non-empty line of `base` occurs in `successor` byte for byte (as a
/// multiset). Returns counts; `missing` lists the first few missing line digests.
pub fn verify_lines(base: &Path, successor: &Path) -> Result<Value> {
    let mut want: HashMap<[u8; 32], u32> = HashMap::new();
    let (bsha, bn) = for_lines(base, |l| {
        *want.entry(digest(l)).or_default() += 1;
        Ok(())
    })?;
    let mut found = 0usize;
    let (ssha, sn) = for_lines(successor, |l| {
        if let Some(c) = want.get_mut(&digest(l)) {
            if *c > 0 {
                *c -= 1;
                found += 1;
            }
        }
        Ok(())
    })?;
    let missing: Vec<String> = want.iter().filter(|(_, c)| **c > 0).take(5).map(|(d, _)| hex::encode(d)).collect();
    Ok(json!({"base": base, "base_sha256": bsha, "base_lines": bn, "successor": successor,
        "successor_sha256": ssha, "successor_lines": sn, "base_lines_found": found,
        "complete": found == bn, "missing_examples": missing}))
}

fn write_new(p: &Path, b: &[u8]) -> Result<String> {
    let mut f = fs::File::create_new(p).with_context(|| format!("create {}", p.display()))?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(sha_hex(b))
}

fn read_json(p: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(p).with_context(|| format!("read {}", p.display()))?)
        .with_context(|| format!("parse {}", p.display()))
}

pub const EVAL_SPLIT_DIRS: [&str; 3] = ["dev-iter", "dev-select", "cal"];
pub const EVAL_FILES: [&str; 3] = ["documents.jsonl", "metadata.jsonl", "skips.jsonl"];

/// Merge evaluation-human directories (each with `<split>/documents.jsonl`,
/// `metadata.jsonl`, `skips.jsonl`, `summary.json` and `ACQUISITION.json`)
/// into `out`, keeping every line's bytes and order (parts in the given
/// order). Refuses duplicate document ids or texts across parts. The result
/// never prints or returns prose, only counts and digests.
pub fn merge_eval_humans(parts: &[&Path], out: &Path) -> Result<Value> {
    ensure!(!out.exists(), "new output directory required");
    ensure!(parts.len() >= 2, "at least two parts");
    let tmp = out.with_extension("tmp-merge");
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(&tmp)?;
    let mut report = serde_json::Map::new();
    for split in EVAL_SPLIT_DIRS {
        let d = tmp.join(split);
        fs::create_dir_all(&d)?;
        let mut ids = HashSet::new();
        let mut texts = HashSet::new();
        let mut files = serde_json::Map::new();
        let mut sums = String::new();
        for name in EVAL_FILES {
            let mut buf = Vec::new();
            let mut per = Vec::new();
            for p in parts {
                let src = p.join(split).join(name);
                let (sha, n) = for_lines(&src, |l| {
                    if name == "documents.jsonl" {
                        let v: Value = serde_json::from_slice(l)?;
                        let id = v["id"].as_str().context("document id")?.to_string();
                        ensure!(ids.insert(id.clone()), "document id {id} in two parts");
                        let t = v["text_sha256"].as_str().context("text_sha256")?.to_string();
                        ensure!(texts.insert(t), "document {id}: text in two parts");
                    }
                    buf.extend_from_slice(l);
                    buf.push(b'\n');
                    Ok(())
                })?;
                per.push(json!({"part": p, "sha256": sha, "lines": n}));
            }
            let lines: usize = per.iter().map(|x| x["lines"].as_u64().unwrap() as usize).sum();
            let sha = write_new(&d.join(name), &buf)?;
            sums.push_str(&format!("{sha}  {name}\n"));
            files.insert(name.into(), json!({"sha256": sha, "lines": lines, "parts": per}));
        }
        let mut summaries = Vec::new();
        for p in parts {
            summaries.push(json!({"part": p, "summary": read_json(&p.join(split).join("summary.json"))?}));
        }
        let docs = files["documents.jsonl"]["lines"].clone();
        let summary = json!({"schema": "slop_ninja_a_competitive_eval_humans_summary_v2", "split": split,
            "documents": docs, "parts": summaries,
            "merge": "lines of every part kept byte for byte, parts in order; document ids and texts unique across parts"});
        let ssha = write_new(&d.join("summary.json"), &serde_json::to_vec_pretty(&summary)?)?;
        sums.push_str(&format!("{ssha}  summary.json\n"));
        write_new(&d.join("SHA256SUMS"), sums.as_bytes())?;
        report.insert(split.into(), json!({"documents": docs, "files": files}));
    }
    let mut acq = Vec::new();
    for p in parts {
        acq.push(json!({"part": p, "acquisition": read_json(&p.join("ACQUISITION.json"))?}));
    }
    let a = json!({"schema": "slop_ninja_a_competitive_eval_human_acquisition_v2",
        "parts": acq, "merged": report,
        "test": "not acquired (Test prose is acquired only after the protocol seal)"});
    let asha = write_new(&tmp.join("ACQUISITION.json"), &serde_json::to_vec_pretty(&a)?)?;
    let mut top = format!("{asha}  ACQUISITION.json\n");
    for split in EVAL_SPLIT_DIRS {
        top.push_str(&format!("{}  {split}/SHA256SUMS\n", sha_hex(&fs::read(tmp.join(split).join("SHA256SUMS"))?)));
    }
    write_new(&tmp.join("SHA256SUMS"), top.as_bytes())?;
    fs::rename(&tmp, out)?;
    Ok(json!({"out": out, "splits": report}))
}

/// Pools index: one row per admitted pool layer named in `spec` (`layers`:
/// label, admitted path, note path, consumed_by), with the file digest and
/// line count, plus the excluded and superseded entries the spec lists.
pub fn pools_index(spec: &Value) -> Result<Value> {
    let mut layers = Vec::new();
    for l in spec["layers"].as_array().context("layers")? {
        let p = Path::new(l["admitted"].as_str().context("admitted")?);
        let (sha, n) = for_lines(p, |_| Ok(()))?;
        let note = match l["note"].as_str() {
            Some(n) => {
                let v = read_json(Path::new(n))?;
                json!({"path": n, "sha256": sha_hex(&fs::read(n)?), "by_collection": v.get("by_collection").or(v.get("admitted_by_collection")).cloned()})
            }
            None => Value::Null,
        };
        let mut row = l.clone();
        row["admitted_sha256"] = json!(sha);
        row["admitted_rows"] = json!(n);
        row["note"] = note;
        layers.push(row);
    }
    Ok(json!({"schema": "slop_ninja_a_competitive_pools_index_v1", "layers": layers,
        "excluded_sources": spec["excluded_sources"], "superseded": spec["superseded"],
        "reading_rule": spec["reading_rule"]}))
}

/// Check a frontier export migrated to v2 records against the v1 export and
/// its admission sidecar it replaces: every old record reappears with the same
/// id, text, spans and generation, and its new label and admission equal the
/// sidecar's. New records collected since are counted, not compared.
pub fn verify_frontier(old: &Path, new: &Path) -> Result<Value> {
    let mut side: HashMap<(String, String), Value> = HashMap::new();
    let sp = old.join("admission-labels.jsonl");
    if sp.is_file() {
        for_lines(&sp, |l| {
            let v: Value = serde_json::from_slice(l)?;
            let k = (v["record_id"].as_str().context("record_id")?.to_string(), v["text_sha256"].as_str().context("text_sha256")?.to_string());
            ensure!(side.insert(k, v).is_none(), "duplicate sidecar row");
            Ok(())
        })?;
    }
    let mut labels = BTreeMap::new();
    let mut problems = Vec::new();
    let (mut total_old, mut total_new) = (0usize, 0usize);
    for e in fs::read_dir(old)? {
        let e = e?;
        let l = e.file_name().to_string_lossy().into_owned();
        if l.starts_with('.') || !e.path().join("records.jsonl").is_file() {
            continue;
        }
        let mut newrows: HashMap<String, Value> = HashMap::new();
        let np = new.join(&l).join("records.jsonl");
        ensure!(np.is_file(), "label {l} missing from the new export");
        for_lines(&np, |b| {
            let v: Value = serde_json::from_slice(b)?;
            ensure!(v["schema"] == crate::FRONTIER_RECORD_SCHEMA, "new export has a non-v2 record");
            newrows.insert(v["id"].as_str().context("id")?.to_string(), v);
            Ok(())
        })?;
        let mut matched = 0usize;
        let mut n_old = 0usize;
        for_lines(&e.path().join("records.jsonl"), |b| {
            n_old += 1;
            let o: Value = serde_json::from_slice(b)?;
            let id = o["id"].as_str().context("id")?.to_string();
            let Some(n) = newrows.get(&id) else {
                problems.push(json!({"label": l, "id": id, "problem": "missing_in_new"}));
                return Ok(());
            };
            let mut bad = Vec::new();
            for k in ["text", "text_sha256", "parent_id", "source_group", "frozen_split", "domain", "origin", "share_target", "share_evidence", "plan"] {
                if o[k] != n[k] {
                    bad.push(k.to_string());
                }
            }
            for k in ["kind", "bin", "prompt_sha256", "transport", "selector_requested", "model_reported", "voice", "voice_ladder", "events_sha256", "receipt_dir"] {
                if o["generation"][k] != n["generation"][k] {
                    bad.push(format!("generation.{k}"));
                }
            }
            if o["schema"] == "slop_ninja_frontier_positive_v1" {
                match side.get(&(id.clone(), o["text_sha256"].as_str().unwrap_or("").to_string())) {
                    Some(s) => {
                        if s["identity"]["attribution_label"] != n["ai_attribution_label"] {
                            bad.push("ai_attribution_label".into());
                        }
                        if s["identity"]["identity_basis"] != n["ai_identity_basis"] {
                            bad.push("ai_identity_basis".into());
                        }
                        if s["training_admission"] != n["training_admission"] {
                            bad.push("training_admission".into());
                        }
                        if s["cohort_id"] != n["cohort_id"] || s["cohort_weight_policy"] != n["cohort_weight_policy"]["policy_id"] {
                            bad.push("cohort".into());
                        }
                        if s["identity"]["recorded_ai_identity"] != n["ai_identity_detail"]["migrated_from"]["ai_identity"] {
                            bad.push("migrated_from".into());
                        }
                    }
                    None => bad.push("no_sidecar_row".into()),
                }
            } else if o != *n {
                bad.push("v2_record_changed".into());
            }
            if bad.is_empty() {
                matched += 1;
            } else {
                problems.push(json!({"label": l, "id": id, "fields": bad}));
            }
            Ok(())
        })?;
        total_old += n_old;
        total_new += newrows.len();
        labels.insert(l.clone(), json!({"old_records": n_old, "matched": matched, "new_records": newrows.len(),
            "collected_since": newrows.len().saturating_sub(matched)}));
    }
    if total_old == 0 {
        bail!("old export has no records");
    }
    Ok(json!({"old": old, "new": new, "sidecar_rows": side.len(), "old_records": total_old,
        "new_records": total_new, "labels": labels, "problems": problems.len(),
        "problem_examples": problems.into_iter().take(20).collect::<Vec<_>>(),
        "complete": labels.values().all(|v| v["old_records"] == v["matched"])}))
}

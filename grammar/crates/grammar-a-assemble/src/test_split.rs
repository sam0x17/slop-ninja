//! Post-seal Test steps around the bundle build: the scoring rows that the A
//! scorers read, the Pangram collection allow-list, and the overlap audit
//! after Test acquisition (candidates for `scan-overlap` and the exclusion
//! receipt that a rebuild applies).
//!
//! Every Test command refuses unless the written seal exists, hashes to the
//! stated digest and binds the running assembler binary (`SealCheck`).
use crate::bundle::{SealCheck, file_sha, set_read_only};
use crate::trainer::{TrainerWeighting, project};
use crate::{TEST_SPLIT, sha_hex, split_name};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

pub const SCORING_ROWS_SCHEMA: &str = "slop_ninja_a_scoring_rows_v1";
pub const EXCLUSION_SCHEMA: &str = "slop_ninja_a_test_overlap_exclusion_v1";
pub const AUDIT_RECEIPT_SCHEMA: &str = "slop_ninja_a_test_overlap_receipt_v1";
/// Bundle populations inside the primary population (protocol `primary_metric`).
pub const PRIMARY_POPULATIONS: [&str; 2] = ["primary", "frontier"];

fn read_json(p: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(p).with_context(|| format!("read {}", p.display()))?)
        .with_context(|| format!("parse {}", p.display()))
}

fn each_row(p: &Path, mut f: impl FnMut(&str, Value) -> Result<()>) -> Result<()> {
    let rd = BufReader::with_capacity(1 << 20, fs::File::open(p).with_context(|| format!("open {}", p.display()))?);
    for (n, line) in rd.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).with_context(|| format!("{} line {}", p.display(), n + 1))?;
        f(&line, v).with_context(|| format!("{} line {}", p.display(), n + 1))?;
    }
    Ok(())
}

fn write_new(p: &Path, b: &[u8]) -> Result<String> {
    let mut f = fs::OpenOptions::new().create_new(true).write(true).open(p).with_context(|| format!("create {}", p.display()))?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(sha_hex(b))
}

fn pretty(v: &Value) -> Vec<u8> {
    let mut b = serde_json::to_vec_pretty(v).unwrap();
    b.push(b'\n');
    b
}

/// The split directory of a build, checked against its manifest.
fn build_split(build: &Path, split: &str) -> Result<(Value, PathBuf, String)> {
    let b = read_json(&build.join("BUILD.json"))?;
    ensure!(b["schema"] == "slop_ninja_a_bundle_build_v1", "not an assembler build");
    ensure!(b["splits"][split].is_object(), "build has no {split} split");
    let d = build.join(split.replace('_', "-"));
    let m = read_json(&d.join("MANIFEST.json"))?;
    let rows_sha = file_sha(&d.join("rows.jsonl"))?;
    ensure!(m["files"]["rows.jsonl"] == rows_sha.as_str() && b["splits"][split]["rows_sha256"] == rows_sha.as_str(),
        "{split} rows differ from their manifest");
    Ok((b, d, rows_sha))
}

/// Gate of a scored split: Test after the seal (the build must name the same
/// seal); CAL only as the mechanical identity check of the scoring path, on a
/// CAL split already opened for its calibration use. Nothing else is scored here.
fn scoring_gate(b: &Value, d: &Path, split: &str, seal: Option<&SealCheck>, binary_sha256: &str) -> Result<Value> {
    match split {
        TEST_SPLIT => {
            let s = seal.context("Test scoring rows need --seal and --seal-sha256")?;
            let rec = s.verify(binary_sha256)?;
            ensure!(b["seal"]["sha256"] == rec["sha256"], "the build was made under another seal");
            Ok(json!({"purpose": "test_scoring", "seal": rec}))
        }
        "cal" => {
            ensure!(d.join("UNSEALED-calibration_fit.json").is_file(),
                "CAL rows are projected only after CAL was opened for calibration_fit");
            Ok(json!({"purpose": "mechanical_identity_check_of_the_test_scoring_path",
                "rule": "CAL is used only to show that the Test scoring path reproduces the CAL scoring bytes; no metric is computed"}))
        }
        s => bail!("split {s} is never scored here (Dev-select is never scored again)"),
    }
}

/// Project one evaluation split of a build into the scorers' row fields
/// (`trainer::project`, the trainer bundle's row form; evaluation rows carry
/// cohort weight 1) as `out/rows.jsonl` plus `out/MANIFEST.json`.
pub fn scoring_rows(build: &Path, split: &str, seal: Option<&SealCheck>, code: &Value, out: &Path) -> Result<Value> {
    let split = split_name(split);
    ensure!(!out.exists(), "{} exists; scoring rows are written once", out.display());
    let (b, d, rows_sha) = build_split(build, &split)?;
    let gate = scoring_gate(&b, &d, &split, seal, code["assembler_sha256"].as_str().unwrap_or(""))?;
    let mut rows = Vec::new();
    let mut pops: BTreeMap<String, usize> = BTreeMap::new();
    let mut n = 0usize;
    each_row(&d.join("rows.jsonl"), |_, r| {
        ensure!(r["split"] == split.as_str(), "row in the wrong split directory");
        let p = project(&r, &split, false, TrainerWeighting::Final)?;
        *pops.entry(r["population"].as_str().unwrap_or("").to_string()).or_default() += 1;
        serde_json::to_writer(&mut rows, &p)?;
        rows.push(b'\n');
        n += 1;
        Ok(())
    })?;
    let tmp = out.with_extension("tmp-scoring");
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(&tmp)?;
    let sha = write_new(&tmp.join("rows.jsonl"), &rows)?;
    let m = json!({"schema": SCORING_ROWS_SCHEMA, "split": split, "build_id": b["build_id"], "build_dir": build,
        "source_rows_sha256": rows_sha, "files": {"rows.jsonl": sha}, "rows": n, "rows_by_population": pops,
        "row_fields": crate::trainer::TRAINER_FIELDS, "projection": "trainer::project (evaluation rows: cohort_weight 1, operation null for humans)",
        "gate": gate, "code": code, "test_opened": split == TEST_SPLIT});
    write_new(&tmp.join("MANIFEST.json"), &pretty(&m))?;
    for f in ["rows.jsonl", "MANIFEST.json"] {
        set_read_only(&tmp.join(f))?;
    }
    fs::rename(&tmp, out)?;
    Ok(json!({"out": out, "rows": n, "rows_sha256": sha, "rows_by_population": m["rows_by_population"]}))
}

/// The Pangram collection allow-list of a Test bundle: the collections of its
/// primary rows (positives inherit their parent's), each of which must be a
/// collection of the Test humans whose every document is admitted for
/// external evaluation (`rights.external_evaluation`).
pub fn public_collections(rows: &Path, humans: &[PathBuf], seal: &SealCheck, binary_sha256: &str, out: &Path) -> Result<Value> {
    ensure!(!out.exists(), "{} exists", out.display());
    let rec = seal.verify(binary_sha256)?;
    let mut wanted: BTreeMap<String, usize> = BTreeMap::new();
    each_row(rows, |_, r| {
        if r["split"] == TEST_SPLIT && PRIMARY_POPULATIONS.contains(&r["population"].as_str().unwrap_or("")) {
            let c = r["collection"].as_str().with_context(|| format!("primary Test row {} without collection", r["id"]))?;
            *wanted.entry(c.to_string()).or_default() += 1;
        }
        Ok(())
    })?;
    ensure!(!wanted.is_empty(), "no primary Test rows");
    // collection -> (documents, documents admitted for external evaluation)
    let mut seen: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut humans_sha = Vec::new();
    for h in humans {
        humans_sha.push(json!({"path": h, "sha256": file_sha(h)?}));
        each_row(h, |_, v| {
            let c = v["collection"].as_str().context("collection")?;
            let e = seen.entry(c.to_string()).or_default();
            e.0 += 1;
            e.1 += usize::from(v["rights"]["external_evaluation"] == true);
            Ok(())
        })?;
    }
    let mut bad = Vec::new();
    for c in wanted.keys() {
        match seen.get(c) {
            Some((n, ok)) if n == ok => {}
            Some((n, ok)) => bad.push(format!("{c}: {ok} of {n} documents admitted for external evaluation")),
            None => bad.push(format!("{c}: not a Test human collection")),
        }
    }
    ensure!(bad.is_empty(), "collections not admitted for Pangram: {}", bad.join("; "));
    let v = json!({"collections": wanted.keys().collect::<Vec<_>>(),
        "basis": "collections of the primary Test bundle rows (positives inherit their parent's collection); every Test human document of each collection carries rights.external_evaluation true",
        "primary_rows_by_collection": wanted, "rows": rows, "rows_sha256": file_sha(rows)?, "humans": humans_sha, "seal": rec});
    write_new(out, &pretty(&v))?;
    Ok(json!({"out": out, "collections": v["collections"]}))
}

fn style_texts(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let st = &v["generation"]["style"];
    for k in ["voice_text", "brief_text"] {
        if let Some(t) = st[k].as_str().filter(|t| !t.trim().is_empty()) {
            out.push(t.to_string());
        }
    }
    if let Some(t) = v["generation"]["voice"]["text"].as_str().filter(|t| !t.trim().is_empty()) {
        out.push(t.to_string());
    }
    out
}

/// Candidate files of the overlap audit after Test acquisition, for
/// `slop_ninja_a_pools scan-overlap --source`: `test-humans` (documents with
/// their work and article aliases), `test-bundle` (every Test bundle row but
/// the diagnostic slices: humans, open-model, OLMo-2 and frontier positives,
/// i.e. generated-text overlap) and `test-prompts` (every distinct voice or brief text of a Test
/// request, i.e. prompt overlap; `prompt-users.jsonl` maps each to the
/// source groups of the positives that used it).
pub fn audit_candidates(rows: &Path, humans: &[PathBuf], records: &[PathBuf], seal: &SealCheck, binary_sha256: &str, out: &Path) -> Result<Value> {
    ensure!(!out.exists(), "{} exists", out.display());
    let rec = seal.verify(binary_sha256)?;
    let tmp = out.with_extension("tmp-audit");
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    for d in ["test-humans", "test-bundle", "test-prompts"] {
        fs::create_dir_all(tmp.join(d))?;
    }
    let mut hb = Vec::new();
    let mut nh = 0usize;
    for h in humans {
        each_row(h, |_, v| {
            ensure!(v["frozen_split"] == TEST_SPLIT, "{} is not a Test human", v["id"]);
            serde_json::to_writer(&mut hb, &json!({"id": v["id"], "text": v["text"], "source_group": v["source_group"],
                "aliases": v["aliases"].as_array().cloned().unwrap_or_default()}))?;
            hb.push(b'\n');
            nh += 1;
            Ok(())
        })?;
    }
    let mut bb = Vec::new();
    let mut nb = 0usize;
    let mut row_groups: BTreeMap<String, String> = BTreeMap::new();
    let mut diagnostics = 0usize;
    each_row(rows, |_, v| {
        ensure!(v["split"] == TEST_SPLIT, "not a Test bundle row");
        // Diagnostic slices were checked at their reservation; they are not Test acquisitions.
        if v["cohort_id"].as_str().is_some_and(|c| c.starts_with("diagnostic:")) {
            diagnostics += 1;
            return Ok(());
        }
        let g = v["source_group"].as_str().context("source_group")?;
        row_groups.insert(v["id"].as_str().context("id")?.to_string(), g.to_string());
        serde_json::to_writer(&mut bb, &json!({"id": v["id"], "text": v["text"], "source_group": g}))?;
        bb.push(b'\n');
        nb += 1;
        Ok(())
    })?;
    // Prompt style texts of the bundle's positives (records not in the bundle are skipped).
    let mut prompts: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
    let mut records_sha = Vec::new();
    for r in records {
        records_sha.push(json!({"path": r, "sha256": file_sha(r)?}));
        each_row(r, |_, v| {
            let Some(g) = v["id"].as_str().and_then(|id| row_groups.get(id)) else { return Ok(()) };
            for t in style_texts(&v) {
                let k = format!("prompt:{}", &sha_hex(t.as_bytes())[..24]);
                prompts.entry(k).or_insert_with(|| (t.clone(), BTreeSet::new())).1.insert(g.clone());
            }
            Ok(())
        })?;
    }
    let mut pb = Vec::new();
    let mut ub = Vec::new();
    for (k, (t, gs)) in &prompts {
        serde_json::to_writer(&mut pb, &json!({"id": k, "text": t}))?;
        pb.push(b'\n');
        serde_json::to_writer(&mut ub, &json!({"id": k, "source_groups": gs}))?;
        ub.push(b'\n');
    }
    let s1 = write_new(&tmp.join("test-humans/candidates.jsonl"), &hb)?;
    let s2 = write_new(&tmp.join("test-bundle/candidates.jsonl"), &bb)?;
    let s3 = write_new(&tmp.join("test-prompts/candidates.jsonl"), &pb)?;
    let s4 = write_new(&tmp.join("prompt-users.jsonl"), &ub)?;
    let m = json!({"schema": "slop_ninja_a_test_overlap_candidates_v1", "rows": rows, "rows_sha256": file_sha(rows)?,
        "diagnostic_rows_not_candidates": diagnostics,
        "records": records_sha, "seal": rec,
        "files": {"test-humans/candidates.jsonl": {"sha256": s1, "rows": nh}, "test-bundle/candidates.jsonl": {"sha256": s2, "rows": nb},
                  "test-prompts/candidates.jsonl": {"sha256": s3, "rows": prompts.len()}, "prompt-users.jsonl": s4}});
    write_new(&tmp.join("MANIFEST.json"), &pretty(&m))?;
    fs::rename(&tmp, out)?;
    Ok(json!({"out": out, "humans": nh, "bundle_rows": nb, "prompts": prompts.len()}))
}

/// Overlap receipt after Test acquisition: every `scan-overlap` hit of a Test
/// candidate is a conflict (the registry names no Test file); each conflict
/// excludes the whole source group of the candidate (protocol
/// `overlap_checks.conflict_action`). Writes the receipt and the exclusion
/// rows the bundle config's `excluded_source_groups` reads. A registry entry
/// that matched no file is an error (`index_completeness`).
pub fn audit_receipt(cands: &Path, scan: &Path, seal: &SealCheck, binary_sha256: &str, receipt: &Path, exclusions: &Path) -> Result<Value> {
    ensure!(!receipt.exists() && !exclusions.exists(), "receipt outputs exist");
    let rec = seal.verify(binary_sha256)?;
    let report = read_json(&scan.join("scan-report.json"))?;
    let missing = report["entries_without_files"].as_array().map_or(0, Vec::len);
    ensure!(missing == 0, "{missing} registry entries match no file (protocol index_completeness)");
    let mut group_of: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut kind_of: BTreeMap<String, &'static str> = BTreeMap::new();
    for (d, kind) in [("test-humans", "human"), ("test-bundle", "bundle_row")] {
        each_row(&cands.join(d).join("candidates.jsonl"), |_, v| {
            let id = v["id"].as_str().context("id")?.to_string();
            group_of.entry(id.clone()).or_default().insert(v["source_group"].as_str().context("source_group")?.to_string());
            kind_of.entry(id).or_insert(kind);
            Ok(())
        })?;
    }
    each_row(&cands.join("prompt-users.jsonl"), |_, v| {
        let id = v["id"].as_str().context("id")?.to_string();
        for g in v["source_groups"].as_array().context("source_groups")? {
            group_of.entry(id.clone()).or_default().insert(g.as_str().context("group")?.to_string());
        }
        kind_of.insert(id, "prompt");
        Ok(())
    })?;
    // source group -> (candidate ids, reasons, registry labels)
    let mut conflicts: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_label: BTreeMap<String, usize> = BTreeMap::new();
    let mut hits = 0usize;
    let hits_path = scan.join("hits.jsonl");
    each_row(&hits_path, |_, h| {
        hits += 1;
        let id = h["candidate_id"].as_str().context("candidate_id")?;
        let groups = group_of.get(id).with_context(|| format!("hit names unknown candidate {id}"))?;
        let label = h["label"].as_str().unwrap_or("?").to_string();
        *by_kind.entry(kind_of[id].to_string()).or_default() += 1;
        *by_label.entry(label.clone()).or_default() += 1;
        for g in groups {
            let e = conflicts.entry(g.clone()).or_default();
            e.0.insert(id.to_string());
            for r in h["reasons"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                e.1.insert(r.to_string());
            }
            e.2.insert(label.clone());
        }
        Ok(())
    })?;
    let mut xb = Vec::new();
    for (g, (ids, reasons, labels)) in &conflicts {
        serde_json::to_writer(&mut xb, &json!({"schema": EXCLUSION_SCHEMA, "source_group": g, "candidate_ids": ids,
            "reasons": reasons, "registry_labels": labels}))?;
        xb.push(b'\n');
    }
    let xs = write_new(exclusions, &xb)?;
    let v = json!({"schema": AUDIT_RECEIPT_SCHEMA,
        "rule": "protocol overlap_checks (when: after Test acquisition): work and article aliases, exact, whitespace-normalised and folded SHA-256, shared 50-word sequences (whitespace and folded) of Test humans, every Test bundle row (generated-text overlap) and every Test request voice or brief text (prompt overlap) against the registry; every hit is a conflict and excludes its whole source group",
        "candidates_manifest_sha256": file_sha(&cands.join("MANIFEST.json"))?,
        "scan_report_sha256": file_sha(&scan.join("scan-report.json"))?, "hits_sha256": file_sha(&hits_path)?,
        "files_scanned": report["files_scanned"], "file_status_counts": report["file_status_counts"],
        "entries_without_files": report["entries_without_files"], "candidates": report["candidates"],
        "hits": hits, "hits_by_candidate_kind": by_kind, "hits_by_registry_label": by_label,
        "excluded_source_groups": conflicts.len(), "exclusions": exclusions, "exclusions_sha256": xs,
        "any_conflict": !conflicts.is_empty(), "seal": rec, "text_written": false,
        "action": if conflicts.is_empty() { "none" } else { "rebuild the Test bundle with excluded_source_groups = this exclusions file" }});
    write_new(receipt, &pretty(&v))?;
    Ok(json!({"receipt": receipt, "hits": hits, "excluded_source_groups": conflicts.len()}))
}

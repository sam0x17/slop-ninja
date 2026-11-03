//! Reverse overlap scan: the candidates of a small pool are held in memory
//! and every registered file is streamed against them, instead of building a
//! protected index of all registered text. Memory scales with the pool, not
//! with the registry, and files that concurrent tracks are still writing are
//! read as they stand and reported.
//!
//! The checks equal `protected::Index::text_conflicts` and the family-key
//! check: exact, whitespace-normalised and folded text hashes and whitespace
//! and folded 50-word sequences of every JSON string of at least 20 words
//! (unparsable lines and plain files as text), plus equality of any string
//! with a candidate family key or alias. A cheap prefilter skips strings
//! that share no folded 10-token window with any candidate; a shared
//! 50-word sequence or a whole-text match always shares such a window.
//!
//! Output: `hits.jsonl` (candidate id, reasons, registry label and path;
//! never text) and `scan-report.json`. `dedup --prior-hits` treats each hit
//! as a prior conflict.
use crate::{io, protected, text};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::SystemTime,
};

pub const REPORT_SCHEMA: &str = "slop_ninja_a_pool_reverse_overlap_scan_v1";
pub const PREFILTER_TOKENS: usize = 10;
const KIND_REASONS: [&str; 5] = [
    "matches_prior_exact_text",
    "matches_prior_whitespace_normalized_text",
    "matches_prior_folded_text",
    "shares_50_word_sequence_with_prior",
    "shares_folded_50_word_sequence_with_prior",
];

fn fnv(tokens: &[String]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for (i, t) in tokens.iter().enumerate() {
        if i > 0 {
            h ^= b' ' as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        for b in t.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

fn windows(tokens: &[String]) -> impl Iterator<Item = u64> + '_ {
    (0..(tokens.len() + 1).saturating_sub(PREFILTER_TOKENS)).map(move |i| fnv(&tokens[i..i + PREFILTER_TOKENS]))
}

/// Candidate keys held in memory.
#[derive(Default)]
pub struct Pool {
    pub ids: Vec<String>,
    keys: [HashMap<u64, Vec<u32>>; 5],
    prefilter: HashSet<u64>,
    names: HashMap<String, Vec<u32>>,
}

impl Pool {
    pub fn add(&mut self, id: &str, t: &str, names: &[String]) {
        let k = self.ids.len() as u32;
        self.ids.push(id.to_owned());
        let [a, b, c] = text::text_keys(t);
        let mut add = |kind: usize, key: u64| {
            let v = self.keys[kind].entry(key).or_default();
            if v.last() != Some(&k) {
                v.push(k);
            }
        };
        add(0, a);
        add(1, b);
        add(2, c);
        for s in text::shingles_ws(t) {
            add(3, s);
        }
        for s in text::shingles_folded(t) {
            add(4, s);
        }
        self.prefilter.extend(windows(&text::folded_tokens(t)));
        for n in names {
            self.names.entry(n.clone()).or_default().push(k);
        }
    }

    /// Candidates a registered string conflicts with, by reason.
    pub fn check(&self, s: &str, hits: &mut BTreeMap<u32, BTreeSet<&'static str>>) {
        if let Some(v) = self.names.get(s) {
            for &k in v {
                hits.entry(k).or_default().insert("family_key_or_alias_in_prior_snapshot");
            }
        }
        if text::words(s) < text::MIN_INDEXED_WORDS {
            return;
        }
        let toks = text::folded_tokens(s);
        if !windows(&toks).any(|w| self.prefilter.contains(&w)) {
            return;
        }
        let [a, b, c] = text::text_keys(s);
        for (kind, key) in [(0, a), (1, b), (2, c)] {
            for &k in self.keys[kind].get(&key).into_iter().flatten() {
                hits.entry(k).or_default().insert(KIND_REASONS[kind]);
            }
        }
        for (kind, list) in [(3, text::shingles_ws(s)), (4, text::shingles_folded(s))] {
            for key in list {
                for &k in self.keys[kind].get(&key).into_iter().flatten() {
                    hits.entry(k).or_default().insert(KIND_REASONS[kind]);
                }
            }
        }
    }
}

fn visit(v: &Value, f: &mut impl FnMut(&str)) {
    match v {
        Value::String(s) => f(s),
        Value::Array(a) => a.iter().for_each(|x| visit(x, f)),
        Value::Object(m) => m.values().for_each(|x| visit(x, f)),
        _ => {}
    }
}

/// Stream one registered file the way `protected` reads it.
fn scan_file(p: &Path, max_bytes: u64, f: &mut impl FnMut(&str)) -> Result<(u64, &'static str)> {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let base = name.trim_end_matches(".zst");
    if base.ends_with(".jsonl") {
        let r = BufReader::with_capacity(1 << 20, io::open(p)?);
        let (mut rows, mut unparsed) = (0u64, false);
        for (i, line) in r.split(b'\n').enumerate() {
            let line = line.with_context(|| format!("{}:{}", p.display(), i + 1))?;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match serde_json::from_slice::<Value>(&line) {
                Ok(v) => visit(&v, f),
                Err(_) => {
                    f(&String::from_utf8_lossy(&line));
                    unparsed = true;
                }
            }
            rows += 1;
        }
        Ok((rows, if unparsed { "jsonl_with_lines_scanned_as_text" } else { "jsonl" }))
    } else if base.ends_with(".json") {
        let b = io::read_all(p, max_bytes)?;
        match serde_json::from_slice::<Value>(&b) {
            Ok(v) => {
                visit(&v, f);
                Ok((1, "json"))
            }
            Err(_) => {
                f(&String::from_utf8_lossy(&b));
                Ok((1, "json_unparsed_scanned_as_text"))
            }
        }
    } else {
        let b = io::read_all(p, max_bytes)?;
        f(&String::from_utf8_lossy(&b));
        Ok((1, "text"))
    }
}

fn stamp(p: &Path) -> Option<(u64, SystemTime)> {
    let m = fs::metadata(p).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// Load candidates from adapter output directories (`candidates.jsonl`).
pub fn load_pool(sources: &[PathBuf]) -> Result<(Pool, Vec<Value>)> {
    let mut pool = Pool::default();
    let mut inputs = Vec::new();
    for s in sources {
        let p = s.join("candidates.jsonl");
        let (sha, bytes) = io::file_sha(&p)?;
        let mut n = 0u64;
        for row in io::jsonl_rows(&p)? {
            let row = row?;
            let id = row["id"].as_str().context("candidate id")?;
            let t = row["text"].as_str().context("candidate text")?;
            let mut names: Vec<String> = row["source_group"].as_str().map(|s| vec![s.to_owned()]).unwrap_or_default();
            names.extend(row["aliases"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned));
            pool.add(id, t, &names);
            n += 1;
        }
        inputs.push(json!({"path": p, "sha256": sha, "bytes": bytes, "candidates": n}));
    }
    Ok((pool, inputs))
}

pub fn run(registry: &Path, root: &Path, sources: &[PathBuf], out: &Path) -> Result<Value> {
    io::fresh_dir(out)?;
    let (pool, inputs) = load_pool(sources)?;
    let (entries, registry_sha) = protected::load_registry(registry, root)?;
    let mut hits_out = io::JsonlWriter::create(&out.join("hits.jsonl"))?;
    let mut files = Vec::new();
    let mut missing = Vec::new();
    let mut seen = BTreeSet::new();
    let mut status_counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut hit_cands: BTreeSet<u32> = BTreeSet::new();
    let mut strings = 0u64;
    for e in &entries {
        let list = protected::entry_files(e)?;
        if list.is_empty() {
            missing.push(json!({"label": e.label, "path": e.path, "required": e.required}));
        }
        for p in list {
            let Ok(canon) = fs::canonicalize(&p) else {
                anyhow::ensure!(!e.required, "required protected input vanished: {}", p.display());
                *status_counts.entry("vanished_before_scan".into()).or_default() += 1;
                files.push(json!({"label": e.label, "path": p, "status": "vanished_before_scan"}));
                continue;
            };
            if !seen.insert(canon) {
                continue;
            }
            let before = stamp(&p);
            let sha = io::file_sha(&p).ok();
            let mut hits: BTreeMap<u32, BTreeSet<&'static str>> = BTreeMap::new();
            let mut n = 0u64;
            let res = scan_file(&p, e.max_bytes, &mut |s| {
                n += 1;
                pool.check(s, &mut hits)
            });
            strings += n;
            let after = stamp(&p);
            let status = match (&res, sha.is_some(), before == after) {
                (Ok(_), true, true) => "scanned".to_owned(),
                (Ok(_), _, _) => "scanned_but_changed_during_scan".to_owned(),
                (Err(err), _, _) => {
                    anyhow::ensure!(!e.required, "protected input {}: {err:#}", p.display());
                    "read_error_partial_scan".to_owned()
                }
            };
            *status_counts.entry(status.clone()).or_default() += 1;
            for (k, reasons) in &hits {
                hit_cands.insert(*k);
                hits_out.write(&json!({"candidate_id": pool.ids[*k as usize], "reasons": reasons, "label": e.label, "path": p}))?;
            }
            let (records, format) = res.as_ref().map(|(r, f)| (json!(r), json!(f))).unwrap_or((Value::Null, Value::Null));
            files.push(json!({"label": e.label, "scope": e.scope, "path": p, "sha256": sha.as_ref().map(|s| &s.0), "bytes": sha.as_ref().map(|s| s.1),
                              "status": status, "records": records, "format": format, "strings_visited": n, "candidates_hit": hits.len(),
                              "error": res.as_ref().err().map(|e| format!("{e:#}"))}));
        }
    }
    let (hits_sha, hit_rows) = hits_out.finish()?;
    let report = json!({
        "schema": REPORT_SCHEMA,
        "finished_utc": io::now_utc(),
        "registry_path": registry,
        "registry_sha256": registry_sha,
        "name_matcher": protected::NAME_MATCHER,
        "candidate_inputs": inputs,
        "candidates": pool.ids.len(),
        "candidates_with_hits": hit_cands.len(),
        "hits": {"file": "hits.jsonl", "rows": hit_rows, "sha256": hits_sha},
        "files_scanned": files.len(),
        "file_status_counts": status_counts,
        "strings_visited": strings,
        "entries_without_files": missing,
        "files": files,
        "method": format!("reverse_scan;{};prefilter_folded_{PREFILTER_TOKENS}_token_fnv_window", protected::METHOD),
        "limits": [
            "Only registered files are checked; unregistered copies elsewhere are not.",
            "Files written concurrently are read as they stood when scanned; their status is recorded.",
            "64-bit key collisions can only exclude a candidate, never admit one."
        ],
    });
    io::write_json(&out.join("scan-report.json"), &report)?;
    Ok(report)
}

/// The static reason label for a hit reason read back from `hits.jsonl`.
pub fn static_reason(s: &str) -> &'static str {
    KIND_REASONS.iter().copied().chain(["family_key_or_alias_in_prior_snapshot"]).find(|r| *r == s).unwrap_or("reverse_scan_prior_overlap")
}

/// `hits.jsonl` as candidate id to reasons, for `dedup --prior-hits`.
pub fn load_hits(p: &Path) -> Result<HashMap<String, BTreeSet<String>>> {
    let mut m: HashMap<String, BTreeSet<String>> = HashMap::new();
    for r in io::jsonl_rows(p)? {
        let r = r?;
        let id = r["candidate_id"].as_str().context("candidate_id")?.to_owned();
        let e = m.entry(id).or_default();
        for x in r["reasons"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            e.insert(x.to_owned());
        }
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn prose(seed: &str, n: usize) -> String {
        (0..n).map(|i| format!("{seed}{i}")).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn reverse_scan_finds_text_sequence_and_family_overlap() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let (a, b, c) = (prose("a", 120), prose("b", 120), prose("c", 120));
        let mut f = fs::File::create(src.join("candidates.jsonl")).unwrap();
        for (id, t, fam) in [("x1", &a, "f:1"), ("x2", &b, "f:2"), ("x3", &c, "f:3")] {
            writeln!(f, "{}", json!({"id": id, "text": t, "source_group": fam, "aliases": [format!("k:{id}")]})).unwrap();
        }
        let snap = d.path().join("snap");
        fs::create_dir_all(snap.join("sub")).unwrap();
        // x1: exact copy nested in a prompt; x2: a 60-word run inside other text.
        let run2: Vec<&str> = b.split(' ').skip(30).take(60).collect();
        fs::write(snap.join("rows.jsonl"), format!("{}\n{}\nnot json {}\n",
            json!({"prompt": {"source": a}}), json!({"text": format!("{} {}", prose("z", 30), run2.join(" "))}), prose("q", 40))).unwrap();
        fs::write(snap.join("sub/meta.json"), json!({"source_group": "f:3"}).to_string()).unwrap();
        let reg = d.path().join("reg.json");
        fs::write(&reg, json!({"schema": protected::REGISTRY_SCHEMA, "entries": [
            {"label": "snap", "path": "snap", "names": ["*.jsonl", "*.json"]},
            {"label": "gone", "path": "absent", "names": ["*.jsonl"], "required": false}]}).to_string()).unwrap();
        let out = d.path().join("out");
        let rep = run(&reg, d.path(), &[src], &out).unwrap();
        assert_eq!(rep["candidates_with_hits"], 3);
        let hits = load_hits(&out.join("hits.jsonl")).unwrap();
        assert!(hits["x1"].contains("matches_prior_exact_text"));
        assert!(hits["x2"].contains("shares_folded_50_word_sequence_with_prior"));
        assert!(!hits["x2"].contains("matches_prior_exact_text"));
        assert!(hits["x3"].contains("family_key_or_alias_in_prior_snapshot"));
        assert_eq!(rep["entries_without_files"].as_array().unwrap().len(), 1);
        assert!(!fs::read_to_string(out.join("hits.jsonl")).unwrap().contains("a1 a2"));
    }
}

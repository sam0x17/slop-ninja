//! Post-adapter candidate selection by identifier only: drop candidates of
//! blocked cases (a case with an opinion outside the rendered buckets) and
//! keep at most `K` candidates per split group, chosen by the lowest
//! domain-separated SHA-256 rank of the document id. Never reads or scores
//! prose; the output is a normal adapter directory for `dedup`.
use crate::{
    adapter::REPORT_SCHEMA,
    io::{self, JsonlWriter},
    text,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
};

pub const RANK_DOMAIN: &str = "slop_ninja_a_pool_group_cap_v1\0";

pub struct Options<'a> {
    pub per_group: Option<usize>,
    /// Row field holding the group key: `writer_key` or `source_group`.
    pub group_field: &'a str,
    pub blocked_case_keys: Option<&'a Path>,
}

fn rank(id: &str) -> u64 {
    text::h64(format!("{RANK_DOMAIN}{id}").as_bytes())
}

pub fn run(source: &Path, out: &Path, opt: &Options) -> Result<Value> {
    let rep: Value = serde_json::from_slice(&std::fs::read(source.join("report.json")).context("source report")?)?;
    ensure!(rep["schema"] == REPORT_SCHEMA && rep["complete"] == true, "incomplete adapter report");
    let (sha, _) = io::file_sha(&source.join("candidates.jsonl"))?;
    ensure!(rep["candidates_sha256"] == sha.as_str(), "candidates changed since adapter report");
    let blocked: HashSet<String> = match opt.blocked_case_keys {
        Some(p) => std::fs::read_to_string(p)?.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect(),
        None => HashSet::new(),
    };
    // Pass 1: ids per group (text is not kept).
    let mut groups: HashMap<String, Vec<(u64, String)>> = HashMap::new();
    let mut dropped: BTreeMap<&'static str, u64> = BTreeMap::new();
    for row in io::jsonl_rows(&source.join("candidates.jsonl"))? {
        let row = row?;
        let id = row["id"].as_str().context("id")?.to_owned();
        if row["provenance"]["case_key"].as_str().is_some_and(|k| blocked.contains(k)) {
            continue;
        }
        let g = row[opt.group_field].as_str().context("group field")?.to_owned();
        groups.entry(g).or_default().push((rank(&id), id));
    }
    let mut keep: HashSet<String> = HashSet::new();
    let mut capped_groups = 0u64;
    for v in groups.values_mut() {
        v.sort();
        let k = opt.per_group.unwrap_or(usize::MAX);
        if v.len() > k {
            capped_groups += 1;
        }
        keep.extend(v.iter().take(k).map(|(_, id)| id.clone()));
    }
    io::fresh_dir(out)?;
    let mut cw = JsonlWriter::create(&out.join("candidates.jsonl"))?;
    let mut rw = JsonlWriter::create(&out.join("rejections.jsonl"))?;
    let mut words = Vec::new();
    for row in io::jsonl_rows(&source.join("candidates.jsonl"))? {
        let row = row?;
        let id = row["id"].as_str().context("id")?;
        if row["provenance"]["case_key"].as_str().is_some_and(|k| blocked.contains(k)) {
            *dropped.entry("case_blocked_by_opinion_outside_rendered_buckets").or_default() += 1;
            rw.write(&json!({"unit_id": id, "reason": "case_blocked_by_opinion_outside_rendered_buckets"}))?;
        } else if !keep.contains(id) {
            *dropped.entry("group_cap_exceeded").or_default() += 1;
            rw.write(&json!({"unit_id": id, "reason": "group_cap_exceeded"}))?;
        } else {
            words.push(row["word_count"].as_u64().unwrap_or(0));
            cw.write(&row)?;
        }
    }
    let (csha, crows) = cw.finish()?;
    let (rsha, rrows) = rw.finish()?;
    let mut r = rep.clone();
    r["candidates"] = json!(crows);
    r["candidates_sha256"] = json!(csha);
    r["rejections"] = json!(rrows);
    r["rejections_sha256"] = json!(rsha);
    r["finished_utc"] = json!(io::now_utc());
    words.sort_unstable();
    r["candidate_word_counts"] = if words.is_empty() { json!(null) } else {
        json!({"min": words[0], "median": words[words.len() / 2], "max": words[words.len() - 1], "mean": words.iter().sum::<u64>() as f64 / words.len() as f64})
    };
    r["post_selection"] = json!({
        "schema": "slop_ninja_a_pool_candidate_selection_v1",
        "source_dir": source, "source_report_sha256": text::sha_hex(&std::fs::read(source.join("report.json"))?),
        "source_candidates": rep["candidates"], "source_candidates_sha256": sha,
        "blocked_case_keys": opt.blocked_case_keys.map(|p| json!({"path": p, "count": blocked.len(), "sha256": io::file_sha(p).ok().map(|x| x.0)})),
        "group_field": opt.group_field, "per_group_cap": opt.per_group, "groups": groups.len(), "capped_groups": capped_groups,
        "dropped": dropped,
        "rule": format!("drop rows whose provenance.case_key is blocked; then per group keep the lowest first-8-bytes SHA-256(\"{}\\0\" + id); identifiers only, prose never read", RANK_DOMAIN.trim_end_matches('\0')),
    });
    io::write_json(&out.join("report.json"), &r)?;
    Ok(r["post_selection"].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Run;

    #[test]
    fn caps_groups_and_drops_blocked_cases() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        let run = Run::start(&src, "t", "c", json!({}), None).unwrap();
        drop(run.finish(json!({})).unwrap());
        // Rewrite candidates by hand with a matching report.
        let rows: Vec<Value> = (0..5).map(|i| json!({"id": format!("d{i}"), "writer_key": if i < 4 { "g1" } else { "g2" }, "word_count": 200,
                                                      "provenance": {"case_key": if i == 0 { "case:x" } else { "case:y" }}})).collect();
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(src.join("candidates.jsonl"), &body).unwrap();
        let mut rep: Value = serde_json::from_slice(&std::fs::read(src.join("report.json")).unwrap()).unwrap();
        rep["candidates_sha256"] = json!(text::sha_hex(body.as_bytes()));
        std::fs::write(src.join("report.json"), rep.to_string()).unwrap();
        let bl = d.path().join("blocked.txt");
        std::fs::write(&bl, "case:x\n").unwrap();
        let out = d.path().join("out");
        let r = super::run(&src, &out, &Options { per_group: Some(2), group_field: "writer_key", blocked_case_keys: Some(&bl) }).unwrap();
        assert_eq!(r["dropped"]["case_blocked_by_opinion_outside_rendered_buckets"], 1);
        assert_eq!(r["dropped"]["group_cap_exceeded"], 1);
        let kept: Vec<Value> = io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(kept.len(), 3);
        let outrep: Value = serde_json::from_slice(&std::fs::read(out.join("report.json")).unwrap()).unwrap();
        assert_eq!(outrep["candidates_sha256"], io::file_sha(&out.join("candidates.jsonl")).unwrap().0);
    }
}

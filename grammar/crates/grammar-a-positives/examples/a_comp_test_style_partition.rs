//! Test voice partition from one further style-library invention round
//! (protocol positive_plan.frontier.test_partition).
//!
//! a_comp_test_style_partition LIBRARY_V2 LIBRARY_NEW FROZEN_PARTITIONS EVAL_MANIFEST_LIST ROUND OUT_DIR
//!     [EXTRA_VOICES.jsonl ...]
//!
//! LIBRARY_V2 is the frozen compiled library behind FROZEN_PARTITIONS
//! (style-library-v2-final), LIBRARY_NEW the compile of a copy of its journals
//! after invention round ROUND (same binary, models and settings), and
//! EXTRA_VOICES the non-library voice files named in the freeze config
//! (frontier voices). Every frozen row must resolve to a text from LIBRARY_V2
//! or EXTRA_VOICES.
//!
//! Rule. For each kind (voice prompts at the library voice threshold 0.5,
//! briefs at 0.3), the library's own TF-IDF leader clustering runs over every
//! existing item (LIBRARY_V2 in acceptance order, then extra voices) followed
//! by the round's new accepted items in acceptance order. A new item is kept
//! iff its cluster leader is itself a new item, so its cluster contains no
//! item of any existing partition (train, dev_iter, dev_select, cal). Kept
//! items are then screened for evaluation authors: the library's recorded
//! conflicts (Test, CAL, Dev-iter, Dev-select manifests of splits-v2), the
//! eval-author withdrawal rule (strict normalised full name or a one-token
//! surname equal to the last token of an evaluation author key) on the
//! imitated author against EVAL_MANIFEST_LIST, and the library rule
//! (`library::author_conflict`) against the same keys. Before anything is
//! kept, LIBRARY_V2's recorded clusters are recomputed from its items and must
//! match exactly, so the threshold semantics are the library's.
//!
//! If fewer than 500 voice prompts or 300 briefs remain, the shortfall is
//! filled from the frozen dev_select partition in SHA-256 order of
//! "a-comp-test-style-partition-v1:shortfall:" + id, after the same author
//! screens, and recorded. Nothing here reads or writes evaluation prose.
use anyhow::{Context, Result, bail, ensure};
use grammar_a_positives::library::{
    BRIEF_TAU, VOICE_TAU, author_conflict, brief_tokens, leader_clusters, quantiles, tfidf, voice_tokens,
};
use grammar_a_positives::text::{sha_hex, stopwords};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};
use unicode_normalization::UnicodeNormalization;

const POLICY: &str = "a-comp-test-style-partition-v1";
const MIN_VOICES: usize = 500;
const MIN_BRIEFS: usize = 300;

fn jsonl(p: &Path) -> Result<Vec<Value>> {
    BufReader::new(fs::File::open(p).with_context(|| p.display().to_string())?)
        .lines()
        .filter(|l| l.as_ref().map_or(true, |l| !l.trim().is_empty()))
        .map(|l| Ok(serde_json::from_str(&l?)?))
        .collect()
}
fn file_sha(p: &Path) -> Result<String> {
    Ok(sha_hex(&fs::read(p).with_context(|| p.display().to_string())?))
}
fn write_jsonl(p: &Path, rows: &[Value]) -> Result<String> {
    let mut b = Vec::new();
    for r in rows {
        serde_json::to_writer(&mut b, r)?;
        b.push(b'\n');
    }
    let mut f = fs::OpenOptions::new().create_new(true).write(true).open(p)?;
    f.write_all(&b)?;
    f.sync_all()?;
    Ok(sha_hex(&b))
}

/// `grammar_a_assemble::styles::normalize_text` (the frozen `text_key` basis).
fn normalize_style_text(s: &str) -> String {
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
fn text_key(s: &str) -> String {
    sha_hex(normalize_style_text(s).as_bytes())
}

/// Withdrawal-rule normalisation: NFKD, ASCII only, lowercase, non-alphanumeric runs to '_'.
fn norm(s: &str) -> String {
    let a: String = s.nfkd().filter(char::is_ascii).collect::<String>().to_lowercase();
    let mut out = String::new();
    let mut sep = false;
    for c in a.chars() {
        if c.is_ascii_alphanumeric() {
            if sep && !out.is_empty() {
                out.push('_');
            }
            out.push(c);
            sep = false;
        } else {
            sep = true;
        }
    }
    out
}

struct Authors {
    keys: BTreeSet<String>,
    full: HashMap<String, BTreeSet<String>>,
    last: HashMap<String, BTreeSet<String>>,
    manifests: Vec<Value>,
}
fn load_authors(list: &Path) -> Result<Authors> {
    let mut a = Authors { keys: BTreeSet::new(), full: HashMap::new(), last: HashMap::new(), manifests: Vec::new() };
    for p in fs::read_to_string(list)?.split_whitespace() {
        let p = Path::new(p);
        for d in jsonl(p)? {
            if let Some(k) = d["group_key"].as_str().and_then(|g| g.strip_prefix("author:")) {
                a.keys.insert(k.to_owned());
            }
            let fks = std::iter::once(&d["family_key"]).chain(d["family_keys"].as_array().into_iter().flatten());
            for fk in fks {
                if let Some(k) = fk.as_str().and_then(|f| f.strip_prefix("author_work_v2:")) {
                    a.keys.insert(k.split(':').next().unwrap_or("").to_owned());
                }
            }
        }
        a.manifests.push(json!({"path": p, "sha256": file_sha(p)?}));
    }
    for k in &a.keys {
        let n = norm(k);
        if !n.is_empty() {
            a.full.entry(n.clone()).or_default().insert(k.clone());
            a.last.entry(n.rsplit('_').next().unwrap().to_owned()).or_default().insert(k.clone());
        }
    }
    Ok(a)
}

/// Every evaluation-author screen; returns the reasons an item is excluded.
fn author_hits(item: &Value, a: &Authors) -> Vec<Value> {
    let mut hits = Vec::new();
    if item["eval_author_conflicts"].as_array().is_some_and(|x| !x.is_empty()) {
        hits.push(json!({"rule": "library_recorded_conflict", "hits": item["eval_author_conflicts"]}));
    }
    let imitated = item["imitated_author"].as_str().filter(|s| !s.trim().is_empty() && s.to_lowercase() != "null");
    if let Some(name) = imitated {
        let n = norm(name);
        if !n.is_empty() {
            if let Some(k) = a.full.get(&n) {
                hits.push(json!({"rule": "withdrawal_strict", "imitated_author": name, "keys": k}));
            } else if !n.contains('_') {
                if let Some(k) = a.last.get(&n) {
                    hits.push(json!({"rule": "withdrawal_surname_only", "imitated_author": name, "keys": k}));
                }
            }
        }
    }
    let text = item["text"].as_str().unwrap_or("");
    let probe = format!("{} {}", imitated.unwrap_or(""), if imitated.is_none() { text } else { "" });
    let lib: Vec<&String> = a.keys.iter()
        .filter(|k| match author_conflict(&probe, &format!("author:{k}")) {
            Some("full_name") => true,
            Some(_) => imitated.is_some(),
            None => false,
        })
        .collect();
    if !lib.is_empty() {
        hits.push(json!({"rule": "library_author_conflict_rule", "keys": lib}));
    }
    hits
}

fn kind_of(t: &str) -> Result<&'static str> {
    Ok(match t {
        "voice" | "voice_prompt" => "voice_prompt",
        "brief" => "brief",
        other => bail!("unknown style kind {other}"),
    })
}

fn tokens(kind: &str, text: &str, stop: &HashSet<&'static str>) -> Vec<String> {
    if kind == "voice_prompt" { voice_tokens(text) } else { brief_tokens(text, stop) }
}

fn run(a: &[PathBuf]) -> Result<()> {
    ensure!(a.len() >= 6, "LIBRARY_V2 LIBRARY_NEW FROZEN_PARTITIONS EVAL_MANIFEST_LIST ROUND OUT_DIR [EXTRA_VOICES.jsonl ...]");
    let (lib2, libn, frozen_p, mlist, round, out) = (&a[0], &a[1], &a[2], &a[3], &a[4], &a[5]);
    let extras = &a[6..];
    let round: u64 = round.to_str().context("round")?.parse()?;
    ensure!(!out.exists(), "new output directory required");
    let stop = stopwords();
    let authors = load_authors(mlist)?;

    // Existing items: frozen library (acceptance order), with recorded clusters.
    let mut existing: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    let mut known_ids: HashMap<String, (String, String)> = HashMap::new(); // id -> (kind, text)
    for (file, kind) in [("voices.accepted.jsonl", "voice_prompt"), ("briefs.accepted.jsonl", "brief")] {
        let rows = jsonl(&lib2.join(file))?;
        for r in &rows {
            ensure!(kind_of(r["type"].as_str().context("type")?)? == kind, "kind of {file}");
            known_ids.insert(r["id"].as_str().context("id")?.to_owned(), (kind.to_owned(), r["text"].as_str().context("text")?.to_owned()));
        }
        existing.insert(kind, rows);
    }
    // Reproduce the frozen library's clusters (threshold semantics check).
    let mut reproduction = serde_json::Map::new();
    for (kind, tau, prefix) in [("voice_prompt", VOICE_TAU, "vc-"), ("brief", BRIEF_TAU, "bc-")] {
        let rows = &existing[kind];
        let toks: Vec<Vec<String>> = rows.iter().map(|r| tokens(kind, r["text"].as_str().unwrap(), &stop)).collect();
        let (assign, _) = leader_clusters(&tfidf(&toks), tau);
        let mut leader: HashMap<usize, String> = HashMap::new();
        let mut mismatched = 0usize;
        for (i, c) in assign.iter().enumerate() {
            let l = leader.entry(*c).or_insert_with(|| rows[i]["id"].as_str().unwrap().to_owned());
            if rows[i]["cluster"].as_str() != Some(&format!("{prefix}{}", &l[5..])) {
                mismatched += 1;
            }
        }
        ensure!(mismatched == 0, "{kind}: {mismatched} recorded library clusters not reproduced");
        reproduction.insert(kind.into(), json!({"items": rows.len(), "clusters": leader.len(), "tau": tau, "mismatched": 0}));
    }
    // Extra (non-library) voices named by the freeze config.
    let mut extra_rows: BTreeMap<&str, Vec<Value>> = BTreeMap::from([("voice_prompt", vec![]), ("brief", vec![])]);
    let mut extra_files = Vec::new();
    let lib_keys: HashSet<String> = known_ids.values().map(|(_, t)| text_key(t)).collect();
    let mut extra_keys = HashSet::new();
    for f in extras {
        let rows = jsonl(f)?;
        let mut added = 0usize;
        for r in rows {
            let id = r["id"].as_str().context("extra id")?.to_owned();
            let kind = kind_of(r["kind"].as_str().context("extra kind")?)?;
            let text = r["text"].as_str().context("extra text")?.to_owned();
            if known_ids.contains_key(&id) {
                ensure!(known_ids[&id].1 == text, "extra voice {id} differs from the library text");
                continue;
            }
            known_ids.insert(id.clone(), (kind.to_owned(), text.clone()));
            let key = text_key(&text);
            if lib_keys.contains(&key) || !extra_keys.insert(key) {
                continue;
            }
            extra_rows.get_mut(kind).unwrap().push(json!({"id": id, "text": text}));
            added += 1;
        }
        extra_files.push(json!({"path": f, "sha256": file_sha(f)?, "added_items": added}));
    }
    // Frozen rows must all resolve to a text, and the recorded text_key must match.
    let frozen = jsonl(frozen_p)?;
    let mut frozen_by_id: HashMap<String, Value> = HashMap::new();
    let mut frozen_parts = BTreeMap::<String, usize>::new();
    for r in &frozen {
        ensure!(r["schema"] == "slop_ninja_a_frozen_style_partition_v1", "frozen schema");
        let id = r["id"].as_str().context("frozen id")?;
        let (kind, text) = known_ids.get(id).with_context(|| format!("frozen row {id} has no known text"))?;
        ensure!(kind_of(r["kind"].as_str().context("kind")?)? == kind, "frozen row {id}: kind");
        ensure!(r["text_key"].as_str() == Some(text_key(text).as_str()), "frozen row {id}: text key");
        *frozen_parts.entry(format!("{kind}:{}", r["partition"].as_str().unwrap_or("?"))).or_default() += 1;
        frozen_by_id.insert(id.to_owned(), r.clone());
    }

    // New items of the round.
    let mut new: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    let mut new_lib_counts = serde_json::Map::new();
    for (file, kind) in [("voices.accepted.jsonl", "voice_prompt"), ("briefs.accepted.jsonl", "brief")] {
        let rows = jsonl(&libn.join(file))?;
        let mut keep = Vec::new();
        let mut old = 0usize;
        for r in rows {
            let id = r["id"].as_str().context("id")?;
            if let Some((_, t)) = known_ids.get(id) {
                ensure!(r["text"].as_str() == Some(t.as_str()), "library item {id} changed text");
                old += 1;
                continue;
            }
            ensure!(r["round"].as_u64() == Some(round), "new library item {id} is not from round {round}");
            keep.push(r);
        }
        ensure!(old == existing[kind].len(), "{kind}: the new compile lost frozen library items");
        new_lib_counts.insert(kind.into(), json!({"frozen_items": old, "new_round_items": keep.len()}));
        new.insert(kind, keep);
    }

    // Cluster existing + new; keep new items in clusters founded by new items.
    let mut kept: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    let mut stats = serde_json::Map::new();
    let mut excluded = Vec::new();
    for (kind, tau, prefix) in [("voice_prompt", VOICE_TAU, "vc-"), ("brief", BRIEF_TAU, "bc-")] {
        let mut texts: Vec<&str> = existing[kind].iter().map(|r| r["text"].as_str().unwrap()).collect();
        texts.extend(extra_rows[kind].iter().map(|r| r["text"].as_str().unwrap()));
        let n_old = texts.len();
        texts.extend(new[kind].iter().map(|r| r["text"].as_str().unwrap()));
        let toks: Vec<Vec<String>> = texts.iter().map(|t| tokens(kind, t, &stop)).collect();
        let (assign, nn) = leader_clusters(&tfidf(&toks), tau);
        let mut first: HashMap<usize, usize> = HashMap::new();
        for (i, c) in assign.iter().enumerate() {
            first.entry(*c).or_insert(i);
        }
        let (mut in_old, mut in_new) = (0usize, 0usize);
        let mut out_rows = Vec::new();
        let mut kept_nn = Vec::new();
        let mut sizes: HashMap<usize, usize> = HashMap::new();
        for (j, r) in new[kind].iter().enumerate() {
            let i = n_old + j;
            let leader = first[&assign[i]];
            if leader < n_old {
                in_old += 1;
                continue;
            }
            in_new += 1;
            let hits = author_hits(r, &authors);
            if !hits.is_empty() {
                excluded.push(json!({"id": r["id"], "kind": kind, "reason": "evaluation_author", "hits": hits}));
                continue;
            }
            let lid = new[kind][leader - n_old]["id"].as_str().unwrap().to_owned();
            let mut row = r.clone();
            row["cluster"] = json!(format!("{prefix}{}", &lid[5..]));
            row["cluster_tau"] = json!(tau);
            row["nearest_neighbour_cosine_all"] = json!(nn[i]);
            row["partition"] = json!("test");
            row["test_partition_policy"] = json!(POLICY);
            *sizes.entry(assign[i]).or_default() += 1;
            kept_nn.push(nn[i]);
            out_rows.push(row);
        }
        let mut sz: Vec<usize> = sizes.values().copied().collect();
        sz.sort_unstable();
        stats.insert(kind.into(), json!({"tau": tau, "existing_items_clustered": n_old,
            "existing_library_items": existing[kind].len(), "extra_voice_items": extra_rows[kind].len(),
            "new_items": new[kind].len(), "new_items_in_existing_clusters": in_old,
            "new_items_in_new_clusters": in_new, "kept": out_rows.len(), "kept_clusters": sz.len(),
            "kept_singleton_clusters": sz.iter().filter(|x| **x == 1).count(),
            "kept_largest_cluster": sz.last().copied().unwrap_or(0),
            "kept_nearest_neighbour_cosine_all_items": quantiles(kept_nn)}));
        kept.insert(kind, out_rows);
    }

    // Shortfall from dev_select, if needed.
    let mut shortfall = serde_json::Map::new();
    for (kind, min, file) in [("voice_prompt", MIN_VOICES, "voices.accepted.jsonl"), ("brief", MIN_BRIEFS, "briefs.accepted.jsonl")] {
        let have = kept[kind].len();
        if have >= min {
            shortfall.insert(kind.into(), json!({"needed": 0}));
            continue;
        }
        let mut pool: Vec<(String, Value)> = jsonl(&lib2.join(file))?
            .into_iter()
            .filter(|r| frozen_by_id.get(r["id"].as_str().unwrap_or("")).is_some_and(|f| f["partition"] == "dev_select"))
            .map(|r| (sha_hex(format!("{POLICY}:shortfall:{}", r["id"].as_str().unwrap()).as_bytes()), r))
            .collect();
        pool.sort_by(|x, y| x.0.cmp(&y.0));
        let mut taken = Vec::new();
        for (_, r) in pool {
            if kept[kind].len() >= min {
                break;
            }
            if !author_hits(&r, &authors).is_empty() {
                continue;
            }
            let mut row = r.clone();
            row["partition"] = json!("test");
            row["test_partition_policy"] = json!(POLICY);
            row["test_partition_source"] = json!("dev_select_shortfall");
            taken.push(r["id"].clone());
            kept.get_mut(kind).unwrap().push(row);
        }
        shortfall.insert(kind.into(), json!({"needed": min - have, "filled": taken.len(), "ids": taken,
            "rule": "frozen dev_select items in SHA-256 order of policy:shortfall:id after the author screens; held out from Train and CAL, seen in selection"}));
    }

    // Outputs.
    fs::create_dir_all(out.join("test-library"))?;
    let voices_sha = write_jsonl(&out.join("test-library/voices.accepted.jsonl"), &kept["voice_prompt"])?;
    let briefs_sha = write_jsonl(&out.join("test-library/briefs.accepted.jsonl"), &kept["brief"])?;
    let mut part_rows = Vec::new();
    for kind in ["voice_prompt", "brief"] {
        for r in &kept[kind] {
            let id = r["id"].as_str().unwrap();
            ensure!(!frozen_by_id.contains_key(id) || r["test_partition_source"] == "dev_select_shortfall", "test id {id} already frozen");
            part_rows.push(json!({"schema": "slop_ninja_a_frozen_style_partition_v1", "id": id, "kind": kind, "partition": "test",
                "text_key": text_key(r["text"].as_str().unwrap()), "cluster": r["cluster"], "origin": r["origin"], "axis": r["axis"],
                "basis": if r["test_partition_source"] == "dev_select_shortfall" { "test_shortfall_from_dev_select" } else { "test_new_cluster_round" },
                "history": {}}));
        }
    }
    part_rows.sort_by(|x, y| x["id"].as_str().cmp(&y["id"].as_str()));
    let part_sha = write_jsonl(&out.join("test-partition-v1.jsonl"), &part_rows)?;
    // Successor frozen file: every frozen row unchanged, then the Test rows (shortfall ids move to test).
    let moved: HashSet<&str> = part_rows.iter().filter(|r| r["basis"] == "test_shortfall_from_dev_select").map(|r| r["id"].as_str().unwrap()).collect();
    let mut all_rows: Vec<Value> = frozen.iter().filter(|r| !moved.contains(r["id"].as_str().unwrap())).cloned().collect();
    all_rows.extend(part_rows.iter().cloned());
    let all_sha = write_jsonl(&out.join("frozen-partitions-v1-plus-test.jsonl"), &all_rows)?;
    let excl_sha = write_jsonl(&out.join("excluded-author.jsonl"), &excluded)?;
    let digests: Vec<Value> = part_rows.iter().map(|r| json!([r["id"], r["kind"], r["text_key"]])).collect();
    let mut cov = BTreeMap::<String, BTreeMap<String, usize>>::new();
    for kind in ["voice_prompt", "brief"] {
        for r in &kept[kind] {
            for (d, k) in [("axis", "axis"), ("inventor", "inventor"), ("origin", "origin"), ("genre", "genre")] {
                *cov.entry(format!("{kind}_by_{d}")).or_default().entry(r[k].as_str().unwrap_or("null").to_owned()).or_default() += 1;
            }
            *cov.entry(format!("{kind}_rewrite_safe")).or_default().entry(r["rewrite_safe"].to_string()).or_default() += 1;
        }
    }
    let record = json!({"schema": "slop_ninja_a_comp_test_style_partition_v1", "policy": POLICY, "round": round,
        "rule": "one further invention round; keep new items whose cluster (library TF-IDF leader clustering, voice tau 0.5, brief tau 0.3) is founded by a new item after every existing item (frozen library v2 in acceptance order, then the freeze config's extra voices); then drop items matching evaluation authors (library recorded conflicts, withdrawal strict/surname rule on the imitated author, library author_conflict rule) against the listed manifests",
        "minimums": {"voice_prompt": MIN_VOICES, "brief": MIN_BRIEFS},
        "counts": {"voice_prompt": kept["voice_prompt"].len(), "brief": kept["brief"].len()},
        "minimums_met_without_shortfall": shortfall.values().all(|v| v["needed"] == 0),
        "shortfall": shortfall, "cluster_stats": stats, "library_cluster_reproduction": reproduction,
        "new_compile": new_lib_counts, "excluded_evaluation_author": excluded.len(),
        "frozen_rows_by_kind_partition": frozen_parts, "coverage": cov,
        "inputs": {"library_v2": {"dir": lib2, "voices_sha256": file_sha(&lib2.join("voices.accepted.jsonl"))?,
                                  "briefs_sha256": file_sha(&lib2.join("briefs.accepted.jsonl"))?},
                   "library_new": {"dir": libn, "voices_sha256": file_sha(&libn.join("voices.accepted.jsonl"))?,
                                   "briefs_sha256": file_sha(&libn.join("briefs.accepted.jsonl"))?,
                                   "summary_sha256": file_sha(&libn.join("summary.json"))?},
                   "frozen_partitions": {"path": frozen_p, "sha256": file_sha(frozen_p)?},
                   "extra_voices": extra_files, "eval_author_manifests": authors.manifests,
                   "evaluation_author_keys": authors.keys.len()},
        "outputs": {"test-library/voices.accepted.jsonl": voices_sha, "test-library/briefs.accepted.jsonl": briefs_sha,
                    "test-partition-v1.jsonl": part_sha, "frozen-partitions-v1-plus-test.jsonl": all_sha,
                    "excluded-author.jsonl": excl_sha},
        "item_digests": digests,
        "use": "sealed before the seal; unused before the seal; drawn only by the Test frontier wave",
        "detector_or_pangram_calls": 0});
    let mut f = fs::OpenOptions::new().create_new(true).write(true).open(out.join("RECORD.json"))?;
    serde_json::to_writer_pretty(&mut f, &record)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!("{}", json!({"counts": record["counts"], "shortfall": record["shortfall"], "stats": record["cluster_stats"],
                          "excluded_evaluation_author": excluded.len()}));
    Ok(())
}

fn main() -> Result<()> {
    run(&std::env::args_os().skip(1).map(PathBuf::from).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn withdrawal_normalisation_and_text_key() {
        assert_eq!(norm("Charlotte Brontë"), "charlotte_bronte");
        assert_eq!(norm("  W. Shakespeare "), "w_shakespeare");
        assert_eq!(norm("---"), "");
        assert_eq!(normalize_style_text("Sound like a Sailor’s ghost!"), "sound like a sailors ghost");
    }
}

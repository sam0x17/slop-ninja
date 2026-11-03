//! Per-blog writer keys for a blog split directory (`splits-v2-blogs/v2`),
//! written as a successor directory; the base directory is only read.
//!
//! * **Writer keys.** Every FineWeb blog row (Train, Train-mining, held and
//!   reserved units) takes `blogs::writer_identity` of its URL as its
//!   writer (reservation: group) key, so a hosted blog that inherited a
//!   platform-wide site group (`fineweb_site:wordpress.com`) and a Blogger
//!   country-domain blog (`fineweb_site:blogspot.co.uk`) become their own
//!   writers. PLOS blog rows are unchanged.
//! * **Sides.** No document changes side. The base key, whose bucket placed
//!   the row on the Train side or in the reservation (and keeps it
//!   consistent with the FineWeb pool's split), is kept as
//!   `split_group_key`; reserved units keep its bucket.
//! * **Train-side closure.** A FineWeb blog Train-side row whose writer
//!   also has a page in the reserved range of the FineWeb pool scan or in a
//!   sealed FineWeb evaluation split is held
//!   (`contributor_in_fineweb_evaluation_scope`).
//! * **Caps.** Per writer: at most `train_max_docs_per_writer` Train rows and
//!   `mining_max_docs_per_writer` Train-side rows in total. Rows are visited
//!   protected first (source passages already used for positives), then
//!   Train before Train-mining, then seeded rank; an extra Train row moves to
//!   Train-mining while the total allows, else it is held
//!   (`over_writer_cap_after_rekey`). Protected rows never move.
//! * **Reservation closure.** Reserved units are regrouped by writer. A
//!   group is blocked (listed separately, not offered for allocation) when
//!   its writer has any Train-side text: a FineWeb blog pool row of any
//!   split (all were rendered) or a page of the FineWeb pool scan below the
//!   reserved range. Groups whose writer also appears in a sealed FineWeb
//!   evaluation split or in the FineWeb reserved range are annotated so an
//!   allocator can keep one contributor in one evaluation split.
use crate::{
    blogs,
    io::{self, JsonlWriter},
    psl::Psl,
    splits::rank,
    text,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
};

pub const SUMMARY_SCHEMA: &str = "slop_ninja_a_competitive_blog_split_rekey_summary_v1";
const FW: &str = crate::fineweb_blogs::COLLECTION;
const NEW_DOMAIN_EVAL_SPLITS: [&str; 4] = ["test", "cal", "dev-select", "dev-iter"];

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

/// Verify every file listed in `dir/SHA256SUMS`.
fn verify_sums(dir: &Path) -> Result<String> {
    let sums = std::fs::read_to_string(dir.join("SHA256SUMS")).context("base SHA256SUMS")?;
    for l in sums.lines().filter(|l| !l.trim().is_empty()) {
        let (h, f) = l.split_once("  ").context("SHA256SUMS line")?;
        let (got, _) = io::file_sha(&dir.join(f))?;
        ensure!(got == h, "base file {f} changed: {got} != {h}");
    }
    Ok(text::sha_hex(sums.as_bytes()))
}

struct Entry {
    split: u8, // 0 train, 1 train_mining
    writer: String,
    rule: &'static str,
    rank: u64,
    protected: bool,
}

pub fn run(config: &Path) -> Result<Value> {
    let raw: Value = serde_json::from_slice(&std::fs::read(config).with_context(|| format!("read {}", config.display()))?)?;
    let p = |k: &str| -> Result<PathBuf> { Ok(PathBuf::from(raw[k].as_str().with_context(|| format!("config {k}"))?)) };
    let seed = raw["seed"].as_str().context("seed")?.to_owned();
    let (base, out, scan, pool, new_domains, psl_path) = (p("base")?, p("out")?, p("fineweb_scan")?, p("fineweb_blogs_pool")?, p("new_domains")?, p("psl")?);
    let lo = raw["reserved_buckets"][0].as_u64().context("reserved_buckets")? as u32;
    ensure!(raw["reserved_buckets"][1] == 1000 && lo > 0 && lo < 1000, "reserved_buckets must be [LO, 1000]");
    let tcap = raw["train_max_docs_per_writer"].as_u64().context("train_max_docs_per_writer")?;
    let mcap = raw["mining_max_docs_per_writer"].as_u64().context("mining_max_docs_per_writer")?;
    ensure!(mcap >= tcap, "mining cap includes Train documents");
    let protected_files: Vec<PathBuf> = raw["protected_passages"].as_array().into_iter().flatten().filter_map(Value::as_str).map(PathBuf::from).collect();
    let base_sums = verify_sums(&base)?;
    io::fresh_dir(&out)?;
    let psl = Psl::parse(&std::fs::read_to_string(&psl_path)?);
    let ident = |url: &str| -> Result<(String, &'static str)> { blogs::writer_identity(&psl, url).with_context(|| format!("no writer identity for {url}")) };

    // -------------------------------------------------- FineWeb pool scan: writers by side
    let mut fw_train: HashSet<String> = HashSet::new();
    let mut fw_reserved: HashSet<String> = HashSet::new();
    let mut scan_families: HashSet<String> = HashSet::new();
    let mut scan_rows = 0u64;
    for r in io::jsonl_rows(&scan)? {
        let r = r?;
        scan_rows += 1;
        let b = r["bucket"].as_u64().context("scan bucket")? as u32;
        let Ok((w, _)) = ident(s(&r, "url")) else { continue };
        scan_families.insert(s(&r, "family_key").to_owned());
        if b < lo { fw_train.insert(w) } else { fw_reserved.insert(w) };
    }
    // Sealed FineWeb evaluation splits of the new-domain extension.
    let mut sealed: HashMap<String, BTreeSet<&'static str>> = HashMap::new();
    let mut sealed_units = 0u64;
    for sp in NEW_DOMAIN_EVAL_SPLITS {
        for r in io::jsonl_rows(&new_domains.join(sp).join("manifest.jsonl"))? {
            let r = r?;
            if s(&r, "domain") != "fineweb" {
                continue;
            }
            let url = r["source_unit"]["url"].as_str().context("sealed FineWeb unit without url")?;
            sealed.entry(ident(url)?.0).or_default().insert(sp);
            sealed_units += 1;
        }
    }
    // Train-side FineWeb pool rows of the extension must be scan pages (so the scan covers their writers).
    let mut v3_train_side_fineweb = (0u64, 0u64);
    for f in raw["fineweb_train_side_manifests"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        for r in io::jsonl_rows(Path::new(f))? {
            let r = r?;
            if s(&r, "domain") == "fineweb" {
                v3_train_side_fineweb.0 += 1;
                if !scan_families.contains(s(&r, "source_group")) {
                    v3_train_side_fineweb.1 += 1;
                }
            }
        }
    }
    drop(scan_families);
    let mut protected: HashSet<String> = HashSet::new();
    for f in &protected_files {
        for r in io::jsonl_rows(f)? {
            protected.insert(s(&r?, "id").to_owned());
        }
    }

    // -------------------------------------------------- held rows of the base: URLs from the pool
    let base_held: Vec<Value> = io::jsonl_rows(&base.join("held").join("manifest.jsonl"))?.collect::<Result<_>>()?;
    let held_fw: HashSet<&str> = base_held.iter().filter(|r| s(r, "collection") == FW).map(|r| s(r, "id")).collect();
    let mut held_url: HashMap<String, String> = HashMap::new();
    for r in io::jsonl_rows(&pool)? {
        let r = r?;
        if held_fw.contains(s(&r, "id")) {
            held_url.insert(s(&r, "id").to_owned(), s(&r["provenance"], "source_url").to_owned());
        }
    }
    ensure!(held_url.len() == held_fw.len(), "pool lacks {} held FineWeb blog rows", held_fw.len() - held_url.len());

    // -------------------------------------------------- pass 1: Train-side rows
    let mut entries: HashMap<String, Entry> = HashMap::new();
    let mut rule_counts: BTreeMap<String, BTreeMap<&'static str, u64>> = BTreeMap::new();
    let mut rekeyed_from: BTreeMap<String, u64> = BTreeMap::new();
    let mut hosted_mismatch = 0u64;
    for (d, sp) in [("train", 0u8), ("train-mining", 1u8)] {
        for r in io::jsonl_rows(&base.join(d).join("documents.jsonl"))? {
            let r = r?;
            if s(&r, "collection") != FW {
                continue;
            }
            let (w, rule) = ident(s(&r["provenance"], "source_url"))?;
            let b = &r["provenance"]["blog"];
            if b["hosted_platform"] == true && s(b, "blog_key") != w {
                hosted_mismatch += 1;
            }
            *rule_counts.entry(d.to_owned()).or_default().entry(rule).or_default() += 1;
            if w != s(&r, "writer_key") {
                *rekeyed_from.entry(s(&r, "writer_key").to_owned()).or_default() += 1;
            }
            let id = s(&r, "id").to_owned();
            entries.insert(id.clone(), Entry { split: sp, writer: w, rule, rank: rank(&seed, "blog_unit_document", &id), protected: protected.contains(&id) });
        }
    }
    ensure!(hosted_mismatch == 0, "{hosted_mismatch} hosted blog rows whose writer identity differs from their blog key");
    // Train-side closure against the FineWeb evaluation scope.
    let mut new_split: HashMap<String, &'static str> = HashMap::new();
    let mut new_held: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut protected_held: Vec<String> = Vec::new();
    for (id, e) in &entries {
        if fw_reserved.contains(&e.writer) || sealed.contains_key(&e.writer) {
            new_split.insert(id.clone(), "held:contributor_in_fineweb_evaluation_scope");
            *new_held.entry("contributor_in_fineweb_evaluation_scope").or_default() += 1;
            if e.protected {
                protected_held.push(id.clone());
            }
        }
    }
    // Caps per writer.
    let mut by_writer: HashMap<&str, Vec<&str>> = HashMap::new();
    for (id, e) in &entries {
        if !new_split.contains_key(id) {
            by_writer.entry(e.writer.as_str()).or_default().push(id.as_str());
        }
    }
    let (mut demoted, mut protected_over_cap) = (0u64, 0u64);
    for ids in by_writer.values_mut() {
        ids.sort_by_key(|id| {
            let e = &entries[*id];
            (!e.protected, e.split, e.rank, id.to_string())
        });
        let (mut t, mut m) = (0u64, 0u64);
        for id in ids.iter() {
            let e = &entries[*id];
            let to: &'static str = if e.protected {
                if (e.split == 0 && t >= tcap) || t + m >= mcap {
                    protected_over_cap += 1;
                }
                if e.split == 0 { "train" } else { "train_mining" }
            } else if e.split == 0 && t < tcap && t + m < mcap {
                "train"
            } else if t + m < mcap {
                if e.split == 0 {
                    demoted += 1;
                }
                "train_mining"
            } else {
                *new_held.entry("over_writer_cap_after_rekey").or_default() += 1;
                "held:over_writer_cap_after_rekey"
            };
            match to {
                "train" => t += 1,
                "train_mining" => m += 1,
                _ => {}
            }
            new_split.insert((*id).to_owned(), to);
        }
    }

    // -------------------------------------------------- pass 2: write Train side and held
    let assignment = |sp: &str| {
        json!({"rule": "read only splits-v2-blogs/v3 train/documents.jsonl or train-mining/documents.jsonl; pool files are not training inputs",
               "schema": crate::splits::DOCUMENT_SCHEMA, "seed": seed, "split": sp, "successor_of": base,
               "writer_key": "per-blog writer identity (blogs::writer_identity); split_group_key is the base key whose bucket placed the row"})
    };
    let mut files: BTreeMap<String, Value> = BTreeMap::new();
    let mut writers: [HashMap<String, u64>; 2] = Default::default();
    let mut train_side_writers: HashSet<String> = HashSet::new();
    let mut held_rows: Vec<Value> = Vec::new();
    let mut moved: BTreeMap<String, u64> = BTreeMap::new();
    std::fs::create_dir_all(out.join("train"))?;
    std::fs::create_dir_all(out.join("train-mining"))?;
    let mut docs = [JsonlWriter::create(&out.join("train").join("documents.jsonl"))?, JsonlWriter::create(&out.join("train-mining").join("documents.jsonl"))?];
    let mut mans = [JsonlWriter::create(&out.join("train").join("manifest.jsonl"))?, JsonlWriter::create(&out.join("train-mining").join("manifest.jsonl"))?];
    for (d, from) in [("train", "train"), ("train-mining", "train_mining")] {
        for r in io::jsonl_rows(&base.join(d).join("documents.jsonl"))? {
            let mut v = r?;
            let id = s(&v, "id").to_owned();
            let to: &str = if s(&v, "collection") == FW {
                let e = &entries[&id];
                let old = v["writer_key"].clone();
                v["split_group_key"] = old;
                v["writer_key"] = json!(e.writer);
                v["writer_key_rule"] = json!(e.rule);
                train_side_writers.insert(e.writer.clone());
                new_split[&id]
            } else {
                from
            };
            if to != from {
                *moved.entry(format!("{from}->{to}")).or_default() += 1;
            }
            if let Some(reason) = to.strip_prefix("held:") {
                held_rows.push(json!({"id": v["id"], "collection": v["collection"], "source_group": v["source_group"], "writer_key": v["writer_key"],
                                      "split_group_key": v.get("split_group_key").cloned().unwrap_or(Value::Null), "reason": reason, "base_split": from}));
                continue;
            }
            let k = if to == "train" { 0 } else { 1 };
            v["frozen_split"] = json!(to);
            v["split_assigned"] = json!(true);
            v["split_assignment"] = assignment(to);
            *writers[k].entry(s(&v, "writer_key").to_owned()).or_default() += 1;
            docs[k].write(&v)?;
            mans[k].write(&json!({"id": v["id"], "collection": v["collection"], "domain": v["domain"], "source_group": v["source_group"],
                                  "writer_key": v["writer_key"], "split_group_key": v.get("split_group_key").cloned().unwrap_or(Value::Null),
                                  "split": to, "text_sha256": v["text_sha256"], "word_count": v["word_count"]}))?;
        }
    }
    let [d0, d1] = docs;
    let [m0, m1] = mans;
    for (dir, d, m) in [("train", d0, m0), ("train-mining", d1, m1)] {
        let (a, n) = d.finish()?;
        let (b, _) = m.finish()?;
        files.insert(format!("{dir}/documents.jsonl"), json!({"sha256": a, "rows": n}));
        files.insert(format!("{dir}/manifest.jsonl"), json!({"sha256": b, "rows": n}));
    }
    // Base held rows, rekeyed, then the new holds.
    let mut held_reasons: BTreeMap<String, u64> = BTreeMap::new();
    std::fs::create_dir_all(out.join("held"))?;
    let mut hw = JsonlWriter::create(&out.join("held").join("manifest.jsonl"))?;
    drop(held_fw);
    for mut r in base_held.into_iter() {
        if s(&r, "collection") == FW {
            let id = s(&r, "id").to_owned();
            let (w, rule) = ident(&held_url[&id])?;
            r["split_group_key"] = r["writer_key"].clone();
            r["writer_key"] = json!(w.clone());
            r["writer_key_rule"] = json!(rule);
            train_side_writers.insert(w);
        }
        r["base_split"] = json!("held");
        *held_reasons.entry(format!("{}:{}", s(&r, "reason"), if s(&r, "collection") == FW { "fineweb_blogs" } else { "plos_blogs" })).or_default() += 1;
        hw.write(&r)?;
    }
    for r in &held_rows {
        *held_reasons.entry(format!("{}:fineweb_blogs", s(r, "reason"))).or_default() += 1;
        hw.write(r)?;
    }
    let (hs, hn) = hw.finish()?;
    files.insert("held/manifest.jsonl".into(), json!({"sha256": hs, "rows": hn}));

    // -------------------------------------------------- reservation: regroup by writer, closure
    let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut res_rules: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut res_base_groups: HashSet<String> = HashSet::new();
    let mut res_units = 0u64;
    for u in io::jsonl_rows(&base.join("evaluation-reserved").join("fineweb-blogs").join("units.jsonl"))? {
        let mut u = u?;
        ensure!(u["prose_opened"] == false && u.get("text").is_none_or(Value::is_null), "reserved unit with prose");
        let (w, rule) = ident(s(&u, "url"))?;
        *res_rules.entry(rule).or_default() += 1;
        res_base_groups.insert(s(&u, "group_key").to_owned());
        u["split_group_key"] = u["group_key"].clone();
        u["group_key"] = json!(w);
        u["writer_key_rule"] = json!(rule);
        u["bucket_basis"] = json!("split_group_key");
        groups.entry(w).or_default().push(u);
        res_units += 1;
    }
    let rdir = out.join("evaluation-reserved").join("fineweb-blogs");
    std::fs::create_dir_all(&rdir)?;
    let mut gw = JsonlWriter::create(&rdir.join("groups.jsonl"))?;
    let mut uw = JsonlWriter::create(&rdir.join("units.jsonl"))?;
    let mut bw = JsonlWriter::create(&rdir.join("blocked-groups.jsonl"))?;
    let mut fams: Vec<String> = Vec::new();
    let mut ordered: Vec<(&String, &Vec<Value>)> = groups.iter().collect();
    ordered.sort_by_key(|(g, _)| (rank(&seed, "blog_reserved_group", g), (*g).clone()));
    let (mut blocked_units, mut linked_sealed, mut in_reserved_scope, mut multi_base) = (0u64, 0u64, 0u64, 0u64);
    let mut blocked_by: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut order = 0usize;
    for (g, us) in ordered {
        let mut us: Vec<&Value> = us.iter().collect();
        us.sort_by_key(|u| rank(&seed, "blog_reserved_unit", s(u, "family_key")));
        let bases: BTreeSet<&str> = us.iter().map(|u| s(u, "split_group_key")).collect();
        let buckets: BTreeSet<u64> = us.iter().filter_map(|u| u["bucket"].as_u64()).collect();
        ensure!(buckets.iter().all(|&b| b >= lo as u64), "reserved unit outside the reserved buckets in group {g}");
        if bases.len() > 1 {
            multi_base += 1;
        }
        let reason = if train_side_writers.contains(g) {
            Some("writer_has_fineweb_blog_pool_text")
        } else if fw_train.contains(g) {
            Some("writer_has_fineweb_scan_page_below_reserved_range")
        } else {
            None
        };
        let families: Vec<&str> = us.iter().map(|u| s(u, "family_key")).collect();
        if let Some(reason) = reason {
            blocked_units += us.len() as u64;
            *blocked_by.entry(reason).or_default() += 1;
            bw.write(&json!({"schema": crate::splits::GROUP_SCHEMA, "domain": "fineweb_blogs", "group_key": g, "split_group_keys": bases,
                             "units": us.len(), "family_keys": families, "blocked": reason, "prose_status": "never_opened"}))?;
            continue;
        }
        let linked: Vec<&str> = sealed.get(g).map(|x| x.iter().copied().collect()).unwrap_or_default();
        if !linked.is_empty() {
            linked_sealed += 1;
        }
        let scope = fw_reserved.contains(g);
        if scope {
            in_reserved_scope += 1;
        }
        let platforms: BTreeSet<&str> = us.iter().map(|u| s(u, "blog_platform")).collect();
        let rules: BTreeSet<&str> = us.iter().map(|u| s(u, "blog_rule")).collect();
        let dumps: BTreeSet<&str> = us.iter().map(|u| s(u, "dump")).collect();
        gw.write(&json!({"schema": crate::splits::GROUP_SCHEMA, "domain": "fineweb_blogs", "group_key": g, "split_group_keys": bases,
                         "bucket": buckets.iter().next(), "bucket_basis": "split_group_key", "group_order": order,
                         "units": us.len(), "blog_platforms": platforms, "blog_rules": rules, "dumps": dumps,
                         "linked_fineweb_evaluation_splits": linked, "writer_in_fineweb_reserved_scope": scope,
                         "allocation": "unallocated; Test, CAL, Dev-select and Dev-iter allocation belongs to the split owner; a writer listed in linked_fineweb_evaluation_splits must stay in that split",
                         "prose_status": "never_opened"}))?;
        for (k, u) in us.iter().enumerate() {
            let mut v = (*u).clone();
            v["group_order"] = json!(order);
            v["unit_order_in_group"] = json!(k);
            uw.write(&v)?;
            fams.push(s(u, "family_key").to_owned());
        }
        order += 1;
    }
    let (gs, gn) = gw.finish()?;
    let (us_sha, un) = uw.finish()?;
    let (bs, bn) = bw.finish()?;
    let fam_text = fams.iter().map(|f| format!("{f}\n")).collect::<String>();
    std::fs::write(rdir.join("families.txt"), &fam_text)?;
    files.insert("evaluation-reserved/fineweb-blogs/groups.jsonl".into(), json!({"sha256": gs, "rows": gn}));
    files.insert("evaluation-reserved/fineweb-blogs/units.jsonl".into(), json!({"sha256": us_sha, "rows": un}));
    files.insert("evaluation-reserved/fineweb-blogs/blocked-groups.jsonl".into(), json!({"sha256": bs, "rows": bn}));
    files.insert("evaluation-reserved/fineweb-blogs/families.txt".into(), json!({"sha256": text::sha_hex(fam_text.as_bytes()), "rows": fams.len()}));

    // -------------------------------------------------- summary
    let stats = |c: &HashMap<String, u64>| {
        let n: u64 = c.values().sum();
        let inv = if n == 0 { 0.0 } else { 1.0 / c.values().map(|&k| (k as f64 / n as f64).powi(2)).sum::<f64>() };
        json!({"docs": n, "writers": c.len(), "max_docs_per_writer": c.values().copied().max().unwrap_or(0),
               "inverse_simpson_effective_writers": (inv * 10.0).round() / 10.0})
    };
    let fw_only = |c: &HashMap<String, u64>| -> HashMap<String, u64> { c.iter().filter(|(k, _)| k.starts_with("fineweb_")).map(|(k, v)| (k.clone(), *v)).collect() };
    let mut both: HashMap<String, u64> = fw_only(&writers[0]);
    for (k, v) in fw_only(&writers[1]) {
        *both.entry(k).or_default() += v;
    }
    let mut top_rekeyed: Vec<(String, u64)> = rekeyed_from.into_iter().collect();
    top_rekeyed.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let rekeyed_docs: u64 = top_rekeyed.iter().map(|x| x.1).sum();
    let summary = json!({
        "schema": SUMMARY_SCHEMA,
        "finished_utc": io::now_utc(),
        "config": raw,
        "config_sha256": io::file_sha(config)?.0,
        "base_read_only": base,
        "base_sha256sums_sha256": base_sums,
        "inputs": {
            "fineweb_scan": {"path": scan, "sha256": io::file_sha(&scan)?.0, "rows": scan_rows, "writers_below_reserved_range": fw_train.len(), "writers_in_reserved_range": fw_reserved.len()},
            "sealed_fineweb_evaluation": {"dir": new_domains, "units": sealed_units, "writers": sealed.len()},
            "fineweb_train_side_manifests": {"fineweb_rows": v3_train_side_fineweb.0, "rows_not_in_scan": v3_train_side_fineweb.1},
            "psl": {"path": psl_path, "sha256": io::file_sha(&psl_path)?.0},
            "fineweb_blogs_pool": {"path": pool, "held_rows_resolved": held_url.len()},
            "protected_passages": {"files": protected_files, "ids": protected.len()},
        },
        "rekey": {
            "train_side_rules": rule_counts,
            "train_side_docs_with_new_writer_key": rekeyed_docs,
            "train_side_base_writer_keys_replaced": top_rekeyed.iter().map(|(k, v)| json!({"base_writer_key": k, "docs": v})).collect::<Vec<_>>(),
            "reserved_unit_rules": res_rules,
            "reserved_units": res_units,
            "reserved_base_groups": res_base_groups.len(),
        },
        "train_side": {
            "moves": moved,
            "new_holds": new_held,
            "train_rows_moved_to_train_mining": demoted,
            "protected_rows_over_cap_kept": protected_over_cap,
            "protected_rows_held": protected_held,
            "fineweb_blogs": {"train": stats(&fw_only(&writers[0])), "train_mining": stats(&fw_only(&writers[1])), "train_side": stats(&both)},
        },
        "held_reasons": held_reasons,
        "reservation": {
            "groups": gn, "units": un, "blocked_groups": bn, "blocked_units": blocked_units, "blocked_by": blocked_by,
            "groups_merging_several_base_groups": multi_base, "groups_linked_to_sealed_fineweb_evaluation": linked_sealed,
            "groups_whose_writer_is_in_fineweb_reserved_scope": in_reserved_scope, "prose_opened": false,
            "rule": "base reserved units regrouped by writer identity; a group whose writer has Train-side text is blocked",
        },
        "rules": {
            "writer_key": "blogs::writer_identity of the page URL: hosted-platform blog (user subdomain or first path segments), Blogger country domain folded to <name>.blogspot.com, else the registered domain",
            "sides": "no row changes side; split_group_key is the base key whose bucket placed it",
            "train_side_closure": "FineWeb blog Train-side rows whose writer has a FineWeb pool scan page in the reserved range or a sealed FineWeb evaluation unit are held",
            "caps": format!("per writer at most {tcap} Train rows and {mcap} Train-side rows; protected rows first, then Train, then seeded rank; extra Train rows move to Train-mining while the total allows, else are held"),
            "reservation_closure": "reserved groups are blocked when their writer has any FineWeb blog pool row or a FineWeb pool scan page below the reserved range",
        },
        "files": files,
        "limits": [
            "Writer identity is URL based: a blog on a custom domain and its platform subdomain, or one person's several blogs, remain separate writers.",
            "Multi-author blog networks on their own registered domain (one site key for many writers) are unchanged.",
            "The FineWeb pool scan is a page sample; writers of unsampled FineWeb pages are not visible to the closure.",
            "The reservation is metadata only; prose was never opened and no evaluation split is allocated here.",
            "Base held rows keep their base reason; rows the base held over a cap computed under a platform-wide key (Blogger country domains) are not restored.",
        ],
    });
    let mut sums = String::new();
    for (k, v) in &files {
        sums.push_str(&format!("{}  {}\n", s(v, "sha256"), k));
    }
    std::fs::write(out.join("SHA256SUMS"), sums)?;
    io::write_json(&out.join("summary.json"), &summary)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn jsonl(p: &Path, rows: &[Value]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, rows.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
    }
    fn fw(id: &str, writer: &str, url: &str, hosted: Option<&str>) -> Value {
        json!({"id": id, "collection": FW, "domain": "web_blog", "source_group": format!("fineweb_page:{id}"), "writer_key": writer,
               "text": "t", "text_sha256": "h", "word_count": 200,
               "provenance": {"source_url": url, "blog": {"hosted_platform": hosted.is_some(), "blog_key": hosted.unwrap_or(writer)}}})
    }
    fn unit(fam: &str, group: &str, bucket: u64, url: &str) -> Value {
        json!({"family_key": fam, "group_key": group, "bucket": bucket, "url": url, "prose_opened": false, "text_sha256": null,
               "blog_platform": "x", "blog_rule": "y", "dump": "CC-MAIN-2013-20"})
    }
    #[test]
    fn rekeys_per_blog_keeps_sides_caps_and_closes_reservations() {
        let t = std::env::temp_dir().join(format!("blog-rekey-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let base = t.join("v2");
        let plos = json!({"id": "p1", "collection": crate::plos_blogs::COLLECTION, "domain": "web_blog", "source_group": "plos:1",
                          "writer_key": "plos_blog_writer:x", "text": "t", "text_sha256": "h", "word_count": 200});
        jsonl(&base.join("train").join("documents.jsonl"), &[
            fw("r1", "fineweb_site:wordpress.com", "https://cats.wordpress.com/2010/01/02/a-post-here/", Some("fineweb_blog:cats.wordpress.com")),
            fw("r2", "fineweb_site:blogspot.co.uk", "http://anna.blogspot.co.uk/2012/07/x.html", None),
            fw("r3", "fineweb_site:blogspot.co.uk", "http://anna.blogspot.co.uk/2012/08/y.html", None),
            fw("r4", "fineweb_site:anna.blogspot.com", "http://anna.blogspot.com/2012/09/z.html", None),
            plos.clone(),
        ]);
        jsonl(&base.join("train-mining").join("documents.jsonl"), &[
            fw("r5", "fineweb_site:blogspot.de", "http://bob.blogspot.de/2012/07/x.html", None),
            fw("r6", "fineweb_site:anna.blogspot.com", "http://anna.blogspot.com/2013/01/w.html", None),
        ]);
        for f in ["train", "train-mining"] {
            std::fs::write(base.join(f).join("manifest.jsonl"), "").unwrap();
        }
        jsonl(&base.join("held").join("manifest.jsonl"), &[json!({"id": "h1", "collection": FW, "source_group": "fineweb_page:h1", "writer_key": "fineweb_site:typepad.com", "reason": "over_writer_cap"})]);
        let r = base.join("evaluation-reserved").join("fineweb-blogs");
        jsonl(&r.join("units.jsonl"), &[
            unit("u1", "fineweb_site:over-blog.com", 971, "https://eve.over-blog.com/article-1.html"),
            unit("u2", "fineweb_site:x9", 950, "https://cats.wordpress.com/2011/01/02/other-post/"),
            unit("u3", "fineweb_site:x8", 950, "http://dan.typepad.com/blog/2009/01/q.html"),
            unit("u4", "fineweb_site:over-blog.com", 971, "https://zed.over-blog.com/article-2.html"),
            unit("u5", "fineweb_site:x7", 950, "http://kim.livejournal.com/1.html"),
            unit("u6", "fineweb_site:over-blog.com", 971, "https://eve.over-blog.com/article-3.html"),
        ]);
        std::fs::write(r.join("groups.jsonl"), "").unwrap();
        std::fs::write(r.join("families.txt"), "").unwrap();
        let mut sums = String::new();
        for f in ["train/documents.jsonl", "train/manifest.jsonl", "train-mining/documents.jsonl", "train-mining/manifest.jsonl", "held/manifest.jsonl",
                  "evaluation-reserved/fineweb-blogs/units.jsonl", "evaluation-reserved/fineweb-blogs/groups.jsonl"] {
            sums.push_str(&format!("{}  {f}\n", io::file_sha(&base.join(f)).unwrap().0));
        }
        std::fs::write(base.join("SHA256SUMS"), &sums).unwrap();
        jsonl(&t.join("pool.jsonl"), &[json!({"id": "h1", "provenance": {"source_url": "http://dan.typepad.com/blog/2008/01/p.html"}}), json!({"id": "zz", "provenance": {}})]);
        jsonl(&t.join("scan.jsonl"), &[
            json!({"family_key": "fineweb_page:s1", "bucket": 950, "url": "http://bob.blogspot.com/2010/01/a.html"}),
            json!({"family_key": "fineweb_page:s2", "bucket": 1, "url": "http://kim.livejournal.com/2.html"}),
        ]);
        let nd = t.join("nd");
        jsonl(&nd.join("test").join("manifest.jsonl"), &[json!({"domain": "fineweb", "source_unit": {"url": "https://zed.over-blog.com/another.html"}}),
                                                          json!({"domain": "global_voices", "source_unit": {}})]);
        for sp in ["cal", "dev-select", "dev-iter"] {
            jsonl(&nd.join(sp).join("manifest.jsonl"), &[]);
        }
        jsonl(&t.join("v3-train.jsonl"), &[json!({"domain": "fineweb", "source_group": "fineweb_page:s2"}), json!({"domain": "fineweb", "source_group": "fineweb_page:nope"})]);
        jsonl(&t.join("passages.jsonl"), &[json!({"id": "r6"})]);
        std::fs::write(t.join("psl.dat"), "com\nde\nuk\nco.uk\norg\nblogspot.com\n").unwrap();
        let cfg = json!({"seed": "s", "base": base, "out": t.join("v3"), "fineweb_scan": t.join("scan.jsonl"), "fineweb_blogs_pool": t.join("pool.jsonl"),
                         "new_domains": nd, "psl": t.join("psl.dat"), "reserved_buckets": [900, 1000], "train_max_docs_per_writer": 2,
                         "mining_max_docs_per_writer": 3, "protected_passages": [t.join("passages.jsonl")], "fineweb_train_side_manifests": [t.join("v3-train.jsonl")]});
        std::fs::write(t.join("cfg.json"), cfg.to_string()).unwrap();
        let s = run(&t.join("cfg.json")).unwrap();
        let ts = &s["train_side"];
        assert_eq!(ts["new_holds"]["contributor_in_fineweb_evaluation_scope"], 1, "{s:#}");
        // anna (three Train rows under two base keys, one protected Train-mining row): the protected row is kept first,
        // two Train rows stay and the third would exceed the total cap.
        assert_eq!(ts["new_holds"]["over_writer_cap_after_rekey"], 1, "{s:#}");
        assert_eq!(ts["train_rows_moved_to_train_mining"], 0);
        assert_eq!(s["inputs"]["fineweb_train_side_manifests"]["rows_not_in_scan"], 1);
        let rows: Vec<Value> = io::jsonl_rows(&t.join("v3").join("train").join("documents.jsonl")).unwrap().map(Result::unwrap).collect();
        let key = |id: &str| rows.iter().find(|r| r["id"] == id).map(|r| (super::s(r, "writer_key").to_owned(), super::s(r, "split_group_key").to_owned()));
        assert_eq!(key("r1"), Some(("fineweb_blog:cats.wordpress.com".into(), "fineweb_site:wordpress.com".into())));
        let anna: Vec<(String, String)> = ["r2", "r3", "r4"].iter().filter_map(|i| key(i)).collect();
        assert_eq!(anna.len(), 2);
        assert!(anna.iter().all(|(w, _)| w == "fineweb_site:anna.blogspot.com"));
        assert!(anna.iter().all(|(_, g)| g == "fineweb_site:blogspot.co.uk" || g == "fineweb_site:anna.blogspot.com"));
        assert_eq!(key("p1").map(|k| k.0), Some("plos_blog_writer:x".into()));
        let held: Vec<Value> = io::jsonl_rows(&t.join("v3").join("held").join("manifest.jsonl")).unwrap().map(Result::unwrap).collect();
        assert_eq!(held[0]["writer_key"], "fineweb_blog:dan.typepad.com");
        assert_eq!(held.len(), 3);
        // Reservation: eve (u1+u6) and zed (linked to sealed Test) stay; cats, dan and kim are blocked.
        let res = &s["reservation"];
        assert_eq!((res["groups"].as_u64(), res["units"].as_u64(), res["blocked_groups"].as_u64()), (Some(2), Some(3), Some(3)), "{s:#}");
        let groups: Vec<Value> = io::jsonl_rows(&t.join("v3").join("evaluation-reserved").join("fineweb-blogs").join("groups.jsonl")).unwrap().map(Result::unwrap).collect();
        let zed = groups.iter().find(|g| g["group_key"] == "fineweb_blog:zed.over-blog.com").unwrap();
        assert_eq!(zed["linked_fineweb_evaluation_splits"], json!(["test"]));
        assert!(groups.iter().any(|g| g["group_key"] == "fineweb_blog:eve.over-blog.com" && g["units"] == 2));
        // A changed base file is refused.
        std::fs::write(base.join("held").join("manifest.jsonl"), "").unwrap();
        let mut c2 = cfg.clone();
        c2["out"] = json!(t.join("v3b"));
        std::fs::write(t.join("cfg2.json"), c2.to_string()).unwrap();
        assert!(run(&t.join("cfg2.json")).is_err());
        std::fs::remove_dir_all(&t).unwrap();
    }
}

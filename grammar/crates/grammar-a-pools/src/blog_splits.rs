//! Blog split sibling of a base split directory (`splits-v2`): Train and
//! Train-mining rows from the admitted FineWeb blog and PLOS blog pools,
//! and a metadata-only evaluation reservation of FineWeb blog groups from
//! the reserved buckets. The base split directory is only read.
//!
//! * **Units.** A connected unit joins rows that share a blog (`writer_key`)
//!   or a dedup `split_component`; a unit is assigned whole.
//! * **Train side.** FineWeb blog rows must have `bucket(writer_key)` below
//!   the reserved range and a writer key that no reservation lists. PLOS
//!   blog posts whose writer (folded display name) is an author of a
//!   reserved PLOS evaluation group of the base split, or whose family is a
//!   base evaluation family, are held. Units are visited in seeded rank
//!   order per domain. Inside a unit the documents are visited in seeded
//!   rank order: up to `train_max_docs_per_writer` per writer (the blog for
//!   FineWeb blog rows, so a hosted blog that inherited a platform-wide
//!   split group keeps its own cap) go to Train while the
//!   domain's Train quota lasts, then up to `mining_max_docs_per_writer`
//!   (counting Train) go to Train-mining; the rest are held
//!   (`over_writer_cap`).
//! * **Reservation.** Metadata rows of the FineWeb blog scan with eligible
//!   metadata, grouped by group key (blog or inherited site), in the
//!   reserved buckets. Groups and units are listed with seeded order keys;
//!   no prose was opened and no evaluation split is allocated here.
use crate::{
    adapter::bucket,
    io::{self, JsonlWriter},
    splits::{self, EVAL_SPLITS, rank},
    text,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
};

pub const SUMMARY_SCHEMA: &str = "slop_ninja_a_competitive_blog_split_summary_v1";
pub const BLOG_DOMAINS: [&str; 2] = ["fineweb_blogs", "plos_blogs"];

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn domain_of(r: &Value) -> Result<usize> {
    Ok(match s(r, "collection") {
        crate::fineweb_blogs::COLLECTION => 0,
        crate::plos_blogs::COLLECTION => 1,
        c => bail!("row {} of collection {c} is not a blog source", s(r, "id")),
    })
}

struct Uf(Vec<usize>);
impl Uf {
    fn find(&mut self, x: usize) -> usize {
        let mut r = x;
        while self.0[r] != r {
            r = self.0[r];
        }
        let mut y = x;
        while self.0[y] != r {
            let n = self.0[y];
            self.0[y] = r;
            y = n;
        }
        r
    }
    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a] = b;
        }
    }
}

/// Register/genre mix of rows (counts only).
fn mix(rows: &[&Value]) -> Value {
    let mut m: BTreeMap<&str, BTreeMap<String, u64>> = BTreeMap::new();
    for r in rows {
        let p = &r["provenance"];
        let u = &r["extraction"]["unit"];
        let mut add = |k: &'static str, v: String| *m.entry(k).or_default().entry(v).or_default() += 1;
        add("register_lexicon_label", s(u, "register_lexicon_label").to_owned());
        add("voice", s(&u["first_person"], "voice").to_owned());
        let fp = u["first_person"]["rate"].as_f64().unwrap_or(0.0);
        add("first_person_rate", (if fp < 0.02 { "<0.02" } else if fp < 0.04 { "0.02-0.04" } else if fp < 0.06 { "0.04-0.06" } else { ">=0.06" }).to_owned());
        let w = r["word_count"].as_u64().unwrap_or(0);
        add("word_count", (if w < 200 { "150-199" } else if w < 250 { "200-249" } else if w < 300 { "250-299" } else { "300-450" }).to_owned());
        add("year", s(&r["date"], "value").get(..4).unwrap_or("").to_owned());
        if s(r, "collection") == crate::fineweb_blogs::COLLECTION {
            add("blog_rule", s(&p["blog"], "rule").to_owned());
            add("blog_platform", s(&p["blog"], "platform").to_owned());
            add("dump", s(p, "dump").to_owned());
        } else {
            add("plos_blog", s(p, "blog").to_owned());
        }
    }
    json!(m)
}

/// Cap key: the blog for FineWeb blog rows (a hosted blog that inherited a
/// platform-wide split group is still its own writer), else the writer key.
fn cap_key(r: &Value) -> &str {
    r["provenance"]["blog"]["blog_key"].as_str().unwrap_or_else(|| s(r, "writer_key"))
}

fn writers(rows: &[&Value]) -> Value {
    let mut c: HashMap<&str, u64> = HashMap::new();
    for r in rows {
        *c.entry(cap_key(r)).or_default() += 1;
    }
    let n: u64 = c.values().sum();
    let inv_simpson = if n == 0 { 0.0 } else { 1.0 / c.values().map(|&k| (k as f64 / n as f64).powi(2)).sum::<f64>() };
    let max = c.values().copied().max().unwrap_or(0);
    json!({"docs": n, "writers": c.len(), "inverse_simpson_effective_writers": (inv_simpson * 10.0).round() / 10.0, "max_docs_per_writer": max})
}

pub fn run(config: &Path) -> Result<Value> {
    let raw: Value = serde_json::from_slice(&std::fs::read(config).with_context(|| format!("read {}", config.display()))?)?;
    let p = |k: &str| -> Result<PathBuf> { Ok(PathBuf::from(raw[k].as_str().with_context(|| format!("config {k}"))?)) };
    let seed = raw["seed"].as_str().context("seed")?.to_owned();
    let (out, base, scan) = (p("out")?, p("base")?, p("fineweb_blogs_scan")?);
    let pools = [p("fineweb_blogs_pool")?, p("plos_blogs_pool")?];
    let plos_scan = p("plos_scan")?;
    let lo = raw["reserved_buckets"][0].as_u64().context("reserved_buckets")? as u32;
    ensure!(raw["reserved_buckets"][1] == 1000 && lo > 0 && lo < 1000, "reserved_buckets must be [LO, 1000]");
    let quota = [raw["train_quota"]["fineweb_blogs"].as_u64().context("quota")?, raw["train_quota"]["plos_blogs"].as_u64().context("quota")?];
    let cap = |k: &str, d: &str| raw[k][d].as_u64().with_context(|| format!("config {k}.{d}"));
    let tcaps = [cap("train_max_docs_per_writer", "fineweb_blogs")?, cap("train_max_docs_per_writer", "plos_blogs")?];
    let mcaps = [cap("mining_max_docs_per_writer", "fineweb_blogs")?, cap("mining_max_docs_per_writer", "plos_blogs")?];
    ensure!(mcaps[0] >= tcaps[0] && mcaps[1] >= tcaps[1], "mining caps include Train documents");
    io::fresh_dir(&out)?;

    // -------------------------------------------------- base split (read only)
    let mut eval_families: HashSet<String> = HashSet::new();
    let mut plos_author_sha: HashSet<String> = HashSet::new();
    let mut plos_unassigned: HashSet<String> = HashSet::new();
    for sp in EVAL_SPLITS {
        let dir = base.join(sp.replace('_', "-"));
        for l in std::fs::read_to_string(dir.join("families.txt"))?.lines() {
            eval_families.insert(l.trim().to_owned());
        }
        for g in io::jsonl_rows(&dir.join("groups.jsonl"))? {
            let g = g?;
            if g["domain"] == "plos" {
                for h in g["contributor_key_sha256"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    plos_author_sha.insert(h.to_owned());
                }
            }
        }
    }
    for g in io::jsonl_rows(&base.join("reserved-unassigned").join("groups.jsonl"))? {
        let g = g?;
        for f in g["family_keys"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            eval_families.insert(f.to_owned());
            if g["domain"] == "plos" {
                plos_unassigned.insert(f.to_owned());
            }
        }
    }
    for r in io::jsonl_rows(&plos_scan.join("reserved-metadata.jsonl"))? {
        let r = r?;
        if plos_unassigned.contains(s(&r, "family_key")) {
            for a in r["authors"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if let Some(k) = splits::author_key(a) {
                    plos_author_sha.insert(text::sha_hex(k.as_bytes()));
                }
            }
        }
    }

    // -------------------------------------------------- reservation from the metadata-only scan
    let mut res_groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let (mut scan_rows, mut scan_ineligible) = (0u64, 0u64);
    for r in io::jsonl_rows(&scan.join("reserved-metadata.jsonl"))? {
        let r = r?;
        scan_rows += 1;
        ensure!(r.get("text").is_none() && r["prose_opened"] == false, "scan row with prose");
        let g = s(&r, "group_key").to_owned();
        ensure!(bucket(&g) >= lo, "scan group {g} outside the reserved buckets");
        if r["eligible_metadata"] != true {
            scan_ineligible += 1;
            continue;
        }
        res_groups.entry(g).or_default().push(r);
    }
    let reserved: HashSet<&str> = res_groups.keys().map(String::as_str).collect();

    // -------------------------------------------------- Train-side rows
    let mut rows: Vec<Value> = Vec::new();
    let mut pool_inputs = Vec::new();
    for (d, path) in pools.iter().enumerate() {
        let (sha, bytes) = io::file_sha(path)?;
        let mut n = 0u64;
        for r in io::jsonl_rows(path)? {
            let r = r?;
            ensure!(domain_of(&r)? == d, "row {} in the wrong pool", s(&r, "id"));
            ensure!(s(&r, "pool_status") == "admitted", "row {} not admitted", s(&r, "id"));
            rows.push(r);
            n += 1;
        }
        pool_inputs.push(json!({"domain": BLOG_DOMAINS[d], "path": path, "sha256": sha, "bytes": bytes, "rows": n}));
    }
    let mut held: Vec<(usize, String)> = Vec::new();
    let mut live: Vec<usize> = Vec::new();
    let mut ids: HashSet<String> = HashSet::new();
    for (i, r) in rows.iter().enumerate() {
        ensure!(ids.insert(s(r, "id").to_owned()), "duplicate id {}", s(r, "id"));
        let wk = s(r, "writer_key");
        ensure!(!wk.is_empty(), "row {} without writer_key", s(r, "id"));
        let d = domain_of(r)?;
        if d == 0 {
            ensure!(bucket(wk) < lo, "FineWeb blog row {} has a reserved-bucket writer key", s(r, "id"));
            ensure!(!reserved.contains(wk), "FineWeb blog row {} has a reserved group", s(r, "id"));
        }
        if eval_families.contains(s(r, "source_group")) {
            held.push((i, "base_evaluation_family".into()));
            continue;
        }
        if d == 1 {
            let blocked = r["provenance"]["authors"].as_array().into_iter().flatten().filter_map(Value::as_str)
                .filter_map(splits::author_key).any(|k| plos_author_sha.contains(&text::sha_hex(k.as_bytes())));
            if blocked {
                held.push((i, "plos_author_of_reserved_evaluation_group".into()));
                continue;
            }
        }
        live.push(i);
    }
    // Connected units over writer keys and split components.
    let mut uf = Uf((0..rows.len()).collect());
    let mut first_by: HashMap<String, usize> = HashMap::new();
    for &i in &live {
        let r = &rows[i];
        for k in [format!("w\0{}", s(r, "writer_key")), format!("c\0{}", s(&r["pool_admission"], "split_component"))] {
            if k.ends_with('\0') {
                continue;
            }
            match first_by.get(&k) {
                Some(&j) => uf.union(i, j),
                None => {
                    first_by.insert(k, i);
                }
            }
        }
    }
    let mut units: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &i in &live {
        let r = uf.find(i);
        units.entry(r).or_default().push(i);
    }
    let mut ordered: [Vec<(u64, Vec<usize>)>; 2] = Default::default();
    for (_, mut members) in units {
        let d = domain_of(&rows[members[0]])?;
        ensure!(members.iter().all(|&i| domain_of(&rows[i]).ok() == Some(d)), "unit spans blog domains");
        let key: BTreeSet<&str> = members.iter().map(|&i| s(&rows[i], "writer_key")).collect();
        let ukey = key.into_iter().collect::<Vec<_>>().join("|");
        members.sort_by_key(|&i| rank(&seed, "blog_unit_document", s(&rows[i], "id")));
        ordered[d].push((rank(&seed, &format!("blog_unit:{}", BLOG_DOMAINS[d]), &ukey), members));
    }
    let mut split_of: Vec<Option<&'static str>> = vec![None; rows.len()];
    let mut train_used = [0u64; 2];
    let mut multi_writer_units = [0u64; 2];
    for d in 0..2 {
        let (tcap, mcap) = (tcaps[d], mcaps[d]);
        ordered[d].sort_by_key(|(k, _)| *k);
        for (_, members) in &ordered[d] {
            let mut per_writer: HashMap<&str, (u64, u64)> = HashMap::new();
            let ws: HashSet<&str> = members.iter().map(|&i| cap_key(&rows[i])).collect();
            if ws.len() > 1 {
                multi_writer_units[d] += 1;
            }
            // Train decision is per unit so that a unit's Train rows never exceed the quota mid-unit.
            let unit_train_capacity: u64 = ws.len() as u64 * tcap;
            let take_train = train_used[d] + unit_train_capacity.min(members.len() as u64) <= quota[d];
            for &i in members {
                let w = per_writer.entry(cap_key(&rows[i])).or_default();
                if take_train && w.0 < tcap {
                    w.0 += 1;
                    train_used[d] += 1;
                    split_of[i] = Some("train");
                } else if w.0 + w.1 < mcap {
                    w.1 += 1;
                    split_of[i] = Some("train_mining");
                } else {
                    held.push((i, "over_writer_cap".into()));
                }
            }
        }
    }

    // -------------------------------------------------- write
    let assignment = |sp: &str| {
        json!({"rule": "read only splits-v2-blogs train/documents.jsonl or train-mining/documents.jsonl; pool files are not training inputs",
               "schema": splits::DOCUMENT_SCHEMA, "seed": seed, "split": sp, "sibling_of": base})
    };
    let mut files: BTreeMap<String, Value> = BTreeMap::new();
    let mut by_split: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for (sp, dir) in [("train", "train"), ("train_mining", "train-mining")] {
        std::fs::create_dir_all(out.join(dir))?;
        let mut docs = JsonlWriter::create(&out.join(dir).join("documents.jsonl"))?;
        let mut man = JsonlWriter::create(&out.join(dir).join("manifest.jsonl"))?;
        let mut list = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            if split_of[i] != Some(sp) {
                continue;
            }
            let mut v = r.clone();
            v["frozen_split"] = json!(sp);
            v["split_assigned"] = json!(true);
            v["split_assignment"] = assignment(sp);
            docs.write(&v)?;
            man.write(&json!({"id": r["id"], "collection": r["collection"], "domain": r["domain"], "source_group": r["source_group"],
                              "writer_key": r["writer_key"], "split": sp, "text_sha256": r["text_sha256"], "word_count": r["word_count"]}))?;
            list.push(r);
        }
        let (a, n) = docs.finish()?;
        let (b, _) = man.finish()?;
        files.insert(format!("{dir}/documents.jsonl"), json!({"sha256": a, "rows": n}));
        files.insert(format!("{dir}/manifest.jsonl"), json!({"sha256": b, "rows": n}));
        by_split.insert(sp, list);
    }
    std::fs::create_dir_all(out.join("held"))?;
    let mut hw = JsonlWriter::create(&out.join("held").join("manifest.jsonl"))?;
    let mut held_reasons: BTreeMap<String, BTreeMap<&str, u64>> = BTreeMap::new();
    held.sort_by_key(|(i, _)| *i);
    for (i, reason) in &held {
        let r = &rows[*i];
        hw.write(&json!({"id": r["id"], "collection": r["collection"], "source_group": r["source_group"], "writer_key": r["writer_key"], "reason": reason}))?;
        *held_reasons.entry(reason.clone()).or_default().entry(BLOG_DOMAINS[domain_of(r)?]).or_default() += 1;
    }
    let (hs, hn) = hw.finish()?;
    files.insert("held/manifest.jsonl".into(), json!({"sha256": hs, "rows": hn}));

    let rdir = out.join("evaluation-reserved").join("fineweb-blogs");
    std::fs::create_dir_all(&rdir)?;
    let mut gw = JsonlWriter::create(&rdir.join("groups.jsonl"))?;
    let mut uw = JsonlWriter::create(&rdir.join("units.jsonl"))?;
    let mut fams: Vec<String> = Vec::new();
    let mut groups_sorted: Vec<(&String, &Vec<Value>)> = res_groups.iter().collect();
    groups_sorted.sort_by_key(|(g, _)| rank(&seed, "blog_reserved_group", g));
    let mut res_mix: BTreeMap<&str, BTreeMap<String, u64>> = BTreeMap::new();
    for (order, (g, us)) in groups_sorted.iter().enumerate() {
        let mut us: Vec<&Value> = us.iter().collect();
        us.sort_by_key(|u| rank(&seed, "blog_reserved_unit", s(u, "family_key")));
        let platforms: BTreeSet<&str> = us.iter().map(|u| s(u, "blog_platform")).collect();
        let rules: BTreeSet<&str> = us.iter().map(|u| s(u, "blog_rule")).collect();
        let dumps: BTreeSet<&str> = us.iter().map(|u| s(u, "dump")).collect();
        for u in &us {
            *res_mix.entry("blog_platform").or_default().entry(s(u, "blog_platform").to_owned()).or_default() += 1;
            *res_mix.entry("blog_rule").or_default().entry(s(u, "blog_rule").to_owned()).or_default() += 1;
            *res_mix.entry("dump").or_default().entry(s(u, "dump").to_owned()).or_default() += 1;
        }
        gw.write(&json!({"schema": splits::GROUP_SCHEMA, "domain": "fineweb_blogs", "group_key": g, "bucket": bucket(g), "group_order": order,
                         "units": us.len(), "blog_platforms": platforms, "blog_rules": rules, "dumps": dumps,
                         "allocation": "unallocated; Test, CAL, Dev-select and Dev-iter allocation belongs to the split owner",
                         "prose_status": "never_opened"}))?;
        for (k, u) in us.iter().enumerate() {
            let mut v = (*u).clone();
            v["schema"] = json!(splits::RESERVATION_SCHEMA);
            v["domain"] = json!("fineweb_blogs");
            v["group_order"] = json!(order);
            v["unit_order_in_group"] = json!(k);
            v["text_sha256"] = json!(null);
            uw.write(&v)?;
            fams.push(s(u, "family_key").to_owned());
        }
    }
    let (gs, gn) = gw.finish()?;
    let (us_sha, un) = uw.finish()?;
    let fam_text = fams.iter().map(|f| format!("{f}\n")).collect::<String>();
    std::fs::write(rdir.join("families.txt"), &fam_text)?;
    files.insert("evaluation-reserved/fineweb-blogs/groups.jsonl".into(), json!({"sha256": gs, "rows": gn}));
    files.insert("evaluation-reserved/fineweb-blogs/units.jsonl".into(), json!({"sha256": us_sha, "rows": un}));
    files.insert("evaluation-reserved/fineweb-blogs/families.txt".into(), json!({"sha256": text::sha_hex(fam_text.as_bytes()), "rows": fams.len()}));

    let mut per_domain = serde_json::Map::new();
    for (d, name) in BLOG_DOMAINS.iter().enumerate() {
        let pick = |sp: &str| -> Vec<&Value> { by_split[sp].iter().copied().filter(|r| domain_of(r).ok() == Some(d)).collect() };
        let (t, m) = (pick("train"), pick("train_mining"));
        let both: Vec<&Value> = t.iter().chain(m.iter()).copied().collect();
        per_domain.insert((*name).into(), json!({
            "train": writers(&t), "train_mining": writers(&m), "train_side": writers(&both),
            "train_quota": quota[d], "units": ordered[d].len(), "multi_writer_units": multi_writer_units[d],
            "mix_train_side": mix(&both),
        }));
    }
    let summary = json!({
        "schema": SUMMARY_SCHEMA,
        "finished_utc": io::now_utc(),
        "config": raw,
        "config_sha256": io::file_sha(config)?.0,
        "base_split_read_only": base,
        "pools": pool_inputs,
        "per_domain": per_domain,
        "held_reasons": held_reasons,
        "reservation": {"scan": scan, "scan_rows": scan_rows, "ineligible_metadata_rows": scan_ineligible, "groups": gn, "units": un,
                        "mix": res_mix, "reserved_buckets": [lo, 1000], "prose_opened": false,
                        "rule": "eligible metadata rows of the FineWeb blog metadata-only scan, grouped by group key; never rendered"},
        "base_evaluation_families": eval_families.len(),
        "plos_reserved_author_keys": plos_author_sha.len(),
        "rules": {
            "units": "rows joined by writer_key or split_component are assigned whole",
            "train": format!("units in seeded rank order per domain; a unit enters Train only if all its capped Train rows fit the quota; at most {tcaps:?} Train documents per writer (fineweb_blogs, plos_blogs)"),
            "train_mining": format!("remaining documents up to {mcaps:?} per writer including Train (fineweb_blogs, plos_blogs); the rest held over_writer_cap"),
            "fineweb_blog_buckets": format!("every FineWeb blog Train-side writer key has bucket < {lo}; reserved groups have bucket >= {lo}"),
            "plos_blogs": "Train side only; posts by authors of reserved PLOS evaluation groups (folded names) are held",
        },
        "files": files,
        "limits": [
            "Blog register is identified from URLs; the first-person screen and register lexicon are heuristics, and the lexicon label is a descriptive tally only.",
            "Every human label is a historical (pre-cutoff) proxy, not an authenticated unaided writing history.",
            "Hosted-platform blogs inherit the FineWeb scan's platform-wide site group only when one of their pages is in that scan; later FineWeb pool extensions must apply the same inheritance.",
            "This directory is a sibling of the base split; it does not modify or re-seal it, and its reservation is not yet allocated to evaluation splits.",
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
    fn key_in(prefix: &str, lo: u32, hi: u32, skip: usize) -> String {
        (0..).map(|i| format!("{prefix}{i}")).filter(|k| (lo..hi).contains(&bucket(k))).nth(skip).unwrap()
    }
    fn row(id: &str, coll: &str, writer: &str, comp: &str, authors: &[&str]) -> Value {
        json!({"id": id, "collection": coll, "domain": "x", "source_group": format!("fam:{id}"), "writer_key": writer, "pool_status": "admitted",
               "pool_admission": {"split_component": comp}, "word_count": 200, "date": "2012-01-01", "text": "t", "text_sha256": "h",
               "provenance": {"authors": authors, "blog": {"rule": "blogspot", "platform": "blogspot", "blog_key": writer}, "dump": "CC-MAIN-2013-20"},
               "extraction": {"unit": {"register_lexicon_label": "general_personal", "first_person": {"voice": "first_person_singular", "rate": 0.05}}}})
    }
    #[test]
    fn assigns_whole_units_caps_writers_holds_reserved_authors_and_reserves_scan_groups() {
        let t = std::env::temp_dir().join(format!("blog-splits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let base = t.join("base");
        let reserved_author = splits::author_key("Jane Roe").unwrap();
        for sp in ["test", "cal", "dev-select", "dev-iter"] {
            std::fs::create_dir_all(base.join(sp)).unwrap();
            std::fs::write(base.join(sp).join("families.txt"), if sp == "test" { "fam:f9\n" } else { "" }).unwrap();
            let g = if sp == "cal" { json!({"domain": "plos", "group_key": "doi:e1", "contributor_key_sha256": [text::sha_hex(reserved_author.as_bytes())]}) } else { json!({"domain": "wikipedia", "group_key": "w"}) };
            jsonl(&base.join(sp).join("groups.jsonl"), &[g]);
        }
        jsonl(&base.join("reserved-unassigned").join("groups.jsonl"), &[]);
        jsonl(&t.join("plos-scan").join("reserved-metadata.jsonl"), &[]);
        let fw = crate::fineweb_blogs::COLLECTION;
        let pl = crate::plos_blogs::COLLECTION;
        let (a, b) = (key_in("fineweb_site:a", 0, 900, 0), key_in("fineweb_site:b", 0, 900, 0));
        let fw_rows: Vec<Value> = (0..5).map(|i| row(&format!("f{i}"), fw, &a, &format!("c{i}"), &[]))
            .chain([row("f5", fw, &b, "c0", &[]), row("f9", fw, &b, "c9", &[])]).collect();
        jsonl(&t.join("fw").join("admitted.jsonl"), &fw_rows);
        jsonl(&t.join("pl").join("admitted.jsonl"), &[row("p1", pl, "plos_blog_writer:x", "d1", &["A Writer"]), row("p2", pl, "plos_blog_writer:jane_roe", "d2", &["Jane Roe"])]);
        let g = key_in("fineweb_blog:r", 900, 1000, 0);
        jsonl(&t.join("scan").join("reserved-metadata.jsonl"), &[
            json!({"family_key": "fineweb_page:u1", "group_key": g, "eligible_metadata": true, "prose_opened": false, "blog_platform": "wordpress_com", "blog_rule": "hosted_platform", "dump": "CC-MAIN-2013-20"}),
            json!({"family_key": "fineweb_page:u2", "group_key": g, "eligible_metadata": false, "prose_opened": false}),
        ]);
        let cfg = json!({"seed": "s", "out": t.join("out"), "base": base, "fineweb_blogs_scan": t.join("scan"), "plos_scan": t.join("plos-scan"),
                         "fineweb_blogs_pool": t.join("fw").join("admitted.jsonl"), "plos_blogs_pool": t.join("pl").join("admitted.jsonl"),
                         "reserved_buckets": [900, 1000], "train_quota": {"fineweb_blogs": 100, "plos_blogs": 100},
                         "train_max_docs_per_writer": {"fineweb_blogs": 2, "plos_blogs": 2}, "mining_max_docs_per_writer": {"fineweb_blogs": 4, "plos_blogs": 4}});
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join("cfg.json"), cfg.to_string()).unwrap();
        let s = run(&t.join("cfg.json")).unwrap();
        // f0..f5 form one unit (writer a joined to writer b through component c0): 2+1 Train, then mining up to 4 per writer.
        let d = &s["per_domain"]["fineweb_blogs"];
        assert_eq!(d["train"]["docs"], 3);
        assert_eq!(d["train_mining"]["docs"], 2);
        assert_eq!(s["held_reasons"]["over_writer_cap"]["fineweb_blogs"], 1);
        assert_eq!(s["held_reasons"]["base_evaluation_family"]["fineweb_blogs"], 1);
        assert_eq!(s["held_reasons"]["plos_author_of_reserved_evaluation_group"]["plos_blogs"], 1);
        assert_eq!(s["per_domain"]["plos_blogs"]["train"]["docs"], 1);
        assert_eq!(s["reservation"]["groups"], 1);
        assert_eq!(s["reservation"]["units"], 1);
        // A Train-side FineWeb blog row in the reserved buckets is an error.
        let bad = t.join("fw2");
        jsonl(&bad.join("admitted.jsonl"), &[row("z", fw, &g, "cz", &[])]);
        let mut c2 = cfg.clone();
        c2["out"] = json!(t.join("out2"));
        c2["fineweb_blogs_pool"] = json!(bad.join("admitted.jsonl"));
        std::fs::write(t.join("cfg2.json"), c2.to_string()).unwrap();
        assert!(run(&t.join("cfg2.json")).is_err());
        std::fs::remove_dir_all(&t).unwrap();
    }
}

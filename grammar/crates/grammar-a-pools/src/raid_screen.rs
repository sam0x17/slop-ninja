//! The pre-registered RAID acquisition screen
//! (`eval-acquisition-raid-screen-v1.json`, schema
//! `slop_ninja_a_eval_acquisition_raid_lineage_screen_v1`), applied inside
//! evaluation acquisition. It is the application of the protocol's
//! `splits.rules` ("every evaluation group has no contributor with
//! Train-side text") and `excerpts.dedup` to the RAID Train cohort of the
//! released bundle, whose rows carry no Wikipedia page id or Reddit author
//! key.
//!
//! - S1 (`raid_train_title_lineage`): a reserved unit whose
//!   `source_unit.title_sha256` is a RAID Train title hash leaves with the
//!   rest of its group. Decided from reservation metadata and hashes alone,
//!   before any prose is rendered; the screened family list is what the
//!   adapter renders.
//! - S2 (`raid_train_reddit_lineage`): every Webis-TLDR-17 post of a
//!   reserved Reddit author group is compared with the RAID Train rows
//!   (whole-text normalized and folded SHA-256, folded 50-word sequences)
//!   and its submission title SHA-256 with the RAID Train titles; any match
//!   skips the whole author group.
//! - S3 (`raid_train_text_lineage`): a reserved unit whose excerpt shares a
//!   whole-text normalized or folded hash or a folded 50-word sequence with
//!   a RAID Train row is skipped.
//!
//! The screens read no prediction, label or score (label-blind), print no
//! text and write text-free receipts. S2 and S3 read evaluation prose and
//! therefore refuse Test unless the written protocol seal is supplied.
use crate::{
    forum,
    io::{self, JsonlWriter},
    text,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    io::BufRead,
    path::{Path, PathBuf},
};

pub const RECORD_SCHEMA: &str = "slop_ninja_a_eval_acquisition_raid_lineage_screen_v1";
pub const SKIP_SCHEMA: &str = "slop_ninja_a_raid_screen_skip_v1";
pub const SUMMARY_SCHEMA: &str = "slop_ninja_a_raid_screen_summary_v1";
pub const S1_REASON: &str = "raid_train_title_lineage";
pub const S2_REASON: &str = "raid_train_reddit_lineage";
pub const S3_REASON: &str = "raid_train_text_lineage";
pub const BASIS: &str = "application of protocol splits.rules (every evaluation group has no contributor with Train-side text) and excerpts.dedup (prior index, Train-side pools and other splits) to the RAID Train cohort of bundle 18b3682973982a3f, whose rows carry no page id or author key; rules S1-S3 of eval-acquisition-raid-screen-v1.json implemented as written; label-blind";

/// The written protocol seal: `path` must hash to `sha256` and say
/// `"sealed": true`. Returns the receipt recorded by every Test step.
pub fn require_seal(path: Option<&Path>, sha256: Option<&str>) -> Result<Value> {
    let (Some(p), Some(want)) = (path, sha256) else {
        bail!("Test needs --seal <SEAL.json> and --seal-sha256 <hex> (the written protocol seal)");
    };
    let (got, bytes) = io::file_sha(p).with_context(|| format!("seal {}", p.display()))?;
    ensure!(got == want.to_ascii_lowercase(), "seal {} hashes to {got}, not {want}", p.display());
    let v: Value = serde_json::from_slice(&std::fs::read(p)?).context("seal is not JSON")?;
    ensure!(v["sealed"] == true, "seal {} does not say \"sealed\": true", p.display());
    Ok(json!({"path": p, "sha256": got, "bytes": bytes, "sealed": true}))
}

fn check_file(p: &Path, want: &str) -> Result<String> {
    let (got, _) = io::file_sha(p).with_context(|| format!("hash {}", p.display()))?;
    ensure!(got == want, "{} hashes to {got}, not the recorded {want}", p.display());
    Ok(got)
}

/// The adopted screen record and its bound inputs.
pub struct Record {
    pub path: PathBuf,
    pub sha256: String,
    pub v: Value,
}

impl Record {
    pub fn load(p: &Path) -> Result<Record> {
        let (sha, _) = io::file_sha(p)?;
        let v: Value = serde_json::from_slice(&std::fs::read(p)?)?;
        ensure!(v["schema"] == RECORD_SCHEMA, "not a RAID screen record: {}", p.display());
        let ids: Vec<&str> = v["rules"].as_array().into_iter().flatten().filter_map(|r| r["id"].as_str()).collect();
        ensure!(ids == ["S1_title", "S2_reddit_author", "S3_text"], "record rules {ids:?}");
        Ok(Record { path: p.to_owned(), sha256: sha, v })
    }

    fn input(&self, k: &str) -> Result<(PathBuf, String)> {
        let i = &self.v["inputs"][k];
        let p = i["path"].as_str().or(i["trainer_bundle"].as_str()).with_context(|| format!("record input {k}"))?;
        let s = i["sha256"].as_str().or(i["rows_sha256"].as_str()).with_context(|| format!("record input {k} sha256"))?;
        Ok((PathBuf::from(p), s.to_owned()))
    }

    pub fn receipt(&self) -> Value {
        json!({"path": self.path, "sha256": self.sha256, "schema": RECORD_SCHEMA, "basis": BASIS})
    }

    /// RAID Train title hashes (S1, S2).
    pub fn titles(&self) -> Result<(HashSet<String>, Value)> {
        let (p, want) = self.input("raid_train_title_sha256")?;
        check_file(&p, &want)?;
        let set: HashSet<String> = std::fs::read_to_string(&p)?.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_ascii_lowercase).collect();
        ensure!(set.iter().all(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())), "title list holds a non-sha256 line");
        if let Some(n) = self.v["inputs"]["raid_train_title_sha256"]["entries"].as_u64() {
            ensure!(set.len() as u64 == n, "title list has {} entries, record says {n}", set.len());
        }
        Ok((set.clone(), json!({"path": p, "sha256": want, "entries": set.len()})))
    }

    /// RAID Train rows of the bundle (S2, S3): `rows.jsonl` rows whose
    /// cohort_id starts with `raid`.
    pub fn raid_rows(&self) -> Result<(RaidKeys, Value)> {
        let i = &self.v["inputs"]["raid_train_rows"];
        let dir = PathBuf::from(i["trainer_bundle"].as_str().context("trainer_bundle")?);
        let rows_p = dir.join("rows.jsonl");
        let want = i["rows_sha256"].as_str().context("rows_sha256")?;
        check_file(&rows_p, want)?;
        let sel = i["selector"].as_str().unwrap_or("");
        ensure!(sel == "cohort_id starts with 'raid'", "unexpected selector {sel}");
        let mut k = RaidKeys::default();
        for row in io::jsonl_rows(&rows_p)? {
            let r = row?;
            if !r["cohort_id"].as_str().unwrap_or("").starts_with("raid") {
                continue;
            }
            ensure!(r["split"] == "train", "RAID row outside Train: {}", r["id"]);
            k.add(r["text"].as_str().context("RAID row text")?);
        }
        if let Some(n) = i["rows"].as_u64() {
            ensure!(k.rows == n, "bundle has {} RAID rows, record says {n}", k.rows);
        }
        let receipt = json!({"rows_path": rows_p, "rows_sha256": want, "selector": sel, "raid_rows": k.rows,
                              "normalized_hashes": k.normalized.len(), "folded_hashes": k.folded.len(), "folded_50_word_sequences": k.shingles.len()});
        Ok((k, receipt))
    }
}

/// Whole-text and 50-word keys of the RAID Train rows.
#[derive(Default)]
pub struct RaidKeys {
    pub rows: u64,
    pub normalized: HashSet<String>,
    pub folded: HashSet<String>,
    pub shingles: HashSet<u64>,
}

impl RaidKeys {
    pub fn add(&mut self, t: &str) {
        self.rows += 1;
        self.normalized.insert(text::normalized_sha(t));
        self.folded.insert(text::folded_sha(t));
        self.shingles.extend(text::shingles_folded(t));
    }

    /// Match kinds of one text: (normalized, folded, shared folded 50-word sequences).
    pub fn matches(&self, t: &str) -> (bool, bool, u64) {
        let s = text::shingles_folded(t).into_iter().filter(|h| self.shingles.contains(h)).count() as u64;
        (self.normalized.contains(&text::normalized_sha(t)), self.folded.contains(&text::folded_sha(t)), s)
    }
}

/// One reservation directory (SEALED.json, groups.jsonl, manifest.jsonl,
/// families.txt), checked against its seal record.
pub struct Reservation {
    pub name: String,
    pub dir: PathBuf,
    pub split: String,
    pub sealed_sha256: String,
}

impl Reservation {
    pub fn open(name: &str, dir: &Path) -> Result<Reservation> {
        let sp = dir.join("SEALED.json");
        let sealed: Value = serde_json::from_slice(&std::fs::read(&sp).with_context(|| format!("{}", sp.display()))?)?;
        for (f, k) in [("groups.jsonl", "groups_sha256"), ("manifest.jsonl", "manifest_sha256")] {
            let (got, _) = io::file_sha(&dir.join(f))?;
            ensure!(sealed[k] == got.as_str(), "{} differs from {}", dir.join(f).display(), sp.display());
        }
        Ok(Reservation { name: name.to_owned(), dir: dir.to_owned(), split: sealed["split"].as_str().context("SEALED.split")?.to_owned(), sealed_sha256: io::file_sha(&sp)?.0 })
    }
}

pub fn parse_named(s: &str) -> Result<(String, PathBuf)> {
    let (n, p) = s.split_once('=').context("expected NAME=DIR")?;
    ensure!(!n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "bad name {n}");
    Ok((n.to_owned(), PathBuf::from(p)))
}

/// S1 over reservation metadata: skip records for every unit of a group
/// with a RAID Train title hash, and per reservation the family list with
/// those groups removed (original order kept). Reads no prose.
pub fn s1(record: &Record, reservations: &[(String, PathBuf)], out: &Path) -> Result<Value> {
    let (titles, titles_receipt) = record.titles()?;
    io::fresh_dir(out)?;
    let mut skip_w = JsonlWriter::create(&out.join("s1-skips.jsonl"))?;
    let mut per = Vec::new();
    let mut all_groups = Vec::new();
    for (name, dir) in reservations {
        let res = Reservation::open(name, dir)?;
        let mut matched: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
        let mut units: Vec<Value> = Vec::new();
        for row in io::jsonl_rows(&dir.join("manifest.jsonl"))? {
            let r = row?;
            ensure!(r["split"] == res.split.as_str(), "manifest row of another split in {}", dir.display());
            if let Some(t) = r["source_unit"]["title_sha256"].as_str() {
                if titles.contains(&t.to_ascii_lowercase()) {
                    let k = (r["domain"].as_str().unwrap_or("").to_owned(), r["group_key"].as_str().context("group_key")?.to_owned());
                    matched.entry(k).or_default().insert(t.to_ascii_lowercase());
                }
            }
            units.push(json!({"domain": r["domain"], "group_key": r["group_key"], "family_key": r["family_key"], "title_sha256": r["source_unit"]["title_sha256"]}));
        }
        let groups: HashSet<&str> = matched.keys().map(|(_, g)| g.as_str()).collect();
        let mut removed: HashSet<String> = HashSet::new();
        let mut n_units = 0u64;
        for u in &units {
            let g = u["group_key"].as_str().unwrap_or("");
            if !groups.contains(g) {
                continue;
            }
            let fam = u["family_key"].as_str().context("family_key")?;
            removed.insert(fam.to_owned());
            let hit = u["title_sha256"].as_str().map(|t| titles.contains(&t.to_ascii_lowercase())).unwrap_or(false);
            skip_w.write(&json!({"schema": SKIP_SCHEMA, "rule": "S1_title", "reason": S1_REASON, "reservation": name, "split": res.split,
                "domain": u["domain"], "group_key": g, "family_key": fam,
                "unit_title_matched": hit, "matched_title_sha256": if hit { u["title_sha256"].clone() } else { Value::Null },
                "decided_from": "reservation metadata source_unit.title_sha256 and the RAID Train title hash list only; no prose read"}))?;
            n_units += 1;
        }
        // Screened family list in the original order.
        let fams_in = dir.join("families.txt");
        let (fsha, _) = io::file_sha(&fams_in)?;
        std::fs::create_dir_all(out.join(name))?;
        let fams_out = out.join(name).join("families.txt");
        let mut kept = String::new();
        let (mut n_in, mut n_out) = (0u64, 0u64);
        for l in std::fs::read_to_string(&fams_in)?.lines() {
            let l = l.trim();
            if l.is_empty() {
                continue;
            }
            n_in += 1;
            if removed.contains(l) {
                continue;
            }
            kept.push_str(l);
            kept.push('\n');
            n_out += 1;
        }
        ensure!(n_in - n_out == removed.len() as u64, "{} lists {} of the {} S1 families", fams_in.display(), n_in - n_out, removed.len());
        std::fs::write(&fams_out, &kept)?;
        let (osha, _) = io::file_sha(&fams_out)?;
        let gl: Vec<Value> = matched.iter().map(|((d, g), t)| json!({"domain": d, "group_key": g, "matched_title_sha256": t})).collect();
        all_groups.extend(matched.iter().map(|((_, g), _)| json!({"reservation": name, "split": res.split, "group_key": g})));
        per.push(json!({"name": name, "dir": dir, "split": res.split, "sealed_sha256": res.sealed_sha256, "manifest_units": units.len(),
            "families_in": {"path": fams_in, "sha256": fsha, "rows": n_in}, "families_screened": {"path": fams_out, "sha256": osha, "rows": n_out},
            "matched_groups": gl, "skipped_units": n_units}));
    }
    let (skip_sha, skip_n) = skip_w.finish()?;
    let summary = json!({"schema": SUMMARY_SCHEMA, "rule": "S1_title", "reason": S1_REASON, "finished_utc": io::now_utc(), "record": record.receipt(),
        "titles": titles_receipt, "reservations": per, "matched_groups": all_groups, "skips_sha256": skip_sha, "skips_rows": skip_n,
        "prose_read": false, "label_blind": true});
    io::write_json(&out.join("summary.json"), &summary)?;
    Ok(summary)
}

/// Text fields of one Webis post compared as whole texts.
const WEBIS_TEXT_FIELDS: [&str; 3] = ["body", "content", "normalizedBody"];

/// S2: every Webis-TLDR-17 post by a reserved Reddit author of the
/// reservation, compared with the RAID Train rows and titles. One receipt
/// per reserved author group (text-free); a skip row per matched group.
pub fn s2(record: &Record, reservation: &(String, PathBuf), zip_path: &Path, zip_sha: &str, seal: &Value, out: &Path) -> Result<Value> {
    let res = Reservation::open(&reservation.0, &reservation.1)?;
    let (titles, titles_receipt) = record.titles()?;
    let (keys, rows_receipt) = record.raid_rows()?;
    check_file(zip_path, zip_sha)?;
    let mut groups: BTreeMap<String, AuthorTally> = BTreeMap::new();
    for row in io::jsonl_rows(&res.dir.join("groups.jsonl"))? {
        let r = row?;
        if r["domain"] == "reddit" {
            let g = r["group_key"].as_str().context("group_key")?;
            ensure!(g.starts_with("reddit_author:"), "Reddit group key {g} is not an author key");
            groups.insert(g.to_owned(), AuthorTally::default());
        }
    }
    ensure!(!groups.is_empty(), "no reserved Reddit author groups in {}", res.dir.display());
    io::fresh_dir(out)?;
    let f = std::fs::File::open(zip_path)?;
    let mut z = zip::ZipArchive::new(f)?;
    let mut names: Vec<String> = (0..z.len()).filter_map(|i| z.by_index(i).ok().map(|e| e.name().to_owned())).filter(|n| n.ends_with(".json") || n.ends_with(".jsonl")).collect();
    names.sort();
    ensure!(!names.is_empty(), "no .json entries in {}", zip_path.display());
    let mut rows_seen = 0u64;
    for name in &names {
        let e = z.by_name(name)?;
        let br = std::io::BufReader::with_capacity(1 << 20, e);
        for line in br.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            rows_seen += 1;
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            let author = v["author"].as_str().unwrap_or("");
            if author.is_empty() || author == "[deleted]" {
                continue;
            }
            let wk = forum::contributor_key("reddit_author", author);
            let Some(t) = groups.get_mut(&wk) else { continue };
            t.posts += 1;
            let mut m = PostMatch::default();
            for k in WEBIS_TEXT_FIELDS {
                if let Some(s) = v[k].as_str() {
                    let (n, fo, sh) = keys.matches(s);
                    m.normalized |= n;
                    m.folded |= fo;
                    m.shingles += sh;
                }
            }
            if let Some(ti) = v["title"].as_str() {
                m.title = titles.contains(&text::sha_hex(ti.as_bytes())) || titles.contains(&text::sha_hex(ti.trim().as_bytes()));
            }
            t.add(&m);
        }
    }
    let mut w = JsonlWriter::create(&out.join("s2-groups.jsonl"))?;
    let mut skip_w = JsonlWriter::create(&out.join("s2-skips.jsonl"))?;
    let (mut matched, mut without_posts) = (0u64, 0u64);
    for (g, t) in &groups {
        let hit = t.matched_posts > 0;
        let rec = json!({"group_key": g, "posts_checked": t.posts, "matched_posts": t.matched_posts,
            "match_kinds": {"normalized_text_sha256": t.normalized, "folded_text_sha256": t.folded, "posts_sharing_folded_50_word_sequence": t.shingle_posts, "submission_title_sha256": t.title}});
        w.write(&rec)?;
        if t.posts == 0 {
            without_posts += 1;
        }
        if hit {
            matched += 1;
            let mut s = rec.clone();
            s["schema"] = json!(SKIP_SCHEMA);
            s["rule"] = json!("S2_reddit_author");
            s["reason"] = json!(S2_REASON);
            s["reservation"] = json!(res.name);
            s["split"] = json!(res.split);
            s["domain"] = json!("reddit");
            skip_w.write(&s)?;
        }
    }
    ensure!(without_posts < groups.len() as u64, "no Webis post found for any reserved author: author keys do not match");
    let (gsha, _) = w.finish()?;
    let (ssha, sn) = skip_w.finish()?;
    let summary = json!({"schema": SUMMARY_SCHEMA, "rule": "S2_reddit_author", "reason": S2_REASON, "finished_utc": io::now_utc(), "record": record.receipt(), "seal": seal,
        "reservation": {"name": res.name, "dir": res.dir, "split": res.split, "sealed_sha256": res.sealed_sha256},
        "webis_zip": {"path": zip_path, "sha256": zip_sha, "json_rows_seen": rows_seen, "entries": names},
        "text_fields_compared": WEBIS_TEXT_FIELDS, "titles": titles_receipt, "raid_rows": rows_receipt,
        "reserved_author_groups": groups.len(), "groups_without_posts": without_posts, "matched_groups": matched,
        "groups_sha256": gsha, "skips_sha256": ssha, "skips_rows": sn, "text_printed": false, "label_blind": true});
    io::write_json(&out.join("summary.json"), &summary)?;
    Ok(summary)
}

#[derive(Default)]
struct PostMatch {
    normalized: bool,
    folded: bool,
    shingles: u64,
    title: bool,
}

#[derive(Default)]
struct AuthorTally {
    posts: u64,
    matched_posts: u64,
    normalized: u64,
    folded: u64,
    shingle_posts: u64,
    title: u64,
}

impl AuthorTally {
    fn add(&mut self, m: &PostMatch) {
        self.normalized += m.normalized as u64;
        self.folded += m.folded as u64;
        self.shingle_posts += (m.shingles > 0) as u64;
        self.title += m.title as u64;
        if m.normalized || m.folded || m.shingles > 0 || m.title {
            self.matched_posts += 1;
        }
    }
}

/// S3: every candidate excerpt of the given adapter outputs against the
/// RAID Train rows. A hit row per matching unit (family), text-free.
pub fn s3(record: &Record, adapters: &[PathBuf], seal: &Value, out: &Path) -> Result<Value> {
    let (keys, rows_receipt) = record.raid_rows()?;
    io::fresh_dir(out)?;
    let mut w = JsonlWriter::create(&out.join("s3-hits.jsonl"))?;
    let mut inputs = Vec::new();
    let mut families_hit: HashMap<String, u64> = HashMap::new();
    let (mut checked, mut hits) = (0u64, 0u64);
    for a in adapters {
        let rep: Value = serde_json::from_slice(&std::fs::read(a.join("report.json")).with_context(|| format!("{}/report.json", a.display()))?)?;
        ensure!(rep["complete"] == true, "incomplete adapter report {}", a.display());
        let cp = a.join("candidates.jsonl");
        let (csha, _) = io::file_sha(&cp)?;
        if let Some(want) = rep["candidates_sha256"].as_str() {
            ensure!(want == csha, "candidates changed in {}", a.display());
        }
        let mut n = 0u64;
        for row in io::jsonl_rows(&cp)? {
            let r = row?;
            let t = r["text"].as_str().context("candidate text")?;
            n += 1;
            let (nm, fm, sh) = keys.matches(t);
            if nm || fm || sh > 0 {
                hits += 1;
                let fam = r["source_group"].as_str().context("source_group")?.to_owned();
                *families_hit.entry(fam.clone()).or_default() += 1;
                w.write(&json!({"schema": SKIP_SCHEMA, "rule": "S3_text", "reason": S3_REASON, "id": r["id"], "family_key": fam, "collection": r["collection"],
                    "text_sha256": r["text_sha256"], "match_kinds": {"normalized_text_sha256": nm, "folded_text_sha256": fm, "shared_folded_50_word_sequences": sh}}))?;
            }
        }
        checked += n;
        inputs.push(json!({"dir": a, "candidates_sha256": csha, "candidates": n, "adapter": rep["adapter"], "collection": rep["collection"]}));
    }
    let (hsha, hn) = w.finish()?;
    let summary = json!({"schema": SUMMARY_SCHEMA, "rule": "S3_text", "reason": S3_REASON, "finished_utc": io::now_utc(), "record": record.receipt(), "seal": seal,
        "raid_rows": rows_receipt, "adapters": inputs, "candidates_checked": checked, "hit_rows": hits, "families_hit": families_hit.len(),
        "hits_sha256": hsha, "hits_rows": hn, "unit_rule": "a unit (family) with any matching excerpt is skipped whole", "text_printed": false, "label_blind": true});
    io::write_json(&out.join("summary.json"), &summary)?;
    Ok(summary)
}

/// Loaded screen outcomes for selection: group keys (S1, S2) and family
/// keys (S3) to skip, with their receipts.
#[derive(Default)]
pub struct Screen {
    pub s1_groups: HashSet<String>,
    pub s2_groups: HashSet<String>,
    pub s3_families: HashSet<String>,
    pub receipts: Vec<Value>,
}

impl Screen {
    /// Load from S1/S2/S3 output directories (each with summary.json and
    /// its skip file), checking the skip file hashes recorded there.
    pub fn load(s1: Option<&Path>, s2: Option<&Path>, s3: Option<&Path>, split: &str) -> Result<Screen> {
        let mut sc = Screen::default();
        for (dir, file, key, rule) in [(s1, "s1-skips.jsonl", "skips_sha256", "S1_title"), (s2, "s2-skips.jsonl", "skips_sha256", "S2_reddit_author"), (s3, "s3-hits.jsonl", "hits_sha256", "S3_text")] {
            let Some(d) = dir else { continue };
            let sum: Value = serde_json::from_slice(&std::fs::read(d.join("summary.json")).with_context(|| format!("{}/summary.json", d.display()))?)?;
            ensure!(sum["schema"] == SUMMARY_SCHEMA && sum["rule"] == rule, "{} is not a {rule} output", d.display());
            let p = d.join(file);
            let (sha, _) = io::file_sha(&p)?;
            ensure!(sum[key] == sha.as_str(), "{} changed", p.display());
            let mut n = 0u64;
            for row in io::jsonl_rows(&p)? {
                let r = row?;
                if let Some(s) = r["split"].as_str() {
                    if s != split {
                        continue;
                    }
                }
                n += 1;
                match rule {
                    "S1_title" => {
                        sc.s1_groups.insert(r["group_key"].as_str().context("group_key")?.to_owned());
                    }
                    "S2_reddit_author" => {
                        sc.s2_groups.insert(r["group_key"].as_str().context("group_key")?.to_owned());
                    }
                    _ => {
                        sc.s3_families.insert(r["family_key"].as_str().context("family_key")?.to_owned());
                    }
                }
            }
            sc.receipts.push(json!({"rule": rule, "dir": d, "file_sha256": sha, "rows_for_split": n, "record": sum["record"]}));
        }
        Ok(sc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_jsonl(p: &Path, rows: &[Value]) -> String {
        let mut w = JsonlWriter::create(p).unwrap();
        for r in rows {
            w.write(r).unwrap();
        }
        w.finish().unwrap().0
    }

    fn words(seed: &str, n: usize) -> String {
        (0..n).map(|i| format!("{seed}{i}")).collect::<Vec<_>>().join(" ")
    }

    /// A synthetic record with a title list, a bundle with RAID and other rows, and one reservation.
    struct Fx {
        d: tempfile::TempDir,
        record: PathBuf,
        res: PathBuf,
        seal: PathBuf,
    }

    fn fixture() -> Fx {
        let d = tempfile::tempdir().unwrap();
        let t = d.path();
        let title_hit = text::sha_hex(b"A Raid Title");
        std::fs::write(t.join("titles.txt"), format!("{title_hit}\n{}\n", text::sha_hex(b"other"))).unwrap();
        let bundle = t.join("bundle");
        std::fs::create_dir_all(&bundle).unwrap();
        let rows = vec![
            json!({"id": "raid-1", "cohort_id": "raid-v1-paired", "split": "train", "text": words("raidhuman", 80)}),
            json!({"id": "raid-2", "cohort_id": "raid-v1-paired", "split": "train", "text": format!("Exact  RAID\ttext {}", words("ex", 30))}),
            json!({"id": "h-1", "cohort_id": "human:x", "split": "train", "text": words("nonraid", 80)}),
        ];
        let rows_sha = write_jsonl(&bundle.join("rows.jsonl"), &rows);
        let (tsha, _) = io::file_sha(&t.join("titles.txt")).unwrap();
        let rec = json!({"schema": RECORD_SCHEMA, "inputs": {
            "raid_train_title_sha256": {"path": t.join("titles.txt"), "sha256": tsha, "entries": 2},
            "raid_train_rows": {"trainer_bundle": bundle, "rows_sha256": rows_sha, "selector": "cohort_id starts with 'raid'", "rows": 2}},
            "rules": [{"id": "S1_title"}, {"id": "S2_reddit_author"}, {"id": "S3_text"}]});
        io::write_json(&t.join("record.json"), &rec).unwrap();
        // Reservation: two wikipedia groups (one with the RAID title), one reddit author group.
        let res = t.join("res");
        std::fs::create_dir_all(&res).unwrap();
        let author = forum::contributor_key("reddit_author", "someone");
        let author2 = forum::contributor_key("reddit_author", "clean_author");
        let units = vec![
            json!({"split": "test", "domain": "wikipedia", "group_key": "wikipedia:page:1", "family_key": "wikipedia:page:1", "source_unit": {"title_sha256": title_hit}}),
            json!({"split": "test", "domain": "wikipedia", "group_key": "wikipedia:page:2", "family_key": "wikipedia:page:2", "source_unit": {"title_sha256": text::sha_hex(b"clean")}}),
            json!({"split": "test", "domain": "instructional", "group_key": "wikibooks:book:b", "family_key": "wikibooks:page:5", "source_unit": {"title_sha256": text::sha_hex(b"x")}}),
            json!({"split": "test", "domain": "instructional", "group_key": "wikibooks:book:b", "family_key": "wikibooks:page:6", "source_unit": {"title_sha256": title_hit}}),
            json!({"split": "test", "domain": "reddit", "group_key": author, "family_key": "reddit_post:t3_a", "source_unit": {}}),
            json!({"split": "test", "domain": "reddit", "group_key": author2, "family_key": "reddit_post:t3_b", "source_unit": {}}),
        ];
        let groups = vec![
            json!({"split": "test", "domain": "wikipedia", "group_key": "wikipedia:page:1"}),
            json!({"split": "test", "domain": "wikipedia", "group_key": "wikipedia:page:2"}),
            json!({"split": "test", "domain": "instructional", "group_key": "wikibooks:book:b"}),
            json!({"split": "test", "domain": "reddit", "group_key": author}),
            json!({"split": "test", "domain": "reddit", "group_key": author2}),
        ];
        let ms = write_jsonl(&res.join("manifest.jsonl"), &units);
        let gs = write_jsonl(&res.join("groups.jsonl"), &groups);
        let fams: String = units.iter().map(|u| format!("{}\n", u["family_key"].as_str().unwrap())).collect();
        std::fs::write(res.join("families.txt"), fams).unwrap();
        io::write_json(&res.join("SEALED.json"), &json!({"split": "test", "groups_sha256": gs, "manifest_sha256": ms})).unwrap();
        let seal = t.join("SEAL.json");
        io::write_json(&seal, &json!({"sealed": true})).unwrap();
        Fx { record: t.join("record.json"), res, seal, d }
    }

    #[test]
    fn s1_skips_whole_groups_from_hashes_and_screens_families() {
        let f = fixture();
        let rec = Record::load(&f.record).unwrap();
        let out = f.d.path().join("s1");
        let s = s1(&rec, &[("v4".into(), f.res.clone())], &out).unwrap();
        assert_eq!(s["skips_rows"], 3);
        let skips: Vec<Value> = io::jsonl_rows(&out.join("s1-skips.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        let fams: BTreeSet<&str> = skips.iter().map(|r| r["family_key"].as_str().unwrap()).collect();
        // The whole wikibooks group leaves, including the page without the title hash.
        assert_eq!(fams, BTreeSet::from(["wikipedia:page:1", "wikibooks:page:5", "wikibooks:page:6"]));
        assert!(skips.iter().all(|r| r["reason"] == S1_REASON));
        assert_eq!(skips.iter().filter(|r| r["unit_title_matched"] == true).count(), 2);
        let kept = std::fs::read_to_string(out.join("v4/families.txt")).unwrap();
        assert_eq!(kept.lines().collect::<Vec<_>>(), vec!["wikipedia:page:2", "reddit_post:t3_a", "reddit_post:t3_b"]);
        let sc = Screen::load(Some(&out), None, None, "test").unwrap();
        assert!(sc.s1_groups.contains("wikipedia:page:1") && sc.s1_groups.contains("wikibooks:book:b") && sc.s1_groups.len() == 2);
        // A different split's screen loads nothing.
        assert!(Screen::load(Some(&out), None, None, "cal").unwrap().s1_groups.is_empty());
    }

    #[test]
    fn s1_refuses_a_changed_title_list() {
        let f = fixture();
        std::fs::write(f.d.path().join("titles.txt"), "00\n").unwrap();
        let rec = Record::load(&f.record).unwrap();
        assert!(s1(&rec, &[("v4".into(), f.res.clone())], &f.d.path().join("s1")).is_err());
    }

    #[test]
    fn seal_is_required_and_checked() {
        let f = fixture();
        assert!(require_seal(None, None).is_err());
        let (sha, _) = io::file_sha(&f.seal).unwrap();
        assert!(require_seal(Some(&f.seal), Some(&sha)).is_ok());
        assert!(require_seal(Some(&f.seal), Some(&"0".repeat(64))).is_err());
        let unsealed = f.d.path().join("U.json");
        io::write_json(&unsealed, &json!({"sealed": false})).unwrap();
        let (u, _) = io::file_sha(&unsealed).unwrap();
        assert!(require_seal(Some(&unsealed), Some(&u)).is_err());
    }

    fn adapter_dir(p: &Path, rows: &[Value]) {
        std::fs::create_dir_all(p).unwrap();
        let s = write_jsonl(&p.join("candidates.jsonl"), rows);
        io::write_json(&p.join("report.json"), &json!({"complete": true, "candidates_sha256": s, "adapter": "x"})).unwrap();
    }

    #[test]
    fn s3_matches_normalized_folded_and_50_word_sequences_only_against_raid_rows() {
        let f = fixture();
        let rec = Record::load(&f.record).unwrap();
        let a = f.d.path().join("adapt");
        let shared = format!("lead in words here {}", words("raidhuman", 55));
        adapter_dir(&a, &[
            json!({"id": "c1", "source_group": "fam:1", "collection": "c", "text": "exact raid TEXT ".to_owned() + &words("ex", 30)}), // folded match
            json!({"id": "c2", "source_group": "fam:2", "collection": "c", "text": "Exact RAID text ".to_owned() + &words("ex", 30)}), // normalized match
            json!({"id": "c3", "source_group": "fam:3", "collection": "c", "text": shared}), // shared 50-word window
            json!({"id": "c4", "source_group": "fam:4", "collection": "c", "text": words("nonraid", 80)}), // only a non-RAID row
            json!({"id": "c5", "source_group": "fam:5", "collection": "c", "text": words("raidhuman", 40)}), // shorter than 50 words: no window
        ]);
        let seal = json!({"sealed": true});
        let out = f.d.path().join("s3");
        let s = s3(&rec, &[a], &seal, &out).unwrap();
        assert_eq!(s["candidates_checked"], 5);
        let hits: Vec<Value> = io::jsonl_rows(&out.join("s3-hits.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        let fams: BTreeSet<&str> = hits.iter().map(|r| r["family_key"].as_str().unwrap()).collect();
        assert_eq!(fams, BTreeSet::from(["fam:1", "fam:2", "fam:3"]));
        let c1 = hits.iter().find(|r| r["id"] == "c1").unwrap();
        assert_eq!(c1["match_kinds"]["folded_text_sha256"], true);
        assert_eq!(c1["match_kinds"]["normalized_text_sha256"], false);
        let c2 = hits.iter().find(|r| r["id"] == "c2").unwrap();
        assert_eq!(c2["match_kinds"]["normalized_text_sha256"], true);
        assert!(hits.iter().all(|r| r.get("text").is_none()));
        let sc = Screen::load(None, None, Some(&out), "test").unwrap();
        assert_eq!(sc.s3_families.len(), 3);
    }

    #[test]
    fn s2_checks_every_post_of_reserved_authors() {
        let f = fixture();
        let rec = Record::load(&f.record).unwrap();
        // Webis-style zip: one author has a post sharing a RAID 50-word window; one has a RAID title;
        // a clean author; and an unreserved author whose post equals a RAID row.
        let zp = f.d.path().join("webis.zip");
        {
            let file = std::fs::File::create(&zp).unwrap();
            let mut z = zip::ZipWriter::new(file);
            z.start_file("corpus-webis-tldr-17.json", zip::write::SimpleFileOptions::default()).unwrap();
            let lines = [
                json!({"author": "someone", "body": format!("intro {}", words("raidhuman", 60)), "content": "x", "title": null, "id": "a"}),
                json!({"author": "clean_author", "body": words("clean", 70), "content": words("clean", 60), "title": "Clean title", "id": "b"}),
                json!({"author": "outsider", "body": words("raidhuman", 80), "title": null, "id": "c"}),
            ];
            for l in lines {
                use std::io::Write;
                writeln!(z, "{l}").unwrap();
            }
            z.finish().unwrap();
        }
        let (zsha, _) = io::file_sha(&zp).unwrap();
        let out = f.d.path().join("s2");
        let s = s2(&rec, &("v4".into(), f.res.clone()), &zp, &zsha, &json!({"sealed": true}), &out).unwrap();
        assert_eq!(s["reserved_author_groups"], 2);
        assert_eq!(s["matched_groups"], 1);
        let skips: Vec<Value> = io::jsonl_rows(&out.join("s2-skips.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(skips.len(), 1);
        assert_eq!(skips[0]["group_key"], forum::contributor_key("reddit_author", "someone"));
        assert_eq!(skips[0]["reason"], S2_REASON);
        // Title rule: give the clean author a RAID title in a second zip.
        let zp2 = f.d.path().join("webis2.zip");
        {
            let file = std::fs::File::create(&zp2).unwrap();
            let mut z = zip::ZipWriter::new(file);
            z.start_file("c.json", zip::write::SimpleFileOptions::default()).unwrap();
            use std::io::Write;
            writeln!(z, "{}", json!({"author": "clean_author", "body": words("clean", 70), "title": " A Raid Title ", "id": "b"})).unwrap();
            z.finish().unwrap();
        }
        let (z2, _) = io::file_sha(&zp2).unwrap();
        let s = s2(&rec, &("v4".into(), f.res.clone()), &zp2, &z2, &json!({"sealed": true}), &f.d.path().join("s2b")).unwrap();
        assert_eq!(s["matched_groups"], 1);
        assert_eq!(s["groups_without_posts"], 1);
        // A wrong zip hash is refused.
        assert!(s2(&rec, &("v4".into(), f.res.clone()), &zp2, &zsha, &json!({}), &f.d.path().join("s2c")).is_err());
    }
}

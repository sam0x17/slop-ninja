//! Acquisition of one reserved evaluation split's human documents after
//! its prose has been rendered (`--families`) and admitted (`dedup` against
//! a successor prior index).
//!
//! The rule is the one sealed in the split's `SEALED.json` before any prose
//! was opened: groups in `group_order`, units in `unit_order_in_group`, at
//! most `max_docs_per_group` passing documents per group, each domain
//! stopped at its target, then the declared backfill from the split's
//! remaining groups of the backfill domain. Inside one unit (a PG work
//! yields up to nine excerpts, a PLOS article an abstract and an
//! introduction) documents are taken in seeded hash order of their ids, so
//! no position in a work is preferred. A source family (`source_group`, the
//! protocol's conflicting source group) with any prior conflict (an earlier
//! snapshot, the Train-side pool or an earlier evaluation split) has already
//! left whole at dedup; it is skipped with a receipt naming the reasons, and
//! the contributor group's other families stay eligible. Every skip is
//! recorded text-free. Nothing here reads, prints or scores prose beyond
//! hashing and window keys.
//!
//! Test is refused unless the written protocol seal is supplied (path and
//! SHA-256 of `SEAL.json`, which must say `"sealed": true`), the config is
//! a v2 select config naming the reservation (its sealed groups and
//! manifest hashes, domains, protocol caps and backfill), and the RAID
//! screen outputs (S1 and S3 always, S2 when the reservation holds Reddit)
//! are given. RAID-screened groups (S1, S2) and units (S3) are skipped with
//! their own reasons before any other rule; selection then continues in
//! group order.
use crate::{
    io::{self, JsonlWriter},
    raid_screen,
    splits::{ALL_DOMAINS, rank},
    text,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
};

/// Evaluation domains: the splits-v3 domains (same indices as
/// `splits::ALL_DOMAINS`) and the eleven amendment-2 domains of splits-v4.
pub const DOMAINS: [&str; 19] = [
    "project_gutenberg", "wikipedia", "wikinews", "plos", "us_federal_register", "stack_exchange", "fineweb", "global_voices",
    "fineweb_blogs", "reddit", "ubuntu_irc", "wikipedia_talk", "wikisource", "instructional", "peer_review", "legal_opinions",
    "spoken_transcripts", "student_essays", "historical_manuscripts",
];

/// Pool collections of the amendment-2 domains (splits-v4 config
/// `evaluation_domains.<d>.collections`; FineWeb blogs from the blog adapter).
pub const NEW_DOMAIN_COLLECTIONS: [(&str, &[&str]); 11] = [
    ("fineweb_blogs", &["fineweb_blog_cc_historical"]),
    ("reddit", &["reddit_webis_tldr17_historical"]),
    ("ubuntu_irc", &["ubuntu_irc_historical"]),
    ("wikipedia_talk", &["wikipedia_talk_wikiconv_historical"]),
    ("wikisource", &["wikisource_enwikisource_historical"]),
    ("instructional", &["wikibooks_enwikibooks_historical", "wikibooks_cookbook_historical", "wikivoyage_enwikivoyage_historical"]),
    ("peer_review", &["openreview_orb_reviews_historical"]),
    ("legal_opinions", &["courtlistener_opinions_historical", "caselaw_access_project_historical"]),
    ("spoken_transcripts", &["fomc_transcripts_historical", "ami_meeting_corpus_manual_transcripts"]),
    ("student_essays", &["persuade_2_0_student_essays", "ellipse_ell_student_essays"]),
    ("historical_manuscripts", &["loc_by_the_people_historical"]),
];

/// Domain index (into `DOMAINS`) of a pool collection.
pub fn domain_of_collection(c: &str) -> Option<usize> {
    if let Some(d) = crate::splits::domain_of_collection_all(c) {
        return Some(d);
    }
    NEW_DOMAIN_COLLECTIONS.iter().find(|(_, cs)| cs.contains(&c)).and_then(|(d, _)| DOMAINS.iter().position(|x| x == d))
}

pub const EVAL_DOCUMENT_SCHEMA: &str = "slop_ninja_a_competitive_eval_human_document_v1";
pub const SELECT_CONFIG_V2_SCHEMA: &str = "slop_ninja_a_competitive_eval_select_config_v2";
pub const SKIP_SCHEMA: &str = "slop_ninja_a_competitive_eval_acquisition_skip_v1";
pub const SUMMARY_SCHEMA: &str = "slop_ninja_a_competitive_eval_acquisition_summary_v1";
const SCOPE: &str = "eval_document_within_unit";

pub struct Options<'a> {
    pub split_dir: &'a Path,
    pub config: &'a Path,
    pub dedup: &'a Path,
    pub adapters: &'a [PathBuf],
    /// Optional diagnostic: rows whose folded `near_window`-word windows are
    /// compared with the selected documents of `near_collection`. Counted
    /// only; never a selection input.
    pub near_reference: &'a [PathBuf],
    pub near_collection: &'a str,
    pub near_window: usize,
    pub out: &'a Path,
    /// Reservation entry of a v2 config (`reservations.<name>`); required for Test.
    pub reservation: Option<&'a str>,
    /// Written protocol seal; required for Test.
    pub seal: Option<&'a Path>,
    pub seal_sha256: Option<&'a str>,
    /// RAID screen outputs (`raid-s1`, `raid-s2`, `raid-s3` directories).
    pub raid_s1: Option<&'a Path>,
    pub raid_s2: Option<&'a Path>,
    pub raid_s3: Option<&'a Path>,
    /// Near-window exclusion without replacement (splits-v4
    /// `near_window_check`, applied by protocol to Global Voices): selected
    /// documents of `near_exclude_collection` sharing any folded
    /// `near_exclude_window`-word window with that collection's rows in
    /// `near_exclude_train` are dropped after selection, not replaced.
    pub near_exclude_train: Option<&'a Path>,
    pub near_exclude_collection: &'a str,
    pub near_exclude_window: usize,
}

fn domain_index(name: &str) -> Result<usize> {
    DOMAINS.iter().position(|x| *x == name).with_context(|| format!("unknown evaluation domain {name}"))
}

/// Protocol cap of a domain in a v2 config: a number or `{split: n}`.
fn config_cap(cfg: &Value, domain: &str, split: &str) -> Result<f64> {
    let c = &cfg["max_docs_per_group"][domain];
    c.as_f64().or_else(|| c[split].as_f64()).with_context(|| format!("config max_docs_per_group for {domain} {split}"))
}

/// True for a dedup exclusion reason that names an earlier snapshot, the
/// Train-side pool or an earlier split (through the successor index), as
/// opposed to a within-split duplicate or screen.
pub fn is_prior_reason(r: &str) -> bool {
    !(r.starts_with("within_pool_") || r.starts_with("within_collection_") || r.starts_with("markup_residue_"))
}

struct Unit {
    family: String,
    group_order: u64,
    unit_order: u64,
    pg_id: Option<u64>,
}

struct Group {
    key: String,
    order: u64,
    cap: usize,
    units: Vec<usize>,
}

fn sha_of(p: &Path) -> Result<String> {
    Ok(io::file_sha(p)?.0)
}

pub fn run(o: &Options) -> Result<Value> {
    let sealed: Value = serde_json::from_slice(&std::fs::read(o.split_dir.join("SEALED.json")).context("SEALED.json")?)?;
    let split = sealed["split"].as_str().context("SEALED.split")?.to_owned();
    ensure!(["dev_iter", "dev_select", "cal", "test"].contains(&split.as_str()), "unknown split {split}");
    // Test: the written seal first, before any input is read further.
    let seal_receipt = if split == "test" {
        let r = raid_screen::require_seal(o.seal, o.seal_sha256).context("Test acquisition is refused without the written protocol seal")?;
        ensure!(o.reservation.is_some(), "Test needs --reservation (a v2 select config entry)");
        ensure!(o.raid_s1.is_some() && o.raid_s3.is_some(), "Test needs the RAID screen outputs (--raid-s1, --raid-s3)");
        Some(r)
    } else {
        None
    };
    let groups_p = o.split_dir.join("groups.jsonl");
    let manifest_p = o.split_dir.join("manifest.jsonl");
    ensure!(sealed["groups_sha256"] == sha_of(&groups_p)?.as_str(), "groups.jsonl differs from SEALED.json");
    ensure!(sealed["manifest_sha256"] == sha_of(&manifest_p)?.as_str(), "manifest.jsonl differs from SEALED.json");
    let cfg: Value = serde_json::from_slice(&std::fs::read(o.config)?)?;
    let seed = cfg["seed"].as_str().context("config seed")?.to_owned();
    let excluded: BTreeSet<String> = cfg["excluded_domains"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
    let target = |d: usize| -> Result<u64> { cfg["targets"][DOMAINS[d]][&split].as_u64().with_context(|| format!("target {} {split}", DOMAINS[d])) };
    let minimum = cfg["protocol_minimum_human_docs"][&split].as_u64().unwrap_or(0);
    // v2 config: one reservation entry binds the sealed files, domains, caps and backfill.
    let resv: Option<Value> = match o.reservation {
        Some(n) => {
            ensure!(cfg["schema"] == SELECT_CONFIG_V2_SCHEMA, "--reservation needs a {SELECT_CONFIG_V2_SCHEMA} config");
            ensure!(cfg["split"] == split.as_str(), "config is for split {}, reservation is {split}", cfg["split"]);
            let r = cfg["reservations"].get(n).cloned().with_context(|| format!("config has no reservation {n}"))?;
            ensure!(r["groups_sha256"] == sealed["groups_sha256"] && r["manifest_sha256"] == sealed["manifest_sha256"], "reservation {n} binds other sealed files than {}", o.split_dir.display());
            Some(r)
        }
        None => None,
    };
    let (domains, backfill, deficit_domains): (Vec<usize>, Option<usize>, Option<Vec<usize>>) = match &resv {
        Some(r) => {
            let ds = r["domains"].as_array().context("reservation domains")?.iter().map(|d| domain_index(d.as_str().unwrap_or(""))).collect::<Result<Vec<_>>>()?;
            if let Some(sd) = sealed["domains"].as_array() {
                let a: BTreeSet<&str> = sd.iter().filter_map(Value::as_str).collect();
                let b: BTreeSet<&str> = ds.iter().map(|&d| DOMAINS[d]).collect();
                ensure!(a == b, "reservation domains {b:?} differ from SEALED.json domains {a:?}");
            }
            for &d in &ds {
                ensure!(!excluded.contains(DOMAINS[d]), "reservation domain {} is excluded", DOMAINS[d]);
                target(d)?;
            }
            let b = r["backfill_domain"].as_str().map(domain_index).transpose()?;
            let dd = r["backfill_deficit_domains"].as_array().into_iter().flatten().map(|d| domain_index(d.as_str().unwrap_or(""))).collect::<Result<Vec<_>>>()?;
            ensure!(b.is_some() || dd.is_empty(), "deficit domains without a backfill domain");
            ensure!(dd.iter().all(|d| ds.contains(d)) && b.is_none_or(|b| ds.contains(&b)), "backfill domains outside the reservation");
            (ds, b, Some(dd))
        }
        // Domains with a target entry in the config (configs written before
        // splits-v3 name only the first six, so their behaviour is unchanged).
        None => (
            (0..ALL_DOMAINS.len()).filter(|d| !excluded.contains(DOMAINS[*d]) && cfg["targets"].get(DOMAINS[*d]).is_some()).collect(),
            cfg["backfill_domain"].as_str().map(|b| DOMAINS.iter().position(|x| *x == b).context("backfill domain")).transpose()?,
            None,
        ),
    };
    if split == "test" && domains.contains(&domain_index("reddit")?) {
        ensure!(o.raid_s2.is_some(), "Test with Reddit needs the RAID S2 output (--raid-s2)");
    }
    let screen = raid_screen::Screen::load(o.raid_s1, o.raid_s2, o.raid_s3, &split)?;

    // Reserved units and groups.
    let mut units: Vec<Unit> = Vec::new();
    let mut by_group: HashMap<(usize, String), Vec<usize>> = HashMap::new();
    let mut family_domain: HashMap<String, usize> = HashMap::new();
    for row in io::jsonl_rows(&manifest_p)? {
        let r = row?;
        ensure!(r["split"] == split.as_str(), "manifest row of another split");
        let d = DOMAINS.iter().position(|x| r["domain"] == *x).context("manifest domain")?;
        ensure!(!excluded.contains(DOMAINS[d]), "manifest row of excluded domain");
        ensure!(resv.is_none() || domains.contains(&d), "manifest row of domain {} outside the reservation", DOMAINS[d]);
        let fam = r["family_key"].as_str().context("family_key")?.to_owned();
        family_domain.insert(fam.clone(), d);
        by_group.entry((d, r["group_key"].as_str().context("group_key")?.to_owned())).or_default().push(units.len());
        units.push(Unit {
            family: fam,
            group_order: r["group_order"].as_u64().context("group_order")?,
            unit_order: r["unit_order_in_group"].as_u64().context("unit_order_in_group")?,
            pg_id: r["source_unit"]["pg_id"].as_u64(),
        });
    }
    // v2: one dedup may cover every reservation of the split; rows of the
    // other reservations' families are recognised (from their bound
    // manifests) and left to their own selection.
    let mut other_families: HashSet<String> = HashSet::new();
    if let Some(n) = o.reservation {
        for (name, r) in cfg["reservations"].as_object().context("reservations")? {
            if name == n {
                continue;
            }
            let Some(dir) = r["dir"].as_str() else { continue };
            let mp = Path::new(dir).join("manifest.jsonl");
            ensure!(r["manifest_sha256"] == sha_of(&mp)?.as_str(), "reservation {name} manifest {} differs from the config", mp.display());
            for row in io::jsonl_rows(&mp)? {
                let r = row?;
                ensure!(r["split"] == split.as_str(), "reservation {name} holds rows of another split");
                other_families.insert(r["family_key"].as_str().context("family_key")?.to_owned());
            }
        }
    }
    let mut foreign_rows = 0u64;
    let mut groups: Vec<Vec<Group>> = (0..DOMAINS.len()).map(|_| Vec::new()).collect();
    for row in io::jsonl_rows(&groups_p)? {
        let r = row?;
        let d = DOMAINS.iter().position(|x| r["domain"] == *x).context("group domain")?;
        let key = r["group_key"].as_str().context("group_key")?.to_owned();
        let mut us = by_group.remove(&(d, key.clone())).with_context(|| format!("group without units {key}"))?;
        us.sort_by_key(|&u| (units[u].unit_order, u));
        let order = r["order_in_split_domain"].as_u64().context("order_in_split_domain")?;
        ensure!(us.iter().all(|&u| units[u].group_order == order), "group_order mismatch in {key}");
        let cap = r["max_docs_per_group"].as_f64().context("max_docs_per_group")?;
        ensure!(cap >= 1.0 && cap.fract() == 0.0, "cap");
        if resv.is_some() {
            // protocol excerpts.cap_deviation: a reservation recording other caps needs an amendment first.
            let want = config_cap(&cfg, DOMAINS[d], &split)?;
            ensure!(cap == want, "group {key} records cap {cap}, protocol cap for {} {split} is {want}: needs an amendment", DOMAINS[d]);
        }
        groups[d].push(Group { key, order, cap: cap as usize, units: us });
    }
    ensure!(by_group.is_empty(), "manifest units without a group row");
    for g in &mut groups {
        g.sort_by_key(|g| g.order);
    }

    // Admitted documents and exclusions of this split's dedup run.
    let dedup_report: Value = serde_json::from_slice(&std::fs::read(o.dedup.join("dedup-report.json"))?)?;
    ensure!(dedup_report["admitted_sha256"] == sha_of(&o.dedup.join("admitted.jsonl"))?.as_str(), "admitted.jsonl changed");
    ensure!(dedup_report["exclusions_sha256"] == sha_of(&o.dedup.join("exclusions.jsonl"))?.as_str(), "exclusions.jsonl changed");
    let mut admitted: HashMap<String, Vec<(u64, Value)>> = HashMap::new();
    for row in io::jsonl_rows(&o.dedup.join("admitted.jsonl"))? {
        let r = row?;
        let fam = r["source_group"].as_str().context("source_group")?.to_owned();
        let Some(&d) = family_domain.get(&fam) else {
            ensure!(other_families.contains(&fam), "admitted family {fam} is not reserved for {split}");
            foreign_rows += 1;
            continue;
        };
        let coll = r["collection"].as_str().unwrap_or("");
        ensure!(domain_of_collection(coll) == Some(d), "collection {coll} does not match reserved domain of {fam}");
        let id = r["id"].as_str().context("id")?.to_owned();
        admitted.entry(fam).or_default().push((rank(&seed, SCOPE, &id), r));
    }
    for v in admitted.values_mut() {
        v.sort_by(|a, b| (a.0, a.1["id"].as_str()).cmp(&(b.0, b.1["id"].as_str())));
    }
    let mut prior_conflict: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut excl_reasons: HashMap<String, BTreeMap<String, u64>> = HashMap::new();
    for row in io::jsonl_rows(&o.dedup.join("exclusions.jsonl"))? {
        let r = row?;
        let fam = r["source_group"].as_str().context("source_group")?.to_owned();
        if !family_domain.contains_key(&fam) {
            ensure!(other_families.contains(&fam), "excluded family {fam} is not reserved for {split}");
            foreign_rows += 1;
            continue;
        }
        for reason in r["reasons"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            *excl_reasons.entry(fam.clone()).or_default().entry(reason.to_owned()).or_default() += 1;
            if is_prior_reason(reason) {
                prior_conflict.entry(fam.clone()).or_default().insert(reason.to_owned());
            }
        }
    }
    // Adapter rejections (text-free) and selection sizes.
    let mut rejections: HashMap<String, BTreeMap<String, u64>> = HashMap::new();
    let mut adapter_reports = Vec::new();
    for a in o.adapters {
        let rep: Value = serde_json::from_slice(&std::fs::read(a.join("report.json"))?)?;
        ensure!(rep["complete"] == true, "incomplete adapter report {}", a.display());
        ensure!(rep["rejections_sha256"] == sha_of(&a.join("rejections.jsonl"))?.as_str(), "rejections changed {}", a.display());
        for row in io::jsonl_rows(&a.join("rejections.jsonl"))? {
            let r = row?;
            let u = r["unit_id"].as_str().context("unit_id")?;
            // PG excerpt ids look like pool-pg{N}-p{F}; other adapters use the family key.
            let key = match u.strip_prefix("pool-pg").and_then(|x| x.split_once("-p")) {
                Some((n, _)) => format!("pg:{n}"),
                None => u.to_owned(),
            };
            *rejections.entry(key).or_default().entry(r["reason"].as_str().unwrap_or("").to_owned()).or_default() += 1;
        }
        adapter_reports.push(json!({"dir": a, "report_sha256": sha_of(&a.join("report.json"))?, "adapter": rep["adapter"],
            "candidates": rep["candidates"], "rejections": rep["rejections"], "family_selection_size": rep["family_selection_size"],
            "units_seen": rep["units_seen"], "rejection_reasons": rep["rejection_reasons"], "counted_without_ledger_rows": rep["counted_without_ledger_rows"]}));
    }

    io::fresh_dir(o.out)?;
    let mut docs_w = JsonlWriter::create(&o.out.join("documents.jsonl"))?;
    let mut meta_w = JsonlWriter::create(&o.out.join("metadata.jsonl"))?;
    let ctx = Ctx { split: &split, units: &units, groups: &groups, admitted: &admitted, prior_conflict: &prior_conflict, excl_reasons: &excl_reasons, rejections: &rejections, screen: &screen };
    let mut st = State {
        skip_w: JsonlWriter::create(&o.out.join("skips.jsonl"))?,
        skip_counts: BTreeMap::new(),
        selected: Vec::new(),
        taken: vec![0; DOMAINS.len()],
        seen_family: HashSet::new(),
    };
    let mut cursor = vec![0usize; DOMAINS.len()];
    for &d in &domains {
        let t = target(d)?;
        let mut got = 0u64;
        while got < t && cursor[d] < groups[d].len() {
            got += st.process(&ctx, d, cursor[d], t - got, None)?;
            cursor[d] += 1;
        }
    }
    let taken_by_domain = st.taken.clone();
    let shortfall: BTreeMap<&str, u64> = domains.iter().map(|&d| (DOMAINS[d], target(d).unwrap_or(0).saturating_sub(taken_by_domain[d]))).filter(|(_, s)| *s > 0).collect();
    let total_target: u64 = domains.iter().map(|&d| target(d)).sum::<Result<u64>>()?;
    let mut backfilled = 0u64;
    if let (Some(b), false) = (backfill, shortfall.is_empty()) {
        // v2: only the declared deficit domains pass a shortfall to the backfill domain;
        // v1: any shortfall, until the split total reaches the sum of its targets.
        let passes = |d: usize| d != b && deficit_domains.as_ref().is_none_or(|dd| dd.contains(&d));
        let for_domains: Vec<&'static str> = domains.iter().filter(|&&d| passes(d) && target(d).unwrap_or(0) > taken_by_domain[d]).map(|&d| DOMAINS[d]).collect();
        let need: u64 = match &deficit_domains {
            Some(_) => domains.iter().filter(|&&d| passes(d)).map(|&d| target(d).unwrap_or(0).saturating_sub(taken_by_domain[d])).sum(),
            None => total_target.saturating_sub(st.taken.iter().sum()),
        };
        while backfilled < need && cursor[b] < groups[b].len() {
            let n = st.process(&ctx, b, cursor[b], need - backfilled, Some(for_domains.clone()))?;
            backfilled += n;
            cursor[b] += 1;
        }
    }
    // Near-window exclusion without replacement (after selection, so no later group replaces a dropped document).
    let mut near_exclusion = Value::Null;
    if let Some(tp) = o.near_exclude_train {
        ensure!(o.near_exclude_window >= 5 && !o.near_exclude_collection.is_empty(), "near exclusion window/collection");
        let mut idx: HashMap<u64, u32> = HashMap::new();
        let mut tids: Vec<String> = Vec::new();
        for r in io::jsonl_rows(tp)? {
            let r = r?;
            if r["collection"] != o.near_exclude_collection {
                continue;
            }
            let k = tids.len() as u32;
            tids.push(r["id"].as_str().unwrap_or("").to_owned());
            for w in crate::splits_v4::windows(r["text"].as_str().unwrap_or(""), o.near_exclude_window) {
                idx.entry(w).or_insert(k);
            }
        }
        ensure!(!tids.is_empty(), "near exclusion reference has no {} rows", o.near_exclude_collection);
        let (mut checked, mut excluded) = (0u64, 0u64);
        let mut keep = Vec::with_capacity(st.selected.len());
        for (d, gi, row, t) in std::mem::take(&mut st.selected) {
            if row["collection"] != o.near_exclude_collection {
                keep.push((d, gi, row, t));
                continue;
            }
            checked += 1;
            let mut shared = 0u64;
            let mut with: BTreeSet<&str> = BTreeSet::new();
            for x in crate::splits_v4::windows(row["text"].as_str().unwrap_or(""), o.near_exclude_window) {
                if let Some(&k) = idx.get(&x) {
                    shared += 1;
                    with.insert(&tids[k as usize]);
                }
            }
            if shared == 0 {
                keep.push((d, gi, row, t));
                continue;
            }
            excluded += 1;
            st.taken[d] -= 1;
            let v = json!({"reason": "near_window_shared_with_train_side_excluded_without_replacement", "group_key": groups[d][gi].key,
                "family_key": row["source_group"], "id": row["id"], "text_sha256": row["text_sha256"], "shared_windows": shared,
                "train_side_ids": with.iter().take(10).collect::<Vec<_>>()});
            st.skip(&ctx, d, v)?;
        }
        st.selected = keep;
        near_exclusion = json!({"rule": "protocol splits.reservation_v4.global_voices_near_window_exclusions: excluded at selection without replacement",
            "collection": o.near_exclude_collection, "window": o.near_exclude_window, "window_basis": "folded alphanumeric tokens (splits_v4::windows)",
            "train_side": {"path": tp, "sha256": sha_of(tp)?, "rows": tids.len(), "distinct_windows": idx.len()},
            "selected_checked": checked, "excluded": excluded, "label_blind": true});
    }
    let taken_by_domain = st.taken.clone();
    let State { skip_w, skip_counts, selected, .. } = st;
    // Groups never reached stay reserved and unused: one count per domain.
    let mut unreached = BTreeMap::new();
    for &d in &domains {
        unreached.insert(DOMAINS[d], groups[d].len() - cursor[d]);
    }

    // Optional diagnostic: shared short windows with a reference pool.
    let mut near_inputs = Vec::new();
    let near_keys: Option<HashSet<u64>> = if o.near_reference.is_empty() {
        None
    } else {
        ensure!(o.near_window >= 5, "near window");
        let mut s = HashSet::new();
        for p in o.near_reference {
            let mut rows = 0u64;
            for row in io::jsonl_rows(p)? {
                let r = row?;
                if r["collection"] == o.near_collection {
                    s.extend(text::shingles_folded_n(r["text"].as_str().unwrap_or(""), o.near_window));
                    rows += 1;
                }
            }
            near_inputs.push(json!({"path": p, "sha256": sha_of(p)?, "rows_of_collection": rows}));
        }
        Some(s)
    };

    let mut per_domain: BTreeMap<&str, Value> = BTreeMap::new();
    let mut near_hits: BTreeMap<String, u64> = BTreeMap::new();
    let mut words: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    let mut fams: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut grp: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut backfill_n: BTreeMap<&str, u64> = BTreeMap::new();
    let mut ids = HashSet::new();
    let mut by_text: HashSet<String> = HashSet::new();
    for (d, gi, mut row, t) in selected {
        let g = &groups[d][gi];
        let id = row["id"].as_str().context("id")?.to_owned();
        ensure!(ids.insert(id.clone()), "duplicate selected id {id}");
        let tx = row["text"].as_str().context("text")?;
        ensure!(row["text_sha256"] == text::sha_hex(tx.as_bytes()), "text hash {id}");
        ensure!(by_text.insert(text::normalized_sha(tx)), "duplicate selected normalized text {id}");
        let fam = row["source_group"].as_str().context("source_group")?.to_owned();
        if let Some(k) = &near_keys {
            if row["collection"] == o.near_collection {
                let hits = text::shingles_folded_n(tx, o.near_window).into_iter().filter(|w| k.contains(w)).count() as u64;
                near_hits.insert(id.clone(), hits);
                row["evaluation_acquisition_diagnostics"] = json!({"reference_shared_folded_window": {"window": o.near_window, "shared_windows": hits,
                    "note": "diagnostic only, not a selection input"}});
            }
        }
        words.entry(DOMAINS[d]).or_default().push(row["word_count"].as_u64().unwrap_or(0));
        fams.entry(DOMAINS[d]).or_default().insert(fam.clone());
        grp.entry(DOMAINS[d]).or_default().insert(g.key.clone());
        if t.backfill_for.is_some() {
            *backfill_n.entry(DOMAINS[d]).or_default() += 1;
        }
        row["frozen_split"] = json!(split);
        row["split_assigned"] = json!(true);
        row["split_assignment"] = json!({
            "schema": EVAL_DOCUMENT_SCHEMA, "split": split, "seed": seed, "group_key": g.key, "group_order": g.order,
            "order_in_split_domain": t.rank_in_split_domain, "within_unit_order": format!("rank(seed, \"{SCOPE}\", id)"),
            "backfill_for": t.backfill_for, "sealed_rule_sha256": text::sha_hex(sealed["acquisition_rule"].as_str().unwrap_or("").as_bytes()),
            "dedup_report_sha256": sha_of(&o.dedup.join("dedup-report.json"))?, "protected_index_sha256": dedup_report["protected_index_sha256"],
            "rule": "evaluation human; never Train, never mined, never a mirror topic outside this split"
        });
        docs_w.write(&row)?;
        if let Some(m) = row.as_object_mut() {
            m.remove("text");
        }
        meta_w.write(&row)?;
    }
    let (docs_sha, docs_n) = docs_w.finish()?;
    let (meta_sha, _) = meta_w.finish()?;
    let (skip_sha, skip_n) = skip_w.finish()?;
    for &d in &domains {
        let mut w = words.remove(DOMAINS[d]).unwrap_or_default();
        w.sort_unstable();
        per_domain.insert(DOMAINS[d], json!({
            "target": target(d)?, "documents": taken_by_domain[d], "backfill_documents": backfill_n.get(DOMAINS[d]).copied().unwrap_or(0),
            "source_families": fams.get(DOMAINS[d]).map_or(0, BTreeSet::len), "source_groups": grp.get(DOMAINS[d]).map_or(0, BTreeSet::len),
            "reserved_groups": groups[d].len(), "groups_processed": cursor[d], "groups_unreached": unreached[DOMAINS[d]],
            "words": if w.is_empty() { json!(null) } else { json!({"min": w[0], "median": w[w.len() / 2], "max": w[w.len() - 1]}) },
            "skips": skip_counts.get(DOMAINS[d]),
        }));
    }
    let near = near_keys.as_ref().map(|k| {
        let n = near_hits.len() as u64;
        let any = near_hits.values().filter(|&&h| h > 0).count() as u64;
        json!({"reference": near_inputs, "collection": o.near_collection, "window": o.near_window, "reference_windows": k.len(),
               "selected_documents_checked": n, "documents_sharing_any_window": any,
               "documents_sharing_10_or_more_windows": near_hits.values().filter(|&&h| h >= 10).count(),
               "note": "diagnostic only; the sealed rule screens 12-word windows within the split, and 50-word sequences against the index"})
    });
    let total: u64 = taken_by_domain.iter().sum();
    let summary = json!({
        "schema": SUMMARY_SCHEMA,
        "split": split,
        "finished_utc": io::now_utc(),
        "documents": total,
        "split_target": total_target,
        "protocol_minimum": minimum,
        "meets_minimum": total >= minimum,
        "backfill_domain": backfill.map(|b| DOMAINS[b]),
        "backfill_documents": backfilled,
        "shortfall_before_backfill": shortfall,
        "per_domain": per_domain,
        "documents_sha256": docs_sha, "documents_rows": docs_n,
        "metadata_sha256": meta_sha,
        "skips_sha256": skip_sha, "skips_rows": skip_n,
        "inputs": {"sealed": o.split_dir.join("SEALED.json"), "sealed_sha256": sha_of(&o.split_dir.join("SEALED.json"))?,
                   "config": o.config, "config_sha256": sha_of(o.config)?, "dedup": o.dedup, "dedup_report_sha256": sha_of(&o.dedup.join("dedup-report.json"))?,
                   "protected_index_sha256": dedup_report["protected_index_sha256"], "adapters": adapter_reports},
        "rule": sealed["acquisition_rule"],
        "within_unit_order": format!("seeded rank(seed, \"{SCOPE}\", document id) ascending"),
        "prior_conflict_rule": "a source family (source_group) whose dedup exclusion names a prior snapshot, the Train-side pool or an earlier split is skipped whole (dedup already excluded it whole); other families of the same contributor group stay eligible",
        "near_reference_diagnostic": near,
        "near_window_exclusion": near_exclusion,
        "reservation": o.reservation.map(|n| json!({"name": n, "entry": resv, "dedup_rows_of_other_reservations": foreign_rows})),
        "seal": seal_receipt,
        "raid_screen": json!({"basis": raid_screen::BASIS, "applied": screen.receipts, "s1_groups": screen.s1_groups.len(), "s2_groups": screen.s2_groups.len(), "s3_families": screen.s3_families.len(),
            "order": "S1 and S2 skip a whole group, S3 a unit (family), before every other rule; acquisition continues with the next unit or group"}),
        "domain_collections": domains.iter().map(|&d| (DOMAINS[d].to_owned(), json!(NEW_DOMAIN_COLLECTIONS.iter().find(|(x, _)| *x == DOMAINS[d]).map(|(_, c)| c.to_vec())))).collect::<serde_json::Map<_, _>>(),
        "prose_inspected": false,
        "detector_calls": 0,
    });
    io::write_json(&o.out.join("summary.json"), &summary)?;
    if docs_n == 0 {
        bail!("no documents selected for {split}");
    }
    Ok(summary)
}

pub const MERGE_SUMMARY_SCHEMA: &str = "slop_ninja_a_competitive_eval_acquisition_merged_summary_v1";

/// Merge the select-eval outputs of one split's reservations (in the given
/// order) into one evaluation-human directory with the same files. Every
/// part must be complete and unchanged, of the same split, and together
/// cover each target domain of the v2 config exactly once; ids and
/// normalized texts stay unique across parts.
pub fn merge(parts: &[PathBuf], config: &Path, seal: Option<&Path>, seal_sha256: Option<&str>, out: &Path) -> Result<Value> {
    let cfg: Value = serde_json::from_slice(&std::fs::read(config)?)?;
    ensure!(cfg["schema"] == SELECT_CONFIG_V2_SCHEMA, "merge needs a {SELECT_CONFIG_V2_SCHEMA} config");
    let split = cfg["split"].as_str().context("config split")?.to_owned();
    let seal_receipt = if split == "test" { Some(raid_screen::require_seal(seal, seal_sha256)?) } else { None };
    let mut sums = Vec::new();
    for p in parts {
        let s: Value = serde_json::from_slice(&std::fs::read(p.join("summary.json")).with_context(|| format!("{}/summary.json", p.display()))?)?;
        ensure!(s["schema"] == SUMMARY_SCHEMA && s["split"] == split.as_str(), "{} is not a {split} select-eval output", p.display());
        for (f, k) in [("documents.jsonl", "documents_sha256"), ("metadata.jsonl", "metadata_sha256"), ("skips.jsonl", "skips_sha256")] {
            ensure!(s[k] == sha_of(&p.join(f))?.as_str(), "{} changed", p.join(f).display());
        }
        if split == "test" {
            ensure!(s["seal"]["sha256"] == seal_receipt.as_ref().map(|r| r["sha256"].clone()).unwrap_or_default(), "{} was selected under another seal", p.display());
        }
        sums.push(s);
    }
    let targets = cfg["targets"].as_object().context("targets")?;
    let mut per_domain = serde_json::Map::new();
    for s in &sums {
        for (d, v) in s["per_domain"].as_object().into_iter().flatten() {
            ensure!(per_domain.insert(d.clone(), v.clone()).is_none(), "domain {d} in two parts");
        }
    }
    let want: BTreeSet<&str> = targets.iter().filter(|(_, t)| t[&split].as_u64().unwrap_or(0) > 0).map(|(d, _)| d.as_str()).collect();
    let got: BTreeSet<&str> = per_domain.keys().map(String::as_str).collect();
    ensure!(want == got, "parts cover domains {got:?}, config targets {want:?}");
    io::fresh_dir(out)?;
    let mut docs_w = JsonlWriter::create(&out.join("documents.jsonl"))?;
    let mut meta_w = JsonlWriter::create(&out.join("metadata.jsonl"))?;
    let mut skip_w = JsonlWriter::create(&out.join("skips.jsonl"))?;
    let (mut ids, mut texts) = (HashSet::new(), HashSet::new());
    for p in parts {
        for row in io::jsonl_rows(&p.join("documents.jsonl"))? {
            let r = row?;
            let id = r["id"].as_str().context("id")?.to_owned();
            ensure!(ids.insert(id.clone()), "duplicate id {id} across parts");
            ensure!(texts.insert(text::normalized_sha(r["text"].as_str().context("text")?)), "duplicate normalized text {id} across parts");
            ensure!(r["frozen_split"] == split.as_str(), "row {id} of another split");
            docs_w.write(&r)?;
        }
        for row in io::jsonl_rows(&p.join("metadata.jsonl"))? {
            meta_w.write(&row?)?;
        }
        for row in io::jsonl_rows(&p.join("skips.jsonl"))? {
            skip_w.write(&row?)?;
        }
    }
    let (docs_sha, docs_n) = docs_w.finish()?;
    let (meta_sha, meta_n) = meta_w.finish()?;
    let (skip_sha, skip_n) = skip_w.finish()?;
    ensure!(docs_n == meta_n, "documents and metadata rows differ");
    let total: u64 = sums.iter().map(|s| s["documents"].as_u64().unwrap_or(0)).sum();
    ensure!(total == docs_n, "part summaries count {total} documents, merged {docs_n}");
    let split_target: u64 = targets.values().filter_map(|t| t[&split].as_u64()).sum();
    let minimum = cfg["protocol_minimum_human_docs"][&split].as_u64().context("protocol minimum")?;
    let summary = json!({
        "schema": MERGE_SUMMARY_SCHEMA, "split": split, "finished_utc": io::now_utc(),
        "documents": docs_n, "split_target": split_target, "protocol_minimum": minimum, "meets_minimum": docs_n >= minimum,
        "per_domain": per_domain,
        "documents_sha256": docs_sha, "documents_rows": docs_n, "metadata_sha256": meta_sha, "skips_sha256": skip_sha, "skips_rows": skip_n,
        "parts": parts.iter().zip(&sums).map(|(p, s)| json!({"dir": p, "summary_sha256": sha_of(&p.join("summary.json")).ok(), "documents": s["documents"],
            "reservation": s["reservation"]["name"], "backfill_documents": s["backfill_documents"], "meets_part_minimum": s["meets_minimum"]})).collect::<Vec<_>>(),
        "config": config, "config_sha256": sha_of(config)?, "seal": seal_receipt,
        "below_minimum_rule": "a split still below its minimum is evaluated at realised size with flag limits from the realised human count (protocol splits.backfill)",
        "prose_inspected": false, "detector_calls": 0,
    });
    io::write_json(&out.join("summary.json"), &summary)?;
    Ok(summary)
}

struct Ctx<'a> {
    split: &'a str,
    units: &'a [Unit],
    groups: &'a [Vec<Group>],
    admitted: &'a HashMap<String, Vec<(u64, Value)>>,
    prior_conflict: &'a HashMap<String, BTreeSet<String>>,
    excl_reasons: &'a HashMap<String, BTreeMap<String, u64>>,
    rejections: &'a HashMap<String, BTreeMap<String, u64>>,
    screen: &'a raid_screen::Screen,
}

struct Taken {
    rank_in_split_domain: u64,
    backfill_for: Option<Vec<&'static str>>,
}

struct State {
    skip_w: JsonlWriter,
    skip_counts: BTreeMap<String, BTreeMap<String, u64>>,
    /// (domain, group index, admitted row, placement)
    selected: Vec<(usize, usize, Value, Taken)>,
    taken: Vec<u64>,
    seen_family: HashSet<String>,
}

impl State {
    fn skip(&mut self, ctx: &Ctx, d: usize, mut v: Value) -> Result<()> {
        let reason = v["reason"].as_str().unwrap_or("").to_owned();
        *self.skip_counts.entry(DOMAINS[d].to_owned()).or_default().entry(reason).or_default() += 1;
        v["schema"] = json!(SKIP_SCHEMA);
        v["split"] = json!(ctx.split);
        v["domain"] = json!(DOMAINS[d]);
        self.skip_w.write(&v)
    }

    fn unit_status(ctx: &Ctx, u: &Unit) -> Value {
        let mut why = serde_json::Map::new();
        if let Some(r) = ctx.excl_reasons.get(&u.family) {
            why.insert("dedup_exclusions".into(), json!(r));
        }
        let rk = u.pg_id.map(|n| format!("pg:{n}")).unwrap_or_else(|| u.family.clone());
        if let Some(r) = ctx.rejections.get(&rk) {
            why.insert("adapter_rejections".into(), json!(r));
        }
        if why.is_empty() {
            why.insert("adapter".into(), json!("no candidate and no rejection row (not in the input, a redirect, outside the namespace, or a counted-only screen)"));
        }
        Value::Object(why)
    }

    /// Take up to `limit` documents from one group; returns the number taken.
    fn process(&mut self, ctx: &Ctx, d: usize, gi: usize, limit: u64, backfill_for: Option<Vec<&'static str>>) -> Result<u64> {
        let g = &ctx.groups[d][gi];
        let mut n = 0u64;
        // RAID screen S1 (title lineage) and S2 (Reddit author lineage): the whole group leaves.
        let group_rule = if ctx.screen.s1_groups.contains(&g.key) {
            Some(("S1_title", raid_screen::S1_REASON))
        } else if ctx.screen.s2_groups.contains(&g.key) {
            Some(("S2_reddit_author", raid_screen::S2_REASON))
        } else {
            None
        };
        if let Some((rule, reason)) = group_rule {
            for &u in &g.units {
                let unit = &ctx.units[u];
                self.seen_family.insert(unit.family.clone());
                let v = json!({"reason": reason, "rule": rule, "group_key": g.key, "family_key": unit.family, "pg_id": unit.pg_id, "basis": "RAID acquisition screen (label-blind)"});
                self.skip(ctx, d, v)?;
            }
            return Ok(0);
        }
        for &u in &g.units {
            let unit = &ctx.units[u];
            if !self.seen_family.insert(unit.family.clone()) {
                let v = json!({"reason": "family_key_repeated_counted_once", "group_key": g.key, "family_key": unit.family, "pg_id": unit.pg_id});
                self.skip(ctx, d, v)?;
                continue;
            }
            if ctx.screen.s3_families.contains(&unit.family) {
                let v = json!({"reason": raid_screen::S3_REASON, "rule": "S3_text", "group_key": g.key, "family_key": unit.family, "pg_id": unit.pg_id, "basis": "RAID acquisition screen (label-blind)"});
                self.skip(ctx, d, v)?;
                continue;
            }
            if let Some(r) = ctx.prior_conflict.get(&unit.family) {
                let v = json!({"reason": "family_has_prior_conflict", "group_key": g.key, "family_key": unit.family, "pg_id": unit.pg_id, "prior_reasons": r});
                self.skip(ctx, d, v)?;
                continue;
            }
            let docs = ctx.admitted.get(&unit.family).map(Vec::as_slice).unwrap_or(&[]);
            if docs.is_empty() {
                let v = json!({"reason": "unit_without_admitted_document", "group_key": g.key, "family_key": unit.family, "pg_id": unit.pg_id, "why": Self::unit_status(ctx, unit)});
                self.skip(ctx, d, v)?;
                continue;
            }
            for (_, row) in docs {
                if n as usize >= g.cap || n >= limit {
                    let reason = if n >= limit { "domain_or_split_target_reached_inside_group" } else { "group_cap_reached" };
                    let v = json!({"reason": reason, "group_key": g.key, "family_key": unit.family, "id": row["id"], "text_sha256": row["text_sha256"]});
                    self.skip(ctx, d, v)?;
                    continue;
                }
                self.selected.push((d, gi, row.clone(), Taken { rank_in_split_domain: self.taken[d], backfill_for: backfill_for.clone() }));
                self.taken[d] += 1;
                n += 1;
            }
        }
        Ok(n)
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

    fn doc(id: &str, fam: &str, coll: &str, seed_text: &str) -> Value {
        let t = (0..160).map(|i| format!("{seed_text}{i}")).collect::<Vec<_>>().join(" ");
        json!({"id": id, "source_group": fam, "collection": coll, "text": t, "text_sha256": text::sha_hex(t.as_bytes()), "word_count": 160})
    }

    struct Fixture {
        _d: tempfile::TempDir,
        split: PathBuf,
        config: PathBuf,
        dedup: PathBuf,
        out: PathBuf,
    }

    fn fixture(split: &str) -> Fixture {
        let d = tempfile::tempdir().unwrap();
        let sd = d.path().join("split");
        std::fs::create_dir_all(&sd).unwrap();
        // (domain, group, order, cap, families)
        let plan: &[(&str, &str, u64, f64, &[&str])] = &[
            ("project_gutenberg", "author:a", 0, 3.0, &["work:a1", "work:a2"]),
            ("project_gutenberg", "author:b", 1, 3.0, &["work:b1", "work:b2"]),
            ("project_gutenberg", "author:c", 2, 3.0, &["work:c1"]),
            ("project_gutenberg", "author:e", 3, 3.0, &["work:e1"]),
            ("plos", "doi:p1", 0, 2.0, &["doi:p1"]),
            ("plos", "doi:p2", 1, 2.0, &["doi:p2"]),
            ("wikipedia", "wikipedia:page:1", 0, 1.0, &["wikipedia:page:1"]),
            ("wikipedia", "wikipedia:page:2", 1, 1.0, &["wikipedia:page:2"]),
            ("wikipedia", "wikipedia:page:3", 2, 1.0, &["wikipedia:page:3"]),
            ("wikipedia", "wikipedia:page:4", 3, 1.0, &["wikipedia:page:4"]),
            ("wikipedia", "wikipedia:page:5", 4, 1.0, &["wikipedia:page:5"]),
        ];
        let (mut groups, mut units) = (vec![], vec![]);
        for (dom, g, o, cap, fams) in plan {
            groups.push(json!({"domain": dom, "group_key": g, "order_in_split_domain": o, "max_docs_per_group": cap, "split": split}));
            for (i, f) in fams.iter().enumerate() {
                units.push(json!({"domain": dom, "group_key": g, "group_order": o, "unit_order_in_group": i, "family_key": f, "split": split, "source_unit": {}}));
            }
        }
        let gs = write_jsonl(&sd.join("groups.jsonl"), &groups);
        let ms = write_jsonl(&sd.join("manifest.jsonl"), &units);
        io::write_json(&sd.join("SEALED.json"), &json!({"split": split, "groups_sha256": gs, "manifest_sha256": ms, "acquisition_rule": "synthetic"})).unwrap();
        let config = d.path().join("config.json");
        let mut targets = serde_json::Map::new();
        for (dom, t) in [("project_gutenberg", 4), ("plos", 2), ("wikipedia", 2), ("wikinews", 0), ("us_federal_register", 0), ("stack_exchange", 0)] {
            targets.insert(dom.into(), json!({split: t}));
        }
        io::write_json(&config, &json!({"seed": "s", "targets": targets, "excluded_domains": ["wikinews", "us_federal_register", "stack_exchange"],
            "protocol_minimum_human_docs": {split: 7}, "backfill_domain": "wikipedia"})).unwrap();
        let dd = d.path().join("dedup");
        std::fs::create_dir_all(&dd).unwrap();
        let pg = "project_gutenberg_historical";
        let wk = "wikipedia_enwiki_historical";
        let admitted = vec![
            doc("a1-1", "work:a1", pg, "q"), doc("a1-2", "work:a1", pg, "r"), doc("a1-3", "work:a1", pg, "s"), doc("a1-4", "work:a1", pg, "t"),
            doc("a2-1", "work:a2", pg, "u"), doc("b1-1", "work:b1", pg, "v"), doc("c1-1", "work:c1", pg, "w"), doc("c1-2", "work:c1", pg, "x"),
            doc("e1-1", "work:e1", pg, "e"),
            doc("p1-a", "doi:p1", "plos_historical", "y"),
            doc("w1", "wikipedia:page:1", wk, "z"), doc("w3", "wikipedia:page:3", wk, "zz"), doc("w5", "wikipedia:page:5", wk, "zzz"),
        ];
        let excl = vec![
            json!({"id": "b2-1", "source_group": "work:b2", "reasons": ["shares_folded_50_word_sequence_with_prior"]}),
            json!({"id": "w2", "source_group": "wikipedia:page:2", "reasons": ["markup_residue_pipe"]}),
        ];
        let a = write_jsonl(&dd.join("admitted.jsonl"), &admitted);
        let e = write_jsonl(&dd.join("exclusions.jsonl"), &excl);
        io::write_json(&dd.join("dedup-report.json"), &json!({"admitted_sha256": a, "exclusions_sha256": e, "protected_index_sha256": "i"})).unwrap();
        let out = d.path().join("out");
        Fixture { split: sd, config, dedup: dd, out, _d: d }
    }

    fn no_test() -> Options<'static> {
        let e = Path::new("");
        Options { split_dir: e, config: e, dedup: e, adapters: &[], near_reference: &[], near_collection: "", near_window: 12,
                  out: e, reservation: None, seal: None, seal_sha256: None, raid_s1: None, raid_s2: None, raid_s3: None,
                  near_exclude_train: None, near_exclude_collection: "", near_exclude_window: 12 }
    }

    #[test]
    fn sealed_order_caps_prior_groups_and_backfill() {
        let f = fixture("dev_iter");
        let o = Options { split_dir: &f.split, config: &f.config, dedup: &f.dedup, adapters: &[], near_reference: &[], near_collection: "x", near_window: 12, out: &f.out, ..no_test() };
        let s = run(&o).unwrap();
        // PG: author a gives 3 (cap); author b gives b1 (b2 has a prior conflict and is skipped alone): target 4 reached.
        assert_eq!(s["per_domain"]["project_gutenberg"]["documents"], 4);
        assert_eq!(s["per_domain"]["project_gutenberg"]["skips"]["family_has_prior_conflict"], 1);
        assert_eq!(s["per_domain"]["project_gutenberg"]["groups_unreached"], 2);
        // PLOS short by one; Wikipedia w1, w3 for its target, then w5 as backfill (w4 has no document).
        assert_eq!(s["per_domain"]["plos"]["documents"], 1);
        assert_eq!(s["shortfall_before_backfill"]["plos"], 1);
        assert_eq!(s["per_domain"]["wikipedia"]["documents"], 3);
        assert_eq!(s["backfill_documents"], 1);
        assert_eq!(s["documents"], 8);
        assert_eq!(s["meets_minimum"], true);
        let rows: Vec<Value> = io::jsonl_rows(&f.out.join("documents.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        let ids: BTreeSet<&str> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert!(ids.contains("a2-1") || rows.iter().filter(|r| r["source_group"] == "work:a1").count() == 3);
        assert!(ids.contains("b1-1") && !ids.contains("c1-1"));
        assert!(ids.contains("w5") && !ids.contains("w2"));
        assert!(rows.iter().all(|r| r["frozen_split"] == "dev_iter"));
        let meta: Vec<Value> = io::jsonl_rows(&f.out.join("metadata.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert!(meta.iter().all(|r| r.get("text").is_none()));
        // Deterministic: a second run selects the same bytes.
        let out2 = f.out.with_file_name("out2");
        let o2 = Options { out: &out2, ..o };
        let s2 = run(&o2).unwrap();
        assert_eq!(s["documents_sha256"], s2["documents_sha256"]);
    }

    #[test]
    fn test_split_is_refused_and_prior_reasons_classified() {
        let f = fixture("test");
        let o = Options { split_dir: &f.split, config: &f.config, dedup: &f.dedup, adapters: &[], near_reference: &[], near_collection: "x", near_window: 12, out: &f.out, ..no_test() };
        assert!(run(&o).is_err());
        assert!(!f.out.exists());
        assert!(is_prior_reason("family_key_in_prior_snapshot") && is_prior_reason("matches_prior_exact_text"));
        assert!(!is_prior_reason("within_pool_exact_duplicate") && !is_prior_reason("within_collection_shares_folded_12_word_window") && !is_prior_reason("markup_residue_pipe"));
    }

    /// One reservation directory with groups (domain, group, cap, families) and a dedup dir of admitted rows.
    fn reservation(root: &Path, name: &str, split: &str, plan: &[(&str, &str, f64, &[&str])], domains: Option<&[&str]>) -> (PathBuf, String, String) {
        let sd = root.join(name);
        std::fs::create_dir_all(&sd).unwrap();
        let (mut groups, mut units) = (vec![], vec![]);
        let mut order: BTreeMap<&str, u64> = BTreeMap::new();
        for (dom, g, cap, fams) in plan {
            let o = order.entry(dom).or_default();
            groups.push(json!({"domain": dom, "group_key": g, "order_in_split_domain": *o, "max_docs_per_group": cap, "split": split}));
            for (i, f) in fams.iter().enumerate() {
                units.push(json!({"domain": dom, "group_key": g, "group_order": *o, "unit_order_in_group": i, "family_key": f, "split": split, "source_unit": {}}));
            }
            *o += 1;
        }
        let gs = write_jsonl(&sd.join("groups.jsonl"), &groups);
        let ms = write_jsonl(&sd.join("manifest.jsonl"), &units);
        let mut sealed = json!({"split": split, "groups_sha256": gs, "manifest_sha256": ms, "acquisition_rule": "synthetic"});
        if let Some(d) = domains {
            sealed["domains"] = json!(d);
        }
        io::write_json(&sd.join("SEALED.json"), &sealed).unwrap();
        (sd, gs, ms)
    }

    fn dedup_dir(p: &Path, admitted: &[Value]) {
        std::fs::create_dir_all(p).unwrap();
        let a = write_jsonl(&p.join("admitted.jsonl"), admitted);
        let e = write_jsonl(&p.join("exclusions.jsonl"), &[]);
        io::write_json(&p.join("dedup-report.json"), &json!({"admitted_sha256": a, "exclusions_sha256": e, "protected_index_sha256": "i"})).unwrap();
    }

    fn raid_out(p: &Path, rule: &str, file: &str, key: &str, rows: &[Value]) {
        std::fs::create_dir_all(p).unwrap();
        let s = write_jsonl(&p.join(file), rows);
        io::write_json(&p.join("summary.json"), &json!({"schema": raid_screen::SUMMARY_SCHEMA, "rule": rule, key: s, "record": {"sha256": "r"}})).unwrap();
    }

    struct TestFx {
        d: tempfile::TempDir,
        base: PathBuf,
        newd: PathBuf,
        config: PathBuf,
        dedup: PathBuf,
        seal: PathBuf,
        seal_sha: String,
        s1: PathBuf,
        s2: PathBuf,
        s3: PathBuf,
    }

    fn test_fixture(cap_override: Option<f64>) -> TestFx {
        let d = tempfile::tempdir().unwrap();
        let t = d.path();
        let (base, bg, bm) = reservation(t, "base", "test", &[
            ("project_gutenberg", "author:a", 3.0, &["work:a1"]),
            ("project_gutenberg", "author:b", 3.0, &["work:b1"]),
            ("plos", "doi:p1", 2.0, &["doi:p1"]),
            ("wikipedia", "wikipedia:page:1", 1.0, &["wikipedia:page:1"]),
            ("wikipedia", "wikipedia:page:2", 1.0, &["wikipedia:page:2"]),
            ("wikipedia", "wikipedia:page:3", 1.0, &["wikipedia:page:3"]),
            ("wikipedia", "wikipedia:page:4", 1.0, &["wikipedia:page:4"]),
            ("wikipedia", "wikipedia:page:5", 1.0, &["wikipedia:page:5"]),
            ("wikipedia", "wikipedia:page:6", 1.0, &["wikipedia:page:6"]),
        ], None);
        let (newd, ng, nm) = reservation(t, "v4", "test", &[
            ("reddit", "reddit_author:aa", 1.0, &["reddit_post:t3_1"]),
            ("reddit", "reddit_author:bb", 1.0, &["reddit_post:t3_2"]),
            ("reddit", "reddit_author:cc", 1.0, &["reddit_post:t3_3"]),
            ("historical_manuscripts", "loc_btp_collection:x", cap_override.unwrap_or(220.0), &["loc_btp_item:1", "loc_btp_item:2"]),
        ], Some(&["reddit", "historical_manuscripts"]));
        let config = t.join("select-test.json");
        io::write_json(&config, &json!({
            "schema": SELECT_CONFIG_V2_SCHEMA, "split": "test", "seed": "s",
            "targets": {"project_gutenberg": {"test": 2}, "plos": {"test": 2}, "wikipedia": {"test": 2}, "reddit": {"test": 3}, "historical_manuscripts": {"test": 2}},
            "max_docs_per_group": {"project_gutenberg": 3, "plos": 2, "wikipedia": 1, "reddit": 1, "historical_manuscripts": 220},
            "protocol_minimum_human_docs": {"test": 9},
            "excluded_domains": [],
            "reservations": {
                "base": {"dir": base, "groups_sha256": bg, "manifest_sha256": bm, "domains": ["project_gutenberg", "plos", "wikipedia"], "backfill_domain": "wikipedia", "backfill_deficit_domains": ["project_gutenberg", "plos"]},
                "v4": {"dir": newd, "groups_sha256": ng, "manifest_sha256": nm, "domains": ["reddit", "historical_manuscripts"], "backfill_domain": null, "backfill_deficit_domains": []}
            }
        })).unwrap();
        let pg = "project_gutenberg_historical";
        let wk = "wikipedia_enwiki_historical";
        let rd = "reddit_webis_tldr17_historical";
        let bt = "loc_by_the_people_historical";
        let dedup = t.join("dedup");
        dedup_dir(&dedup, &[
            doc("a1-1", "work:a1", pg, "q"), doc("b1-1", "work:b1", pg, "v"), doc("p1-a", "doi:p1", "plos_historical", "y"),
            doc("w1", "wikipedia:page:1", wk, "z"), doc("w2", "wikipedia:page:2", wk, "zy"), doc("w3", "wikipedia:page:3", wk, "zx"),
            doc("w4", "wikipedia:page:4", wk, "zw"), doc("w5", "wikipedia:page:5", wk, "zv"), doc("w6", "wikipedia:page:6", wk, "zu"),
            doc("r1", "reddit_post:t3_1", rd, "ra"), doc("r2", "reddit_post:t3_2", rd, "rb"), doc("r3", "reddit_post:t3_3", rd, "rc"),
            doc("h1", "loc_btp_item:1", bt, "ha"), doc("h2", "loc_btp_item:2", bt, "hb"),
        ]);
        let seal = t.join("SEAL.json");
        io::write_json(&seal, &json!({"sealed": true, "protocol_sha256": "x"})).unwrap();
        let seal_sha = io::file_sha(&seal).unwrap().0;
        let s1 = t.join("s1");
        raid_out(&s1, "S1_title", "s1-skips.jsonl", "skips_sha256", &[json!({"split": "test", "group_key": "wikipedia:page:2", "family_key": "wikipedia:page:2"})]);
        let s2 = t.join("s2");
        raid_out(&s2, "S2_reddit_author", "s2-skips.jsonl", "skips_sha256", &[json!({"split": "test", "group_key": "reddit_author:bb"})]);
        let s3 = t.join("s3");
        raid_out(&s3, "S3_text", "s3-hits.jsonl", "hits_sha256", &[json!({"family_key": "wikipedia:page:4", "id": "w4"})]);
        TestFx { d, base, newd, config, dedup, seal, seal_sha, s1, s2, s3 }
    }

    fn test_opts<'a>(f: &'a TestFx, split_dir: &'a Path, res: &'a str, out: &'a Path) -> Options<'a> {
        Options { split_dir, config: &f.config, dedup: &f.dedup, out, reservation: Some(res), seal: Some(&f.seal), seal_sha256: Some(&f.seal_sha),
                  raid_s1: Some(&f.s1), raid_s2: Some(&f.s2), raid_s3: Some(&f.s3), ..no_test() }
    }

    #[test]
    fn test_mode_needs_the_written_seal_reservation_and_raid_screen() {
        let f = test_fixture(None);
        let out = f.d.path().join("o");
        let good = test_opts(&f, &f.base, "base", &out);
        assert!(run(&Options { seal: None, ..good }).is_err());
        assert!(run(&Options { seal_sha256: Some(&"0".repeat(64)), ..good }).is_err());
        let unsealed = f.d.path().join("U.json");
        io::write_json(&unsealed, &json!({"sealed": false})).unwrap();
        let us = io::file_sha(&unsealed).unwrap().0;
        assert!(run(&Options { seal: Some(&unsealed), seal_sha256: Some(&us), ..good }).is_err());
        assert!(run(&Options { reservation: None, ..good }).is_err());
        assert!(run(&Options { raid_s1: None, ..good }).is_err());
        assert!(run(&Options { raid_s3: None, ..good }).is_err());
        // The reservation entry must bind this directory's sealed files.
        assert!(run(&Options { reservation: Some("v4"), ..good }).is_err());
        // Reddit needs S2.
        let out4 = f.d.path().join("o4");
        assert!(run(&Options { raid_s2: None, ..test_opts(&f, &f.newd, "v4", &out4) }).is_err());
        assert!(!out.exists() && !out4.exists());
        assert!(run(&good).is_ok());
    }

    #[test]
    fn test_mode_applies_raid_skips_and_backfills_only_original_domains() {
        let f = test_fixture(None);
        let ob = f.d.path().join("ob");
        let s = run(&test_opts(&f, &f.base, "base", &ob)).unwrap();
        // PG 2 of 2; PLOS 1 of 2 (deficit 1, passes to Wikipedia).
        // Wikipedia: w1, w2 skipped by S1, w3 -> target 2 reached with w1, w3; backfill: w4 skipped by S3, w5.
        assert_eq!(s["per_domain"]["project_gutenberg"]["documents"], 2);
        assert_eq!(s["per_domain"]["plos"]["documents"], 1);
        assert_eq!(s["per_domain"]["wikipedia"]["documents"], 3);
        assert_eq!(s["backfill_documents"], 1);
        assert_eq!(s["per_domain"]["wikipedia"]["skips"]["raid_train_title_lineage"], 1);
        assert_eq!(s["per_domain"]["wikipedia"]["skips"]["raid_train_text_lineage"], 1);
        let rows: Vec<Value> = io::jsonl_rows(&ob.join("documents.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        let ids: BTreeSet<&str> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, BTreeSet::from(["a1-1", "b1-1", "p1-a", "w1", "w3", "w5"]));
        assert!(rows.iter().all(|r| r["frozen_split"] == "test"));
        assert_eq!(s["seal"]["sha256"], f.seal_sha.as_str());
        let o4 = f.d.path().join("o4");
        let s4 = run(&test_opts(&f, &f.newd, "v4", &o4)).unwrap();
        // Reddit: author bb skipped by S2, realised 2 of 3, no backfill.
        assert_eq!(s4["per_domain"]["reddit"]["documents"], 2);
        assert_eq!(s4["per_domain"]["reddit"]["skips"]["raid_train_reddit_lineage"], 1);
        assert_eq!(s4["backfill_documents"], 0);
        assert_eq!(s4["per_domain"]["historical_manuscripts"]["documents"], 2);
        // Merge: 6 + 4 documents, below the minimum 9? no: 10 >= 9.
        let m = f.d.path().join("merged");
        let ms = merge(&[ob.clone(), o4.clone()], &f.config, Some(&f.seal), Some(&f.seal_sha), &m).unwrap();
        assert_eq!(ms["documents"], 10);
        assert_eq!(ms["meets_minimum"], true);
        assert_eq!(ms["split_target"], 11);
        assert!(merge(&[ob.clone()], &f.config, Some(&f.seal), Some(&f.seal_sha), &f.d.path().join("m2")).is_err(), "missing domains");
        assert!(merge(&[ob.clone(), o4.clone()], &f.config, None, None, &f.d.path().join("m3")).is_err(), "Test merge needs the seal");
        let meta: Vec<Value> = io::jsonl_rows(&m.join("metadata.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert!(meta.iter().all(|r| r.get("text").is_none()) && meta.len() == 10);
    }

    #[test]
    fn test_mode_refuses_a_reservation_cap_other_than_the_protocol_cap() {
        let f = test_fixture(Some(200.0));
        let o4 = f.d.path().join("o4");
        let e = run(&test_opts(&f, &f.newd, "v4", &o4)).unwrap_err();
        assert!(format!("{e:#}").contains("needs an amendment"));
        assert!(!o4.exists());
    }

    #[test]
    fn new_domain_collections_map_to_their_domains() {
        assert_eq!(domain_of_collection("wikivoyage_enwikivoyage_historical").map(|d| DOMAINS[d]), Some("instructional"));
        assert_eq!(domain_of_collection("ami_meeting_corpus_manual_transcripts").map(|d| DOMAINS[d]), Some("spoken_transcripts"));
        assert_eq!(domain_of_collection("fineweb_blog_cc_historical").map(|d| DOMAINS[d]), Some("fineweb_blogs"));
        assert_eq!(domain_of_collection("global_voices_historical").map(|d| DOMAINS[d]), Some("global_voices"));
        assert_eq!(domain_of_collection("stackexchange_historical").map(|d| DOMAINS[d]), Some("stack_exchange"));
        assert_eq!(&DOMAINS[..8], &ALL_DOMAINS[..]);
    }

    #[test]
    fn global_voices_near_window_hits_are_excluded_without_replacement() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path();
        let (gv, gs, ms) = reservation(t, "nd", "test", &[
            ("global_voices", "gv_author:a", 15.0, &["global_voices_post:1", "global_voices_post:2"]),
            ("global_voices", "gv_author:b", 15.0, &["global_voices_post:3"]),
            ("global_voices", "gv_author:c", 15.0, &["global_voices_post:4"]),
        ], Some(&["global_voices"]));
        let config = t.join("cfg.json");
        io::write_json(&config, &json!({"schema": SELECT_CONFIG_V2_SCHEMA, "split": "test", "seed": "s",
            "targets": {"global_voices": {"test": 3}}, "max_docs_per_group": {"global_voices": {"test": 15}},
            "protocol_minimum_human_docs": {"test": 1}, "excluded_domains": [],
            "reservations": {"nd": {"dir": gv, "groups_sha256": gs, "manifest_sha256": ms, "domains": ["global_voices"], "backfill_domain": null}}})).unwrap();
        let c = "global_voices_historical";
        let mut d2 = doc("g2", "global_voices_post:2", c, "gb");
        // g2 quotes 12 consecutive words of a Train-side Global Voices post.
        let quoted = "the minister said on tuesday that the new law would protect every journalist";
        let t2 = format!("{} {}", d2["text"].as_str().unwrap(), quoted);
        d2["text"] = json!(t2);
        d2["text_sha256"] = json!(text::sha_hex(t2.as_bytes()));
        let dd = t.join("dedup");
        dedup_dir(&dd, &[doc("g1", "global_voices_post:1", c, "ga"), d2, doc("g3", "global_voices_post:3", c, "gc"), doc("g4", "global_voices_post:4", c, "gd")]);
        let train = t.join("train.jsonl");
        write_jsonl(&train, &[json!({"id": "tr-1", "collection": c, "text": format!("Earlier: {quoted}, according to reports.")}),
                              json!({"id": "tr-2", "collection": "fineweb_cc_historical", "text": doc("x", "f", "f", "ga")["text"]})]);
        let seal = t.join("SEAL.json");
        io::write_json(&seal, &json!({"sealed": true})).unwrap();
        let ss = io::file_sha(&seal).unwrap().0;
        let s1 = t.join("s1");
        raid_out(&s1, "S1_title", "s1-skips.jsonl", "skips_sha256", &[]);
        let s3 = t.join("s3");
        raid_out(&s3, "S3_text", "s3-hits.jsonl", "hits_sha256", &[]);
        let out = t.join("o");
        let o = Options { split_dir: &gv, config: &config, dedup: &dd, out: &out, reservation: Some("nd"), seal: Some(&seal), seal_sha256: Some(&ss),
                          raid_s1: Some(&s1), raid_s3: Some(&s3), near_exclude_train: Some(&train), near_exclude_collection: c, near_exclude_window: 12, ..no_test() };
        let s = run(&o).unwrap();
        // Target 3 reached by g1, g2 (author a) and g3 (author b); g2 is then dropped and author c is not reached.
        assert_eq!(s["per_domain"]["global_voices"]["documents"], 2);
        assert_eq!(s["near_window_exclusion"]["excluded"], 1);
        assert_eq!(s["near_window_exclusion"]["selected_checked"], 3);
        assert_eq!(s["per_domain"]["global_voices"]["skips"]["near_window_shared_with_train_side_excluded_without_replacement"], 1);
        let ids: BTreeSet<String> = io::jsonl_rows(&out.join("documents.jsonl")).unwrap().map(|r| r.unwrap()["id"].as_str().unwrap().to_owned()).collect();
        assert_eq!(ids, BTreeSet::from(["g1".to_owned(), "g3".to_owned()]));
        // Without the reference nothing is excluded.
        let s0 = run(&Options { out: &t.join("o0"), near_exclude_train: None, ..o }).unwrap();
        assert_eq!(s0["per_domain"]["global_voices"]["documents"], 3);
    }
}

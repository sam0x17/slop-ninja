//! Cross-source admission: prior-snapshot overlap, within-pool exact and
//! near-duplicate removal, family components and per-source admission
//! records.
//!
//! Policy:
//! - any family with a candidate that overlaps a prior snapshot (text,
//!   50-word sequence, family key, PG id or title alias) is excluded whole,
//!   matching the established "entire conflicting source family" rule;
//! - within the pool, candidates are visited in a fixed hash order of their
//!   ids; the first copy is kept and any later exact, normalised, folded or
//!   shared-50-word-sequence copy is excluded and named;
//! - families joined by an excluded cross-family near-duplicate share one
//!   `split_component`, which later split assignment must keep together.
use crate::{
    io::{self, JsonlWriter},
    protected::Index,
    record,
    text,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};

pub const ADMISSION_SCHEMA: &str = "slop_ninja_a_pool_admission_v1";
pub const DEDUP_SCHEMA: &str = "slop_ninja_a_pool_dedup_report_v1";
pub const METHOD: &str = "prior_family_exclusion_then_ranked_within_pool_exact_normalized_folded_hash_and_folded_50_word_sequence_v1";
/// Method label when the optional screens are on (see `Options`).
pub const METHOD_V2: &str = "prior_family_exclusion_then_ranked_within_pool_exact_normalized_folded_hash_and_folded_50_word_sequence_plus_markup_residue_and_within_collection_short_window_screens_v2";

/// Optional screens on top of the v1 policy.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Folded window length for the within-collection near-duplicate screen
    /// (0 disables it).
    pub near_window: usize,
    /// Collections the short-window screen applies to. Within each, a
    /// candidate that shares any folded `near_window`-word window with an
    /// earlier-ranked admitted candidate of the same collection is excluded
    /// (template families keep one member). No family link is made.
    pub near_collections: Vec<String>,
    /// Collections screened for MediaWiki rendering residue
    /// (`text::markup_residue`).
    pub residue_collections: Vec<String>,
    /// Aliases (for example `pg:37029`) whose families leave the pool whole,
    /// like a prior conflict: works a review receipt names.
    pub exclude_aliases: Vec<String>,
    /// Reverse-scan hits (`scan::load_hits`): each listed candidate counts as
    /// a prior conflict with the recorded reasons, like an index match.
    pub prior_hits: Option<HashMap<String, BTreeSet<String>>>,
    /// Path and SHA-256 of the hits file, for the report.
    pub prior_hits_file: Option<(PathBuf, String)>,
    /// Source families with rows in reserved (never rendered) buckets: a
    /// candidate of such a family leaves the pool with its whole family, so
    /// a family whose bucket key changed between units (for example a
    /// project whose contact PI changed) never straddles the reservation.
    pub reserved_families: BTreeSet<String>,
    /// Path, SHA-256 and row count of each reserved-metadata file read.
    pub reserved_files: Vec<(PathBuf, String, usize)>,
}

/// Family keys (`family_key`) of `reserved-metadata.jsonl` files.
pub fn load_reserved_families(paths: &[PathBuf]) -> Result<(BTreeSet<String>, Vec<(PathBuf, String, usize)>)> {
    let mut fams = BTreeSet::new();
    let mut files = Vec::new();
    for p in paths {
        let mut n = 0;
        for row in io::jsonl_rows(p)? {
            let row = row?;
            ensure!(row.get("text").is_none(), "reserved metadata carries text: {}", p.display());
            fams.insert(row["family_key"].as_str().with_context(|| format!("family_key in {}", p.display()))?.to_owned());
            n += 1;
        }
        files.push((p.clone(), io::file_sha(p)?.0, n));
    }
    Ok((fams, files))
}
const RANK_DOMAIN: &str = "slop_ninja_a_pool_dedup_rank_v1\0";

struct Cand {
    id: String,
    collection: String,
    family: usize,
    rank: u64,
    keys: [u64; 3],
    shingles: Vec<u64>,
    prior: Vec<&'static str>,
    near: Option<(usize, Vec<u64>)>,
    residue: Vec<&'static str>,
}

enum Decision {
    Admitted,
    Excluded { reasons: Vec<String>, duplicate_of: Option<String> },
}

struct UnionFind(Vec<usize>);
impl UnionFind {
    fn find(&mut self, x: usize) -> usize {
        let mut r = x;
        while self.0[r] != r {
            r = self.0[r];
        }
        let mut c = x;
        while self.0[c] != r {
            let n = self.0[c];
            self.0[c] = r;
            c = n;
        }
        r
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.0[ra.max(rb)] = ra.min(rb);
        }
    }
}

fn stats(w: &mut [usize]) -> Value {
    if w.is_empty() {
        return json!(null);
    }
    w.sort_unstable();
    json!({"min": w[0], "median": w[w.len() / 2], "max": w[w.len() - 1],
           "mean": w.iter().sum::<usize>() as f64 / w.len() as f64})
}

pub fn run(index_dir: &Path, sources: &[PathBuf], out: &Path, min_shared: usize) -> Result<Value> {
    run_with(index_dir, sources, out, min_shared, &Options::default())
}

pub fn run_with(index_dir: &Path, sources: &[PathBuf], out: &Path, min_shared: usize, opt: &Options) -> Result<Value> {
    ensure!(min_shared >= 1, "min_shared must be at least 1");
    ensure!(opt.near_collections.is_empty() || opt.near_window >= 5, "near_window must be at least 5 when near_collections are set");
    let method = if opt.near_collections.is_empty() && opt.residue_collections.is_empty() && opt.exclude_aliases.is_empty() && opt.prior_hits.is_none() && opt.reserved_families.is_empty() { METHOD } else { METHOD_V2 };
    let near_coll: BTreeMap<&str, usize> = opt.near_collections.iter().enumerate().map(|(i, c)| (c.as_str(), i)).collect();
    ensure!(!sources.is_empty(), "no candidate sources");
    io::fresh_dir(out)?;
    let index = Index::load(index_dir)?;

    // Pass 1: compact keys, prior conflicts and families.
    let mut reports = Vec::new();
    let mut cands = Vec::new();
    let mut fam_ids: HashMap<String, usize> = HashMap::new();
    let mut fam_names: Vec<String> = Vec::new();
    let mut ids = BTreeSet::new();
    for s in sources {
        let rep: Value = serde_json::from_slice(&std::fs::read(s.join("report.json")).with_context(|| format!("report in {}", s.display()))?)?;
        ensure!(rep["schema"] == crate::adapter::REPORT_SCHEMA && rep["complete"] == true, "incomplete adapter report: {}", s.display());
        let (sha, _) = io::file_sha(&s.join("candidates.jsonl"))?;
        ensure!(rep["candidates_sha256"] == sha.as_str(), "candidates changed since adapter report: {}", s.display());
        reports.push((s.clone(), rep, text::sha_hex(&std::fs::read(s.join("report.json"))?)));
        for row in io::jsonl_rows(&s.join("candidates.jsonl"))? {
            let row = row?;
            ensure!(row["schema"] == record::DOCUMENT_SCHEMA, "candidate schema");
            let id = row["id"].as_str().context("candidate id")?.to_owned();
            ensure!(ids.insert(id.clone()), "duplicate candidate id {id}");
            let t = row["text"].as_str().context("candidate text")?;
            ensure!(row["text_sha256"] == text::sha_hex(t.as_bytes()), "text hash mismatch {id}");
            let fam = row["source_group"].as_str().context("source_group")?.to_owned();
            let next = fam_names.len();
            let family = *fam_ids.entry(fam.clone()).or_insert_with(|| {
                fam_names.push(fam);
                next
            });
            let mut prior = index.text_conflicts(t);
            prior.extend(index.family_conflicts(&row));
            if let Some(r) = opt.prior_hits.as_ref().and_then(|h| h.get(&id)) {
                prior.extend(r.iter().map(|x| crate::scan::static_reason(x)));
                prior.sort();
                prior.dedup();
            }
            if row["aliases"].as_array().into_iter().flatten().filter_map(Value::as_str).any(|a| opt.exclude_aliases.iter().any(|x| x == a)) {
                prior.push("alias_listed_for_whole_family_exclusion");
            }
            if opt.reserved_families.contains(row["source_group"].as_str().unwrap_or("")) {
                prior.push("family_has_reserved_bucket_rows");
            }
            let collection = row["collection"].as_str().unwrap_or("");
            let near = near_coll.get(collection).map(|&k| (k, text::shingles_folded_n(t, opt.near_window)));
            let residue = if opt.residue_collections.iter().any(|c| c == collection) { text::markup_residue(t) } else { Vec::new() };
            cands.push(Cand {
                near,
                residue,
                rank: text::h64(format!("{RANK_DOMAIN}{id}").as_bytes()),
                id,
                collection: row["collection"].as_str().unwrap_or("").to_owned(),
                family,
                keys: text::text_keys(t),
                shingles: text::shingles_folded(t),
                prior,
            });
        }
    }

    // Families touched by any prior conflict leave the pool whole.
    let prior_families: BTreeSet<usize> = cands.iter().filter(|c| !c.prior.is_empty()).map(|c| c.family).collect();

    // Pass 2: ranked within-pool deduplication.
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| (cands[a].rank, &cands[a].id).cmp(&(cands[b].rank, &cands[b].id)));
    let mut decisions: Vec<Option<Decision>> = (0..cands.len()).map(|_| None).collect();
    let mut exact: [HashMap<u64, usize>; 3] = Default::default();
    let mut shingle_owner: HashMap<u64, usize> = HashMap::new();
    let mut near_owner: Vec<HashMap<u64, usize>> = (0..opt.near_collections.len()).map(|_| HashMap::new()).collect();
    let mut uf = UnionFind((0..fam_names.len()).collect());
    let mut cross_family_links = 0u64;
    let mut near_excluded = 0u64;
    let mut residue_excluded = 0u64;
    for &i in &order {
        let c = &cands[i];
        if prior_families.contains(&c.family) {
            let mut reasons: Vec<String> = c.prior.iter().map(|r| r.to_string()).collect();
            if reasons.is_empty() {
                reasons.push("family_has_prior_snapshot_conflict".into());
            }
            decisions[i] = Some(Decision::Excluded { reasons, duplicate_of: None });
            continue;
        }
        let labels = ["within_pool_exact_duplicate", "within_pool_whitespace_normalized_duplicate", "within_pool_folded_duplicate"];
        let mut hit: Option<(String, usize)> = None;
        for k in 0..3 {
            if let Some(&o) = exact[k].get(&c.keys[k]) {
                hit = Some((labels[k].into(), o));
                break;
            }
        }
        if hit.is_none() {
            let mut shared: HashMap<usize, usize> = HashMap::new();
            for s in &c.shingles {
                if let Some(&o) = shingle_owner.get(s) {
                    *shared.entry(o).or_default() += 1;
                }
            }
            if let Some((&o, _)) = shared.iter().filter(|&(_, &n)| n >= min_shared).min_by_key(|&(&o, _)| (cands[o].rank, o)) {
                hit = Some(("within_pool_shares_folded_50_word_sequence".into(), o));
            }
        }
        if let Some((reason, o)) = hit {
            if cands[o].family != c.family {
                uf.union(cands[o].family, c.family);
                cross_family_links += 1;
            }
            decisions[i] = Some(Decision::Excluded { reasons: vec![reason], duplicate_of: Some(cands[o].id.clone()) });
            continue;
        }
        if !c.residue.is_empty() {
            residue_excluded += 1;
            decisions[i] = Some(Decision::Excluded { reasons: c.residue.iter().map(|r| format!("markup_residue_{r}")).collect(), duplicate_of: None });
            continue;
        }
        if let Some((k, wins)) = &c.near {
            let owner = wins.iter().filter_map(|w| near_owner[*k].get(w)).min_by_key(|&&o| (cands[o].rank, o));
            if let Some(&o) = owner {
                near_excluded += 1;
                decisions[i] = Some(Decision::Excluded {
                    reasons: vec![format!("within_collection_shares_folded_{}_word_window", opt.near_window)],
                    duplicate_of: Some(cands[o].id.clone()),
                });
                continue;
            }
            for w in wins {
                near_owner[*k].entry(*w).or_insert(i);
            }
        }
        for k in 0..3 {
            exact[k].entry(c.keys[k]).or_insert(i);
        }
        for s in &c.shingles {
            shingle_owner.entry(*s).or_insert(i);
        }
        decisions[i] = Some(Decision::Admitted);
    }
    // Component key: smallest family key among members.
    let mut comp_key: BTreeMap<usize, String> = BTreeMap::new();
    let mut comp_size: BTreeMap<usize, usize> = BTreeMap::new();
    for f in 0..fam_names.len() {
        let r = uf.find(f);
        let e = comp_key.entry(r).or_insert_with(|| fam_names[f].clone());
        if fam_names[f] < *e {
            *e = fam_names[f].clone();
        }
        *comp_size.entry(r).or_default() += 1;
    }
    let by_id: HashMap<&str, usize> = cands.iter().enumerate().map(|(i, c)| (c.id.as_str(), i)).collect();

    // Pass 3: write outputs.
    std::fs::create_dir_all(out.join("by-collection"))?;
    std::fs::create_dir_all(out.join("admission"))?;
    let mut admitted = JsonlWriter::create(&out.join("admitted.jsonl"))?;
    let mut excluded_rows = JsonlWriter::create(&out.join("excluded-records.jsonl"))?;
    let mut exclusions = JsonlWriter::create(&out.join("exclusions.jsonl"))?;
    let mut per: BTreeMap<String, JsonlWriter> = BTreeMap::new();
    #[derive(Default)]
    struct Stat {
        candidates: u64,
        admitted: u64,
        reasons: BTreeMap<String, u64>,
        families: BTreeSet<String>,
        admitted_families: BTreeSet<String>,
        writers: BTreeSet<String>,
        words: Vec<usize>,
        licenses: BTreeMap<String, u64>,
        domains: BTreeMap<String, u64>,
    }
    let mut st: BTreeMap<String, Stat> = BTreeMap::new();
    for (s, _, _) in &reports {
        for row in io::jsonl_rows(&s.join("candidates.jsonl"))? {
            let mut row = row?;
            let i = by_id[row["id"].as_str().expect("id checked")];
            let c = &cands[i];
            let e = st.entry(c.collection.clone()).or_default();
            e.candidates += 1;
            e.families.insert(fam_names[c.family].clone());
            let root = uf.find(c.family);
            match decisions[i].as_ref().expect("every candidate decided") {
                Decision::Admitted => {
                    e.admitted += 1;
                    e.admitted_families.insert(fam_names[c.family].clone());
                    if let Some(w) = row["writer_key"].as_str() {
                        e.writers.insert(w.to_owned());
                    }
                    e.words.push(row["word_count"].as_u64().unwrap_or(0) as usize);
                    *e.licenses.entry(row["rights"]["license"].as_str().unwrap_or("").to_owned()).or_default() += 1;
                    *e.domains.entry(row["domain"].as_str().unwrap_or("").to_owned()).or_default() += 1;
                    row["pool_status"] = json!("admitted");
                    row["pool_admission"] = json!({"method": method, "protected_index_sha256": index.sha256,
                        "split_component": comp_key[&root], "component_families": comp_size[&root],
                        "min_shared_sequences": min_shared});
                    admitted.write(&row)?;
                    let w = match per.entry(c.collection.clone()) {
                        std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
                        std::collections::btree_map::Entry::Vacant(v) => {
                            let name = if c.collection.is_empty() { "unknown".into() } else { c.collection.clone() };
                            v.insert(JsonlWriter::create(&out.join("by-collection").join(format!("{name}.jsonl")))?)
                        }
                    };
                    w.write(&row)?;
                }
                Decision::Excluded { reasons, duplicate_of } => {
                    for r in reasons {
                        *e.reasons.entry(r.clone()).or_default() += 1;
                    }
                    let x = json!({"id": c.id, "collection": c.collection, "source_group": fam_names[c.family],
                                   "text_sha256": row["text_sha256"], "reasons": reasons, "duplicate_of": duplicate_of,
                                   "split_component": comp_key[&root]});
                    exclusions.write(&x)?;
                    row["pool_status"] = json!("excluded");
                    row["pool_exclusion"] = x;
                    excluded_rows.write(&row)?;
                }
            }
        }
    }
    let (adm_sha, adm_rows) = admitted.finish()?;
    let (exr_sha, _) = excluded_rows.finish()?;
    let (exc_sha, exc_rows) = exclusions.finish()?;
    let mut per_sha = BTreeMap::new();
    for (k, w) in per {
        per_sha.insert(k, w.finish()?);
    }
    let multi_family_components = comp_size.values().filter(|&&n| n > 1).count();
    let source_list: Vec<Value> = reports
        .iter()
        .map(|(p, r, sha)| json!({"dir": p, "adapter": r["adapter"], "collection": r["collection"], "report_sha256": sha,
                                 "candidates_sha256": r["candidates_sha256"], "candidates": r["candidates"]}))
        .collect();
    let report = json!({
        "schema": DEDUP_SCHEMA,
        "method": method,
        "options": {"near_window": opt.near_window, "near_collections": opt.near_collections, "residue_collections": opt.residue_collections,
                    "exclude_aliases": opt.exclude_aliases,
                    "reserved_metadata": opt.reserved_files.iter().map(|(p, sha, n)| json!({"path": p, "sha256": sha, "rows": n})).collect::<Vec<_>>(),
                    "reserved_families": opt.reserved_families.len(),
                    "reserved_rule": "a candidate whose source family has a row in a reserved bucket is a prior conflict; its family leaves the pool whole",
                    "near_rule": "within each listed collection, a candidate sharing any folded near_window-word window with an earlier-ranked admitted candidate of that collection is excluded and names it; no family link",
                    "residue_rule": "pipe, empty parentheses after a non-word character, literal [n] citation marker (1-3 digits), unbalanced square brackets"},
        "near_window_excluded": near_excluded,
        "markup_residue_excluded": residue_excluded,
        "finished_utc": io::now_utc(),
        "protected_index_sha256": index.sha256,
        "protected_index_dir": index_dir,
        "prior_hits": opt.prior_hits_file.as_ref().map(|(p, sha)| json!({"path": p, "sha256": sha, "candidates": opt.prior_hits.as_ref().map(HashMap::len),
                                                                       "rule": "a listed candidate is a prior conflict; its family leaves the pool whole"})),
        "sources": source_list,
        "candidates": cands.len(),
        "admitted": adm_rows,
        "excluded": exc_rows,
        "families": fam_names.len(),
        "families_excluded_for_prior_conflict": prior_families.len(),
        "cross_family_duplicate_links": cross_family_links,
        "multi_family_split_components": multi_family_components,
        "min_shared_sequences": min_shared,
        "admitted_sha256": adm_sha,
        "exclusions_sha256": exc_sha,
        "excluded_records_sha256": exr_sha,
        "by_collection_sha256": per_sha.iter().map(|(k, (s, n))| (k.clone(), json!({"sha256": s, "rows": n}))).collect::<serde_json::Map<_, _>>(),
    });
    let report_sha = io::write_json(&out.join("dedup-report.json"), &report)?;

    // Per-source admission records.
    for (collection, mut s) in st {
        let adapters: Vec<Value> = reports
            .iter()
            .filter(|(_, r, _)| r["collection"] == collection.as_str())
            .map(|(p, r, sha)| json!({"dir": p, "report_sha256": sha, "adapter": r["adapter"], "inputs": r["inputs"],
                                     "parameters": r["parameters"], "units_seen": r["units_seen"],
                                     "adapter_rejections": r["rejections"], "adapter_rejection_reasons": r["rejection_reasons"]}))
            .collect();
        let (sha, rows) = per_sha.get(&collection).cloned().unwrap_or_default();
        let record = json!({
            "schema": ADMISSION_SCHEMA,
            "date": io::now_utc().get(..10),
            "status": "human_pool_candidates_admitted_not_split_not_fitted",
            "collection": collection,
            "domains": s.domains,
            "adapters": adapters,
            "requested": s.candidates,
            "admitted": s.admitted,
            "excluded": s.candidates - s.admitted,
            "exclusion_reasons": s.reasons,
            "source_families": s.families.len(),
            "admitted_source_families": s.admitted_families.len(),
            "documented_writer_keys": s.writers.len(),
            "individual_human_identity": if s.writers.is_empty() { "unavailable; collaborative or institutional" } else { "writer_key where the source documents a single owner" },
            "licenses": s.licenses,
            "rights_decision": "2026-10-27: admit public domain, CC0, CC BY and CC BY-SA sources consistent with existing precedent; user decisions of 2026-10-27 also admit FineWeb (pre-cutoff crawls, ODC-By, never redistributed), Global Voices (CC BY 3.0), the archived 2022-06-06 Stack Exchange dump (Train and Train-mining only) and PLOS prose licence statements (Train and Train-mining only); Blog Authorship and PELIC are excluded and hosted captures never enter Train. Each row's rights field records its own licence and decision.",
            "date_policy": format!("Dated text strictly before {}; public-domain historical text otherwise.", record::CUTOFF),
            "word_bound": [text::MIN_WORDS, text::MAX_WORDS],
            "word_counts": stats(&mut s.words),
            "overlap": {"method": method, "protected_index_sha256": index.sha256, "dedup_report_sha256": report_sha,
                        "prior_conflict_policy": "entire_conflicting_source_family_exclusion_no_replacement",
                        "within_pool_policy": "first_in_fixed_id_hash_order_kept; later_copies_excluded_and_named",
                        "min_shared_sequences": min_shared},
            "records_sha256": sha,
            "records_rows": rows,
            "split_assigned": false,
            "final_test_opened": false,
            "new_paid_detector_calls": 0,
            "detector_outputs_used_as_labels_or_selectors": false,
            "new_fitted_model": false,
            "limitations": [
                "Human labels are historical or pre-cutoff proxies, not authenticated unaided production histories.",
                "Pretraining exposure for the encoder and comparator is unknown.",
                "Overlap protection covers only the registered prior snapshots and this pool.",
                "Several excerpts of one family are not independent sources; split by split_component.",
                "Word bounds do not establish tokenizer fit.",
                "Admission does not establish model quality; that requires fitting and independent evaluation."
            ],
        });
        let name = if collection.is_empty() { "unknown".to_owned() } else { collection };
        io::write_json(&out.join("admission").join(format!("{name}.json")), &record)?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{adapter::Run, protected, record::Doc, text::Excerpt};

    fn prose(seed: &str, n: usize) -> String {
        format!("{}.", (0..n).map(|i| format!("{seed}{i}")).collect::<Vec<_>>().join(" "))
    }

    fn write_source(dir: &Path, collection: &str, docs: &[(&str, &str, String)]) {
        let mut run = Run::start(dir, "synthetic", collection, json!({}), None).unwrap();
        for (id, fam, t) in docs {
            let ex = Excerpt { text: t.clone(), start: 0, end: 1, words: text::words(t), complete_unit: true };
            let doc = Doc {
                id: (*id).into(), collection, domain: "test", source_group: (*fam).into(), writer_key: None,
                evidence: "historical_proxy", excerpt: &ex, extraction_method: "synthetic", unit: json!({}),
                provenance: json!({}), date: Some("2020-01-01".into()), date_basis: "synthetic",
                cutoff_policy: "dated_before_cutoff",
                rights: record::rights("CC-BY-4.0", "synthetic".into(), "https://example.org", ""),
                aliases: if fam.starts_with("author_work") { vec!["pg:77".into()] } else { vec![] },
            };
            assert!(run.accept(id, &doc).unwrap());
        }
        run.finish(json!({})).unwrap();
    }

    #[test]
    fn prior_family_exclusion_and_within_pool_duplicates() {
        let d = tempfile::tempdir().unwrap();
        let prior_text = prose("old", 200);
        std::fs::create_dir(d.path().join("prior")).unwrap();
        std::fs::write(d.path().join("prior/records.jsonl"), format!("{}\n{}\n",
            json!({"text": prior_text, "source_group": "wikipedia:page:1"}),
            json!({"pg_id": 77}))).unwrap();
        let reg = d.path().join("reg.json");
        std::fs::write(&reg, json!({"schema": protected::REGISTRY_SCHEMA, "entries": [{"label": "p", "path": "prior", "names": ["*.jsonl"]}]}).to_string()).unwrap();
        protected::build(&reg, d.path(), &d.path().join("ix")).unwrap();

        let shared = prose("shared", 60);
        let a = d.path().join("a");
        write_source(&a, "coll_a", &[
            ("a1", "fam:a", prose("alpha", 200)),
            // Family fam:b has one prior-overlapping excerpt; both its excerpts leave.
            ("b1", "fam:b", format!("{} {}", prose("x", 120), &prior_text[..400])),
            ("b2", "fam:b", prose("beta", 200)),
            ("c1", "wikipedia:page:1", prose("gamma", 200)),
            ("d1", "fam:d", format!("{} {}", prose("delta", 120), shared)),
        ]);
        let b = d.path().join("b");
        write_source(&b, "coll_b", &[
            ("e1", "fam:e", format!("{} {}", shared, prose("eps", 120))),
            ("f1", "fam:f", prose("alpha", 200).replace(' ', "  ")),
            ("g1", "author_work_v2:x:y", prose("zeta", 200)),
        ]);
        let out = d.path().join("out");
        let rep = run(&d.path().join("ix"), &[a.clone(), b.clone()], &out, 1).unwrap();
        assert_eq!(rep["candidates"], 8);
        let rows: Vec<Value> = io::jsonl_rows(&out.join("exclusions.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        let reasons: BTreeMap<String, Vec<String>> = rows.iter().map(|r| (r["id"].as_str().unwrap().to_owned(),
            r["reasons"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_owned()).collect())).collect();
        assert!(reasons["b1"].contains(&"shares_50_word_sequence_with_prior".to_owned()));
        assert_eq!(reasons["b2"], vec!["family_has_prior_snapshot_conflict"]);
        assert_eq!(reasons["c1"], vec!["family_key_in_prior_snapshot"]);
        assert_eq!(reasons["g1"], vec!["pg_id_in_prior_snapshot"]);
        // Exactly one of each within-pool pair survives, and the loser names it.
        let dup_pairs = [("a1", "f1"), ("d1", "e1")];
        for (x, y) in dup_pairs {
            assert!(reasons.contains_key(x) ^ reasons.contains_key(y), "{x}/{y}");
        }
        assert_eq!(rep["admitted"], 2);
        assert_eq!(rep["multi_family_split_components"], 2);
        let adm: Vec<Value> = io::jsonl_rows(&out.join("admitted.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        for r in &adm {
            assert_eq!(r["pool_status"], "admitted");
            assert_eq!(r["pool_admission"]["component_families"], 2);
        }
        let ad: Value = serde_json::from_slice(&std::fs::read(out.join("admission/coll_b.json")).unwrap()).unwrap();
        assert_eq!(ad["schema"], ADMISSION_SCHEMA);
        assert_eq!(ad["requested"], 3);
        assert_eq!(ad["final_test_opened"], false);
        // A changed candidate file is refused.
        let out2 = d.path().join("out2");
        std::fs::write(b.join("candidates.jsonl"), "").unwrap();
        assert!(run(&d.path().join("ix"), &[a, b], &out2, 1).is_err());
    }

    #[test]
    fn short_window_screen_keeps_one_template_member_per_collection() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("prior")).unwrap();
        std::fs::write(d.path().join("prior/records.jsonl"), format!("{}\n", json!({"text": prose("old", 30)}))).unwrap();
        let reg = d.path().join("reg.json");
        std::fs::write(&reg, json!({"schema": protected::REGISTRY_SCHEMA, "entries": [{"label": "p", "path": "prior", "names": ["*.jsonl"]}]}).to_string()).unwrap();
        protected::build(&reg, d.path(), &d.path().join("ix")).unwrap();
        // A 15-word boilerplate sentence shared by three FR documents and one wiki article.
        let boiler = "comments must be received on or before the date shown above in order to be considered fully";
        let fr = d.path().join("fr");
        write_source(&fr, "us_federal_register_historical", &[
            ("r1", "federal_register:1", format!("{} {}", prose("ra", 160), boiler)),
            ("r2", "federal_register:2", format!("{} {}", boiler, prose("rb", 160))),
            ("r3", "federal_register:3", format!("{} {} {}", prose("rc", 80), boiler, prose("rd", 80))),
            ("r4", "federal_register:4", prose("re", 170)),
        ]);
        let wk = d.path().join("wk");
        write_source(&wk, "wikipedia_enwiki_historical", &[
            ("w1", "wikipedia:page:9", format!("{} {}", prose("wa", 160), boiler)),
            ("w2", "wikipedia:page:10", format!("{} | {}", prose("wb", 80), prose("wc", 80))),
        ]);
        let out = d.path().join("out");
        let opt = Options { near_window: 12, near_collections: vec!["us_federal_register_historical".into()],
                            residue_collections: vec!["wikipedia_enwiki_historical".into()], exclude_aliases: vec![], ..Default::default() };
        let rep = run_with(&d.path().join("ix"), &[fr, wk], &out, 1, &opt).unwrap();
        assert_eq!(rep["method"], METHOD_V2);
        assert_eq!(rep["near_window_excluded"], 2);
        assert_eq!(rep["markup_residue_excluded"], 1);
        let adm: Vec<String> = io::jsonl_rows(&out.join("admitted.jsonl")).unwrap().map(|r| r.unwrap()["id"].as_str().unwrap().to_owned()).collect();
        assert_eq!(adm.iter().filter(|x| x.starts_with('r')).count(), 2, "{adm:?}");
        assert!(adm.contains(&"r4".to_owned()) && adm.contains(&"w1".to_owned()), "the screen is per collection");
        assert_eq!(rep["multi_family_split_components"], 0, "no family link from the short-window screen");
        // A listed alias excludes its family whole.
        let pg = d.path().join("pg");
        write_source(&pg, "project_gutenberg_historical", &[("p1", "author_work_v2:a:b", prose("pa", 170)), ("p2", "author_work_v2:a:b", prose("pb", 170))]);
        let opt = Options { exclude_aliases: vec!["pg:77".into()], ..Default::default() };
        let ix2 = d.path().join("ix");
        let rep = run_with(&ix2, &[pg], &d.path().join("out-pg"), 1, &opt).unwrap();
        assert_eq!(rep["admitted"], 0);
        assert_eq!(rep["method"], METHOD_V2);
        // Reverse-scan hits exclude the listed candidate's family whole.
        let hs = d.path().join("hs");
        write_source(&hs, "coll_h", &[("h1", "fam:h", prose("ha", 170)), ("h2", "fam:h", prose("hb", 170)), ("h3", "fam:k", prose("hc", 170))]);
        let mut hits = HashMap::new();
        hits.insert("h1".to_owned(), BTreeSet::from(["shares_folded_50_word_sequence_with_prior".to_owned()]));
        let opt = Options { prior_hits: Some(hits), prior_hits_file: Some((PathBuf::from("hits.jsonl"), "0".repeat(64))), ..Default::default() };
        let rep = run_with(&ix2, &[hs], &d.path().join("out-hs"), 1, &opt).unwrap();
        assert_eq!(rep["admitted"], 1);
        assert_eq!(rep["prior_hits"]["candidates"], 1);
        let exc: Vec<Value> = io::jsonl_rows(&d.path().join("out-hs/exclusions.jsonl")).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(exc.len(), 2);
        assert!(exc.iter().any(|e| e.to_string().contains("shares_folded_50_word_sequence_with_prior")));
        // A family with reserved-bucket rows leaves the pool whole.
        let rm = d.path().join("reserved-metadata.jsonl");
        std::fs::write(&rm, format!("{}\n{}\n", json!({"family_key": "fam:r", "bucket": 950}), json!({"family_key": "fam:z", "bucket": 951}))).unwrap();
        let (reserved_families, reserved_files) = load_reserved_families(&[rm.clone()]).unwrap();
        assert_eq!(reserved_files[0].2, 2);
        let rs = d.path().join("rs");
        write_source(&rs, "coll_r", &[("q1", "fam:r", prose("qa", 170)), ("q2", "fam:r", prose("qb", 170)), ("q3", "fam:s", prose("qc", 170))]);
        let opt = Options { reserved_families, reserved_files, ..Default::default() };
        let rep = run_with(&ix2, &[rs], &d.path().join("out-rs"), 1, &opt).unwrap();
        assert_eq!(rep["admitted"], 1);
        assert_eq!(rep["method"], METHOD_V2);
        assert_eq!(rep["options"]["reserved_families"], 2);
        std::fs::write(&rm, format!("{}\n", json!({"family_key": "fam:r", "text": "x"}))).unwrap();
        assert!(load_reserved_families(&[rm]).is_err(), "reserved metadata never carries text");
    }
}

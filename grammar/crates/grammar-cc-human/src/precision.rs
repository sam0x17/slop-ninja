//! Domain precision review of the pilot pool.
//!
//! - `sample`: per cohort domain (general_web excluded), a seeded draw of
//!   admitted pilot documents, allocated to crawl years in proportion to the
//!   domain's admitted documents per year (largest remainder) and drawn by
//!   ascending keyed rank inside each year, so each domain's sample is
//!   self-weighting. The page text is joined from `pages/` so that changed
//!   rules can be re-run on exactly what the pilot saw.
//! - `score`: joins hand labels to the sample and writes precision with
//!   Wilson intervals, register match, excluded categories, the confusion
//!   table and, for a second rule set, how each sampled document would move.
//! - `reassign`: re-runs a rule set over every admitted page and counts the
//!   domain transitions and new exclusions, by rule id, with the email
//!   channel split (press release, list archive, cues only) before and
//!   after; it can also write the per-document assignment.
//! - A sample drawn from such an assignment (`sample` with `assignments`)
//!   stratifies on the re-assigned domain, so a fresh draw measures the new
//!   rules directly, and withholds documents already read (an id list).
//!
//! Reviewed documents are exposed: they never enter cohorts (pilot
//! documents never do). No network, model or detector call happens here.
use crate::{
    domain::{self, Rules},
    measure::{self, crawl_year, file_sha256, wilson, write_jsonl},
    pages, screens, store,
};
use anyhow::{Context, Result, ensure};
use grammar_a_pools::splits::rank;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::Path,
};

pub const SCOPE: &str = "cc_domain_precision_v1";
pub const EXCLUDED_CATEGORIES: &[&str] = &["adult", "piracy", "ao3_or_objecting_community", "machine_text", "boilerplate"];

/// The admitted-pool fields the review reads.
#[derive(Deserialize)]
pub struct PoolRow {
    pub id: String,
    pub crawl: String,
    pub domain: String,
    pub domain_rule: String,
    #[serde(default)]
    pub cues: BTreeMap<String, usize>,
    pub group: String,
    pub host: String,
    pub registered_domain: String,
    pub url: String,
    pub warc_date: String,
    pub wet_path: String,
    pub record_id: String,
    pub words: usize,
    pub text: String,
    pub text_sha256: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SampleRow {
    pub n: usize,
    pub id: String,
    pub domain: String,
    pub domain_rule: String,
    pub cues: BTreeMap<String, usize>,
    pub year: u16,
    pub crawl: String,
    pub group: String,
    pub host: String,
    pub registered_domain: String,
    pub url: String,
    pub warc_date: String,
    pub record_id: String,
    pub wet_path: String,
    pub words: usize,
    pub rank: String,
    pub text: String,
    pub text_sha256: String,
    pub page_text: String,
    pub page_sha256: String,
    /// The domain and rule the pool was built with, when the sample is drawn
    /// from a re-assignment (`domain`/`domain_rule` are then the new ones).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_rule: Option<String>,
}

/// Largest-remainder (Hamilton) allocation of `total` over `counts`, never
/// above a stratum's size; ties go to the earlier stratum.
pub fn allocate(counts: &[usize], total: usize) -> Vec<usize> {
    let n: usize = counts.iter().sum();
    let total = total.min(n);
    if n == 0 {
        return vec![0; counts.len()];
    }
    let mut out: Vec<usize> = counts.iter().map(|&c| c * total / n).collect();
    let mut rem: Vec<(usize, usize)> = counts.iter().enumerate().map(|(i, &c)| ((c * total) % n, i)).collect();
    rem.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut left = total - out.iter().sum::<usize>();
    for &(_, i) in rem.iter().cycle().take(counts.len() * 2) {
        if left == 0 {
            break;
        }
        if out[i] < counts[i] {
            out[i] += 1;
            left -= 1;
        }
    }
    out
}

/// Tier of a rule id (`news:weak_path:3` → `weak_path`).
pub fn rule_tier(rule: &str) -> &str {
    rule.split(':').nth(1).unwrap_or("unknown")
}

pub struct SampleArgs<'a> {
    pub root: &'a Path,
    pub out: &'a Path,
    pub seed: &'a str,
    pub per_domain: usize,
    /// Per-document assignment written by `reassign`: stratify on its
    /// domain (excluded documents are skipped) instead of the pool's.
    pub assignments: Option<&'a Path>,
    /// The rules that produced `assignments`; every pick is re-run with them
    /// on the joined page text and must reproduce its assignment.
    pub rules: Option<&'a Rules>,
    /// Ids that must not be drawn (documents already read).
    pub withhold: &'a HashSet<String>,
}

/// One document's re-assignment (`reassign --per-doc`); no text.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DocAssignment {
    pub id: String,
    pub crawl: String,
    pub pool_domain: String,
    pub pool_rule: String,
    pub domain: String,
    pub rule: String,
    pub excluded: Option<String>,
}

/// Ids from a list file: one per line, or JSONL rows with an `id` field;
/// blank lines and `#` comments are skipped.
pub fn read_ids(path: &Path) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    for (i, l) in fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?
        .lines()
        .enumerate()
    {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let id = if l.starts_with('{') {
            let v: Value = serde_json::from_str(l).with_context(|| format!("{} line {}", path.display(), i + 1))?;
            v["id"].as_str().with_context(|| format!("{} line {}: no id", path.display(), i + 1))?.to_owned()
        } else {
            l.to_owned()
        };
        out.insert(id);
    }
    Ok(out)
}

pub fn sample(a: &SampleArgs) -> Result<Value> {
    ensure!(!a.out.join("SAMPLE.json").exists(), "{} already holds a sample", a.out.display());
    ensure!(
        a.assignments.is_none() || a.rules.is_some(),
        "a sample from assignments needs the rules that made them"
    );
    let pool_path = a.root.join("pool/admitted.jsonl.zst");
    let pool: Vec<PoolRow> = store::rows(&pool_path)?;
    let assigned: Option<HashMap<String, DocAssignment>> = match a.assignments {
        Some(p) => {
            let rows: Vec<DocAssignment> = store::rows(p)?;
            ensure!(rows.len() == pool.len(), "{} assignments for {} pool rows", rows.len(), pool.len());
            Some(rows.into_iter().map(|r| (r.id.clone(), r)).collect())
        }
        None => None,
    };
    // The stratum (domain, rule) of each pool row under the sampling frame.
    let frame = |r: &PoolRow| -> Result<Option<(String, String)>> {
        Ok(match &assigned {
            None => Some((r.domain.clone(), r.domain_rule.clone())),
            Some(m) => {
                let d = m.get(&r.id).with_context(|| format!("{}: no assignment", r.id))?;
                ensure!(d.pool_domain == r.domain && d.pool_rule == r.domain_rule, "{}: assignment is for another pool", r.id);
                d.excluded.is_none().then(|| (d.domain.clone(), d.rule.clone()))
            }
        })
    };
    // domain -> year -> (rank, index); admitted counts include withheld ids.
    let mut cells: BTreeMap<String, BTreeMap<u16, Vec<(u64, usize)>>> = BTreeMap::new();
    let mut admitted: BTreeMap<String, BTreeMap<u16, usize>> = BTreeMap::new();
    let mut withheld: BTreeMap<String, usize> = BTreeMap::new();
    let mut strata: Vec<Option<(String, String)>> = Vec::with_capacity(pool.len());
    for (i, r) in pool.iter().enumerate() {
        let st = frame(r)?;
        if let Some((dom, _)) = &st
            && dom != domain::GENERAL
        {
            let y = crawl_year(&r.crawl)?;
            *admitted.entry(dom.clone()).or_default().entry(y).or_default() += 1;
            let cell = cells.entry(dom.clone()).or_default().entry(y).or_default();
            if a.withhold.contains(&r.id) {
                *withheld.entry(dom.clone()).or_default() += 1;
            } else {
                cell.push((rank(a.seed, SCOPE, &r.id), i));
            }
        }
        strata.push(st);
    }
    let mut picks: Vec<(usize, u64)> = Vec::new();
    let mut alloc_json = BTreeMap::new();
    for (dom, years) in &mut cells {
        let ys: Vec<u16> = years.keys().copied().collect();
        let counts: Vec<usize> = ys.iter().map(|y| years[y].len()).collect();
        let k = allocate(&counts, a.per_domain);
        let mut aj = BTreeMap::new();
        for (j, y) in ys.iter().enumerate() {
            let v = years.get_mut(y).unwrap();
            v.sort_unstable();
            for &(rk, i) in v.iter().take(k[j]) {
                picks.push((i, rk));
            }
            let adm = admitted[dom][y];
            aj.insert(
                y.to_string(),
                if a.withhold.is_empty() {
                    json!({"admitted": adm, "sampled": k[j]})
                } else {
                    json!({"admitted": adm, "eligible": counts[j], "sampled": k[j]})
                },
            );
        }
        alloc_json.insert(
            dom.clone(),
            json!({"admitted": admitted[dom].values().sum::<usize>(), "years": aj}),
        );
    }
    // Join page text from pass 1, one pages file at a time.
    let mut by_file: BTreeMap<String, HashMap<String, usize>> = BTreeMap::new();
    for (j, (i, _)) in picks.iter().enumerate() {
        let r = &pool[*i];
        by_file.entry(pages::stem(&r.wet_path)).or_default().insert(r.record_id.clone(), j);
    }
    let mut page_of: HashMap<usize, (String, String)> = HashMap::new();
    for (st, want) in &by_file {
        let rows: Vec<PageText> = store::rows(&pages::pages_path(a.root, st))?;
        for p in rows {
            if let Some(&j) = want.get(&p.record_id) {
                page_of.insert(j, (p.text, p.page_sha256));
            }
        }
    }
    let mut rows = Vec::with_capacity(picks.len());
    for (j, (i, rk)) in picks.iter().enumerate() {
        let r = &pool[*i];
        let (page_text, page_sha256) = page_of.remove(&j).with_context(|| format!("{}: page not found in pages/", r.id))?;
        ensure!(measure::sha256_hex(r.text.as_bytes()) == r.text_sha256, "{}: pool text hash", r.id);
        let (dom, rule) = strata[*i].clone().context("picked outside the frame")?;
        let (cues, pool_domain, pool_rule) = match (&assigned, a.rules) {
            (Some(_), Some(rules)) => (rules.cues(&page_text), Some(r.domain.clone()), Some(r.domain_rule.clone())),
            _ => (r.cues.clone(), None, None),
        };
        let row = SampleRow {
            n: j,
            id: r.id.clone(),
            domain: dom,
            domain_rule: rule,
            cues,
            year: crawl_year(&r.crawl)?,
            crawl: r.crawl.clone(),
            group: r.group.clone(),
            host: r.host.clone(),
            registered_domain: r.registered_domain.clone(),
            url: r.url.clone(),
            warc_date: r.warc_date.clone(),
            record_id: r.record_id.clone(),
            wet_path: r.wet_path.clone(),
            words: r.words,
            rank: format!("{rk:016x}"),
            text: r.text.clone(),
            text_sha256: r.text_sha256.clone(),
            page_text,
            page_sha256,
            pool_domain,
            pool_rule,
        };
        if let (Some(_), Some(rules)) = (&assigned, a.rules) {
            let (d, rl, ex) = rerun(rules, &row);
            ensure!(
                ex.is_none() && d == row.domain && rl == row.domain_rule,
                "{}: the rules give {d} / {rl} / {ex:?}, the assignment {} / {}",
                row.id,
                row.domain,
                row.domain_rule
            );
        }
        rows.push(row);
    }
    fs::create_dir_all(a.out)?;
    let sha = write_jsonl(&a.out.join("sample.jsonl"), &rows)?;
    let groups: BTreeMap<&str, usize> = cells
        .keys()
        .map(|d| {
            (
                d.as_str(),
                rows.iter()
                    .filter(|r| &r.domain == d)
                    .map(|r| r.group.as_str())
                    .collect::<HashSet<_>>()
                    .len(),
            )
        })
        .collect();
    let mut summary = json!({
        "schema": "slop_ninja_cc_human_precision_sample_v1",
        "pool": pool_path, "pool_sha256": file_sha256(&pool_path)?, "pool_rows": pool.len(),
        "seed": a.seed, "scope": SCOPE, "per_domain": a.per_domain,
        "rule": "per cohort domain (general_web excluded): largest-remainder allocation over crawl years in proportion to admitted documents, then ascending rank(seed, scope, id) within each year; self-weighting within a domain",
        "selected": rows.len(), "sample_sha256": sha, "distinct_groups": groups, "allocation": alloc_json,
        "exposure": "every sampled document is read by a reviewer and is exposed",
    });
    if let (Some(p), Some(rules)) = (a.assignments, a.rules) {
        summary["schema"] = json!("slop_ninja_cc_human_precision_sample_v2");
        summary["rule"] = json!("per cohort domain of the re-assignment (general_web and excluded documents skipped): largest-remainder allocation over crawl years in proportion to the documents eligible to be drawn (withheld ids removed), then ascending rank(seed, scope, id) within each year; self-weighting within a domain. `admitted` counts the domain's documents including withheld ones; every pick is re-run with the rules and reproduces its assignment");
        summary["assignments"] = json!(p);
        summary["assignments_sha256"] = json!(file_sha256(p)?);
        summary["rules"] = json!(rules.version);
        summary["rules_sha256"] = json!(rules.sha256);
        summary["screens"] = json!(screens::SCREENS_VERSION);
        summary["excerpt_screens"] = json!(screens::EXCERPT_SCREENS_VERSION);
    }
    if !a.withhold.is_empty() {
        let mut ids: Vec<&String> = a.withhold.iter().collect();
        ids.sort();
        let joined = ids.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
        summary["withheld"] = json!({"ids": ids.len(), "ids_sha256": measure::sha256_hex(joined.as_bytes()),
            "in_frame_by_domain": withheld, "note": "ids sorted and joined by newlines before hashing"});
    }
    store::write_json(&a.out.join("SAMPLE.json"), &summary)?;
    Ok(json!({"selected": rows.len()}))
}

#[derive(Deserialize)]
struct PageText {
    record_id: String,
    page_sha256: String,
    text: String,
}

// ------------------------------------------------------------------ labels

/// One hand label. `true_domain` is a cohort domain or `general_web`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Label {
    pub id: String,
    pub true_domain: String,
    pub register: String,
    pub register_match: bool,
    #[serde(default)]
    pub excluded: Vec<String>,
    #[serde(default)]
    pub note: String,
}

pub fn check_label(l: &Label, names: &[&str]) -> Result<()> {
    ensure!(
        l.true_domain == domain::GENERAL || names.contains(&l.true_domain.as_str()),
        "{}: unknown true_domain {}",
        l.id,
        l.true_domain
    );
    for e in &l.excluded {
        ensure!(EXCLUDED_CATEGORIES.contains(&e.as_str()), "{}: unknown excluded category {e}", l.id);
    }
    ensure!(!l.register.is_empty(), "{}: empty register", l.id);
    Ok(())
}

fn rate(k: usize, n: usize) -> Value {
    let (lo, hi) = wilson(k, n);
    json!({"k": k, "n": n, "p": if n > 0 { k as f64 / n as f64 } else { f64::NAN }, "wilson95": [lo, hi]})
}

/// Wilson 95% interval for a proportion `p` from an effective size `n`.
pub fn wilson_p(p: f64, n: f64) -> (f64, f64) {
    if n <= 0.0 {
        return (0.0, 1.0);
    }
    let z = 1.959963984540054;
    let d = 1.0 + z * z / n;
    let c = (p + z * z / (2.0 * n)) / d;
    let h = z * ((p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt()) / d;
    ((c - h).max(0.0), (c + h).min(1.0))
}

/// Stratified estimate Σ w_d p_d with a normal 95% interval and a Wilson
/// interval at the effective size n_eff = p(1-p)/Var (Kish; when every
/// stratum is 0 or 1, n_eff = 1/Σ w_d²/n_d), capped at Σ n_d. Weights are
/// normalised over the domains present.
pub fn weighted(cells: &[(f64, usize, usize)]) -> Value {
    let wsum: f64 = cells.iter().filter(|c| c.2 > 0).map(|c| c.0).sum();
    let (mut p, mut v, mut w2n, mut ntot) = (0.0, 0.0, 0.0, 0usize);
    for &(w, k, n) in cells {
        if n == 0 || wsum <= 0.0 {
            continue;
        }
        let (w, pd) = (w / wsum, k as f64 / n as f64);
        p += w * pd;
        v += w * w * pd * (1.0 - pd) / n as f64;
        w2n += w * w / n as f64;
        ntot += n;
    }
    let h = 1.959963984540054 * v.sqrt();
    let n_eff = if v > 0.0 {
        p * (1.0 - p) / v
    } else if w2n > 0.0 {
        1.0 / w2n
    } else {
        0.0
    }
    .min(ntot as f64);
    let (lo, hi) = wilson_p(p, n_eff);
    json!({"p": p, "normal95": [(p - h).max(0.0), (p + h).min(1.0)], "n_eff": n_eff, "wilson95_neff": [lo, hi]})
}

pub struct ScoreArgs<'a> {
    pub sample: &'a Path,
    pub labels: &'a Path,
    pub pool_counts: &'a BTreeMap<String, usize>,
    pub mix: &'a BTreeMap<String, f64>,
    pub rules_new: Option<&'a Rules>,
    pub out: &'a Path,
}

/// What a second rule set does to one sampled document.
pub fn rerun(rules: &Rules, r: &SampleRow) -> (String, String, Option<&'static str>) {
    let path = domain::url_path(&r.url);
    let a = rules.assign_with_excerpt(&r.host, path, &r.page_text, &r.text);
    let ex = screens::url_exclusion(&r.host, &r.registered_domain, &HashSet::new())
        .or_else(|| screens::path_exclusion(&r.host, path))
        .or_else(|| screens::excerpt_exclusion(&r.text));
    (a.domain, a.rule, ex)
}

pub fn score(a: &ScoreArgs) -> Result<Value> {
    let sample: Vec<SampleRow> = store::rows_plain(a.sample)?;
    let labels: Vec<Label> = store::rows_plain(a.labels)?;
    let names_owned: Vec<String> = sample
        .iter()
        .map(|r| r.domain.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let names: Vec<&str> = names_owned.iter().map(String::as_str).collect();
    let mut lab: HashMap<&str, &Label> = HashMap::new();
    for l in &labels {
        check_label(l, &names)?;
        ensure!(lab.insert(l.id.as_str(), l).is_none(), "{}: labelled twice", l.id);
    }
    ensure!(
        labels.len() == sample.len(),
        "{} labels for {} sampled documents",
        labels.len(),
        sample.len()
    );
    let mut per = BTreeMap::new();
    let mut confusion: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut by_tier: BTreeMap<String, BTreeMap<String, (usize, usize)>> = BTreeMap::new();
    let mut wcells = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut moves = BTreeMap::new();
    for d in &names {
        let rows: Vec<&SampleRow> = sample.iter().filter(|r| r.domain == *d).collect();
        let n = rows.len();
        let mut correct = 0;
        let mut usable = 0;
        let mut reg_ok = 0;
        let mut excl: BTreeMap<String, usize> = BTreeMap::new();
        let mut registers: BTreeMap<String, usize> = BTreeMap::new();
        let mut years: BTreeMap<u16, (usize, usize)> = BTreeMap::new();
        // (domain-correct, usable, n) by rule id and by where the pool had it.
        let mut by_rule: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
        let mut by_origin: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
        for r in &rows {
            let l = lab.get(r.id.as_str()).with_context(|| format!("{}: no label", r.id))?;
            let ok = l.true_domain == *d;
            let clean = l.excluded.is_empty();
            let use_ok = ok && clean && l.register_match;
            let e = by_rule.entry(r.domain_rule.clone()).or_default();
            *e = (e.0 + ok as usize, e.1 + use_ok as usize, e.2 + 1);
            if let Some(pd) = &r.pool_domain {
                let o = if pd == d {
                    "same_pool_domain"
                } else if pd == domain::GENERAL {
                    "from_general_web"
                } else {
                    "from_other_domain"
                };
                let e = by_origin.entry(o).or_default();
                *e = (e.0 + ok as usize, e.1 + use_ok as usize, e.2 + 1);
            }
            correct += ok as usize;
            usable += use_ok as usize;
            reg_ok += (ok && l.register_match) as usize;
            for e in &l.excluded {
                *excl.entry(e.clone()).or_default() += 1;
            }
            *registers.entry(l.register.clone()).or_default() += 1;
            *confusion.entry((*d).to_owned()).or_default().entry(l.true_domain.clone()).or_default() += 1;
            let t = by_tier
                .entry((*d).to_owned())
                .or_default()
                .entry(rule_tier(&r.domain_rule).to_owned())
                .or_default();
            t.0 += ok as usize;
            t.1 += 1;
            let y = years.entry(r.year).or_default();
            y.0 += ok as usize;
            y.1 += 1;
        }
        let w_pool = *a.pool_counts.get(*d).unwrap_or(&0) as f64;
        let w_mix = *a.mix.get(*d).unwrap_or(&0.0);
        wcells.0.push((w_pool, correct, n));
        wcells.1.push((w_mix, correct, n));
        wcells.2.push((w_pool, usable, n));
        wcells.3.push((w_mix, usable, n));
        let excluded_any = rows.iter().filter(|r| !lab[r.id.as_str()].excluded.is_empty()).count();
        let split = |m: &BTreeMap<String, (usize, usize, usize)>| -> BTreeMap<String, Value> {
            m.iter()
                .map(|(k, (c, u, n))| (k.clone(), json!({"domain_correct": [c, n], "usable": rate(*u, *n)})))
                .collect()
        };
        let mut pd = json!({
            "n": n,
            "domain_precision": rate(correct, n),
            "register_match_given_domain": rate(reg_ok, correct),
            "usable_precision": rate(usable, n),
            "excluded_any": rate(excluded_any, n),
            "excluded": excl,
            "registers": registers,
            "by_rule_tier": by_tier[*d].iter().map(|(t, (k, n))| (t.clone(), rate(*k, *n))).collect::<BTreeMap<_, _>>(),
            "by_rule": split(&by_rule),
            "by_year": years.iter().map(|(y, (k, n))| (y.to_string(), json!([k, n]))).collect::<BTreeMap<_, _>>(),
        });
        if !by_origin.is_empty() {
            let m: BTreeMap<String, (usize, usize, usize)> = by_origin.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect();
            pd["by_pool_origin"] = json!(split(&m));
        }
        per.insert((*d).to_owned(), pd);
        if let Some(rn) = a.rules_new {
            let mut kept = (0usize, 0usize);
            let mut lost_correct = 0usize;
            let mut to: BTreeMap<String, usize> = BTreeMap::new();
            let mut gained_usable = 0usize;
            for r in &rows {
                let l = lab[r.id.as_str()];
                let (nd, _rule, ex) = rerun(rn, r);
                let dest = match ex {
                    Some(e) => format!("excluded:{e}"),
                    None => nd.clone(),
                };
                *to.entry(dest).or_default() += 1;
                let ok = l.true_domain == *d && l.excluded.is_empty() && l.register_match;
                if ex.is_none() && nd == *d {
                    kept.1 += 1;
                    kept.0 += ok as usize;
                } else if ok {
                    lost_correct += 1;
                }
                if ex.is_none() && nd != *d && l.true_domain == nd && l.excluded.is_empty() {
                    gained_usable += 1;
                }
            }
            moves.insert(
                (*d).to_owned(),
                json!({"destinations": to, "usable_precision_of_kept": rate(kept.0, kept.1),
                       "usable_documents_lost": lost_correct, "moved_to_their_true_domain": gained_usable}),
            );
        }
    }
    // Per-document rows (ids, rules and labels only; no text).
    let docs: Vec<Value> = sample
        .iter()
        .map(|r| {
            let l = lab[r.id.as_str()];
            let mut v = json!({"n": r.n, "id": r.id, "year": r.year, "domain": r.domain, "rule": r.domain_rule,
                "true_domain": l.true_domain, "register": l.register, "register_match": l.register_match, "excluded": l.excluded,
                "usable": l.true_domain == r.domain && l.excluded.is_empty() && l.register_match});
            if let (Some(pd), Some(pr)) = (&r.pool_domain, &r.pool_rule) {
                v["pool_domain"] = json!(pd);
                v["pool_rule"] = json!(pr);
            }
            if let Some(rn) = a.rules_new {
                let (nd, rule, ex) = rerun(rn, r);
                v["rerun"] = json!({"domain": nd, "rule": rule, "excluded": ex,
                    "excerpt_cues": rn.cues(&r.text), "page_cues": rn.cues(&r.page_text)});
            }
            v
        })
        .collect();
    let out = json!({
        "schema": "slop_ninja_cc_human_precision_score_v1",
        "documents": docs,
        "sample": a.sample, "sample_sha256": file_sha256(a.sample)?,
        "labels": a.labels, "labels_sha256": file_sha256(a.labels)?,
        "definitions": {
            "domain_precision": "true_domain equals the assigned domain",
            "register_match_given_domain": "register fits the cohort definition, among domain-correct documents",
            "usable_precision": "domain correct, register match and no excluded category",
            "pool_weighted": "domains weighted by admitted documents (general_web excluded)",
            "mix_weighted": "domains weighted by the launch shares given",
        },
        "per_domain": per,
        "pool_weighted": {"domain_precision": weighted(&wcells.0), "usable_precision": weighted(&wcells.2)},
        "mix_weighted": {"domain_precision": weighted(&wcells.1), "usable_precision": weighted(&wcells.3), "mix": a.mix},
        "confusion_assigned_to_true": confusion,
        "rerun": a.rules_new.map(|r| json!({"rules": r.version, "rules_sha256": r.sha256,
            "screens": screens::SCREENS_VERSION, "caveat": "kept-precision is in-sample, and optimistic, when the rules were changed after reading these labels (precision-v1); on a fresh draw from this rule set's re-assignment every document is kept and it equals usable precision",
            "per_domain": moves})),
    });
    store::write_json(a.out, &out)?;
    Ok(out)
}

// ------------------------------------------------------------------ reassign

/// Wire and press-release hosts among the email rules' hosts.
pub const PRESS_WIRE_HOSTS: &[&str] = &[
    "businesswire.com",
    "prnewswire.com",
    "globenewswire.com",
    "marketwired.com",
    "prweb.com",
    "einpresswire.com",
    "openpr.com",
    "prlog.org",
];

/// The email/business channel a rule id stands for, read with the rules
/// that produced it: `press_release` (a wire host, or a press- or
/// news-release path), `cue_only` (content cues), `list_archive` (any other
/// email URL rule), `unknown` (an email id the rules do not hold). `None`
/// for other domains. This is the split behind the pilot pool's 1,351 /
/// 118 / 149 email count under rules v1.
pub fn email_channel(rules: &Rules, rule: &str) -> Option<&'static str> {
    if !rule.starts_with("email:") {
        return None;
    }
    Some(match rules.rule_source(rule) {
        Some(("content_cues", _)) => "cue_only",
        Some(("host" | "weak_host", h)) if PRESS_WIRE_HOSTS.iter().any(|w| h == *w) => "press_release",
        Some((_, src)) if src.contains("press") || src.contains("news-?release") => "press_release",
        Some(_) => "list_archive",
        None => "unknown",
    })
}

#[derive(Deserialize)]
struct PageFull {
    record_id: String,
    url: String,
    host: String,
    registered_domain: String,
    text: String,
}

#[derive(Deserialize)]
struct PoolKey {
    id: String,
    crawl: String,
    record_id: String,
    wet_path: String,
    domain: String,
    domain_rule: String,
    text: String,
}

pub struct ReassignArgs<'a> {
    pub root: &'a Path,
    /// The rules to re-run.
    pub rules: &'a Rules,
    /// The rules the pool was built with (to read its rule ids).
    pub pool_rules: &'a Rules,
    pub threads: usize,
    pub out: &'a Path,
    /// Write every document's re-assignment here (zstd JSONL, no text).
    pub per_doc: Option<&'a Path>,
    /// Domains whose rule-to-rule transitions are written in full.
    pub detail: &'a [String],
}

/// Re-run `rules` (and the current screens) over every admitted page:
/// counts by (old domain, new domain or exclusion), by new rule id, the
/// email channel split before and after, and rule-to-rule transitions for
/// the `detail` domains.
pub fn reassign(a: &ReassignArgs) -> Result<Value> {
    let (root, rules) = (a.root, a.rules);
    let pool: Vec<PoolKey> = store::rows(&root.join("pool/admitted.jsonl.zst"))?;
    let mut by_file: BTreeMap<String, HashMap<String, usize>> = BTreeMap::new();
    for (i, r) in pool.iter().enumerate() {
        by_file.entry(pages::stem(&r.wet_path)).or_default().insert(r.record_id.clone(), i);
    }
    let files: Vec<(&String, &HashMap<String, usize>)> = by_file.iter().collect();
    let tp = rayon::ThreadPoolBuilder::new().num_threads(a.threads.max(1)).build()?;
    type Part = Vec<(usize, String, String, Option<&'static str>)>;
    let parts: Vec<Result<Part>> = tp.install(|| {
        files
            .par_iter()
            .map(|(st, want)| {
                let mut m = Vec::with_capacity(want.len());
                let f = fs::File::open(pages::pages_path(root, st))?;
                let rd = std::io::BufRead::lines(std::io::BufReader::with_capacity(1 << 20, zstd::Decoder::new(f)?));
                for l in rd {
                    let l = l?;
                    // Cheap pre-filter on the record id before a full parse.
                    let Some(k) = l.find("\"record_id\":\"") else {
                        continue;
                    };
                    let rid = &l[k + 13..];
                    let Some(e) = rid.find('"') else { continue };
                    let Some(&i) = want.get(&rid[..e]) else {
                        continue;
                    };
                    let p: PageFull = serde_json::from_str(&l)?;
                    debug_assert_eq!(p.record_id, pool[i].record_id);
                    let path = domain::url_path(&p.url);
                    let asg = rules.assign_with_excerpt(&p.host, path, &p.text, &pool[i].text);
                    let ex = screens::url_exclusion(&p.host, &p.registered_domain, &HashSet::new())
                        .or_else(|| screens::path_exclusion(&p.host, path))
                        .or_else(|| screens::excerpt_exclusion(&pool[i].text));
                    m.push((i, asg.domain, asg.rule, ex));
                }
                ensure!(m.len() == want.len(), "{st}: found {} of {} admitted pages", m.len(), want.len());
                Ok(m)
            })
            .collect()
    });
    let mut docs: Vec<Option<(String, String, Option<&'static str>)>> = vec![None; pool.len()];
    for p in parts {
        for (i, d, r, ex) in p? {
            ensure!(docs[i].replace((d, r, ex)).is_none(), "{}: seen twice", pool[i].id);
        }
    }
    let mut total: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut by_rule: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut rule_tr: BTreeMap<String, BTreeMap<String, BTreeMap<String, usize>>> = BTreeMap::new();
    let mut ch_before: BTreeMap<String, usize> = BTreeMap::new();
    let mut ch_after: BTreeMap<String, usize> = BTreeMap::new();
    let mut ch_tr: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut new_rules: HashSet<String> = HashSet::new();
    let mut old_rules: HashSet<String> = HashSet::new();
    let mut w = a.per_doc.map(store::ZWriter::create).transpose()?;
    for (r, d) in pool.iter().zip(&docs) {
        let (nd, nr, ex) = d.as_ref().with_context(|| format!("{}: not re-assigned", r.id))?;
        let dest = match ex {
            Some(e) => format!("excluded:{e}"),
            None => nd.clone(),
        };
        *total.entry(r.domain.clone()).or_default().entry(dest.clone()).or_default() += 1;
        *by_rule.entry(dest.clone()).or_default().entry(nr.clone()).or_default() += 1;
        new_rules.insert(nr.clone());
        if a.detail.contains(&r.domain) || (ex.is_none() && a.detail.contains(nd)) {
            *rule_tr
                .entry(r.domain_rule.clone())
                .or_default()
                .entry(dest.clone())
                .or_default()
                .entry(nr.clone())
                .or_default() += 1;
            old_rules.insert(r.domain_rule.clone());
        }
        let before = email_channel(a.pool_rules, &r.domain_rule);
        let after = if ex.is_none() { email_channel(rules, nr) } else { None };
        if let Some(b) = before {
            *ch_before.entry(b.to_owned()).or_default() += 1;
        }
        if let Some(c) = after {
            *ch_after.entry(c.to_owned()).or_default() += 1;
        }
        if before.is_some() || after.is_some() {
            let from = before.map(str::to_owned).unwrap_or_else(|| format!("from:{}", r.domain));
            let to = after.map(str::to_owned).unwrap_or_else(|| dest.clone());
            *ch_tr.entry(from).or_default().entry(to).or_default() += 1;
        }
        if let Some(w) = w.as_mut() {
            w.write(&DocAssignment {
                id: r.id.clone(),
                crawl: r.crawl.clone(),
                pool_domain: r.domain.clone(),
                pool_rule: r.domain_rule.clone(),
                domain: nd.clone(),
                rule: nr.clone(),
                excluded: ex.map(str::to_owned),
            })?;
        }
    }
    let per_doc = match w {
        Some(w) => {
            let (sha, rows) = w.finish()?;
            json!({"path": a.per_doc, "rows": rows, "jsonl_sha256": sha})
        }
        None => Value::Null,
    };
    let before: BTreeMap<&String, usize> = total.iter().map(|(k, v)| (k, v.values().sum())).collect();
    let mut after: BTreeMap<String, usize> = BTreeMap::new();
    for v in total.values() {
        for (k, n) in v {
            *after.entry(k.clone()).or_default() += n;
        }
    }
    let sources = |rs: &Rules, ids: &HashSet<String>| -> BTreeMap<String, Value> {
        ids.iter()
            .filter_map(|id| rs.rule_source(id).map(|(k, s)| (id.clone(), json!([k, s]))))
            .collect()
    };
    let res = json!({
        "schema": "slop_ninja_cc_human_reassign_v2",
        "rules": rules.version, "rules_sha256": rules.sha256, "screens": screens::SCREENS_VERSION,
        "excerpt_screens": screens::EXCERPT_SCREENS_VERSION,
        "pool_rules": a.pool_rules.version, "pool_rules_sha256": a.pool_rules.sha256,
        "admitted": pool.len(), "before": before, "after": after, "transitions": total,
        "after_by_rule": by_rule,
        "email_channels": {
            "definition": "press_release: a wire host or a press-/news-release path; list_archive: any other email URL rule; cue_only: content cues. 'before' reads the pool's rule ids with pool_rules, 'after' the new ids with rules (excluded documents have no channel)",
            "before": ch_before, "after": ch_after, "transitions": ch_tr,
        },
        "detail_domains": a.detail,
        "rule_transitions": rule_tr,
        "rule_sources": {"rules": sources(rules, &new_rules), "pool_rules": sources(a.pool_rules, &old_rules)},
        "per_doc": per_doc,
        "note": "URL exclusions here are the current lists only (FineWeb's list and the other pass-1 gates already applied); rule_transitions cover documents whose pool domain, or kept new domain, is a detail domain",
    });
    store::write_json(a.out, &res)?;
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_is_proportional_capped_and_exact() {
        assert_eq!(allocate(&[10, 30, 60], 10), vec![1, 3, 6]);
        assert_eq!(allocate(&[1, 1, 1], 2), vec![1, 1, 0]);
        assert_eq!(allocate(&[2, 100], 50).iter().sum::<usize>(), 50);
        assert_eq!(allocate(&[3, 4], 50), vec![3, 4]);
        assert_eq!(allocate(&[], 5), Vec::<usize>::new());
        let a = allocate(&[5, 0, 995], 50);
        assert_eq!(a.iter().sum::<usize>(), 50);
        assert_eq!(a[1], 0);
    }

    #[test]
    fn weighted_estimate_matches_hand_value() {
        let v = weighted(&[(3.0, 45, 50), (1.0, 25, 50)]);
        let p = v["p"].as_f64().unwrap();
        assert!((p - (0.75 * 0.9 + 0.25 * 0.5)).abs() < 1e-12);
        let lo = v["normal95"][0].as_f64().unwrap();
        assert!(lo < p && lo > 0.6);
        // One stratum: the effective size is n and the interval is Wilson's.
        let one = weighted(&[(1.0, 45, 50)]);
        assert!((one["n_eff"].as_f64().unwrap() - 50.0).abs() < 1e-9);
        let (wl, wh) = wilson(45, 50);
        assert!((one["wilson95_neff"][0].as_f64().unwrap() - wl).abs() < 1e-12);
        assert!((one["wilson95_neff"][1].as_f64().unwrap() - wh).abs() < 1e-12);
        // All strata at 1: n_eff = 1 / sum(w^2 / n), and the interval stays below 1.
        let all = weighted(&[(3.0, 50, 50), (1.0, 50, 50)]);
        let ne = all["n_eff"].as_f64().unwrap();
        assert!((ne - 1.0 / (0.5625 / 50.0 + 0.0625 / 50.0)).abs() < 1e-9);
        assert!(all["wilson95_neff"][0].as_f64().unwrap() < 1.0);
        // Uneven weights shrink the effective size below the total; a
        // stratification gain is capped at the total.
        let uneven = weighted(&[(9.0, 40, 50), (1.0, 40, 50)]);
        assert!((uneven["n_eff"].as_f64().unwrap() - 50.0 / 0.82).abs() < 1e-9);
        assert_eq!(v["n_eff"].as_f64().unwrap(), 100.0);
    }

    #[test]
    fn email_channels_reproduce_the_v1_split_and_read_v2_ids() {
        let v1 = Rules::pilot_v1();
        let v2 = Rules::pilot_v2();
        let ch = |r: &Rules, host: &str, path: &str| {
            let a = r.assign(host, path, "");
            (a.rule.clone(), email_channel(r, &a.rule))
        };
        assert_eq!(ch(&v1, "www.prweb.com", "/releases/2014/5/prweb1.htm").1, Some("press_release"));
        assert_eq!(ch(&v1, "www.example.com", "/press-releases/a-new-product").1, Some("press_release"));
        assert_eq!(ch(&v1, "www.example.com", "/news-release/a-new-product").1, Some("press_release"));
        assert_eq!(ch(&v1, "lists.example.org", "/x").1, Some("list_archive"));
        assert_eq!(ch(&v1, "mail.example.org", "/recipes/soup.html").1, Some("list_archive"));
        assert_eq!(ch(&v1, "www.example.org", "/pipermail/dev/2009/000123.html").1, Some("list_archive"));
        assert_eq!(ch(&v1, "www.example.org", "/load/garden/msg0512.html").1, Some("list_archive"));
        // Path indices differ between the files; each id reads with its own rules.
        let (r1, _) = ch(&v1, "www.example.com", "/press-releases/a");
        let (r2, c2) = ch(&v2, "www.example.com", "/press-releases/a");
        assert_ne!(r1, r2);
        assert_eq!(c2, Some("press_release"));
        assert_eq!(email_channel(&v1, "email:content_cues:3"), Some("cue_only"));
        assert_eq!(email_channel(&v2, "email:path:99"), Some("unknown"));
        assert_eq!(email_channel(&v2, "news:path:0"), None);
        assert_eq!(email_channel(&v2, "general_web:no_rule"), None);
    }

    /// Synthetic pilot root: reassign writes per-document rows, the email
    /// channels move as the rules say, and a sample from the assignment
    /// stratifies on the new domain and never draws a withheld id.
    #[test]
    fn reassign_then_sample_from_the_assignment() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let wet = "crawl-data/CC-MAIN-2016-07/segments/1/wet/CC-MAIN-x-00001.warc.wet.gz";
        let plain = "The weather was mild and the market stayed open until late in the evening.";
        // (id, host, path, pool domain, pool rule under v1)
        let docs = [
            ("d1", "lists.example.org", "/pipermail/dev/2009/000123.html", "email", "email:host_pattern:0"),
            ("d2", "www.example.com", "/press-releases/a-new-thing", "email", "email:path:6"),
            ("d3", "mail.example.org", "/recipes/soup.html", "email", "email:host_pattern:1"),
            ("d4", "www.prweb.com", "/releases/x.htm", "email", "email:host:prweb.com"),
            ("d5", "someone.blogspot.co.at", "/2013/06/", "general_web", "general_web:no_rule"),
            ("d6", "www.example.com", "/press-releases/b-thing", "email", "email:path:6"),
        ];
        let v1 = Rules::pilot_v1();
        let mut pool = store::ZWriter::create(&root.join("pool/admitted.jsonl.zst")).unwrap();
        let mut pages = store::ZWriter::create(&pages::pages_path(root, &pages::stem(wet))).unwrap();
        for (id, host, path, dom, rule) in docs {
            let url = format!("https://{host}{path}");
            let a = v1.assign(host, path, plain);
            assert_eq!((a.domain.as_str(), a.rule.as_str()), (dom, rule), "{id}");
            let rid = format!("<urn:uuid:{id}>");
            pool.write(&json!({"id": id, "crawl": "CC-MAIN-2016-07", "domain": dom, "domain_rule": rule,
                "group": format!("cc_site:{host}"), "host": host, "registered_domain": host, "url": url,
                "warc_date": "2016-02-10T00:00:00Z", "wet_path": wet, "record_id": rid, "words": 14,
                "text": plain, "text_sha256": measure::sha256_hex(plain.as_bytes())}))
                .unwrap();
            pages
                .write(&json!({"record_id": rid, "page_sha256": "p", "url": url, "host": host,
                    "registered_domain": host, "text": plain}))
                .unwrap();
        }
        pool.finish().unwrap();
        pages.finish().unwrap();
        let v2 = Rules::pilot_v2();
        let per_doc = root.join("assign.jsonl.zst");
        let r = reassign(&ReassignArgs {
            root,
            rules: &v2,
            pool_rules: &v1,
            threads: 2,
            out: &root.join("reassign.json"),
            per_doc: Some(&per_doc),
            detail: &["email".to_owned()],
        })
        .unwrap();
        let e = &r["email_channels"];
        assert_eq!(e["before"], json!({"list_archive": 2, "press_release": 3}));
        assert_eq!(e["after"], json!({"list_archive": 1, "press_release": 3}));
        assert_eq!(e["transitions"]["list_archive"], json!({"general_web": 1, "list_archive": 1}));
        assert_eq!(e["transitions"]["from:general_web"], Value::Null);
        assert_eq!(r["after"]["essays"], json!(1));
        assert_eq!(r["after_by_rule"]["email"]["email:host:prweb.com"], json!(1));
        assert_eq!(r["rule_sources"]["pool_rules"]["email:host_pattern:1"][0], json!("host_pattern"));
        assert_eq!(r["per_doc"]["rows"], json!(6));
        let rows: Vec<DocAssignment> = store::rows(&per_doc).unwrap();
        assert_eq!(rows[2].domain, domain::GENERAL);
        assert_eq!(rows[4].domain, "essays");
        let withhold: HashSet<String> = ["d6".to_owned()].into();
        let out = root.join("sample");
        sample(&SampleArgs {
            root,
            out: &out,
            seed: "t",
            per_domain: 10,
            assignments: Some(&per_doc),
            rules: Some(&v2),
            withhold: &withhold,
        })
        .unwrap();
        let s: Vec<SampleRow> = store::rows_plain(&out.join("sample.jsonl")).unwrap();
        let mut got: Vec<(&str, &str)> = s.iter().map(|r| (r.id.as_str(), r.domain.as_str())).collect();
        got.sort();
        assert_eq!(got, vec![("d1", "email"), ("d2", "email"), ("d4", "email"), ("d5", "essays")]);
        let d5 = s.iter().find(|r| r.id == "d5").unwrap();
        assert_eq!(d5.pool_domain.as_deref(), Some("general_web"));
        let summ: Value = store::read_json(&out.join("SAMPLE.json")).unwrap();
        assert_eq!(summ["allocation"]["email"]["admitted"], json!(4));
        assert_eq!(summ["allocation"]["email"]["years"]["2016"]["eligible"], json!(3));
        assert_eq!(summ["withheld"]["in_frame_by_domain"]["email"], json!(1));
        assert_eq!(summ["rules"], json!(v2.version));
        // Ids read from plain lines and JSONL rows alike.
        let ids = root.join("ids.txt");
        fs::write(&ids, "# read\nd1\n{\"id\": \"d2\", \"n\": 0}\n\n").unwrap();
        assert_eq!(read_ids(&ids).unwrap(), ["d1".to_owned(), "d2".to_owned()].into());
    }

    #[test]
    fn labels_are_checked() {
        let names = ["news", "legal"];
        let l = Label {
            id: "x".into(),
            true_domain: "news".into(),
            register: "news_report".into(),
            register_match: true,
            excluded: vec!["adult".into()],
            note: String::new(),
        };
        assert!(check_label(&l, &names).is_ok());
        let mut bad = l.clone();
        bad.true_domain = "poetry".into();
        assert!(check_label(&bad, &names).is_err());
        let mut bad = l.clone();
        bad.excluded = vec!["spam".into()];
        assert!(check_label(&bad, &names).is_err());
        assert_eq!(rule_tier("news:weak_path:3"), "weak_path");
        assert_eq!(rule_tier("general_web:no_rule"), "no_rule");
    }
}

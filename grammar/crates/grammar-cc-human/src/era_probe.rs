//! Era probe: an L2-regularised logistic regression on
//! hashed word unigrams and bigrams that separates early (2013–2016) from late
//! (2020–2022) crawls on human pilot text. Classes are balanced per cohort
//! domain, and train/test are split by group, so the probe cannot score by
//! site identity or by domain mix. A second variant masks every token that
//! contains a digit, which removes explicit dates. The probe is a measurement
//! of how much era signal exists; it never labels or filters a document.
use crate::measure::{self, EARLY, LATE, PoolRow, Rng, auc, cluster_boot, crawl_year, file_sha256, read_jsonl};
use crate::store;
use anyhow::{Context, Result, ensure};
use grammar_a_pools::splits::rank;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader},
    path::PathBuf,
};

pub const SELECT_SCOPE: &str = "cc_era_probe_select_v1";
pub const SPLIT_SCOPE: &str = "cc_era_probe_split_v1";

pub fn fnv1a(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Lower-cased alphanumeric tokens (apostrophes kept inside words).
pub fn tokens(text: &str, mask_digits: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let t = cur.trim_matches('\'').to_string();
        if !t.is_empty() {
            out.push(if mask_digits && t.chars().any(|c| c.is_numeric()) { "<num>".into() } else { t });
        }
        cur.clear();
    };
    for c in text.chars() {
        if c.is_alphanumeric() {
            cur.extend(c.to_lowercase());
        } else if (c == '\'' || c == '\u{2019}') && !cur.is_empty() {
            cur.push('\'');
        } else {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    out
}

pub fn ngrams(toks: &[String]) -> Vec<String> {
    let mut g: Vec<String> = toks.iter().map(|t| format!("u:{t}")).collect();
    g.extend(toks.windows(2).map(|w| format!("b:{} {}", w[0], w[1])));
    g
}

/// Sorted unique buckets with equal weights of unit L2 norm.
pub fn features(text: &str, mask_digits: bool, dims_log2: u32) -> Vec<u32> {
    let mask = (1u64 << dims_log2) - 1;
    let mut v: Vec<u32> = ngrams(&tokens(text, mask_digits)).iter().map(|g| (fnv1a(g.as_bytes()) & mask) as u32).collect();
    v.sort_unstable();
    v.dedup();
    v
}

pub struct Model {
    pub w: Vec<f64>,
    pub b: f64,
}

fn sigmoid(z: f64) -> f64 {
    1.0 / (1.0 + (-z).exp())
}

impl Model {
    pub fn logit(&self, x: &[u32]) -> f64 {
        let s = if x.is_empty() { 0.0 } else { 1.0 / (x.len() as f64).sqrt() };
        self.b + x.iter().map(|&j| self.w[j as usize]).sum::<f64>() * s
    }
}

/// AdaGrad SGD on the logistic loss with lazy L2 on active coordinates.
pub fn train(xs: &[Vec<u32>], ys: &[f64], dims_log2: u32, epochs: usize, lr: f64, l2: f64, seed: u64) -> Model {
    let d = 1usize << dims_log2;
    let (mut w, mut g2) = (vec![0.0; d], vec![0.0; d]);
    let (mut b, mut gb) = (0.0, 0.0);
    let mut order: Vec<usize> = (0..xs.len()).collect();
    let mut rng = Rng(seed);
    for _ in 0..epochs {
        for i in (1..order.len()).rev() {
            order.swap(i, rng.below(i + 1));
        }
        for &i in &order {
            let x = &xs[i];
            let s = if x.is_empty() { 0.0 } else { 1.0 / (x.len() as f64).sqrt() };
            let z = b + x.iter().map(|&j| w[j as usize]).sum::<f64>() * s;
            let g = sigmoid(z) - ys[i];
            for &j in x {
                let j = j as usize;
                let grad = g * s + l2 * w[j];
                g2[j] += grad * grad;
                w[j] -= lr * grad / (g2[j].sqrt() + 1e-8);
            }
            gb += g * g;
            b -= lr * g / (gb.sqrt() + 1e-8);
        }
    }
    Model { w, b }
}

pub struct ProbeArgs {
    pub pool: PathBuf,
    pub out: PathBuf,
    pub seed: String,
    pub per_domain_class: usize,
    pub dims_log2: u32,
    pub epochs: usize,
    pub lr: f64,
    pub l2: f64,
    pub draws: usize,
    /// sample.jsonl of the A measurement: excluded from training, then scored.
    pub sample: Option<PathBuf>,
    /// per-document.jsonl of the report (A involvement), for the correlation.
    pub a_scores: Option<PathBuf>,
    /// Tier C Test scoring rows (text, origin, domain), scored by origin.
    pub tierc_rows: Option<PathBuf>,
}

fn in_range(y: u16, r: (u16, u16)) -> bool {
    (r.0..=r.1).contains(&y)
}

struct Item<'a> {
    row: &'a PoolRow,
    y: f64,
    test: bool,
}

fn variant(a: &ProbeArgs, items: &[Item], mask: bool, extra: &Extra) -> Result<Value> {
    let feats: Vec<Vec<u32>> = items.iter().map(|it| features(it.row.scored_text().0, mask, a.dims_log2)).collect();
    let tr: Vec<usize> = (0..items.len()).filter(|&i| !items[i].test).collect();
    let te: Vec<usize> = (0..items.len()).filter(|&i| items[i].test).collect();
    let xs: Vec<Vec<u32>> = tr.iter().map(|&i| feats[i].clone()).collect();
    let ys: Vec<f64> = tr.iter().map(|&i| items[i].y).collect();
    let m = train(&xs, &ys, a.dims_log2, a.epochs, a.lr, a.l2, rank(&a.seed, "cc_era_probe_shuffle_v1", if mask { "masked" } else { "raw" }));
    let score: Vec<f64> = te.iter().map(|&i| m.logit(&feats[i])).collect();
    let lab: Vec<bool> = te.iter().map(|&i| items[i].y > 0.5).collect();
    let auc_idx = |idx: &[usize]| {
        let l: Vec<f64> = idx.iter().filter(|&&k| lab[k]).map(|&k| score[k]).collect();
        let e: Vec<f64> = idx.iter().filter(|&&k| !lab[k]).map(|&k| score[k]).collect();
        auc(&l, &e)
    };
    let all: Vec<usize> = (0..te.len()).collect();
    let mut gm: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (k, &i) in te.iter().enumerate() {
        gm.entry(items[i].row.group.as_str()).or_default().push(k);
    }
    let groups: Vec<Vec<usize>> = gm.into_values().collect();
    let a0 = auc_idx(&all);
    let ci = cluster_boot(&groups, a.draws, 0x5eed_e0a1, auc_idx);
    let acc = all.iter().filter(|&&k| (score[k] > 0.0) == lab[k]).count() as f64 / all.len() as f64;
    let ll = all.iter().map(|&k| {
        let p = sigmoid(score[k]).clamp(1e-12, 1.0 - 1e-12);
        if lab[k] { -p.ln() } else { -(1.0 - p).ln() }
    }).sum::<f64>() / all.len() as f64;
    let mut by_domain = BTreeMap::new();
    let mut doms: Vec<&str> = te.iter().map(|&i| items[i].row.domain.as_str()).collect();
    doms.sort();
    doms.dedup();
    for d in doms {
        let idx: Vec<usize> = all.iter().copied().filter(|&k| items[te[k]].row.domain == d).collect();
        by_domain.insert(d.to_string(), json!({"test_documents": idx.len(), "auc": auc_idx(&idx)}));
    }
    // Top features in each direction, named by their most frequent training n-gram.
    let mut ix: Vec<usize> = (0..m.w.len()).collect();
    ix.sort_by(|&x, &y| m.w[y].total_cmp(&m.w[x]));
    let top_late: Vec<usize> = ix.iter().take(40).copied().collect();
    let top_early: Vec<usize> = ix.iter().rev().take(40).copied().collect();
    let want: HashSet<usize> = top_late.iter().chain(&top_early).copied().collect();
    let mut names: HashMap<usize, HashMap<String, usize>> = HashMap::new();
    let maskb = (1u64 << a.dims_log2) - 1;
    for &i in &tr {
        let mut seen = HashSet::new();
        for g in ngrams(&tokens(items[i].row.scored_text().0, mask)) {
            let j = (fnv1a(g.as_bytes()) & maskb) as usize;
            if want.contains(&j) && seen.insert(g.clone()) {
                *names.entry(j).or_default().entry(g).or_default() += 1;
            }
        }
    }
    let name = |j: usize| {
        let mut v: Vec<(String, usize)> = names.get(&j).map(|m| m.iter().map(|(k, c)| (k.clone(), *c)).collect()).unwrap_or_default();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        json!({"weight": m.w[j], "ngram": v.first().map(|x| x.0.clone()), "train_docs": v.first().map(|x| x.1), "bucket_ngrams": v.len()})
    };
    // Scored extras: the A sample by year, and Tier C Test rows by origin.
    let p_late = |t: &str| sigmoid(m.logit(&features(t, mask, a.dims_log2)));
    let mut sample_out = Value::Null;
    if !extra.sample.is_empty() {
        let mut by_year: BTreeMap<u16, (usize, f64, usize)> = BTreeMap::new();
        let mut cells: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
        let mut flagged = Vec::new();
        let mut unflagged = Vec::new();
        for (id, year, domain, text) in &extra.sample {
            let p = p_late(text);
            let e = by_year.entry(*year).or_default();
            e.0 += 1;
            e.1 += p;
            e.2 += (p > 0.5) as usize;
            if let Some((inv, flag)) = extra.a.get(id) {
                cells.entry(format!("{year}|{domain}")).or_default().push((p, *inv));
                if *flag { flagged.push(p) } else { unflagged.push(p) }
            }
        }
        // Pearson correlation of P(late) and A involvement within year x domain cells.
        let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
        for v in cells.values() {
            let n = v.len() as f64;
            let (mx, my) = (v.iter().map(|x| x.0).sum::<f64>() / n, v.iter().map(|x| x.1).sum::<f64>() / n);
            for &(x, y) in v {
                sxy += (x - mx) * (y - my);
                sxx += (x - mx) * (x - mx);
                syy += (y - my) * (y - my);
            }
        }
        sample_out = json!({
            "by_year": by_year.iter().map(|(y, (n, s, k))| json!({"year": y, "n": n, "mean_p_late": s / *n as f64, "share_p_late_above_0_5": *k as f64 / *n as f64})).collect::<Vec<_>>(),
            "within_cell_pearson_p_late_vs_a_involvement": if sxx > 0.0 && syy > 0.0 { sxy / (sxx * syy).sqrt() } else { f64::NAN },
            "auc_p_late_a_flagged_vs_unflagged": auc(&flagged, &unflagged), "a_flagged": flagged.len(),
        });
    }
    let mut tierc_out = Value::Null;
    if !extra.tierc.is_empty() {
        let mut by: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        let scored: Vec<f64> = extra.tierc.iter().map(|(_, _, text)| p_late(text)).collect();
        for ((origin, domain, _), &p) in extra.tierc.iter().zip(scored.iter()) {
            by.entry(format!("origin:{origin}")).or_default().push(p);
            if origin == "human_only" {
                by.entry(format!("human_domain:{domain}")).or_default().push(p);
            }
        }
        let early_test: Vec<f64> = te.iter().enumerate().filter(|(k, _)| !lab[*k]).map(|(k, _)| sigmoid(score[k])).collect();
        let late_test: Vec<f64> = te.iter().enumerate().filter(|(k, _)| lab[*k]).map(|(k, _)| sigmoid(score[k])).collect();
        let summary = |v: &Vec<f64>| {
            json!({"n": v.len(), "mean_p_late": v.iter().sum::<f64>() / v.len() as f64,
                "share_p_late_above_0_5": v.iter().filter(|x| **x > 0.5).count() as f64 / v.len() as f64,
                "auc_vs_cc_early_test": auc(v, &early_test), "auc_vs_cc_late_test": auc(v, &late_test)})
        };
        // Within Test domains: does the probe separate AI rows from the same domain's humans?
        let mut per: BTreeMap<(&str, &str), Vec<f64>> = BTreeMap::new();
        for ((origin, domain, _), p) in extra.tierc.iter().zip(scored.iter()) {
            per.entry((domain.as_str(), origin.as_str())).or_default().push(*p);
        }
        let within = |ai: &str| {
            let mut v = Vec::new();
            for ((dom, o), xs) in &per {
                if *o == ai
                    && let Some(h) = per.get(&(*dom, "human_only"))
                {
                    v.push(auc(xs, h));
                }
            }
            json!({"domains": v.len(), "mean_auc_vs_same_domain_humans": v.iter().sum::<f64>() / v.len().max(1) as f64})
        };
        let mut t: BTreeMap<String, Value> = by.iter().map(|(k, v)| (k.clone(), summary(v))).collect();
        t.insert("within_domain:model_only".into(), within("model_only"));
        t.insert("within_domain:mixed".into(), within("mixed"));
        tierc_out = json!(t);
    }
    Ok(json!({
        "mask_digits": mask, "train_documents": tr.len(), "test_documents": te.len(),
        "test_auc": a0, "test_auc_group_bootstrap95": [ci.0, ci.1], "test_accuracy_at_0_5": acc, "test_log_loss": ll,
        "test_by_domain": by_domain,
        "top_late_features": top_late.iter().map(|&j| name(j)).collect::<Vec<_>>(),
        "top_early_features": top_early.iter().map(|&j| name(j)).collect::<Vec<_>>(),
        "a_sample": sample_out, "tier_c_test": tierc_out,
    }))
}

#[derive(Default)]
struct Extra {
    sample: Vec<(String, u16, String, String)>,
    a: HashMap<String, (f64, bool)>,
    tierc: Vec<(String, String, String)>,
}

pub fn run(a: &ProbeArgs) -> Result<Value> {
    let pool = measure::read_pool(&a.pool)?;
    let mut extra = Extra::default();
    let mut exclude = HashSet::new();
    if let Some(p) = &a.sample {
        for v in read_jsonl(p)? {
            let id = v["id"].as_str().context("id")?.to_string();
            exclude.insert(id.clone());
            extra.sample.push((id, v["year"].as_u64().context("year")? as u16, v["domain"].as_str().unwrap_or("").into(), v["text"].as_str().context("text")?.into()));
        }
    }
    if let Some(p) = &a.a_scores {
        for v in read_jsonl(p)? {
            if let (Some(id), Some(inv), Some(f)) = (v["id"].as_str(), v["a_involvement"].as_f64(), v["a_flag"].as_bool()) {
                extra.a.insert(id.into(), (inv, f));
            }
        }
    }
    if let Some(p) = &a.tierc_rows {
        for l in BufReader::new(File::open(p)?).lines() {
            let v: Value = serde_json::from_str(&l?)?;
            extra.tierc.push((v["origin"].as_str().unwrap_or("").into(), v["domain"].as_str().unwrap_or("").into(), v["text"].as_str().context("text")?.into()));
        }
    }
    // Balanced per domain: min(cap, early, late) from each class in rank order.
    let mut by: BTreeMap<(String, bool), Vec<(u64, usize)>> = BTreeMap::new();
    for (i, r) in pool.iter().enumerate() {
        if exclude.contains(&r.id) {
            continue;
        }
        let y = crawl_year(&r.crawl)?;
        let class = if in_range(y, LATE) { true } else if in_range(y, EARLY) { false } else { continue };
        by.entry((r.domain.clone(), class)).or_default().push((rank(&a.seed, SELECT_SCOPE, &r.id), i));
    }
    let mut domains: Vec<String> = by.keys().map(|k| k.0.clone()).collect();
    domains.dedup();
    let mut items = Vec::new();
    let mut per_domain = BTreeMap::new();
    for d in &domains {
        let n = a.per_domain_class
            .min(by.get(&(d.clone(), false)).map_or(0, |v| v.len()))
            .min(by.get(&(d.clone(), true)).map_or(0, |v| v.len()));
        per_domain.insert(d.clone(), n);
        for class in [false, true] {
            let mut v = by[&(d.clone(), class)].clone();
            v.sort();
            for &(_, i) in v.iter().take(n) {
                let r = &pool[i];
                items.push(Item { row: r, y: class as u8 as f64, test: rank(&a.seed, SPLIT_SCOPE, &r.group).is_multiple_of(5) });
            }
        }
    }
    ensure!(items.iter().any(|i| i.test) && items.iter().any(|i| !i.test), "empty split");
    let raw = variant(a, &items, false, &extra)?;
    let masked = variant(a, &items, true, &extra)?;
    let out = json!({
        "schema": "slop_ninja_cc_human_era_probe_v1",
        "pool": a.pool, "pool_sha256": file_sha256(&a.pool)?, "seed": a.seed,
        "classes": {"early": format!("crawl years {}-{}", EARLY.0, EARLY.1), "late": format!("crawl years {}-{}", LATE.0, LATE.1)},
        "selection": "per domain, min(cap, early, late) documents of each class in ascending rank(seed, select scope, id); A-sample documents excluded",
        "split": "test when rank(seed, split scope, group) % 5 == 0 (whole groups)",
        "features": format!("lower-cased word unigrams and bigrams, FNV-1a hashed into 2^{} buckets, binary, unit L2 norm", a.dims_log2),
        "model": {"loss": "logistic", "optimizer": "AdaGrad SGD", "epochs": a.epochs, "lr": a.lr, "l2": a.l2},
        "per_domain_class": per_domain, "documents": items.len(),
        "raw": raw, "digits_masked": masked,
    });
    store::write_json(&a.out.join("era-probe.json"), &out)?;
    Ok(json!({"documents": items.len(), "raw_test_auc": out["raw"]["test_auc"], "masked_test_auc": out["digits_masked"]["test_auc"]}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_lowercase_keep_apostrophes_and_mask_digits() {
        assert_eq!(tokens("Don’t STOP in 2019, ok?", false), vec!["don't", "stop", "in", "2019", "ok"]);
        assert_eq!(tokens("in 2019 or 3g", true), vec!["in", "<num>", "or", "<num>"]);
        assert_eq!(ngrams(&tokens("a b c", false)), vec!["u:a", "u:b", "u:c", "b:a b", "b:b c"]);
    }

    #[test]
    fn probe_learns_a_separable_toy_problem() {
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        for i in 0..200 {
            let late = i % 2 == 0;
            let t = if late { format!("pandemic zoom item{i}") } else { format!("myspace blackberry item{i}") };
            xs.push(features(&t, false, 12));
            ys.push(late as u8 as f64);
        }
        let m = train(&xs, &ys, 12, 3, 0.5, 1e-6, 7);
        assert!(m.logit(&features("pandemic zoom", false, 12)) > 1.0);
        assert!(m.logit(&features("blackberry myspace", false, 12)) < -1.0);
    }
}

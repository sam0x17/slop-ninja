//! Domain- and cohort-stratified Train item selection for `generate --request-order`.
//!
//! Plan request files are grouped by domain, so a plain `--limit` covers only
//! the first domains. This module chooses, per generator, a bounded set of
//! pending Train items whose domain mix follows the Train human counts (with a
//! per-domain floor and credit for positives already accepted), mixes cohorts
//! and share bins inside each domain, keeps voice ladders whole, and admits only
//! styles whose frozen partition is Train. The output order interleaves domains
//! so that any prefix of the list is itself stratified. In the cohorts of
//! [`BALANCED_COHORTS`] the kinds are balanced across batches: each pick takes
//! the kind with the fewest accepted plus selected items in its domain.
use crate::text::sha_hex;
use anyhow::{Result, ensure};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// One Train item as the selector sees it.
#[derive(Clone, Debug)]
pub struct Item {
    pub item_id: String,
    pub generator: String,
    pub domain: String,
    pub parent_id: String,
    pub cohort: String,
    /// Share bin for span/full rewrites, otherwise the item kind.
    pub stratum: String,
    pub style_kind: String,
    /// Voice-ladder key; items of one ladder are selected together.
    pub ladder: Option<String>,
    /// Style ids (voice, brief) the item depends on.
    pub style_ids: Vec<String>,
    pub requests: Vec<String>,
}

impl Item {
    pub fn from_plan(v: &Value) -> Option<Item> {
        let s = |k: &str| v[k].as_str().map(String::from);
        let st = &v["style"];
        let mut style_ids = Vec::new();
        for k in ["voice_id", "brief_id"] {
            if let Some(id) = st[k].as_str() {
                style_ids.push(id.to_owned());
            }
        }
        Some(Item {
            item_id: s("item_id")?,
            generator: s("generator")?,
            domain: s("domain")?,
            parent_id: s("parent_id")?,
            cohort: s("cohort").unwrap_or_else(|| "none".into()),
            stratum: s("bin").or_else(|| s("kind")).unwrap_or_else(|| "none".into()),
            style_kind: st["style_kind"].as_str().unwrap_or("none").to_owned(),
            ladder: st["ladder_id"].as_str().map(String::from),
            style_ids,
            requests: v["requests"].as_array()?.iter().filter_map(|r| r.as_str().map(String::from)).collect(),
        })
    }
}

/// Selection parameters for one generator.
#[derive(Clone, Debug)]
pub struct Params {
    pub budget: usize,
    pub floor: usize,
    /// Target cohort shares inside a domain (renormalised over available cohorts).
    pub cohort_shares: Vec<(String, f64)>,
    pub priority_domains: BTreeSet<String>,
    pub seed: String,
    /// Cohorts whose strata (item kinds) are balanced by count across
    /// batches: inside a domain each pick takes the fitting stratum with the
    /// fewest credited plus already selected items, ties in rotation order.
    /// Other cohorts rotate their strata from the first on every call.
    pub balanced_cohorts: BTreeSet<String>,
    /// Accepted items credited per (domain, cohort, stratum), read only for
    /// `balanced_cohorts`.
    pub stratum_credit: BTreeMap<(String, String, String), usize>,
}

/// Cohorts `stratify-train` balances by kind across batches: the Train base
/// cohort (continuation and splice 1:1, protocol A2 §6.4).
pub const BALANCED_COHORTS: [&str; 1] = [crate::span_ops::COHORT_BASE];

/// Selected item ids in run order plus the per-domain quota table.
#[derive(Debug, Default)]
pub struct Selection {
    pub order: Vec<String>,
    pub quota: BTreeMap<String, usize>,
    pub available: BTreeMap<String, usize>,
    pub existing: BTreeMap<String, usize>,
}

/// Per-domain quotas: water-fill `budget` so existing + new tracks the human
/// mix, with `floor` (bounded by availability) and availability caps.
pub fn quotas(
    humans: &BTreeMap<String, f64>,
    available: &BTreeMap<String, usize>,
    existing: &BTreeMap<String, usize>,
    budget: usize,
    floor: usize,
) -> BTreeMap<String, usize> {
    let doms: Vec<&String> = available.keys().filter(|d| available[*d] > 0).collect();
    let htot: f64 = doms.iter().map(|d| humans.get(*d).copied().unwrap_or(0.0)).sum();
    let ex = |d: &str| existing.get(d).copied().unwrap_or(0) as f64;
    let total = budget as f64 + doms.iter().map(|d| ex(d)).sum::<f64>();
    let lo = |d: &str| floor.min(available[d]) as f64;
    let val = |d: &str, lam: f64| {
        let want = if htot > 0.0 { lam * total * humans.get(d).copied().unwrap_or(0.0) / htot } else { lam };
        (want - ex(d)).max(lo(d)).min(available[d] as f64)
    };
    let cap: f64 = doms.iter().map(|d| available[*d] as f64).sum();
    let target = (budget as f64).min(cap);
    let (mut a, mut b) = (0.0f64, 1.0f64);
    while doms.iter().map(|d| val(d, b)).sum::<f64>() < target && b < 1e9 {
        b *= 2.0;
    }
    for _ in 0..200 {
        let m = 0.5 * (a + b);
        if doms.iter().map(|d| val(d, m)).sum::<f64>() < target {
            a = m;
        } else {
            b = m;
        }
    }
    let raw: Vec<(String, f64)> = doms.iter().map(|d| ((*d).clone(), val(d, b))).collect();
    let mut q: BTreeMap<String, usize> = raw.iter().map(|(d, v)| (d.clone(), v.floor() as usize)).collect();
    let mut short = (target.round() as usize).saturating_sub(q.values().sum());
    let mut rem: Vec<(f64, String)> = raw.iter().map(|(d, v)| (v - v.floor(), d.clone())).collect();
    rem.sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.cmp(&y.1)));
    for (_, d) in rem {
        if short == 0 {
            break;
        }
        if q[&d] < available[&d] {
            *q.get_mut(&d).unwrap() += 1;
            short -= 1;
        }
    }
    q
}

/// S0-10's value (B5b part 2 item 5): the base-cohort stratification.
pub const S0_10_SCHEMA: &str = "slop_ninja_a_positives_base_stratification_v1";
/// How S0-10 is fixed and read.
pub const S0_10_RULE: &str = "S0-10, the base-cohort stratification (protocol A2 sections 6.4 and 12.6: stratify-train by human counts, before D7; B5b part 2 item 5; amendment 27): stratify-train --target granite_base --target smollm3_base --target ernie_base --values VALUES-R2.json --write-stratification ENTRY runs part 1's selection at v_fam (S0-7) over the base plan and records, per Train base family and Train domain, the target of accepted rows = the existing accepted Train items (this plan's runs and --prior, the plans and runs of the existing base rows) + the selection's domain quota, water-filled so existing plus new track the base plan's Train human counts (plan.json humans.train), bounded by availability, with floor 0, no domain weights, no priority domains, cohort shares train_base_v3=1 and the seed r2_base_cohort_stratification_v1 (any other option value is refused); the entry (values.S0-10 with id S0-10, name base_cohort_stratification and value) is copied unchanged into VALUES-R2.json by a later A9 step; D7's step, stratify-train --target ... --values --stratification, takes the same fixed options and is refused while S0-10 is absent or when its plan, v_fam, prior plans, partitions, floor, seed, cohort shares, domain weights or Train human counts differ from S0-10's, and selects per domain the target less the accepted items so far (bounded by availability), so a first pass equals part 1's selection and later passes fill the frozen targets";
/// S0-10's floor (amendment 27): stratified by Train human counts alone.
pub const S0_10_FLOOR: usize = 0;
/// S0-10's selection seed (amendment 27).
pub const S0_10_SEED: &str = "r2_base_cohort_stratification_v1";
/// `stratify-train`'s floor when S0-10's options are off (the tool's
/// default, not a protocol value).
pub const DEFAULT_FLOOR: usize = 60;

/// Choose and order items for one generator.
pub fn select(
    cands: &[Item],
    humans: &BTreeMap<String, f64>,
    existing: &BTreeMap<String, usize>,
    p: &Params,
) -> Result<Selection> {
    select_with(cands, humans, existing, p, None)
}

/// [`select`] with the per-domain quotas `fixed` (S0-10's targets less the
/// accepted items, D7's step) in place of [`quotas`]; each is bounded by the
/// domain's availability, and a domain `fixed` omits gets none.
pub fn select_with(
    cands: &[Item],
    humans: &BTreeMap<String, f64>,
    existing: &BTreeMap<String, usize>,
    p: &Params,
    fixed: Option<&BTreeMap<String, usize>>,
) -> Result<Selection> {
    // selection units: whole ladders, otherwise single items
    let mut units: BTreeMap<String, Vec<&Item>> = BTreeMap::new();
    for it in cands {
        let key = match &it.ladder {
            Some(l) => format!("ladder:{}:{l}", it.domain),
            None => format!("item:{}", it.item_id),
        };
        units.entry(key).or_default().push(it);
    }
    // domain -> cohort -> stratum -> units (hash order)
    type Strata<'a> = BTreeMap<String, Vec<(String, Vec<&'a Item>)>>;
    let mut by_dom: BTreeMap<String, BTreeMap<String, Strata>> = BTreeMap::new();
    let mut available: BTreeMap<String, usize> = BTreeMap::new();
    for (key, mut its) in units {
        its.sort_by(|a, b| a.item_id.cmp(&b.item_id));
        let dom = its[0].domain.clone();
        // a ladder's cohort is its ladder cohort (rung 0 is a diverse item)
        let cohort = its.iter().map(|i| i.cohort.clone()).find(|c| c.starts_with("ladder")).unwrap_or_else(|| its[0].cohort.clone());
        let stratum = its.iter().find(|i| i.cohort == cohort).map(|i| i.stratum.clone()).unwrap_or_default();
        *available.entry(dom.clone()).or_default() += its.len();
        let h = sha_hex(format!("{}:{key}", p.seed).as_bytes());
        by_dom.entry(dom).or_default().entry(cohort).or_default().entry(stratum).or_default().push((h, its));
    }
    for cs in by_dom.values_mut() {
        for ss in cs.values_mut() {
            for us in ss.values_mut() {
                us.sort_by(|a, b| a.0.cmp(&b.0));
            }
        }
    }
    let quota = match fixed {
        Some(f) => available.iter().map(|(d, a)| (d.clone(), f.get(d).copied().unwrap_or(0).min(*a))).collect(),
        None => quotas(humans, &available, existing, p.budget, p.floor),
    };
    // per-domain picks
    let mut picks: BTreeMap<String, Vec<Vec<&Item>>> = BTreeMap::new();
    for (dom, cs) in &by_dom {
        let q = quota.get(dom).copied().unwrap_or(0);
        let mut queues: BTreeMap<&str, (Vec<&str>, usize, BTreeMap<&str, std::collections::VecDeque<&Vec<&Item>>>)> = BTreeMap::new();
        for (c, ss) in cs {
            let strata: Vec<&str> = ss.keys().map(String::as_str).collect();
            let qs = ss.iter().map(|(s, us)| (s.as_str(), us.iter().map(|(_, u)| u).collect())).collect();
            queues.insert(c.as_str(), (strata, 0, qs));
        }
        let share = |c: &str| p.cohort_shares.iter().find(|(n, _)| n == c).map(|(_, s)| *s).unwrap_or(0.0);
        let mut got: BTreeMap<&str, usize> = BTreeMap::new();
        let mut picked: BTreeMap<(&str, &str), usize> = BTreeMap::new();
        let mut used_parents: BTreeSet<&str> = BTreeSet::new();
        let mut n = 0usize;
        let mut out = Vec::new();
        loop {
            if n >= q {
                break;
            }
            let room = q - n;
            // cohorts that still hold a unit that fits
            let live: Vec<&str> = queues
                .iter()
                .filter(|(_, (_, _, qs))| qs.values().any(|u| u.iter().any(|x| x.len() <= room)))
                .map(|(c, _)| *c)
                .collect();
            if live.is_empty() {
                break;
            }
            let wsum: f64 = live.iter().map(|c| share(c)).sum();
            let c = *live
                .iter()
                .max_by(|a, b| {
                    let need = |c: &str| {
                        let w = if wsum > 0.0 { share(c) / wsum } else { 1.0 / live.len() as f64 };
                        w * (n + 1) as f64 - got.get(c).copied().unwrap_or(0) as f64
                    };
                    need(a).total_cmp(&need(b)).then(b.cmp(a))
                })
                .unwrap();
            let (strata, cursor, qs) = queues.get_mut(c).unwrap();
            // next stratum (share bin / kind) in rotation that has a fitting
            // unit; in a balanced cohort, the fewest credited plus picked first
            let mut order: Vec<usize> = (0..strata.len()).map(|k| (*cursor + k) % strata.len()).collect();
            if p.balanced_cohorts.contains(c) {
                let count: Vec<usize> = strata
                    .iter()
                    .map(|s| {
                        p.stratum_credit.get(&(dom.clone(), c.to_owned(), (*s).to_owned())).copied().unwrap_or(0)
                            + picked.get(&(c, *s)).copied().unwrap_or(0)
                    })
                    .collect();
                // stable: equal counts keep the rotation order
                order.sort_by_key(|&i| count[i]);
            }
            let mut chosen = None;
            for i in order {
                let s = strata[i];
                let qd = qs.get_mut(s).unwrap();
                let fits = |u: &&Vec<&Item>| u.len() <= room;
                let pos = qd
                    .iter()
                    .position(|u| fits(u) && !used_parents.contains(u[0].parent_id.as_str()))
                    .or_else(|| qd.iter().position(fits));
                if let Some(j) = pos {
                    chosen = qd.remove(j).map(|u| (s, u));
                    *cursor = (i + 1) % strata.len();
                    break;
                }
            }
            let (s, u) = chosen.expect("live cohort has a fitting unit");
            *picked.entry((c, s)).or_default() += u.len();
            for it in u.iter() {
                used_parents.insert(it.parent_id.as_str());
            }
            n += u.len();
            *got.entry(c).or_default() += u.len();
            out.push(u.clone());
        }
        picks.insert(dom.clone(), out);
    }
    // interleave domains by virtual time; priority domains are front-loaded
    let mut slots: Vec<(f64, String, usize, &Item)> = Vec::new();
    for (dom, us) in &picks {
        let total: usize = us.iter().map(Vec::len).sum();
        let f = if p.priority_domains.contains(dom) { 0.5 } else { 1.0 };
        let mut k = 0usize;
        for (ui, u) in us.iter().enumerate() {
            let t = f * (k as f64 + 0.5) / total.max(1) as f64;
            for it in u {
                slots.push((t, dom.clone(), ui, it));
            }
            k += u.len();
        }
    }
    slots.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)).then(a.3.item_id.cmp(&b.3.item_id)));
    let order: Vec<String> = slots.iter().map(|s| s.3.item_id.clone()).collect();
    let uniq: BTreeSet<&String> = order.iter().collect();
    ensure!(uniq.len() == order.len(), "duplicate item in selection");
    let existing = existing.iter().filter(|(d, _)| available.contains_key(*d)).map(|(d, n)| (d.clone(), *n)).collect();
    Ok(Selection { order, quota, available, existing })
}

/// Whether every style an item depends on is frozen into the Train partition.
pub fn styles_train(it: &Item, partitions: &HashMap<String, String>) -> bool {
    it.style_ids.iter().all(|id| partitions.get(id).is_some_and(|p| p == "train"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, dom: &str, cohort: &str, bin: &str, ladder: Option<&str>) -> Item {
        Item {
            item_id: id.into(),
            generator: "g".into(),
            domain: dom.into(),
            parent_id: format!("p-{id}"),
            cohort: cohort.into(),
            stratum: bin.into(),
            style_kind: "none".into(),
            ladder: ladder.map(String::from),
            style_ids: vec![],
            requests: vec![format!("r-{id}")],
        }
    }

    fn params(budget: usize, floor: usize) -> Params {
        Params {
            budget,
            floor,
            cohort_shares: vec![("mix".into(), 0.5), ("mir".into(), 0.5)],
            priority_domains: ["small".to_owned()].into(),
            seed: "s".into(),
            balanced_cohorts: BTreeSet::new(),
            stratum_credit: BTreeMap::new(),
        }
    }

    #[test]
    fn quotas_follow_humans_with_floor_and_existing_credit() {
        let h: BTreeMap<String, f64> = [("big".to_owned(), 900.0), ("small".to_owned(), 100.0)].into();
        let a: BTreeMap<String, usize> = [("big".to_owned(), 1000), ("small".to_owned(), 1000)].into();
        let q = quotas(&h, &a, &BTreeMap::new(), 100, 20);
        assert_eq!((q["big"], q["small"]), (80, 20));
        let q = quotas(&h, &a, &BTreeMap::new(), 1000, 20);
        assert_eq!((q["big"], q["small"]), (900, 100));
        // 100 existing in big: total 1100 -> big wants 990, minus 100 = 890
        let e: BTreeMap<String, usize> = [("big".to_owned(), 100)].into();
        let q = quotas(&h, &a, &e, 1000, 20);
        assert_eq!((q["big"], q["small"]), (890, 110));
        // availability caps
        let a2: BTreeMap<String, usize> = [("big".to_owned(), 1000), ("small".to_owned(), 5)].into();
        let q = quotas(&h, &a2, &BTreeMap::new(), 100, 20);
        assert_eq!((q["big"], q["small"]), (95, 5));
    }

    #[test]
    fn select_mixes_cohorts_keeps_ladders_and_front_loads_priority() {
        let mut c = Vec::new();
        for i in 0..40 {
            c.push(item(&format!("b{i:02}"), "big", if i % 2 == 0 { "mix" } else { "mir" }, &format!("R{}", i % 4), None));
        }
        for i in 0..10 {
            c.push(item(&format!("s{i:02}"), "small", "mix", "R0", None));
        }
        c.push(item("l0", "big", "mir", "mirror", Some("L")));
        c.push(item("l1", "big", "ladder_mirror", "mirror", Some("L")));
        let h: BTreeMap<String, f64> = [("big".to_owned(), 900.0), ("small".to_owned(), 100.0)].into();
        let s = select(&c, &h, &BTreeMap::new(), &params(30, 5)).unwrap();
        assert_eq!(s.order.len(), 30);
        assert_eq!(s.quota["small"], 5);
        let small: Vec<usize> = s.order.iter().enumerate().filter(|(_, x)| x.starts_with('s')).map(|(i, _)| i).collect();
        assert!(*small.last().unwrap() < 20, "priority domain front-loaded: {small:?}");
        let bigs: Vec<&String> = s.order.iter().filter(|x| x.starts_with('b')).collect();
        let mix = bigs.iter().filter(|x| x[1..].parse::<usize>().unwrap() % 2 == 0).count();
        assert!(mix.abs_diff(bigs.len() - mix) <= 2, "cohorts mixed");
        let l: Vec<usize> = s.order.iter().enumerate().filter(|(_, x)| x.starts_with('l')).map(|(i, _)| i).collect();
        assert!(l.is_empty() || (l.len() == 2 && l[1] == l[0] + 1), "ladder kept whole and adjacent");
        // deterministic
        let s2 = select(&c, &h, &BTreeMap::new(), &params(30, 5)).unwrap();
        assert_eq!(s.order, s2.order);
    }

    /// The base cohort's kinds start at the first kind on every call, so odd
    /// per-domain quotas favour it; with kind credit, batches balance it.
    #[test]
    fn balanced_cohort_kinds_balance_across_batches() {
        let base = crate::span_ops::COHORT_BASE;
        let mut c = Vec::new();
        for (dom, n) in [("d1", 40usize), ("d2", 40), ("d3", 40)] {
            for i in 0..n {
                c.push(item(&format!("{dom}-{i:02}"), dom, base, if i % 2 == 0 { "continuation" } else { "splice" }, None));
            }
        }
        let h: BTreeMap<String, f64> = [("d1".to_owned(), 1.0), ("d2".to_owned(), 1.0), ("d3".to_owned(), 1.0)].into();
        let p = |balanced: bool, credit: BTreeMap<(String, String, String), usize>| Params {
            budget: 9,
            floor: 0,
            cohort_shares: vec![(base.into(), 1.0)],
            priority_domains: BTreeSet::new(),
            seed: "s".into(),
            balanced_cohorts: if balanced { [base.to_owned()].into() } else { BTreeSet::new() },
            stratum_credit: credit,
        };
        let kinds = |order: &[String], dom: &str| -> [usize; 2] {
            let mut k = [0; 2];
            for id in order.iter().filter(|x| x.starts_with(dom)) {
                k[usize::from(id[dom.len() + 1..].parse::<usize>().unwrap() % 2 == 1)] += 1;
            }
            k
        };
        // the first batch is the plain rotation either way: 2:1 in each domain
        let s1 = select(&c, &h, &BTreeMap::new(), &p(false, BTreeMap::new())).unwrap();
        let s1b = select(&c, &h, &BTreeMap::new(), &p(true, BTreeMap::new())).unwrap();
        assert_eq!(s1.order, s1b.order);
        for d in ["d1", "d2", "d3"] {
            assert_eq!(kinds(&s1.order, d), [2, 1], "{d}");
        }
        // credit the first batch as accepted; drop its items from the candidates
        let mut credit = BTreeMap::new();
        let mut existing = BTreeMap::new();
        for id in &s1.order {
            let it = c.iter().find(|x| &x.item_id == id).unwrap();
            *credit.entry((it.domain.clone(), it.cohort.clone(), it.stratum.clone())).or_default() += 1;
            *existing.entry(it.domain.clone()).or_default() += 1;
        }
        let rest: Vec<Item> = c.iter().filter(|x| !s1.order.contains(&x.item_id)).cloned().collect();
        let s2 = select(&rest, &h, &existing, &p(true, credit.clone())).unwrap();
        let s2_plain = select(&rest, &h, &existing, &p(false, credit)).unwrap();
        for d in ["d1", "d2", "d3"] {
            let (a, b, plain) = (kinds(&s1.order, d), kinds(&s2.order, d), kinds(&s2_plain.order, d));
            assert_eq!([a[0] + b[0], a[1] + b[1]], [3, 3], "{d}: balanced over two batches");
            assert_eq!([a[0] + plain[0], a[1] + plain[1]], [4, 2], "{d}: without credit the first kind gains each batch");
        }
        // credit only counts within its own cohort
        let other: BTreeMap<(String, String, String), usize> =
            [(("d1".to_owned(), "other".to_owned(), "continuation".to_owned()), 5)].into();
        let s3 = select(&c, &h, &BTreeMap::new(), &p(true, other)).unwrap();
        assert_eq!(s3.order, s1.order);
    }

    #[test]
    fn fixed_quotas_replace_the_water_fill_bounded_by_availability() {
        let mut c = Vec::new();
        for i in 0..40 {
            c.push(item(&format!("b{i:02}"), "big", "mix", "R0", None));
        }
        for i in 0..6 {
            c.push(item(&format!("s{i:02}"), "small", "mix", "R0", None));
        }
        let h: BTreeMap<String, f64> = [("big".to_owned(), 900.0), ("small".to_owned(), 100.0)].into();
        let p = params(20, 0);
        let s = select(&c, &h, &BTreeMap::new(), &p).unwrap();
        // the fixed quotas equal to the water-fill's give the same selection
        let same = select_with(&c, &h, &BTreeMap::new(), &p, Some(&s.quota)).unwrap();
        assert_eq!((same.order, same.quota), (s.order.clone(), s.quota.clone()));
        // other quotas: bounded by availability, and an omitted domain gets none
        let f: BTreeMap<String, usize> = [("small".to_owned(), 9)].into();
        let t = select_with(&c, &h, &BTreeMap::new(), &p, Some(&f)).unwrap();
        assert_eq!((t.quota["small"], t.quota["big"], t.order.len()), (6, 0, 6));
        assert!(t.order.iter().all(|x| x.starts_with('s')));
    }

    #[test]
    fn style_partition_filter() {
        let mut it = item("a", "d", "mix", "R0", None);
        let parts: HashMap<String, String> = [("v1".to_owned(), "train".to_owned()), ("v2".to_owned(), "dev_iter".to_owned())].into();
        assert!(styles_train(&it, &parts));
        it.style_ids = vec!["v1".into()];
        assert!(styles_train(&it, &parts));
        it.style_ids = vec!["v1".into(), "v2".into()];
        assert!(!styles_train(&it, &parts));
        it.style_ids = vec!["v3".into()];
        assert!(!styles_train(&it, &parts));
    }
}

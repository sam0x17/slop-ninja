//! Capacity planning for a human stock under a cohort mix and a rewrite-source
//! reuse rule, by exact cohort-by-cohort simulation of a per-domain passage ledger.
//!
//! Every cohort scores the same batch for all entrants, so human demand depends on
//! `n`, the mix and the reuse rule, never on the roster. A cohort of `n` cases
//! needs `h` pure-human negatives and `m` mixed children (kind counts by largest
//! remainder, as the engine computes them). Negatives are always fresh stock and
//! scored once. Each mixed child needs a parent passage of its own domain:
//!
//! - [`Reuse::Ledger`] (`rewrite_source_ledger_v1`): a passage may be a rewrite
//!   source at most `max_uses` times, with source uses at least `min_gap` cohorts
//!   apart, in its own cohort or later. Parents come first from the cohort's own
//!   negatives, then from earlier passages that are eligible again (oldest first),
//!   then from fresh stock used as a parent only.
//! - [`Reuse::EngineV1`] (`never_reused_one_child_v1`): the live engine's rule. A
//!   parent is one of the cohort's own negatives, each parents at most one child,
//!   and nothing is ever drawn again.
//!
//! Domain quotas per kind follow weights in parts per million with a carried
//! remainder, so every batch sums exactly and each domain's running allocation
//! stays within one document of its weight. A domain that cannot supply its quota
//! binds; the plan then retires it and spreads its weight over the remaining
//! domains in proportion (what a later manifest's reweighting would do), until no
//! domain is left. The optional probe is one batch of `probe_docs` cases drawn
//! before the first cohort with the same mix, whose humans are never drawn again.
//!
//! Draws that are not cohorts also take fresh stock and are modelled:
//! - with `probe_redraw_on_reweight`, a new probe version is drawn whenever a
//!   reweighting changes the domain mix (a material batch-design change);
//! - with a [`Canary`] plan, every `every`-th cohort audits a promotion candidate
//!   on `audits` fresh draws of `docs` human documents (the lookup canary's fresh
//!   draw and, after a first flag, its re-audit), never drawn again. Its quotas
//!   follow the same weights with their own carried remainder.
//!
//! A cohort counts only when its batch and every draw scheduled in it fill.
//! Documents are counted one by one: the engine draws whole source groups, so a
//! domain whose groups hold more documents than its per-cohort quota relies on a
//! carried quota in the engine (recorded per domain in the capacity report).
//!
//! Each cohort consumes at least `h >= 1` fresh negatives, so a run on finite stock
//! ends within `stock / h + 1` cohorts.
use crate::engine_v1::kind_counts;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PLAN_RULE: &str = "public_stock_capacity_v2";
pub const LEDGER_RULE: &str = "rewrite_source_ledger_v1";
pub const ENGINE_V1_RULE: &str = "never_reused_one_child_v1";
pub const WEIGHT_RULE: &str = "test_mass_waterfill_v1";
pub const PPM: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case")]
pub enum Reuse {
    /// `rewrite_source_ledger_v1`.
    Ledger { max_uses: u32, min_gap: u32 },
    /// `never_reused_one_child_v1`.
    EngineV1,
}

impl Reuse {
    pub fn rule(&self) -> &'static str {
        match self {
            Reuse::Ledger { .. } => LEDGER_RULE,
            Reuse::EngineV1 => ENGINE_V1_RULE,
        }
    }
}

/// Lookup-canary audits drawn from the same stock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Canary {
    /// Fresh human documents per audit draw.
    pub docs: u32,
    /// Audit draws per promotion candidate (2: a first flag and its re-audit).
    pub audits: u32,
    /// A promotion candidate every `every` cohorts.
    pub every: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Params {
    pub n: u32,
    /// `[human_only, mixed, model_only]` parts.
    pub mix: [u32; 3],
    pub reuse: Reuse,
    pub probe_docs: u32,
    /// Draw a new probe version after every reweighting.
    #[serde(default)]
    pub probe_redraw_on_reweight: bool,
    /// Canary audits drawn from this stock (None: none are).
    #[serde(default)]
    pub canary: Option<Canary>,
}

impl Params {
    pub fn new(n: u32, mix: [u32; 3], reuse: Reuse, probe_docs: u32) -> Self {
        Params { n, mix, reuse, probe_docs, probe_redraw_on_reweight: false, canary: None }
    }

    pub fn check(&self) -> Result<()> {
        ensure!(self.n > 0, "n must be positive");
        ensure!(self.mix.iter().sum::<u32>() > 0, "mix must have a positive part");
        ensure!(kind_counts(self.n, self.mix)[0] > 0, "the mix must give at least one pure-human negative per cohort");
        if let Reuse::Ledger { max_uses, min_gap } = self.reuse {
            ensure!(max_uses > 0 && min_gap > 0, "reuse limit and gap must be positive");
        }
        if let Some(c) = self.canary {
            ensure!(c.docs > 0 && c.audits > 0 && c.every > 0, "canary documents, audits and interval must be positive");
            ensure!(c.docs.checked_mul(c.audits).is_some(), "canary documents per candidate overflow");
        }
        ensure!(!self.probe_redraw_on_reweight || self.probe_docs > 0, "probe re-draws need a probe");
        Ok(())
    }
}

/// What a draw is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Probe,
    Cohort,
    Canary,
}

/// Per-domain passage ledger.
#[derive(Clone, Debug, Default)]
struct Domain {
    fresh: u64,
    /// Passages eligible as parents now: (eligible since, uses left) -> count.
    ready: BTreeMap<(u32, u32), u64>,
    /// Passages waiting out the gap: eligible cohort -> uses left -> count.
    waiting: BTreeMap<u32, BTreeMap<u32, u64>>,
    /// Carried remainders: negatives, mixed, canary.
    carry: [i64; 3],
    weight: u64,
    retired_at: Option<u32>,
    used_negatives: u64,
    used_fresh_parents: u64,
}

impl Domain {
    fn release(&mut self, cohort: u32) {
        let due: Vec<u32> = self.waiting.range(..=cohort).map(|(c, _)| *c).collect();
        for c in due {
            for (uses, k) in self.waiting.remove(&c).unwrap_or_default() {
                *self.ready.entry((c, uses)).or_default() += k;
            }
        }
    }
}

/// What one batch would take from one domain.
struct Take {
    negatives: u64,
    ready_parents: Vec<((u32, u32), u64)>,
    fresh_parents: u64,
    mixed: u64,
}

fn plan_domain(d: &Domain, h: u64, m: u64, reuse: Reuse, probe: bool) -> Option<Take> {
    if d.fresh < h {
        return None;
    }
    match reuse {
        // One child per same-cohort negative; nothing else is a parent. Children
        // follow their parents' domains, so the batch compares its mixed count with
        // all drawn negatives, not with a per-domain mixed quota.
        Reuse::EngineV1 => {
            Some(Take { negatives: h, ready_parents: Vec::new(), fresh_parents: 0, mixed: h })
        }
        Reuse::Ledger { .. } => {
            let mut need = m.saturating_sub(h);
            let mut ready_parents = Vec::new();
            if !probe {
                for (key, k) in &d.ready {
                    if need == 0 {
                        break;
                    }
                    let t = (*k).min(need);
                    ready_parents.push((*key, t));
                    need -= t;
                }
            }
            if d.fresh - h < need {
                return None;
            }
            Some(Take { negatives: h, ready_parents, fresh_parents: need, mixed: m })
        }
    }
}

fn apply_take(d: &mut Domain, t: &Take, cohort: u32, reuse: Reuse, probe: bool) {
    d.fresh -= t.negatives + t.fresh_parents;
    d.used_negatives += t.negatives;
    d.used_fresh_parents += t.fresh_parents;
    let Reuse::Ledger { max_uses, min_gap } = reuse else { return };
    if probe {
        return; // probe humans are never drawn again
    }
    let next = cohort + min_gap;
    let own_parents = t.mixed.min(t.negatives);
    // Negatives that parented a child in their own cohort wait out the gap; the
    // others are eligible from the next cohort with every use left.
    if max_uses > 1 && own_parents > 0 {
        *d.waiting.entry(next).or_default().entry(max_uses - 1).or_default() += own_parents;
    }
    if t.negatives > own_parents {
        *d.ready.entry((cohort + 1, max_uses)).or_default() += t.negatives - own_parents;
    }
    for (key, k) in &t.ready_parents {
        let left = d.ready.get_mut(key).expect("planned from ready");
        *left -= k;
        if *left == 0 {
            d.ready.remove(key);
        }
        if key.1 > 1 {
            *d.waiting.entry(next).or_default().entry(key.1 - 1).or_default() += k;
        }
    }
    if max_uses > 1 && t.fresh_parents > 0 {
        *d.waiting.entry(next).or_default().entry(max_uses - 1).or_default() += t.fresh_parents;
    }
}

/// Splits `total` by the domains' weights with a carried remainder per kind slot:
/// each weighted domain gains `weight * total` ppm-documents and takes the whole
/// part; the leftover goes to the largest carries, ties by key order.
fn carried_split(domains: &mut BTreeMap<String, Domain>, slot: usize, total: u64) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let mut given = 0u64;
    for (k, d) in domains.iter_mut() {
        if d.weight == 0 {
            out.insert(k.clone(), 0);
            continue;
        }
        d.carry[slot] += (d.weight * total) as i64;
        let q = if d.carry[slot] > 0 { d.carry[slot] as u64 / PPM } else { 0 };
        d.carry[slot] -= (q * PPM) as i64;
        given += q;
        out.insert(k.clone(), q);
    }
    let mut order: Vec<(i64, String)> =
        domains.iter().filter(|(_, d)| d.weight > 0).map(|(k, d)| (d.carry[slot], k.clone())).collect();
    order.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let left = total.saturating_sub(given);
    for (_, k) in order.into_iter().cycle().take(left as usize) {
        *out.get_mut(&k).expect("weighted domain") += 1;
        domains.get_mut(&k).expect("weighted domain").carry[slot] -= PPM as i64;
    }
    out
}

enum Batch {
    /// Fresh documents taken, and the domains retired on the way.
    Filled(u64, Vec<String>),
    /// Every domain has bound.
    Exhausted,
    /// The reuse rule cannot supply the mixed parents with the stock it has.
    MixedShort { have: u64, want: u64 },
}

struct Sim {
    domains: BTreeMap<String, Domain>,
    reuse: Reuse,
    /// Retire and re-spread binding domains (false: stop at the first bind).
    reweight: bool,
}

impl Sim {
    fn new(stock: &BTreeMap<String, u64>, weights: &BTreeMap<String, u64>, reuse: Reuse, reweight: bool) -> Result<Self> {
        ensure!(weights.values().sum::<u64>() == PPM, "domain weights must sum to {PPM} ppm");
        ensure!(weights.keys().all(|k| stock.contains_key(k)), "a weighted domain has no stock entry");
        let domains = stock
            .iter()
            .map(|(k, s)| (k.clone(), Domain { fresh: *s, weight: weights.get(k).copied().unwrap_or(0), ..Domain::default() }))
            .collect();
        Ok(Sim { domains, reuse, reweight })
    }

    fn batch(&mut self, cohort: u32, c: [u32; 3], kind: Kind) -> Batch {
        // Probe and canary humans are never drawn again.
        let probe = kind != Kind::Cohort;
        let (h_slot, m) = if kind == Kind::Canary { (2, 0) } else { (0, c[1]) };
        let mut retired = Vec::new();
        loop {
            if self.domains.values().all(|d| d.weight == 0) {
                return Batch::Exhausted;
            }
            for d in self.domains.values_mut() {
                d.release(cohort);
            }
            let saved: Vec<[i64; 3]> = self.domains.values().map(|d| d.carry).collect();
            let hq = carried_split(&mut self.domains, h_slot, u64::from(c[0]));
            let mq = if m > 0 {
                carried_split(&mut self.domains, 1, u64::from(m))
            } else {
                self.domains.keys().map(|k| (k.clone(), 0)).collect()
            };
            let mut takes = BTreeMap::new();
            let mut failed = Vec::new();
            for (k, d) in &self.domains {
                if d.weight > 0 {
                    match plan_domain(d, hq[k], mq[k], self.reuse, probe) {
                        Some(t) => {
                            takes.insert(k.clone(), t);
                        }
                        None => failed.push(k.clone()),
                    }
                }
            }
            if failed.is_empty() {
                let have: u64 = takes.values().map(|t| t.mixed).sum();
                if have < u64::from(m) {
                    for (d, s) in self.domains.values_mut().zip(saved) {
                        d.carry = s;
                    }
                    return Batch::MixedShort { have, want: u64::from(m) };
                }
                let mut fresh = 0;
                for (k, t) in &takes {
                    fresh += t.negatives + t.fresh_parents;
                    apply_take(self.domains.get_mut(k).expect("planned domain"), t, cohort, self.reuse, probe);
                }
                return Batch::Filled(fresh, retired);
            }
            for (d, s) in self.domains.values_mut().zip(saved) {
                d.carry = s;
            }
            if !self.reweight {
                retired.extend(failed);
                for k in &retired {
                    self.domains.get_mut(k).expect("domain").retired_at = Some(cohort);
                }
                return Batch::Exhausted;
            }
            let freed: u64 = failed.iter().map(|k| self.domains[k].weight).sum();
            for k in &failed {
                let d = self.domains.get_mut(k).expect("domain");
                d.weight = 0;
                d.retired_at = Some(cohort);
            }
            retired.extend(failed);
            let rest: u64 = self.domains.values().map(|d| d.weight).sum();
            if rest == 0 {
                return Batch::Exhausted;
            }
            // Spread in proportion, largest remainder, so the weights sum to PPM again.
            let mut add: Vec<(u64, String, u64)> = Vec::new();
            let mut given = 0;
            for (k, d) in &self.domains {
                if d.weight > 0 {
                    let x = u128::from(freed) * u128::from(d.weight);
                    let q = (x / u128::from(rest)) as u64;
                    given += q;
                    add.push(((x % u128::from(rest)) as u64, k.clone(), q));
                }
            }
            add.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            let extra = freed - given;
            for (i, (_, k, q)) in add.into_iter().enumerate() {
                self.domains.get_mut(&k).expect("domain").weight += q + u64::from((i as u64) < extra);
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DomainOutcome {
    pub stock: u64,
    pub weight_ppm: u64,
    pub binds_at_cohort: Option<u32>,
    pub used_negatives: u64,
    pub used_fresh_parents: u64,
    pub fresh_left: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    pub rule: &'static str,
    pub params: Params,
    pub counts_per_cohort: [u32; 3],
    pub probe_counts: Option<[u32; 3]>,
    pub probe_filled: bool,
    /// Probe versions drawn (the first included).
    pub probe_versions: u32,
    /// Canary audit draws filled.
    pub canary_audits: u32,
    /// Fresh documents taken by purpose: probe versions, cohorts and canary audits.
    pub fresh_by_purpose: BTreeMap<String, u64>,
    /// Cohorts filled under the declared weights before any domain bound.
    pub cohorts_before_first_bind: u32,
    /// Cohort at which the first domain bound (0 is the probe), and those domains.
    pub first_bind: Option<(u32, Vec<String>)>,
    /// Cohorts filled in all, retiring each binding domain and re-spreading its weight.
    pub cohorts_with_reweighting: u32,
    /// Why the next cohort could not be filled.
    pub stop: String,
    /// Fresh stock taken by the probe, cohort 1, the first cohort with reuse, and the last.
    pub fresh_taken: BTreeMap<String, u64>,
    pub domains: BTreeMap<String, DomainOutcome>,
}

fn run(stock: &BTreeMap<String, u64>, weights: &BTreeMap<String, u64>, p: &Params, reweight: bool, cohorts: Option<u32>) -> Result<Outcome> {
    p.check()?;
    let mut sim = Sim::new(stock, weights, p.reuse, reweight)?;
    let counts = kind_counts(p.n, p.mix);
    let bound = match cohorts {
        Some(c) => c,
        None => u32::try_from(stock.values().sum::<u64>() / u64::from(counts[0]) + 1).unwrap_or(u32::MAX),
    };
    let probe_counts = (p.probe_docs > 0).then(|| kind_counts(p.probe_docs, p.mix));
    let mut out = Outcome {
        rule: PLAN_RULE,
        params: p.clone(),
        counts_per_cohort: counts,
        probe_counts,
        probe_filled: false,
        probe_versions: 0,
        canary_audits: 0,
        fresh_by_purpose: ["probe", "cohorts", "canary"].iter().map(|k| (k.to_string(), 0)).collect(),
        cohorts_before_first_bind: 0,
        first_bind: None,
        cohorts_with_reweighting: 0,
        stop: String::new(),
        fresh_taken: BTreeMap::new(),
        domains: BTreeMap::new(),
    };
    let stop_reason = |b: &Batch| match b {
        Batch::Exhausted => "every weighted domain has bound".to_string(),
        Batch::MixedShort { have, want } => format!("mixed parents {have} of {want}: the reuse rule cannot supply them"),
        Batch::Filled(..) => String::new(),
    };
    let mut go = true;
    if let Some(pc) = probe_counts {
        match sim.batch(0, pc, Kind::Probe) {
            Batch::Filled(fresh, retired) => {
                out.probe_filled = true;
                out.probe_versions = 1;
                out.fresh_taken.insert("probe".into(), fresh);
                *out.fresh_by_purpose.get_mut("probe").expect("key") += fresh;
                if !retired.is_empty() {
                    out.first_bind = Some((0, retired));
                }
            }
            b => {
                out.stop = format!("probe: {}", stop_reason(&b));
                go = false;
            }
        }
    }
    let reuse_cohort = match p.reuse {
        Reuse::Ledger { min_gap, .. } => min_gap + 1,
        Reuse::EngineV1 => 2,
    };
    if go {
        'cohorts: for cohort in 1..=bound {
            // The cohort's batch, then any new probe version and canary audit it schedules.
            let mut draws: Vec<(Kind, [u32; 3], &str)> = vec![(Kind::Cohort, counts, "")];
            let mut i = 0;
            while i < draws.len() {
                let (kind, c, what) = draws[i];
                match sim.batch(cohort, c, kind) {
                    Batch::Filled(fresh, retired) => {
                        if !retired.is_empty() && out.first_bind.is_none() {
                            out.first_bind = Some((cohort, retired.clone()));
                        }
                        // A reweighting changes the batch design: draw a new probe version
                        // (a probe draw is already made under the new weights).
                        if let (true, false, Some(pc), false) =
                            (p.probe_redraw_on_reweight, retired.is_empty(), probe_counts, kind == Kind::Probe)
                        {
                            draws.push((Kind::Probe, pc, "new probe version"));
                        }
                        match kind {
                            Kind::Cohort => {
                                *out.fresh_by_purpose.get_mut("cohorts").expect("key") += fresh;
                                if cohort == 1 || cohort == reuse_cohort {
                                    out.fresh_taken.insert(format!("cohort_{cohort}"), fresh);
                                }
                                out.fresh_taken.insert("last_cohort".into(), fresh);
                                if let Some(cn) = p.canary.filter(|cn| cohort % cn.every == 0) {
                                    draws.push((Kind::Canary, [cn.docs * cn.audits, 0, 0], "canary audit"));
                                }
                            }
                            Kind::Probe => {
                                out.probe_versions += 1;
                                *out.fresh_by_purpose.get_mut("probe").expect("key") += fresh;
                            }
                            Kind::Canary => {
                                out.canary_audits += p.canary.map_or(0, |cn| cn.audits);
                                *out.fresh_by_purpose.get_mut("canary").expect("key") += fresh;
                            }
                        }
                    }
                    b => {
                        if out.first_bind.is_none() {
                            let retired: Vec<String> =
                                sim.domains.iter().filter(|(_, d)| d.retired_at == Some(cohort)).map(|(k, _)| k.clone()).collect();
                            if !retired.is_empty() {
                                out.first_bind = Some((cohort, retired));
                            }
                        }
                        out.stop = if what.is_empty() {
                            stop_reason(&b)
                        } else {
                            format!("cohort {cohort} {what}: {}", stop_reason(&b))
                        };
                        break 'cohorts;
                    }
                }
                i += 1;
            }
            out.cohorts_with_reweighting = cohort;
        }
        if out.stop.is_empty() {
            out.stop = format!("ran the requested {bound} cohorts");
        }
    }
    out.cohorts_before_first_bind = match &out.first_bind {
        Some((c, _)) => c.saturating_sub(1),
        None => out.cohorts_with_reweighting,
    };
    out.domains = sim
        .domains
        .into_iter()
        .map(|(k, d)| {
            let o = DomainOutcome {
                stock: stock[&k],
                weight_ppm: weights.get(&k).copied().unwrap_or(0),
                binds_at_cohort: d.retired_at,
                used_negatives: d.used_negatives,
                used_fresh_parents: d.used_fresh_parents,
                fresh_left: d.fresh,
            };
            (k, o)
        })
        .collect();
    Ok(out)
}

/// Runs cohorts on finite `stock` with declared weights until the stock cannot fill
/// one, retiring and re-spreading each binding domain.
pub fn simulate(stock: &BTreeMap<String, u64>, weights: &BTreeMap<String, u64>, p: &Params) -> Result<Outcome> {
    run(stock, weights, p, true, None)
}

/// Fresh documents each domain takes over `cohorts` cohorts (plus the probe) under
/// `weights`, with unlimited stock.
pub fn demand(weights: &BTreeMap<String, u64>, p: &Params, cohorts: u32) -> Result<BTreeMap<String, u64>> {
    let unlimited: BTreeMap<String, u64> = weights.keys().map(|k| (k.clone(), u64::MAX / 4)).collect();
    let o = run(&unlimited, weights, p, false, Some(cohorts))?;
    ensure!(o.cohorts_with_reweighting == cohorts, "{}", o.stop);
    Ok(o.domains.into_iter().map(|(k, d)| (k, d.used_negatives + d.used_fresh_parents)).collect())
}

/// Largest-remainder ppm of non-negative weights (ties by key order).
pub fn to_ppm(w: &BTreeMap<String, f64>) -> BTreeMap<String, u64> {
    let sum: f64 = w.values().sum();
    let mut out: BTreeMap<String, u64> = w.keys().map(|k| (k.clone(), 0)).collect();
    if sum <= 0.0 {
        return out;
    }
    let mut rem = Vec::new();
    let mut given = 0u64;
    for (k, v) in w {
        let x = v / sum * PPM as f64;
        let q = x.floor() as u64;
        given += q;
        out.insert(k.clone(), q);
        rem.push((x - q as f64, k.clone()));
    }
    rem.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    for (_, k) in rem.into_iter().take(PPM.saturating_sub(given) as usize) {
        *out.get_mut(&k).expect("key") += 1;
    }
    out
}

/// Stock-proportional weights: the live engine's per-draw rule, held fixed.
pub fn stock_weights(stock: &BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    to_ppm(&stock.iter().map(|(k, v)| (k.clone(), *v as f64)).collect())
}

#[derive(Clone, Debug, Serialize)]
pub struct Reweighting {
    pub rule: &'static str,
    pub horizon_cohorts: u32,
    pub base_ppm: BTreeMap<String, u64>,
    pub weights_ppm: BTreeMap<String, u64>,
    /// Domains with no stock (weight 0).
    pub retired: Vec<String>,
    /// Domains held to what their stock sustains over the horizon.
    pub reduced: Vec<String>,
    /// Total variation distance between the base mass and the weights, in ppm.
    pub estimand_change_tv_ppm: u64,
}

/// Water-filling with per-domain caps (fractions): capped domains take their cap,
/// the rest share what is left in proportion to their base mass.
fn fill(base: &BTreeMap<String, u64>, caps: &BTreeMap<String, f64>) -> Result<(BTreeMap<String, u64>, Vec<String>)> {
    let mut fixed: BTreeMap<String, f64> = BTreeMap::new();
    loop {
        let rest = 1.0 - fixed.values().sum::<f64>();
        let free: Vec<&String> = base.keys().filter(|k| !fixed.contains_key(*k)).collect();
        let mass: f64 = free.iter().map(|k| base[*k] as f64).sum();
        ensure!(mass > 0.0 && rest > 0.0, "the stock cannot supply the horizon in any domain mix");
        let over: Vec<String> =
            free.iter().filter(|k| base[**k] as f64 / mass * rest > caps[**k]).map(|k| (*k).clone()).collect();
        if over.is_empty() {
            let mut w = fixed.clone();
            for k in free {
                w.insert(k.clone(), base[k] as f64 / mass * rest);
            }
            let mut ppm = to_ppm(&w);
            // Rounding must not lift a capped domain over its cap.
            let mut surplus = 0;
            for (k, v) in &fixed {
                let ceiling = (v * PPM as f64).floor() as u64;
                if ppm[k] > ceiling {
                    surplus += ppm[k] - ceiling;
                    *ppm.get_mut(k).expect("key") = ceiling;
                }
            }
            if surplus > 0 {
                let top = ppm.iter().filter(|(k, _)| !fixed.contains_key(*k)).max_by_key(|(_, v)| **v).map(|(k, _)| k.clone());
                *ppm.get_mut(&top.expect("a free domain")).expect("key") += surplus;
            }
            return Ok((ppm, fixed.keys().cloned().collect()));
        }
        for k in over {
            let c = caps[&k];
            fixed.insert(k, c);
        }
    }
}

/// `test_mass_waterfill_v1`: start from the base domain mass, retire domains with
/// no stock, and hold every domain whose stock cannot last `horizon` cohorts to the
/// largest weight its stock sustains for them (checked by simulating the demand),
/// spreading the remainder over the other domains in proportion to their mass.
pub fn waterfill(base: &BTreeMap<String, u64>, stock: &BTreeMap<String, u64>, p: &Params, horizon: u32) -> Result<Reweighting> {
    ensure!(horizon > 0, "horizon must be positive");
    ensure!(base.keys().all(|k| stock.contains_key(k)), "a base domain has no stock entry");
    let one: BTreeMap<String, u64> = [("unit".to_string(), PPM)].into();
    let unit = demand(&one, p, horizon)?["unit"];
    let mut caps: BTreeMap<String, f64> = base
        .keys()
        .map(|k| (k.clone(), if base[k] == 0 { 0.0 } else { stock[k] as f64 / unit as f64 }))
        .collect();
    // Each round lowers at least one cap by at least 1 ppm, so it ends.
    loop {
        let (weights, capped) = fill(base, &caps)?;
        let used = demand(&weights, p, horizon)?;
        let over: Vec<&String> = weights.keys().filter(|k| weights[*k] > 0 && used[*k] > stock[*k]).collect();
        if over.is_empty() {
            let base_ppm = to_ppm(&base.iter().map(|(k, v)| (k.clone(), *v as f64)).collect());
            let tv = base_ppm.keys().map(|k| base_ppm[k].abs_diff(weights[k])).sum::<u64>() / 2;
            return Ok(Reweighting {
                rule: WEIGHT_RULE,
                horizon_cohorts: horizon,
                retired: weights.iter().filter(|(_, v)| **v == 0).map(|(k, _)| k.clone()).collect(),
                reduced: capped.into_iter().filter(|k| weights[k] > 0).collect(),
                base_ppm,
                weights_ppm: weights,
                estimand_change_tv_ppm: tv,
            });
        }
        for k in over {
            let w = weights[k] as f64 / PPM as f64;
            let scaled = w * stock[k] as f64 / used[k] as f64;
            let c = caps.get_mut(k).expect("key");
            *c = (c.min(scaled) - 1.0 / PPM as f64).max(0.0);
        }
    }
}

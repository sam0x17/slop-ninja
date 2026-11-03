//! `r2_test2_carve_v3`: the Test-2 T carve and the worst-case mock reserve taken from
//! pool v2's stock, for protocol A2's step A8 (§5.3, §12.5, §14.2; amendment 1 items
//! 1.4, 1.5, 1.11 and 1.16; amendment 3).
//!
//! **Frame.** The carve draws from pool v2's reservation-stage stock: the
//! Test-reservation remainder (the reserved-unassigned groups and the Wikipedia index
//! remainder) after the reservation gate, so every carved group has already passed
//! closure against Train, the sealed Test, the evaluation splits, the reference's
//! training lineage, the Common Crawl stock and public-candidate-v1. The consumed
//! Test's own groups (set (i)) are checked absent by group id.
//!
//! **Units** (amendment 3 item 3.1). Every Test-2 T target and every mock demand is
//! counted in pool v2's lower-bound documents, as A8's simulation is (S0-5: H = 365,
//! lower-bound yield): a selection holding `u` expected micro-documents in a domain
//! counts floor(u / 10^6 × realization_lower), exactly as the pool's capacity does, so
//! a carve's removal reproduces in a pool rebuild. The four small domains' target is
//! ⌈1.3 × 150⌉ = 195 of these documents, and every carved domain's group floor is
//! ⌈1.3 × 15⌉ = 20 groups.
//!
//! **Keyed order** (amendment 3 item 3.2). Within each carved domain, groups are ranked
//! by HMAC-SHA-256(K_test2, `slop_ninja_a2_test2_carve_v3:` + domain + `:` +
//! group_key) over the UTF-8 bytes, compared as 32-byte big-endian integers, ascending.
//! Test-2 T is the shortest prefix holding the domain's target and the group floor;
//! the reserve continues in the same order until the carve plus reserve covers the
//! domain's mock demand. The key is read from an owner-only file and checked against
//! its A6 commitment as the freeze log records it; neither the key nor any rank is
//! written or printed.
//!
//! **Steps.** The pool's manifest weights are first reproduced from its own groups and
//! capacity record. The reserve is sized at B_res mock batches under
//! `rewrite_source_ledger_v1` at the manifest weights renormalised over the carved
//! domains; the H = 365 surplus bound is checked for domains that do not bind at the
//! horizon; the stock less the carve and reserve is re-waterfilled (W_A8); AR-8 is
//! re-run; the reserve is checked against W_A8's mock demand, with one second iteration
//! by the same rule if a domain's demand rises above it. On failure the binding domains,
//! the largest B_res′ ≤ B_res at which AR-8 passes and the largest at which every A8
//! criterion passes are reported, each by the full procedure, and nothing can be applied.
//!
//! **Apply** (F0b) needs the freeze log's hash chain intact, the draft's digest logged
//! as `carve_draft_sha256`, and recorded user decisions on U1, U2, U16 and U17 with U1
//! confirmed (amendment 1 item 1.5; amendment 3 item 3.3). It recomputes the draft,
//! writes allow-list v3 and rebuilds pool v2 under it; the build refuses before writing
//! if its weights are not W_A8.
//! **Release** returns unused reserve groups only by a logged amendment, after the
//! Test-2 mock is built and one of item 1.11's conditions holds. It keeps every group
//! of the sealed Test-2 T membership and every group the mock used, both bound by
//! digest to freeze-log entries, and re-waterfills on the stock in force, after every
//! current exclusion, the active reference's lineage included (amendment 3 item 3.5).
use crate::capacity::{self, PPM, Params, Reuse};
use crate::gate::Gate;
use crate::pool_v2::{self, BuildParams, CanaryReport};
use crate::stock::{
    self, CarveExclusion, FileRef, MICRO, StockAllowlist, StockGroup, check_reference, for_each_line, read_checked,
    resolve_reference,
};
use crate::{check_rel, data_dir, is_digest, rel_under, resolve, sha256_hex};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const CARVE_RULE: &str = "r2_test2_carve_v3";
pub const RECORD_SCHEMA: &str = "slop_ninja_a2_carve_record_v1";
pub const CARVE_GROUP_SCHEMA: &str = "slop_ninja_a2_carve_group_v1";
pub const RELEASE_SCHEMA: &str = "slop_ninja_a2_carve_release_v1";
pub const RANK_LABEL: &str = "slop_ninja_a2_test2_carve_v3:";
pub const ROLE_TEST2: &str = "test2_t";
pub const ROLE_RESERVE: &str = "mock_reserve";
pub const KEY_BYTES: usize = 32;
/// Item 1.11: reserve release waits for one of these.
pub const RELEASE_CONDITIONS: [&str; 3] = ["effective_cohort_closed", "replacement_lapsed", "u17_failed"];
/// Item 1.5: A8's write needs a recorded user decision on each (amendment 3 item 3.3).
pub const A8_DECISIONS: [&str; 4] = ["U1", "U2", "U16", "U17"];
/// Freeze-log fields the tool reads (amendment 3 items 3.3 and 3.5).
pub const LOG_DRAFT_FIELD: &str = "carve_draft_sha256";
pub const LOG_DECISIONS_FIELD: &str = "user_decisions";
pub const LOG_TEST2_T_FIELD: &str = "test2_t_groups_sha256";
pub const LOG_MOCK_USED_FIELD: &str = "test2_mock_used_groups_sha256";
pub const DRAFT_FILE: &str = "CARVE-A2.draft.json";
pub const GROUPS_FILE: &str = "carve-groups.jsonl";

// ------------------------------------------------------------------ keyed order

/// HMAC-SHA-256 (RFC 2104) over `sha2`.
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let (mut ipad, mut opad) = ([0x36u8; BLOCK], [0x5cu8; BLOCK]);
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new().chain_update(ipad).chain_update(msg).finalize();
    Sha256::new().chain_update(opad).chain_update(inner).finalize().into()
}

/// A group's rank in its domain's keyed order.
pub fn rank(key: &[u8; KEY_BYTES], domain: &str, group_key: &str) -> [u8; 32] {
    let mut m = Vec::with_capacity(RANK_LABEL.len() + domain.len() + 1 + group_key.len());
    m.extend_from_slice(RANK_LABEL.as_bytes());
    m.extend_from_slice(domain.as_bytes());
    m.push(b':');
    m.extend_from_slice(group_key.as_bytes());
    hmac_sha256(key, &m)
}

// ------------------------------------------------------------------ parameters

/// A carved domain's Test-2 T target: lower-bound documents and the group floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub lower_documents: u64,
    pub min_groups: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CarveParams {
    pub targets: BTreeMap<String, Target>,
    /// Mock batches the reserve is sized for (B_res).
    pub b_res: u32,
    /// The mock batch: n, mix and ledger, no probe.
    pub mock: Params,
    /// The manifest's horizon (H), in cohorts.
    pub horizon: u32,
    /// AR-8: days of capacity at the configured n and mix.
    pub ar8_min_days: u32,
    /// The first sizing plus the second-iteration rule.
    pub iterations: u32,
    /// The one domain whose Test-2 T is raised at Stage 3 from its own reserve (§12.5).
    pub stage3_raised_domain: String,
}

/// The inherited reservation yield buffer, in tenths (1.3), and the gated-slice minimum.
pub const BUFFER_TENTHS: u64 = 13;
pub const SLICE_HUMANS: u64 = 150;
pub const SLICE_GROUPS: u64 = 15;

fn buffered(x: u64) -> u64 {
    (x * BUFFER_TENTHS).div_ceil(10)
}

impl CarveParams {
    /// Protocol A2's values: §12.5 / V10 targets in lower-bound documents, the slice
    /// minimum at the inherited 1.3 buffer (195 documents, 20 groups; amendment 3 item
    /// 3.1), B_res = 58 and the mock batch of V6, the manifest horizon H = 365, AR-8's 90
    /// days, one second iteration (§5.3 step 4), and legal raised at Stage 3.
    pub fn protocol_a2() -> Self {
        let floor = buffered(SLICE_GROUPS);
        let mut targets = BTreeMap::new();
        let mut put = |d: &str, docs: u64| {
            targets.insert(d.to_string(), Target { lower_documents: docs, min_groups: floor });
        };
        put("legal_opinions", 450);
        put("wikipedia", 400);
        for d in ["reddit", "ubuntu_irc", "wikipedia_talk", "fineweb_blogs"] {
            put(d, 300);
        }
        put("fineweb", 220);
        for d in ["wikisource", "plos", "instructional", "peer_review"] {
            put(d, buffered(SLICE_HUMANS));
        }
        CarveParams {
            targets,
            b_res: 58,
            mock: Params::new(1000, [20, 55, 25], Reuse::Ledger { max_uses: 4, min_gap: 7 }, 0),
            horizon: 365,
            ar8_min_days: 90,
            iterations: 2,
            stage3_raised_domain: "legal_opinions".into(),
        }
    }

    fn check(&self) -> Result<()> {
        ensure!(!self.targets.is_empty() && self.b_res > 0 && self.horizon > 0 && self.iterations > 0, "carve parameters");
        ensure!(self.targets.contains_key(&self.stage3_raised_domain), "the Stage-3 raised domain must be carved");
        ensure!(self.mock.probe_docs == 0 && !self.mock.probe_redraw_on_reweight && self.mock.canary.is_none(), "the mock batch has no probe or canary");
        self.mock.check()
    }
}

// ------------------------------------------------------------------ inputs

#[derive(Clone, Debug)]
pub struct Candidate {
    pub rank: [u8; 32],
    pub group_key: String,
    pub units: u64,
    pub udocs: u64,
}

#[derive(Clone, Debug, Default)]
pub struct DomainStock {
    pub test_mass: u64,
    pub realization: f64,
    pub realization_lower: f64,
    /// Expected micro-documents of every stock group of the domain.
    pub udocs: u64,
    /// The domain's groups in keyed order (carved domains only).
    pub candidates: Vec<Candidate>,
}

#[derive(Clone, Debug)]
pub struct Inputs {
    pub domains: BTreeMap<String, DomainStock>,
    pub plan: Params,
    pub cohorts_per_day: u32,
    pub horizon: u32,
    pub manifest_weights: BTreeMap<String, u64>,
    /// The pool's recorded lower-bound documents, reproduced from the groups.
    pub recorded_lower: BTreeMap<String, u64>,
    /// Every group the consumed Test's split holds (a superset of set (i)).
    pub consumed_test: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarveGroup {
    pub schema: String,
    pub rule: String,
    pub domain: String,
    pub group_key: String,
    pub role: String,
    /// Position in the domain's keyed order.
    pub order: u64,
    /// The sizing iteration that took it (1, or 2 under the second-iteration rule).
    pub iteration: u32,
    pub units: u64,
    pub expected_udocs: u64,
}

pub fn lower(udocs: u64, r: f64) -> u64 {
    (udocs as f64 / MICRO * r).floor() as u64
}

fn renorm(w: &BTreeMap<String, u64>, over: &BTreeSet<String>) -> Result<BTreeMap<String, u64>> {
    let sub: BTreeMap<String, f64> = over.iter().map(|d| (d.clone(), w.get(d).copied().unwrap_or(0) as f64)).collect();
    ensure!(sub.values().any(|v| *v > 0.0), "no weight on the carved domains");
    Ok(capacity::to_ppm(&sub))
}

/// Total variation distance in ppm (as `test_mass_waterfill_v1` reports it).
pub fn tv_ppm(a: &BTreeMap<String, u64>, b: &BTreeMap<String, u64>) -> u64 {
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.iter().map(|k| a.get(*k).copied().unwrap_or(0).abs_diff(b.get(*k).copied().unwrap_or(0))).sum::<u64>() / 2
}

/// Extends a keyed prefix until `done` holds; false if the domain runs out first.
fn extend(c: &[Candidate], mut end: usize, mut udocs: u64, done: impl Fn(usize, u64) -> bool) -> (usize, u64, bool) {
    while !done(end, udocs) {
        if end == c.len() {
            return (end, udocs, false);
        }
        udocs += c[end].udocs;
        end += 1;
    }
    (end, udocs, true)
}

#[derive(Clone, Debug, Default)]
struct Pick {
    test2: usize,
    test2_udocs: u64,
    test2_filled: bool,
    whole: usize,
    whole_udocs: u64,
    whole_filled: bool,
    /// (end index, iteration) of each reserve extension.
    marks: Vec<(usize, u32)>,
}

/// The lower-bound and realized stock after removing `removed` micro-documents.
fn stock_after(inp: &Inputs, removed: &BTreeMap<String, u64>) -> (BTreeMap<String, u64>, BTreeMap<String, u64>) {
    let mut lo = BTreeMap::new();
    let mut re = BTreeMap::new();
    for (d, s) in &inp.domains {
        let u = s.udocs.saturating_sub(removed.get(d).copied().unwrap_or(0));
        lo.insert(d.clone(), lower(u, s.realization_lower));
        re.insert(d.clone(), lower(u, s.realization));
    }
    (lo, re)
}

fn base(inp: &Inputs) -> BTreeMap<String, u64> {
    inp.domains.iter().map(|(d, s)| (d.clone(), s.test_mass)).collect()
}

fn retired(w: &BTreeMap<String, u64>) -> BTreeSet<String> {
    w.iter().filter(|(_, v)| **v == 0).map(|(k, _)| k.clone()).collect()
}

/// The manifest in force must reproduce from the pool's own groups, capacity record and
/// plan before anything is computed on it.
fn reproduce(inp: &Inputs, horizon: u32) -> Result<()> {
    for (d, s) in &inp.domains {
        let got = lower(s.udocs, s.realization_lower);
        let want = inp.recorded_lower.get(d).copied().unwrap_or(0);
        ensure!(got == want, "refused: {d}: the groups give {got} lower-bound documents, the pool records {want}");
    }
    let rw = capacity::waterfill(&base(inp), &inp.recorded_lower, &inp.plan, horizon)?;
    ensure!(rw.weights_ppm == inp.manifest_weights, "refused: the pool's manifest weights do not reproduce from its stock and plan");
    Ok(())
}

/// Re-waterfill and AR-8 for one removal.
struct Trial {
    weights: Result<capacity::Reweighting, String>,
    ar8: Value,
    ar8_pass: bool,
}

fn trial(inp: &Inputs, p: &CarveParams, removed: &BTreeMap<String, u64>) -> Result<Trial> {
    let (lo, re) = stock_after(inp, removed);
    let w = match capacity::waterfill(&base(inp), &lo, &inp.plan, p.horizon) {
        Ok(w) => w,
        Err(e) => {
            let msg = format!("{e:#}");
            return Ok(Trial {
                ar8: json!({"pass": false, "waterfill": msg}),
                weights: Err(msg),
                ar8_pass: false,
            });
        }
    };
    let sim_lo = capacity::simulate(&lo, &w.weights_ppm, &inp.plan)?;
    let sim_re = capacity::simulate(&re, &w.weights_ppm, &inp.plan)?;
    let per_day = f64::from(inp.cohorts_per_day);
    let days_lo = f64::from(sim_lo.cohorts_before_first_bind) / per_day;
    let newly: Vec<String> = retired(&w.weights_ppm).difference(&retired(&inp.manifest_weights)).cloned().collect();
    let pass = days_lo >= f64::from(p.ar8_min_days) && newly.is_empty();
    let ar8 = json!({
        "rule": "AR-8: at least ar8_min_days of capacity at the configured n and mix (lower-bound yield), and no newly retired domain",
        "pass": pass,
        "days_before_first_bind_lower_yield": days_lo,
        "days_before_first_bind": f64::from(sim_re.cohorts_before_first_bind) / per_day,
        "days_with_reweighting": f64::from(sim_re.cohorts_with_reweighting) / per_day,
        "first_bind_lower_yield": sim_lo.first_bind,
        "newly_retired": newly,
        "stock_lower_documents": lo,
    });
    Ok(Trial { weights: Ok(w), ar8, ar8_pass: pass })
}

/// The outcome of a carve computation.
pub struct Computed {
    pub result: Value,
    pub groups: Vec<CarveGroup>,
    pub pass: bool,
}

/// Runs `r2_test2_carve_v3` on in-memory inputs.
pub fn compute(inp: &Inputs, p: &CarveParams) -> Result<Computed> {
    compute_inner(inp, p, true)
}

fn compute_inner(inp: &Inputs, p: &CarveParams, search: bool) -> Result<Computed> {
    p.check()?;
    ensure!(inp.horizon == p.horizon, "refused: the manifest's horizon is {} cohorts, the protocol's H is {}", inp.horizon, p.horizon);
    let carved: BTreeSet<String> = p.targets.keys().cloned().collect();
    for d in &carved {
        ensure!(inp.domains.contains_key(d), "carved domain {d} has no stock entry");
    }
    // The manifest in force, reproduced from the pool's own groups and capacity record.
    reproduce(inp, p.horizon)?;
    let wm = &inp.manifest_weights;
    let sim_m = capacity::simulate(&inp.recorded_lower, wm, &inp.plan)?;
    let d_h = capacity::demand(wm, &inp.plan, p.horizon)?;
    let binds_at_h = |d: &str| sim_m.domains.get(d).and_then(|o| o.binds_at_cohort).is_some_and(|c| c <= p.horizon + 1);
    let unit: BTreeMap<String, u64> = [("unit".to_string(), PPM)].into();
    let mock_unit_demand = capacity::demand(&unit, &p.mock, p.b_res)?["unit"];

    // Test-2 T prefixes.
    let mut picks: BTreeMap<String, Pick> = BTreeMap::new();
    for (d, t) in &p.targets {
        let s = &inp.domains[d];
        let r = s.realization_lower;
        let floor = usize::try_from(t.min_groups).unwrap_or(usize::MAX);
        let (end, u, ok) = extend(&s.candidates, 0, 0, |n, u| lower(u, r) >= t.lower_documents && n >= floor);
        picks.insert(d.clone(), Pick { test2: end, test2_udocs: u, test2_filled: ok, whole: end, whole_udocs: u, whole_filled: true, marks: Vec::new() });
    }

    // Sizing iterations: the worst case at the manifest weights renormalised over the
    // carved domains, then (if needed) once more at W_A8's demand.
    let mut sizing = renorm(wm, &carved)?;
    let mut iterations = Vec::new();
    let mut last: Option<(Trial, BTreeMap<String, bool>)> = None;
    for it in 1..=p.iterations {
        let need = capacity::demand(&sizing, &p.mock, p.b_res)?;
        for (d, pk) in picks.iter_mut() {
            let s = &inp.domains[d];
            let r = s.realization_lower;
            let want = need[d];
            let start = pk.whole;
            let (end, u, ok) = extend(&s.candidates, pk.whole, pk.whole_udocs, |_, u| lower(u, r) >= want);
            pk.whole = end;
            pk.whole_udocs = u;
            pk.whole_filled = ok;
            if end > start {
                pk.marks.push((end, it));
            }
        }
        let removed: BTreeMap<String, u64> = picks.iter().map(|(d, pk)| (d.clone(), pk.whole_udocs)).collect();
        let t = trial(inp, p, &removed)?;
        let (wa8_need, covered) = match &t.weights {
            Ok(w) => {
                let n = capacity::demand(&renorm(&w.weights_ppm, &carved)?, &p.mock, p.b_res)?;
                let c: BTreeMap<String, bool> =
                    picks.iter().map(|(d, pk)| (d.clone(), lower(pk.whole_udocs, inp.domains[d].realization_lower) >= n[d])).collect();
                (n, c)
            }
            Err(_) => (BTreeMap::new(), carved.iter().map(|d| (d.clone(), false)).collect()),
        };
        let per_domain: BTreeMap<String, Value> = picks
            .iter()
            .map(|(d, pk)| {
                let s = &inp.domains[d];
                let r = s.realization_lower;
                let units = |a: usize, b: usize| s.candidates[a..b].iter().map(|c| c.units).sum::<u64>();
                let reserve_udocs = pk.whole_udocs - pk.test2_udocs;
                (d.clone(), json!({
                    "target": p.targets[d],
                    "test2_t": {"groups": pk.test2, "units": units(0, pk.test2), "expected_documents": pk.test2_udocs as f64 / MICRO,
                        "lower_documents": lower(pk.test2_udocs, r), "filled": pk.test2_filled},
                    "mock_reserve": {"groups": pk.whole - pk.test2, "units": units(pk.test2, pk.whole),
                        "expected_documents": reserve_udocs as f64 / MICRO},
                    "carve_plus_reserve": {"groups": pk.whole, "lower_documents": lower(pk.whole_udocs, r),
                        "expected_documents": pk.whole_udocs as f64 / MICRO, "share_of_stock_lower": lower(pk.whole_udocs, r) as f64 / inp.recorded_lower[d].max(1) as f64},
                    "sizing_mock_demand": need[d],
                    "covers_sizing_demand": pk.whole_filled,
                    "w_a8_mock_demand": wa8_need.get(d),
                    "covers_w_a8_demand": covered[d],
                }))
            })
            .collect();
        let all_covered = covered.values().all(|c| *c);
        iterations.push(json!({
            "iteration": it,
            "sizing_weights": if it == 1 { "the manifest weights renormalised over the carved domains" } else { "W_A8 of the previous iteration, renormalised over the carved domains" },
            "sizing_weights_ppm": sizing,
            "sizing_mock_demand_total": need.values().sum::<u64>(),
            "domains": per_domain,
            "reweighting": t.weights.as_ref().ok(),
            "waterfill_error": t.weights.as_ref().err(),
            "ar8": t.ar8,
            "reserve_covers_w_a8_demand": all_covered,
        }));
        let next = match &t.weights {
            Ok(w) if !all_covered => Some(renorm(&w.weights_ppm, &carved)?),
            _ => None,
        };
        last = Some((t, covered));
        match next {
            Some(n) => sizing = n,
            None => break,
        }
    }
    let (fin, covered) = last.context("no sizing iteration ran")?;

    // H = 365 surplus bound (§5.3 step 3), on the final carve.
    let mut surplus = BTreeMap::new();
    let mut surplus_ok = true;
    for (d, pk) in &picks {
        let c = lower(pk.whole_udocs, inp.domains[d].realization_lower);
        let s_d = inp.recorded_lower[d];
        let left = s_d as i64 - d_h.get(d).copied().unwrap_or(0) as i64;
        let v = if binds_at_h(d) {
            json!({"binds_at_h": true, "stock_lower": s_d, "demand_at_h": d_h.get(d), "surplus": left, "carve_plus_reserve": c,
                "status": "binds at the horizon under the manifest in force: the carve shortens its horizon, and U1 accepts the re-waterfill (§5.3 steps 3-4)"})
        } else {
            let ok = c as i64 <= left;
            surplus_ok &= ok;
            json!({"binds_at_h": false, "stock_lower": s_d, "demand_at_h": d_h.get(d), "surplus": left, "carve_plus_reserve": c,
                "status": if ok { "within the surplus at the horizon" } else { "exceeds the surplus at the horizon" }, "holds": ok})
        };
        surplus.insert(d.clone(), v);
    }

    // The groups, and set (i).
    let mut groups = Vec::new();
    for (d, pk) in &picks {
        let s = &inp.domains[d];
        for (i, c) in s.candidates[..pk.whole].iter().enumerate() {
            let (role, iteration) = if i < pk.test2 {
                (ROLE_TEST2, 1)
            } else {
                (ROLE_RESERVE, pk.marks.iter().find(|(end, _)| i < *end).map_or(1, |m| m.1))
            };
            groups.push(CarveGroup {
                schema: CARVE_GROUP_SCHEMA.into(),
                rule: CARVE_RULE.into(),
                domain: d.clone(),
                group_key: c.group_key.clone(),
                role: role.into(),
                order: i as u64,
                iteration,
                units: c.units,
                expected_udocs: c.udocs,
            });
        }
    }
    let hits = groups.iter().filter(|g| inp.consumed_test.contains(&g.group_key)).count();
    ensure!(hits == 0, "refused: {hits} carved groups belong to the consumed Test's split (set (i), amendment 1 item 1.16)");

    let filled: Vec<&String> = picks.iter().filter(|(_, pk)| !(pk.test2_filled && pk.whole_filled)).map(|(d, _)| d).collect();
    let all_covered = covered.values().all(|c| *c);
    let mut reasons = Vec::new();
    if !filled.is_empty() {
        reasons.push(format!("the stock cannot fill the target or the mock demand in {filled:?}"));
    }
    if !surplus_ok {
        reasons.push("a domain that does not bind at H exceeds its surplus".to_string());
    }
    if !fin.ar8_pass {
        reasons.push("AR-8 fails with the carve and the reserve".to_string());
    }
    if !all_covered {
        reasons.push("the reserve does not cover W_A8's demand after the second iteration".to_string());
    }
    let pass = reasons.is_empty();

    // TV report.
    let tv = match &fin.weights {
        Ok(w) => {
            let test2_only: BTreeMap<String, u64> = picks.iter().map(|(d, pk)| (d.clone(), pk.test2_udocs)).collect();
            let (lo_t, _) = stock_after(inp, &test2_only);
            let t_only = match capacity::waterfill(&base(inp), &lo_t, &inp.plan, p.horizon) {
                Ok(wt) => json!({"weights_ppm": wt.weights_ppm, "tv_from_manifest_ppm": tv_ppm(wm, &wt.weights_ppm),
                    "tv_from_manifest_carved_domains_ppm": tv_ppm(&renorm(wm, &carved)?, &renorm(&wt.weights_ppm, &carved)?)}),
                Err(e) => json!({"waterfill_error": format!("{e:#}")}),
            };
            json!({
                "full_carve_and_reserve": {"tv_from_manifest_ppm": tv_ppm(wm, &w.weights_ppm),
                    "tv_from_manifest_carved_domains_ppm": tv_ppm(&renorm(wm, &carved)?, &renorm(&w.weights_ppm, &carved)?),
                    "tv_from_test_mass_ppm": w.estimand_change_tv_ppm},
                "test2_t_carve_alone": t_only,
            })
        }
        Err(e) => json!({"waterfill_error": e}),
    };

    // Failure path (item 1.4; amendment 3 item 3.6): the binding domains, and the largest
    // B_res' at which AR-8 passes and at which every A8 criterion passes, each by this
    // same procedure (both iterations, coverage, the surplus bound and the fill).
    let failure = if pass || !search {
        Value::Null
    } else {
        let mut tried = Vec::new();
        let (mut ar8_at, mut a8_at) = (None, None);
        for b in (1..=p.b_res).rev() {
            let q = CarveParams { b_res: b, ..p.clone() };
            let entry = match compute_inner(inp, &q, false) {
                Ok(c) => {
                    let ar8 = c.result["ar8"]["pass"] == true;
                    if ar8 && ar8_at.is_none() {
                        ar8_at = Some(b);
                    }
                    if c.pass && a8_at.is_none() {
                        a8_at = Some(b);
                    }
                    json!({"b_res": b, "ar8_pass": ar8, "a8_pass": c.pass, "reasons": c.result["verdict"]["reasons"],
                        "days_before_first_bind_lower_yield": c.result["ar8"]["days_before_first_bind_lower_yield"]})
                }
                Err(e) => json!({"b_res": b, "error": format!("{e:#}")}),
            };
            tried.push(entry);
            if ar8_at.is_some() && a8_at.is_some() {
                break;
            }
        }
        json!({
            "applied": false,
            "reasons": reasons,
            "binding_domains": {
                "first_bind_lower_yield": fin.ar8["first_bind_lower_yield"],
                "newly_retired": fin.ar8["newly_retired"],
                "reduced_by_the_re_waterfill": fin.weights.as_ref().map(|w| w.reduced.clone()).unwrap_or_default(),
                "not_covering_w_a8_demand": covered.iter().filter(|(_, c)| !**c).map(|(d, _)| d).collect::<Vec<_>>(),
                "unfilled": filled,
                "over_the_surplus": surplus.iter().filter(|(_, v)| v["holds"] == false).map(|(d, _)| d).collect::<Vec<_>>(),
            },
            "largest_b_res_prime_with_ar8_passing": ar8_at,
            "largest_b_res_prime_passing_a8": a8_at,
            "search_rule": "descending from B_res; each B_res' runs the full procedure (both iterations, coverage at W_A8, the surplus bound and the fill); the search stops once both values are found",
            "search": tried,
            "next": "nothing is applied and no pool-v2 acquisition starts; the user chooses (i) an amended reserve at B_res' or (ii) pool v2 without the reserve (amendment 1 item 1.4)",
        })
    };

    let mut by_domain_role: BTreeMap<&str, BTreeMap<&str, u64>> = BTreeMap::new();
    for g in &groups {
        *by_domain_role.entry(g.domain.as_str()).or_default().entry(if g.role == ROLE_TEST2 { ROLE_TEST2 } else { ROLE_RESERVE }).or_default() += 1;
    }
    let groups_body = groups_body(&groups)?;
    let w_a8 = fin.weights.as_ref().ok();
    let result = json!({
        "rule": CARVE_RULE,
        "params": p,
        "units": "pool v2 lower-bound documents: floor(expected micro-documents / 10^6 x realization_lower) per domain, as the pool's capacity counts them (amendment 3 item 3.1)",
        "keyed_order": format!("HMAC-SHA-256(K_test2, \"{RANK_LABEL}\" + domain + \":\" + group_key), compared as 32-byte big-endian integers, ascending; Test-2 T is the shortest prefix holding the target and the group floor, the reserve continues in the same order (amendment 3 item 3.2)"),
        "mock_unit_demand_at_b_res": mock_unit_demand,
        "manifest_in_force": {"weights_ppm": wm, "reproduced": true, "stock_lower_documents": inp.recorded_lower,
            "binding_at_h": carved.iter().filter(|d| binds_at_h(d)).collect::<Vec<_>>(),
            "first_bind_lower_yield": sim_m.first_bind},
        "iterations": iterations,
        "surplus_bound": surplus,
        "w_a8": w_a8,
        "ar8": fin.ar8,
        "tv": tv,
        "set_i_absent": true,
        "carve_groups": {"sha256": sha256_hex(groups_body.as_bytes()), "rows": groups.len(), "by_domain_role": by_domain_role},
        "exclusion": {"carve_groups_sha256": sha256_hex(groups_body.as_bytes()), "rows": groups.len(),
            "weights_ppm": w_a8.map(|w| &w.weights_ppm)},
        "verdict": {"pass": pass, "reasons": reasons},
        "failure": failure,
        "release": {"rule": "unused reserve groups return to pool v2 only by a recorded amendment after the Test-2 mock is built, once R_0' 's effective cohort has closed, the replacement has lapsed or U17 has failed (§5.3 step 5; amendment 1 item 1.11); groups of the sealed Test-2 T membership and groups the mock used never return (amendment 3 item 3.5)",
            "released": 0},
    });
    Ok(Computed { result, groups, pass })
}

fn groups_body(groups: &[CarveGroup]) -> Result<String> {
    let mut s = String::new();
    for g in groups {
        s.push_str(&serde_json::to_string(g)?);
        s.push('\n');
    }
    Ok(s)
}

// ------------------------------------------------------------------ the freeze log

/// `FROZEN-A2.json`'s log with its hash chain verified (amendment 1 item 1.6).
pub struct FreezeLog {
    pub entries: Vec<Value>,
    /// The file's digest as read.
    pub sha256: String,
    /// The first entry that carries `prev_sha256`.
    pub chained_from: usize,
}

#[derive(Deserialize)]
struct LogSpans<'a> {
    #[serde(borrow)]
    log: Vec<&'a RawValue>,
}

impl FreezeLog {
    /// Parses a freeze record and verifies its chain on the file's own bytes: the file
    /// just before entry k was appended is its bytes up to the end of entry k − 1
    /// followed by the bytes after the last entry. From the first entry that carries
    /// `prev_sha256`, every entry must carry one and a `utc`, and each must match.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let spans: LogSpans = serde_json::from_slice(bytes).context("freeze log JSON")?;
        let base = bytes.as_ptr() as usize;
        let ends: Vec<usize> = spans.log.iter().map(|r| r.get().as_ptr() as usize - base + r.get().len()).collect();
        let entries: Vec<Value> = spans.log.iter().map(|r| serde_json::from_str(r.get())).collect::<Result<_, _>>()?;
        let last = *ends.last().context("refused: the freeze log is empty")?;
        let tail = &bytes[last..];
        let first = entries.iter().position(|e| !e["prev_sha256"].is_null()).context("refused: the freeze log is not hash-chained (amendment 1 item 1.6)")?;
        ensure!(first > 0, "refused: the freeze log's first entry cannot carry prev_sha256");
        for k in first..entries.len() {
            let e = &entries[k];
            let prev = e["prev_sha256"].as_str().filter(|s| is_digest(s))
                .with_context(|| format!("refused: freeze-log entry {k} has no prev_sha256 after the chain began (amendment 1 item 1.6)"))?;
            ensure!(e["utc"].is_string(), "refused: freeze-log entry {k} has no utc");
            let got = hex::encode(Sha256::new().chain_update(&bytes[..ends[k - 1]]).chain_update(tail).finalize());
            ensure!(got == prev, "refused: freeze-log entry {k} ({}) does not chain: its prev_sha256 is not the digest of the log before it", e["utc"]);
        }
        Ok(FreezeLog { entries, sha256: sha256_hex(bytes), chained_from: first })
    }

    pub fn read(repo: &Path, rel: &str) -> Result<Self> {
        check_reference(rel)?;
        let b = fs::read(resolve_reference(repo, rel)?).with_context(|| format!("read {rel}"))?;
        Self::parse(&b).with_context(|| format!("the freeze log {rel}"))
    }

    /// The first entry whose `field` is `sha`.
    pub fn with_digest(&self, field: &str, sha: &str) -> Option<&Value> {
        self.entries.iter().find(|e| e[field] == sha)
    }

    /// A6's entry and its K_test2 commitment (exactly one entry records one).
    pub fn k_test2(&self) -> Result<(&Value, &str)> {
        let hits: Vec<&Value> = self.entries.iter().filter(|e| e["commitments_sha256"]["K_test2"].is_string()).collect();
        ensure!(hits.len() == 1, "refused: the freeze log must hold exactly one A6 entry with commitments_sha256.K_test2; it holds {}", hits.len());
        let c = hits[0]["commitments_sha256"]["K_test2"].as_str().expect("checked");
        ensure!(is_digest(c), "refused: A6's K_test2 commitment is not a lowercase SHA-256");
        Ok((hits[0], c))
    }

    /// The recorded user decisions (amendment 3 item 3.3): per item, the last decision
    /// logged and its entry's utc. Each value must be `confirmed` or `overridden`.
    pub fn decisions(&self) -> Result<BTreeMap<String, (String, Value)>> {
        let mut out = BTreeMap::new();
        for e in &self.entries {
            let Some(m) = e.get(LOG_DECISIONS_FIELD) else { continue };
            let m = m.as_object().context("refused: user_decisions must be an object")?;
            for (k, v) in m {
                let d = v.as_str().filter(|d| matches!(*d, "confirmed" | "overridden"))
                    .with_context(|| format!("refused: user_decisions.{k} must be \"confirmed\" or \"overridden\""))?;
                out.insert(k.clone(), (d.to_string(), e["utc"].clone()));
            }
        }
        Ok(out)
    }
}

/// `carve-v3-log-check`: verifies the freeze log's chain and reports what the carve
/// reads from it (counts, utc stamps and decisions; no digest of a sealed file).
pub fn log_check(repo: &Path, rel: &str) -> Result<Value> {
    let log = FreezeLog::read(repo, rel)?;
    let a6 = log.k_test2().map(|(e, _)| e["utc"].clone()).map_err(|e| format!("{e:#}"));
    let decisions: BTreeMap<String, Value> =
        log.decisions()?.into_iter().map(|(k, (d, utc))| (k, json!({"decision": d, "entry_utc": utc}))).collect();
    Ok(json!({"entries": log.entries.len(), "chained_from": log.chained_from, "chain": "verified", "sha256": log.sha256,
        "a6_entry_utc": a6.as_ref().ok(), "a6_error": a6.as_ref().err(), "user_decisions": decisions,
        "carve_draft_entries": log.entries.iter().filter(|e| e[LOG_DRAFT_FIELD].is_string()).count()}))
}

// ------------------------------------------------------------------ loading

/// Where the allow-list the pool was built from comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    /// The pool v2 directory (repository-relative).
    pub pool_dir: String,
    /// `tracked` (the crate's allow-list) or a repository-relative path.
    pub allowlist_v2: String,
    /// The K_test2 key file (repository-relative), owner-only.
    pub key_file: String,
    /// A6's SHA-256 commitment of K_test2, as the freeze log records it.
    pub key_commitment: String,
    /// The protocol's split set's `SHA256SUMS` (`data/splits-v4`, amendment 1 item
    /// 1.16), which must list the same digests for the consumed Test and reservation
    /// files the allow-list reads (it reads `splits-v5`). Required (amendment 3 item 3.4).
    pub protocol_sums: FileRef,
}

fn allowlist_bytes(repo: &Path, src: &str) -> Result<Vec<u8>> {
    if src == "tracked" {
        return Ok(stock::TRACKED_STOCK_ALLOWLIST.as_bytes().to_vec());
    }
    check_reference(src)?;
    fs::read(resolve_reference(repo, src)?).with_context(|| format!("read {src}"))
}

/// Reads the key file: owner-only, exactly 32 bytes, matching its commitment.
pub fn read_key(repo: &Path, rel: &str, commitment: &str) -> Result<[u8; KEY_BYTES]> {
    ensure!(is_digest(commitment), "the key commitment must be a lowercase SHA-256");
    let path = resolve_reference(repo, rel)?;
    let meta = fs::metadata(&path)?;
    ensure!(meta.permissions().mode() & 0o077 == 0, "refused: the key file must be owner-only (mode 600)");
    let dir = path.parent().context("key directory")?;
    ensure!(fs::metadata(dir)?.permissions().mode() & 0o077 == 0, "refused: the key directory must be owner-only (mode 700)");
    let b = fs::read(&path)?;
    ensure!(b.len() == KEY_BYTES, "refused: the key file must hold exactly {KEY_BYTES} bytes");
    ensure!(sha256_hex(&b) == commitment, "refused: the key does not match its A6 commitment");
    Ok(<[u8; KEY_BYTES]>::try_from(b.as_slice()).expect("length checked"))
}

struct PoolFiles {
    manifest: Value,
    manifest_sha: String,
    capacity: Value,
}

/// A reservation-stage pool v2 directory: before the carve (`carved` false) or built
/// under allow-list v3 (`carved` true).
fn pool_files(repo: &Path, dir: &str, carved: bool) -> Result<PoolFiles> {
    let mpath = format!("{dir}/manifest.json");
    let mb = fs::read(resolve(repo, &mpath)?).with_context(|| format!("read {mpath}"))?;
    let manifest: Value = serde_json::from_slice(&mb).context("pool manifest JSON")?;
    ensure!(manifest["schema"] == pool_v2::POOL_V2_SCHEMA && manifest["stage"] == "reservation", "refused: {mpath} is not a reservation-stage pool v2 manifest");
    if carved {
        ensure!(manifest.get("carve_exclusion").is_some(), "refused: {mpath} excludes no carve");
    } else {
        ensure!(manifest.get("carve_exclusion").is_none(), "refused: {mpath} already excludes a carve");
    }
    let cpath = format!("{dir}/capacity.json");
    let cap_sha = manifest["capacity"]["sha256"].as_str().context("capacity digest")?;
    let capacity: Value = serde_json::from_slice(&read_checked(&resolve(repo, &cpath)?, &cpath, cap_sha)?).context("capacity JSON")?;
    ensure!(capacity["schema"] == pool_v2::CAPACITY_SCHEMA, "capacity schema");
    Ok(PoolFiles { manifest, manifest_sha: sha256_hex(&mb), capacity })
}

/// The sorted, deduplicated keys' count and digest (each key followed by a newline).
pub fn sorted_digest(keys: &mut Vec<String>) -> (u64, String) {
    keys.sort_unstable();
    keys.dedup();
    let mut h = Sha256::new();
    for k in keys.iter() {
        h.update(k.as_bytes());
        h.update(b"\n");
    }
    (keys.len() as u64, hex::encode(h.finalize()))
}

#[derive(Deserialize)]
struct PoolGroupRow {
    schema: String,
    domain: String,
    group_key: String,
    units: u64,
    expected_udocs: u64,
}

#[derive(Deserialize)]
struct ExclusionRow {
    schema: String,
    group_key: String,
}

#[derive(Deserialize)]
struct TestRow {
    schema: String,
    split: String,
    domain: String,
    group_key: String,
    order_in_split_domain: u64,
}

/// Per-domain capacity, plan, horizon and weights from a pool's records, with the
/// domains' stock (`domains`, from its groups) completed and checked.
fn capacity_inputs(pf: &PoolFiles, mut domains: BTreeMap<String, DomainStock>, consumed_test: BTreeSet<String>) -> Result<Inputs> {
    let (m, cap) = (&pf.manifest, &pf.capacity);
    let by = cap["stock"]["by_domain"].as_object().context("capacity by_domain")?;
    let mut recorded_lower = BTreeMap::new();
    for (d, e) in by {
        let s = domains.entry(d.clone()).or_default();
        s.test_mass = e["test_mass"].as_u64().context("test_mass")?;
        s.realization = e["realization"].as_f64().context("realization")?;
        s.realization_lower = e["realization_lower"].as_f64().context("realization_lower")?;
        recorded_lower.insert(d.clone(), e["capacity_lower_documents"].as_u64().context("capacity_lower_documents")?);
    }
    ensure!(domains.len() == by.len(), "refused: pool groups name a domain the capacity record lacks");
    let plan: Params = serde_json::from_value(cap["plan"].clone()).context("capacity plan")?;
    let manifest_weights: BTreeMap<String, u64> = serde_json::from_value(m["domain_weights"]["weights_ppm"].clone())?;
    let recorded: BTreeMap<String, u64> = serde_json::from_value(cap["reweighting"]["weights_ppm"].clone())?;
    ensure!(manifest_weights == recorded, "refused: the manifest and capacity weights differ");
    Ok(Inputs {
        domains,
        plan,
        cohorts_per_day: u32::try_from(cap["cohorts_per_day"].as_u64().context("cohorts_per_day")?)?,
        horizon: u32::try_from(m["domain_weights"]["horizon_cohorts"].as_u64().context("horizon")?)?,
        manifest_weights,
        recorded_lower,
        consumed_test,
    })
}

/// Loads the carve's inputs from a pool v2 directory and the allow-list it was built
/// from. Ranks are computed only with a key.
pub fn load(repo: &Path, src: &Sources, key: Option<&[u8; KEY_BYTES]>, carved: &BTreeSet<String>) -> Result<(Inputs, Value)> {
    check_rel(&format!("{}/manifest.json", src.pool_dir))?;
    let pf = pool_files(repo, &src.pool_dir, false)?;
    let m = &pf.manifest;
    let abytes = allowlist_bytes(repo, &src.allowlist_v2)?;
    let a_sha = sha256_hex(&abytes);
    ensure!(m["allowlist"]["sha256"] == a_sha.as_str(), "refused: the allow-list is not the one pool v2 was built from");
    let a = StockAllowlist::parse(&abytes)?;
    ensure!(a.schema == stock::STOCK_ALLOWLIST_SCHEMA, "refused: the carve starts from a v2 allow-list");

    // Set (i): the consumed Test's groups, and those its acquisition never reached.
    let summary = &a.test_rules.acquisition_summary;
    let sv: Value = serde_json::from_slice(&read_checked(&resolve_reference(repo, &summary.path)?, &summary.path, &summary.sha256)?)?;
    a.assignment_prose_guard.check(&sv).context("the acquisition summary")?;
    let processed: BTreeMap<String, u64> = sv["per_domain"].as_object().context("summary per_domain")?.iter()
        .map(|(d, x)| (d.clone(), x["groups_processed"].as_u64().unwrap_or(0))).collect();
    let mut test_all = Vec::new();
    let mut test_unreached = Vec::new();
    let mut test_files = Vec::new();
    for f in &a.test_rules.groups {
        let n = for_each_line(&resolve_reference(repo, &f.path)?, &f.path, &f.sha256, |_, l| {
            let v: Value = serde_json::from_str(l)?;
            a.assignment_prose_guard.check(&v)?;
            let r: TestRow = serde_json::from_value(v)?;
            ensure!(r.schema == stock::GROUP_SCHEMA && r.split == "test", "not a Test group record");
            if r.order_in_split_domain >= processed.get(&r.domain).copied().unwrap_or(0) {
                test_unreached.push(r.group_key.clone());
            }
            test_all.push(r.group_key);
            Ok(())
        })?;
        test_files.push(json!({"path": f.path, "sha256": f.sha256, "rows": n}));
    }
    let consumed_test: BTreeSet<String> = test_all.iter().cloned().collect();
    let (n_all, d_all) = sorted_digest(&mut test_all);
    let (n_un, d_un) = sorted_digest(&mut test_unreached);

    // The protocol's split set must hold the same files (amendment 1 item 1.16).
    let s = &src.protocol_sums;
    let body = read_checked(&resolve_reference(repo, &s.path)?, &s.path, &s.sha256)?;
    let mut sums: BTreeMap<String, String> = BTreeMap::new();
    for line in std::str::from_utf8(&body)?.lines().filter(|l| !l.is_empty()) {
        let (sha, rel) = line.split_once("  ").context("SHA256SUMS line")?;
        sums.insert(rel.strip_prefix("./").unwrap_or(rel).to_string(), sha.to_string());
    }
    let root = a.split_sums.path.strip_suffix("/SHA256SUMS").context("split_sums path")?;
    let mut matched = Vec::new();
    let files = a.test_rules.groups.iter().map(|f| (&f.path, &f.sha256)).chain(a.group_reservations.iter().map(|g| (&g.path, &g.sha256)));
    for (p, sha) in files {
        let rel = p.strip_prefix(root).and_then(|r| r.strip_prefix('/')).with_context(|| format!("{p} is not under the allow-list's split set"))?;
        let want = sums.get(rel).with_context(|| format!("refused: {} does not list {rel}", s.path))?;
        ensure!(want == sha, "refused: {} records another digest for {rel}", s.path);
        matched.push(rel.to_string());
    }
    let sums_check = json!({"path": s.path, "sha256": s.sha256, "matched": matched});

    // Stock: every kept group (set (ii) after the gate), and the gate's exclusions.
    let mut set_ii = Vec::new();
    let mut domains: BTreeMap<String, DomainStock> = BTreeMap::new();
    let gpath = format!("{}/groups.jsonl", src.pool_dir);
    let gsha = m["groups"]["sha256"].as_str().context("groups digest")?;
    let rows = for_each_line(&resolve(repo, &gpath)?, &gpath, gsha, |_, l| {
        let r: PoolGroupRow = serde_json::from_str(l)?;
        ensure!(r.schema == pool_v2::GROUP_ROW_SCHEMA, "pool group schema {}", r.schema);
        let e = domains.entry(r.domain.clone()).or_default();
        e.udocs += r.expected_udocs;
        if let (Some(k), true) = (key, carved.contains(&r.domain)) {
            e.candidates.push(Candidate { rank: rank(k, &r.domain, &r.group_key), group_key: r.group_key.clone(), units: r.units, udocs: r.expected_udocs });
        }
        set_ii.push(r.group_key);
        Ok(())
    })?;
    ensure!(m["groups"]["rows"] == rows, "refused: {gpath} has {rows} rows, the manifest records {}", m["groups"]["rows"]);
    let kept = rows;
    let epath = format!("{}/exclusions.jsonl", src.pool_dir);
    let esha = m["exclusions"]["sha256"].as_str().context("exclusions digest")?;
    let erows = for_each_line(&resolve(repo, &epath)?, &epath, esha, |_, l| {
        let r: ExclusionRow = serde_json::from_str(l)?;
        ensure!(r.schema == pool_v2::EXCLUSION_SCHEMA, "exclusion schema {}", r.schema);
        set_ii.push(r.group_key);
        Ok(())
    })?;
    ensure!(m["exclusions"]["rows"] == erows, "refused: {epath} has {erows} rows, the manifest records {}", m["exclusions"]["rows"]);
    let (n_ii, d_ii) = sorted_digest(&mut set_ii);
    ensure!(n_ii == kept + erows, "refused: pool v2 names a group twice");
    for s in domains.values_mut() {
        s.candidates.sort_unstable_by_key(|c| c.rank);
        ensure!(s.candidates.windows(2).all(|w| w[0].rank != w[1].rank), "refused: two groups share a keyed rank");
    }

    let inp = capacity_inputs(&pf, domains, consumed_test)?;
    let record = json!({
        "sources": src,
        "pool": {"dir": src.pool_dir, "manifest_sha256": pf.manifest_sha, "pool_id": m["pool_id"],
            "groups": {"sha256": gsha, "rows": kept}, "exclusions": {"sha256": esha, "rows": erows},
            "capacity_sha256": m["capacity"]["sha256"]},
        "allowlist_v2": {"id": a.allowlist_id, "sha256": a_sha},
        "set_i": {"definition": "groups assigned to the consumed Test's split that its acquisition never reached (order_in_split_domain >= groups_processed)",
            "groups": n_un, "group_ids_sha256": d_un,
            "consumed_test_assignment": {"groups": n_all, "group_ids_sha256": d_all, "files": test_files,
                "acquisition_summary": {"path": summary.path, "sha256": summary.sha256}},
            "check": "the carve and the reserve hold no group of the consumed Test's whole assignment, a superset of set (i); the reservation gate already refuses a stock sharing a group with these files"},
        "set_ii": {"definition": "the Test-reservation remainder pool v2 is built from: its reserved-unassigned groups (amendment 1 item 1.16) and the Wikipedia index remainder (amendment 3 item 3.4), before the reservation gate",
            "groups": n_ii, "group_ids_sha256": d_ii, "kept_by_the_gate": kept, "excluded_by_the_gate": erows,
            "reservations": m["inputs"],
            "index_remainder": a.index_reservations.iter().map(|i| json!({"domain": i.domain, "path": i.path, "sha256": i.sha256, "remainder_units": i.remainder_units})).collect::<Vec<_>>(),
            "carve_frame": "the groups the gate kept"},
        "protocol_split_sums": sums_check,
    });
    Ok((inp, record))
}

// ------------------------------------------------------------------ files

pub fn utc_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    utc_from_unix(secs)
}

/// RFC 3339 UTC from Unix seconds (civil-from-days).
pub fn utc_from_unix(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!("{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn write_private(path: &Path, body: &[u8]) -> Result<String> {
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
        .with_context(|| format!("create {} (existing records are never replaced)", path.display()))?;
    f.write_all(body)?;
    f.sync_all()?;
    Ok(sha256_hex(body))
}

/// A new output path, validated before anything is written: its parent exists, it is
/// under `data/`, its name passes the name refusals, and nothing is there yet.
struct OutPath {
    path: PathBuf,
    rel: String,
}

fn out_path(repo: &Path, out: &Path) -> Result<OutPath> {
    let name = out.file_name().and_then(|n| n.to_str()).with_context(|| format!("output name of {}", out.display()))?;
    let parent = out.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let parent = parent.canonicalize().with_context(|| format!("the directory of {} must exist", out.display()))?;
    let path = parent.join(name);
    let rel = rel_under(&data_dir(repo)?, &path).context("refused: carve records are written only under data/")?;
    check_rel(&rel)?;
    ensure!(fs::symlink_metadata(&path).is_err(), "refused: {rel} exists (records are never replaced)");
    Ok(OutPath { path, rel })
}

/// Creates a validated output path as a new owner-only directory.
fn create_private_dir(o: &OutPath) -> Result<()> {
    fs::create_dir(&o.path).with_context(|| format!("create {} (existing directories are never replaced)", o.rel))?;
    fs::set_permissions(&o.path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Distinct, validated output paths.
fn out_paths<const N: usize>(repo: &Path, outs: [&Path; N]) -> Result<[OutPath; N]> {
    let v: Vec<OutPath> = outs.iter().map(|o| out_path(repo, o)).collect::<Result<_>>()?;
    let distinct: BTreeSet<&PathBuf> = v.iter().map(|o| &o.path).collect();
    ensure!(distinct.len() == N, "refused: the output paths must differ");
    v.try_into().map_err(|_| anyhow::anyhow!("output paths"))
}

/// Computes the carve from a pool v2 directory with the K_test2 key, whose commitment
/// must be the one A6 logged.
pub fn compute_from(repo: &Path, src: &Sources, p: &CarveParams, log: &FreezeLog) -> Result<Computed> {
    let (a6, committed) = log.k_test2()?;
    ensure!(src.key_commitment == committed, "refused: the K_test2 commitment given is not the one A6 logged in the freeze log");
    let key = read_key(repo, &src.key_file, &src.key_commitment)?;
    let carved: BTreeSet<String> = p.targets.keys().cloned().collect();
    let (inp, inputs) = load(repo, src, Some(&key), &carved)?;
    let mut c = compute(&inp, p)?;
    c.result["inputs"] = inputs;
    c.result["key"] = json!({"name": "K_test2", "commitment_sha256": src.key_commitment, "bytes": KEY_BYTES,
        "a6_log_entry": {"utc": a6["utc"], "prev_sha256": a6["prev_sha256"]},
        "use": "keyed order only; the key and the ranks are never written"});
    Ok(c)
}

/// `carve-v3`: computes the draft and writes it, with its groups file, to a new
/// owner-only directory. Nothing is applied. Returns the draft and its digest.
pub fn draft(repo: &Path, src: &Sources, p: &CarveParams, freeze_log: &str, out: &Path) -> Result<(Value, String)> {
    let o = out_path(repo, out)?;
    let log = FreezeLog::read(repo, freeze_log)?;
    let c = compute_from(repo, src, p, &log)?;
    let body = groups_body(&c.groups)?;
    create_private_dir(&o)?;
    let gsha = write_private(&o.path.join(GROUPS_FILE), body.as_bytes())?;
    let record = json!({
        "schema": RECORD_SCHEMA,
        "rule": CARVE_RULE,
        "status": "computed_draft",
        "computed_utc": utc_now(),
        "note": "a hashed draft (amendment 1 item 1.5): nothing is applied to pool v2 until the user decisions are logged; log this file's digest as carve_draft_sha256 (S0-5)",
        "freeze_log_at_draft": {"path": freeze_log, "sha256": log.sha256, "entries": log.entries.len()},
        "files": {"carve_groups": {"path": format!("{}/{GROUPS_FILE}", o.rel), "sha256": gsha, "rows": c.groups.len()}},
        "result": c.result,
    });
    let bytes = serde_json::to_vec_pretty(&record)?;
    let sha = write_private(&o.path.join(DRAFT_FILE), &bytes)?;
    Ok((record, sha))
}

fn read_record(repo: &Path, rel: &str, sha: &str) -> Result<Value> {
    check_reference(rel)?;
    let b = read_checked(&resolve_reference(repo, rel)?, rel, sha)?;
    let v: Value = serde_json::from_slice(&b).context("carve record JSON")?;
    ensure!(v["schema"] == RECORD_SCHEMA && v["rule"] == CARVE_RULE, "refused: {rel} is not a carve record");
    Ok(v)
}

/// Rebuild parameters of an existing pool, from its own capacity record.
pub fn build_params_of(capacity: &Value, pool_id: &str) -> Result<BuildParams> {
    let plan: Params = serde_json::from_value(capacity["plan"].clone())?;
    let report_horizons = capacity["horizons"].as_array().context("horizons")?.iter()
        .map(|h| h["horizon_cohorts"].as_u64().and_then(|x| u32::try_from(x).ok()).context("horizon")).collect::<Result<_>>()?;
    let engine = capacity["scenarios"].as_array().context("scenarios")?.iter().find(|s| s["name"] == "engine_v1_rule_engine_mix").context("engine scenario")?;
    let engine_mix: [u32; 3] = serde_json::from_value(engine["outcome"]["params"]["mix"].clone())?;
    let canary_report = match capacity["canary"].as_object() {
        None => None,
        Some(c) => {
            let rows = c["scenarios"].as_array().context("canary scenarios")?;
            let mut every: Vec<u32> = Vec::new();
            let mut flagged = 1u32;
            for r in rows {
                let e = u32::try_from(r["candidate_every_cohorts"].as_u64().context("canary interval")?)?;
                if !every.contains(&e) {
                    every.push(e);
                }
                flagged = flagged.max(u32::try_from(r["audits_per_candidate"].as_u64().context("canary audits")?)?);
            }
            Some(CanaryReport { docs: u32::try_from(c["documents_per_audit"].as_u64().context("canary documents")?)?, flagged_audits: flagged, every })
        }
    };
    Ok(BuildParams {
        pool_id: pool_id.to_string(),
        horizon: u32::try_from(capacity["reweighting"]["horizon_cohorts"].as_u64().context("horizon")?)?,
        report_horizons,
        plan,
        engine_mix,
        roster: u32::try_from(capacity["roster"].as_u64().context("roster")?)?,
        cohorts_per_day: u32::try_from(capacity["cohorts_per_day"].as_u64().context("cohorts per day")?)?,
        canary_report,
    })
}

fn v3_id(id: &str) -> String {
    match id.strip_suffix("v2") {
        Some(stem) => format!("{stem}v3"),
        None => format!("{id}-v3"),
    }
}

/// Paths and digests of an application.
pub struct ApplyArgs<'a> {
    pub draft: &'a str,
    pub draft_sha256: &'a str,
    pub freeze_log: &'a str,
    pub allowlist_out: &'a Path,
    pub pool_out: &'a Path,
    pub pool_id: Option<&'a str>,
    pub record_out: &'a Path,
}

/// `carve-v3-apply` (F0b): with the freeze log's chain intact, the draft's digest
/// logged and the user decisions A8's write needs, recomputes the draft, rebuilds pool
/// v2 under allow-list v3 (the build refuses unless its weights are W_A8), and writes
/// allow-list v3 and the applied record. Every output path is checked first.
pub fn apply(repo: &Path, args: &ApplyArgs, p: &CarveParams) -> Result<Value> {
    let d = read_record(repo, args.draft, args.draft_sha256)?;
    ensure!(d["status"] == "computed_draft", "refused: {} is not a draft", args.draft);
    ensure!(d["result"]["verdict"]["pass"] == true, "refused: the draft's verdict is not a pass; nothing is applied (amendment 1 item 1.4)");
    let log = FreezeLog::read(repo, args.freeze_log)?;
    let logged = log.with_digest(LOG_DRAFT_FIELD, args.draft_sha256)
        .context("refused: no freeze-log entry records the draft's digest as carve_draft_sha256 (S0-5; amendment 3 item 3.3)")?;
    let decisions = log.decisions()?;
    for u in A8_DECISIONS {
        ensure!(decisions.contains_key(u), "refused: the freeze log records no user decision on {u}; A8's write needs U1, U2, U16 and U17 (amendment 1 item 1.5; amendment 3 item 3.3)");
    }
    ensure!(decisions["U1"].0 == "confirmed", "refused: U1 is recorded as overridden; the carve is not applied");
    let [al, po, ro] = out_paths(repo, [args.allowlist_out, args.pool_out, args.record_out])?;
    let src: Sources = serde_json::from_value(d["result"]["inputs"]["sources"].clone()).context("draft sources")?;
    let c = compute_from(repo, &src, p, &log)?;
    ensure!(c.pass, "refused: the recomputed carve does not pass");
    ensure!(c.result == d["result"], "refused: the recomputed carve differs from the draft");
    let gpath = d["files"]["carve_groups"]["path"].as_str().context("draft groups path")?;
    let gsha = d["files"]["carve_groups"]["sha256"].as_str().context("draft groups digest")?;
    let body = groups_body(&c.groups)?;
    ensure!(sha256_hex(body.as_bytes()) == gsha, "refused: the recomputed groups differ from the draft's");
    read_checked(&resolve(repo, gpath)?, gpath, gsha)?;
    let w_a8: BTreeMap<String, u64> = serde_json::from_value(c.result["w_a8"]["weights_ppm"].clone())?;

    // Allow-list v3.
    let v2_bytes = allowlist_bytes(repo, &src.allowlist_v2)?;
    let mut a = StockAllowlist::parse(&v2_bytes)?;
    a.schema = stock::STOCK_ALLOWLIST_SCHEMA_V3.into();
    a.allowlist_id = v3_id(&a.allowlist_id);
    a.carve_exclusion = Some(CarveExclusion {
        rule: CARVE_RULE.into(),
        groups: FileRef { path: gpath.into(), sha256: gsha.into() },
        rows: c.groups.len() as u64,
        record: FileRef { path: args.draft.into(), sha256: args.draft_sha256.into() },
        weights_ppm: w_a8.clone(),
        base_allowlist_sha256: sha256_hex(&v2_bytes),
    });
    let v3 = serde_json::to_vec_pretty(&a)?;
    StockAllowlist::parse(&v3)?;

    // Rebuild pool v2 under it.
    let pf = pool_files(repo, &src.pool_dir, false)?;
    let pool_id = args.pool_id.map(str::to_string).unwrap_or_else(|| pf.manifest["pool_id"].as_str().unwrap_or_default().to_string());
    let bp = build_params_of(&pf.capacity, &pool_id)?;
    let m = pool_v2::build(repo, &v3, &bp, &po.path)?;
    ensure!(m["domain_weights"]["weights_ppm"] == json!(w_a8), "the rebuilt pool's weights are not W_A8");
    let al_sha = write_private(&al.path, &v3)?;
    write_private(&po.path.join("allowlist-v3.json"), &v3)?;
    let mbytes = fs::read(po.path.join("manifest.json"))?;

    let decided: BTreeMap<&str, Value> = A8_DECISIONS.iter().map(|u| (*u, json!({"decision": decisions[*u].0, "entry_utc": decisions[*u].1}))).collect();
    let mut rec = d.clone();
    rec["status"] = json!("applied");
    rec["note"] = json!("F0b: allow-list v3 is pool v2's exclusion; pool v2 is rebuilt at W_A8");
    rec["application"] = json!({
        "applied_utc": utc_now(),
        "draft": {"path": args.draft, "sha256": args.draft_sha256, "logged_in_entry_utc": logged["utc"]},
        "user_decisions": decided,
        "freeze_log": {"path": args.freeze_log, "sha256_at_apply": log.sha256, "entries": log.entries.len(), "chain": "verified"},
        "allowlist_v3": {"path": al.rel, "id": a.allowlist_id, "sha256": al_sha},
        "pool": {"dir": po.rel, "pool_id": pool_id, "manifest_sha256": sha256_hex(&mbytes), "groups": m["groups"],
            "domain_weights_ppm": m["domain_weights"]["weights_ppm"], "weights_equal_w_a8": true},
        "next": "append this record's digest to FROZEN-A2.json's log (F0b) before any pool-v2 acquisition or upload",
    });
    let rb = serde_json::to_vec_pretty(&rec)?;
    let sha = write_private(&ro.path, &rb)?;
    rec["written"] = json!({"path": ro.rel, "sha256": sha});
    Ok(rec)
}

// ------------------------------------------------------------------ pool build hook

/// The carve groups a v3 pool build removes from its stock.
pub struct Carved {
    pub groups: BTreeMap<u32, String>,
    pub summary: Value,
}

/// Reads allow-list v3's carve exclusion and maps it onto the stock. Refuses a carve
/// group outside the stock, in another domain, excluded by the gate (with its own
/// message when the active reference's lineage hits it), or named twice, and a record
/// whose exclusion section differs from the allow-list's.
pub fn exclusion(repo: &Path, a: &StockAllowlist, groups: &[StockGroup], g: &Gate) -> Result<Option<Carved>> {
    let Some(c) = &a.carve_exclusion else { return Ok(None) };
    let rec: Value = serde_json::from_slice(&read_checked(&resolve_reference(repo, &c.record.path)?, &c.record.path, &c.record.sha256)?)?;
    let x = &rec["result"]["exclusion"];
    ensure!(x["carve_groups_sha256"] == c.groups.sha256.as_str() && x["rows"] == c.rows && x["weights_ppm"] == json!(c.weights_ppm),
        "refused: the carve record's exclusion is not the allow-list's");
    let index: HashMap<&str, u32> = groups.iter().enumerate().map(|(i, s)| (s.group_key.as_str(), i as u32)).collect();
    let active = format!("{}:", stock::ACTIVE_LINEAGE_ROLE.0);
    let mut out: BTreeMap<u32, String> = BTreeMap::new();
    let mut by: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let n = for_each_line(&resolve(repo, &c.groups.path)?, &c.groups.path, &c.groups.sha256, |_, l| {
        let r: CarveGroup = serde_json::from_str(l)?;
        ensure!(r.schema == CARVE_GROUP_SCHEMA && r.rule == CARVE_RULE, "carve group schema");
        ensure!(r.role == ROLE_TEST2 || r.role == ROLE_RESERVE, "carve group role {}", r.role);
        let i = *index.get(r.group_key.as_str()).context("refused: the carve names a group outside the stock")?;
        ensure!(groups[i as usize].domain == r.domain, "refused: a carve group's domain differs from the stock's");
        if let Some(reasons) = g.excluded.get(&i) {
            if reasons.iter().any(|x| x.starts_with(&active)) {
                bail!("refused: the active reference's training lineage hits a carve group (a {} {} group): Train-2 lineage touches Test-2 T or the mock reserve; nothing was written, and the carve's integrity needs an amendment", r.domain, r.role);
            }
            bail!("refused: the gate excludes a carve group; the carve was drawn from another stock");
        }
        ensure!(out.insert(i, r.role.clone()).is_none(), "refused: the carve names a group twice");
        *by.entry(r.domain).or_default().entry(r.role).or_default() += 1;
        Ok(())
    })?;
    ensure!(n == c.rows, "refused: the carve groups file has {n} rows, the allow-list records {}", c.rows);
    let summary = json!({"rule": c.rule, "groups": {"path": c.groups.path, "sha256": c.groups.sha256, "rows": n},
        "record": {"path": c.record.path, "sha256": c.record.sha256}, "by_domain_role": by,
        "weights": "the pool's weights must equal the carve record's; carve groups get no group line and no receipt here"});
    Ok(Some(Carved { groups: out, summary }))
}

// ------------------------------------------------------------------ release

#[derive(Deserialize)]
struct KeyRow {
    group_key: String,
}

/// Paths and digests of a release.
pub struct ReleaseArgs<'a> {
    /// The applied carve record (`CARVE-A2.json`).
    pub record: &'a str,
    pub record_sha256: &'a str,
    /// The allow-list v3 in force, and the pool directory built from it.
    pub allowlist: &'a FileRef,
    pub pool_dir: &'a str,
    /// The sealed Test-2 T membership (rows with a `group_key`), logged by the seal.
    pub test2_t: &'a FileRef,
    /// The carve groups the Test-2 mock used (rows with a `group_key`), logged with the
    /// mock's sealed journal.
    pub mock_used: &'a FileRef,
    pub freeze_log: &'a str,
    pub out: &'a Path,
    pub pool_out: &'a Path,
    pub pool_id: Option<&'a str>,
}

/// Group keys from a JSONL file, each a carve group, none twice.
fn carve_keys(repo: &Path, f: &FileRef, carve: &HashMap<&str, &CarveGroup>, what: &str) -> Result<BTreeSet<String>> {
    let mut keys = BTreeSet::new();
    for_each_line(&resolve_reference(repo, &f.path)?, &f.path, &f.sha256, |_, l| {
        let r: KeyRow = serde_json::from_str(l)?;
        ensure!(carve.contains_key(r.group_key.as_str()), "refused: {what} names a group outside the carve");
        ensure!(keys.insert(r.group_key), "refused: {what} names a group twice");
        Ok(())
    })?;
    Ok(keys)
}

/// The stock in force: a pool built under allow-list v3 (with any active reference),
/// its allow-list, and its stock after every current exclusion, reproduced.
struct Current {
    inp: Inputs,
    allow: StockAllowlist,
    manifest: Value,
    manifest_sha: String,
    capacity: Value,
}

fn load_current(repo: &Path, pool_dir: &str, allowlist: &FileRef, horizon: u32) -> Result<Current> {
    check_rel(&format!("{pool_dir}/manifest.json"))?;
    let pf = pool_files(repo, pool_dir, true)?;
    let ab = read_checked(&resolve_reference(repo, &allowlist.path)?, &allowlist.path, &allowlist.sha256)?;
    ensure!(pf.manifest["allowlist"]["sha256"] == allowlist.sha256.as_str(), "refused: {pool_dir} was not built from {}", allowlist.path);
    let allow = StockAllowlist::parse(&ab)?;
    ensure!(allow.schema == stock::STOCK_ALLOWLIST_SCHEMA_V3, "refused: the allow-list in force is not allow-list v3");
    let mut domains: BTreeMap<String, DomainStock> = BTreeMap::new();
    let gpath = format!("{pool_dir}/groups.jsonl");
    let gsha = pf.manifest["groups"]["sha256"].as_str().context("groups digest")?;
    let rows = for_each_line(&resolve(repo, &gpath)?, &gpath, gsha, |_, l| {
        let r: PoolGroupRow = serde_json::from_str(l)?;
        ensure!(r.schema == pool_v2::GROUP_ROW_SCHEMA, "pool group schema {}", r.schema);
        domains.entry(r.domain).or_default().udocs += r.expected_udocs;
        Ok(())
    })?;
    ensure!(pf.manifest["groups"]["rows"] == rows, "refused: {gpath} has {rows} rows, the manifest records {}", pf.manifest["groups"]["rows"]);
    let inp = capacity_inputs(&pf, domains, BTreeSet::new())?;
    ensure!(inp.horizon == horizon, "refused: the pool's horizon is {} cohorts, the protocol's H is {horizon}", inp.horizon);
    reproduce(&inp, horizon)?;
    let ce = allow.carve_exclusion.as_ref().context("allow-list v3 needs its carve exclusion")?;
    ensure!(ce.weights_ppm == inp.manifest_weights, "refused: the pool's weights are not its allow-list's carve weights");
    Ok(Current { inp, allow, manifest: pf.manifest, manifest_sha: pf.manifest_sha, capacity: pf.capacity })
}

/// `carve-v3-release`: after the Test-2 mock is built, returns the reserve groups that
/// neither the sealed Test-2 T membership holds nor the mock used to pool v2, by a
/// logged amendment naming the applied record and one of item 1.11's conditions. The
/// membership and the mock's used groups are bound by digest to freeze-log entries.
/// The re-waterfill runs on the stock in force (after every current exclusion, the
/// active reference's lineage included) plus the released groups; the successor
/// allow-list v3 keeps the allow-list in force, active reference and all, with the
/// successor carve exclusion, and pool v2 is rebuilt under it (the build refuses unless
/// its weights are the release record's). TV from W_A8 is reported; the mock's evidence
/// stays at W_A8.
pub fn release(repo: &Path, args: &ReleaseArgs, p: &CarveParams) -> Result<Value> {
    let r = read_record(repo, args.record, args.record_sha256)?;
    ensure!(r["status"] == "applied", "refused: only an applied carve releases reserve groups");
    ensure!(r["result"]["params"] == json!(p), "refused: the carve record was computed with other parameters");
    let log = FreezeLog::read(repo, args.freeze_log)?;
    let amend = log.entries.iter()
        .find(|e| e["releases_carve"] == args.record_sha256 && e["release_condition"].as_str().is_some_and(|c| RELEASE_CONDITIONS.contains(&c)))
        .context("refused: no freeze-log amendment releases this carve under one of item 1.11's conditions")?;
    let sealed_entry = log.with_digest(LOG_TEST2_T_FIELD, &args.test2_t.sha256)
        .context("refused: no freeze-log entry records this Test-2 T membership as test2_t_groups_sha256 (the seal's entry; amendment 3 item 3.5)")?;
    let mock_entry = log.with_digest(LOG_MOCK_USED_FIELD, &args.mock_used.sha256)
        .context("refused: no freeze-log entry records these mock groups as test2_mock_used_groups_sha256 (the Test-2 mock's sealed journal entry; amendment 3 item 3.5)")?;
    let gpath = r["files"]["carve_groups"]["path"].as_str().context("groups path")?;
    let gsha = r["result"]["exclusion"]["carve_groups_sha256"].as_str().context("groups digest")?;
    let mut carve: Vec<CarveGroup> = Vec::new();
    for_each_line(&resolve(repo, gpath)?, gpath, gsha, |_, l| {
        carve.push(serde_json::from_str(l)?);
        Ok(())
    })?;
    let by_key: HashMap<&str, &CarveGroup> = carve.iter().map(|g| (g.group_key.as_str(), g)).collect();
    let sealed = carve_keys(repo, args.test2_t, &by_key, "the sealed Test-2 T membership")?;
    let used = carve_keys(repo, args.mock_used, &by_key, "the Test-2 mock record")?;
    ensure!(!used.is_empty(), "refused: the Test-2 mock record is empty; release waits until the mock is built");
    ensure!(carve.iter().all(|g| g.role != ROLE_TEST2 || sealed.contains(&g.group_key)),
        "refused: the sealed Test-2 T membership lacks a carved Test-2 T group; Test-2 T groups never return");
    let mut promoted: BTreeMap<&str, u64> = BTreeMap::new();
    for k in &sealed {
        let g = by_key[k.as_str()];
        if g.role == ROLE_RESERVE {
            ensure!(g.domain == p.stage3_raised_domain, "refused: the sealed Test-2 T membership moves a {} reserve group into Test-2 T; only {}'s reserve is raised at Stage 3 (§12.5)", g.domain, p.stage3_raised_domain);
            *promoted.entry(g.domain.as_str()).or_default() += 1;
        }
    }
    let (kept, released): (Vec<CarveGroup>, Vec<CarveGroup>) =
        carve.iter().cloned().partition(|g| sealed.contains(&g.group_key) || used.contains(&g.group_key));
    ensure!(!released.is_empty(), "nothing to release: the sealed Test-2 T membership and the mock hold every reserve group");

    // The stock in force, plus the released groups.
    let cur = load_current(repo, args.pool_dir, args.allowlist, p.horizon)?;
    let ce = cur.allow.carve_exclusion.as_ref().context("carve exclusion")?;
    let draft_sha = r["application"]["draft"]["sha256"].as_str().unwrap_or_default();
    ensure!(ce.groups.sha256 == gsha && ce.record.sha256 == draft_sha,
        "refused: the allow-list in force does not exclude this applied carve (a release starts from the applied carve, with nothing released yet)");
    let mut inp = cur.inp.clone();
    for g in &released {
        inp.domains.get_mut(&g.domain).with_context(|| format!("refused: the pool in force has no {} domain", g.domain))?.udocs += g.expected_udocs;
    }
    let t = trial(&inp, p, &BTreeMap::new())?;
    let w = t.weights.map_err(|e| anyhow::anyhow!("refused: the stock after the release cannot be re-waterfilled: {e}"))?;
    let w_a8: BTreeMap<String, u64> = serde_json::from_value(r["result"]["w_a8"]["weights_ppm"].clone())?;
    let carved: BTreeSet<String> = p.targets.keys().cloned().collect();
    let mut by: BTreeMap<&str, u64> = BTreeMap::new();
    for g in &released {
        *by.entry(g.domain.as_str()).or_default() += 1;
    }
    let pool_id = args.pool_id.map(str::to_string).unwrap_or_else(|| cur.manifest["pool_id"].as_str().unwrap_or_default().to_string());
    let bp = build_params_of(&cur.capacity, &pool_id)?;
    let [out, po] = out_paths(repo, [args.out, args.pool_out])?;

    let body = groups_body(&kept)?;
    create_private_dir(&out)?;
    let written = (|| -> Result<Value> {
        let rel = &out.rel;
        let ksha = write_private(&out.path.join(GROUPS_FILE), body.as_bytes())?;
        let record = json!({
            "schema": RECORD_SCHEMA,
            "rule": CARVE_RULE,
            "kind": RELEASE_SCHEMA,
            "status": "released_by_amendment",
            "computed_utc": utc_now(),
            "files": {"carve_groups": {"path": format!("{rel}/{GROUPS_FILE}"), "sha256": ksha, "rows": kept.len()}},
            "result": {
                "carve_record": {"path": args.record, "sha256": args.record_sha256},
                "test2_t_sealed": {"path": args.test2_t.path, "sha256": args.test2_t.sha256, "rows": sealed.len(),
                    "promoted_from_reserve_by_domain": promoted, "logged_in_entry_utc": sealed_entry["utc"]},
                "mock_used": {"path": args.mock_used.path, "sha256": args.mock_used.sha256, "rows": used.len(), "logged_in_entry_utc": mock_entry["utc"]},
                "amendment": {"entry_utc": amend["utc"], "release_condition": amend["release_condition"], "freeze_log_sha256": log.sha256, "chain": "verified"},
                "released": {"rows": released.len(), "by_domain": by, "role": ROLE_RESERVE},
                "kept": {"rows": kept.len()},
                "stock_in_force": {"pool_dir": args.pool_dir, "manifest_sha256": cur.manifest_sha, "allowlist": args.allowlist,
                    "allowlist_id": cur.allow.allowlist_id, "active_reference": cur.allow.active_reference, "weights_ppm": cur.inp.manifest_weights},
                "weights": w,
                "ar8": t.ar8,
                "tv_from_w_a8_ppm": tv_ppm(&w_a8, &w.weights_ppm),
                "tv_from_w_a8_carved_domains_ppm": tv_ppm(&renorm(&w_a8, &carved)?, &renorm(&w.weights_ppm, &carved)?),
                "tv_from_stock_in_force_ppm": tv_ppm(&cur.inp.manifest_weights, &w.weights_ppm),
                "mock_evidence": "stays at W_A8 (§5.3 step 5)",
                "exclusion": {"carve_groups_sha256": ksha, "rows": kept.len(), "weights_ppm": w.weights_ppm},
            },
        });
        let rb = serde_json::to_vec_pretty(&record)?;
        let rsha = write_private(&out.path.join("release.json"), &rb)?;
        // The successor allow-list v3: the allow-list in force with the successor exclusion.
        let mut a = cur.allow.clone();
        a.allowlist_id = format!("{}-release", cur.allow.allowlist_id);
        a.carve_exclusion = Some(CarveExclusion {
            rule: CARVE_RULE.into(),
            groups: FileRef { path: format!("{rel}/{GROUPS_FILE}"), sha256: ksha },
            rows: kept.len() as u64,
            record: FileRef { path: format!("{rel}/release.json"), sha256: rsha.clone() },
            weights_ppm: w.weights_ppm.clone(),
            base_allowlist_sha256: ce.base_allowlist_sha256.clone(),
        });
        let v3 = serde_json::to_vec_pretty(&a)?;
        StockAllowlist::parse(&v3)?;
        let asha = write_private(&out.path.join("allowlist-v3.json"), &v3)?;
        let m = pool_v2::build(repo, &v3, &bp, &po.path)?;
        ensure!(m["domain_weights"]["weights_ppm"] == json!(w.weights_ppm), "the rebuilt pool's weights are not the release's");
        write_private(&po.path.join("allowlist-v3.json"), &v3)?;
        Ok(json!({"dir": rel, "release_sha256": rsha, "allowlist_v3_sha256": asha, "record": record,
            "pool": {"dir": po.rel, "pool_id": pool_id, "groups": m["groups"], "domain_weights_ppm": m["domain_weights"]["weights_ppm"]}}))
    })();
    if written.is_err() && !po.path.exists() {
        // Nothing reached pool v2: remove this call's own new directory so a retry starts clean.
        let _ = fs::remove_dir_all(&out.path);
    }
    written
}

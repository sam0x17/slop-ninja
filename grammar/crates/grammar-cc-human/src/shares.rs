//! Versioned launch-share tables for CC human parents, and the density-rule
//! caps they are derived from.
//!
//! **Density rule** (`DENSITY_RULE_ID`, RECOMMENDATION.md §3 item 4): for every
//! guessable stratum S, its share of human parents f_S ≤ budget · I_S / (g · C),
//! where I_S is the ideal index size of S (every valid passage run of every
//! page in the stratum's frame, at −log2(FPR) bits per key), g the gain per
//! unit coverage and C the learned capacity of an artifact at the cap. The
//! share cap uses the measured partial-coverage g; the whole-stratum cap uses
//! g = 1. A stratum's frame is the lookup-v1 unique-document estimate scaled
//! by the stratum's pilot count under the rule set being priced, so caps
//! depend on the rule version: `caps` records which rules made the
//! assignment it reads.
//!
//! **Share tables** live in `rules/launch-shares-v*.json`, in percent of human
//! parents. `derive` cuts named strata to their caps (floored to three
//! significant figures, so a share never exceeds its cap) and gives the freed
//! points to one stratum without a binding cap. Email/business is split into
//! press releases and list mail, the two channels the caps price separately.
use crate::{domain::Rules, precision, store};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

pub const DENSITY_RULE_ID: &str = "cc_density_rule_v1";
/// Total lookup gain allowed, as a share of B0.
pub const BUDGET: f64 = 0.005;
/// Gain per unit coverage for excerpt keys: the measured maximum over 1-10%
/// coverage (lookup-v1).
pub const G_PARTIAL: f64 = 0.44;
/// Gain per unit coverage of an attacker who completes a stratum.
pub const G_WHOLE: f64 = 1.0;
/// Learned capacity at the parameter cap: 160M parameters at 3.6 bits each.
pub const CAPACITY_BYTES: f64 = 72e6;
/// False-positive rate of the ideal index the caps price.
pub const FPR: f64 = 1e-4;
pub const DERIVATION_ID: &str = "cc_share_derivation_v1";

pub const SHARES_V1: &str = include_str!("../rules/launch-shares-v1.json");
pub const SHARES_V2: &str = include_str!("../rules/launch-shares-v2.json");
pub const SHARES_V3: &str = include_str!("../rules/launch-shares-v3.json");
pub const SHARES_V4: &str = include_str!("../rules/launch-shares-v4.json");
pub const SHARES_V5: &str = include_str!("../rules/launch-shares-v5.json");
/// Every table, in version order.
pub const TABLES: [&str; 5] = [SHARES_V1, SHARES_V2, SHARES_V3, SHARES_V4, SHARES_V5];
/// The default table for new batches: every stratum at or under its share
/// cap on the rules-v3 assignment (a default pending confirmation).
pub const DEFAULT_ID: &str = "cc_launch_shares_v5";

/// Cohort domains in launch order (the era batch allocates in this order).
pub const DOMAINS: [&str; 9] = ["news", "essays", "technical", "reviews", "forum", "legal", "speech", "fiction", "email"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EmailSplit {
    pub press_release: f64,
    pub list_mail: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ShareTable {
    pub id: String,
    pub status: String,
    pub note: String,
    /// Percent of human parents per cohort domain (email = press + list mail).
    pub shares: BTreeMap<String, f64>,
    pub email_split: EmailSplit,
    /// How the table was derived (base table, caps file and its rules, cut strata).
    #[serde(default)]
    pub derivation: Value,
}

impl ShareTable {
    pub fn parse(text: &str) -> Result<ShareTable> {
        let t: ShareTable = serde_json::from_str(text).context("share table JSON")?;
        t.check()?;
        Ok(t)
    }

    /// The table with this id.
    pub fn by_id(id: &str) -> Result<ShareTable> {
        for t in TABLES {
            let x = ShareTable::parse(t)?;
            if x.id == id {
                return Ok(x);
            }
        }
        bail!("unknown share table {id}")
    }

    pub fn default_table() -> ShareTable {
        ShareTable::by_id(DEFAULT_ID).expect("default share table")
    }

    pub fn check(&self) -> Result<()> {
        let keys: Vec<&str> = self.shares.keys().map(String::as_str).collect();
        let mut want: Vec<&str> = DOMAINS.to_vec();
        want.sort_unstable();
        ensure!(keys == want, "{}: domains {keys:?}", self.id);
        ensure!(self.shares.values().all(|&v| v >= 0.0), "{}: negative share", self.id);
        let total: f64 = self.shares.values().sum();
        ensure!((total - 100.0).abs() < 1e-6, "{}: shares sum to {total}", self.id);
        let e = self.email_split.press_release + self.email_split.list_mail;
        ensure!((e - self.shares["email"]).abs() < 1e-9, "{}: email split {e} != {}", self.id, self.shares["email"]);
        Ok(())
    }

    /// (domain, fraction) in launch order.
    pub fn fractions(&self) -> Vec<(&'static str, f64)> {
        DOMAINS.iter().map(|d| (*d, self.shares[*d] / 100.0)).collect()
    }

    /// Share of a stratum: a domain, `press_release` or `list_mail`.
    pub fn stratum(&self, s: &str) -> Option<f64> {
        match s {
            "press_release" => Some(self.email_split.press_release),
            "list_mail" => Some(self.email_split.list_mail),
            d => self.shares.get(d).copied(),
        }
    }
}

/// Floors a positive value to three significant figures.
pub fn floor_sig3(x: f64) -> f64 {
    if x <= 0.0 || !x.is_finite() {
        return 0.0;
    }
    let e = x.log10().floor() as i32 - 2;
    let m = 10f64.powi(e);
    // A tiny tolerance keeps exact three-figure values (9.4 stored as 9.39999...) intact.
    ((x / m) + 1e-9).floor() * m
}

/// Ideal index bits per key at `FPR`.
pub fn bits_per_key() -> f64 {
    -FPR.log2()
}

/// Share caps (percent) of a stratum with `unique` documents and `keys_per_doc` keys.
pub fn stratum_caps(unique: f64, keys_per_doc: f64) -> (f64, f64, f64) {
    let ideal_bytes = unique * keys_per_doc * bits_per_key() / 8.0;
    let cap = |g: f64| 100.0 * BUDGET * ideal_bytes / (g * CAPACITY_BYTES);
    (ideal_bytes, cap(G_PARTIAL), cap(G_WHOLE))
}

#[derive(Deserialize)]
struct PoolRuns {
    id: String,
    valid_runs: usize,
}

pub struct CapsArgs<'a> {
    /// lookup-v1 `frame.json` (per-domain unique documents and v1 pilot counts).
    pub frame: &'a Path,
    /// Pilot root (holds `pool/admitted.jsonl.zst`, for valid runs per page).
    pub root: &'a Path,
    /// Per-document assignment (`reassign --per-doc`).
    pub assignments: &'a Path,
    /// The rules that made the assignment (for the email channels).
    pub rules: &'a Rules,
    pub out: &'a Path,
}

/// Density-rule caps of every cohort domain and email channel under one
/// assignment: count, mean valid runs per page, scaled unique documents,
/// ideal index bytes, share cap and whole-stratum cap.
pub fn caps(a: &CapsArgs) -> Result<Value> {
    let frame: Value = store::read_json(a.frame)?;
    let pd = frame["per_domain"].as_object().context("frame per_domain")?;
    let runs: HashMap<String, usize> = store::rows::<PoolRuns>(&a.root.join("pool/admitted.jsonl.zst"))?
        .into_iter()
        .map(|r| (r.id, r.valid_runs))
        .collect();
    let docs: Vec<precision::DocAssignment> = store::rows(a.assignments)?;
    ensure!(docs.len() == runs.len(), "{} assignments for {} pool rows", docs.len(), runs.len());
    // stratum -> (count, sum of valid runs)
    let mut acc: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut excluded = 0usize;
    for d in &docs {
        let v = *runs.get(&d.id).with_context(|| format!("{}: not in the pool", d.id))?;
        if d.excluded.is_some() {
            excluded += 1;
            continue;
        }
        let e = acc.entry(d.domain.clone()).or_default();
        *e = (e.0 + 1, e.1 + v);
        if let Some(ch) = precision::email_channel(a.rules, &d.rule) {
            let key = if ch == "press_release" { "press_release" } else { "list_mail" };
            let e = acc.entry(key.to_owned()).or_default();
            *e = (e.0 + 1, e.1 + v);
            if ch != "press_release" && ch != "list_archive" {
                let e = acc.entry(format!("list_mail_{ch}")).or_default();
                *e = (e.0 + 1, e.1 + v);
            }
        }
    }
    let mut out = BTreeMap::new();
    for (s, (n, rsum)) in &acc {
        // Email channels scale the email frame.
        let fd = if s.starts_with("press_release") || s.starts_with("list_mail") { "email" } else { s.as_str() };
        let Some(f) = pd.get(fd) else { continue };
        let v1_docs = f["pilot_docs"].as_f64().context("pilot_docs")?;
        let unique_v1 = f["unique_docs"].as_f64().context("unique_docs")?;
        let unique = unique_v1 * *n as f64 / v1_docs;
        let k = *rsum as f64 / *n as f64;
        let (bytes, cap, whole) = stratum_caps(unique, k);
        out.insert(s.clone(), json!({"count": n, "v1_pilot_count": v1_docs, "runs_per_page": k, "unique_docs": unique,
            "ideal_index_bytes": bytes, "share_cap_percent": cap, "whole_stratum_cap_percent": whole}));
    }
    let rec = json!({
        "schema": "slop_ninja_cc_human_density_caps_v1", "density_rule": DENSITY_RULE_ID,
        "parameters": {"budget_share_of_b0": BUDGET, "g_partial": G_PARTIAL, "g_whole": G_WHOLE, "capacity_bytes": CAPACITY_BYTES,
            "fpr": FPR, "bits_per_key": bits_per_key(),
            "rule": "cap = 100 * budget * I / (g * C); I = unique * runs_per_page * bits_per_key / 8; unique = lookup-v1 unique_docs * count / v1 pilot count; email channels scale the email frame"},
        "frame": a.frame, "frame_sha256": crate::measure::file_sha256(a.frame)?,
        "assignments": a.assignments, "assignments_sha256": crate::measure::file_sha256(a.assignments)?,
        "rules": a.rules.version, "rules_sha256": a.rules.sha256,
        "excluded": excluded, "strata": out,
    });
    store::write_json(a.out, &rec)?;
    Ok(rec)
}

/// Cuts each stratum in `cut` (domains, `press_release` or `list_mail`) to
/// its floored share cap from a `caps` record when the base share exceeds
/// it, and gives the freed points to `to`. The new table records its
/// derivation; it is not checked against strata that are not cut.
pub fn derive(base: &ShareTable, caps: &Value, cut: &[String], to: &str, id: &str, note: &str) -> Result<ShareTable> {
    ensure!(DOMAINS.contains(&to) && to != "email", "freed points must go to a cohort domain other than email");
    ensure!(!cut.iter().any(|c| c == to), "{to} is both cut and the receiver");
    let mut t = base.clone();
    let mut freed = 0.0;
    let mut steps = Vec::new();
    for s in cut {
        let cap = caps["strata"][s]["share_cap_percent"].as_f64().with_context(|| format!("no cap for {s}"))?;
        let have = base.stratum(s).with_context(|| format!("unknown stratum {s}"))?;
        let new = have.min(floor_sig3(cap));
        freed += have - new;
        match s.as_str() {
            "press_release" => t.email_split.press_release = new,
            "list_mail" => t.email_split.list_mail = new,
            "email" => bail!("cut email by its channels (press_release, list_mail)"),
            d => {
                t.shares.insert(d.to_owned(), new);
            }
        }
        steps.push(json!({"stratum": s, "base": have, "cap": cap, "new": new}));
    }
    t.shares.insert("email".into(), t.email_split.press_release + t.email_split.list_mail);
    *t.shares.get_mut(to).unwrap() += freed;
    // Exact decimal representation for the table file.
    for v in t.shares.values_mut() {
        *v = (*v * 1e9).round() / 1e9;
    }
    t.email_split.press_release = (t.email_split.press_release * 1e9).round() / 1e9;
    t.email_split.list_mail = (t.email_split.list_mail * 1e9).round() / 1e9;
    t.shares.insert("email".into(), ((t.email_split.press_release + t.email_split.list_mail) * 1e9).round() / 1e9);
    let others: f64 = t.shares.iter().filter(|(k, _)| k.as_str() != to).map(|(_, v)| v).sum();
    t.shares.insert(to.to_owned(), ((100.0 - others) * 1e9).round() / 1e9);
    t.id = id.to_owned();
    t.status = "derived".into();
    t.note = note.to_owned();
    t.derivation = json!({"rule": DERIVATION_ID, "base": base.id, "caps_rules": caps["rules"], "caps_rules_sha256": caps["rules_sha256"],
        "caps_assignments_sha256": caps["assignments_sha256"], "density_rule": caps["density_rule"], "cut": steps, "freed_to": to, "freed": freed,
        "method": "each cut stratum: min(base, cap floored to three significant figures); the freed points go to the receiver"});
    t.check()?;
    Ok(t)
}

/// Strata of `t` whose share exceeds their share cap in a `caps` record.
pub fn over_caps(t: &ShareTable, caps: &Value) -> Vec<(String, f64, f64)> {
    let mut out = Vec::new();
    for s in DOMAINS.iter().filter(|d| **d != "email").map(|d| d.to_string()).chain(["press_release".to_owned(), "list_mail".to_owned()]) {
        if let (Some(have), Some(cap)) = (t.stratum(&s), caps["strata"][&s]["share_cap_percent"].as_f64())
            && have > cap + 1e-12
        {
            out.push((s, have, cap));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_parses_sums_to_100_and_has_unique_ids() {
        let mut ids = std::collections::HashSet::new();
        for t in TABLES {
            let x = ShareTable::parse(t).unwrap();
            assert!(ids.insert(x.id.clone()), "{}", x.id);
            let f: f64 = x.fractions().iter().map(|(_, v)| v).sum();
            assert!((f - 1.0).abs() < 1e-9);
        }
        assert_eq!(ShareTable::default_table().id, DEFAULT_ID);
        assert!(ShareTable::by_id("nope").is_err());
        // Derived tables keep their receiver exact to 1e-9.
        for id in ["cc_launch_shares_v3", "cc_launch_shares_v4", "cc_launch_shares_v5"] {
            let t = ShareTable::by_id(id).unwrap();
            assert_eq!(t.status, "derived");
            assert_eq!(t.derivation["freed_to"], json!("essays"));
            assert_eq!((t.shares["essays"] * 1e9).round() / 1e9, t.shares["essays"]);
        }
        // v1 is the era-v1 batch table.
        let v1 = ShareTable::by_id("cc_launch_shares_v1").unwrap();
        assert_eq!(v1.fractions()[0], ("news", 0.275));
        assert_eq!(v1.stratum("list_mail"), Some(0.05));
    }

    #[test]
    fn caps_reproduce_the_section_1_arithmetic() {
        // Technical under v1 (lookup-v1 frame): 70.8M unique, 6.97 runs per page -> 12.9%.
        let (_, cap, whole) = stratum_caps(70_819_853.0, 6.973_084_886);
        assert!((cap - 12.9).abs() < 0.05, "{cap}");
        assert!((whole - cap * G_PARTIAL).abs() < 1e-9);
        assert!((bits_per_key() - 13.2877).abs() < 1e-4);
        assert_eq!(floor_sig3(9.389), 9.38);
        assert_eq!(floor_sig3(0.03836), 0.0383);
        assert_eq!(floor_sig3(9.4), 9.4);
        assert_eq!(floor_sig3(0.0), 0.0);
    }

    #[test]
    fn derive_cuts_listed_strata_and_frees_points_to_the_receiver() {
        let base = ShareTable::by_id("cc_launch_shares_v2").unwrap();
        let caps = json!({"rules": "r", "rules_sha256": "s", "assignments_sha256": "a", "density_rule": DENSITY_RULE_ID, "strata": {
            "technical": {"share_cap_percent": 9.3876}, "legal": {"share_cap_percent": 3.1849}, "reviews": {"share_cap_percent": 6.4123},
            "list_mail": {"share_cap_percent": 0.03836}, "news": {"share_cap_percent": 28.1}, "press_release": {"share_cap_percent": 0.683},
            "forum": {"share_cap_percent": 7.8}, "speech": {"share_cap_percent": 2.8}, "fiction": {"share_cap_percent": 1.9}}});
        let cut: Vec<String> = ["technical", "legal", "reviews", "list_mail"].iter().map(|s| s.to_string()).collect();
        let t = derive(&base, &caps, &cut, "essays", "t", "test").unwrap();
        assert_eq!(t.shares["technical"], 9.38);
        assert_eq!(t.shares["legal"], 3.18);
        assert_eq!(t.shares["reviews"], 6.41);
        assert_eq!(t.email_split.list_mail, 0.0383);
        assert!((t.shares["email"] - 0.6883).abs() < 1e-9);
        let freed = (12.0 - 9.38) + (5.4 - 3.18) + (6.5 - 6.41) + (0.05 - 0.0383);
        assert!((t.shares["essays"] - (36.0 + freed)).abs() < 1e-6);
        t.check().unwrap();
        // Forum sat at its cap in v2 (7.8 = 7.8): not over; nothing else is over.
        assert!(over_caps(&t, &caps).is_empty());
        let mut over = t.clone();
        *over.shares.get_mut("news").unwrap() = 30.0;
        assert_eq!(over_caps(&over, &caps)[0].0, "news");
        // A share already below its cap is kept.
        let low = json!({"strata": {"technical": {"share_cap_percent": 50.0}}});
        let t2 = derive(&base, &low, &["technical".to_string()], "essays", "t2", "").unwrap();
        assert_eq!(t2.shares["technical"], 12.0);
        assert!(derive(&base, &caps, &["essays".to_string()], "essays", "x", "").is_err());
        assert!(derive(&base, &caps, &["email".to_string()], "essays", "x", "").is_err());
    }
}

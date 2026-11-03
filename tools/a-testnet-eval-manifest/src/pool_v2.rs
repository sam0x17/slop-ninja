//! Builds the pool v2 manifest (`slop_ninja_testnet_human_pool_v2`, reservation
//! stage) from the public-stock allow-list: loads the reservations, runs the
//! reservation gate, applies its closure exclusions, reweights the domains and
//! writes the capacity report. Everything is written to a new directory under
//! `data/` with owner-only permissions; nothing is uploaded.
//!
//! A reservation-stage manifest names the stock's groups and their expected
//! documents but holds no text: the v1 engine reads only
//! `slop_ninja_testnet_human_pool_v1` and refuses it, and the rows (text shards,
//! digests, the text-level gate) come from acquisition under the same allow-list.
//! The manifest lists the screens acquisition must still apply.
//!
//! Capacity uses the yield the sealed Test's acquisition realized, and the domain
//! weights are filled on its one-sided lower confidence bound
//! ([`YIELD_MARGIN_RULE`]), so a realized yield at that bound still lasts the
//! horizon.
use crate::capacity::{self, Canary, Params, Reuse};
use crate::gate::{self, StockIndex};
use crate::sha256_hex;
use crate::stock::{self, FileRef, MICRO, StockAllowlist, StockGroup};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub const POOL_V2_SCHEMA: &str = "slop_ninja_testnet_human_pool_v2";
pub const GROUP_ROW_SCHEMA: &str = "slop_ninja_testnet_human_pool_v2_group_v1";
pub const EXCLUSION_SCHEMA: &str = "slop_ninja_testnet_human_pool_v2_exclusion_v2";
pub const CAPACITY_SCHEMA: &str = "slop_ninja_testnet_public_stock_capacity_v2";
/// The yield margin: the one-sided 95% lower bound of the Test's realized document
/// count (Poisson, Wilson-Hilferty).
pub const YIELD_MARGIN_RULE: &str = "poisson_lower_wilson_hilferty_v1";
pub const YIELD_MARGIN_CONFIDENCE: f64 = 0.95;
/// The standard normal quantile at [`YIELD_MARGIN_CONFIDENCE`].
pub const YIELD_MARGIN_Z: f64 = 1.644_853_626_951_472_2;

/// Canary scenarios reported (not reserved in the weights unless `plan.canary`).
#[derive(Clone, Debug)]
pub struct CanaryReport {
    /// Fresh documents per audit draw.
    pub docs: u32,
    /// Audit draws per candidate when the first audit flags (the re-audit).
    pub flagged_audits: u32,
    /// Cohorts between promotion candidates.
    pub every: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct BuildParams {
    pub pool_id: String,
    /// Horizon the manifest's domain weights are filled to, in cohorts.
    pub horizon: u32,
    /// Further horizons reported (not used for the weights).
    pub report_horizons: Vec<u32>,
    pub plan: Params,
    /// Live engine mix, reported for comparison.
    pub engine_mix: [u32; 3],
    /// Entrants per cohort, recorded: human demand does not depend on it.
    pub roster: u32,
    pub cohorts_per_day: u32,
    pub canary_report: Option<CanaryReport>,
}

fn write_private(dir: &Path, name: &str, body: &[u8]) -> Result<String> {
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(dir.join(name))?;
    f.write_all(body)?;
    f.sync_all()?;
    Ok(sha256_hex(body))
}

fn days(cohorts: u32, per_day: u32) -> f64 {
    f64::from(cohorts) / f64::from(per_day)
}

fn scenario(name: &str, stock: &BTreeMap<String, u64>, w: &BTreeMap<String, u64>, p: &Params, per_day: u32) -> Result<Value> {
    let o = capacity::simulate(stock, w, p)?;
    Ok(json!({"name": name, "weights_ppm": w,
        "days_before_first_bind": days(o.cohorts_before_first_bind, per_day),
        "days_with_reweighting": days(o.cohorts_with_reweighting, per_day),
        "outcome": o}))
}

/// Capacity at a realization factor per domain.
fn capacity_at(agg: &BTreeMap<String, u64>, factor: impl Fn(&str) -> f64) -> BTreeMap<String, u64> {
    agg.iter().map(|(d, u)| (d.clone(), (*u as f64 / MICRO * factor(d)).floor() as u64)).collect()
}

/// Builds pool v2 under `out` and returns its manifest.
pub fn build(repo: &Path, allowlist_bytes: &[u8], bp: &BuildParams, out: &Path) -> Result<Value> {
    ensure!(
        !bp.pool_id.is_empty() && bp.pool_id.len() <= 64 && bp.pool_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "pool id must be 1-64 ASCII letters, digits or hyphens"
    );
    ensure!(bp.cohorts_per_day > 0, "cohorts per day must be positive");
    bp.plan.check()?;
    let a = StockAllowlist::parse(allowlist_bytes)?;
    let rules = stock::test_rules(repo, &a.test_rules, &a.assignment_prose_guard)?;
    let index_domains: BTreeSet<String> = a.index_reservations.iter().map(|i| i.domain.clone()).collect();
    let assigned = gate::evaluation_assignments(repo, &a, &index_domains)?;
    let st = stock::load_stock(repo, &a, &rules, &assigned)?;
    let mut ix = StockIndex::build(&st.groups)?;
    let url_inputs = ix.add_unit_urls(repo, &a.unit_urls)?;
    // Index reservations carry their units' titles too.
    let title_files: Vec<FileRef> = a.unit_titles.iter().cloned()
        .chain(a.index_reservations.iter().map(|i| FileRef { path: i.path.clone(), sha256: i.sha256.clone() }))
        .collect();
    let title_inputs = ix.add_unit_titles(repo, &title_files)?;
    let mut g = gate::run(repo, &a, &st.groups, &ix)?;
    drop(ix);
    // Allow-list v3: the Test-2 T carve and the mock reserve leave the stock too.
    let carved = crate::carve_v3::exclusion(repo, &a, &st.groups, &g)?;
    let is_carved = |i: usize| carved.as_ref().is_some_and(|c| c.groups.contains_key(&(i as u32)));

    // Per-domain stock after the closure exclusions.
    #[derive(Default)]
    struct Agg {
        groups: u64,
        units: u64,
        expected_udocs: u64,
        max_group_udocs: u64,
        max_group_units: u64,
        excluded_groups: u64,
        excluded_udocs: u64,
        carved_groups: u64,
        carved_udocs: u64,
    }
    let mut agg: BTreeMap<String, Agg> = rules.keys().map(|d| (d.clone(), Agg::default())).collect();
    for (i, s) in st.groups.iter().enumerate() {
        let e = agg.get_mut(&s.domain).expect("stock domains have Test rules");
        if g.excluded.contains_key(&(i as u32)) {
            e.excluded_groups += 1;
            e.excluded_udocs += s.expected_udocs;
        } else if is_carved(i) {
            e.carved_groups += 1;
            e.carved_udocs += s.expected_udocs;
        } else {
            e.groups += 1;
            e.units += s.units;
            e.expected_udocs += s.expected_udocs;
            e.max_group_udocs = e.max_group_udocs.max(s.expected_udocs);
            e.max_group_units = e.max_group_units.max(s.units);
        }
    }
    let udocs: BTreeMap<String, u64> = agg.iter().map(|(d, e)| (d.clone(), e.expected_udocs)).collect();
    let cap_docs = capacity_at(&udocs, |d| rules[d].realization());
    let lower_docs = capacity_at(&udocs, |d| rules[d].realization_lower(YIELD_MARGIN_Z));
    let expected_docs = capacity_at(&udocs, |_| 1.0);
    let base: BTreeMap<String, u64> = rules.iter().map(|(d, r)| (d.clone(), r.mass)).collect();
    // The manifest weights hold at the lower bound of the realized yield.
    let rw = capacity::waterfill(&base, &lower_docs, &bp.plan, bp.horizon)?;
    if let Some(c) = &a.carve_exclusion {
        ensure!(
            rw.weights_ppm == c.weights_ppm,
            "refused: the weights after the carve exclusion differ from the carve record's; nothing was written. A removal that would change a manifest weight before the effective cohort waits for an amendment (PROTOCOL-A2 amendment 1, item 1.11)"
        );
    }
    let counts = crate::engine_v1::kind_counts(bp.plan.n, bp.plan.mix);
    let mut per_domain = BTreeMap::new();
    for (d, e) in &agg {
        let r = &rules[d];
        let w = rw.weights_ppm[d];
        // Negatives a cohort takes from the domain at its weight, and how many
        // cohorts' quota its largest group holds (whole-group draws need a carried quota).
        let per_cohort = w as f64 * f64::from(counts[0]) / capacity::PPM as f64;
        let largest = e.max_group_udocs as f64 / MICRO;
        let mut row = json!({"groups": e.groups, "units": e.units, "expected_documents": e.expected_udocs as f64 / MICRO,
            "excluded_groups": e.excluded_groups, "excluded_expected_documents": e.excluded_udocs as f64 / MICRO,
            "realization": r.realization(), "realization_lower": r.realization_lower(YIELD_MARGIN_Z),
            "capacity_documents": cap_docs[d], "capacity_lower_documents": lower_docs[d], "test_mass": r.mass,
            "largest_group": {"units": e.max_group_units, "expected_documents": largest},
            "negatives_per_cohort_at_weight": per_cohort,
            "cohorts_of_quota_in_largest_group": if per_cohort > 0.0 { Some(largest / per_cohort) } else { None },
            "rule": {"cap": r.cap, "min_yield": r.min_yield, "yields_seen": r.yields_seen,
                "capped_min_units": r.capped_min_units, "test_groups": r.test_groups,
                "processed_groups": r.processed_groups, "processed_expected": r.processed_expected,
                "realized_documents": r.realized_documents}});
        if carved.is_some() {
            row["carved_groups"] = json!(e.carved_groups);
            row["carved_expected_documents"] = json!(e.carved_udocs as f64 / MICRO);
        }
        per_domain.insert(d.clone(), row);
    }

    // Scenarios.
    let per_day = bp.cohorts_per_day;
    let nonempty: BTreeMap<String, f64> =
        base.iter().filter(|(d, _)| cap_docs[*d] > 0).map(|(d, m)| (d.clone(), *m as f64)).collect();
    let test_mass = capacity::to_ppm(&nonempty);
    let mut test_mass_all = test_mass.clone();
    for d in cap_docs.keys() {
        test_mass_all.entry(d.clone()).or_insert(0);
    }
    let proportional = capacity::stock_weights(&cap_docs);
    let engine_as_coded = Params { mix: bp.engine_mix, reuse: Reuse::EngineV1, canary: None, ..bp.plan.clone() };
    let engine_target = Params { reuse: Reuse::EngineV1, canary: None, ..bp.plan.clone() };
    let mut horizons = Vec::new();
    for h in &bp.report_horizons {
        match capacity::waterfill(&base, &lower_docs, &bp.plan, *h) {
            Ok(w) => {
                let lo = capacity::simulate(&lower_docs, &w.weights_ppm, &bp.plan)?;
                let o = capacity::simulate(&cap_docs, &w.weights_ppm, &bp.plan)?;
                horizons.push(json!({"horizon_cohorts": h, "estimand_change_tv_ppm": w.estimand_change_tv_ppm,
                    "retired": w.retired, "reduced": w.reduced, "weights_ppm": w.weights_ppm,
                    "days_before_first_bind_lower_yield": days(lo.cohorts_before_first_bind, per_day),
                    "days_before_first_bind": days(o.cohorts_before_first_bind, per_day),
                    "days_with_reweighting": days(o.cohorts_with_reweighting, per_day)}));
            }
            Err(e) => horizons.push(json!({"horizon_cohorts": h, "infeasible": e.to_string()})),
        }
    }
    let manifest_lower = scenario("manifest_weights_lower_yield", &lower_docs, &rw.weights_ppm, &bp.plan, per_day)?;
    let manifest_run = scenario("manifest_weights", &cap_docs, &rw.weights_ppm, &bp.plan, per_day)?;
    let scenarios = vec![
        manifest_lower.clone(),
        manifest_run.clone(),
        scenario("manifest_weights_expected_yield", &expected_docs, &rw.weights_ppm, &bp.plan, per_day)?,
        scenario("test_mass_without_empty_domains", &cap_docs, &test_mass_all, &bp.plan, per_day)?,
        scenario("stock_proportional", &cap_docs, &proportional, &bp.plan, per_day)?,
        scenario("stock_proportional_expected_yield", &expected_docs, &capacity::stock_weights(&expected_docs), &bp.plan, per_day)?,
        scenario("engine_v1_rule_engine_mix", &cap_docs, &proportional, &engine_as_coded, per_day)?,
        scenario("engine_v1_rule_target_mix", &cap_docs, &proportional, &engine_target, per_day)?,
    ];
    // Canary audits drawn from this stock, under stated promotion-candidate rates.
    let mut canary_rows = Vec::new();
    if let Some(cr) = &bp.canary_report {
        for every in &cr.every {
            for audits in BTreeSet::from([1, cr.flagged_audits]) {
                let p = Params { canary: Some(Canary { docs: cr.docs, audits, every: *every }), ..bp.plan.clone() };
                let o = capacity::simulate(&cap_docs, &rw.weights_ppm, &p)?;
                let lo = capacity::simulate(&lower_docs, &rw.weights_ppm, &p)?;
                canary_rows.push(json!({"candidate_every_cohorts": every, "audits_per_candidate": audits,
                    "days_before_first_bind": days(o.cohorts_before_first_bind, per_day),
                    "days_before_first_bind_lower_yield": days(lo.cohorts_before_first_bind, per_day),
                    "days_with_reweighting": days(o.cohorts_with_reweighting, per_day),
                    "canary_audits": o.canary_audits, "fresh_by_purpose": o.fresh_by_purpose, "stop": o.stop}));
            }
        }
    }
    let canary = bp.canary_report.as_ref().map(|cr| json!({
        "documents_per_audit": cr.docs,
        "steady_cohorts_per_audit": f64::from(cr.docs) / f64::from(counts[0]),
        "source": "the lookup canary's fresh draw (design notes, Lookup canary): every promotion candidate is audited, and a first flag triggers a re-audit on a fresh, independent draw",
        "in_manifest_weights": bp.plan.canary.is_some(),
        "scenarios": canary_rows,
    }));
    let total_cap: u64 = cap_docs.values().sum();
    let total_lower: u64 = lower_docs.values().sum();
    let total_expected: u64 = expected_docs.values().sum();
    let capacity_report = json!({
        "schema": CAPACITY_SCHEMA,
        "rule": capacity::PLAN_RULE,
        "plan": bp.plan,
        "roster": bp.roster,
        "roster_note": "every entrant, R_k and R_{k-1} score the same batch and the same probe, so human demand does not depend on the roster",
        "cohorts_per_day": per_day,
        "kind_counts_per_cohort": counts,
        "yield_margin": {"rule": YIELD_MARGIN_RULE, "one_sided_confidence": YIELD_MARGIN_CONFIDENCE, "z": YIELD_MARGIN_Z,
            "applies_to": "the Test acquisition's realized document count per domain; the manifest weights are filled on this lower bound"},
        "stock": {"capacity_documents": total_cap, "capacity_lower_documents": total_lower,
            "expected_documents": total_expected, "by_domain": per_domain},
        "reweighting": rw,
        "scenarios": scenarios,
        "horizons": horizons,
        "canary": canary,
        "limits": [
            "Expected documents come from reservation metadata under the sealed Test's excerpt rules; capacity scales them by the yield the Test's own acquisition realized, never above 1, and the manifest weights hold at the one-sided 95% lower bound of that yield. No stock prose has been acquired; re-run on acquired rows.",
            "Unmeasured yield losses are not in the margin: parents need at least two sentences (all 570 pool v1 rows had them), and acquisition still applies the 2,048-token length filter and the text-level screens; each moves the first bind earlier in proportion.",
            "Documents are counted one by one, but the engine draws whole source groups: a domain whose largest group holds more than one cohort's quota (cohorts_of_quota_in_largest_group above 1) is drawn in lumps and needs a carried quota in the engine to stay on its weight.",
            "New probe versions are drawn after every reweighting (probe_redraw_on_reweight); a version replaced for disclosure is not modelled.",
            "Canary audits drawn from this stock are reported as scenarios and are reserved in the weights only when the plan names them; a promotion candidate with a first flag takes two audit draws.",
            "Shortfall tolerance is zero: a cohort counts only when every kind and every draw scheduled in it is filled.",
        ],
    });

    // Write the pool directory.
    let dir = stock::new_output_dir(repo, out)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let mut groups_body = String::new();
    let mut ex_body = String::new();
    let mut kept = 0u64;
    if let Some(c) = &carved {
        g.report["carve_exclusion"] = c.summary.clone();
    }
    for (i, s) in st.groups.iter().enumerate() {
        if is_carved(i) {
            continue; // counted in the manifest; the carve record names them
        }
        match g.excluded.get(&(i as u32)) {
            None => {
                kept += 1;
                groups_body.push_str(&group_line(s)?);
            }
            Some(checks) => {
                ex_body.push_str(&serde_json::to_string(&json!({"schema": EXCLUSION_SCHEMA, "domain": s.domain,
                    "group_key": s.group_key, "reservation": s.reservation, "action": "group_excluded",
                    "reasons": checks}))?);
                ex_body.push('\n');
            }
        }
    }
    let groups_sha = write_private(&dir, "groups.jsonl", groups_body.as_bytes())?;
    let ex_sha = write_private(&dir, "exclusions.jsonl", ex_body.as_bytes())?;
    let gate_bytes = serde_json::to_vec_pretty(&g.report)?;
    let gate_sha = write_private(&dir, "gate.json", &gate_bytes)?;
    let cap_bytes = serde_json::to_vec_pretty(&capacity_report)?;
    let cap_sha = write_private(&dir, "capacity.json", &cap_bytes)?;
    let title_excluded = g.report["excluded_by_reason"]["r0_training_lineage:raid_train_title_lineage"].as_u64().unwrap_or(0);
    let mut manifest = json!({
        "schema": POOL_V2_SCHEMA,
        "stage": "reservation",
        "purpose": stock::STOCK_PURPOSE,
        "pool_id": bp.pool_id,
        "evidence": "testnet only: dated public text, every document recognizable; never a mainnet evaluation source",
        "reuse": bp.plan.reuse,
        "negatives": "a passage is scored as a pure-human negative at most once",
        "allowlist": {"id": a.allowlist_id, "sha256": sha256_hex(allowlist_bytes), "removed_by": a.removed_by},
        "inputs": st.inputs,
        "unit_urls": url_inputs,
        "unit_titles": title_inputs,
        "expected_rule": stock::EXPECTED_RULE,
        "groups": {"file": "groups.jsonl", "rows": kept, "sha256": groups_sha},
        "exclusions": {"file": "exclusions.jsonl", "rows": g.excluded.len(), "sha256": ex_sha},
        "gate": {"file": "gate.json", "sha256": gate_sha, "verdict": g.report["verdict"],
            "checked": gate_checked(&a)},
        "capacity": {"file": "capacity.json", "sha256": cap_sha,
            "capacity_documents": total_cap, "capacity_lower_documents": total_lower, "expected_documents": total_expected,
            "days_before_first_bind_lower_yield": manifest_lower["days_before_first_bind"],
            "days_before_first_bind": manifest_run["days_before_first_bind"],
            "days_with_reweighting": manifest_run["days_with_reweighting"],
            "canary": if bp.plan.canary.is_some() {
                "the weights reserve the plan's canary audits"
            } else {
                "the weights reserve no canary audit; capacity.json reports days under stated promotion-candidate rates"
            }},
        "domain_weights": {"rule": rw.rule, "horizon_cohorts": rw.horizon_cohorts, "weights_ppm": rw.weights_ppm,
            "filled_on": YIELD_MARGIN_RULE,
            "base": "sealed Test human documents per domain", "base_ppm": rw.base_ppm,
            "retired": rw.retired, "reduced": rw.reduced, "estimand_change_tv_ppm": rw.estimand_change_tv_ppm,
            "rederive": "sigma, delta and w are re-derived for these weights before live rewards"},
        "acquisition_screens": acquisition_screens(&g.report["lineage_screen"], title_excluded, &g.report["mirror_host_rows_pending_text_check"]),
        "rows": "none at the reservation stage: text shards and digests come from acquisition under this allow-list",
        "engine": "the v1 engine reads only slop_ninja_testnet_human_pool_v1 and refuses this manifest",
    });
    if let Some(c) = &carved {
        manifest["carve_exclusion"] = c.summary.clone();
    }
    let mbytes = serde_json::to_vec_pretty(&manifest)?;
    write_private(&dir, "manifest.json", &mbytes)?;
    Ok(manifest)
}

/// The exact artifacts each disjointness claim covers, by role.
fn gate_checked(a: &StockAllowlist) -> Value {
    let mut by_role: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for g in &a.gate {
        by_role.entry(g.role.as_str()).or_default().push(g.path.as_str());
    }
    for f in &a.test_rules.groups {
        let v = by_role.entry("sealed_test").or_default();
        if !v.contains(&f.path.as_str()) {
            v.push(f.path.as_str());
        }
    }
    json!(by_role)
}

/// Screens acquisition must apply before any stock row is admitted, in order. S1
/// ran here on title hashes; the rest need the rows' text.
fn acquisition_screens(screen: &Value, s1_excluded: u64, mirror_rows: &Value) -> Value {
    let rule = |id: &str| screen["rules"].as_array().and_then(|r| r.iter().find(|x| x["id"] == id)).map(|x| x["rule"].clone());
    json!([
        {"id": "S1_title", "source": screen["path"], "rule": rule("S1_title"),
            "status": format!("applied at the reservation stage from title hashes: {s1_excluded} groups excluded (raid_train_title_lineage)")},
        {"id": "S2_reddit_author", "source": screen["path"], "rule": rule("S2_reddit_author"),
            "status": "mandatory at acquisition, before any reserved Reddit author group is admitted"},
        {"id": "S3_text", "source": screen["path"], "rule": rule("S3_text"), "status": "mandatory at acquisition"},
        {"id": "near_duplicate_text",
            "rule": "compare every acquired row's 12-token folded windows (as splits-v5/checks) and folded 50-word sequences (as S3), besides its exact, normalized and folded text digests, with Train-side text (Train, Train-mining and held), the reference model's trainer bundle, the Common Crawl admitted rows and the public-candidate texts; a hit excludes the whole group",
            "status": "mandatory at acquisition: key checks cannot see mirrors, re-posts or overlapping windows"},
        {"id": "mirror_hosts", "rule": "Common Crawl rows on hosts that republish a stock domain's documents are compared with that domain's acquired rows under near_duplicate_text",
            "rows_by_domain_host": mirror_rows, "status": "mandatory at acquisition"},
        {"id": "length_filter", "rule": "drop rows longer than 2,048 tokens under the R_k tokenizer", "status": "mandatory at acquisition"},
        {"id": "two_sentence_parents", "rule": "a rewrite parent needs at least two sentences under the engine's splitter", "status": "measured at acquisition; capacity assumes every group qualifies"},
        {"id": "text_digest_gate", "rule": "re-run this gate with the acquired rows' text digests", "status": "mandatory at acquisition"},
    ])
}

fn group_line(s: &StockGroup) -> Result<String> {
    let mut l = serde_json::to_string(&json!({"schema": GROUP_ROW_SCHEMA, "domain": s.domain, "group_key": s.group_key,
        "reservation": s.reservation, "units": s.units, "expected_udocs": s.expected_udocs,
        "family_keys": if s.family_keys.is_empty() { vec![s.group_key.clone()] } else { s.family_keys.clone() }}))?;
    l.push('\n');
    Ok(l)
}

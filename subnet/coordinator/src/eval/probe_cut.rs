//! Operator-side preparation of a same-authority probe cut and of the
//! standing template reinstall that names its version (`slop_ninja_eval_ops
//! probe-cut` and `template-reinstall`). Pure functions over documents the
//! operator fetched (the eval status, the probe build's private record read
//! back from the eval bucket): nothing here reads the network or a store.
//!
//! A probe cut gives an authority whose cohorts are open a new probe version
//! without a chain-record restart: a probe build (`reward_v1.probe_cut`) over
//! the chain's current references, queued with `POST /eval/plan`, that runs
//! beside the open cohorts and changes none of their plans
//! (`calendar::check_probe_cut`, `engine::advance`). Its probe draw joins
//! the pool's used set, so a later draw (an open cohort's included) prefers
//! rows neither drew. It holds the cohort number after the highest held.
//! Once it closes, the template naming its version is installed with
//! `POST /eval/template`; it governs only the cohorts the calendar queues
//! after the install (`calendar::install`), numbered from one past the cut.
// JSON documents are read with `serde_json::Value`'s `[]`, which yields
// `Null` for a missing key and never panics, so `indexing_slicing` is not
// denied here; no slice is indexed.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use crate::config::{Authority, DeployedAuthority};
use crate::eval::plan::{CohortPlan, Ceilings, PROBE_PLAN_SCHEMA, TASK_ORDER_SEEDED_RANK};
use serde_json::{Value, json};

/// The cut record `probe-cut` writes beside the probe plan.
pub const PROBE_CUT_SCHEMA: &str = "slop_ninja_eval_probe_cut_v1";

/// What a probe cut is prepared from.
pub struct CutInputs<'a> {
    /// `GET /eval/status` (or the Worker's `/status`, which carries it), read
    /// just before the cut.
    pub status: &'a Value,
    /// The authority the tree to deploy tracks (its `wrangler.toml`).
    pub tracked: &'a Authority,
    /// The deployment's plan ceilings (`ROSTER_CAP`).
    pub ceilings: Ceilings,
    /// A probe plan supplying every other value (pool, generators, mix,
    /// probe size as `n`, budgets, fan-out, gates).
    pub probe_template: Value,
    /// The chain's current reference and predecessor, as the stored records
    /// name them (the eval Worker compares them exactly).
    pub reference: Value,
    pub previous_reference: Option<Value>,
    /// The probe version to cut; default one past every version built here.
    pub probe_version: Option<u32>,
}

/// A prepared cut: the probe plan's bytes (checked with the coordinator's
/// own parser and the authority's label and number rule) and its record.
pub struct Cut {
    pub plan: Vec<u8>,
    pub record: Value,
}

/// The probe version a cohort entry of an eval status names: its
/// `reward_v1.probe_version`, else its label `<prefix>probe-v<N>`.
fn entry_probe_version(c: &Value, prefix: &str) -> Option<u32> {
    if let Some(v) = c["reward_v1"]["probe_version"].as_u64() {
        return u32::try_from(v).ok();
    }
    c["label"].as_str()?.strip_prefix(prefix)?.strip_prefix("probe-v")?.parse().ok()
}

/// Prepares a same-authority probe cut, or says why not. Refuses a status
/// whose authority is not the tracked one or is superseded, an authority
/// with no cohort or no reference chain, an open probe cut, a promotion
/// pending in a cohort's transition, a version at or below one already
/// built, and references that do not match the chain's shape (a predecessor
/// exactly when the chain has one). The eval Worker checks all of these
/// again, and the references exactly, when the plan is queued.
pub fn prepare(i: CutInputs<'_>) -> Result<Cut, String> {
    let deployed = DeployedAuthority::from_status(i.status).map_err(|e| format!("status: {e}"))?;
    if deployed.name != i.tracked.name() {
        return Err(format!("the status names {}, the tree tracks {}", deployed.name, i.tracked.name()));
    }
    if let Some(by) = &deployed.superseded_by {
        return Err(format!("{} is superseded by {by}", deployed.name));
    }
    let tracked_preds: Vec<String> = i.tracked.predecessors().into_iter().map(str::to_owned).collect();
    if deployed.first_cohort != i.tracked.first_cohort() || deployed.predecessors != tracked_preds {
        return Err("the status's first cohort or predecessors differ from the tree's".into());
    }
    let st = if i.status["eval_cohort"].is_object() { &i.status["eval_cohort"] } else { i.status };
    let cohorts = st["cohorts"].as_array().ok_or("status: no cohorts")?;
    if cohorts.is_empty() {
        return Err("the authority holds no cohort: its first probe build is a chain record's, not a cut".into());
    }
    let prefix = i.tracked.label_prefix().unwrap_or_else(|| "testnet-".to_owned());
    let mut highest = 0u64;
    let mut built = 0u32;
    let mut open = Vec::new();
    for c in cohorts {
        let label = c["label"].as_str().ok_or("status: a cohort without a label")?;
        let number = c["cohort"].as_u64().ok_or("status: a cohort without a number")?;
        let phase = c["phase"].as_str().unwrap_or("");
        highest = highest.max(number);
        if c["reward_v1"]["probe_build"] == true || label.starts_with(&format!("{prefix}probe-v")) {
            built = built.max(entry_probe_version(c, &prefix).unwrap_or(0));
        }
        let closed = matches!(phase, "closed" | "abandoned");
        if c["reward_v1"]["probe_cut"] == true && !closed {
            return Err(format!("probe cut {label} is still open ({phase})"));
        }
        if phase == "transition" && !c["reward_v1"]["promoted"].is_null() {
            return Err(format!("{label} holds a pending promotion: cut after its close"));
        }
        if !closed {
            open.push(json!({"label": label, "cohort": number, "phase": phase}));
        }
    }
    let version = match i.probe_version {
        Some(v) if v > built => v,
        Some(v) => return Err(format!("probe version {v} is not above the highest built here ({built})")),
        None => built.checked_add(1).ok_or("probe version")?,
    };
    let chain = &st["reward"]["references"];
    if !chain.is_object() || chain["current_content"].is_null() {
        return Err("status: no reference chain (reward.references)".into());
    }
    match (chain["predecessor_content"].is_null(), &i.previous_reference) {
        (true, Some(_)) => return Err("the chain has no predecessor: give no previous reference".into()),
        (false, None) => return Err("the chain has a predecessor: give it as the previous reference".into()),
        _ => {}
    }
    let cohort = highest.saturating_add(1).max(i.tracked.first_cohort());
    let label = format!("{prefix}probe-v{version}");
    let mut plan = i.probe_template;
    let o = plan.as_object_mut().ok_or("probe template: not an object")?;
    o.insert("schema".into(), json!(PROBE_PLAN_SCHEMA));
    o.insert("cohort".into(), json!(cohort));
    o.insert("label".into(), json!(label));
    o.insert("reference".into(), i.reference.clone());
    o.insert("previous_reference".into(), i.previous_reference.clone().unwrap_or(Value::Null));
    o.insert("entrants".into(), json!([]));
    o.insert("allow_list".into(), json!([]));
    o.remove("admission");
    let v = o.get_mut("reward_v1").and_then(Value::as_object_mut).ok_or("probe template: no reward_v1 block")?;
    v.insert("probe_version".into(), json!(version));
    v.insert("probes".into(), json!([]));
    v.insert("task_order".into(), json!(TASK_ORDER_SEEDED_RANK));
    v.insert("probe_cut".into(), json!(true));
    if let Some(c) = v.get_mut("canary").and_then(Value::as_object_mut) {
        c.insert("drill".into(), json!([]));
    }
    let bytes = serde_json::to_vec_pretty(&plan).map_err(|e| e.to_string())?;
    let parsed = CohortPlan::parse(&bytes, i.ceilings).map_err(|e| format!("the coordinator refuses this plan: {}", e.0))?;
    i.tracked.admits(&parsed.label, parsed.cohort).map_err(|e| format!("the authority refuses this plan: {e}"))?;
    if !parsed.is_probe_cut() {
        return Err("the plan is not a probe cut".into());
    }
    // The Worker refuses a new plan without its generation pause bound
    // (`engine::admit`).
    if parsed.fanout.as_ref().and_then(|f| f.generation.failures_before_pause).is_none() {
        return Err("probe template: fanout.generation.failures_before_pause (required in a new plan)".into());
    }
    let record = json!({
        "schema": PROBE_CUT_SCHEMA,
        "authority": i.tracked.name(),
        "first_cohort": i.tracked.first_cohort(),
        "label_prefix": prefix,
        "cohort": cohort,
        "label": label,
        "probe_version": version,
        "probe_record_key": format!("{}/record.json", i.tracked.probe_prefix(version)),
        "probe_plan_sha256": crate::eval::engine::sha_hex(&bytes),
        "reference": i.reference,
        "previous_reference": i.previous_reference,
        "chain": chain.clone(),
        "observed_epoch": st["observed"]["epoch"]["index"].clone(),
        "open_cohorts": open,
    });
    Ok(Cut { plan: bytes, record })
}

/// The `reward_v1.probes` entry a reinstalled template names for a cut's
/// version: the version, size and key of the cut's probe record and the
/// SHA-256 of the record bytes read back from the eval bucket. Refuses a
/// record of another version or of another size than it lists.
pub fn template_probe(cut: &Value, record_bytes: &[u8]) -> Result<Value, String> {
    if cut["schema"] != PROBE_CUT_SCHEMA {
        return Err("not a probe cut record".into());
    }
    let record: Value = serde_json::from_slice(record_bytes).map_err(|e| format!("record: {e}"))?;
    let version = record["version"].as_u64().ok_or("record: version")?;
    if Some(version) != cut["probe_version"].as_u64() {
        return Err("the record is not the cut's probe version".into());
    }
    let size = record["size"].as_u64().ok_or("record: size")?;
    if record["docs"].as_array().map(Vec::len) != usize::try_from(size).ok() {
        return Err("record: its documents are not its size".into());
    }
    Ok(json!({"version": version, "size": size, "key": cut["probe_record_key"].clone(),
        "sha256": crate::eval::engine::sha_hex(record_bytes)}))
}

//! The standing plan template and the cohort calendar.
//!
//! The operator installs one standing template: a reward-v1 (v3) plan with an
//! `admission` block and no entrants. The coordinator then opens every cohort
//! itself: when no cohort is queued and the latest one has reached its
//! transition (or closed or was abandoned), it queues the next one from the
//! template, numbered after the last, labelled `<label_prefix>-<cohort>`. The
//! engine opens a queued cohort at the first epoch boundary after the
//! previous one closes, so cohorts run back to back on tempo boundaries:
//! cohort `N` opens at epoch `E_N`, admits until at least
//! `E_N + admission_min`, and closes at or after `E_N + K`, where cohort
//! `N + 1` opens.
//!
//! The next cohort names the references its seal will meet: the chain as
//! `reference_state` holds it, after the promotion of a cohort still in its
//! transition (decided at its packaging, applied at its close, which comes
//! before the next cohort opens); the template's own references only while
//! no chain exists. A reinstalled template never resets the chain.
//!
//! A template is checked when it is installed, at its own roster cap and
//! with a predecessor reference (as after a promotion): the plan validates
//! with `roster_cap` admitted entrants, its scoring budget covers their
//! probe-inclusive upper bound, its package step fits the memory model
//! (`arena_routes::package_fits`), and every receipt it can record (every
//! scored document at every attempt, `CohortPlan::receipt_bound`) fits one
//! receipt index (`plan::receipt_capacity`, the codec's sequence bound).
//! That is also what a native validator can hold: its bundle schema v3
//! names the receipt indexes and chunks of a target's cohort and of its
//! carrier by reference instead of embedding them, so the validators'
//! 64 MiB file bound (`VALIDATOR_BUNDLE_BYTES`) limits only the embedded
//! records. Schema v2 embeds the receipts hex-encoded; [`capacity`] reports
//! whether that form would still fit ([`embedded_bundle_bytes`]). The roster
//! cap a template can take therefore follows from its shape (n, the probe
//! and audit draws, `max_attempts`) and the protocol's bounds, never from a
//! tuned figure. It runs the testnet plan defaults of the
//! 2026-11-01 ruling: one scoring lane per artifact (`lane_docs >= n`, so
//! W = 1), no scoring pre-start, and the incentive rule with its provisional
//! gates flag. Installing a template never changes a queued or running
//! cohort.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)
)]

use crate::eval::engine::{self, EvalConfig, EvalError, R, i, opt_int, s};
use crate::eval::plan::{ArtifactJson, CohortPlan, IdentityJson, PLAN_SCHEMA_V3};
use crate::sql::{Sql, bind_u64, int, text};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const TEMPLATE_SCHEMA: &str = "slop_ninja_eval_cohort_template_v1";

/// A standing template as installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub schema: String,
    /// Cohort labels are `<label_prefix>-<cohort>`.
    pub label_prefix: String,
    /// The plan every opened cohort runs; its `cohort`, `label` and
    /// `entrants` are set per cohort, and its references follow the chain.
    pub plan: CohortPlan,
}

/// `(jobs, documents per job)` of a plan's package step: its jobs and the
/// documents admission counts for them (as `engine::admit` does), spread
/// over the jobs.
pub fn package_shape(plan: &CohortPlan) -> (u64, u64) {
    let jobs = (plan.jobs() as u64).max(1);
    (jobs, plan.scored_docs().div_ceil(jobs))
}

/// The most tasks one job of `plan` scores: an entrant's batch, probe and
/// audit tasks under reward-v1 (a reference job scores the batch alone),
/// else the batch.
pub fn max_job_tasks(plan: &CohortPlan) -> u64 {
    match &plan.reward_v1 {
        Some(v) if !plan.is_probe_build() => {
            let probe: u64 = v.probes.iter().map(|p| u64::from(p.size)).sum();
            u64::from(plan.n) + probe + u64::from(plan.audit_humans()) + u64::from(plan.audit_b_humans())
        }
        Some(v) => v
            .probes
            .iter()
            .map(|p| u64::from(p.size))
            .sum::<u64>()
            .max(u64::from(plan.n)),
        None => u64::from(plan.n),
    }
}

/// Whether a plan's package step fits the memory model.
pub fn fits(plan: &CohortPlan) -> bool {
    let (jobs, docs) = package_shape(plan);
    crate::arena_routes::package_fits(jobs, docs, max_job_tasks(plan))
}

/// The bytes a bundle schema v2 (receipts embedded) needs for the receipts
/// when its target's cohort and the cohort carrying it are both `plan`:
/// twice the protocol's [`receipt_bundle_bytes`] at the plan's receipt bound
/// (every scored document at every attempt). Schema v3 names them by
/// reference and needs none of this inside the file.
///
/// [`receipt_bundle_bytes`]: slop_ninja_protocol::target_bundle::receipt_bundle_bytes
pub fn embedded_bundle_bytes(plan: &CohortPlan) -> Option<u64> {
    slop_ninja_protocol::target_bundle::receipt_bundle_bytes(plan.receipt_bound()).ok()?.checked_mul(2)
}

/// A synthetic full roster for the template checks: `cap` distinct
/// admitted-entrant records in position order (never stored).
fn full_roster(plan: &CohortPlan, cap: u16) -> Vec<ArtifactJson> {
    (0..cap)
        .map(|n| {
            let mut a = plan.reference.clone();
            let tag = slop_ninja_protocol::domain_hash("slop_ninja.eval/template-check/v1", &n.to_le_bytes()).0;
            a.owner = IdentityJson {
                uid: n.saturating_add(1),
                hotkey: format!("0x{}", hex::encode(tag)),
                registration_block: 1,
            };
            a.package_sha256 =
                hex::encode(slop_ninja_protocol::domain_hash("slop_ninja.eval/template-check/package/v1", &tag).0);
            a.commitment_block = 2;
            a.opening_nonce = Some(hex::encode(tag));
            a.extrinsic_index = Some(u32::from(n));
            a.object_key = Some(format!("admission/template-check/{n}.object"));
            a
        })
        .collect()
}

/// Checks a template at its roster cap and returns it.
pub fn check(cfg: &EvalConfig, bytes: &[u8]) -> R<Template> {
    let t: Template = serde_json::from_slice(bytes).map_err(|_| EvalError::Plan("template json"))?;
    let e = |c: bool, what: &'static str| if c { Ok(()) } else { Err(EvalError::Plan(what)) };
    e(t.schema == TEMPLATE_SCHEMA, "template schema")?;
    e(
        !t.label_prefix.is_empty()
            && t.label_prefix.len() <= 64
            && t.label_prefix.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "template label_prefix",
    )?;
    e(t.plan.schema == PLAN_SCHEMA_V3, "template plan (v3)")?;
    e(t.plan.entrants.is_empty(), "template entrants (admission fills them)")?;
    let a = t
        .plan
        .admission
        .as_ref()
        .ok_or(EvalError::Plan("template admission block"))?;
    // The outside-entrant rule compares exact content digests, owning
    // coldkeys and hotkeys; a near copy of a published artifact is caught
    // only by the copy gate at scoring, after the draw. A standing template
    // therefore never unlocks fresh stock: cohorts with fresh stock are
    // operator plans until a pre-draw similarity probe exists.
    e(a.fresh_pool.is_none(), "template admission.fresh_pool (none in a standing template)")?;
    // The testnet plan defaults (ruling of 2026-11-01): one scoring lane per
    // artifact, no pre-start, and the incentive rule the manifest binds.
    let f = t.plan.fanout.as_ref().ok_or(EvalError::Plan("template fanout"))?;
    e(
        f.score.lane_docs >= t.plan.n,
        "template fanout.score.lane_docs (at least n: one lane per artifact)",
    )?;
    e(
        f.score.prestart_lead_ms == 0,
        "template fanout.score.prestart_lead_ms (0: no pre-start)",
    )?;
    let incentive = t
        .plan
        .reward_v1
        .as_ref()
        .and_then(|v| v.incentive.as_ref())
        .ok_or(EvalError::Plan("template reward_v1.incentive (the D11 rule)"))?;
    e(
        incentive.provisional_gates || cfg.chain.genesis != slop_ninja_protocol::TESTNET_GENESIS,
        "template reward_v1.incentive.provisional_gates (testnet gate thresholds are provisional)",
    )?;
    // The plan as admission leaves it at the cap, with a predecessor (as
    // after a promotion, so two references score): it validates, its budget
    // covers that roster, and its package step fits the memory model.
    let mut full = t.plan.clone();
    full.label = format!("{}-{}", t.label_prefix, u64::MAX);
    // The calendar numbers a named authority's cohorts from its first one
    // (`due`), so the template is checked at a number it can open.
    full.cohort = full.cohort.max(cfg.authority.first_cohort());
    full.entrants = full_roster(&t.plan, a.roster_cap);
    if full.previous_reference.is_none() {
        let mut p = t.plan.reference.clone();
        p.package_sha256 = hex::encode(
            slop_ninja_protocol::domain_hash(
                "slop_ninja.eval/template-check/predecessor/v1",
                p.package_sha256.as_bytes(),
            )
            .0,
        );
        p.manifest_key = format!("artifacts/packages/{}.json", p.package_sha256);
        full.previous_reference = Some(p);
    }
    let full_bytes = serde_json::to_vec(&full).map_err(|_| EvalError::Plan("template json"))?;
    let (plan, _) = engine::admit(cfg, &full_bytes)?;
    e(
        fits(&plan),
        "arena_capacity: the package step at the template's roster cap exceeds the memory model",
    )?;
    Ok(t)
}

/// The template's roster cap and what it needs at that cap, for the install
/// reply and the operator's template tool: receipts per cohort against the
/// receipt index bound, and the bytes a schema v2 validator bundle would
/// embed for a target and its carrier against the validators' file bound
/// (beyond it, validators hold the receipts through bundle schema v3).
pub fn capacity(t: &Template) -> Value {
    let mut full = t.plan.clone();
    let cap = t.plan.admission.as_ref().map_or(0, |a| a.roster_cap);
    full.entrants = full_roster(&t.plan, cap);
    if full.previous_reference.is_none() {
        full.previous_reference = Some(t.plan.reference.clone());
    }
    let embedded = embedded_bundle_bytes(&full);
    let bound = slop_ninja_protocol::target_bundle::VALIDATOR_BUNDLE_BYTES;
    json!({"roster_cap": cap, "receipts_per_cohort": full.receipt_bound(),
        "receipt_index_bound": slop_ninja_protocol::MAX_SEQUENCE_LEN,
        "embedded_bundle_receipt_bytes": embedded, "bundle_bound": bound,
        "embedded_bundle_fits": embedded.is_some_and(|b| b <= bound),
        "validator_bundle_schema": if embedded.is_some_and(|b| b <= bound) { "v2 or v3" } else { "v3" }})
}

/// Installs (or replaces) the standing template. A reference chain that
/// exists must resolve to a stored artifact record (the calendar names it);
/// the reply says which references the next cohort names.
pub fn install(sql: &impl Sql, cfg: &EvalConfig, bytes: &[u8], now_ms: u64) -> R<Value> {
    let t = check(cfg, bytes)?;
    let (next_ref, next_prev, source) = references(sql, &t)?;
    let text = std::str::from_utf8(bytes).map_err(|_| EvalError::Plan("template utf-8"))?;
    let sha = engine::sha_hex(bytes);
    sql.exec(
        "INSERT INTO standing_template (id, template, sha256, installed_ms, paused, first_cohort) VALUES (1, ?, ?, ?, 0, ?)
         ON CONFLICT (id) DO UPDATE SET template = excluded.template, sha256 = excluded.sha256,
            installed_ms = excluded.installed_ms, paused = 0, last_error = NULL, first_cohort = excluded.first_cohort",
        &[s(text), s(&sha), bind_u64(now_ms)?, bind_u64(cfg.authority.first_cohort())?],
    )?;
    Ok(json!({"installed": true, "sha256": sha, "label_prefix": t.label_prefix,
        "roster_cap": t.plan.admission.as_ref().map(|a| a.roster_cap), "capacity": capacity(&t),
        "admission_window": admission_window(sql)?,
        "next_references": {"source": source, "reference": next_ref.package_sha256,
            "previous_reference": next_prev.map(|p| p.package_sha256),
            "template_reference_followed": source == "template"}}))
}

/// Pauses or resumes opening cohorts from the template.
pub fn set_paused(sql: &impl Sql, paused: bool) -> R<bool> {
    Ok(!sql
        .exec(
            "UPDATE standing_template SET paused = ? WHERE id = 1 RETURNING id",
            &[i(i64::from(paused))],
        )?
        .is_empty())
}

/// The installed template's plan, if any.
pub fn template(sql: &impl Sql) -> R<Option<CohortPlan>> {
    Ok(stored(sql)?.map(|(t, _)| t.plan))
}

fn stored(sql: &impl Sql) -> R<Option<(Template, bool)>> {
    let rows = sql.exec("SELECT template, paused FROM standing_template WHERE id = 1", &[])?;
    let Some(r) = rows.first() else { return Ok(None) };
    let t: Template = serde_json::from_str(text(r, 0)?).map_err(|_| EvalError::State("stored template"))?;
    Ok(Some((t, int(r, 1)? != 0)))
}

/// An artifact record as a reference: scored as a reference, never as an
/// entrant.
fn as_reference(mut a: ArtifactJson) -> ArtifactJson {
    a.opening_nonce = None;
    a.extrinsic_index = None;
    a.object_key = None;
    a
}

/// The stored artifact record whose content digest is `content`: a v3
/// cohort's reference, predecessor or entrant. Two indexed reads, whatever
/// the number of cohorts: the cohort that published the content (a promoted
/// entrant is published at its cohort's close), else the newest v3 cohort
/// (every cohort names the chain's current reference and its predecessor).
fn resolve(sql: &impl Sql, content: &str) -> R<Option<ArtifactJson>> {
    let mut rows = sql.exec("SELECT seq FROM published_contents WHERE content = ?", &[s(content)])?;
    rows.extend(sql.exec("SELECT seq FROM cohorts WHERE v3 IS NOT NULL ORDER BY seq DESC LIMIT 1", &[])?);
    for r in &rows {
        let seq = int(r, 0)?;
        let st = crate::eval::v3::state(sql, seq)?;
        let c = engine::cohort(sql, seq)?;
        if st.reference.is_some_and(|d| d.content.0.to_string() == content) {
            return Ok(Some(as_reference(c.plan.reference)));
        }
        if st.predecessor.is_some_and(|d| d.content.0.to_string() == content)
            && let Some(p) = c.plan.previous_reference
        {
            return Ok(Some(as_reference(p)));
        }
        if let Some(n) = st.entrants.iter().position(|d| d.content.0.to_string() == content)
            && let Some(a) = c.plan.entrants.get(n)
        {
            return Ok(Some(as_reference(a.clone())));
        }
    }
    Ok(None)
}

/// The references the next cohort names, and where they come from: a
/// promotion in the cohort now in its transition (`pending_promotion`), else
/// the chain `reference_state` holds (`chain`), else the template's own
/// (`template`, while no chain exists). A chain that names a record this
/// store does not hold is an error (the template cannot stand in for it).
fn references(sql: &impl Sql, t: &Template) -> R<(ArtifactJson, Option<ArtifactJson>, &'static str)> {
    let rows = sql.exec(
        "SELECT seq FROM cohorts WHERE phase = 'transition' AND v3 IS NOT NULL ORDER BY seq DESC LIMIT 1",
        &[],
    )?;
    if let Some(r) = rows.first() {
        let seq = int(r, 0)?;
        let st = crate::eval::v3::state(sql, seq)?;
        if let Some(p) = st.promoted {
            let c = engine::cohort(sql, seq)?;
            let promoted = c
                .plan
                .entrants
                .get(p)
                .cloned()
                .ok_or(EvalError::State("promoted entrant"))?;
            return Ok((
                as_reference(promoted),
                Some(as_reference(c.plan.reference)),
                "pending_promotion",
            ));
        }
    }
    let chain = sql.exec("SELECT current, predecessor FROM reference_state WHERE id = 1", &[])?;
    let Some(r) = chain.first() else {
        return Ok((t.plan.reference.clone(), t.plan.previous_reference.clone(), "template"));
    };
    let current = resolve(sql, text(r, 0)?)?.ok_or(EvalError::Plan(
        "template_reference_unresolved: the reference chain names an artifact no stored cohort records",
    ))?;
    let previous = match engine::opt_text(r, 1) {
        Some(p) => Some(resolve(sql, &p)?.ok_or(EvalError::Plan(
            "template_reference_unresolved: the reference chain's predecessor is not recorded",
        ))?),
        None => None,
    };
    Ok((current, previous, "chain"))
}

/// The next cohort's plan bytes when the calendar says one is due: a
/// template is installed and not paused, nothing is queued, and the latest
/// cohort is in its transition, closed or abandoned (or there is none). A
/// retired main authority (superseded, or not the one the deployment names)
/// opens nothing. A named authority numbers from its first cohort
/// (`config::Authority`), and its template's labels carry its prefix (the
/// template check refuses any other).
pub fn due(sql: &impl Sql, cfg: &EvalConfig) -> R<Option<(String, Vec<u8>)>> {
    if cfg.authority.is_retired() {
        return Ok(None);
    }
    due_any(sql, cfg.authority.first_cohort())
}

/// Whether the calendar has a cohort to open under `cfg`, as a pure read:
/// the conditions of [`due`] without naming the references, so it writes
/// nothing and reads no cohort records. Status and the step's reason to run
/// use this; only the calendar's own step ([`due`]) records a reference
/// error and pauses the template.
pub fn ready(sql: &impl Sql, cfg: &EvalConfig) -> R<bool> {
    if cfg.authority.is_retired() {
        return Ok(false);
    }
    Ok(template_ready(sql)?.is_some())
}

/// The installed template when it is not paused, nothing is queued and the
/// latest cohort is in its transition, closed or abandoned (or there is none).
fn template_ready(sql: &impl Sql) -> R<Option<Template>> {
    let Some((t, paused)) = stored(sql)? else {
        return Ok(None);
    };
    if paused {
        return Ok(None);
    }
    let rows = sql.exec("SELECT phase FROM cohorts ORDER BY seq DESC LIMIT 1", &[])?;
    if let Some(r) = rows.first()
        && !matches!(text(r, 0)?, "transition" | "closed" | "abandoned")
    {
        return Ok(None);
    }
    Ok(Some(t))
}

/// [`due`] without the authority check, numbering from `first_cohort`.
pub fn due_any(sql: &impl Sql, first_cohort: u64) -> R<Option<(String, Vec<u8>)>> {
    let Some(t) = template_ready(sql)? else {
        return Ok(None);
    };
    let next = next_number(sql, first_cohort)?;
    let (reference, previous, _) = match references(sql, &t) {
        Ok(r) => r,
        // The calendar cannot name the chain's references: it pauses with
        // the reason rather than failing every step.
        Err(EvalError::Plan(code)) => {
            note_error(sql, code)?;
            set_paused(sql, true)?;
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let mut plan = t.plan.clone();
    plan.cohort = next;
    plan.label = format!("{}-{}", t.label_prefix, next);
    plan.reference = reference;
    plan.previous_reference = previous;
    plan.entrants = Vec::new();
    let bytes = serde_json::to_vec(&plan).map_err(|_| EvalError::State("next plan"))?;
    Ok(Some((plan.label, bytes)))
}

/// Records why the calendar could not queue the next cohort (for status).
pub fn note_error(sql: &impl Sql, code: &str) -> R<()> {
    sql.exec("UPDATE standing_template SET last_error = ? WHERE id = 1", &[s(code)])?;
    Ok(())
}

/// The number the calendar gives the next cohort: one past every cohort this
/// store holds, never below the authority's first cohort (`first_cohort`).
fn next_number(sql: &impl Sql, first_cohort: u64) -> R<u64> {
    let max = sql.exec("SELECT COALESCE(MAX(cohort), 0) FROM cohorts", &[])?;
    let max = u64::try_from(max.first().map_or(Ok(0), |r| int(r, 0))?).unwrap_or(0);
    Ok(max.saturating_add(1).max(first_cohort))
}

/// The next cohort and its earliest opening epoch: a queued cohort, else the
/// one the template opens next, numbered as [`due_any`] numbers it (from the
/// authority's first cohort, recorded with the template at its install).
/// The earliest opening is the first boundary after the cohort before it can
/// enter its transition (`opened + K - transition`), and never an epoch
/// already past.
pub fn next(sql: &impl Sql) -> R<Option<(u64, Option<u64>)>> {
    let last = sql.exec("SELECT cohort, phase, opened_epoch, k_tempos, transition_tempos FROM (
            SELECT cohort, phase, opened_epoch, json_extract(plan, '$.k_tempos') AS k_tempos,
                json_extract(plan, '$.transition_tempos') AS transition_tempos, seq FROM cohorts) ORDER BY seq DESC LIMIT 2", &[])?;
    let now = engine::observed(sql)?.epoch.map(|e| e.index);
    let earliest = |row: Option<&crate::sql::Row>| -> Option<u64> {
        let after_now = now.map(|n| n.saturating_add(1));
        let Some(row) = row else { return after_now };
        let boundary = match text(row, 1).ok()? {
            "transition" | "closed" | "abandoned" => None,
            _ => {
                let opened = u64::try_from(opt_int(row, 2)?).ok()?;
                let k = u64::try_from(opt_int(row, 3)?).ok()?;
                let t = u64::try_from(opt_int(row, 4).unwrap_or(0)).ok()?;
                Some(opened.saturating_add(k.saturating_sub(t)).saturating_add(1))
            }
        };
        match (boundary, after_now) {
            (Some(b), Some(n)) => Some(b.max(n)),
            (b, n) => b.or(n),
        }
    };
    if let Some(r) = last.first()
        && text(r, 1)? == "queued"
    {
        return Ok(Some((u64::try_from(int(r, 0)?).unwrap_or(0), earliest(last.get(1)))));
    }
    if stored(sql)?.is_some_and(|(_, paused)| !paused) {
        let first = sql.exec("SELECT COALESCE(first_cohort, 0) FROM standing_template WHERE id = 1", &[])?;
        let first = u64::try_from(first.first().map_or(Ok(0), |r| int(r, 0))?).unwrap_or(0);
        return Ok(Some((next_number(sql, first)?, earliest(last.first()))));
    }
    Ok(None)
}

/// The calendar for status and the public snapshot: the current cohort's
/// tempo boundaries and the next cohort, in subnet epoch indices.
pub fn view(sql: &impl Sql) -> R<Value> {
    let rows = sql.exec(
        "SELECT cohort, label, phase, opened_epoch, admission_closed_epoch, closed_epoch,
            json_extract(plan, '$.k_tempos'), json_extract(plan, '$.admission_tempos'), json_extract(plan, '$.transition_tempos')
         FROM cohorts ORDER BY seq DESC LIMIT 1",
        &[],
    )?;
    // The coordinator admission window of the current cohort: `open` while
    // it admits (sessions accepted), `closing` once the window has closed
    // and the roster waits for its freeze (the phase stays `admission`), and
    // `false`/`false` for a cohort without coordinator admission.
    let window = |cohort: Option<i64>| -> R<(bool, bool)> {
        let Some(c) = cohort else { return Ok((false, false)) };
        let w = sql.exec(
            "SELECT state FROM adm_cohorts WHERE cohort = ? ORDER BY seq DESC LIMIT 1",
            &[i(c)],
        )?;
        let state = w.first().and_then(|r| engine::opt_text(r, 0));
        Ok((state.as_deref() == Some("admitting"), state.as_deref() == Some("closing")))
    };
    let (window_open, window_closing) = window(rows.first().and_then(|r| opt_int(r, 0)))?;
    let current = rows.first().map(|r| {
        let opened = opt_int(r, 3);
        json!({"cohort": int(r, 0).ok(), "label": text(r, 1).ok(), "phase": text(r, 2).ok(), "opened_epoch": opened,
            "window_open": window_open, "window_closing": window_closing,
            "planned_cutoff_epoch": opened.zip(opt_int(r, 7)).map(|(o, a)| o + a),
            "admission_closed_epoch": opt_int(r, 4), "earliest_close_epoch": opened.zip(opt_int(r, 6)).map(|(o, k)| o + k),
            "closed_epoch": opt_int(r, 5), "k_tempos": opt_int(r, 6), "admission_tempos": opt_int(r, 7),
            "transition_tempos": opt_int(r, 8)})
    });
    let template = sql.exec(
        "SELECT sha256, paused, last_error FROM standing_template WHERE id = 1",
        &[],
    )?;
    let template = template.first().map(|r| json!({"sha256": text(r, 0).ok(), "paused": int(r, 1).ok() == Some(1), "last_error": crate::eval::engine::opt_text(r, 2)}));
    let next = next(sql)?.map(|(c, e)| json!({"cohort": c, "earliest_open_epoch": e}));
    Ok(json!({"rule": "back_to_back_on_tempo_boundaries/v1", "current": current, "next": next, "template": template,
        "admission_window": admission_window(sql)?}))
}

/// Nominal block interval, for the operator's display of a window in
/// minutes only (nothing is scheduled or refused on it).
const NOMINAL_BLOCK_SECS: u64 = 12;

/// The admission window a miner has per cohort: the standing template's
/// `admission_tempos`, and the last closed window as measured on chain
/// (`cutoff_block - open_block`, in blocks and nominal minutes). Commitments
/// before a window opens are refused, so commit, logging and the upload of
/// the largest object must fit inside it.
pub fn admission_window(sql: &impl Sql) -> R<Value> {
    let tempos = stored(sql)?.map(|(t, _)| t.plan.admission_tempos);
    let last = sql.exec(
        "SELECT cohort, open_block, cutoff_block FROM adm_cohorts WHERE cutoff_block IS NOT NULL ORDER BY seq DESC LIMIT 1",
        &[],
    )?;
    let measured = last.first().and_then(|r| {
        let open = u64::try_from(opt_int(r, 1)?).ok()?;
        let cutoff = u64::try_from(opt_int(r, 2)?).ok()?;
        let blocks = cutoff.saturating_sub(open);
        Some(json!({"cohort": opt_int(r, 0), "blocks": blocks,
            "nominal_minutes": blocks.saturating_mul(NOMINAL_BLOCK_SECS) / 60}))
    });
    Ok(json!({"template_admission_tempos": tempos, "last_measured": measured}))
}

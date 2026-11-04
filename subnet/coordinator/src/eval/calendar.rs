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
//! signed receipt index (`plan::receipt_capacity`): the tighter of the
//! codec's sequence bound and the entries whose record fits the protocol's
//! 64 MiB signed-record limit (`target_bundle::receipt_index_capacity`,
//! 69 bytes an entry), so the coordinator can sign the index it packages.
//! A native validator holds that too: its bundle schema v3 names the
//! receipt indexes and chunks of a target's cohort and of its carrier by
//! reference instead of embedding them, so the validators' 64 MiB file
//! bound (`VALIDATOR_BUNDLE_BYTES`) limits only the embedded records, and
//! its fetch session reads the two signed indexes within their own
//! allowance (`target_bundle::validator_receipt_index_bytes`, two indexes
//! at that capacity) beside the file bound. Schema v2 embeds the receipts
//! hex-encoded; [`capacity`] reports every bound and whether that form
//! would still fit ([`embedded_bundle_bytes`]). The roster
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
use slop_ninja_admission_wire::json::NextCohort;

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
    // Held-out exposure finding L4: a cohort opened from a standing template
    // takes its task indices from the seeded rank, so position predicts
    // neither a document's kind nor its set. The check runs at install: a
    // template already stored keeps the layout it names.
    e(
        t.plan.reward_v1.as_ref().is_some_and(super::plan::RewardV1Plan::seeded_task_order),
        "template reward_v1.task_order (seeded_rank_v1)",
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
/// reply and the operator's template tool: receipts per cohort against each
/// receipt bound (the codec's sequence bound, the entries one signed index
/// holds, the entries a validator session holds for a target and its
/// carrier, and the tightest of them, which the template check applies),
/// the signed index's bytes, and the bytes a schema v2 validator bundle
/// would embed for a target and its carrier against the validators' file
/// bound (beyond it, validators hold the receipts through bundle schema v3).
pub fn capacity(t: &Template) -> Value {
    let mut full = t.plan.clone();
    let cap = t.plan.admission.as_ref().map_or(0, |a| a.roster_cap);
    full.entrants = full_roster(&t.plan, cap);
    if full.previous_reference.is_none() {
        full.previous_reference = Some(t.plan.reference.clone());
    }
    let embedded = embedded_bundle_bytes(&full);
    let bound = slop_ninja_protocol::target_bundle::VALIDATOR_BUNDLE_BYTES;
    use slop_ninja_protocol::target_bundle as tb;
    // The largest count whose two signed indexes (target and carrier) fit
    // the validator session's receipt-index allowance.
    let session = (|| {
        let (base, one) = (tb::receipt_index_bytes(0).ok()?, tb::receipt_index_bytes(1).ok()?);
        Some((tb::validator_receipt_index_bytes().ok()? / 2).checked_sub(base)? / one.checked_sub(base)?)
    })();
    let sequence = u64::from(slop_ninja_protocol::MAX_SEQUENCE_LEN);
    json!({"roster_cap": cap, "receipts_per_cohort": full.receipt_bound(),
        "receipt_index_bound": crate::eval::plan::receipt_index_bound(),
        "receipt_bounds": {"sequence": sequence, "signed_record": tb::receipt_index_capacity().ok(),
            "validator_session": session},
        "receipt_index_bytes": tb::receipt_index_bytes(full.receipt_bound()).ok(),
        "validator_receipt_index_bytes": tb::validator_receipt_index_bytes().ok(),
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

/// The standing template's plan while it is unpaused (the plan the
/// calendar's next cohort opens with when none is queued).
pub fn active_template(sql: &impl Sql) -> R<Option<CohortPlan>> {
    Ok(stored(sql)?.filter(|(_, paused)| !paused).map(|(t, _)| t.plan))
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
    // A template installed before the generation pause bound was required
    // gives its cohorts the bound those builds compiled in, written into the
    // plan so the cohort's seal binds it.
    if let Some(f) = plan.fanout.as_mut() {
        f.generation.failures_before_pause.get_or_insert(crate::eval::plan::GEN_FAILURES_BEFORE_PAUSE_COMPILED);
    }
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

/// The next cohort: a queued cohort, else the one the template opens next,
/// numbered as [`due_any`] numbers it (from the authority's first cohort,
/// recorded with the template at its install); its admission window in
/// tempos (the queued plan's, else the template's); and its earliest
/// opening epoch, a lower bound that is always a later boundary than the
/// observed one.
///
/// The engine opens the next cohort only at the boundary that closes the one
/// before it (cohorts never overlap), so the earliest opening is that
/// cohort's earliest close ([`earliest_close`], in the tempos the
/// coordinator credits), or the next boundary when it has closed (or there
/// is none).
pub fn next(sql: &impl Sql) -> R<Option<NextCohort>> {
    let last = sql.exec(
        "SELECT cohort, phase, opened_epoch, packaged_epoch, published_epoch, json_extract(plan, '$.k_tempos'),
            json_extract(plan, '$.admission_tempos'), json_extract(plan, '$.transition_tempos'),
            json_extract(plan, '$.reward_v1') IS NOT NULL, opened_active, packaged_active, published_active
         FROM cohorts ORDER BY seq DESC LIMIT 2",
        &[],
    )?;
    let observed = engine::observed(sql)?;
    let now = observed.epoch.map(|e| e.index);
    let earliest = |row: Option<&crate::sql::Row>| -> Option<u64> {
        let after_now = now.map(|n| n.saturating_add(1));
        let Some(row) = row else { return after_now };
        match text(row, 1).ok()? {
            "closed" | "abandoned" => after_now,
            phase => {
                let u = |n: usize| opt_int(row, n).and_then(|v| u64::try_from(v).ok());
                let shape = CloseShape {
                    opened: u(2)?,
                    k: u(5)?,
                    admission: u(6).unwrap_or(0),
                    transition: u(7).unwrap_or(0),
                    v3: opt_int(row, 8) == Some(1),
                };
                let transition = phase == "transition";
                let packaged = if transition { u(3) } else { None };
                let credited = u(9).map(|opened| Credited {
                    now: observed.active_tempos,
                    opened,
                    packaged: if transition { u(10) } else { None },
                    published: u(11),
                });
                Some(earliest_close(&shape, packaged, u(4), now.unwrap_or(0), credited.as_ref()))
            }
        }
    };
    let tempos = |row: &crate::sql::Row| opt_int(row, 6).and_then(|v| u64::try_from(v).ok());
    if let Some(r) = last.first()
        && text(r, 1)? == "queued"
    {
        return Ok(Some(NextCohort {
            cohort: u64::try_from(int(r, 0)?).unwrap_or(0),
            earliest_open_epoch: earliest(last.get(1)),
            admission_tempos: tempos(r),
            ..Default::default()
        }));
    }
    if let Some((t, false)) = stored(sql)? {
        let first = sql.exec("SELECT COALESCE(first_cohort, 0) FROM standing_template WHERE id = 1", &[])?;
        let first = u64::try_from(first.first().map_or(Ok(0), |r| int(r, 0))?).unwrap_or(0);
        return Ok(Some(NextCohort {
            cohort: next_number(sql, first)?,
            earliest_open_epoch: earliest(last.first()),
            admission_tempos: Some(t.plan.admission_tempos),
            ..Default::default()
        }));
    }
    Ok(None)
}

/// A cohort's frozen tempo shape, for [`earliest_close`] (epoch indices).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloseShape {
    pub opened: u64,
    pub k: u64,
    pub admission: u64,
    pub transition: u64,
    /// A reward-v1 cohort: its transition counts from publication, the first
    /// boundary after its package is durable.
    pub v3: bool,
}

/// Credited tempos for [`earliest_close`]: the coordinator's count now
/// (`eval_obs.active_tempos`) and the cohort's own marks (`opened_active`,
/// and `packaged_active` and `published_active` once recorded). The engine's
/// phase minimums count these, and a boundary credits at most one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Credited {
    pub now: u64,
    pub opened: u64,
    pub packaged: Option<u64>,
    pub published: Option<u64>,
}

/// The earliest epoch at which a cohort that has not closed can close, by
/// the engine's close rule (`engine::advance`), at the observed epoch `now`:
/// `K` credited tempos after it opened, and `transition` credited tempos
/// after its publication (reward-v1, at a boundary after its package) or
/// its package (earlier plans). The package is durable at the earliest in
/// the current epoch or the one its admission window closes at. In epoch
/// indices (`packaged`, `published`), and, with `credited`, in the tempos
/// the coordinator still has to credit: each later boundary credits at most
/// one, so a gap the engine did not credit (an outage, a stale heartbeat)
/// moves the bound later by exactly the tempos still owed. Always after
/// `now`: a cohort still open once this epoch's boundary has been applied
/// closes at a later one (every plan has a transition of at least one
/// tempo). A lower bound: work that runs past a boundary, or later silence,
/// moves the close later, never earlier.
pub fn earliest_close(
    c: &CloseShape,
    packaged: Option<u64>,
    published: Option<u64>,
    now: u64,
    credited: Option<&Credited>,
) -> u64 {
    let pk = packaged.unwrap_or_else(|| now.max(c.opened.saturating_add(c.admission)));
    let from = if c.v3 {
        published.unwrap_or_else(|| pk.saturating_add(1).max(now))
    } else {
        pk
    };
    let by_epoch = c.opened.saturating_add(c.k).max(from.saturating_add(c.transition));
    let by_credit = credited.map_or(0, |a| {
        let owed = |target: u64| target.saturating_sub(a.now);
        let k = owed(a.opened.saturating_add(c.k));
        // Tempos still owed before the window can close (the package can be
        // durable in that epoch at the earliest).
        let cutoff = owed(a.opened.saturating_add(c.admission));
        let from = if c.v3 {
            match (published, a.published) {
                (Some(_), Some(pa)) => owed(pa.saturating_add(c.transition)),
                (Some(_), None) => 0,
                // Publication at a boundary after the package, the
                // transition from there.
                (None, _) => {
                    let package = if packaged.is_some() { 0 } else { cutoff };
                    package.saturating_add(1).saturating_add(c.transition)
                }
            }
        } else {
            match (packaged, a.packaged) {
                (Some(_), Some(pka)) => owed(pka.saturating_add(c.transition)),
                (Some(_), None) => 0,
                (None, _) => cutoff.saturating_add(c.transition),
            }
        };
        now.saturating_add(k.max(from))
    });
    by_epoch.max(by_credit).max(now.saturating_add(1))
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
    // `false`/`false` for a cohort without coordinator admission. While
    // closing, `freeze_wait` names what the freeze waits on.
    let window = |cohort: Option<i64>| -> R<(bool, bool, Option<&'static str>)> {
        let Some(c) = cohort else { return Ok((false, false, None)) };
        let w = sql.exec(
            "SELECT state, seq FROM adm_cohorts WHERE cohort = ? ORDER BY seq DESC LIMIT 1",
            &[i(c)],
        )?;
        let state = w.first().and_then(|r| engine::opt_text(r, 0));
        let closing = state.as_deref() == Some("closing");
        let wait = match w.first() {
            Some(r) if closing => crate::eval::admission::freeze_wait(sql, int(r, 1)?)?,
            _ => None,
        };
        Ok((state.as_deref() == Some("admitting"), closing, wait))
    };
    let (window_open, window_closing, freeze_wait) = window(rows.first().and_then(|r| opt_int(r, 0)))?;
    let current = rows.first().map(|r| {
        let opened = opt_int(r, 3);
        json!({"cohort": int(r, 0).ok(), "label": text(r, 1).ok(), "phase": text(r, 2).ok(), "opened_epoch": opened,
            "window_open": window_open, "window_closing": window_closing, "freeze_wait": freeze_wait,
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
    Ok(json!({"rule": "back_to_back_on_tempo_boundaries/v1", "current": current, "next": next(sql)?, "template": template,
        "admission_window": admission_window(sql)?}))
}

/// Nominal block interval. The coordinator schedules nothing on it: it
/// counts tempos and blocks. The operator's display of a window in minutes
/// uses it, and the operator-side window derivation
/// ([`derive_admission_tempos`]) takes it as its default block interval
/// (`WindowInputs::block_secs`), an assumption the tool prints with the
/// derivation.
pub const NOMINAL_BLOCK_SECS: u64 = 12;

/// A window of `tempos` tempos of `tempo_blocks` blocks each, in tempos,
/// blocks and nominal minutes (display only).
pub fn window_json(tempos: u64, tempo_blocks: u64) -> Value {
    let blocks = tempos.saturating_mul(tempo_blocks);
    json!({"tempos": tempos, "blocks": blocks, "nominal_minutes": blocks.saturating_mul(NOMINAL_BLOCK_SECS) / 60})
}

/// The admission window a miner has per cohort: the standing template's
/// `admission_tempos`, the last closed window as measured on chain
/// (`cutoff_block - open_block`, in blocks and nominal minutes, over the
/// epoch boundaries it spanned and the tempos its plan admitted for), and
/// the template's window at the blocks per tempo that measurement shows.
/// `blocks_per_tempo` is the window's blocks over its epoch span (from
/// boundary to boundary, so the chain's tempo interval), never over the
/// plan's tempos: a window the coordinator extended past tempos it did not
/// credit (an outage) spans more epochs than its plan's tempos, and
/// `extended` says so. Commitments before a window opens are refused, so
/// commit, logging and the upload of the largest object must fit inside it.
pub fn admission_window(sql: &impl Sql) -> R<Value> {
    let tempos = stored(sql)?.map(|(t, _)| t.plan.admission_tempos);
    let last = sql.exec(
        "SELECT a.cohort, a.open_block, a.cutoff_block, json_extract(c.plan, '$.admission_tempos'), c.opened_epoch,
            a.closed_at_epoch
         FROM adm_cohorts a JOIN cohorts c ON c.seq = a.seq
         WHERE a.cutoff_block IS NOT NULL ORDER BY a.seq DESC LIMIT 1",
        &[],
    )?;
    let measured = last.first().and_then(|r| {
        let open = u64::try_from(opt_int(r, 1)?).ok()?;
        let cutoff = u64::try_from(opt_int(r, 2)?).ok()?;
        let blocks = cutoff.saturating_sub(open);
        let u = |n: usize| opt_int(r, n).and_then(|v| u64::try_from(v).ok());
        let epochs = u(4).zip(u(5)).map(|(o, c)| c.saturating_sub(o)).filter(|e| *e > 0);
        let per_tempo = epochs.map(|e| blocks / e);
        Some((
            json!({"cohort": opt_int(r, 0), "blocks": blocks,
                "nominal_minutes": blocks.saturating_mul(NOMINAL_BLOCK_SECS) / 60, "epochs": epochs,
                "admission_tempos": u(3), "extended": epochs.zip(u(3)).map(|(e, t)| e > t),
                "blocks_per_tempo": per_tempo}),
            per_tempo,
        ))
    });
    let template_window = tempos.zip(measured.as_ref().and_then(|m| m.1)).map(|(t, b)| window_json(t, b));
    Ok(json!({"template_admission_tempos": tempos, "last_measured": measured.map(|m| m.0),
        "template_window": template_window}))
}

/// What a standing template's admission window is derived from
/// ([`derive_admission_tempos`]): measurements on the live deployment and
/// the block interval assumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowInputs {
    /// Blocks per tempo: the chain's boundary interval, as a window measured
    /// on chain shows it over its epoch span ([`admission_window`]'s
    /// `blocks_per_tempo`), never a window's blocks over its plan's tempos.
    pub tempo_blocks: u64,
    /// The block interval in seconds the derivation assumes (the tool's
    /// default is [`NOMINAL_BLOCK_SECS`]; it prints the value it used).
    pub block_secs: u64,
    /// A measured cohort's work after its cutoff: from the window's close
    /// to its package, target and receipts being durable.
    pub work_secs: u64,
    /// A measured submission: from the commitment to the session's
    /// `verified` answer (commit, finality, logging, upload, framing and
    /// the verdict run), for an object of `upload_bytes` bytes.
    pub upload_secs: u64,
    pub upload_bytes: u64,
    /// A measured scoring lane: from its cohort's scoring start to its end
    /// (cold path, model load and every chunk), scoring `score_docs`
    /// documents with an object of `score_bytes` bytes.
    pub score_secs: u64,
    pub score_docs: u64,
    pub score_bytes: u64,
    /// A measured cold path (container start, artifact stream and model
    /// load) of a lane loading an object of `score_bytes` bytes: an
    /// admission verdict is one such fresh instance, so the verdict queue at
    /// the cutoff is counted in it ([`WindowDerivation::verdict_queue_secs`]).
    pub cold_path_secs: u64,
}

/// A derived admission window and what it rests on (seconds at the assumed
/// block interval).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowDerivation {
    pub admission_tempos: u64,
    pub tempo_secs: u64,
    /// The measured submission scaled to the spec's largest object.
    pub largest_upload_secs: u64,
    /// One model load of the largest object at the plan's allowance.
    pub load_secs: u64,
    /// The measured scoring lane scaled to one entrant's documents
    /// (`CohortPlan::entrant_docs`) and to the spec's largest object.
    pub entrant_lane_secs: u64,
    /// One admission verdict of the largest object: the measured cold path
    /// scaled to its bytes, plus one model load of it at the plan's
    /// allowance (the measured cold path already holds a smaller load, so
    /// this counts the load twice, conservatively).
    pub verdict_secs: u64,
    /// Rounds of the cohort object's verdict step a full roster's sessions
    /// still verifying at the cutoff need: the roster over
    /// `STEP_READS_IN_FLIGHT` verdicts a round, rounded up.
    pub verdict_rounds: u64,
    /// `verdict_rounds x verdict_secs`: the freeze waits for every session
    /// verifying at the close (`admission::ready`).
    pub verdict_queue_secs: u64,
    /// The work after a cutoff at a full roster, bounded: the measured work,
    /// plus the largest submission's time (a session that completes at the
    /// cutoff is framed and verified after it, before the freeze), plus the
    /// verdict queue of a full roster completing near the cutoff, plus one
    /// model load of the largest object, plus one entrant's scoring lane of
    /// it (one scoring lane per artifact, all lanes in parallel within
    /// `MAX_SCORE_LANES`, which the template check holds the roster to). The
    /// measured work already holds its references' lane, so counting the
    /// entrant lane in full is conservative. Sessions verifying at the
    /// cutoff beyond the roster (one per registered hotkey) are not in it:
    /// they move the freeze, and the close, later, never skip a cohort, and
    /// `phase_overrun` reports a freeze still waiting at the package
    /// deadline.
    pub work_bound_secs: u64,
    /// Tempos the work after the cutoff is given: its bound at most half of
    /// them.
    pub work_tempos: u64,
}

/// The longest admission window a standing template can give each cohort
/// while a full cohort still closes `K` tempos after it opened: the cohort
/// period less the transition minimum and the tempos its work after the
/// cutoff needs (work in the tempo the window closes at, publication at the
/// next boundary, the transition counted from there; `engine::advance`).
/// Work is sized to at most half of its tempos (the design note's scoring
/// rule), and the largest object's submission must fit in at most half of
/// the window, so a miner who starts late in the window still finishes.
/// Every input is a measurement or the stated block interval, and every
/// bound follows from the plan; no figure is tuned. `roster` is the roster
/// the verdict queue is counted for: the template's roster cap, or a bound
/// on it (the deployment's ceiling); 0 counts no queue (the first
/// derivation's single post-cutoff verdict). Work that runs longer moves
/// the close by whole tempos and never skips or shortens a cohort.
pub fn derive_admission_tempos(
    plan: &CohortPlan,
    object_bytes: u64,
    roster: u64,
    m: &WindowInputs,
) -> Result<WindowDerivation, &'static str> {
    if m.tempo_blocks == 0 || m.block_secs == 0 || m.upload_bytes == 0 || m.score_docs == 0 || m.score_bytes == 0 {
        return Err("tempo_blocks, block_secs, upload_bytes, score_docs and score_bytes must be positive");
    }
    let tempo_secs = m.tempo_blocks.saturating_mul(m.block_secs);
    let scaled = u128::from(m.upload_secs) * u128::from(object_bytes);
    let largest_upload_secs =
        u64::try_from(scaled.div_ceil(u128::from(m.upload_bytes))).map_err(|_| "upload time overflows")?;
    let per_mib = plan
        .fanout
        .as_ref()
        .and_then(|f| f.score.load_ms_per_mib)
        .map_or(0, u64::from);
    let load_secs = object_bytes.div_ceil(1 << 20).saturating_mul(per_mib).div_ceil(1000);
    let lane = u128::from(m.score_secs) * u128::from(plan.entrant_docs()) * u128::from(object_bytes);
    let entrant_lane_secs = u64::try_from(lane.div_ceil(u128::from(m.score_docs) * u128::from(m.score_bytes)))
        .map_err(|_| "scoring time overflows")?;
    let cold = u128::from(m.cold_path_secs) * u128::from(object_bytes);
    let verdict_secs = u64::try_from(cold.div_ceil(u128::from(m.score_bytes)))
        .map_err(|_| "verdict time overflows")?
        .saturating_add(load_secs);
    let verdict_rounds = roster.div_ceil(crate::arena_routes::STEP_READS_IN_FLIGHT as u64);
    let verdict_queue_secs = verdict_rounds.saturating_mul(verdict_secs);
    let work_bound_secs = m
        .work_secs
        .saturating_add(largest_upload_secs)
        .saturating_add(verdict_queue_secs)
        .saturating_add(load_secs)
        .saturating_add(entrant_lane_secs);
    let work_tempos = work_bound_secs.saturating_mul(2).div_ceil(tempo_secs).max(1);
    let admission_tempos = plan
        .k_tempos
        .checked_sub(plan.transition_tempos)
        .and_then(|r| r.checked_sub(work_tempos))
        .filter(|a| *a >= 1)
        .ok_or("the cohort period leaves no admission window after the transition and the work")?;
    if largest_upload_secs.saturating_mul(2) > admission_tempos.saturating_mul(tempo_secs) {
        return Err("the largest object's submission does not fit in half of the window");
    }
    Ok(WindowDerivation {
        admission_tempos,
        tempo_secs,
        largest_upload_secs,
        load_secs,
        entrant_lane_secs,
        verdict_secs,
        verdict_rounds,
        verdict_queue_secs,
        work_bound_secs,
        work_tempos,
    })
}

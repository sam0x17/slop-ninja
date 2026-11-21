//! Working-set estimate for finalizing a reward package with a committed
//! chunked receipt index. Legacy packages and probe builds use their original
//! fits. Task vectors are charged once per assigned task, even if it failed.
use super::calendar::{max_job_tasks, package_shape};
use super::engine::{EvalError, R, SUMMARY_FIXED_BYTES};
use super::plan::CohortPlan;
use crate::arena_routes as routes;
use crate::sql::{Sql, SqlValue, uint};

pub fn compact(plan: &CohortPlan) -> bool {
    plan.is_v3() && !plan.is_probe_build() && plan.receipt_index_format.is_some()
}

/// Count archive metadata and only the prediction slots used on the active
/// versions. The coordinator also looks up keys in the current published
/// index, so contents, metadata and key bytes include inactive versions.
pub fn archive_bytes(sql: &impl Sql, plan: &CohortPlan) -> R<u64> {
    if !compact(plan) {
        return Ok(0);
    }
    let versions = plan
        .reward_v1
        .iter()
        .flat_map(|v| &v.probes)
        .map(|p| (p.version, p.size))
        .collect::<Vec<_>>();
    let versions =
        serde_json::to_string(&versions).map_err(|_| EvalError::State("package probe sizes"))?;
    let rows = sql.exec(
        "WITH versions AS MATERIALIZED (
             SELECT version, count(*) AS vectors, sum(length(CAST(vector_key AS BLOB))) AS key_bytes
               FROM fingerprints GROUP BY version
         ), active AS (
             SELECT value ->> 0 AS version, value ->> 1 AS words FROM json_each(?)
         )
         SELECT (SELECT count(*) FROM published_contents),
                (SELECT coalesce(sum(vectors), 0) FROM versions),
                (SELECT coalesce(sum(key_bytes), 0) FROM versions),
                (SELECT coalesce(sum(versions.vectors * active.words), 0) FROM active JOIN versions USING (version))",
        &[SqlValue::Text(versions)],
    )?;
    let row = rows
        .first()
        .ok_or(EvalError::State("package archive counts"))?;
    // Live SQL rows, sealed hex and decoded records, B-tree keys and
    // transient copies. The active values are Vec<Option<Probability>>.
    Ok(uint(row, 0)?
        .saturating_mul(1024)
        .saturating_add(uint(row, 1)?.saturating_mul(2048))
        .saturating_add(uint(row, 2)?.saturating_mul(4))
        .saturating_add(uint(row, 3)?.saturating_mul(8)))
}

pub fn assembly_bytes(plan: &CohortPlan, archive: u64) -> usize {
    let (jobs, docs, span, feedback) = package_shape(plan);
    if !compact(plan) {
        return routes::package_step_bytes(jobs, docs, span, feedback);
    }
    compact_bytes(plan, archive, plan.entrants.len() as u64)
}

fn compact_bytes(plan: &CohortPlan, archive: u64, entrants: u64) -> usize {
    let tasks = max_job_tasks(plan);
    let refs = 1 + u64::from(plan.previous_reference.is_some()) + u64::from(plan.anchor_job());
    let jobs = refs.saturating_add(entrants);
    let scored = refs
        .saturating_mul(u64::from(plan.n))
        .saturating_add(entrants.saturating_mul(plan.entrant_docs()));
    let span = plan.span_gate();
    let feedback = plan
        .reward_v1
        .as_ref()
        .is_some_and(|v| v.feedback.is_some());
    let probes = plan.reward_v1.iter().flat_map(|v| &v.probes);
    let versions = probes.clone().count() as u64;
    let probe_slots = probes.fold(0u64, |n, p| n.saturating_add(u64::from(p.size)));
    // Start with the existing measured fixed package allowance. Boxed
    // predictions hold two 8-byte Options and one role byte per assigned
    // task. The encoded output fingerprints use four bytes per probe slot.
    let mut bytes = (routes::package_bytes(0, 0) as u64)
        .saturating_add(scored.saturating_mul(17))
        .saturating_add(entrants.saturating_mul(probe_slots).saturating_mul(4))
        .saturating_add(archive);
    // Per job: roster, signed package fields, component maps, closure and
    // SQL rows. Per entrant/version: fingerprint keys, canonical records
    // and reference comparisons. Per task: label rows, seeded permutation
    // and transient grading or comparison arrays at native pointer width.
    bytes = bytes
        .saturating_add(jobs.saturating_mul(SUMMARY_FIXED_BYTES as u64))
        .saturating_add(entrants.saturating_mul(versions).saturating_mul(2048))
        .saturating_add(tasks.saturating_mul(1024));
    // During decoding the growing arrays can have almost twice their
    // logical length. Charge these temporaries and the decoded JSON maps
    // across a whole read group. R2 block runs are counted separately.
    let temp = tasks
        .saturating_mul(34)
        .saturating_add(4 * SUMMARY_FIXED_BYTES as u64);
    bytes = bytes
        .saturating_add(temp.saturating_mul(routes::STEP_READS_IN_FLIGHT as u64))
        .saturating_add(routes::quant_package_bytes(jobs.saturating_sub(1)) as u64);
    if span {
        bytes = bytes.saturating_add(routes::span_gate_bytes(jobs) as u64);
    }
    if feedback {
        bytes = bytes.saturating_add(routes::feedback_package_bytes(entrants) as u64);
    }
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

pub fn fits(plan: &CohortPlan, archive: u64) -> bool {
    let child = if compact(plan) {
        let cap = plan
            .admission
            .as_ref()
            .map_or(0, |a| u64::from(a.roster_cap));
        compact_bytes(plan, archive, cap.max(plan.entrants.len() as u64))
            .max(routes::receipt_page_bytes(false))
    } else {
        assembly_bytes(plan, archive)
    };
    let limit = slop_ninja_arena::model::LARGEST_ROUTE_BYTES;
    child <= limit
        && routes::scope_bytes(routes::ALARM_CONTROL_BYTES)
            .saturating_add(routes::scope_bytes(child))
            .saturating_add(routes::package_reads_bytes(max_job_tasks(plan)))
            <= limit
}

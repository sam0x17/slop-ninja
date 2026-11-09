//! Detailed receipt retention (the N+2 rule).
//!
//! Cohort N's detailed receipts are deleted once two later cohorts (N+1 and
//! N+2) have completed in this store, judged by durable completion only: a
//! cohort is complete when it is `closed` and, for a `reward-v1` cohort, its
//! closure is recorded (`closure_digest`). Probe builds and abandoned cohorts
//! never count as completions (a probe build has no package or target and is
//! outside the public record, so the publisher's N+1 and N+2 are the same
//! cohorts); their own receipts follow the same rule. Nothing here reads
//! time, tempos, adoption or publication, so coordinator silence, slow work
//! or a missed wakeup only moves the deletion later.
//!
//! "Detailed receipts" are exactly what the published layout calls them
//! (`publisher::publish::is_detailed_receipt`): in the receipts bucket, the
//! cohort's chunk objects and bundles under `cohorts/<label>/receipts/`, its
//! `receipt-index.signed` and `chunk-map.json` (a probe build's index and
//! map sit under its probe prefix, deleted only by the latest build of that
//! probe version, which wrote them), and in this store its `receipts` and
//! `receipt_objects` rows. Everything else stays: the signed manifest,
//! package (which binds the receipt root and count), target, closure,
//! components and keys records, the cohort row with `receipt_root` and
//! `receipt_count`, targets, rows and every recovery record. Before the
//! first object goes, a content-free marker `cohorts/<label>/receipts-retired.json`
//! is written ([`marker`]), so a publisher that had not copied the cohort
//! yet records it as retired before publication instead of waiting for
//! objects that will never return.
//!
//! **Held cohorts.** New commitments need the receipts of what they commit
//! to. A cohort is held, whatever completed after it, while
//! - its target is one a signing path reads: the newest `reward-v1` target
//!   (`v3::active_target_digest`), or the newest target of any rule (the
//!   predecessor a reward-v0 package chains to). A rule that stopped
//!   producing targets is released once a later target activates. A
//!   superseded authority holds no target once it accepted its successor's
//!   notice ([`accept_notice`]): the restart set its targets aside and the
//!   successor's two completions show its own chain is active; or
//! - the current authorization names it through its target ([`Hold`]):
//!   while the authorization commits that target, it is not superseded, so
//!   the approved N+2 rule (delete after N+1 and N+2 complete and N's
//!   target is superseded) keeps its cohort. The authorization's carrying
//!   cohort is recorded but not held: holding it past N+2 is a change to
//!   that rule awaiting the user's decision (the operator proposal records
//!   the text), and until then a validator that starts after the carrier's
//!   deletion waits for the next authorization (`receipts_expired`). Read
//!   from the private pointer
//!   `authorizations/current.json` (the shared contract
//!   `slop_ninja_admission_wire::receipts`) before every deletion step. An
//!   authorization stays valid until a newer one replaces the pointer, so the
//!   hold has no count or time limit; a revocation the pointer lists does not
//!   end it (only a pointer naming another authorization does). A pointer
//!   that cannot be read, parsed or matched to this chain stops deletion
//!   until it can, and the stall is reported ([`stalled`]); no pointer holds
//!   nothing beyond the active targets (a launch authorization names no
//!   receipts).
//!
//! A held cohort becomes due at the first step after the hold ends, provided
//! two later completions exist. Its `receipts_due_after` is the completion
//! that satisfied the rule: the second later completion, or the cohort whose
//! target superseded N's when that is later. An authorization hold delays
//! deletion without changing that number.
//!
//! **Predecessors.** A superseded main authority completes nothing more. Its
//! successor tells it, once two of the successor's own cohorts completed
//! ([`notice_due`]); the predecessor then marks every remaining cohort due
//! ([`accept_notice`]), since the chain-record restart superseded every
//! target it held (the coordinator's `restart_target` and the validators'
//! `reset-target` set them aside). Cohort numbers continue across
//! authorities, so the successor's completions are the predecessor's N+1
//! and N+2. The successor repeats the notice until the predecessor reports
//! nothing left, because a retired authority's alarm re-arms only while it
//! is kicked. Due deletions never block succession (`engine::blocking_work`):
//! whatever remains finishes under the successor's notices. Each predecessor
//! is told independently ([`notices_pending`]), so one that never answers
//! holds up no other. A predecessor covered through a later predecessor's
//! record (it is never opened) is not told when that later predecessor's
//! own notice to it already reached past its highest cohort + 2
//! (`config::retention_covered`): it is recorded with that
//! notice's cohort instead.
//!
//! Deletion is resumable: each step deletes at most one listing page of
//! objects (R2's list bound) and then, after the listing is empty and the two
//! single objects are gone, one page of rows; the cohort is marked deleted in
//! the transaction that removes its last row. A failed step is recorded
//! (`last_error`, `error_since_ms` in `/eval/status`) and retried. Steps run
//! whenever the root has nothing else to do: with no active cohort, or while
//! the active cohort only waits on its lanes (`engine::next_work`). A due
//! cohort still retained after a further completion, or a predecessor not
//! yet finished after one, is reported as stalled ([`stalled`]), which the
//! publisher alerts on (`retention_stalled`).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

use crate::eval::engine::{EvalConfig, EvalError, R, Work, i, s};
use crate::sql::{Row, Sql, SqlValue, bind_u64, int, text};
use serde_json::{Value, json};

/// v13: when each cohort's detailed receipts became due, started going and
/// were deleted; the predecessors this authority has told; the current
/// authorization's hold, the last failed step, and the successor notice this
/// (superseded) authority accepted first.
pub const SCHEMA_V13: &[&str] = &[
    "ALTER TABLE cohorts ADD COLUMN receipts_due_after INTEGER",
    "ALTER TABLE cohorts ADD COLUMN receipts_retiring_ms INTEGER",
    "ALTER TABLE cohorts ADD COLUMN receipts_deleted_ms INTEGER",
    "ALTER TABLE cohorts ADD COLUMN receipts_deleted_objects INTEGER NOT NULL DEFAULT 0",
    "CREATE TABLE retention_notices (
        predecessor TEXT PRIMARY KEY, after_cohort INTEGER NOT NULL, notified_ms INTEGER NOT NULL
    ) STRICT",
    "CREATE TABLE retention_state (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        hold_kind TEXT, hold_cohort INTEGER, hold_manifest TEXT, hold_target TEXT, hold_read_ms INTEGER,
        last_error TEXT, error_since_ms INTEGER, errors INTEGER NOT NULL DEFAULT 0,
        notice_after INTEGER
    ) STRICT",
    "INSERT INTO retention_state (id) VALUES (1)",
];

/// The versioned rule, reported in `/eval/status`.
pub const RULE: &str = "receipts_after_two_later_completions_v2";

/// Objects and rows deleted per step: R2's `list` returns at most 1,000 keys
/// per call, and `delete_multiple` takes the same, so one page is one listing.
pub const PAGE: usize = 1_000;

/// The shared receipts-bucket contract (pointer key, kinds, bounds, marker),
/// imported by the publisher too.
pub use slop_ninja_admission_wire::receipts as contract;
use slop_ninja_admission_wire::json::ObjectRef;

/// The closure a finished probe build records (`v3::commit_probe_finish`).
const PROBE_BUILD_CLOSURE: &str = "probe-build";

/// An optional non-negative integer column.
fn ou(row: &Row, idx: usize) -> Option<u64> {
    crate::eval::engine::opt_int(row, idx).and_then(|v| u64::try_from(v).ok())
}

/// A completed cohort: closed, with its closure recorded when it has one; a
/// probe build (no package, no target, outside the public record) is not.
fn completed_where() -> String {
    format!("phase = 'closed' AND (v3 IS NULL OR (closure_digest IS NOT NULL AND closure_digest != '{PROBE_BUILD_CLOSURE}'))")
}

/// The cohort numbers of this store's completed cohorts, ascending.
fn completed(sql: &impl Sql) -> R<Vec<u64>> {
    let rows = sql.exec(&format!("SELECT cohort FROM cohorts WHERE {} ORDER BY cohort", completed_where()), &[])?;
    rows.iter().map(|r| u64::try_from(int(r, 0)?).map_err(|_| EvalError::State("cohort number"))).collect()
}

/// Whether this (superseded) authority accepted a successor's notice.
fn notice_accepted(sql: &impl Sql) -> R<bool> {
    let rows = sql.exec("SELECT notice_after FROM retention_state WHERE id = 1", &[])?;
    Ok(rows.first().and_then(|r| ou(r, 0)).is_some())
}

/// The cohorts whose target a signing path reads: the newest `reward-v1`
/// target (`v3::active_target_digest`) and the newest target of any rule
/// (the predecessor a reward-v0 package chains to). None once a successor's
/// notice was accepted.
fn active_target_seqs(sql: &impl Sql) -> R<Vec<i64>> {
    if notice_accepted(sql)? {
        return Ok(Vec::new());
    }
    sql.exec(
        "SELECT seq FROM eval_targets WHERE seq = (SELECT MAX(seq) FROM eval_targets)
            OR seq = (SELECT MAX(seq) FROM eval_targets WHERE rule = 'reward-v1')
         ORDER BY seq",
        &[],
    )?
    .iter()
    .map(|r| Ok(int(r, 0)?))
    .collect()
}

/// What the current authorization names (`authorizations/current.json`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hold {
    /// No pointer is written: nothing beyond the active targets is held.
    None,
    /// A launch authorization: it names the launch burn row and no receipts.
    Launch,
    /// A reward-intent authorization: its carrying cohort (number and
    /// manifest digest, hex) and the target active after its package (hex).
    Cohort { cohort: u64, manifest: String, target: String },
}

/// The pointer's authorization kind and object reference, under the shared
/// checks (`contract::parse_pointer`: the known schema, this deployment's
/// chain binding, content-addressed object keys). Refused otherwise;
/// deletion then waits until it passes. A listed revocation is not read: it
/// never ends a hold on its own.
pub fn pointer_ref(bytes: &[u8], chain: &slop_ninja_protocol::ChainBinding) -> std::result::Result<(String, ObjectRef), &'static str> {
    let p = contract::parse_pointer(bytes, chain)?;
    Ok((p.kind, p.authorization))
}

/// The hold of the authorization object `bytes` the pointer names as `r`,
/// of kind `kind`. The object must match the pointer's digest and length.
/// Its signature is not checked here: a hold only keeps receipts, so an
/// unauthenticated one can never cause a deletion.
pub fn hold_of(kind: &str, r: &ObjectRef, bytes: &[u8]) -> std::result::Result<Hold, &'static str> {
    if bytes.len() as u64 != r.bytes || slop_ninja_protocol::object_sha256(bytes).hex() != r.sha256 {
        return Err("authorization_object");
    }
    match kind {
        contract::KIND_LAUNCH => Ok(Hold::Launch),
        contract::KIND_COHORT => {
            let a = slop_ninja_protocol::Signed::<slop_ninja_protocol::RewardIntentAuthorization>::from_bytes(bytes)
                .map_err(|_| "authorization_object")?;
            Ok(Hold::Cohort {
                cohort: a.record.cohort,
                manifest: a.record.cohort_manifest.hex(),
                target: a.record.target.hex(),
            })
        }
        _ => Err("authorization_kind"),
    }
}

/// The hold recorded at the last deletion step.
pub fn hold(sql: &impl Sql) -> R<Hold> {
    let rows = sql.exec("SELECT hold_kind, hold_cohort, hold_manifest, hold_target FROM retention_state WHERE id = 1", &[])?;
    let r = rows.first().ok_or(EvalError::State("retention_state row"))?;
    Ok(match crate::eval::engine::opt_text(r, 0).as_deref() {
        Some("launch") => Hold::Launch,
        Some("cohort") => Hold::Cohort {
            cohort: ou(r, 1).ok_or(EvalError::State("hold cohort"))?,
            manifest: crate::eval::engine::opt_text(r, 2).ok_or(EvalError::State("hold manifest"))?,
            target: crate::eval::engine::opt_text(r, 3).ok_or(EvalError::State("hold target"))?,
        },
        _ => Hold::None,
    })
}

/// The cohorts the current authorization holds: its target's cohort, whose
/// target the authorization still commits (module docs). The carrier
/// (number and manifest digest) is recorded with the hold but not held
/// under the current rule.
fn held_by_authorization(sql: &impl Sql) -> R<Vec<i64>> {
    let Hold::Cohort { target, .. } = hold(sql)? else { return Ok(Vec::new()) };
    sql.exec(
        "SELECT seq FROM cohorts WHERE seq IN (SELECT seq FROM eval_targets WHERE target_digest = ?) ORDER BY seq",
        &[s(target)],
    )?
    .iter()
    .map(|r| Ok(int(r, 0)?))
    .collect()
}

/// Every held cohort: the active targets' and the authorization's.
fn held(sql: &impl Sql) -> R<Vec<i64>> {
    let mut out = active_target_seqs(sql)?;
    out.extend(held_by_authorization(sql)?);
    Ok(out)
}

/// Records the hold read before a deletion step. A due cohort it names
/// whose deletion has not started (`receipts_retiring_ms`, set once the
/// marker is written) goes back to waiting. `true` when `seq` is still due.
pub fn apply_hold(sql: &impl Sql, h: &Hold, seq: i64, now_ms: u64) -> R<bool> {
    let (kind, cohort, manifest, target) = match h {
        Hold::None => (SqlValue::Null, SqlValue::Null, SqlValue::Null, SqlValue::Null),
        Hold::Launch => (s("launch"), SqlValue::Null, SqlValue::Null, SqlValue::Null),
        Hold::Cohort { cohort, manifest, target } => (s("cohort"), bind_u64(*cohort)?, s(manifest), s(target)),
    };
    sql.exec(
        "UPDATE retention_state SET hold_kind = ?, hold_cohort = ?, hold_manifest = ?, hold_target = ?, hold_read_ms = ?
         WHERE id = 1",
        &[kind, cohort, manifest, target, bind_u64(now_ms)?],
    )?;
    for held_seq in held(sql)? {
        sql.exec(
            "UPDATE cohorts SET receipts_due_after = NULL
             WHERE seq = ? AND receipts_deleted_ms IS NULL AND receipts_retiring_ms IS NULL",
            &[i(held_seq)],
        )?;
    }
    let rows = sql.exec("SELECT 1 FROM cohorts WHERE seq = ? AND receipts_due_after IS NOT NULL", &[i(seq)])?;
    Ok(!rows.is_empty())
}

/// The cohort whose target released `seq`'s, when `seq` produced one: for a
/// `reward-v1` target the next `reward-v1` target, for any other the next
/// target of any rule ([`active_target_seqs`]).
fn superseding_cohort(sql: &impl Sql, seq: i64) -> R<Option<u64>> {
    let rows = sql.exec(
        "SELECT c.cohort FROM eval_targets t JOIN cohorts c ON c.seq = t.seq
         WHERE t.seq > ?1 AND (t.rule = 'reward-v1' OR (SELECT rule FROM eval_targets WHERE seq = ?1) != 'reward-v1')
         ORDER BY t.seq LIMIT 1",
        &[i(seq)],
    )?;
    Ok(rows.first().and_then(|r| ou(r, 0)))
}

/// Cohorts whose receipts are newly due: `(seq, after)`, where `after` is the
/// completion that satisfied the rule (the second completed cohort later
/// than it, or the cohort whose target superseded its own, if later).
/// Held cohorts are skipped. Read only.
fn newly_due(sql: &impl Sql) -> R<Vec<(i64, u64)>> {
    let done = completed(sql)?;
    let held = held(sql)?;
    let rows = sql.exec(
        "SELECT seq, cohort FROM cohorts WHERE receipts_due_after IS NULL AND receipts_deleted_ms IS NULL
           AND phase IN ('closed','abandoned') ORDER BY seq",
        &[],
    )?;
    let mut out = Vec::new();
    for r in &rows {
        let seq = int(r, 0)?;
        if held.contains(&seq) {
            continue;
        }
        let n = u64::try_from(int(r, 1)?).map_err(|_| EvalError::State("cohort number"))?;
        if let Some(after) = done.iter().copied().filter(|m| *m > n).nth(1) {
            let after = superseding_cohort(sql, seq)?.map_or(after, |m| m.max(after));
            out.push((seq, after));
        }
    }
    Ok(out)
}

/// Records every newly due cohort's `receipts_due_after`.
pub fn mark_due(sql: &impl Sql) -> R<u64> {
    let due = newly_due(sql)?;
    for (seq, after) in &due {
        sql.exec(
            "UPDATE cohorts SET receipts_due_after = ? WHERE seq = ? AND receipts_due_after IS NULL",
            &[bind_u64(*after)?, i(*seq)],
        )?;
    }
    Ok(due.len() as u64)
}

/// The receipts-bucket prefix and single keys of cohort `label`'s detailed
/// receipts. A probe build's index and chunk map under its probe prefix are
/// included only when `probe_keys` names its version (the latest build of
/// that version, which wrote them: [`probe_keys_owner`]).
pub fn detailed_keys(cfg: &EvalConfig, label: &str, probe_keys: Option<u32>) -> (String, Vec<String>) {
    let base = crate::eval::engine::receipt_prefix(label);
    let mut keys = vec![format!("{base}/receipt-index.signed"), format!("{base}/chunk-map.json")];
    if let Some(v) = probe_keys {
        let p = crate::eval::v3::probe_prefix(cfg, v);
        keys.push(format!("{p}/receipt-index.signed"));
        keys.push(format!("{p}/chunk-map.json"));
    }
    (format!("{base}/receipts/"), keys)
}

/// Whether cohort `seq` committed a probe build's finish (it wrote the index
/// and map under its probe prefix).
fn probe_finished(sql: &impl Sql, seq: i64) -> R<bool> {
    let rows = sql.exec("SELECT closure_digest FROM cohorts WHERE seq = ?", &[i(seq)])?;
    Ok(rows.first().and_then(|r| crate::eval::engine::opt_text(r, 0)).as_deref() == Some(PROBE_BUILD_CLOSURE))
}

/// The probe version whose prefix keys cohort `c` owns: `c` is a probe
/// build that committed its finish (the index and map under the probe
/// prefix are its own) and no later cohort here builds the same version
/// (one that finished overwrote the keys; one still open may write them).
/// A later build abandoned before its finish never wrote them.
fn probe_keys_owner(sql: &impl Sql, c: &crate::eval::engine::Cohort) -> R<Option<u32>> {
    let version = |p: &crate::eval::plan::CohortPlan| {
        if p.is_probe_build() { p.reward_v1.as_ref().and_then(|r| r.probe_version) } else { None }
    };
    let Some(v) = version(&c.plan) else { return Ok(None) };
    if !probe_finished(sql, c.seq)? {
        return Ok(None);
    }
    for o in crate::eval::engine::cohorts(sql)? {
        if o.seq > c.seq && version(&o.plan) == Some(v) && (o.phase != "abandoned" || probe_finished(sql, o.seq)?) {
            return Ok(None);
        }
    }
    Ok(Some(v))
}

/// The marker written before a cohort's first object is deleted: content
/// free, so the publisher can tell receipts deleted before publication from
/// receipts not yet stored.
pub fn marker(label: &str, cohort: u64, after: u64) -> (String, String) {
    let key = format!("{}/{}", crate::eval::engine::receipt_prefix(label), contract::RETIRED_MARKER);
    let body = contract::RetiredMarker {
        schema: contract::RETIRED_MARKER_SCHEMA.into(),
        label: label.into(),
        cohort,
        rule: RULE.into(),
        due_after_cohort: after,
    };
    (key, serde_json::to_string(&body).unwrap_or_default())
}

/// The next deletion step: the oldest due cohort whose receipts remain, with
/// its marker until the marker is recorded. Marks newly due cohorts first.
pub fn next_work(sql: &impl Sql, cfg: &EvalConfig) -> R<Option<Work>> {
    mark_due(sql)?;
    let rows = sql.exec(
        "SELECT seq, receipts_due_after, receipts_retiring_ms FROM cohorts
         WHERE receipts_due_after IS NOT NULL AND receipts_deleted_ms IS NULL ORDER BY seq LIMIT 1",
        &[],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    let seq = int(r, 0)?;
    let after = ou(r, 1).ok_or(EvalError::State("due after"))?;
    let retiring = ou(r, 2).is_some();
    let c = crate::eval::engine::cohort(sql, seq)?;
    let probe = probe_keys_owner(sql, &c)?;
    let (prefix, keys) = detailed_keys(cfg, &c.label, probe);
    let marker = (!retiring).then(|| marker(&c.label, c.plan.cohort, after));
    Ok(Some(Work::RetireReceipts { seq, prefix, keys, marker }))
}

/// The marker is stored: deletion of `seq` has started and is no longer
/// returned to waiting by a hold.
pub fn note_retiring(sql: &impl Sql, seq: i64, now_ms: u64) -> R<()> {
    sql.exec(
        "UPDATE cohorts SET receipts_retiring_ms = ? WHERE seq = ? AND receipts_retiring_ms IS NULL",
        &[bind_u64(now_ms)?, i(seq)],
    )?;
    Ok(())
}

/// A deletion step failed with `code` (retried at a later step).
pub fn note_error(sql: &impl Sql, code: &str, now_ms: u64) -> R<()> {
    sql.exec(
        "UPDATE retention_state SET last_error = ?, error_since_ms = COALESCE(error_since_ms, ?), errors = errors + 1
         WHERE id = 1",
        &[s(code), bind_u64(now_ms)?],
    )?;
    Ok(())
}

/// A deletion step succeeded: any earlier failure is over.
pub fn clear_error(sql: &impl Sql) -> R<()> {
    sql.exec("UPDATE retention_state SET last_error = NULL, error_since_ms = NULL WHERE id = 1", &[])?;
    Ok(())
}

/// Whether a deletion step is due (read only; for `engine::has_work`).
pub fn has_work(sql: &impl Sql) -> R<bool> {
    let pending = !sql
        .exec("SELECT 1 FROM cohorts WHERE receipts_due_after IS NOT NULL AND receipts_deleted_ms IS NULL LIMIT 1", &[])?
        .is_empty();
    Ok(pending || !newly_due(sql)?.is_empty())
}

/// One page of `seq`'s objects was deleted (`n` keys).
pub fn note_page(sql: &impl Sql, seq: i64, n: u64) -> R<()> {
    sql.exec(
        "UPDATE cohorts SET receipts_deleted_objects = receipts_deleted_objects + ? WHERE seq = ?",
        &[bind_u64(n)?, i(seq)],
    )?;
    Ok(())
}

/// After `seq`'s objects are gone: deletes at most `page` of its receipt rows
/// and receipt-object rows, and marks its receipts deleted once none remain.
/// `true` when the cohort is done.
pub fn finish(sql: &impl Sql, seq: i64, page: usize, now_ms: u64) -> R<bool> {
    let limit = i64::try_from(page).map_err(|_| EvalError::State("page"))?;
    for table in ["receipts", "receipt_objects"] {
        sql.exec(
            &format!("DELETE FROM {table} WHERE rowid IN (SELECT rowid FROM {table} WHERE seq = ? LIMIT ?)"),
            &[i(seq), i(limit)],
        )?;
    }
    let left = sql.exec(
        "SELECT (SELECT COUNT(*) FROM receipts WHERE seq = ?) + (SELECT COUNT(*) FROM receipt_objects WHERE seq = ?)",
        &[i(seq), i(seq)],
    )?;
    if left.first().map(|r| int(r, 0)).transpose()?.unwrap_or(0) > 0 {
        return Ok(false);
    }
    sql.exec(
        "UPDATE cohorts SET receipts_deleted_ms = ? WHERE seq = ? AND receipts_deleted_ms IS NULL",
        &[bind_u64(now_ms)?, i(seq)],
    )?;
    Ok(true)
}

/// A predecessor this authority has not yet told about its completions, with
/// the cohort after which the predecessor's receipts are due (the second
/// completed cohort here): the first of [`notices_pending`]. The original
/// and retired authorities tell nobody.
pub fn notice_due(sql: &impl Sql, cfg: &EvalConfig) -> R<Option<Work>> {
    Ok(notices_pending(sql, cfg)?.and_then(|(after, preds)| {
        preds.into_iter().next().map(|p| Work::RetentionNotice { predecessor: p, after_cohort: after })
    }))
}

/// Every predecessor this authority has not yet told (no `retention_notices`
/// row), in the deployment's order, with the cohort after which their
/// receipts are due; `None` before the second completion here, and for the
/// original and retired authorities.
///
/// The notice step (`cf_eval`'s `RetentionNotice` arm) handles each one
/// independently: a predecessor that does not answer never
/// holds up the ones after it. A predecessor covered through a later one's
/// record (`config::succession_through`) whose own successor already
/// delivered its notice past that predecessor's highest cohort + 2
/// (`config::retention_covered`) is recorded without being asked, with that
/// successor's cohort; any other is told directly, and recorded once it
/// reports nothing left.
pub fn notices_pending(sql: &impl Sql, cfg: &EvalConfig) -> R<Option<(u64, Vec<String>)>> {
    if cfg.authority.is_retired() {
        return Ok(None);
    }
    let preds = cfg.authority.predecessors();
    if preds.is_empty() {
        return Ok(None);
    }
    let Some(after) = completed(sql)?.get(1).copied() else { return Ok(None) };
    let mut pending = Vec::new();
    for p in preds {
        if sql.exec("SELECT 1 FROM retention_notices WHERE predecessor = ?", &[s(p)])?.is_empty() {
            pending.push(p.to_owned());
        }
    }
    Ok((!pending.is_empty()).then_some((after, pending)))
}

/// The notices this authority recorded, `(predecessor, after cohort)` in
/// name order, for its succession record (`config::SuccessionRecord`): a
/// later successor that covers one of these predecessors through this
/// authority's record skips its own notice by them
/// (`config::retention_covered`).
pub fn notices_recorded(sql: &impl Sql) -> R<Vec<(String, u64)>> {
    sql.exec("SELECT predecessor, after_cohort FROM retention_notices ORDER BY predecessor", &[])?
        .iter()
        .map(|r| Ok((text(r, 0)?.to_owned(), ou(r, 1).ok_or(EvalError::State("notice cohort"))?)))
        .collect()
}

/// The predecessor reported nothing left to delete after `after`.
pub fn record_notice(sql: &impl Sql, predecessor: &str, after: u64, now_ms: u64) -> R<()> {
    sql.exec(
        "INSERT INTO retention_notices (predecessor, after_cohort, notified_ms) VALUES (?, ?, ?)
         ON CONFLICT (predecessor) DO NOTHING",
        &[s(predecessor), bind_u64(after)?, bind_u64(now_ms)?],
    )?;
    Ok(())
}

/// A successor's notice that `after_cohort` is its second completed cohort.
/// Refused unless this authority recorded a successor (`not_superseded`) and
/// `after_cohort` is later than every cohort here (`notice_cohort`: numbers
/// continue across authorities). Records the first accepted notice (from
/// then on no target is held here: [`active_target_seqs`]), marks every
/// remaining closed or abandoned cohort due (a superseded authority holds no
/// other phase), and reports how many still hold receipts.
pub fn accept_notice(sql: &impl Sql, after_cohort: u64) -> R<std::result::Result<Value, &'static str>> {
    if crate::eval::succession::superseded_by(sql)?.is_none() {
        return Ok(Err("not_superseded"));
    }
    let highest = sql.exec("SELECT MAX(cohort) FROM cohorts", &[])?;
    if let Some(h) = highest.first().and_then(|r| ou(r, 0))
        && after_cohort <= h
    {
        return Ok(Err("notice_cohort"));
    }
    sql.exec(
        "UPDATE retention_state SET notice_after = ? WHERE id = 1 AND notice_after IS NULL",
        &[bind_u64(after_cohort)?],
    )?;
    sql.exec(
        "UPDATE cohorts SET receipts_due_after = ? WHERE receipts_due_after IS NULL AND receipts_deleted_ms IS NULL
           AND phase IN ('closed','abandoned')",
        &[bind_u64(after_cohort)?],
    )?;
    Ok(Ok(json!({"remaining": remaining(sql)?})))
}

/// Cohorts here whose detailed receipts are not yet deleted.
pub fn remaining(sql: &impl Sql) -> R<u64> {
    let rows = sql.exec("SELECT COUNT(*) FROM cohorts WHERE receipts_deleted_ms IS NULL", &[])?;
    Ok(rows.first().map(|r| int(r, 0)).transpose()?.map_or(0, |n| u64::try_from(n).unwrap_or(0)))
}

/// A cohort's retention record for `/eval/status`; `held` from [`held_seqs`].
pub fn cohort_status(sql: &impl Sql, seq: i64, held: &[i64]) -> R<Value> {
    let rows = sql.exec(
        "SELECT receipts_due_after, receipts_deleted_ms, receipts_deleted_objects, receipts_retiring_ms FROM cohorts
         WHERE seq = ?",
        &[i(seq)],
    )?;
    let r = rows.first().ok_or(EvalError::State("cohort"))?;
    let deleted = ou(r, 1);
    Ok(json!({
        "retained": deleted.is_none(),
        "due_after_cohort": ou(r, 0),
        "deleted_ms": deleted,
        "objects_deleted": ou(r, 2).unwrap_or(0),
        "retiring_ms": ou(r, 3),
        "held": held.contains(&seq),
    }))
}

/// The store-wide retention view for `/eval/status`.
pub fn status(sql: &impl Sql) -> R<Value> {
    let notices: Vec<Value> = sql
        .exec("SELECT predecessor, after_cohort, notified_ms FROM retention_notices ORDER BY predecessor", &[])?
        .iter()
        .map(|r| Ok(json!({"predecessor": text(r, 0)?, "after_cohort": ou(r, 1), "notified_ms": ou(r, 2)})))
        .collect::<R<_>>()?;
    let st = sql.exec(
        "SELECT hold_read_ms, last_error, error_since_ms, errors, notice_after FROM retention_state WHERE id = 1",
        &[],
    )?;
    let st = st.first().ok_or(EvalError::State("retention_state row"))?;
    let hold = match hold(sql)? {
        Hold::None => json!({"kind": null}),
        Hold::Launch => json!({"kind": "launch"}),
        Hold::Cohort { cohort, manifest, target } => {
            json!({"kind": "cohort", "carrier_cohort": cohort, "carrier_manifest": manifest, "target": target})
        }
    };
    Ok(json!({
        "rule": RULE, "page": PAGE, "retained_cohorts": remaining(sql)?, "predecessor_notices": notices,
        "authorization_hold": hold, "hold_read_ms": ou(st, 0),
        "last_error": crate::eval::engine::opt_text(st, 1), "error_since_ms": ou(st, 2), "errors": ou(st, 3).unwrap_or(0),
        "notice_accepted_after": ou(st, 4),
    }))
}

/// What has waited too long, judged by completions only (no clock): a due
/// cohort (marked, or due at the next step) whose receipts remain after a
/// completion later than the one that made it due, and a predecessor not yet
/// reported finished after a completion later than the notice's. Either
/// means deletion is not keeping up with durable completion (a failing step,
/// an unreadable pointer, an unreachable predecessor, or no idle root step);
/// the publisher alerts on a non-empty list (`retention_stalled`).
pub fn stalled(sql: &impl Sql, cfg: &EvalConfig) -> R<Vec<Value>> {
    let done = completed(sql)?;
    let since = |after: u64| done.iter().filter(|m| **m > after).count() as u64;
    let mut out = Vec::new();
    let mut due: Vec<(i64, u64)> = sql
        .exec(
            "SELECT seq, receipts_due_after FROM cohorts WHERE receipts_due_after IS NOT NULL
               AND receipts_deleted_ms IS NULL ORDER BY seq",
            &[],
        )?
        .iter()
        .map(|r| Ok((int(r, 0)?, ou(r, 1).ok_or(EvalError::State("due after"))?)))
        .collect::<R<_>>()?;
    due.extend(newly_due(sql)?);
    due.sort_unstable();
    for (seq, after) in due {
        let n = since(after);
        if n > 0 {
            let c = crate::eval::engine::cohort(sql, seq)?;
            out.push(json!({
                "kind": "cohort", "cohort": c.plan.cohort, "label": c.label, "due_after_cohort": after,
                "completions_since": n,
            }));
        }
    }
    if let Some((after_cohort, pending)) = notices_pending(sql, cfg)? {
        let n = since(after_cohort);
        if n > 0 {
            for p in pending {
                out.push(json!({
                    "kind": "predecessor", "predecessor": p, "after_cohort": after_cohort, "completions_since": n,
                }));
            }
        }
    }
    Ok(out)
}

/// The held cohorts (active targets and the authorization's), for
/// [`cohort_status`].
pub fn held_seqs(sql: &impl Sql) -> R<Vec<i64>> {
    held(sql)
}

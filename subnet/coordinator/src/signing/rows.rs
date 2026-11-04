//! The signed coordinator row other validators copy.
//!
//! Under the copy model (user decision of 2026-11-04) every validator other
//! than the coordinator commits the coordinator's row and does nothing else.
//! The row is derived from a finalized snapshot by the coordinator's own
//! step, separate from its hotkey pipeline: once a tempo (at the first
//! confirmed block of each commit epoch, [`snapshot_due`]), and whenever a
//! new reward-v1 target is installed, a schedule read and a registration
//! read at one finalized block ([`apply_schedule`], [`apply_registration`])
//! derive the installed target's row exactly as the coordinator's own
//! commitment does and journal it (`sign_row_snapshot`). That step runs in
//! every signer state: signing paused (an operator hold, a deploy, a
//! disabled commit/reveal flag), a signer fault, a preflight deferral (rate
//! limit, version key, commit/reveal version) or a hotkey or nonce fault
//! delays nothing other validators receive. It shares only the signer's
//! claim, its RPC budget share and the confirmed (finalized) block with the
//! commit pipeline; its failures back off at the capped rate and never
//! settle (`row_snapshot` on `/status`).
//!
//! [`refresh`] then signs a [`CoordinatorRow`] of the latest derived row of
//! the installed target (the snapshot's, or the coordinator's own
//! commitment's `commit_ops.row_json` when that is later by commit epoch and
//! registration block) with the authorization key (the same Worker secret
//! that signs authorizations, never the hotkey) and journals it in
//! `sign_row_record` (schema v13). The pointer
//! ([`super::authority::pointer_plan`]) names the record as `row` beside the
//! authorization it was derived under, so it reaches the receipts bucket
//! within one cron tick and the publisher copies it to the public host.
//! Validators verify it and commit its entries
//! (`slop_ninja_protocol::coordinator_row`). While no record covers the
//! installed authorization, `/status` shows `signing.row_record.pending_*`
//! (the authorization, since when and the code it waits on) and the
//! publisher alerts after a tempo (`row_record_stalled`); validators keep
//! committing the record they hold meanwhile and a fresh validator waits
//! (`row_unpublished`).
//!
//! - **One record per distinct row.** A record covers the commitment of the
//!   epoch it names and every later one of the same target, authorization
//!   and paid entries (UID, hotkey, registration block, weight, and the
//!   owner hotkey): a new one is signed only when one of those changes (a
//!   deregistration, a re-registration, a UID changing hands, an owner
//!   move), so validators keep committing the last record through any
//!   number of tempos, and through coordinator silence (nothing here
//!   expires). Under one authorization records are ordered by (commit
//!   epoch, registration block), both signed: a re-derivation in the same
//!   epoch that changes the entries signs a record with a later
//!   registration block, validators install a later record by that order,
//!   and no record is ever re-signed back to an earlier derivation.
//! - **Only the installed target.** The record is built from the latest
//!   derived row of the installed target, the hotkeys and registration
//!   blocks its snapshot read at each UID, and the installed bundle's
//!   verified authorization. A row of another target (a switch in progress)
//!   signs nothing.
//! - **The pointer names it only under its own authorization.** While the
//!   installed authorization differs from the record's (a new target was
//!   installed and its row not derived yet), the pointer carries no row:
//!   validators keep their installed record until the new one appears.
//! - **Failures are retried and shown.** Waiting codes
//!   (`row_waiting_derivation`, `row_waiting_issue`, or the snapshot's own
//!   code such as `row_deferred_owner_unregistered`) and failures
//!   (`row_hotkeys_missing` for a row derived by a build that did not record
//!   the hotkeys, a missing or mismatched authorization key, ...) are
//!   recorded on `/status` (logged once per code) and retried at the next
//!   tick; the previous record stays journaled. A failure clears once the
//!   journaled record covers the installed authorization and the latest
//!   row again, signed or not. The cheap checks (the
//!   decoded installed authorization, its issue epoch, the journaled row's
//!   fields) run before the installed bundle is verified, which happens only
//!   when a new record is due.

use super::{Verified, active_target, log};
use crate::sql::{Sql, SqlValue, StoreError, bind_u64, text, uint};
use crate::target::RowSummary;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use slop_ninja_protocol::{AccountId, CoordinatorRow, CoordinatorRowEntry, Record, TrustedKeys, sign};

/// The longest row record: the protocol's bound (a roster's entrants and one
/// burn UID), which also bounds the journaled hex and the reply that serves
/// it to the pointer writer.
pub fn record_bound() -> usize {
    CoordinatorRow::max_signed_len().map_or(0, |n| n as usize)
}

/// What one [`refresh`] did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Refresh {
    /// The journaled record covers the installed target's latest row.
    Current,
    /// No record can be signed for the installed authorization yet; the
    /// code says on what it waits (also on `/status`).
    Waiting(String),
    /// A new record was signed for the operation of this epoch.
    Signed { epoch: u64, sha256: String },
    /// Signing failed with this code; retried at the next tick.
    Failed(String),
}

/// The journaled record's bindings and signed bytes.
struct Stored {
    target: String,
    authorization: String,
    row_digest: String,
    signed: Vec<u8>,
}

impl Stored {
    /// The signed record's `(epoch, registration block)`: under one
    /// authorization a record supersedes another by this order.
    fn order(&self) -> Option<(u64, u64)> {
        slop_ninja_protocol::Signed::<CoordinatorRow>::from_bytes(&self.signed)
            .ok()
            .map(|s| (s.record.epoch, s.record.registration_block))
    }
}

/// Clears a recorded signing failure: the journaled record covers the
/// installed authorization and the latest derived row again, so nothing is
/// failing any more (a failure that ended without a new signature, such as
/// a transient store error, never stays on `/status`).
fn clear_error(sql: &impl Sql) -> Result<(), StoreError> {
    sql.exec(
        "UPDATE sign_row_record SET error = NULL, error_since_ms = NULL WHERE id = 1 AND error IS NOT NULL",
        &[],
    )?;
    Ok(())
}

/// The latest derived row of `target`: the coordinator's own commitment's
/// (`commit_ops.row_json`) or the row snapshot's (`sign_row_snapshot`),
/// whichever is later by `(commit epoch, registration block)`, with the
/// operation it was derived for (none for the snapshot's).
fn latest_row(sql: &impl Sql, target: &str) -> Result<Option<(Option<String>, u64, RowSummary)>, StoreError> {
    let mut candidates = Vec::new();
    for r in sql.exec(
        "SELECT op_id, epoch, row_json FROM commit_ops
         WHERE target = ? AND row_json IS NOT NULL
         ORDER BY epoch DESC LIMIT 1",
        &[target.into()],
    )? {
        let summary: RowSummary = serde_json::from_str(text(&r, 2)?).map_err(|_| StoreError("row json"))?;
        candidates.push((Some(text(&r, 0)?.to_owned()), uint(&r, 1)?, summary));
    }
    for r in sql.exec(
        "SELECT row_epoch, row_json FROM sign_row_snapshot
         WHERE id = 1 AND target = ? AND row_json IS NOT NULL AND row_epoch IS NOT NULL",
        &[target.into()],
    )? {
        let summary: RowSummary = serde_json::from_str(text(&r, 1)?).map_err(|_| StoreError("row json"))?;
        candidates.push((None, uint(&r, 0)?, summary));
    }
    Ok(candidates.into_iter().max_by_key(|(_, epoch, s)| (*epoch, s.registration_block)))
}

/// Why the row snapshot produced no row for `target`, when it says.
fn snapshot_code(sql: &impl Sql, target: &str) -> Result<Option<String>, StoreError> {
    Ok(sql
        .exec("SELECT code FROM sign_row_snapshot WHERE id = 1 AND target = ?", &[target.into()])?
        .first()
        .and_then(|r| text(r, 0).ok().map(str::to_owned)))
}

fn stored(sql: &impl Sql) -> Result<Option<Stored>, StoreError> {
    let rows = sql.exec(
        "SELECT target, authorization, row_digest, signed_hex FROM sign_row_record
         WHERE id = 1 AND signed_hex IS NOT NULL",
        &[],
    )?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(Stored {
        target: text(r, 0)?.to_owned(),
        authorization: text(r, 1)?.to_owned(),
        row_digest: text(r, 2)?.to_owned(),
        signed: hex::decode(text(r, 3)?).map_err(|_| StoreError("row record hex"))?,
    }))
}

/// The journaled record's signed bytes when it was committed under the
/// authorization with record digest `authorization` (hex), else `None`.
pub fn for_authorization(sql: &impl Sql, authorization: &str) -> Result<Option<Vec<u8>>, StoreError> {
    Ok(stored(sql)?.filter(|s| s.authorization == authorization).map(|s| s.signed))
}

/// Records a signing failure (kept with the time its run began, logged once
/// per code). Also used by the pointer route when a refresh itself failed
/// to run (`row_record_store`), so the pointer is published regardless.
pub fn note_error(sql: &impl Sql, code: &str, now_ms: u64) -> Result<Refresh, StoreError> {
    let previous = sql
        .exec("SELECT error FROM sign_row_record WHERE id = 1", &[])?
        .first()
        .and_then(|r| text(r, 0).ok().map(str::to_owned));
    sql.exec(
        "INSERT INTO sign_row_record (id, error, error_since_ms) VALUES (1, ?, ?)
         ON CONFLICT(id) DO UPDATE SET error = excluded.error,
             error_since_ms = COALESCE(sign_row_record.error_since_ms, excluded.error_since_ms)",
        &[code.into(), bind_u64(now_ms)?],
    )?;
    if previous.as_deref() != Some(code) {
        log(sql, now_ms, None, None, None, "row_record_failed", Some(code))?;
    }
    Ok(Refresh::Failed(code.to_owned()))
}

/// Records that no record covers the installed authorization `installed`
/// yet, waiting on `code` (the time is kept from the first pass for that
/// authorization; the code is logged once per change), or clears the mark
/// when the journaled record covers it (`covered`).
fn mark_pending(sql: &impl Sql, installed: &str, covered: bool, code: &str, now_ms: u64) -> Result<(), StoreError> {
    if covered {
        sql.exec(
            "UPDATE sign_row_record SET pending_authorization = NULL, pending_since_ms = NULL, pending_code = NULL
             WHERE id = 1 AND pending_authorization IS NOT NULL",
            &[],
        )?;
        return Ok(());
    }
    let previous = sql
        .exec("SELECT pending_authorization, pending_code FROM sign_row_record WHERE id = 1", &[])?
        .first()
        .map(|r| (text(r, 0).ok().map(str::to_owned), text(r, 1).ok().map(str::to_owned)));
    sql.exec(
        "INSERT INTO sign_row_record (id, pending_authorization, pending_since_ms, pending_code) VALUES (1, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
             pending_since_ms = CASE WHEN sign_row_record.pending_authorization IS excluded.pending_authorization
                 THEN COALESCE(sign_row_record.pending_since_ms, excluded.pending_since_ms)
                 ELSE excluded.pending_since_ms END,
             pending_authorization = excluded.pending_authorization,
             pending_code = excluded.pending_code",
        &[installed.into(), bind_u64(now_ms)?, code.into()],
    )?;
    let same = previous.is_some_and(|(a, c)| a.as_deref() == Some(installed) && c.as_deref() == Some(code));
    if !same {
        log(sql, now_ms, None, None, None, "row_record_waiting", Some(code))?;
    }
    Ok(())
}

/// The paid entries of a journaled row: `(uid, hotkey, registration block,
/// weight)` and the owner hotkey, or `None` when the row was derived by a
/// build that did not record them (or a UID's hotkey was not read).
struct Paid {
    entries: Vec<(u16, String, u64, u16)>,
    owner: String,
}

fn paid(summary: &RowSummary) -> Option<Paid> {
    let hotkeys = summary.hotkeys.as_ref().filter(|h| h.len() == summary.entries.len())?;
    let blocks = summary.registration_blocks.as_ref().filter(|b| b.len() == summary.entries.len())?;
    let owner = summary.owner_hotkey.clone()?;
    let entries = summary
        .entries
        .iter()
        .zip(hotkeys)
        .zip(blocks)
        .map(|(((uid, weight), hotkey), block)| (*uid, hotkey.clone(), *block, *weight))
        .collect();
    Some(Paid { entries, owner })
}

/// Signs and journals the record of the latest derived row of the installed
/// target when no journaled record covers it (module docs). Runs in the
/// pointer route, in its own transaction, before the pointer is planned.
pub fn refresh(
    sql: &impl Sql,
    cfg: &super::SignConfig,
    keys: &TrustedKeys,
    key: &impl super::auto::AuthorizationKeySource,
    now_ms: u64,
) -> Result<Refresh, StoreError> {
    // Cheap first: the installed target and its authorization, decoded
    // only, against the journaled record.
    let Some((installed_target, _)) = super::signer_target_bundle(sql)? else {
        return Ok(Refresh::Current);
    };
    let Some((installed, issued_epoch)) = super::authority::installed_authorization(sql, cfg, keys)? else {
        // No reward-v1 bundle installed: no record applies.
        return Ok(Refresh::Current);
    };
    let held = stored(sql)?;
    let covered = held.as_ref().is_some_and(|s| s.authorization == installed && s.target == installed_target);
    let waiting = |code: &str| -> Result<Refresh, StoreError> {
        mark_pending(sql, &installed, covered, code, now_ms)?;
        if covered {
            clear_error(sql)?;
        }
        Ok(if covered { Refresh::Current } else { Refresh::Waiting(code.to_owned()) })
    };
    // The latest derived row of the installed target, from the row snapshot
    // or the coordinator's own commitment; no sign attempt is waited for.
    let Some((op_id, epoch, summary)) = latest_row(sql, &installed_target)? else {
        let code = snapshot_code(sql, &installed_target)?;
        return waiting(code.as_deref().unwrap_or("row_waiting_derivation"));
    };
    // A row derived before the installed authorization's issue is not one
    // under it: wait for the first that is.
    if epoch < issued_epoch {
        return waiting("row_waiting_issue");
    }
    let Some(paid) = paid(&summary) else {
        mark_pending(sql, &installed, covered, "row_hotkeys_missing", now_ms)?;
        return note_error(sql, "row_hotkeys_missing", now_ms);
    };
    let entries_digest = entries_digest(&paid);
    if let Some(s) = &held
        && covered
        && (s.row_digest == entries_digest
            // Never re-signed back to an earlier derivation: under one
            // authorization records are ordered by (epoch, registration
            // block), the order validators install them in.
            || s.order().is_some_and(|held| (epoch, summary.registration_block) <= held))
    {
        mark_pending(sql, &installed, true, "", now_ms)?;
        clear_error(sql)?;
        return Ok(Refresh::Current);
    }
    // A new record is due: verify the installed bundle now.
    let failed = |code: &str| -> Result<Refresh, StoreError> {
        mark_pending(sql, &installed, covered, code, now_ms)?;
        note_error(sql, code, now_ms)
    };
    let t = match active_target(sql, cfg, keys)? {
        Some(Ok(t)) => t,
        // Nothing installed, or the installed target refused (revoked):
        // no record for it.
        _ => return waiting("row_target_refused"),
    };
    let Verified::V2(v) = &t.verified else {
        return Ok(Refresh::Current);
    };
    if t.digest != installed_target {
        return waiting("row_target_changed");
    }
    let terms = match v.checked_terms(keys) {
        Ok(terms) => terms,
        Err(_) => return failed("authorization_unchecked"),
    };
    let authorization = terms.digest.to_string();
    let Some(owner) = hotkey_of(&paid.owner) else {
        return failed("row_hotkey_unreadable");
    };
    let mut entries = Vec::with_capacity(paid.entries.len());
    for (uid, hotkey, registration_block, weight) in &paid.entries {
        let Some(hotkey) = hotkey_of(hotkey) else {
            return failed("row_hotkey_unreadable");
        };
        entries.push(CoordinatorRowEntry { uid: *uid, hotkey, registration_block: *registration_block, weight: *weight });
    }
    let record = match CoordinatorRow::of_verified(v, keys, epoch, summary.registration_block, owner, entries) {
        Ok(record) => record,
        Err(_) => return failed("row_record_invalid"),
    };
    let signing_key = match key.authorization_key(keys) {
        Ok(k) => k,
        Err(code) => return failed(code),
    };
    let bytes = match sign(record, &signing_key).and_then(|s| s.to_bytes()) {
        Ok(b) => b,
        Err(_) => return failed("row_record_unsigned"),
    };
    drop(signing_key);
    if bytes.len() > record_bound() {
        return failed("row_record_too_large");
    }
    let sha256 = hex::encode(Sha256::digest(&bytes));
    sql.exec(
        "INSERT INTO sign_row_record (id, op_id, epoch, target, authorization, row_digest, signed_hex, sha256,
             signed_at_ms, error, error_since_ms, pending_authorization, pending_since_ms, pending_code)
         VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL)
         ON CONFLICT(id) DO UPDATE SET op_id = excluded.op_id, epoch = excluded.epoch, target = excluded.target,
             authorization = excluded.authorization, row_digest = excluded.row_digest,
             signed_hex = excluded.signed_hex, sha256 = excluded.sha256, signed_at_ms = excluded.signed_at_ms,
             error = NULL, error_since_ms = NULL,
             pending_authorization = NULL, pending_since_ms = NULL, pending_code = NULL",
        &[
            op_id.as_deref().map_or(SqlValue::Null, Into::into),
            bind_u64(epoch)?,
            installed_target.as_str().into(),
            authorization.as_str().into(),
            entries_digest.as_str().into(),
            hex::encode(&bytes).into(),
            sha256.as_str().into(),
            bind_u64(now_ms)?,
        ],
    )?;
    log(sql, now_ms, None, op_id.as_deref(), None, "row_record_signed", Some(&sha256))?;
    Ok(Refresh::Signed { epoch, sha256 })
}

/// SHA-256 over the paid entries (UID, weight, hotkey and registration
/// block) and the owner hotkey.
fn entries_digest(paid: &Paid) -> String {
    let mut h = Sha256::new();
    h.update(b"slop_ninja.coordinator/row-entries/v2");
    h.update((paid.owner.len() as u32).to_le_bytes());
    h.update(paid.owner.as_bytes());
    for (uid, hotkey, block, weight) in &paid.entries {
        h.update(uid.to_le_bytes());
        h.update(weight.to_le_bytes());
        h.update(block.to_le_bytes());
        h.update((hotkey.len() as u32).to_le_bytes());
        h.update(hotkey.as_bytes());
    }
    hex::encode(h.finalize())
}

fn hotkey_of(hex_text: &str) -> Option<AccountId> {
    let b: [u8; 32] = hex::decode(hex_text).ok()?.try_into().ok()?;
    Some(AccountId(b))
}

/// The journaled record as `/status` shows it.
#[derive(Clone, Debug, Serialize)]
pub struct RowRecordView {
    pub epoch: Option<u64>,
    pub target: Option<String>,
    pub authorization: Option<String>,
    pub sha256: Option<String>,
    pub age_ms: Option<u64>,
    /// The last signing failure's code and how long its run has lasted.
    pub error: Option<String>,
    pub error_age_ms: Option<u64>,
    /// The installed authorization no record covers yet, how long it has
    /// waited and on what (`row_waiting_derivation`, `row_waiting_issue`,
    /// `row_hotkeys_missing`, a signing failure's code). Absent while the
    /// record covers the installed authorization.
    pub pending_authorization: Option<String>,
    pub pending_age_ms: Option<u64>,
    pub pending_code: Option<String>,
    /// The row snapshot step: the commit epoch of its last schedule read,
    /// the finalized block it read at, the epoch it last derived a row for
    /// (or ran without one) and why it produced none (`code`).
    pub snapshot_epoch: Option<u64>,
    pub snapshot_block: Option<u64>,
    pub snapshot_derived_epoch: Option<u64>,
    pub snapshot_code: Option<String>,
}

pub fn view(sql: &impl Sql, now_ms: u64) -> Result<Option<RowRecordView>, StoreError> {
    let rows = sql.exec(
        "SELECT epoch, target, authorization, sha256, signed_at_ms, error, error_since_ms,
             pending_authorization, pending_since_ms, pending_code FROM sign_row_record WHERE id = 1",
        &[],
    )?;
    let snaps = sql.exec(
        "SELECT epoch, block_number, derived_epoch, code FROM sign_row_snapshot WHERE id = 1",
        &[],
    )?;
    if rows.is_empty() && snaps.is_empty() {
        return Ok(None);
    }
    let opt_u = |r: Option<&crate::sql::Row>, i: usize| match r.and_then(|r| r.get(i)) {
        Some(SqlValue::Integer(v)) => Some(*v as u64),
        _ => None,
    };
    let opt_t = |r: Option<&crate::sql::Row>, i: usize| r.and_then(|r| text(r, i).ok().map(str::to_owned));
    let (r, n) = (rows.first(), snaps.first());
    Ok(Some(RowRecordView {
        epoch: opt_u(r, 0),
        target: opt_t(r, 1),
        authorization: opt_t(r, 2),
        sha256: opt_t(r, 3),
        age_ms: opt_u(r, 4).map(|t| now_ms.saturating_sub(t)),
        error: opt_t(r, 5),
        error_age_ms: opt_u(r, 6).map(|t| now_ms.saturating_sub(t)),
        pending_authorization: opt_t(r, 7),
        pending_age_ms: opt_u(r, 8).map(|t| now_ms.saturating_sub(t)),
        pending_code: opt_t(r, 9),
        snapshot_epoch: opt_u(n, 0),
        snapshot_block: opt_u(n, 1),
        snapshot_derived_epoch: opt_u(n, 2),
        snapshot_code: opt_t(n, 3),
    }))
}

/// The record's digest, for tests and logs.
pub fn digest_of(signed: &[u8]) -> Option<String> {
    slop_ninja_protocol::Signed::<CoordinatorRow>::from_bytes(signed)
        .ok()
        .and_then(|s| s.record.digest().ok())
        .map(|d| d.to_string())
}

// ------------------------------------------------------- the row snapshot

/// What the row snapshot's schedule read asks for (pallet, item, key kind
/// as [`super::item_key`] takes it): the runtime that wrote the block, the
/// subnet's existence, its epoch schedule and its population.
pub(super) const ROW_SCHEDULE_ITEMS: &[(&str, &str, u8)] = &[
    ("System", "LastRuntimeUpgrade", 0),
    (crate::chain::SUBTENSOR, "NetworksAdded", 1),
    (crate::chain::SUBTENSOR, "Tempo", 1),
    (crate::chain::SUBTENSOR, "LastEpochBlock", 1),
    (crate::chain::SUBTENSOR, "PendingEpochAt", 1),
    (crate::chain::SUBTENSOR, "SubnetEpochIndex", 1),
    (crate::chain::SUBTENSOR, "BlocksSinceLastStep", 1),
    (crate::chain::SUBTENSOR, "SubnetworkN", 1),
];

/// The row snapshot step's journaled state (`sign_row_snapshot`).
struct SnapshotState {
    target: String,
    block: u64,
    tempo: u16,
    last_epoch_block: u64,
    pending_epoch_at: u64,
    subnet_epoch_index: u64,
    blocks_since_last_step: u64,
    epoch: u64,
    derived_epoch: Option<u64>,
}

fn snapshot_state(sql: &impl Sql) -> Result<Option<SnapshotState>, StoreError> {
    let rows = sql.exec(
        "SELECT target, block_number, tempo, last_epoch_block, pending_epoch_at, subnet_epoch_index,
             blocks_since_last_step, epoch, derived_epoch FROM sign_row_snapshot WHERE id = 1",
        &[],
    )?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(SnapshotState {
        target: text(r, 0)?.to_owned(),
        block: uint(r, 1)?,
        tempo: u16::try_from(uint(r, 2)?).unwrap_or(u16::MAX),
        last_epoch_block: uint(r, 3)?,
        pending_epoch_at: uint(r, 4)?,
        subnet_epoch_index: uint(r, 5)?,
        blocks_since_last_step: uint(r, 6)?,
        epoch: uint(r, 7)?,
        derived_epoch: match r.get(8) {
            Some(SqlValue::Integer(v)) => Some(*v as u64),
            _ => None,
        },
    }))
}

/// Whether the row snapshot step is due at confirmed block `c`: a reward-v1
/// target is installed and no derivation ran for it at the commit epoch `c`
/// is in (predicted from the last schedule read, which is refreshed at least
/// once a tempo). So the row other validators copy is derived once per
/// tempo boundary from a finalized snapshot, whatever the coordinator's own
/// signing pipeline does: signing paused (an operator hold, a deploy, a
/// disabled commit/reveal flag), a signer fault, a preflight deferral.
pub(super) fn snapshot_due(sql: &impl Sql, c: crate::wire::BlockRef) -> Result<bool, StoreError> {
    let Some(target) = sql
        .exec("SELECT target FROM sign_targets WHERE active = 1", &[])?
        .first()
        .map(|r| text(r, 0).map(str::to_owned))
        .transpose()?
    else {
        return Ok(false);
    };
    let s = match snapshot_state(sql)? {
        Some(s) if s.target == target => s,
        // A target not read for yet: due when it is a reward-v1 one (only
        // then is the bundle read).
        _ => {
            return Ok(super::signer_target_bundle(sql)?
                .is_some_and(|(_, bundle)| crate::target::is_v2(bundle.as_bytes())));
        }
    };
    if c.number <= s.block {
        return Ok(false);
    }
    if c.number - s.block > u64::from(s.tempo).max(1) || s.derived_epoch != Some(s.epoch) {
        return Ok(true);
    }
    let base = crate::chain::schedule(
        s.tempo,
        s.last_epoch_block,
        s.pending_epoch_at,
        s.subnet_epoch_index,
        s.blocks_since_last_step,
        s.block,
    );
    let state = rtao_core::timelock::epoch_schedule::advance_blocks(&base, s.block + 1, c.number);
    Ok(crate::chain::commit_epoch(&state) > s.epoch)
}

/// Marks the derivation of the snapshot's epoch done without a row (`code`
/// says why), so the step waits for the next epoch.
fn snapshot_done_without_row(sql: &impl Sql, epoch: u64, code: &str, now_ms: u64) -> Result<(), StoreError> {
    let previous = sql
        .exec("SELECT code FROM sign_row_snapshot WHERE id = 1", &[])?
        .first()
        .and_then(|r| text(r, 0).ok().map(str::to_owned));
    sql.exec(
        "UPDATE sign_row_snapshot SET derived_epoch = ?, code = ?, updated_at_ms = ? WHERE id = 1",
        &[bind_u64(epoch)?, code.into(), bind_u64(now_ms)?],
    )?;
    if previous.as_deref() != Some(code) {
        log(sql, now_ms, None, None, None, "row_snapshot_without_row", Some(code))?;
    }
    Ok(())
}

/// The schedule read: records the subnet schedule and the commit epoch at
/// `at`, then asks for the registrations at the same block unless this
/// epoch's row was already derived for the installed target.
pub(super) fn apply_schedule(
    sql: &impl Sql,
    ctx: &super::Ctx<'_>,
    rt: &crate::chain::ChainRuntime,
    at: crate::wire::BlockRef,
    values: &super::Values,
    now_ms: u64,
) -> Result<Option<super::Chained>, super::SignError> {
    use crate::chain::{ChainError, as_bool};
    let Some((target, _)) = super::signer_target_bundle(sql)? else {
        return Ok(None);
    };
    let mut v = std::collections::BTreeMap::new();
    for (p, i, k) in ROW_SCHEDULE_ITEMS {
        let key = super::item_key(rt, ctx.cfg, 0, p, i, *k)?;
        v.insert(*i, values.get(rt, p, i, &key)?);
    }
    if !as_bool(super::pick(&v, "NetworksAdded")?).unwrap_or(false) {
        return Err(super::SignError::Chain(ChainError::new("subnet_not_registered", "")));
    }
    let n = |item: &'static str| super::number(super::pick(&v, item)?, item);
    let tempo = u16::try_from(n("Tempo")?)
        .map_err(|_| super::SignError::Chain(ChainError::new("storage_shape", "tempo")))?;
    let (last_epoch_block, pending_epoch_at) = (n("LastEpochBlock")?, n("PendingEpochAt")?);
    let (subnet_epoch_index, blocks_since_last_step) = (n("SubnetEpochIndex")?, n("BlocksSinceLastStep")?);
    let population = u16::try_from(n("SubnetworkN")?).unwrap_or(u16::MAX);
    let state = crate::chain::schedule(
        tempo,
        last_epoch_block,
        pending_epoch_at,
        subnet_epoch_index,
        blocks_since_last_step,
        at.number,
    );
    let epoch = crate::chain::commit_epoch(&state);
    let previous = snapshot_state(sql)?;
    let same_target = previous.as_ref().is_some_and(|s| s.target == target);
    sql.exec(
        "INSERT INTO sign_row_snapshot (id, target, block_number, tempo, last_epoch_block, pending_epoch_at,
             subnet_epoch_index, blocks_since_last_step, epoch, updated_at_ms)
         VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET target = excluded.target, block_number = excluded.block_number,
             tempo = excluded.tempo, last_epoch_block = excluded.last_epoch_block,
             pending_epoch_at = excluded.pending_epoch_at, subnet_epoch_index = excluded.subnet_epoch_index,
             blocks_since_last_step = excluded.blocks_since_last_step, epoch = excluded.epoch,
             updated_at_ms = excluded.updated_at_ms",
        &[
            target.as_str().into(),
            bind_u64(at.number)?,
            SqlValue::Integer(tempo.into()),
            bind_u64(last_epoch_block)?,
            bind_u64(pending_epoch_at)?,
            bind_u64(subnet_epoch_index)?,
            bind_u64(blocks_since_last_step)?,
            bind_u64(epoch)?,
            bind_u64(now_ms)?,
        ],
    )?;
    if !same_target {
        // Another target: nothing derived for it yet.
        sql.exec(
            "UPDATE sign_row_snapshot SET derived_epoch = NULL, row_epoch = NULL, row_json = NULL, code = NULL
             WHERE id = 1",
            &[],
        )?;
    } else if previous.as_ref().and_then(|s| s.derived_epoch) == Some(epoch) {
        return Ok(None);
    }
    // A target that cannot be committed under (revoked, refused, not a
    // reward-v1 one) gets no row this epoch.
    let t = match super::active_target(sql, ctx.cfg, ctx.trusted)? {
        Some(Ok(t)) if t.digest == target => t,
        Some(Err(r)) => {
            snapshot_done_without_row(sql, epoch, r.code, now_ms)?;
            return Ok(None);
        }
        _ => {
            snapshot_done_without_row(sql, epoch, "row_target_changed", now_ms)?;
            return Ok(None);
        }
    };
    if !matches!(t.verified, Verified::V2(_)) {
        snapshot_done_without_row(sql, epoch, "row_not_reward_v1", now_ms)?;
        return Ok(None);
    }
    Ok(Some(super::Chained::RowRegistration { at, epoch, population }))
}

/// The registration read: derives the installed target's row at `at` (the
/// same derivation as the coordinator's own commitment) and journals it as
/// the snapshot's row for commit epoch `epoch`; [`refresh`] signs it.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_registration(
    sql: &impl Sql,
    ctx: &super::Ctx<'_>,
    rt: &crate::chain::ChainRuntime,
    at: crate::wire::BlockRef,
    epoch: u64,
    population: u16,
    values: &super::Values,
    now_ms: u64,
) -> Result<Option<super::Chained>, super::SignError> {
    use crate::chain::{ChainError, as_account};
    let chain_err = |code: &'static str| super::SignError::Chain(ChainError::new(code, ""));
    let Some(s) = snapshot_state(sql)? else {
        return Ok(None);
    };
    if s.epoch != epoch || s.block != at.number {
        // A later schedule read replaced this one.
        return Ok(None);
    }
    let t = match super::active_target(sql, ctx.cfg, ctx.trusted)? {
        Some(Ok(t)) if t.digest == s.target => t,
        _ => return Ok(None),
    };
    let Verified::V2(v) = &t.verified else {
        return Ok(None);
    };
    let Some(neurons) = super::read_neurons(rt, ctx.cfg, population, values)? else {
        return Err(chain_err("registration_unreadable"));
    };
    let ok = super::owner_key(rt, ctx.cfg)?;
    let Some(owner) = as_account(&values.get(rt, crate::chain::SUBTENSOR, "SubnetOwnerHotkey", &ok)?) else {
        return Err(chain_err("owner_hotkey_unreadable"));
    };
    let snapshot = crate::target::snapshot_v2(&v.chain, at.number, at.hash.0, owner, neurons)
        .map_err(|r| super::SignError::Chain(ChainError::new(r.code, r.detail)))?;
    let summary = match super::paid_summary_v2(v, &snapshot) {
        Ok(summary) => summary,
        Err(code) if code == super::ROW_DEFERRED_OWNER => {
            snapshot_done_without_row(sql, epoch, code, now_ms)?;
            return Ok(None);
        }
        Err(code) => return Err(chain_err(code)),
    };
    sql.exec(
        "UPDATE sign_row_snapshot SET derived_epoch = ?, row_epoch = ?, row_json = ?, code = NULL, updated_at_ms = ?
         WHERE id = 1",
        &[
            bind_u64(epoch)?,
            bind_u64(epoch)?,
            serde_json::to_string(&summary).map_err(|_| StoreError("row json"))?.into(),
            bind_u64(now_ms)?,
        ],
    )?;
    Ok(None)
}

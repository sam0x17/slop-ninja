//! Signed revocations and the private authorization pointer.
//!
//! No authorization expires. The coordinator commits its installed target
//! for as long as that target is the held active one, whatever time has
//! passed, until a newer bundle supersedes it or a signed revocation
//! withdraws it. A revocation ([`install_revocation`]) is checked under the
//! trusted authorization key and this chain, its `issued_epoch` must not pass
//! the finalized epoch index by more than one (a re-authorization must be
//! issued later than the revocation and cannot commit before its own issue,
//! so a mistyped far-future epoch would withdraw the target for that long),
//! and it is journaled in `sign_revocations` (only ever added) and takes
//! effect in the same transaction: an installed target whose authorization
//! it withdraws is refused from then on (`authorization_revoked` from
//! [`super::active_target`] and every per-epoch operation), and its preparing
//! operations, never signed, fail with that code. Committed operations keep
//! their read-back and reveal checks. A journaled revocation is not verified
//! again ([`slop_ninja_protocol::RewardIntentRevocation::journaled`]), and
//! only the one with the latest `issued_epoch` for the installed target is
//! read ([`revocation_of`]): it withdraws whatever an earlier one of that
//! target does, so each check reads one entry of the `(target,
//! issued_epoch, seq)` index (schema v10) however many are journaled.
//!
//! The pointer ([`pointer_plan`]) is `authorizations/current.json` in the
//! receipts bucket, the publisher's source for the public copy that
//! validators fetch: the installed bundle's authorization, its renew-by hint
//! refresh if it carries one, and the revocation set, each stored as
//! `authorizations/<sha256>.signed`. The set is the concatenation of the
//! latest journaled revocation of every revoked target signed under the
//! trusted authorization key, oldest first, each of the schema's fixed
//! signed length; a validator that missed earlier revocations journals them
//! all from it, whatever target it holds. The plan names each object by
//! key, digest and length only; the Worker fetches an object's bytes
//! ([`pointer_object`]) only when the bucket lacks it. The Worker's cron tick
//! reads the stored pointer back every minute and rewrites it (objects
//! first) whenever its bytes differ, and checks every object it names on
//! every tick, rewriting one that is missing; a successful revocation also
//! runs the same pass at once. Every pass reports its outcome
//! ([`pointer_outcome`]): the pointer it confirmed stored, or the failure it
//! met, so `/status` shows when the stored pointer was last confirmed equal
//! to the plan and what has failed since ([`AuthorizationView`]), and the
//! publisher alerts on a writer that has not confirmed it for a tempo. A lost
//! write, a deleted object or another writer's copy is corrected at the
//! next pass. The pointer has no validity window.

use super::{fail_op, log, ops_where, signer_target_bundle};
use crate::journal;
use crate::sql::{Sql, StoreError, bind_u64, text, uint};
use crate::target;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use slop_ninja_admission_wire::json::{AUTHORIZATION_POINTER_SCHEMA, AuthorizationPointer, Chain, ObjectRef};
use slop_ninja_protocol::{ChainBinding, Digest, Record, RewardIntentRevocation, Signed, TrustedKeys};

/// The pointer's key in the receipts bucket: the shared contract's key,
/// which the retention hold and the publisher read.
pub const POINTER_KEY: &str = slop_ninja_admission_wire::receipts::AUTHORIZATION_POINTER_KEY;

/// A signed authorization, renewal or revocation object's key.
pub fn object_key(sha256: &str) -> String {
    format!("authorizations/{sha256}.signed")
}

fn chain_of(cfg: &super::SignConfig) -> ChainBinding {
    ChainBinding {
        genesis: Digest(cfg.genesis.0),
        netuid: cfg.netuid,
        mechanism: 0,
    }
}

fn refuse<T>(code: &'static str, detail: impl Into<String>) -> Result<Result<T, target::Refusal>, StoreError> {
    Ok(Err(target::Refusal {
        code,
        detail: detail.into(),
    }))
}

fn decode(signed_hex: &str) -> Result<Signed<RewardIntentRevocation>, StoreError> {
    let bytes = hex::decode(signed_hex).map_err(|_| StoreError("revocation hex"))?;
    Signed::<RewardIntentRevocation>::from_bytes(&bytes).map_err(|_| StoreError("revocation record"))
}

/// The journaled revocation of `target` with the latest `issued_epoch`
/// (then the latest row), signed, if any: one entry of the target index.
fn latest_of(sql: &impl Sql, target: &str) -> Result<Option<Signed<RewardIntentRevocation>>, StoreError> {
    sql.exec(
        "SELECT signed_hex FROM sign_revocations WHERE target = ? ORDER BY issued_epoch DESC, seq DESC LIMIT 1",
        &[target.into()],
    )?
    .first()
    .map(|r| decode(text(r, 0)?))
    .transpose()
}

/// The journaled revocation of `target` with the latest `issued_epoch`, if
/// any: the only one a check of that target needs, because it withdraws
/// every authorization an earlier one does. Each was checked at install and
/// is not verified again (module docs). At most one record, read through
/// the target index.
pub fn revocation_of(sql: &impl Sql, target: &str) -> Result<Vec<RewardIntentRevocation>, StoreError> {
    Ok(latest_of(sql, target)?.map(|s| s.record).into_iter().collect())
}

/// What the revocation set may occupy. The set is one Durable Object reply
/// that the routes driving the pointer read whole: the cron tick
/// (`CONTINUATION_CONTROL_BYTES`) and the operator's revoke route, which
/// runs the same pass at once (`WORKER_CONTROL_BYTES`). A reply a route holds
/// whole is bounded by its control footprint (`cf.rs`), so the smaller one
/// bounds the set. The journal builds the set and the plan within its own
/// footprint (`JOURNAL_CONTROL_BYTES`): the stored hex rows and the set they
/// decode to, three times the set at most, checked at full capacity by the
/// arena witness test.
pub const REVOCATION_SET_BYTES: usize = crate::arena_routes::WORKER_CONTROL_BYTES;

/// How many revoked targets the revocation set can carry: whole signed
/// records of the schema's fixed length within [`REVOCATION_SET_BYTES`].
/// Not a stop condition: a target that can no longer be revoked is still
/// withdrawn by installing a successor bundle, which every validator adopts.
pub fn revocation_set_capacity(signed_len: usize) -> usize {
    REVOCATION_SET_BYTES / signed_len.max(1)
}

/// The subnet epoch index at the finalized cursor: the last preflight's
/// schedule advanced to the cursor block. `None` before any preflight (see
/// [`install_revocation`] for the Worker's RPC reading used then).
pub fn finalized_epoch(sql: &impl Sql) -> Result<Option<u64>, StoreError> {
    let rows = sql.exec(
        "SELECT block_number, tempo, last_epoch_block, pending_epoch_at, subnet_epoch_index,
                blocks_since_last_step FROM chain_schedule WHERE id = 1",
        &[],
    )?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    let at_block = uint(r, 0)?;
    let base = crate::chain::schedule(
        uint(r, 1)? as u16,
        uint(r, 2)?,
        uint(r, 3)?,
        uint(r, 4)?,
        uint(r, 5)?,
        at_block,
    );
    let cursor = journal::cursor(sql)?.map_or(at_block, |c| c.number);
    if cursor <= at_block {
        return Ok(Some(base.subnet_epoch_index));
    }
    Ok(Some(
        rtao_core::timelock::epoch_schedule::advance_blocks(&base, at_block + 1, cursor).subnet_epoch_index,
    ))
}

/// What [`install_revocation`] did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Revoked {
    pub revocation: String,
    pub target: String,
    pub issued_epoch: u64,
    /// `false` when it was already journaled.
    pub new: bool,
    /// Whether it withdraws the installed target's authorization.
    pub withdraws_installed: bool,
    /// Preparing operations of the installed target failed by it.
    pub failed_ops: u64,
}

/// Journals the signed revocation `signed_hex` after checking it under
/// `keys` and this chain, and applies it at once (module docs). A refusal
/// changes nothing. Allowed whether signing is paused or active.
///
/// The issue-epoch bound reads the finalized epoch index from the journal's
/// own schedule ([`finalized_epoch`]) and from `observed_epoch`, the
/// Worker's RPC reading of `SubnetEpochIndex` at the finalized cursor block
/// taken just before the call, whichever is later; so a journal that never
/// ran a signing preflight (paused since it was created) still accepts a
/// revocation. With neither, it is refused (`revocation_epoch_unknown`); with
/// a cursor lagging the chain by more than an epoch (catch-up, webhook
/// silence), one dated at the chain's current epoch is refused
/// (`revocation_epoch_future`) until the cursor catches up. Both refusals
/// say to retry; neither journals anything.
pub fn install_revocation(
    sql: &impl Sql,
    cfg: &super::SignConfig,
    keys: &TrustedKeys,
    signed_hex: &str,
    observed_epoch: Option<u64>,
    now_ms: u64,
) -> Result<Result<Revoked, target::Refusal>, StoreError> {
    let Ok(bytes) = hex::decode(signed_hex.trim()) else {
        return refuse("revocation_malformed", "the body is not hex");
    };
    let signed = match Signed::<RewardIntentRevocation>::from_bytes(&bytes) {
        Ok(s) => s,
        Err(e) => return refuse("revocation_malformed", e.to_string()),
    };
    let checked = match RewardIntentRevocation::check(&signed, keys, &chain_of(cfg)) {
        Ok(c) => c,
        Err(e) => return refuse("revocation_refused", format!("{e} ({})", e.code())),
    };
    let r = checked.revocation();
    let digest = checked.digest().to_string();
    let new = sql
        .exec("SELECT 1 FROM sign_revocations WHERE digest = ?", &[digest.as_str().into()])?
        .is_empty();
    if new {
        // A far-future epoch would withdraw the target until a later
        // authorization can commit (module docs): refused, from chain state.
        match finalized_epoch(sql)?.max(observed_epoch) {
            None => {
                return refuse(
                    "revocation_epoch_unknown",
                    "no finalized epoch index is known (no signing preflight has run and the RPC reading failed); retry",
                );
            }
            Some(finalized) if r.issued_epoch > finalized.saturating_add(1) => {
                return refuse(
                    "revocation_epoch_future",
                    format!(
                        "issued epoch {} is past the finalized epoch index {finalized} plus one; retry once the cursor catches up",
                        r.issued_epoch
                    ),
                );
            }
            Some(_) => {}
        }
        // The revocation set carries one entry per revoked target whose
        // latest revocation is under the trusted key; this one adds an entry
        // only when it becomes its target's latest and that target had none
        // carried, and the set must still fit what the Worker reads whole.
        let target = r.target.to_string();
        let adds = match latest_of(sql, &target)? {
            None => true,
            Some(prev) => r.issued_epoch >= prev.record.issued_epoch && !carried(&prev, keys),
        };
        if adds {
            let carried_now = revocation_set(sql, cfg, keys)?.map_or(0, |set| set.len() / bytes.len().max(1));
            let capacity = revocation_set_capacity(bytes.len());
            if carried_now >= capacity {
                return refuse(
                    "revocation_set_full",
                    format!(
                        "{carried_now} revoked targets fill the {REVOCATION_SET_BYTES}-byte revocation set; \
                         withdraw this target by installing a successor bundle instead"
                    ),
                );
            }
        }
        sql.exec(
            "INSERT INTO sign_revocations (digest, target, authorization, issued_epoch, signed_hex, at_ms)
             VALUES (?, ?, ?, ?, ?, ?)",
            &[
                digest.as_str().into(),
                r.target.to_string().into(),
                r.authorization.to_string().into(),
                bind_u64(r.issued_epoch)?,
                hex::encode(&bytes).into(),
                bind_u64(now_ms)?,
            ],
        )?;
        journal::bump(sql, "sign_revocations")?;
    }
    // Whether the installed target's authorization is withdrawn now.
    let installed = installed_terms(sql, cfg, keys)?;
    let withdraws_installed = installed
        .as_ref()
        .is_some_and(|(target, terms)| *target == r.target.to_string() && terms.issued_epoch <= r.issued_epoch);
    let mut failed_ops = 0;
    if withdraws_installed && let Some((target, _)) = &installed {
        for o in ops_where(sql, "state = 'preparing'")? {
            let rows = sql.exec("SELECT target FROM commit_ops WHERE op_id = ?", &[o.op_id.as_str().into()])?;
            if rows.first().map(|row| text(row, 0)).transpose()? == Some(target.as_str()) {
                fail_op(sql, &o.op_id, "authorization_revoked", now_ms)?;
                failed_ops += 1;
            }
        }
    }
    log(
        sql,
        now_ms,
        None,
        None,
        None,
        if withdraws_installed { "authorization_revoked" } else { "revocation_recorded" },
        Some(&format!("{digest} target {} through epoch {}", r.target, r.issued_epoch)),
    )?;
    Ok(Ok(Revoked {
        revocation: digest,
        target: r.target.to_string(),
        issued_epoch: r.issued_epoch,
        new,
        withdraws_installed,
        failed_ops,
    }))
}

/// The installed reward-v1 bundle's target digest and checked
/// authorization terms, or `None` (no target, a T12 v1 bundle, or a bundle
/// that no longer verifies, which [`super::active_target`] reports).
fn installed_terms(
    sql: &impl Sql,
    cfg: &super::SignConfig,
    keys: &TrustedKeys,
) -> Result<Option<(String, slop_ninja_protocol::AuthorizationTerms)>, StoreError> {
    let Some((digest, bundle)) = signer_target_bundle(sql)? else {
        return Ok(None);
    };
    if !target::is_v2(bundle.as_bytes()) {
        return Ok(None);
    }
    let held = super::held_target(sql, cfg)?;
    let Ok(verified) = target::parse_v2(bundle.as_bytes())
        .and_then(|b| target::verify_v2(&b, keys, &cfg.genesis.0, cfg.netuid, &held))
    else {
        return Ok(None);
    };
    Ok(verified.checked_terms(keys).ok().map(|terms| (digest, terms)))
}

/// The pointer the receipts bucket should hold now ([`pointer_plan`]).
/// Objects are named, never carried: the Worker fetches the bytes of one
/// the bucket lacks ([`pointer_object`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerPlan {
    /// SHA-256 (hex) of `body`.
    pub sha256: String,
    /// The pointer's exact bytes (pretty JSON).
    pub body: String,
    /// Every object the pointer names (key, digest, length), to store
    /// before the pointer.
    pub objects: Vec<ObjectRef>,
    /// The SHA-256 of the pointer last confirmed written, if any.
    pub written: Option<String>,
}

/// Whether a journaled revocation is carried in the set: signed under the
/// trusted authorization key. Its signature was verified under that key
/// when it was journaled, so the key comparison is the whole check; one
/// signed under a rotated trust root is left out (it withdraws only
/// authorizations signed under the old key, which no longer verify either).
fn carried(signed: &Signed<RewardIntentRevocation>, keys: &TrustedKeys) -> bool {
    signed.public_key == *keys.key(slop_ninja_protocol::KeyRole::Authorization)
}

/// Rows [`revocation_set`] reads per query: a batching step that keeps the
/// journal's peak at the set plus one batch of hex rows, not a limit on the
/// set.
const REVOCATION_ROWS_PER_READ: u64 = 256;

/// The revocation set (module docs): the latest journaled revocation of
/// each revoked target signed under the trusted authorization key
/// ([`carried`]), oldest first, concatenated; `None` while there is none.
/// The rows are read in batches through the primary key (each row's
/// "latest of its target" test reads the target index), so the journal
/// holds the set and one batch of hex rows at once.
pub fn revocation_set(
    sql: &impl Sql,
    cfg: &super::SignConfig,
    keys: &TrustedKeys,
) -> Result<Option<Vec<u8>>, StoreError> {
    // Reserved once at its largest (every target's latest carried), so the
    // set never holds a doubling buffer's old copy while it grows.
    let targets = sql
        .exec("SELECT COUNT(DISTINCT target) FROM sign_revocations", &[])?
        .first()
        .map(|r| uint(r, 0))
        .transpose()?
        .unwrap_or(0);
    let len = slop_ninja_admission_wire::receipts::revocation_len(&chain_of(cfg));
    let mut set = Vec::new();
    set.try_reserve_exact(usize::try_from(targets.saturating_mul(len)).map_err(|_| StoreError("revocation set"))?)
        .map_err(|_| StoreError("revocation set"))?;
    let mut after = 0u64;
    loop {
        let rows = sql.exec(
            "SELECT r.seq, r.signed_hex FROM sign_revocations r
             WHERE r.seq > ? AND NOT EXISTS (
                 SELECT 1 FROM sign_revocations n
                 WHERE n.target = r.target
                   AND (n.issued_epoch > r.issued_epoch OR (n.issued_epoch = r.issued_epoch AND n.seq > r.seq)))
             ORDER BY r.seq LIMIT ?",
            &[bind_u64(after)?, bind_u64(REVOCATION_ROWS_PER_READ)?],
        )?;
        let Some(last) = rows.last() else { break };
        after = uint(last, 0)?;
        for row in &rows {
            let signed = decode(text(row, 1)?)?;
            if !carried(&signed, keys) {
                continue;
            }
            set.extend(signed.to_bytes().map_err(|_| StoreError("revocation bytes"))?);
        }
        if (rows.len() as u64) < REVOCATION_ROWS_PER_READ {
            break;
        }
    }
    Ok((!set.is_empty()).then_some(set))
}

/// The installed reward-v1 bundle's pointer parts: kind, authorization,
/// renewal and revocation set bytes. `None` while no reward-v1 bundle is
/// installed.
struct Parts {
    kind: String,
    authorization: Vec<u8>,
    renewal: Option<Vec<u8>>,
    revocations: Option<Vec<u8>>,
}

fn parts(sql: &impl Sql, cfg: &super::SignConfig, keys: &TrustedKeys, with_set: bool) -> Result<Option<Parts>, StoreError> {
    let Some((_, bundle)) = signer_target_bundle(sql)? else {
        return Ok(None);
    };
    if !target::is_v2(bundle.as_bytes()) {
        return Ok(None);
    }
    let Ok(b) = target::parse_v2(bundle.as_bytes()) else {
        return Ok(None);
    };
    let unhex = |h: &str| hex::decode(h).map_err(|_| StoreError("bundle authorization hex"));
    let authorization = unhex(&b.authorization.authorization)?;
    let renewal = b.authorization.renewal.as_deref().map(unhex).transpose()?;
    let kind = b.authorization.kind.clone();
    drop(b);
    drop(bundle);
    let revocations = if with_set { revocation_set(sql, cfg, keys)? } else { None };
    Ok(Some(Parts { kind, authorization, renewal, revocations }))
}

fn object_ref(bytes: &[u8]) -> ObjectRef {
    let sha = hex::encode(Sha256::digest(bytes));
    ObjectRef {
        path: object_key(&sha),
        sha256: sha,
        bytes: bytes.len() as u64,
    }
}

/// The pointer for the installed reward-v1 bundle, with the revocation set;
/// `None` while no reward-v1 bundle is installed. Built from stored records
/// only (each was verified when installed); the publisher and every
/// validator verify them again.
pub fn pointer_plan(
    sql: &impl Sql,
    cfg: &super::SignConfig,
    keys: &TrustedKeys,
) -> Result<Option<PointerPlan>, StoreError> {
    let Some(p) = parts(sql, cfg, keys, true)? else {
        return Ok(None);
    };
    let authorization = object_ref(&p.authorization);
    let renewal = p.renewal.as_deref().map(object_ref);
    let revocations = p.revocations.as_deref().map(object_ref);
    drop(p.revocations);
    let objects: Vec<ObjectRef> =
        core::iter::once(&authorization).chain(renewal.as_ref()).chain(revocations.as_ref()).cloned().collect();
    let pointer = AuthorizationPointer {
        schema: AUTHORIZATION_POINTER_SCHEMA.into(),
        chain: Chain::from_binding(&chain_of(cfg)),
        kind: p.kind,
        authorization,
        renewal,
        revocations,
    };
    let body = serde_json::to_string_pretty(&pointer).map_err(|_| StoreError("pointer json"))?;
    let written = sql
        .exec("SELECT sha256 FROM sign_pointer WHERE id = 1", &[])?
        .first()
        .map(|r| text(r, 0).map(str::to_owned))
        .transpose()?;
    Ok(Some(PointerPlan {
        sha256: hex::encode(Sha256::digest(body.as_bytes())),
        body,
        objects,
        written,
    }))
}

/// The bytes of the object the current pointer names with SHA-256
/// `sha256` (hex), for the Worker to store; `None` when the pointer names no
/// such object now. The revocation set is built only when the
/// authorization and renewal do not match.
pub fn pointer_object(
    sql: &impl Sql,
    cfg: &super::SignConfig,
    keys: &TrustedKeys,
    sha256: &str,
) -> Result<Option<Vec<u8>>, StoreError> {
    let matches = |b: &[u8]| hex::encode(Sha256::digest(b)) == sha256;
    let Some(p) = parts(sql, cfg, keys, false)? else {
        return Ok(None);
    };
    if matches(&p.authorization) {
        return Ok(Some(p.authorization));
    }
    if let Some(r) = p.renewal.filter(|r| matches(r)) {
        return Ok(Some(r));
    }
    drop(p.authorization);
    Ok(revocation_set(sql, cfg, keys)?.filter(|s| matches(s)))
}

/// One pass of the Worker's pointer writer (`/sign-pointer-outcome`): the
/// SHA-256 of the pointer it confirmed stored (read back equal to the plan,
/// every named object present at its length), or the failure that stopped
/// it (a short code, see `cf.rs::pointer_drive`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointerOutcome {
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// The longest failure code a pass records (codes are fixed identifiers).
pub const POINTER_ERROR_BYTES: usize = 64;

/// Records one pointer pass ([`PointerOutcome`]). A confirmation records the
/// pointer as written (logged when it changes, as before), refreshes the
/// confirmation time and clears the failure; a failure keeps the last
/// confirmation and records its code with the time its run of failures
/// began. Logs only transitions (a new pointer, a new failure code, a
/// recovery), never every pass. Returns whether the written pointer changed.
pub fn pointer_outcome(sql: &impl Sql, o: &PointerOutcome, now_ms: u64) -> Result<bool, StoreError> {
    let row = sql.exec("SELECT error FROM sign_pointer_drive WHERE id = 1", &[])?;
    let previous_error = row.first().and_then(|r| text(r, 0).ok().map(str::to_owned));
    let sha = o
        .sha256
        .as_deref()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    match (sha, &o.error) {
        (Some(sha), None) => {
            let changed = pointer_written(sql, sha, now_ms)?;
            sql.exec(
                "INSERT INTO sign_pointer_drive (id, confirmed_sha256, confirmed_at_ms, error, error_since_ms)
                 VALUES (1, ?, ?, NULL, NULL)
                 ON CONFLICT(id) DO UPDATE SET confirmed_sha256 = excluded.confirmed_sha256,
                     confirmed_at_ms = excluded.confirmed_at_ms, error = NULL, error_since_ms = NULL",
                &[sha.into(), bind_u64(now_ms)?],
            )?;
            if let Some(e) = previous_error {
                log(sql, now_ms, None, None, None, "authorization_pointer_recovered", Some(&format!("after {e}")))?;
            }
            Ok(changed)
        }
        (_, error) => {
            let mut code = error.as_deref().unwrap_or("outcome_malformed").to_owned();
            code.truncate(POINTER_ERROR_BYTES);
            if !code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                code = "outcome_malformed".into();
            }
            sql.exec(
                "INSERT INTO sign_pointer_drive (id, confirmed_sha256, confirmed_at_ms, error, error_since_ms)
                 VALUES (1, NULL, NULL, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET error = excluded.error,
                     error_since_ms = COALESCE(sign_pointer_drive.error_since_ms, excluded.error_since_ms)",
                &[code.as_str().into(), bind_u64(now_ms)?],
            )?;
            if previous_error.as_deref() != Some(code.as_str()) {
                log(sql, now_ms, None, None, None, "authorization_pointer_failed", Some(&code))?;
            }
            Ok(false)
        }
    }
}

/// Records that the pointer with body SHA-256 `sha256` was written and read
/// back. Logs only a change.
pub fn pointer_written(sql: &impl Sql, sha256: &str, now_ms: u64) -> Result<bool, StoreError> {
    let previous = sql
        .exec("SELECT sha256 FROM sign_pointer WHERE id = 1", &[])?
        .first()
        .map(|r| text(r, 0).map(str::to_owned))
        .transpose()?;
    let changed = previous.as_deref() != Some(sha256);
    if changed {
        sql.exec(
            "INSERT INTO sign_pointer (id, sha256, written_at_ms) VALUES (1, ?, ?)
             ON CONFLICT(id) DO UPDATE SET sha256 = excluded.sha256, written_at_ms = excluded.written_at_ms",
            &[sha256.into(), bind_u64(now_ms)?],
        )?;
        log(sql, now_ms, None, None, None, "authorization_pointer_written", Some(sha256))?;
    }
    Ok(changed)
}

/// The installed authorization as `/status` shows it: decoded from the
/// stored bundle for display (it was verified at install, and every
/// operation verifies it again), never a reason to commit.
#[derive(Clone, Debug, Serialize)]
pub struct AuthorizationView {
    pub kind: String,
    pub digest: String,
    pub issued_epoch: u64,
    /// Non-binding: nothing pauses when it passes.
    pub renew_by_epoch: u64,
    /// The journaled revocation that withdraws it, if any.
    pub revoked_by: Option<String>,
    pub revocations: u64,
    /// SHA-256 of the pointer last written (changed), and the age of that
    /// write.
    pub pointer_sha256: Option<String>,
    pub pointer_age_ms: Option<u64>,
    /// Whether the writer's last pass confirmed the stored pointer equal to
    /// its plan ([`pointer_outcome`]). Every healthy cron pass confirms it,
    /// so `pointer_confirmed_age_ms` stays within a cron period or two; the
    /// publisher alerts (`pointer_stale`) when it is false or older than two
    /// cron periods for a whole tempo.
    pub pointer_current: bool,
    /// Age of the last confirmation; `None` before the first.
    pub pointer_confirmed_age_ms: Option<u64>,
    /// The failure code of the last pass when it failed, and how long its
    /// run of failures has lasted.
    pub pointer_error: Option<String>,
    pub pointer_error_age_ms: Option<u64>,
}

/// See [`AuthorizationView`]; `None` while no reward-v1 bundle is installed.
pub fn view(sql: &impl Sql, now_ms: u64) -> Result<Option<AuthorizationView>, StoreError> {
    let Some((target_digest, bundle)) = signer_target_bundle(sql)? else {
        return Ok(None);
    };
    let Ok(b) = target::parse_v2(bundle.as_bytes()) else {
        return Ok(None);
    };
    let Ok(bytes) = hex::decode(&b.authorization.authorization) else {
        return Ok(None);
    };
    let (digest, issued_epoch, renew_by_epoch) = match b.authorization.kind.as_str() {
        "launch" => match Signed::<slop_ninja_protocol::LaunchAuthorization>::from_bytes(&bytes) {
            Ok(s) => (s.record.digest().map_err(|_| StoreError("digest"))?, s.record.issued_epoch, s.record.renew_by_epoch),
            Err(_) => return Ok(None),
        },
        _ => match Signed::<slop_ninja_protocol::RewardIntentAuthorization>::from_bytes(&bytes) {
            Ok(s) => (s.record.digest().map_err(|_| StoreError("digest"))?, s.record.issued_epoch, s.record.renew_by_epoch),
            Err(_) => return Ok(None),
        },
    };
    let revoked_by = sql
        .exec(
            "SELECT digest FROM sign_revocations WHERE target = ? AND issued_epoch >= ? ORDER BY seq LIMIT 1",
            &[target_digest.as_str().into(), bind_u64(issued_epoch)?],
        )?
        .first()
        .map(|r| text(r, 0).map(str::to_owned))
        .transpose()?;
    let revocations = sql
        .exec("SELECT COUNT(*) FROM sign_revocations", &[])?
        .first()
        .map(|r| uint(r, 0))
        .transpose()?
        .unwrap_or(0);
    let pointer = sql.exec("SELECT sha256, written_at_ms FROM sign_pointer WHERE id = 1", &[])?;
    let pointer = pointer.first();
    let drive = sql.exec("SELECT confirmed_at_ms, error, error_since_ms FROM sign_pointer_drive WHERE id = 1", &[])?;
    let drive = drive.first();
    let age = |r: &crate::sql::Row, i: usize| uint(r, i).ok().map(|t| now_ms.saturating_sub(t));
    let confirmed_age = drive.and_then(|r| age(r, 0));
    let pointer_error = drive.and_then(|r| text(r, 1).ok().map(str::to_owned));
    Ok(Some(AuthorizationView {
        kind: b.authorization.kind.clone(),
        digest: digest.to_string(),
        issued_epoch,
        renew_by_epoch,
        revoked_by,
        revocations,
        pointer_sha256: pointer.map(|r| text(r, 0).map(str::to_owned)).transpose()?,
        pointer_age_ms: pointer.map(|r| uint(r, 1).map(|t| now_ms.saturating_sub(t))).transpose()?,
        pointer_current: confirmed_age.is_some() && pointer_error.is_none(),
        pointer_confirmed_age_ms: confirmed_age,
        pointer_error_age_ms: drive.filter(|_| pointer_error.is_some()).and_then(|r| age(r, 2)),
        pointer_error,
    }))
}

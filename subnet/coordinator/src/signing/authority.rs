//! Signed revocations and the private authorization pointer.
//!
//! No authorization expires. The coordinator commits its installed target
//! for as long as that target is the held active one, whatever time has
//! passed, until a newer bundle supersedes it or a signed revocation
//! withdraws it. A revocation ([`install_revocation`]) is checked under the
//! trusted authorization key and this chain, journaled in `sign_revocations`
//! (only ever added) and takes effect in the same transaction: an installed
//! target whose authorization it withdraws is refused from then on
//! (`authorization_revoked` from [`super::active_target`] and every per-epoch
//! operation), and its preparing operations, never signed, fail with that
//! code. Committed operations keep their read-back and reveal checks.
//!
//! The pointer ([`pointer_plan`]) is `authorizations/current.json` in the
//! receipts bucket, the publisher's source for the public copy that
//! validators fetch: the installed bundle's authorization, its renew-by hint
//! refresh if it carries one, and the latest revocation, each stored as
//! `authorizations/<sha256>.signed`. The Worker's cron tick reads the stored
//! pointer back every minute and rewrites it (objects first) whenever its
//! bytes differ, then records the write ([`pointer_written`]); a lost write,
//! a deleted object or another writer's copy is corrected at the next tick.
//! The pointer has no validity window.

use super::{fail_op, log, ops_where, signer_target_bundle};
use crate::journal;
use crate::sql::{Sql, StoreError, bind_u64, text, uint};
use crate::target;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use slop_ninja_admission_wire::json::{AUTHORIZATION_POINTER_SCHEMA, AuthorizationPointer, Chain, ObjectRef};
use slop_ninja_protocol::{ChainBinding, Digest, Record, RewardIntentRevocation, Signed, TrustedKeys};

/// The pointer's key in the receipts bucket.
pub const POINTER_KEY: &str = "authorizations/current.json";

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

/// Every journaled revocation, oldest first, as stored (each was checked at
/// install).
pub fn revocations(sql: &impl Sql) -> Result<Vec<Signed<RewardIntentRevocation>>, StoreError> {
    sql.exec("SELECT signed_hex FROM sign_revocations ORDER BY seq", &[])?
        .iter()
        .map(|r| {
            let bytes = hex::decode(text(r, 0)?).map_err(|_| StoreError("revocation hex"))?;
            Signed::<RewardIntentRevocation>::from_bytes(&bytes).map_err(|_| StoreError("revocation record"))
        })
        .collect()
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
pub fn install_revocation(
    sql: &impl Sql,
    cfg: &super::SignConfig,
    keys: &TrustedKeys,
    signed_hex: &str,
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

/// One object the pointer names, to store before the pointer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerObject {
    pub key: String,
    pub hex: String,
}

/// The pointer the receipts bucket should hold now ([`pointer_plan`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerPlan {
    /// SHA-256 (hex) of `body`.
    pub sha256: String,
    /// The pointer's exact bytes (pretty JSON).
    pub body: String,
    pub objects: Vec<PointerObject>,
    /// The SHA-256 of the pointer last confirmed written, if any.
    pub written: Option<String>,
}

fn object(bytes: &[u8], objects: &mut Vec<PointerObject>) -> ObjectRef {
    let sha = hex::encode(Sha256::digest(bytes));
    let key = object_key(&sha);
    objects.push(PointerObject {
        key: key.clone(),
        hex: hex::encode(bytes),
    });
    ObjectRef {
        path: key,
        sha256: sha,
        bytes: bytes.len() as u64,
    }
}

/// The pointer for the installed reward-v1 bundle, with the latest
/// journaled revocation; `None` while no reward-v1 bundle is installed.
/// Built from stored records only (each was verified when installed); the
/// publisher and every validator verify them again.
pub fn pointer_plan(sql: &impl Sql, cfg: &super::SignConfig) -> Result<Option<PointerPlan>, StoreError> {
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
    let mut objects = Vec::new();
    let authorization = object(&unhex(&b.authorization.authorization)?, &mut objects);
    let renewal = match &b.authorization.renewal {
        Some(r) => Some(object(&unhex(r)?, &mut objects)),
        None => None,
    };
    let revocation = match sql
        .exec("SELECT signed_hex FROM sign_revocations ORDER BY seq DESC LIMIT 1", &[])?
        .first()
    {
        Some(row) => Some(object(&unhex(text(row, 0)?)?, &mut objects)),
        None => None,
    };
    let pointer = AuthorizationPointer {
        schema: AUTHORIZATION_POINTER_SCHEMA.into(),
        chain: Chain::from_binding(&chain_of(cfg)),
        kind: b.authorization.kind.clone(),
        authorization,
        renewal,
        revocation,
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

/// Records that the pointer with body SHA-256 `sha256` was written and read
/// back. Logs only a change.
pub fn pointer_written(sql: &impl Sql, sha256: &str, now_ms: u64) -> Result<bool, StoreError> {
    let previous = sql
        .exec("SELECT sha256 FROM sign_pointer WHERE id = 1", &[])?
        .first()
        .map(|r| text(r, 0).map(str::to_owned))
        .transpose()?;
    sql.exec(
        "INSERT INTO sign_pointer (id, sha256, written_at_ms) VALUES (1, ?, ?)
         ON CONFLICT(id) DO UPDATE SET sha256 = excluded.sha256, written_at_ms = excluded.written_at_ms",
        &[sha256.into(), bind_u64(now_ms)?],
    )?;
    let changed = previous.as_deref() != Some(sha256);
    if changed {
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
    /// SHA-256 of the pointer last confirmed written, and its age.
    pub pointer_sha256: Option<String>,
    pub pointer_age_ms: Option<u64>,
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
    Ok(Some(AuthorizationView {
        kind: b.authorization.kind.clone(),
        digest: digest.to_string(),
        issued_epoch,
        renew_by_epoch,
        revoked_by,
        revocations,
        pointer_sha256: pointer.map(|r| text(r, 0).map(str::to_owned)).transpose()?,
        pointer_age_ms: pointer.map(|r| uint(r, 1).map(|t| now_ms.saturating_sub(t))).transpose()?,
    }))
}

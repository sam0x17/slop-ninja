//! Account deletion: one journaled operation, for the holder's own deletion
//! (step-up and the typed word), the operator's recorded legal or abuse
//! decision (`delete_account`), and an account `x_signin_purge` leaves with
//! no sign-in method.
//!
//! The detect Worker's `owner_purge {user_id, mode}` is enqueued first; only
//! then does one batch revoke every session, delete the identities (and an X
//! avatar), the saved links and any open flow, void unused grants (non-cash)
//! and leave the user row `deleted` with no personal field. Payments stay
//! for the statutory period, tied to the tombstoned id. The detect Worker
//! acts only on pages whose owner hash is this account's, so bookmarks and
//! pages another account owns are never touched. If the batch fails after
//! the message was sent, the request answers an error and can be repeated:
//! both steps are idempotent.

use serde_json::json;
use slop_ninja_detect_page::queue::{Job, QueueMessage};
use slop_ninja_detect_page::{PurgeMode, RefusalCode, UserId};

use crate::config::Config;
use crate::host::{DbError, Host, Stmt, Val, guard};
use crate::journal::{self, Entry};

/// Who asked for the deletion, and the journal fields that record it.
pub struct Actor<'a> {
    pub actor: &'a str,
    pub reason: Option<&'a str>,
    pub signature: Option<[u8; 64]>,
    pub nonce_h: Option<[u8; 32]>,
    /// Extra content-free journal args (the operator's `basis`).
    pub extra: Vec<(&'static str, serde_json::Value)>,
}

/// Every statement of the deletion except the journal row.
pub fn statements(uid: &UserId, now: u64) -> Vec<Stmt> {
    let u = || Val::from(uid.0);
    vec![
        guard("EXISTS (SELECT 1 FROM users WHERE user_id = ? AND state != 'deleted')", vec![u()]),
        Stmt::new("DELETE FROM sessions WHERE user_id = ?", vec![u()]),
        Stmt::new(
            "DELETE FROM avatars WHERE identity_id IN (SELECT identity_id FROM identities WHERE user_id = ?)",
            vec![u()],
        ),
        Stmt::new("DELETE FROM identities WHERE user_id = ?", vec![u()]),
        Stmt::new("DELETE FROM owned WHERE user_id = ?", vec![u()]),
        Stmt::new("DELETE FROM pending_links WHERE user_id = ? OR from_user_id = ?", vec![u(), u()]),
        Stmt::new("DELETE FROM auth_flows WHERE user_id = ?", vec![u()]),
        Stmt::new("DELETE FROM email_tokens WHERE user_id = ?", vec![u()]),
        Stmt::new("DELETE FROM wallet_chals WHERE user_id = ?", vec![u()]),
        Stmt::new("DELETE FROM api_keys WHERE user_id = ?", vec![u()]),
        Stmt::new(
            "UPDATE payments SET state = 'void', tokens_left = 0 WHERE user_id = ? AND kind = 'grant' AND state = 'active'",
            vec![u()],
        ),
        Stmt::new(
            "UPDATE plan_periods SET state = 'ended', allowance = used, api_cap = api_used \
             WHERE user_id = ? AND cash = 0 AND state = 'active'",
            vec![u()],
        ),
        Stmt::new(
            "UPDATE users SET state = 'deleted', deleted_at = ?, flags = 0, merged_into = NULL WHERE user_id = ?",
            vec![Val::from(now), u()],
        ),
    ]
}

/// Deletes `user`'s account; answers the journal row's `seq`.
pub async fn delete_account<H: Host>(
    cfg: &Config,
    host: &H,
    uid: &UserId,
    mode: PurgeMode,
    by: Actor<'_>,
) -> Result<u64, RefusalCode> {
    let now = host.now_ms();
    let msg = QueueMessage::new(Job::OwnerPurge { user_id: *uid, mode });
    if host.enqueue(&msg).await.is_err() {
        crate::metrics::error(host, "queue");
        return Err(RefusalCode::Internal);
    }
    let mut args = vec![("user_id", json!(uid.hex())), ("mode", json!(mode.as_str()))];
    args.extend(by.extra);
    let mut stmts = statements(uid, now);
    stmts.push(
        Entry {
            at: now,
            actor: by.actor,
            op: journal::op_text("delete_account", journal::args(args)),
            user_h: Some(cfg.keys.user_h(uid)),
            reason: by.reason,
            signature: by.signature,
            nonce_h: by.nonce_h,
        }
        .stmt(),
    );
    match host.batch(stmts).await {
        Ok(rows) => {
            crate::metrics::session(host, "revoked");
            Ok(rows.last().and_then(|r| r.first()).and_then(|r| r.u64("seq")).unwrap_or(0))
        }
        Err(DbError::Guard) => Err(RefusalCode::NotFound),
        Err(DbError::Constraint) => Err(RefusalCode::OpReusedNonce),
        Err(DbError::Unavailable) => Err(RefusalCode::Internal),
    }
}

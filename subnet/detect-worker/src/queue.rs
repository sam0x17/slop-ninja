//! The `detect-jobs` queue consumer's store messages.
//!
//! The consumer runs with `max_batch_size = 1`, so each invocation handles
//! one message. Messages are content-free; payloads are never logged.
//!
//! - `purge {urls}`: the zone purge when a purge token exists, otherwise
//!   dropped (the page cache bound covers the delay).
//! - `owner_purge {user_id, mode}` (account deletion): the consumer derives
//!   `owner_h` itself and acts only on rows `WHERE owner_h = ?`, 50 per
//!   batch. `delete` tombstones each deletable page `WHERE id_h = ? AND
//!   owner_h = ?` with its re-run copies, then deletes their documents
//!   (D1 first; the backstop removes a document left by a failed delete),
//!   and releases the account's pages under a legal state (their state
//!   stands). `release` clears the owner of every page. A message that has
//!   more left after its rounds sends itself again. Idempotent: a
//!   redelivery finds nothing left. No URL is purged (no slug is known).
//! - `owner_rekey {from, to}` (a merge): both hashes derived here; one batch
//!   moves `detections`, `jobs` and `strikes` (a case both accounts held
//!   keeps one strike).
//! - `legal_notice {case_id, kind}`: the case's notice to the relay's legal
//!   stream, a secret page named only as "private detection" (its slug
//!   masked in every field; the sealed notice keeps the link).
//! - `sitemap_dirty {shard}`: marks the shard for the hourly rebuild.
//! - `indexnow`: not sent before public pages exist; dropped.
//!
//! - `commit_hold {det_id_h}`: ends a paid job's hold as its state asks
//!   (`paid::end_hold`, which needs the ledger; the Worker's consumer calls
//!   it before this one). An account's deleted pages cancel their running
//!   jobs in the delete's own batch, and those holds go back the same way.
//!
//! The other messages belong to the URL flows; until those are built the
//! consumer leaves them to the queue's retries (and the dead-letter queue,
//! which raises an alert).

use slop_ninja_detect_page::codes::PurgeMode;
use slop_ninja_detect_page::ids::UserId;
use slop_ninja_detect_page::queue::{Job, QueueMessage};

use crate::db::{Io, IoError, Stmt, Val, delete_docs};
use crate::keys::{Bytes32, StoreKeys, open_with};
use crate::manage::{DELETE_BY_OWNER, DELETE_COPIES};
use crate::store::secret_doc_key;

/// What the consumer does with a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ack {
    /// Done (or nothing to do): acknowledge.
    Done,
    /// Ask the queue to deliver it again after this many seconds.
    Retry(u32),
}

/// Pages per owner-purge batch.
pub const OWNER_BATCH: u64 = 50;
/// Batches per message before it sends itself again.
pub const OWNER_ROUNDS: u32 = 20;
/// The retry delay of a failed step.
pub const RETRY_S: u32 = 60;
/// The delay of a message whose flow is not built yet.
pub const NOT_BUILT_RETRY_S: u32 = 3600;

/// An account's deletable pages.
pub const SELECT_OWNED_DELETABLE: &str = "SELECT hex(id_h) AS id_h FROM detections \
WHERE owner_h = ?1 AND state IN ('live','pending','failed','disabled') LIMIT ?2";

/// Releases up to `?2` of an account's pages.
pub const RELEASE_OWNED: &str = "UPDATE detections SET owner_h = NULL, owner_via = NULL \
WHERE id_h IN (SELECT id_h FROM detections WHERE owner_h = ?1 LIMIT ?2)";

/// A merge: the merged account's rows follow it (`?1` the merged account's
/// hash, `?2` the surviving one's).
pub const REKEY: [&str; 3] = [
    "UPDATE detections SET owner_h = ?2 WHERE owner_h = ?1",
    "UPDATE jobs SET owner_h = ?2 WHERE owner_h = ?1",
    "UPDATE OR IGNORE strikes SET owner_h = ?2 WHERE owner_h = ?1",
];
/// The strikes a case both accounts held leaves behind.
pub const REKEY_LEFTOVER: &str = "DELETE FROM strikes WHERE owner_h = ?1";

/// A case for the legal stream.
pub const SELECT_CASE: &str = "SELECT kind, received_at FROM cases WHERE case_id = ?1";
/// A case's public targets (secret targets are named only as such).
pub const SELECT_CASE_TARGETS: &str = "SELECT d.public_slug AS public_slug, d.kind AS kind FROM case_targets t \
LEFT JOIN detections d ON d.id_h = t.det_id_h WHERE t.case_id = ?1";

/// The AAD of a case's notice (`c/<case_id>`, under `K_cases`).
pub const CASE_AAD: &[u8] = b"slop_ninja_detect_case_v1";
/// The relay's legal stream schema.
pub const LEGAL_SCHEMA: &str = "slop_ninja.detect.legal_notice/v1";

/// The legal stream's message.
#[derive(serde::Serialize)]
struct LegalBody<'a> {
    schema: &'static str,
    case_id: &'a str,
    kind: &'a str,
    received_at: u64,
    /// Public slugs, or "private detection" for each secret target.
    targets: Vec<String>,
    /// The notice as the claimant wrote it, opened from `c/<case_id>`, in
    /// its legal stream copy: a secret page's link and slug masked
    /// ([`crate::cases::Notice::for_relay`]).
    notice: crate::cases::Notice,
}

/// Handles one message.
pub async fn consume(io: &impl Io, keys: &StoreKeys, msg: &QueueMessage) -> Ack {
    let r = match &msg.job {
        Job::Purge { urls } => io.cache_purge(urls).await.map(|_| ()),
        Job::OwnerPurge { user_id, mode } => owner_purge(io, keys, *user_id, *mode).await,
        Job::OwnerRekey { from_user_id, to_user_id } => rekey(io, keys, *from_user_id, *to_user_id).await,
        Job::LegalNotice { case_id, .. } => legal_notice(io, keys, case_id).await,
        Job::SitemapDirty { shard } => io
            .run(Stmt::new(crate::sitemap::MARK_DIRTY, vec![Val::int(u64::from(*shard))]))
            .await
            .map(|_| ())
            .map_err(IoError::from),
        Job::Indexnow { .. } => Ok(()),
        // Handled with the ledger (`paid::end_hold`), never here.
        Job::CommitHold { .. } => return Ack::Retry(RETRY_S),
        _ => return Ack::Retry(NOT_BUILT_RETRY_S),
    };
    match r {
        Ok(()) => Ack::Done,
        Err(IoError) => Ack::Retry(RETRY_S),
    }
}

async fn owner_purge(io: &impl Io, keys: &StoreKeys, user_id: UserId, mode: PurgeMode) -> Result<(), IoError> {
    let o: Bytes32 = keys.owner_h(&user_id.0);
    let again = || io.enqueue(vec![QueueMessage::new(Job::OwnerPurge { user_id, mode })]);
    if mode == PurgeMode::Delete {
        let mut rounds = 0;
        loop {
            let d = io.run(Stmt::new(SELECT_OWNED_DELETABLE, vec![Val::blob(&o), Val::int(OWNER_BATCH)])).await?;
            let ids: Vec<Bytes32> = d.rows.iter().filter_map(|r| r.hex32("id_h")).collect();
            if ids.is_empty() {
                break;
            }
            if rounds == OWNER_ROUNDS {
                // More left than one message's rounds: continue in a new one.
                return again().await;
            }
            rounds += 1;
            let now = io.now_ms();
            let mut batch = Vec::with_capacity(ids.len() * 3);
            for id in &ids {
                batch.push(Stmt::new(DELETE_BY_OWNER, vec![Val::blob(id), Val::blob(&o), Val::int(now)]));
                batch.push(Stmt::new(DELETE_COPIES, vec![Val::blob(id), Val::int(now)]));
                batch.push(Stmt::new(crate::paid::CANCEL_DELETED, vec![Val::blob(id), Val::int(now)]));
            }
            let done = io.batch(batch).await?;
            let mut docs = Vec::new();
            for (part, id) in done.chunks(3).zip(&ids) {
                if part.first().is_some_and(|x| x.changes == 1) {
                    docs.push(secret_doc_key(id));
                    if let Some(c) = part.get(1) {
                        docs.extend(c.rows.iter().filter_map(|r| r.hex32("id_h")).map(|c| secret_doc_key(&c)));
                    }
                    // A paid page's running job: its hold goes back.
                    crate::paid::after_cancel(io, part.get(2)).await;
                }
            }
            let _ = delete_docs(io, &docs).await;
        }
    }
    // Pages left (all of them on release; on delete, pages under a legal
    // state) keep their state and lose their owner.
    for _ in 0..OWNER_ROUNDS {
        let d = io.run(Stmt::new(RELEASE_OWNED, vec![Val::blob(&o), Val::int(OWNER_BATCH * 10)])).await?;
        if d.changes < OWNER_BATCH * 10 {
            return Ok(());
        }
    }
    again().await
}

async fn rekey(io: &impl Io, keys: &StoreKeys, from: UserId, to: UserId) -> Result<(), IoError> {
    let (a, b) = (keys.owner_h(&from.0), keys.owner_h(&to.0));
    if a == b {
        return Ok(());
    }
    let mut batch: Vec<Stmt> = REKEY.iter().map(|s| Stmt::new(s, vec![Val::blob(&a), Val::blob(&b)])).collect();
    batch.push(Stmt::new(REKEY_LEFTOVER, vec![Val::blob(&a)]));
    io.batch(batch).await?;
    Ok(())
}

async fn legal_notice(io: &impl Io, keys: &StoreKeys, case_id: &str) -> Result<(), IoError> {
    let done = io
        .batch(vec![
            Stmt::new(SELECT_CASE, vec![Val::text(case_id)]),
            Stmt::new(SELECT_CASE_TARGETS, vec![Val::text(case_id)]),
        ])
        .await?;
    let Some(case) = done.first().and_then(|d| d.first()) else {
        // No such case: nothing to send.
        io.metric("error", &["legal_no_case"]);
        return Ok(());
    };
    let kind = case.text("kind").unwrap_or_default().to_owned();
    let received_at = case.u64("received_at").unwrap_or_default();
    let targets = done
        .get(1)
        .map(|d| {
            d.rows
                .iter()
                .map(|r| match r.text("public_slug") {
                    Some(s) => s.to_owned(),
                    None => crate::cases::PRIVATE_TARGET.to_owned(),
                })
                .collect()
        })
        .unwrap_or_default();
    let sealed = io.doc_get(&format!("c/{case_id}")).await?.ok_or(IoError)?;
    let plain = open_with(keys.cases_key(), CASE_AAD, &sealed).map_err(|_| IoError)?;
    drop(sealed);
    let full: crate::cases::Notice = serde_json::from_slice(&plain).map_err(|_| IoError)?;
    drop(plain);
    // A secret page's link opens it: the relay keeps the notice until its
    // digest goes out and mails it to an outside mailbox, so the copy it
    // gets names the page only as a private detection. A notice that cannot
    // be masked is not sent (its retries end in the dead-letter alert).
    let notice = full.for_relay().ok_or(IoError)?;
    drop(full);
    let body = LegalBody { schema: LEGAL_SCHEMA, case_id, kind: &kind, received_at, targets, notice };
    io.relay_legal(serde_json::to_vec(&body).map_err(|_| IoError)?).await
}

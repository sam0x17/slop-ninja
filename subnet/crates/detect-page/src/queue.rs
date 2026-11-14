//! The `detect-jobs` queue's messages (content-free).
//!
//! Producers: the detect Worker and the account Worker (`owner_purge`,
//! `owner_rekey`). Consumer: the detect Worker, with `max_batch_size = 1`, so
//! each invocation handles exactly one message of any type. A message never
//! carries a secret slug, text, an email address, an `owner_h`, a list of
//! `id_h`, an X post id or an X URL; the one exception is the `purge` URL of a
//! deleted page, which then answers 410. `owner_purge` and `owner_rekey` carry
//! user ids, which only the detect Worker turns into owner hashes; the X
//! messages carry a D1 row id (`sources.source_id`), a journal sequence and
//! a code value (`cause`), or nothing at all (`x_sweep`).
//! Consumer payloads are never logged.

use serde::{Deserialize, Serialize};

use crate::codes::{CaseKind, IndexChange, PurgeMode, XDeleteCause};
use crate::ids::{IdH, UserId};

/// The message schema version (`v`).
pub const QUEUE_V: u8 = 1;

/// One queue message: `{"v":1,"type":…,…}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueMessage {
    pub v: u8,
    #[serde(flatten)]
    pub job: Job,
}

impl QueueMessage {
    pub fn new(job: Job) -> QueueMessage {
        QueueMessage { v: QUEUE_V, job }
    }
}

/// What a message asks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Job {
    /// Purge cached URLs (zone purge when a purge token exists; otherwise
    /// dropped, and the cache TTL bounds the delay).
    Purge { urls: Vec<String> },
    /// An account was deleted: delete or release every page it owns. The
    /// consumer derives `owner_h` and acts only on rows `WHERE owner_h = ?`,
    /// 50 per batch, with their re-run copies on delete. Idempotent.
    OwnerPurge { user_id: UserId, mode: PurgeMode },
    /// Retry a paid job's ledger commit.
    CommitHold { det_id_h: IdH },
    /// Send a case's notice to the legal stream of the alerts relay.
    LegalNotice { case_id: String, kind: CaseKind },
    /// A sitemap shard changed (coalesced into the hourly rebuild).
    SitemapDirty { shard: u32 },
    /// Ping IndexNow for a public page.
    Indexnow { public_slug: String, change: IndexChange },
    /// An operator re-run of one source (background class).
    RerunOp { source_id: u64 },
    /// Accounts merged: move detections, jobs and strikes from one owner
    /// hash to the other.
    OwnerRekey { from_user_id: UserId, to_user_id: UserId },
    /// One message of the daily existence check of X posts. The hourly
    /// cron sends it, and so does a message whose batch was full. The
    /// consumer reads up to `X_CHECK_BATCH` due X sources in order with a
    /// SELECT (`check_queued_at` first, NULL first, then `fetched_at`, then
    /// `source_id`), claims them with a guarded UPDATE, keeps the SELECT's
    /// order in code, and makes at most one `DETECT_FETCH /x/check` call for
    /// the batch, which asks the two control posts first. An empty claim
    /// makes no call. It carries nothing: no position, post id or slug.
    XSweep {},
    /// One page of `x_purge`: delete up to `X_CHECK_BATCH` X sources and
    /// everything they hold, then send the next page until none is left.
    /// `journal_seq` is the purge's `op_journal` row.
    XPurgePage { journal_seq: u64 },
    /// Finish deleting one X source whose deletion stopped: its R2 delete of
    /// the run documents or its D1 batch failed, so the source row and its
    /// `canon_url` remain. Sent by the check consumer, or by the case flow
    /// for a removal. The consumer repeats the deletion (read the document
    /// keys, the R2 delete, the D1 batch, the page purge) and on a failure
    /// asks the queue to retry with a delay; a message that runs out of
    /// retries reaches the dead-letter queue. It holds exactly a
    /// `sources.source_id` and the cause, and nothing else: no post id, X URL
    /// or slug.
    XDelete { source_id: u64, cause: XDeleteCause },
}

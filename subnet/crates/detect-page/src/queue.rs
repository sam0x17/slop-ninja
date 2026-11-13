//! The `detect-jobs` queue's messages (content-free).
//!
//! Producers: the detect Worker and the account Worker (`owner_purge`,
//! `owner_rekey`). Consumer: the detect Worker, with `max_batch_size = 1`, so
//! each invocation handles exactly one message of any type. A message never
//! carries a secret slug, text, an email address, an `owner_h`, a list of
//! `id_h`, an X post id or an X URL; the one exception is the `purge` URL of a
//! deleted page, which then answers 410. `owner_purge` and `owner_rekey` carry
//! user ids, which only the detect Worker turns into owner hashes; the X
//! messages carry D1 row ids (`sources.source_id`) and a journal sequence.
//! Consumer payloads are never logged.

use serde::{Deserialize, Serialize};

use crate::codes::{CaseKind, IndexChange, PurgeMode, XCheckWhy};
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
    /// One page of the X sweep. The hourly cron sends `{"after": null}`; the
    /// consumer reads one page of due X sources (oldest first, through the
    /// `sources_x_fetched`, `sources_x_candidate` and `sources_x_edit`
    /// indexes), marks them queued, sends their `x_check` messages in batches
    /// of `X_CHECK_BATCH`, deletes up to `X_CHECK_BATCH` expired
    /// `x_post_unavailable` rows, and sends the next `x_sweep` when the page
    /// was full. `after` is the position the previous page ended at, a
    /// `sources.source_id` (the consumer fixes the order it reads in); it is
    /// never a post id.
    XSweep { after: Option<u64> },
    /// Check these X sources (`sources.source_id`, at most `X_CHECK_BATCH`):
    /// the oEmbed existence check, or the edit lookup when a source's
    /// `edit_check_at` has passed. `why = edit` is sent with the queue's own
    /// delay (up to 12 hours) to `edit_check_at`; an edit message skips a
    /// source whose `edit_check_at` is empty or still ahead.
    XCheck { source_ids: Vec<u64>, why: XCheckWhy },
    /// One page of `x_purge`: delete up to `X_CHECK_BATCH` X sources and
    /// everything they hold, then send the next page until none is left.
    /// `journal_seq` is the purge's `op_journal` row.
    XPurgePage { journal_seq: u64 },
}

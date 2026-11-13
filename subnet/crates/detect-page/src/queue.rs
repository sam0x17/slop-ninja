//! The `detect-jobs` queue's messages (content-free).
//!
//! Producers: the detect Worker and the account Worker (`owner_purge`,
//! `owner_rekey`). Consumer: the detect Worker. A message never carries a
//! secret slug, text, an email address, an `owner_h` or a list of `id_h`;
//! the one exception is the `purge` URL of a deleted secret page, which then
//! answers 410. `owner_purge` and `owner_rekey` carry user ids, which only
//! the detect Worker turns into owner hashes. Consumer payloads are never
//! logged.

use serde::{Deserialize, Serialize};

use crate::codes::{CaseKind, IndexChange, PurgeMode};
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
}

//! The hourly cron's store work.
//!
//! - **Expiry sweep.** Rows past `expires_at` (`live`, `pending`, `failed`)
//!   lose their R2 documents first and are then tombstoned `expired`, so no
//!   document outlives the row that points at it. An expired family root
//!   takes its live and pending re-run copies with it, in the same batch.
//!   Expiry sweeps cannot purge the cache (no slug is known); the page cache
//!   bound covers them.
//! - **Backstop** (orphan documents). A delete that failed between its D1
//!   tombstone and its R2 delete, a cancelled paid job or a failed one leaves
//!   documents nothing reads. The backstop lists `p/` and `j/` a few pages
//!   per run (its cursor is kept in `schema_info`) and deletes a document
//!   whose detection is missing or ended (`deleted`, `expired`, or a failed
//!   paid job), and a job object whose job is no longer running or scored.
//!   Objects younger than [`BACKSTOP_MIN_AGE_MS`] are left alone, so a link
//!   being made (its document is put before its row) is never touched.
//! - **Paid jobs** (`crate::paid::sweep`). Running jobs whose page ended or
//!   expired are cancelled, and every hold whose end is an hour overdue is
//!   asked for again (`job {commit_retry}`); each goes to the
//!   `commit_hold` consumer. Paid starts left behind (`crate::paid::
//!   sweep_starts`, which needs the ledger) run beside this work in the
//!   Worker's own cron handler.
//! - **Claims.** Claims past their lease are deleted; a pending page with no
//!   live claim offers "Run it".
//! - **Sitemaps.** Dirty shards are rebuilt ([`crate::sitemap`]).
//!
//! Counters are not cron work: the gate's alarm flushes them.

use slop_ninja_detect_page::origins::Origins;

use crate::db::{Done, Io, IoError, Stmt, Val, delete_docs};
use crate::store::secret_doc_key;

/// Rows expired per sweep round, and rounds per run (each round is one D1
/// read, one R2 delete and one D1 batch).
pub const SWEEP_BATCH: u64 = 100;
pub const SWEEP_ROUNDS: u32 = 10;
/// R2 listing pages (of up to 1,000 objects) the backstop reads per run.
pub const BACKSTOP_PAGES: u32 = 5;
/// The backstop's grace: an hour, far beyond the seconds between a link's
/// document put and its row insert.
pub const BACKSTOP_MIN_AGE_MS: u64 = 3_600_000;
/// Ids per D1 read (D1 binds at most 100 parameters).
pub const IDS_PER_READ: usize = 90;

/// Due rows, oldest expiry first (`det_expiry`).
pub const SELECT_DUE: &str = "SELECT hex(id_h) AS id_h FROM detections \
WHERE expires_at IS NOT NULL AND expires_at <= ?1 AND state IN ('live','pending','failed') \
ORDER BY expires_at LIMIT ?2";

/// The live and pending copies of a family root.
pub const SELECT_COPIES: &str =
    "SELECT hex(id_h) AS id_h FROM detections WHERE copy_of = ?1 AND state IN ('live','pending')";

/// The expiry tombstone.
pub const EXPIRE: &str = "UPDATE detections SET state = 'expired', doc_key = NULL, doc_bytes = NULL, \
manage_h = NULL, owner_h = NULL, owner_via = NULL, state_v = state_v + 1, updated_at = ?2 \
WHERE id_h = ?1 AND state IN ('live','pending','failed') AND expires_at <= ?2";

/// An expired root's copies end with it (guarded on the root having expired).
pub const EXPIRE_COPIES: &str = "UPDATE detections SET state = 'expired', doc_key = NULL, doc_bytes = NULL, \
manage_h = NULL, owner_h = NULL, owner_via = NULL, state_v = state_v + 1, updated_at = ?2 \
WHERE copy_of = ?1 AND state IN ('live','pending') \
AND (SELECT r.state FROM detections r WHERE r.id_h = ?1) = 'expired'";

/// Failure holds past the robots cache period.
pub const DELETE_OLD_HOLDS: &str =
    "DELETE FROM refusal_marks WHERE det_id_h IN (SELECT id_h FROM detections WHERE updated_at < ?1)";

/// Claims past their lease.
pub const DELETE_STALE_CLAIMS: &str = "DELETE FROM claims WHERE lease_until < ?1";

/// What one run did (content-free counts).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hourly {
    pub expired: u64,
    pub orphans: u64,
    pub claims: u64,
    pub shards: u64,
    /// Paid jobs whose hold the sweep sent to `commit_hold`.
    pub holds: usize,
    /// Quarantined documents of closed or rejected cases deleted.
    pub quarantine: u64,
}

fn ids(d: &Done) -> Vec<[u8; 32]> {
    d.rows.iter().filter_map(|r| r.hex32("id_h")).collect()
}

/// One sweep round: returns how many rows it expired.
pub async fn sweep_round(io: &impl Io, now: u64) -> Result<u64, IoError> {
    let due = ids(&io.run(Stmt::new(SELECT_DUE, vec![Val::int(now), Val::int(SWEEP_BATCH)])).await?);
    if due.is_empty() {
        return Ok(0);
    }
    let mut copies = Vec::new();
    for id in &due {
        copies.extend(ids(&io.run(Stmt::new(SELECT_COPIES, vec![Val::blob(id)])).await?));
    }
    // R2 first: a row is tombstoned only after its documents are gone.
    let mut keys: Vec<String> = Vec::new();
    for id in due.iter().chain(&copies) {
        keys.push(secret_doc_key(id));
    }
    delete_docs(io, &keys).await?;
    let mut batch = Vec::with_capacity(due.len() * 2);
    for id in &due {
        batch.push(Stmt::new(EXPIRE, vec![Val::blob(id), Val::int(now)]));
        batch.push(Stmt::new(EXPIRE_COPIES, vec![Val::blob(id), Val::int(now)]));
    }
    let done = io.batch(batch).await?;
    Ok(done.iter().map(|d| d.changes).sum())
}

/// The backstop cursor's key in `schema_info`.
pub fn cursor_key(prefix: &str) -> String {
    format!("cron.backstop.{prefix}")
}

/// `p/<64 hex>` → the id; `j/<64 hex>/<part>` → the job's id.
pub fn object_id(key: &str) -> Option<[u8; 32]> {
    let rest = key.strip_prefix("p/").or_else(|| key.strip_prefix("j/"))?;
    let hexpart = rest.get(..64)?;
    let tail = rest.get(64..)?;
    let ok_tail = if key.starts_with("p/") { tail.is_empty() } else { tail.starts_with('/') && tail.len() > 1 };
    if !ok_tail {
        return None;
    }
    let mut out = [0u8; 32];
    hex::decode_to_slice(hexpart, &mut out).ok()?;
    Some(out)
}

fn in_sql(head: &str, n: usize, tail: &str) -> String {
    let marks: Vec<String> = (1..=n).map(|i| format!("?{i}")).collect();
    format!("{head} ({}){tail}", marks.join(", "))
}

/// The ids among `ids` whose documents are still needed.
async fn kept(io: &impl Io, prefix: &str, want: &[[u8; 32]]) -> Result<Vec<[u8; 32]>, IoError> {
    let mut keep = Vec::new();
    for chunk in want.chunks(IDS_PER_READ) {
        let sql = if prefix == "p/" {
            in_sql(
                "SELECT hex(id_h) AS id_h FROM detections WHERE id_h IN",
                chunk.len(),
                " AND state NOT IN ('deleted','expired') AND NOT (kind = 'private_paid' AND state = 'failed')",
            )
        } else {
            in_sql(
                "SELECT hex(det_id_h) AS id_h FROM jobs WHERE det_id_h IN",
                chunk.len(),
                " AND state IN ('running','scored')",
            )
        };
        let d = io.run(Stmt::owned(sql, chunk.iter().map(|i| Val::blob(i)).collect())).await?;
        keep.extend(ids(&d));
    }
    Ok(keep)
}

/// One backstop pass over `prefix` (`p/` or `j/`).
pub async fn backstop(io: &impl Io, prefix: &str, now: u64) -> Result<u64, IoError> {
    let ck = cursor_key(prefix);
    let cur = io.run(Stmt::owned("SELECT v FROM schema_info WHERE k = ?1".into(), vec![Val::text(ck.clone())])).await?;
    let mut cursor = cur.first().and_then(|r| r.text("v")).map(str::to_owned);
    let mut deleted = 0;
    for _ in 0..BACKSTOP_PAGES {
        let page = io.doc_list(prefix, cursor.as_deref()).await?;
        let old: Vec<(String, [u8; 32])> = page
            .objects
            .iter()
            .filter(|(_, up)| up.saturating_add(BACKSTOP_MIN_AGE_MS) <= now)
            .filter_map(|(k, _)| Some((k.clone(), object_id(k)?)))
            .collect();
        let mut uniq: Vec<[u8; 32]> = old.iter().map(|(_, i)| *i).collect();
        uniq.sort_unstable();
        uniq.dedup();
        let keep = kept(io, prefix, &uniq).await?;
        let orphans: Vec<String> = old.into_iter().filter(|(_, i)| !keep.contains(i)).map(|(k, _)| k).collect();
        delete_docs(io, &orphans).await?;
        deleted += orphans.len() as u64;
        cursor = page.cursor;
        if cursor.is_none() {
            break;
        }
    }
    let save = match &cursor {
        Some(c) => Stmt::owned(
            "INSERT INTO schema_info (k, v) VALUES (?1, ?2) ON CONFLICT (k) DO UPDATE SET v = excluded.v".into(),
            vec![Val::text(ck), Val::text(c.clone())],
        ),
        None => Stmt::owned("DELETE FROM schema_info WHERE k = ?1".into(), vec![Val::text(ck)]),
    };
    io.run(save).await?;
    Ok(deleted)
}

/// The hourly store work. Each part runs even when an earlier one failed;
/// the first error is returned after all of them ran.
pub async fn hourly(io: &impl Io, origins: &Origins, index_on: bool) -> Result<Hourly, IoError> {
    let now = io.now_ms();
    let mut h = Hourly::default();
    let mut first_err = None;
    for _ in 0..SWEEP_ROUNDS {
        match sweep_round(io, now).await {
            Ok(0) => break,
            Ok(n) => h.expired += n,
            Err(e) => {
                first_err.get_or_insert(e);
                break;
            }
        }
    }
    for prefix in ["p/", "j/"] {
        match backstop(io, prefix, now).await {
            Ok(n) => h.orphans += n,
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    match crate::paid::sweep(io).await {
        Ok(n) => h.holds = n,
        Err(e) => {
            first_err.get_or_insert(e);
        }
    }
    match io.run(Stmt::new(DELETE_STALE_CLAIMS, vec![Val::int(now)])).await {
        Ok(d) => {
            h.claims = d.changes;
            // Each claim released here ended past its lease.
            if d.changes > 0 {
                io.metric_n("job", &["claim_expired"], d.changes as f64);
            }
        }
        Err(e) => {
            first_err.get_or_insert(e.into());
        }
    }
    match io
        .run(Stmt::new(DELETE_OLD_HOLDS, vec![Val::int(now.saturating_sub(crate::url_flow::REFUSAL_HOLD_MS))]))
        .await
    {
        Ok(_) => {}
        Err(e) => {
            first_err.get_or_insert(e.into());
        }
    }
    // The daily existence check of X posts: one message an hour; a sweep
    // with nothing due makes no call.
    if let Err(e) = io
        .enqueue(vec![slop_ninja_detect_page::queue::QueueMessage::new(slop_ninja_detect_page::queue::Job::XSweep {})])
        .await
    {
        first_err.get_or_insert(e);
    }
    match crate::sitemap::rebuild(io, origins, index_on).await {
        Ok(n) => h.shards = n,
        Err(e) => {
            first_err.get_or_insert(e);
        }
    }
    // The quarantine of closed and rejected cases (`close_case` deletes its
    // own; this is the backstop).
    match crate::cases::sweep_quarantine(io).await {
        Ok(n) => h.quarantine = n,
        Err(e) => {
            first_err.get_or_insert(e);
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(h),
    }
}

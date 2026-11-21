//! The hourly cron's store work.
//!
//! - **No expiry sweep.** No page ends by time. A pasted text's page still
//!   `pending` after [`BACKSTOP_MIN_AGE_MS`] (a save that stopped between
//!   its claim and its flip) is tombstoned `deleted`, its document with it,
//!   so its slug stays dead and the text's next check takes the next
//!   generation.
//! - **Backstop** (orphan documents). A delete that failed between its D1
//!   tombstone and its R2 delete, a cancelled paid job or a failed one leaves
//!   documents nothing reads. The backstop lists `p/` and `j/` a few pages
//!   per run (its cursor is kept in `schema_info`) and deletes a document
//!   whose detection is missing or ended (`deleted`, `expired`, or a failed
//!   paid job), and a job object whose job is no longer running or scored.
//!   Objects younger than [`BACKSTOP_MIN_AGE_MS`] are left alone, so a page
//!   being made is never touched.
//! - **Paid jobs** (`crate::paid::sweep`). Running jobs whose page ended are
//!   cancelled, running jobs past their hold's end (`crate::paid::
//!   JOB_HOLD_IDLE_MS` without a move) are cancelled with their page failed
//!   `job_stopped`, and every hold whose end is an hour overdue is
//!   asked for again (`job {commit_retry}`); each goes to the
//!   `commit_hold` consumer. Paid starts left behind (`crate::paid::
//!   sweep_starts`, which needs the ledger) run beside this work in the
//!   Worker's own cron handler.
//! - **Claims.** Claims past their lease are deleted; a pending page with no
//!   live claim offers "Run it".
//! - **Sitemaps.** Dirty shards are rebuilt ([`crate::sitemap`], which reads
//!   `DETECT_INDEX` and `DETECT_X` itself).
//!
//! Counters are not cron work: the gate's alarm flushes them.

use slop_ninja_detect_page::origins::Origins;

use crate::db::{Done, Io, IoError, Stmt, Val, delete_docs};
use crate::store::secret_doc_key;

/// Stale pending text pages tombstoned per run.
pub const SWEEP_BATCH: u64 = 100;
/// R2 listing pages (of up to 1,000 objects) the backstop reads per run.
pub const BACKSTOP_PAGES: u32 = 5;
/// The backstop's grace: an hour, far beyond the seconds between a save's
/// claim and its flip.
pub const BACKSTOP_MIN_AGE_MS: u64 = 3_600_000;
/// Ids per D1 read (D1 binds at most 100 parameters).
pub const IDS_PER_READ: usize = 90;

/// Failure holds past the robots cache period.
pub const DELETE_OLD_HOLDS: &str =
    "DELETE FROM refusal_marks WHERE det_id_h IN (SELECT id_h FROM detections WHERE updated_at < ?1)";

/// Claims past their lease.
pub const DELETE_STALE_CLAIMS: &str = "DELETE FROM claims WHERE lease_until < ?1";

/// What one run did (content-free counts).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hourly {
    /// Stale pending text pages tombstoned.
    pub stale_pending: u64,
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

/// The pending text pages older than [`BACKSTOP_MIN_AGE_MS`]: each is
/// tombstoned (its seal let go), then its document deleted. Returns how many
/// it ended.
pub async fn sweep_pending(io: &impl Io, now: u64) -> Result<u64, IoError> {
    let cutoff = now.saturating_sub(BACKSTOP_MIN_AGE_MS);
    let due = ids(&io
        .run(Stmt::new(crate::save::SELECT_STALE_PENDING, vec![Val::int(cutoff), Val::int(SWEEP_BATCH)]))
        .await?);
    if due.is_empty() {
        return Ok(0);
    }
    let batch = due
        .iter()
        .map(|id| Stmt::new(crate::save::DROP_STALE_PENDING, vec![Val::blob(id), Val::int(cutoff), Val::int(now)]))
        .collect();
    let done = io.batch(batch).await?;
    let ended: Vec<[u8; 32]> = due.iter().zip(&done).filter(|(_, d)| d.changes == 1).map(|(id, _)| *id).collect();
    let keys: Vec<String> = ended.iter().map(secret_doc_key).collect();
    delete_docs(io, &keys).await?;
    Ok(ended.len() as u64)
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

pub(crate) fn in_sql(head: &str, n: usize, tail: &str) -> String {
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

pub(crate) async fn read_cursor(io: &impl Io, prefix: &str) -> Result<Option<String>, IoError> {
    let row = io.run(Stmt::new("SELECT v FROM schema_info WHERE k = ?1", vec![Val::text(cursor_key(prefix))])).await?;
    Ok(row.first().and_then(|r| r.text("v")).map(str::to_owned))
}

pub(crate) async fn save_cursor(io: &impl Io, prefix: &str, cursor: Option<&str>) -> Result<(), IoError> {
    let ck = cursor_key(prefix);
    let save = match cursor {
        Some(c) => Stmt::owned(
            "INSERT INTO schema_info (k, v) VALUES (?1, ?2) ON CONFLICT (k) DO UPDATE SET v = excluded.v".into(),
            vec![Val::text(ck), Val::text(c)],
        ),
        None => Stmt::owned("DELETE FROM schema_info WHERE k = ?1".into(), vec![Val::text(ck)]),
    };
    io.run(save).await?;
    Ok(())
}

/// One backstop pass over `prefix` (`p/` or `j/`).
pub async fn backstop(io: &impl Io, prefix: &str, now: u64) -> Result<u64, IoError> {
    let mut cursor = read_cursor(io, prefix).await?;
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
    save_cursor(io, prefix, cursor.as_deref()).await?;
    Ok(deleted)
}

/// The hourly store work. Each part runs even when an earlier one failed;
/// the first error is returned after all of them ran.
pub async fn hourly(io: &impl Io, origins: &Origins) -> Result<Hourly, IoError> {
    let now = io.now_ms();
    let mut h = Hourly::default();
    let mut first_err = None;
    match sweep_pending(io, now).await {
        Ok(n) => h.stale_pending = n,
        Err(e) => {
            first_err.get_or_insert(e);
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
    match crate::sitemap::rebuild(io, origins).await {
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

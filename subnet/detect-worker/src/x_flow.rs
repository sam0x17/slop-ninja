//! Pages about posts on X after they are made: the daily existence check
//! (`x_sweep`), deleting a set of X sources, removals (`x_remove`, the
//! removal form, `remove` on an X page) and their reversal (`restore`), the
//! `x_purge` pages and their dry run, and the `status` op's X lines.
//!
//! We hold only posts that X's free embed showed in full. Each live X page
//! keeps the post's id (its `canon_url`) and our document-level result,
//! never the post's text, handle, name, avatar, media or markup. When the
//! embed stops showing a post, for any reason, the sweep deletes its page
//! within about a day; a request from X or the post's author removes it at
//! once. Nothing here writes a post id or a slug to Analytics Engine.
//!
//! **The sweep** (one `x_sweep {}` message per invocation; the hourly cron
//! sends the first, a message whose batch was full sends the next):
//! 1. a SELECT reads up to [`XConfig::batch`] due X sources in order
//!    (`check_queued_at` first, NULL first, then `fetched_at`, then
//!    `source_id`, served by `sources_x_due`), and a guarded UPDATE claims
//!    them; the batch is the SELECT's rows the UPDATE returned, in the
//!    SELECT's order (SQLite gives RETURNING rows in no set order);
//! 2. an empty claim ends the message: no call, no `xcheck` point, no next
//!    message, only the lateness point;
//! 3. one `DETECT_FETCH /x/check` call for the batch, controls first;
//! 4. failed controls move no `fetched_at`, clear the batch's claims, write
//!    `xcheck {control}` and stop the chain;
//! 5. otherwise `present` moves `fetched_at` and clears the claim, `gone`
//!    deletes the source and its pages (`x_gone`), `not_checked` clears the
//!    claim, and `unexpected` and `failed` keep theirs, so the next claim
//!    puts them behind every source without one; one `xcheck` point per
//!    outcome that occurred, its double the count; any `unexpected` or
//!    `failed` stops the chain;
//! 6. one `xsweep` point, the age of the oldest `fetched_at` (a NULL read
//!    as 0);
//! 7. a full batch, with nothing stopping the chain, sends the next message.
//!
//! A message makes at most one binding call and no Durable Object call, and
//! writes at most six points, whatever its batch size.
//!
//! **Deleting a set** ([`delete_set`]): R2 first, then one D1 batch, its
//! statements taking the set through one JSON parameter (`json_each`):
//! 1. the run document keys and the page slugs are read from D1;
//! 2. the run documents `r/<run_id>` are deleted from R2 (at most 1,000 keys
//!    a call; a missing key is not an error, so the step repeats safely);
//! 3. only after R2 confirms, the D1 batch in foreign-key order: the
//!    detections (state by the cause; for a gone post or the purge, source
//!    cleared), the runs step 1 read cleared from their pages, the cases
//!    that name a source about to go, those runs, the aliases, then the
//!    source rows (deleted, or reduced to the `opted_out` tombstone that
//!    keeps only `canon_key`);
//! 4. the page URLs are purged when a purge token exists.
//!
//! A check of the post can store between step 1 and step 3. The batch then
//! never orphans its run: a page whose run step 1 did not read keeps that
//! run, and its source, so every run row and document stays reachable from
//! a page. A removal leaves no such window: it writes the tombstone first,
//! in its own batch, and the store's fence (`url_flow::X_BLOCKED`) ends
//! every later check of the post `failed` (`x_removed`) keeping nothing;
//! what stored before is in the set it reads, and a last read after its
//! batch deletes anything else. A gone post's newer page stays for the next
//! sweep. A gone deletion never takes a source that has become a tombstone
//! meanwhile, so a removal that lands while the sweep asks X still holds.
//!
//! When step 2 or 3 fails the source rows keep their `canon_url`, and the
//! caller sends `x_delete {source_id, cause}` for each of them (gone and
//! removal causes; a purge page needs none, the next page reads again).

use serde::Serialize;
use slop_ninja_detect_fetch_wire::{XCheckAnswer, XCheckRequest, XExists};
use slop_ninja_detect_page::codes::{CaseKind, HostRole, XDeleteCause};
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::queue::{Job, QueueMessage};
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_page::slug::{self, SlugRoute};
use slop_ninja_detect_page::x::{parse_x_canonical, x_source_key};

use crate::db::{Done, Io, IoError, Stmt, Val, delete_docs};
use crate::keys::Bytes32;
use crate::net::FetchApi;
use crate::store::run_doc_key;

/// A post is checked again once its last check is older than this (20
/// hours), so every post is checked within a day with room for one retry.
pub const X_CHECK_EVERY_MS: u64 = 20 * 3_600_000;
/// An unfinished claim older than this (one hour) is claimed again.
pub const X_REQUEUE_MS: u64 = 3_600_000;
/// The age past which a check is late (`x_sweep_late`, `status`).
pub const X_LATE_MS: u64 = 24 * 3_600_000;
/// Rendered X pages and cards stay in the Cache API at most this long.
pub const X_PAGE_CACHE_S: u64 = 3_600;

/// The smaller of the consumer's and detect-fetch's subrequest limits
/// (Workers Paid's default, each Worker's `limits.subrequests`), taken as
/// one budget because whether a bound Worker's subrequests count against its
/// caller's is not documented.
pub const SUBREQUESTS: u64 = 10_000;
/// The consumer's own subrequests per message besides the R2 deletes and
/// the `x_delete` sends, counted from this module: the claim's SELECT and
/// UPDATE, the binding call, the outcome batch, the deletion's key read and
/// D1 batch, its cache purge, the lateness read and the next message.
pub const CONSUMER_SUBREQUESTS: u64 = 9;
/// Simultaneous outgoing connections (`detect-fetch-wire::X_CHECK_K`).
pub const K: u64 = slop_ninja_detect_fetch_wire::X_CHECK_K as u64;
/// A queue consumer's wall-time limit: 15 minutes.
pub const CONSUMER_WALL_MS: u64 = 15 * 60_000;
/// The consumer's fixed time besides the call (`m`): a bound to replace
/// with the staging measurement (two D1 batches, one R2 round and a queue
/// send take well under a minute).
pub const CONSUMER_FIXED_MS: u64 = 60_000;
/// The URL flow's fetch wait when `FETCH_WAIT_MS` is unset; every X request
/// takes it as its deadline (`X_CALL_MS`).
pub const X_CALL_MS_DEFAULT: u64 = 10_000;

/// `X_CHECK_BATCH = min(S − r − 2, k · (floor((W − m) / X_CALL_MS) − 1))`
/// for the worst R2 and queue fan-out of a full batch: every source gone,
/// each with [`RUNS_PER_SOURCE`] run documents (`ceil(keys / 1000)` deletes)
/// and one `x_delete` each in `sendBatch` calls of 100.
pub const fn x_check_batch(call_ms: u64) -> u64 {
    let call_ms = if call_ms == 0 { 1 } else { call_ms };
    let rounds = (CONSUMER_WALL_MS - CONSUMER_FIXED_MS) / call_ms;
    let time = K * rounds.saturating_sub(1);
    let r = CONSUMER_SUBREQUESTS + (time * RUNS_PER_SOURCE).div_ceil(1000) + time.div_ceil(100);
    let subrequests = SUBREQUESTS - r - 2;
    if time < subrequests { time } else { subrequests }
}

/// The run documents one X source may hold for the bound above: one per
/// package that scored it while it was live (a re-run makes a new run), a
/// generous bound since a package change is rare.
pub const RUNS_PER_SOURCE: u64 = 16;

/// The sweep's settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XConfig {
    /// The deadline of each X request (`X_CALL_MS`).
    pub call_ms: u64,
    /// Sources per message (`X_CHECK_BATCH`).
    pub batch: u64,
}

impl XConfig {
    pub fn new(call_ms: u64) -> XConfig {
        XConfig { call_ms, batch: x_check_batch(call_ms) }
    }

    /// From the deploy vars (`FETCH_WAIT_MS`).
    pub fn from_io(io: &impl Io) -> XConfig {
        let ms = io.var("FETCH_WAIT_MS").and_then(|v| v.parse::<u64>().ok()).filter(|v| *v > 0);
        XConfig::new(ms.unwrap_or(X_CALL_MS_DEFAULT))
    }
}

/// `sources.canon_key` of post `id`: `H("source_v1" ‖ "x:status:<id>")`.
pub fn x_canon_key(id: u64) -> Bytes32 {
    crate::keys::source_canon_key(&x_source_key(id))
}

/// A JSON array of integers, the parameter `json_each` reads.
pub fn json_ids(ids: &[u64]) -> String {
    let mut s = String::with_capacity(ids.len() * 8 + 2);
    s.push('[');
    for (i, id) in ids.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&id.to_string());
    }
    s.push(']');
    s
}

// ------------------------------------------------------------------ sweep

/// The due sources, in the claim's order.
pub const SELECT_DUE: &str = "SELECT source_id, canon_url FROM sources \
WHERE adapter = 'x' AND canon_url IS NOT NULL AND coalesce(fetched_at, 0) < ?1 \
AND (check_queued_at IS NULL OR check_queued_at < ?2) \
ORDER BY check_queued_at, fetched_at, source_id LIMIT ?3";

/// The guarded claim: the due test again, so a source another chain
/// claimed between the two statements is not claimed twice.
pub const CLAIM_DUE: &str = "UPDATE sources SET check_queued_at = ?2 \
WHERE source_id IN (SELECT value FROM json_each(?1)) AND adapter = 'x' AND canon_url IS NOT NULL \
AND coalesce(fetched_at, 0) < ?3 AND (check_queued_at IS NULL OR check_queued_at < ?4) RETURNING source_id";

/// `present`: checked now, claim cleared.
pub const MARK_PRESENT: &str = "UPDATE sources SET fetched_at = ?2, check_queued_at = NULL \
WHERE source_id IN (SELECT value FROM json_each(?1)) AND adapter = 'x' AND canon_url IS NOT NULL";

/// Claims cleared, `fetched_at` kept (`not_checked`, failed controls).
pub const CLEAR_CLAIMS: &str =
    "UPDATE sources SET check_queued_at = NULL WHERE source_id IN (SELECT value FROM json_each(?1)) AND adapter = 'x'";

/// The lateness point's read.
pub const SELECT_OLDEST: &str = "SELECT count(*) AS n, min(coalesce(fetched_at, 0)) AS oldest FROM sources \
WHERE adapter = 'x' AND canon_url IS NOT NULL";

/// What one sweep message did (for tests and the consumer).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// The sources claimed, in the order asked.
    pub batch: Vec<u64>,
    /// The SELECT returned a full batch.
    pub full: bool,
    /// Controls failed, or an `unexpected` or `failed` answer.
    pub stopped: bool,
    /// `DETECT_FETCH` calls made (0 or 1).
    pub calls: u32,
    /// Sources deleted as gone.
    pub gone: Vec<u64>,
    /// The next message was sent.
    pub next: bool,
}

/// The `xcheck` outcome of one answer.
fn outcome(a: &XExists) -> &'static str {
    match a {
        XExists::Present => "present",
        XExists::Gone => "gone",
        XExists::Unexpected => "unexpected",
        XExists::Failed { .. } => "failed",
        XExists::NotChecked => "not_checked",
    }
}

/// One sweep message. An error is a store failure (the queue retries the
/// message); every answer of X's is acknowledged, so a failing X costs one
/// round of requests an hour and fills no dead-letter queue.
pub async fn sweep(
    io: &impl Io,
    net: &impl FetchApi,
    origins: &Origins,
    cfg: &XConfig,
) -> Result<SweepReport, IoError> {
    let now = io.now_ms();
    let due_before = now.saturating_sub(X_CHECK_EVERY_MS);
    let requeue_before = now.saturating_sub(X_REQUEUE_MS);
    let mut rep = SweepReport::default();
    let sel = io
        .run(Stmt::new(SELECT_DUE, vec![Val::int(due_before), Val::int(requeue_before), Val::int(cfg.batch)]))
        .await?;
    rep.full = sel.rows.len() as u64 >= cfg.batch;
    let rows: Vec<(u64, Option<u64>)> = sel
        .rows
        .iter()
        .filter_map(|r| Some((r.u64("source_id")?, r.text("canon_url").and_then(parse_x_canonical))))
        .collect();
    let mut claimed = std::collections::BTreeSet::new();
    if !rows.is_empty() {
        let ids: Vec<u64> = rows.iter().map(|(s, _)| *s).collect();
        let upd = io
            .run(Stmt::new(
                CLAIM_DUE,
                vec![Val::text(json_ids(&ids)), Val::int(now), Val::int(due_before), Val::int(requeue_before)],
            ))
            .await?;
        claimed.extend(upd.rows.iter().filter_map(|r| r.u64("source_id")));
    }
    // The batch keeps the SELECT's order; RETURNING only says which rows
    // this message claimed.
    let batch: Vec<(u64, Option<u64>)> = rows.into_iter().filter(|(s, _)| claimed.contains(s)).collect();
    if batch.is_empty() {
        lateness(io, now, "empty").await?;
        return Ok(rep);
    }
    rep.batch = batch.iter().map(|(s, _)| *s).collect();
    // A canon_url that does not parse cannot be asked: it keeps its claim as
    // an unexpected answer would.
    let askable: Vec<(u64, u64)> = batch.iter().filter_map(|(s, id)| id.map(|i| (*s, i))).collect();
    let unparsable = batch.len() - askable.len();
    let mut counts: Vec<(&'static str, u64)> = Vec::new();
    let mut bump = |o: &'static str, n: u64| {
        if n == 0 {
            return;
        }
        match counts.iter_mut().find(|(k, _)| *k == o) {
            Some((_, c)) => *c += n,
            None => counts.push((o, n)),
        }
    };
    bump("unexpected", unparsable as u64);
    if unparsable > 0 {
        rep.stopped = true;
    }
    let mut present = Vec::new();
    let mut gone = Vec::new();
    let mut clear = Vec::new();
    if !askable.is_empty() {
        let req = XCheckRequest { ids: askable.iter().map(|(_, i)| *i).collect(), deadline_ms: cfg.call_ms };
        rep.calls += 1;
        match net.x_check(&req).await {
            Ok(XCheckAnswer::ControlsFailed { .. }) => {
                // No id was asked: every claim goes, nothing is recorded.
                clear.extend(askable.iter().map(|(s, _)| *s));
                bump("control", 1);
                rep.stopped = true;
            }
            Ok(XCheckAnswer::Checked { answers }) if answers.len() == askable.len() => {
                for ((s, _), a) in askable.iter().zip(&answers) {
                    bump(outcome(a), 1);
                    match a {
                        XExists::Present => present.push(*s),
                        XExists::Gone => gone.push(*s),
                        XExists::NotChecked => clear.push(*s),
                        XExists::Unexpected | XExists::Failed { .. } => rep.stopped = true,
                    }
                }
            }
            Ok(XCheckAnswer::Checked { .. }) => {
                // An answer of the wrong length: nothing can be trusted.
                bump("unexpected", askable.len() as u64);
                rep.stopped = true;
            }
            Err(IoError) => {
                // The binding itself failed: the claims stay and expire.
                bump("failed", askable.len() as u64);
                rep.stopped = true;
            }
        }
    }
    let mut stmts = Vec::new();
    if !present.is_empty() {
        stmts.push(Stmt::new(MARK_PRESENT, vec![Val::text(json_ids(&present)), Val::int(now)]));
    }
    if !clear.is_empty() {
        stmts.push(Stmt::new(CLEAR_CLAIMS, vec![Val::text(json_ids(&clear))]));
    }
    if !stmts.is_empty() {
        io.batch(stmts).await?;
    }
    if !gone.is_empty() {
        match delete_set(io, origins, &gone, Cause::Gone, now).await {
            Ok(_) => rep.gone = gone,
            Err(IoError) => {
                // The rows keep canon_url; x_delete finishes, and failing
                // that the next sweep finds them gone again.
                let _ = send_x_delete(io, &gone, XDeleteCause::XGone).await;
            }
        }
    }
    for (o, n) in &counts {
        io.metric_n("xcheck", &[o], *n as f64);
    }
    lateness(io, now, "claimed").await?;
    if rep.full && !rep.stopped {
        io.enqueue(vec![QueueMessage::new(Job::XSweep {})]).await?;
        rep.next = true;
    }
    Ok(rep)
}

/// The lateness point: the age in seconds of the oldest check of an X
/// source that holds a `canon_url` (0 with none).
async fn lateness(io: &impl Io, now: u64, step: &'static str) -> Result<(), IoError> {
    let age = oldest_age_ms(io, now).await?;
    io.metric_n("xsweep", &[step], (age / 1000) as f64);
    Ok(())
}

/// The age of the oldest check (`fetched_at`, a NULL read as 0) of an X
/// source holding a `canon_url`; 0 when there is none.
pub async fn oldest_age_ms(io: &impl Io, now: u64) -> Result<u64, IoError> {
    let d = io.run(Stmt::new(SELECT_OLDEST, Vec::new())).await?;
    let r = d.first();
    let n = r.and_then(|r| r.u64("n")).unwrap_or(0);
    let oldest = r.and_then(|r| r.u64("oldest")).unwrap_or(now);
    Ok(if n == 0 { 0 } else { now.saturating_sub(oldest) })
}

async fn send_x_delete(io: &impl Io, ids: &[u64], cause: XDeleteCause) -> Result<(), IoError> {
    for chunk in ids.chunks(100) {
        let msgs = chunk.iter().map(|s| QueueMessage::new(Job::XDelete { source_id: *s, cause })).collect();
        io.enqueue(msgs).await?;
    }
    Ok(())
}

// --------------------------------------------------------------- deleting

/// Why a set of X sources is deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// X's embed no longer shows the post: pages `deleted` (`x_gone`),
    /// source rows deleted.
    Gone,
    /// The removal form, `x_remove` or `remove` on an X page: pages `removed`
    /// (`x_removed`), each source kept as the `opted_out` tombstone.
    Removed,
    /// `x_purge`: pages `deleted` (`x_purged`), source rows (tombstones
    /// included) deleted.
    Purged,
}

impl Cause {
    fn code(self) -> RefusalCode {
        match self {
            Cause::Gone => RefusalCode::XGone,
            Cause::Removed => RefusalCode::XRemoved,
            Cause::Purged => RefusalCode::XPurged,
        }
    }

    /// The `x_delete` cause, for the causes that send one.
    pub fn delete_cause(self) -> Option<XDeleteCause> {
        match self {
            Cause::Gone => Some(XDeleteCause::XGone),
            Cause::Removed => Some(XDeleteCause::XRemoved),
            Cause::Purged => None,
        }
    }

    pub fn of(c: XDeleteCause) -> Cause {
        match c {
            XDeleteCause::XGone => Cause::Gone,
            XDeleteCause::XRemoved => Cause::Removed,
        }
    }
}

/// The source ids a deletion by `cause` takes from the JSON array `?1`. A
/// gone deletion takes only sources that still hold their `canon_url`, so a
/// removal's tombstone, written after the sweep claimed the source, stays.
pub fn set_of(cause: Cause) -> &'static str {
    match cause {
        Cause::Gone => {
            "SELECT source_id FROM sources WHERE source_id IN (SELECT value FROM json_each(?1)) AND adapter = 'x' \
AND canon_url IS NOT NULL"
        }
        Cause::Removed | Cause::Purged => "SELECT value FROM json_each(?1)",
    }
}

/// The set's run documents (step 1).
pub fn select_set_runs(cause: Cause) -> String {
    format!("SELECT DISTINCT run_id FROM detections WHERE source_id IN ({}) AND run_id IS NOT NULL", set_of(cause))
}

/// The set's page slugs (step 1).
pub fn select_set_slugs(cause: Cause) -> String {
    format!(
        "SELECT public_slug FROM detections WHERE source_id IN ({}) AND public_slug IS NOT NULL AND platform = 'x'",
        set_of(cause)
    )
}

/// Detections of the set whose source row goes (`?1` the set, `?2` the
/// code, `?3` the time, `?4` the runs step 1 read): run and source cleared,
/// ended by the cause (pages already ended or removed keep their state). A
/// page holding a run step 1 did not read (a check stored after the read)
/// is left whole, and keeps its source.
pub fn end_detections(cause: Cause) -> String {
    format!(
        "UPDATE detections SET \
state = CASE WHEN state IN ('deleted','expired','removed') THEN state ELSE 'deleted' END, \
refusal = CASE WHEN state IN ('deleted','expired','removed') THEN refusal ELSE ?2 END, \
run_id = NULL, source_id = NULL, state_v = state_v + 1, updated_at = ?3 \
WHERE source_id IN ({}) AND (run_id IS NULL OR run_id IN (SELECT value FROM json_each(?4)))",
        set_of(cause)
    )
}

/// The sources of the set that go: those no page names any more, after
/// [`end_detections`].
fn going(cause: Cause) -> String {
    format!(
        "SELECT s.source_id FROM sources s WHERE s.source_id IN ({}) \
AND NOT EXISTS (SELECT 1 FROM detections d WHERE d.source_id = s.source_id)",
        set_of(cause)
    )
}

/// Cases that name a source about to go.
pub fn unlink_cases(cause: Cause) -> String {
    format!("UPDATE cases SET source_id = NULL WHERE source_id IN ({})", going(cause))
}

/// The aliases of the sources about to go.
pub fn delete_going_aliases(cause: Cause) -> String {
    format!("DELETE FROM source_aliases WHERE source_id IN ({})", going(cause))
}

/// The source rows about to go.
pub fn delete_sources(cause: Cause) -> String {
    format!("DELETE FROM sources WHERE source_id IN ({}) AND adapter = 'x'", going(cause))
}

/// Detections of the set whose source stays as a tombstone: `removed`
/// (pages already ended or removed stay as they are). Their runs go with
/// [`CLEAR_RUNS`].
pub const REMOVE_DETECTIONS: &str = "UPDATE detections SET state = 'removed', refusal = 'x_removed', \
state_v = state_v + 1, updated_at = ?2 \
WHERE source_id IN (SELECT value FROM json_each(?1)) AND state NOT IN ('deleted','expired','removed')";

/// The runs step 1 read (`?2`), cleared from the set's pages (`?1`). A run
/// it did not read stays on its page, for the removal's last read.
pub const CLEAR_RUNS: &str = "UPDATE detections SET run_id = NULL \
WHERE source_id IN (SELECT value FROM json_each(?1)) AND run_id IN (SELECT value FROM json_each(?2))";

/// The runs step 1 read.
pub const DELETE_RUNS: &str = "DELETE FROM runs WHERE run_id IN (SELECT value FROM json_each(?1))";
/// The set's aliases (a removal).
pub const DELETE_ALIASES: &str = "DELETE FROM source_aliases WHERE source_id IN (SELECT value FROM json_each(?1))";
/// The set's sources reduced to tombstones: `canon_key`, `adapter = 'x'`,
/// `host = 'x.com'`, `rank_list_v = 'x'`, `state = 'opted_out'` and
/// `reputation = 'unranked'`, nothing else.
pub const TOMBSTONE_SOURCES: &str = "UPDATE sources SET canon_url = NULL, fetch_outcome = NULL, fetched_at = NULL, \
fresh_until = NULL, check_queued_at = NULL, latest_det = NULL, state = 'opted_out', reputation = 'unranked', \
rank_list_v = 'x' WHERE source_id IN (SELECT value FROM json_each(?1)) AND adapter = 'x'";

/// What a removal may have left on its tombstone (`?1`): a page still live,
/// or a page holding a run.
pub const SELECT_LEFT: &str = "SELECT count(*) AS n FROM detections WHERE source_id = ?1 \
AND (run_id IS NOT NULL OR state = 'live')";

/// What a deletion took.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Deleted {
    /// The detections' public slugs.
    pub slugs: Vec<String>,
    /// The run documents deleted.
    pub docs: usize,
}

/// The D1 statements of deleting `ids` by `cause` (step 3), with the run
/// ids step 1 read.
pub fn delete_stmts(ids: &[u64], runs: &[u64], cause: Cause, now: u64) -> Vec<Stmt> {
    let set = Val::text(json_ids(ids));
    let runs = Val::text(json_ids(runs));
    let mut v = Vec::with_capacity(6);
    match cause {
        Cause::Removed => {
            v.push(Stmt::new(REMOVE_DETECTIONS, vec![set.clone(), Val::int(now)]));
            v.push(Stmt::new(CLEAR_RUNS, vec![set.clone(), runs.clone()]));
            v.push(Stmt::new(DELETE_RUNS, vec![runs]));
            v.push(Stmt::new(DELETE_ALIASES, vec![set.clone()]));
            v.push(Stmt::new(TOMBSTONE_SOURCES, vec![set]));
        }
        Cause::Gone | Cause::Purged => {
            v.push(Stmt::owned(
                end_detections(cause),
                vec![set.clone(), Val::text(cause.code().as_str()), Val::int(now), runs.clone()],
            ));
            v.push(Stmt::owned(unlink_cases(cause), vec![set.clone()]));
            v.push(Stmt::new(DELETE_RUNS, vec![runs]));
            v.push(Stmt::owned(delete_going_aliases(cause), vec![set.clone()]));
            v.push(Stmt::owned(delete_sources(cause), vec![set]));
        }
    }
    v
}

/// Reads the set's run documents and slugs (step 1).
pub async fn read_set(io: &impl Io, ids: &[u64], cause: Cause) -> Result<(Vec<u64>, Vec<String>), IoError> {
    let set = Val::text(json_ids(ids));
    let d = io
        .batch(vec![
            Stmt::owned(select_set_runs(cause), vec![set.clone()]),
            Stmt::owned(select_set_slugs(cause), vec![set]),
        ])
        .await?;
    let runs = d.first().map(|d| d.rows.iter().filter_map(|r| r.u64("run_id")).collect()).unwrap_or_default();
    let slugs = d
        .get(1)
        .map(|d| d.rows.iter().filter_map(|r| r.text("public_slug")).map(str::to_owned).collect())
        .unwrap_or_default();
    Ok((runs, slugs))
}

/// The URLs of an X page the cache may hold: the page, its widget route and
/// its card.
pub fn page_urls(origins: &Origins, slugs: &[String]) -> Vec<String> {
    let o = origins.of(HostRole::Site);
    let mut v = Vec::with_capacity(slugs.len() * 3);
    for s in slugs {
        for r in [SlugRoute::Page, SlugRoute::XWidget, SlugRoute::Card] {
            v.push(o.url(&slug::path(HostRole::Site, r, s)));
        }
    }
    v
}

/// Deletes the set `ids` by `cause` at `now`: R2, then one D1 batch, then
/// the page purge. `before` statements run first in the same batch (an
/// op's journal row), `after` ones last (a case, its targets). An `Err`
/// leaves D1 as it was (R2 may already be done, which a repeat tolerates).
/// The returned results are the batch's, `before` first.
pub async fn delete_set_with(
    io: &impl Io,
    origins: &Origins,
    ids: &[u64],
    cause: Cause,
    now: u64,
    before: Vec<Stmt>,
    after: Vec<Stmt>,
) -> Result<(Deleted, Vec<Done>), IoError> {
    let (runs, slugs) = read_set(io, ids, cause).await?;
    let keys: Vec<String> = runs.iter().map(|r| run_doc_key(*r)).collect();
    delete_docs(io, &keys).await?;
    let mut batch = before;
    batch.extend(delete_stmts(ids, &runs, cause, now));
    batch.extend(after);
    let done = io.batch(batch).await?;
    if !slugs.is_empty() {
        // Without a purge token the cache bound (X_PAGE_CACHE_S) covers it.
        let _ = io.cache_purge(&page_urls(origins, &slugs)).await;
    }
    Ok((Deleted { slugs, docs: keys.len() }, done))
}

/// Deletes the set `ids` by `cause` at `now`.
pub async fn delete_set(
    io: &impl Io,
    origins: &Origins,
    ids: &[u64],
    cause: Cause,
    now: u64,
) -> Result<Deleted, IoError> {
    delete_set_with(io, origins, ids, cause, now, Vec::new(), Vec::new()).await.map(|(d, _)| d)
}

/// The `x_delete {source_id, cause}` consumer: repeats the deletion for one
/// source whose deletion stopped. A source that is already gone is done.
/// For a removal the tombstone comes first, as in [`remove_post`]; a
/// tombstone with no live page and no run left is done.
pub async fn x_delete(io: &impl Io, origins: &Origins, source_id: u64, cause: XDeleteCause) -> Result<(), IoError> {
    let d = io
        .run(Stmt::new(
            "SELECT canon_url FROM sources WHERE source_id = ?1 AND adapter = 'x'",
            vec![Val::int(source_id)],
        ))
        .await?;
    if d.first().is_none() {
        return Ok(());
    }
    if cause == XDeleteCause::XRemoved {
        let d = io
            .batch(vec![
                Stmt::new(TOMBSTONE_SOURCES, vec![Val::text(json_ids(&[source_id]))]),
                Stmt::new(SELECT_LEFT, vec![Val::int(source_id)]),
            ])
            .await?;
        if d.get(1).and_then(Done::first).and_then(|r| r.u64("n")).unwrap_or(0) == 0 {
            return Ok(());
        }
    }
    delete_set(io, origins, &[source_id], Cause::of(cause), io.now_ms()).await.map(|_| ())
}

// --------------------------------------------------------------- removals

/// Where a removal's case comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaseRef {
    /// The case the removal form opened (its own row): the removal names the
    /// tombstone in it.
    Existing(String),
    /// A new `x_request` case: `x_remove`, or `remove` acting outside a
    /// case. No notice document, no status token.
    New(String),
}

/// The reason a removal journals (`x_remove {reason, canon_key}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum XReason {
    Author,
    X,
    Other,
}

impl XReason {
    pub fn as_str(self) -> &'static str {
        match self {
            XReason::Author => "author",
            XReason::X => "x",
            XReason::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<XReason> {
        Some(match s {
            "author" => XReason::Author,
            "x" => XReason::X,
            "other" => XReason::Other,
            _ => return None,
        })
    }

    /// The removal form's track as a reason: `author`, or `x` when the
    /// report says it comes from X.
    pub fn of_track(kind: CaseKind, from_x: bool) -> XReason {
        if from_x {
            XReason::X
        } else if kind == CaseKind::Author {
            XReason::Author
        } else {
            XReason::Other
        }
    }
}

/// The journal actor of the second, unsigned row the form and `remove`
/// write beside their own, so `replay` re-applies the removal.
pub const AUTO_TOMBSTONE_ACTOR: &str = "auto:x_tombstone";

/// The journal's `op` value of an `x_remove`: `{"op":"x_remove","args":
/// {"reason":…,"canon_key":…}}`. It names the post only by the hash the
/// tombstone keeps; `x_purge` strips `args.canon_key`.
pub fn x_remove_op_json(reason: XReason, canon_key: &Bytes32) -> String {
    serde_json::json!({ "op": "x_remove", "args": { "reason": reason.as_str(), "canon_key": hex::encode(canon_key) } })
        .to_string()
}

/// The `auto:x_tombstone` journal row.
pub fn auto_journal_row(reason: XReason, canon_key: &Bytes32, now: u64) -> Stmt {
    Stmt::new(
        "INSERT INTO op_journal (at, actor, op, signature, nonce_h) VALUES (?1, ?2, ?3, NULL, NULL)",
        vec![Val::int(now), Val::text(AUTO_TOMBSTONE_ACTOR), Val::text(x_remove_op_json(reason, canon_key))],
    )
}

/// Writes (or keeps) the tombstone: a post no new check may start on.
pub const UPSERT_TOMBSTONE: &str = "INSERT INTO sources (canon_key, canon_url, host, adapter, state, reputation, \
rank_list_v) VALUES (?1, NULL, 'x.com', 'x', 'opted_out', 'unranked', 'x') ON CONFLICT (canon_key) DO UPDATE SET \
canon_url = NULL, fetch_outcome = NULL, fetched_at = NULL, fresh_until = NULL, check_queued_at = NULL, \
latest_det = NULL, state = 'opted_out', reputation = 'unranked', rank_list_v = 'x' WHERE sources.adapter = 'x'";

/// The new `x_request` case (accepted at once: deleting is the compliant
/// direction, and `restore` reverses the opt-out).
pub const INSERT_X_CASE: &str = "INSERT INTO cases (case_id, status_h, kind, state, received_at, updated_at, \
due_at, notice_key, source_id) VALUES (?1, NULL, 'x_request', 'accepted', ?2, ?2, NULL, NULL, \
(SELECT source_id FROM sources WHERE canon_key = ?3))";

/// The form's case names the tombstone.
pub const LINK_CASE: &str = "UPDATE cases SET source_id = (SELECT source_id FROM sources WHERE canon_key = ?2), updated_at = ?3 \
WHERE case_id = ?1";

/// The case's targets: the removed detections, by `id_h`.
pub const CASE_TARGETS: &str = "INSERT INTO case_targets (case_id, det_id_h) SELECT ?1, d.id_h FROM detections d \
WHERE d.source_id = (SELECT source_id FROM sources WHERE canon_key = ?2) AND d.state = 'removed' \
ON CONFLICT DO NOTHING";

/// The public removal record of each page this removal took (the pages it
/// moved to `removed` at `?1`).
pub const RECORD_REMOVALS: &str = "INSERT INTO removals (at, public_slug, scope, basis, jurisdiction) \
SELECT ?1, d.public_slug, 'page', ?3, 'none' FROM detections d \
WHERE d.source_id = (SELECT source_id FROM sources WHERE canon_key = ?2) AND d.state = 'removed' \
AND d.refusal = 'x_removed' AND d.updated_at = ?1 AND d.public_slug IS NOT NULL";

/// The removals record's `basis` of an X removal.
pub fn removal_basis(reason: XReason) -> &'static str {
    match reason {
        XReason::Author => "x_author_request",
        XReason::X => "x_request",
        XReason::Other => "x_other_request",
    }
}

/// One X removal, the same for the form, `remove` on an X page and
/// `x_remove`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removal {
    /// The post's source key hash (the tombstone's `canon_key`).
    pub canon_key: Bytes32,
    pub reason: XReason,
    pub case: CaseRef,
    /// Write the `auto:x_tombstone` journal row (the form and `remove`);
    /// `x_remove`'s own signed row is the op route's.
    pub auto_journal: bool,
}

/// What a removal answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Removed {
    pub case_id: String,
    /// Detections removed.
    pub removed: u64,
}

/// The source of a post by its `canon_key`.
pub const SELECT_BY_KEY: &str = "SELECT source_id, canon_url FROM sources WHERE canon_key = ?1 AND adapter = 'x'";

/// Removes every page about the post `r.canon_key` and leaves (or keeps) its
/// `opted_out` tombstone, so no new check of it can start; whether or not
/// we held any page. `journal` is the op route's own journal row (empty for
/// the form, which writes its case first). The case names the tombstone and
/// targets the removed detections.
///
/// 1. The tombstone first, in its own batch with the read of its id. From
///    here the store's fence (`url_flow::X_BLOCKED`) ends every check of
///    the post that has not stored yet `failed` (`x_removed`) and keeps
///    nothing of it; a check that stored before is in the set read next.
/// 2. The deletion ([`delete_set_with`], cause [`Cause::Removed`]) with the
///    journal row, the case, its targets and the removal records in its D1
///    batch.
/// 3. A last read of the tombstone: a live page or a run still on it (none
///    is expected) is deleted the same way, and failing that `x_delete`
///    finishes it.
///
/// On an R2 or D1 failure in step 2 nothing of that batch is written and
/// `x_delete` is sent, so the deletion completes; the caller retries for the
/// case and journal (the tombstone stays meanwhile).
pub async fn remove_post(io: &impl Io, origins: &Origins, r: &Removal, journal: Vec<Stmt>) -> Result<Removed, IoError> {
    let now = io.now_ms();
    let key = Val::blob(&r.canon_key);
    let found = io
        .batch(vec![Stmt::new(UPSERT_TOMBSTONE, vec![key.clone()]), Stmt::new(SELECT_BY_KEY, vec![key.clone()])])
        .await?;
    let source = found.get(1).and_then(Done::first).and_then(|x| x.u64("source_id"));
    let case_id = match &r.case {
        CaseRef::Existing(c) | CaseRef::New(c) => c.clone(),
    };
    let mut pre = journal;
    if r.auto_journal {
        pre.push(auto_journal_row(r.reason, &r.canon_key, now));
    }
    let mut post = vec![Stmt::new(UPSERT_TOMBSTONE, vec![key.clone()])];
    match &r.case {
        CaseRef::New(c) => post.push(Stmt::new(INSERT_X_CASE, vec![Val::text(c), Val::int(now), key.clone()])),
        CaseRef::Existing(c) => post.push(Stmt::new(LINK_CASE, vec![Val::text(c), key.clone(), Val::int(now)])),
    }
    post.push(Stmt::new(CASE_TARGETS, vec![Val::text(&case_id), key.clone()]));
    post.push(Stmt::new(RECORD_REMOVALS, vec![Val::int(now), key.clone(), Val::text(removal_basis(r.reason))]));
    let n_pre = pre.len();
    let ids: Vec<u64> = source.into_iter().collect();
    let done = match delete_set_with(io, origins, &ids, Cause::Removed, now, pre, post).await {
        Ok((_, done)) => done,
        Err(e) => {
            if let Some(s) = source {
                let _ = send_x_delete(io, &[s], XDeleteCause::XRemoved).await;
            }
            return Err(e);
        }
    };
    // The first deletion statement moved the pages to `removed`.
    let removed = if source.is_some() { done.get(n_pre).map_or(0, |d| d.changes) } else { 0 };
    if let Some(s) = source {
        settle(io, origins, s, now).await;
    }
    io.metric("removal", &["x", "removed"]);
    Ok(Removed { case_id, removed })
}

/// Whether the tombstone `source_id` still holds a live page or a run.
async fn left_on(io: &impl Io, source_id: u64) -> Result<bool, IoError> {
    let d = io.run(Stmt::new(SELECT_LEFT, vec![Val::int(source_id)])).await?;
    Ok(d.first().and_then(|r| r.u64("n")).unwrap_or(0) > 0)
}

/// A removal's last read (step 3): anything still on the tombstone is
/// deleted with one more pass; a failure, or anything left after it, goes
/// to `x_delete`. The removal itself is already written, so this never
/// fails it.
async fn settle(io: &impl Io, origins: &Origins, source_id: u64, now: u64) {
    let settled = match left_on(io, source_id).await {
        Ok(false) => true,
        Ok(true) => {
            delete_set(io, origins, &[source_id], Cause::Removed, now).await.is_ok()
                && matches!(left_on(io, source_id).await, Ok(false))
        }
        Err(IoError) => false,
    };
    if !settled {
        let _ = send_x_delete(io, &[source_id], XDeleteCause::XRemoved).await;
    }
}

/// `restore {case_id}` on an X case: lifts the opt-out and nothing more.
/// One D1 batch in foreign-key order: the tombstone's detections let go of
/// it (they stay `removed`), every case that names it lets go (this one
/// `restored`), then the tombstone is deleted. `journal` is the op route's
/// row. `Ok(false)`: the case's tombstone is already gone (purged or
/// restored through another case), and nothing changed.
pub async fn restore_x(io: &impl Io, case_id: &str, journal: Vec<Stmt>) -> Result<bool, IoError> {
    let d = io.run(Stmt::new("SELECT source_id FROM cases WHERE case_id = ?1", vec![Val::text(case_id)])).await?;
    let Some(source_id) = d.first().and_then(|r| r.u64("source_id")) else { return Ok(false) };
    let now = io.now_ms();
    let mut batch = journal;
    batch.push(Stmt::new("UPDATE detections SET source_id = NULL WHERE source_id = ?1", vec![Val::int(source_id)]));
    batch.push(Stmt::new(
        "UPDATE cases SET state = CASE WHEN case_id = ?2 THEN 'restored' ELSE state END, source_id = NULL, \
updated_at = ?3 WHERE source_id = ?1",
        vec![Val::int(source_id), Val::text(case_id), Val::int(now)],
    ));
    batch.push(Stmt::new(
        "DELETE FROM sources WHERE source_id = ?1 AND adapter = 'x' AND state = 'opted_out' AND canon_url IS NULL",
        vec![Val::int(source_id)],
    ));
    io.batch(batch).await?;
    Ok(true)
}

// ------------------------------------------------------------------ purge

/// What `x_purge {dry_run: true}` counts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PurgeCounts {
    /// X detections by state.
    pub detections: Vec<(String, u64)>,
    pub runs: u64,
    /// Run documents (one per run).
    pub run_docs: u64,
    /// X sources by state.
    pub sources: Vec<(String, u64)>,
    /// Cases that name a source.
    pub cases: u64,
    /// `x_remove` journal rows that still hold a `canon_key`.
    pub journal: u64,
}

impl PurgeCounts {
    /// Whether nothing of X's is left (the closing dry run).
    pub fn is_zero(&self) -> bool {
        let live: u64 = self
            .detections
            .iter()
            .filter(|(s, _)| !matches!(s.as_str(), "deleted" | "expired" | "removed"))
            .map(|(_, n)| n)
            .sum();
        live == 0
            && self.runs == 0
            && self.run_docs == 0
            && self.sources.iter().all(|(_, n)| *n == 0)
            && self.cases == 0
            && self.journal == 0
    }
}

/// The `x_remove` journal rows that still hold a hash (a row whose `op` is
/// not JSON is never read as one).
pub const X_REMOVE_ROWS: &str = "CASE WHEN json_valid(op) THEN json_extract(op, '$.op') END = 'x_remove' \
AND CASE WHEN json_valid(op) THEN json_extract(op, '$.args.canon_key') END IS NOT NULL";

/// `x_purge {dry_run: true}`: counts only; the op route writes its journal
/// row and nothing else.
pub async fn purge_counts(io: &impl Io) -> Result<PurgeCounts, IoError> {
    let d = io
        .batch(vec![
            Stmt::new("SELECT state, count(*) AS n FROM detections WHERE platform = 'x' GROUP BY state ORDER BY state", vec![]),
            Stmt::new(
                "SELECT count(DISTINCT d.run_id) AS n FROM detections d WHERE d.platform = 'x' AND d.run_id IS NOT NULL",
                vec![],
            ),
            Stmt::new("SELECT state, count(*) AS n FROM sources WHERE adapter = 'x' GROUP BY state ORDER BY state", vec![]),
            Stmt::new(
                "SELECT count(*) AS n FROM cases c JOIN sources s ON s.source_id = c.source_id WHERE s.adapter = 'x'",
                vec![],
            ),
            Stmt::owned(format!("SELECT count(*) AS n FROM op_journal WHERE {X_REMOVE_ROWS}"), vec![]),
        ])
        .await?;
    let pairs = |d: Option<&Done>| -> Vec<(String, u64)> {
        d.map(|d| d.rows.iter().filter_map(|r| Some((r.text("state")?.to_owned(), r.u64("n")?))).collect())
            .unwrap_or_default()
    };
    let one = |d: Option<&Done>| d.and_then(Done::first).and_then(|r| r.u64("n")).unwrap_or(0);
    let runs = one(d.get(1));
    Ok(PurgeCounts {
        detections: pairs(d.first()),
        runs,
        run_docs: runs,
        sources: pairs(d.get(2)),
        cases: one(d.get(3)),
        journal: one(d.get(4)),
    })
}

/// `x_purge {dry_run: false}`: the statements of the op's own batch beside
/// its journal row (`DETECT_X = purged`, recorded under that row), and the
/// first page's message to send once the batch is in.
pub fn purge_start(now: u64, journal_seq: u64) -> (Vec<Stmt>, QueueMessage) {
    let set = Stmt::new(
        "INSERT INTO switches (name, state, at, journal_seq) VALUES ('DETECT_X', 'purged', ?1, \
(SELECT max(seq) FROM op_journal)) ON CONFLICT (name) DO UPDATE SET state = 'purged', at = excluded.at, \
journal_seq = excluded.journal_seq",
        vec![Val::int(now)],
    );
    (vec![set], QueueMessage::new(Job::XPurgePage { journal_seq }))
}

/// The journal rows one purge page strips (their signature goes with the
/// bytes it covered).
pub fn strip_journal(limit: u64) -> Stmt {
    Stmt::owned(
        format!(
            "UPDATE op_journal SET op = json_remove(op, '$.args.canon_key'), signature = NULL \
WHERE seq IN (SELECT seq FROM op_journal WHERE {X_REMOVE_ROWS} ORDER BY seq LIMIT ?1)"
        ),
        vec![Val::int(limit)],
    )
}

/// What one purge page did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PurgePage {
    pub sources: usize,
    pub journal_rows: u64,
    /// More is left: the next page was sent.
    pub next: bool,
}

/// One `x_purge_page {journal_seq}`: deletes up to `cfg.batch` X sources
/// (tombstones included) as one set, strips `canon_key` from up to as many
/// `x_remove` journal rows, ends the X pages no source holds, and sends the
/// next page while anything is left. It writes no point per source.
pub async fn purge_page(
    io: &impl Io,
    origins: &Origins,
    cfg: &XConfig,
    journal_seq: u64,
) -> Result<PurgePage, IoError> {
    let d = io
        .run(Stmt::new(
            "SELECT source_id FROM sources WHERE adapter = 'x' ORDER BY source_id LIMIT ?1",
            vec![Val::int(cfg.batch)],
        ))
        .await?;
    let ids: Vec<u64> = d.rows.iter().filter_map(|r| r.u64("source_id")).collect();
    let mut page = PurgePage { sources: ids.len(), ..PurgePage::default() };
    if !ids.is_empty() {
        delete_set(io, origins, &ids, Cause::Purged, io.now_ms()).await?;
    }
    let now = io.now_ms();
    let done = io
        .batch(vec![
            strip_journal(cfg.batch),
            Stmt::new(
                "UPDATE detections SET state = 'deleted', refusal = 'x_purged', run_id = NULL, \
state_v = state_v + 1, updated_at = ?1 WHERE platform = 'x' AND source_id IS NULL \
AND state IN ('pending','failed','live','excerpt_withheld')",
                vec![Val::int(now)],
            ),
            Stmt::owned(
                format!(
                    "SELECT (SELECT count(*) FROM sources WHERE adapter = 'x') AS s, \
(SELECT count(*) FROM op_journal WHERE {X_REMOVE_ROWS}) AS j"
                ),
                vec![],
            ),
        ])
        .await?;
    page.journal_rows = done.first().map_or(0, |d| d.changes);
    let left = done.get(2).and_then(Done::first);
    let more = left.is_some_and(|r| r.u64("s").unwrap_or(0) > 0 || r.u64("j").unwrap_or(0) > 0);
    if more {
        io.enqueue(vec![QueueMessage::new(Job::XPurgePage { journal_seq })]).await?;
        page.next = true;
    }
    Ok(page)
}

// ----------------------------------------------------------------- status

/// The slugs of live X pages whose source has gone unchecked longer than
/// [`X_LATE_MS`], oldest first, after `after` (a slug) for paging. Slugs are
/// ours, so this names no post.
pub const SELECT_LATE_PAGES: &str = "SELECT d.public_slug AS public_slug FROM detections d \
JOIN sources s ON s.source_id = d.source_id WHERE d.platform = 'x' AND d.state = 'live' \
AND s.canon_url IS NOT NULL AND coalesce(s.fetched_at, 0) < ?1 AND d.public_slug > ?2 \
ORDER BY coalesce(s.fetched_at, 0), d.public_slug LIMIT ?3";

/// The `status` op's X lines.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct XStatus {
    /// Age in seconds of the oldest check (0 with no X source).
    pub oldest_check_s: u64,
    /// Live X pages unchecked for over 24 hours (one page of them).
    pub late_pages: Vec<String>,
    /// X sources left (the purge's progress).
    pub sources: u64,
}

pub async fn status(io: &impl Io, after: &str, limit: u64) -> Result<XStatus, IoError> {
    let now = io.now_ms();
    let oldest = oldest_age_ms(io, now).await?;
    let d = io
        .batch(vec![
            Stmt::new(
                SELECT_LATE_PAGES,
                vec![Val::int(now.saturating_sub(X_LATE_MS)), Val::text(after), Val::int(limit)],
            ),
            Stmt::new("SELECT count(*) AS n FROM sources WHERE adapter = 'x'", vec![]),
        ])
        .await?;
    Ok(XStatus {
        oldest_check_s: oldest / 1000,
        late_pages: d
            .first()
            .map(|d| d.rows.iter().filter_map(|r| r.text("public_slug")).map(str::to_owned).collect())
            .unwrap_or_default(),
        sources: d.get(1).and_then(Done::first).and_then(|r| r.u64("n")).unwrap_or(0),
    })
}

/// After a D1 point-in-time restore and its replay: every X source that
/// holds a `canon_url` is due at once (0, not NULL), so the next sweeps
/// delete the posts X deleted meanwhile.
pub const REPLAY_RECHECK: &str = "UPDATE sources SET fetched_at = 0 WHERE adapter = 'x' AND canon_url IS NOT NULL";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_batch_is_derived_from_the_limits() {
        // At a 10 s call: floor((900 − 60) / 10) − 1 = 83 rounds of 6.
        assert_eq!(x_check_batch(10_000), 498);
        assert_eq!(XConfig::new(10_000).batch, 498);
        // At 7 s: floor(840 / 7) − 1 = 119 rounds.
        assert_eq!(x_check_batch(7_000), 714);
        // The subrequest term never binds at a realistic call time, even
        // with every source gone and holding RUNS_PER_SOURCE documents.
        for ms in [1_000u64, 3_000, 7_000, 10_000, 20_000] {
            let time = K * ((CONSUMER_WALL_MS - CONSUMER_FIXED_MS) / ms - 1);
            assert_eq!(x_check_batch(ms), time.min(x_check_batch(ms)));
            assert!(x_check_batch(ms) <= SUBREQUESTS - CONSUMER_SUBREQUESTS - 2);
        }
        // A call time near the whole wall time leaves no batch.
        assert_eq!(x_check_batch(CONSUMER_WALL_MS), 0);
    }

    #[test]
    fn json_ids_is_a_json_array() {
        assert_eq!(json_ids(&[]), "[]");
        assert_eq!(json_ids(&[1, 22, 333]), "[1,22,333]");
        let v: Vec<u64> = serde_json::from_str(&json_ids(&[7, u64::from(u32::MAX)])).unwrap();
        assert_eq!(v, [7, u64::from(u32::MAX)]);
    }

    #[test]
    fn x_remove_names_the_post_by_its_hash_only() {
        let key = x_canon_key(1855);
        assert_eq!(key, crate::keys::h(crate::keys::SOURCE_DOMAIN, b"x:status:1855"));
        let j = x_remove_op_json(XReason::Author, &key);
        assert!(j.contains(&hex::encode(key)) && !j.contains("1855") && !j.contains("status"));
        assert_eq!(XReason::parse("x"), Some(XReason::X));
        assert_eq!(XReason::parse("X"), None);
        assert_eq!(XReason::of_track(CaseKind::Author, false), XReason::Author);
        assert_eq!(XReason::of_track(CaseKind::Copyright, true), XReason::X);
    }

    #[test]
    fn the_sweep_path_makes_one_call_and_no_object_call() {
        // Source test over this module: the one DETECT_FETCH call of a sweep
        // message is outside every loop, and no Durable Object is reached.
        let src = include_str!("x_flow.rs");
        let sweep = &src[src.find("pub async fn sweep(").unwrap()..src.find("async fn lateness(").unwrap()];
        assert_eq!(sweep.matches("net.x_check(").count(), 1);
        assert!(!sweep.contains("net.fetch(") && !sweep.contains("net.x_post("));
        let before = &sweep[..sweep.find("net.x_check(").unwrap()];
        for kw in ["for ", "while ", "loop {"] {
            assert!(!before.contains(kw), "a loop before the call: {kw}");
        }
        for banned in ["HostApi", "stub(", "DETECT_HOST", ".slot(", ".screen("] {
            assert!(!sweep.contains(banned), "{banned}");
        }
    }
}

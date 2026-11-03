//! The relay's sent-event ledger: each event id is delivered once per sink
//! (the alert feed's object under [`crate::feed::ledger_key`], the email
//! under the bare id). An email is sent twice at most after an attempt whose
//! outcome was unknown; a feed object is written until a put is
//! acknowledged ([`settle_acknowledged`]: a reader dedups by event id, so
//! a second copy is harmless and a lost one is not). Below, "send" is
//! either sink's write or send.
//!
//! Before sending, the relay claims every id of the batch; a claimed id is
//! sent by its claimant only, a sent id is acknowledged without sending
//! again. Ids are the senders' [`slop_ninja_alerts_wire::event_id`]s, so a
//! post retried after a lost answer, or a relay tick that overlaps another,
//! finds its ids claimed or sent.
//!
//! - A claim lasts [`CLAIM_MS`] (one cron period: a retry arrives no sooner
//!   than the sender's next run). A claim whose holder never finished (the
//!   isolate stopped) is taken over after that.
//! - A refused send (nothing went out) releases its claims at once, so the
//!   next attempt can claim them.
//! - A send that timed out may have gone out. Its ids become
//!   [`Mark::Uncertain`], held for one more claim period and then retried.
//!   For email ([`settle`]) the retry's outcome is final: sent, a second
//!   timeout (recorded as sent, so an event is emailed at most twice), or
//!   refused (free again, and still one timeout behind, so a later timeout
//!   also records it as sent); an event whose two sends both timed out
//!   without delivering is lost. For the feed ([`settle_acknowledged`]) a
//!   timeout is never final: the id stays uncertain and is written again
//!   after each claim period until a put is acknowledged.
//! - A sender keeps posting an id until its answer is acknowledged; the id
//!   never changes until then. A claim that finds the id sent refreshes its
//!   row, so a sender that keeps retrying keeps its row.
//! - [`sweep`] deletes rows untouched for [`retention_ms`] (every tick): the
//!   senders' renotify interval, and never less than the time the relay
//!   gives the publisher before it counts it as down
//!   ([`crate::rules::PUBLISHER_DOWN_MS`]), which covers the publisher's
//!   slowest retry while it runs (a run killed while waiting holds its lease
//!   for two cron periods; the next run starts within one more). A retry
//!   after a longer outage finds no row and is sent again. Unsettled rows
//!   (claims, unknown outcomes) are kept the same way, so a timed-out send
//!   is retried once, not once per cron period.
//! - The ledger records how far its sweeps reached ([`Table::cutoff`]). An
//!   unconfirmed resolution whose firing has no row is settled without an
//!   email only while the ledger still covers that firing's whole episode
//!   (it started at or after the cutoff); otherwise its firing may have been
//!   swept, and the resolution is emailed ([`maybe_sent`]).

use slop_ninja_alerts_wire::PUBLISHER_CRON_PERIOD_MS;

use crate::feed::{FiringEntry, WriteNote};
use crate::relay::SendError;

/// How long a claim excludes other senders.
pub const CLAIM_MS: u64 = PUBLISHER_CRON_PERIOD_MS;

/// How long a row is kept after it was last touched, for a sender renotify
/// interval of `renotify_ms` (see the module note).
pub const fn retention_ms(renotify_ms: u64) -> u64 {
    let floor = crate::rules::PUBLISHER_DOWN_MS;
    if renotify_ms > floor {
        renotify_ms
    } else {
        floor
    }
}
const _: () = assert!(retention_ms(0) >= 2 * CLAIM_MS);

/// The state of one id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// Claimed by a sender that is sending it.
    Claimed,
    /// Sent (or recorded as sent after its second unknown outcome).
    Sent,
    /// One send timed out (it may have gone out). Held as a claim until
    /// [`CLAIM_MS`] has passed, then retried once.
    Uncertain,
    /// One send timed out and a later one was refused: free to claim, and
    /// the next attempt is that retry.
    UncertainFree,
}

impl Mark {
    /// The stored integer (the Durable Object's `mark` column).
    pub fn code(self) -> u64 {
        match self {
            Self::Claimed => 0,
            Self::Sent => 1,
            Self::Uncertain => 2,
            Self::UncertainFree => 3,
        }
    }

    pub fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            0 => Self::Claimed,
            1 => Self::Sent,
            2 => Self::Uncertain,
            3 => Self::UncertainFree,
            _ => return None,
        })
    }
}

/// One ledger row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub mark: Mark,
    /// When it was claimed, sent, or its last send ended.
    pub at_ms: u64,
}

/// A store failure (content-free).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreError(pub &'static str);

/// The relay's durable store: the ledger's table and the alert feed's
/// index in the Durable Object's SQLite, and the feed's objects in its
/// private bucket (maps in tests). The SQLite calls are synchronous
/// (Durable Object SQL is); the bucket calls are the only awaits.
#[allow(async_fn_in_trait)]
pub trait Table {
    fn get(&self, id: &str) -> Result<Option<Row>, StoreError>;
    fn put(&self, id: &str, row: Row) -> Result<(), StoreError>;
    fn remove(&self, id: &str) -> Result<(), StoreError>;
    /// Removes rows whose `at_ms` is before `before_ms`, and records the
    /// cutoff (the largest one so far).
    fn sweep(&self, before_ms: u64) -> Result<(), StoreError>;
    /// The largest cutoff any sweep applied: rows touched before it may be
    /// gone; rows touched since are all there.
    fn cutoff(&self) -> Result<u64, StoreError>;

    // The alert feed ([`crate::feed`]).

    /// A feed key's time: at least `now_ms` (and the clock at the call, in
    /// the Worker) and strictly after the last one stamped, so keys sort in
    /// write order.
    fn feed_stamp(&self, now_ms: u64) -> Result<u64, StoreError>;
    /// Records a key before its object is put, so the retention sweep
    /// deletes it even when the put's outcome was unknown.
    fn feed_record(&self, key: &str, at_ms: u64) -> Result<(), StoreError>;
    /// Puts one object.
    async fn feed_put(&self, key: &str, body: Vec<u8>) -> Result<(), SendError>;
    /// Sets (`Some`, at `at_ms`) or clears (`None`) one entry of the firing
    /// set.
    fn feed_firing(
        &self,
        key: &str,
        entry: Option<&FiringEntry>,
        at_ms: u64,
    ) -> Result<(), StoreError>;
    /// Drops firing-set rows last set before `before_ms`
    /// ([`crate::feed::firing_ttl_ms`]).
    fn feed_firing_sweep(&self, before_ms: u64) -> Result<(), StoreError>;
    /// Records the last event write's outcome.
    fn feed_note(&self, note: &WriteNote) -> Result<(), StoreError>;
    /// Up to `max` recorded keys written before `before_ms`, oldest first.
    fn feed_expired(&self, before_ms: u64, max: usize) -> Result<Vec<String>, StoreError>;
    /// Deletes objects.
    async fn feed_delete(&self, keys: &[String]) -> Result<(), SendError>;
    /// Drops deleted keys from the record.
    fn feed_forget(&self, keys: &[String]) -> Result<(), StoreError>;
    /// The firing set's entries, oldest first, as many as fit one post's
    /// events bound ([`slop_ninja_alerts_wire::POST_EVENTS_BYTES`]), and
    /// how many it holds in all.
    fn feed_firing_entries(&self) -> Result<(Vec<FiringEntry>, u64), StoreError>;
    /// The last event write's outcome.
    fn feed_last_note(&self) -> Result<Option<WriteNote>, StoreError>;
}

/// What a claim found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// This caller holds it now and sends it; `retry` when an earlier send
    /// of it timed out (this attempt's outcome is final).
    Mine { retry: bool },
    /// Already sent: acknowledge without sending.
    Sent,
    /// Another sender holds a live claim: answer "retry".
    Busy,
}

/// The decision for one id, given its row.
pub fn decide(row: Option<Row>, now_ms: u64) -> Claim {
    let live = |at_ms: u64| now_ms.saturating_sub(at_ms) < CLAIM_MS;
    match row {
        None => Claim::Mine { retry: false },
        Some(Row {
            mark: Mark::Sent, ..
        }) => Claim::Sent,
        Some(Row {
            mark: Mark::Claimed | Mark::Uncertain,
            at_ms,
        }) if live(at_ms) => Claim::Busy,
        Some(Row {
            mark: Mark::Claimed,
            ..
        }) => Claim::Mine { retry: false },
        Some(Row {
            mark: Mark::Uncertain | Mark::UncertainFree,
            ..
        }) => Claim::Mine { retry: true },
    }
}

/// Claims `id` when it is free (or its claim expired). A claim that finds it
/// sent refreshes its row: the sender is still retrying it.
pub fn claim(t: &impl Table, id: &str, now_ms: u64) -> Result<Claim, StoreError> {
    let c = decide(t.get(id)?, now_ms);
    let mark = match c {
        Claim::Mine { retry: true } => Mark::Uncertain,
        Claim::Mine { retry: false } => Mark::Claimed,
        Claim::Sent => Mark::Sent,
        Claim::Busy => return Ok(c),
    };
    t.put(
        id,
        Row {
            mark,
            at_ms: now_ms,
        },
    )?;
    Ok(c)
}

/// How one send of a claimed id ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Sent,
    /// Nothing went out.
    Refused,
    /// No answer within the bound: it may have gone out.
    TimedOut,
}

/// Records how the send of `id` (claimed as `retry`) ended. Returns whether
/// the id now counts as sent.
pub fn settle(
    t: &impl Table,
    id: &str,
    retry: bool,
    outcome: Outcome,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let put = |mark| {
        t.put(
            id,
            Row {
                mark,
                at_ms: now_ms,
            },
        )
    };
    match (outcome, retry) {
        (Outcome::Sent, _) | (Outcome::TimedOut, true) => put(Mark::Sent).map(|()| true),
        (Outcome::TimedOut, false) => put(Mark::Uncertain).map(|()| false),
        (Outcome::Refused, true) => put(Mark::UncertainFree).map(|()| false),
        (Outcome::Refused, false) => t.remove(id).map(|()| false),
    }
}

/// Records how a write of `id` to a sink that dedups by event id (the alert
/// feed) ended: only an acknowledged write counts as delivered. A timed-out
/// write stays [`Mark::Uncertain`] (held one claim period, then written
/// again), however often it times out, so a write that never landed is
/// never acknowledged to its sender; a retry may land a second copy under a
/// new key, which a reader drops by its event id. A refused write frees
/// the id (kept as possibly written after an earlier timeout). Returns
/// whether the id now counts as delivered.
pub fn settle_acknowledged(
    t: &impl Table,
    id: &str,
    retry: bool,
    outcome: Outcome,
    now_ms: u64,
) -> Result<bool, StoreError> {
    match outcome {
        Outcome::Sent => settle(t, id, retry, outcome, now_ms),
        Outcome::TimedOut => t
            .put(
                id,
                Row {
                    mark: Mark::Uncertain,
                    at_ms: now_ms,
                },
            )
            .map(|()| false),
        Outcome::Refused => settle(t, id, retry, outcome, now_ms),
    }
}

/// Whether the firing `id`, of an episode first seen at `since_ms` (the
/// sender's clock), may have been emailed: it has a row (sent, or a send of
/// it is out or timed out), or the ledger no longer covers the episode (a
/// sweep reached past its start, so a row for it may be gone). An
/// unconfirmed resolution is emailed only then.
pub fn maybe_sent(t: &impl Table, id: &str, since_ms: u64) -> Result<bool, StoreError> {
    Ok(t.get(id)?.is_some() || since_ms < t.cutoff()?)
}

/// Deletes rows untouched for `retain_ms` ([`retention_ms`]).
pub fn sweep(t: &impl Table, now_ms: u64, retain_ms: u64) -> Result<(), StoreError> {
    t.sweep(now_ms.saturating_sub(retain_ms.max(2 * CLAIM_MS)))
}

/// An in-memory store (tests and tools). `feed_script` answers the next
/// puts in order (then `Ok`), so tests can make a write time out or fail.
#[derive(Debug, Default)]
pub struct MemTable {
    rows: std::cell::RefCell<std::collections::BTreeMap<String, Row>>,
    cutoff: std::cell::Cell<u64>,
    stamp: std::cell::Cell<u64>,
    records: std::cell::RefCell<std::collections::BTreeMap<String, u64>>,
    objects: std::cell::RefCell<std::collections::BTreeMap<String, Vec<u8>>>,
    firing: std::cell::RefCell<std::collections::BTreeMap<String, (u64, FiringEntry)>>,
    note: std::cell::RefCell<Option<WriteNote>>,
    pub feed_script: std::cell::RefCell<Vec<Result<(), SendError>>>,
    /// Scripted timeouts land nothing (a put that never reached the bucket)
    /// instead of landing their object.
    pub feed_timeouts_lost: std::cell::Cell<bool>,
}

impl MemTable {
    /// The number of ledger rows held.
    pub fn len(&self) -> usize {
        self.rows.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The feed objects' keys, in the bucket's (lexicographic) order.
    pub fn objects(&self) -> Vec<String> {
        self.objects.borrow().keys().cloned().collect()
    }

    /// One feed object's body.
    pub fn object(&self, key: &str) -> Option<Vec<u8>> {
        self.objects.borrow().get(key).cloned()
    }

    /// The feed's event objects, decoded, in key order.
    pub fn feed(&self) -> Vec<crate::feed::Object> {
        self.objects
            .borrow()
            .iter()
            .filter(|(k, _)| k.starts_with(crate::feed::EVENTS_PREFIX))
            .filter_map(|(_, v)| serde_json::from_slice(v).ok())
            .collect()
    }

    /// The firing set, in key order.
    pub fn firing_set(&self) -> Vec<FiringEntry> {
        self.firing
            .borrow()
            .values()
            .map(|(_, e)| e.clone())
            .collect()
    }

    /// The last write's outcome.
    pub fn last_note(&self) -> Option<WriteNote> {
        self.note.borrow().clone()
    }

    /// Makes the next puts answer `answers` in order.
    pub fn script_feed(&self, answers: Vec<Result<(), SendError>>) {
        *self.feed_script.borrow_mut() = answers.into_iter().rev().collect();
    }
}

impl Table for MemTable {
    fn get(&self, id: &str) -> Result<Option<Row>, StoreError> {
        Ok(self.rows.borrow().get(id).copied())
    }
    fn put(&self, id: &str, row: Row) -> Result<(), StoreError> {
        self.rows.borrow_mut().insert(id.to_owned(), row);
        Ok(())
    }
    fn remove(&self, id: &str) -> Result<(), StoreError> {
        self.rows.borrow_mut().remove(id);
        Ok(())
    }
    fn sweep(&self, before_ms: u64) -> Result<(), StoreError> {
        let cutoff = self.cutoff.get().max(before_ms);
        self.rows.borrow_mut().retain(|_, r| r.at_ms >= cutoff);
        self.cutoff.set(cutoff);
        Ok(())
    }
    fn cutoff(&self) -> Result<u64, StoreError> {
        Ok(self.cutoff.get())
    }
    fn feed_stamp(&self, now_ms: u64) -> Result<u64, StoreError> {
        let at = now_ms.max(self.stamp.get().saturating_add(1));
        self.stamp.set(at);
        Ok(at)
    }
    fn feed_record(&self, key: &str, at_ms: u64) -> Result<(), StoreError> {
        self.records.borrow_mut().insert(key.to_owned(), at_ms);
        Ok(())
    }
    async fn feed_put(&self, key: &str, body: Vec<u8>) -> Result<(), SendError> {
        let answer = self.feed_script.borrow_mut().pop().unwrap_or(Ok(()));
        // A put that timed out may still have landed: tests see it written,
        // unless they script lost timeouts.
        let landed = match answer {
            Ok(()) => true,
            Err(SendError::Timeout) => !self.feed_timeouts_lost.get(),
            Err(SendError::Refused(_)) => false,
        };
        if landed {
            // Kept at its length (the Worker hands the body to the bucket and
            // drops it).
            let mut body = body;
            body.shrink_to_fit();
            self.objects.borrow_mut().insert(key.to_owned(), body);
        }
        answer
    }
    fn feed_firing(
        &self,
        key: &str,
        entry: Option<&FiringEntry>,
        at_ms: u64,
    ) -> Result<(), StoreError> {
        let mut f = self.firing.borrow_mut();
        match entry {
            Some(e) => f.insert(key.to_owned(), (at_ms, e.clone())),
            None => f.remove(key),
        };
        Ok(())
    }
    fn feed_firing_sweep(&self, before_ms: u64) -> Result<(), StoreError> {
        self.firing
            .borrow_mut()
            .retain(|_, (at, _)| *at >= before_ms);
        Ok(())
    }
    fn feed_note(&self, note: &WriteNote) -> Result<(), StoreError> {
        *self.note.borrow_mut() = Some(note.clone());
        Ok(())
    }
    fn feed_expired(&self, before_ms: u64, max: usize) -> Result<Vec<String>, StoreError> {
        let mut old: Vec<(u64, String)> = self
            .records
            .borrow()
            .iter()
            .filter(|(_, at)| **at < before_ms)
            .map(|(k, at)| (*at, k.clone()))
            .collect();
        old.sort();
        Ok(old.into_iter().take(max).map(|(_, k)| k).collect())
    }
    async fn feed_delete(&self, keys: &[String]) -> Result<(), SendError> {
        let mut o = self.objects.borrow_mut();
        for k in keys {
            o.remove(k);
        }
        Ok(())
    }
    fn feed_forget(&self, keys: &[String]) -> Result<(), StoreError> {
        let mut r = self.records.borrow_mut();
        for k in keys {
            r.remove(k);
        }
        Ok(())
    }
    fn feed_firing_entries(&self) -> Result<(Vec<FiringEntry>, u64), StoreError> {
        let f = self.firing.borrow();
        let mut rows: Vec<&(u64, FiringEntry)> = f.values().collect();
        rows.sort_by_key(|(at, _)| *at);
        Ok((
            rows.into_iter().map(|(_, e)| e.clone()).collect(),
            f.len() as u64,
        ))
    }
    fn feed_last_note(&self) -> Result<Option<WriteNote>, StoreError> {
        Ok(self.note.borrow().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: Claim = Claim::Mine { retry: false };
    const RETRY: Claim = Claim::Mine { retry: true };

    #[test]
    fn claims_settles_and_sweeps() {
        let t = MemTable::default();
        let now = 10_000_000;
        assert_eq!(claim(&t, "a", now).unwrap(), FIRST);
        // A concurrent sender (an overlapping tick, or a retry while the
        // first send is still out) waits.
        assert_eq!(claim(&t, "a", now + 1).unwrap(), Claim::Busy);
        assert!(settle(&t, "a", false, Outcome::Sent, now + 2).unwrap());
        // A retried post finds it sent: acknowledged without a second email.
        assert_eq!(claim(&t, "a", now + 60_000).unwrap(), Claim::Sent);
        // A refused send frees its claim at once.
        assert_eq!(claim(&t, "b", now).unwrap(), FIRST);
        assert!(!settle(&t, "b", false, Outcome::Refused, now).unwrap());
        assert!(!maybe_sent(&t, "b", now).unwrap());
        assert_eq!(claim(&t, "b", now + 1).unwrap(), FIRST);
        // A claim whose holder never finished is taken over after CLAIM_MS.
        assert_eq!(claim(&t, "b", now + 1 + CLAIM_MS - 1).unwrap(), Claim::Busy);
        assert_eq!(claim(&t, "b", now + 1 + CLAIM_MS).unwrap(), FIRST);
        // A retry that finds it sent refreshes the row: kept for the
        // retention after the sender's last retry, then swept.
        let last = now + 60_000;
        sweep(&t, last + 3_600_000, 3_600_000).unwrap();
        assert_eq!(t.get("a").unwrap().map(|r| r.mark), Some(Mark::Sent));
        sweep(&t, last + 1 + 3_600_000, 3_600_000).unwrap();
        assert_eq!(t.get("a").unwrap(), None);
        assert_eq!(t.cutoff().unwrap(), last + 1);
        // A sweep that reaches less far never lowers the cutoff.
        sweep(&t, last + 2 + 3_600_000, 3_600_000 * 2).unwrap();
        assert_eq!(t.cutoff().unwrap(), last + 1);
    }

    /// The retention never falls below the publisher-down bound, whatever
    /// the renotify interval, so a slow retry still finds its row; past the
    /// cutoff, an episode's firing counts as possibly sent.
    #[test]
    fn retention_covers_the_publishers_retries() {
        assert_eq!(retention_ms(60_000), crate::rules::PUBLISHER_DOWN_MS);
        assert_eq!(retention_ms(6 * 3_600_000), 6 * 3_600_000);
        let t = MemTable::default();
        let now = 10_000_000;
        assert_eq!(claim(&t, "e", now).unwrap(), FIRST);
        settle(&t, "e", false, Outcome::TimedOut, now).unwrap();
        // Swept every minute with the shortest renotify interval: the
        // unknown outcome survives until the publisher's slowest retry.
        let retry = now + crate::rules::PUBLISHER_DOWN_MS;
        sweep(&t, retry, retention_ms(60_000)).unwrap();
        assert_eq!(claim(&t, "e", retry).unwrap(), RETRY);
        // An episode that started before the cutoff may have had its firing
        // swept; one that started after it has every row it ever had.
        let t = MemTable::default();
        sweep(&t, now, 0).unwrap();
        let cut = t.cutoff().unwrap();
        assert!(maybe_sent(&t, "f", cut - 1).unwrap());
        assert!(!maybe_sent(&t, "f", cut).unwrap());
    }

    /// A timed-out send is held one claim period, retried once, and its
    /// second unknown outcome records it as sent: at most two emails.
    #[test]
    fn unknown_outcomes_are_retried_once() {
        let t = MemTable::default();
        let now = 10_000_000;
        assert_eq!(claim(&t, "c", now).unwrap(), FIRST);
        assert!(!settle(&t, "c", false, Outcome::TimedOut, now + 5).unwrap());
        assert!(maybe_sent(&t, "c", now).unwrap());
        assert_eq!(claim(&t, "c", now + 5 + CLAIM_MS - 1).unwrap(), Claim::Busy);
        assert_eq!(claim(&t, "c", now + 5 + CLAIM_MS).unwrap(), RETRY);
        assert!(settle(&t, "c", true, Outcome::TimedOut, now + CLAIM_MS + 10).unwrap());
        assert_eq!(claim(&t, "c", now + 10 * CLAIM_MS).unwrap(), Claim::Sent);
        // A refused retry frees it but keeps the count: the next timeout is
        // final too.
        assert_eq!(claim(&t, "d", now).unwrap(), FIRST);
        settle(&t, "d", false, Outcome::TimedOut, now).unwrap();
        assert_eq!(claim(&t, "d", now + CLAIM_MS).unwrap(), RETRY);
        settle(&t, "d", true, Outcome::Refused, now + CLAIM_MS).unwrap();
        assert_eq!(claim(&t, "d", now + CLAIM_MS + 1).unwrap(), RETRY);
        assert!(settle(&t, "d", true, Outcome::TimedOut, now + CLAIM_MS + 2).unwrap());
        for m in [
            Mark::Claimed,
            Mark::Sent,
            Mark::Uncertain,
            Mark::UncertainFree,
        ] {
            assert_eq!(Mark::from_code(m.code()), Some(m));
        }
        assert_eq!(Mark::from_code(4), None);
    }

    /// A feed write that keeps timing out is never recorded as delivered:
    /// it is written again after every claim period until a put is
    /// acknowledged; a refusal after a timeout keeps it as possibly written.
    #[test]
    fn feed_timeouts_are_never_final() {
        let t = MemTable::default();
        let now = 10_000_000;
        assert_eq!(claim(&t, "f", now).unwrap(), FIRST);
        let mut at = now;
        for _ in 0..5 {
            assert!(!settle_acknowledged(&t, "f", true, Outcome::TimedOut, at).unwrap());
            assert!(maybe_sent(&t, "f", now).unwrap());
            assert_eq!(claim(&t, "f", at + CLAIM_MS - 1).unwrap(), Claim::Busy);
            at += CLAIM_MS;
            assert_eq!(claim(&t, "f", at).unwrap(), RETRY);
        }
        assert!(!settle_acknowledged(&t, "f", true, Outcome::Refused, at).unwrap());
        assert!(maybe_sent(&t, "f", now).unwrap());
        assert_eq!(claim(&t, "f", at + 1).unwrap(), RETRY);
        assert!(settle_acknowledged(&t, "f", true, Outcome::Sent, at + 2).unwrap());
        assert_eq!(claim(&t, "f", at + 3).unwrap(), Claim::Sent);
        // A first refusal frees the id as before.
        assert_eq!(claim(&t, "g", now).unwrap(), FIRST);
        assert!(!settle_acknowledged(&t, "g", false, Outcome::Refused, now).unwrap());
        assert!(!maybe_sent(&t, "g", now).unwrap());
    }
}

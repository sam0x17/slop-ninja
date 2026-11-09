//! The admission gate's decisions (the `DetectGate` Durable Object's memory),
//! natively tested.
//!
//! - **Tickets only.** The gate holds `{ticket id, client key}` and leases,
//!   never a text: the text stays in the requesting invocation while its
//!   ticket waits.
//! - **Fair queue.** Waiting tickets are admitted round-robin across client
//!   keys (an HMAC of the address prefix under a key held only in the gate's
//!   memory), so many tickets from one source lengthen only that source's
//!   turn. Nobody is refused by address: schools share one.
//! - **Slots.** At most `slots` scans run at once: the Container instances
//!   times their sessions, a deployment decision, never an autoscale.
//! - **Leases.** A waiting ticket stays while its invocation keeps asking
//!   (each ask renews it for the hold the invocation names); an admitted one
//!   until it reports done or its scan lease ends (`waits::Waits::
//!   scan_lease_ms`, the lane's own bound plus one hold). A ticket whose
//!   invocation went away is dropped at its lease's end.
//! - **Wakes.** A waiting ask is answered early only when its ticket is
//!   settled (admitted, or dropped from the line); otherwise it is answered
//!   when its hold ends, with its current position. The gate lists the
//!   tickets each change settles ([`Gate::take_settled`]), so a queue change
//!   wakes the one ticket it admits, not every waiting ticket.
//! - **Warmth.** The gate tracks when the instance can next sleep: the last
//!   scan's end plus the idle timeout. A warm starts the instance only if
//!   none is awake and never moves that time; only scans do.
//! - **Capacity.** The ticket table is bounded by the gate's share of memory
//!   (`max_tickets`, derived from the object's arena by the caller); past it
//!   a new ticket is refused as busy.

use std::collections::{BTreeMap, VecDeque};

/// A ticket id (random, chosen by the gate's host).
pub type Ticket = u128;
/// A client key (the gate host's keyed hash of the address prefix).
pub type Client = u64;

/// The gate's deployment parameters.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Scans that may run at once.
    pub slots: usize,
    /// An admitted ticket's lease, derived from the liveness waits
    /// (`waits::Waits::scan_lease_ms`): the lane's bound for a start, a load
    /// and one scan, plus one hold.
    pub scan_lease_ms: u64,
    /// The instance's idle timeout (the Container's inactivity timeout).
    pub idle_ms: u64,
    /// The ticket table's bound.
    pub max_tickets: usize,
}

/// A ticket's state as its invocation sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Admitted,
    /// 1-based position in the admission order.
    Queued { position: usize },
    /// Unknown or expired: the invocation must start over.
    Gone,
}

#[derive(Clone, Copy, Debug)]
struct Waiting {
    ticket: Ticket,
    lease_until: u64,
}

/// What a warm request learns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Warm {
    /// Start the instance now.
    pub start: bool,
    /// The earliest moment the instance could sleep (ms clock).
    pub awake_until: u64,
}

#[derive(Debug)]
pub struct Gate {
    cfg: Config,
    /// Client keys with waiting tickets, in turn order.
    order: VecDeque<Client>,
    waiting: BTreeMap<Client, VecDeque<Waiting>>,
    /// Waiting ticket -> its client.
    owner: BTreeMap<Ticket, Client>,
    /// Admitted ticket -> lease end.
    admitted: BTreeMap<Ticket, u64>,
    awake_until: u64,
    last_service_ms: Option<u64>,
    /// Waiting tickets admitted or dropped since the last `take_settled`.
    settled: Vec<Ticket>,
}

impl Gate {
    pub fn new(cfg: Config) -> Gate {
        Gate {
            cfg,
            order: VecDeque::new(),
            waiting: BTreeMap::new(),
            owner: BTreeMap::new(),
            admitted: BTreeMap::new(),
            awake_until: 0,
            last_service_ms: None,
            settled: Vec::new(),
        }
    }

    pub fn tickets(&self) -> usize {
        self.owner.len() + self.admitted.len()
    }

    pub fn waiting(&self) -> usize {
        self.owner.len()
    }

    pub fn running(&self) -> usize {
        self.admitted.len()
    }

    pub fn awake_until(&self) -> u64 {
        self.awake_until
    }

    /// The most recent completed scan's service time.
    pub fn last_service_ms(&self) -> Option<u64> {
        self.last_service_ms
    }

    /// A new ticket. `None`: the table is full (busy).
    pub fn enter(&mut self, ticket: Ticket, client: Client, now: u64, hold_ms: u64) -> Option<Status> {
        self.expire(now);
        if self.owner.contains_key(&ticket) || self.admitted.contains_key(&ticket) {
            return Some(self.status(ticket));
        }
        if self.tickets() >= self.cfg.max_tickets {
            return None;
        }
        let q = self.waiting.entry(client).or_default();
        if q.is_empty() {
            self.order.push_back(client);
        }
        q.push_back(Waiting { ticket, lease_until: now.saturating_add(hold_ms) });
        self.owner.insert(ticket, client);
        self.pump(now);
        Some(self.status(ticket))
    }

    /// A waiting invocation asks again, renewing its lease for `hold_ms`.
    pub fn ask(&mut self, ticket: Ticket, now: u64, hold_ms: u64) -> Status {
        self.expire(now);
        if let Some(client) = self.owner.get(&ticket).copied()
            && let Some(w) = self.waiting.get_mut(&client).and_then(|q| q.iter_mut().find(|w| w.ticket == ticket))
        {
            w.lease_until = now.saturating_add(hold_ms);
        }
        self.pump(now);
        self.status(ticket)
    }

    /// A ticket's scan ended. `served`: the scan's service time when the
    /// instance scored it (it then stays awake for the idle timeout).
    pub fn done(&mut self, ticket: Ticket, now: u64, served: Option<u64>) -> bool {
        let known = self.admitted.remove(&ticket).is_some();
        if known && let Some(ms) = served {
            self.awake_until = self.awake_until.max(now.saturating_add(self.cfg.idle_ms));
            self.last_service_ms = Some(ms);
        }
        self.pump(now);
        known
    }

    /// A ticket withdrawn by its invocation.
    pub fn leave(&mut self, ticket: Ticket, now: u64) {
        self.admitted.remove(&ticket);
        self.drop_waiting(ticket);
        self.pump(now);
    }

    /// A content-free warm request.
    pub fn warm(&mut self, now: u64) -> Warm {
        self.expire(now);
        if self.awake_until > now || !self.admitted.is_empty() {
            return Warm { start: false, awake_until: self.awake_until.max(now) };
        }
        // A warm start: the instance's own inactivity timeout runs from now.
        self.awake_until = now.saturating_add(self.cfg.idle_ms);
        Warm { start: true, awake_until: self.awake_until }
    }

    /// The instance stopped or failed: it is not awake.
    pub fn asleep(&mut self) {
        self.awake_until = 0;
    }

    /// A ticket's status.
    pub fn status(&self, ticket: Ticket) -> Status {
        if self.admitted.contains_key(&ticket) {
            return Status::Admitted;
        }
        match self.position(ticket) {
            Some(position) => Status::Queued { position },
            None => Status::Gone,
        }
    }

    /// The waiting tickets admitted or dropped from the line since the last
    /// call: the ones whose waiting ask should be answered now. A position
    /// change alone waits for the hold. The caller takes them after every
    /// change, so the list holds at most the tickets one change settles.
    pub fn take_settled(&mut self) -> Vec<Ticket> {
        std::mem::take(&mut self.settled)
    }

    /// Estimated wait before `position` is admitted: the most recent scan's
    /// service time per place in line (none until a scan completes).
    pub fn estimate_ms(&self, position: usize) -> Option<u64> {
        self.last_service_ms.map(|s| s.saturating_mul(position as u64))
    }

    fn drop_waiting(&mut self, ticket: Ticket) {
        let Some(client) = self.owner.remove(&ticket) else { return };
        self.settled.push(ticket);
        if let Some(q) = self.waiting.get_mut(&client) {
            q.retain(|w| w.ticket != ticket);
            if q.is_empty() {
                self.waiting.remove(&client);
                self.order.retain(|c| *c != client);
            }
        }
    }

    /// Drops waiting tickets past their lease and frees slots whose scan
    /// lease ended.
    pub fn expire(&mut self, now: u64) {
        let before = (self.owner.len(), self.admitted.len());
        self.admitted.retain(|_, until| *until >= now);
        let stale: Vec<Ticket> =
            self.waiting.values().flat_map(|q| q.iter()).filter(|w| w.lease_until < now).map(|w| w.ticket).collect();
        for t in stale {
            self.drop_waiting(t);
        }
        if before != (self.owner.len(), self.admitted.len()) {
            self.pump(now);
        }
    }

    /// Admits waiting tickets round-robin while slots are free.
    fn pump(&mut self, now: u64) {
        while self.admitted.len() < self.cfg.slots {
            let Some(client) = self.order.pop_front() else { return };
            let Some(q) = self.waiting.get_mut(&client) else { continue };
            let Some(w) = q.pop_front() else {
                self.waiting.remove(&client);
                continue;
            };
            if q.is_empty() {
                self.waiting.remove(&client);
            } else {
                self.order.push_back(client);
            }
            self.owner.remove(&w.ticket);
            self.settled.push(w.ticket);
            self.admitted.insert(w.ticket, now.saturating_add(self.cfg.scan_lease_ms));
        }
    }

    /// 1-based admission position under round-robin from the current turn.
    fn position(&self, ticket: Ticket) -> Option<usize> {
        let client = *self.owner.get(&ticket)?;
        let at = self.order.iter().position(|c| *c == client)?;
        let i = self.waiting.get(&client)?.iter().position(|w| w.ticket == ticket)?;
        let mut before = 0usize;
        for (j, c) in self.order.iter().enumerate() {
            let len = self.waiting.get(c).map_or(0, VecDeque::len);
            before += match j.cmp(&at) {
                std::cmp::Ordering::Less => len.min(i + 1),
                std::cmp::Ordering::Equal => i,
                std::cmp::Ordering::Greater => len.min(i),
            };
        }
        Some(before + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(slots: usize) -> Gate {
        Gate::new(Config { slots, scan_lease_ms: 60_000, idle_ms: 600_000, max_tickets: 100 })
    }

    #[test]
    fn admits_up_to_the_slots_then_queues() {
        let mut g = gate(1);
        assert_eq!(g.enter(1, 10, 0, 5_000), Some(Status::Admitted));
        assert_eq!(g.enter(2, 20, 0, 5_000), Some(Status::Queued { position: 1 }));
        assert_eq!(g.enter(3, 30, 0, 5_000), Some(Status::Queued { position: 2 }));
        assert!(g.done(1, 10, Some(2_000)));
        assert_eq!(g.status(2), Status::Admitted);
        assert_eq!(g.status(3), Status::Queued { position: 1 });
        assert_eq!(g.estimate_ms(1), Some(2_000));
    }

    #[test]
    fn many_tickets_from_one_client_only_lengthen_its_own_turn() {
        let mut g = gate(1);
        g.enter(1, 10, 0, 5_000);
        for t in 100..110 {
            g.enter(t, 66, 0, 5_000);
        }
        // A newcomer from another client waits behind one ticket of the flooder.
        assert_eq!(g.enter(200, 77, 0, 5_000), Some(Status::Queued { position: 2 }));
        g.done(1, 1, Some(1_000));
        assert_eq!(g.status(100), Status::Admitted);
        assert_eq!(g.status(200), Status::Queued { position: 1 });
        g.done(100, 2, Some(1_000));
        assert_eq!(g.status(200), Status::Admitted);
        assert_eq!(g.status(101), Status::Queued { position: 1 });
    }

    #[test]
    fn positions_follow_round_robin() {
        let mut g = gate(1);
        g.enter(1, 1, 0, 5_000); // admitted
        g.enter(2, 1, 0, 5_000);
        g.enter(3, 1, 0, 5_000);
        g.enter(4, 2, 0, 5_000);
        g.enter(5, 3, 0, 5_000);
        g.enter(6, 2, 0, 5_000);
        // Order of admission: 2 (c1), 4 (c2), 5 (c3), 3 (c1), 6 (c2).
        let pos: Vec<_> = [2, 4, 5, 3, 6].iter().map(|t| g.status(*t)).collect();
        assert_eq!(
            pos,
            (1..=5).map(|position| Status::Queued { position }).collect::<Vec<_>>()
        );
        let mut seen = Vec::new();
        let mut done = 1;
        for now in 1..=5 {
            g.done(done, now, Some(10));
            let next = [2u128, 3, 4, 5, 6].into_iter().find(|t| g.status(*t) == Status::Admitted).unwrap();
            seen.push(next);
            done = next;
        }
        assert_eq!(seen, vec![2, 4, 5, 3, 6]);
    }

    #[test]
    fn abandoned_tickets_expire_and_free_their_slot() {
        let mut g = gate(1);
        g.enter(1, 1, 0, 5_000);
        g.enter(2, 2, 0, 5_000);
        g.enter(3, 3, 0, 5_000);
        // 2 keeps asking; 3 does not.
        assert_eq!(g.ask(2, 4_000, 5_000), Status::Queued { position: 1 });
        assert_eq!(g.ask(2, 8_000, 5_000), Status::Queued { position: 1 });
        assert_eq!(g.status(3), Status::Gone);
        // The admitted ticket never reports: its scan lease frees the slot.
        assert_eq!(g.ask(2, 60_001, 5_000), Status::Gone, "2 stopped asking at 8 s");
        let mut g = gate(1);
        g.enter(1, 1, 0, 5_000);
        g.enter(2, 2, 0, 100_000);
        assert_eq!(g.ask(2, 59_000, 100_000), Status::Queued { position: 1 });
        assert_eq!(g.ask(2, 60_001, 100_000), Status::Admitted);
    }

    #[test]
    fn a_queue_change_settles_only_the_ticket_it_admits() {
        let mut g = gate(1);
        g.enter(1, 1, 0, 60_000);
        assert_eq!(g.take_settled(), vec![1], "admitted on entry");
        for t in 2..50u128 {
            g.enter(t, t as u64, 0, 60_000);
        }
        assert!(g.take_settled().is_empty(), "entries behind the line settle nobody");
        g.done(1, 1, Some(1_000));
        assert_eq!(g.take_settled(), vec![2]);
        assert_eq!(g.status(2), Status::Admitted);
        g.leave(30, 2);
        assert_eq!(g.take_settled(), vec![30], "a ticket that left is gone");
        // An ask that changes nothing settles nobody.
        g.ask(3, 50_000, 60_000);
        assert!(g.take_settled().is_empty());
        // At 100 s the running scan's lease and every ticket that stopped
        // asking have expired: those are dropped and 3 is admitted.
        g.ask(3, 100_000, 60_000);
        let mut settled = g.take_settled();
        settled.sort_unstable();
        assert_eq!(settled, (3..50u128).filter(|t| *t != 30).collect::<Vec<_>>());
        assert_eq!(g.status(3), Status::Admitted);
    }

    #[test]
    fn the_table_is_bounded() {
        let mut g = Gate::new(Config { slots: 1, scan_lease_ms: 1, idle_ms: 1, max_tickets: 2 });
        assert!(g.enter(1, 1, 0, 100).is_some());
        assert!(g.enter(2, 1, 0, 100).is_some());
        assert_eq!(g.enter(3, 2, 0, 100), None);
    }

    #[test]
    fn warms_start_once_and_never_extend_the_idle_timer() {
        let mut g = gate(1);
        assert_eq!(g.warm(0), Warm { start: true, awake_until: 600_000 });
        assert_eq!(g.warm(1_000), Warm { start: false, awake_until: 600_000 });
        assert_eq!(g.warm(599_000), Warm { start: false, awake_until: 600_000 });
        // A scan extends it.
        g.enter(1, 1, 700_000, 5_000);
        assert_eq!(g.warm(700_001), Warm { start: false, awake_until: 700_001 });
        g.done(1, 701_000, Some(1_000));
        assert_eq!(g.awake_until(), 1_301_000);
        g.asleep();
        assert!(g.warm(702_000).start);
    }

    #[test]
    fn leave_and_unknown_done() {
        let mut g = gate(1);
        g.enter(1, 1, 0, 5_000);
        g.enter(2, 2, 0, 5_000);
        g.leave(2, 1);
        assert_eq!(g.status(2), Status::Gone);
        assert!(!g.done(9, 1, Some(1)));
        g.leave(1, 2);
        assert_eq!(g.running(), 0);
    }
}

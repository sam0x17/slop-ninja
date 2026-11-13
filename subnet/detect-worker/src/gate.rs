//! The admission gate's decisions (the `DetectGate` Durable Object's memory),
//! natively tested.
//!
//! - **Tickets only.** The gate holds `{ticket id, client key, class}` and
//!   leases, never a text: the text stays in the requesting invocation while
//!   its ticket waits.
//! - **Classes** ([`TicketClass`]): `interactive` (a free paste, a short
//!   text, a secret re-run), `url` (a public URL's first check or re-run),
//!   `paid` (a paid section or an API document) and `background` (operator
//!   re-runs). Admission alternates `paid` and the free group (`interactive`
//!   with `url`) while both wait; within a group it goes round-robin across
//!   client keys, first in first out per key; `background` is admitted only
//!   when nothing else waits.
//! - **Fair queue.** Client keys are keyed hashes the gate's host derives
//!   (an HMAC of the address prefix for anonymous checks, of the owner hash
//!   or the API key hash for paid ones), so many tickets from one source
//!   lengthen only that source's turn. Nobody is refused by address: schools
//!   share one.
//! - **One at a time per key.** A client key with an admitted ticket gets
//!   no second admission until the first is done or its lease ends.
//! - **Slots.** At most `slots` scans run at once: the Container instances
//!   times their sessions, a deployment decision, never an autoscale.
//! - **Leases.** A waiting ticket stays while its invocation keeps asking
//!   (each ask renews it for the hold the invocation names); an admitted one
//!   until it reports done or its scan lease ends (`waits::Waits::
//!   scan_lease_ms`, the lane's own bound plus one hold). A ticket whose
//!   invocation went away is dropped at its lease's end.
//! - **Position-scaled hold** ([`Gate::hold_ms`]): a ticket at position p is
//!   held `max(QUEUE_HOLD_MS, (p - 1) × the fastest measured scan / slots)`,
//!   the earliest it could be admitted, so the gate's ask rate grows with the
//!   logarithm of the line rather than its length.
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

use slop_ninja_detect_page::TicketClass;
use std::collections::{BTreeMap, VecDeque};

/// A ticket id (random, chosen by the gate's host).
pub type Ticket = u128;
/// A client key (the gate host's keyed hash of the client's identity).
pub type Client = u64;

/// Admitted tickets one client key may hold at once.
pub const IN_FLIGHT_PER_CLIENT: usize = 1;

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
    /// 1-based position in the admission order (an estimate: arrivals may
    /// overtake it within its group's round-robin).
    Queued {
        position: usize,
    },
    /// Unknown or expired: the invocation must start over.
    Gone,
}

/// The admission groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Paid,
    Free,
    Background,
}

impl Group {
    fn of(class: TicketClass) -> Group {
        match class {
            TicketClass::Paid => Group::Paid,
            TicketClass::Interactive | TicketClass::Url => Group::Free,
            TicketClass::Background => Group::Background,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Waiting {
    ticket: Ticket,
    lease_until: u64,
}

/// One group's line: client keys in turn order, each with its tickets.
#[derive(Debug, Default)]
struct Line {
    order: VecDeque<Client>,
    waiting: BTreeMap<Client, VecDeque<Waiting>>,
    len: usize,
}

impl Line {
    fn push(&mut self, client: Client, w: Waiting) {
        let q = self.waiting.entry(client).or_default();
        if q.is_empty() {
            self.order.push_back(client);
        }
        q.push_back(w);
        self.len += 1;
    }

    fn remove(&mut self, client: Client, ticket: Ticket) {
        let Some(q) = self.waiting.get_mut(&client) else { return };
        let before = q.len();
        q.retain(|w| w.ticket != ticket);
        self.len -= before - q.len();
        if q.is_empty() {
            self.waiting.remove(&client);
            self.order.retain(|c| *c != client);
        }
    }

    fn renew(&mut self, client: Client, ticket: Ticket, until: u64) {
        if let Some(w) = self.waiting.get_mut(&client).and_then(|q| q.iter_mut().find(|w| w.ticket == ticket)) {
            w.lease_until = until;
        }
    }

    fn has_eligible(&self, busy: &BTreeMap<Client, usize>) -> bool {
        self.order.iter().any(|c| !busy.contains_key(c))
    }

    /// The first ticket of the first client in turn order that holds no
    /// admission; that client goes to the back of the order if it has more.
    /// Clients skipped for their admission keep their turn.
    fn take(&mut self, busy: &BTreeMap<Client, usize>) -> Option<(Client, Waiting)> {
        let at = self.order.iter().position(|c| !busy.contains_key(c))?;
        let client = self.order.remove(at)?;
        let q = self.waiting.get_mut(&client)?;
        let w = q.pop_front()?;
        self.len -= 1;
        if q.is_empty() {
            self.waiting.remove(&client);
        } else {
            self.order.push_back(client);
        }
        Some((client, w))
    }

    fn stale(&self, now: u64) -> Vec<(Client, Ticket)> {
        self.waiting
            .iter()
            .flat_map(|(c, q)| q.iter().filter(|w| w.lease_until < now).map(move |w| (*c, w.ticket)))
            .collect()
    }

    /// 1-based admission position within this line under round-robin from
    /// the current turn.
    fn position(&self, client: Client, ticket: Ticket) -> Option<usize> {
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

/// What a warm request learns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Warm {
    /// Start the instance now.
    pub start: bool,
    /// The earliest moment the instance could sleep (ms clock).
    pub awake_until: u64,
}

#[derive(Clone, Copy, Debug)]
struct Running {
    until: u64,
    client: Client,
}

#[derive(Debug)]
pub struct Gate {
    cfg: Config,
    paid: Line,
    free: Line,
    background: Line,
    /// Waiting ticket -> its client and group.
    owner: BTreeMap<Ticket, (Client, Group)>,
    /// Admitted ticket -> its lease end and client.
    admitted: BTreeMap<Ticket, Running>,
    /// Client -> its admitted tickets (absent when none).
    in_flight: BTreeMap<Client, usize>,
    /// The group of the last paid or free admission, for the alternation.
    last: Option<Group>,
    awake_until: u64,
    last_service_ms: Option<u64>,
    /// The fastest completed scan's service time.
    min_service_ms: Option<u64>,
    /// Waiting tickets admitted or dropped since the last `take_settled`.
    settled: Vec<Ticket>,
}

impl Gate {
    fn line(&self, g: Group) -> &Line {
        match g {
            Group::Paid => &self.paid,
            Group::Free => &self.free,
            Group::Background => &self.background,
        }
    }

    fn line_mut(&mut self, g: Group) -> &mut Line {
        match g {
            Group::Paid => &mut self.paid,
            Group::Free => &mut self.free,
            Group::Background => &mut self.background,
        }
    }

    pub fn new(cfg: Config) -> Gate {
        Gate {
            cfg,
            paid: Line::default(),
            free: Line::default(),
            background: Line::default(),
            owner: BTreeMap::new(),
            admitted: BTreeMap::new(),
            in_flight: BTreeMap::new(),
            last: None,
            awake_until: 0,
            last_service_ms: None,
            min_service_ms: None,
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

    /// The fastest completed scan's service time.
    pub fn min_service_ms(&self) -> Option<u64> {
        self.min_service_ms
    }

    /// A new ticket of `class`. `None`: the table is full (busy).
    pub fn enter(
        &mut self,
        ticket: Ticket,
        client: Client,
        class: TicketClass,
        now: u64,
        hold_ms: u64,
    ) -> Option<Status> {
        self.expire(now);
        if self.owner.contains_key(&ticket) || self.admitted.contains_key(&ticket) {
            return Some(self.status(ticket));
        }
        if self.tickets() >= self.cfg.max_tickets {
            return None;
        }
        let g = Group::of(class);
        self.line_mut(g).push(client, Waiting { ticket, lease_until: now.saturating_add(hold_ms) });
        self.owner.insert(ticket, (client, g));
        self.pump(now);
        Some(self.status(ticket))
    }

    /// A waiting invocation asks again, renewing its lease for `hold_ms`.
    pub fn ask(&mut self, ticket: Ticket, now: u64, hold_ms: u64) -> Status {
        self.expire(now);
        if let Some((client, g)) = self.owner.get(&ticket).copied() {
            self.line_mut(g).renew(client, ticket, now.saturating_add(hold_ms));
        }
        self.pump(now);
        self.status(ticket)
    }

    /// A ticket's scan ended. `served`: the scan's service time when the
    /// instance scored it (it then stays awake for the idle timeout).
    pub fn done(&mut self, ticket: Ticket, now: u64, served: Option<u64>) -> bool {
        let known = self.release(ticket);
        if known && let Some(ms) = served {
            self.awake_until = self.awake_until.max(now.saturating_add(self.cfg.idle_ms));
            self.last_service_ms = Some(ms);
            self.min_service_ms = Some(self.min_service_ms.map_or(ms, |m| m.min(ms)));
        }
        self.pump(now);
        known
    }

    /// A ticket withdrawn by its invocation.
    pub fn leave(&mut self, ticket: Ticket, now: u64) {
        self.release(ticket);
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

    /// The hold for the next ask of a ticket at `position`: at least `base`,
    /// and otherwise the earliest it could be admitted, `(position - 1)`
    /// fastest scans per slot. `base` until a scan has completed.
    pub fn hold_ms(&self, position: usize, base: u64) -> u64 {
        let Some(m) = self.min_service_ms else { return base };
        let ahead = u64::try_from(position.saturating_sub(1)).unwrap_or(u64::MAX);
        let slots = u64::try_from(self.cfg.slots.max(1)).unwrap_or(1);
        base.max(ahead.saturating_mul(m) / slots)
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

    /// Frees an admitted ticket's slot and its client's turn.
    fn release(&mut self, ticket: Ticket) -> bool {
        let Some(r) = self.admitted.remove(&ticket) else { return false };
        if let Some(n) = self.in_flight.get_mut(&r.client) {
            *n -= 1;
            if *n == 0 {
                self.in_flight.remove(&r.client);
            }
        }
        true
    }

    fn drop_waiting(&mut self, ticket: Ticket) {
        let Some((client, g)) = self.owner.remove(&ticket) else { return };
        self.settled.push(ticket);
        self.line_mut(g).remove(client, ticket);
    }

    /// Drops waiting tickets past their lease and frees slots whose scan
    /// lease ended.
    pub fn expire(&mut self, now: u64) {
        let before = (self.owner.len(), self.admitted.len());
        let ended: Vec<Ticket> = self.admitted.iter().filter(|(_, r)| r.until < now).map(|(t, _)| *t).collect();
        for t in ended {
            self.release(t);
        }
        let stale: Vec<Ticket> =
            [&self.paid, &self.free, &self.background].iter().flat_map(|l| l.stale(now)).map(|(_, t)| t).collect();
        for t in stale {
            self.drop_waiting(t);
        }
        if before != (self.owner.len(), self.admitted.len()) {
            self.pump(now);
        }
    }

    /// The group whose turn it is while both paid and free tickets wait.
    fn alternate(&self) -> Group {
        if self.last == Some(Group::Paid) { Group::Free } else { Group::Paid }
    }

    /// Admits waiting tickets while slots are free: paid and free in turn,
    /// background only when neither can be admitted.
    fn pump(&mut self, now: u64) {
        while self.admitted.len() < self.cfg.slots {
            let busy = &self.in_flight;
            let paid = self.paid.has_eligible(busy);
            let free = self.free.has_eligible(busy);
            let g = match (paid, free) {
                (true, true) => self.alternate(),
                (true, false) => Group::Paid,
                (false, true) => Group::Free,
                (false, false) if self.background.has_eligible(busy) => Group::Background,
                (false, false) => return,
            };
            let Gate { paid, free, background, in_flight, .. } = self;
            let line = match g {
                Group::Paid => paid,
                Group::Free => free,
                Group::Background => background,
            };
            let Some((client, w)) = line.take(in_flight) else { return };
            self.owner.remove(&w.ticket);
            self.settled.push(w.ticket);
            self.admitted.insert(w.ticket, Running { until: now.saturating_add(self.cfg.scan_lease_ms), client });
            *self.in_flight.entry(client).or_default() += 1;
            debug_assert!(self.in_flight.get(&client).copied().unwrap_or(0) <= IN_FLIGHT_PER_CLIENT);
            if g != Group::Background {
                self.last = Some(g);
            }
        }
    }

    /// 1-based admission position: the position within the ticket's line,
    /// interleaved with the other group's waiting tickets while both wait;
    /// a background ticket waits behind every other.
    fn position(&self, ticket: Ticket) -> Option<usize> {
        let (client, g) = *self.owner.get(&ticket)?;
        let q = self.line(g).position(client, ticket)?;
        let (paid, free) = (self.paid.len, self.free.len);
        Some(match g {
            Group::Background => paid + free + q,
            Group::Paid | Group::Free => {
                let other = if g == Group::Paid { free } else { paid };
                let first = usize::from(self.alternate() == g);
                q + other.min(q - first)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const I: TicketClass = TicketClass::Interactive;
    const P: TicketClass = TicketClass::Paid;

    fn gate(slots: usize) -> Gate {
        Gate::new(Config { slots, scan_lease_ms: 60_000, idle_ms: 600_000, max_tickets: 100 })
    }

    #[test]
    fn admits_up_to_the_slots_then_queues() {
        let mut g = gate(1);
        assert_eq!(g.enter(1, 10, I, 0, 5_000), Some(Status::Admitted));
        assert_eq!(g.enter(2, 20, I, 0, 5_000), Some(Status::Queued { position: 1 }));
        assert_eq!(g.enter(3, 30, I, 0, 5_000), Some(Status::Queued { position: 2 }));
        assert!(g.done(1, 10, Some(2_000)));
        assert_eq!(g.status(2), Status::Admitted);
        assert_eq!(g.status(3), Status::Queued { position: 1 });
        assert_eq!(g.estimate_ms(1), Some(2_000));
    }

    #[test]
    fn many_tickets_from_one_client_only_lengthen_its_own_turn() {
        let mut g = gate(1);
        g.enter(1, 10, I, 0, 5_000);
        for t in 100..110 {
            g.enter(t, 66, I, 0, 5_000);
        }
        // A newcomer from another client waits behind one ticket of the flooder.
        assert_eq!(g.enter(200, 77, I, 0, 5_000), Some(Status::Queued { position: 2 }));
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
        g.enter(1, 1, I, 0, 5_000); // admitted
        g.enter(2, 1, I, 0, 5_000);
        g.enter(3, 1, I, 0, 5_000);
        g.enter(4, 2, I, 0, 5_000);
        g.enter(5, 3, I, 0, 5_000);
        g.enter(6, 2, I, 0, 5_000);
        // Order of admission: 2 (c1), 4 (c2), 5 (c3), 3 (c1), 6 (c2).
        let pos: Vec<_> = [2, 4, 5, 3, 6].iter().map(|t| g.status(*t)).collect();
        assert_eq!(pos, (1..=5).map(|position| Status::Queued { position }).collect::<Vec<_>>());
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
        g.enter(1, 1, I, 0, 5_000);
        g.enter(2, 2, I, 0, 5_000);
        g.enter(3, 3, I, 0, 5_000);
        // 2 keeps asking; 3 does not.
        assert_eq!(g.ask(2, 4_000, 5_000), Status::Queued { position: 1 });
        assert_eq!(g.ask(2, 8_000, 5_000), Status::Queued { position: 1 });
        assert_eq!(g.status(3), Status::Gone);
        // The admitted ticket never reports: its scan lease frees the slot.
        assert_eq!(g.ask(2, 60_001, 5_000), Status::Gone, "2 stopped asking at 8 s");
        let mut g = gate(1);
        g.enter(1, 1, I, 0, 5_000);
        g.enter(2, 2, I, 0, 100_000);
        assert_eq!(g.ask(2, 59_000, 100_000), Status::Queued { position: 1 });
        assert_eq!(g.ask(2, 60_001, 100_000), Status::Admitted);
    }

    #[test]
    fn a_queue_change_settles_only_the_ticket_it_admits() {
        let mut g = gate(1);
        g.enter(1, 1, I, 0, 60_000);
        assert_eq!(g.take_settled(), vec![1], "admitted on entry");
        for t in 2..50u128 {
            g.enter(t, t as u64, I, 0, 60_000);
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
        assert!(g.enter(1, 1, I, 0, 100).is_some());
        assert!(g.enter(2, 1, I, 0, 100).is_some());
        assert_eq!(g.enter(3, 2, I, 0, 100), None);
    }

    #[test]
    fn warms_start_once_and_never_extend_the_idle_timer() {
        let mut g = gate(1);
        assert_eq!(g.warm(0), Warm { start: true, awake_until: 600_000 });
        assert_eq!(g.warm(1_000), Warm { start: false, awake_until: 600_000 });
        assert_eq!(g.warm(599_000), Warm { start: false, awake_until: 600_000 });
        // A scan extends it.
        g.enter(1, 1, I, 700_000, 5_000);
        assert_eq!(g.warm(700_001), Warm { start: false, awake_until: 700_001 });
        g.done(1, 701_000, Some(1_000));
        assert_eq!(g.awake_until(), 1_301_000);
        g.asleep();
        assert!(g.warm(702_000).start);
    }

    #[test]
    fn leave_and_unknown_done() {
        let mut g = gate(1);
        g.enter(1, 1, I, 0, 5_000);
        g.enter(2, 2, I, 0, 5_000);
        g.leave(2, 1);
        assert_eq!(g.status(2), Status::Gone);
        assert!(!g.done(9, 1, Some(1)));
        g.leave(1, 2);
        assert_eq!(g.running(), 0);
    }

    #[test]
    fn paid_and_free_alternate_and_background_waits_for_both() {
        let mut g = gate(1);
        g.enter(1, 1, I, 0, 60_000); // admitted
        g.enter(2, 2, TicketClass::Background, 0, 60_000);
        g.enter(3, 3, I, 0, 60_000);
        g.enter(4, 4, TicketClass::Url, 0, 60_000);
        g.enter(5, 5, P, 0, 60_000);
        g.enter(6, 6, P, 0, 60_000);
        g.enter(7, 7, I, 0, 60_000);
        // The last admission was a free one: the other group goes next, then they take turns.
        let want = [5u128, 3, 6, 4, 7, 2];
        for (i, t) in want.iter().enumerate() {
            assert_eq!(g.status(*t), Status::Queued { position: i + 1 }, "ticket {t}");
        }
        let mut seen = Vec::new();
        let mut done = 1;
        for now in 1..=6 {
            g.done(done, now, Some(10));
            done = want.iter().copied().find(|t| g.status(*t) == Status::Admitted).unwrap();
            seen.push(done);
        }
        assert_eq!(seen, want);
    }

    #[test]
    fn one_admission_per_client_key() {
        let mut g = gate(2);
        assert_eq!(g.enter(1, 9, I, 0, 60_000), Some(Status::Admitted));
        // A second slot is free, but the client already holds one.
        assert_eq!(g.enter(2, 9, I, 0, 60_000), Some(Status::Queued { position: 1 }));
        assert_eq!(g.running(), 1);
        // Another client takes the free slot at once, ahead of client 9.
        assert_eq!(g.enter(3, 8, I, 0, 60_000), Some(Status::Admitted));
        g.done(1, 1, Some(5));
        assert_eq!(g.status(2), Status::Admitted);
        // A lease that ends frees the client's turn too.
        let mut g = gate(2);
        g.enter(1, 9, I, 0, 60_000);
        g.enter(2, 9, I, 0, 200_000);
        assert_eq!(g.ask(2, 60_001, 200_000), Status::Admitted);
    }

    #[test]
    fn the_hold_scales_with_the_place_in_line() {
        let mut g = gate(1);
        assert_eq!(g.hold_ms(50, 20_000), 20_000, "no scan measured yet");
        g.enter(1, 1, I, 0, 5_000);
        g.done(1, 1, Some(4_000));
        g.enter(2, 1, I, 2, 5_000);
        g.done(2, 3, Some(2_000));
        assert_eq!(g.min_service_ms(), Some(2_000));
        assert_eq!(g.last_service_ms(), Some(2_000));
        g.enter(3, 1, I, 4, 5_000);
        g.done(3, 5, Some(3_000));
        assert_eq!(g.min_service_ms(), Some(2_000), "the fastest scan");
        assert_eq!(g.hold_ms(1, 20_000), 20_000);
        assert_eq!(g.hold_ms(11, 20_000), 20_000);
        assert_eq!(g.hold_ms(12, 20_000), 22_000);
        assert_eq!(g.hold_ms(301, 20_000), 600_000);
        // Two slots halve it.
        let mut g2 = gate(2);
        g2.enter(1, 1, I, 0, 5_000);
        g2.done(1, 1, Some(2_000));
        assert_eq!(g2.hold_ms(301, 20_000), 300_000);
        // The ask rate: tickets at positions 1..n ask about once per
        // (p - 1) scans each, so a hold's asks grow with log n, not n.
        let asks = |n: usize| -> f64 { (1..=n).map(|p| 600_000.0 / g.hold_ms(p, 20_000) as f64).sum() };
        let (small, full) = (asks(1_000), asks(32_768));
        assert!(full < 2.0 * small, "{small} {full}");
        assert!(full * 100.0 < 32_768.0 * 30.0, "a flat hold would ask 30 times per ticket: {full}");
    }

    #[test]
    fn a_held_ask_keeps_its_ticket_while_it_waits() {
        let mut g = gate(1);
        g.enter(1, 1, I, 0, 1_000_000);
        g.enter(2, 2, I, 0, 40_000);
        // The ask carries the scaled hold; its lease covers two of them.
        assert_eq!(g.ask(2, 30_000, 2 * 500_000), Status::Queued { position: 1 });
        // Long after the first scan's lease ended, the held ticket kept its
        // place and is admitted.
        assert_eq!(g.ask(2, 900_000, 2 * 500_000), Status::Admitted);
    }
}

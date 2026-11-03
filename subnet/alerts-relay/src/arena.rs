//! Bounded memory for the relay's Wasm build (`slop_ninja.alerts_relay.arena/v2`:
//! every input bound derived from its fields; v1 used round allowances),
//! the publisher's rule (`slop_ninja.publisher.arena/v2`) applied to the
//! relay's inputs:
//!
//! 1. **Fixed linear memory.** Initial memory equals maximum memory
//!    ([`LINEAR_MEMORY_BYTES`], set by `.cargo/config.toml`; a test checks
//!    they agree), so the module never grows at run time.
//! 2. **Every input has a bound fixed before it is read.** A post's bound is
//!    the wire contract's ([`slop_ninja_alerts_wire::post_bound`]), a
//!    heartbeat's is [`slop_ninja_alerts_wire::HEARTBEAT_BYTES`], the tracker
//!    upload's comes from its schema ([`crate::tracker::upload_bound`]) and a
//!    coordinator status body's from the route scope the coordinator answers
//!    it in ([`STATUS_BODY_BOUND`]). A declared length over the bound is
//!    refused before any byte is read; a body without one is refused as soon
//!    as it passes the bound ([`Reader`]).
//! 3. **Sum what is held.** One isolate-wide [`Budget`] accounts every body
//!    while it is held and every message while it is built; a reservation
//!    that would exceed [`READBACK_BUDGET`] is refused before allocating, and
//!    the request answers 503 (a post is retried by the publisher's next
//!    run, a tick by the next cron).
//!
//! The budget is the working set the relay serves at once: one tick (the
//! Durable Object runs one at a time) holding one status body and its growth
//! copy and then building one message, and two posts (the publisher posts its
//! batches one at a time; a retry can overlap a slow earlier post), each with
//! its body, growth copy and message.

use std::cell::Cell;

use slop_ninja_alerts_wire::{HEARTBEAT_BYTES, MIN_EVENT_BYTES, post_bound};

pub const ARENA_RULE: &str = "slop_ninja.alerts_relay.arena/v2";

/// A coordinator Worker's `/status` body: the coordinator answers it inside
/// one `WORKER_CONTROL_BYTES` scope (1 MiB, `subnet/coordinator/src/
/// arena_routes.rs`; a test checks they agree), which holds the response
/// it serializes.
pub const STATUS_BODY_BOUND: u64 = 1 << 20;

/// Memory outside the budget: the 1 MiB stack rustc links first, static
/// data, the runtime glue's own allocations, the small documents a request
/// builds, the decoded copies of what a budgeted body holds, and allocator
/// fragmentation. A versioned parameter of this rule, not a measurement: it
/// is the publisher's reserve for the same runtime (`slop_ninja.publisher.
/// arena/v2`), which no Workers measurement has confirmed yet. The native
/// measurement in `tests/arena.rs` covers only the relay's own part of it
/// ([`UNACCOUNTED_ALLOWANCE`]); the deploy drill logs the budget's peak in
/// the live Worker (`cf.rs`, each tick's log line). Widen it, with a new
/// rule version, if a live peak or a trap shows less headroom.
pub const RESERVED_BYTES: u64 = 16 << 20;

/// What the relay may hold outside its reservations: a quarter of
/// [`RESERVED_BYTES`] (a versioned parameter of this rule), leaving the rest
/// for the stack, the runtime glue and fragmentation. The native measurement
/// in `tests/arena.rs` checks the relay's real heap against the accounted
/// peak plus this allowance for its largest inputs.
pub const UNACCOUNTED_ALLOWANCE: u64 = RESERVED_BYTES / 4;

/// Quoted-printable output for a text of `text` bytes: at most three
/// characters per byte, and a three-byte soft break (`=` CRLF) for every
/// encoded line, which holds at least 73 characters before it breaks.
pub const fn qp_bound(text: u64) -> u64 {
    3 * text + 3 * (3 * text / 73 + 1)
}

/// A message's size bound for a text of `text` bytes: the encoded text and
/// the header block.
pub const fn message_bound(text: u64) -> u64 {
    qp_bound(text) + crate::mime::HEADER_BYTES as u64
}

/// What building a message holds: its text and the message (encoded
/// straight into its buffer, sized once).
pub const fn message_footprint(text: u64) -> u64 {
    text + message_bound(text)
}

/// The text of one message: at most one post's fields (its events', and the
/// rule set and network its envelope names), the fixed text around each
/// event ([`crate::mime::EVENT_FIXED_BYTES`]) and the message's own
/// ([`crate::mime::MESSAGE_FIXED_BYTES`]), both derived from the text's
/// pieces. A post holds at most one event per [`MIN_EVENT_BYTES`] of its
/// bound.
pub const fn text_bound() -> u64 {
    let post = post_bound() as u64;
    let events = post / MIN_EVENT_BYTES as u64;
    post + events * crate::mime::EVENT_FIXED_BYTES as u64 + crate::mime::MESSAGE_FIXED_BYTES as u64
}

/// A body held while it is read: the bytes and a growing buffer's old copy.
pub const fn read_footprint(bound: u64) -> u64 {
    2 * bound
}

/// One post: its body and the message built from it.
pub const fn post_footprint() -> u64 {
    read_footprint(post_bound() as u64) + message_footprint(text_bound())
}

/// One tick: a status body (the largest of its reads, which it holds one at
/// a time) and then one message of its own events.
pub const fn tick_footprint() -> u64 {
    read_footprint(STATUS_BODY_BOUND) + message_footprint(text_bound())
}

/// What the isolate may hold at once (see the module note).
pub const READBACK_BUDGET: u64 =
    tick_footprint() + 2 * post_footprint() + 2 * read_footprint(HEARTBEAT_BYTES as u64);

/// Wasm page size.
pub const PAGE: u64 = 64 << 10;

/// Fixed Wasm linear memory, initial and maximum alike: the reserve and the
/// budget, in whole pages. Must equal the `--initial-memory` and
/// `--max-memory` link arguments in `.cargo/config.toml`.
pub const LINEAR_MEMORY_BYTES: u64 = (RESERVED_BYTES + READBACK_BUDGET).div_ceil(PAGE) * PAGE;

// The working set fits the budget, and the linked memory holds the reserve
// and the budget in whole pages.
const _: () = assert!(tick_footprint() + 2 * post_footprint() <= READBACK_BUDGET);
const _: () = assert!(LINEAR_MEMORY_BYTES.is_multiple_of(PAGE));
const _: () = assert!(LINEAR_MEMORY_BYTES >= RESERVED_BYTES + READBACK_BUDGET);

/// A refused reservation: nothing was allocated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused {
    pub want: u64,
    pub held: u64,
    pub limit: u64,
}

/// Accounting of held memory.
#[derive(Debug)]
pub struct Budget {
    limit: u64,
    held: Cell<u64>,
    peak: Cell<u64>,
}

impl Budget {
    pub const fn new(limit: u64) -> Self {
        Self {
            limit,
            held: Cell::new(0),
            peak: Cell::new(0),
        }
    }

    pub fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.held.get())
    }

    pub fn held(&self) -> u64 {
        self.held.get()
    }

    pub fn peak(&self) -> u64 {
        self.peak.get()
    }

    /// Reserves `n` bytes until the returned hold drops.
    pub fn reserve(&self, n: u64) -> Result<Hold<'_>, Refused> {
        let held = self.held.get();
        let next = held
            .checked_add(n)
            .filter(|&t| t <= self.limit)
            .ok_or(Refused {
                want: n,
                held,
                limit: self.limit,
            })?;
        self.held.set(next);
        self.peak.set(self.peak.get().max(next));
        Ok(Hold { budget: self, n })
    }
}

/// A reservation; released on drop.
#[derive(Debug)]
pub struct Hold<'a> {
    budget: &'a Budget,
    n: u64,
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        self.budget.held.set(self.budget.held.get() - self.n);
    }
}

/// Why a body was not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// Declared or streamed past its bound; reading stopped there.
    TooLarge,
    /// The budget could not hold it; nothing was allocated.
    Budget,
    /// The transport failed.
    Transport,
}

/// Reads one body within `bound`: refuses a declared length over the bound
/// before any byte is read, then accepts chunks until they pass it. The
/// reservation (the body and its growth copy) is taken by [`Reader::new`]
/// before the buffer is allocated.
pub struct Reader<'a> {
    bound: u64,
    out: Vec<u8>,
    _hold: Hold<'a>,
}

impl<'a> Reader<'a> {
    pub fn new(budget: &'a Budget, bound: u64, declared: Option<u64>) -> Result<Self, ReadError> {
        if declared.is_some_and(|n| n > bound) {
            return Err(ReadError::TooLarge);
        }
        let hold = budget
            .reserve(read_footprint(bound))
            .map_err(|_| ReadError::Budget)?;
        Ok(Self {
            bound,
            out: Vec::with_capacity(declared.unwrap_or(0) as usize),
            _hold: hold,
        })
    }

    /// Appends one chunk, or refuses it when the body would pass its bound.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), ReadError> {
        if (self.out.len() as u64).saturating_add(chunk.len() as u64) > self.bound {
            return Err(ReadError::TooLarge);
        }
        self.out.extend_from_slice(chunk);
        Ok(())
    }

    /// The body, with the hold that accounts it.
    pub fn finish(self) -> (Vec<u8>, Hold<'a>) {
        (self.out, self._hold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_sum_and_release() {
        let b = Budget::new(100);
        let a = b.reserve(60).unwrap();
        assert_eq!(
            b.reserve(41).unwrap_err(),
            Refused {
                want: 41,
                held: 60,
                limit: 100
            }
        );
        let c = b.reserve(40).unwrap();
        assert_eq!(b.remaining(), 0);
        drop(a);
        drop(c);
        assert_eq!(b.held(), 0);
        assert_eq!(b.peak(), 100);
        assert!(b.reserve(u64::MAX).is_err());
    }

    /// Oversized bodies are refused from their declared length before
    /// anything is reserved, or as soon as streamed chunks pass the bound;
    /// a body the budget cannot hold allocates nothing.
    #[test]
    fn oversized_bodies_are_refused_before_allocating() {
        let b = Budget::new(READBACK_BUDGET);
        assert_eq!(
            Reader::new(&b, 10, Some(11)).err(),
            Some(ReadError::TooLarge)
        );
        assert_eq!(b.held(), 0, "refused before any reservation");
        let mut r = Reader::new(&b, 10, None).unwrap();
        assert_eq!(b.held(), 20);
        r.push(b"12345").unwrap();
        r.push(b"67890").unwrap();
        assert_eq!(r.push(b"x"), Err(ReadError::TooLarge));
        let (body, hold) = r.finish();
        assert_eq!(body, b"1234567890");
        drop(hold);
        assert_eq!(b.held(), 0);
        let small = Budget::new(15);
        assert_eq!(
            Reader::new(&small, 10, Some(5)).err(),
            Some(ReadError::Budget)
        );
    }

    /// The link arguments are the accounted memory (the budget's own sums
    /// are compile-time assertions above).
    #[test]
    fn linker_memory_matches() {
        let config = include_str!("../.cargo/config.toml");
        let want = LINEAR_MEMORY_BYTES.to_string();
        assert!(
            config.contains(&format!("--initial-memory={want}")),
            "initial memory {want}"
        );
        assert!(
            config.contains(&format!("--max-memory={want}")),
            "max memory {want}"
        );
    }

    /// The status bound is the coordinator's route scope.
    #[test]
    fn status_bound_is_the_coordinator_scope() {
        let routes = include_str!("../../coordinator/src/arena_routes.rs");
        assert!(routes.contains("pub const WORKER_CONTROL_BYTES: usize = MIB;"));
        assert!(routes.contains("const KIB: usize = 1024;"));
        assert!(routes.contains("const MIB: usize = 1024 * KIB;"));
        assert_eq!(STATUS_BODY_BOUND, 1 << 20);
    }
}

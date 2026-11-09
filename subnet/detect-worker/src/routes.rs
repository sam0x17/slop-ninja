//! The detect Worker's arena routes, footprints and the isolate's
//! scan memory. Compiled natively and for wasm32, so the numbers the tests check
//! against the memory model are the ones the Worker uses.
//!
//! **The scan memory.** Score requests run concurrently in one isolate, and
//! a request waiting in the queue keeps its text. So each isolate keeps one
//! fixed share of linear memory for scans, derived from the memory model,
//! and each score request reserves its worst-case footprint from it, sized
//! from its declared body length, before reading a byte of the body. A
//! request that does not fit gets 503 "busy, try again" unread; one that
//! could never fit, even alone, is refused as too large. The scan memory is a
//! byte pool rather than a count of worst-case arenas, so short texts do not
//! each hold a 9 MB arena.

use crate::bound;
use slop_ninja_arena::{AsyncRoute, limits, model};

pub mod id {
    pub const SCORE: u8 = 1;
    pub const WARM: u8 = 2;
    pub const FEEDBACK: u8 = 3;
    pub const OTHER: u8 = 4;
}

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;

/// The longest documented Workers wall limit stands in as the abandonment
/// deadline, as in the other Workers. A score request's wall time is its
/// queue wait plus a cold start plus one scan.
pub const FETCH_MS: u64 = limits::ALARM_MS;

pub const R_SCORE: AsyncRoute = AsyncRoute::new(id::SCORE, "detect/score", FETCH_MS);
pub const R_WARM: AsyncRoute = AsyncRoute::new(id::WARM, "detect/warm", FETCH_MS);
pub const R_FEEDBACK: AsyncRoute = AsyncRoute::new(id::FEEDBACK, "detect/feedback", FETCH_MS);
pub const R_OTHER: AsyncRoute = AsyncRoute::new(id::OTHER, "detect/other", FETCH_MS);

/// The scorer's reply for one document, at most: one sentence record per
/// content token at the most (a sentence holds at least one token) and one
/// span record per 24 words, each under 256 bytes of JSON, plus the
/// document prediction (origin, attribution) within 512 KiB; rounded up to
/// whole pages (1 MiB).
pub const REPLY_BYTES: usize = MIB;
const _: () = assert!(REPLY_BYTES >= bound::CONTENT_TOKENS as usize * 256 + 512 * KIB);

/// The fixed part of a score request: the reply, its parsed allow-list, the
/// answer line and the workers-rs request and response objects.
pub const SCORE_CONTROL_BYTES: usize = REPLY_BYTES + 512 * KIB;
/// A warm request: no body, one small JSON answer.
pub const WARM_CONTROL_BYTES: usize = 64 * KIB;
/// A feedback request: at most [`FEEDBACK_BODY_BYTES`] in, nothing out.
pub const FEEDBACK_CONTROL_BYTES: usize = 64 * KIB;
/// A 404: nothing beyond the route's own state.
pub const OTHER_CONTROL_BYTES: usize = slop_ninja_arena::PAGE;

/// A feedback body: `{"receipt":"<32 hex>.<band>.<bin>.<32 hex>","claim":"was_ai"}`.
pub const FEEDBACK_BODY_BYTES: usize = 256;

pub const MIN_CONTROL_BYTES: usize = OTHER_CONTROL_BYTES;
pub const LARGEST_INVOCATION_BYTES: usize = SCORE_CONTROL_BYTES + slop_ninja_arena::PAGE;

/// Bytes a score request can hold per byte of its body, at most: the body
/// (1), the unescaped text (at most 1: unescaping never lengthens), the
/// text's NFC copy inside the token count (3: NFC never more than triples
/// UTF-8), the count's symbol buffer and merge queue for its longest
/// pre-token (12 + 24 per byte, doubled as they grow: 72), and the body
/// forwarded to the scorer (6: minimal JSON escaping is at most `\u00XX`
/// per byte).
pub const BYTES_PER_BODY_BYTE: usize = 1 + 1 + 3 + 72 + 6;

/// The Durable Objects' share of an isolate they may share with the
/// Worker: the gate's ticket table ([`GATE_TABLE_BYTES`]) and the lane's
/// manifest and replies.
pub const GATE_TABLE_BYTES: usize = 8 * MIB;
pub const LANE_RESIDENT_BYTES: usize = REPLY_BYTES + 2 * MIB;

/// Bytes one ticket holds in the gate: its waiting entry, its owner entry
/// and their B-tree and deque overhead, rounded up.
pub const TICKET_BYTES: usize = 128;
/// The gate's ticket table bound, from its memory share.
pub const MAX_TICKETS: usize = GATE_TABLE_BYTES / TICKET_BYTES;

/// The isolate's scan memory: the model's linear memory less the stack, the
/// glue partition, the objects' share and room for one invocation of every
/// other route at once per glue ticket's worth of them (bounded by the
/// largest non-score footprint times the glue tickets).
pub const SCAN_MEMORY_BYTES: usize = model::LINEAR_MEMORY_BYTES
    .saturating_sub(model::STACK_BYTES)
    .saturating_sub(model::GLUE_BUDGET_BYTES)
    .saturating_sub(GATE_TABLE_BYTES)
    .saturating_sub(LANE_RESIDENT_BYTES)
    .saturating_sub(WARM_CONTROL_BYTES * 16);

/// A score request's reservation for a declared body of `body_bytes`
/// (`None`: no declared length, so the bound).
pub fn scan_footprint(body_bytes: Option<u64>) -> usize {
    let n = body_bytes.unwrap_or(bound::RAW_BODY_BYTES).min(bound::RAW_BODY_BYTES);
    let n = usize::try_from(n).unwrap_or(usize::MAX);
    n.saturating_mul(BYTES_PER_BODY_BYTE).saturating_add(SCORE_CONTROL_BYTES)
}

/// The isolate's scan reservations (wasm32 isolates are single-threaded).
#[derive(Debug, Default)]
pub struct ScanMemory {
    held: std::cell::Cell<usize>,
}

/// Why a reservation was not granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoRoom {
    /// Others hold the room now: try again.
    Busy,
    /// Even alone this request would not fit.
    TooLarge,
}

impl ScanMemory {
    pub const fn new() -> ScanMemory {
        ScanMemory { held: std::cell::Cell::new(0) }
    }

    /// Takes `bytes` from the pool of scan memory; the caller gives them back with
    /// [`ScanMemory::give_back`] when its request ends (a guard's `Drop`).
    pub fn take(&self, bytes: usize) -> Result<(), NoRoom> {
        if bytes > SCAN_MEMORY_BYTES {
            return Err(NoRoom::TooLarge);
        }
        let held = self.held.get();
        if held.saturating_add(bytes) > SCAN_MEMORY_BYTES {
            return Err(NoRoom::Busy);
        }
        self.held.set(held + bytes);
        Ok(())
    }

    pub fn give_back(&self, bytes: usize) {
        self.held.set(self.held.get().saturating_sub(bytes));
    }

    pub fn held(&self) -> usize {
        self.held.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_matches_the_route_table() {
        assert_eq!(model::MIN_ROUTE_BYTES, MIN_CONTROL_BYTES);
        assert_eq!(model::LARGEST_ROUTE_BYTES, LARGEST_INVOCATION_BYTES);
        const { assert!(FEEDBACK_BODY_BYTES + 4 * KIB < FEEDBACK_CONTROL_BYTES) };
        const { assert!(model::LARGEST_ROUTE_BYTES < model::LINEAR_MEMORY_BYTES) };
    }

    #[test]
    fn the_scan_memory_holds_ordinary_texts_side_by_side() {
        // A 1,500-word paste is about 10 KB of JSON.
        let one = scan_footprint(Some(10_000));
        assert!(SCAN_MEMORY_BYTES / one >= 32, "scan memory {SCAN_MEMORY_BYTES}, one {one}");
        // No declared length reserves the bound, which no isolate holds.
        assert!(scan_footprint(None) > SCAN_MEMORY_BYTES);
    }

    #[test]
    fn reservations_return_their_bytes() {
        let b = ScanMemory::new();
        let big = SCAN_MEMORY_BYTES / 2 + 1;
        b.take(big).unwrap();
        assert_eq!(b.take(big).unwrap_err(), NoRoom::Busy);
        assert_eq!(b.take(SCAN_MEMORY_BYTES + 1).unwrap_err(), NoRoom::TooLarge);
        b.give_back(big);
        assert_eq!(b.held(), 0);
        assert!(b.take(big).is_ok());
    }
}

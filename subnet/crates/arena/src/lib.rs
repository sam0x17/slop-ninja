//! Bounded per-invocation memory arenas for the SlopNinja Workers.
//!
//! Implements `docs/workers-arena-design.md` sections 2 to 6; the global
//! allocator of both Workers built from `subnet/coordinator`. The rule it
//! implements is that **no allocation happens outside a bounded memory
//! arena**:
//!
//! - [`ArenaAlloc`] is the global allocator. It owns every heap page through a
//!   page pool whose size derives from the versioned memory model
//!   (`memory-model.json`, [`model`]): `floor((isolate_bytes - static_bytes -
//!   js_reserve_bytes) / 64 KiB)` pages, never more.
//! - The pool is a hierarchy of byte owners: the isolate (the pool, with a
//!   fixed glue partition and a quarantine budget), each invocation (a
//!   [`Scope`] opened by [`enter`]), and each step or round (a child scope
//!   carved from the pages its parent lent, [`ScopeRef::child`]).
//! - A scope's admission physically holds its pages: heap runs sized by
//!   dlmalloc footprint ([`Footprint`]), and one run per admitted block,
//!   which serves exactly the allocation it was admitted for, directly. An
//!   allocation inside an admitted footprint never fails for want of pages
//!   or contiguity, in any allocation order.
//! - Closing a scope returns all of its pages. A scope that still owns live
//!   bytes is trimmed and quarantined; one whose future stops being polled
//!   past its route's deadline is counted as abandoned. Both pin pages
//!   against the quarantine budget; past it, or when a refusal is caused by
//!   pinned pages, the pool reports [`Health::Poisoned`] and the host
//!   recycles the instance after recording a durable fault keyed by the
//!   cause ([`PoisonCause`]); a second recycle for the same cause pauses the
//!   route instead of looping.
//! - Allocations made while no scope is current go to the glue partition,
//!   admitted per entry with a [`glue::Ticket`]. [`strict`] mode refuses
//!   unscoped allocations on an armed thread.
//! - Every refusal is an allocation-free [`Refusal`] whose kind is chosen by
//!   cause: `Retry`, `Capacity`, `Poisoned`, `Invalid` or `Defect`.
//! - [`io`] holds the zero-copy building blocks: exact-capacity buffers,
//!   bounded ingest that never truncates, streaming hash, count and chunk
//!   sinks, a re-hashing verifier, and a canonical encoder over any sink.
//!
//! The same code runs natively, over one region of the ceiling's size, and
//! on `wasm32-unknown-unknown`, over linear memory grown with `memory.grow`.
#![deny(unsafe_op_in_unsafe_fn)]
// The wasm32 stack is linear memory outside every arena: frames, futures and
// stack arrays stay below the measured bound (clippy.toml, design §5.1).
#![warn(clippy::large_stack_frames, clippy::large_futures, clippy::large_stack_arrays)]

#[cfg(all(target_family = "wasm", target_feature = "atomics"))]
compile_error!("slop-ninja-arena assumes single-threaded wasm (no atomics)");

pub mod glue;
pub mod io;
pub mod measured;
pub mod model;
mod pool;
mod scope;
mod stats;
pub mod strict;
mod sys;

#[cfg(feature = "selftest")]
pub mod selftest;

#[cfg(feature = "knobs")]
pub use pool::{reset_latches, set_mode, set_page_limit, shrink_free_to};
pub use pool::{
    ArenaAlloc, BLOCK_OVERHEAD, Footprint, Health, MALLOC_ALIGN, MAX_PAGES, MIN_CHUNK, Mode, Owner, PAGE, PinKind,
    PoisonCause, SEGMENT_OVERHEAD, STATE_BYTES, TOP_FOOT, aligned_chunk_bytes, block_bytes, chunk_bytes, mode,
    owner_of, page_limit, pages_of, scope_pages,
};
pub use scope::{
    AsyncRoute, Closed, Continuation, Entered, Reason, Refusal, RefusalKind, Route, Scope, ScopeRef, ScopeStats,
    Scoped, admit_current_block, admit_current_heap, child_bytes, enter, health, limits, tick, wait_until,
};
pub use stats::{
    Failure, FailureKind, POSTMORTEM_WORDS, Postmortem, RouteStats, Stats, postmortem, route_stats, stats,
};

/// Recycles the instance: a deliberate trap at a safe point (an export
/// entry, before any state change). worker-build 0.8.7's panic recovery
/// re-instantiates the module on the next call, which gives a fresh pool.
///
/// Before calling it the host has (design §2.6):
/// 1. written a durable recycle intent keyed by `cause` (route, kind, and
///    the memory-model version [`model::VERSION`]), so co-resident steps the
///    trap aborts are attributed to the recycle, not counted as their own
///    crashes;
/// 2. applied the stop rule: a second recycle for the same key pauses that
///    route durably (`arena_leak`, operator clear), so a route that leaks on
///    every invocation cannot recycle the isolate indefinitely.
///
/// The cause is recorded for the [`postmortem`] before the trap.
#[cold]
pub fn recycle(cause: PoisonCause) -> ! {
    pool::note_recycle(cause);
    sys::abort()
}

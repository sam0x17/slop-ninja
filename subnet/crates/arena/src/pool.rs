//! Page pool, per-scope dlmalloc heaps, direct block runs, the glue
//! partition, quarantine, abandonment and the global allocator.
//!
//! Layout (design §2.1): pool page `i` lives at `base + i × 64 KiB`. Pages
//! `[0, G)` are the glue partition (`G` = the model's glue budget); only the
//! glue heap uses them and scopes never can. Pages `[G, limit)` belong to
//! scopes. A page-owner map holds one `u16` per page: four special values
//! (`FREE`, `GLUE_FREE`, `GLUE`, `OUT`), or a scope id (the index of the
//! scope's first page) with flag bits:
//!
//! - `id`: used by the dlmalloc heap of scope `id` (its control and
//!   footprint allocations);
//! - `BLOCK | id`: one admitted block of scope `id`, served directly from its
//!   own run (never by the heap);
//! - `HELD | id`: held for scope `id`'s heap by its control or footprint
//!   admission, not yet in use;
//! - `HELD | BLOCK | id`: a block run admitted for one allocation of an exact
//!   layout, not yet (or no longer) in use;
//! - `HELD | LEND | id`: pages scope `id` lent to its children. Only a child
//!   can carve them, and carved pages come back here when the child closes.
//!
//! Nobody else can take a held page, and the heap never takes a block or
//! lent page, so an allocation inside an admitted footprint or block never
//! needs a page someone else could have taken (design §2.6, guarantee 1 and
//! 2).
//!
//! A scope's arena id is the index of its first page, which holds its
//! [`ScopeState`] (a dlmalloc 0.2.12 instance plus accounting and a `u64`
//! epoch, unique for the instance's life).
//!
//! Accounting per scope: `reserved` (R) pages admitted, `used` (U) pages in
//! heap segments or in-use blocks, `held` (H) pages held unused, and
//! `overdraft_now` pages used beyond R. Invariant: `U + H == R + overdraft_now`.
//!
//! Refusals are classed by cause (design §2.4): `Capacity` when the need can
//! never fit beside the requester's own pages and its ancestors' (which
//! waiting cannot free), `Poisoned` when pages pinned by quarantined or
//! abandoned scopes (or the fragmentation they cause) block it, `Retry` when
//! other open scopes' pages would satisfy it once they close.
//!
//! Locking: natively every entry point takes the pool lock once; everything
//! called from dlmalloc's system-allocator hooks runs under it and never takes
//! it again. Nothing under the lock may panic: arithmetic is checked with
//! [`abort`] on invariant failure, counters saturate, and table accesses are
//! bounds-checked through accessors that abort. wasm32 (no atomics) is
//! single-threaded.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

use dlmalloc::{Allocator, Dlmalloc};

use crate::model;
use crate::scope::{Closed, Reason, Refusal, RefusalKind, ScopeStats};
use crate::stats::{Counters, Failure, FailureKind, RouteCounters};
use crate::sys::{self, Racy, Region, abort, current, lock};

/// Pool page size: the wasm page and dlmalloc's system granularity.
pub const PAGE: usize = model::PAGE_BYTES;
/// Size of the static tables: the pages in a whole isolate. The usable pool
/// (`page_limit`) is the model's heap ceiling, which is smaller.
pub const MAX_PAGES: usize = model::ISOLATE_BYTES / PAGE;
const WORDS: usize = MAX_PAGES.div_ceil(64);
pub(crate) const NONE: u16 = u16::MAX;
const FREE: u16 = u16::MAX;
const GLUE_FREE: u16 = u16::MAX - 1;
pub(crate) const GLUE: u16 = u16::MAX - 2;
/// A page outside the pool (at or above `limit`). The free bitmap and
/// `free_count` track exactly the pages whose owner is `FREE`.
pub(crate) const OUT: u16 = u16::MAX - 3;
pub(crate) const HELD: u16 = 0x8000;
const BLOCK: u16 = 0x4000;
const LEND: u16 = 0x2000;
const ID: u16 = 0x1FFF;
const _: () = assert!(MAX_PAGES <= ID as usize);
// The largest flagged encoding stays below the special values.
const _: () = assert!((HELD | BLOCK | ID) < OUT && (HELD | LEND | ID) < OUT);

/// Whether an owner value is one of the four special values.
#[inline]
const fn special(o: u16) -> bool {
    o >= OUT
}

/// The scope a page belongs to (used, block, held or lent), if any.
#[inline]
const fn scope_of(o: u16) -> Option<u16> {
    if special(o) { None } else { Some(o & ID) }
}

// ------------------------------------------------------------------ dlmalloc 0.2.12 footprint

const W: usize = size_of::<usize>();
/// dlmalloc's `malloc_alignment()`: two words.
pub const MALLOC_ALIGN: usize = 2 * W;
/// dlmalloc's `chunk_overhead()`: one word per in-use chunk.
const CHUNK_OVERHEAD: usize = W;
/// dlmalloc's `min_chunk_size()`: `align_up(size_of::<Chunk>() = 4W, 2W)`.
pub const MIN_CHUNK: usize = 4 * W;
/// dlmalloc's `top_foot_size()`: `pad_request(size_of::<Segment>() = 4W)
/// (= 6W) + min_chunk_size (4W)`, the same 10W on wasm32 and 64-bit.
pub const TOP_FOOT: usize = 10 * W;
/// What `sys_alloc` adds to a request before rounding to the 64 KiB
/// granularity: `top_foot_size + malloc_alignment`.
pub const SEGMENT_OVERHEAD: usize = TOP_FOOT + MALLOC_ALIGN;
/// Kept for callers of the previous API: the most a single block's segment
/// request exceeds its length by, before page rounding (`SEGMENT_OVERHEAD`
/// plus the largest `pad_request` slack, `CHUNK_OVERHEAD + MALLOC_ALIGN`).
pub const BLOCK_OVERHEAD: usize = SEGMENT_OVERHEAD + CHUNK_OVERHEAD + MALLOC_ALIGN;

const fn align_up(n: usize, a: usize) -> usize {
    match n.checked_add(a - 1) {
        Some(v) => v & !(a - 1),
        None => usize::MAX & !(a - 1),
    }
}

/// dlmalloc's `request2size`: the chunk an allocation of `req` bytes takes.
pub const fn chunk_bytes(req: usize) -> usize {
    if req < MIN_CHUNK - CHUNK_OVERHEAD - 1 { MIN_CHUNK } else { align_up(req.saturating_add(CHUNK_OVERHEAD), MALLOC_ALIGN) }
}

/// The chunk an allocation of `len` bytes at `align` takes. An alignment above
/// dlmalloc's own goes through `memalign`, which mallocs
/// `request2size(len) + max(align, min_chunk) + min_chunk - chunk_overhead`.
pub const fn aligned_chunk_bytes(len: usize, align: usize) -> usize {
    if align <= MALLOC_ALIGN {
        chunk_bytes(len)
    } else {
        let a = if align < MIN_CHUNK { MIN_CHUNK } else { align };
        chunk_bytes(chunk_bytes(len).saturating_add(a).saturating_add(MIN_CHUNK - CHUNK_OVERHEAD))
    }
}

/// Bytes a reservation holds for one block of `len` bytes at `align`: the
/// segment dlmalloc 0.2.12 would request for it, rounded to pages. A block
/// is served directly from its own run, which needs at most `len` plus the
/// alignment padding, so this is an upper bound kept equal to the heap's
/// request (the measured models are derived with it).
pub const fn block_bytes(len: usize, align: usize) -> usize {
    align_up(aligned_chunk_bytes(len, align).saturating_add(SEGMENT_OVERHEAD), PAGE)
}

/// The dlmalloc footprint of a set of simultaneously live allocations: the
/// sum of their chunks, plus the largest one again (the segment `sys_alloc`
/// may request for it while the heap's top still has free space it does not
/// count) and the segment overhead. Admission takes footprints, never raw
/// byte counts (design §2.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Footprint {
    chunks: usize,
    largest: usize,
}

impl Footprint {
    pub const ZERO: Footprint = Footprint { chunks: 0, largest: 0 };

    /// One live allocation of `size` bytes at `align`.
    pub const fn alloc(self, size: usize, align: usize) -> Footprint {
        self.allocs(size, align, 1)
    }

    /// `count` live allocations of `size` bytes each at `align`.
    pub const fn allocs(self, size: usize, align: usize, count: usize) -> Footprint {
        if count == 0 {
            return self;
        }
        let c = aligned_chunk_bytes(size, align);
        Footprint {
            chunks: self.chunks.saturating_add(c.saturating_mul(count)),
            largest: if c > self.largest { c } else { self.largest },
        }
    }

    /// A growing buffer's footprint at its peak: during the last doubling the
    /// old block (`final_cap / 2`) and the new one coexist.
    pub const fn growth(self, final_cap: usize, align: usize) -> Footprint {
        self.alloc(final_cap, align).alloc(final_cap / 2, align)
    }

    /// Bytes to reserve in a scope heap for this footprint.
    pub const fn bytes(self) -> usize {
        self.chunks.saturating_add(self.largest).saturating_add(SEGMENT_OVERHEAD)
    }

    pub const fn chunks(self) -> usize {
        self.chunks
    }
}

/// Whole pages for `bytes`, saturating at `u32::MAX` (a need no pool holds).
pub const fn pages_of(bytes: usize) -> u32 {
    let p = bytes.div_ceil(PAGE);
    if p > u32::MAX as usize { u32::MAX } else { p as u32 }
}

#[inline]
fn add32(a: u32, b: u32) -> u32 {
    match a.checked_add(b) {
        Some(v) => v,
        None => abort(),
    }
}
#[inline]
fn sub32(a: u32, b: u32) -> u32 {
    match a.checked_sub(b) {
        Some(v) => v,
        None => abort(),
    }
}
#[inline]
fn addz(a: usize, b: usize) -> usize {
    match a.checked_add(b) {
        Some(v) => v,
        None => abort(),
    }
}
#[inline]
fn subz(a: usize, b: usize) -> usize {
    match a.checked_sub(b) {
        Some(v) => v,
        None => abort(),
    }
}
#[inline]
fn inc(c: &mut u64) {
    *c = c.saturating_add(1);
}

/// What a bound violation does (design §2.6 and §3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    /// Record everything, refuse nothing the pool can physically serve
    /// (migration step 1).
    Observe = 0,
    /// Admission refuses; an overdraft takes only free pages and is recorded
    /// (production).
    Enforce = 1,
    /// Like `Enforce`, and overdrafts are refused; on an armed thread an
    /// allocation outside every scope is refused (tests and drills).
    Strict = 2,
}

/// Which arena owns a pointer's page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Glue,
    Scope(u16),
}

/// What pins the pages that poison a pool (the key of the host's durable
/// stop rule, design §2.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PinKind {
    /// Scopes closed with live bytes (a value outlived its invocation).
    Quarantined = 1,
    /// Scopes not polled past their route's deadline (an await that never
    /// settled).
    Abandoned = 2,
    /// A refusal was classed `Poisoned` and nothing is pinned any more.
    Refusal = 3,
    /// The platform could not provide the pool.
    Platform = 4,
}

/// The cause of a poisoned pool: the route whose scopes pin the most pages,
/// how they pin them, and how many pages that route pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoisonCause {
    pub route: u8,
    pub kind: PinKind,
    pub pages: u32,
}

/// Whether the pool can still admit its declared routes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Healthy,
    /// Pages pinned by quarantined or abandoned scopes exceed the model's
    /// quarantine budget, or a refusal was classed `Poisoned`. Only an
    /// instance recycle returns them: the host records a durable fault keyed
    /// by `cause` (route, kind, memory-model version), applies the stop rule
    /// and recycles at its next safe point (design §2.6).
    Poisoned { pinned_pages: u32, cause: PoisonCause },
}

pub(crate) const OPEN: u8 = 1;
pub(crate) const QUARANTINED: u8 = 2;
pub(crate) const ABANDONED: u8 = 3;

/// Lives at the start of a scope's first page.
#[repr(C)]
pub(crate) struct ScopeState {
    heap: Dlmalloc<PageSource>,
    pub epoch: u64,
    pub parent_epoch: u64,
    pub route: u8,
    pub status: u8,
    pub polling: u16,
    first_seg_done: bool,
    bound_exceeded: bool,
    /// The scope lives for the instance (a home scope): its pages are never
    /// freed by waiting, so a refusal they cause is `Capacity`.
    permanent: bool,
    pub parent: u16,
    /// Pages this scope holds or uses that were carved from its parent's
    /// lent pages (they go back to the parent when released).
    pub carved_pages: u32,
    pub reserved_bytes: usize,
    pub reserved_pages: u32,
    pub used_pages: u32,
    pub held_pages: u32,
    pub lent_pages: u32,
    pub overdraft_now: u32,
    pub overdraft_pages: u32,
    pub peak_pages: u32,
    /// Block runs held and not in use, the smallest admitted length among
    /// them (a lower bound), and the most recently admitted or freed run.
    pending_blocks: u32,
    pending_min: usize,
    last_block: u32,
    last_end: u32,
    lo: u32,
    hi: u32,
    pub live: usize,
    pub peak_live: usize,
    pub allocs: u64,
    pub deadline_ms: u64,
    pub children: u32,
}

/// Bytes at the start of a scope's first page taken by its state; the rest
/// of that page starts the scope's first heap segment.
pub const STATE_BYTES: usize = align_up(size_of::<ScopeState>(), MALLOC_ALIGN);
const _: () = assert!(STATE_BYTES < PAGE);

/// Pages a scope needs for `bytes` of heap footprint, its state included.
pub const fn scope_pages(bytes: usize) -> u32 {
    pages_of(STATE_BYTES.saturating_add(bytes))
}

/// One admitted block run, recorded at the run's first page (`pages > 0`
/// marks a first page; the other pages of a run hold zeros).
#[derive(Clone, Copy)]
struct Blk {
    len: usize,
    align: usize,
    pages: u32,
    /// The run was served and freed since its last admission: a later
    /// admission of this scope may re-key it instead of holding a new run.
    returned: bool,
}

impl Blk {
    const ZERO: Blk = Blk { len: 0, align: 0, pages: 0, returned: false };
}

/// What an admission holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hold {
    /// Pages for the scope's heap (control and footprint allocations).
    Heap,
    /// One run for one allocation of exactly `len` bytes at `align`.
    Block { len: usize, align: usize },
    /// Pages lent to children.
    Lend,
}

impl Hold {
    const fn tag(self, id: u16) -> u16 {
        match self {
            Hold::Heap => HELD | id,
            Hold::Block { .. } => HELD | BLOCK | id,
            Hold::Lend => HELD | LEND | id,
        }
    }
}

pub(crate) struct Pool {
    ready: bool,
    failed: bool,
    pub region: Region,
    pub limit: u32,
    pub region_pages: u32,
    pub glue_budget: u32,
    pub glue_used: u32,
    /// Glue pages outside the partition (`glue-spill` only).
    pub glue_spill: u32,
    pub free_count: u32,
    pub in_use: u32,
    pub peak_in_use: u32,
    pub high_water: u32,
    pub epoch: u64,
    pub clock_ms: u64,
    pub tickets: u32,
    pub tickets_peak: u32,
    pub poisoned_refusals: u64,
    /// The pinning cause when a refusal was first classed `Poisoned`.
    pub poison_latch: Option<PoisonCause>,
    /// Set by `recycle` just before its trap (read by the abort hook).
    pub recycle_intent: Option<PoisonCause>,
    closing: u16,
    /// Why the last system-hook call returned no segment, if not exhaustion.
    pending: Option<FailureKind>,
    pub mode: Mode,
    free: [u64; WORDS],
    owner: [u16; MAX_PAGES],
    /// Per page, a stack of "carved from the holder's parent" bits (bit 0 is
    /// the current holder): pages return to the ancestor they came from.
    carve: [u32; MAX_PAGES],
    blk: [Blk; MAX_PAGES],
    pub c: Counters,
    pub routes: [RouteCounters; 256],
}

impl Pool {
    const fn new() -> Self {
        Self {
            ready: false,
            failed: false,
            region: Region { base: ptr::null_mut(), static_bytes: 0, committed: 0 },
            limit: 0,
            region_pages: 0,
            glue_budget: 0,
            glue_used: 0,
            glue_spill: 0,
            free_count: 0,
            in_use: 0,
            peak_in_use: 0,
            high_water: 0,
            epoch: 0,
            clock_ms: 0,
            tickets: 0,
            tickets_peak: 0,
            poisoned_refusals: 0,
            poison_latch: None,
            recycle_intent: None,
            closing: NONE,
            pending: None,
            mode: if cfg!(feature = "strict") {
                Mode::Strict
            } else if cfg!(feature = "observe") {
                Mode::Observe
            } else {
                Mode::Enforce
            },
            free: [0; WORDS],
            owner: [OUT; MAX_PAGES],
            carve: [0; MAX_PAGES],
            blk: [Blk::ZERO; MAX_PAGES],
            c: Counters::ZERO,
            routes: [RouteCounters::ZERO; 256],
        }
    }

    #[inline]
    pub(crate) fn own(&self, i: u32) -> u16 {
        match self.owner.get(i as usize) {
            Some(v) => *v,
            None => abort(),
        }
    }

    #[inline]
    fn set_own(&mut self, i: u32, v: u16) {
        let was_free = self.own(i) == FREE;
        let now_free = v == FREE;
        match self.owner.get_mut(i as usize) {
            Some(o) => *o = v,
            None => abort(),
        }
        if was_free != now_free {
            let Some(w) = self.free.get_mut(i as usize / 64) else { abort() };
            if now_free {
                *w |= 1 << (i % 64);
                self.free_count = add32(self.free_count, 1);
            } else {
                *w &= !(1 << (i % 64));
                self.free_count = sub32(self.free_count, 1);
            }
        }
    }

    #[inline]
    fn carve_at(&self, i: u32) -> u32 {
        match self.carve.get(i as usize) {
            Some(v) => *v,
            None => abort(),
        }
    }

    #[inline]
    fn set_carve(&mut self, i: u32, v: u32) {
        match self.carve.get_mut(i as usize) {
            Some(c) => *c = v,
            None => abort(),
        }
    }

    #[inline]
    fn blk(&self, i: u32) -> Blk {
        match self.blk.get(i as usize) {
            Some(b) => *b,
            None => abort(),
        }
    }

    #[inline]
    fn set_blk(&mut self, i: u32, b: Blk) {
        match self.blk.get_mut(i as usize) {
            Some(x) => *x = b,
            None => abort(),
        }
    }

    #[inline]
    fn word(&self, i: u32) -> u64 {
        match self.free.get(i as usize / 64) {
            Some(w) => *w,
            None => abort(),
        }
    }

    /// Lowest run of `k` pages in `[from, to)` for which `ok(page)` holds.
    fn find_run_by(&self, from: u32, to: u32, k: u32, ok: impl Fn(u32, u16) -> bool) -> Option<u32> {
        if k == 0 || to < from || k > to - from {
            return None;
        }
        let (mut start, mut len) = (from, 0u32);
        for i in from..to {
            if ok(i, self.own(i)) {
                if len == 0 {
                    start = i;
                }
                len += 1;
                if len == k {
                    return Some(start);
                }
            } else {
                len = 0;
            }
        }
        None
    }

    /// Lowest run of `k` free scope pages (first fit keeps the high-water low).
    fn find_free_run(&self, k: u32) -> Option<u32> {
        let (from, to) = (self.glue_budget, self.limit);
        if k == 0 || to <= from || k > to - from {
            return None;
        }
        let (mut start, mut len) = (from, 0u32);
        let mut i = from;
        while i < to {
            if i.is_multiple_of(64) && self.word(i) == 0 {
                len = 0;
                i = i.saturating_add(64);
                continue;
            }
            if (self.word(i) >> (i % 64)) & 1 == 1 {
                if len == 0 {
                    start = i;
                }
                len += 1;
                if len == k {
                    return Some(start);
                }
            } else {
                len = 0;
            }
            i += 1;
        }
        None
    }

    /// Largest run of free pages in the scope partition.
    pub(crate) fn largest_run(&self) -> u32 {
        let (mut best, mut len) = (0u32, 0u32);
        for i in self.glue_budget..self.limit {
            if self.own(i) == FREE {
                len += 1;
                best = best.max(len);
            } else {
                len = 0;
            }
        }
        best
    }

    /// Pages scopes can ever hold: the pool minus the glue partition.
    pub(crate) fn scope_partition(&self) -> u32 {
        self.limit.saturating_sub(self.glue_budget)
    }

    fn commit(&mut self, end: u32) -> bool {
        if !sys::commit(&mut self.region, end) {
            return false;
        }
        self.high_water = self.high_water.max(end);
        true
    }

    fn page_ptr(&self, i: u32) -> *mut u8 {
        self.region.base.wrapping_add(i as usize * PAGE)
    }

    /// Whole pages spanned by `[ptr, ptr + size)`.
    fn span(&self, ptr: *mut u8, size: usize) -> (u32, u32) {
        let off = (ptr as usize).wrapping_sub(self.region.base as usize);
        let start = off / PAGE;
        let end = match off.checked_add(size) {
            Some(e) => e.div_ceil(PAGE),
            None => abort(),
        };
        if end > self.region_pages as usize || start >= end {
            abort();
        }
        (start as u32, (end - start) as u32)
    }

    pub(crate) fn page_of(&self, ptr: *const u8) -> Option<u32> {
        let off = (ptr as usize).wrapping_sub(self.region.base as usize);
        let page = off / PAGE;
        (self.ready && page < self.region_pages as usize).then_some(page as u32)
    }

    /// Whether `page` is the state page of a live scope.
    pub(crate) fn is_state_page(&self, page: u32) -> bool {
        page < self.region_pages && self.own(page) == page as u16
    }

    /// Whether `(id, epoch)` names a live scope (open, abandoned or
    /// quarantined).
    fn is_scope(&self, id: u16, epoch: u64) -> bool {
        id != NONE && self.is_state_page(id as u32) && state_ref(self, id).epoch == epoch
    }

    /// Whether `(id, epoch)` names a scope that may still allocate and admit:
    /// open, or abandoned (a late poll recovers it).
    fn is_active(&self, id: u16, epoch: u64) -> bool {
        self.is_scope(id, epoch) && matches!(state_ref(self, id).status, OPEN | ABANDONED)
    }

    /// The live parent of scope `id`, if its recorded parent still exists.
    fn parent_of(&self, id: u16) -> u16 {
        let s = state_ref(self, id);
        if self.is_scope(s.parent, s.parent_epoch) { s.parent } else { NONE }
    }

    /// Whether `id` is `chain` or one of its ancestors.
    fn in_chain(&self, id: u16, chain: u16) -> bool {
        let mut c = chain;
        while c != NONE {
            if c == id {
                return true;
            }
            c = self.parent_of(c);
        }
        false
    }

    /// Pages pinned by quarantined or abandoned scopes (used and held).
    pub(crate) fn pinned_pages(&self) -> u32 {
        let mut n = 0u32;
        for page in self.glue_budget..self.region_pages {
            if self.is_state_page(page) {
                let s = state_ref(self, page as u16);
                if s.status == QUARANTINED || s.status == ABANDONED {
                    n = n.saturating_add(s.used_pages.saturating_add(s.held_pages));
                }
            }
        }
        n
    }

    fn is_permanent_scope(&self, id: u16) -> bool {
        self.is_state_page(id as u32) && state_ref(self, id).permanent
    }

    fn is_pinned_scope(&self, id: u16) -> bool {
        self.is_state_page(id as u32) && matches!(state_ref(self, id).status, QUARANTINED | ABANDONED)
    }

    /// The route whose quarantined or abandoned scopes pin the most pages.
    pub(crate) fn dominant_pin(&self) -> Option<PoisonCause> {
        // Per (route, kind) pages, on the stack: 2 × 256 counters (2 KiB).
        let mut by = [[0u32; 256]; 2];
        let mut any = false;
        for page in self.glue_budget..self.region_pages {
            if self.is_state_page(page) {
                let s = state_ref(self, page as u16);
                let k = match s.status {
                    QUARANTINED => 0,
                    ABANDONED => 1,
                    _ => continue,
                };
                let Some(slot) = by[k].get_mut(s.route as usize) else { abort() };
                *slot = slot.saturating_add(s.used_pages.saturating_add(s.held_pages));
                any = true;
            }
        }
        if !any {
            return None;
        }
        let mut best = PoisonCause { route: 0, kind: PinKind::Quarantined, pages: 0 };
        for (k, row) in by.iter().enumerate() {
            for (r, &n) in row.iter().enumerate() {
                if n > best.pages {
                    best = PoisonCause { route: r as u8, kind: if k == 0 { PinKind::Quarantined } else { PinKind::Abandoned }, pages: n };
                }
            }
        }
        Some(best)
    }

    /// Classifies a shortfall of `need` contiguous pages by cause.
    ///
    /// `chain` is the requesting scope (for a new child, its parent; `NONE`
    /// for a root scope). Its pages and its ancestors' pages are never freed
    /// by waiting: the requester is waiting on them. Pages `lend_from` lent
    /// are holdable by the requester, so they count as available.
    fn classify(&mut self, route: u8, need: u32, chain: u16, lend_from: u16) -> Refusal {
        let part = self.scope_partition();
        let lent = if lend_from == NONE { NONE } else { HELD | LEND | lend_from };
        let (mut own, mut pinned) = (0u32, 0u32);
        let (mut run_own, mut best_own, mut run_pin, mut best_pin) = (0u32, 0u32, 0u32, 0u32);
        // Pages of one scope are usually contiguous: cache the last verdict.
        let (mut last_id, mut last_class) = (NONE, 0u8);
        for i in self.glue_budget..self.limit {
            let o = self.own(i);
            // 0: free or other (waiting frees it), 1: never freed by
            // waiting (the chain's or a permanent home scope's), 2: pinned.
            // Glue spilled into the scope partition is class 0: a spilled
            // segment goes back to the pool when its last allocation is
            // freed (`unuse_glue`), which the spilling entry's end does, so
            // a refusal it causes clears by waiting (`Retry`), never a
            // durable `Capacity` pause.
            let class = match scope_of(o) {
                _ if o == lent => 0,
                None => 0,
                Some(id) if id == last_id => last_class,
                Some(id) => {
                    let c = if self.in_chain(id, chain) || self.is_permanent_scope(id) {
                        1
                    } else if self.is_pinned_scope(id) {
                        2
                    } else {
                        0
                    };
                    (last_id, last_class) = (id, c);
                    c
                }
            };
            match class {
                1 => {
                    own += 1;
                    run_own = 0;
                    run_pin = 0;
                }
                2 => {
                    pinned += 1;
                    run_own += 1;
                    run_pin = 0;
                }
                _ => {
                    run_own += 1;
                    run_pin += 1;
                }
            }
            best_own = best_own.max(run_own);
            best_pin = best_pin.max(run_pin);
        }
        let (kind, reason) = if need > part.saturating_sub(own) {
            (RefusalKind::Capacity, Reason::Pages)
        } else if need > best_own {
            (RefusalKind::Capacity, Reason::Contiguity)
        } else if need > part.saturating_sub(own).saturating_sub(pinned) {
            (RefusalKind::Poisoned, Reason::Pages)
        } else if need > best_pin {
            (RefusalKind::Poisoned, Reason::Contiguity)
        } else if self.free_count >= need {
            (RefusalKind::Retry, Reason::Contiguity)
        } else {
            (RefusalKind::Retry, Reason::Pages)
        };
        if kind == RefusalKind::Poisoned && self.poison_latch.is_none() {
            self.poison_latch = self.dominant_pin();
        }
        self.count_refusal(route, kind);
        let have = if reason == Reason::Contiguity { self.largest_run() } else { self.free_count };
        Refusal::new(route, kind, reason, (need as usize).saturating_mul(PAGE), (have as usize).saturating_mul(PAGE))
    }

    pub(crate) fn count_refusal(&mut self, route: u8, kind: RefusalKind) {
        let Some(r) = self.routes.get_mut(route as usize) else { abort() };
        match kind {
            RefusalKind::Retry => {
                inc(&mut self.c.refusals_retry);
                inc(&mut r.refusals_retry);
            }
            RefusalKind::Capacity => {
                inc(&mut self.c.refusals_capacity);
                inc(&mut r.refusals_capacity);
            }
            RefusalKind::Poisoned => {
                inc(&mut self.c.refusals_poisoned);
                inc(&mut r.refusals_poisoned);
                inc(&mut self.poisoned_refusals);
            }
            RefusalKind::Invalid | RefusalKind::Defect => {}
        }
    }

    fn route_mut(&mut self, route: u8) -> &mut RouteCounters {
        match self.routes.get_mut(route as usize) {
            Some(r) => r,
            None => abort(),
        }
    }

    fn fail(&mut self, kind: FailureKind, arena: u16, route: u8, size: usize, align: usize) {
        inc(&mut self.c.alloc_failures);
        if arena != GLUE {
            inc(&mut self.route_mut(route).alloc_failures);
        }
        self.c.last_failure = Failure {
            kind,
            glue: arena == GLUE,
            route,
            size,
            align,
            in_use_pages: self.in_use,
            free_pages: self.free_count,
            limit_pages: self.limit,
        };
    }

    /// A refusal for a handle whose scope is gone or quarantined.
    fn stale(&mut self, route: u8) -> Refusal {
        inc(&mut self.c.stale_admissions);
        Refusal::new(route, RefusalKind::Defect, Reason::Stale, 0, 0)
    }

    pub(crate) fn ready_for_postmortem(&self) -> bool {
        self.ready
    }

    pub(crate) fn memory_bytes(&self) -> usize {
        sys::memory_bytes(&self.region, self.high_water)
    }

    pub(crate) fn health(&self) -> Health {
        let pinned = self.pinned_pages();
        if pinned > quarantine_budget(self) || self.poisoned_refusals > 0 {
            let cause = self
                .dominant_pin()
                .or(self.poison_latch)
                .unwrap_or(PoisonCause { route: 0, kind: PinKind::Refusal, pages: 0 });
            let cause = if pinned == 0 { PoisonCause { kind: PinKind::Refusal, ..cause } } else { cause };
            Health::Poisoned { pinned_pages: pinned, cause }
        } else {
            Health::Healthy
        }
    }

    /// Records an invocation whose admissions (its scope and its ancestors')
    /// exceed the model's `largest_route_bytes`: the model's derivation is
    /// wrong for this route, a defect counted once per scope.
    fn check_route_bound(&mut self, id: u16) {
        let mut total = 0u64;
        let mut c = id;
        while c != NONE {
            total = total.saturating_add(state_ref(self, c).reserved_pages as u64);
            c = self.parent_of(c);
        }
        let bytes = total.saturating_mul(PAGE as u64);
        let route = state_ref(self, id).route;
        let r = self.route_mut(route);
        r.max_invocation_bytes = r.max_invocation_bytes.max(bytes);
        if bytes > model::LARGEST_ROUTE_BYTES as u64 {
            // SAFETY: `id` is a live scope (callers checked); its state is at
            // the start of its first page.
            let s = self.page_ptr(id as u32).cast::<ScopeState>();
            unsafe {
                if !(*s).bound_exceeded {
                    (*s).bound_exceeded = true;
                    inc(&mut self.c.route_bound_exceeded);
                    inc(&mut self.route_mut(route).bound_exceeded);
                }
            }
        }
    }
}

/// The model's quarantine budget for this pool: the scope partition minus the
/// largest declared route footprint, so that pinned pages within the budget
/// never stop the largest route from being admitted by page count.
pub(crate) fn quarantine_budget(p: &Pool) -> u32 {
    p.scope_partition().saturating_sub(pages_of(model::LARGEST_ROUTE_BYTES))
}

static POOL: Racy<Pool> = Racy::new(Pool::new());
static GLUE_HEAP: Racy<Dlmalloc<PageSource>> = Racy::new(Dlmalloc::new_with_allocator(PageSource { id: GLUE }));

/// The pool. Caller holds the lock; the reference is never kept across a
/// dlmalloc call.
#[allow(clippy::mut_from_ref)]
pub(crate) unsafe fn pool() -> &'static mut Pool {
    unsafe { &mut *POOL.get() }
}

unsafe fn state(id: u16) -> *mut ScopeState {
    unsafe { pool().page_ptr(id as u32).cast() }
}

fn state_ref(p: &Pool, id: u16) -> &ScopeState {
    // SAFETY: callers check `is_state_page(id)` first; the state lives at the
    // start of that page for as long as the page is owned by the scope.
    unsafe { &*p.page_ptr(id as u32).cast::<ScopeState>() }
}

/// Initialises the pool on first use. Caller holds the lock.
pub(crate) unsafe fn ensure_init() -> bool {
    let p = unsafe { pool() };
    if p.ready {
        return true;
    }
    if p.failed {
        return false;
    }
    sys::stack::paint();
    let pages = |static_bytes: usize| model::pool_pages(static_bytes).min(MAX_PAGES);
    let Some((region, n)) = sys::init(pages) else {
        p.failed = true;
        return false;
    };
    let glue = model::GLUE_BUDGET_PAGES;
    if glue as usize >= n {
        p.failed = true;
        return false;
    }
    p.region = region;
    p.region_pages = n as u32;
    p.limit = n as u32;
    p.glue_budget = glue;
    for i in 0..glue {
        p.set_own(i, GLUE_FREE);
    }
    for i in glue..n as u32 {
        p.set_own(i, FREE);
    }
    p.ready = true;
    true
}

/// Sets the host clock (milliseconds, any monotonic origin). Allocation-free.
pub(crate) fn set_clock(now_ms: u64) {
    let _g = lock::hold();
    unsafe {
        let p = pool();
        p.clock_ms = p.clock_ms.max(now_ms);
    }
}

fn platform_poisoned() -> Health {
    Health::Poisoned { pinned_pages: 0, cause: PoisonCause { route: 0, kind: PinKind::Platform, pages: 0 } }
}

/// Marks open scopes whose deadline passed while not being polled as
/// abandoned. Allocation-free. Returns the pool's health afterwards.
pub(crate) fn sweep() -> Health {
    let _g = lock::hold();
    unsafe {
        if !ensure_init() {
            return platform_poisoned();
        }
        let p = pool();
        let now = p.clock_ms;
        for page in p.glue_budget..p.region_pages {
            if !p.is_state_page(page) {
                continue;
            }
            let s = state(page as u16);
            if (*s).status == OPEN && (*s).polling == 0 && (*s).deadline_ms != 0 && now >= (*s).deadline_ms {
                (*s).status = ABANDONED;
                inc(&mut p.c.abandoned);
                let route = (*s).route;
                inc(&mut p.route_mut(route).abandoned);
            }
        }
        let h = p.health();
        if let Health::Poisoned { .. } = h {
            inc(&mut p.c.poisoned_sweeps);
        }
        h
    }
}

pub(crate) fn health() -> Health {
    let _g = lock::hold();
    unsafe {
        if !ensure_init() {
            return platform_poisoned();
        }
        pool().health()
    }
}

/// Records the cause a recycle is for, just before the trap. Allocation-free.
pub(crate) fn note_recycle(cause: PoisonCause) {
    let _g = lock::hold();
    unsafe {
        let p = pool();
        p.recycle_intent = Some(cause);
        inc(&mut p.c.recycles);
    }
}

// ------------------------------------------------------------------ dlmalloc system hooks

/// dlmalloc's system allocator for one arena: pages from the pool.
pub(crate) struct PageSource {
    id: u16,
}

// SAFETY: every hook runs under the pool lock (see module docs).
unsafe impl Allocator for PageSource {
    fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
        unsafe { if self.id == GLUE { glue_sys_alloc(size) } else { scope_sys_alloc(self.id, size) } }
    }
    fn remap(&self, _ptr: *mut u8, _old: usize, _new: usize, _can_move: bool) -> *mut u8 {
        ptr::null_mut()
    }
    fn free_part(&self, ptr: *mut u8, old: usize, new: usize) -> bool {
        unsafe { sys_free_part(self.id, ptr, old, new) }
    }
    fn free(&self, ptr: *mut u8, size: usize) -> bool {
        unsafe { sys_free(self.id, ptr, size) }
    }
    fn can_release_part(&self, _flags: u32) -> bool {
        true
    }
    fn allocates_zeros(&self) -> bool {
        // Released pages are reused without clearing.
        false
    }
    fn page_size(&self) -> usize {
        PAGE
    }
}

const NULL_SEG: (*mut u8, usize, u32) = (ptr::null_mut(), 0, 0);

unsafe fn glue_sys_alloc(size: usize) -> (*mut u8, usize, u32) {
    let p = unsafe { pool() };
    let k = pages_of(size);
    // Glue lives only in its partition; it never takes scope pages, and
    // scopes never take its pages.
    let (start, spill) = match p.find_run_by(0, p.glue_budget, k, |_, o| o == GLUE_FREE) {
        Some(start) => (start, false),
        None => match glue_spill_run(p, k) {
            Some(start) => (start, true),
            None => {
                p.pending = Some(FailureKind::GlueExhausted);
                return NULL_SEG;
            }
        },
    };
    let end = add32(start, k);
    if !p.commit(end) {
        p.pending = Some(FailureKind::Platform);
        return NULL_SEG;
    }
    for i in start..end {
        p.set_own(i, GLUE);
    }
    if spill {
        p.glue_spill = add32(p.glue_spill, k);
        inc(&mut p.c.glue_spills);
        p.c.glue_spill_peak_pages = p.c.glue_spill_peak_pages.max(p.glue_spill);
    } else {
        p.glue_used = add32(p.glue_used, k);
    }
    p.in_use = add32(p.in_use, k);
    p.peak_in_use = p.peak_in_use.max(p.in_use);
    (p.page_ptr(start), k as usize * PAGE, 0)
}

/// A free run in the scope partition for a glue segment the partition cannot
/// hold, with the transitional `glue-spill` feature only: the entry points
/// of a Worker that are not yet wrapped in scopes (their handler bodies run
/// with no scope current) keep working, every spilled segment is counted
/// (`glue_spills`, `glue_spill_pages`), and nothing is spilled in
/// [`Mode::Strict`] or on an armed thread, so a strict test still fails on
/// any unscoped allocation. Without the feature glue never leaves its
/// partition (design §2.2).
fn glue_spill_run(p: &Pool, k: u32) -> Option<u32> {
    if !cfg!(feature = "glue-spill") || p.mode == Mode::Strict || current::armed() {
        return None;
    }
    p.find_free_run(k)
}

/// Returns glue pages `[start, start + n)`: partition pages to the glue
/// partition, spilled pages to the free pool.
fn unuse_glue(p: &mut Pool, start: u32, n: u32) {
    for i in start..add32(start, n) {
        if p.own(i) != GLUE {
            abort();
        }
        if i < p.glue_budget {
            p.set_own(i, GLUE_FREE);
            p.glue_used = sub32(p.glue_used, 1);
        } else {
            p.set_own(i, FREE);
            p.glue_spill = sub32(p.glue_spill, 1);
        }
    }
    p.in_use = sub32(p.in_use, n);
}

/// Takes `[start, start + k)` into scope `id`'s heap. Pages held for the
/// heap leave the held count; free pages are an overdraft.
unsafe fn take_pages(id: u16, start: u32, k: u32) -> bool {
    unsafe {
        let p = pool();
        let s = state(id);
        let end = add32(start, k);
        if !p.commit(end) {
            p.pending = Some(FailureKind::Platform);
            return false;
        }
        for i in start..end {
            match p.own(i) {
                o if o == (HELD | id) => (*s).held_pages = sub32((*s).held_pages, 1),
                FREE => (*s).overdraft_now = add32((*s).overdraft_now, 1),
                _ => abort(),
            }
            p.set_own(i, id);
        }
        (*s).used_pages = add32((*s).used_pages, k);
        (*s).peak_pages = (*s).peak_pages.max((*s).used_pages);
        (*s).lo = (*s).lo.min(start);
        (*s).hi = (*s).hi.max(end);
        p.in_use = add32(p.in_use, k);
        p.peak_in_use = p.peak_in_use.max(p.in_use);
        true
    }
}

/// Best-fit run of `k` pages held for `id`'s heap inside its span.
fn best_held_run(p: &Pool, id: u16, lo: u32, hi: u32, k: u32) -> Option<u32> {
    let want = HELD | id;
    let (mut best, mut best_len) = (None, u32::MAX);
    let (mut start, mut len) = (lo, 0u32);
    let hi = hi.min(p.region_pages);
    let mut i = lo;
    while i <= hi {
        let held = i < hi && p.own(i) == want;
        if held {
            if len == 0 {
                start = i;
            }
            len += 1;
        } else {
            if len >= k && len < best_len {
                best = Some(start);
                best_len = len;
            }
            len = 0;
        }
        i += 1;
    }
    best
}

/// A segment for scope `id`'s heap. Only pages held for the heap (`HELD |
/// id`) or, as a recorded overdraft, free pages: never a block run or a lent
/// page, so the heap's segment requests cannot take another admission's run.
unsafe fn scope_sys_alloc(id: u16, size: usize) -> (*mut u8, usize, u32) {
    unsafe {
        let p = pool();
        let s = state(id);
        let route = (*s).route;
        let held_by_me = HELD | id;
        // The first segment extends the state page in place when the pages
        // after it are held, so the state page's tail is not wasted.
        if !(*s).first_seg_done {
            let k1 = pages_of(size.saturating_add(STATE_BYTES));
            let more = k1.saturating_sub(1);
            if more == 0 || p.find_run_by(id as u32 + 1, add32(id as u32 + 1, more), more, |_, o| o == held_by_me) == Some(id as u32 + 1) {
                if more > 0 && !take_pages(id, id as u32 + 1, more) {
                    return NULL_SEG;
                }
                (*s).first_seg_done = true;
                (*s).last_end = add32(id as u32 + 1, more);
                let base = p.page_ptr(id as u32).wrapping_add(STATE_BYTES);
                return (base, (more as usize + 1) * PAGE - STATE_BYTES, 0);
            }
        }
        let k = pages_of(size);
        let (lo, hi) = ((*s).lo, (*s).hi);
        // 1. In place after the heap's last segment, all held for the heap:
        //    dlmalloc merges it with the top segment.
        // 2. Best-fit held heap run anywhere in the scope's span.
        // 3. In place, mixing held heap pages and free pages (the free part
        //    overdraws).
        // 4. Any free run (overdraft).
        let last = (*s).last_end;
        let mut plan = None;
        if p.find_run_by(last, add32(last, k).min(p.limit), k, |_, o| o == held_by_me) == Some(last) {
            plan = Some(last);
        } else if let Some(start) = best_held_run(p, id, lo, hi, k) {
            plan = Some(start);
        }
        let start = match plan {
            Some(v) => v,
            None => {
                let mixed = p.find_run_by(last, add32(last, k).min(p.limit), k, |_, o| o == held_by_me || o == FREE);
                let start = match mixed {
                    Some(v) if v == last && last >= p.glue_budget => v,
                    _ => match p.find_free_run(k) {
                        Some(v) => v,
                        None => {
                            // A held run exists only in pieces, or the pool is out
                            // of free pages: nothing can serve it.
                            return NULL_SEG;
                        }
                    },
                };
                let extra = (start..add32(start, k)).filter(|&i| p.own(i) == FREE).count() as u32;
                if extra > 0 {
                    if current::armed() {
                        current::violation(1);
                    }
                    let denied = match p.mode {
                        Mode::Strict => true,
                        Mode::Enforce | Mode::Observe => false,
                    };
                    if denied {
                        inc(&mut p.c.overdrafts_denied);
                        p.pending = Some(FailureKind::OverdraftDenied);
                        return NULL_SEG;
                    }
                    inc(&mut p.c.overdrafts);
                    p.c.overdraft_pages = p.c.overdraft_pages.saturating_add(extra as u64);
                    inc(&mut p.route_mut(route).overdrafts);
                    (*s).overdraft_pages = (*s).overdraft_pages.saturating_add(extra);
                }
                start
            }
        };
        if !take_pages(id, start, k) {
            return NULL_SEG;
        }
        (*s).first_seg_done = true;
        let end = add32(start, k);
        if start == last || end > (*s).last_end {
            (*s).last_end = end;
        }
        (p.page_ptr(start), k as usize * PAGE, 0)
    }
}

/// Where a page released by scope `id` goes: back to the parent it was carved
/// from, as lent pages (while that parent is open or abandoned), else to the
/// free pool.
unsafe fn release_page(id: u16, page: u32) {
    unsafe {
        let p = pool();
        let s = state(id);
        p.set_blk(page, Blk::ZERO);
        let stack = p.carve_at(page);
        if stack & 1 == 1 {
            (*s).carved_pages = (*s).carved_pages.saturating_sub(1);
            let parent = (*s).parent;
            if parent != id && p.is_active(parent, (*s).parent_epoch) {
                let ps = state(parent);
                p.set_carve(page, stack >> 1);
                p.set_own(page, HELD | LEND | parent);
                (*ps).held_pages = add32((*ps).held_pages, 1);
                (*ps).lent_pages = add32((*ps).lent_pages, 1);
                (*ps).reserved_pages = add32((*ps).reserved_pages, 1);
                (*ps).lo = (*ps).lo.min(page);
                (*ps).hi = (*ps).hi.max(add32(page, 1));
                inc(&mut p.c.pages_returned_to_parent);
                return;
            }
        }
        p.set_carve(page, 0);
        p.set_own(page, FREE);
    }
}

/// Returns `[start, start + n)`, used by scope `id`'s heap, to its held heap
/// pages, or to the free pool for the part that was an overdraft.
unsafe fn unuse_pages(id: u16, start: u32, n: u32) {
    unsafe {
        let p = pool();
        let s = state(id);
        let quarantined = (*s).status == QUARANTINED;
        for i in start..add32(start, n) {
            if p.own(i) != id {
                abort();
            }
            if quarantined {
                // A quarantined scope keeps only pages that hold live bytes.
                release_page(id, i);
                (*s).reserved_pages = (*s).reserved_pages.saturating_sub(1);
            } else if (*s).overdraft_now > 0 {
                (*s).overdraft_now -= 1;
                p.set_own(i, FREE);
            } else {
                p.set_own(i, HELD | id);
                (*s).held_pages = add32((*s).held_pages, 1);
            }
        }
        (*s).used_pages = sub32((*s).used_pages, n);
        p.in_use = sub32(p.in_use, n);
        if (*s).last_end == add32(start, n) {
            (*s).last_end = start;
        }
    }
}

unsafe fn sys_free(id: u16, ptr: *mut u8, size: usize) -> bool {
    unsafe {
        let p = pool();
        let (start, n) = p.span(ptr, size);
        if id == GLUE {
            unuse_glue(p, start, n);
            return true;
        }
        if p.closing == id {
            // `destroy` at close: the pages leave the scope entirely.
            for i in start..add32(start, n) {
                if p.own(i) != id {
                    abort();
                }
                if i != id as u32 {
                    release_page(id, i);
                }
            }
            let s = state(id);
            let k = if start <= id as u32 && (id as u32) < add32(start, n) { n - 1 } else { n };
            (*s).used_pages = sub32((*s).used_pages, k);
            p.in_use = sub32(p.in_use, k);
            return true;
        }
        if start == id as u32 {
            // The segment holds the state page; it is released at close.
            return false;
        }
        unuse_pages(id, start, n);
        true
    }
}

unsafe fn sys_free_part(id: u16, ptr: *mut u8, old: usize, new: usize) -> bool {
    unsafe {
        let p = pool();
        let base = p.region.base as usize;
        let cut = (ptr as usize).wrapping_add(new).wrapping_sub(base);
        let end = (ptr as usize).wrapping_add(old).wrapping_sub(base);
        if !cut.is_multiple_of(PAGE) || !end.is_multiple_of(PAGE) || end <= cut {
            return false;
        }
        let (start, n) = ((cut / PAGE) as u32, ((end - cut) / PAGE) as u32);
        if id == GLUE {
            unuse_glue(p, start, n);
            return true;
        }
        if start <= id as u32 {
            return false;
        }
        unuse_pages(id, start, n);
        true
    }
}

// ------------------------------------------------------------------ direct block runs

/// A held block run of scope `id` admitted for exactly `(size, align)`.
fn pending_exact(p: &Pool, id: u16, s: &ScopeState, size: usize, align: usize) -> Option<u32> {
    let want = HELD | BLOCK | id;
    let fits = |i: u32| {
        let b = p.blk(i);
        p.own(i) == want && b.pages > 0 && b.len == size && b.align == align
    };
    if s.last_block < p.region_pages && fits(s.last_block) {
        return Some(s.last_block);
    }
    (s.lo..s.hi.min(p.region_pages)).find(|&i| fits(i))
}

/// The smallest held block run of scope `id` whose capacity holds
/// `(size, align)`: the target of a block's own growth (a JS string's
/// transient `passStringToWasm0` realloc, a vector growing into the larger
/// block admitted for it).
fn pending_fit(p: &Pool, id: u16, s: &ScopeState, size: usize, align: usize) -> Option<u32> {
    let want = HELD | BLOCK | id;
    let mut best: Option<(u32, u32)> = None;
    for i in s.lo..s.hi.min(p.region_pages) {
        let b = p.blk(i);
        if p.own(i) == want && b.pages > 0 && block_capacity(p, i, b.pages, align) >= size && best.is_none_or(|(_, k)| b.pages < k) {
            best = Some((i, b.pages));
        }
    }
    best.map(|(i, _)| i)
}

/// The smallest returned block run of scope `id` (served and freed since its
/// admission) of at least `pages` pages that holds `(len, align)`.
fn returned_block(p: &Pool, id: u16, s: &ScopeState, pages: u32, len: usize, align: usize) -> Option<u32> {
    if s.pending_blocks == 0 {
        return None;
    }
    let want = HELD | BLOCK | id;
    let mut best: Option<(u32, u32)> = None;
    for i in s.lo..s.hi.min(p.region_pages) {
        let b = p.blk(i);
        if p.own(i) == want
            && b.pages >= pages
            && b.returned
            && block_capacity(p, i, b.pages, align) >= len
            && best.is_none_or(|(_, k)| b.pages < k)
        {
            best = Some((i, b.pages));
        }
    }
    best.map(|(i, _)| i)
}

/// Bytes an allocation at `align` can take in a run of `pages` at `start`.
fn block_capacity(p: &Pool, start: u32, pages: u32, align: usize) -> usize {
    let base = p.page_ptr(start) as usize;
    let off = align_up(base, align.max(1)) - base;
    (pages as usize * PAGE).saturating_sub(off)
}

/// Puts the held block run at `start` into use for one allocation.
unsafe fn serve_block(id: u16, start: u32, size: usize, align: usize, zero: bool) -> *mut u8 {
    unsafe {
        let p = pool();
        let s = state(id);
        let k = p.blk(start).pages;
        for i in start..add32(start, k) {
            if p.own(i) != (HELD | BLOCK | id) {
                abort();
            }
            p.set_own(i, BLOCK | id);
        }
        (*s).held_pages = sub32((*s).held_pages, k);
        (*s).used_pages = add32((*s).used_pages, k);
        (*s).peak_pages = (*s).peak_pages.max((*s).used_pages);
        (*s).pending_blocks = sub32((*s).pending_blocks, 1);
        if (*s).pending_blocks == 0 {
            (*s).pending_min = usize::MAX;
        }
        p.in_use = add32(p.in_use, k);
        p.peak_in_use = p.peak_in_use.max(p.in_use);
        inc(&mut p.c.blocks_served);
        let base = p.page_ptr(start) as usize;
        let q = p.page_ptr(start).wrapping_add(align_up(base, align.max(1)) - base);
        if zero {
            ptr::write_bytes(q, 0, size);
        }
        q
    }
}

/// The first page of the in-use block run of scope `id` holding `page`.
fn block_start(p: &Pool, id: u16, page: u32) -> u32 {
    let mut j = page;
    loop {
        if p.own(j) != (BLOCK | id) {
            abort();
        }
        if p.blk(j).pages > 0 {
            return j;
        }
        if j == 0 {
            abort();
        }
        j -= 1;
    }
}

/// Returns an in-use block run to its scope: held again for the same layout,
/// or released when the scope is quarantined (then the scope is released at
/// its last live byte, by the caller).
unsafe fn release_block(id: u16, start: u32) {
    unsafe {
        let p = pool();
        let s = state(id);
        let b = p.blk(start);
        let quarantined = (*s).status == QUARANTINED;
        for i in start..add32(start, b.pages) {
            if p.own(i) != (BLOCK | id) {
                abort();
            }
            if quarantined {
                release_page(id, i);
                (*s).reserved_pages = (*s).reserved_pages.saturating_sub(1);
            } else {
                p.set_own(i, HELD | BLOCK | id);
            }
        }
        (*s).used_pages = sub32((*s).used_pages, b.pages);
        p.in_use = sub32(p.in_use, b.pages);
        inc(&mut p.c.blocks_returned);
        if !quarantined {
            (*s).held_pages = add32((*s).held_pages, b.pages);
            (*s).pending_blocks = add32((*s).pending_blocks, 1);
            (*s).pending_min = (*s).pending_min.min(b.len);
            (*s).last_block = start;
            p.set_blk(start, Blk { returned: true, ..b });
        }
    }
}

// ------------------------------------------------------------------ scopes

/// Whether a page can be taken into a hold for a scope whose parent is
/// `from`: free, or lent by the parent with room left in its carve stack.
fn holdable(p: &Pool, i: u32, from: u16) -> bool {
    match p.own(i) {
        FREE => true,
        o => from != NONE && o == (HELD | LEND | from) && p.carve_at(i) & (1 << 31) == 0,
    }
}

/// Holds `[start, start + k)` for scope `id` as `hold`, taking free pages
/// and pages lent by `from` (its parent, when carving). Commits the memory so
/// an allocation inside the reservation never needs a `memory.grow`.
unsafe fn hold_run(id: u16, start: u32, k: u32, from: u16, hold: Hold) -> bool {
    unsafe {
        let p = pool();
        let end = add32(start, k);
        if !p.commit(end) {
            return false;
        }
        let s = state(id);
        let tag = hold.tag(id);
        for i in start..end {
            if !holdable(p, i, from) {
                abort();
            }
            if p.own(i) == FREE {
                p.set_carve(i, 0);
            } else {
                let ps = state(from);
                (*ps).held_pages = sub32((*ps).held_pages, 1);
                (*ps).lent_pages = sub32((*ps).lent_pages, 1);
                (*ps).reserved_pages = sub32((*ps).reserved_pages, 1);
                p.set_carve(i, (p.carve_at(i) << 1) | 1);
                (*s).carved_pages = add32((*s).carved_pages, 1);
            }
            p.set_own(i, tag);
            p.set_blk(i, Blk::ZERO);
        }
        match hold {
            Hold::Block { len, align } => {
                p.set_blk(start, Blk { len, align, pages: k, returned: false });
                (*s).pending_blocks = add32((*s).pending_blocks, 1);
                (*s).pending_min = (*s).pending_min.min(len);
                (*s).last_block = start;
            }
            Hold::Lend => (*s).lent_pages = add32((*s).lent_pages, k),
            Hold::Heap => {}
        }
        (*s).held_pages = add32((*s).held_pages, k);
        (*s).reserved_pages = add32((*s).reserved_pages, k);
        (*s).lo = (*s).lo.min(start);
        (*s).hi = (*s).hi.max(end);
        true
    }
}

/// A run of `k` pages for a hold under `parent` (if any): first wholly inside
/// the parent's lent pages, then mixing them with free pages (the parent's
/// lent slack covers part, the pool the difference), then in place at one of
/// `after` (the heap's end, then the scope's span end) when free, then any
/// free run. A child never carves its parent's heap pages or blocks.
fn find_hold_run(p: &Pool, parent: u16, after: [Option<u32>; 2], k: u32) -> Option<u32> {
    let (lo, hi) = (p.glue_budget, p.limit);
    if parent != NONE && p.is_state_page(parent as u32) {
        let ps = state_ref(p, parent);
        let want = HELD | LEND | parent;
        if ps.lent_pages > 0 {
            if let Some(s) = p.find_run_by(ps.lo.max(lo), ps.hi.min(hi), k, |i, o| o == want && holdable(p, i, parent)) {
                return Some(s);
            }
            if let Some(s) = p.find_run_by(lo, hi, k, |i, o| (o == FREE || o == want) && holdable(p, i, parent)) {
                return Some(s);
            }
        }
    }
    for a in after.into_iter().flatten() {
        if a >= lo && p.find_run_by(a, a.saturating_add(k).min(hi), k, |_, o| o == FREE) == Some(a) {
            return Some(a);
        }
    }
    p.find_free_run(k)
}

/// Opens a scope: admission (a contiguous run of state plus control pages,
/// physically held), then its state. Allocation-free. A child's run is carved
/// from its parent's lent pages first (and goes back to them at close); it
/// inherits the parent's absolute deadline, which the caller replaces when
/// the child's route declares its own.
pub(crate) fn open(
    route: u8,
    name: &'static str,
    footprint_bytes: usize,
    parent: Option<(u16, u64)>,
    physical: bool,
) -> Result<(u16, u64), Refusal> {
    let _g = lock::hold();
    unsafe {
        if !ensure_init() {
            return Err(Refusal::new(route, RefusalKind::Capacity, Reason::Platform, 0, 0));
        }
        let p = pool();
        let need = scope_pages(block_or_zero(footprint_bytes));
        let (pid, parent_epoch, deadline) = match parent {
            Some((pid, pepoch)) => {
                if !p.is_active(pid, pepoch) {
                    return Err(p.stale(route));
                }
                (pid, pepoch, state_ref(p, pid).deadline_ms)
            }
            None => (NONE, 0, 0),
        };
        // A physical admission (`ScopeRef::child_physical`) is refused in
        // every mode when the pool could not serve the footprint even
        // scattered: its free pages and the parent's unused lent pages are
        // fewer than it needs. `Observe` admits everything else, so its
        // excess stays visible as overdrafts, but never an allocation the
        // pool cannot physically back (which would trap the instance).
        if physical {
            let lent = if pid == NONE { 0 } else { state_ref(p, pid).lent_pages };
            if p.free_count.saturating_add(lent) < need {
                return Err(p.classify(route, need, pid, pid));
            }
        }
        let (start, k) = match find_hold_run(p, pid, [None, None], need) {
            Some(s) => (s, need),
            None if p.mode == Mode::Observe => {
                // Record the would-be refusal and admit with the state page
                // alone, so the excess stays visible as overdrafts.
                inc(&mut p.c.observed_refusals);
                p.c.observed_excess_pages = p.c.observed_excess_pages.saturating_add(need.saturating_sub(1) as u64);
                match find_hold_run(p, pid, [None, None], 1) {
                    Some(s) => (s, 1),
                    None => return Err(p.classify(route, need, pid, pid)),
                }
            }
            None => return Err(p.classify(route, need, pid, pid)),
        };
        let id = start as u16;
        // The state page must be writable before the state is.
        if !p.commit(add32(start, k)) {
            p.fail(FailureKind::Platform, id, route, PAGE, PAGE);
            return Err(Refusal::new(route, RefusalKind::Retry, Reason::Platform, need as usize * PAGE, 0));
        }
        p.epoch = p.epoch.saturating_add(1);
        let epoch = p.epoch;
        ptr::write(
            state(id),
            ScopeState {
                heap: Dlmalloc::new_with_allocator(PageSource { id }),
                epoch,
                parent_epoch,
                route,
                status: OPEN,
                polling: 0,
                first_seg_done: false,
                bound_exceeded: false,
                permanent: false,
                parent: pid,
                carved_pages: 0,
                reserved_bytes: footprint_bytes,
                reserved_pages: 0,
                used_pages: 0,
                held_pages: 0,
                lent_pages: 0,
                overdraft_now: 0,
                overdraft_pages: 0,
                peak_pages: 1,
                pending_blocks: 0,
                pending_min: usize::MAX,
                last_block: u32::MAX,
                last_end: start + 1,
                lo: start,
                hi: start + 1,
                live: 0,
                peak_live: 0,
                allocs: 0,
                deadline_ms: deadline,
                children: 0,
            },
        );
        if !hold_run(id, start, k, pid, Hold::Heap) {
            abort();
        }
        // The state page is in use from the start.
        let s = state(id);
        p.set_own(start, id);
        (*s).held_pages = sub32((*s).held_pages, 1);
        (*s).used_pages = 1;
        p.in_use = add32(p.in_use, 1);
        p.peak_in_use = p.peak_in_use.max(p.in_use);
        if pid != NONE {
            let ps = state(pid);
            (*ps).children = add32((*ps).children, 1);
            if (*s).carved_pages > 0 {
                inc(&mut p.c.children_carved);
            } else {
                inc(&mut p.c.children_pooled);
            }
        }
        inc(&mut p.c.scopes_opened);
        let r = p.route_mut(route);
        r.name = name;
        inc(&mut r.opened);
        r.max_reserved_bytes = r.max_reserved_bytes.max(footprint_bytes);
        p.check_route_bound(id);
        Ok((id, epoch))
    }
}

/// `block_bytes`-style rounding for a heap footprint; zero stays one page of
/// heap so a scope always has a first segment.
pub(crate) const fn block_or_zero(bytes: usize) -> usize {
    align_up(bytes.saturating_add(SEGMENT_OVERHEAD), PAGE)
}

/// Sets a scope's abandonment deadline: `clock + after_ms` (0: never).
/// Ignored for a handle whose scope is gone.
pub(crate) fn set_deadline(id: u16, epoch: u64, after_ms: u64) {
    let _g = lock::hold();
    unsafe {
        let p = pool();
        if !p.is_active(id, epoch) {
            return;
        }
        let s = state(id);
        (*s).deadline_ms = if after_ms == 0 { 0 } else { p.clock_ms.saturating_add(after_ms).max(1) };
    }
}

/// Marks a scope permanent (an instance-lifetime home scope): refusals its
/// pages cause are classed `Capacity`, never `Retry`. Ignored for a handle
/// whose scope is gone.
pub(crate) fn set_permanent(id: u16, epoch: u64) {
    let _g = lock::hold();
    unsafe {
        let p = pool();
        if p.is_active(id, epoch) {
            (*state(id)).permanent = true;
        }
    }
}

/// Whether a handle still names an open (or abandoned) scope; counts a stale
/// entry. Allocation-free.
pub(crate) fn check_enter(id: u16, epoch: u64) -> bool {
    let _g = lock::hold();
    unsafe {
        let p = pool();
        let ok = p.ready && p.is_active(id, epoch);
        if !ok {
            inc(&mut p.c.stale_enters);
        }
        ok
    }
}

/// Grows a scope's reservation by one contiguous run of `pages`, physically
/// held as `hold`: from the parent's lent pages for a child (charged against
/// the parent's reservation), else in place after the heap for heap holds,
/// else any free run. Allocation-free; on refusal nothing changes.
pub(crate) fn reserve(id: u16, epoch: u64, route_id: u8, bytes: usize, pages: u32, hold: Hold) -> Result<(), Refusal> {
    let _g = lock::hold();
    unsafe {
        let p = pool();
        if !p.ready || !p.is_active(id, epoch) {
            return Err(p.stale(route_id));
        }
        let s = state(id);
        let route = (*s).route;
        if pages == 0 {
            return Ok(());
        }
        // A block admission first re-keys a run this scope already holds
        // that was served and freed (the smallest that holds it), so a scope
        // that reads many objects one after another holds as many runs as it
        // has blocks live at once, never one per object.
        if let Hold::Block { len, align } = hold
            && let Some(start) = returned_block(p, id, &*s, pages, len, align)
        {
            let b = p.blk(start);
            p.set_blk(start, Blk { len, align, pages: b.pages, returned: false });
            (*s).pending_min = (*s).pending_min.min(len);
            (*s).last_block = start;
            inc(&mut p.c.blocks_reused);
            return Ok(());
        }
        let parent = p.parent_of(id);
        let after = match hold {
            Hold::Heap => [Some((*s).last_end), Some((*s).hi)],
            Hold::Block { .. } | Hold::Lend => [None, None],
        };
        let start = match find_hold_run(p, parent, after, pages) {
            Some(v) => v,
            None => {
                if p.mode == Mode::Observe {
                    inc(&mut p.c.observed_refusals);
                    p.c.observed_excess_pages = p.c.observed_excess_pages.saturating_add(pages as u64);
                    return Ok(());
                }
                return Err(p.classify(route, pages, id, parent));
            }
        };
        if !hold_run(id, start, pages, parent, hold) {
            return Err(Refusal::new(route, RefusalKind::Retry, Reason::Platform, pages as usize * PAGE, 0));
        }
        (*s).reserved_bytes = (*s).reserved_bytes.saturating_add(bytes);
        let total = (*s).reserved_bytes;
        let r = p.route_mut(route);
        r.max_reserved_bytes = r.max_reserved_bytes.max(total);
        p.check_route_bound(id);
        Ok(())
    }
}

/// Releases a scope whose live bytes are zero: every segment, every held
/// page, then the state page. Caller holds the lock.
unsafe fn release_scope(id: u16) {
    unsafe {
        let p = pool();
        let s = state(id);
        let route = (*s).route;
        let peak_live = (*s).peak_live;
        let peak_pages = (*s).peak_pages;
        let (lo, hi) = ((*s).lo, (*s).hi);
        p.closing = id;
        let heap = ptr::read(&raw const (*s).heap);
        heap.destroy();
        p.closing = NONE;
        // Held pages of every kind (a used heap page or block dlmalloc did not
        // hand back would be a bug: abort).
        for i in lo..hi.min(p.region_pages) {
            if i == id as u32 {
                continue;
            }
            let o = p.own(i);
            if scope_of(o) == Some(id) {
                if o & HELD != 0 {
                    release_page(id, i);
                } else {
                    abort();
                }
            }
        }
        let parent = (*s).parent;
        let parent_epoch = (*s).parent_epoch;
        let used = (*s).used_pages;
        // Only the state page may remain in use.
        if used != 1 {
            abort();
        }
        p.in_use = sub32(p.in_use, 1);
        // The state page goes last: release_page reads the state.
        release_page(id, id as u32);
        if p.is_scope(parent, parent_epoch) {
            let ps = state(parent);
            (*ps).children = (*ps).children.saturating_sub(1);
        }
        inc(&mut p.c.scopes_released);
        let r = p.route_mut(route);
        inc(&mut r.released);
        r.peak_live = r.peak_live.max(peak_live);
        r.peak_pages = r.peak_pages.max(peak_pages);
    }
}

/// Closes a scope: released if nothing it allocated is still live, else
/// trimmed and quarantined until its last value is freed. Allocation-free.
pub(crate) fn close(id: u16, epoch: u64) -> Closed {
    let _g = lock::hold();
    unsafe {
        let p = pool();
        // Only the owning `Scope` closes, once: anything else is a bug.
        if !p.is_scope(id, epoch) || state_ref(p, id).status == QUARANTINED {
            abort();
        }
        let s = state(id);
        let was_abandoned = (*s).status == ABANDONED;
        if was_abandoned {
            // The future was dropped after its deadline: reclaimed.
            inc(&mut p.c.abandon_reclaimed);
        }
        let (live, peak_live, peak_pages) = ((*s).live, (*s).peak_live, (*s).peak_pages);
        if live == 0 {
            let used = (*s).used_pages;
            release_scope(id);
            return Closed::Released { pages: used, peak_live, peak_pages };
        }
        // Keep only the pages that still hold live bytes: trim the heap's top
        // and unused segments, then return every held page of every kind.
        (*s).heap.trim(0);
        let (lo, hi) = ((*s).lo, (*s).hi);
        for i in lo..hi.min(p.region_pages) {
            let o = p.own(i);
            if o & HELD != 0 && scope_of(o) == Some(id) {
                release_page(id, i);
                (*s).held_pages = sub32((*s).held_pages, 1);
            }
        }
        (*s).status = QUARANTINED;
        (*s).reserved_pages = (*s).used_pages;
        (*s).overdraft_now = 0;
        (*s).reserved_bytes = 0;
        (*s).lent_pages = 0;
        (*s).pending_blocks = 0;
        (*s).pending_min = usize::MAX;
        inc(&mut p.c.quarantines);
        let route = (*s).route;
        let used = (*s).used_pages;
        let r = p.route_mut(route);
        inc(&mut r.quarantined);
        r.peak_live = r.peak_live.max(peak_live);
        r.peak_pages = r.peak_pages.max(peak_pages);
        if current::armed() {
            current::violation(2);
        }
        Closed::Quarantined { live, pages: used }
    }
}

/// Marks the start of a poll of scope `id`. A scope found abandoned is
/// recovered: its awaited promise settled late. Allocation-free; a stale
/// handle marks nothing (its allocations are refused).
pub(crate) fn poll_begin(id: u16, epoch: u64) {
    let _g = lock::hold();
    unsafe {
        let p = pool();
        if !p.is_active(id, epoch) {
            inc(&mut p.c.stale_enters);
            return;
        }
        let s = state(id);
        (*s).polling = (*s).polling.saturating_add(1);
        if (*s).status == ABANDONED {
            (*s).status = OPEN;
            inc(&mut p.c.abandon_recovered);
        }
    }
}

pub(crate) fn poll_end(id: u16, epoch: u64) {
    let _g = lock::hold();
    unsafe {
        let p = pool();
        if p.is_scope(id, epoch) {
            let s = state(id);
            (*s).polling = (*s).polling.saturating_sub(1);
        }
    }
}

pub(crate) fn scope_stats(id: u16, epoch: u64) -> ScopeStats {
    let _g = lock::hold();
    unsafe {
        let p = pool();
        if !p.is_scope(id, epoch) {
            return ScopeStats::gone(id, epoch);
        }
        let s = state_ref(p, id);
        ScopeStats {
            id,
            epoch,
            route: s.route,
            status: s.status,
            reserved_bytes: s.reserved_bytes,
            reserved_pages: s.reserved_pages,
            used_pages: s.used_pages,
            held_pages: s.held_pages,
            lent_pages: s.lent_pages,
            peak_pages: s.peak_pages,
            live: s.live,
            peak_live: s.peak_live,
            allocs: s.allocs,
            overdraft_pages: s.overdraft_pages,
            children: s.children,
            carved_pages: s.carved_pages,
            pending_blocks: s.pending_blocks,
            deadline_ms: s.deadline_ms,
        }
    }
}

/// Status and pages (used plus held) of the scope whose state is on `page`.
pub(crate) unsafe fn scope_at(page: u32) -> Option<(u8, u32)> {
    unsafe {
        let p = pool();
        if !p.is_state_page(page) {
            return None;
        }
        let s = state_ref(p, page as u16);
        Some((s.status, s.used_pages.saturating_add(s.held_pages)))
    }
}

/// The route of the scope `(id, epoch)` names, if it is still live (takes
/// the pool lock).
pub(crate) fn current_route(id: u16, epoch: u64) -> Option<u8> {
    let _g = lock::hold();
    unsafe { route_of(id, epoch) }
}

/// The route of the scope `(id, epoch)` names, if it is still live.
pub(crate) unsafe fn route_of(id: u16, epoch: u64) -> Option<u8> {
    unsafe {
        let p = pool();
        (p.ready && p.is_scope(id, epoch)).then(|| state_ref(p, id).route)
    }
}

// ------------------------------------------------------------------ glue tickets

/// Admits one in-flight entry's glue: allocation-free; refused once the
/// model's ticket count is in flight.
pub(crate) fn ticket_acquire() -> Result<(), Refusal> {
    let _g = lock::hold();
    unsafe {
        if !ensure_init() {
            return Err(Refusal::new(0, RefusalKind::Capacity, Reason::Platform, 0, 0));
        }
        let p = pool();
        if p.tickets >= model::GLUE_TICKETS {
            inc(&mut p.c.tickets_refused);
            inc(&mut p.c.refusals_retry);
            return Err(Refusal::new(0, RefusalKind::Retry, Reason::Glue, model::GLUE_TICKET_BYTES, 0));
        }
        p.tickets = add32(p.tickets, 1);
        p.tickets_peak = p.tickets_peak.max(p.tickets);
        inc(&mut p.c.tickets_issued);
        Ok(())
    }
}

pub(crate) fn ticket_release() {
    let _g = lock::hold();
    unsafe {
        let p = pool();
        p.tickets = p.tickets.saturating_sub(1);
    }
}

// ------------------------------------------------------------------ test and drill knobs

/// Sets the enforcement mode for the whole pool. Test and drill builds only;
/// production builds fix the mode by feature.
#[cfg(feature = "knobs")]
// scan-ok: the knob's definition, gated by `knobs`.
pub fn set_mode(m: Mode) -> Mode {
    let _g = lock::hold();
    unsafe {
        ensure_init();
        core::mem::replace(&mut pool().mode, m)
    }
}

pub fn mode() -> Mode {
    let _g = lock::hold();
    unsafe { pool().mode }
}

/// Usable pool pages (glue partition included). Starts at the model's ceiling.
pub fn page_limit() -> u32 {
    let _g = lock::hold();
    unsafe {
        ensure_init();
        pool().limit
    }
}

/// Lowers (or restores, up to the model's ceiling) the usable pool. Refuses
/// when a page at or above `pages` is not free, or `pages` would not leave the
/// glue partition. Returns the previous limit. Test and drill builds only.
#[cfg(feature = "knobs")]
// scan-ok: the knob's definition, gated by `knobs`.
pub fn set_page_limit(pages: u32) -> Result<u32, u32> {
    let _g = lock::hold();
    unsafe {
        if !ensure_init() {
            return Err(0);
        }
        let p = pool();
        if pages > p.region_pages || pages <= p.glue_budget {
            return Err(p.limit);
        }
        if pages < p.limit {
            if (pages..p.limit).any(|i| p.own(i) != FREE) {
                return Err(p.limit);
            }
            for i in pages..p.limit {
                p.set_own(i, OUT);
            }
        } else {
            for i in p.limit..pages {
                if p.own(i) != OUT {
                    abort();
                }
                p.set_own(i, FREE);
            }
        }
        Ok(core::mem::replace(&mut p.limit, pages))
    }
}

/// Lowers the pool to the smallest limit at which `free` scope pages are
/// free and every page above the limit is free. Returns the free pages left.
/// Test and drill builds only.
#[cfg(feature = "knobs")]
// scan-ok: the knob's definition, gated by `knobs`.
pub fn shrink_free_to(free: u32) -> Result<u32, u32> {
    let _g = lock::hold();
    unsafe {
        if !ensure_init() {
            return Err(0);
        }
        let p = pool();
        let mut top = p.glue_budget;
        for i in p.glue_budget..p.limit {
            if p.own(i) != FREE {
                top = i + 1;
            }
        }
        let (mut lim, mut n) = (p.glue_budget, 0u32);
        while lim < p.limit && (n < free || lim < top) {
            if p.own(lim) == FREE {
                n += 1;
            }
            lim += 1;
        }
        for i in lim..p.limit {
            p.set_own(i, OUT);
        }
        p.limit = lim;
        Ok(p.free_count)
    }
}

/// Clears the poisoned-refusal latch and the glue peak (test knob: a real
/// instance clears them only by recycling).
#[cfg(feature = "knobs")]
// scan-ok: the knob's definition, gated by `knobs`.
pub fn reset_latches() {
    let _g = lock::hold();
    unsafe {
        let p = pool();
        p.poisoned_refusals = 0;
        p.poison_latch = None;
        p.c.glue_peak = p.c.glue_live;
        p.c.glue_fp_peak = p.c.glue_fp_live;
        p.tickets_peak = p.tickets;
    }
}

/// The arena owning the page that holds `ptr`, if the pool owns it.
pub fn owner_of<T: ?Sized>(ptr: *const T) -> Option<Owner> {
    let _g = lock::hold();
    unsafe {
        let p = pool();
        let page = p.page_of(ptr.cast())?;
        match p.own(page) {
            FREE | GLUE_FREE | OUT => None,
            GLUE => Some(Owner::Glue),
            o if o & HELD != 0 => None,
            o => Some(Owner::Scope(o & ID)),
        }
    }
}

// ------------------------------------------------------------------ global allocator

/// The global allocator: every heap byte lives in a scope arena or in the
/// glue partition.
///
/// ```ignore
/// #[global_allocator]
/// static ALLOC: slop_ninja_arena::ArenaAlloc = slop_ninja_arena::ArenaAlloc;
/// ```
pub struct ArenaAlloc;

unsafe impl GlobalAlloc for ArenaAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        unsafe { raw_alloc(l, false) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        unsafe { raw_alloc(l, true) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, l: Layout) {
        unsafe { raw_dealloc(ptr, l) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, l: Layout, new: usize) -> *mut u8 {
        unsafe { raw_realloc(ptr, l, new) }
    }
}

/// Records an allocation outside every scope; returns whether to refuse it.
unsafe fn unscoped(size: usize) -> bool {
    unsafe {
        let p = pool();
        inc(&mut p.c.unscoped_allocs);
        p.c.unscoped_bytes = p.c.unscoped_bytes.saturating_add(size as u64);
        if current::armed() {
            current::violation(0);
            inc(&mut p.c.armed_unscoped);
            if p.mode == Mode::Strict {
                inc(&mut p.c.unscoped_denied);
                return true;
            }
        }
        false
    }
}

unsafe fn raw_alloc(l: Layout, zero: bool) -> *mut u8 {
    let _g = lock::hold();
    unsafe {
        if !ensure_init() {
            return ptr::null_mut();
        }
        let (size, align) = (l.size(), l.align());
        let (cur, epoch) = current::get();
        if cur == NONE {
            if unscoped(size) {
                pool().fail(FailureKind::UnscopedDenied, GLUE, 0, size, align);
                return ptr::null_mut();
            }
            let h = &mut *GLUE_HEAP.get();
            let q = if zero { h.calloc(size, align) } else { h.malloc(size, align) };
            let p = pool();
            if q.is_null() {
                let kind = p.pending.take().unwrap_or(FailureKind::GlueExhausted);
                p.fail(kind, GLUE, 0, size, align);
                return q;
            }
            inc(&mut p.c.allocs);
            p.c.glue_live = p.c.glue_live.saturating_add(size);
            p.c.glue_peak = p.c.glue_peak.max(p.c.glue_live);
            p.c.glue_fp_live = p.c.glue_fp_live.saturating_add(aligned_chunk_bytes(size, align));
            p.c.glue_fp_peak = p.c.glue_fp_peak.max(p.c.glue_fp_live);
            inc(&mut p.c.glue_allocs_live);
            return q;
        }
        let p = pool();
        if !p.is_active(cur, epoch) {
            // A stale handle (its scope closed, its page possibly reused by
            // another scope) or a quarantined scope: never write into it.
            // The page may now hold an unrelated scope: charge no route.
            inc(&mut p.c.stale_allocs);
            inc(&mut p.c.alloc_failures);
            p.c.last_failure = Failure {
                kind: FailureKind::StaleScope,
                glue: false,
                route: 0,
                size,
                align,
                in_use_pages: p.in_use,
                free_pages: p.free_count,
                limit_pages: p.limit,
            };
            return ptr::null_mut();
        }
        let s = state(cur);
        // An admitted block of exactly this layout is served from its run.
        if (*s).pending_blocks > 0
            && size >= (*s).pending_min
            && let Some(start) = pending_exact(p, cur, &*s, size, align)
        {
            let q = serve_block(cur, start, size, align, zero);
            let p = pool();
            inc(&mut p.c.allocs);
            inc(&mut (*s).allocs);
            (*s).live = addz((*s).live, size);
            (*s).peak_live = (*s).peak_live.max((*s).live);
            return q;
        }
        let heap = &mut (*s).heap;
        let q = if zero { heap.calloc(size, align) } else { heap.malloc(size, align) };
        let p = pool();
        if q.is_null() {
            let kind = p.pending.take().unwrap_or(FailureKind::PoolExhausted);
            p.fail(kind, cur, (*s).route, size, align);
            return q;
        }
        inc(&mut p.c.allocs);
        inc(&mut (*s).allocs);
        (*s).live = addz((*s).live, size);
        (*s).peak_live = (*s).peak_live.max((*s).live);
        q
    }
}

/// The owner of `ptr`'s page: `GLUE`, or `(id, is_block)`.
unsafe fn owner_or_abort(ptr: *mut u8) -> (u16, bool) {
    let p = unsafe { pool() };
    match p.page_of(ptr) {
        Some(page) => match p.own(page) {
            FREE | GLUE_FREE | OUT => abort(),
            GLUE => (GLUE, false),
            o if o & HELD != 0 => abort(),
            o => {
                // A pointer into a scope's heap segment or block run; the
                // scope's state is on its id page.
                let id = o & ID;
                if !p.is_state_page(id as u32) {
                    abort();
                }
                (id, o & BLOCK != 0)
            }
        },
        None => abort(),
    }
}

unsafe fn raw_dealloc(ptr: *mut u8, l: Layout) {
    let _g = lock::hold();
    unsafe {
        let (id, is_block) = owner_or_abort(ptr);
        let p = pool();
        inc(&mut p.c.frees);
        if id == GLUE {
            (*GLUE_HEAP.get()).free(ptr, l.size(), l.align());
            if pool().glue_spill > 0 {
                // Spilled glue pages go back to the free pool as soon as the
                // glue heap no longer uses them (dlmalloc would otherwise keep
                // its freed top segment).
                (*GLUE_HEAP.get()).trim(0);
            }
            let p = pool();
            p.c.glue_live = subz(p.c.glue_live, l.size());
            p.c.glue_fp_live = p.c.glue_fp_live.saturating_sub(aligned_chunk_bytes(l.size(), l.align()));
            p.c.glue_allocs_live = p.c.glue_allocs_live.saturating_sub(1);
            return;
        }
        let s = state(id);
        if is_block {
            let Some(page) = p.page_of(ptr) else { abort() };
            let start = block_start(p, id, page);
            release_block(id, start);
        } else {
            (*s).heap.free(ptr, l.size(), l.align());
        }
        (*s).live = subz((*s).live, l.size());
        if (*s).status == QUARANTINED && (*s).live == 0 {
            inc(&mut pool().c.quarantine_releases);
            release_scope(id);
        }
    }
}

unsafe fn raw_realloc(ptr: *mut u8, l: Layout, new: usize) -> *mut u8 {
    let _g = lock::hold();
    unsafe {
        let (id, is_block) = owner_or_abort(ptr);
        let (old, align) = (l.size(), l.align());
        // A value keeps its owner: growth is charged to the arena that holds it.
        if id == GLUE {
            if new > old && unscoped(new - old) {
                pool().fail(FailureKind::UnscopedDenied, GLUE, 0, new, align);
                return ptr::null_mut();
            }
            let q = (*GLUE_HEAP.get()).realloc(ptr, old, align, new);
            let p = pool();
            if q.is_null() {
                let kind = p.pending.take().unwrap_or(FailureKind::GlueExhausted);
                p.fail(kind, GLUE, 0, new, align);
                return q;
            }
            inc(&mut p.c.reallocs);
            p.c.glue_live = addz(subz(p.c.glue_live, old), new);
            p.c.glue_peak = p.c.glue_peak.max(p.c.glue_live);
            p.c.glue_fp_live = p.c.glue_fp_live.saturating_sub(aligned_chunk_bytes(old, align)).saturating_add(aligned_chunk_bytes(new, align));
            p.c.glue_fp_peak = p.c.glue_fp_peak.max(p.c.glue_fp_live);
            return q;
        }
        let p = pool();
        let s = state(id);
        let q = if is_block {
            let Some(page) = p.page_of(ptr) else { abort() };
            let start = block_start(p, id, page);
            // In-place capacity: the run's bytes from `ptr` to its end.
            let cap = (p.blk(start).pages as usize * PAGE).saturating_sub(ptr as usize - p.page_ptr(start) as usize);
            if new <= cap {
                // In place inside its own run.
                ptr
            } else {
                // Its exactly admitted successor, else the smallest admitted
                // block that holds it, else the heap (an overdraft in Strict
                // terms if nothing admitted it).
                let target = pending_exact(p, id, &*s, new, align).or_else(|| pending_fit(p, id, &*s, new, align));
                let q = match target {
                    Some(t) => serve_block(id, t, new, align, false),
                    None => (*s).heap.malloc(new, align),
                };
                if q.is_null() {
                    let p = pool();
                    let kind = p.pending.take().unwrap_or(FailureKind::PoolExhausted);
                    p.fail(kind, id, (*s).route, new, align);
                    return q;
                }
                ptr::copy_nonoverlapping(ptr, q, old.min(new));
                release_block(id, start);
                if target.is_some() {
                    inc(&mut pool().c.block_moves);
                }
                q
            }
        } else if new > old
            && (*s).pending_blocks > 0
            && new >= (*s).pending_min
            && let Some(t) = pending_exact(p, id, &*s, new, align)
        {
            // Growth admitted as a block of exactly the new size moves there.
            let q = serve_block(id, t, new, align, false);
            ptr::copy_nonoverlapping(ptr, q, old);
            (*s).heap.free(ptr, old, align);
            inc(&mut pool().c.block_moves);
            q
        } else {
            let q = (*s).heap.realloc(ptr, old, align, new);
            if q.is_null() {
                let p = pool();
                let kind = p.pending.take().unwrap_or(FailureKind::PoolExhausted);
                p.fail(kind, id, (*s).route, new, align);
                return q;
            }
            q
        };
        inc(&mut pool().c.reallocs);
        (*s).live = addz(subz((*s).live, old), new);
        (*s).peak_live = (*s).peak_live.max((*s).live);
        q
    }
}

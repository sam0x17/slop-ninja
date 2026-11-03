//! Counters and exhaustion reports (design §2.8). Fixed static tables, read
//! under the pool lock into `Copy` snapshots; writing them never allocates.

use core::fmt;

use crate::model;
use crate::pool::{self, ABANDONED, Health, Mode, OPEN, PAGE, PinKind, PoisonCause, QUARANTINED};
use crate::sys::{self, current, lock};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FailureKind {
    #[default]
    None,
    /// No held run and no free pages left for the segment.
    PoolExhausted,
    /// The glue partition has no run for the segment.
    GlueExhausted,
    /// An overdraft refused (strict mode).
    OverdraftDenied,
    /// An allocation outside every scope on an armed thread, in strict mode.
    UnscopedDenied,
    /// `memory.grow` (or the native region) failed.
    Platform,
    /// An allocation through a handle whose scope is closed (its page may
    /// belong to a later scope) or quarantined: refused, never misrouted.
    StaleScope,
}

/// The last allocation the allocator could not serve. Readable after a
/// trap: it lives in static memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub glue: bool,
    pub route: u8,
    pub size: usize,
    pub align: usize,
    pub in_use_pages: u32,
    pub free_pages: u32,
    pub limit_pages: u32,
}

impl Failure {
    const ZERO: Failure = Failure {
        kind: FailureKind::None,
        glue: false,
        route: 0,
        size: 0,
        align: 0,
        in_use_pages: 0,
        free_pages: 0,
        limit_pages: 0,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Counters {
    pub allocs: u64,
    pub frees: u64,
    pub reallocs: u64,
    pub scopes_opened: u64,
    pub scopes_released: u64,
    pub quarantines: u64,
    pub quarantine_releases: u64,
    pub refusals_retry: u64,
    pub refusals_capacity: u64,
    pub refusals_poisoned: u64,
    pub observed_refusals: u64,
    pub observed_excess_pages: u64,
    pub overdrafts: u64,
    pub overdraft_pages: u64,
    pub overdrafts_denied: u64,
    pub alloc_failures: u64,
    pub unscoped_allocs: u64,
    pub unscoped_bytes: u64,
    pub armed_unscoped: u64,
    pub unscoped_denied: u64,
    pub abandoned: u64,
    pub abandon_recovered: u64,
    pub abandon_reclaimed: u64,
    pub poisoned_sweeps: u64,
    pub children_carved: u64,
    pub children_pooled: u64,
    pub pages_returned_to_parent: u64,
    pub tickets_issued: u64,
    pub tickets_refused: u64,
    pub blocks_served: u64,
    pub blocks_returned: u64,
    /// Admissions served by re-keying a returned block run of the scope.
    pub blocks_reused: u64,
    pub block_moves: u64,
    pub stale_enters: u64,
    pub stale_allocs: u64,
    pub stale_admissions: u64,
    pub route_bound_exceeded: u64,
    pub recycles: u64,
    pub glue_live: usize,
    pub glue_peak: usize,
    /// dlmalloc footprint (chunk bytes) of live glue allocations, and its peak.
    pub glue_fp_live: usize,
    pub glue_fp_peak: usize,
    pub glue_allocs_live: u64,
    /// Glue segments placed outside the partition (`glue-spill`), and the
    /// most spilled pages live at once.
    pub glue_spills: u64,
    pub glue_spill_peak_pages: u32,
    pub last_failure: Failure,
}

impl Counters {
    pub const ZERO: Counters = Counters {
        allocs: 0,
        frees: 0,
        reallocs: 0,
        scopes_opened: 0,
        scopes_released: 0,
        quarantines: 0,
        quarantine_releases: 0,
        refusals_retry: 0,
        refusals_capacity: 0,
        refusals_poisoned: 0,
        observed_refusals: 0,
        observed_excess_pages: 0,
        overdrafts: 0,
        overdraft_pages: 0,
        overdrafts_denied: 0,
        alloc_failures: 0,
        unscoped_allocs: 0,
        unscoped_bytes: 0,
        armed_unscoped: 0,
        unscoped_denied: 0,
        abandoned: 0,
        abandon_recovered: 0,
        abandon_reclaimed: 0,
        poisoned_sweeps: 0,
        children_carved: 0,
        children_pooled: 0,
        pages_returned_to_parent: 0,
        tickets_issued: 0,
        tickets_refused: 0,
        blocks_served: 0,
        blocks_returned: 0,
        blocks_reused: 0,
        block_moves: 0,
        stale_enters: 0,
        stale_allocs: 0,
        stale_admissions: 0,
        route_bound_exceeded: 0,
        recycles: 0,
        glue_live: 0,
        glue_peak: 0,
        glue_fp_live: 0,
        glue_fp_peak: 0,
        glue_allocs_live: 0,
        glue_spills: 0,
        glue_spill_peak_pages: 0,
        last_failure: Failure::ZERO,
    };
}

/// Per-route counters, indexed by `Route::id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteCounters {
    pub name: &'static str,
    pub opened: u64,
    pub released: u64,
    pub quarantined: u64,
    pub max_reserved_bytes: usize,
    pub peak_live: usize,
    pub peak_pages: u32,
    pub refusals_retry: u64,
    pub refusals_capacity: u64,
    pub refusals_poisoned: u64,
    pub abandoned: u64,
    pub overdrafts: u64,
    pub alloc_failures: u64,
    /// Invocations whose admissions (scope plus ancestors) exceeded the
    /// model's `largest_route_bytes`: a derivation defect.
    pub bound_exceeded: u64,
    /// The largest such total seen, in bytes.
    pub max_invocation_bytes: u64,
}

impl RouteCounters {
    pub const ZERO: RouteCounters = RouteCounters {
        name: "",
        opened: 0,
        released: 0,
        quarantined: 0,
        max_reserved_bytes: 0,
        peak_live: 0,
        peak_pages: 0,
        refusals_retry: 0,
        refusals_capacity: 0,
        refusals_poisoned: 0,
        abandoned: 0,
        overdrafts: 0,
        alloc_failures: 0,
        bound_exceeded: 0,
        max_invocation_bytes: 0,
    };
}

pub type RouteStats = RouteCounters;

/// Isolate-level snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    pub model_version: &'static str,
    pub mode: Mode,
    pub healthy: bool,
    pub static_bytes: usize,
    /// `memory.size` on wasm32; natively the region up to its high-water.
    pub memory_bytes: usize,
    pub limit_pages: u32,
    pub glue_budget_pages: u32,
    pub glue_used_pages: u32,
    pub free_pages: u32,
    pub held_pages: u32,
    pub in_use_pages: u32,
    pub peak_in_use_pages: u32,
    pub high_water_pages: u32,
    pub largest_free_run: u32,
    pub open_scopes: u32,
    pub quarantined_scopes: u32,
    pub abandoned_scopes: u32,
    pub pinned_pages: u32,
    pub quarantine_budget_pages: u32,
    pub tickets_in_flight: u32,
    pub tickets_peak: u32,
    pub ticket_limit: u32,
    pub epoch: u64,
    pub clock_ms: u64,
    pub allocs: u64,
    pub frees: u64,
    pub reallocs: u64,
    pub scopes_opened: u64,
    pub scopes_released: u64,
    pub quarantines: u64,
    pub quarantine_releases: u64,
    pub refusals_retry: u64,
    pub refusals_capacity: u64,
    pub refusals_poisoned: u64,
    pub observed_refusals: u64,
    pub observed_excess_pages: u64,
    pub overdrafts: u64,
    pub overdraft_pages: u64,
    pub overdrafts_denied: u64,
    pub alloc_failures: u64,
    pub unscoped_allocs: u64,
    pub unscoped_bytes: u64,
    pub armed_unscoped: u64,
    pub unscoped_denied: u64,
    pub abandoned: u64,
    pub abandon_recovered: u64,
    pub abandon_reclaimed: u64,
    pub poisoned_sweeps: u64,
    pub children_carved: u64,
    pub children_pooled: u64,
    pub pages_returned_to_parent: u64,
    pub tickets_issued: u64,
    pub tickets_refused: u64,
    pub blocks_served: u64,
    pub blocks_returned: u64,
    /// Admissions served by re-keying a returned block run of the scope.
    pub blocks_reused: u64,
    pub block_moves: u64,
    pub stale_enters: u64,
    pub stale_allocs: u64,
    pub stale_admissions: u64,
    pub route_bound_exceeded: u64,
    pub recycles: u64,
    pub glue_live: usize,
    pub glue_peak: usize,
    pub glue_fp_live: usize,
    pub glue_fp_peak: usize,
    pub glue_allocs_live: u64,
    /// Glue pages outside the partition now, their peak, and the segments
    /// spilled (`glue-spill`: entry points not yet wrapped in scopes).
    pub glue_spill_pages: u32,
    pub glue_spill_peak_pages: u32,
    pub glue_spills: u64,
    /// The model's fixed linear memory (`LINEAR_MEMORY_BYTES`); on wasm32 a
    /// `memory_bytes` above it means the module was not linked with it.
    pub linear_memory_model_bytes: usize,
    /// The linked wasm32 stack (`__stack_high - __stack_low`; 0 natively)
    /// and the model's `stack_bytes`; a mismatch is a model defect.
    pub stack_bytes: usize,
    pub stack_model_bytes: usize,
    /// The deepest the stack has reached since the pool was initialised
    /// (painted high-water, wasm32; 0 natively).
    pub stack_high_water: usize,
    pub last_failure: Failure,
}

/// Current isolate counters. Allocation-free.
pub fn stats() -> Stats {
    let _g = lock::hold();
    unsafe {
        pool::ensure_init();
        let p = pool::pool();
        let (mut open, mut quarantined, mut abandoned, mut held) = (0, 0, 0, 0u32);
        for page in p.glue_budget..p.region_pages {
            if let Some((status, _)) = pool::scope_at(page) {
                match status {
                    OPEN => open += 1,
                    QUARANTINED => quarantined += 1,
                    ABANDONED => abandoned += 1,
                    _ => {}
                }
            }
            if p.own(page) & 0x8000 != 0 && p.own(page) < pool::OUT {
                held = held.saturating_add(1);
            }
        }
        let c = p.c;
        Stats {
            model_version: model::VERSION,
            mode: p.mode,
            healthy: p.health() == Health::Healthy,
            static_bytes: p.region.static_bytes,
            memory_bytes: p.memory_bytes(),
            limit_pages: p.limit,
            glue_budget_pages: p.glue_budget,
            glue_used_pages: p.glue_used,
            free_pages: p.free_count,
            held_pages: held,
            in_use_pages: p.in_use,
            peak_in_use_pages: p.peak_in_use,
            high_water_pages: p.high_water,
            largest_free_run: p.largest_run(),
            open_scopes: open,
            quarantined_scopes: quarantined,
            abandoned_scopes: abandoned,
            pinned_pages: p.pinned_pages(),
            quarantine_budget_pages: pool::quarantine_budget(p),
            tickets_in_flight: p.tickets,
            tickets_peak: p.tickets_peak,
            ticket_limit: model::GLUE_TICKETS,
            epoch: p.epoch,
            clock_ms: p.clock_ms,
            allocs: c.allocs,
            frees: c.frees,
            reallocs: c.reallocs,
            scopes_opened: c.scopes_opened,
            scopes_released: c.scopes_released,
            quarantines: c.quarantines,
            quarantine_releases: c.quarantine_releases,
            refusals_retry: c.refusals_retry,
            refusals_capacity: c.refusals_capacity,
            refusals_poisoned: c.refusals_poisoned,
            observed_refusals: c.observed_refusals,
            observed_excess_pages: c.observed_excess_pages,
            overdrafts: c.overdrafts,
            overdraft_pages: c.overdraft_pages,
            overdrafts_denied: c.overdrafts_denied,
            alloc_failures: c.alloc_failures,
            unscoped_allocs: c.unscoped_allocs,
            unscoped_bytes: c.unscoped_bytes,
            armed_unscoped: c.armed_unscoped,
            unscoped_denied: c.unscoped_denied,
            abandoned: c.abandoned,
            abandon_recovered: c.abandon_recovered,
            abandon_reclaimed: c.abandon_reclaimed,
            poisoned_sweeps: c.poisoned_sweeps,
            children_carved: c.children_carved,
            children_pooled: c.children_pooled,
            pages_returned_to_parent: c.pages_returned_to_parent,
            tickets_issued: c.tickets_issued,
            tickets_refused: c.tickets_refused,
            blocks_served: c.blocks_served,
            blocks_returned: c.blocks_returned,
            blocks_reused: c.blocks_reused,
            block_moves: c.block_moves,
            stale_enters: c.stale_enters,
            stale_allocs: c.stale_allocs,
            stale_admissions: c.stale_admissions,
            route_bound_exceeded: c.route_bound_exceeded,
            recycles: c.recycles,
            glue_live: c.glue_live,
            glue_peak: c.glue_peak,
            glue_fp_live: c.glue_fp_live,
            glue_fp_peak: c.glue_fp_peak,
            glue_allocs_live: c.glue_allocs_live,
            glue_spill_pages: p.glue_spill,
            glue_spill_peak_pages: c.glue_spill_peak_pages,
            glue_spills: c.glue_spills,
            linear_memory_model_bytes: model::LINEAR_MEMORY_BYTES,
            stack_bytes: { let (lo, hi) = sys::stack::bounds(); hi - lo },
            stack_model_bytes: model::STACK_BYTES,
            stack_high_water: sys::stack::high_water(),
            last_failure: c.last_failure,
        }
    }
}

/// One route's counters. Allocation-free.
pub fn route_stats(id: u8) -> RouteStats {
    let _g = lock::hold();
    unsafe { pool::pool().routes.get(id as usize).copied().unwrap_or(RouteCounters::ZERO) }
}

impl Stats {
    /// Every numeric field, in a fixed order, for status JSON and wasm
    /// exports (one source of truth for both).
    pub fn fields(&self) -> [(&'static str, u64); 81] {
        let f = &self.last_failure;
        [
            ("mode", self.mode as u64),
            ("healthy", self.healthy as u64),
            ("page_bytes", PAGE as u64),
            ("static_bytes", self.static_bytes as u64),
            ("memory_bytes", self.memory_bytes as u64),
            ("limit_pages", self.limit_pages as u64),
            ("glue_budget_pages", self.glue_budget_pages as u64),
            ("glue_used_pages", self.glue_used_pages as u64),
            ("free_pages", self.free_pages as u64),
            ("held_pages", self.held_pages as u64),
            ("in_use_pages", self.in_use_pages as u64),
            ("peak_in_use_pages", self.peak_in_use_pages as u64),
            ("high_water_pages", self.high_water_pages as u64),
            ("largest_free_run", self.largest_free_run as u64),
            ("open_scopes", self.open_scopes as u64),
            ("quarantined_scopes", self.quarantined_scopes as u64),
            ("abandoned_scopes", self.abandoned_scopes as u64),
            ("pinned_pages", self.pinned_pages as u64),
            ("quarantine_budget_pages", self.quarantine_budget_pages as u64),
            ("tickets_in_flight", self.tickets_in_flight as u64),
            ("tickets_peak", self.tickets_peak as u64),
            ("ticket_limit", self.ticket_limit as u64),
            ("epoch", self.epoch),
            ("allocs", self.allocs),
            ("frees", self.frees),
            ("reallocs", self.reallocs),
            ("scopes_opened", self.scopes_opened),
            ("scopes_released", self.scopes_released),
            ("quarantines", self.quarantines),
            ("quarantine_releases", self.quarantine_releases),
            ("refusals_retry", self.refusals_retry),
            ("refusals_capacity", self.refusals_capacity),
            ("refusals_poisoned", self.refusals_poisoned),
            ("observed_refusals", self.observed_refusals),
            ("observed_excess_pages", self.observed_excess_pages),
            ("overdrafts", self.overdrafts),
            ("overdraft_pages", self.overdraft_pages),
            ("overdrafts_denied", self.overdrafts_denied),
            ("alloc_failures", self.alloc_failures),
            ("unscoped_allocs", self.unscoped_allocs),
            ("unscoped_bytes", self.unscoped_bytes),
            ("armed_unscoped", self.armed_unscoped),
            ("unscoped_denied", self.unscoped_denied),
            ("abandoned", self.abandoned),
            ("abandon_recovered", self.abandon_recovered),
            ("abandon_reclaimed", self.abandon_reclaimed),
            ("poisoned_sweeps", self.poisoned_sweeps),
            ("children_carved", self.children_carved),
            ("children_pooled", self.children_pooled),
            ("pages_returned_to_parent", self.pages_returned_to_parent),
            ("tickets_issued", self.tickets_issued),
            ("tickets_refused", self.tickets_refused),
            ("blocks_served", self.blocks_served),
            ("blocks_returned", self.blocks_returned),
            ("blocks_reused", self.blocks_reused),
            ("block_moves", self.block_moves),
            ("stale_enters", self.stale_enters),
            ("stale_allocs", self.stale_allocs),
            ("stale_admissions", self.stale_admissions),
            ("route_bound_exceeded", self.route_bound_exceeded),
            ("recycles", self.recycles),
            ("stack_bytes", self.stack_bytes as u64),
            ("stack_model_bytes", self.stack_model_bytes as u64),
            ("stack_high_water", self.stack_high_water as u64),
            ("glue_live", self.glue_live as u64),
            ("glue_peak", self.glue_peak as u64),
            ("glue_fp_live", self.glue_fp_live as u64),
            ("glue_fp_peak", self.glue_fp_peak as u64),
            ("glue_allocs_live", self.glue_allocs_live),
            ("glue_spill_pages", self.glue_spill_pages as u64),
            ("glue_spill_peak_pages", self.glue_spill_peak_pages as u64),
            ("glue_spills", self.glue_spills),
            ("linear_memory_model_bytes", self.linear_memory_model_bytes as u64),
            ("clock_ms", self.clock_ms),
            ("last_failure_kind", f.kind as u64),
            ("last_failure_glue", f.glue as u64),
            ("last_failure_route", f.route as u64),
            ("last_failure_size", f.size as u64),
            ("last_failure_in_use_pages", f.in_use_pages as u64),
            ("last_failure_free_pages", f.free_pages as u64),
            ("last_failure_limit_pages", f.limit_pages as u64),
        ]
    }

    /// Writes the snapshot as one JSON object (the design's `arena` status
    /// section) into any formatter, e.g. [`crate::io::FmtBuf`].
    pub fn write_json(&self, w: &mut impl fmt::Write) -> fmt::Result {
        write!(w, "{{\"model\":\"{}\"", self.model_version)?;
        for (k, v) in self.fields() {
            write!(w, ",\"{k}\":{v}")?;
        }
        w.write_str("}")
    }
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write_json(f)
    }
}

/// What an abort hook copies out of a dying instance (design §2.6). worker-
/// build 0.8.7 recovers from a trap by instantiating a new instance with fresh
/// memory, so nothing in the old instance's statics survives: the host's
/// abort hook (`wasm_bindgen::handler::set_on_abort`, chained before
/// worker's re-instantiation) reads this allocation-free snapshot and hands
/// its [`words`](Postmortem::words) to JS, and the new instance's first entry
/// records it durably beside any stale step marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Postmortem {
    /// The scope current when the instance died (`u16::MAX`: none).
    pub scope: u16,
    pub epoch: u64,
    /// That scope's route, if the handle was still live.
    pub route: Option<u8>,
    pub last_failure: Failure,
    /// Set when the death was a deliberate `recycle`.
    pub recycle: Option<PoisonCause>,
    pub pinned_pages: u32,
    pub stack_high_water: usize,
    pub alloc_failures: u64,
}

/// Field count of [`Postmortem::words`].
pub const POSTMORTEM_WORDS: usize = 14;

impl Postmortem {
    /// The snapshot as `u32` words for a scalar JS import: `[version 1,
    /// scope, route + 1 (0 none), last_failure kind, glue, failure route,
    /// failure size, recycle kind (0 none), recycle route, recycle pages,
    /// pinned pages, stack high-water, alloc failures (low 32 bits), epoch
    /// (low 32 bits)]`.
    pub fn words(&self) -> [u32; POSTMORTEM_WORDS] {
        let f = &self.last_failure;
        let (rk, rr, rp) = match self.recycle {
            Some(c) => (c.kind as u32, c.route as u32, c.pages),
            None => (0, 0, 0),
        };
        [
            1,
            self.scope as u32,
            self.route.map_or(0, |r| r as u32 + 1),
            f.kind as u32,
            f.glue as u32,
            f.route as u32,
            f.size.min(u32::MAX as usize) as u32,
            rk,
            rr,
            rp,
            self.pinned_pages,
            self.stack_high_water.min(u32::MAX as usize) as u32,
            self.alloc_failures as u32,
            self.epoch as u32,
        ]
    }

    /// The pin kind a recycle word names.
    pub fn pin_kind(word: u32) -> Option<PinKind> {
        match word {
            1 => Some(PinKind::Quarantined),
            2 => Some(PinKind::Abandoned),
            3 => Some(PinKind::Refusal),
            4 => Some(PinKind::Platform),
            _ => None,
        }
    }
}

/// The postmortem snapshot. Allocation-free and safe to call from an abort
/// hook after a trap: it reads statics only (no initialisation, no sweep).
pub fn postmortem() -> Postmortem {
    let _g = lock::hold();
    unsafe {
        let p = pool::pool();
        let (scope, epoch) = current::get();
        let pinned = if p.ready_for_postmortem() { p.pinned_pages() } else { 0 };
        Postmortem {
            scope,
            epoch,
            route: pool::route_of(scope, epoch),
            last_failure: p.c.last_failure,
            recycle: p.recycle_intent,
            pinned_pages: pinned,
            stack_high_water: sys::stack::high_water(),
            alloc_failures: p.c.alloc_failures,
        }
    }
}

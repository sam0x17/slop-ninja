//! Scopes: admission, current-arena routing for synchronous code and futures,
//! abandonment tracking and allocation-free refusals (design §2.3 to §2.6).

use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

use crate::glue;
use crate::pool::{self, Footprint, Health, Hold, PAGE, block_bytes, pages_of, scope_pages};
use crate::sys::current;

/// Documented platform wall limits (design §2.5; [Q] in
/// `data/task-roots/testnet/fanout/LIMITS.md`). A route's abandonment
/// deadline is derived from these, or from the route's own await bounds.
pub mod limits {
    /// "`waitUntil()` extends execution for up to 30 seconds".
    pub const WAIT_UNTIL_MS: u64 = 30_000;
    /// Cron Triggers, Queue consumers and Durable Object alarm handlers each
    /// have "15 minutes" of wall time.
    pub const CRON_MS: u64 = 15 * 60 * 1000;
    pub const QUEUE_MS: u64 = 15 * 60 * 1000;
    pub const ALARM_MS: u64 = 15 * 60 * 1000;
    /// CPU per HTTP request or Durable Object request: "30 seconds
    /// (default)". An HTTP route's deadline is its sequential await bounds
    /// plus this.
    pub const REQUEST_CPU_MS: u64 = 30_000;
}

/// A synchronous route's identity. The `u8` id indexes fixed static
/// counters, so the key space is the type's; an application declares its
/// routes as constants. A `Route` from [`Route::new`] has no abandonment
/// deadline: its scope never yields to JS while open (`Scope::open` with
/// `enter(|| ..)`), so it can never be abandoned. A scope that awaits must use
/// an [`AsyncRoute`], which always declares one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub id: u8,
    pub name: &'static str,
    /// Zero for a synchronous route; set only through [`AsyncRoute`].
    abandon_after_ms: u64,
}

impl Route {
    pub const fn new(id: u8, name: &'static str) -> Self {
        Self { id, name, abandon_after_ms: 0 }
    }

    /// How long a scope of this route may stay open before it counts as
    /// abandoned when not being polled (0: synchronous, never).
    pub const fn abandon_after_ms(&self) -> u64 {
        self.abandon_after_ms
    }
}

/// A route whose scope awaits with its scope open (an `enter` handler, a
/// `waitUntil` continuation, a [`Scoped`] future). Its `abandon_after_ms` is
/// mandatory and non-zero (design §2.5): the platform wall limit of its entry
/// kind where one is documented ([`limits`]), else the sum of the route's
/// sequential await bounds (every platform await carries an `AbortSignal`
/// timeout) plus the CPU limit. Declared as a constant, a zero deadline is a
/// compile error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AsyncRoute(Route);

impl AsyncRoute {
    pub const fn new(id: u8, name: &'static str, abandon_after_ms: u64) -> Self {
        assert!(abandon_after_ms > 0, "an async route declares its abandonment deadline (design §2.5)");
        Self(Route { id, name, abandon_after_ms })
    }

    pub const fn route(self) -> Route {
        self.0
    }

    pub const fn id(self) -> u8 {
        self.0.id
    }

    pub const fn abandon_after_ms(self) -> u64 {
        self.0.abandon_after_ms
    }
}

impl From<AsyncRoute> for Route {
    fn from(r: AsyncRoute) -> Route {
        r.0
    }
}

/// What the host does about a refusal. Chosen by cause, never fixed per
/// reason (design §2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalKind {
    /// Pages held by other open scopes, or an input that may arrive whole
    /// later: re-arm, redeliver or 503.
    Retry,
    /// No retry can satisfy it under this build's bounds: the need exceeds
    /// what the scope partition leaves beside the requester's own pages and
    /// its ancestors' (which waiting cannot free). A durable pause
    /// (`arena_capacity`), never a skip.
    Capacity,
    /// Only pages pinned by quarantined or abandoned scopes, or the
    /// fragmentation they cause, block it. Nothing in this instance returns
    /// them: record a durable fault and recycle the instance at a safe point.
    Poisoned,
    /// The input breaks its schema bound: a 4xx; retrying it unchanged fails
    /// the same way.
    Invalid,
    /// A deterministic internal inconsistency (an encoder's length disagrees
    /// with its declared total, a re-hash disagrees with a signed root, a
    /// handle used after its scope closed): a durable fault naming the route
    /// and digest.
    Defect,
}

/// What was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Not enough pages for the reservation.
    Pages,
    /// No free run of pages long enough for one block.
    Contiguity,
    /// A declared length above its schema or route bound.
    Bound,
    /// A write past an exact capacity or declared length.
    Overflow,
    /// Fewer bytes than declared.
    Truncated,
    /// The allocator returned no memory despite admission (see
    /// `stats().last_failure`).
    Allocator,
    /// The platform could not provide the pool.
    Platform,
    /// Every glue ticket is in flight.
    Glue,
    /// The handle's scope is closed (its page may belong to a later scope)
    /// or quarantined.
    Stale,
    /// A future on a scope without an abandonment deadline.
    Deadline,
}

/// An allocation-free refusal: `Copy`, no heap, safe to return from any
/// handler and to turn into a static status code. `need` and `have` are in
/// bytes (pages × 64 KiB for page reasons).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub route: u8,
    pub kind: RefusalKind,
    pub reason: Reason,
    pub need: usize,
    pub have: usize,
}

impl Refusal {
    pub const fn new(route: u8, kind: RefusalKind, reason: Reason, need: usize, have: usize) -> Self {
        Self { route, kind, reason, need, have }
    }

    /// A stable numeric code for status responses and wasm exports.
    pub const fn code(&self) -> u32 {
        let kind = match self.kind {
            RefusalKind::Retry => 0x100,
            RefusalKind::Capacity => 0x200,
            RefusalKind::Poisoned => 0x300,
            RefusalKind::Invalid => 0x400,
            RefusalKind::Defect => 0x500,
        };
        let reason = match self.reason {
            Reason::Pages => 1,
            Reason::Contiguity => 2,
            Reason::Bound => 3,
            Reason::Overflow => 4,
            Reason::Truncated => 5,
            Reason::Allocator => 6,
            Reason::Platform => 7,
            Reason::Glue => 8,
            Reason::Stale => 9,
            Reason::Deadline => 10,
        };
        kind | reason
    }

    /// The same refusal with a caller-chosen kind (the caller knows the
    /// cause; see `io::Kinds`).
    pub const fn with_kind(self, kind: RefusalKind) -> Self {
        Self { kind, ..self }
    }
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "arena refusal: route {} {:?} {:?} need {} have {}", self.route, self.kind, self.reason, self.need, self.have)
    }
}

/// A scope's accounting snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopeStats {
    pub id: u16,
    pub epoch: u64,
    pub route: u8,
    /// 0 gone (closed), 1 open, 2 quarantined, 3 abandoned.
    pub status: u8,
    pub reserved_bytes: usize,
    pub reserved_pages: u32,
    pub used_pages: u32,
    pub held_pages: u32,
    /// Held pages lent to children (part of `held_pages`).
    pub lent_pages: u32,
    pub peak_pages: u32,
    pub live: usize,
    pub peak_live: usize,
    pub allocs: u64,
    pub overdraft_pages: u32,
    pub children: u32,
    pub carved_pages: u32,
    /// Admitted block runs not in use.
    pub pending_blocks: u32,
    /// Absolute abandonment deadline (pool clock), 0 for none.
    pub deadline_ms: u64,
}

impl ScopeStats {
    pub(crate) const fn gone(id: u16, epoch: u64) -> Self {
        Self {
            id,
            epoch,
            route: 0,
            status: 0,
            reserved_bytes: 0,
            reserved_pages: 0,
            used_pages: 0,
            held_pages: 0,
            lent_pages: 0,
            peak_pages: 0,
            live: 0,
            peak_live: 0,
            allocs: 0,
            overdraft_pages: 0,
            children: 0,
            carved_pages: 0,
            pending_blocks: 0,
            deadline_ms: 0,
        }
    }
}

/// How a scope ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    /// Every page went back to the pool (or to the parent it was carved from).
    Released { pages: u32, peak_live: usize, peak_pages: u32 },
    /// A value outlived the scope: the pages still holding live bytes stay
    /// owned until the last such value is freed. Recorded as a leak and
    /// counted against the quarantine budget; strict mode fails on it.
    Quarantined { live: usize, pages: u32 },
}

/// Bytes a parent lends ([`ScopeRef::lend`]) for one child of
/// `control_bytes`: the child's state page and control run.
pub const fn child_bytes(control_bytes: usize) -> usize {
    (scope_pages(pool::block_or_zero(control_bytes)) as usize).saturating_mul(PAGE)
}

/// A handle to an open scope: everything but closing it. A handler future
/// built by [`enter`] receives one, since the future lives inside the
/// [`Entered`] that owns the [`Scope`]. It is neither `Copy` nor `Clone`;
/// every use is checked against the scope's `u64` epoch: a handle that
/// outlives its scope (or whose scope is quarantined) allocates nothing (the
/// allocation fails and is recorded as `StaleScope`), and its admissions are
/// refused (`Defect`/`Stale`). It never writes into a later scope that reuses
/// the page.
#[derive(Debug)]
pub struct ScopeRef {
    id: u16,
    epoch: u64,
    route: Route,
    /// Whether the scope has an abandonment deadline (its own route's, or an
    /// ancestor's it inherited).
    deadline: bool,
    _not_send: PhantomData<*mut ()>,
}

impl ScopeRef {
    pub fn id(&self) -> u16 {
        self.id
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn route(&self) -> Route {
        self.route
    }

    /// Whether futures may run in this scope ([`scoped`](Self::scoped)).
    pub fn has_deadline(&self) -> bool {
        self.deadline
    }

    /// A child scope (a step, a chunk round, a `waitUntil` continuation). Its
    /// run is carved from the pages this scope lent ([`lend`](Self::lend)),
    /// so it is charged against this scope's reservation, and its pages go
    /// back to the lent pages when it closes. Only the part the lent pages
    /// cannot cover comes from the pool; this scope's own heap pages and
    /// blocks are never carved. Its abandonment deadline is its own route's
    /// (from the pool clock now) when the route declares one, else this
    /// scope's. Values cross scopes only by copy.
    pub fn child(&self, route: impl Into<Route>, control_bytes: usize) -> Result<Scope, Refusal> {
        self.child_in(route.into(), control_bytes, false)
    }

    /// [`child`](Self::child), refused in every mode, `Observe` included,
    /// when the pool cannot physically serve `control_bytes` even scattered
    /// (its free pages and this scope's unused lent pages are fewer). For a
    /// large step whose overdraft would otherwise exhaust a fixed or
    /// capped linear memory and trap the instance (two package steps of
    /// co-resident objects); the refusal is classed by cause like any other.
    pub fn child_physical(&self, route: impl Into<Route>, control_bytes: usize) -> Result<Scope, Refusal> {
        self.child_in(route.into(), control_bytes, true)
    }

    fn child_in(&self, route: Route, control_bytes: usize, physical: bool) -> Result<Scope, Refusal> {
        let (id, epoch) = pool::open(route.id, route.name, control_bytes, Some((self.id, self.epoch)), physical)?;
        if route.abandon_after_ms != 0 {
            pool::set_deadline(id, epoch, route.abandon_after_ms);
        }
        Ok(Scope::from_parts(id, epoch, route, route.abandon_after_ms != 0 || self.deadline))
    }

    /// Lends `bytes` (whole pages, one contiguous run) to this scope's
    /// children: held for this scope, carvable only by its children. Size it
    /// from [`child_bytes`] plus the children's own admissions
    /// ([`block_bytes`], [`Footprint::bytes`]). Allocation-free.
    pub fn lend(&self, bytes: usize) -> Result<(), Refusal> {
        pool::reserve(self.id, self.epoch, self.route.id, 0, pages_of(bytes), Hold::Lend)
    }

    /// [`admit_block`](Self::admit_block) for `bytes` at alignment 1: one
    /// allocation of exactly `bytes` where an exact size becomes known
    /// (object size, `Content-Length`, a counted row set).
    pub fn reserve(&self, bytes: usize) -> Result<(), Refusal> {
        self.admit_block(bytes, 1)
    }

    /// Admits one block: a single allocation of exactly `len` bytes at
    /// `align` that is about to be made (an `ExactBuf`, a `BoundedVec`, a
    /// boxed future, or a buffer's growth to exactly `len`). Its pages are
    /// held as one contiguous run, and the allocation of that layout is
    /// served directly from the run, never by the scope's heap, so it cannot
    /// fail for want of pages or contiguity, in any order relative to the
    /// scope's other admissions. Freed, the run is held again for the same
    /// layout until the scope closes. Allocation-free.
    pub fn admit_block(&self, len: usize, align: usize) -> Result<(), Refusal> {
        let bytes = block_bytes(len, align);
        pool::reserve(self.id, self.epoch, self.route.id, len, pages_of(bytes), Hold::Block { len, align })
    }

    /// Marks this scope as living for the instance (a home scope that is
    /// never closed): a refusal its pages cause is classed `Capacity`
    /// instead of `Retry`, since no wait frees them.
    pub fn set_permanent(&self) {
        pool::set_permanent(self.id, self.epoch);
    }

    /// Admits a set of small allocations as one contiguous run for the
    /// scope's heap.
    pub fn admit_footprint(&self, f: Footprint) -> Result<(), Refusal> {
        pool::reserve(self.id, self.epoch, self.route.id, f.chunks(), pages_of(f.bytes()), Hold::Heap)
    }

    /// Admits a JS string of `utf16_len` code units passed into Rust by
    /// wasm-bindgen: `passStringToWasm0` mallocs `len`, reallocs to at most
    /// `len + 3 × len` while encoding, then shrinks, so the old and the grown
    /// block can coexist. The first is a block of exactly `len`; the grown
    /// one moves into the second block, the smallest admitted run that
    /// holds it.
    pub fn admit_js_string(&self, utf16_len: usize) -> Result<(), Refusal> {
        self.admit_block(utf16_len, 1)?;
        self.admit_block(utf16_len.saturating_mul(4), 1)
    }

    /// Runs `f` with this scope current. A stale handle runs `f` with
    /// nothing current that can allocate: every allocation in it fails
    /// (`StaleScope`), never lands in another scope.
    pub fn enter<R>(&self, f: impl FnOnce() -> R) -> R {
        let _ = pool::check_enter(self.id, self.epoch);
        let _g = current::Guard::enter(self.id, self.epoch);
        f()
    }

    /// Wraps a future so that every poll runs with this scope current and
    /// marks the scope as being polled (an abandonment sweep never takes a
    /// scope mid-poll; a poll of an abandoned scope recovers it). Refused
    /// (`Defect`/`Deadline`) on a scope with no abandonment deadline: a
    /// synchronous route must not hold a future.
    pub fn scoped<F: Future>(&self, fut: F) -> Result<Scoped<'_, F>, Refusal> {
        if !self.deadline {
            return Err(Refusal::new(self.route.id, RefusalKind::Defect, Reason::Deadline, 0, 0));
        }
        Ok(Scoped { id: self.id, epoch: self.epoch, fut, _scope: PhantomData })
    }

    /// The scope's accounting (status 0 once it is gone).
    pub fn stats(&self) -> ScopeStats {
        pool::scope_stats(self.id, self.epoch)
    }
}

/// One invocation's arena (or a child of one): the owning handle, which
/// closes the scope when closed or dropped. `!Send`: it routes allocations of
/// the thread (or wasm instance) that polls it. It is identified by its first
/// page and an epoch, so a stale handle can never act on a later scope that
/// reuses the page. Derefs to [`ScopeRef`] for admission and entry.
pub struct Scope {
    r: ScopeRef,
    open: bool,
}

impl core::ops::Deref for Scope {
    type Target = ScopeRef;
    fn deref(&self) -> &ScopeRef {
        &self.r
    }
}

impl Scope {
    fn from_parts(id: u16, epoch: u64, route: Route, deadline: bool) -> Scope {
        Scope { r: ScopeRef { id, epoch, route, deadline, _not_send: PhantomData }, open: true }
    }

    /// Admits a heap footprint of `control_bytes` for the route's control
    /// path (allocations other than its admitted blocks), held as one
    /// contiguous run with the scope's state page. Allocation-free; refuses
    /// when no such run exists. The bytes are a dlmalloc footprint: derive
    /// them with [`Footprint`] or from a witness test's peak pages, never from
    /// raw live bytes. A [`Route`] gives a synchronous scope (no deadline); an
    /// [`AsyncRoute`] gives one whose deadline runs from the pool clock now.
    pub fn open(route: impl Into<Route>, control_bytes: usize) -> Result<Scope, Refusal> {
        let route = route.into();
        let (id, epoch) = pool::open(route.id, route.name, control_bytes, None, false)?;
        if route.abandon_after_ms != 0 {
            pool::set_deadline(id, epoch, route.abandon_after_ms);
        }
        Ok(Scope::from_parts(id, epoch, route, route.abandon_after_ms != 0))
    }

    /// [`open`](Self::open) with a [`Footprint`].
    pub fn open_footprint(route: impl Into<Route>, f: Footprint) -> Result<Scope, Refusal> {
        Scope::open(route, f.bytes())
    }

    /// A second handle for the future boxed inside this scope (`enter`,
    /// `wait_until`). Not public: a handle outliving its scope is refused at
    /// every use, but it should not be easy to make one.
    pub(crate) fn handle(&self) -> ScopeRef {
        ScopeRef { id: self.r.id, epoch: self.r.epoch, route: self.r.route, deadline: self.r.deadline, _not_send: PhantomData }
    }

    /// Releases every page, or quarantines the scope if a value outlived it.
    pub fn close(mut self) -> Closed {
        self.open = false;
        pool::close(self.r.id, self.r.epoch)
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if self.open {
            pool::close(self.r.id, self.r.epoch);
        }
    }
}

/// Marks a scope as being polled for its lifetime, including on unwind.
struct PollGuard {
    id: u16,
    epoch: u64,
}

impl PollGuard {
    fn begin(id: u16, epoch: u64) -> Self {
        pool::poll_begin(id, epoch);
        Self { id, epoch }
    }
}

impl Drop for PollGuard {
    fn drop(&mut self) {
        pool::poll_end(self.id, self.epoch);
    }
}

/// A future polled with its scope current.
pub struct Scoped<'s, F> {
    id: u16,
    epoch: u64,
    fut: F,
    _scope: PhantomData<&'s Scope>,
}

impl<F: Future> Future for Scoped<'_, F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        // SAFETY: `fut` is structurally pinned and never moved.
        let this = unsafe { self.get_unchecked_mut() };
        let _p = PollGuard::begin(this.id, this.epoch);
        let _g = current::Guard::enter(this.id, this.epoch);
        unsafe { Pin::new_unchecked(&mut this.fut) }.poll(cx)
    }
}

/// An entry point's whole lifetime: the scope, and the handler future boxed
/// inside it (design §2.5). Dropping it, whenever the platform drops the
/// task, drops the future inside the scope and closes the scope: the drop
/// guard that reclaims a cancelled invocation.
pub struct Entered<F: Future> {
    scope: Option<Scope>,
    fut: Option<Pin<Box<F>>>,
}

/// Admits one block of exactly `(len, align)` in the scope current on this
/// thread (wasm32: instance), the scope whose future is being polled: for a
/// helper that learns an exact size (an R2 object's, a body's declared
/// length) without holding the invocation's [`ScopeRef`]. Allocation-free.
/// `Ok(true)`: admitted. `Ok(false)`: no scope is current (an entry point not
/// yet wrapped in a scope); nothing is admitted and the allocation goes to
/// glue as before. A refusal is classed by cause like any admission.
pub fn admit_current_block(len: usize, align: usize) -> Result<bool, Refusal> {
    let (id, epoch) = current::get();
    if id == pool::NONE {
        return Ok(false);
    }
    let route = pool::current_route(id, epoch).unwrap_or(0);
    let bytes = block_bytes(len, align);
    pool::reserve(id, epoch, route, len, pages_of(bytes), Hold::Block { len, align }).map(|()| true)
}

/// Admits `bytes` of heap in the scope current on this thread (wasm32:
/// instance), as one contiguous run: for a helper that learns a measured
/// need from an exact size (a parse whose peak scales with its input's
/// length) without holding the invocation's [`ScopeRef`], before it
/// allocates. `bytes` is a measured peak of scope memory, not a dlmalloc
/// footprint. Allocation-free. `Ok(true)`: admitted. `Ok(false)`: no scope is
/// current; nothing is admitted. A refusal is classed by cause like any
/// admission.
pub fn admit_current_heap(bytes: usize) -> Result<bool, Refusal> {
    let (id, epoch) = current::get();
    if id == pool::NONE {
        return Ok(false);
    }
    let route = pool::current_route(id, epoch).unwrap_or(0);
    pool::reserve(id, epoch, route, bytes, pages_of(bytes), Hold::Heap).map(|()| true)
}

/// Sets the pool clock (milliseconds) and sweeps for abandoned scopes. The
/// host calls it at every export entry, before anything allocates; it is
/// allocation-free. Returns the pool's health: when `Poisoned`, the host
/// records a durable fault keyed by its cause and recycles the instance
/// before changing state.
pub fn tick(now_ms: u64) -> Health {
    pool::set_clock(now_ms);
    pool::sweep()
}

/// The pool's health without sweeping.
pub fn health() -> Health {
    pool::health()
}

/// Boxes `make(handle)` inside `scope`: the box is the scope's admitted block.
fn box_in<F: Future>(scope: &Scope, make: impl FnOnce(ScopeRef) -> F) -> Result<Pin<Box<F>>, Refusal> {
    if size_of::<F>() != 0 {
        scope.admit_block(size_of::<F>(), align_of::<F>())?;
    }
    let h = scope.handle();
    Ok(scope.enter(|| Box::pin(make(h))))
}

/// Opens a scope for `route`, builds and boxes the handler future inside it,
/// and returns a future that polls it under the scope and closes the scope
/// when it completes. The route is an [`AsyncRoute`]: a handler that awaits
/// always has an abandonment deadline. The output must hold no scope heap
/// (turn errors into static codes, responses into JS objects); if it does,
/// the scope is quarantined and recorded. Sweeps for abandoned scopes first
/// (with the clock last passed to [`tick`]).
pub fn enter<F: Future>(route: AsyncRoute, control_bytes: usize, make: impl FnOnce(ScopeRef) -> F) -> Result<Entered<F>, Refusal> {
    let _ = pool::sweep();
    let scope = Scope::open(route, control_bytes)?;
    let fut = box_in(&scope, make)?;
    Ok(Entered { scope: Some(scope), fut: Some(fut) })
}

impl<F: Future> Entered<F> {
    pub fn scope(&self) -> Option<&Scope> {
        self.scope.as_ref()
    }
}

impl<F: Future> Future for Entered<F> {
    type Output = (F::Output, Closed);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let (Some(scope), Some(fut)) = (this.scope.as_ref(), this.fut.as_mut()) else {
            // Polled after completion: a host bug. Never panic (the panic
            // machinery would allocate); stay pending.
            return Poll::Pending;
        };
        let out = {
            let _p = PollGuard::begin(scope.r.id, scope.r.epoch);
            scope.enter(|| fut.as_mut().poll(cx))
        };
        match out {
            Poll::Pending => Poll::Pending,
            Poll::Ready(v) => {
                let fut = this.fut.take();
                scope.enter(|| drop(fut));
                let closed = this.scope.take().map(Scope::close).unwrap_or(Closed::Released { pages: 0, peak_live: 0, peak_pages: 0 });
                Poll::Ready((v, closed))
            }
        }
    }
}

impl<F: Future> Drop for Entered<F> {
    fn drop(&mut self) {
        if let Some(scope) = self.scope.take() {
            let fut = self.fut.take();
            scope.enter(|| drop(fut));
            scope.close();
        }
    }
}

/// A `waitUntil` continuation (design §2.5): a child scope of the invocation
/// that spawned it, its future boxed inside it, and its own glue ticket (its
/// js-sys task is glue that outlives the invocation's ticket). It may outlive
/// its parent: its pages then go back to the pool when it closes.
pub struct Continuation<F: Future> {
    entered: Entered<F>,
    _ticket: glue::Ticket,
}

impl<F: Future> Continuation<F> {
    /// Hands the continuation to the platform (`future_to_promise` and
    /// `ctx.wait_until`) with no scope current, so the task box the spawner
    /// allocates is glue under the continuation's ticket, not memory of the
    /// spawning invocation's scope.
    pub fn spawn(self, spawner: impl FnOnce(Self)) {
        glue::outside(|| spawner(self))
    }

    pub fn scope(&self) -> Option<&Scope> {
        self.entered.scope()
    }
}

impl<F: Future> Future for Continuation<F> {
    type Output = (F::Output, Closed);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().entered).poll(cx)
    }
}

/// Admits a `waitUntil` continuation before it is spawned: a glue ticket, a
/// child scope of `parent` carved from its lent pages (else from the pool)
/// with `control_bytes` of control footprint, and the future's box. Its
/// deadline is `route`'s from `now_ms`, the clock at spawn (for a
/// `waitUntil` continuation, [`limits::WAIT_UNTIL_MS`]). Refused, nothing is
/// spawned: the next delivery or the watchdog does the work.
pub fn wait_until<F: Future>(
    parent: &ScopeRef,
    route: AsyncRoute,
    control_bytes: usize,
    now_ms: u64,
    make: impl FnOnce(ScopeRef) -> F,
) -> Result<Continuation<F>, Refusal> {
    let ticket = glue::Ticket::admit()?;
    pool::set_clock(now_ms);
    let child = parent.child(route, control_bytes)?;
    let fut = box_in(&child, make)?;
    Ok(Continuation { entered: Entered { scope: Some(child), fut: Some(fut) }, _ticket: ticket })
}

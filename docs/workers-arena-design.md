# Bounded-arena memory for the Workers coordinator

This note designs memory management for the two Workers built from
`subnet/coordinator`: the coordinator Worker (`slop-ninja-coordinator-testnet`)
and the eval Worker (`slop-ninja-eval-testnet`). Both run as
`wasm32-unknown-unknown` with workers-rs. The rule implemented here is:
**no allocation happens outside a bounded memory arena.** The note covers:

1. the allocator architecture;
2. how the rule is enforced;
3. a zero-copy plan for each payload;
4. how each bound is derived and versioned;
5. dependencies that allocate internally, and the wasm-bindgen boundary;
6. the native validator and miner tools;
7. an ordered migration with tests and measurement gates.

A standalone prototype of sections 2 to 6 is in
[`subnet/crates/arena`](../subnet/crates/arena/README.md): the page pool with
physically held reservations (heap runs, one directly served run per admitted
block, and pages lent to child scopes), scope arenas with `u64` epochs and
epoch-checked handles, synchronous and async routes (an async route always
declares its deadline), child scopes and `waitUntil` continuations, the
bounded glue partition with tickets, quarantine and abandonment with a
quarantine budget, poison causes for the host's stop rule, cause-classed
refusals, strict mode, the stack measurement, the abort-hook postmortem,
exhaustion reporting and the zero-copy helpers. Its `glue-probe` measures
workers-rs 0.8.7 glue for every entry kind in local workerd, with every export
hand-written and ticketed, and its drill measures the JS heap through the
inspector and runs the durable fault protocol. Results are in that crate's
`MEASUREMENTS.md`. The coordinator now runs on it: section 8 ends with the
migration status.

## Evidence labels and revisions

| Label | Meaning |
| --- | --- |
| [R] | Code read at a pinned revision. **F** is `subnet-testnet/fanout` at `1e23b121`. **V** is `subnet-testnet/reward-v1` at `2481e461`. Dependency sources are the versions in the coordinator's `Cargo.lock`: worker 0.8.7, worker-macros 0.8.7, wasm-bindgen 0.2.129, js-sys 0.3.106, serde_json 1.0.154 and serde-wasm-bindgen 0.6.5. The dlmalloc 0.2.12 source was also read. The toolchain is rustc 1.99.1. |
| [M] | Measured on 2026-10-31 on the build host. A probe harness compiles F's `eval/score.rs`, `eval/receipts.rs` and `eval/package.rs` unchanged and reproduces the allocation pattern of `engine::prepare_package`, `cf_lanes::bundle_lane` and `engine::prepare_seal`. It ran natively with a counting allocator, and as `wasm32-unknown-unknown` under Node, where the dlmalloc linear-memory size was read after each run; those figures are Rust heap only. The arena prototype was then measured in local workerd 2026-10-23 (wrangler 4.154.0, Node 26.13.0): the glue probe (a workers-rs 0.8.7 Worker built with worker-build 0.8.7) and the drill, with the inspector reading the JS heap. Those runs are recorded in `subnet/crates/arena/MEASUREMENTS.md`. |
| [Q] | Cloudflare documentation, quoted with page dates in `data/task-roots/testnet/fanout/LIMITS.md`. |
| [C] | Computed from protocol encodings. |
| Design | What this note proposes. |

Provenance: paths under `data/` are the build host's ignored run records and
notes; the figures this design relies on are quoted here or in
`subnet/crates/arena/MEASUREMENTS.md`. Commit ids that are not on `main` (the
prototype's review rounds, such as `4962ae49` and `58d4de9d`) are on the
retained `workers-arena` evidence branch, which stays until this design's
measurements are re-run on the migrated build; the crate itself landed as
one squash commit ("Land the bounded-arena allocator crate and its design").

## 1. Facts the design rests on

### 1.1 Platform

- [Q] Memory is "128 MB per isolate", and the Workers limits page says it "is
  per-isolate, not per-invocation". Durable Objects of one class "may run in
  the same isolate on the same physical machine and share the 128 MB of
  memory".
- [Q] SQLite-backed Durable Objects allow a string, BLOB or row of at most
  2 MB, and at most 100 bound parameters per query.
- [Q] Alarm limits:
  - Alarms "are retried automatically when the `alarm()` handler throws",
    "with up to 6 retries allowed".
  - To retry indefinitely, a handler can catch the exception "and schedule
    a new alarm before returning".
  - "Only one instance of `alarm()` will ever run at a given time per Durable
    Object instance."
- [Q] Queue limits: messages of at most 128 KB, consumer batches of at most
  100 messages, and up to 100 retries.
- [Q] Each invocation can have at most 6 connections waiting for response
  headers. `waitUntil()` "extends execution for up to 30 seconds".
- [R] dlmalloc 0.2.12's wasm backend (`src/wasm.rs`) grows memory with
  `memory.grow` and returns `false` from `free`, `free_part` and
  `can_release_part`. Linear memory therefore only grows, so an isolate keeps
  its peak for as long as it lives. The crate also offers
  `Dlmalloc::new_with_allocator`, which takes a custom system allocator, and
  `destroy`, which hands every segment back to that allocator. Its system
  allocation granularity is 64 KiB, the wasm page size.
- [R] When an allocation fails, Rust calls `handle_alloc_error`, which aborts.
  On wasm the abort is an `unreachable` trap, not a recoverable error. The
  probe's panic path reproduced the trap [M].
- [R] worker-build 0.8.7 builds with panic recovery by default
  (`--experimental-reset-state-function --force-enable-abort-handler`), and
  worker 0.8.7's `init.rs` schedules a re-instantiation on abort. [M] In
  local workerd a trap fails the trapping request and every request in flight
  in the same isolate, and the next request runs on a fresh instance
  (section 2.6).
- [R] The re-instantiation is `new WebAssembly.Instance(..)` with fresh
  memory (the bundle's `O()`), so nothing in the dying instance's statics
  reaches the new one. Before it, the bundle's `re()` calls the handler
  registered with `wasm_bindgen::handler::set_on_abort` inside the dying
  instance; worker registers its own (`on_abort`, which schedules the
  re-instantiation) at start. A handler registered later and chaining to it
  can copy state out first [M] (section 2.6).
- [R] rustc's `wasm32-unknown-unknown` links the stack first:
  `[__stack_low, __stack_high)` is `[0, 1,048,576)` (rust-lld's default
  `-z stack-size`), below static data and outside every arena. Overflowing it
  is a trap that no admission can refuse.
- [M] A `FixedLengthStream` whose writer stops short of its declared length,
  with neither `close()` nor `abort()`, leaves its consumer pending: an R2 put
  of its readable side (miniflare's in-memory R2 in local workerd) was still
  pending after a 3 s bound, and so was a `Response` consumer. `abort(reason)`
  rejected the put in 3 ms and `close()` in 154 ms, and nothing was stored in
  either case (section 4.2).
- [M] The inspector's `Runtime.getHeapUsage` works in local workerd. It
  reports the V8 heap, the embedder heap and ArrayBuffer backing stores; the
  wasm linear memory is not among them.

### 1.2 Coordinator code (F)

- **No global allocator.** No `#[global_allocator]` exists in
  `subnet/coordinator` or `subnet/crates/protocol`. The only one is the
  test-only probe in `crates/protocol/tests/allocation.rs`. The wasm build
  therefore uses std's default, dlmalloc. The release profile sets
  `opt-level = "s"`, `lto = true` and `codegen-units = 1`.
- **State kept between invocations.** Two things:
  - The `PLANS` thread-local cache (`eval/engine.rs:545-566`). It is capped by
    count (`m.len() > 64` clears it), not by bytes, and every hit deep-clones
    a plan. Plans are up to 512 KiB of JSON.
  - `GenLimiter`'s `RefCell<Option<Limiter>>` (`cf_lanes.rs:1415-1443`). It
    holds `BTreeMap`s keyed by model names taken from requests.

  Nothing else is kept: there is no other `static`, `OnceLock`, `OnceCell` or
  `lazy_static`.
- **Entry points.** Three Worker events and six Durable Object classes:
  - Worker `#[event(fetch)]` (`cf.rs:777`) handles `/hook`, `/status`,
    `/fault/clear` and `/eval/*`.
  - Worker `#[event(scheduled)]` (`cf.rs:793`) and `#[event(queue)]`
    (`cf.rs:818`).
  - `ChainJournal` (fetch).
  - `EvalCohort` (fetch and alarm).
  - `AScorer` (fetch and alarm; the scoring Container lane).
  - `EvalLane` (fetch and alarm: generation, summary and bundle lanes).
  - `GenLimiter` and `SpendLedger` (fetch).
- **Macro glue that runs before handler code** (worker-macros 0.8.7):
  - Every Durable Object method and event is an `async fn` exported through
    wasm-bindgen. Its future is boxed by
    `js_sys::futures::future_to_promise`: one `Rc<Task>` holding
    `Box::pin(future)`, queued in a `VecDeque`.
  - Each Durable Object instance is stored as `Rc<WasmRefCell<T>>`.
  - Converting `web_sys::Request` into `worker::Request` allocates `method`
    and `path` strings before the handler body runs (`request.rs:50-60`).
- **Transactions.** `transaction()` (`cf.rs:106`) runs a closure inside
  `transactionSync` and rolls back by throwing from it.
  `engine::prepare_package` runs inside such a transaction
  (`cf_eval.rs:913`).
- **Failure paths a refusal can use** (all existing):
  - A lane alarm returns `LaneEnd::Rearm(ms)`.
  - A failed `EvalCohort` step increments `step_errors` and re-arms after
    30 s while the heartbeat is fresh (`cf_eval.rs:525-559`).
  - The queue consumer retries a failed kick after 5 s, and the sweep re-sends
    lost kicks (`cf_lanes.rs:1526-1542`).
  - The webhook route returns 503, which the provider redelivers, and the
    watchdog backfills any gap through RPC.
  - Cohort faults are durable and cleared by the operator.

### 1.3 Measured peaks (Rust heap, [M])

| Step and shape | Receipts | Peak live heap | dlmalloc linear memory |
| --- | --- | --- | --- |
| Package, J = 41, n = 1,000 | 41,000 | 18.73 MiB | 29.56 MiB |
| Package, J = 65, n = 1,000 | 65,000 | 30.12 MiB | 44.38 MiB |
| Package, J = 129, n = 1,000 (the R_0 + 128 shape) | 129,000 | 58.37 MiB | 83.31 MiB |
| Package, J = 257, n = 1,000 | 257,000 | 143.91 MiB | 177.31 MiB |
| Package, J = 129, n = 4,000 | 516,000 | 188.38 MiB | 245.31 MiB |
| Package, J = 257, n = 4,000 | 1,028,000 | refused with `PackageError("sign")` after the full build (native); trap (wasm) | not reached |
| Seal, n = 1,000, 1.3 KB texts | n/a | 10.67 MiB | 12.69 MiB |
| Seal, n = 4,000 | n/a | 42.46 MiB | 47.44 MiB |
| Bundle lane, n = 1,000 | 1,000 | 0.45 MiB | 2.19 MiB |
| Bundle lane, n = 4,000 | 4,000 | 1.82 MiB | 5.94 MiB |

- Linear memory before each run was 1.06 MiB, which is the probe module's
  static size, not the coordinator's.
- Linear memory grew 0.61 MiB per extra job between J = 65 and 129, and
  0.73 MiB per job between J = 129 and 257, at n = 1,000. The rising slope
  comes from the J² pairwise JSON in `components`.
- Taken alone, linear memory crosses 128 MB (122.07 MiB) at about 182k
  receipts at n = 1,000, by linear interpolation [C]. JS-side memory, which
  was not measured, lowers that point.
- The fan-out design (`data/task-roots/testnet/fanout/DESIGN.md` §5.2)
  projected about 250k.
- `subnet/coordinator/TESTNET.md` records the R_0 + 128 run
  (`load-s2-r0p128-n1000`) as 129,000 receipts packaged in 39 s, with no
  invocation failing on CPU or memory limits.

The package peak comes from four sources [R]:

- the J × n `TaskResult` records, which clone the `kind` and `source_group`
  strings for every task of every job (`engine.rs:1928-1943`);
- J² per-job `rms` JSON objects (`engine.rs:1997-2017`);
- `Record::digest`, which re-encodes the whole record into a `Vec` to hash
  it (`protocol/records.rs:75-77`);
- `package::signed_bytes`, which takes the index by value (so the caller
  clones it) and builds the envelope in two doubling buffers
  (`protocol/sign.rs:164-182`, `codec.rs:19-33`).

### 1.4 Encoded sizes [C]

| Object | Size |
| --- | --- |
| `AttemptReceipt` | 151 B (155 B with its chunk length prefix) |
| Receipt chunk object (32 receipts) | 4,960 B |
| Bundle | n × 155 B: 155,000 B at n = 1,000 (`TESTNET.md`: 20.0 MB for 129 bundles) |
| `ReceiptIndex` | 55 B + 69 B per entry: 8,901,055 B (8.49 MiB) at 129k |
| `Signed` envelope | record + 113 B |
| Largest index under `MAX_SIGNED_RECORD_LEN` (64 MiB) | 972,591 entries |
| Batch shard | 64 documents (`BATCH_SHARD_DOCS`) |
| Native T12 bundle (hex JSON) | about 443 B per receipt: 57,147,336 B at 129k, so the 64 MiB `MAX_BUNDLE_BYTES` holds about 151k receipts |

## 2. Allocator architecture (design)

### 2.1 Linear memory layout

```
0 ........... stack (`stack_bytes`, linked first) and static data, including the arena tables
heap_base ... glue partition: G pages (section 5.1), used only by the glue heap
heap_base + G × 64 KiB ... scope partition: P - G pages, held by scope admissions
heap_base + P × 64 KiB ... ceiling: the pool never grows past it
```

- One `#[global_allocator]` owns all heap memory. It is the only caller of
  `memory.grow`, and it grows only up to the ceiling.
- Because wasm memory never shrinks, an isolate's linear memory stays at or
  below `heap_base + P × 64 KiB` for its whole life.
- P is `floor(heap_ceiling / 65,536)`. Section 5.1 derives `heap_ceiling`.
  At the full 128 MB, P would be 1,953.
- The pool keeps four static tables, each sized for a whole isolate (1,953
  pages):
  - a page-owner map, one `u16` per page: free, glue (used or unused),
    outside the pool, or a scope id with flags: in scope `id`'s heap, in one
    of its block runs, or held for it (for its heap, as a block run, or lent
    to its children);
  - a free-page bitmap that tracks exactly the free scope pages;
  - a per-page carve stack (`u32`): which ancestor a page carved by a child
    scope returns to (section 2.4);
  - a per-page block table: the admitted layout and length of each block
    run, at its first page.
- Pages are 64 KiB, which equals both the wasm page size and dlmalloc's
  system granularity.
- **The stack** is linear memory too, outside the pool. Its size,
  `stack_bytes` (1 MiB, the link-time size), is a versioned memory-model
  input; at initialisation the pool paints the unused stack, and `stats()`
  reports the painted high-water (the lowest overwritten word). The model
  records the measured high-water over the build's routes and requires it to
  be at most half the stack; the clippy frame, future and stack-array lints
  take `stack_bytes / 2 - high-water` as their threshold (section 3.2), and
  large futures are boxed inside their scope (`enter` boxes the handler).

### 2.2 Arenas

**Scope arena: one per invocation.**

- A scope arena is a dlmalloc 0.2.12 instance built with
  `Dlmalloc::new_with_allocator`. Its system allocator hands out the pages
  the scope's admissions hold.
- **Admission holds pages physically**, in three kinds of contiguous run,
  each committed with `memory.grow` at admission:
  - **heap runs**: the state page plus the control footprint at open, and
    each admitted footprint, preferably right after the heap so dlmalloc
    merges them into one segment. Only the scope's own dlmalloc heap takes
    them. A segment the heap gives back returns to them, not to the pool,
    until the scope closes;
  - **block runs**: one per admitted block, for one allocation of exactly the
    admitted layout. That allocation is served directly from its run, never
    by the heap, so the heap's segment requests (in place or not) can never
    take a block's pages, and a block finds its run by its layout whatever
    order the scope allocates in. Freed, the run is held again for the same
    layout;
  - **lent runs**: pages the scope holds for its children, which only a
    child can carve (section 2.4).

  Nobody else can take a held page, so an allocation inside the admitted
  footprint never needs a page, a contiguous run or a `memory.grow` that
  might be refused later.
- The allocator's state (about 0.6 KiB on wasm32) lives at the start of the
  scope's first page, with a `u64` epoch unique for the instance's life, and
  the arena id is that page's index. Every handle carries the epoch, so a
  stale handle can never act on a later scope that reuses the page.
- Within a scope, freed memory is reused, so a long step's churn costs its
  peak live bytes plus fragmentation inside its own pages, not the sum of
  everything it ever allocated.

**Scope close.**

- If the scope's live byte count is zero, `destroy()` returns every segment
  to the pool. This is the per-invocation reset.
- If live bytes remain, a value outlived its scope. Releasing the pages
  would make that value dangle, so the scope is quarantined instead:
  - the heap is trimmed (`Dlmalloc::trim`) and every held page goes back, so
    only pages that still hold live bytes stay owned;
  - frees into it still work;
  - it is released when its live count reaches zero.

  Quarantine keeps the allocator sound whatever safe Rust does. It is
  recorded as a leak (section 2.8), counted against the quarantine budget
  (section 2.6), and strict mode fails on it.
- If the scope's future stops being polled past its route's deadline, the
  scope is **abandoned** (section 2.6): still open, its pages pinned and
  counted against the same budget.

**Glue arena: a bounded, admitted partition.**

- It receives allocations made while no scope is current. Those come from
  platform glue only:
  - each Durable Object instance's `Rc<WasmRefCell<T>>` (24 B measured);
  - each entry's js-sys `Task` and its boxed export future;
  - with the worker-macros exports, the `worker::Request` conversion made
    before the handler body opens a scope.
- It is a dlmalloc instance over a **fixed partition** of G pages at the
  bottom of the pool. Glue never takes a scope page and no scope ever takes a
  glue page, so glue usage never changes how a scope refusal is classed.
- **Admission.** Every export first takes a glue ticket, an allocation-free
  counter, before anything allocates: one per entry, so a Worker-to-Durable-
  Object hop takes one in each export and no ticket ever covers a call chain.
  When every ticket is in flight the export answers a JS-only 503 (an alarm
  rejects, so the platform retries it; a queue batch is retried; a cron tick
  resolves). A Durable Object constructor takes a resident token the same
  way, and a `waitUntil` continuation takes its own ticket (section 2.5). With tickets and residents bounded, glue live bytes are bounded by
  `glue_static + tickets × glue_ticket + residents × glue_resident`, which is
  the partition (section 5.1).
- This needs hand-written exports (section 2.5): the worker-macros exports
  allocate the task, the future box and the request conversion before any
  handler code can refuse.

**No long-lived application region.**

- Both kinds of application state that persist today move to Durable Object
  SQL, which is synchronous and survives eviction:
  - **`PLANS`** is replaced by normalized plan rows (section 4.2), so a hot
    route reads only the plan bytes it needs.
  - **`GenLimiter`'s token buckets and stats** become SQL rows keyed by the
    index of a priced model. `textgen::PRICES` is a static list, so the key
    space is fixed, and `/refund` and `/feedback` names are validated against
    it.
- Durable Object structs then hold only handles: `State`, `Env`, and
  `Result<u32, &'static str>` for the schema.

### 2.3 Which arena an allocation goes to

- The current arena is held in a static cell as `(scope id, epoch)`. wasm32
  without atomics is single-threaded.
- `Scoped<F>` wraps every handler future. On each `poll` it sets the current
  arena to its scope and restores the previous value on return.
  - Requests to one Durable Object can interleave at `await` points, so the
    current arena must follow the future being polled. A thread-local
    "current request" set once at entry would be wrong.
  - JS callbacks that run synchronously inside a poll, such as the
    `transactionSync` closure, stay in the scope.
- Each poll also marks its scope as being polled (a drop guard clears the
  mark on unwind), so an abandonment sweep never takes a scope mid-poll, and
  a poll of an abandoned scope recovers it.
- A handler future built inside `arena::enter` receives a `ScopeRef`
  (admission and entry, not closing), because the future lives inside the
  value that owns the scope. A `ScopeRef` is neither `Copy` nor `Clone`, and
  every use is checked against the epoch: a handle that outlives its scope
  (or whose scope is quarantined) allocates nothing and admits nothing. Its
  allocations fail as `StaleScope` and are counted; its admissions are
  refused `Defect`/`Stale`. Neither ever lands in a later scope that reuses
  the page.
- **`alloc`** goes to the current arena when `(id, epoch)` still names an
  open or abandoned scope, or to glue when no scope is current. An
  allocation of exactly an admitted block's layout is served from that
  block's run; anything else goes to the scope's heap.
- **`dealloc` and `realloc`** go to the arena that owns the pointer's page,
  whichever scope is polling. A value keeps its owner until it is freed, and
  growing it charges the owner: a block grows in place inside its run, else
  moves to the admitted block of exactly the new size, else to the smallest
  admitted block that holds it, else to the heap; a heap value growing to
  exactly an admitted block's size moves into that block.

### 2.4 Scope lifecycle and admission

The pool is a hierarchy of byte owners: the isolate (the pool, with its glue
partition and quarantine budget), each invocation (a scope opened by
`arena::enter`), and each step or round (a child scope carved from the pages
its parent lent).

| Operation | Behaviour |
| --- | --- |
| `Scope::open(route, footprint)` | Allocation-free. Holds one contiguous heap run of `scope_pages(footprint)` pages (state page included) and commits it. Refuses, classed by cause, when no such run exists. A `Route` gives a synchronous scope (no deadline); an `AsyncRoute` one whose deadline runs from the pool clock (section 2.5). |
| `scope.admit_block(len, align)` / `reserve(bytes)` | Allocation-free. Holds one contiguous block run of `block_bytes(len, align)` pages for **one allocation of exactly `(len, align)`**, served directly from the run, never by the heap. Freed, the run is held again for that layout until the scope closes. Used where an exact size becomes known: R2 object size, `Content-Length`, SQL `COUNT` or byte length, a JS string's or ArrayBuffer's length, a boxed future. An allocation of another layout is not the block's: it goes to the heap (an overdraft, refused in `Strict`). |
| `scope.admit_footprint(f)` | Allocation-free. Holds a heap run for a set of small allocations described by a `Footprint` (below), in place after the heap when free. |
| `scope.admit_js_string(len)` | Holds `block(len)` and `block(4 × len)`: wasm-bindgen's `passStringToWasm0` mallocs `len`, reallocs to at most `len + 3 × len` while encoding, then shrinks. The first is a block of exactly `len`; the grown copy moves into the smallest admitted block that holds it (the second); the shrink stays in place. |
| Buffers | Every variable-size buffer is created with `try_reserve_exact` after its admission. If the capacity differs from the size requested, that counts as a refusal. Growth admits the new block (exactly the new capacity) before reallocating; the old one keeps its run until the move. |
| `scope.lend(bytes)` | Allocation-free. Holds one run of whole pages lent to this scope's children: only a child can carve it. Sized from `child_bytes(control)` plus the children's own admissions. The scope's heap runs and block runs are never carved. |
| `scope.child(route, footprint)` | A child scope (a step, a chunk round, a `waitUntil` continuation) **charged against the parent's reservation**: its run, and every admission it makes, is carved from the pages the parent lent, so the lent pages shrink while the child is open and go back to them when it closes (the per-page carve stack routes them to the right ancestor through any depth). Only the part the lent pages cannot cover comes from the pool; the parent's own heap and blocks stay held for the parent. Its deadline is its own route's (from the pool clock now) when the route declares one, else the parent's. A child may outlive its parent (a `waitUntil` continuation); its pages then go back to the pool. |
| `scope.close()` | `destroy()` if live is zero (pages back to the parent they were carved from, else to the pool); otherwise trim and quarantine. |

**The route bound.** After every admission the pool sums the reservation of
the requesting scope and its ancestors. A total above the model's
`largest_route_bytes` means the model's derivation does not cover that route
(the quarantine budget assumes the largest route fits beside the pinned
pages), so it is recorded as a defect (`route_bound_exceeded`, per route
with the largest total seen) once per scope.

**Footprints, not byte counts.** A reservation must hold what dlmalloc
0.2.12 actually takes, not the bytes requested. With W the word size:
`chunk(req) = max(4W, align_up(req + W, 2W))`; an alignment above 2W goes
through `memalign`, which takes `chunk(chunk(len) + align + 3W)`; a segment
adds `top_foot + malloc_alignment = 12W`. `Footprint` sums the chunks of the
simultaneously live allocations, adds the largest chunk again (the segment
`sys_alloc` may request for it while the heap's top still has unused space)
and the segment overhead; `Footprint::growth` adds the old block of a
doubling buffer. A byte count undercounts small allocations: 16 Ki 16-byte
boxes take 512 KiB of chunks on a 64-bit host, twice their bytes. Per-route
control footprints come from this formula or from a witness test's measured
peak pages, never from peak live bytes.

**Refusals.** A refusal is a `Copy` value: route id, kind, reason, bytes
needed and available. Its kind is chosen by cause, never fixed per reason:

| Kind | When | Host action |
| --- | --- | --- |
| `Retry` | Pages held by other open scopes would satisfy it once they close; or a transient input condition (a truncated network read). | 503, redeliver, re-arm, queue retry. |
| `Capacity` | No wait can satisfy it: the need exceeds what the scope partition (`P - G`) leaves beside the requester's own pages and its ancestors' (the requester is waiting on them), by count or by contiguity (the largest run with only those pages counted as occupied is too short). Retrying the same invocation would reproduce it. It never depends on current glue usage. | Durable `arena_capacity` pause; the cohort is never skipped. |
| `Poisoned` | Pages pinned by quarantined or abandoned scopes, with the requester's own, block it, or the fragmentation they cause does. | Durable `arena_poisoned` fault keyed by the pool's poison cause, then the stop rule and a recycle at the next safe point (section 2.6). |
| `Invalid` | The input breaks its schema bound (a declared length above the bound, a stream longer than its bound or declared length). | 4xx; retrying it unchanged fails the same way. |
| `Defect` | A deterministic inconsistency: an encoder's output disagrees with its declared total, a pass-B re-hash disagrees with the signed root, the allocator fails inside an admitted footprint, or a handle is used after its scope closed (`Stale`) or for a future on a scope with no deadline (`Deadline`). | Durable `arena_defect` fault naming the route and digest. |

The I/O helpers take the buffer's cause (`Kinds::NETWORK`, `PLAN`,
`ENCODER`) so overflow and truncation get the right kind: network overflow is
`Invalid` and network truncation `Retry`; a plan-derived overflow is
`Capacity`; an encoder mismatch is a `Defect`.

### 2.5 Entry points and refusal paths

**Hand-written exports.** Every export is a non-async `#[wasm_bindgen]`
function with the macro export's signature (for `fetch`:
`(web_sys::Request, worker::Env, worker_sys::Context) -> js_sys::Promise`),
not the worker-macros `async fn`. In order, before anything allocates:

1. take a glue ticket (allocation-free); refuse with a JS-only 503 when none
   is free (`web_sys::Response` built from a `&str`, which JS copies out of
   linear memory);
2. `arena::tick(Date.now())`: set the pool clock and sweep for abandoned
   scopes, allocation-free; if the pool is `Poisoned`, record the durable
   fault and recycle (section 2.6) before any state changes;
3. `future_to_promise(async move { ... })`: the js-sys task and the future
   box are glue, under the ticket; the handler body is a single
   `arena::enter(route, ...)` call that converts the request, runs the
   handler and builds a `web_sys::Response` inside the scope. The response is
   a JS object and holds no Rust heap; errors become static codes before the
   scope closes.

The same order applies to every entry kind: the Worker `queue` and
`scheduled` functions (a refused batch is retried, a refused cron tick
resolves) and each Durable Object class, written by hand as a
`#[wasm_bindgen]` struct whose constructor takes a resident token and whose
`fetch` and `alarm` each take their own ticket (a refused alarm rejects, so
the platform retries it). The glue probe measured both forms on workers-rs
0.8.7 for every entry kind (section 5.1): a hand-written export costs 192 to
304 B of glue per in-flight entry (the js-sys task and the future box)
whatever the URL, while the macro exports cost up to 514 B, convert the
request in glue before any handler code runs (a 4 KB URL added 3.5 KB of
glue), and put a Durable Object hop's glue under the Worker's entry (979 B).

**Deadlines.** A scope that awaits always has an abandonment deadline: how
long it may stay open before it counts as abandoned when not being polled.
The type system enforces it. `arena::enter` and `arena::wait_until` take an
`AsyncRoute`, whose deadline is mandatory and non-zero (a zero deadline in a
route constant is a compile error); a `Route` (from `Route::new`) has none and
serves only synchronous scopes (`Scope::open` with `enter(|| ..)`), which
cannot hold a future: `ScopeRef::scoped` refuses one (`Defect`/`Deadline`).
The deadline is the platform wall limit of the entry kind where one is
documented ([Q]: `waitUntil` 30 s; cron, queue and alarm handlers 15 min;
"Incoming HTTP request: Unlimited"; the crate's `limits` module). For HTTP and
Durable Object fetch routes, which have no wall limit, it is the route's own
bound: every platform await carries an `AbortSignal` timeout, and the
deadline is the sum of the route's sequential await timeouts plus the CPU
limit (30 s by default [Q]). An await without a timeout is a lint failure
(section 3.2).

A child scope takes its own route's deadline (from the pool clock when it
opens) when its route declares one, else its parent's. A `waitUntil`
continuation is `arena::wait_until(parent, route, footprint, now, make)`: it
takes its own glue ticket, admits a child scope (carved from what the parent
lent, else from the pool) with the 30 s limit from the clock at spawn, and
boxes the future inside it before anything is spawned; `Continuation::spawn`
then hands it to `future_to_promise` and `ctx.waitUntil` with no scope
current, so the task box is glue under the continuation's own ticket. The
continuation may outlive the invocation that spawned it.

| Entry point [R] | Scope | Refusal uses (existing path) |
| --- | --- | --- |
| Worker `POST /hook` (`cf.rs:492`) | One scope. The `waitUntil` continuation is `arena::wait_until` (a child scope with its own ticket and the 30 s deadline from the clock at spawn), admitted before it is spawned. | 503, so the provider redelivers. A refused continuation is not spawned, and the next delivery or the watchdog drives the RPC round. Oversized bodies are `Invalid` (413). |
| Worker `GET /status`, `POST /fault/clear`, `/eval/*` | One scope | 503; `Invalid` as 4xx |
| `scheduled` (`cf.rs:793`) | One scope (deadline 15 min) | Return. The next minute's tick does the work. |
| `queue` (`cf.rs:818`) | One scope per batch (deadline 15 min) | `retry_all` (allocation-free) and return `Ok`. The handler never returns `Err`: worker-macros turns that into a panic, which is a trap (section 2.6). The existing sweep re-sends lost kicks. |
| `ChainJournal` fetch (`cf.rs:207`) | One scope per request | 503 `store_unavailable`. Callers already retry. |
| `EvalCohort` fetch: `/kick`, lane routes, `/plan`, `/status` | One scope per request | 503. Lanes `Rearm`; a kick repeats on the next block. |
| `EvalCohort` alarm (`cf_eval.rs:525`) | Alarm scope (deadline 15 min) plus a child scope per step | `Retry` goes through the existing `step_errors` count and 30 s re-arm. `Capacity` raises the durable `arena_capacity` fault: the cohort pauses and is never skipped. `Poisoned` and `Defect` raise their durable faults (section 2.6). |
| `AScorer` fetch and alarm (`cf_lanes.rs:205`, `:258`) | Per request; the alarm opens a child scope per chunk round | `LaneEnd::Rearm`, or the existing claim hand-back. |
| `EvalLane` fetch and alarm (`cf_lanes.rs:1024`, `:1071`) | Per item or lane step | `LaneEnd::Rearm` |
| `GenLimiter`, `SpendLedger` fetch | One scope per request | 503 |
| Durable Object `new()` | A hand-written class constructor: a resident token first, then a synchronous scope around `migrate_storage`. The class's `fetch` and `alarm` each take their own ticket. | Out of resident tokens: the constructor throws a JS string and the caller retries. A migration that fails sets the existing `schema: Result` field, so `fetch` returns 503. A refused constructor scope does not: the migration runs in the State home (an overdraft there, counted), and a refused State home builds the object with no scope current (glue, counted), because a constructor runs once per instance and a refusal kept in `schema` would answer 503 for the instance's whole life. |

**Long alarms.** A lane alarm can run for a 600 s step budget. It holds a
reservation only per chunk round (claim, score, put, commit), so lanes placed
in the same isolate are not starved by idle waits.

### 2.6 Exhaustion, traps, abandonment and recycling

**What a trap does in this build** ([R] worker-build 0.8.7, [M] local
workerd). worker-build's default panic recovery adds
`--experimental-reset-state-function --force-enable-abort-handler`; worker's
`init.rs` registers an abort handler that schedules a re-instantiation. In the
glue probe, a request that trapped answered 500; a second request in flight in
the same isolate also failed (500, then workerd cancelled it as hung, since
its promise belonged to the dead instance and never settled); the next request
ran on a new instance with a fresh pool. So:

- one trap aborts every co-resident in-flight invocation of the isolate, not
  only the trapping step;
- the old instance's linear memory stays reachable until V8 collects it
  (section 2.7 records what local workerd showed);
- a deterministic trap in a step repeats on every retry unless something
  records it.

**The guarantee.** Inside an admitted footprint no allocation can fail:

1. admission holds the pages physically and commits them (no later
   `memory.grow`, no page another scope can take, and a child carves only
   pages its parent lent, never the parent's heap or blocks);
2. each block's run is contiguous and serves exactly the allocation it was
   admitted for, directly: the heap never takes block pages, so an admitted
   block finds its run whatever order the scope allocates in (a 4,000-seed
   fuzz with fragmenting blockers, every admitted allocation live at once in
   random order, `Strict`: no refusal);
3. footprints count dlmalloc chunks and segment overhead, not bytes;
4. every variable-size buffer goes through `try_reserve_exact` after its
   admission, so a refused growth is an error value, not a trap;
5. glue is admitted per entry by tickets into a partition sized from
   measured per-entry costs.

An allocation beyond its footprint is a derivation defect. In `Enforce` mode
it is served from free pages if any and recorded as an overdraft fault; in
`Strict` mode (tests, drills) it fails. **Residual risk**, stated: a trap can
still happen if (a) a footprint underestimates a pattern no witness test
covers, while no free page is left; (b) a dependency makes an infallible
allocation the admissions did not account for (the source scan and witness
tests reduce, not eliminate, this); (c) a glue cost exceeds its measurement
(an entry kind or request shape the probe did not exercise); (d) a stack
frame beyond the measured bound (section 2.1; the clippy stack lints reduce
this). Each case is recorded and handled by the step-crash protocol below.

**The postmortem.** The new instance has fresh memory, so the old instance's
counters do not survive the trap by themselves. The host registers an abort
hook (`wasm_bindgen::handler::set_on_abort` at its first export entry,
chained to worker's own handler, which schedules the re-instantiation). In the
dying instance it reads `arena::postmortem()`, an allocation-free snapshot of
the current scope and route, the last allocation failure, the recycle cause
if the trap was a recycle, the pinned pages and the stack high-water, and
passes its words to JS as scalar arguments (a JS global or a pre-allocated
array). The new instance's first entry records them durably beside any stale
step marker. The glue probe measured it in both builds: after a trap in the
Worker fetch route, the next instance read a postmortem naming that route.

**Step-crash protocol (durable, never a skip).** A step writes
`step_until_ms` before it runs and clears it after. A stale marker found at
alarm or kick time means the step did not finish. It is attributed first:

- to a recycle, when a durable recycle intent (below) was written after the
  marker: the step is re-run without counting a crash of its own;
- to another route's trap, when the postmortem names a different route: the
  same;
- otherwise to the step itself: the cohort records a durable crash count
  keyed by step and plan digest, re-runs the step once, and on a second crash
  raises a durable `arena_trap` pause.

An innocent step aborted by another invocation's recycle or trap is therefore
never paused for it. The drill exercises this protocol (section 8).

**Abandonment.** A future that the platform abandons keeps its scope open:
the js-sys task holds itself through its waker until the future completes, and
a pending `JsFuture`'s callbacks hold it until the promise settles, so an
unsettled promise pins the scope forever. Rules:

1. every scope that awaits has a deadline (an `AsyncRoute`), and every
   platform await settles within it (an `AbortSignal` timeout on fetch, R2
   and stub calls; never a `select` with `Delay`, which drops a pending
   `JsFuture` and leaves its callbacks holding scope memory);
2. at every entry, `arena::tick` marks open scopes whose deadline passed while
   not being polled as **abandoned**: counted per route and pinned;
3. a late poll (the promise settled after all) **recovers** the scope; if the
   platform drops the task, the `Entered` drop guard closes the scope inside
   it and the scope is **reclaimed**;
4. per-await `JsFuture` state may live in a small child scope, so a stray
   callback pins one page.

**Quarantine budget and recycling.** Pages pinned by quarantined and abandoned
scopes count against the quarantine budget: the scope partition minus the
largest declared route's footprint (section 5.1), so pinned pages within the
budget never stop the largest route from being admitted by page count. Past
the budget, or as soon as a refusal is classed `Poisoned`, the pool reports
`Poisoned { pinned_pages, cause }`, where the cause names the route whose
scopes pin the most pages and how (`Quarantined` or `Abandoned`; `Refusal`
when a latched `Poisoned` refusal outlived its pins). Nothing in the instance
can return those pages, so the host, at the next safe point (an export entry,
before any state change):

1. writes a durable **recycle intent**: the cause's route and kind, the
   memory-model version, the pinned pages, the plan digest and the time.
   Co-resident steps the recycle aborts are attributed to it (step-crash
   protocol above);
2. applies the **stop rule**, keyed by (route, kind, memory-model version)
   and counted in Durable Object SQL across steps and instances: on the
   second recycle for the same key it raises a durable `arena_leak` pause for
   that route, which an operator clears. That second recycle still runs (only
   it frees the pinned pages, and other routes need them); the pause stops a
   third. The paused route's invocations are refused (their work waits;
   nothing is skipped), so a route that leaks on every invocation recycles
   the isolate at most twice, not indefinitely;
3. recycles: a deliberate trap (`arena::recycle(cause)`, which records the
   cause for the postmortem), so worker-build's re-instantiation gives a
   fresh pool (for a Durable Object, `state.abort()` has the same effect).

The pool never answers `Retry` for pages only a recycle returns.

**Transactions.** `arena::transaction(scope, bound, f)` is the only entry into
`transactionSync`, and nothing allocation-heavy runs inside one:

- It first checks that the scope has `bound` bytes of headroom, and refuses
  before entering.
- `prepare_package` moves out of the transaction. Synchronous reads see a
  consistent state because no other request runs during synchronous code.
- Only the final write runs inside, and it uses pre-sized buffers.

If a derivation is wrong and a trap still happens inside the closure, the
re-instantiation abandons the transaction; whether `transactionSync` rolls
back in that case is confirmed by the squeeze test in migration step 1. R2
writes use deterministic keys and are reconciled by the existing design.

### 2.7 JS-side memory

**What cannot be in the arena.** These live outside linear memory and no Rust
allocator can redirect them, yet they count toward the same 128 MB:

- R2 `arrayBuffer` and stream chunks;
- request and response bodies;
- SQL cursor values;
- `Uint8Array` copies;
- Workers AI results;
- Queue messages.

**What the measurement showed** ([M] 2026-10-31, local workerd 2026-10-23,
inspector `Runtime.getHeapUsage` after every invocation, collections forced
at intervals). The drill's handler receives its body as a request body (an
ArrayBuffer, as an R2 object or a request body would be) and writes its bundle
to an exact-size JS buffer, so its per-invocation JS need is body plus bundle.
The V8 heap stayed at 1.6 to 2.1 MiB; the backing stores of dead ArrayBuffers
are what grows, because V8 frees external memory on its own heuristics, not
when the invocation ends:

| Receipts | Per-invocation JS need | JS high-water (heap + backing stores) | Ratio |
| --- | --- | --- | --- |
| 1,000 | 0.42 MB | 10.5 MB | 24.9 |
| 4,000 | 1.70 MB | 19.0 MB | 11.2 |
| 8,000 | 3.39 MB | 36.0 MB | 10.6 |
| 13,000 | 5.51 MB | 57.2 MB | 10.4 |
| 20,000 | 8.48 MB | 77.9 MB | 9.2 |
| 26,000 | 11.0 MB | 79.2 MB | 7.2 |
| 41,000 | 17.4 MB | 88.5 MB | 5.1 |
| 65,000 | 27.6 MB | 111.8 MB | 4.1 |

Up to about 13,000 receipts the backlog held about ten invocations' worth;
above that it saturated near 62 MB plus about twice the need (V8's
external-memory threshold). A forced collection did not always release it
(the 13,000-receipt run's minimum after collection equalled its maximum).
These are 40- to 200-invocation runs, and they are not maxima. Long runs
(inspector after every invocation, a forced collection every 100; [M]
2026-10-31) kept rising well past them:

| Run | Invocations | Shapes (receipts) | JS high-water | Last new maximum at |
| --- | --- | --- | --- | --- |
| drill, package | 1,000 | 26,000 | 86.4 MB | invocation 690 to 718 |
| drill, streamed package | 1,000 | 26,000 | 90.5 MB | 36 to 66 |
| steady package | 5,000 | 26,000 | 86.7 MB | 354 |
| steady streamed package | 5,000 | 26,000 | 90.0 MB | 12 |
| steady package | 5,000 | 20,000 | 83.7 MB | 2,832 |
| steady streamed package | 5,000 | 20,000 | 78.6 MB | 23 |
| mixed, small shapes first | 10,000 | 3,000 at 1,000, then 1,000 to 26,000 | 83.5 MB (44.6 MB in the small phase) | 3,440 and 4,040 |

The 26,000-receipt envelope's 80-invocation high-water (79.7 MB) was
exceeded by 10.8 MB, more than the one-invocation margin, and the steady
package run set a new maximum after 2,832 invocations. Per-scope JS admission
therefore cannot bound the JS heap; the reserve must include the backlog,
measured over runs long enough to show that it stopped rising.

**How the design bounds it.**

- Code never asks the platform for a buffer of unknown size. It reads
  streams through fixed buffers and checks declared sizes first. Whole-body
  JS buffers are what makes the backlog large: at 65,000 receipts the JS
  high-water alone (111.8 MB) plus the route's 36 MB of linear memory is
  above the isolate limit. The streamed design of section 4 keeps the JS need
  per invocation at a chunk or a row.
- `js_reserve_bytes` is the measured JS high-water with its garbage backlog
  (V8 committed heap, backing stores and the embedder heap) over long runs
  (thousands of invocations, steady at the **envelope** shape and mixed with
  small shapes first) after which it no longer rises, plus one more
  invocation's JS need as margin. It is never zero, and the build refuses an
  unmeasured value outside measurement builds.
- `js_envelope_bytes` is that shape's per-invocation JS need. An invocation or
  plan needing more is outside the envelope the reserve was measured for and
  is refused at `/plan` (in the drill, before the handler allocates).
- The envelope is the largest measured shape whose reserve leaves a pool that
  holds the largest route and a quarantine budget at least as large. For the
  drill that is 20,000 receipts: the long-run high-water of 83.7 MB plus
  8.5 MB of margin gives a 92.2 MB reserve, a 528-page pool, a 163-page
  largest route and a 364-page quarantine budget. 26,000 receipts would need
  a long-run high-water of at most 88.2 MB (210-page route); it reached
  90.5 MB, so it does not fit.
- The coordinator's envelope is measured at migration step 0 under its own
  shapes, and the testnet gate (step 9) re-checks it with `exceededMemory`
  outcomes.

**After a trap.** In the drill's trap run (13,000 receipts, an 8.8 MB
instance, ten re-instantiations, collections forced every ten invocations)
workerd's resident set grew by 25 MB in all, so the old instances' memories
were collected rather than kept. The platform's timing is not established
(section 9); the reserve does not count on it.

### 2.8 Observability

- **In-memory counters.** Fixed static per-route counters: invocations,
  maximum reservation, peak live bytes, peak pages, refusals by kind
  (`Retry`, `Capacity`, `Poisoned`), abandonments, overdrafts, quarantines
  and allocation failures. Isolate counters: `memory.size`, pool high-water,
  free and held pages, glue partition use, glue live and peak bytes and
  footprint, tickets in flight and refused, pinned pages against the
  quarantine budget, open, quarantined and abandoned scopes, recoveries and
  reclaims, carved children, blocks served and moved, stale-handle entries,
  allocations and admissions, route-bound excesses (with the largest
  invocation total per route), recycles, the linked stack and its painted
  high-water, observed excess (Observe mode), the pool's health with its
  poison cause, and the last allocation failure (copied out of a dying
  instance by the abort hook's postmortem, section 2.6).
- **Durable copy.** A Durable Object writes a route's row to its SQL only
  when a new maximum is observed. After warm-up that is rare, so row-write
  billing stays bounded.
- **Worker routes** log new maxima to the Worker tail.
- **Status.** `/status` and `/eval/status` add an `arena` section with these
  values and the memory-model version.

## 3. Enforcement (design)

### 3.1 Strict mode

The `arena-strict` cargo feature turns every bound violation into a failure.

**Native tests.**

- The test binary installs the same page pool and per-scope dlmalloc code
  over one `System`-allocated region of the ceiling's size, so the allocator
  logic is identical on both targets. Only `size_of` differs; formulas use it
  for the build target.
- On an armed thread, the following are recorded:
  - any allocation while no scope is current;
  - any overdraft;
  - any scope closing with live bytes.
- The test harness `strict_test!` arms the thread, runs the body inside a
  scope with its declared bound, and panics on any record. It also asserts
  that the peak stays within the declaration.
- The strict suite runs with one test thread.
- The existing seams, the storage-neutral `Sql` trait (with rusqlite in
  tests) and `LaneIo`, keep production logic testable natively.

**Wasm (local workerd).**

- Glue allocations are legitimate outside scopes, so they are counted rather
  than failed.
- A strict-only, operator-gated route `/__arena` returns the counters.
- The drill (section 3.3) fails on:
  - any overdraft or quarantine;
  - glue growth across invocations;
  - linear memory above the ceiling.

**Witness tests.**

- A counting probe records the admitted bytes at each allocation. The tests
  assert:
  - admission happens before the allocator call;
  - a reservation one byte short refuses with zero allocations;
  - every refusal path performs zero allocations;
  - a route's peak **pages** stay within its admitted footprint (peak live
    bytes undercount dlmalloc chunks), including for adversarial inputs
    inside the schema bound (malformed JSON, unknown keys, strings where
    numbers belong).
- Each migrated route has one causal test that fails on the pre-migration
  revision (checked in a `git archive` copy) and passes after it.

### 3.2 Lint

Two layers, both run locally.

**Clippy configuration** (`subnet/coordinator/clippy.toml`; the prototype
carries the same lists in `subnet/crates/arena/clippy.toml` and
`glue-probe/clippy.toml`):

- `disallowed-methods`:
  - `worker::Request::{bytes,text,json}`, `worker::Response::{bytes,text,json}`
    and `worker::ObjectBody::{bytes,text}`;
  - `worker::FixedLengthStream::wrap` and `worker::Context::wait_until`;
  - `js_sys::futures::{future_to_promise,spawn_local}` outside the export
    wrappers;
  - `js_sys::Uint8Array::{view,view_mut_raw}`: a view of linear memory can
    outlive its buffer or be detached by `memory.grow` while an async JS
    consumer holds it (section 6);
  - `futures_util::future::select`: a dropped pending `JsFuture` keeps its
    callbacks and scope pages until the promise settles (section 2.6);
  - `wasm_bindgen::closure::Closure::{forget,into_js_value}`;
  - `Box::leak`, `Vec::leak`, `String::leak` and `std::mem::forget`;
  - `std::panic::set_hook` (the hook's box outlives the scope it is
    installed in);
  - `serde_json::{to_value,from_value}`.
- `disallowed-types`: `serde_json::Value`, `serde_json::Map`,
  `worker::ByteStream`, `std::sync::OnceLock` and `std::cell::OnceCell`.
- `disallowed-macros`: `serde_json::json` and `std::thread_local`.
- The stack lints `large_stack_frames`, `large_futures` and
  `large_stack_arrays`, warned at each crate root, with
  `stack-size-threshold`, `future-size-threshold` and
  `array-size-threshold` derived from the build's memory model:
  `stack_bytes / 2 - stack_high_water_bytes`, so the deepest measured stack
  plus any one new frame, future or stack array stays within half the linked
  stack (section 2.1).
- Tests and the `arena` and `cfio` modules may allow these locally with a
  stated reason.

**Test knobs are features.** `set_mode`, `set_page_limit`, `shrink_free_to`
and `reset_latches` exist only with the arena's `knobs` feature (enabled by
`selftest` and `strict`). A production build fixes the mode by feature
(`observe`, default `Enforce`) and the limit by the memory model, and a
manifest check refuses `knobs`, `selftest`, `strict` or `unmeasured-model` in
a production crate's features.

**Source-scan test** (`subnet/tests/no_unbounded_alloc.rs`, modelled on the
existing `no_runtime_gates` lint; the prototype's is
`subnet/crates/arena/tests/source_scan.rs`):

- Every export (Worker `fetch`, `queue`, `scheduled`; each Durable Object
  class's `fetch` and `alarm`) is hand-written and takes its glue ticket as
  its first statement, and each Durable Object constructor its resident
  token; worker-macros exports exist only in a comparison build. Each body is
  a single `arena::enter` call.
- No `Route::new` is passed inline to `enter`, `wait_until` or `scoped` (the
  types already require an `AsyncRoute` for the first two).
- The clippy stack thresholds equal the model's derivation.
- Infallible allocation APIs (`format!`, `to_string`, `to_owned`, `vec!`,
  `Vec::with_capacity`, `push`, `push_str`, `collect`, `Box::new`, `clone`,
  `into_owned`) are flagged in modules that handle variable-size input unless
  the line carries an `alloc-ok:` justification (inside an admitted
  capacity, or a fixed-size value inside an admitted footprint).
- `Uint8Array::view`, `view_mut_raw`, `set_hook`, `select`, leaks and test
  knobs are flagged everywhere outside tests.
- Durable Object struct fields come from a fixed allow-list of handle types.
- No `static` or `thread_local!` holds a heap type, except the arena's own
  tables.
- No `#[global_allocator]` appears outside the arena crate's binaries.
- Every platform await carries an `AbortSignal` timeout.
- No bare `#[test]` appears in coordinator production-path test modules
  (they use `strict_test!`).

### 3.3 Local check (no CI)

`arena-check` is a Rust tool run on the build host. It runs as a durable job
under `systemd-run` with `MemoryMax`, `MemorySwapMax=0`, one Cargo job, a
task-owned `CARGO_TARGET_DIR` and the shared `data/locks/cargo.lock`. Steps:

1. Clippy on native and wasm32 with the configuration above.
2. The source-scan lint.
3. The native strict suite.
4. A wasm32 `arena-strict` build.
5. The glue probe: worker-build output served by `workerd serve` on
   127.0.0.1, glue per entry kind at 1 to 16 in-flight invocations, a ticket
   burst and a trap with a co-resident request (prototype:
   `glue-probe/run/drive.mjs`).
6. The local workerd drill:
   - `workerd serve` on 127.0.0.1 with synthetic secrets and a task-owned
     state directory (prototype: `drill/worker/drive.mjs`).
   - The `arena-drill` feature replaces Container calls with a deterministic
     in-Worker stub, so no Docker is involved.
   - 1,000 consecutive invocations per entry point, reading `/__arena`
     after each one, including abandoned futures, traps with the step-crash
     protocol, quarantine with recycling and the JS envelope.
   - With `--inspect`, the inspector reads `Runtime.getHeapUsage` after
     every invocation and forces a collection at intervals.
7. Writes its report under `data/task-roots/testnet/workers-arena/runs/`.

It issues no live model, detector or chain-write calls. GitHub Actions is not
used, and commits carry `[skip ci]`.

## 4. Zero-copy plan per payload (design)

### 4.1 Building blocks

The `cfio` module calls `worker_sys`, `web_sys` and `js_sys` directly where the
worker helpers buffer whole bodies:

- **Bounded body reader.**
  - Checks the declared length first, using `Content-Length` or the R2 object
    size.
  - Reads the stream with a default reader and copies each chunk into an
    admitted buffer with `Uint8Array::subarray(..).copy_to(..)`, whatever the
    platform's chunk size.
  - Once the buffer is full it reads once more into a one-byte stack probe.
    Any byte there refuses the body: `Bound` for a stream with no declared
    length (past its schema or input limit), `Overflow` past a declared
    length. A body is never accepted truncated, with or without a declared
    length.
  - Cancels the JS stream on refusal.
- **Exact-size R2 get.** Checks `size` before touching `body()`, admits
  exactly that size, copies, and refuses on truncation. Ranged reads are used
  for headers.
- **Streamed R2 put.**
  - Builds `worker_sys::FixedLengthStream::new(len)` directly, passes its
    readable side to `put`, and writes chunks from the scoped future through
    its writer, awaiting each write.
  - This avoids worker's `FixedLengthStream::wrap`. That helper is pulled by
    a JS callback outside the scope (through `wasm_streams`) and copies a
    `Vec` per chunk.
  - Each JS copy comes from an arena slice; there is no Rust `Vec` per chunk.
  - The stream is never left short of its declared length. `io::ChunkSink`
    takes the writer's `write` and `abort` together; every refusal (an
    overflow, a failed write, a short total at `finish`, or the sink dropped
    unfinished after an early return) calls `abort` once, bound to
    `writer.abort(reason)`, and the host then awaits the put's rejection
    inside the scope with an `AbortSignal` bound. Measured with an R2 binding
    (miniflare in local workerd): abort rejected the put in 3 ms and stored
    nothing, while a writer left short kept the put pending past a 3 s
    bound (section 1.1).
- **Row visitor.**
  - `Sql::exec` returning `Vec<Row>` becomes
    `Sql::query(q, params, visit)` over the raw cursor iterator, one row at a
    time.
  - Integers are read without allocating. A text value is copied into an
    admitted buffer after a length check; a UTF-8 length is at most 3 × the
    UTF-16 length.
  - Digests become 32-byte BLOB columns read into stack arrays. SQLite
    compares BLOBs bytewise, so `ORDER BY attempt` matches the protocol's
    order.
  - Paging is keyset-based. A page's row count is
    `scope budget / row bytes`.
- **Streaming encoder.**
  - The protocol `Encoder` writes to a `Sink` trait instead of a `Vec`. Sinks:
    a counter, which gives exact `encoded_len`; `Sha256`; a bounded buffer;
    and a chunk writer that feeds a stream.
  - `Record::digest` streams into `Sha256`, and `Record::domain()` stops
    allocating a `String`.
  - `Signed::to_bytes` is replaced by a streaming writer. The envelope layout,
    `str("signed") || u16 || u8 role || u32 len || record || key || signature`,
    puts the signature after the record, so one pass streams the record to
    both the hasher and R2, and the key and signature go last. Ed25519 is
    deterministic, so the bytes are identical to today's.
- **Two-pass JSON.** Typed `Serialize` structs with borrowed fields are
  written once to a counting writer and once to an exact-size buffer or a
  `FixedLengthStream`. This replaces `json!` and `Value` trees.
- **Borrowed decode with bounded errors.** serde_json allocates in
  proportion to the input on its error and escape paths: `deny_unknown_fields`
  formats the unknown key, an `invalid type` message embeds the whole
  offending string (serde_json's `peek_invalid_type` does this for every typed
  `deserialize_*` call, whatever the visitor), and an escaped string goes
  through a scratch buffer. The decoder therefore:
  - pre-scans the body allocation-free and refuses (`Invalid`) any byte the
    schema never needs: a backslash (no schema string needs an escape, so the
    scratch buffer is never used), a sign, fraction or exponent, or a digit
    run longer than the schema's widest number;
  - reads every position with `deserialize_any`, so a type mismatch reaches
    the visitor, and gives every visitor a static message for a string it
    does not expect and for an unknown key (a field-identifier visitor
    replaces `deny_unknown_fields`);
  - admits a control footprint derived from what remains: serde_json's boxed
    error plus one message of at most `MAX_ERROR_MESSAGE` bytes (a static
    message, or `invalid type: integer `N`, expected E`), with its doubling
    growth. The drill's witness tests measured 0 to 202 B against the derived
    bound for every adversarial class, including the review's 300 KB unknown
    key and 2,000-times escaped string.
  - `#[serde(borrow)]` structs with `&str` and `&RawValue` fields replace
    `Value`-first parsing.
  - A declared SCALE or canonical sequence count is accepted only if it is at
    most `remaining_bytes / minimum element size`.
- **JS strings and values.**
  - `JsString::length()` is checked before any copy. For Workers AI output,
    `response` is read with `Reflect::get` from the raw result rather than
    through `serde_wasm_bindgen::from_value` into a `Value`.

Buffer sizes (64 KiB stream chunks) trade CPU for memory and limit no input.

### 4.2 Payloads

| Payload (F) | Today [R] | Design |
| --- | --- | --- |
| Webhook body (`/hook`), at most 256 KiB | Content-length pre-check only when present; then the whole body is read with `req.bytes()`; `Value` tree (`webhook.rs:354`); re-serialized to JSON for `/ingest` | Bounded reader into a 256 KiB buffer of exact capacity. HMAC updated per chunk. Borrowed typed parse of the challenge and envelope. `/ingest` receives a fixed-size canonical encoding written into a stack buffer and passed as one `Uint8Array` copy. |
| RPC replies, at most 64 KiB | `response.bytes()` unbounded when `Content-Length` is absent; checked afterwards (`cf.rs:397-406`, `rpc.rs:62-64`) | Bounded reader. Typed borrowed parse, with `RawValue` for passed-through parts. The request body is formatted into a fixed buffer. |
| Operator bodies (`/fault/clear` 1,024 B, `/eval/*` 512 KiB) | Whole body read, then checked (`cf.rs:667-669`, `:705-711`) | Bounded reader. |
| Internal Durable Object requests (`/ingest`, `/kick`, lane routes) | `req.bytes()` or `req.json()` with no bound (`cf_eval.rs:574`) | Canonical binary with a declared length. Exact-size read. Route bound from the schema and `lane_docs`. |
| Plan (`/plan`, at most 512 KiB) | Parsed twice; up to 3 stored copies; `PLANS` deep clones on every hot call (`engine.rs:545-566`, `:762-785`) | Parsed once inside the `/plan` scope and normalized into a header row plus one row per job. The text is stored once, for its digest. Hot routes read the header and one roster row. |
| Lane claim, commit and report replies | `serde_json::to_value`, then `to_string`, then a JS copy (`lanes.rs:1987-1989`) | Typed structs written into a bounded buffer; one JS copy. |
| `job_objects` and `job_receipts` | `job_objects` unpaged; receipts in pages of 5,000 hex-string pairs (`lanes.rs:1659-1682`) | Both keyset-paged at `budget / row bytes`, with BLOB digests. |
| Pool manifest (draw) | `get_bytes` with no size check, then fully parsed (`cf_eval.rs:134-142`) | Exact-size admission from the object size. The format becomes JSON Lines, read one line at a time into a line buffer whose length is bounded by the schema. |
| Used ledger (draw) | Every `used/*.json` ever written, read and parsed (`cf_eval.rs:155-176`) | Each entry streamed with exact-size admission into a used-group bitset, sized by the pool's group count. |
| Pool shards (draw) | Every row of every touched shard kept (`cf_eval.rs:801-817`); texts cloned again for `NewDoc` and the SQL parameters | Shards streamed one at a time. Only drawn rows go, one by one, to the SQL insert. |
| Batch shards (seal), 64 documents each | Each text exists as an SQL `String`, a `json!` clone, the shard `String`, a pretty index, up to 6 `body.clone()`s and an `index.clone()`, about 8 × the text bytes [M] | Per shard: rows visited by keyset, two-pass count, exact buffer, incremental SHA-256, exact-length put. At most `PARALLEL` (6, the connection limit) shard buffers in flight. Index and manifest are encoded with `encoded_len`. |
| Load-seal source index | Whole index parsed into a `Value` tree (`engine.rs:1598`) | Streamed as JSON Lines. |
| Receipt chunks (scoring lanes) | 4,960 B `Vec`s; `put_retry` calls `body.to_vec()` per attempt (`lane_logic.rs:216`) | A fixed buffer of exactly 32 × 155 B. Each attempt copies the same slice to JS. |
| Shard texts (scoring lanes) | `ShardCache` lives for the whole alarm and never evicts (`lane_logic.rs:189-253`) | The cache belongs to the chunk-round scope and holds only the claim's shards, each admitted by exact object size. Container request bodies are written from borrowed slices of the shard buffer. |
| Artifact manifest (scoring lanes) | Read whole with no bound (`cf_lanes.rs:580-588`); parts already piped zero-copy | Exact-size admission bounded by the artifact schema. Parts stay piped. |
| Container replies (scoring lanes) | `Value` with a `.cloned()` results array (`cf_lanes.rs:738-739`) | Bounded reader (`lane_docs` × result schema). Borrowed parse. |
| Summaries (summary lane, then the package) | JSON summaries with n `PredRow`s holding strings, then `TaskResult`s, then `bytes.clone()` | A canonical binary summary of n × `u32` involvement in parts per billion, with a sentinel for "no prediction", plus fixed fields. Exact length; streamed and hashed. |
| Bundle (bundle lane) | All chunks concatenated by `extend_from_slice` with no reserve; an `expected` `BTreeMap` of all receipts; the whole bundle cloned for the put (`cf_lanes.rs:1272-1325`) | Exact total length from `/job/objects`. Chunk objects read at most `PARTS_IN_FLIGHT` at a time, each verified by a merge-join against receipt pages ordered by chunk, then hashed and written to a `FixedLengthStream`. |
| Package: index, root and signature | Entries pushed with no capacity from 10k-row hex pages; `build_index`; digest re-encodes; `index.clone()`; two envelope buffers (`engine.rs:1869-1898`, `cf_eval.rs:919-936`) | The receipt set is **sealed** durably first: a `receipts_sealed` row records the set's version, and lane commits for the cohort refuse once it exists. Then two passes over receipt rows in attempt order. Pass A streams the canonical encoding into `Sha256` to get the root and count, checking order and uniqueness incrementally, and the signature is made over that root. Pass B, at put time, streams the header, length and record into one `FixedLengthStream` through a re-hashing sink (`io::Recheck`); the key and signature, fixed by pass A, are written only if the re-hash equals the root. On a mismatch (a row changed despite the seal) `Recheck` aborts the stream: the writer is aborted (`writer.abort(reason)`), the put rejects and nothing is stored ([M], section 1.1: a writer merely left short would leave the put pending forever), and the host awaits that rejection inside the scope under an `AbortSignal` bound; a `Defect` names the route and digest. The stored object is read back and verified (`Signed::open`) before the target is published. Memory is one row plus one chunk buffer. |
| Package: scores, clusters and `components.json` | J × n `TaskResult`s; J² `rms` `Value`s | Compact arrays: J × n × 4 B involvement plus n × 8 B labels (kind as `u8`, source group as `u32` index). Utility, RMS and clusters computed from slices. `components.json` written by two-pass streaming per job. |
| Package, target, chunk map, keys | Six objects encoded into one array before the first put | Each encoded and put alone with an exact length. |
| Workers AI input and output (generation lane) | `serde_wasm_bindgen` into a serde_json `Value`; `ai_text` clones (`cf_eval.rs:963-1006`) | `response` read through `Reflect::get` with a length check, then admission and a copy. |
| Queue batch (`LaneKick`) | `messages()` deserializes the whole batch through swb; `LaneKick` holds three `String`s (`lanes.rs:74-83`) | `LaneKick` becomes fixed-size: instance and kind as `u8` enums, the name derived. The batch bound is 100 × the fixed size. |
| `/status`, `/eval/status`, `/kick`, the alarm's `has_work` | Load every cohort ever and deep-clone each plan (`engine.rs:592-597`, `:2158-2259`); re-parse and re-serialize status JSON (`cf.rs:599-607`, `:720-729`) | SQL aggregates over indexed columns. Per-cohort status paged. Durable Object status forwarded as bytes inside a bounded reader. |

Protocol decoding already limits up-front reservations to
`MAX_PREALLOC_BYTES` (64 KiB) and sequences to `MAX_SEQUENCE_LEN`
(`codec.rs:13-17`, `:170-180`). V's `ArtifactReader` already streams an object
of any size through `update(chunk)` and buffers only its header, which is at
most 1 MiB (`artifact.rs:27`, `:214-260`). It is the pattern for streaming
index and bundle verification.

## 5. How bounds are derived and versioned (design)

Bounds follow the project rule against tuned caps:

- A bound is an exact size when one is known before allocation.
- Otherwise it is derived from a schema, a platform limit or a measured
  high-water.
- Every bound is versioned. None names a chain runtime version or gates on
  one.

### 5.1 Isolate level: the build's memory model

A checked-in, versioned `subnet/coordinator/memory-model.json`
(`slop_ninja_worker_memory_v2`) is compiled into constants and reported by
`/status`. Every input records its source in a `<field>_source` string, and
`build.rs` refuses a model without one. The prototype carries two measured
models: the drill's (`subnet/crates/arena/memory-model.json`) and the glue
probe's (`glue-probe/memory-model.json`).

| Field | Source |
| --- | --- |
| `isolate_bytes` | 128,000,000 [Q] "128 MB per isolate". This reads MB as 10^6 bytes, the smaller of the two readings. |
| `static_bytes` | Read at the first allocation as `memory.size × 64 KiB`: the module's own stack and data. Not a parameter. |
| `js_reserve_bytes` | Measured JS high-water with its garbage backlog over long runs at the envelope shape, plus one invocation's need (section 2.7). Never zero; `js_reserve_measured` must be true outside measurement builds. Drill: 92,173,872 B. |
| `js_envelope_bytes` | The per-invocation JS need the reserve was measured for. Drill: 8,480,236 B (20,000 receipts). |
| `glue_static_bytes`, `glue_ticket_bytes`, `glue_resident_bytes` | Measured glue: idle glue, the largest glue footprint of one entry over the entry kinds (each entry takes its own ticket), and the glue per co-resident Durable Object instance. The model lists the measured cost per entry kind (`glue_ticket_kinds`) and `build.rs` refuses a ticket cost below any of them. Probe (workers-rs 0.8.7, every export hand-written): 2,048, 512 and 32 B (measured 872 B, the 304-B `waitUntil` continuation and 24 B, each rounded up to the next power of two as the stated margin). |
| `stack_bytes`, `stack_high_water_bytes` | The link-time stack (1,048,576 B, [R] rust-lld's default for `wasm32-unknown-unknown`) and the painted stack high-water measured over the build's routes (section 2.1): drill 2,944 B, probe 3,912 B. `build.rs` requires `2 × high-water <= stack_bytes` and refuses an unmeasured high-water outside measurement builds. |
| `min_route_bytes`, `largest_route_bytes` | The smallest and largest declared route footprints, from the route table (section 5.3). |
| `heap_ceiling` | `isolate_bytes - static_bytes - js_reserve_bytes` |

Derived by `build.rs`, never typed in:

| Constant | Derivation |
| --- | --- |
| `GLUE_TICKETS` | `floor(pool_pages_upper / min_scope_pages)`: the most scopes the pool could admit at once, so a ticket is refused exactly when no scope could be admitted for it either. |
| `GLUE_RESIDENTS` | `GLUE_TICKETS` (an instance is created by an admitted invocation; one past the budget is refused at construction). |
| `GLUE_BUDGET_PAGES` (G) | `glue_static + tickets × glue_ticket + residents × glue_resident`, plus the glue heap's segment overhead, in whole pages. |
| Quarantine budget | `(P - G) - pages(largest_route_bytes)`: pinned pages within it never stop the largest route from being admitted by page count. |
| `STACK_FRAME_BOUND_BYTES` | `stack_bytes / 2 - stack_high_water_bytes`: the clippy frame, future and stack-array threshold (section 3.2). Drill 521,344 B, probe 520,376 B. |

**Glue measured per entry point** ([M] glue probe, workers-rs 0.8.7,
worker-build 0.8.7, wasm-bindgen 0.2.129, local workerd 2026-10-23; glue
high-water at 1, 2, 4, 8 and 16 in-flight invocations as dlmalloc footprint:
the least-squares slope and the largest per-invocation ratio):

| Entry kind | worker-macros export | Hand-written export (own ticket) |
| --- | --- | --- |
| Worker `fetch` | 482 to 514 B; a 4 KB URL adds 3.5 KB | 272 to 290 B with the request converted inside the scope; no URL dependence |
| Durable Object `fetch` | 498 B (and the Worker's 482 to 514 B under the same entry) | 274 B in its own entry (the 544 to 563 B Worker-to-DO chain takes two tickets) |
| Durable Object `alarm` | 264 to 480 B | 187 to 248 B |
| `queue` (one batch) | 304 to 330 B | 224 to 239 B |
| `scheduled` | 464 to 495 B | 192 to 215 B |
| `waitUntil` continuation | (needs the context: hand-written only) | 304 B (its own ticket) |
| Durable Object instance (resident) | 24 B | 24 B (a resident token in the constructor) |
| Idle | 1,096 B | 872 B |

Both builds were measured twice (runs `round2/probe1` and `round2/probe2`;
the second under the model derived from the first, whose 512-B ticket cost
gives a 3-page partition for 244 tickets). In the admitted build every export
is hand-written: the Worker `fetch`, `queue` and `scheduled` functions, and
the Durable Object class (constructor with a resident token, `fetch` and
`alarm` each with a ticket), so a Worker-to-Durable-Object request takes two
tickets and no ticket covers a call chain. A burst of 276 concurrent
requests against 244 tickets peaked at 1 glue page (admitted) or 2 (macro),
with no glue allocation failure; the excess was refused with 503 before
allocating (the scopes ran out at 205, consistent with the ticket
derivation).

The ceiling applies to every route of the isolate, including routes that have
no plan, which is why it is a build parameter and not a plan parameter.

### 5.2 Plan level: `fanout.memory` (plan schema v3)

Plan schema v3 adds `fanout.memory`, next to the other operational fan-out
parameters (`max_lanes`, `lease_ms`, `step_budget_ms`). It holds:

- `model`: the memory-model version;
- per-route formula versions;
- the plan-dependent inputs.

These are operational fields. They change no evaluation semantics, so they
stay out of the cohort manifest's evaluation fields.

`/plan` admission evaluates every step's upper bound from the plan's J, n,
`max_attempts`, `lane_docs`, `chunk_docs` and shard size, against the build's
`heap_ceiling`. It refuses an oversized plan before anything runs, and stores
the per-route bounds with the cohort. Today the oversized case is found at
signing, after the whole build [M].

A later build with a lower ceiling turns a step that no longer fits into the
durable `arena_capacity` pause. The cohort is never skipped.

### 5.3 Per-route formulas

All sizes use `size_of` on the build target.

| Route or step | Bound |
| --- | --- |
| `/hook` | `MAX_BODY_BYTES + size_of(borrowed envelope) + C_hook` |
| RPC call | `MAX_RESPONSE_BYTES + C_rpc` |
| Lane claim, commit, report | `req_max(lane_docs) + resp_max(lane_docs) + header row + roster row + C_route`. `req_max` and `resp_max` come from the wire schemas. |
| `job_receipts` page | `rows × row_bytes`, with `rows = (reservation - C_route) / row_bytes` |
| Seal | `PARALLEL × max_k shard_k + len(batch index) + encoded_len(manifest) + C_seal`. `shard_k` is exact from a two-pass count over SQL byte lengths. |
| Draw | `max shard object size + bitset(groups / 8) + one row + C_draw` |
| Bundle lane | `PARTS_IN_FLIGHT × 4,960 + one receipt page + stream chunk + C_bundle` |
| Summary lane | `encoded_len(summary) = header + n × 4` |
| Package | `J × n × 4 + n × 8 + J × size_of(JobScore) + stream chunk + one row + C_package`. Independent of the receipt count R. |
| Index envelope | `55 + 69 × R + 113 <= MAX_SIGNED_RECORD_LEN`, checked at `/plan` with `R = J × n × max_attempts` |

What these give [C]:

- At the R_0 + 128 shape (J = 129, n = 1,000), the package's compact arrays
  are 0.49 MiB. At J = 257 and n = 4,000 they are 3.92 MiB.
- The measured 58.37 MiB peak at the R_0 + 128 shape [M] comes from the
  terms that the streaming design removes.

These are projections of the streaming design. Section 8 does not build it
for testnet; the two-level receipt index before mainnet revisits them, and
step 0 re-fits the current package and seal footprints instead.

### 5.4 Existing hard-coded limits

None of these seven inherited limits had a recorded derivation. Each now
either derives from a schema or a platform limit, or is marked for
re-derivation at a named migration step with the measurement that sets it.
Until its step, an inherited value stays as the admission bound, checked
before the read, and its refusal is `Invalid` (network input) or `Capacity`
(plan-derived).

| Limit | Value | Derivation, or re-derivation and the measurement that sets it |
| --- | --- | --- |
| `webhook::MAX_BODY_BYTES` | 256 KiB | **Schema-derived.** The receiver contract v1 body is closed: every struct is `deny_unknown_fields`, and every field has a bounded encoding (`Id` 66 B as quoted hex, `DecimalU64` at most 22 B, `version` 3 B, the challenge at most `MAX_CHALLENGE_BYTES` 256 B, `state_readiness` a fixed enum). The bound is the larger of the two envelopes' maximum compact encodings, computed by a `const fn` over the wire structs at step 3; it is a few hundred bytes, not 256 KiB. A body with insignificant whitespace past that bound is `Invalid`, since the HMAC covers the exact bytes and the provider sends compact JSON. |
| `rpc::MAX_RESPONSE_BYTES` | 64 KiB | **Re-derive at step 3.** `chain_getFinalizedHead` and the `chain_getBlockHash` list (at most `MAX_HASHES_PER_CALL` 32 hashes, 32 × 68 B plus framing) are schema-bounded. `chain_getHeader` is not: its `digest.logs` are runtime-defined (Aura pre-runtime and seal items, and GRANDPA consensus messages whose size grows with the authority set). The measurement: the largest `chain_getHeader` response over every finalized block of the target chain (an archive scan through rtao-rpc), and the GRANDPA scheduled-change item at the runtime's maximum authority count. The bound is the larger of the list bound and that header bound, versioned with the scan's block range. |
| `MAX_OPERATOR_BODY` | 1,024 B | **Schema-derived at step 3:** the maximum compact encoding of the typed `/fault/clear` request (a `const fn` over its fields: a fault kind from a fixed enum and a cohort id). |
| `eval_route` body | 512 KiB | **Re-derive at step 5** from plan schema v3: every string field gets a schema maximum, and the bound is the header plus `MAX_ROSTER` (256) roster entries at their maximum encoding. The measurement that validates it: the encoded size of the largest plan the schema admits, generated by the plan fixtures. |
| `MAX_SIGNED_RECORD_LEN` | 64 MiB | **Re-derive with the two-level receipt index** (section 8, before mainnet). It becomes the binding limit once the index streams: an index of R entries is `55 + 69 × R + 113` bytes, so 64 MiB holds 972,591 entries. Two consumers bound it: the native validator, which verifies by streaming (no whole-record buffer), and any whole-buffer consumer, which must fit its isolate's heap ceiling. The measurement: package verification high-water natively at the largest plan-admissible R, against the validator's `MemoryMax`. The two-level index decision (fan-out design §5.2) gates it. Meanwhile `/plan` refuses a plan whose `J × n × max_attempts` exceeds the current value. |
| `plan::MAX_CASES` | 4,000 | **Derived from the envelope, and inconsistent today:** 4,000 cases × `MAX_ROSTER` 256 jobs is 1,024,000 receipts at one attempt, above the 972,591-entry envelope. Derived, `MAX_CASES = floor((MAX_SIGNED_RECORD_LEN - 168) / (69 × J_max × max_attempts))` = 3,799 at J = 256 and one attempt. Step 5 replaces the constant by this per-plan check at `/plan` together with the per-route bounds of section 5.3 (seal, summary and package terms in n). |
| `MAX_BUNDLE_BYTES` (native T12) | 64 MiB | **Re-derive with the two-level receipt index** (section 8, before mainnet). The T12 file is the whole cohort in hex JSON, about 443 B per receipt, so its bound is `R_max × 443` with `R_max` from plan admission; milestone M6 retires it for per-bundle binary reads of `n × max_attempts × 155` bytes. The measurement: the validator's native high-water verifying the largest admissible bundle (section 7). |
| `ROSTER_CAP` / `LOAD_ROSTER_CAP` (testnet vars) | 40 / 128 | Testnet operating choices; "absent means no cap (mainnet)" (`wrangler.toml`) |
| V's `MAX_ROSTER` | 256 | "The chain's UID space holds at most 256 registrations per subnet" (`records_v2.rs:22-24`) |

## 6. Dependencies and the wasm-bindgen boundary (design)

**Rust allocations by dependency.** These all go through the global allocator,
so they land in whichever arena is current. The work is to bound their size,
not to capture them.

| Dependency | Allocates [R] | Handling |
| --- | --- | --- |
| worker 0.8.7 bodies | `Request::bytes` (`to_vec`), `Request::json` (JSON.parse, then swb), `ObjectBody::bytes` (`vec![0; len]` after a JS ArrayBuffer), `Response::bytes`/`text`/`json` (`try_fold` append, no cap), `ByteStream` (a `Vec` per chunk) | Not used. `cfio` bounded readers replace them. |
| worker `FixedLengthStream::wrap` | Pulled by JS outside the scope, through `wasm_streams`; a `Vec` per chunk | Not used. Writer on `worker_sys::FixedLengthStream`. |
| worker `Response::from_json`/`from_bytes` | `to_string`, then a JS copy | Replaced by a `web_sys::Response` built from an arena slice. |
| worker SQL cursor | `to_array`/`one` through swb; `raw()` builds a `Vec<SqlStorageValue>` per row, with owned `String`s and BLOB `Vec`s | Row visitor over the raw JS iterator, with typed column reads. |
| worker KV `Storage::get`/`put` | swb per value | Only fixed-schema small values, bounded by type, or moved to SQL. |
| worker `Ai::run` | swb in and out, into owned types | Raw binding call, `Reflect::get`, length check, then copy. |
| worker Queue, `MessageBatch` | swb per message | Fixed-size `LaneKick`. |
| worker `Context::wait_until` | `future_to_promise` box in the current scope | `arena::wait_until`: the continuation takes its own ticket, its future is boxed in a child scope with the 30 s deadline, and `Continuation::spawn` creates the task outside any scope, so the task box is glue under that ticket. |
| worker-macros exports | The export future box, the js-sys task and the `worker::Request` conversion, all in glue before the handler runs; a queue handler's `Err` becomes a panic | Replaced by hand-written exports (section 2.5): ticket first, conversion inside the scope; the queue handler never returns `Err`. |
| js-sys futures | `Rc<Task>` + `Box::pin` and the queue `VecDeque` (glue, under the entry's ticket); `JsFuture`'s `Rc` and two `Closure::once` (current scope) | Bounded by tickets. A pending `JsFuture` is never dropped by `select`; every await has an `AbortSignal` timeout. |
| wasm-bindgen | Exported struct `Rc<WasmRefCell<T>>` (glue, 24 B measured, freed by its `FinalizationRegistry` when JS collects the object); `__wbindgen_malloc`/`realloc` for JS-to-Rust strings (current scope, up to 4 × the UTF-16 length transiently); `Closure` boxes | Durable Object instances take resident tokens; strings are admitted with `admit_js_string(len)` before the call; `forget` and `into_js_value` banned. |
| serde_json | `Value` trees, owned strings in `DeserializeOwned`, `to_string`/`to_vec` buffers, `json!`; error messages that embed input; the escape scratch buffer | Borrowed structs with the pre-scan, `deserialize_any` and static-message visitors (section 4.1); `to_writer` into bounded sinks; `Value` and `json!` banned in production paths. |
| serde-wasm-bindgen | Proportional to a JS value whose size is unknown before the walk | Only for fixed-schema values with type-derived bounds. |
| futures-util `join_all` | A `Vec` of futures in scope | Bounded by the group size (`PARALLEL` 6, `PARTS_IN_FLIGHT` 3). |
| hex | A `String` per `encode` | `encode_to_slice` into stack buffers. |
| slop-ninja-protocol | `Encoder` `Vec` with no reserve; `digest` re-encode; `domain()` `format!`; two envelope buffers | `Sink` encoder (section 4.1); golden bytes unchanged. |
| sha2, hmac, ed25519-dalek, subtle, zeroize | No heap allocation | None. |

**Copies across the wasm-bindgen boundary.**

- A `&str` passed from Rust to JS is decoded by JS directly from linear
  memory, with no Rust allocation.
- A string passed from JS to Rust is allocated through `__wbindgen_malloc`
  in the current scope, after a `length()` admission.
- `Uint8Array::copy_to` and `copy_from` make one copy between a JS buffer and
  an admitted slice.
- `Uint8Array::view` and `view_mut_raw` over linear memory are banned
  (clippy and the source scan). A pending JS reader could still hold the view
  when the buffer is reused, or when `memory.grow` detaches the buffer.
  worker 0.8.7 itself copies with `copy_from`, so only application code is at
  risk.
- A `JsValue` handle lives on the JS side.

## 7. Native validator and miner tools

**Facts [R, F].**

- The native validator is Rust on rtao and runs as systemd units with
  `MemoryMax=1G` and `MemorySwapMax=0` (`crates/validator/deploy/`).
- Its T12 shortcut reads one bundle file of at most 64 MiB, hex in JSON,
  whole, and parses it with `serde_json` (`crates/validator/src/package.rs`).
  At 129k receipts that file is about 57.1 MB [C].
- Milestone M6 replaces the file with per-bundle R2 reads. The fan-out design
  states that validators "need memory for one bundle, not the whole cohort"
  (`data/task-roots/testnet/fanout/DESIGN.md` §5.1).
- No miner client exists in `subnet/` at F or V.
- AGENTS.md requires Linux and macOS support for miner and validator tooling.

**What does not carry over from the Workers rule.** A native process has:

- no isolate cap;
- no per-invocation boundary;
- an async runtime and HTTP and TLS stacks that allocate internally
  (`subnet/Cargo.lock` has tokio 1.54.1 and rustls 0.23.46);
- OS virtual memory, so the failure mode is the systemd OOM kill on Linux.

A per-invocation arena allocator has nothing to scope there. A capped
allocator would turn an OS-level kill into an allocator abort, with the same
result.

**Recommendation.** Apply the rule's substance, not its mechanism:

1. Native tools use the same protocol crate, so the streaming encoder,
   streaming verification (the `ArtifactReader` pattern) and borrowed
   decoders reach them unchanged.
2. Inputs follow the same discipline as on Workers:
   - exact-size admission;
   - one bundle in memory at a time in milestone M6;
   - no whole-file hex JSON once T12 is retired.
3. Native tests apply per-task high-water limits with the counting allocator
   already used in `protocol/tests/allocation.rs`. The cases are package
   verification at the R_0 + 128 shape and at 257k receipts.
4. On Linux, systemd `MemoryMax` and `MemorySwapMax=0` remain the process
   ceiling. On macOS, the tested code paths are the bound.
5. No production arena allocator is used natively. Wallets, journals and
   recovery code are unchanged.

## 8. Migration plan

The arena as built is the scope through the testnet announcement: the
bounded readers, the scoped arenas with their route footprints, the refusal
at `/plan`, and the linear memory ceiling (growable in `Observe`, fixed once
the arena enforces). Streaming the protocol encoder, the draw, the seal and
the package and bundle, plan rows for full 256-UID rosters, and the native
validator alignment are not part of this plan. Before mainnet, a two-level
receipt index replaces them: a per-job index whose per-job bounds replace
the attempt-multiplied cohort-wide checks. It is required, because the flat
index refuses a full 256-UID roster even at n = 1,000.

The remaining steps follow, in order. Every step runs on the build host
through `arena-check` (section 3.3), is committed with `[skip ci]`, and
passes its gate before the next one starts.

**Step 0. Measured model.**

- Record from the built wasm of each Worker role (the coordinator Worker:
  deliveries, cron and signing; the eval Worker: the cohort steps): the
  static linear memory, the module size and the largest export future size.
- Re-run the glue probe's measurement on the coordinator build (glue per
  entry kind at 1 to 16 in-flight invocations, residency per Durable Object
  instance), the painted stack high-water over every route, and the
  inspector measurement (`Runtime.getHeapUsage`, qualified in local workerd
  on 2026-10-31) under each role's shapes, long enough to show the JS
  high-water's plateau.
- Re-run the probe harness on the reward-v1 package and seal steps
  (`package_v3`, `seal_v3`) at the `ROSTER_CAP` and `LOAD_ROSTER_CAP`
  shapes, with probes and audits, and re-fit and version `package_bytes`
  and `seal_bytes`. `package_fits` is a hard `/plan` gate the standing
  template relies on.
- Gate: a baseline report per role, which seeds that role's memory model
  (`slop_ninja_worker_memory_v2`): every input with its source, the JS
  reserve measured with its garbage backlog at its plateau, never zero; the
  stack high-water within half the linked stack. The coordinator Worker's
  largest route comes from its own measured peaks, the eval Worker's from
  the package step, so each role's quarantine budget is its own.

**Step 2 remainder. Entry wrappers.**

- The fan-out lane objects (`AScorer`, `EvalLane`, `GenLimiter`,
  `SpendLedger`, in `cf_lanes.rs`) become hand-written exports in scopes,
  their `select` + `Delay` timeouts and whole-body reads replaced, and
  their R2 reads bounded by object kind instead of the largest route.
- The step-crash protocol (attribution, a crash count, then a durable
  `arena_trap` pause), the durable recycle intent keyed by poison cause, and
  the per-cause stop rule, shipped together with the recycle. The stop rule
  never pauses `/hook`: refusing deliveries would trigger the provider's
  ten-retry automatic pause.
- Strict witnesses for the delivery continuation, the cron tick, a signing
  step with a maximum-size target bundle, the draw, seal and package steps at
  the widest admitted plan, the queue batch, the operator target install and
  the Worker status aggregation; `CONTINUATION_CONTROL_BYTES` and
  `QUEUE_CONTROL_BYTES` then come from their measured peaks.
- The clippy configuration and the source-scan lint.
- Gate: the native strict suite passes; the local drill (criteria below)
  runs 1,000 consecutive invocations per entry point with no growth.

**Step 3 remainder. Schema-derived bounds.**

- The webhook delivery body bound from the delivery schema (section 5.4),
  and the operator body bounds from theirs.
- Per-kind R2 read bounds from declared sizes: byte sizes in the pool
  manifest and shard schema or in the plan, read against them; a pool-v2
  export shape the draw step can hold (sliced manifests, or rows loaded
  into SQL incrementally), agreed with the plan and pool owners; the pool
  tool refuses a manifest above the coordinator's bound; a draw test at the
  AR-8 shape (90 days of stock at the configured n and mix).
- `LANE_BODY_BYTES` and `STEP_OBJECT_BYTES` from the schemas of the bodies
  and objects they bound, versioned in the model.
- Gate: one byte over each derived bound is refused with zero allocation
  and the right kind.

**Step 5 part. Plan admission.** Done for the package step (`package_fits`,
including its in-flight summary reads). Remaining: the standing-template
generator refuses a roster or manifest above the coordinator's bounds.

**Step 6. Lane payloads.**

- Typed bounded claim, commit and report.
- Paged `job_objects` and `job_receipts`.
- A per-round `ShardCache`.
- Bounded Container request and reply.
- `put_retry` without `to_vec`.
- Fixed-size `LaneKick`.
- Gate: the lane alarm's high-water does not depend on the chunk count.

**Step 9. Enforce mode.**

- Before it: refusal classification checked against live layouts (the home
  scopes opened at a fixed end of the pool or at instance initialisation, so
  they never split the run a large step needs); glue tickets reserved for
  the cron and the Durable Object entries; the measured models of step 0.
- Refusals are enabled, overdrafts and leaks become faults, and the build
  links its fixed linear memory (`initial = maximum`).
- Gate, local: the drill for every entry point (criteria below).
- Gate, testnet: a repeat of `load-s2-r0p128-n1000` on the load instance.
  - The shape: J = 129, n = 1,000, W = 2, 258 scoring lanes, 4,128 chunks,
    129 summary lanes, 129 bundle lanes and 129,000 receipts.
  - Pass criteria:
    - no `exceededMemory` outcome in the tail;
    - every route's peak is at or below its reservation;
    - pool high-water plus static bytes is at or below the ceiling;
    - no quarantined or abandoned pages, no overdrafts, no glue
      allocation failure and no ticket refusal;
    - per-entry-point high-waters are recorded in the run record and
      versioned into the memory model.

**Local drill criteria** (steps 2 and 9):

- For each entry point, 1,000 consecutive invocations of one input shape.
- After each invocation:
  - pool pages in use return to their idle value;
  - glue live bytes return to their idle value;
  - quarantine stays at zero.
- `memory.size` after invocation 1,000 equals its value after invocation 2,
  the first after warm-up.
- After a forced garbage collection, the JS heap read through the inspector
  shows no monotonic growth across invocations 10, 100 and 1,000, and the
  high-water with garbage backlog stays within the model's JS reserve.
- Fault scenarios, each 1,000 invocations: abandoned futures (some recovered
  by a late settle, some reclaimed by a drop, some abandoned for good, which
  must poison the pool and recycle it, never loop on `Retry`); traps (the
  co-resident invocations aborted and attributed to the trapping route by
  the postmortem, the step re-run once, then a durable `arena_trap` pause);
  a leak on every invocation (quarantine, recycle at the budget, and at most
  two recycles per cause before the stop rule's `arena_leak` pause); the same
  leak with co-resident steps under the step-crash protocol (aborted by the
  recycles, attributed to them, never counted as their crashes); shapes
  outside the JS envelope (refused before allocating).
- Local workerd does not enforce the 128 MB limit, so only the testnet run
  establishes enforcement.

### Migration status (2026-11-02)

Implemented in both Workers built from `subnet/coordinator`:

- **Arena crate.** The pool starts at `__heap_base`; under a fixed linear
  memory it counts every pool page as committed, under growable memory it
  commits pages with `memory.grow` as it hands them out.
  `admit_current_block` (an exact block in the scope being polled); a block
  admission re-keys a run of its scope that was served and freed (the
  smallest that holds it), so a scope reading many objects holds as many
  runs as it has blocks live at once (`blocks_reused`); permanent scopes
  (`Scope::set_permanent`, the coordinator's homes), whose pages count as
  never freed by waiting when a refusal is classified (`Capacity`), while
  glue spilled into the scope partition counts as freed by waiting
  (`Retry`: the segment returns when its allocation is freed);
  `ScopeRef::child_physical`, refused in every mode when the pool cannot
  serve a footprint even scattered; the transitional `glue-spill` feature
  (counted; never in `Strict` or on an armed thread); and `build.rs`
  refuses `unmeasured-model` without `observe` or the test knobs, so an
  unmeasured model never enforces.
- **Model** `subnet/coordinator/memory-model.json`: a 96 MiB linear memory
  ceiling from the largest measured Rust peak (the package step at R_0 +
  128, n = 1,000, with its in-flight summary blocks); glue from the glue
  probe; the JS reserve (the remainder) and the stack high-water not yet
  measured on this build (`unmeasured-model`); one model for both roles.
- **Mode: `Observe`** (step 1), linked with growable memory under the
  ceiling, so each isolate holds the linear memory it uses and the JS heap
  keeps the room it had before the arena until step 0 measures it.
  Admissions do not refuse what the pool can physically serve; would-be
  refusals (`observed_refusals`, `observed_excess_pages`) and overdrafts are
  counted on `/status`. The seal and package steps are the exception: they
  are admitted physically, so a second large step of a co-resident object
  waits (`Retry`) instead of exhausting the ceiling.
- **Step 2, in part.** Hand-written exports with glue tickets and the abort
  hook for the Worker `fetch`, `scheduled` and `queue` and for the
  `ChainJournal` and `EvalCohort` classes (resident tokens; their state and
  the two bounded caches live in three home scopes, opened on the first use
  that can admit them, their refused opens counted); the alarm export
  rejects on a handler error, so the platform retries it; the delivery
  continuation through `wait_until`; a child scope per cohort step on its
  own route (draw, seal, load-seal, package, close), the seal and package
  with their shape-derived footprints (the seal sized by the reward-v1 task
  count); `Capacity` and `Defect` refusals of a step, and a step's object
  read over its kind's bound or refused for capacity, fault the cohort
  durably; a plan whose package exceeds the model is refused at `/plan`.
  Every await of the hand-written routes has a platform timeout or one
  raced in JS: RPC and Durable Object calls carry `AbortSignal.timeout`
  (verified for Durable Object stubs in local workerd); R2 reads and writes,
  body reads and Workers AI runs are raced against their deadline in JS
  (`cf_io::settle`); no `select` with `Delay` remains outside the lane
  objects. The delivery continuation starts each journal call, kick, RPC
  round and signing step only while its allowance fits the 30 s window, and
  the journal hands out a signing step only when its worst case fits the
  caller's budget, so no continuation outlives its window or takes and
  abandons the signer claim. `/status` reports an arena `fault` for
  abandoned scopes or a poisoned pool. The poisoned-pool recycle is **not**
  wired: `entry_check` sweeps and reports health.
- **Step 3, in part.** Bounded readers for the webhook, operator, eval,
  internal Durable Object and lane-route bodies, Durable Object replies
  (bounded by the holding route's control footprint), RPC replies and R2
  objects, each through its stream's own reader, the bound checked before
  reading, blocks admitted only as bytes arrive (the exact length's block,
  or a buffer grown with the bytes received). The webhook receiver sheds a
  request without a signature header naming its subscription in JS, before
  any ticket or scope, checks the header's fields before any body byte, and
  stops reading a body still incomplete after the provider's 5 s
  acknowledgement window (408). Each cohort-step R2 read has its kind's
  bound: job summaries (`summary_bytes`, per task), fingerprint vectors (4
  bytes per probe document), the liveness record, and `STEP_OBJECT_BYTES`
  (half the JS envelope) for the kinds without a tighter schema bound. An
  internal body that cannot be read answers its own status (503 for an
  arena refusal), one that does not parse 400.
- **Runtime home.** The runtime cache evicts to `PARSED_RUNTIMES - 1`
  before it parses and parses uncached when an evicted entry is still held
  or the home lacks the metadata's measured parse need (scaled by length).

Not yet done: step 0 (the measured JS reserve and stack, per-role models,
and the re-fit of `package_bytes` and `seal_bytes` on `package_v3` and
`seal_v3`); the step 2, 3 and 5 remainders and step 6 listed above; and
step 9.

## 9. Open questions

- **Cited lines.** Section 1 cites the fan-out code (F); `reward-v1-live` and
  the migration moved some of them.
- **Platform accounting.** Whether Cloudflare counts wasm linear memory by
  its length or by committed pages, and whether "128 MB" means 10^6 or 2^20
  bytes, is not documented in the quoted pages. The design assumes length and
  10^6.
- **Isolate sharing.** Whether Durable Objects of different classes in one
  script, or the stateless Worker, share an isolate is not documented in the
  quoted pages. The pool is per wasm instance either way. Observed on
  testnet on 2026-11-02 (growable memory): the stateless Worker, the
  ChainJournal and each EvalCohort instance reported different linear
  memory sizes and arena epochs in every snapshot, so separate instances,
  in that sample.
- **After a trap.** Re-instantiation and the abort of co-resident
  invocations are established for this build in local workerd (section 2.6).
  Still to confirm in step 1: whether a trap inside `transactionSync` always
  rolls back, and how long the old instance's linear memory stays allocated
  on the platform (local workerd's figures are in section 2.7).
- **JS-heap measurement** is qualified in local workerd (inspector
  `Runtime.getHeapUsage`: V8 heap, embedder heap and ArrayBuffer backing
  stores, which exclude the wasm memory). Production V8 flags may collect
  external memory at different thresholds, so the testnet gate (step 9)
  re-checks the reserve with `exceededMemory` outcomes.
- **Generated-text bound.** No structural per-document bound exists for
  generated text beyond the 2 MB SQL value limit. Workers AI tokenizers are
  not pinned, so `max_tokens` does not bound bytes. Seal admission is exact at
  seal time; a document that exceeds the budget raises `arena_capacity`
  before sealing.
- **Envelope limit.** `MAX_SIGNED_RECORD_LEN` and the two-level index
  decision (section 5.4).
- **Plan digest.** Whether `fanout.memory` should enter the plan digest that
  the cohort records is undecided.

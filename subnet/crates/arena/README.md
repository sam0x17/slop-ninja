# slop-ninja-arena

Bounded per-invocation memory arenas for the Slop Ninja Workers, implementing
[`docs/workers-arena-design.md`](../../../docs/workers-arena-design.md)
sections 2 to 6. **Status:** the global allocator of both Workers built from
`subnet/coordinator` (the coordinator and the eval Worker), which link the
memory model's linear memory ceiling (growable in their current `Observe`
build, fixed once the arena enforces); see "Use in the coordinator" below and
the design's migration status. The rule it implements: **no allocation
happens outside a bounded memory arena**, and exceeding a bound is a clean
refusal or a durable pause, never a trap, an out-of-memory or a silent
truncation.

## What it provides

- **Page pool** (`src/pool.rs`). One `#[global_allocator]`, `ArenaAlloc`, owns
  every heap page. On `wasm32-unknown-unknown` the pool starts at the end of
  the module's initial memory and grows with `memory.grow`, which only this
  crate calls. Natively it is one `System` region of the ceiling's size. The
  usable size is `floor((isolate_bytes - static_bytes - js_reserve_bytes) /
  64 KiB)` pages, from the versioned memory model
  [`memory-model.json`](memory-model.json) (`slop_ninja_worker_memory_v2`),
  compiled into constants by `build.rs`, which also derives the glue
  partition, the ticket count and the residents from measured inputs. Every
  input records its source; the build refuses a zero or unmeasured JS reserve
  or glue cost unless the `unmeasured-model` feature is set (measurement and
  unit-test builds only).
- **Physically held reservations.** A scope's admission holds actual pages,
  committed with `memory.grow` at admission, in three kinds of run: heap runs
  (the state page and control footprint, and each admitted footprint), which
  only the scope's dlmalloc heap takes; one block run per admitted block, for
  one allocation of exactly the admitted layout, served directly from the run
  and never by the heap (so no segment request can take another block's
  pages, in any allocation order); and pages lent to children. An allocation
  inside an admitted footprint therefore never needs a page, a contiguous run
  or a grow that might be refused later. Admission takes dlmalloc footprints
  (`Footprint`, `chunk_bytes`, `block_bytes`), not byte counts.
- **Scopes** (`src/scope.rs`). `Scope::open(route, footprint)`; `admit_block`,
  `reserve`, `admit_footprint` and `admit_js_string` hold more where an exact
  size becomes known, and `lend` holds pages for children. Each scope is a
  dlmalloc 0.2.12 instance whose state (with a `u64` epoch) sits at the start
  of its first page. `close` returns every page. A scope whose value outlives
  it is trimmed and quarantined; it keeps only the pages that hold live bytes
  and is released at its last free. `ScopeRef::child` carves a child's run,
  and its admissions, only from the pages the parent lent (never the parent's
  heap or blocks), and a per-page carve stack returns them to the right
  ancestor. `enter` boxes a handler future inside its scope and hands it a
  `ScopeRef`, neither `Copy` nor `Clone`: the current arena is `(id, epoch)`,
  so a handle that outlives its scope (or whose scope is quarantined)
  allocates nothing and admits nothing, and never reaches a later scope that
  reuses the page. `Scoped` routes allocations by the future being polled and
  marks the scope as polled.
- **Routes and deadlines.** A `Route` (`Route::new`) is synchronous: its
  scope has no deadline and cannot hold a future (`scoped` refuses). `enter`
  and `wait_until` take an `AsyncRoute`, whose abandonment deadline is
  mandatory (a zero deadline is a compile error in a route constant): a
  platform wall limit from `limits` or the route's own await bounds plus the
  CPU limit. A child takes its own route's deadline, else its parent's.
  `wait_until` admits a `waitUntil` continuation (its own ticket, a child
  scope, the future's box) before it is spawned, with the 30 s limit from the
  clock at spawn; `Continuation::spawn` hands it to the platform outside any
  scope.
- **Abandonment and the quarantine budget.** Routes declare
  `abandon_after_ms` (the platform wall limit of the entry kind, or the
  route's own await bound). `tick(now)` at every entry marks scopes unpolled
  past their deadline as abandoned. A late poll recovers one; dropping its
  `Entered` (the platform cancelled the task) reclaims it. Pinned pages
  (quarantined and abandoned) count against the quarantine budget: the scope
  partition minus the largest declared route. Past it, or when a refusal is
  caused by pinned pages, `health()` is `Poisoned` with a cause (the route
  pinning the most pages and how). The host writes a durable recycle intent
  keyed by (route, kind, model version), applies the stop rule (the second
  recycle for the same key pauses that route durably: `arena_leak`) and calls
  `recycle(cause)` (a deliberate trap at an export entry, which worker-build
  0.8.7 answers with a fresh instance).
- **Postmortem.** worker-build's re-instantiation has fresh memory, so the
  host's abort hook (`wasm_bindgen::handler::set_on_abort`, chained to
  worker's) copies `postmortem()` (current scope and route, last failure,
  recycle cause, pinned pages, stack high-water) out as scalar words before
  the new instance starts.
- **Stack.** The wasm32 stack (`stack_bytes`, linked first, outside the pool)
  is painted at initialisation; `stats()` reports its high-water. The model
  records the measured high-water, `build.rs` requires it within half the
  stack, and the clippy stack lints use `stack_bytes / 2 - high-water`.
- **Glue partition** (`src/glue.rs`). Allocations made while no scope is
  current go to a static dlmalloc over a fixed partition of
  `GLUE_BUDGET_PAGES`. Scopes never take its pages and glue never leaves it.
  Exports take a `glue::Ticket` (Durable Object constructors a `Resident`)
  before anything allocates; past the derived count the export answers a
  JS-only 503.
- **Refusals by cause.** Every refusal is an allocation-free `Copy` value with
  a kind: `Retry` (other open scopes' pages would satisfy it), `Capacity`
  (no wait can: it does not fit beside the requester's own and its
  ancestors' pages), `Poisoned`, `Invalid` (input past its schema bound) or
  `Defect` (including stale handles). The I/O helpers take the buffer's cause
  (`io::Kinds::{NETWORK, PLAN, ENCODER}`). An invocation whose admissions
  exceed the model's `largest_route_bytes` is recorded as a defect.
- **Enforcement** (`Mode`, `src/strict.rs`). `Observe` holds what it can and
  records every would-be refusal and its excess separately (no counter wraps;
  nothing under the pool lock can panic: arithmetic is checked with an abort
  on invariant failure). `Enforce` refuses admission; an overdraft takes only
  free pages and is recorded. `Strict` also refuses overdrafts and refuses
  allocation outside a scope on an armed thread or instance. `strict::run`,
  `run_strict` and `strict_test!` fail a test on any violation. The mode and
  pool-limit knobs exist only with the `knobs` feature.
- **Exhaustion reporting** (`src/stats.rs`). `stats()` and `route_stats()`
  return `Copy` snapshots: pool, free and held pages, glue partition use, glue
  bytes and footprint, tickets, pinned pages against the budget, scopes by
  status, abandonment, recoveries and reclaims, carved children, refusals by
  kind, overdrafts, allocation failures and the last failure (static, so
  readable after a trap). `Stats::write_json` writes them into a fixed buffer.
- **Zero-copy helpers** (`src/io.rs`). `ExactBuf`; `Ingest`, the bounded body
  reader, which probes for one more byte once full and refuses any stream
  longer than its bound or declared length; sinks (`Count`, `Hash`,
  `ChunkSink`, which never flushes a short tail and aborts its stream on
  every refusal, `Tee`); `Recheck`, pass B of a two-pass signed write, which
  aborts the stream on a mismatch; `Enc`; `BoundedVec`; allocation-free hex
  and `FmtBuf`.

## Use in the coordinator

- `subnet/coordinator/memory-model.json` is the Workers' model (selected by
  `SLOP_NINJA_MEMORY_MODEL` in `subnet/coordinator/.cargo/config.toml`).
  Its `build.rs` links the wasm32 module with `--max-memory =
  model::LINEAR_MEMORY_BYTES`, `floor((isolate_bytes - js_reserve_bytes) /
  page_bytes)` pages, and, in a build whose arena enforces, `--initial-memory`
  at the same size: the pool begins at `__heap_base`, every pool page is
  present from the start and nothing grows at run time
  (`fixed_linear_memory_never_grows`; the drill's `drill-arena-fixed` module
  checks it as wasm32). The `Observe` build links growable memory under the
  same ceiling: the pool commits pages with `memory.grow` as it hands them
  out, so an isolate holds only the linear memory it uses.
- `ScopeRef::child_physical(route, bytes)` is `child` refused in every mode,
  `Observe` included, when the pool could not serve `bytes` even scattered
  (its free pages and the parent's unused lent pages are fewer): for a large
  step whose overdraft would exhaust the ceiling and trap the instance
  (`tests/physical.rs`).
- `admit_current_block(len, align)` admits an exact block in the scope being
  polled, for helpers that learn a size (an R2 object, a declared body
  length) without the scope handle.
- `glue-spill` (transitional) lets glue segments the partition cannot hold
  take free scope pages, counted (`glue_spills`, `glue_spill_pages`), never
  in `Strict` or on an armed thread: the entry points not yet wrapped in
  scopes (the fan-out lane objects) keep working during the migration.
- The coordinator's route table, footprints and home scope are
  `subnet/coordinator/src/arena_routes.rs`; its hand-written exports are
  `src/cf_arena.rs`.

## Bounds

Every bound is exact or derived, and each is a versioned model or plan
parameter. None names or gates on a chain runtime version.

| Bound | Source |
| --- | --- |
| Pool pages | `floor(heap_ceiling / 65,536)`, `heap_ceiling = isolate_bytes - static_bytes - js_reserve_bytes`. `isolate_bytes` is 128,000,000 ("128 MB per isolate", read as 10^6). `static_bytes` is `__heap_base` rounded up to a page (wasm32). |
| Linear memory ceiling | `LINEAR_MEMORY_BYTES = floor((isolate_bytes - js_reserve_bytes) / 65,536)` pages, linked as the maximum by the Worker's build script (and as `initial = maximum` once the arena enforces). |
| JS reserve and envelope | Measured in local workerd through the inspector, with the garbage backlog, at the envelope shape; see `memory-model.json` and `MEASUREMENTS.md`. |
| Glue partition, tickets, residents | Derived by `build.rs`: `tickets = floor(pool_pages_upper / min_scope_pages)`, `residents = tickets`, `glue = static + tickets × ticket + residents × resident`, from measured glue per entry (one ticket per entry; the ticket cost covers every measured entry kind). |
| Stack | `stack_bytes` (link-time size) and the measured `stack_high_water_bytes`, with `2 × high-water <= stack_bytes`; clippy thresholds `stack_bytes / 2 - high-water`. |
| Quarantine budget | Scope partition minus the largest declared route footprint (`largest_route_bytes`). |
| Per-block reservation | `block_bytes(len, align)`: dlmalloc 0.2.12's segment request for one block (`chunk(len)` plus `top_foot + malloc_alignment`, or the `memalign` request above 2W), rounded to 64 KiB. |
| Footprints | `Footprint`: the chunks of the simultaneously live allocations, the largest again, and the segment overhead. |
| Abandonment deadlines | Per async route, mandatory: the platform wall limit of its entry kind ([Q] `waitUntil` 30 s; cron, queue, alarm 15 min; `limits`) or the route's own await bounds plus the CPU limit. |
| Static tables | `isolate_bytes / 65,536` = 1,953 pages |
| Route counters | 256 static slots, the `u8` route-id key space |

## Tests

All compute runs on the build host as durable `systemd-run --user` jobs: one
Cargo job (`-j 4`) under `data/locks/cargo.lock`, with `MemoryMax` and zero
swap. The suites assert on isolate-wide counters and need one test thread.

```sh
cd subnet/crates/arena
cargo test --offline -j 4 --workspace -- --test-threads=1
```

- `tests/selftest.rs` runs the shared cases in `src/selftest.rs` (49 cases,
  also run as wasm32 in local workerd): the model ceiling, owner routing,
  quarantine and release, realloc charging, refusal classes, one-byte-short
  refusals with zero allocations, allocation-free refusals, overdrafts,
  strict refusals, exhaustion reported (not aborted), Observe mode,
  interleaved scoped futures, `enter`, flat repeated scopes, trims, the block
  bound, the allocation-free report, two-pass encode and hash, ingest bounds,
  `BoundedVec`, one case per first-review finding (`f_…`) and one per
  second-review finding (`f2_…`): blocks served in any order, a 4,000-seed
  fuzz of admitted allocations (fragmenting blockers, every admitted
  allocation live at once, random order, `Strict`), stale handles, `Capacity`
  for needs only the requester's own pages block, children carving only lent
  pages, child deadlines, async scopes with deadlines, `wait_until`, poison
  causes, the postmortem, the route bound, aborted short streams and the
  stack measurement.
- `tests/fuzz.rs` keeps the second review's randomized check natively
  (`RR_TRIALS`, default 4,000; `RR_SEED` traces one seed);
  `tests/deadline.rs` checks that a hung async route is abandoned at its
  deadline and poisons instead of answering `Retry`.
- `tests/regressions.rs` and `tests/abandon.rs` re-run the review's
  reproductions (a dropped pending `JsFuture`, glue under a full pool, glue
  bursts, the js-sys task cycle) and assert the fixed behaviour.
- `tests/glue_spill.rs` (`--features glue-spill`): spilled glue is counted
  and returned, never spills in `Strict` or on an armed thread, and a
  refusal it causes is `Retry` (a spilled segment returns to the pool when
  its allocation is freed), never `Capacity`.
- `tests/observe_overflow.rs` (no harness) checks that an impossible reserve
  in Observe mode returns and wraps nothing.
- `tests/native.rs`: per-thread current scope, cross-thread free, a panic in
  a scope, and the strict harness. `tests/flat.rs`: proptest properties for
  random workloads.
- `tests/source_scan.rs`: the source-scan rules (design §3.2) over this crate,
  the drill and the probe, including that every hand-written probe export
  takes its ticket (or resident token) first, that no sync route reaches an
  async entry, and that the clippy stack thresholds equal the models;
  `clippy.toml` and `glue-probe/clippy.toml` carry the disallowed lists and
  the thresholds.
- `drill/tests/handler.rs` and `drill/tests/decode_bounds.rs`: the synthetic
  handler equals a reference encoding inside its declared bound, and every
  malformed body class stays inside its derived control footprint.

## wasm32 self-tests under Node

`drill/build.sh <out>` also builds `drill-arena-fixed.wasm`, linked with the
model's fixed linear memory. `node drill/selftest-node.mjs <module>
[expected bytes]` runs every self-test case inside a module with inert
imports (no workerd, no network) and, with an expected size, checks that
linear memory equals it before and after (it never grew).

## Local workerd drill (wasm32)

The [`drill`](drill) crate is the synthetic handler: it ingests a JSON receipt
list through the bounded reader, decodes it (pre-scan, `deserialize_any`,
static messages) into a vector admitted from the declared count, and streams
the receipts into the bundle, the bundle digest and the index root. It builds
twice, once with the default allocator and once with `ArenaAlloc`; the module
has no wasm-bindgen.

```sh
drill/build.sh <out>                # drill-default.wasm, drill-arena.wasm, drill-arena-fixed.wasm
drill/run.sh <out> drill/plans/drill.json <workerd> [--inspect]
```

`run.sh` starts `workerd serve` on 127.0.0.1 with one isolate per (allocator,
scenario) and a router, and drives one invocation per request, sending each
body as a request body (an ArrayBuffer in the drill isolate, as an R2 object
would be). Scenarios: steady shapes; a leak on every invocation
(quarantine, recycle, then the stop rule's durable `arena_leak` pause);
abandoned futures (recovered, reclaimed, abandoned for good, which must
recycle); traps (co-resident invocations aborted, the step re-run once, then
a durable `arena_trap` pause); the leak with co-resident steps under the
step-crash protocol; and shapes outside the JS envelope (refused before
allocating). The driver keeps the coordinator's durable state (recycle
intents keyed by cause, the stop rule, pauses, step crashes and their
attribution). After a trap it calls the module's abort hook, never reads the
dying instance's memory, and replaces the instance, as worker-build does.
`drill/plans/jsplateau.json` runs 10,000 invocations (small shapes first,
then mixed) to show the JS high-water's plateau. `--inspect` attaches the inspector and records the JS
heap after every invocation. No remote bindings, no network beyond loopback,
no deploy. Local workerd does not enforce the 128 MB limit, so the drill
measures and does not establish enforcement.

## Glue probe (wasm32, workers-rs)

[`glue-probe`](glue-probe) is a workers-rs 0.8.7 Worker (its own workspace,
pinned by the coordinator's lockfile) with every handler body inside
`arena::enter`: Worker fetch, a Durable Object's fetch and alarm, queue,
cron and a `waitUntil` continuation. Built with worker-build twice: with the
worker-macros exports, and with every export hand-written and ticketed
(`--features admitted`: the Worker `fetch`, `queue` and `scheduled`
functions and the Durable Object class with a resident token in its
constructor). `run/drive.mjs` measures glue per entry kind at 1 to 16
in-flight invocations, Durable Object residency, a ticket burst, and a trap
with a co-resident request, after which the next instance reports the
postmortem the abort hook copied out. Its measured inputs are in
[`glue-probe/memory-model.json`](glue-probe/memory-model.json).

## Stream probe (local workerd, miniflare R2)

[`stream-probe`](stream-probe) checks the abort path `ChunkSink` and
`Recheck` rely on: a `FixedLengthStream(10)` whose readable side is an R2 put
(miniflare's in-memory R2 binding, `cf: false`, loopback only), five bytes
written, then the writer closed, aborted, left short, or completed.

```sh
stream-probe/run.sh <wrangler>/node_modules/miniflare/dist/src/index.js
```

## Measurements

See [MEASUREMENTS.md](MEASUREMENTS.md).

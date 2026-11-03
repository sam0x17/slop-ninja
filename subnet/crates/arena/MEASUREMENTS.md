# Arena prototype measurements

Measured on 2026-10-31 on the build host. Every run is a durable
`systemd-run --user` unit in `slop-a-comp.slice` with `MemoryMax` and
`MemorySwapMax=0`; Cargo runs one job at a time (`-j 4`) under
`data/locks/cargo.lock` with a task-owned target directory. Run records (logs,
per-invocation series, inspector samples) are under
`data/task-roots/testnet/workers-arena/runs/<name>`, and those named
`round2/<name>` (after the second review) under
`data/task-roots/testnet/workers-arena/round2/runs/<name>`. Those run
directories are the build host's ignored records; the figures below are the
tracked evidence. Commit ids not on `main` (`4962ae49`, `58d4de9d`, the
review rounds) are on the retained `workers-arena` evidence branch; the crate
landed on `main` as one squash commit.

Tools: workerd 2026-10-23 (bundled with wrangler 4.154.0), Node 26.13.0,
rustc 1.99.1, worker-build 0.8.7, wasm-bindgen 0.2.129, worker 0.8.7, js-sys
0.3.106, dlmalloc 0.2.12. Local workerd does not enforce the 128 MB isolate
limit, so these runs measure; they do not establish platform enforcement.
All workerd traffic is on 127.0.0.1 (`workerd serve`); nothing is deployed and
no remote binding exists.

## Native and wasm32 tests

- `cargo test --workspace -- --test-threads=1` (run `round2/c2-test`): every
  target passes: the 47 shared self-test cases, `regressions` (4), `abandon`
  (1), `deadline` (1), `fuzz` (4,000 trials), `observe_overflow` (no
  harness), `native` (7), `flat` (2 properties, 48 cases each),
  `source_scan` (5), `drill/handler` (4) and `drill/decode_bounds` (5).
- The same 47 self-test cases as wasm32 in a fresh arena instance in local
  workerd: 47 passed, 0 failed (runs `round2/smoke1`, `stack1`, `f2`, `f3`),
  including the 4,000-seed fuzz.
- `cargo clippy --workspace --all-targets` natively and for
  `wasm32-unknown-unknown` (drill with and without `arena`), and the glue
  probe's own workspace for wasm32 (both exports), with the stack lints at
  the models' derived thresholds: no warnings (run `round2/c2`).
- `observe_overflow`: an Observe-mode `reserve(usize::MAX / 2)`,
  `reserve(usize::MAX)` and an impossible aligned block return without
  panicking; held and free pages unchanged; the excess is recorded in
  `observed_excess_pages`.

### Second review: the reviewed defects, before and after

Each second-review reproduction ran against `4962ae49` in
`data/task-roots/testnet/workers-arena/rereview` (its tests assert the
defect) and is now a regression asserting the fixed behaviour:

| Finding | Before (4962ae49) | After |
| --- | --- | --- |
| Out-of-order blocks | 8-page block refused with 9 held pages (Strict: `OverdraftDenied`; Enforce, no free page: `PoolExhausted`); the randomized check refused an admitted allocation in 54 of 4,000 trials, all block admissions | served in both modes (`f2_out_of_order_blocks_are_served`); `tests/fuzz.rs`, the same trials: 0 of 4,000 refused, natively and as the wasm32 self-test |
| Default route deadline | a never-settling handler stayed an open, healthy scope for a simulated year; every need it blocked was `Retry` | `enter` takes an `AsyncRoute` (mandatory deadline); `tests/deadline.rs`: abandoned at its deadline, pinned, `Poisoned` with cause (route 1, `Abandoned`), never `Retry` |
| Child deadline | a 30 s child of a no-deadline parent never abandoned; of a 1 s parent abandoned after 2 s | own deadline (`f2_child_takes_its_own_route_deadline`): 30 s either way; a sync child inherits |
| Own pages | half the partition opened, then a half-partition block: `Retry` on all 50 retries | `Capacity` (`f2_need_blocked_by_own_pages_is_capacity`); a need another open scope blocks stays `Retry` |
| Carving the parent's block | parent held 6 -> 1 pages while its child was open; the parent's admitted 256 KiB allocation refused | children carve only lent pages; the parent's block stays held and is served with no overdraft (`f2_child_carves_only_lent_pages`) |
| Stale handle | allocated 1,000 B into the later scope reusing its page (quarantined against that route); a quarantined scope's handle grew its pins 1 -> 18 pages | the allocation fails (`StaleScope`), the later scope is untouched and released; pins do not grow; admissions refused `Defect`/`Stale` (`f2_stale_handle_never_reaches_another_scope`) |
| Short stream | the design relied on a short `FixedLengthStream` failing the put | measured pending; `ChunkSink` and `Recheck` abort (below) |
| Postmortem | worker-build's new instance cannot read the old one's statics | the abort hook's postmortem reached the next instance, naming the trapping route (below) |

### Decode error footprint (drill route, run t5)

The route's control footprint is derived (432 B: one serde_json error box and
one message of at most `MAX_ERROR_MESSAGE` bytes with its doubling growth).
Measured control peak per malformed class, under `strict::run` in `Enforce`
mode, all refused with no violation:

| Input (inside its schema bound) | Refused as | Control peak |
| --- | --- | --- |
| 300 KB unknown key (review reproduction) | `Json` | 191 B |
| 2,000-times escaped schema id (review reproduction) | `Invalid` (pre-scan) | 0 B |
| 200 KB string where a number belongs | `Json` | 202 B |
| 300 KB top-level string | `Json` | under 320 B |
| 5,000-digit number, float, negative | `Invalid` (pre-scan) | 0 B |
| out-of-range number, duplicate or missing field, bad hex, 10,000 nested brackets, truncated body | `Json` or `Schema` | under 320 B |

The reviewed decoder overdrew to 21 pages (900,634 B live) on the first input
and 14 pages on the second.

## Glue per entry point (workers-rs 0.8.7, runs round2/probe1, round2/probe2)

`glue-probe` built with worker-build 0.8.7 twice: with the worker-macros
exports, and with every export hand-written (`admitted`: the Worker `fetch`,
`queue` and `scheduled` functions and the Durable Object class with a
resident token in its constructor and a ticket in its `fetch` and `alarm`).
Served by `workerd serve`. For each entry kind, k = 1, 2, 4, 8 and 16
invocations held in flight (each awaiting a 400 ms `Delay` inside its
handler scope); the glue high-water is read before and after, as dlmalloc
footprint. Per invocation: the least-squares slope over k and the largest
ratio (delta / k). probe2 ran under the model derived from probe1 (ticket
512 B, a 3-page partition); both runs agree.

| Entry kind | worker-macros export | Hand-written export | Tickets at k = 16 |
| --- | --- | --- | --- |
| Worker `fetch` | 514 B slope, 483 B ratio; a 4 KB URL adds 3.5 KB | 290 B slope, 272 B ratio; 240 to 256 B with a 4 KB URL | 16 |
| Worker fetch with a Durable Object hop | 1,011 B slope, 979 B ratio (one entry) | 563 B slope, 544 B ratio: 272 B per entry | 32 (one per entry) |
| Durable Object `alarm` | 264 to 330 B slope, 480 B ratio | 187 to 189 B slope, 248 B ratio | 16 |
| `queue` (one batch) | 330 B slope, 304 B ratio | 239 B slope, 224 B ratio | 16 |
| `scheduled` | 495 B slope, 464 B ratio | 206 to 215 B slope, 192 B ratio | 16 |
| `waitUntil` continuation (`arena::wait_until`) | needs the context (hand-written only) | 304 B | 17 (its own ticket) |
| Durable Object instance (resident, 64 new instances) | 24 B (one allocation) | 24 B | |
| Idle (one `/stats` request in flight) | 1,096 B in 8 allocations | 872 B in 7 allocations | |

The probe's measured model (`glue-probe/memory-model.json`): static 2,048 B,
ticket 512 B (the largest entry, the 304-B continuation, rounded up to the
next power of two), resident 32 B, with the per-kind costs listed in
`glue_ticket_kinds` (`build.rs` refuses a ticket cost below any). Derived:
244 tickets, a 3-page partition.

**Ticket burst** (both runs): 276 concurrent `/hold` requests against 244
tickets: 205 served, 71 refused with 503 before allocating (scope admission
ran out first, at 205, consistent with the ticket derivation), glue peaked at
1 page (admitted build; 62.6 KB footprint) or 2 pages (macro build;
109 KB), no glue allocation failure, no ticket refused.

**Trap** (both runs, both builds): with a `/hold?ms=1500` request in flight,
a `/trap` request traps (`unreachable` inside its scope). The trapping request
answers 500; the co-resident request also fails with 500 (workerd then
cancels it as hung: its promise belonged to the dead instance). The next
request runs on a re-instantiated module (a new instance id) with a fresh
pool, and reads the postmortem the abort hook copied out of the dying
instance (`set_on_abort`, chained to worker's): version 1, the trapping
scope, route 1 (`probe/fetch`), no allocation failure, no recycle, and that
instance's stack high-water (3,848 B admitted, 3,912 B macro).

## Stack (painted high-water, runs round2/stack1, round2/probe1)

The linked wasm32 stack is `[0, 1,048,576)` (rust-lld's default for
`wasm32-unknown-unknown`, stack first; scratch probe 2026-10-31). The pool
paints the unused stack at initialisation (a 256-B guard below the painting
frame stays unpainted, so the figures include it):

| Build | Routes | High-water |
| --- | --- | --- |
| drill (arena) | lane, package, packagestream, mixed, quarantine, jsenvelope | 2,848 B |
| drill (arena) | abandon, trap, leakstep (async) | 2,944 B |
| glue probe (admitted) | every entry kind | 3,848 B |
| glue probe (macro) | every entry kind | 3,912 B |

Models: drill 2,944 B, probe 3,912 B, both far inside half the stack; the
clippy frame, future and stack-array thresholds are `stack_bytes / 2 -
high-water` (521,344 and 520,376 B).

## Short streams and the abort path (round2/fls, local workerd)

`stream-probe` under miniflare 5.20261023.1-beta (its bundled workerd
2026-10-23, an in-memory R2 bucket, `cf: false`, loopback only): a
`FixedLengthStream(10)` whose readable side is `R2Bucket.put`, five bytes
written, then:

| Writer ends with | Put | After | Stored |
| --- | --- | --- | --- |
| five more bytes and `close()` | stored | 148 ms | yes, 10 B |
| `close()` | rejected: "FixedLengthStream did not see all expected bytes before close()" | 154 ms | no |
| `abort(reason)` | rejected with the reason | 3 ms | no |
| nothing (`releaseLock()`) | still pending at the 3 s bound | 3,001 ms | no |

A `Response` consumer behaved the same way in the second review
(`rereview/runs/fls1`). So `ChunkSink` and `Recheck` abort the stream on
every refusal, and the host awaits the put's rejection under a bound.

## JS heap (inspector, runs j2, j3)

The drill handler receives its body as a request body (an ArrayBuffer in the
drill isolate) and writes its bundle to an exact-size JS buffer. After every
invocation the inspector reads `Runtime.getHeapUsage`; collections are forced
every 5 to 20 invocations. JS high-water = V8 committed heap + ArrayBuffer
backing stores + embedder heap (the wasm memory is not among them).

| Receipts | Invocations | JS need per invocation | JS high-water | V8 heap | Backing stores | After forced GC |
| --- | --- | --- | --- | --- | --- | --- |
| 1,000 | 200 | 0.42 MB | 11.1 MB | 1.8 MB | 8.7 MB | 10.0 to 10.5 MB |
| 4,000 | 80 | 1.70 MB | 19.5 MB | 1.8 MB | 17.2 MB | 18.5 to 19.0 MB |
| 8,000 | 80 | 3.39 MB | 36.5 MB | 1.8 MB | 34.1 MB | 35.5 to 36.0 MB |
| 13,000 | 80 | 5.51 MB | 57.7 MB | 1.8 MB | 55.3 MB | 56.7 to 57.2 MB |
| 20,000 | 80 | 8.48 MB | 78.4 MB | 1.8 MB | 76.5 MB | 19.0 to 23.9 MB |
| 26,000 | 80 | 11.0 MB | 79.7 MB | 1.8 MB | 77.4 MB | 46.2 to 52.6 MB |
| 41,000 | 60 | 17.4 MB | 89.0 MB | 1.6 MB | 87.1 MB | 65.0 to 71.1 MB |
| 65,000 | 40 | 27.6 MB | 112.3 MB | 1.6 MB | 110.5 MB | 46.6 to 111.8 MB |

These short runs are not maxima (the second review's finding): the
1,000-invocation drill went 10.8 MB past the 26,000-receipt high-water above.

### Long runs (runs round2/f2, round2/plat1, round2/steady-*)

Arena build, inspector after every invocation, a collection forced every
100. The JS high-water's last new maximum shows whether it stopped rising
(`round2/jobs/jsplateau.py`; per-run figures in each run's `jsplateau.json`).

| Run | Invocations | Shapes (receipts) | JS high-water | Last new maximum | After forced GC |
| --- | --- | --- | --- | --- | --- |
| f2 package | 1,000 | 26,000 | 86,133,420 B (82.14 MiB) | 718 | |
| f2 packagestream | 1,000 | 26,000, streamed | 90,508,484 B (86.32 MiB) | 66 | |
| steady-package26k | 5,000 | 26,000 | 86,653,860 B | 354 | 46.9 to 64.6 MB |
| steady-packagestream26k | 5,000 | 26,000, streamed | 89,983,316 B | 12 | 20.3 to 64.4 MB |
| steady-package20k | 5,000 | 20,000 | 83,693,636 B | 2,832 | 16.1 to 66.9 MB |
| steady-packagestream20k | 5,000 | 20,000, streamed | 78,633,280 B | 23 | 24.6 to 53.2 MB |
| plat1 (arena) | 10,000 | 3,000 at 1,000, then 1,000 to 26,000 mixed | 83,544,676 B | 4,040 | 4.2 to 66.4 MB |
| plat1 (default allocator) | 10,000 | the same | 83,521,716 B | 3,440 | 7.4 to 79.7 MB |

The small-shape phase of plat1 plateaued at 44.6 MB within its first 2,000
invocations. These plans (`drill/plans/jsplateau.json`, `jssteady.json`) ran
on the modules of the previous model (envelope 26,000), whose JS check
admitted every shape; the JS heap does not depend on the pool.

The drill's measured model takes the envelope at 20,000 receipts, the largest
measured shape that fits (26,000 would need a long-run high-water of at most
88,205,460 B and reached 90,508,484 B): `js_reserve_bytes` = 83,693,636 B
(the long-run high-water) + 8,480,236 B (one more invocation's need) =
92,173,872 B; `js_envelope_bytes` = 8,480,236 B. Pool: 528 pages beside
1,179,648 B of static memory; glue partition 1 page (273 tickets of 64 B);
largest route 163 pages; quarantine budget 364 pages.

An earlier run (j1, discarded) generated the bodies as JSON strings inside
the drill isolate; its V8 heap reached 540 MB, which is the harness, not the
handler.

**After a trap** (run j2, 13,000 receipts, an 8.8 MB instance, a trap every
10 invocations, collections forced every 10): workerd's resident set grew by
25 MB over ten re-instantiations, so the old instances' memories were
collected rather than retained.

## 1,000-invocation drill (runs round2/f3, round2/f3b)

Plan `drill/plans/drill.json`, final modules (run f3: arena `4a21b612…`,
default `c550b7be…`), the drill's measured model
(`arena-drill-2026-10-31-r2`), the inspector attached for the whole run. Each
(scenario, allocator) is its own isolate; 1,000 invocations each (300 for the
envelope scenario). After a trap the driver calls the module's abort hook,
never reads the dying instance's memory, and replaces the instance, as
worker-build does; it keeps the coordinator's durable state (recycle intents
keyed by cause with the stop rule, pauses, step crashes and their
attribution). Linear memory in MiB after invocations 1, 2 and the last; the
arena ceiling is static memory plus the pool (34.12 MiB). JS is the inspector
high-water (heap, backing stores and embedder heap) in MiB. Both allocators
produced identical bundle and index digests for every shape (CONSISTENCY all
true), and every expectation in the plan held. The `leakstep` rows are run
f3b (same modules): in f3 its co-resident steps used the scenario's
12,900-receipt shape, one was refused beside the leak, and none happened to be
in flight at either recycle, so its `attributed.recycle` expectation failed;
the plan now gives those steps a 1,000-receipt shape (`shape` on the fate),
so a step is always in flight when the recycle fires.

| Scenario | Allocator | Memory 1 / 2 / last | Max | JS high-water | Outcome |
| --- | --- | --- | --- | --- | --- |
| lane (1,000) | default | 1.75 / 1.75 / 1.75 | 1.75 | 42.86 | 1000 served; growth after invocation 2: 0 B |
| lane (1,000) | arena | 1.94 / 1.94 / 1.94 | 1.94 | 42.55 | 1000 served; growth after invocation 2: 0 B |
| package (20,000) | default | 11.19 / 11.19 / 11.19 | 11.19 | 74.99 | 1000 served; growth after invocation 2: 0 B |
| package (20,000) | arena | 11.38 / 11.38 / 11.38 | 11.38 | 72.24 | 1000 served; growth after invocation 2: 0 B |
| packagestream (20,000) | default | 8.31 / 8.31 / 8.31 | 8.31 | 74.99 | 1000 served; growth after invocation 2: 0 B |
| packagestream (20,000) | arena | 8.50 / 8.50 / 8.50 | 8.50 | 75.24 | 1000 served; growth after invocation 2: 0 B |
| mixed (1,000, 20,000, 13,000, 4,000, 16,000) | default | 1.75 / 11.81 / 11.81 | 11.81 | 72.79 | 1000 served; growth after invocation 2: 0 B |
| mixed (1,000, 20,000, 13,000, 4,000, 16,000) | arena | 1.94 / 11.38 / 11.38 | 11.38 | 71.24 | 1000 served; growth after invocation 2: 0 B |
| quarantine (12,900) | default | 7.62 / 10.94 / 219.50 | 219.50 | 73.27 | 1000 served; nothing bounds the leak |
| quarantine (12,900) | arena | 7.81 / 11.25 / 28.44 | 28.44 | 38.72 | 14 served; 986 refused while paused; 2 recycles (cause 1:1:arena-drill-2026-10-31-r2: 2); `arena_leak` pause at invocation 15; 14 quarantines |
| abandon (1,000) | default | 1.75 / 1.94 / 29.69 | 29.69 | 42.62 | 600 served; abandoned bodies never reclaimed |
| abandon (1,000) | arena | 2.00 / 2.75 / 1.94 | 32.06 | 41.33 | 600 served; 171 refused while paused; 2 recycles (cause 2:2:arena-drill-2026-10-31-r2: 2); `arena_leak` pause at invocation 574; co-resident steps aborted: 14 (attributed to a recycle 14, to another route's trap 0, counted as their own crash 0); 154 abandoned (49 recovered, 49 reclaimed) |
| trap (1,000) | default | 1.75 / 1.75 / 1.94 | 1.94 | 40.08 | 880 served; 40 step crashes, 20 `arena_trap` pauses; co-resident steps aborted: 20 (attributed to a recycle 0, to another route's trap 20, counted as their own crash 0) |
| trap (1,000) | arena | 1.94 / 1.94 / 2.75 | 2.75 | 34.71 | 880 served; 40 step crashes, 20 `arena_trap` pauses; co-resident steps aborted: 20 (attributed to a recycle 0, to another route's trap 20, counted as their own crash 0); 40 postmortems named the trapping route |
| leakstep (12,900) | default | 7.62 / 7.62 / 220.00 | 220.00 | 72.49 | 800 served; nothing bounds the leak |
| leakstep (12,900) | arena | 7.81 / 7.81 / 2.81 | 29.94 | 39.53 | 14 served; 786 refused while paused; 2 recycles (cause 1:1:arena-drill-2026-10-31-r2: 2); `arena_leak` pause at invocation 19; co-resident steps aborted: 2 (attributed to a recycle 2, to another route's trap 0, counted as their own crash 0); 14 quarantines |
| jsenvelope (1,000, 20,000, 26,000) | default | 1.75 / 11.81 / 11.81 | 11.81 | 76.33 | 200 served; 100 outside the JS envelope refused before allocating |
| jsenvelope (1,000, 20,000, 26,000) | arena | 1.94 / 11.38 / 11.38 | 11.38 | 74.81 | 200 served; 100 outside the JS envelope refused before allocating |

Arena counters across the steady scenarios (lane, package, packagestream,
mixed): every scope released, glue at 0 bytes, three blocks served directly
per invocation (body, entry vector, bundle or chunk buffer), no quarantine,
overdraft, refusal, allocation failure, stale use, route-bound excess or
strict violation. In the fault scenarios the glue partition peaked at 1 page
(2,128 B for 38 tickets in flight); no glue allocation failed and no ticket
was refused. The JS high-water peaked at 76.33 MiB (80.0 MB) against the
92.2 MB reserve. The painted stack high-water was 2,848 B (synchronous
routes) and 2,944 B (async).

The previous model (envelope 26,000, run round2/f2) passed the same plan with
the fault protocol giving the same outcomes (2 recycles per cause, then the
pause), but its inspector readings were what showed the reserve was too low
(90.5 MB against 90.8 MB; section "Long runs").

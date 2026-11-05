# Workers coordinator (testnet)

This crate is the serverless coordinator for the Slop Ninja testnet subnet
(netuid 601): a Rust Cloudflare Worker built with workers-rs 0.8.7. It follows
finalized blocks, keeps a durable chain journal and, from M3, signs and submits
its own commit-reveal v4 timelocked weight commit once per subnet epoch with
the coordinator hotkey, journaled in the ChainJournal Durable Object (see
[Signing](#signing-m3)). It is deployed on testnet as
`slop-ninja-coordinator-testnet`; [TESTNET.md](TESTNET.md) is the runbook, with
the 2026-10-30 measurements, the first live commit and reveal, and the drills.
The same crate also runs hidden evaluation (milestone M4,
[below](#hidden-evaluation-m4)), deployed on testnet as the separate Worker
`slop-ninja-eval-testnet`, which has no coordinator hotkey and signs no
weights.

The crate is its own Cargo workspace, separate from the native `subnet/`
adapter, so the Wasm build never pulls the native rtao runtime. It depends on
the shared, wasm-clean [protocol crate](../crates/protocol/README.md) by path:
the testnet genesis guard checks against its `TESTNET_GENESIS`, the T12
target is verified and its row derived with it, and `src/wire.rs` adds only
JSON encodings of chain identifiers. Chain encoding and cryptography come from
rtao `fa83052d`'s portable core seam, `rtao-core` with default features off
(upstream `bittensor-core` without `host`). All logic except `src/cf.rs` is
plain Rust, tested natively against SQLite, which is the dialect Durable Object
storage uses. `src/cf.rs` holds the Workers entry points, the Durable Objects
and the `fetch` RPC transport, and compiles only for `wasm32`.

| Module | Role |
| --- | --- |
| `webhook` | rtao-rpc receiver: contracts `rtao-block-webhooks/1` and `/2` (wire version 1), HMAC authentication and body validation |
| `journal` | delivery dedup, the per-block path, the cursor, durable faults, RPC audits |
| `heartbeat` | what one delivery, tick or RPC round does inside one transaction |
| `watchdog` | primary/fallback mode, thresholds and durable transitions |
| `claims` | durable leases with fencing generations, and the shared RPC budget |
| `schedule` | the bounded scheduling step each block drives (no-op hook for now) |
| `chain` | rtao core seam: by-name calls, storage keys and values, signing payloads, extrinsics, sr25519, timelock, epochs |
| `signing` | the signing journal: one commit per epoch, attempts, fencing, reconciliation, read-back and reveal; `signing::auto`, the automatic target handoff (each closed cohort installed and authorized once) |
| `target` | T12 bundle verification and the pluggable reward-rule row |
| `rpc` | JSON-RPC request bodies and response validation, including the single write |
| `config` | vars, secrets, testnet guards, names and bearer-token checks |
| `eval::engine` | cohort phases, claims, the draw, generation, seal, scoring, receipts and packaging over SQL |
| `eval::plan`, `eval::pool` | the frozen operator plan; the hidden human pool and the never-reuse draw |
| `eval::textgen` | Workers AI prompts, output validation, exact-span splicing and list prices |
| `eval::score`, `eval::receipts`, `eval::package` | outcomes and components; chunked receipts; package inputs and the reward policy |
| `eval::chain` | twox storage keys and the `state_queryStorageAt` epoch read |

## Routes

| Entry point | Behaviour |
| --- | --- |
| `POST /hook` | rtao-rpc finalized-block webhook: the primary heartbeat. |
| `GET /status` | Content-free JSON, served only to the status gate's callers (below). |
| `POST /fault/clear` | Operator action, body `{"fault_id": n, "reanchor": false}`. Needs `COORDINATOR_OPERATOR_TOKEN`. |
| `POST /signing/target` | Operator action: install a T12 bundle as the current target (body: the bundle JSON, at most 1 MiB). |
| `POST /signing/control` | Operator action, body `{"action": "resume" \| "pause" \| "clear_fault" \| "arm_drill" \| "auto_pause" \| "auto_resume" \| "auto_abandoned" \| "auto_unabandoned" \| "auto_gap", "drill": ..., "count": n, "label": ..., "cohort": n}`. |
| `POST /signing/kick` | Operator action: run one signing round now (the duplicate-wakeup drill). |
| `GET /eval/status` | `EVAL_ROLE = "eval"` only. Content-free cohort status, with every scorer slot's running state. The operator or monitor token, or the publisher's token through its service binding. |
| `GET /eval/admission/link` | `EVAL_ROLE = "eval"` only. Probes the header-check link to the upload Worker (service binding, internal host, shared interface token) without an upload, and reports whether the signer's binding and token are configured: `{"header": "ok", "signer": {...}}` or the failure. Operator or monitor token. |
| `POST /eval/plan`, `/eval/clear` | Queue a cohort plan; clear a resumable cohort fault (`{"label": ...}`). Operator token. |
| `POST /eval/abandon` | End a faulted cohort without a target (`{"label": ..., "reason": "<code>"}`); the only exit of a terminal fault. Operator token. |
| `POST /eval/probe`, `/eval/scorer-probe` | Pre-flight one generator, or the scorer path, on fixed synthetic text; the measured cost is settled in the spend ledger. Operator token. |
| `GET /eval/gen/health`, `POST /eval/gen/measure` | Generation v2: whether the generation Worker (`../gen-worker`, service binding `GEN_TESTNET`) resolves both vendor keys; and one call of the compiled synthetic measurement set (`{manifest: {key, sha256, bytes}, model, effort, call, attempt}` and no other field, never text), each vendor call settled in the spend ledger under its own key (`probe:gen-measure-<vendor>:<model>:<effort>:<call>:<attempt>`), a call without final usage at its reservation bound. Operator token. Plans naming `generation` are refused by this build, although the draw, lanes, commit and seal run them (`src/eval/gen_v2.rs`, store v26). |
| `POST /eval/gen/vendor-clear` | Generation v2: the operator's clear of a vendor pause (`{"vendor": "openai" \| "anthropic"}`; a spend cap or key failure pauses that vendor for every later cohort until cleared). Answers `{vendor, cleared}`; idempotent. Operator token. |
| `POST /eval/template`, `/eval/template/pause`, `GET /eval/calendar` | Install the standing cohort template (journaled; the installed bytes again are a replay that changes nothing); pause or resume opening cohorts from it (`{"paused": bool}`); the cohort calendar. Operator token. |
| `POST /eval/session/refuse` | Refuse a stalled session (`{session, reason}`). Operator token. |
| `/admission/...` | Always 404. The public admission routes (contract `slop_ninja.admission/v2`, `docs/testnet/admission-api.md`) are the key-free upload Worker's (`subnet/admission-worker`, `slop-ninja-admission-testnet`), not this Worker's. |
| `POST /internal/admission/v2/<op>` | `EVAL_ROLE = "eval"` only. The internal admission interface, reachable only through the upload Worker's service binding: a request must name the internal host `eval-admission.internal`, which no public route carries, and present the shared `ADMISSION_INTERFACE_TOKEN` (compared in constant time). Anything else answers 404. The body is one forwarded, already checked request (`slop_ninja_admission_wire::internal::Forward`), relayed to the cohort authority. |
| cron `* * * * *` | The watchdog. On the coordinator it is the only timer, and no Durable Object alarms are set; on the eval Worker it also kicks the cohort authority and, outside scoring, stops idle scorer slots, while the cohort and scorer Durable Objects keep bounded alarms. The finalized commitment reader is per-block work: each new block's delivery drives it, and the cron only while the watchdog is in fallback (when no delivery arrives), never as its own schedule. |

Without its token secret, a route returns 503. A wrong token gets 401. Every
token must be at least 32 bytes. They are compared in constant time over
SHA-256 digests.

**The status gate** (`access`). The status documents carry no text, label or
prediction, but they are a real-time timing and aggregate feed: per-job call,
server and CPU times, chunk-attempt histograms, document counts by kind and
status, the draw summary and spend. Both Workers keep a `workers.dev` name
(the rtao-rpc webhook reaches `POST /hook` there, authenticated by its
signature), so the detailed reads are served only to:

| Caller | Token (Worker secret) | From | Reads |
| --- | --- | --- | --- |
| operator | `COORDINATOR_OPERATOR_TOKEN` | any host | every status route, including `GET /eval/load/status`, `GET /eval/admission/link` and `GET /eval/authority/<name>/status` |
| monitor (the operator's unattended read-only jobs) | `STATUS_TOKEN_MONITOR` | any host | every status route, as the operator; no route that changes state |
| publisher | `STATUS_TOKEN_PUBLISHER` | its service binding only (`https://status.internal/...`) | `GET /status`, `GET /eval/status` |
| alerts relay | `STATUS_TOKEN_ALERTS` | its service binding only | `GET /status`, reduced to the liveness fields its rules read |
| pre-split shared token | `COORDINATOR_STATUS_TOKEN`, while installed | service bindings only | as the publisher |

"From a service binding" means addressed to the host `status.internal`, which
no public route of either Worker carries; a public request arrives on the
`workers.dev` name, so a consumer token presented there gets 401 however it
leaked. That depends on Cloudflare never delivering a public request whose URL
names that host: the gate has no other sign that a request came through a
binding, so TESTNET.md ("Status token split") probes it with a forged `Host`
and an absolute-URL request line after each gate deploy.

The gate covers only the GET status routes above. Every route that changes
state checks the operator token alone, so the monitor token never installs a
target, revokes, restarts or controls signing, abandons a cohort or clears a
fault. It exists so that the deploy idle and schema checks, the spend tracker
and the watchers do not hold the operator token. The trade-off: it is the one
token besides the operator's that reads the full timing feed from a public
host. It is held only on the operator's host, never by a Worker, and leaking
it exposes the feed but no control. Giving those jobs the operator token
instead would leave no public-host read-only credential, but its leak would
hand over signing control.

The alerts relay's `/status` is its liveness subset: the ChainJournal's
confirmed watermark age, its durable fault's kind and the signing state
(`access::alerts_journal`), with no cohort status, liveness record or arena
counters. For the other callers, `/status` also embeds the main cohort's status
(`eval_cohort`), the same document `GET /eval/status` serves without its
limiter and ledger blocks. Each token is a separate value per Worker. `/status`
reports `access`: the caller, which tokens are installed (never their values),
whether they are all distinct, and whether the shared pre-split token is still
installed. It is accepted only so that the publisher and the relay can move to
their own tokens without a gap; delete it from both Workers once they have
(TESTNET.md). The publisher alerts on equal tokens and on a shared token left
installed beside both consumer tokens (`status_access_warning`). The public
status the publisher derives from these reads names no work phase, although
publication itself still follows packaging (publisher README).

`/status` reports only:

- configuration presence and the RPC host
- public chain identifiers: genesis, netuid, block heights and hashes
- the provisional cursor, the RPC-confirmed watermark with its basis and age,
  and the count of provisional blocks
- the mode, its reason and ages, the lag and the behind flag
- `chain_journal.watchdog`, the same at a glance: `source` (`webhook` in
  primary, `otf_rpc` while the cron walks the cursor through the public
  RPC), `cursor_lag_blocks`, `last_finalized` (the cursor's height, hash and
  age), `confirmed` (the RPC-confirmed watermark's), and `subscription`
  ([Watchdog](#watchdog))
- the last five mode transitions
- the active fault, if any, with its public block hashes
- the backfill claim's state
- RPC calls used this minute
- the settings and named counters

It also reports the `signing` section: the signer state and pause reason, the
coordinator's public hotkey, the nonce floor, the live runtime versions, the
active target digest, the signer claim, the last six operations with their
attempts (nonce, era, state, sends, extrinsic hash, inclusion, dispatch) and
reveal observations, armed drills and the last twelve signing-log events.

It never shows bodies, delivery, event or subscription IDs, body digests,
secrets, signed bytes, ciphertexts or Cloudflare account identifiers. Native
tests check this.

## Heartbeat

The rtao-rpc finalized-stream subscription delivers every finalized block. Each
delivery is a separate Worker invocation and is the coordinator's clock. A
delivery goes through these steps:

1. **Authenticate.** The Worker checks `X-Rtao-Signature` against the exact raw
   body before parsing anything. It requires:
   - signature version 1, the configured key ID and the configured subscription
   - a nonzero attempt and at most 300 s of timestamp skew
   - a valid MAC: HMAC-SHA256 under the hex-decoded 32-byte secret, in the
     contract's framing, compared in constant time

   Bodies over 256 KiB, unknown fields, non-canonical hex and non-decimal u64
   strings are refused. An authenticated challenge gets its nonce back in
   `X-Rtao-Challenge-Response` with 204 and is never recorded. Block events must
   match the signed `event_id`, event version 1 and the testnet genesis. Only
   `finalized` is accepted. `X-Rtao-Webhook-Contract` must name
   `rtao-block-webhooks/1` or `rtao-block-webhooks/2`, or be absent (contract
   1 before the provider named it); each new delivery counts toward
   `deliveries_contract_v1`, `_v2` or `_unnamed`
   ([Contract 2 timing](#contract-2-timing)).
2. **Persist, deduplicate and run the step.** One `transactionSync` in the
   ChainJournal Durable Object:
   - records the delivery and its block event
   - deduplicates by delivery ID, and by (stream, block hash) for the block
   - notes the delivery time for the watchdog
   - runs the per-block path
   - runs one bounded scheduling step
   - claims any RPC work under the durable `rpc-backfill` lease and charges its
     calls to the shared budget
3. **Acknowledge.** The Worker returns 204 only after that commit. Any planned
   RPC round then runs in `ctx.waitUntil`, after the response.

Each case gets a different response:

| Case | Response |
| --- | --- |
| Provider retry: same delivery ID, same bytes | 204, nothing processed again |
| Same delivery ID, changed bytes | 409 |
| Same event ID naming another block | 409 |
| New delivery ID for a recorded block (re-registration, or a block RPC backfill already supplied) | 204, nothing processed again |
| Authentication failure | 401 |
| Invalid body | 400 or 422 |
| Unknown `X-Rtao-Webhook-Contract` | 422 (`webhook_contract`) |
| Missing configuration, or storage failure | 503, which the provider retries |

The provider retries every non-2xx answer and pauses the subscription after
its last retry (see the receiver contract below), so a persistent conflict
pauses it.

### The per-block path

Webhook deliveries and RPC backfill share one code path (`journal::drain`).
Recorded blocks are applied in height order:

- **Advance.** The block at `cursor + 1` advances the cursor only when its
  parent hash equals the cursor hash. The cursor and the retained finalized
  chain (`finalized_blocks`) are written together.
- **Late block.** A block at or below the cursor is compared with the retained
  chain. A block below the anchor is recorded as `below_anchor`.
- **Gap.** A missing height below a recorded block is a gap.
- **Conflict.** A broken parent link, or two hashes at one height, is a
  conflict. It is recorded in `faults` in the same transaction and stays active
  until an operator clears it.

While a fault is active, deliveries are still authenticated, persisted and
acknowledged, but the cursor does not move, and neither deliveries nor ticks
make RPC calls. `POST /fault/clear` either resumes or, with `reanchor: true`,
drops the cursor and the retained chain so the next delivery or tick anchors
again. Blocks set aside as conflicting are judged again on new evidence after a
clear.

### RPC use and trust

The heartbeat's transport (`src/rpc.rs`, `cf.rs`) is read-only. It uses three
methods, `chain_getFinalizedHead`, `chain_getHeader` and the list form of
`chain_getBlockHash`, sent as one JSON-RPC call per HTTPS POST with a named
User-Agent. Each call has an 8 s abort timeout and a 64 KiB response cap. The
signing journal's calls ([Steps](#steps)) go through the same transport and
budget; metadata, block bodies and storage reads may be up to 4 MiB, and
metadata has a 20 s timeout.

| Situation | RPC calls |
| --- | --- |
| Delivery that extends the cursor | none |
| First cursor | `chain_getFinalizedHead` and its header (2) |
| Gap | one `chain_getBlockHash` list per round, from the cursor height up to 32 heights and never above the RPC finalized head last read; at most 2 rounds per delivery, the rest carried to later deliveries; a gap above that head waits for the next tick |
| Watchdog head read | 2, at most once per tick |
| Fallback | at most 3 list calls per tick |

Gap backfill accepts a hash list only when its first entry equals the retained
hash at the cursor height. Each later entry becomes a block whose parent is the
previous entry, and it goes through the same per-block path. The list stops at
the first height the node does not have yet (`rpc_lists_not_served`), and the
invocation spends no further round on it: the next delivery or tick asks again.

A list never reaches above the RPC finalized head last read (`gaps_above_rpc_head`
counts the deliveries that found the gap above it). Above that head the endpoint
answers from its best chain, which can still be another fork than the block a
delivery named final; recording such a hash would apply it (its parent is the
cursor) and then fault on the delivered block's parent. The tick's head read
raises the head, and its next round fills the gap from the endpoint's finalized
chain in the same invocation. Only the provisional cursor waits: the confirmed
watermark moves on RPC head reads alone, so nothing that acts on blocks waits
longer.

All calls are charged to a budget held in the Durable Object and shared by every
invocation: `RPC_CALLS_PER_MINUTE`, default 12, at most 20. The public endpoint
throttles near 25 calls a minute per source. Work the budget defers is carried
over.

**Trust: provisional cursor, RPC-confirmed watermark.** Notifications alone
establish neither finality nor authorization. An authenticated finalized
delivery advances the cursor without RPC, but only along an unbroken parent
chain from an anchor taken from the RPC finalized head, and the blocks it adds
are provisional. A separate watermark, `rpc_confirmed` (schema v3), records the
highest retained block that RPC evidence independent of the provider covers.
Three things move it:

- **Anchor:** the anchor itself, read from the RPC finalized head.
- **Head audit:** a watchdog head read whose hash equals the retained block at
  its height. When the cursor already holds that height, the hashes are
  compared at once. Otherwise the head is kept as an audit point (at most 16)
  and checked when the cursor reaches it.
- **RPC list:** a block applied from an RPC hash list at or below the last RPC
  finalized head. Lists are never planned above that head (gap backfill,
  above), so every listed block is confirmed as it applies.

A block hash commits to its parent, so confirming one retained height confirms
the retained chain below it. The watermark never moves down. A re-anchor drops
it together with the cursor. A mismatch is an `rpc_head_conflict` fault and
never confirms anything.

Scheduling reads only the watermark (`schedule::due_operations`). Cohort
transitions and scoring will, and signing does, act only on confirmed blocks,
never on the provisional cursor. This uses no RPC beyond the one head read per
tick. In primary mode a delivered block is normally confirmed by the next tick,
within about a minute. A head the provider has not delivered yet is confirmed
when its delivery arrives. In fallback, blocks are confirmed as they are
applied. `/status` reports the watermark, its basis and age, and
`provisional_blocks` (cursor height minus confirmed height). The watermark
removes the old exposure: a parent-linked but wrong chain from a compromised or
faulty provider could advance the cursor until the next audit. It still
advances only the cursor; nothing acts on those blocks.

### Contract 2 timing

Contract `rtao-block-webhooks/2` differs from `/1` only in timing: the provider
pushes each finalized block within about a second of finality, possibly before
the RPC endpoint this Worker reads serves that block (by 0 to 3 blocks). The
receiver accepts both. Nothing on the delivery path reads the delivered block
through RPC:

- **Cursor.** A delivery advances the provisional cursor on the parent link
  alone, with no RPC call.
- **Anchor and head reads.** They read the endpoint's own finalized head, which
  it serves. A head the endpoint reports, whose header a node behind it does
  not serve yet, answers `null`: that is `rpc_not_served`, retried in the
  delivery's second round or at the next delivery or tick, never a fault. A
  head below the cursor is compared at once (it confirms the retained block);
  the behind rule's lag saturates at 0.
- **Backfill.** Lists stay at or below the RPC finalized head (above), and a
  `null` height ends the list for that invocation.
- **Confirmed watermark, signing, cohort kicks and the commitment reader.**
  They act and read only at the confirmed watermark or retained blocks below
  it, and the watermark is never above a finalized head the endpoint reported.
  Under `/2` a delivered block is confirmed at the first tick whose head read
  reaches it, typically the next one.

`heartbeat::lag_tests` drives the heartbeat against an RPC that lags the
deliveries by 0 to 3 blocks, with and without a different best-chain fork above
its finalized head, retried and reordered deliveries, `null` headers and
truncated lists. After every step it checks that there is no fault, that the
cursor and the watermark stay on the delivered chain, and that the watermark
stays at or below the endpoint's finalized head. At the end it checks that
every height was applied exactly once.

## Watchdog

The cron trigger fires every minute. A tick is cheap: it reads the last
accepted delivery time and the cursor, and makes at most one head read.

- **Silent.** No valid delivery for `WATCHDOG_SILENCE_SECS` (default 300). Before
  the first delivery, silence counts from the first tick.
- **Behind.** The head read finds the cursor more than `WATCHDOG_BEHIND_BLOCKS`
  (default 25, which is 300 s of 12 s blocks) below the RPC finalized head, even
  while deliveries arrive.
- **Fallback.** Entered when silent or behind. Each tick then walks the cursor
  toward the RPC finalized head through the same per-block path, with the same
  dedup, cursor and claim, at most 3 × 31 new heights per tick. The rest carries
  over to the next tick. A fallback round that advances the cursor also runs
  the scheduling step, since no delivery does.
- **Back to primary.** A valid delivery returns the coordinator to primary when
  it is not behind (`webhook_resumed`). A tick returns it once deliveries are
  fresh and the lag is within the threshold (`caught_up`, or `webhook_resumed`).
  Overlap is safe: a delivery for a block that fallback already applied is a
  duplicate.

Every mode change is a row in `mode_transitions`, with the time, both modes, the
reason, and the cursor and head heights. `/status` shows the current mode, the
reason, the lag, the behind flag and recent transitions.

A tick that fires twice within 50 s skips its second head read. If no tick runs,
nothing is lost: the next delivery or tick continues from durable state.

When the cursor has no anchor, the first tick's head read anchors it too, so a
deployment with no subscription still starts following after the first tick,
and switches to fallback once the silence threshold has passed.

**A paused subscription.** The provider pauses a subscription after the last
failed attempt of one delivery and never resumes it by itself
([below](#long-work-claims-and-invocation-limits)). The coordinator cannot
read the subscription's state (the provider's management API needs a
credential the Worker does not hold), so it infers it from durable state
(`watchdog::subscription`): `delivering` (a valid delivery within the
silence threshold), `silent` (fallback carries the cursor; the provider may
still be retrying), or `presumed_paused` once no valid delivery has arrived
for longer than the provider can retry one delivery while the RPC finalized
head is past the last delivered block. That horizon, `pause_horizon_ms`
(about 7.0 h), is derived from the provider's versioned retry contract
(`rtao_rpc_webhook_retry_v1`: ten delays at their full 20% jitter plus
eleven 5 s acknowledgement windows; `/hook` never sends `Retry-After`). A
chain that stops finalizing owes no delivery, so silence alone is never a
pause. Failures on the provider's own side (`service_error`) never count
toward its retries and never pause, so a provider-side stall longer than the
horizon also reads as `presumed_paused`. Each tick that finds it presumed
paused counts `subscription_presumed_paused_ticks`; the publisher's
`webhook_paused` rule alerts on the state, and an operator checks the
subscription's state (`GET`, TESTNET.md) and resumes it only when it is
paused (blocks during a pause are not owed: fallback has already carried the
cursor, and the next delivery returns the coordinator to primary). The
service does not name its retry contract; the horizon assumes the deployed
service runs the one above, which the operator checks against the service's
`GET` shape when it changes.

## Long work: claims and invocation limits

Each block's delivery is its own invocation, so a slow task started by block N
must not block block N+1, and no Queue or other dispatch layer is used. Work is
claimed in the Durable Object under an idempotent operation ID
(`claims::claim`), with a holder, a generation and an expiry:

- **Overlap.** An overlapping invocation sees a live claim and skips the work.
- **Crash.** A claim whose holder crashed expires and is re-claimed with the
  next generation. The stale holder's late result, renewal or completion is
  refused, which fences it.
- **Completion.** Completion is durable, and later claims return `Done`.

The `rpc-backfill` claim serializes RPC anchoring and backfill this way. The
scheduling step claims due operations the same way. It runs once per new
delivery, and once per RPC round that raises the confirmed watermark. It judges
what is due at the watermark. None is due yet: cohort transitions and scoring
arrive in M4. Signing has its own claim, `signer`, and its own step loop
([Signing](#signing-m3)), driven after each delivery and tick.

Two sets of limits shaped the design.

**Provider: rtao-rpc receiver contract and pilot notes.**

- A receiver must acknowledge within 5 s.
- Any non-2xx answer (redirects, 401, 403, 404, 408, 429 and 5xx included), a
  timeout, or a connection, DNS or TLS failure is a failed attempt. A failed
  delivery is retried with the same ID and body up to ten times after the
  first attempt, after about 2 s, 6 s, 18 s, 54 s, 2.7 min, 8.1 min, 24.3 min,
  72.9 min, 2 h and 2 h (each with jitter of up to 20%), about 5 h 49 min in
  all; `Retry-After` can lengthen a delay up to 2 h.
- After the last failed attempt the subscription pauses (cause
  `receiver_failures`, with the last undelivered block). Resuming starts at
  the head, or replays from the last undelivered block on request. Blocks
  during a pause are not owed, so after any pause the coordinator backfills
  from its finalized cursor through RPC. A registration starts paused.
- Each subscription has one delivery in flight at a time, so a slow
  acknowledgment delays every later block.
- The coordinator has no resume path of its own: after a long outage the
  subscription stays paused and the cron and RPC fallback carries progress;
  the watchdog reports it `presumed_paused` and the publisher alerts
  (`webhook_paused`), so it is an alerting condition, not a correctness one.

The coordinator therefore acknowledges as soon as the delivery and its step are
committed. It never holds the request open for slow work.

**Cloudflare Workers Paid limits** (developers.cloudflare.com, read
2026-10-29):

| Limit | Value |
| --- | --- |
| CPU time per HTTP invocation | 30 s default (5 min max) |
| CPU time for a cron of under 1 hour | 30 s |
| Cron wall time | 15 min |
| `waitUntil` after the response | 30 s |
| Subrequests per invocation | 10,000 |
| Open outgoing connections per invocation | 6 |
| Memory per isolate | 128 MB |
| Durable Object request duration | no hard limit while the caller is connected |

Per-invocation work is sized well inside these. A delivery makes one DO
transaction, at most 2 RPC rounds of at most 2 calls, and one DO transaction per
round, all inside `waitUntil`. The claim lease is 25 s, below the 30 s
allowance. A tick makes at most 4 rounds. Each transaction applies at most 512
blocks. Work that does not fit is carried by later deliveries or ticks, from
durable state.

## Memory: bounded arenas

Every hand-written export of both Workers allocates in its own bounded scope
of [`slop-ninja-arena`](../crates/arena/README.md)
([design](../../docs/workers-arena-design.md)); the fan-out lane objects and
the constructor fallbacks spill or overdraw, counted. The build runs in
`Observe` mode (design migration step 1): nothing the pool can physically
serve is refused, and every would-be refusal and overdraft is counted, until
the JS reserve and the stack are measured on this build (step 0) and the
testnet load gate passes (step 9).

- **Linear memory ceiling.** `build.rs` links the module with a maximum of
  `floor((isolate_bytes - js_reserve_bytes) / 65,536)` pages of
  [`memory-model.json`](memory-model.json) (96 MiB). This `Observe` build
  links it growable: the pool commits pages with `memory.grow` as scopes are
  admitted, so an isolate holds the linear memory it uses (the heartbeat
  Worker a few MiB) and the JS heap beside it keeps its room until its
  high-water is measured. A build whose arena enforces (a measured model)
  links `initial = maximum`, a fixed memory. The model records the source of
  every input: the ceiling comes from the largest measured Rust peak (the
  package step at R_0 + 128 jobs, n = 1,000); the JS reserve is the
  remainder and, with the stack high-water, is not yet measured on this
  build (the build uses the arena's `unmeasured-model` feature, which the
  arena accepts only with `observe`, and `build.rs` refuses the test-only
  arena features). `cargo run --example arena_wasm_check -- <module.wasm>`
  checks a built module's memory limits, its hand-written exports and its
  static end (`__heap_base`) against the static size the route table's fit
  assumes.
- **One scope per invocation.** Every export is hand-written
  (`src/cf_arena.rs`): it takes a glue ticket, sweeps for abandoned scopes
  at that safe point (a poisoned pool is reported, not recycled: the recycle
  waits for its durable intent and stop rule), opens the invocation's scope
  with its route's declared
  footprint ([`src/arena_routes.rs`](src/arena_routes.rs)), converts the
  request inside it and closes it with the handler: the Worker `fetch`,
  `scheduled` and `queue` exports, the delivery's `waitUntil` continuation
  (a child scope admitted before it is spawned, holding copies of what it
  needs, and whose rounds start only while their worst case fits the 30 s
  window), and the ChainJournal and EvalCohort classes (`fetch`, `alarm`;
  each cohort step runs in a child scope of its own route: the seal and
  package steps admit their shape-derived footprints physically, refused
  with `Retry` in every mode when the pool cannot serve them even scattered,
  so a second large step of a co-resident object waits instead of exhausting
  linear memory). Every await has a timeout the platform enforces or one
  raced in JS, never a dropped `select` future: RPC calls and Durable Object
  calls carry `AbortSignal.timeout` (a journal call or kick on the delivery,
  cron and signing paths: `liveness::TIMEOUT_MS`), and R2 reads and writes,
  body reads and Workers AI runs are raced against their deadline in JS
  (`cf_io::settle`), so the promise Rust awaits always settles. A delivery
  continuation starts each journal call, kick, RPC round and signing step
  only while its allowance still fits the 30 s window, and the journal hands
  out a signing step only when its worst case fits the budget the caller
  sends (`signing::step_worst_ms`): a metadata fetch or a submission never
  fits a delivery and stays for the cron tick, so deliveries never take and
  abandon the signer claim.
- **Webhook ingress.** A `/hook` request with no signature header naming
  this receiver's subscription is refused in JS (401) before it takes a glue
  ticket or a scope. The Rust receiver then checks every header field
  (`webhook::precheck`: key, subscription, attempt and timestamp window)
  before any body byte is admitted, admits the body as its bytes arrive,
  stops reading a body still incomplete after the provider's 5 s
  acknowledgement window (`webhook::DELIVERY_ACK_MS`, 408) and
  authenticates the HMAC over the exact body.
- **Refusals.** A scope the pool cannot hold answers a JS-only 503 (an alarm
  rejects and is retried, a queue batch is retried, a cron tick resolves).
  A step or a step's object read that no wait can hold (`Capacity`), or an
  object above its kind's bound, faults its cohort durably
  (`arena_capacity`, `object_over_bound`): it pauses and is never skipped. A
  plan whose package (with its in-flight summary reads) the model cannot
  hold is refused at `/plan`. In `Observe` mode these admissions do not
  refuse; the would-be refusals are counted.
- **Bounded reads** (`src/cf_io.rs`): request bodies (the webhook, the
  operator routes, the internal Durable Object bodies, the lane routes),
  Durable Object replies, RPC replies and R2 objects are read through their
  stream's own reader (JS holds one chunk at a time), refused above their
  bound before they are read, and admitted as blocks only when bytes
  arrive: one block of the exact length when it is known (an unencoded
  body, an R2 object's size), else a buffer grown with the bytes received
  (doubling, never past the bound; a `gzip` or `br` reply keeps its encoded
  `Content-Length`). A Durable Object reply is bounded by the control
  footprint of the route that holds it. Each R2 object kind has its bound
  (`arena_routes::STEP_OBJECT_BYTES`, the job-summary and fingerprint
  bounds); a scope reading many objects re-uses one returned block run
  instead of holding one per object. An internal body that cannot be read
  answers its own status (503 for an arena refusal or a transport failure,
  which the caller retries), one that does not parse 400.
- **Home scopes.** What outlives an invocation lives in three bounded
  scopes opened on first use and kept for the isolate's life: the Durable
  Objects' own state, the parsed chain runtimes and the parsed cohort plans
  (a fixed entry count each; a cache miss re-reads storage, so it changes
  CPU only). They are marked permanent, so a refusal they cause is
  `Capacity`, never `Retry`. A home refused at its first use (a large step
  holding the pool) opens at a later use; refusals are counted
  (`home_open_failures`). The runtime cache evicts before it parses and
  parses uncached when the home lacks room for the metadata's measured
  parse need.
- **Status.** Every `/status` (Worker, ChainJournal `instance.arena`,
  EvalCohort `arena`) reports the arena counters: pool and glue use,
  overdrafts, refusals by kind, quarantine, abandonment, spilled glue, the
  stack high-water, per-route peaks (the cohort steps each on their own
  route: `cohort/draw`, `cohort/seal`, `cohort/load-seal`, `cohort/package`,
  `cohort/close`), the home scopes, the previous instance's postmortem if it
  trapped, and `fault`: `arena_abandoned_scopes` when a scope stopped being
  polled past its deadline (its pages, and a continuation's glue ticket,
  stay held until the instance is evicted) or `arena_poisoned` when pinned
  pages passed the quarantine budget, for an operator alert to key on.
- **Witnesses.** `cargo test --features arena-witness arena_witness --
  --test-threads=1` runs the heaviest native path of each light route under
  `Strict` in exactly its declared footprint (any allocation outside the
  scope, overdraft or leak fails), and measures the home entries. With the
  feature and no filter, the whole suite runs on the arena allocator.
- **Local wasm32 smoke run.** `node arena-smoke/run.mjs <miniflare
  dist/src/index.js> build <linear bytes> [requests]` loads worker-build's
  output into miniflare's local workerd (loopback only, in-memory Durable
  Objects and R2, test-only bindings, every subrequest answered 503) and
  checks the memory ceiling, the per-invocation reset over many requests,
  the ChainJournal and EvalCohort classes, the static size, the body
  bounds, the shed of unsigned deliveries, and a stalled delivery body
  cancelled at the acknowledgement window (408); then 40 signed deliveries (the public fixture receiver) against a
  mock JSON-RPC chain on a loopback HTTP server, reached through a small JS
  relay Worker so that the reply crosses workerd's own network fetch (with
  gzip, the coordinator sees the decoded body with the encoded
  `Content-Length`, as on Cloudflare), with plain and with gzip replies, and
  the cron export through miniflare's local trigger route. Subrequest bodies
  the coordinator does not read are cancelled (`cf_io::discard`): workerd
  keeps a cron, queue or `waitUntil` invocation open while one is unread.

## Signing (M3)

The coordinator signs and submits its own `commit_timelocked_mechanism_weights`
(commit-reveal v4) once per subnet epoch with the coordinator hotkey. The chain
reveals it itself one epoch later.

### Wasm path

Checked on 2026-10-30 against rtao `b5553a76` (then the pin) and rtao `main`
at `57191ba` (six commits later, none touching the core seam or Wasm support),
with `cargo check --target wasm32-unknown-unknown`. The pin moved to
`0c128160` (tag `snapshot-2026-11-03.2`, no `rtao-core` or `rtao-core-ext`
source change) and is now `fa83052d` (tag `snapshot-2026-11-05.2`), whose
`rtao-core` adds only the allocation-free `commit_reveal` module (D-110,
D-122), built for `wasm32-unknown-unknown` with default features off. The
coordinator's wasm32 Clippy and `worker-build` passed at each pin:

| rtao crate | wasm32 | Why |
| --- | --- | --- |
| `rtao-core`, default features off | builds | upstream `bittensor-core` without `host` is a declared wasm32 seam: codec, metadata runtime, signed extrinsics, sr25519 keys (pure-Rust `sp-core`), drand timelock (`tle`, `w3f-bls`) |
| `rtao-signer`, default features | builds | `KeypairSigner` is `rtao-core`'s `Keypair` |
| `rtao-client`, default features off | fails | tokio `net` (mio) |
| `rtao-tx` | fails | tokio `net`, reqwest/rustls |

So the coordinator depends on `rtao-core` alone and replaces only the client's
I/O and journaling. `src/chain.rs` uses the same functions rtao's client uses,
with the same parameters:

- calls are composed by name from live V15 metadata (`Runtime::compose_call`);
  rtao composes only from V15, and so does this;
- storage keys and values are resolved by pallet and item name;
- the signing payload is `call ++ extra ++ additional` over the runtime's own
  signed extensions. The spec and transaction versions in `additional` are the
  live ones (`state_getRuntimeVersion`), observed and compared nowhere;
- the mortal era is rtao's `{period, current}` anchored at the birth block,
  with `CheckMetadataHash` disabled, as rtao's keyfile signer has no metadata
  hash capability;
- the payload, the drand round (`reveal_round_at`, with rtao's three-block
  security offset) and the epoch prediction are rtao's `timelock_commit` and
  `epoch_schedule`; encryption is upstream `encrypt_and_compress`;
- the commit carries the stored `CommitRevealWeightsVersion` read at its
  preflight, whatever it is (D-086): the chain checks only that the tag
  equals the stored value, and runtime code alone decides which payload a
  reveal decodes. The payload is always rtao's native v4
  `WeightsTlockPayload` (`signing::NATIVE_CR_PAYLOAD_VERSION`, taken from
  `rtao_core::commit_reveal::NATIVE_PAYLOAD_VERSION`). A followed
  non-native tag is logged (`commit_reveal_tag_followed`) and shown per
  operation on `/status` (`commit_reveal_tag`). Nothing defers it: when our
  latest commitment with the tag that reached a verdict left the chain's
  queue with no reveal event (`reveal_dropped`), the log reports that epoch
  beside the followed tag (`...; epoch E (spec S) dropped unrevealed`,
  `signing::cr_tag_unrevealed`), and the next epoch commits again. A drop
  also covers weights refused when set (a deregistered UID, a raised
  `MinAllowedWeights`), which says nothing about the tag, and a commit that
  is not made could never reveal and clear it. One operation per epoch bounds
  the probing; a dropped commit costs that epoch's weights, what a skipped
  one costs. A queued commitment past its reveal epoch (a late drand pulse)
  is not reported. A stored value no `u16` tag can
  carry is refused at the preflight (`commit_reveal_version_unencodable`),
  which backs off and re-checks. 601 stores 4. This is rtao's own decision
  at `fa83052d` (D-122): `commit_reveal::select_tag` under its default
  `Unreviewed::Follow` follows the stored tag under any runtime code, and
  after a refuted reveal asks only for at most one commit per subnet epoch,
  the coordinator's cadence. The Worker reads no `:code` hash, which
  `select_tag` would need only to report a basis; calling it here would add
  a read and decide nothing, so the coordinator keeps its own rule and a
  test (`the_coordinator_tag_rule_is_rtaos_select_tag`) checks that the two
  agree for every code and reveal evidence. Native validators at this pin
  follow the stored tag the same way, so after a tag change both of our
  validators keep committing the same row.

The same `[patch]` set as `subnet/Cargo.toml` pins `bittensor-core`, `w3f-bls`
and `subtensor-macros`.

Byte-for-byte checks, natively:

- `chain::tests` compare the composed call and both signing payloads with the
  native validator's golden file, which rtao's own builders wrote for the same
  inputs; encrypt to a past quicknet round and decrypt with that round's
  published signature; and pin storage keys, defaults and epoch arithmetic.
- `parity/` (its own workspace, so the Wasm build never sees rtao's native
  runtime) feeds rtao's generated builders and typed values (`NetUid`, `MechId`,
  `AccountId`, `NetUidStorageIndex`) and the coordinator the same inputs:
  commit calls, eleven storage keys, signing payloads and signed extrinsics
  with one shared signature, the timelock payload, round and first reveal
  block, and ciphertexts that each side decrypts.

### Target and row

Each closed cohort's target is installed automatically, with the
authorization the coordinator signs for it
([Automatic target handoff](#automatic-target-handoff)); the operator can
also install a bundle (`POST /signing/target`), the same file the native
validator reads, through the same checks. The ChainJournal verifies it with the configured trust
roots, re-verifies it from storage on each use, and derives the row from the
coordinator's own registration snapshot read at the operation's confirmed
block. `target::derive_row` is the one dispatch point from the package's frozen
reward rule to its calculation. A `testnet-reward-v0` bundle (refused off
testnet by the protocol crate) derives a v1 row. A `reward-v1` bundle
(`slop_ninja.reward_v1_bundle/v1`: the launch burn row, or a cohort target with
its records and authorization) is verified by the protocol crate's
`target_bundle`, which recomputes the outcome and checks the closure and the
authorization; each epoch's operation is the `WeightOperationV2` issued only
under the authorization checked again at that epoch, and the row is derived
against a snapshot that also reads the finalized `SubnetOwnerHotkey`. A row
that must burn while the owner hotkey holds no UID is deferred to a later
epoch.

### One operation per epoch

The identity of the commit for subnet epoch `e` is
`slop_ninja:coordinator-commit:v1:` + SHA-256 of a domain, the protocol's
weight operation ID for (target, hotkey) and big-endian `e`. `commit_ops.epoch`
is unique, so no epoch holds two operations.

An operation is prepared when the RPC-confirmed watermark is at least
`SIGN_COMMIT_OFFSET` blocks into an epoch and more than the era plus an
eight-block guard before the next one, so any attempt lands in the epoch it was
encrypted for. With tempo 360 and the defaults, that is blocks 12 to about 287
of each epoch. A preflight with nothing to do backs off ten blocks.

### Steps

The heartbeat drives everything. After each new delivery is acknowledged, and
after each cron tick, the Worker runs one bounded signing round in `waitUntil`
(3 steps for a delivery, 8 for a tick). The ChainJournal plans one step at a
time under the durable `signer` claim (25 s lease, fencing generation),
records it as the pending step, and the Worker makes that step's one RPC call
and posts the outcome back. A wakeup that finds the claim held skips at once
(`sign_claim_held_skips`). Every step is charged to the shared RPC budget and
to signing's own share of it (`SIGN_RPC_CALLS_PER_MINUTE`, default the shared
budget minus four, so 8 of 12), which leaves the heartbeat's anchoring, audits
and fallback at least four calls a minute however busy signing is.

Steps are planned in this order: the head runtime when none is stored; the
live attempt, whose nonce decides everything else; new work (a preflight)
while active and no attempt is live; then diagnostics, the read-back and
reveal tracking of committed operations. A diagnostic that is failing or
backing off is skipped, so it never delays a commit.

| Step | RPC call | At |
| --- | --- | --- |
| Runtime | `state_getRuntimeVersion` | the confirmed block (head runtime); the parent of a block written by an unknown runtime; the preflight block before signing |
| Metadata | `state_call Metadata_metadata_at_version(15)`, once per runtime, stored in the DO | the block whose version was not stored |
| Preflight | one `state_queryStorageAt`: `LastRuntimeUpgrade`, the hotkey's `System.Account`, `Timestamp.Now`, `NetworksAdded`, `Uids`, `CommitRevealWeightsEnabled`, `CommitRevealWeightsVersion`, `RevealPeriodEpochs`, `WeightsVersionKey`, `WeightsSetRateLimit`, `LastUpdate`, the five schedule items, `SubnetworkN`, `MinAllowedWeights`, `MaxWeightsLimit` | the confirmed block |
| Registration | `Keys` and `BlockAtRegistration` for every UID, and the chain's own `TimelockedWeightCommits` queue for the epoch | the same block |
| Signing runtime | `state_getRuntimeVersion` (and its metadata once, if new) | the same block |
| Submit | `author_submitExtrinsic`, after the mark | |
| Nonce check, probes | `System.Account` | the confirmed block, then a binary search over retained blocks |
| Inclusion | `chain_getBlock` | the block where the nonce moved |
| Events | `System.Events` | that block |
| Read-back | `TimelockedWeightCommits` for the epoch and the next | the confirmed block |
| Reveal | the queue, `LastUpdate`, `Weights`, then `System.Events` and `Weights` at the reveal block | confirmed blocks after the predicted reveal block |

Every read is pinned to a block hash from the retained, RPC-confirmed chain,
and includes `LastRuntimeUpgrade`. A read is decoded with the runtime that
wrote its block, the one its own `LastRuntimeUpgrade` names. The version and
metadata APIs at block `b` run the code in `b`'s post-state, which executes
`b + 1`: at the block that enacts an upgrade they already report the new
runtime, while that block was executed, and its storage and events written,
by the old one, whose spec its `LastRuntimeUpgrade` still holds until the
next block's initialization. So the writer of `b` is learned at `b`'s parent,
whose reported spec must equal the one `b` names, never at `b` itself. Stored
runtimes are kept by spec (two parsed in memory), so a search across an
upgrade decodes each block with its own runtime without learning anything
twice. A planned key the writer derives differently is never read as absent:
the read is planned again with the writer's keys. Re-learning is capped at
four per read before it counts as a failure (below).

Keys are planned with the head runtime, the one reported at the newest block
the signer asked about. Signing reads the executing runtime fresh: after the
registration read, a runtime version at the preflight block (its post-state
code, which executes the attempt) chooses the metadata the call is composed
and signed with. No spec or transaction version gates anything. If live
metadata has no type-correct encoding of the call, the operation is deferred
alone and re-evaluated at its next preflight.

### Signing an attempt

The Registration outcome is processed in one DO transaction that:

1. refuses if this hotkey already has a commit in the chain's queue for the
   epoch (`commit_already_present`), so a lost or rebuilt journal cannot
   commit twice;
2. derives the row and checks it against `MinAllowedWeights`;
3. allocates the nonce: the hotkey's nonce at the confirmed block, which must be
   at least the journal's floor (otherwise `fault:nonce_regressed`);
4. reads `COORDINATOR_HOTKEY_SEED`, derives the sr25519 key and requires it to
   be the configured public hotkey (`fault:hotkey_mismatch`);
5. computes the drand round from that block's `Timestamp.Now` and schedule, so
   the round follows chain time rather than the Worker's clock; encrypts;
   composes the call; signs; verifies the signature; assembles the extrinsic;
6. journals the attempt (nonce, era birth and expiry, anchor hash, runtime
   versions, round, digests, the extrinsic hash and bytes) as `signed`.

Steps 1 to 3 run with the registration read; steps 4 to 6 run with the
signing-runtime step that follows it at the same block. Nothing is sent in
either transaction.

### Submission and reconciliation

The Worker sends an attempt only after a one-shot mark, `signed` → `sending`,
which the journal grants only to the current, unexpired claim holder whose
pending step is that submission. The column `sends` is constrained to 0 or 1.
Then:

| Reply | Attempt | Next |
| --- | --- | --- |
| extrinsic hash equal to the journal's | `submitted` | reconcile |
| JSON-RPC 1010, 1011, 1016 or -32602, unless it says outdated or stale | `rejected` | the operation returns to `preparing` (detail `pool_rejected`): its next preflight re-reads the chain state and re-learns the runtime, and one fresh attempt is signed with the same nonce (a runtime upgrade between signing and submission is the rejection this cures). A rejection of that attempt too, or the epoch passing first, fails the operation as `pool_rejected` |
| 1012, 1013, 1014, outdated, stale, any other error, 429, 5xx, timeout, transport failure, malformed reply | `uncertain` | reconcile |
| none (the invocation or the DO died after the mark) | `uncertain` when the next holder claims (`abandoned_after_mark`) | reconcile |

Reconciliation uses only RPC-confirmed state and never resends:

- **Nonce passed:** binary search over the retained finalized blocks between
  the anchoring block and the confirmed block finds the inclusion block. Its
  body must contain these exact bytes, otherwise `fault:foreign_nonce_use`.
  Its events give `ExtrinsicSuccess` or `ExtrinsicFailed` and
  `TimelockedWeightsCommitted`. The floor becomes nonce + 1. If the endpoint
  answers a probe there with a JSON-RPC error (it no longer serves that
  state, as a pruning node does after a long outage), the search goes on by
  block bodies, which need no state: each block after the last one known not
  to hold the bytes, up to the block that consumed the nonce or the era's
  expiry (`search_scan`, journal v11; logged `search_scan`). Each such error
  counts against the attempt (code `state_unavailable`, a failing subject in
  `/status`), and a later answer clears it, so a burst of endpoint errors
  (an overload or internal error rather than pruned state) recovers by
  itself. Only refusals through 60 confirmed blocks of backoff (seven
  failures: 1 + 2 + 4 + 8 + 16 + 32 blocks between them) settle it, and
  never pause the signer: an included attempt whose events cannot be read
  settles as `events_unverifiable`; an attempt whose bodies cannot be read
  either settles as `inclusion_unverifiable` (dispatch `unverifiable`, the
  nonce floor past its nonce, which the head showed consumed, and the
  operation `failed` with `inclusion: "unknown"` in its status, so a
  delivery audit reads that epoch as unknown rather than missing, and the
  publisher alerts it once, `inclusion_unknown`; an included commit is
  revealed by the chain on its own), and the next epoch commits with no
  operator clear. None is retried
  forever behind the signer-wide backoff.
- **Era expired:** at a confirmed block at or past the expiry with the nonce
  unconsumed, the attempt can never be included. Only then may the operation
  sign a new attempt, with the nonce read again, if the epoch's window is
  still open.

### Failures and backoff

A step can fail in two ways, and neither rolls the journal back into a retry
loop:

- **RPC errors** (transport, timeout, HTTP 429, any other status) end the
  round at once and back the whole signer off in time: 5 s doubling to 120 s,
  or 30 s doubling for 429. Any answered call resets it. Nothing settles on an
  RPC error.
- **Deterministic failures** (a value, event list or metadata that does not
  decode or parse, a block hash the journal does not retain, a parent whose
  runtime is not the one a block names, a re-learn cap) are recorded durably
  in `sign_failures` against the step's subject: the head runtime, the
  preflight, one attempt, one operation's read-back, or its reveal. The
  subject backs off by confirmed blocks (1, 2, 4 ... up to 60), and other
  subjects keep planning. After five failures (seven for
  `state_unavailable`, above) it settles:

| Subject | Settles as |
| --- | --- |
| reveal of an operation | diagnostic `reveal_unverifiable:CODE`; the operation stays `committed` and its reveal is no longer checked (the chain reveals on its own) |
| read-back | read-back `unverifiable`, diagnostic `readback_unverifiable:CODE`; the reveal is still tracked |
| an included attempt (its events do not decode) | dispatch `unverifiable`; the nonce floor still advances past its nonce; the operation is `committed` with diagnostic `events_unverifiable:CODE`, and the read-back then checks the commit in the chain's queue |
| an attempt whose consumed nonce's inclusion the endpoint keeps refusing (`state_unavailable`) | dispatch `unverifiable`; the nonce floor advances past its nonce; the operation is `failed` with diagnostic `inclusion_unverifiable:state_unavailable`; the signer stays active |
| any other attempt (its nonce cannot be read) | `fault:attempt_unverifiable`, since no later nonce can be allocated safely |
| head runtime, preflight | never: they keep the 60-block backoff, so unparsable metadata is fetched again about every 12 minutes, not every round |

`/status` reports `signing.health` (`ok`, `backing_off`, `failing`,
`diagnostic`, `paused` or `fault`), the last error, the RPC backoff, each
failing subject with its count, code, retry block and whether it settled, the
stored runtimes, and signing's calls this minute. Operation and attempt rows
carry their diagnostic.

### Read-back and reveal

After inclusion, `TimelockedWeightCommits(netuid, epoch)` at a confirmed block
must hold this hotkey, the inclusion block, the ciphertext digest and the
round (`readback = ok`).

The pallet takes an epoch's queue at the first block of its reveal epoch and
puts back only the commits whose drand pulse has not arrived yet, retrying
every block. So the entry disappears in exactly the block that revealed or
dropped it. After the predicted reveal block plus two, the journal reads the
queue at the confirmed block: if the entry is still there it checks again five
blocks later, and fails the operation as `reveal_missed` once the reveal epoch
has passed. If it is gone, a binary search over the queue at retained blocks,
from the last block known to hold it (the predicted block, or the last check
that found it), finds the block that removed it, and that block's events must
hold `TimelockedWeightsRevealed` by this hotkey (otherwise `reveal_dropped`).
Each check (`reveal_check`, with `queued, next N` or `gone, search LO..HI`)
and each probe (`reveal_probe`, at its block) is in the signing log. `Weights` there is compared with the row. With
commit-reveal enabled the reveal does not move `LastUpdate`, which stays at
the commit block.

### Pauses, faults and drills

`signer.state` starts `paused` (`operator_hold`). `POST /signing/control`
resumes or pauses it. While paused, live attempts keep reconciling, and so do
read-back and reveal checks, but no new operation is prepared. A disabled
commit/reveal flag pauses with `commit_reveal_disabled` and never falls back
to direct weights. A `fault:` reason (foreign nonce use, nonce regression, key
mismatch, an unverifiable signature, an attempt whose nonce cannot be read)
also stops reconciliation and needs `clear_fault` before `resume`; clearing it
also lets a settled attempt be retried. A chain-journal fault stops signing
too.

### Authorization without expiry

No authorization expires. An installed reward-v1 bundle yields one operation
per epoch for as long as its target is the held one: its renew-by hint is
shown in `/status` (`signing.authorization.renew_by_epoch`) and never
refuses or pauses anything, so a coordinator outage of any length resumes
committing the same target. It stops only when a newer bundle supersedes it
or a signed revocation withdraws it.

`POST /signing/revoke` (operator token), with a signed
`RewardIntentRevocation` as the body (the bytes `reward_v1_verify revoke`
writes), runs the journal action `revoke`, which `/signing/control` refuses.
It checks the signature under the trusted authorization key and this chain,
refuses an issue epoch past the finalized epoch index plus one
(`revocation_epoch_future`), because a re-authorization must be issued later
and cannot commit before its own issue. The finalized epoch index is the
later of the journal's last preflight schedule advanced to the finalized
cursor and the Worker's RPC reading of `SubnetEpochIndex` at the finalized
cursor block, taken just before the call; with neither (no preflight and the
reading failed, for example no cursor yet) the revocation is refused
`revocation_epoch_unknown`, and while the cursor lags the chain by more than
an epoch (catch-up, webhook silence) a revocation dated at the chain's
current epoch is refused `revocation_epoch_future` until it catches up. Both
journal nothing; retry them. Do not resume signing to obtain a preflight
while the target to withdraw is installed: that would commit it. It refuses
a revocation that would add a new target once the revocation set is full
(`revocation_set_full`: the set is one journal reply the revoke route reads
whole, so its bound is that route's control footprint,
`authority::REVOCATION_SET_BYTES`, divided by the revocation's fixed signed
length, about 4,000 targets (258-byte records); only targets whose latest revocation is signed
under the trusted key count). That is not a stop: a target that can no
longer be revoked is withdrawn by installing a successor bundle. It journals
the revocation in `sign_revocations` (schema v9, indexed by target in v10;
only ever added; a second
post answers `new: false`), and applies it in the same transaction:
when it withdraws the installed authorization (same target, authorization
issued at or before the revocation's epoch), every preparing operation of
that target, never signed, fails `authorization_revoked`, and from then on
the active target is refused with that code at every step, before any
preflight, registration read or signature. Committed operations keep their
read-back and reveal checks. It answers `{"revoked": {...,
"withdraws_installed": ..., "failed_ops": ...}}` or `{"refused": {"code":
"revocation_malformed" | "revocation_refused" | "revocation_epoch_future" |
"revocation_epoch_unknown" | "revocation_set_full", ...}}` with nothing
changed. A journaled revocation is written to the pointer at once (the
reply's `pointer` is `confirmed` or the pass's failure code; the cron tick
retries a failure). Each check reads only the installed target's journaled
revocation with the latest issue epoch, one entry of the target index (it
withdraws whatever an earlier one of that target does), and does not verify
its signature again, so neither the number of revocations nor a rotated
trust root changes its cost or outcome.
Installing a bundle whose authorization a journaled revocation withdraws is
refused (`authorization_revoked`); a bundle with an authorization of the same
target issued after the revocation re-authorizes it. Signing may be paused
or active.

The cron tick keeps the private authorization pointer
(`authorizations/current.json` in the receipts bucket) equal to the journal:
the installed bundle's authorization, its hint refresh if any, and the
revocation set (pointer schema v2: the latest journaled revocation of every
revoked target signed under the trusted authorization key, oldest first,
concatenated), and `row`, the coordinator's signed row, each stored first as
`authorizations/<sha256>.signed`. A validator that missed earlier
revocations journals them all from the set. The row is what every other
validator commits (the copy model, 2026-11-04; `signing::rows`): before
each pointer pass, in its own transaction (a failure there is recorded as
`row_record_store` and the pointer, with its revocation set, is planned
regardless), the journal takes the latest derived row of the installed
target and, when no journaled record covers its paid entries (UID, the
hotkey and registration block its snapshot read at that UID, canonical
weight, and the owner hotkey) under the installed target and authorization,
signs a `slop_ninja_protocol::CoordinatorRow` with the authorization key
(the `AUTHORIZATION_SIGNING_KEY` secret, never the hotkey) and journals it
(schema v13, `sign_row_record`). The row is derived by the signing
journal's own row snapshot step (`sign_row_snapshot`): a schedule read and
a registration read at one finalized block, once per commit epoch and when
a target is installed, in every signer state (paused, faulted, a preflight
deferred), so neither the coordinator's own weight delivery nor its commit
preflight holds another validator's row; its failures back off at the
capped rate and never settle (subject `row_snapshot`). The coordinator's
own commitment's row (`commit_ops.row_json`) is used instead when it is
later by (commit epoch, registration block). One record covers every later
commitment of the same entries; a change of entries under the same
authorization signs a new record later by (epoch, registration block),
which validators install by that order, and a record is never re-signed
back to an earlier derivation. The pointer names a row only beside the
authorization it was derived under, so after a new target or authorization
is installed it carries none until the first row derived under it. While no
record covers the installed authorization, `/status`
`signing.row_record.pending_authorization`, `pending_age_ms` and
`pending_code` say since when and on what it waits
(`row_waiting_derivation`, `row_waiting_issue`, the snapshot's own code such
as `row_deferred_owner_unregistered`, `row_hotkeys_missing` for a row an
older build journaled without the hotkeys, or a signing failure's code);
logged once per change (`row_record_waiting`); `snapshot_epoch`,
`snapshot_block`, `snapshot_derived_epoch` and `snapshot_code` show the
step. A signing failure (`authorization_key_missing`, ...) is retried at
the next tick and shown on `/status` (`signing.row_record.error`), logged
once per code (`row_record_failed`; `row_record_signed` on success), and
cleared once the journaled record covers the installed authorization and
the latest row again. The publisher alerts either after a tempo
(`row_record_stalled`).
The journal's plan names the objects (key, digest, length); the Worker
fetches an object's bytes from the journal only when the bucket lacks it.
Each pass checks every object the pointer names (a missing one, or one of
another length, is written again), reads the stored pointer back and
rewrites it whenever its bytes differ (a lost write, another writer). The
tick reserves the pass's worst case (three journal calls and one shared
bucket deadline) before its catch-up and signing rounds, so they never
starve it. Every pass reports its outcome to the journal (schema v10,
`sign_pointer_drive`): `/status` `signing.authorization` shows
`pointer_current` (the last pass confirmed the stored pointer equal to the
plan), `pointer_confirmed_age_ms` (refreshed by every healthy pass, so
within a cron period or two), `pointer_error` and `pointer_error_age_ms`
(the last pass's failure code, such as `plan_unreadable`, `object_head`,
`object_put`, `pointer_put` or `pointer_read_back`, and how long the
failures have lasted), beside `pointer_sha256` and `pointer_age_ms` of the
last changed write. The sign log records only transitions
(`authorization_pointer_written`, `_failed`, `_recovered`). The publisher
alerts `pointer_stale` when the last pass failed or the confirmation is
older than two cron periods for a whole tempo, and copies the pointer to
`v1/authorizations/` for validators. Neither the pointer nor the
authorization has a validity window.

`POST /signing/restart?from_epoch=F`, with the launch bundle as the request
body (operator token; signing paused only, refused `signing_active`
otherwise), is the coordinator's counterpart of the native validator's
`reset-target`, for a chain record restarted under the same trust roots (a
new main authority on the eval Worker, R_0 with no predecessor). It runs the
journal action `restart_target`, which `/signing/control` refuses (its body
holds only small JSON actions). Without `from_epoch` the route answers 400
`from_epoch`. In one transaction the action checks that `F` (the restart
record's first epoch) is after the epoch the held target activated
(`restart_before_held_record`; so a low or mistyped `F` cannot let the
earlier record back in), not before an earlier restart's and after every
epoch an installed reward-v1 bundle was issued (launch) or opened (cohort)
at (`restart_regressed`; the high-water mark is `sign_record_epoch`, schema
v8, reported as `record_epoch` in `/status`, so a second restart while the
launch burn row is held, with no activation to bound it, never reuses an
earlier restart's floor and readmits that record's bundles), and that the bundle is a launch bundle that verifies
against the launch burn row and whose authorization was issued at or after
`F`; then it moves the held record to `sign_held_target_resets` (journal
schema v8; never deleted) and installs the bundle as the only active target.
Any refusal changes nothing, so the action is never a probe of the deployed
version (that is `/status`'s journal schema version). After a restart, a
bundle installs only when it is the restarted record's
(`signing::restart_floor_ok`: a launch authorization issued, or a cohort
opened, at or after `F`), so the earlier record's bundles, signed by the same
keys, never install again, and no T12 bundle installs at all
(`v1_after_restart`: it carries no epoch to hold to the floor); `/status`
reports the floor (`restarted_from_epoch`). Each restart is logged
(`held_target_reset`, the record set aside and the floor) and counted
(`held_target_resets`).
Operations, attempts and reveals are left alone: a live attempt keeps
reconciling from finalized state and is never sent again, committed
operations keep their read-back and reveal checks, and an epoch already
holding an operation never takes a second one (`commit_ops.epoch` is unique),
so the restarted target's first commit takes a later epoch.

Testnet drills are armed per step (`arm_drill`, at most 3):

| Drill | Effect |
| --- | --- |
| `stall_before_mark` | the holder of a submission step has its lease cut to 2 s, sleeps 15 s, then tries to mark and to post an outcome; both must be refused |
| `never_send` | the submission is marked but never sent, and reported uncertain (a lost request); only the era's expiry with the nonce unconsumed allows a second attempt, which reuses the nonce |
| `evict_after_send` | after sending, the invocation stops and never reports. A redeploy restarts the DO, but on testnet a deploy took 1 to 5 minutes to reach the running instance, so arming `trap` while the attempt is `sending` restarts it at once (TESTNET.md; `state.abort()` wedged the instance and is not used) |
| `uncertain_after_send` | after sending, the Worker discards the reply and reports uncertainty |
| `ack_failure` | the next new delivery is committed but answered 503, so the provider retries the same delivery |
| `trap` | the next `/sign-step` request panics in the ChainJournal outside any transaction (the drill is consumed in its own committed transaction first); workers-rs marks the Wasm instance broken and rebuilds it, and the DO object, on the next request |

Submission drills are taken in the order `stall_before_mark`, `never_send`,
`evict_after_send`, `uncertain_after_send`, one per submission step, so one
epoch can run several in sequence.

### Automatic target handoff

The coordinator installs each closed cohort's target as its signed
calculation target and signs that cohort's reward-intent authorization
itself (`signing::auto`, journal schema v12), with no operator step. The
eval Worker (T13) writes each cohort's signed records to the receipts
bucket under `cohorts/<label>/`; every cron tick runs one bounded pass
(`/sign-auto`, its bucket operations sharing one journal call's allowance)
before the pointer pass, so an installed target reaches the private pointer
in the same tick. The pass is one function (`signing::auto::run_pass`)
over a small I/O trait, so the native tests run the Worker's own loop:

1. List the cohort labels (`cohorts/`, delimiter `/`, a page a step) once per
   finalized epoch (every pass while no signing preflight has given an
   epoch). A label found while its cohort is still open is found again at
   the next epoch at the latest; cohorts close tempos after they are sealed.
   A failed page does not hold the pass: the labels already known are still
   resolved and checked, and a failed continuation restarts the listing
   from its first page.
2. Resolve each new label from its signed manifest. The handoff is bound
   to the main chain record: the var `AUTO_TARGET_AUTHORITIES` (tracked in
   `wrangler.toml`, equal to the eval Worker's `EVAL_MAIN_AUTHORITY`, a test
   checks it) names the eval authorities whose labels (`testnet-<name>-`)
   are handed in; unset or malformed, the pass reads nothing and reports
   `record_authorities_unset` / `record_authorities_malformed`. A manifest
   signed by the trusted package key, naming this chain and its own label,
   under a main-record prefix gives the cohort number and opening epoch
   (`in_record`). One not signed by that key, or naming another chain or
   label (load cohorts), is `foreign`; one under another prefix (a scratch
   authority's drill in the same bucket) is `outside`. Both are classified
   under a fingerprint of the package key, chain and prefixes and are read
   again whenever those change, so a deploy that briefly carried a wrong
   key or list loses no cohort. A manifest whose signature is valid but
   whose record this build cannot decode (a newer schema) is `unreadable`:
   a failure (`manifest_undecodable`), read again at every listing, never
   `foreign`. A label with no manifest yet (a probe build never has one)
   is `unsealed` until the next listing. Labels opened before the restart
   floor are left out. `/status` counts labels per state and the `foreign`
   and `outside` ones per reason (`unclassified`).
3. Check the in-record cohorts after the last one handled or pending, in
   cohort order, and hand in each closed one (`closure.signed`) with its
   records (manifest, package, closure, target when present, the published
   index at its seal) and, for a target cohort, its receipts. The first
   cohort not closed ends the step: a later cohort is never handed in past
   an earlier sealed one. While a later cohort has closed behind an
   unclosed one the stall is on `/status` (`earlier_cohort_unclosed`).
   Numbers continue without gaps: cohort N is handed in only after N - 1
   was handled, is pending, or was passed by the operator; the first cohort
   of a chain record is the lowest in-record one. A gap stalls
   (`cohort_gap`). The operator passes an abandoned in-record cohort with
   `{"action": "auto_abandoned", "label": ...}` (refused while its closure
   is in the bucket, `cohort_closed`; `auto_unabandoned` takes it back while
   nothing after it was handled) and a number with no in-record label with
   `{"action": "auto_gap", "cohort": n}` (a label later resolving to it is
   reported, `cohort_marked_absent`, as is an in-record cohort that resolves
   only after later ones were handled, `cohort_behind_record`: neither is
   skipped silently). The eval Worker writes no abandonment
   record the coordinator could read instead, so an abandoned cohort that
   later cohorts follow still needs that operator step.

The journal then verifies the records under the trust roots (the
manifest, the package bound to it, the published index bound by the
manifest, the outcome recomputed, the closure checked against them) and, in
one transaction:

- A cohort without a target signs nothing: the held target and its
  authorization stay (no authorization expires), so the pointer keeps naming
  the target's own cohort, whose receipts the eval Worker retains as the
  newest target. Carried authorizations naming the later no-target cohort
  are not signed: that cohort's receipts go after N+2, and holding them is
  the user's open ruling.
- A cohort with a target gets a `RewardIntentAuthorization` for its own
  manifest, package and target, issued at its closing epoch (when the target
  activates), with the manifest's renew-by distance as the non-binding hint
  and the installed authorization's checkpoint generation (an installed
  bundle that does not read refuses the handoff,
  `installed_generation_unreadable`), signed with the Worker secret
  `AUTHORIZATION_SIGNING_KEY`. That key must derive the trusted
  `AUTHORIZATION_PUBLIC_KEY` and differ from the hotkey's seed
  (`authorization_key_mismatch`, `authorization_key_is_hotkey`); the package
  and receipt keys stay in the eval Worker. It never leaves the journal
  except as the signature. The coordinator's own bundle form (records only)
  is installed through the operator path's checks (`install_target_v2_from`:
  the held target or its successor, the restart floor, the held record moved
  forward). A target already held is recorded `held`, one superseded by a
  later held target `superseded`.

The target changes only to a cohort whose detailed receipts are retained:
its signed receipt index authenticates under the receipt key to the receipt
root and count its package binds (streamed through a 64 KiB buffer, so an
index of any length is checked in a fixed footprint;
`receipt_index_unverified`, `receipt_index_mismatch`), its chunk map is
present and there is no `receipts-retired.json`. A target cohort whose
receipts are already retired (possible only after a coordinator outage
longer than two cohorts) is never installed or authorized on its own: it is
checked from the held target and journaled as pending (`sign_auto_pending`:
label, number, outcome, the held target it was checked from and the one it
leaves), which does not count as handled. Cohorts after it that need no
install are journaled behind it. Each pass resumes after the last pending
cohort, so a chain longer than one pass's deadline completes over as many
passes as it needs and nothing is verified twice. The pending cohorts are
recorded `intermediate` (or `no_target`, `held`, `superseded`) only in the
transaction that installs the next closed target cohort with retained
receipts, whose predecessor the last of them leaves held (the held record
moves straight to that one, as a validator's walk through intermediate
targets does); that transaction first checks that the held target is still
the one the first pending cohort was checked from (`held_target_changed`
drops them otherwise, and they are checked again), and every move of the
held record (an operator bundle, a chain-record restart) drops them too.
Until that cohort closes they wait (`receipts_retired`) and the held target
keeps committing; a refusal between the two installs nothing, so neither
signing nor the pointer pass ever sees an active target whose receipts are
gone. A target cohort with neither its receipt index and chunk map nor the
retirement marker waits the same way but is reported `receipts_missing`: a
retention or write-order defect, not N+2 retention. A pass whose deadline
passes before it has recorded or journaled anything reports `pass_budget`
on the cohort it was on, so a cohort that cannot be checked within one
deadline is never silent.

Each cohort is handled once (`sign_auto_cohorts`, by label): a duplicate or
reordered wakeup answers the recorded outcome and signs nothing, and a
later cohort handed in before an earlier closed one is refused
(`predecessor_not_held`) without being recorded. Ed25519 signatures are
deterministic, so a transaction cut short (an eviction mid-sign) signs the
same bytes when it runs again. A cohort that cannot be handled yet (records
missing, a refusal, the key absent) is retried at every tick, the cohorts
after it wait, and its failure is on `/status` (`signing.auto_target.error`,
`error_label`, `error_age_ms`, logged once per distinct failure as
`auto_target_failed`); the publisher alerts `auto_target_stalled` after a
tempo. A failure clears itself when the step it came from next succeeds: a
listing failure at the next listing page, an unreadable object at its
label's next read, a cohort's refusal when it is handled or passed. Every
pass also checks that `AUTHORIZATION_SIGNING_KEY` is present and derives the
trusted root, without signing and without reading the hotkey's secret (it
is compared with the authorization key only when an authorization is
signed) (`key_ok`, `key_error`; logged on change), so a missing or wrong
secret is reported before the first target cohort needs it, without holding
cohorts that need no signature. Nothing waits on another
validator, and paused signing does not hold it.

Revocation stays a signed manual operator action (`POST /signing/revoke`).
The handoff never re-authorizes: a cohort's authorization is signed once, at
its closing epoch, so a revocation dated at or after it withdraws it for
good. A revocation journaled before the handoff runs does not stop the
handoff at that cohort: the target is installed as revoked (its checked
closure activated it; `revoked`, `target_installed_revoked`), so nothing
commits for it, exactly as when the revocation comes after the install, and
the next cohort's target installs as its successor in either order. A
re-authorization is an operator bundle. `POST /signing/control` with
`{"action": "auto_pause"}` holds the handoff (the held target keeps
committing) and `{"action": "auto_resume"}` continues it from the same
cohort.

`/status` `signing.auto_target` shows the hold, the listing, label counts
per state and the unclassified ones per reason, the pending cohorts (count
and first), the recorded gaps, the next cohort waiting for its closure, the
last cohort handled (number, label, outcome, target, authorization digest,
age), the last failure and the key check.

Memory: every later signing step (`journal/fetch`,
`arena_routes::JOURNAL_CONTROL_BYTES`) parses and verifies the stored bundle
again, so no path installs a bundle larger than those reads fit
(`arena_routes::SIGNING_BUNDLE_BYTES`: half the journal footprint, measured
natively, `arena_witness_measure_the_signing_reads_of_a_stored_bundle`, and
witnessed at the bound,
`arena_witness_the_signing_reads_fit_the_journal_footprint_at_the_bundle_bound`).
The operator path refuses a larger bundle
(`bundle_exceeds_signing_footprint`), and the handoff's source bound is
derived from it (`signing::auto::SOURCE_BYTES`): a cohort whose records
exceed it is refused (`cohort_too_large`) before anything is signed and
waits, visibly. The pass runs in its own arena route (`journal/sign-auto`),
whose footprint is derived from that source bound
(`arena_routes::JOURNAL_AUTO_BYTES`) and witnessed natively at it
(`arena_witness_the_automatic_handoff_fits_at_its_source_bound`); `/status`
reports its deployed peak. Storing the verified target at install, so
signing steps stop re-parsing the bundle and the bound can grow to one
stored value, is not done.

### ChainJournal liveness

The heartbeat, the watchdog and signing all go through the one ChainJournal
instance, so a wedged instance stalls all three and cannot report it. The cron
Worker therefore bounds its `/tick` call to the journal at 10 s, times it, and
keeps the outcome outside the Durable Object, as one small R2 object
(`ops/chain-journal-liveness.json` in the evaluation bucket): the check time,
latency, HTTP status, consecutive failures, first failure and last answer.
`/status` bounds its own journal calls at 10 s and reports that record as
`chain_journal_liveness` with a verdict: `ok`, `slow` (an answer over 3 s),
`unresponsive` (the last tick failed or timed out), `stale` (no tick for 3
minutes: the cron itself is not running) or `unknown`. The evaluation Worker
(`EVAL_ROLE = "eval"`) binds the same bucket and probes its own ChainJournal,
so it keeps its own record at `ops/eval/chain-journal-liveness.json` and never
reads or writes the coordinator's; one shared record would let either
Worker's answered tick reset the other's failures and hide a stopped cron.
The counter
`chain_journal_starts` counts every construction of the instance (a deploy,
an eviction or the Wasm rebuild after a trap). TESTNET.md has the runbook.

The crate denies `unwrap`, `expect`, `panic!` and unchecked indexing outside
tests (`clippy::unwrap_used`, `expect_used`, `panic`, `indexing_slicing`); the
one deliberate panic is the testnet trap drill, and the one `expect` is HMAC's
constructor, which accepts any key length.

## Durable Objects and storage

**ChainJournal** holds one chain's observations. Its instance name is
`chain-journal/testnet/<genesis>`. Migrations run once when an instance starts,
inside one transaction. The ChainJournal refuses a schema from another class
and a newer stored schema, and a failed upgrade rolls back.

- **Schema v1:** deliveries, reconciliation tasks, the cursor, the notified and
  RPC heads, and counters.
- **Schema v2:** migrates v1 in place. It adds the delivered block to
  `deliveries`, drops `reconcile_tasks`, and adds `block_events`,
  `finalized_blocks`, `faults`, `watchdog`, `mode_transitions`, `claims`,
  `rpc_budget` and `rpc_audits`.
- **Schema v3:** migrates v2 in place. It adds `rpc_confirmed` and seeds it at
  the retained anchor, the only RPC evidence v2 kept by height. The next
  passing head audit raises it.

- **Schema v4:** adds the signing journal (below): `signer`, `sign_targets`,
  `runtime_cache`, `chain_schedule`, `commit_ops`, `sign_attempts`, `sign_log`
  and `sign_drills`. Signing starts paused (`operator_hold`). On testnet the
  live v3 journal migrated in place on 2026-10-30.
- **Schema v5:** failure accounting. `signer` gains the block its head
  runtime was reported at (seeded from the stored metadata's block), the RPC
  backoff, the last error, a re-learn counter and signing's RPC sub-budget
  window; `sign_failures` holds per-subject failures; `commit_ops` gains
  `diagnostic`. The live v4 journal migrated in place on 2026-10-30.

**EvalCohort** is the cohort authority at `eval-cohort/testnet/<genesis>/<netuid>`
(see [Hidden evaluation](#hidden-evaluation-m4)), and the separate load-rehearsal
instance at the same name plus `/load`. A named main authority
(`EVAL_MAIN_AUTHORITY`, the chain-record restart) is the same name plus
`/authority/<name>`. Each object records the instance and the authority its
first request named and refuses any other (`instance_mismatch`,
`authority_mismatch`); a read-only request (the audit route, a successor's
supersede call) to an object that records nothing answers 404 and writes
nothing. Schema v12 adds `authority_succession`, the successor a superseded
main authority recorded. Schema v2 migrated the v1 stub in place:
it dropped `cohort_state` and added `eval_obs`, `cohorts`, `phase_log`,
`pool_used`, `pool_groups_used`, `docs`, `gen_usage`, `jobs`, `chunks`, `preds`,
`receipts`, `receipt_objects` and `eval_targets`. Schema v3 added the
active-tempo clock, abandonment and per-chunk attempt bookkeeping. Schema v4
(fan-out) adds `lanes`, `lane_events` and `batch_shards`, chunk holders, claim
generations, reconciliation and crash counts, generation fences, backoff,
throttling and budget reservations, job-scoped faults, summaries and bundles,
and the load instance's hold flag; it rebuilds `jobs` to allow `faulted`.
Without `EVAL_ROLE = "eval"` it is never kicked and stays empty.

The lane objects (eval deployment only): **AScorer** is a scoring lane at
`a-scorer/testnet/{main,load}/lane-<k>` that owns one `a-scorer-testnet`
Container instance; **EvalLane** runs generation, summary and bundle lanes at
`eval-lane/testnet/{main,load}/<kind>-<k>` (a named main authority's lanes at
`.../main/authority/<name>/...`); **GenLimiter** holds the shared
Workers AI token buckets in memory; **SpendLedger** holds the monthly spend
ledger (SQLite). Lanes keep only their assignment `(seq, gen)`, metering and
container bookkeeping; no text.

## Hidden evaluation (M4)

Testnet only, with `EVAL_ROLE = "eval"`. A cohort is a frozen operator plan
(`eval::plan`): n cases per model, the human_only/mixed/model_only mix, K
tempos with admission and transition minimums, the pool, the generators and
their budget, the roster (R_k, optional R_{k-1}, entrants), the burn identity
and an optional per-kind shortfall tolerance (`max_kind_shortfall_permille`,
at most 50; absent means 0). Plans `slop_ninja_eval_cohort_plan_v1` (stored
cohorts only) and `_v2` use `testnet-reward-v0` with an allow-list and the
40/40/20 mix; the deployed main instance no longer queues new v2 plans
(`Ceilings::accept_reward_v0` is off; the engine's plumbing tests still drive
them). Plan `_v3` and the probe plan `slop_ninja_eval_probe_plan_v1` run
`reward-v1` (see **Reward-v1 cohorts** below); their mix
is a parameter (launch 20/55/25).

**Heartbeat and steps.** Every webhook delivery kicks both cohort instances
(`main` and `load`) with the RPC-confirmed watermark; every watchdog tick also
reads `SubtensorModule.SubnetEpochIndex` and `LastEpochBlock` for the netuid at
the confirmed block hash (one `state_queryStorageAt` call, charged to the shared
RPC budget). The kick records both monotonically, advances boundary phases and,
when work is runnable, arms the cohort object's alarm. The alarm runs the root
steps (draw, seal, package, ledger settlement), each a claim, its I/O and a
commit, and then one **sweep** of the lane table; it re-arms about every 10 s
while a heartbeat arrived in the last 10 minutes, so coordinator silence pauses
progress and the next heartbeat resumes the same cohort. Nothing is skipped
because time passed.

**Fan-out lanes.** Every bulk step runs as a fan-out job of lanes: challenge
generation, paired scoring, receipt bundling and per-job score summaries. A
lane is a Durable Object whose alarm runs the lane loop, so each lane has its
own invocation limits (6 connections, 10,000 subrequests, its own CPU budget,
a 15-minute alarm wall). The cohort object stays the only authority:

- **Planning.** At generation start (after the draw) the cohort plans
  G = min(plan cap, ceil(pending / items per lane)) generation lanes and the
  scoring lanes; at the end of scoring, one summary and one bundle lane per
  job. A scoring job is one scored artifact (R_k, R_{k-1}, each entrant) and
  gets W = max(1, min(clamp(ceil(n / lane_docs), 1, C), floor(cap / J))) lanes,
  C its chunk count and cap the smallest of the plan's lane cap, the
  deployment's `MAX_SCORE_LANES` (equal to the Container `max_instances`) and
  the lanes the scoring budget affords. Lane i of a job prefers chunks
  [floor(i C / W), floor((i + 1) C / W)) and steals any unclaimed chunk of its
  job. When J exceeds the cap, every job gets one lane and live lanes are held
  at the cap.
- **Dispatch.** The sweep gives each due lane `gen + 1` in a transaction and
  only then sends its kick `{v, instance, seq, kind, lane, gen, name}` (ids
  only) through the `eval-lanes-queue-testnet` Queue, 100 per
  `sendBatch`, or directly (at most 6 at a time) if `sendBatch` throws. The
  queue consumer calls the lane's `/kick`; a lane keeps the lexicographically
  larger `(seq, gen)`, so duplicates and stale kicks are no-ops and lane names
  can be reused across cohorts. A lane is due when it is pending within the
  live cap, kicked but unseen for 30 s, silent past its staleness bound
  (3 leases) with work left, paused while heartbeats are fresh again, or done
  while its job has claimable work and no live lane. Lanes report at alarm
  entry and at least every 20 s of a cold path or a wait, so a slow start is
  never mistaken for a lost kick, and a lane superseded by its own newer kick
  exits without stopping its container (the newer alarm reuses it). An
  isolation run records each document's outcome as it resolves, which also
  refreshes the lane; a scoring lane's staleness bound is three times the
  larger of the lease and the plan's single-document limit. Authoritative
  plans need a lease of at least 30 s.
- **Request bounds.** A chunk request is bounded by its claim's lease less
  15 s, and at least 5 s, so a lane's commit is never later than its own
  claim. A request that outlives the bound is a timeout: its container is
  destroyed and the chunk is re-run in isolation. A single-document request in
  isolation is bounded by the plan's declared `doc_timeout_ms` (a resource
  limit, independent of the lease).
- **Lane limits are plan parameters** (`fanout.score`, required in new plans):
  `step_budget_ms` (a lane starts no step, whether a chunk, a restart or a
  single-document request, that could end past it, and the budget plus one
  lease fits the platform's 15-minute alarm wall), `doc_timeout_ms`,
  `artifact_max_mib` and `load_ms_per_mib` (the artifact stream and the model
  load each get `artifact_max_mib x load_ms_per_mib`), and
  `artifact_failures_before_model_error`. Plans stored before 2026-10-31 read
  with the values earlier versions compiled in. A lane gives up starting after
  `failures_before_pause` consecutive infrastructure start failures.
- **Claims and fences.** A lane claims through the existing lease tables and
  commits through the cohort, one transaction each. A commit lands only from
  the holder of the chunk's latest claim; an identical repeated commit answers
  `already`. A claim with no fresh heartbeat (or, on the load instance, under
  the operator hold) answers `Paused`; lanes then stop their containers and do
  not re-arm.
- **Spend.** Lanes report cumulative metering (container provisioned time and
  CPU, lane Durable Object time) with every report and commit. Each sweep
  checks measured spend and the remaining work's upper bound against the
  plan's `scoring_budget_microusd`; passing it raises the cohort's budget
  alert (`/eval/status` cohort `budget_alert`, the publisher's
  `scorer_budget_fault`) and pauses nothing: spend is tracked, never capped.
  The generation budget is the same alert: an item past it is still claimed.
  No lane stops, and the monthly `SpendLedger` never refuses or pauses
  either. (A store that paused as `scoring_budget` or `generation_budget`
  under the earlier guard still resumes with a clear.) A lane silent past its staleness bound is
  charged a bound for its unreported metering (from its last report through
  the staleness bound plus its container's inactivity grace); that bound is
  reported and settled, but never part of the guard, because a lane that
  stops reporting through an outage is time passing, not spend.
- **Status.** `/eval/status` reports, per cohort, lane counts by kind and
  status, kicks and re-kicks, starts and start failures, lost and reconciled
  attempts, fenced commits, hand-back and attribution events (in all and per
  job), each job's isolated crashes and artifact failures, cold-path,
  cold-path CPU and chunk-time percentiles, host CPU models, generation call
  times and throttles, the timeline, and spend against the budget and the
  ledger.

**Active tempos.** The phase minimums count active tempos, not raw epoch-index
differences (`live_epoch_single_increment_v3`), by chain epochs observed and
never by wall-clock time. Each tick reads the epoch index at the RPC-confirmed
watermark and, when the watermark trails the RPC finalized head, at the head
too (one more call); an observation is live when both name the same epoch,
and an epoch observed live once stays live. An increase of the index by
exactly one credits one active tempo when the earlier epoch was observed
live and the new observation is live. Every other increase credits none: a
jump of several, and every boundary a lagging watermark replays while it
catches up after an outage (those tempos passed while nobody observed them).
A tick whose head reading fails or is refused by the RPC budget proves no
lag: it records the watermark but not a new epoch index, and the next tick
with a head reading decides that increment. The watermark's epoch as read
is reported beside the stored one (`/eval/status` `watermark_epoch`); while
head readings keep failing it runs ahead, and two epochs ahead (a whole
tempo of watermark progress) the publisher alerts `epoch_deferred`.
Uncredited tempos are counted on `/eval/status` and never consume a minimum,
so after an outage and its catch-up the same cohort resumes and advances at
later boundaries, its admission and transition windows never compressed into
the replay.

**Boundary journal (store v23).** Each new epoch the engine stores is
journaled once, at its first observation (`boundary_log`: the boundary's
first block, the observation time, whether it was live, whether it credited
a tempo, the credited count after it, and the watermark block the tick read
it at); a later tick in the same epoch changes nothing. An observation is
the boundary's observed time only when that watermark is within one
fallback sweep of the boundary (`TICK_SWEEP_SECS`, the eval Worker's
one-minute cron, checked against `wrangler.toml` by a test); a first tick
later in the tempo, after silence, is dated back by its block lag and
reported as derived. The v23 migration seeds the journal from the store's own
boundary-step records (openings, admission window closes, the end of
admission without coordinator admission, closes, and the stored epoch's
block); work commits, notes and the openings of load cohorts and probe
builds never seed it. `/eval/status` gives each cohort `opened_ms`,
`cutoff_block` and `boundaries[]`: every epoch its dates use (opening,
window close, publication, close, each phase-log epoch and the boundary
after it, and the stored epoch while it runs), resolved by
`eval::boundaries::resolve` to an observed or derived block and time
(linear between journal rows, or from one side at the nominal block time,
or by epoch across a tempo change), or `null` with `at_reason`
(`before_tracking` or `not_derivable`). The public dashboard dates every
stage by these points; no work commit's own time reaches them. Rows no
cohort record names are pruned once per stored epoch, keeping the nearest
anchors of every referenced epoch, so the journal is bounded by the records
the store keeps and a closed cohort's derived dates never move.
Store v23 is a rollback floor like v21: an earlier build refuses the newer
store, so roll back with a forward revert that keeps `SCHEMA_V23`.

**Phases.**

| Phase | Moves on |
| --- | --- |
| `queued` | the first epoch-index increment after queueing (and after the previous cohort closed or was abandoned): opens, roster frozen, `opened_block` = that epoch's block |
| `admission` | active tempos >= opened + admission tempos |
| `generating` | the last generation item resolves and the batch meets the plan's kind counts within the tolerance |
| `sealing` | the batch, its index and the signed manifest are stored |
| `scoring` | every job's every task resolved |
| `packaging` | receipt index, package, target and components are stored |
| `transition` | active tempos >= opened + K and >= packaged + transition tempos: the target activates |
| `closed` | private texts are deleted from Durable Object storage |
| `abandoned` | (operator, from a faulted work phase) no target; private texts deleted; later cohorts may open |

A fault pauses the cohort with its state intact. Two kinds:

- **Resumable** (outages, platform refusals, unresolved infrastructure and
  failed root steps): `generation_unavailable` (an item's consecutive
  Workers AI outages reached the plan's
  `fanout.generation.failures_before_pause`: required in a newly queued
  plan; a cohort the calendar opens from a template without it gets 10
  written into its plan, and only plans stored before it was required fall
  back to 10), `scorer_unavailable` (the plan's `fanout.score.failures_before_pause`
  consecutive scorer failures that never reached the model),
  `arena_capacity`, `arena_defect` and `object_over_bound` (a platform bound
  a corrected deployment lifts), `roster_freeze_refused` (its code in the
  phase log; the clear runs the freeze again on the same roster),
  `unresolved_infrastructure` (below), and `step_refused` (below).
  `POST /eval/clear {"label"}` resumes the same cohort with fresh outage
  allowances; no attempt, answer or receipt is reset. The list is
  `/eval/status` `resumable_faults`. (`scoring_budget` and
  `generation_budget` are listed for stores that paused under the earlier
  budget guard; the guards now only alert.)
- **Automatic resume.** `generation_unavailable` and `scorer_unavailable` in
  a work phase can clear by themselves: while the cohort is paused, a step
  (one probe at a time, at most once a minute after the previous probe
  ended, only while heartbeats are fresh) runs one bounded probe, a
  generation on a fixed synthetic prompt by a paused item's
  generator, or the control document on a freshly started instance of the
  artifact whose slots failed (the reference, or the previous reference)
  under the cohort's label, spec and limits, on one lane object per instance
  and authority (`.../fault-probe`). The first probe that succeeds resumes
  the same cohort (`fault_auto_cleared:` in the phase log); one that fails
  only keeps it paused, with no count or time limit. A generator the
  provider stopped serving counts as served (the work rotates the item's
  generator). The fault records its cause (`fault_cause` on
  `/eval/status`): a probe clears `scorer_unavailable` only when every
  counted failure was a scorer slot that did not start, load or take the
  request (`slot`, `slot:previous_reference`). Bucket failures (`store`), a
  refused commit (`commit`), an unclassified failure, a definitive refusal
  of a reference (`definitive:<role>`, or `definitive:probe` when a probe
  met one) wait for an operator clear, paused and visible
  (`operator_clear_needed` on `/eval/status`, which the publisher alerts
  on), so a fault the probe cannot see never loops. A cohort that pauses
  again with no chunk committed or generation answered since its last
  automatic clear (a platform only partly back) is still probed and
  cleared, never held: each such clear doubles the wait before the next
  probe (`probes.no_progress_clears`, `probes.interval_ms`; at most six
  doublings, 64 minutes, under one default tempo), and progress resets it,
  so the same cohort resumes by itself once the platform is fully back. A
  probe's start, the fault it is for and its deadline (its run's
  allowances: two loads, the inactivity bound and the control limit, or a
  generation's call timeout, plus one Durable Object call) are recorded
  before its call with a bound on its cost (those allowances at full rates;
  a generation's prompt bytes and completion allowance), charged if the
  step is cut off before its outcome. The call is aborted at that deadline,
  the step's lease is raised to cover it so no kick schedules an
  overlapping step, and no second probe starts before it; an outcome
  applies only to the fault the probe was started for, so a probe still
  running when an operator clears a fault never clears or holds a later
  pause. Each probe's time and spend are recorded (`probes`) and charged
  with the cohort. A probe runs only on fixed control text and touches no
  job, and it writes no R2 receipt bound into the signed score package:
  this is an open exception to the rule that every inference attempt has
  one (as for the operator's `/eval/probe` and `/eval/scorer-probe` and the
  admission runs), recorded for a decision. A probe runs after the
  bookkeeping of finished cohorts (purge of private predictions, settles,
  receipt deletion, the abandonment record), and a bookkeeping step that
  fails waits out a retry time (`bookkeeping_retry` on `/eval/status`), so
  neither holds the other. Admission's outage faults
  (`signer_unavailable`, `upload_path_unavailable`,
  `upload_front_unavailable`, `commitment_state_unavailable`, a verdict's `scorer_unavailable`) record
  their subject (the mint or canary, the commitment resolution, the verdict) and clear once no subject meets the pause
  condition any more: re-read after that subject succeeds and on every
  admission step, so a subject that leaves without succeeding (a refused
  session) clears it too, while another subject's success never does (a
  session or resolution or verdict still waiting, or one at the plan's
  bound, keeps it). No terminal fault is ever cleared this way.
  `/eval/status` `auto_cleared_faults` lists both sets.
- **Unresolved infrastructure.** Every attempt that leaves a document
  unresolved is infrastructure-caused (an expired lease with nothing stored,
  a crash, a failed request, or a reference's unattributed model error; an
  entrant's model failure resolves its document as a `model_error`), so a
  chunk that used its `max_attempts` with documents
  unresolved gets another round of `max_attempts` by itself, logged in the
  phase log (`infrastructure attempts extended to N`) and counted in the
  cohort's `attempt_extensions` and `extension_receipts`. The bounds are
  structural: attempt numbers stay within 255, and the plan's receipt
  reservation (every scored document at `max_attempts`) plus each
  extension's worst case (its unresolved documents at every added attempt)
  stays within one signed receipt index. Only when no further round fits
  does that job fault alone and the cohort pause at the end of scoring
  (`unresolved_infrastructure`, the job in `receipt_bound_reached_jobs`); a
  clear returns the job to scoring, whose next claim extends again under the
  deployed build's bound or faults again.
  The sealed manifest's `max_attempts` is then the allowance per round; the
  signed records do not name extensions (the receipt index lists every
  attempt's receipt, so a verifier sees all of them).
- **Terminal under the frozen plan:**
  `draw_shortfall` and `batch_shortfall` (the pool or generation cannot meet
  the plan's kind counts within its tolerance), and `reference_failure` (a
  reference answered invalidly: an answer that does not parse or holds no
  valid prediction). A reference never has an unanswered document attributed
  to it: for a scorer error or a crash reproduced on a proven instance the
  receipt records the model error the scorer gave, marked unattributed, and
  the document stays unresolved and runs again (above). A reference that
  errs on the same document every time reaches the receipt bound: the job
  is listed in `receipt_bound_reached_jobs` and the phase log says so, a
  clear runs it into the same bound unless the deployed bound grew, and
  abandonment is the way out. A clear of a terminal fault would fault again
  at once, so it is refused with `abandon_required`. `POST /eval/abandon
  {"label", "reason"}` ends the cohort without a target. The reason is a
  public-safe code: one of `terminal_fault`, `plan_defect`,
  `reference_unusable`, `pool_shortfall`, `authority_restart` or `drill`
  (`engine::ABANDON_REASONS`), or the cohort's own fault code; no figures,
  account or provider names, because the signed abandonment record carries
  it beyond the eval store (the legacy budget fault codes are published as
  `resource_guard`, as fault and reason). After it,
  the calculation target stays the previous cohort's, every receipt and the
  phase log stay, its drawn humans stay used, and the next queued cohort opens
  at the next boundary. The main instance then writes a signed, content-free
  `cohorts/<label>/abandoned.signed` (the shared contract's
  `ABANDONMENT_RECORD`, which the publisher reads and verifies;
  `CohortAbandonment`: chain, cohort,
  label, manifest digest when sealed, fault, reason, epoch) beside the
  cohort's other signed records (a reason stored before the vocabulary is
  published as `unlisted`). The record is written when admission and the
  active cohort have nothing to send; a write that fails waits out its retry
  time (`abandonment_record_retry`) and holds no other work. Elapsed time
  never abandons a cohort, and a platform or time-caused fault never needs
  abandonment.

**Root step errors.** A cohort's root step (draw, seal, package, closure) that
fails is recorded against its cohort: `/eval/status` cohort `step_error`
(code, since, the stored epoch at its first failure, count, retry time, and
whether it is deterministic). A transient code (a bucket read or write, a
storage transaction, an arena retry) runs again after 30 s; one that keeps
failing through a whole tempo of chain progress (the stored epoch two past
`since_epoch`) is alerted as `cohort_fault` and still pauses nothing. An
object that is not there has its own `... missing` code, apart from a failed
read of it. A code no wait changes (a digest or object that does not
match its record, a missing object or key, stored bytes that do not parse, an
engine refusal under the same stored inputs) pauses the cohort as
`step_refused`, named once in its phase log (`step_refused:<code>`) and
alerted as `cohort_fault`; a clear runs the step again once the cause is
corrected. A closure that fails waits out its retry time (or its fault) while
every other step runs, so a closure that cannot be written holds up no
admission, later cohort, settlement or purge; it still counts as held work
for a succession. If even its failure cannot be recorded (the store refuses
the transaction), the step ends and the alarm's 30 s re-arm retries it,
rather than running the same closure again at once.

**Humans.** The pool is the allow-listed Dev-iter `human_only` rows exported by
[`tools/a-testnet-eval-manifest`](../../tools/a-testnet-eval-manifest/README.md)
`pool`: a content-free manifest and private shards in the eval bucket. The draw
(`whole_group_domain_proportional_v1`) takes whole source groups, domain
quotas proportional to what remains, and never a row or group any earlier
cohort used. The never-reuse set has two durable copies: the Durable Object's
tables, which commit with the cohort's documents, and a ledger outside it, one
content-free entry per cohort beside the pool
(`pools/<pool>/used/<label>.<authority>.json`: row IDs, text digests and
groups, with the writing authority). The entry is written before the draw
commits; a draw avoids the union of both copies and refuses to draw if the
ledger cannot be listed, read or parsed. A new or emptied authority (another
Durable Object namespace after T13 is removed, a deleted Worker) therefore
still never reuses a drawn human. Before drawing, the cohort checks the plan's
kind counts: a pool that cannot supply the humans, or their sentences the
mixed cases, beyond the tolerance pauses it (`draw_shortfall`) with nothing
drawn, exposed or marked used.

**Generation** (Workers AI, `eval::textgen`). Each drawn human is a
`human_only` case and the parent of one `mixed` case: 10, 40 or 80 % of its
sentences (start, middle or end, by private digest) are rewritten by a
generator and spliced back (`sentence_splice_v1`), with the exact final byte
span and the realized share `words(model) / words(final)`
(`unicode_split_whitespace_v1`); at least one human sentence remains.
`model_only` cases are written from scratch from a domain register and a topic
(`register_topic_scratch_v1`), with no human text in the prompt. Generators
rotate by private digest; each item's next generator is stored. Generation
lanes first acquire grants from the deployment's shared `GenLimiter` (a global
token bucket and one per model, 290 a minute plus a burst of 10 each, with AIMD
on throttles), then claim items whose next generator is granted, each under a
claim token and a lease and reserving its upper-bound cost (a claim succeeds
only if spent plus reserved plus its bound fits the plan budget), run up to
`inflight_per_lane` calls, refund unused grants, and commit. A result whose
claim is no longer the item's latest records its usage and is discarded. A
rejected answer (preamble, markup, thinking, unchanged, outside the length
window, empty after generating tokens, or a final document over the scorer's
2,048-token limit, `length_tokens`) retries on the next generator, at
most 4 answers per item,
as does a model the provider no longer serves (5007, 3042). A throttle (a 429
or 3040 by error text, since the Rust binding exposes neither status nor
code) uses no answer and keeps the generator: the item backs off from 2 s to
60 s, until its accumulated throttling passes the plan's `max_throttle_ms`,
after which a throttle counts as an outage. An outage (a transport failure, or
no text and no generated tokens) uses no answer: the item backs off from 5 s
doubling to 120 s, and its 10th consecutive outage (about 10 minutes) pauses
the cohort, so an outage can never drop cases or change the mix. A length
rejection records its direction (`length_short` or `length_long`; before
2026-11-03 both were `length`), and `/eval/status` counts each cohort's
rejection codes overall (`rejects`) and per document kind
(`rejects_by_kind`: rewrites are `mixed`, scratch writing `model_only`), so
the rewrite window (0.5 to 1.8 times the source words plus 4) can be checked
against measured rejections before it is changed.

**Token limit.** The final document of an accepted answer (the spliced
child, or the scratch text) is counted under the pinned tokenizer by the
table compiled into the build (`slop-ninja-scorer-tokens`: derived from the
pinned `tokenizer.json` and checked against the scorer's own tokenizers
build over every code point, synthetic documents around the limit and the
pool shards). One over 2,048 tokens with `[CLS]` and `[SEP]`, which no
artifact could score, is rejected as `length_tokens`. Before the cohort
leaves `generating`, and before each seal step of a cohort already in
`sealing`, the pre-seal screen (`engine::screen_generated_lengths`) counts
every ready generated document again; one over the limit (only one accepted
by a build without the check can be) goes back to `pending` for the next
generator, its answer moved from accepted to rejected in the usage. The
earlier build's acceptance never costs the item its replacement: a
sent-back item gets at least one further answer even after its fourth. A
sealing cohort returns to `generating` (nothing is exposed before the seal
commits). A clean complete screen is noted once in the phase log (`token
screen clean (<table id>)`), so a retried seal step does not count the
batch again. An item whose last answer is rejected on arrival is dropped
as for any rejection: spares and the shortfall tolerance cover it, and past
them the cohort faults as the terminal `batch_shortfall`. A rejection
advances the item's claim token, so a redelivered commit of the rejected
answer is fenced (usage recorded again as fenced, never a second attempt). The
count adds no call to the scorer or to any hosted service, and its buffers
are admitted as exact arena blocks of the step. A count the arena refuses
fails nothing else: in a generation commit the item is handled as an outage
(no attempt used, back-off, counted toward `failures_before_pause`) while
the commit's other results and every result's usage apply; in a screen the
document is skipped for that pass and the cohort seals only after a
complete pass. A busy pool is the transient step error `scorer token
scratch` on `/eval/status`, cleared by the next count that finishes; a
capacity or arena defect refusal pauses the cohort resumably
(`arena_capacity`, `arena_defect`). A plan whose artifact spec (spec v2 when
it names none) pins another tokenizer than the table's is refused when it
is queued. Limit: a cohort sealed
by a build without the check keeps its sealed documents, so one over the
limit there is still found only at packaging, as `reference_failure`.

**Pre-start.** With `prestart_lead_ms` set, once the generation tail is
projected (pending items times the mean time per resolved item) to end within
that lead, the sweep kicks the scoring lanes once: they start their containers
and load their artifacts while generation finishes, hold no claim, and wait
for the seal; a lane that waits past `prestart_wait_ms` after loading stops
its container and is kicked again when scoring starts. Nothing is warm at
rest. The assignment a pre-started lane read had no manifest digest yet, so
its first claim re-reads it before scoring. A start the platform leaves
unplaced costs `SCORER_START_WAIT_MS` and a retry, so a lead shorter than
that bound plus a cold path does not hide it.

**Seal.** Generation ends only if the ready documents meet the plan's per-kind
counts within the declared tolerance (`kind_shortfall_permille_v1`); otherwise
the cohort pauses (`batch_shortfall`) before anything is sealed. Ready
documents get task indices 0..n-1, each its own reward group (the plain mean
over the self-weighting batch). The batch goes to the eval bucket as
64-document JSONL shards plus a text-free index (labels, spans, digests, and
the plan's, realized and missing counts per kind); the evaluation-set digest
binds the plan bytes and the index, so any shortfall is bound into the
manifest. The v1
cohort manifest is signed with the package key and stored in the receipts
bucket.

**Scoring.** One job per roster artifact, each on W scoring lanes. A scoring
lane reports at alarm entry, starts its container with Internet access off,
waits for it to answer (bounded by `SCORER_START_WAIT_MS`), checks its build
identity (the scorer binary's and ONNX Runtime's SHA-256 against the pinned
`SCORER_BINARY_SHA256` and `SCORER_ORT_SHA256`), streams the artifact's files
under its files manifest v2 (a miner's from the admission bucket, a reference
package's from the eval bucket) in ranged reads of at most 64 MiB pinned to
the entity tag admission recorded, piped untouched to the scorer's writes at
their offsets, and loads it with no hash (the digests are the header's, which
R2 verified at upload),
reporting every 20 s and holding no claim; it sets the container's inactivity
timeout so the instance survives between its alarm invocations. It then claims
chunks (`chunk_docs` tasks), reads their texts from the sealed batch shards in
the eval bucket (never through the cohort object), checks every text digest
against the claim, scores, stores the private results object
(`cohorts/<label>/results/...`, eval bucket) and then the receipt object, and
commits. The commit rebuilds the receipt object from the committed outcomes,
the claim and the cohort seed and refuses a mismatch. A lane works until its
job has no claimable chunk (then stops its container) or its step budget is
spent (then re-arms at once). The lane's decisions for each claimed chunk are
one function, `lane_logic::run_chunk`, which the Worker runs over R2, the
cohort routes and its container, and the native tests run over a simulation.
Every attempt gets a v1 `AttemptReceipt`:

- each chunk keeps its highest allocated attempt, the highest attempt whose
  receipts committed, and a claim fence. An expired claim whose attempt may
  have stored its objects is first claimed for **reconciliation**: the lane
  fetches the results object and the receipt object, rebuilds the receipt
  object, and commits the stored answer with no compute if the bytes match
  (re-storing the receipt object, a pure function of the results, if only it
  is missing). Only a confirmed absence of the results object records the
  attempt lost; a read error hands the claim back for a later retry, counted
  as a scorer failure. The next attempt's object carries an
  infrastructure-error receipt for a lost attempt; consecutive losses each get
  one receipt. A late commit from an expired claim is fenced;
- **hand-back before dispatch.** A claim whose documents never reached the
  model goes back with no attempt spent and no receipt (`/lane/give-back`): a
  batch shard that cannot be read or does not match, a missing cohort
  manifest digest, a scorer that refused the request (409 or 503), or an
  isolation restart that fails. Each counts a scorer failure, so an outage
  pauses the cohort (`scorer_unavailable`, resumable by a clear) instead of
  spending attempts. A put that keeps failing after dispatch (retried while
  the lease has time) releases the claim with its attempt allocated, for
  reconciliation;
- **crash evidence.** Only container death is a crash: the container stopped
  or stopped answering `/healthz` after the call, or a request outlived its
  bound and was destroyed. A dispatched request that failed while the
  container stayed up and ready with the artifact is an infrastructure error,
  retried as a new attempt without isolation. The scorer aborts on any panic,
  so a panic is a death;
- **crash isolation.** After a crash the chunk's next claim runs its
  unresolved documents alone, from a freshly restarted instance, recording
  each outcome with the cohort as it resolves (`/lane/iso-progress`, which
  renews the lease). A document that kills its instance is not attributed
  yet: a fresh instance is started, must first score a fixed synthetic
  control document, and then runs the document alone again. Only if this
  second run kills the proven instance too is it a `model_error` for that
  document of that job only (utility 0 there); if it answers, its answer is
  the outcome. A run checks its step budget, and whether a newer kick
  superseded the lane, before every step; when either stops it, it hands the
  attempt back (`/lane/iso-yield`) with everything it recorded, and the next
  claim continues the same attempt number, never recomputing a recorded
  document. A lane that dies mid-run is continued the same way after its
  lease;
- **artifact attribution.** A load that fails or outlasts its allowance, a
  container that dies during the load, missing or rejected parts, or a
  control document that a freshly restarted instance cannot score, are
  artifact failures when they happen on a freshly started instance
  (`/lane/artifact-failure`). After `artifact_failures_before_model_error` of
  them with no prediction of that job committed in between, an entrant's
  unresolved documents are `model_error`s, committed without a container, and
  the cohort proceeds. A manifest over `artifact_max_mib` or not matching its
  package digest attributes at once. Reference artifacts are never
  attributed: their failures count as scorer failures and pause the cohort;
- under the v1 manifest `max_attempts` is the attempt allowance per round.
  When a round's last attempt is spent, a record-only claim writes the
  remaining lost receipts (`...-attempt-N-lost.bin`) and the chunk gets
  another round (unresolved infrastructure, above). Only when none fits does
  that **job alone** fault (`unresolved_infrastructure`): the other lanes
  finish, and the cohort pauses at the end of scoring, since packaging needs
  every job.

**Receipts** are stored as chunk objects (`slop_ninja_receipt_chunk_v1`:
`u32_le(len) || receipt` repeated). At packaging, one bundle lane per job
concatenates the job's referenced chunk objects into
`receipts/bundle-job-<j>.bin` (itself a valid chunk object), checks every
receipt against the job's receipt rows, and reports its digest and count. The
v1 receipt index binds every receipt's own digest and length, so
`ReceiptIndex::verify_object` checks receipts cut from bundles;
`chunk-map.json` lists each bundle's key, digest, length and count. Result
salts derive from the private seed and the attempt ID.

**Scorer containers.** One per scoring lane, started on demand and stopped by
the lane when its job is done or the cohort pauses. Each instance serves one
artifact for one cohort: before using a running instance the lane reads its
`/healthz`, and destroys one bound to another package or cohort, or whose load
of this artifact failed, without counting a start failure. Lanes name their
cohort label on load and score when the scorer reports `single_use`, and the
operator scorer probe names none. A `refused <code>` reply is a definitive
artifact failure, except a failed load's `refused object`, which can also be
the instance's own read fault and counts only on a fresh instance; 409
`instance_bound`, 409 `missing_parts` and 507
`insufficient_memory` are the platform's (`eval/scorer_client.rs`).
`AScorer`'s idle alarm stops an instance after 120 s without use as a
backstop; a stop whose `destroy()` is refused, or which still reads as
running, is re-checked with backoff (30 s doubling to 5 min). At the end of
scoring the sweep stops any lane that has not reported its container stopped.

**Package.** One summary lane per job reads its job's prediction rows with
their labels in pages and writes a private summary
(`cohorts/<label>/summaries/job-<j>.json`, eval bucket, deleted with the
private texts): the utility, outcome counts, content-free components and the
involvement vector, with the row count and a digest of the rows read, which
the cohort recomputes from its own rows before accepting it. The packaging
root step reads the summaries (checked against the reported digests),
recomputes every utility from the involvement vectors and the private labels,
builds the sorted receipt index from the receipt rows in pages, and hands
`PackageInputs` to a `RewardPolicy`. `RewardV0` is the only rule (the reward-v1 mapping is under
review): R_k's utility is the baseline and entrants' utilities are published.
The package, its calculation target (predecessor: the last activated target)
and the index are signed with the package and receipt keys. These are the T12
trust roots that the native validator and the coordinator are configured with
(`PACKAGE_PUBLIC_KEY`, `RECEIPT_PUBLIC_KEY`, pinned in
[`testnet-trust.json`](testnet-trust.json)): the eval Worker refuses to sign
unless its secrets derive exactly the configured roots. `components.json` adds
content-free diagnostics per job: Brier by kind, share error, attribution label
rates, the paired Brier gain over R_k with a source-group clustered SE, and
pairwise prediction RMS, plus the published `crash_rule` and `artifact_rule`
and per job its isolated crashes and any recorded artifact failure. Exact
packages, or RMS at most 0.01, form one near-duplicate cluster. Copies of one
artifact scored on different lanes are not bitwise identical: their
predictions differed by an RMS of about 10^-8 on 2026-10-31 (TESTNET.md), so
under `testnet-reward-v0` a copy can land on either side of R_k by noise. R_{k-1} is scored on the same batch when the plan names
one. The v1 manifest has no predecessor field, so R_{k-1} is listed among the
manifest's scored artifacts (its receipts verify) and never credited; manifest
v2 should name it separately. While R_k is R_0 there is no predecessor.

**Receipt retention (the N+2 rule).** Cohort N's detailed receipts are
deleted once two later cohorts (N+1 and N+2) have completed in the same
cohort object (`eval::retention`, rule
`receipts_after_two_later_completions_v2`). Completion is durable: the
cohort is `closed` and, for a `reward-v1` cohort, its closure is recorded.
Probe builds (no package or target, outside the public record) and
abandoned cohorts are not completions, so the coordinator's N+1 and N+2
are the publisher's; their own receipts follow the same rule. Nothing
reads wall time, tempos, adoption or publication, so silence or slow work
only moves a deletion later.

- **What goes:** in the receipts bucket, the chunk objects and bundles
  under `cohorts/<label>/receipts/`, `receipt-index.signed` and
  `chunk-map.json`, and a probe build's index and map under its probe
  prefix (only by the latest finished build of that probe version, which
  wrote them); in the cohort object, the cohort's `receipts` and
  `receipt_objects` rows. These are the objects the publisher calls
  detailed receipts. Before the first object goes, a content-free marker
  `cohorts/<label>/receipts-retired.json` (`slop_ninja.receipts_retired/v1`:
  label, cohort, rule, `due_after_cohort`) is written. The marker, the
  pointer's key, kinds, checks and read bounds are one contract the
  coordinator and the publisher both import
  (`slop_ninja_admission_wire::receipts`).
- **What stays:** the signed manifest, package (with the receipt root and
  count), target, closure and their companions, the cohort row with
  `receipt_root` and `receipt_count`, targets, rows, and every recovery
  record.
- **Held cohorts:** new commitments need the receipts of what they commit
  to. A cohort is held, whatever completes after it, while its target is
  one a signing path reads: the newest `reward-v1` target (the row
  `active_target_digest` reads) or the newest target of any rule (the
  predecessor a reward-v0 package chains to). A rule that stopped
  producing targets is released once a later target activates. A
  superseded authority holds no target once it accepted its successor's
  notice. A cohort is also held while the current authorization names its
  target (that target is not superseded while the authorization commits
  it, so the approved rule keeps it). The authorization's carrying cohort
  is recorded but not held: holding it past N+2 changes the approved rule
  and awaits the user's decision, so until then a validator that starts
  after a carrier's deletion waits for the next authorization
  (`receipts_expired`). The authorization is read
  from the private pointer `authorizations/current.json` and the object it
  names before every deletion step; the pointer must carry the known
  schema, this deployment's chain binding and content-addressed object
  keys. It stays valid until a newer one replaces the pointer, so this hold
  has no count or time limit; a revocation the pointer lists does not end
  it by itself. A due cohort the pointer names goes back to waiting unless
  its marker is already written. A pointer that cannot be read, parsed or
  matched (including an unknown kind) stops deletion until it can, and the
  stall is reported; without a pointer only the active targets are held,
  and a launch authorization names no receipts.
- **When:** a cohort becomes due at the first step at which it is not held
  and two later completions exist. `due_after_cohort` is the completion
  that satisfied the rule: the second later completion, or the cohort whose
  target released N's when that is later. An authorization hold delays
  deletion without changing it. Deletion steps (and predecessor notices)
  run when the root has nothing else to do: with no active cohort, or while
  the active cohort only waits on its lanes, so back-to-back cohorts never
  hold the rule up.
- **Predecessors:** a superseded main authority completes nothing more.
  After its own second completion, the successor tells each predecessor
  (`POST /succession/retention`, refused before supersession and for a
  cohort number not later than the predecessor's own). The predecessor then
  records the notice, stops holding its own targets (the chain-record
  restart set them aside, and the successor's two completions show its own
  chain is active) and marks every remaining cohort due; only a cohort its
  current authorization still names waits, and the next notice marks it
  again once the pointer moves on. The successor repeats the notice until the
  predecessor reports nothing left, because a retired authority's alarm
  re-arms only while it is kicked. An unanswered notice, or one answered
  with work left, is retried at each tick (a kick that carries the epoch
  arms one step for it, `engine::kick_due`), on an otherwise idle successor
  too; it never keeps the successor's alarm armed by itself. Due deletions keep an object's alarm armed but are not `work` for
  a succession or an authority switch (`engine::blocking_work`; the status
  reports them as `retention_work`): whatever remains finishes under the
  successor's notices, so a storage fault there never holds up a restart.
- **Resumable:** each step deletes at most one listing page (R2's
  1,000-key bound), then the two single objects, then one page of rows; the
  transaction that removes a cohort's last row marks it deleted. A failed
  step (pointer, marker, listing or delete) is recorded and retried at a
  later step, and holds up nothing else.
- **Status:** each cohort in `/eval/status` carries `receipt_retention`
  (`retained`, `due_after_cohort`, `retiring_ms`, `deleted_ms`,
  `objects_deleted`, `held`), and the top level the rule, the retained
  count, the predecessor notices, the authorization hold, the last
  failure (`last_error`, `error_since_ms`, `errors`), the notice a
  superseded authority accepted (`notice_accepted_after`) and `stalled`:
  each due cohort whose receipts remain after a completion later than the
  one that made it due, and each predecessor not yet finished after a
  completion later than the notice's. The publisher's `retention_stalled`
  alert fires while that list is non-empty (completions, never a clock).
- **Publisher:** a publisher that had not copied a cohort before its
  receipts went finds the marker, deletes any detailed receipts an
  interrupted copy left in the public bucket, and records the cohort once
  in the public index's `retired_unpublished`, never pending or retried. A
  retired record-starting cohort still starts the record for later ones.

**Spend guards and the tracking ledger.** Evaluation spend per cohort is
bounded by the Container `max_instances` (equal to `MAX_SCORE_LANES`), the
plan's lane caps and the per-cohort `scoring_budget_microusd` (checked at
admission against the plan's own upper bound, J x n x `doc_ub` + lanes x
`cold_ub`, with every entrant's probe and audit documents counted, and at
every sweep against measured spend plus the remaining bound, which raises an
alert and pauses nothing). The `SpendLedger` tracks; it never refuses or
pauses anything for spend. A plan's generation and scoring budgets are
recorded as a reservation before it queues (keyed by the billing cycle from
`BILLING_CYCLE_DAY`: 1, calendar months, a user decision), a finished
cohort's measured spend is charged once, operator probes settle their
measured cost under their own keys, and each admission run settles its
instance time at the cohort's Container rates (`<instance>:<label>:admission:
<ms>`). Every open reservation counts toward the cycle's total, whatever
cycle it was made in. The ledger status reports the cycle's settled and
reserved totals and `over_threshold` once they pass the deploy-time alert
threshold; the publisher's `ledger_threshold` alert keys on that flag. The
`SpendLedger` object (`src/cf_lanes.rs`) reads the threshold from
`SPEND_ALERT_MICROUSD` through `ledger::alert_threshold`; without it the
ledger standing reports `alert_microusd: null`, the publisher raises
`ledger_threshold_unset` (so a deploy that dropped the variable is visible),
and the ledger still records every reservation and settle. A label already queued never touches the
ledger again, a refused queue releases only a reservation its own request
made, and a ledger outage only leaves a record missing; it never delays
evaluation. Rates and bounds are the plan's `fanout.cost`; none is a
compiled-in constant.

**Admission.** A `reward-v1` plan with an `admission` block is admitted by
the coordinator (`eval::admission`, contract `slop_ninja.admission/v2`,
store v25; [admission API](../../docs/testnet/admission-api.md)):

- The window opens at the cohort's opening boundary and closes at the
  boundary that reaches the admission minimum (`cutoff_block`, that
  boundary's first block), once that boundary's canary passed. The
  validity window, the roster cap and the upload URL policy
  (`admission.upload_policy`, `slop_ninja_upload_url_policy_v1`) are plan
  parameters; the object bound derives from the plan's artifact spec
  (`SpecBounds`: the largest served graph class, the pinned tokenizer, the
  spec's JSON budget and the protocol header bound), and the scoring lanes
  size their load allowance from the same spec (`fanout.score.artifact_spec`).
  Admission plans name spec v2 or v3 (`admission::ADMISSION_SPECS`); `/plan`
  and `/template` refuse any other spec (`admission_spec_unsupported`) and
  a plan with v1's `part_size` or `max_parts`.
- **Hash once, direct to R2.** Miners PUT the header bytes and each file
  straight to the admission bucket under presigned URLs the internal signer
  Worker (`subnet/r2-signer`, binding `R2_SIGNER_TESTNET`, token
  `SIGNER_INTERFACE_TOKEN`) mints for this Worker only, one signer request
  per `urls` call. Each URL binds the derived key, the committed length, the
  committed SHA-256 (R2 verifies it at ingest) and create-only. No upload
  byte passes through any of our Workers, and nothing here hashes a model
  byte: objects are recorded from R2's `head()` (size, stored SHA-256, entity
  tag, and the stored-attribute rule `slop_ninja_object_attributes_v1`,
  named in the limits record: no customer key, the standard storage class,
  and empty HTTP and custom metadata, since R2 stores six unsigned standard
  headers of a presigned PUT as HTTP metadata).
  The small records hashed are a session-creation body, the files manifest
  (the package digest), the canary body and the control document; the
  subnet lint `tests/no_object_hash` checks it.
- Requests are signed by the hotkey they concern. The upload Worker
  verifies the signature and body digest statelessly before forwarding; the
  cohort object checks them again against the RPC-confirmed cursor and
  requires the session's own hotkey for `urls`, `complete` and `repair`.
  Sessions (one per hotkey and commitment), their objects (`adm_objects`:
  the header and each distinct file, with the recorded entity tag) and each
  hotkey's last refused request live in the cohort object, with one
  constant-size mint aggregate per session (`mints`, `url_until_ms`), never
  a row per URL.
- **The header first.** `urls` (and `complete`, an event or the alarm
  sweep) settles the header before any file URL is minted: R2's head must
  show the envelope's size and SHA-256; the bytes are read pinned to that
  entity tag and piped as a stream into the upload Worker's header route
  (`/internal/upload/v2/header`), whose bounded outcome is checked here: the
  table is rebuilt, its content digest recomputed (a mismatch with the
  outcome's is the platform's fault, never the miner's), the contract and
  committed content compared, and `SubmissionUpload::admit_header` run
  against the logged commitment. No header object yet: `header_missing` with
  a fresh header URL.
- **Files.** Recorded from R2's heads by the client's `complete`, the R2
  object-create event (Queue `admission-events-queue-testnet`, a
  wakeup only), or the alarm sweep (once a tick, sessions that had a URL
  minted). An object present with other bytes or attributes is deleted
  (while it keeps the tag seen) and may be PUT again; a head error or a
  missing stored checksum records nothing. When the header and every file
  are recorded the session verifies: its files manifest v2 (each file's key
  and entity tag) is written to the eval bucket, and its admission run
  follows.
- **Upload-path faults.** A mint the signer could not make persists
  `signer_unavailable`; a failed end-to-end canary (a presigned PUT of our
  own 64 bytes under `health/`, confirmed by a head, then deleted)
  persists `upload_path_unavailable`; a header settle the platform could
  not run (R2's head or pinned read, the upload Worker's binding, header
  route or answer) or a failed link probe (the canary's call of the header
  route, which also runs the upload Worker's registration read) persists
  `upload_front_unavailable`, cleared by a settle the upload Worker
  answered or a passing probe. Each cancels earlier successes and defers
  the close to the first boundary after a later success; the canary
  retries once a tick while a fault holds and is the close's first step, so
  the platform's outage never ends a miner's upload. All show in
  `/eval/status` (`admission.upload_path`). URLs the signer minted after
  `closing` was persisted are not handed out (`after_cutoff`).
- **Lost objects after admission.** A scoring lane whose load meets an
  admitted entrant's object absent or changed (`/lane/object-missing`), or
  the publisher's report of a missing object, audits the session: its
  recorded objects are headed, an absent one is listed `repair_needed`, an
  unclean one (the committed bytes with a stored attribute) is deleted for a
  clean PUT, and the session waits in stage `repair`. The lane holds,
  counted neither as a scorer failure nor as the artifact's, and retries
  each admission tick until the repair is recorded.
- **The close.** `closing` is persisted at the boundary (after its canary):
  from then `urls` and `complete` answer `after_cutoff` and only the close
  sweep records presence. It heads every unrecorded object of every open
  session once, in batches within the step's R2 calls
  (`admission::R2_CALLS_PER_STEP`, half the documented per-invocation
  subrequest limit), its progress persisted per object; a head that errors
  defers it after a backoff, and only a definite absence counts as missing.
  Then the close is recorded: complete sessions verify, the rest are refused
  `incomplete_at_cutoff`.
- **Repair and audit.** A verdict run that met an object missing or
  changed since admission recorded it (`scorer_client::OBJECT_CODES`) sends
  its session to an audit: an absent object is the platform's (the run
  waits, the session lists `repair_needed`, and `repair` mints a URL for
  exactly that recorded key, digest and length while the object is still
  needed, before its publication is recorded); one present with its digest
  but another entity tag or a forbidden attribute refuses the session
  `object_replaced` (the miner's, never a cohort pause).
- **Retention.** Superseded sessions' objects are deleted at once, those of
  sessions that ended without admission at the freeze, admitted ones once
  their publication is recorded and their cohort closed; a standing
  reconciliation lists `admission/v2/` one page a step and deletes any key
  no live session holds (a stale URL can recreate one). It never ends on time
  or on an empty list and gates nothing. There is no lifecycle rule.
- A verdict run's platform failures pause the cohort (`scorer_unavailable`),
  and so does a commitment resolution whose reads keep serving no state
  while the hotkey has a session uploading or verifying
  (`commitment_state_unavailable`). No parser of graph or role-file bytes
  runs here, and no uploaded header is decoded in this Worker, which holds
  the package and receipt keys.
- The verdict comes from an admission run on the scorer Container, through
  the scoring lane's admission route (`AScorer` `POST /admit`): a fresh
  single-use instance per try loads the files manifest v2 (each file read
  from the admission bucket in 64 MiB ranged reads pinned to the recorded
  entity tag, nothing hashed) under the cohort's label, its
  spec digest (spec v2 or v3) and its lane limits (size bound, load
  allowance, inactivity and document time limit), runs the scorer's own
  admission (graph pre-parse included) and scores the control document; the
  reply carries the prepare's `definitive` flag. One run per hotkey and
  commitment. A verdict records the `SCORER_BINARY_SHA256` and
  spec it was reached on (rule (b)); when either differs from the
  deployment's, it runs again, refusals included, and its session waits on
  it. Only a definitive `refused <code>` refuses. A host fault the scorer
  files under `object`, a load that failed, died or outlasted its
  allowance, and a control document that did not score count on fresh
  instances toward the plan's `artifact_failures_before_model_error`.
  Everything else (placement, `insufficient_memory`, the coordinator's own
  manifest, a chunk transfer, a code this build does not know) is the
  platform's: retried with a doubling backoff from 60 s to the 15-minute
  run lease, and reported in the admission status (`platform_retries`) for
  alerting. Runs count against `MAX_SCORE_LANES` together with the live
  scoring lanes.
- The roster freezes at a tempo boundary once its work is in: every
  completed upload verified or refused, every verdict in on the
  deployment's build, the commitment snapshot at `cutoff_block - 1` read.
  Ready within the epoch the window closed in, it freezes then; ready later,
  at the next boundary (`admission_closed_epoch` is that boundary). Until
  then the cohort waits in `admission`, never skipped. Sessions whose
  commitment a later one replaced, whose registration changed, or whose
  commitment shares its extrinsic with another hotkey's (a batch) are
  refused; the rest are ranked by `(block, extrinsic)`, the first
  `roster_cap` admitted, the rest marked overflow for the next cohort. The
  admitted roster, with each opening's nonce, extrinsic index and object, is
  written into the stored plan (so the seal binds it), and the cohort moves
  to `generating`. The decision is computed before anything is written; a
  frozen plan the engine refuses faults the cohort (`roster_freeze_refused`,
  its code in the phase log) instead of failing every step; once the cause is
  corrected a clear runs the freeze again on the same roster.
- **Ways out.** `POST /eval/session/refuse` (`{session, reason}`, main
  instance) refuses one stalled session with a recorded reason
  (`check_failed`, `operator:<reason>`), and its pending verdict with it.
  `POST /eval/abandon` also abandons a faulted cohort in `admission`: its
  window closes for good and its live sessions are refused.
- **Clean-up and publication.** After the freeze (or an abandonment),
  sessions that ended without admission lose their objects. Admitted objects
  stay in the admission bucket until the cohort closes and their publication
  is recorded. Publication happens at scoring start, in the publisher
  (`subnet/publisher`, `src/artifacts.rs`): once the signed manifest seals
  the roster, it copies each admitted entrant's files and header from the
  admission bucket to the public bucket's digest keys, by a binding stream R2
  verifies or a server-side `CopyObject`, hashing nothing; the closure no
  longer names objects for a later copy. A standing reconciliation lists
  `admission/v2/` and deletes any object no open session or admitted entrant
  awaiting publication holds; it never ends on time and gates nothing.
- **The commitment log** (`eval::commitments`) runs from the cron tick on
  the RPC-confirmed watermark, within its own share of the per-minute RPC
  budget (`READER_RPC_CALLS_PER_MINUTE`; by default the budget less the
  heartbeat's four calls and the tick's epoch read), each call also
  reserved in the shared budget, at most one large read a round: the
  snapshot first, then resolutions oldest first, then the poll. While a
  cohort admits, at most once a minute, it reads the subnet's
  `Commitments.CommitmentOf` keys and values (with `System.
  LastRuntimeUpgrade`); for each new commitment, its block's hash, then
  `System.Events` and `Uids` at that block (the extrinsic index of the last
  `Commitments.Commitment` event for the hotkey), then
  `BlockAtRegistration`; at the cutoff, the snapshot. Storage decodes by
  name from the runtime's own V15 metadata, kept beside the log and re-read
  when `LastRuntimeUpgrade` reports another spec. Every read at a past
  block also reads `LastRuntimeUpgrade` there: a resolution stage or a
  snapshot read under another runtime reads the metadata at its own block
  and is read again, so a runtime upgrade never makes a commitment
  unresolvable. A read the RPC endpoint answers as discarded state is sent
  again to `ARCHIVE_RPC_URL` when the deployment sets one (each call
  reserved in the budget), and only the archive's answer is ingested.
  Failures stay local: a value or event list that does not decode, an
  extrinsic that committed for several hotkeys (`shared_extrinsic`) and
  state no endpoint keeps (`state_discarded`) end that commitment's
  resolution as unresolvable and never abort the round, and a session that
  opens it gets `commitment_unresolvable` with the reason, so the miner
  commits again; a key listing whose block is gone restarts at the head; a
  snapshot whose block is gone pauses the closing cohort
  (`snapshot_state_discarded`, resumable) and is read again each round, and
  the first read that answers resumes the same cohort. Later state is never
  substituted. The cutoff snapshot also reads each hotkey's `Owner` (its
  coldkey at the cutoff).
- **Fresh stock** is drawn only when the frozen roster holds an admitted
  outside entrant (a hotkey not in the plan's `operator_hotkeys`, not owned
  at the cutoff by a coldkey in `operator_coldkeys` (an owner that does not
  read counts as the operator's), whose content is not already published,
  whose artifact loaded and scored the control document) and the plan's
  `admission.fresh_pool` names the pool; a plan with a fresh pool must list
  the operator's hotkeys and coldkeys, and a standing template never names
  one (the content check is exact, and the copy gate that catches a near
  copy runs only at scoring). Every other cohort draws only
  `EXPOSED_POOLS`.

**Standing template and calendar.** `POST /eval/template` installs one
template (`slop_ninja_eval_cohort_template_v1`: a `label_prefix` and a v3
plan with an `admission` block and no entrants), checked at its own roster
cap and with a predecessor reference: the plan validates with that many
admitted entrants, its scoring budget covers their upper bound with two
references, its package step fits the memory model
(`arena_routes::package_fits`), and every receipt a cohort at the cap can
record (every scored document at every attempt, `CohortPlan::receipt_bound`)
fits one signed receipt index (`plan::receipt_capacity`): the tighter of the
codec's sequence bound (2^20 entries) and the entries whose record fits the
protocol's 64 MiB signed-record limit at 69 bytes an entry
(`target_bundle::receipt_index_capacity`, 972,591 entries), so the
coordinator can sign every index it packages. Validators hold a target's
receipts through bundle schema v3, which names the receipt indexes and
chunks of the target's cohort and its carrier by path, digest and length
instead of embedding them, so the native validator's 64 MiB file bound
(`VALIDATOR_BUNDLE_BYTES`) limits only the embedded records; its fetch
session reads the two signed indexes into memory within their own
allowance, two indexes at that capacity
(`target_bundle::validator_receipt_index_bytes`). The structural bound on
a template's cap thus follows from its shape and the protocol's bounds (with
the package step's memory model and the scoring lanes);
`slop_ninja_eval_ops standing-template` derives it and takes the largest cap
it admits up to `ROSTER_CAP`, a deployment ceiling (40 on testnet, set by
the design note's testnet plan; frozen into each cohort's plan; widened by
a deploy that changes it), and the install reply reports receipts per cohort against each
bound (`receipt_bounds`: sequence, signed record, validator session), the
signed index's bytes, and whether the embedded schema v2 form would still
fit (`capacity`). It must run the testnet plan defaults of the
2026-11-01 ruling: `lane_docs >= n` (one scoring lane per artifact),
`prestart_lead_ms = 0`, and `reward_v1.incentive` with its provisional-gates
flag. When nothing is queued and the latest cohort other than a probe cut
is in its transition (or closed or abandoned), the cohort object queues the
next cohort from it, numbered one past every cohort held (probe cuts
included) and labelled `<label_prefix>-<cohort>`, naming the
references its seal will meet: a promotion in the cohort now in its
transition (the promoted entrant as R_k, that cohort's reference as
R_{k-1}), else the chain `reference_state` holds, else (no chain yet) the
template's own. A reinstalled template never resets the chain; the install
reply names the next references and their source. The engine opens the
queued cohort at the boundary where the previous one closes, so cohorts run
back to back on tempo boundaries (`GET /eval/calendar`, also `calendar` in
`/eval/status`, which the publisher reads): the current cohort with its
admission window's `window_open` and `window_closing` (and, while closing,
`freeze_wait`: what the roster freeze waits on, read without changing
anything), the next cohort with its earliest opening epoch (the earliest
boundary that can close the cohort before it: `K` credited tempos after that
one opened and its transition minimum after its publication, counted in the
tempos the coordinator still has to credit, so an outage it did not credit
moves it later; always a later boundary than the observed one) and its
`admission_tempos`, and `admission_window` (the template's
`admission_tempos`; the last window measured on chain, with the epochs it
spanned, whether it was `extended` past its plan's tempos, and its blocks per
tempo over that epoch span, the chain's tempo interval; and the template's
window at that interval in tempos, blocks and nominal minutes). The
template's window is derived from measurements by `slop_ninja_eval_ops
standing-template` (`calendar::derive_admission_tempos`: `K` less the
transition minimum and the tempos the work after a cutoff needs, that work
bound, which includes one entrant's scoring lane of the largest object, at
most half of them, and the largest object's measured submission at most half
the window), which prints it in blocks and minutes and names the block
interval it assumed. A template plan the
engine refuses, or a chain the store cannot name, pauses the template with
the reason; only the calendar's own step does that, never a status read. A seal whose plan names other references than the chain faults
the cohort (`reference_chain_mismatch`) rather than retrying.

**Template installs and same-authority probe cuts.** Installs are
journaled (eval store v22, `template_installs`): every install of other
bytes than the installed template's appends its version, digest, the probe
versions it names and `governs_from`, one past every cohort number the
store holds at the install. A queued or running cohort keeps the plan bytes
it was queued with, so a template governs only the cohorts the calendar
queues after its install. Posting the installed bytes again is a replay:
nothing changes, no entry is added, a paused calendar stays paused (resume
with `/eval/template/pause` and `{"paused": false}`), and the reply carries
`replayed: true` with the original entry. A template names only probe
versions the store recorded (a closed probe build's record, by version,
digest, size and key: `template_probe_unrecorded`), and the next cohort's
references must hold a fingerprint on each version it names
(`template_probe_unfingerprinted`: refused at install; at the calendar's
own step it pauses the template with that reason instead of queuing a
cohort whose probe comparisons could not package, as after a promotion the
template's probe versions never measured). `GET /eval/calendar` reports the
template's `version` and `governs_from`.

A new probe version for an authority whose cohorts are open comes from a
**probe cut**: a probe build plan with `reward_v1.probe_cut` (the seeded
task order, no admission block) queued with `POST /eval/plan`. In the
queueing transaction the cohort object refuses it unless the authority
already holds a cohort, no other cut is open, its version exceeds every
probe version any build here named, no promotion is pending in a cohort's
transition, and its references are exactly the chain's current ones as the
stored records name them (`probe_cut: ...`); the seal checks the chain
again (`probe_cut_chain`). The cut takes the next cohort number and starts
at once, whatever the cohorts before it are doing. Cohort work otherwise
runs in seq order, but a cut's work yields to every other cohort's: it runs
only when no other cohort's work is runnable, earlier or later, so it never
holds a cohort back (a yielding cut keeps its state and resumes; replies
from its lanes already running still commit). Its lanes are lane objects of
their own (`<scope>/probe-cut/...`): a lane object keeps only the newest
`(seq, gen)` it was kicked with, and the cut's seq is above the cohort
queued before it, so on shared lane objects that cohort's kicks would be
refused for good. Cut scoring lanes are Container instances under the same
`max_instances` as the cohorts': a cut's lanes and a concurrent cohort's
together must fit `MAX_SCORE_LANES`. The calendar ignores cuts when it decides whether a
cohort is due and which cohorts are current and next, but numbers its next
cohort past them, so the standing cohorts' numbers skip the cut's (the
publisher leaves probe builds out of the public record), and an admission
overflow names the next standing cohort, never a cut. The cut draws,
generates and seals its probe documents (the draw joins the pool's used set
like any other, so later draws, an already queued cohort's included, prefer
rows neither drew; an exposed pool can draw a row again, so nothing falls
short, and no plan or template changes), scores the chain's references on them, records the version and
their fingerprints (publishing nothing new) and closes with no package or
target; like every probe build it is not a completion for receipt
retention. `slop_ninja_eval_ops probe-cut` prepares the plan from the eval
status, and `template-reinstall` the template naming the cut's version
(TESTNET.md, "Same-authority template reinstall"). No chain-record restart
is involved: the chain, the published index, the targets and every open
cohort's plan stay as they are.

**Load instance.** `eval-cohort/testnet/<genesis>/<netuid>/load` accepts only
`slop_ninja_eval_load_plan_v1`: it re-scores an existing sealed batch through
its text-free index (lanes read the source's private shards), against
synthetic, never-registered identities, up to `LOAD_ROSTER_CAP`. It goes from
`sealing` straight to `scoring` (no boundary), and from `packaging` to
`closed` with no transition, no calculation target and no write to the pool's
used set. It signs with keys derived from `EVAL_PRIVATE_SEED`
(`slop_ninja.eval/load-signing/v1/{package,receipt}`), never the T12 roots, and
reports them on `/eval/load/status`; its objects sit under
`cohorts/load-<label>/`. Fault hooks in the plan (`faults`): lanes that
destroy their container mid-request once, lanes that stop after their puts
before the commit, lanes whose alarm throws after a claim, dropped and
duplicated kicks, documents that crash their container every time, jobs
whose every request (the control included) kills the container, a document
that kills it once in isolation, jobs whose artifact load fails, a window of
batch-shard read failures after scoring starts (an R2 read outage), and a
lease shorter than a chunk call. `POST /eval/load/hold` makes every claim
answer `Paused`, as silence does, without stopping the heartbeat.

**Main authority (chain-record restart).** `EVAL_MAIN_AUTHORITY`,
`EVAL_MAIN_FIRST_COHORT` and `EVAL_MAIN_PREVIOUS` name the main instance's
authority. They are tracked in `wrangler.toml` and never passed with `--var`,
so every deploy carries the authority its tree names, and
`slop_ninja_eval_ops authority-check` (`config::deploy_check`) refuses an
eval deploy whose tree names another authority than the deployed one. It
fails closed: a status it cannot read (an error reply, no eval status, a
missing authority, succession or work field) refuses. It reads succession
from what the stores recorded (`superseded_by`, `succession_recorded`), never
from the cohorts listed, since succession is recorded before a plan queues
or a template's first cohort opens. A routine deploy over a superseded
authority is refused (`--freeze` alone redeploys it frozen). `--switch`
passes only the switch commit over an idle authority with no work left, its
template paused, its labels and numbers outside the new authority's and no
other successor recorded, or a rollback before any succession was recorded
on either side (it needs `--target-status`, the earlier authority's own
status; `--freeze` for a deliberate rollback after succession);
`--first-deploy` passes only the one deploy of this support over a build
that reports no authority. `main` (the
default) is the original, whose object, lane names
and probe keys are as they were. Another name (lowercase letters and digits,
never `load`) is a fresh cohort object, so it starts its own reference chain
(the probe build that names R_0 with no predecessor records
`reference_state`), published index, fingerprints, probe versions and reuse
ledger. Nothing it writes can be an earlier authority's: its labels start
with `testnet-<name>-` and its cohort numbers at `EVAL_MAIN_FIRST_COHORT`
(both checked at queue time, `config::Authority`), so its batches, receipts,
manifests, packages, closures and openings never share a label or a cohort
number; its probe objects sit under `probes/authority/<name>/v<version>/` in
both buckets; and its lane objects are new, so no lane holding an earlier
authority's later `(seq, gen)` refuses its kicks. Kicks name the authority
(`LaneKick.authority`), and a lane's calls go back to the object that kicked
it whatever the deployment names now. The pool's durable used ledger is keyed
by the object's id, so the new authority still sees every earlier draw.

Succession is durable (`eval::succession`). Every earlier authority (the
original, then each name `EVAL_MAIN_PREVIOUS` lists, oldest first) is a
predecessor. Before a named authority takes its first plan or installs a
template, and before its calendar opens a cohort, each predecessor records it
as its successor (`/succession/supersede`, inside the predecessor's own
transaction), which a predecessor does only while all its cohorts are closed
or abandoned, it has no work left (`engine::blocking_work`: a closure, a
spend settlement, a purge of private results and summaries, or an admission
clean-up; refused `predecessor_work`, since the ticks stop kicking it once
succession is complete and work left then would never run; a due receipt
deletion does not count, since the successor's retention notices drive it
to the end), and none of its
labels has the new prefix or reaches the new first cohort. A predecessor already superseded answers with the successor it
recorded first, after the same checks against the asking one; the named
authority accepts that answer only when it names itself or an authority it
lists after that predecessor, which it then asks in turn
(`config::succession_reply`), so a deployment that leaves an earlier
authority out of `EVAL_MAIN_PREVIOUS` never completes succession. Each
answer is kept with the first cohort it was checked for, and a lower first
cohort asks again; `/eval/status` reports them (`succession_recorded`,
`succession_complete`) with `work` and `superseded_by`. Otherwise the named
authority refuses (409 `predecessor_busy`, `predecessor_work`,
`predecessor_numbers`, `predecessor_superseded_elsewhere`,
`predecessor_unknown`, `predecessor_unreadable`) and its calendar pauses with
that reason. A superseded authority admits no plan and opens no cohort again,
whatever a later deployment names (its calendar is paused at supersession,
and the queueing transaction itself reads the succession, so a plan admitted
just before a successor recorded it is refused too), so a deploy that
reverts the vars refuses plans instead of re-admitting the restarted chain's
numbers or labels. An authority the deployment no longer names is retired:
it admits no plan and its calendar opens nothing (so it is no reason for its
alarm to run), keeps its own names for any work it still holds, is kicked on
cron ticks while the deployment lists it as a predecessor and the named
authority's succession is incomplete (its kick reply reports
`succession_complete`; before that a cohort the predecessor still held at the
switch finishes and is never skipped, after it every predecessor holds
nothing), and is reachable at `/eval/authority/<name>/status` (operator or
monitor token, read only) and `/eval/authority/<name>/{clear,abandon}` (operator token),
for known names only (`config::Authority::known`), so no request creates an
object for any other name. The eval Worker's ChainJournal and the
coordinator's signing journal are untouched by the switch; the coordinator's
held target is restarted separately (`POST /signing/restart`, above), and a native
validator holding the earlier chain's target runs `reset-target` (validator
README). Runbook: TESTNET.md, "Chain-record restart". A restart installs a
fresh R_0 with no predecessor, so it belongs before the announcement: after
it, a change of reference is decision U2's single, pre-announced replacement
(the round-2 reference as `k = 1`, a successor of R_0 on the same record),
and any restart needs a new recorded decision from the user
(`slop_ninja_eval_ops restart --decision` names it).

**Scorer loads and the artifact spec.** A plan may name the artifact spec of
its cohort (`fanout.score.artifact_spec`, a spec id the scorer admits loads
by: spec v2 or v3, refused otherwise at queue time), with its digest frozen
in the plan (`fanout.score.artifact_spec_sha256`, or the admission block's
`artifact_spec_sha256`), which must be this build's digest of that spec.
The spec's bounds size the artifact (W3's spec-derived bound; a plan names
exactly one of `artifact_spec` and `artifact_max_mib`), and every scoring
lane's load names the frozen digest (`spec=<digest>` on `/v1/load`), read
from the plan, never rebuilt per lane report, so the scorer admits the
cohort's artifacts by it; spec v3 admits the span tower class. A plan that
names none sends none and is admitted by the scorer's unnamed-load spec
(spec v2), as before. The client sends the digest only when the scorer's
`/healthz` lists it; a scorer that does not fails the prepare as the
platform's (`scorer does not admit by the cohort's artifact spec`), never the
artifact's, and an instance bound under another spec is never reused. A plan
that carries `fanout.score.artifact_spec_sha256` binds the v2 limits record
(`slop_ninja_eval_limits_v2`, domain `slop_ninja.eval/limits/v2`: v1's fields
plus the spec's id and digest), published in `components.json` (`limits`),
and the publisher checks its digest against the manifest's. Every other plan
keeps the v1 record and its digest, unpublished, so a stored plan's record
never changes with the build (one that names its spec only in the admission
block bound v1 before that field existed). The operator probe (`POST
/eval/scorer-probe`) takes an optional `artifact_spec` (id) and loads by it
the same way, and an optional `"spans": true` that asks the scorer for each
probe document's sentences and spans (a package with a token head; cohort
lanes never ask). Admission runs (the admission lane's `/run`) still load by
spec v2 only (`admission::ADMISSION_SPECS`).

**Reward-v1 cohorts (plan v3).** `eval::v3` runs a `reward-v1` cohort through
the same phases, lanes and receipts, with every rule value a plan parameter
bound into the signed manifest (`reward_v1`: `sigma` and its basis, the
reward-intent lease, rewrite-source reuse `max_uses` and `min_gap`, the probe
versions in use, the gate policy, the canary policy, generation spares and
the output contract); none is compiled in. Differences from the v2 engine:

- **Draw.** Pure-human negatives are fresh and scored once. Each mixed child's
  parent is, in order, a same-cohort negative (one child per parent per
  cohort), an earlier passage with fewer than `max_uses` parent uses whose
  last use is at least `min_gap` cohorts earlier (oldest first), or fresh
  stock used only as a parent, never as a task. Same-cohort reuse counts
  toward the limit, and a reused parent avoids its earlier children's
  generators where the plan offers another. Two fresh canary audit draws and
  every probe version's documents join the cohort's documents. Mixed and
  model-only items carry spares; the seal keeps exactly each kind's planned
  count by private rank, so the manifest's mix is exact.
- **Tasks.** A plan with `reward_v1.task_order = "seeded_rank_v1"` seals
  every document, batch, probe and audit alike, in a second private rank
  keyed by the document (`pool::rank(seed, "task", key)`), independent of
  the selection rank, its kind and its set, so position predicts neither.
  The standing-template check requires it; a plan that names no order keeps
  the appended layout: the batch is tasks `[0, n)`, then the probe versions
  in version order, then audit draws A and B. Which tasks the references
  score still names the batch set to anyone who can read the reference
  receipts, the receipt index or the chunk maps; the seeded order hides the
  set only where those are private. Under either order the references
  score the batch tasks only, every executed entrant scores every task, and
  a chunk holds a run of its job's task list from `chunks.first_task`, a
  task index (a reference's run takes batch tasks only); the package reads
  every vector back into the appended layout, so measurements, probe
  fingerprints (in probe-record order) and the package are the same under
  both. An entrant whose inference content is already published is refused
  unexecuted.
- **Input screen** (seeded plans; `v3::INPUT_SCREEN_RULE`, named in the
  private batch index v3). Before anything is drawn, the draw refuses a
  pool whose export records no per-row token bound that stays inside the
  scorer's 2,048-token limit with `[CLS]` and `[SEP]` (`pool token bound`,
  deterministic: the cohort pauses as `step_refused`), and holds out of
  every role each pool row the scorer would refuse as empty (its own rule,
  `artifact-check` `text::scorer_is_empty`), with its source group; the
  cohort's ledger entry lists the held rows (`screened`). At seal an empty
  generated document gives way to a spare, and any other empty document
  (a probe-record defect) pauses the cohort as `step_refused`
  (`unscorable sealed document`) before anything is exposed; a clear
  repeats that refusal (TESTNET.md). Generated documents are counted
  under the scorer's own tokenizer in the Worker (every plan, see
  generation above) when they arrive and again before the seal; a
  generated document over the limit at seal gives way to a spare. A
  seeded seal also refuses a batch human over the limit (`unscorable
  sealed document`): a pool that broke its recorded bound. A cohort sealed
  by a build without the generated-document check can still end as
  `reference_failure` at packaging. A document that any job's scorer
  reply refused as over-length is left out of the strict false-positive
  count, since no artifact can answer it.
- **Package.** `reward_v1::finalize` with the cohort's measurements: the
  public-copy statistics on the batch and the probes, the share, attribution
  and loose false-positive gates on the entrant's own cohort documents, and
  the promotion audits (the strict tier on audit draw A, the canary, a
  re-audit on draw B after a first flag). Testnet has no canary anchors, so
  no canary statistic is computed; flags come only from the plan's drill list
  (reason `canary.drill`). The manifest (v4), package (v4), target, receipt
  index and published index are stored under `cohorts/<label>/` in the
  receipts bucket.
- **Closure.** At the closing boundary the cohort's inference contents and
  canonical fingerprints join the published index (`published-contents-after.bin`,
  `fingerprint-index-after.bin`), a promotion advances the reference chain
  `R_k`, `R_k-1`, and a signed `CohortClosure` (`closure.signed`) records the
  phase epochs and every entrant's opening; the target activates there.
- **Probe builds.** A probe plan draws, generates and seals a probe version
  like a batch, with each kind's count by largest remainder over the mix,
  scores the references on it, records their fingerprints and the private
  probe record, and closes with no package or target.

`/eval/status` reports the regime under `reward`: the rule, the active target
and whether it is the launch burn row, and the reference chain's contents.
`examples/reward_v1_verify.rs` verifies a stored `reward-v1` cohort as a
validator would (records, receipt chunks, published index, recomputed
outcome, closure and an authorization) and writes the active-target bundle
(`slop_ninja.reward_v1_bundle/v1`) in two forms: the validator's, with the
receipt index and chunks, and the coordinator's, without them (a Durable
Object value is bounded at 2 MB; the coordinator produced the receipts). It
also writes the validator's bundle schema v3 form
(`reward-v1-bundle-v3.json`), which names the receipt indexes and chunks in
DIR by absolute path, digest and length; it is the form for a cohort whose
embedded receipts exceed the native validator's 64 MiB file bound (the report
says whether the v2 form fits), and the files must stay in place on the
validator's host. The `launch` mode writes the launch burn bundle.

**Verification.** `examples/eval_verify.rs` checks a stored package from the
receipts bucket (an operator check of the coordinator's output; it writes no
validator bundle: validators commit the coordinator's signed row and refuse
the T12 fixture package). Pass the keys pinned in `testnet-trust.json`, never the
`keys.json` the Worker writes beside the package (that file is only compared
against the pin):

```sh
cargo run --manifest-path subnet/coordinator/Cargo.toml --example eval_verify -- \
  DIR PACKAGE_PUBLIC_KEY RECEIPT_PUBLIC_KEY --snapshot 0:OWNER_HOTKEY:REG --snapshot 3:MINER_HOTKEY:REG
```

**Rehearsal tooling.** `examples/slop_ninja_eval_ops.rs` writes load plans
(R_0 and copies owned by synthetic, never-registered identities, checked with
the coordinator's own parser), gates a deploy or a load run on both instances'
status (refused while a cohort generates, seals, scores or packages, and a
load run also while the main instance has a cohort queued or in admission),
and summarizes status documents, Worker tails and package components (whether
copies of R_k tie). It reads files only; the operator's shell fetches them.

```sh
cargo run --manifest-path subnet/coordinator/Cargo.toml --example slop_ninja_eval_ops -- \
  load-plan --label load-x --cohort 1010 --copies 2 --max-lanes 64 --budget-usd 2 \
  --source-label <sealed cohort> --source-index index.json --source-tasks 1000 --reference r0-artifact.json
cargo run ... -- gate --for load main-status.json load-status.json
```

The two October 30 rehearsals before the T12 signing change were signed with
the eval Worker's own keys (`earlier_eval_keys` in the trust file): they verify
only under those keys, and a configured validator refuses them.

**Testnet shortcuts.**

- **T13 (`separate_eval_worker`).** Evaluation runs as the separate Worker
  `slop-ninja-eval-testnet` (its own journal, webhook subscription and cron), so
  parallel coordinator deploys cannot interrupt a cohort. Removed when the
  coordinator Worker hosts `EvalCohort` itself. That move starts a new Durable
  Object namespace with an empty used set: keep the eval bucket's
  `pools/<pool>/used/` ledger (the draw reads it), and before deleting this
  Worker confirm every cohort's used rows appear there (`/eval/status`
  `pool_rows_used` against the ledger's rows).
- **`testnet_operator_roster`.** A plan without an `admission` block keeps
  the operator-declared roster (artifacts already in the eval bucket), for
  rehearsals. Coordinator-opened cohorts admit their rosters.
- **`testnet_plumbing_only` pool.** Dev-iter humans are exposed by design;
  scores test plumbing, not quality, and the pool holds 570 humans. The
  `reward-v1` rehearsal pool adds 4,593 public-candidate humans
  (`pool-public-candidate`: rows whose every experiment role was consumed
  Test, held or diagnostic), exposed in the same sense.

## Configuration and naming

`wrangler.toml` is tracked and holds no account ID; Wrangler resolves the
account from the operator's existing login. Deploy only the testnet environment:

```sh
WRANGLER_WRITE_LOGS=false wrangler deploy --env testnet \
  --tag testnet --message "slop-ninja coordinator testnet"
```

Every Cloudflare name carries the testnet marker:

- **Worker:** `slop-ninja-coordinator-testnet`, on workers.dev, with no preview
  URLs.
- **Top level:** a local-only shell named
  `slop-ninja-coordinator-local-testnet`, with no vars, bindings, triggers or
  workers.dev route. An accidental default deploy is still marked testnet and
  cannot overwrite the real Worker.
- **Durable Objects:** bindings `CHAIN_JOURNAL_TESTNET` and
  `EVAL_COHORT_TESTNET`, which the code derives from `NETWORK`. Namespaces are
  `slop-ninja-coordinator-testnet_ChainJournal` and `_EvalCohort`. The class
  names stay `ChainJournal` and `EvalCohort`, because Wrangler takes them from
  the exported Rust types.
- **R2 buckets** (private): `receipts-bucket-testnet` and
  `eval-bucket-testnet`, bound as `RECEIPTS_TESTNET` and `EVAL_TESTNET`. The
  coordinator writes only the liveness object to the evaluation bucket. Create
  them before the first deploy, and confirm that the public dev URL is
  disabled and that no custom domain exists.
- **Vars:** `NETWORK = "testnet"`, which the build requires, plus the testnet
  genesis, `SUBNET_NETUID = "601"`, the RPC URL and the watchdog settings.
- **Tags:** Wrangler 4.154 has no Worker-tags setting in its configuration
  schema. Deploys carry the version tag and message above instead.

Content logging (`observability`) is off.

Signing vars: `COORDINATOR_HOTKEY_SS58` (the public coordinator hotkey; the
secret must derive it), `PACKAGE_PUBLIC_KEY`, `RECEIPT_PUBLIC_KEY` and
`AUTHORIZATION_PUBLIC_KEY` (the public T12 trust roots, the same ones the native
validator configures; not chain keys), `WEIGHTS_VERSION_KEY` (default 0),
`SIGN_ERA_PERIOD` (a power of two, default 64), `SIGN_COMMIT_OFFSET`
(default 12) and `SIGN_RPC_CALLS_PER_MINUTE` (default and maximum: the shared
budget minus four).

Install these secrets with `wrangler secret put --env testnet <NAME>`, reading
each value from stdin:

| Secret | Value |
| --- | --- |
| `RTAO_WEBHOOK_SIGNING_SECRET` | the 64-hex `signing_secret` returned at registration |
| `RTAO_WEBHOOK_SUBSCRIPTION_ID` | the `0x` subscription ID |
| `COORDINATOR_OPERATOR_TOKEN` | at least 32 random bytes |
| `STATUS_TOKEN_MONITOR`, `STATUS_TOKEN_PUBLISHER`, `STATUS_TOKEN_ALERTS` | at least 32 random bytes each, one value per consumer and Worker (the status gate) |
| `COORDINATOR_STATUS_TOKEN` | the pre-split shared status token: delete it once the publisher and the relay present their own |
| `COORDINATOR_HOTKEY_SEED` | the coordinator hotkey's `0x` 32-byte sr25519 mini-secret (`secretSeed`), piped from the wallet file |

A secret change deploys a new Worker version. A Durable Object instance that
started before it can still see the old environment; on 2026-10-30 the first
signing step after the secret upload paused with `hotkey_secret_unavailable`,
and an operator `resume` a minute later signed.

The resource inventory, with account-specific identifiers and teardown commands,
is kept outside git.

**Testnet-only guards.**

- `testnet_only_genesis` (`config::TESTNET_ONLY_GENESIS`): the build refuses any
  `CHAIN_GENESIS` but testnet.
- `config::NETWORK`: the build refuses any `NETWORK` but `testnet`.
- The default RPC is the public OpenTensor testnet endpoint.

These are removed when the coordinator passes Finney qualification after
milestone M7.

## Not yet implemented

- **Arena: the fan-out lane objects.** `AScorer`, `EvalLane`, `GenLimiter`
  and `SpendLedger` (`src/cf_lanes.rs`) keep their worker-macros exports, so
  their handlers run with no scope current; their allocations spill out of
  the glue partition, counted (`glue_spills`), until they get hand-written
  exports like the other classes. The arena runs in `Observe` mode (step 1);
  `Enforce` waits for the step-0 measurements (the JS reserve through the
  inspector, the painted stack), the poisoned-pool recycle for its durable
  intent and stop rule, and the step-9 testnet load gate. Streaming
  packaging, seal and bundle verification (design steps 4 to 8), a
  per-role memory model (both Workers share one, so the coordinator
  Worker's quarantine budget is sized by the eval package step), a re-fit
  of the package and seal footprints on the reward-v1 steps, and witnesses
  for the continuation, cron, queue and signing routes are still to come.

- **Cohort control in the coordinator Worker.** The scheduling hook still has
  no due operations; testnet cohorts run in the eval Worker (T13), and the
  coordinator follows their signed records in the receipts bucket
  ([Automatic target handoff](#automatic-target-handoff)) rather than
  receiving them from a cohort object of its own.
- **Score publication to validators (M5/M6).** Validators fetch the current
  authorization and its cohort's records from the publisher's public layout
  (`[fetch]`), which the publisher copies from the receipts bucket. The
  public host is not deployed yet: until the user enables public access to
  the public testnet bucket, validators (the operator's included) install
  bundles by hand; a validator can read a local copy of the layout
  (`fetch.base_url = "file:///<dir>/"`, validator README), and installing
  one for the operator's validator awaits the user's decision (TESTNET.md,
  "Automatic target handoff").
- **Admission, open items.** The eval Worker decodes no uploaded object:
  the header check runs in the key-free upload Worker, through its service
  binding. Testnet enforces protocol bounds only (decision D6): no per-IP or
  per-hotkey rate limit, `rate_limited` is never sent; the upload Worker
  refuses unsigned and unregistered requests before any body read or
  forward, and serves the one unsigned route, the cohort read, from the
  cohort snapshot the cohort object writes to the eval bucket (which no
  presigning credential reaches; the snapshot also carries the gate's
  repair list)
  ([admission API](../../docs/testnet/admission-api.md), section 13). The
  R2 behaviours admission v2 relies on were qualified against R2 on
  2026-11-05, not by these tests: the signed checksum verified on a
  presigned PUT and a mismatch stored nowhere, create-only enforced, expiry
  checked when the request starts (so no upload rate is required), the
  stored checksum in the binding's head, unsigned standard headers stored as
  HTTP metadata (hence the attribute rule), multipart and version
  parameters refused with 501. The session, event and admission-step
  footprints (`cohort_session_bytes`, `ADMISSION_EVENT_BYTES`,
  `ADMISSION_STEP_BYTES`) are counted from the code; native arena witnesses
  run them at their bounds (`--features arena-witness`), not yet measured
  on wasm32. Publication at scoring start is the publisher's (a binding
  stream by default, a server-side `CopyObject` once probe G7-x qualifies
  it). The publisher reports each entrant's outcome to this Worker
  (`POST /eval/admission/published`, its binding and its own status token:
  the one non-read route that token opens, `access::StatusRoute::Publication`;
  never the monitor's): a recorded publication releases the admitted
  objects for deletion once the cohort closes, and a missing object starts
  the audit above. The report carries the cohort's label, which routes it
  to the authority that ran the cohort (`Authority::label_authority`); a
  cohort that authority does not hold answers 404 and stays pending at the
  publisher. Retained admitted objects count as work that holds a
  succession (`admission::objects_retained`).
  The outside-entrant rule compares exact content digests; a pre-draw
  similarity probe is still to come. A full 40-entrant template fits the
  coordinator's memory model and one signed receipt index; a native validator
  holds its target's receipts only through bundle schema v3 (by reference),
  so a validator whose build predates it refuses such a target's bundle
  (`bundle_too_large`) and keeps its old target. The span outputs are not
  served.
- **Fresh stock.** No cohort has had an admitted outside entrant, so every
  draw so far is from `EXPOSED_POOLS` (testnet pool v1 and the
  public-candidate refill), where a row may be drawn again (never-drawn rows
  first, never twice in one cohort, the rewrite-source reuse limit and gap
  kept); a plan naming any other pool is refused at queue time, and the draw
  faults (`fresh_stock_refused`) unless the frozen roster unlocks the plan's
  fresh pool.
- **Receipt retention, open points.** The N+2 rule is implemented
  ([Receipt retention](#receipt-retention-the-n2-rule)). A publisher that
  falls more than two completed cohorts behind cannot copy the deleted
  receipts of the cohorts it missed; it lists them as retired before
  publication (`retired_unpublished`), a gap in the public record. The
  authorization hold protects the target's cohort the current
  authorization names (not its carrier, above) from the coordinator's first write of the private
  pointer `authorizations/current.json` after the bundle's install (the
  next cron tick; `/status` `signing.authorization` shows the write). A
  deletion step in between reads the previous pointer, so a cohort
  authorization whose target's cohort is older than the two newest
  completions and no longer active is not held until that write is
  confirmed. A retention step that keeps failing shows
  in `/eval/status` and raises `retention_stalled` once a further cohort
  completes.
- **Published-index rebuilds.** Validators and the coordinator's signing path
  check the published index a bundle carries against the signed manifest
  binding (and that a carrier's index extends it), but do not rebuild it from
  every closed cohort's packages; they trust the coordinator's index.
- **Bundle and receipt bounds that grow.** A `reward-v1` bundle embeds the
  whole published index, which grows with every closed cohort. The
  coordinator stores a bundle in one Durable Object value (2,000,000 bytes),
  which the published index alone outgrows at about 5,000 published
  artifacts (about 130 full testnet cohorts); binding the index by digest and
  keeping it in R2 or chunked rows is still to do. The flat receipt index is
  bounded by what one signed record holds (972,591 entries, below the
  canonical sequence bound of 2^20), and plans are refused when a cohort's
  receipts could exceed it.
- **Authorization issue.** Each closed cohort's reward-intent authorization
  is signed by the coordinator itself
  ([Automatic target handoff](#automatic-target-handoff)). Launch
  authorizations, re-authorizations after a revocation, carried
  authorizations naming a later no-target cohort (waiting on the user's
  ruling on holding a carrier's receipts past N+2) and revocations stay
  operator actions with `reward_v1_verify`. None expires: the coordinator commits its installed
  target, whatever time has passed, until a newer bundle supersedes it or a
  signed revocation withdraws it ([Authorization without expiry](#authorization-without-expiry)).
  The coordinator checks each bundle against the target it holds active
  (`sign_held_target`, recorded from checked closures), so a replaced target
  or the launch bundle after the first activation is refused.
- **Native validators during the burn regime.** rtao holds a row whose
  positive entries are only the owner hotkey's (or the validator's own) UID
  unless the work opts in with `OwnerRowPolicy::Submit`. A native validator
  opts in for an owner burn row (the launch row under a valid launch
  authorization, or a verified target whose row pays only the owner UID) and
  commits it while the chain limits allow (`MinAllowedWeights` of 1 for a
  validator that is not the owner); it defers a tempo whose row rtao would
  still hold, or a burn row the limits refuse (exit 75), and its previous
  weights stay on chain. The coordinator commits burn rows itself.
- **Lookup canary.** No anchors exist on testnet; the canary statistic is not
  computed and flags come only from the plan's drill list. User decision
  D12b (2026-11-03): testnet promotions run without the lookup canary,
  labelled testnet-only in the guides, the terms and the leaderboard
  (`promotion_rule` in `v1/leaderboard.json`, a note on the page). The canary
  is required before mainnet.
- **Fan-out stage 2.** Job ledgers (one Durable Object per scored job) and a
  two-level receipt index are not implemented; the flat v1 index and one
  cohort object hold every receipt row, which fits the testnet rosters
  measured (TESTNET.md).
- **The 1-vCPU instance type and ENAM placement constraint.** Not qualified;
  scoring lanes use standard-4 with no placement constraint.
- **Signing limits** (see [Signing](#signing-m3)): mechanism 0 only; one RPC
  endpoint; `CheckMetadataHash` stays disabled, as for rtao's keyfile signer;
  a block's writer runtime is learned at its parent, which the journal must
  retain; the journal assumes exclusive use of the hotkey and faults on any
  foreign nonce use; no MEV shielding (not required for this path). The fake
  chain does not enforce mortality or check signatures, so the era-expiry path
  rests on the live drill (TESTNET.md). Every signing round claims and
  releases the claim, so its generation grows by about one per block.
- **Liveness alerting.** The liveness record and `/status` verdict detect a
  wedged ChainJournal, but nothing pages an operator yet, and a wedged
  instance still needs the redeploy in the runbook. A panic is recovered by
  workers-rs on the next request (qualified live); `state.abort()` is not. The
  instance is also constructed more often than deploys alone explain (see
  TESTNET.md); `/status` reports its age, requests and Wasm memory so the
  cause can be narrowed.
- **Pruning.** Deliveries, block events and the retained chain are small but
  kept indefinitely: about 7,200 rows each per day.
- **Launch qualification of lag.** One 15-minute primary window and one
  fallback drill were measured on 2026-10-30 ([TESTNET.md](TESTNET.md)); longer
  runs, provider outages and public RPC throttling of Workers egress remain
  unmeasured.

## Checks

These ran on `build-host` (Linux, Rust 1.99.1, worker-build 0.8.7, Wrangler
4.154.0), last on 2026-10-31, each as one Cargo job under the shared lock.
Run them from `subnet/coordinator`: Cargo reads the crate's
`.cargo/config.toml` (which sets `SLOP_NINJA_MEMORY_MODEL`) only from the
working directory, so `--manifest-path` from elsewhere builds against the
default memory model and fails the route-table assertions in
`arena_routes.rs`.

```sh
cd subnet/coordinator
cargo test
cargo test --features arena-witness arena_witness -- --test-threads=1
cargo clippy --all-targets -- -D warnings
cargo clippy --release --target wasm32-unknown-unknown -- -D warnings
(cd parity && cargo test --release)
worker-build --release
```

**On main, 2026-10-31,** after the M3 signing, M4 evaluation and fan-out
branches were merged and the liveness record was keyed by deployment role:
164 native tests passed, the 4 parity tests passed, and Clippy passed with
warnings denied natively and for `wasm32-unknown-unknown`. The four tests
added with the role key cover the `EVAL_ROLE` parse, the two keys, a
simulated wedge and stopped cron on the coordinator while the evaluation
Worker keeps answering, and a source check that the Worker reads and writes
the record only under its own role.
The crate denies `unwrap`, `expect`, `panic!` and unchecked indexing outside
tests (M3); the hidden-evaluation modules, their Durable Objects and their
routes are exempt until converted. The branch records follow.

**Native, M3 branch: 98 tests passed.** Beyond the heartbeat suite below:

- `chain` (10): the commit call and both signing payloads equal rtao's golden
  bytes; the signed extrinsic verifies and decodes back to the call and nonce;
  storage keys and defaults by name; the mortal era; the payload is upstream's
  SCALE and decrypts with a published quicknet round signature; rtao's round
  vector; epoch arithmetic; the runtime API metadata wrapper; seed errors carry
  fixed codes only.
- `signing` (27), against a fake chain whose every storage value, block body
  and event list is encoded by name from the runtime that executed that
  block. Runtimes are per block: an upgrade enacted in block `E` puts the new
  code in `E`'s post-state, so the version and metadata reported at `E` are
  the new runtime's, while `E`'s storage, events and `LastRuntimeUpgrade` are
  the old one's. The second runtime is the recorded metadata with
  `frame_system`'s `ExtrinsicSuccess` and `ExtrinsicFailed` indices swapped,
  so a block decoded with the wrong runtime visibly fails. Nothing is
  read or signed while paused; one commit per epoch with read-back, the reveal
  observed at the predicted block with matching weights, and the next epoch's
  own operation and nonce; an uncertain reply reconciled without resending;
  eviction after sending recovered as uncertain, never resent; a lost
  submission waits for era expiry before a second attempt with the same nonce;
  a stale holder's mark and outcome are fenced; duplicate wakeups take one
  claim; a late drand pulse is found by the queue search and a commit dropped
  without a reveal event is reported; an existing chain commit refuses a second one; a disabled flag pauses
  without fallback; a pool rejection is answered by one fresh attempt from the
  next preflight's chain state and runtime (same nonce), which commits within
  the epoch when the runtime upgraded between signing and submission, and a
  second rejection fails the epoch; a stored non-native
  tag is followed under 486 and the real 487 metadata; an unexplained drop
  under a followed tag is reported and the next epoch commits with the same
  tag, the report clearing once a commitment reveals; a native-tag drop
  defers nothing; foreign
  nonce use is a fault that needs a clear; per-epoch identities; content-free
  status. Added with the failure accounting: an upgrade enacted in the
  inclusion block, which the nonce search also probes as a midpoint, is
  decoded with its writer, and nothing is ever asked of that block's own
  version or metadata (the runtime is learned once, at a parent); an upgrade
  enacted in the reveal block, likewise; an upgrade during reconciliation
  fetches each runtime's metadata once and the next epoch signs with the new
  runtime; undecodable inclusion events settle after five reads into
  `events_unverifiable` with the nonce floor advanced and the next epoch
  committing; a reveal search that cannot finish (unretained blocks) never
  starves the next commit and settles as `reveal_unverifiable`; an RPC error
  (429) ends the round and backs off; unparsable metadata is refetched at most
  six times in 40 blocks; signing's RPC share leaves the heartbeat four calls;
  reveal checks and probes are logged; a reveal search cut short by a
  three-step round continues from its stored bounds with no abandoned steps;
  v4 storage upgrades with its signing journal intact.
- `liveness` (1): consecutive failures, verdicts and staleness of the
  ChainJournal's external liveness record.
- `target`, `rpc` and `config` additions: bundle and trust-root refusals,
  block-pinned reads, submission reply classification, signing configuration.

**Parity: 4 tests passed** against rtao `b5553a76` natively (see
[Wasm path](#wasm-path)), again against `0c128160` on 2026-11-03, and
against `fa83052d` on 2026-11-05.

**Wasm.** Clippy passed on both targets, with the panic lints denied.
`worker-build` produced `index.js` 31.5 kB and `index_bg.wasm` 2,039,481 bytes
(664.2 KiB gzip as uploaded, against the 10 MB compressed Workers Paid limit).
Startup time is 7 ms.

**Native, fan-out branch: 116 tests passed** (2026-10-31, after the fan-out review fixes; 105
for the fan-out lanes, 83 after the M4 review fixes, 54 before M4). The evaluation tests run whole cohorts against
SQLite through a simulated lane runtime (a queue carrying content-free kicks,
lane objects that keep the larger `(seq, gen)`, alarm loops with per-run
claim budgets, and both buckets), with the real engine:

- a cohort end to end through generation lanes (a rejected output retried on
  the next generator), the seal, scoring lanes, summary and bundle lanes and
  packaging, every bundled receipt verified against the index and the
  package, the burn row for copies of R_k, closing at opened + K with its
  spend settled once, and a second cohort that reuses no human;
- failure semantics: lease expiry reconciled with no compute when the expired
  attempt's objects are durable and recorded lost (attempt 2 resolving) when
  they are not; a crashed lane re-kicked once stale, its old generation
  fenced; a container crash or OOM isolating the chunk, with a crash that
  kills a proven fresh instance a second time a model error for that job's
  document only and a transient crash never attributed; an isolation run cut
  short by its step budget handed back and continued under the same attempt;
  an entrant that crashes on everything, or never loads, becoming model errors
  after the declared failures while a reference that never loads pauses; an
  R2 read outage before dispatch pausing with no attempt spent and the same
  cohort resuming; reconciliation read errors and a missing receipt object
  never losing a durable answer; a failure with the container up never
  isolating; an isolation restart that fails spending no attempt; duplicate kicks and
  every fourth kick lost, with one receipt per task; the re-kick livelock
  regression (a lane reporting through a 60 s cold path never re-kicked by
  10 s sweeps, a silent one re-kicked after 30 s); coordinator silence pausing
  lanes at their next claim with the same cohort resuming, and the load
  instance's hold doing the same;
- generation: throttles using no attempt and keeping the generator, every
  reservation released; concurrent claims never reserving past the budget; a
  late result from an expired claim fenced with its usage recorded;
- a job that spends every attempt faulting alone while the others finish,
  then the cohort pausing; the scoring budget faulting the cohort once
  measured spend would exceed it, and a plan whose budget is below its own
  upper bound refused; requests that never reached the model returning their
  attempt numbers until a pause and a clear; a generation outage pausing with
  no case dropped; batch and draw shortfalls within or beyond the tolerance;
  the durable used ledger; phase minimums ignoring silent tempos; the pre-start
  kicking every scoring lane once during the generation tail, holding no
  claim; load mode re-scoring a sealed batch with no target; and the v3 store
  migrating to v7 in place;
- the lane planner at its edges (J above the cap, C not divisible by W), the
  cost bounds, the token buckets (no 60 s window above rate plus burst; AIMD),
  billing cycles and the monthly ledger (idempotent settles, repeated
  reservations reported, open reservations across a cycle boundary), chunk builds
  identical in lane and cohort, bundle verification, shard and claim digests,
  and the isolation procedure (reproduction on a proven fresh instance, a
  one-off failure, a control that fails, a restart that fails, the step
  budget and a continuation);
- R_{k-1} scored but never credited;
- twox128 and XXH64 vectors and the epoch reply bound to its block;
- sentence splitting, span selection, splicing and word share, and output
  validation; plan validation and the shortfall tolerance; the draw,
  proportional quotas and the used ledger (v1 entries without an authority
  always count; only an authority's own write-ahead entry is redrawn); outcome
  mapping, components and paired SE; chunk encoding and verification; the v0
  policy, signing and the row.

Webhook and configuration:

- HMAC framing against an independently computed vector, field and skew
  enforcement, and body validation
- the testnet genesis and network guards, settings bounds, RPC URL rules and
  bearer-token comparison

Journal:

- migrations v1→v6 and v2→v6 in place (the watermark seeded at the anchor),
  and rollback of a failed upgrade
- webhook blocks staying provisional until a head audit, an audit point or an
  RPC list at or below the RPC head confirms them; a contradicting audit never
  confirming; and a re-anchor dropping the watermark
- dedup by delivery ID and by (stream, block hash), and rollback of a failed
  ingest
- parent-linked advance, gap detection and hash-list backfill
- durable parent-mismatch, hash-conflict, RPC-junction and RPC-head faults that
  pause until cleared or re-anchored
- RPC head audits

Heartbeat, against a fake chain:

- no RPC call for a delivery without a gap
- bounded gap rounds with carry-over
- overlapping invocations skipping a held claim, and stale holders fenced
- silence entering fallback and ticks walking to the head, then a return to
  primary with redelivered blocks deduplicated
- per-tick bounds, behind-with-live-webhooks fallback and `caught_up`
- one head read per tick, and the shared budget
- pause on a fault, one scheduling step per new delivery, and content-free status
- scheduling held at the watermark until the tick's head read confirms the
  delivered blocks (no extra RPC), and fallback blocks confirmed as they apply

RPC parsing uses the response shapes recorded from the public testnet endpoint.

**Wasm, fan-out branch.** Clippy passed on both targets (2026-10-31). The
`no_runtime_gates` lint
(`cargo test --manifest-path subnet/Cargo.toml --test no_runtime_gates`)
passed.

The 2026-10-29 local-runtime runs (`wrangler dev --env testnet --local`) of the
heartbeat are in git history; the M3 signing path was exercised live on
testnet instead ([TESTNET.md](TESTNET.md)).

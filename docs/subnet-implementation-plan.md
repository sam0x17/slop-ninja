# Subnet implementation plan

November 9, 2026. This plan implements the [Cloudflare design](cloudflare-subnet-draft.md)
and [whitepaper draft 0.62](../whitepaper/main.tex). It describes work to build and qualify;
it does not establish a deployed subnet. Launch includes A alone. B and C have
separate activation requirements. A reads raw text; the native grammar serves
later B and C. The shared protocol crate implements the `reward-v2`
calculation (the `reward-v1` band rule on `quant-v1` quantized scores), the
copy policy, private submission records, the launch and reward-intent
authorizations, the cohort phase checks and the coordinator's signed row. On
testnet the coordinator's evaluation Worker runs `reward-v2` cohorts with
them, admitting entrants from their on-chain commitments; the coordinator
signs each target's row, and the native validator verifies that row and
commits it.

The first milestone is a Rust Workers coordinator that can durably sign, submit,
and recover a weight commitment through restart and uncertain RPC outcomes.
Model qualification and shared protocol work can proceed alongside that milestone.
The complete first release adds hosted A evaluation, native participants,
recoverable updates, and a qualified cohort lifecycle. Paid service opens after
its own quality, funding, and payment checks.

**Implementation commitments**

- The coordinator is the lead validator. Its application logic, signing, chain
  reconciliation, and scheduling run serverlessly in Rust on Cloudflare Workers
  and Durable Objects. All production challenge generation, inference, and
  scoring also run on qualified serverless compute.
- Native miners and other validators use Rust and rtao. Shared protocol logic
  supports native execution and Wasm; platform bindings remain separate.
- Coordinator silence preserves accepted work and remaining active-time budgets.
  Slow work and missed wakeups move advancement to a later eligible tempo
  boundary. Customer obligations and native chain deadlines keep their own clocks.
- Other validators' availability, updates, acknowledgments, and delivered rows
  never gate application progress. Their timely chain participation still affects
  native consensus and payouts. No coordinator can pause the chain's emissions.
- A miners submit text model artifacts (ONNX graph, fixed tokenizer, heads,
  calibration) within the artifact cap: an allow-listed ModernBERT-base graph,
  with or without the token span head, at most 160M parameters, the
  allow-listed tokenizer byte for byte, and schema-validated calibration and
  contract files (no separate byte cap). A miner commits only the digest on chain
  and uploads the artifact privately; the coordinator publishes it after the
  cohort ends, licensed CC BY-SA 4.0. SlopNinja pays
  hosted evaluation and serving costs. Training remains the miner's expense.
  Miners receive no customer text or hidden evaluation data. Project code and
  documentation are MIT OR Apache-2.0, except files and datasets with their
  own licence or notice. SlopNinja's own references (`R_0` and any later
  reference it trains) are published under CC BY-SA 4.0 like any `R_k`; a
  public reference package uses synthetic replay cases, carries no evaluation
  text, and passes a package-corpus scan. Paid A serves a published, qualified
  artifact (`R_0` once its public package is published).
- Rewards follow `reward-v1`: a reference model that changes only on
  improvement (on testnet, `R_0` on a fresh chain record, replaceable once,
  announced in advance, by the next qualified reference; mainnet opens a fresh
  chain record with the reference testnet then holds), SN78's geometric
  band mechanism with frozen signed parameters, no credit for exact or near
  copies of published artifacts, equal splits
  within near-duplicate clusters, a 15% fallback cutoff against
  the previous reference, and burns for unregistered credit and for cohorts
  with no new target before any target exists. Under the documented emission
  rule a burn shrinks the subnet's TAO-side emission share by the burned
  fraction of miner incentive, to zero for a full burn.
- Cohorts last at least 20 tempos (about one day) and are paid with a lag:
  validators re-commit the active target's signed row every tempo under a
  reward-intent authorization that never expires; only a newer target or a
  signed revocation ends it. No record, identity, or gate carries a chain
  runtime version.
- Updates preserve wallets, journals, pending operations, and frozen evaluation
  semantics. A machine missing several releases runs the required migrations
  sequentially and reconciles external effects before resuming signing.
- Workers code allocates only inside a bounded memory arena: fixed Wasm linear
  memory sized from measured peaks, declared lengths checked against the
  budget before any allocation (the Workers admission path always passes its
  budget to the artifact checker), per-request totals for in-flight sizes,
  streamed objects, and no re-reading of inputs already applied.

**rtao improvements are part of implementation**

I will fix or improve rtao as integration exposes defects or missing capabilities,
including the related rtao-rpc service where needed. Upstream gaps belong in the
implementation work; recording them does not complete the affected milestone.
Application scoring, authorization, and reward policy remain in SlopNinja.

For each issue:

1. Reproduce it at an exact revision and add the smallest meaningful regression
   fixture. Distinguish an SDK defect from incorrect application use or an
   unavailable provider/runtime capability.
2. Implement reusable transport, journal, scheduler, encoding, recovery, or
   compatibility changes in rtao. Implement service delivery changes in
   rtao-rpc. Preserve native behavior while adding the Workers path.
3. Test the upstream change and exercise it through SlopNinja. A passing SDK
   fixture alone does not qualify application recovery or current chain semantics.
4. Prepare the upstream fix for review and integrate an exact tested revision.
   Keep required core patches and lockfiles aligned. Record remaining qualification
   limits. Coordinate with ongoing rtao work and preserve unrelated changes.

Use isolated branches/checkouts for upstream work. Avoid a permanent private copy
of the SDK submission engine. Preserve safety refusals until their underlying
requirements are satisfied; never disable journaling to make a transaction pass.
Some issues require provider changes or additional evidence. In those cases,
preserve the affected state, state the concrete blocker, and continue independent
work without claiming the feature is ready.

The [local adapter](../subnet/README.md) pins rtao
[`fa83052d`](https://github.com/sam0x17/rtao/commit/fa83052d37b39f002f7a4d3cc5c04f25503e1d11)
(immutable tag `snapshot-2026-11-05.2`, November 5), with runtime 486 metadata
and schema-3 journal migration. At that pin rtao composes weight calls and reads
its validator storage by name when its compiled descriptors no longer match,
follows the stored commit-reveal tag under any runtime code (after an
unexplained missed reveal it commits at most once per epoch instead of
pausing), lets work opt in to committing an owner burn row
(`OwnerRowPolicy::Submit`), and decides that guard from the UIDs at the
dispatch block, reporting held queued work as `OwnerRowHeld` for the
application to retire (`retire_pending`). Journaled MEV-shield dispatch (supported upstream
from per-runtime evidence) and the v482 port are not qualified here, and rtao
declares no validated runtime range. Ordinary weight commit/reveal ran live on
testnet subnet 601 at `b5553a76` (October 30), and a native validator
committed and revealed the burn row there at `0c128160` on November 3 and
has run at `fa83052d` since November 7. A later
re-pin, such as one adopting rtao's bounded-memory, zero-copy rework, re-runs
the subnet tests, the journal migration check and the `no_runtime_gates` lint.
The Workers journal/transport composition remains implementation work. The
provider's unbilled block webhooks are live: a Finney pilot at
`https://finney.rtao.io/webhooks` and a separate testnet service with its own
management password, subscriptions, and signing keys. Receiver integration and
SlopNinja qualification remain our work. On the authorized compute host,
follow the operator's private Finney and testnet webhook instruction files for
each service's current contract and private credential location. Provider
checks of the deployed services do not qualify our receiver. Recheck upstream before each
registration.

**Work sequence and acceptance criteria**

1. **Establish the integration baseline and shared Rust contracts.**

   Inventory the existing subnet adapter, the parser-free A crates
   (`grammar-a-baseline` and related), evaluation code, and receipt prototypes.
   Reuse tested components. The A launch path has no native-parser dependency.
   Keep the grammar workspace independent of the subnet workspace; its native
   parser serves later B.
   Record upstream revisions and available RPC methods, authentication, history
   coverage, and submission behavior. Resolve the observed RPC HTTP 403 before
   relying on that endpoint; service fees remain budgeted at zero.

   Organize `subnet/` into shared protocol/reward code, the native chain adapter,
   a Workers chain adapter, coordinator bindings, miner and validator applications,
   and an update supervisor. These are proposed boundaries; create crates as
   their implementations become useful. Reuse a common evaluation library for
   admitted model execution where native and Wasm behavior is qualified.

   Define versioned cohort, artifact, assignment, attempt, result, score-package,
   receipt, calculation-target, and release records. The cohort manifest records
   `K`, the phase minimums, the opening epoch index, batch size, strata and mix,
   the artifact cap and compute limits, the human pool manifest with its share
   caps and sampling-key commitment, the reference models, the
   published-artifact index and fingerprint probe versions, and the `reward-v1`
   parameters (`delta`, `w`, `epsilon`, `r`, the near-copy distance `d` with its
   `w/2` clause, and the 15% fallback). Miner score records carry the content
   digest, commitment block and extrinsic index, failed gates with reason codes,
   public-copy statistics, and cluster assignment.
   Calculation targets carry the sealing block and activation epoch. Weight
   operation identities include the tempo's epoch index, so per-tempo re-commits
   of one target are distinct operations. Specify domain-separated
   signatures, canonical serialization, stable operation identities, registration
   bindings, and key rotation. Keep coordinator authorization, receipt, and
   validator signing keys distinct. Define storage versions and migration entry
   points before persisting the first production record.

   Fix deterministic reward arithmetic, ordering, normalization, rounding, and
   native weight encoding. Evaluation may use qualified numeric tolerances;
   validators must produce byte-identical canonical rows from the same package.
   Specify zero-pool, perfect-baseline, overflow, and UID-reuse behavior before
   live weights. Resolve remaining reward parameters through frozen configuration.
   Define cohort timing, authorization freshness, retry limits, and correction
   rules before accepting real evaluation work under those rules.

   Done when shared fixtures run consistently on supported native targets and
   Wasm, malformed or mismatched signed records are rejected, and the dependency
   graph and initial schemas are documented, and `reward-v1` golden fixtures cover
   promotion, the fallback cutoff, cohorts with no new target, the public-copy
   gate and fingerprint index, clusters, the inclusive near-equal drop, bands,
   largest-remainder ties, burns, deferred burn rows, the launch authorization,
   renewals, the per-tempo active-target check, and reference failures. These
   are in place in the protocol crate; signed corrections and the commitment
   call's chain encoding remain.

2. **Build the serverless validator transaction path and repair rtao as needed.**

   Qualify Workers-compatible RPC, call encoding, hotkey signing, and current
   runtime checks. Refactor reusable rtao logic away from unconditional native
   dependencies where necessary. Implement a Durable Object journal adapter with
   durable ownership, generation checks, and exclusive nonce/signing authority.
   If the synchronous store interface cannot preserve those guarantees in Workers,
   improve the upstream interface rather than disguising asynchronous persistence.

   Persist logical intent and signed attempts before author submission. Treat an
   RPC timeout as uncertainty, then reconcile finalized execution, nonce state,
   pending commitments, and reveal history. Recover before allowing new signing;
   a bounded recovery call may require continuation. Retain original operation
   identities and reveal material through runtime changes and new score targets.
   Fence stale instances so a delayed invocation cannot regain signing ownership.

   Use finalized state for recovery and cohort observations. Refresh the current
   submission context before signing, including live-metadata call resolution, registration,
   rate limits, current authorization, and the enabled commit/reveal setting.
   Establish the owner-authorized activation path and check the setting at startup
   as well. An unexpectedly disabled flag pauses new
   commitments without falling back to direct submission. No spec or transaction
   version gates anything: an operation with no type-correct encoding under live
   metadata is deferred alone and re-evaluated every block, while every other
   operation continues. Native timelock reveal continues on chain during
   application pauses.

   Update all rtao dependency pins, patches, and lockfiles together, with a
   journal-migration fixture. Keep ordinary commit/reveal distinct from MEV
   shielding. If the selected chain path requires shielding, complete its upstream
   recovery/review requirements and qualify it before using that path.

   Done when a Rust Worker/DO can submit an ordinary weight commitment on the
   qualified test chain, survive eviction before and after submission, discover
   finalized inclusion and reveal, and avoid duplicate effects after an uncertain
   response. Exercise a runtime transition (work continues; only an operation
   without a type-correct encoding defers) and stale concurrent invocation.
   Record which transport and storage combinations passed and the runtimes
   observed, as observations rather than an accepted list. Testnet signing on
   subnet 601 is authorized. The native validator's live commitments,
   per-tempo re-commitments and fault drills there passed on October 30, 2026
   under a fixture package. The coordinator's own commit/reveal has since run
   live there through the Workers chain path; qualifying it remains. Finney
   signing needs separate approval, and registering hotkeys needs the owner's
   coldkey.

3. **Implement durable cohort control and block-driven recovery.**

   Store cohort phase, frozen manifest, results, remaining active-time budgets,
   and publication progress in authoritative Durable Objects. Separate per-subnet
   phase authority, per-hotkey signing ownership, jobs, and accounting so heavy
   inference does not serialize through one global object. Keep each balance and
   its reservations within one transactional owner. Persist dispatch intents with
   state changes and use idempotent messages between objects.

   Integrate the live rtao-rpc webhooks using the procedure below, on testnet
   first. The finalized stream delivers every finalized block, and that
   per-block delivery is the coordinator's primary heartbeat and clock; each
   delivery is its own Worker invocation. Deduplicate deliveries and verify
   observations through RPC using a finalized height/hash cursor; notifications
   grant neither finality nor authority. A `header_only` event does not establish
   that runtime/state reads at that hash are available. Backfill height gaps
   through bounded RPC, persist pending catch-up, and retry unavailable reads
   without advancing the cursor. After the cursor advances, claim due work
   through a durable lease with an idempotent operation ID, holder, and expiry;
   overlapping invocations skip claimed work, later blocks re-claim stale claims,
   and work too long for one invocation continues in resumable steps.

   A one-minute cron watchdog is the only timer and a no-op while valid
   deliveries arrive. After five minutes (configurable) without one, or when the
   cursor lags the public OpenTensor finalized head by that threshold (checked at
   most once per tick), the coordinator enters fallback mode. Each tick then
   processes finalized blocks from the cursor toward that head through the same
   per-block path, bounded per tick and within public rate limits, carrying the
   remainder to the next tick. It returns to primary mode automatically when
   deliveries resume; dedup and the cursor make the overlap safe. Record mode
   transitions durably and expose the mode and lag on the status endpoint.
   Measure actual observation delay including provider polling, the watchdog
   threshold, scheduler delay, and RPC latency.

   Implement the admission, generation, scoring, publication, transition, and
   paused states with durable generation checks and per-phase boundary counts.
   Detect boundaries as finalized increments of the epoch index. Transition
   issues no new challenges and lasts at least two tempos; the score package is
   published before it and the artifacts and openings at its closing boundary.
   A cohort lasts at least `K = 20` tempos (`K = 4` in rehearsal manifests).
   Advance only after required application work and phase minima are complete at an eligible tempo
   boundary. Record one weight intent per epoch for the active target. Lease expiry stops new authorized work; recovery does not debit the
   silent interval or synthesize missed cohorts. Preserve late evidence from an
   already recorded attempt while preventing a stale claimant from dispatching.

   Run bulk evaluation work as fan-out jobs: the cohort object records each
   lane's assignment and holds the task leases; each lane runs in its own
   Durable Object invocation and claims and commits through short, idempotent
   requests; each scoring lane drives its own Container instance; generation
   lanes share one Workers AI limiter; receipts and packaging summaries also
   fan out. Containers start on demand, with just-in-time pre-start during the
   generation tail and nothing warm at rest. Queue messages carry content-free
   lane wakeups, not documents or credentials. Reconstruct pending dispatch from
   stored tasks after queue retention expires. Reclaiming a
   task does not itself spend a compute retry. Persist outcomes before acknowledging
   delivery, and select retry results by the frozen attempt-order rule.

   Done when missed/duplicate/reordered webhooks, a delivery stall that enters and
   leaves fallback mode, queue loss, delayed work, expired claims, and deployment
   interruptions preserve the same cohort. Rehearse a
   long pause and restore from durable exports. No incomplete cohort or its needed
   evidence expires through a time-based storage rule.

   Webhook integration procedure, to execute when implementation begins:

   - Reread the instructions for the target network on the authorized compute host.
     For Finney, use `https://finney.rtao.io/webhooks` as the management API base;
     the testnet instructions name that service's base. Never reuse a management
     password, subscription, or signing key across networks. Start with
     `streams: "finalized"`; best/both streams require separately qualified reorg
     handling. Prepare a public HTTPS receiver on port 443 without redirects.
     Keep the management password in the private file identified by the
     instructions. Prefer management calls from that host; if transfer is needed,
     use encrypted transport and restricted storage. The receiver needs only its
     own subscription signing key, never the management password.
   - Register with `POST /v1/subscriptions`, name `<subscription name>`, and the actual
     receiver URL. Registration starts paused and identical requests are
     idempotent. Store the returned subscription ID, `key_id`, and signing secret
     privately. Install the 32-byte hex-decoded key as a receiver secret; exclude
     it and the registration response from source, logs, and callback payloads.
   - Authenticate the exact raw request body using `X-Rtao-Signature` and the
     instructions' HMAC-SHA256 v1 framing. Enforce signature version, expected
     subscription/key IDs, at most 300 seconds of timestamp skew, and a
     constant-time MAC comparison. Preserve decimal-string u64 values without
     floating-point conversion. After authentication, check the event body's
     `event_id` against the signed context and validate chain and event schema.
   - Handle the authenticated challenge separately from block delivery. Verify
     its subscription and return the nonce in `X-Rtao-Challenge-Response` with a
     2xx status; enqueue no block work. Once the receiver and key are installed,
     call `POST /v1/subscriptions/<subscription name>/verify` to activate delivery.
   - In one Durable Object database transaction, persist the delivery ID and
     exact-body digest together with the durable application task/outbox record.
     Return 2xx within the provider's five-second timeout only after that commit;
     continue bounded due work after the response within the Workers
     post-response limit, so slow work cannot cause a retry or pause.
     Dispatch queue notifications afterward and reconstruct them from the outbox
     after interruption. A repeated ID with identical body bytes receives 2xx
     without another task; reuse with different bytes is rejected. Retry signature
     timestamps and attempt numbers may change without creating a new delivery.
   - Monitor delivery and paused-subscription status independently of callbacks.
     The provider retries any non-2xx delivery with backoff from 2 s to 2 h, ten
     times over about 5 h 49 min, then pauses the subscription with pending work
     preserved and the last undelivered event recorded; nothing is delivered
     until an authenticated resume. The watchdog then carries the coordinator and
     reports the fallback state and the time since the last valid delivery for an
     operator alert. Repair the receiver and resume through the authenticated
     endpoint (from the last undelivered event; the finalized cursor
     deduplicates). Automatic resumption is not implemented: it needs a
     subscription-management credential in the Worker (a user decision) or an
     rtao-rpc resume path. Until then a pause lasts until an operator acts. The
     Finney source and delivery polls each run every three seconds; the testnet source polls every
     six seconds. Callbacks are therefore not an immediate or complete source of
     chain observations. Keep RPC gap backfill and
     the cron watchdog's public-RPC fallback operational while delivery is paused.
   - Treat the provider's seven-day journal retention as a transport limit.
     SlopNinja retains its own unfinished cohort and finalized cursor. On a
     reported retention gap, reconcile the application through RPC before
     deregistering, registering a fresh subscription, installing its new key,
     and verifying again. URL or stream changes also require that registration
     sequence. `DELETE /v1/subscriptions/<subscription name>` stops future attempts but
     cannot recall a callback already in flight. Fence retired subscription IDs
     without discarding their already accepted durable work.

   Qualify signed challenges, actual finalized deliveries, tampering, wrong
   subscription/key IDs, stale timestamps, integer boundaries, lost acknowledgments,
   conflicting duplicate IDs, persistence failure, receiver redeployment, provider
   pause/resume, retention gaps, and deregistration with an in-flight callback.
   Provider examples and schema are available at the private paths in the
   instructions; inspect them locally as needed without publishing provider source.
   Record registration and receiver evidence separately from provider-reported
   tests. No registration or deployment is part of this planning update.

4. **Implement A admission, cohort batches, and hosted evaluation.**

   The input contract is raw text, and the seed-17 document model of the
   baseline release candidate is the first reference `R_0`. Testnet opens a fresh chain record with `R_0` and
   no predecessor; it may replace `R_0` once, announced in advance on the status
   page, by the next reference SlopNinja trains (retrained and recalibrated on
   normalised, length-matched positives), only if that model passes every
   required gate. The first span-serving package takes `R_0`'s position only
   through the signed span-equivalent reference decision before the first
   cohort that enforces the span gate; that decision is not the replacement,
   so the replacement stays available. Re-export `R_0` with synthetic replay cases, run the
   package-corpus scan, and publish it under CC BY-SA 4.0. Admit text
   artifacts (ONNX graph, fixed tokenizer, heads, calibration) with supported
   operators, never arbitrary miner plugins. A
   returns document-level share and confidence in any AI involvement, plus
   conditional human/model attribution; both attribution roles use named
   matches or their Other human/Other AI fallback, and no unknown label is
   introduced. Validate probability consistency, including the model-only lower
   bound and involvement upper bound on expected share.

   Implement private submission: an on-chain digest commitment through the
   chain's commitment call, a private upload of the artifact and opening into
   the coordinator's private evaluation bucket (encrypted to the coordinator key
   where qualified), digest checks, and publication of every artifact and opening
   in the package after the cohort closes. Enforce the manifest's graph allow-list,
   parameter limit, tokenizer match, file schemas and compute limits. Enforce isolation from network access, online
   learning, undeclared mutable state, and cross-job model state. A miner needs
   no Cloudflare account or delegated inference credential.

   Build each cohort's batch serverlessly under a per-cohort budget: about 1,000
   paired cases on testnet and 4,000 on mainnet (floor 2,000), 20/55/25
   `human_only`/`mixed`/`model_only`, self-weighting by domain × origin, whole
   source groups, and fresh AI-side text from Workers AI or hosted APIs. Score
   each passage of fresh stock as a pure-human negative at most once; it may
   also be a rewrite source, in its own cohort or later ones, at most four times
   with uses at least seven cohorts apart, with a fresh generator, seed, voice
   and operation each time, recorded per use in the rewrite ledger; the signed
   manifest binds the use limit, the gap (counted in cohorts that drew human
   text) and the voice/operation set. Qualify batch invariance: a document scored alone or
   within its batch gives identical outputs.
   Mainnet draws human text from pre-cutoff Common Crawl under frozen quarterly
   pool manifests, a secret ranking key, domain rules v3, the share table
   `cc_launch_shares_v5` (essays 51.25%, news 26.5%, email 0.688%), the cohort
   generation profile `cc_cohort_profile_v1` (parent-conditioned normaliser,
   form screen and a 150–450-word window on every class) and the gate
   definitions `cc_era_gate_defs_v2` (era-only and layout/typography
   adversaries ≤ 5% of headroom; character-class features and probe AUC
   reported only). Record measured domain precision as a limit and size pools
   by it. Re-measure the era gate before mainnet live rewards on normalised,
   length-matched positives, with an upper bound below 5%. Testnet uses the
   public stock only, and a testnet cohort whose roster has no admitted outside
   entrant with a working artifact draws only already-exposed documents (pool
   v1, the public-candidate refill and earlier testnet draws), redrawing them as
   negatives without limit while parent uses keep the use limit and gap; fresh
   stock is drawn only for a cohort with such an entrant, and the coordinator
   refuses any other pool (`EXPOSED_POOLS`, `fresh_stock_refused`). When a domain or the stock runs short, reweight or
   retire domains under a new manifest,
   or lower `n` toward the floor; if even the floor cannot be filled, open no
   cohort and keep the last target live. Score every entrant, `R_k`, and
   `R_{k-1}` on the same batch in Containers with measured limits, then run
   every executed entrant once on the private fingerprint probe; Containers own
   no cohort authority. A crash counts against an artifact only when it
   reproduces on a fresh instance in single-document isolation under the
   frozen limits. Apply `reward-v1` and preserve assigned failures in
   denominators. Audit every promotion candidate with the lookup canary; a
   first flag triggers a same-cohort re-audit on a fresh Common Crawl draw,
   and two flags block the candidate, so the next retained cluster head in rank
   order that passes every promotion condition, including the strict pure-human
   false-positive tier on the canary's draw (on testnet, audit draw A from the
   cohort's pools), is promoted instead; a signed
   operator review may later clear a blocked candidate without changing past
   promotions. Gate each entrant on a loose false-positive tier over its own
   cohort's humans and report seven-cohort pooled rates as monitoring.

   Span outputs (a token-level head, character-offset AI spans with per-span
   confidence, and per-sentence share and confidence) are measured as a
   qualification gate with thresholds fixed after measurement. The release
   ships `R_0` plus the S0+ token head (export v3, document outputs
   bit-identical to `R_0`). Serve span heads in the scorer and mark the span
   graph class served before admission accepts it; until then admission
   refuses it with `graph_class_not_served`. S2c, the follow-up arm S2's
   token head fed by a trained four-layer encoder tower, is adopted as the span
   head of the first package that serves spans
   ([results](a-spans-results.md#follow-up-arm-s2)): document outputs
   byte-identical to `R_0`, spans on 21 of 570 Dev-iter humans at seed 17,
   `span_report` character F1 0.519 against the shipped head's 0.410, 210
   against 156 of 6,132 consumed-Test humans with a span, and 18–20% more
   encoder CPU time. Build and replay a calibrated S2c package before span
   serving; its graph is neither allow-listed class and exceeds the
   160M-parameter limit, so admitting such a graph needs a versioned manifest.
   The shipped head stays S0+ until span serving lands.

   Done when one private artifact completes a hidden cohort within funded
   serverless limits, with reproducible scoring, a published package, and no
   dependence on a miner or development machine.

   Status (testnet, November 1): the `EvalCohort` engine in
   [`subnet/coordinator`](../subnet/coordinator/README.md#hidden-evaluation-m4)
   runs cohorts on finalized epoch-index boundaries with resumable claimed
   steps; phase minimums count active tempos, so silence consumes none. Outage
   faults resume by an operator clear, and faults the frozen plan cannot
   resume end only by explicit abandonment without a target. Its main instance
   queues only `reward-v1` (plan v3) and probe plans. It draws humans only from
   exposed pools under the rewrite-source ledger, refuses batches short of the
   plan's mix, generates mixed (spliced sentence rewrites with exact spans and a
   recorded voice and operation) and scratch model texts with Workers AI,
   scores every roster artifact, `R_k` and `R_{k-1}` on the same batch, and
   every executed entrant on the probe, in fan-out lanes of the on-demand
   `a-scorer-testnet` Container, stores content-free chunked receipts (one per
   attempt, lost ones included), and builds and signs `reward-v1` packages
   (published index, two-tier false-positive gate, canary drills, closures)
   under the T12 roots validators trust. Rehearsal results are in
   [TESTNET.md](../subnet/coordinator/TESTNET.md#reward-v1-rehearsal-2026-10-31-to-2026-11-01).
   Still open: on-chain artifact admission and miner artifact qualification
   wired into cohorts (rosters are operator-declared), the fresh-stock path for
   cohorts with an admitted outside entrant, the canary statistic (testnet has
   no anchors; flags come only from drills), scoring audit draws for promotion
   candidates alone (every executed entrant scores them), receipt deletion,
   span outputs, and cohort control inside the coordinator Worker (T13).

5. **Complete native participants, score publication, and parallel commitments.**

   Build the miner client for enrollment, digest commitment, private artifact
   upload, qualification status, and model-version management. Build the validator client around the
   existing journaled adapter. Startup reconciles history before loading the
   signing path for new operations.

   Publish aggregate per-miner score components and enough public inputs to
   recompute eligibility arithmetic, allocation, and the canonical row. Publish
   a content-free receipt for every inference attempt, including failures and
   uncertain outcomes. Bind the full R2 index, count, object digests, and root to
   the signed package. Exclude private texts, labels, per-example predictions,
   parsed features, credentials, and commitment openings.

   Validators verify current authority, package signatures, artifact/registration
   bindings, each commitment's position against their own finalized view, the
   copy-check consistency, and every retained receipt object before a new
   commitment. Make large
   package verification bounded and resumable. Derive the row independently and
   compare any proposed row with that result. A local verification failure pauses
   that validator's new commitments without blocking the coordinator. These
   checks verify arithmetic and receipt integrity; hidden evaluation results
   remain coordinator assertions.

   All validators re-commit the active target at every tempo using separate
   keys, nonces, and journals. No one waits for the coordinator's chain row or a
   peer acknowledgment. Publish the coordinator's finalized-row certificate later
   as consistency evidence. A new calculation target requires a complete successor
   package and receipts and activates at its boundary; a partial publication does
   not displace a valid target. Each package is sealed with a separately
   signed reward-intent authorization; a manifest-free launch authorization
   covers the burn row until the first cohort target activates. No
   authorization expires: it yields commitments from its issue epoch for as
   long as its target is the validator's active target, and only a newer
   target or a signed revocation ends it, so a silent coordinator never stops
   validators committing. Every commitment re-checks the validator's active
   target. Pending obligations remain recoverable under their original
   identities.

   Delete cohort N's detailed receipts and index after N+1 and N+2 complete and
   `T_N` is superseded, writing receipts in chunks. Keep the artifacts of `R_k`
   and `R_{k-1}` while they are references. Preserve compact score records,
   roots/counts, calculated rows, certificates, and chain/accounting recovery
   state. Expired receipts cannot authorize a new
   commitment; already submitted commitments recover from their journals.

   Done when independently running validators derive identical bytes on Linux
   and macOS, submit in parallel, and recover delayed operations without stale
   rows overwriting newer ones. Complete another cohort while every other validator
   is offline or outdated. Verify receipt deletion and retained recovery evidence.

   Status (testnet, November 1): validators receive the active target as an
   operator-installed `reward-v1` bundle (T12) rather than fetching packages.
   The coordinator and the native validator verify it with
   `target_bundle::verify` against their own held-target record, derive each
   tempo's row from a fresh finalized snapshot, and commit it max-upscaled
   under the authorization checked for that epoch. The native validator retires
   superseded work that never landed and commits an owner burn row (the launch
   row, or a target whose row pays only the owner UID) under rtao's
   `OwnerRowPolicy::Submit` when the chain limits allow it
   (`MinAllowedWeights` of 1 for a validator that is not the owner, only the
   owner UID listed under a `MaxWeightsLimit` below 65535). It defers a tempo
   whose row rtao would still hold (self-only or self-plus-owner), a burn row
   the limits refuse, and deferred and unauthorized tempos, leaving its
   previous weights on chain. The coordinator commits burn rows too. Still
   open: per-tempo renewals, authorization of the burn row by a package without
   a cohort target, rebuilding the published index from closed cohorts instead
   of trusting the coordinator's, binding that index by digest (bundles embed
   it whole), receipt deletion, and a live row paying more than one UID.

6. **Finish signed updates, ordered migrations, and runtime-upgrade recovery.**

   Implement the Rust supervisor and signed release channel. Verify artifact
   digest, platform, compatibility, storage headroom, and release authority before
   staging. Checkpoint work and stop new signing before switching binaries. Start
   release/schema fixtures during phase 1 and exercise them as each component lands.

   Retain every required migration step from each released schema. A returning
   node may install the current binary and run those steps sequentially without
   downloading and executing each intervening application release. Persist intent
   and completion for each transactional or resumable step. Reconcile chain effects
   before signing resumes. Auto-update must not replace a miner's enrolled model.

   Support compatible overlapping Worker deployments, versioned queue messages,
   and bounded storage migration. Roll back only to a binary that understands the
   resulting state. Unknown or corrupt state stays preserved in a local pause;
   repair never restores an old journal and blindly resubmits its operations.
   Settle update deferral/opt-out and staggered rollout policy before release,
   without requiring peer adoption or permitting incompatible signing.

   Qualify journal growth and storage migration before backend capacity is
   exhausted. The inspected native backend has a 64 MiB image cap and retains
   operation identities; its 256-outcome reveal diagnostics ring cannot hold the
   application's durable evidence. Preserve that evidence separately, and test
   migration without dropping unresolved attempts or duplicate-effect protection.

   Done when interruption at each migration step, several missed releases, a
   pending reveal during update, key rotation, and a chain runtime change all
   preserve recoverable obligations. Demonstrate pause, forward repair, and resume
   of the same cohort, with signing ownership held by only one valid generation.

7. **Implement the private customer API and payment/burn accounting.**

   Build authenticated quotes, funding/capacity reservations, bounded A jobs,
   status, and private result retrieval. Pin the artifact, workload, price, limits,
   deadlines, refund terms, and retention at acceptance. Use idempotency keys and
   keep customer plaintext out of queues, public receipts, and operational logs.
   Start execution only after funding and admission are durably confirmed.
   Deliver encrypted results before authorizing a burn.

   Implement card, TAO, and alpha funding adapters. Qualify the selected Coinbase
   Onramp route to native TAO, custody, settlement evidence, and refund support.
   Keep any additional Solana route separate from the required launch paths.
   Track original-currency liabilities through conversion. A completed onramp
   purchase does not establish that the service can refund the card. Verify the
   owner-authorized burn and hyperparameter paths under the ownership or lease
   agreement before activating either operation.

   Apply the fixed principal routes: card-funded USD buys TAO, TAO buys alpha,
   and alpha is burned; TAO purchases skip the first step; alpha purchases burn
   directly. Make custody, delivery, conversion, refund, and burn separate durable
   states. Honor the quoted pre-burn refund window and reconcile uncertain chain
   effects before retrying. Prevent one burn or payment from being credited twice.

   Fund operator runtime, evaluation, payment processing, conversion/burn fees,
   quote shortfalls, and refund reserves separately from customer burn principal.
   Top up provider-deducted fees so the full principal reaches the burn route.
   Other validators fund their own runtime and chain fees. RPC service fees are
   zero. Preserve `price = M_price * C_model_ref`, measuring all model invocations
   per delivered job. Use `C_total_ref`, including non-model delivery costs, for
   operating viability. Calibrate the multiplier against measured costs and
   comparable Pangram detection workloads; approximately 25x is an initial
   sensitivity scenario.

   Paid-use emissions remain inactive unless a separate policy defines model
   attribution, self-purchase controls, caps, and unused-pool handling. Sales may
   open with quality-only rewards. Price markup and alpha burning do not guarantee
   miner solvency or a particular market price.

   Done when sandbox-funded jobs survive timeouts and duplicate callbacks, return
   the quoted refunds, and burn the correct principal once after valid delivery.
   Qualify live payment/chain behavior and funded reserves before opening sales.
   The coordinator records commercial activation only after measured model quality
   meets the selected requirements; benchmark eligibility alone does not open checkout.

8. **Qualify the integrated release and operate a bounded pilot.**

   Run a complete A cohort through admission, hidden evaluation, package/receipt
   publication, independent calculation, native commit/reveal, transition, and
   the next cohort. Exercise serverless eviction and recovery at every external
   effect boundary, including custody and burns when commercial service is enabled.
   Inject runtime upgrades (work continues; only an operation without a
   type-correct encoding defers), RPC outages, nonce conflicts, UID reuse, stale owners,
   incomplete packages, corrupt receipts, missing validators, and interrupted
   migrations. Confirm that each failure pauses only the actions it invalidates.

   Keep synthetic/local-chain checks separate from evidence for the intended
   deployed runtime and Cloudflare environment. Record tested OS/architecture,
   SDK revisions, chain runtime, resource limits, latency, costs, and unresolved
   constraints. Upstream CI or successful Wasm compilation cannot replace those
   measurements. Verify that restore procedures retain unfinished cohorts beyond
   any platform backup window and that expired customer content is actually removed.

   Add redacted operational status for cohort progress, blocked actions, pending
   submissions/reveals, receipt availability, storage growth, migrations, and
   funding. Supply recovery procedures that preserve evidence and never require
   deleting a journal or skipping a cohort. Use an isolated canary deployment and
   bounded funded pilot before expanding service.

   Done when the full acceptance matrix passes on the selected deployment, open
   launch decisions are recorded, and the operator can diagnose and recover the
   rehearsed failures without a peer-validator cooperation step.

   Owner actions that need the subnet owner's coldkey stay with the owner:
   registering the project hotkeys and raising testnet subnet 601's immunity
   period to 21,600 blocks, above the 25–49 hour first-payout lag. The testnet
   roster cap is 40 entrants; mainnet has no roster cap beyond the 256-UID
   space and no spending cap. The mainnet emission split is not
   selected; testnet 601's split is operator configuration.

**Parallel work and scope**

After the shared contracts are defined, the serverless rtao path, cohort engine,
and A viability/evaluation work can proceed independently. Native clients can
consume signed fixtures before the coordinator publishes live packages. Payment
integration can use synthetic jobs before commercial activation. Updater and
migration coverage grow with every persisted component.

Later B adds qualified private miner-account inference, author-profile
feedback, preservation checks, and its own evaluation/funding rules. Writer
profiles use the A encoder plus stylometric features; a learned matcher on fresh
authors, with thresholds measured on withheld writers and real style-transfer
data, must qualify before any profile backs `provided_writer` or B's author-fit
feedback. Later B work uses the native grammar for edit units and revision
guidance, and grammar-level explanations belong to that guidance. C's
contribution format, linguistic review, duplicate/overlap handling, and one-time
award settlement follow their own activation plan, which may precede B's. A has
no CFG input, and word-level explanations remain a later feature. None becomes
an implicit A launch dependency.

**Validation and workspace practice**

Run focused builds, tests, and experiments on the authorized compute host with
explicit resource bounds and durable task-owned jobs. Start with one Cargo job
and zero swap; increase concurrency only after checking headroom. The repository has no CI.
Tests use synthetic or authorized fixtures and do not
silently invoke paid models or detectors. Use only authorized runners for
platform-specific checks; a target remains unqualified until its evidence exists.

Keep temporary work under one recorded task root per host. Before cleanup, check
processes, configuration, symlinks, and environment dependencies; preserve useful
bounded evidence and remove only validated disposable paths. Preserve wallets,
private datasets, model assets, participant state, and recovery journals.

Each milestone should leave reviewed code, focused regression evidence, updated
integration notes, and an exact dependency pin. Update the whitepaper at meaningful
evaluated milestones or when an explicit design decision changes. Keep this plan
current as work completes rather than accumulating conflicting implementation plans.

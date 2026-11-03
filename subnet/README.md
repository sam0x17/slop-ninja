# Native subnet chain integration

This independent Rust crate integrates Slop Ninja's native weight-delivery path
with `rtao` at
[`b5553a76ff67f0f54350472ef0306f225e8a4c44`](https://github.com/sam0x17/rtao/tree/b5553a76ff67f0f54350472ef0306f225e8a4c44).
It is a library, an offline journal-control CLI, and the native validator in
[`crates/validator`](crates/validator/README.md). The validator verifies a
coordinator-signed score package (testnet shortcut T12: a local bundle file),
derives the row with the protocol crate and commits it through this adapter;
it signs only under `run --dispatch`. [TESTNET-M2.md](crates/validator/TESTNET-M2.md)
is the subnet 601 procedure. The testnet Workers coordinator lives in
[`coordinator/`](coordinator/README.md) and is deployed on testnet. It follows
finalized blocks through the webhook heartbeat and a one-minute cron watchdog
with RPC fallback, and from M3 it signs and journals its own timelocked weight
commit once per subnet epoch with the coordinator hotkey. The repository does
not yet have a production validator or a production Workers coordinator.

The workspace also holds the shared [protocol crate](crates/protocol/README.md).
It defines versioned records, canonical encoding, domain-separated signatures,
operation identities and the `testnet-reward-v0` row calculation, and it builds
for native targets and `wasm32-unknown-unknown`. This adapter and the
coordinator take their record and operation definitions from that crate.

The [implementation plan](../docs/subnet-implementation-plan.md) covers the
serverless coordinator, native participants, upstream rtao fixes, and qualification.

One lead validator serves as the coordinator and is operated by Slop Ninja.
The coordinator is the serverless service: it performs hidden evaluation and
scheduling as well as its own row calculation, hotkey signing, submission,
finalized reconciliation, and recovery on Workers and Durable Objects. Other
validators run native Rust applications and receive no private evaluation data.
This adapter serves the native participants; its runtime and file journal are
not assumed to support the serverless coordinator unchanged.

The adapter uses upstream `OperationJournal`, `WeightScheduler::with_journal`,
`with_semantic_guard`, `enqueue`, `recover_finalized`, and `on_finalized`.
It preserves the upstream transaction/reveal recovery implementation rather than
maintaining a second submission engine. Slop Ninja still owns the scoring and
authorization described in the [design draft](../docs/cloudflare-subnet-draft.md).

## Native lifecycle

1. Open `NativeWeightAdapter` at a persistent path with explicit genesis, subnet,
   validator hotkey, weight version, semantic contract, and semantic guard.
   Opening persists pause, including after restart. Identity mismatches refuse
   reuse of an existing journal. Required MEV shielding is unsupported at this pin.
   The journal identity is chain genesis, subnet and hotkey; no runtime version
   is part of it.
2. Call `recover_finalized` with a public client and finalized header, without
   loading a private signer. Each external recovery call persists pause before
   reading history and requires an explicit resume afterward. Calls are bounded;
   repeat as needed while inspecting
   unresolved attempts and pending reveal progress. A successful call
   need not mean the history is fully reconciled.
3. Explicitly resume after recovery. This enables scheduling; it does not approve
   a model, prove chain delivery, or settle a cohort. The application must
   check current coordinator authorization and compatibility at dispatch.
4. Enqueue an authenticated frozen row using `WeightWork`. Operation identity
   binds chain, validator, cohort, manifest, revision, and mechanism. Changed
   input for the same identity is refused. Rows must contain finite nonnegative
   scores with at least one positive entry. Empty-pool reward policy remains an
   application decision.
5. Supply the caller-owned signing context to `dispatch_finalized`. It re-reads
   the live runtime, re-evaluates every signed operation against its metadata
   (see "Runtime upgrades" below), and offers only queued work to the upstream
   scheduler. Inspect per-mechanism attempts and reveal outcomes; completing a
   scheduling call is not proof of inclusion, finality, or business settlement.

## Runtime upgrades

No code here compares a runtime spec or transaction version, and nothing is
certified for a runtime. Operation IDs, cohorts, rows, the journal identity and
the semantic contract contain no runtime version, so a cohort and its journal
survive any number of upgrades unchanged. rtao signs with the spec and
transaction versions it reads from the chain at signing time; that is their
only use. `RuntimeSeen` records them as observations in readiness records.

The [`surface`](src/surface.rs) module resolves each signed call by pallet,
call and argument name in the live metadata, so moved pallet and call indices
need no code change. It lowers live argument types into their wire structure
(single-field wrappers are transparent) and compares them with the types the
application supplies:

- unchanged types encode as they are;
- a wider integer, or a new trailing `Option` argument encoded as `None`, is a
  safe adaptation and is recorded;
- an absent pallet or call, a removed, renamed or reordered argument, a changed
  type or a new required argument has no type-correct encoding, so that
  operation alone is deferred with a reason code.

Encoded bytes are decoded again against the same live metadata before use. All
traversal is bounded and fallible; malformed or recursive metadata defers, and a
fuzz test covers corrupted bytes and randomly rewired type registries.

`evaluate_live` (called by `dispatch_finalized` on every block before rtao
runs, and by the validator at startup before recovery) refreshes live metadata, reads
`CommitRevealWeightsVersion` by name and writes readiness under the journal
extension `slopninja.operation-readiness.v1` whenever a state, reason code or
adaptation changes, with a bounded transition log. Besides the two weight
calls it records `rtao.finalized_observation`, the state of rtao's finalized
observation, reveal tracking and recovery. A deferral survives restart,
appears in `JournalSummary.readiness` with `alert = true`, and clears by itself
when later metadata admits a safe encoding. A deferral never pauses the
journal. A deferral of one weight call leaves the other call, finalized
observation, reveal tracking and recovery running. The exception is
`upstream_storage_refused` (below), which records that rtao itself cannot
observe or recover at this pin.

The readiness record is advisory and recomputed every block. Its types decode
tolerantly, and a record this build cannot decode (a later schema or
adaptation kind after a rollback, or corruption) is treated as absent: it is
kept byte for byte under `slopninja.operation-readiness.unreadable.v1`,
reported as `readiness.unreadable` with an alert, and rewritten by the next
evaluation. Inspection, recovery and dispatch never depend on it.

At the signing boundary, the installed guard reads fresh live metadata, refuses
a call composed for an earlier runtime (rtao then recomposes it), decodes the
call by name, re-encodes its arguments through `surface::encode` and requires
identical bytes before the application guard runs. A refusal blocks that attempt
only; rtao keeps the operation and a later attempt re-resolves it.

Three upstream limits of the pinned rtao weight scheduler are recorded as
deferrals rather than worked around. Each clears automatically when the live
chain changes back or rtao gains a by-name path:

- **`upstream_descriptor_refused`**: rtao composes weight calls through its
  compiled descriptors, which refuse a moved index or any argument shape
  change, and it offers no hook for an application encoder. Such a change
  defers that call even where `surface::encode` has a safe encoding.
- **`upstream_storage_refused`**: rtao's scheduler, weight preflight and
  reveal watcher read 23 storage items (`surface::upstream_storage`, for
  example `Uids`, `LastUpdate`, `MechanismCountCurrent`, `StakeThreshold`,
  `SubnetOwnerHotkey`, `CommitRevealWeightsVersion`, `TimelockedWeightCommits`,
  `SubnetEpochIndex`, `RevealPeriodEpochs`, `ValidatorPermit` and
  `Drand.LastStoredRound`) through compiled typed descriptors. Its typed read
  fails the whole read when any one item's shape changes, with no by-name
  fallback. Then `recover_finalized` and `on_finalized` return errors, so
  finalized observation, reveal tracking, recovery and both weight calls stop
  together. The adapter checks these descriptors with rtao's own test and
  defers `rtao.finalized_observation` and both calls, naming the items. At
  startup a recovery error with this deferral is a classified retry
  (exit 75), not a crash. The fix belongs upstream: rtao should read these
  items by name, as the validator's `subnet_state` fallback already does for
  its own reads.
- **`commit_reveal_version_unsupported`**: rtao's timelock encoder produces
  only `SUPPORTED_CR_VERSION` (4). If `CommitRevealWeightsVersion`, read by
  name, differs, rtao refuses every commit attempt. A commit for an unknown
  version has no correct encoding, so only the commit operation is deferred.

`dispatch_finalized` keeps in-process recovery after an rtao scheduler error
only when that call signed nothing (no new attempt, none unresolved or
finalized failed). rtao reports every per-mechanism refusal as an attempt
outcome and returns `Ok`. An `Err` comes from its finalized recovery, a typed
storage read or RPC, and the next block retries it, with rtao re-running its
recovery first.

Application checks must bind the intended row to the actual call. Every validator
independently calculates the native row from the coordinator's signed per-miner
score components, baseline, denominators, eligibility summaries, allocation
parameters, and registration bindings. The calculation contract fixes arithmetic,
ordering, normalization, rounding, and native encoding. Identical manifests and
finalized registration bindings must yield identical canonical UID/weight
entries; the chain runtime version is not an input.
Other validators trust hidden evaluation and receive no held-out documents, labels,
per-example predictions, customer text, or private model weights. They verify
the full score-bound R2 receipt index, counts, and object digests before new
commitments. The native validator implements this for the T12 bundle: signed
records, receipt-index root and count, every receipt object, the registration
snapshot and `testnet-reward-v0`, and for the `reward-v1` bundle also the
published index, the recomputed outcome, the closure, the reward-intent or
launch authorization checked at every tempo, and the finalized owner hotkey
for burned units. R2 retrieval, authorization issue and renewal, target
supersession and the coordinator consistency checks remain to be implemented.

All validators, including the coordinator, commit in the same target window using
their own keys, nonces, and journals, without waiting for coordinator finality or
peers. The coordinator's later signed certificate binds its finalized row to the
calculation and receipt package as consistency evidence. Another validator's
missing or mismatched row cannot block local signing with valid inputs and
authority, or coordinator progress. Detailed receipts expire after two subsequent
completed cohorts; compact arithmetic inputs, calculated rows, certificates, and
chain recovery records remain. Already submitted obligations recover from the
journal without expired receipt access.
Preserve original hotkey/registration credit through
UID changes, and enforce current authorization. Passing arbitrary JSON or a
permissive semantic guard is not a substitute for those checks. The native
journal contains sensitive signed bytes and commit material; do not publish it.

The pilot policy enables native weight commit/reveal. The recognized subnet owner
must set `commit_reveal_weights_enabled = true`; the operator verifies the
effective setting. A lease must provide the required owner-authorized execution
path. Production application checks must
verify that setting at startup against finalized state and before signing against
fresh submission state; an unexpectedly disabled flag pauses affected new commitments
without silently falling back to direct submission. The native validator performs
the startup check and, in its signing guard, the check before every signature,
which also refuses any direct-weights call. Neither is qualified on a live chain. Upstream recovery support for commit/reveal does not
establish that the adapter enforces this submission policy. Model-artifact
commitment and reveal admission rules are separate.

The coordinator publishes a signed calculation target, which remains usable when
a newer incomplete score publication appears. Its successor needs the complete
signed score and retained receipt package, with no coordinator finality prerequisite.
Expiry, revocation, registration changes, and compatibility limits still apply.
Public score inputs make the row reconstructible; commit/reveal does not prevent
copying or enforce independent calculation. Package verification, inclusion,
native reveal timing, finality, and rate limits prevent any atomic-switch promise.

Preserve pending commitments and their original identities through restarts,
upgrades, and target changes. Timelocked reveal can occur before the coordinator's
later consistency certificate. Reconcile obligations before new signing and
prevent stale recovery from overwriting a newer row. A missed window records
delayed delivery; use the current authorized target at a later eligible window
without discarding cohort results or waiting for another validator.

## Journal controls

The CLI requires an existing application journal and never loads keys or calls
an RPC endpoint:

```sh
cargo run --manifest-path subnet/Cargo.toml --bin slop-ninja-journal -- \
  --journal /persistent/validator/weights-journal.json inspect
cargo run --manifest-path subnet/Cargo.toml --bin slop-ninja-journal -- \
  --journal /persistent/validator/weights-journal.json pause
```

Inspection prints counts and pause state, excluding payloads and signed bytes.
The library's `last_attempt()` and `reveals()` accessors expose sensitive internal
diagnostics. Do not serialize them into public status or logs; use the redacted
summaries for those surfaces.
The native backend holds a lifetime lock. Stop the owning process before using
the offline CLI, or use its shared `JournalControls` handle inside that process.
Never remove the lock file to force takeover. Live admin transport is not
implemented. Resume requires recovery through the library; there is no offline
command that clears uncertainty.

## Gaps against the merged revision

| Area | Integration boundary |
| --- | --- |
| Journaled MEV shield | Explicit/required shielding refuses. Upstream journaled shield dispatch is still refused at the pinned revision. Never retry without the journal to bypass this refusal. Ordinary weight commit/reveal follows the supported upstream path; the pilot still requires downstream testing on a live chain. |
| Runtime upgrades | No spec gate. Every signed operation is resolved by name against live metadata and deferred alone when it has no type-correct encoding (above). At this pin rtao's weight scheduler still composes calls and reads its storage only through compiled descriptors, and its timelock encoder supports one commit-reveal version. These defer as `upstream_descriptor_refused`, `upstream_storage_refused` (which also stops rtao's finalized observation and recovery) and `commit_reveal_version_unsupported` until rtao composes and reads by name. The live spec 486 surface is in [the runtime 486 reference](../docs/subnet-runtime-486-diff.md). |
| Application authority | The native validator verifies a T12 bundle (signed manifest, package, target and receipt index, every receipt object), reads its registration snapshot, derives the row with `testnet-reward-v0`, and its guard signs only that verified row as a timelocked commit while commit/reveal is enabled. The Workers coordinator verifies the same T12 bundle with the protocol crate and derives its own row from its own registration snapshot (M3). Per-tempo re-commit of the verified target is implemented natively (plan decision 12; each re-commit has its own protocol tempo identity) and ran live on 601. Both also verify `reward-v1` bundles (the launch burn row or a cohort target, with the recomputed outcome, closure and authorization) and commit per-tempo `WeightOperationV2` identities. The native validator also fetches the published records from the public status host, verifies them against its held target and supersedes its target natively: a renewal refreshes the authorization in place, and a successor is installed at its eligible boundary once no commitment of the old target is in flight, with the run continuing on it in the same process ([`crates/validator/README.md`](crates/validator/README.md#fetcher)). That path awaits the public host's deployment and the coordinator's storage of the authorizations and renewals it issues, which the publisher copies; until both exist the operator installs bundles (T12). Coordinator row consistency checks, authorization issue and renewal by the coordinator itself, and production empty-pool policy still need the production application. rtao holds (does not commit) a row whose positive entries are only the validator itself or the owner hotkey's UID, which includes the launch burn row; a native validator therefore defers each such tempo (`row_would_be_held`, exit 75), committing nothing while its previous weights stay on chain, until upstream offers an opt-in. Both sides check bundles against the target they hold active (recorded from checked closures), commit a `reward-v1` row in its max-upscaled form, and the native validator re-derives each tempo's row from a fresh finalized snapshot and retires superseded work that never landed. Elapsed time must not expire the cohort; no validator's delivery gates application progress. |
| Automatic updates | Signed Rust supervisor, ordered resumable schema migrations, and post-migration recovery remain to be built. Updates must preserve journals and frozen work without waiting for other validators. |
| Finalized failures and historical missed reveals | Require application adjudication; they are not successful work and cannot authorize blind replacement. There is no safe operator shortcut that deletes history or clears uncertainty. |
| Durable evidence size | Native journal images are limited to 64 MiB and retain operation identities. A storage migration is needed before exhaustion. Completed reveal diagnostics retain a 256-outcome ring; cohort evidence requires separate durable retention. |
| Workers coordinator | The service is also the lead validator. Workers-compatible RPC transport, canonical call encoding, hotkey signing, nonce/attempt journaling, finalized reconciliation, weight/reveal recovery, and a Durable Object journal backend remain to be integrated and qualified. The native synchronous file backend supplies none of that storage integration. |
| Block webhooks | The rtao-rpc pilot is live for Finney and testnet. The testnet `coordinator/` implements its v1 contract and treats each finalized-block delivery as its heartbeat: it authenticates and persists the delivery before acknowledging, deduplicates by delivery ID and block hash, and advances a parent-linked finalized cursor anchored and audited through read-only RPC. Webhook-advanced blocks stay provisional until RPC confirms them, and scheduling judges due work at that RPC-confirmed watermark. It backfills gaps through bounded RPC and records conflicts as durable faults. A one-minute cron watchdog falls back to RPC after 300 s of webhook silence or when the cursor falls behind. It is deployed on testnet ([TESTNET.md](coordinator/TESTNET.md) records the measured delivery latency and fallback drill); launch lag remains unqualified. |
| Full validator startup | Upstream `NeuronRuntime` unlocks the signer and starts application callbacks before its scheduler recovery. `slop_ninja_validator` instead opens the journal, records live-metadata readiness of signed operations (never halting on a runtime change; an upstream storage refusal during recovery is a recorded retry), recovers without a signer, checks at finalized state that the subnet exists, the hotkey is registered and commit/reveal is enabled, resumes explicitly, and verifies the package and refuses a row rtao would hold before enqueueing. The dry run never writes the journal's work; only `run --dispatch` enqueues and unlocks the hotkey. On 2026-10-30 the dry run and then live dispatch ran against testnet subnet 601: CRv4 commits from validator2 in epochs 7–11 and 14, each read back as one commit-queue entry and revealed as the exact row, through crash, kill, 429, failover and era-expiry drills with no duplicate commit. Measurements and the rtao limits they exposed (admission window, metagraph polling, reveal replay cost, terminal 429) are in [`crates/validator/TESTNET-M2.md`](crates/validator/TESTNET-M2.md). |
| Testing | The offline tests cover local storage, adapter controls, journal migration, live-surface resolution against recorded and mutated metadata, package verification, restart at every application boundary, and a signed attempt whose response was lost. Downstream finalized-RPC/reveal recovery fixtures, the live drills in TESTNET-M2.md, a live runtime upgrade, target-Mac filesystem behavior, and hardware power-loss recovery remain untested. Upstream CI does not establish those properties for this application. |

The native build carries the upstream `bittensor-core` and `w3f-bls` patches at
the root of this workspace. Cargo does not inherit a Git dependency's workspace
patches. Keep these selectors aligned with the `rtao` revision and lockfile when
updating; an unpatched dependency graph differs from the graph upstream tested.

Validated on `build-host` (Linux x86_64, Rust 1.99.1) on 2026-10-30 at rtao
`b5553a76`: 38 adapter unit tests (including the schema-2 migration fixture,
mutated-metadata cases for calls and for the storage rtao reads, the
commit-reveal version deferral, an unreadable readiness record, three
no-panic fuzz tests over corrupted metadata, rewired type registries and
random call bytes, and a lost-response restart test), the 2-test
`no_runtime_gates` lint, 40 validator tests (package verification, fault
boundaries, the journal-free dry run, the held-row refusal, step 0 and
payload golden vectors, by-name storage keys and values) and 16 protocol tests passed; workspace Clippy passed
with warnings denied; the protocol crate's wasm32 Clippy and debug/release
builds passed with native, wasm32 and golden reports identical. The journal CLI
refused a missing journal without creating files. No private key was loaded and
no transaction was submitted.

On main after the testnet stack merge (2026-10-31, same host and toolchain):
39 adapter unit tests, the 2-test `no_runtime_gates` lint (which now also
scans the coordinator's signing code), 44 validator tests and the protocol
crate's 63 unit, 3 allocation and 6 whitepaper-number tests and 3 doctests
passed with `--test-threads=1`; workspace Clippy passed with warnings denied,
with and without the validator's `testnet-drills` feature.

Run focused checks on the authorized compute host, not GitHub Actions. Set
`SLOP_NINJA_TEST_TMPDIR` to an existing task-owned temporary root; the tests use
it for disposable synthetic journals and remove their per-test directories.

```sh
cargo test --manifest-path subnet/Cargo.toml --locked -- --test-threads=1
cargo clippy --manifest-path subnet/Cargo.toml --locked --all-targets -- -D warnings
```

The `no_runtime_gates` integration test scans the subnet crates and the
coordinator and fails if production code compares, allow-lists or branches on a
runtime spec, transaction version, spec name or metadata hash, or reintroduces
a removed runtime gate.

## rtao pin

[`b5553a76`](https://github.com/sam0x17/rtao/commit/b5553a76ff67f0f54350472ef0306f225e8a4c44)
is rtao main as of October 30, 2026 (`git ls-remote`). It vendors Finney
runtime 477–486 V15/V16 metadata; its testnet metadata pins stop at 481. The
testnet 486 V15 snapshot recorded here
([`fixtures/runtime-486`](fixtures/runtime-486/)) is byte-identical to rtao's
Finney 486 V15 (sha256 `582b1d05…1a81`). It records no validated runtime range in
[`COMPAT.md`](https://github.com/sam0x17/rtao/blob/b5553a76ff67f0f54350472ef0306f225e8a4c44/COMPAT.md),
and only logs, never refuses, when the connected runtime is outside its
evidence. Its `[patch]` selectors (`bittensor-core` at `5f2477f5`, `w3f-bls`
and `subtensor-macros` at `v478`) are mirrored at the root of this workspace;
its `ruint` patch is outside this dependency graph.

Its `OperationJournal` uses schema 3. Opening a schema 1 or 2 journal migrates it
in place: attempts gain stable IDs and roles, records gain an execution mode, and
every original field must survive a typed round trip, or the open is refused
with the original bytes retained. A newer schema, or a legacy field that
collides with a schema-3 name, is refused the same way. Journals must be
owner-only files (mode `0600`); wider permissions are refused with a clear error,
never reset. [`fixtures/journal-schema2`](fixtures/journal-schema2/README.md)
holds a journal written by this adapter at rtao `aa8627ba`; tests prove it
migrates with its work, attempt and legacy extension intact.

Upstream refusals that remain at this pin: production journaled shield
dispatch; composition of a weight call whose live indices or argument shape
differ from rtao's compiled descriptor; typed reads of the 23 storage items
its scheduler and reveal watcher use, which stop finalized observation and
recovery when any one changes shape; and its single supported commit-reveal
version (4). The last three are deferred here as described above.

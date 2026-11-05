# Native subnet chain integration

This independent Rust crate integrates Slop Ninja's native weight-delivery path
with `rtao` at
[`0c12816009baa65eec3135bac9a9dbcc8eaba77d`](https://github.com/sam0x17/rtao/tree/0c12816009baa65eec3135bac9a9dbcc8eaba77d)
(immutable tag `snapshot-2026-11-03.2`).
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
certified for a runtime. Tests run against two recorded V15 metadata blobs:
`fixtures/runtime-486/testnet-486.v15.scale` (testnet and Finney 486) and
`fixtures/runtime-487/pr3294-f3654266.v15.scale`, extracted offline from the
try-runtime build that CI published for subtensor PR #3294 (runtime 487, head
`f3654266`, unmerged on 2026-11-04; sha256 `5e4f52fc...97de`). The second is
test data only, re-extracted if that PR changes before it merges. Operation IDs, cohorts, rows, the journal identity and
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

rtao composes weight calls through its compiled descriptors while their live
shape matches and by name against live metadata otherwise (D-088), and reads
the storage and constants of its weight preflight, scheduler and reveal
watcher the same way (D-089). Those fallbacks are recorded as adaptations
(`UpstreamComposedByName`, `UpstreamReadByName`). What rtao itself still
refuses is recorded as a deferral, which clears by itself when the live chain
changes back or rtao gains support:

- **`upstream_compose_refused`**: a weight call that neither rtao's compiled
  descriptor nor its by-name composition can produce, with rtao's own verdict
  (`ValidatorSurface`). Only that call waits.
- **`upstream_storage_refused`**: a storage item or constant that rtao reads
  neither through its compiled descriptor nor by name (`surface::upstream_storage`
  is rtao's own list, so this check never drifts from what it reads). Then
  `recover_finalized` and `on_finalized` return errors, so finalized
  observation, reveal tracking, recovery and both weight calls stop together.
  At startup a recovery error with this deferral is a classified retry
  (exit 75), not a crash.
- **`commit_reveal_version_unencodable`**: a stored `CommitRevealWeightsVersion`
  that no `u16` commit tag can carry.

For any other stored commit-reveal version than the payload version rtao
encodes (`NATIVE_CR_PAYLOAD_VERSION`, 4), the record says what rtao will do
(D-086). `evaluate_live` also reads the `:code` storage hash at the same
block, the runtime code identity rtao checks:

- under code on rtao's reviewed list (484 and 486 at this pin) rtao follows the
  stored tag: an adaptation of the commit (`CommitRevealTagChanged`);
- under any other code rtao follows the tag only after one of this
  validator's own commitments revealed under that code, and otherwise pauses
  timelocked commits for the mechanism every block. That evidence lives in
  rtao's dispatch journal, out of the record's reach, so the commit is
  recorded as deferred (`commit_reveal_tag_unproven`) with an alert, never as
  adapted while rtao may be pausing;
- when the hash read fails, the code is unknown, not unreviewed: the record
  keeps the commit's previous tag-dependent state, so a transient RPC failure
  writes no transition and raises no alert. Only with no such earlier state is
  the commit recorded as deferred `runtime_code_unread`.

The reviewed-code list is a runtime gate in effect: every runtime after 486,
487 included, is unlisted, and a validator with nothing outstanding can never
gather the evidence. We have asked upstream to follow the stored tag by
default under any code and to keep committing after a missed reveal, at worst
rate-limited, never stopped: a missed reveal can have causes unrelated to the
tag, and a commitment that is not made can never reveal. The coordinator
already does this (`subnet/coordinator`, `signing::cr_tag_unrevealed`: it
reports the drop and commits every epoch). Until a pin carries that, a tag
change under an unreviewed runtime alerts here.
The validator's dry run, which also has no journal evidence, prepares the
commit with the stored tag and reports the basis (`commit_reveal_tag_basis`:
`native`, `reviewed_code` or `unproven_code`) instead of failing.

Two stop risks remain for a later runtime; neither affects 486 or 487, and
both are queued before a runtime that would trigger them:

- **Metadata above about 1 MB.** The coordinator stores each fetched runtime
  as hex in one Durable Object SQLite value of at most 2 MB, so a larger
  runtime fails to store and the coordinator cannot sign under it (487's
  metadata is 0.44 MB). Storing the bytes as a BLOB doubles that ceiling;
  `arena_routes::MAX_STORED_RUNTIME_METADATA_BYTES` follows the encoding.
- **The epoch-index key layout.** The eval reader reads `SubnetEpochIndex` and
  `LastEpochBlock` through a compiled key layout (the `Identity` hasher and a
  `u64` value). A runtime that changes either would stop cohort boundaries
  from advancing until that read derives its key and type by name.

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
for burned units. No authorization expires: one stays valid until a newer
authorization supersedes it or a signed revocation withdraws it, so a silent
coordinator never stops validators committing the last authorized target.
Authorization issue by the coordinator itself and the coordinator
consistency checks remain to be implemented.

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
cargo run --manifest-path subnet/Cargo.toml --bin slop-ninja-journal -- \
  --journal /persistent/validator/weights-journal.json \
  review-failure --operation <operation_id> --attempt <n>
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

A finalized failure is a signed weight call the chain included but whose
dispatch failed. Nothing was committed and its bytes cannot land again; rtao
hands the operation back. Finalized recovery reads each new failure's receipt
and decodes its dispatch error by pallet and variant name through the
inclusion block's metadata, with no runtime version check, and records it on
the attempt (`src/review.rs`). A failure whose receipt cannot be read yet
awaits decoding (`failures_awaiting_decode`): recovery and resume stay
incomplete without halting, and the next round reads it again. Recovery
reports each failed read (`SettleReport::unreadable`) by a fixed class from
the typed rtao error, never its text. A read that cannot clear by itself
(`state_discarded`, `block_not_found`, `decode`, `metadata`) is recorded as
a `receipt_unavailable` diagnostic, which awaits review like a decoded
failure; it is read again at each later round, and the first successful read
replaces it. No retry is counted and nothing expires. A decoded failure
awaits review (`finalized_failures`), and recovery and resume refuse while any
does; the validator then halts with `recovery_requires_review`. A receipt that
contradicts the recorded inclusion or outcome is recorded as a
`receipt_contradiction` and awaits review the same way. Two paths write a
permanent review marker for the attempt, kept in its operation record:

- Recovery settles a decoded error that is an ordinary weight-call refusal
  expected to clear without action (`SubtensorModule`
  `CommittingWeightsTooFast`, `SettingWeightsTooFast`,
  `NotEnoughStakeToSetWeights`; rule `slopninja.settled-weight-refusals.v1`)
  and keeps it as a diagnostic. It never halts, and recovery reports each one
  (`SettleReport::refusals`, which the validator logs as the alert
  `weight_commit_refused`), so a refusal that recurs every tempo stays
  visible. `TooManyUnrevealedCommits` and `NeuronNoValidatorPermit` are not
  settled, because a queue that never drains or a permit the stake rank never
  earns can be a lasting misconfiguration.
- `review-failure` records an operator's review of any other failure. `inspect`
  lists the failures awaiting review under `review_required`, and those
  awaiting decoding under `awaiting_decode`, each with its operation ID,
  attempt number, block and decoded error name. The command refuses unknown
  IDs, unresolved attempts (recovery must classify them first), attempts that
  expired or landed, and operations with contradictory evidence. A failure
  whose receipt contradicts the journal (`receipt_reports_success`,
  `receipt_block_differs`: the call may have landed) is refused
  (`receipt_contradiction`) unless the operator adds
  `--accept-contradiction`; the marker is then a reviewed contradiction
  (`contradiction_reviewed`), counted under `reviewed_contradictions`, never
  as a reviewed failure, and its operation retires as
  `receipt_contradiction_reviewed`. Find out what
  the error means before you review it: a wrong weights version, a disabled
  commit-reveal flag or a deregistered hotkey fails every later tempo too.

Counts include only failures without a marker (`finalized_failures`,
`failures_awaiting_decode`); `reviewed_failures` and
`reviewed_contradictions` count the rest. Markers
delete nothing and survive reopening. Once every failure of a handed-back
operation is reviewed, the operation is also marked retired
(`finalized_failure_reviewed`, `finalized_failure_settled` or
`receipt_contradiction_reviewed`), so the next
tempo's commit follows.

## Gaps against the merged revision

| Area | Integration boundary |
| --- | --- |
| Journaled MEV shield | Explicit/required shielding refuses. The pinned revision supports journaled shield dispatch from per-runtime evidence; this adapter has not qualified it and submits unshielded only. Never retry without the journal to bypass a refusal. Ordinary weight commit/reveal follows the supported upstream path; the pilot still requires downstream testing on a live chain. |
| Runtime upgrades | No spec gate. Every signed operation is resolved by name against live metadata and deferred alone when it has no type-correct encoding (above). rtao composes weight calls and reads its storage by name when its compiled descriptors no longer match, and follows a changed commit-reveal tag under reviewed or proven runtime code. What it still refuses defers as `upstream_compose_refused`, `upstream_storage_refused` (which also stops rtao's finalized observation and recovery) and `commit_reveal_version_unencodable`. rtao reads `Timestamp.Now` for its timelock clock without a by-name fallback; the validator's own by-name read of the same item keeps the clock anchored. The live spec 486 surface is in [the runtime 486 reference](../docs/subnet-runtime-486-diff.md). |
| Application authority | The native validator verifies a T12 bundle (signed manifest, package, target and receipt index, every receipt object), reads its registration snapshot, derives the row with `testnet-reward-v0`, and its guard signs only that verified row as a timelocked commit while commit/reveal is enabled. The Workers coordinator verifies the same T12 bundle with the protocol crate and derives its own row from its own registration snapshot (M3). Per-tempo re-commit of the verified target is implemented natively (plan decision 12; each re-commit has its own protocol tempo identity) and ran live on 601. Both also verify `reward-v1` bundles (the launch burn row or a cohort target, with the recomputed outcome, closure and authorization) and commit per-tempo `WeightOperationV2` identities. The native validator also fetches the published records from the public status host, verifies them against its held target and supersedes its target natively: a newer authorization of the same target refreshes it in place, a signed revocation (fetched or installed with `revoke`) withdraws it, and a successor is installed at its eligible boundary once no commitment of the old target is in flight, with the run continuing on it in the same process ([`crates/validator/README.md`](crates/validator/README.md#fetcher)). The coordinator writes the private authorization pointer from its installed bundle and journaled revocations, and the publisher copies it; that path awaits the public host's deployment, and until then the operator installs bundles (T12). No authorization expires. Coordinator row consistency checks, authorization issue by the coordinator itself, and production empty-pool policy still need the production application. rtao holds (does not commit) a row whose positive entries are only the validator itself or the owner hotkey's UID unless the work is queued under its owner-row policy `Submit` (D-093), and then still holds self-only and self-plus-owner rows. A native validator queues a `reward-v1` owner burn row (the launch burn row under the launch authorization, or a verified target whose row pays only the owner UID) under `Submit`, journaled with the work, after checking `MinAllowedWeights` and `MaxWeightsLimit` by name; it queues every other row under `Hold` and defers a tempo whose row rtao would hold (`row_would_be_held`, exit 75), committing nothing while its previous weights stay on chain. Both sides check bundles against the target they hold active (recorded from checked closures), commit a `reward-v1` row in its max-upscaled form, and the native validator re-derives each tempo's row from a fresh finalized snapshot and retires superseded work that never landed. Elapsed time must not expire the cohort; no validator's delivery gates application progress. |
| Automatic updates | Signed Rust supervisor, ordered resumable schema migrations, and post-migration recovery remain to be built. Updates must preserve journals and frozen work without waiting for other validators. |
| Finalized failures and historical missed reveals | A finalized failure is never successful work and never authorizes replacing its signed bytes. Recovery waits (without halting) until its receipt is read, then halts on it until it is reviewed: known weight-call refusals settle automatically by decoded error name and are logged as alerts, and any other needs `review-failure` ([Journal controls](#journal-controls)). Nothing deletes history or clears uncertainty. |
| Durable evidence size | Native journal images are limited to 64 MiB and retain operation identities. A storage migration is needed before exhaustion. Completed reveal diagnostics retain a 256-outcome ring; cohort evidence requires separate durable retention. |
| Workers coordinator | The service is also the lead validator. Workers-compatible RPC transport, canonical call encoding, hotkey signing, nonce/attempt journaling, finalized reconciliation, weight/reveal recovery, and a Durable Object journal backend remain to be integrated and qualified. The native synchronous file backend supplies none of that storage integration. |
| Block webhooks | The rtao-rpc pilot is live for Finney and testnet. The testnet `coordinator/` implements its v1 contract and treats each finalized-block delivery as its heartbeat: it authenticates and persists the delivery before acknowledging, deduplicates by delivery ID and block hash, and advances a parent-linked finalized cursor anchored and audited through read-only RPC. Webhook-advanced blocks stay provisional until RPC confirms them, and scheduling judges due work at that RPC-confirmed watermark. It backfills gaps through bounded RPC and records conflicts as durable faults. A one-minute cron watchdog falls back to RPC after 300 s of webhook silence or when the cursor falls behind. It is deployed on testnet ([TESTNET.md](coordinator/TESTNET.md) records the measured delivery latency and fallback drill); launch lag remains unqualified. |
| Full validator startup | Upstream `NeuronRuntime` unlocks the signer and starts application callbacks before its scheduler recovery. `slop_ninja_validator` instead opens the journal, records live-metadata readiness of signed operations (never halting on a runtime change; an upstream storage refusal during recovery is a recorded retry), recovers without a signer, checks at finalized state that the subnet exists, the hotkey is registered and commit/reveal is enabled, resumes explicitly, and verifies the package, chooses the row's owner-row policy and refuses a row rtao would hold under it before enqueueing. The dry run never writes the journal's work; only `run --dispatch` enqueues and unlocks the hotkey. On 2026-10-30 the dry run and then live dispatch ran against testnet subnet 601: CRv4 commits from validator2 in epochs 7–11 and 14, each read back as one commit-queue entry and revealed as the exact row, through crash, kill, 429, failover and era-expiry drills with no duplicate commit. Measurements and the rtao limits they exposed (admission window, metagraph polling, reveal replay cost, terminal 429) are in [`crates/validator/TESTNET-M2.md`](crates/validator/TESTNET-M2.md); rtao `0c128160` addresses them (D-094 to D-097), and the validator sizes the admission window from its signing budget, keeps its sessions through refused reconnects and follows only the newest finalized head ([`crates/validator/README.md`](crates/validator/README.md#paced-endpoints)). |
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

Re-pinned to rtao `0c128160` (2026-11-03, the development host, Linux x86_64,
Rust 1.99.1): 46 adapter unit tests (including the journaled owner-row policy
and its immutability, `unsigned_retry` checkpoints with and without an
uncertain slot, the rollback check and a scan that no production code clears
uncertainty by hand), the 2-test `no_runtime_gates` lint, 83 validator tests, the miner's 37 and the protocol crate's 82 unit, 3
allocation and 6 whitepaper-number tests passed with `--test-threads=1`;
workspace Clippy passed with warnings denied, with and without the
validator's `testnet-drills` feature; and the protocol crate's `v1` and
`reward-v1` reports were byte-identical on native, `wasm32-unknown-unknown`
and their golden files. Existing journals written at `b5553a76`, including a
live validator's (copied read-only, not checked in), opened and replayed with
their work, effects bytes and checkpoints unchanged, and the rollback check
found nothing in them that `b5553a76` would refuse. rtao's own weights suite
at the pin passed (110 tests, among them the burn row's commit and reveal,
restart mid-commit, the rejected-commitment re-prepare and a duplicate ID
under another policy). No key was loaded and nothing was submitted.

Run focused checks on the authorized compute host, not GitHub Actions. The
tests write disposable synthetic journals under `SLOP_NINJA_TEST_TMPDIR` when it
names an existing task-owned temporary root, otherwise under the system
temporary directory, and remove their per-test directories.

```sh
cargo test --manifest-path subnet/Cargo.toml --locked -- --test-threads=1
cargo clippy --manifest-path subnet/Cargo.toml --locked --all-targets -- -D warnings
```

The coordinator and its parity checks are separate workspaces: run them from
`subnet/coordinator` (and `subnet/coordinator/parity`), whose
`.cargo/config.toml` sets `SLOP_NINJA_MEMORY_MODEL`; with `--manifest-path`
from elsewhere the coordinator's memory-model assertion fails to compile.

The `no_runtime_gates` integration test scans the subnet crates and the
coordinator and fails if production code compares, allow-lists or branches on a
runtime spec, transaction version, spec name or metadata hash, or reintroduces
a removed runtime gate.

## rtao pin

[`0c128160`](https://github.com/sam0x17/rtao/commit/0c12816009baa65eec3135bac9a9dbcc8eaba77d)
is the immutable tag `snapshot-2026-11-03.2` (tag object `f38ab8ee`), rtao
main on November 3, 2026 (UTC), resolved with `git ls-remote`. It is a snapshot,
not a versioned release (the workspace stays 0.1.0), and a fast-forward of
`snapshot-2026-11-03` (`3e936dae`). Every rtao dependency here (the adapter,
the validator, the miner, the coordinator's `rtao-core` and its parity
checks) is pinned to that full commit ID (`slop_ninja_subnet::RTAO_PIN`; a test
checks every manifest against it). It vendors Finney runtime 477–486
V15/V16 metadata; its testnet metadata pins stop at 481. The testnet 486 V15
snapshot recorded here ([`fixtures/runtime-486`](fixtures/runtime-486/)) is
byte-identical to rtao's Finney 486 V15 (sha256 `582b1d05…1a81`). It records
no validated runtime range in
[`COMPAT.md`](https://github.com/sam0x17/rtao/blob/0c12816009baa65eec3135bac9a9dbcc8eaba77d/COMPAT.md),
and only logs, never refuses, when the connected runtime is outside its
evidence. Its `[patch]` selectors (`bittensor-core` at `5f2477f5`, `w3f-bls`
and `subtensor-macros` at `v478`) are mirrored at the root of this workspace;
its `ruint` patch (for `rtao-arena`) is outside this dependency graph.

What this pin adds over `b5553a76`, and how the adapter uses it:

- **Owner-row policy (D-093).** `OwnerRowPolicy { Hold, Submit }`, journaled
  with durable work: `Submit` work records `"owner_rows":"submit"` in its
  effects, `Hold` work keeps the bytes earlier builds wrote, and the same
  operation ID under another policy is refused like changed input.
  `NativeWeightAdapter::enqueue_with_owner_rows` queues through rtao's
  `enqueue_with_owner_rows`; `enqueue` is `Hold`. Held entries an earlier
  build journaled keep holding; nothing retires them but this application's
  own retirement of never-signed work.
- **Rollback.** A build at `b5553a76` decodes the effects and attempt
  material of every weight operation it has not handed back, and the weight
  checkpoint, with `deny_unknown_fields`, so it refuses the whole scope when
  it meets a field this pin added: `owner_rows` in pending `Submit` work (in
  steady state the next tempo's burn row is almost always queued and not yet
  attempted), `followed_cr` in a commit or pending reveal that followed a
  non-native commit-reveal tag, or the watcher's `revealed_code`, retained
  evidence once a reveal completed or missed under such a tag. Handed-back
  and retired work blocks nothing, except a finalized failed attempt: every
  build before failure review counts each one the journal retains,
  reviewed or settled included, and halts on it with no way to review it,
  so `rollback_blockers` also lists each (`attempt.outcome.finalized_failure`
  or `extensions.slopninja.failure-review.v1`, never retirable) and
  `retire_for_rollback` then retires nothing: such a journal is
  roll-forward only. `JournalControls::rollback_blockers`
  lists these fields and `retire_for_rollback` retires the blocking work
  that never signed; the validator's `rollback-check --retire`, run with the
  unit stopped, does both and exits 0 only when an older build can open the
  journal. Work with a signed attempt or an unhanded landed commit settles
  only under this build, and after rtao has followed a non-native tag the
  rollback is roll-forward only. A test decodes journals with the strict
  structures copied from `b5553a76`'s source and requires them to agree
  with `rollback_blockers`. Restarting this build after a retirement queues
  that tempo afresh under the next label, which then runs one epoch ahead
  of its commit epoch.
- **By-name reads and composition (D-088, D-089)** and **commit-reveal tag
  following (D-086)**, recorded as described above.
  `SUPPORTED_CR_VERSION` is a deprecated alias of `NATIVE_CR_PAYLOAD_VERSION`.
  The two sides differ on a non-native tag: native validators follow a
  stored tag under runtime code rtao has reviewed (or under which one of
  their commitments revealed), while the coordinator encodes the native tag
  4 and pauses its commits on any other stored tag. 601 stores 4. The
  agreement that both of our validators commit the same row each cohort
  (AR-2/AR-3) assumes tag 4: after a tag change, native validators could
  keep committing while the coordinator pauses. Neither commits a wrong
  row.
- **Paced endpoints (D-094 to D-097).** The scheduler anchors its timelock
  clock at each commit's preflight block; a dispatch that provably signed
  nothing reports `Failed` and retries in the same epoch after a 1 to 32
  block backoff, checkpointed as the slot's `unsigned_retry` (the adapter
  never clears uncertainty by hand); the admission window is configurable
  (`SchedulerConfig::submit_deadline`); a refused reconnect is retried within
  the session; finalized heads follow a `Backfill` policy; the reveal
  watcher and metagraph sync read less.

Its `OperationJournal` uses schema 3. Opening a schema 1 or 2 journal migrates it
in place: attempts gain stable IDs and roles, records gain an execution mode, and
every original field must survive a typed round trip, or the open is refused
with the original bytes retained. A newer schema, or a legacy field that
collides with a schema-3 name, is refused the same way. Journals must be
owner-only files (mode `0600`); wider permissions are refused with a clear error,
never reset. [`fixtures/journal-schema2`](fixtures/journal-schema2/README.md)
holds a journal written by this adapter at rtao `aa8627ba`; tests prove it
migrates with its work, attempt and legacy extension intact. A journal written
at `b5553a76` opens unchanged: its work reads back as `Hold` with the same
effects bytes, and its checkpoints load as written.

Not qualified here at this pin: journaled shield dispatch (rtao supports it
from per-runtime evidence; this adapter submits unshielded only), the v482
port (rtao's independent semantic review of it remains open) and any runtime
range (rtao validates none). rtao's own residuals: `Timestamp.Now` has no
by-name fallback; an unknown commit-reveal tag pauses commits under
unreviewed, unproven runtime code; an endpoint without
`author_pendingExtrinsics` falls back to the slower shield carrier re-seal;
`on_finalized` can block for a long time (a lost watch's reconcile, an
era-expiry scan); and a reveal period that churns to 0 and back within one
skipped span could go unseen by the reveal watcher.
